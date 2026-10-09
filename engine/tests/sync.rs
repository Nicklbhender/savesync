//! End-to-end: a real server and two engines ("pc" and "phone") syncing temp folders.

use std::{path::PathBuf, sync::Arc, time::Duration};

use savesync_engine::{
    Engine, EngineConfig, EngineError, EngineEvent, FsProvider, GameConfig, ImportPolicy, Resolution,
};
use savesync_server::{build, config::Config};
use tempfile::TempDir;
use tokio::sync::broadcast;

const ENROLL_KEY: &str = "test-enroll-key-0123456789";
const GAME: &str = "my-game";
const SAVE: &str = "game.sav";
/// Nothing listens here, so it behaves like having no connection.
const OFFLINE_URL: &str = "http://127.0.0.1:9";

async fn start_server() -> (String, TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: tmp.path().to_path_buf(),
        enroll_key: ENROLL_KEY.into(),
        keep_versions: 20,
        max_file_bytes: 16 * 1024 * 1024,
        push_rewrite: None,
    };
    let (app, _) = build(config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), tmp)
}

struct Device {
    engine: Engine,
    saves: PathBuf,
    events: broadcast::Receiver<EngineEvent>,
    _tmp: TempDir,
}

impl Device {
    async fn new(server: &str, name: &str, policy: ImportPolicy) -> Self {
        Self::with_game(server, name, policy, |_| {}).await
    }

    async fn with_game(server: &str, name: &str, policy: ImportPolicy, tweak: impl FnOnce(&mut GameConfig)) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let saves = tmp.path().join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        let mut config = EngineConfig::new(tmp.path().join("engine"), name, "test");
        config.settle_delay = Duration::from_millis(100);
        config.quiet_period = Duration::from_millis(200);
        let engine = Engine::open(config, Arc::new(FsProvider)).unwrap();
        let events = engine.events();
        engine.pair(server, ENROLL_KEY).await.unwrap();
        let mut game = GameConfig {
            id: GAME.into(),
            name: "My Game".into(),
            location: saves.to_string_lossy().into_owned(),
            include: vec![],
            ignore: vec![],
            processes: vec![],
            import_policy: policy,
        };
        tweak(&mut game);
        engine.add_game(game).unwrap();
        Self { engine, saves, events, _tmp: tmp }
    }

    fn write(&self, path: &str, contents: &str) {
        let p = self.saves.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.saves.join(path)).ok()
    }

    fn view(&self) -> savesync_engine::GameView {
        self.engine.game(GAME).unwrap()
    }

    fn drain(&mut self) -> Vec<EngineEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            out.push(e);
        }
        out
    }
}

async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test]
async fn upload_then_auto_import() {
    let (url, _s) = start_server().await;
    let pc = Device::new(&url, "PC", ImportPolicy::Ask).await;
    let phone = Device::new(&url, "Phone", ImportPolicy::AutoWhenSafe).await;

    pc.write(SAVE, "badges: 3");
    pc.write(".DS_Store", "junk");
    pc.engine.sync_now().await.unwrap();
    assert_eq!(pc.view().base_version, 1);
    assert!(pc.view().queued_upload.is_none());

    phone.engine.sync_now().await.unwrap();
    assert_eq!(phone.read(SAVE).as_deref(), Some("badges: 3"));
    assert_eq!(phone.read(".DS_Store"), None, "junk isn't synced");
    let v = phone.view();
    assert_eq!(v.base_version, 1);
    assert!(v.staged.is_none());

    // Nothing changed, so syncing again uploads nothing.
    phone.engine.sync_now().await.unwrap();
    pc.engine.sync_now().await.unwrap();
    assert_eq!(pc.view().base_version, 1);
}

#[tokio::test]
async fn ask_policy_prompts_backs_up_and_restores() {
    let (url, _s) = start_server().await;
    let pc = Device::new(&url, "PC", ImportPolicy::Ask).await;
    let mut phone = Device::new(&url, "Phone", ImportPolicy::Ask).await;

    pc.write(SAVE, "v1");
    pc.engine.sync_now().await.unwrap();
    phone.engine.sync_now().await.unwrap();

    // Downloaded and staged, but the folder isn't touched until the user says so.
    assert_eq!(phone.read(SAVE), None);
    let staged = phone.view().staged.expect("staged");
    assert_eq!(staged.version, 1);
    assert_eq!(staged.device_name.as_deref(), Some("PC"));
    let prompts: Vec<_> = phone.drain().into_iter().filter(|e| matches!(e, EngineEvent::ImportAvailable { .. })).collect();
    assert_eq!(prompts.len(), 1);

    // Syncing again doesn't prompt again for the same version.
    phone.engine.sync_now().await.unwrap();
    assert!(!phone.drain().iter().any(|e| matches!(e, EngineEvent::ImportAvailable { .. })));

    phone.engine.import(GAME).await.unwrap();
    assert_eq!(phone.read(SAVE).as_deref(), Some("v1"));
    assert!(phone.engine.backups(GAME).unwrap().is_empty(), "nothing to back up in an empty folder");

    pc.write(SAVE, "v2");
    pc.engine.sync_now().await.unwrap();
    phone.engine.sync_now().await.unwrap();
    phone.engine.import(GAME).await.unwrap();
    assert_eq!(phone.read(SAVE).as_deref(), Some("v2"));
    let backups = phone.engine.backups(GAME).unwrap();
    assert_eq!(backups.len(), 1);
    assert_eq!(backups[0].reason, "before importing v2");

    // Restoring puts v1 back and syncs it as the newest version.
    phone.engine.restore_backup(backups[0].id).await.unwrap();
    assert_eq!(phone.read(SAVE).as_deref(), Some("v1"));
    assert_eq!(phone.view().base_version, 3);
    pc.engine.sync_now().await.unwrap();
    assert_eq!(pc.view().staged.unwrap().version, 3);
}

#[tokio::test]
async fn offline_session_is_queued_then_uploaded() {
    let (url, _s) = start_server().await;
    let pc = Device::new(&url, "PC", ImportPolicy::Ask).await;
    let phone = Device::new(&url, "Phone", ImportPolicy::Ask).await;
    pc.engine.sync_now().await.unwrap(); // registers the game while online

    pc.engine.set_server_url(OFFLINE_URL).unwrap();
    pc.write(SAVE, "played on a plane");
    pc.engine.sync_now().await.unwrap();
    let queued = pc.view().queued_upload.expect("queued while offline");
    assert!(!queued.blocked);
    assert!(!pc.engine.is_online());

    pc.engine.set_server_url(&url).unwrap();
    pc.engine.sync_now().await.unwrap();
    assert!(pc.engine.is_online());
    assert!(pc.view().queued_upload.is_none());
    assert_eq!(pc.view().base_version, 1);

    // The version records when the session ended, not when it finally uploaded.
    phone.engine.sync_now().await.unwrap();
    let staged = phone.view().staged.expect("staged");
    let mtime = std::fs::metadata(pc.saves.join(SAVE)).unwrap().modified().unwrap();
    let mtime_ms = mtime.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    assert_eq!(staged.played_at, Some(mtime_ms));
    phone.engine.import(GAME).await.unwrap();
    assert_eq!(phone.read(SAVE).as_deref(), Some("played on a plane"));
}

async fn diverged(url: &str) -> (Device, Device) {
    let pc = Device::new(url, "PC", ImportPolicy::AutoWhenSafe).await;
    let phone = Device::new(url, "Phone", ImportPolicy::AutoWhenSafe).await;
    pc.write(SAVE, "v1");
    pc.engine.sync_now().await.unwrap();
    phone.engine.sync_now().await.unwrap();
    assert_eq!(phone.read(SAVE).as_deref(), Some("v1"));

    // Phone plays offline while the PC plays and uploads.
    phone.engine.set_server_url(OFFLINE_URL).unwrap();
    phone.write(SAVE, "phone progress");
    phone.engine.sync_now().await.unwrap();
    pc.write(SAVE, "pc progress");
    pc.engine.sync_now().await.unwrap();
    assert_eq!(pc.view().base_version, 2);

    phone.engine.set_server_url(url).unwrap();
    phone.engine.sync_now().await.unwrap();
    (pc, phone)
}

#[tokio::test]
async fn conflict_keep_remote() {
    let (url, _s) = start_server().await;
    let (_pc, mut phone) = diverged(&url).await;

    let v = phone.view();
    let conflict = v.conflict.expect("conflict detected");
    assert_eq!(conflict.remote.version, 2);
    assert_eq!(conflict.remote.device_name.as_deref(), Some("PC"));
    assert!(v.queued_upload.unwrap().blocked);
    assert_eq!(phone.read(SAVE).as_deref(), Some("phone progress"), "nothing overwritten yet");
    assert!(phone.drain().iter().any(|e| matches!(e, EngineEvent::Conflict { .. })));
    assert!(matches!(phone.engine.import(GAME).await, Err(EngineError::Invalid(_))));

    phone.engine.resolve_conflict(GAME, Resolution::KeepRemote).await.unwrap();
    assert_eq!(phone.read(SAVE).as_deref(), Some("pc progress"));
    let v = phone.view();
    assert_eq!(v.base_version, 2);
    assert!(v.conflict.is_none() && v.queued_upload.is_none() && v.staged.is_none());
    let backups = phone.engine.backups(GAME).unwrap();
    assert_eq!(backups.len(), 1, "the phone's save was backed up before being replaced");
}

#[tokio::test]
async fn conflict_keep_local() {
    let (url, _s) = start_server().await;
    let (pc, phone) = diverged(&url).await;

    phone.engine.resolve_conflict(GAME, Resolution::KeepLocal).await.unwrap();
    assert_eq!(phone.read(SAVE).as_deref(), Some("phone progress"));
    let v = phone.view();
    assert_eq!(v.base_version, 3, "uploaded on top of the PC's version");
    assert!(v.conflict.is_none() && v.queued_upload.is_none());

    pc.engine.sync_now().await.unwrap();
    assert_eq!(pc.read(SAVE).as_deref(), Some("phone progress"));
    assert_eq!(pc.view().base_version, 3);
}

#[tokio::test]
async fn sessions_hold_imports_and_upload_on_exit() {
    let (url, _s) = start_server().await;
    let pc = Device::new(&url, "PC", ImportPolicy::Ask).await;
    let phone = Device::new(&url, "Phone", ImportPolicy::AutoWhenSafe).await;

    // An incoming save waits while the emulator is open, even with auto-import.
    phone.engine.session_started(GAME);
    pc.write(SAVE, "v1");
    pc.engine.sync_now().await.unwrap();
    phone.engine.sync_now().await.unwrap();
    assert_eq!(phone.view().staged.as_ref().map(|s| s.version), Some(1));
    assert_eq!(phone.read(SAVE), None);
    assert!(matches!(phone.engine.import(GAME).await, Err(EngineError::GameRunning(_))));

    phone.engine.session_ended(GAME);
    eventually("phone imports after the session", || phone.read(SAVE).as_deref() == Some("v1")).await;

    // Changes made during a session upload when it ends, not before.
    pc.engine.session_started(GAME);
    pc.write(SAVE, "v2 from a long session");
    pc.engine.sync_now().await.unwrap();
    assert_eq!(pc.view().base_version, 1, "not uploaded mid-session");
    pc.engine.session_ended(GAME);
    eventually("pc uploads after the session", || pc.view().base_version == 2).await;
}

#[tokio::test]
async fn playing_during_an_incoming_save_is_a_conflict() {
    let (url, _s) = start_server().await;
    let pc = Device::new(&url, "PC", ImportPolicy::AutoWhenSafe).await;
    let phone = Device::new(&url, "Phone", ImportPolicy::AutoWhenSafe).await;
    pc.write(SAVE, "v1");
    pc.engine.sync_now().await.unwrap();
    phone.engine.sync_now().await.unwrap();

    phone.engine.session_started(GAME);
    phone.write(SAVE, "phone mid-session");
    pc.write(SAVE, "pc v2");
    pc.engine.sync_now().await.unwrap();
    phone.engine.sync_now().await.unwrap();
    assert!(phone.view().conflict.is_none(), "decided only once the session ends");

    phone.engine.session_ended(GAME);
    eventually("conflict after session", || phone.view().conflict.is_some()).await;
    assert_eq!(phone.read(SAVE).as_deref(), Some("phone mid-session"));
}

#[tokio::test]
async fn subfolders_and_deletions() {
    let (url, _s) = start_server().await;
    let pc = Device::new(&url, "PC", ImportPolicy::Ask).await;
    let phone = Device::new(&url, "Phone", ImportPolicy::AutoWhenSafe).await;

    pc.write("memcards/Mcd001.ps2", "card");
    pc.write("old.sav", "stale");
    pc.engine.sync_now().await.unwrap();
    phone.engine.sync_now().await.unwrap();
    assert_eq!(phone.read("memcards/Mcd001.ps2").as_deref(), Some("card"));
    assert_eq!(phone.read("old.sav").as_deref(), Some("stale"));

    std::fs::remove_file(pc.saves.join("old.sav")).unwrap();
    pc.engine.sync_now().await.unwrap();
    phone.engine.sync_now().await.unwrap();
    assert_eq!(phone.read("old.sav"), None, "deletions sync too");
    assert_eq!(phone.read("memcards/Mcd001.ps2").as_deref(), Some("card"));
}

#[tokio::test]
async fn shared_folder_with_include_patterns() {
    let (url, _s) = start_server().await;
    let only_game_a = |g: &mut GameConfig| g.include = vec!["GameA.*".into()];
    let pc = Device::with_game(&url, "PC", ImportPolicy::Ask, only_game_a).await;
    let phone = Device::with_game(&url, "Phone", ImportPolicy::AutoWhenSafe, only_game_a).await;

    pc.write("GameA.sav", "game a");
    pc.write("GameB.sav", "pc game b");
    phone.write("GameB.sav", "phone game b");
    pc.engine.sync_now().await.unwrap();
    phone.engine.sync_now().await.unwrap();

    assert_eq!(phone.read("GameA.sav").as_deref(), Some("game a"));
    assert_eq!(phone.read("GameB.sav").as_deref(), Some("phone game b"), "other games' files are left alone");
}

#[tokio::test]
async fn missing_folder_is_reported_not_uploaded() {
    let (url, _s) = start_server().await;
    let pc = Device::new(&url, "PC", ImportPolicy::Ask).await;
    pc.write(SAVE, "v1");
    pc.engine.sync_now().await.unwrap();

    std::fs::remove_dir_all(&pc.saves).unwrap();
    pc.engine.sync_now().await.unwrap();
    let v = pc.view();
    assert!(v.last_error.unwrap().contains("not found"));
    assert_eq!(v.base_version, 1);
    assert!(v.queued_upload.is_none(), "a missing folder never uploads as an empty save");
}

#[tokio::test]
async fn realtime_over_websocket() {
    let (url, _s) = start_server().await;
    let pc = Device::new(&url, "PC", ImportPolicy::Ask).await;
    let phone = Device::new(&url, "Phone", ImportPolicy::AutoWhenSafe).await;
    let pc_engine = pc.engine.clone();
    let phone_engine = phone.engine.clone();
    tokio::spawn(async move { pc_engine.run().await });
    tokio::spawn(async move { phone_engine.run().await });
    eventually("both online", || pc.engine.is_online() && phone.engine.is_online()).await;

    // A folder change (as reported by the desktop watcher) uploads after a quiet
    // period, and the phone gets it over the socket with no polling.
    pc.write(SAVE, "live");
    pc.engine.local_change(GAME);
    eventually("phone receives it live", || phone.read(SAVE).as_deref() == Some("live")).await;
}
