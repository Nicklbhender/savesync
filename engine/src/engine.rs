use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures_util::StreamExt;
use globset::{Glob, GlobSet, GlobSetBuilder};
use savesync_protocol::{
    AckState, CommitRequest, FileEntry, Manifest, PushPayload, ServerMsg, same_contents, valid_game_id, valid_save_path,
    valid_sha256,
};
use tokio::sync::{Notify, broadcast};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    cache::BlobCache,
    client::{Client, CommitOutcome, WsStream},
    error::{EngineError, Result},
    state::{GameRow, OutboxRow, State, keys},
    storage::{StorageProvider, TEMP_PREFIX},
    types::{
        BackupInfo, ConflictInfo, EngineEvent, GameConfig, GameView, ImportPolicy, QueuedUpload, Resolution, ServerInfo,
    },
};

/// Junk that's never synced, on top of each game's own ignore patterns.
const ALWAYS_IGNORE: &[&str] = &["**/.DS_Store", "**/._*", "**/Thumbs.db", "**/desktop.ini", "**/*.tmp", "**/*~"];

/// The server pings every 30s; this long without hearing anything means the connection is dead.
const WS_SILENCE_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Where the engine keeps its database and file cache.
    pub data_dir: PathBuf,
    /// Shown to other devices, e.g. "Gaming PC".
    pub device_name: String,
    /// "windows", "macos" or "android".
    pub platform: String,
    /// Wait this long after a game closes before capturing its save, so the
    /// emulator can finish writing.
    pub settle_delay: Duration,
    /// After a file change with no game session, wait for this much quiet first.
    pub quiet_period: Duration,
    /// Full sync this often even without any signal (a safety net).
    pub periodic_interval: Duration,
    /// Local backups kept per game (made before every import).
    pub keep_backups: usize,
}

impl EngineConfig {
    pub fn new(data_dir: impl Into<PathBuf>, device_name: impl Into<String>, platform: impl Into<String>) -> Self {
        Self {
            data_dir: data_dir.into(),
            device_name: device_name.into(),
            platform: platform.into(),
            settle_delay: Duration::from_secs(3),
            quiet_period: Duration::from_secs(10),
            periodic_interval: Duration::from_secs(5 * 60),
            keep_backups: 5,
        }
    }
}

/// The sync engine. Cheap to clone; all clones share one engine.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    config: EngineConfig,
    state: State,
    cache: BlobCache,
    storage: Arc<dyn StorageProvider>,
    events: broadcast::Sender<EngineEvent>,
    /// Active session count per game (several processes can map to one game).
    sessions: Mutex<HashMap<String, u32>>,
    /// Bumped on every local change; a debounced capture only runs if it's unchanged.
    change_seq: Mutex<HashMap<String, u64>>,
    /// Serializes everything that reads or changes sync state.
    op: tokio::sync::Mutex<()>,
    wake: Notify,
    online: AtomicBool,
}

fn build_globs(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(Glob::new(p).map_err(|e| EngineError::Invalid(format!("pattern {p:?}: {e}")))?);
    }
    builder.build().map_err(|e| EngineError::Invalid(e.to_string()))
}

fn build_ignore(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for p in ALWAYS_IGNORE.iter().copied().chain(patterns.iter().map(String::as_str)) {
        builder.add(Glob::new(p).map_err(|e| EngineError::Invalid(format!("ignore pattern {p:?}: {e}")))?);
    }
    builder.add(Glob::new(&format!("**/{TEMP_PREFIX}*")).expect("valid glob"));
    builder.build().map_err(|e| EngineError::Invalid(e.to_string()))
}

/// File lists from the server are checked before anything is downloaded or written:
/// a hostile server (or someone tampering with plain-HTTP traffic) must not be able
/// to make the app write outside the save folder or the cache.
fn validate_files(files: &[FileEntry]) -> Result<()> {
    let mut seen = HashSet::new();
    for f in files {
        if !valid_save_path(&f.path) || !valid_sha256(&f.sha256) || f.size < 0 || !seen.insert(f.path.as_str()) {
            return Err(EngineError::Other(format!("rejected an invalid file entry from the server: {:?}", f.path)));
        }
    }
    Ok(())
}

fn latest_mtime(files: &[FileEntry]) -> Option<i64> {
    files.iter().filter_map(|f| f.mtime).max()
}

impl Engine {
    pub fn open(config: EngineConfig, storage: Arc<dyn StorageProvider>) -> Result<Self> {
        // reqwest is built without a bundled TLS backend; use ring.
        let _ = rustls::crypto::ring::default_provider().install_default();
        std::fs::create_dir_all(&config.data_dir)?;
        let state = State::open(&config.data_dir.join("engine.db"))?;
        let cache = BlobCache::open(&config.data_dir.join("cache"))?;
        let (events, _) = broadcast::channel(256);
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                state,
                cache,
                storage,
                events,
                sessions: Mutex::default(),
                change_seq: Mutex::default(),
                op: tokio::sync::Mutex::new(()),
                wake: Notify::new(),
                online: AtomicBool::new(false),
            }),
        })
    }

    pub fn events(&self) -> broadcast::Receiver<EngineEvent> {
        self.inner.events.subscribe()
    }

    fn emit(&self, event: EngineEvent) {
        let _ = self.inner.events.send(event);
    }

    pub fn is_online(&self) -> bool {
        self.inner.online.load(Ordering::Relaxed)
    }

    fn set_online(&self, online: bool) {
        if self.inner.online.swap(online, Ordering::Relaxed) != online {
            self.emit(EngineEvent::Connection { online });
        }
    }

    // ------------------------------------------------------------ pairing

    /// Registers this device with the server using the enroll key.
    pub async fn pair(&self, server_url: &str, enroll_key: &str) -> Result<ServerInfo> {
        let name = self.inner.config.device_name.clone();
        self.pair_as(server_url, enroll_key, &name).await
    }

    /// Like [`pair`](Self::pair), with a device name chosen by the user.
    pub async fn pair_as(&self, server_url: &str, enroll_key: &str, device_name: &str) -> Result<ServerInfo> {
        let name = device_name.trim();
        if name.is_empty() {
            return Err(EngineError::Invalid("device name is required".into()));
        }
        let resp = Client::enroll(server_url, enroll_key, name, &self.inner.config.platform).await?;
        let st = &self.inner.state;
        let url = server_url.trim().trim_end_matches('/');
        st.set_setting(keys::SERVER_URL, Some(url))?;
        st.set_setting(keys::DEVICE_ID, Some(&resp.device_id))?;
        st.set_setting(keys::TOKEN, Some(&resp.token))?;
        st.set_setting(keys::DEVICE_NAME, Some(name))?;
        // Each game must be registered under the new identity.
        st.set_registered_all(false)?;
        self.nudge();
        Ok(ServerInfo { url: url.into(), device_id: resp.device_id, device_name: name.into() })
    }

    pub fn server(&self) -> Result<Option<ServerInfo>> {
        let st = &self.inner.state;
        match (st.setting(keys::SERVER_URL)?, st.setting(keys::DEVICE_ID)?, st.setting(keys::TOKEN)?) {
            (Some(url), Some(device_id), Some(_)) => Ok(Some(ServerInfo {
                url,
                device_id,
                device_name: st.setting(keys::DEVICE_NAME)?.unwrap_or_default(),
            })),
            _ => Ok(None),
        }
    }

    /// Points at a different address for the same server (e.g. the NAS got a new IP).
    pub fn set_server_url(&self, url: &str) -> Result<()> {
        self.inner.state.set_setting(keys::SERVER_URL, Some(url.trim().trim_end_matches('/')))?;
        self.nudge();
        Ok(())
    }

    /// Forgets the server. Local games and saves are kept.
    pub fn unpair(&self) -> Result<()> {
        let st = &self.inner.state;
        for key in [keys::SERVER_URL, keys::DEVICE_ID, keys::TOKEN] {
            st.set_setting(key, None)?;
        }
        st.set_registered_all(false)?;
        self.set_online(false);
        Ok(())
    }

    /// Android: the UnifiedPush endpoint from the distributor (e.g. ntfy).
    pub async fn set_push_endpoint(&self, endpoint: Option<String>) -> Result<()> {
        self.client()?.set_push_endpoint(endpoint).await
    }

    fn client(&self) -> Result<Client> {
        let st = &self.inner.state;
        match (st.setting(keys::SERVER_URL)?, st.setting(keys::TOKEN)?) {
            (Some(url), Some(token)) => Ok(Client::new(&url, &token)),
            _ => Err(EngineError::NotPaired),
        }
    }

    // ------------------------------------------------------------ games

    fn validate(&self, config: &GameConfig) -> Result<()> {
        if !valid_game_id(&config.id) {
            return Err(EngineError::Invalid(
                "Save ID must be 1-64 lowercase letters, numbers, dashes, underscores or dots (no spaces), starting with a letter or number".into(),
            ));
        }
        if config.name.trim().is_empty() {
            return Err(EngineError::Invalid("game name is required".into()));
        }
        build_ignore(&config.ignore)?;
        build_globs(&config.include)?;
        self.inner
            .storage
            .open(&config.location)?
            .list()
            .map_err(|e| EngineError::Invalid(format!("can't read save folder: {e}")))?;
        Ok(())
    }

    pub fn add_game(&self, config: GameConfig) -> Result<GameView> {
        self.validate(&config)?;
        self.inner.state.insert_game(&config)?;
        self.emit(EngineEvent::GamesChanged);
        self.nudge();
        self.game(&config.id)
    }

    pub fn update_game(&self, config: GameConfig) -> Result<GameView> {
        self.validate(&config)?;
        self.inner.state.update_config(&config)?;
        self.emit(EngineEvent::GamesChanged);
        self.nudge();
        self.game(&config.id)
    }

    /// Stops syncing a game on this device. Its save folder is left untouched.
    pub async fn remove_game(&self, id: &str) -> Result<()> {
        let _op = self.inner.op.lock().await;
        self.require_game(id)?;
        if let Ok(client) = self.client()
            && let Err(e) = client.unsubscribe(id).await
        {
            tracing::warn!("couldn't unsubscribe {id} on the server: {e}");
        }
        self.inner.state.delete_game(id)?;
        self.emit(EngineEvent::GamesChanged);
        Ok(())
    }

    pub fn games(&self) -> Result<Vec<GameView>> {
        self.inner.state.games()?.into_iter().map(|g| self.view(g)).collect()
    }

    pub fn game(&self, id: &str) -> Result<GameView> {
        self.view(self.require_game(id)?)
    }

    fn require_game(&self, id: &str) -> Result<GameRow> {
        self.inner.state.game(id)?.ok_or_else(|| EngineError::UnknownGame(id.into()))
    }

    fn view(&self, g: GameRow) -> Result<GameView> {
        let queued_upload = self.inner.state.outbox(&g.config.id)?.map(|o| QueuedUpload {
            played_at: o.played_at,
            created_at: o.created_at,
            blocked: o.blocked,
        });
        Ok(GameView {
            in_session: self.in_session(&g.config.id),
            base_version: g.base_version,
            registered: g.registered,
            queued_upload,
            staged: g.staged.map(|m| m.info),
            conflict: g.conflict,
            last_error: g.last_error,
            config: g.config,
        })
    }

    pub fn backups(&self, id: &str) -> Result<Vec<BackupInfo>> {
        Ok(self.inner.state.backups(id)?.into_iter().map(|b| b.info).collect())
    }

    // ------------------------------------------------------------ platform signals

    /// The game's emulator started (desktop process watcher, or Android foreground events).
    pub fn session_started(&self, id: &str) {
        let first = {
            let mut sessions = self.inner.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let count = sessions.entry(id.to_string()).or_default();
            *count += 1;
            *count == 1
        };
        if first {
            self.emit(EngineEvent::SessionStarted { game_id: id.into() });
        }
    }

    /// The game's emulator closed (or left the foreground on Android). After a short
    /// settle delay, the save is captured and uploaded, and held downloads are offered.
    pub fn session_ended(&self, id: &str) {
        let ended = {
            let mut sessions = self.inner.sessions.lock().unwrap_or_else(|p| p.into_inner());
            match sessions.get_mut(id) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => {
                    sessions.remove(id);
                    true
                }
                None => false,
            }
        };
        if !ended {
            return;
        }
        self.emit(EngineEvent::SessionEnded { game_id: id.into() });
        let engine = self.clone();
        let id = id.to_string();
        let played_at = now_ms();
        tokio::spawn(async move {
            tokio::time::sleep(engine.inner.config.settle_delay).await;
            if !engine.in_session(&id) {
                engine.sync_game(&id, Some(played_at)).await;
            }
        });
    }

    pub fn in_session(&self, id: &str) -> bool {
        self.inner.sessions.lock().unwrap_or_else(|p| p.into_inner()).contains_key(id)
    }

    /// Something in the game's save folder changed (desktop folder watcher). Captured
    /// once the folder has been quiet for a while, unless a session is active, in
    /// which case the session end handles it.
    pub fn local_change(&self, id: &str) {
        let seq = {
            let mut seqs = self.inner.change_seq.lock().unwrap_or_else(|p| p.into_inner());
            let s = seqs.entry(id.to_string()).or_default();
            *s += 1;
            *s
        };
        let engine = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(engine.inner.config.quiet_period).await;
            let latest = engine.inner.change_seq.lock().unwrap_or_else(|p| p.into_inner()).get(&id).copied();
            if latest == Some(seq) && !engine.in_session(&id) {
                engine.sync_game(&id, None).await;
            }
        });
    }

    /// Something happened that's worth checking the server for: woke from sleep,
    /// network came back, screen unlocked, frontend opened.
    pub fn nudge(&self) {
        self.inner.wake.notify_one();
    }

    /// A UnifiedPush message arrived (Android).
    pub fn handle_push(&self, payload: &str) {
        match serde_json::from_str::<PushPayload>(payload) {
            Ok(p) => tracing::debug!("push: {} v{}", p.game_id, p.version),
            Err(e) => tracing::debug!("unrecognized push payload ({e}); syncing anyway"),
        }
        self.nudge();
    }

    // ------------------------------------------------------------ user actions

    /// Captures local changes, uploads queued saves and downloads new ones.
    pub async fn sync_now(&self) -> Result<()> {
        self.sync_all().await
    }

    /// Imports the downloaded save from another device. The current local save is backed up first.
    pub async fn import(&self, id: &str) -> Result<()> {
        let _op = self.inner.op.lock().await;
        let game = self.require_game(id)?;
        if game.conflict.is_some() {
            return Err(EngineError::Invalid("this game has a conflict; resolve it instead".into()));
        }
        let staged = game.staged.ok_or(EngineError::NothingTo("import"))?;
        self.import_staged(id, staged).await
    }

    /// Declines the downloaded save and makes this device's save the newest version everywhere.
    pub async fn keep_local(&self, id: &str) -> Result<()> {
        let _op = self.inner.op.lock().await;
        let staged = self.require_game(id)?.staged.ok_or(EngineError::NothingTo("decline"))?;
        self.override_with_local(id, staged).await
    }

    pub async fn resolve_conflict(&self, id: &str, resolution: Resolution) -> Result<()> {
        let _op = self.inner.op.lock().await;
        let game = self.require_game(id)?;
        let conflict = game.conflict.clone().ok_or(EngineError::NothingTo("resolve"))?;
        let remote = match game.staged {
            Some(m) => m,
            None => {
                let client = self.client()?;
                let m = client.manifest(id, conflict.remote.version).await?;
                self.download(&client, &m).await?;
                m
            }
        };
        match resolution {
            Resolution::KeepRemote => self.import_staged(id, remote).await,
            Resolution::KeepLocal => self.override_with_local(id, remote).await,
        }
    }

    /// Puts a backup back into the save folder; it then syncs as the newest version.
    pub async fn restore_backup(&self, backup_id: i64) -> Result<()> {
        let _op = self.inner.op.lock().await;
        let backup = self.inner.state.backup(backup_id)?.ok_or(EngineError::NothingTo("restore"))?;
        let id = backup.info.game_id.clone();
        self.apply(&id, backup.files, "before restoring a backup".into()).await?;
        self.snapshot(&id, None).await?;
        if let Ok(client) = self.client() {
            self.ignore_offline(self.flush_outbox(&client).await)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ main loop

    /// Runs forever: keeps a WebSocket open for instant notices, reconnects with
    /// backoff, and syncs on connect, on [`nudge`](Self::nudge), and periodically.
    pub async fn run(&self) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let client = match self.client() {
                Ok(c) => c,
                Err(_) => {
                    self.wait(Duration::from_secs(30)).await;
                    continue;
                }
            };
            if let Err(e) = self.sync_all().await {
                self.report(None, &e);
            }
            match client.connect_ws().await {
                Ok(ws) => {
                    self.set_online(true);
                    backoff = Duration::from_secs(1);
                    self.ws_session(&client, ws).await;
                }
                Err(e) => {
                    if !e.is_offline() {
                        self.report(None, &e);
                    }
                    self.set_online(false);
                }
            }
            self.wait(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    async fn wait(&self, d: Duration) {
        tokio::select! {
            _ = tokio::time::sleep(d) => {}
            _ = self.inner.wake.notified() => {}
        }
    }

    async fn ws_session(&self, client: &Client, mut ws: WsStream) {
        let mut periodic = tokio::time::interval(self.inner.config.periodic_interval);
        periodic.tick().await;
        loop {
            tokio::select! {
                msg = tokio::time::timeout(WS_SILENCE_TIMEOUT, ws.next()) => match msg {
                    Ok(Some(Ok(Message::Text(text)))) => self.handle_server_msg(client, text.as_str()).await,
                    Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) | Err(_) => break,
                    Ok(Some(Ok(_))) => {}
                },
                _ = periodic.tick() => self.sync_logged().await,
                _ = self.inner.wake.notified() => self.sync_logged().await,
            }
        }
        tracing::debug!("websocket closed");
    }

    async fn handle_server_msg(&self, client: &Client, text: &str) {
        match serde_json::from_str::<ServerMsg>(text) {
            // Lists what we missed; a full sync picks it up and flushes queued uploads.
            Ok(ServerMsg::Hello { .. }) => self.sync_logged().await,
            Ok(ServerMsg::VersionAvailable { manifest, .. }) => {
                let id = manifest.info.game_id.clone();
                let _op = self.inner.op.lock().await;
                let result = async {
                    self.receive(client, manifest).await?;
                    self.evaluate_staged(&id).await
                }
                .await;
                if let Err(e) = self.per_game(&id, result) {
                    self.report(None, &e);
                }
            }
            Ok(ServerMsg::Acked { .. }) => {}
            Ok(ServerMsg::Error { message }) => tracing::warn!("server: {message}"),
            Err(e) => tracing::warn!("unrecognized server message: {e}"),
        }
    }

    async fn sync_logged(&self) {
        if let Err(e) = self.sync_all().await {
            self.report(None, &e);
        }
    }

    // ------------------------------------------------------------ sync passes

    async fn sync_all(&self) -> Result<()> {
        let _op = self.inner.op.lock().await;
        // Capturing works offline: sessions that end without a connection queue up here.
        for g in self.inner.state.games()? {
            let id = g.config.id;
            if !self.in_session(&id) {
                let r = self.snapshot(&id, None).await.map(|_| ());
                self.per_game(&id, r)?;
            }
        }
        let client = match self.client() {
            Ok(c) => c,
            Err(EngineError::NotPaired) => return Ok(()),
            Err(e) => return Err(e),
        };
        let result = async {
            self.register_games(&client).await?;
            self.flush_outbox(&client).await?;
            self.pull_pending(&client).await?;
            for g in self.inner.state.games()? {
                if g.staged.is_some() {
                    let r = self.evaluate_staged(&g.config.id).await;
                    self.per_game(&g.config.id, r)?;
                }
            }
            Ok(())
        }
        .await;
        if result.is_ok() {
            self.set_online(true);
        }
        self.collect_garbage().await;
        self.ignore_offline(result)
    }

    /// Capture + upload for one game; used after sessions end and after quiet periods.
    async fn sync_game(&self, id: &str, played_at: Option<i64>) {
        let _op = self.inner.op.lock().await;
        let result = async {
            self.snapshot(id, played_at).await?;
            let Ok(client) = self.client() else { return Ok(()) };
            self.ignore_offline(self.flush_outbox(&client).await)?;
            self.evaluate_staged(id).await
        }
        .await;
        if let Err(e) = self.per_game(id, result) {
            self.report(None, &e);
        }
    }

    fn ignore_offline(&self, result: Result<()>) -> Result<()> {
        match result {
            Err(e) if e.is_offline() => {
                self.set_online(false);
                Ok(())
            }
            other => other,
        }
    }

    /// Records a per-game failure and carries on, except for failures that affect
    /// everything (offline, unauthorized), which are passed up.
    fn per_game(&self, id: &str, result: Result<()>) -> Result<()> {
        match result {
            Ok(()) => Ok(()),
            Err(e @ (EngineError::Offline(_) | EngineError::Unauthorized | EngineError::NotPaired)) => Err(e),
            Err(e) => {
                if let Ok(Some(mut g)) = self.inner.state.game(id) {
                    g.last_error = Some(e.to_string());
                    let _ = self.inner.state.save_game(&g);
                }
                self.report(Some(id), &e);
                Ok(())
            }
        }
    }

    fn report(&self, game_id: Option<&str>, e: &EngineError) {
        tracing::warn!("{}: {e}", game_id.unwrap_or("sync"));
        self.emit(EngineEvent::Error { game_id: game_id.map(Into::into), message: e.to_string() });
    }

    async fn collect_garbage(&self) {
        let inner = self.inner.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(keep) = inner.state.referenced_blobs() {
                inner.cache.retain(&keep);
            }
        })
        .await;
    }

    async fn register_games(&self, client: &Client) -> Result<()> {
        for mut g in self.inner.state.games()? {
            if g.registered {
                continue;
            }
            let id = g.config.id.clone();
            let r = async {
                client.subscribe(&id, &g.config.name).await?;
                g.registered = true;
                self.inner.state.save_game(&g)
            }
            .await;
            self.per_game(&id, r)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ capturing

    /// Hashes the save folder, copying changed files into the cache.
    async fn capture(&self, config: &GameConfig) -> Result<Vec<FileEntry>> {
        let inner = self.inner.clone();
        let config = config.clone();
        tokio::task::spawn_blocking(move || inner.capture_blocking(&config))
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?
    }

    /// Captures the save folder and queues it for upload if it changed since the last sync.
    async fn snapshot(&self, id: &str, played_at: Option<i64>) -> Result<bool> {
        let game = self.require_game(id)?;
        let files = self.capture(&game.config).await?;
        if files.is_empty() {
            // Never upload an empty save: far more likely a missing card or a bad
            // path than the user deliberately deleting everything.
            return Ok(false);
        }
        let st = &self.inner.state;
        let outbox = st.outbox(id)?;
        if same_contents(&files, &game.base_files) {
            if outbox.as_ref().is_some_and(|o| !o.blocked) {
                st.delete_outbox(id)?;
            }
            return Ok(false);
        }
        if outbox.as_ref().is_some_and(|o| same_contents(&o.files, &files)) {
            return Ok(false);
        }
        st.put_outbox(&OutboxRow {
            game_id: id.into(),
            base_version: game.base_version,
            played_at: played_at.or_else(|| latest_mtime(&files)),
            files,
            created_at: now_ms(),
            blocked: outbox.is_some_and(|o| o.blocked),
        })?;
        self.emit(EngineEvent::Queued { game_id: id.into() });
        Ok(true)
    }

    // ------------------------------------------------------------ uploading

    async fn flush_outbox(&self, client: &Client) -> Result<()> {
        for row in self.inner.state.outbox_all()? {
            if !row.blocked {
                let id = row.game_id.clone();
                let r = self.upload(client, row).await;
                self.per_game(&id, r)?;
            }
        }
        Ok(())
    }

    async fn upload_blobs(&self, client: &Client, shas: &[String]) -> Result<()> {
        for sha in shas {
            client.upload_blob(sha, &self.inner.cache.path(sha)).await?;
        }
        Ok(())
    }

    async fn upload(&self, client: &Client, mut row: OutboxRow) -> Result<()> {
        let id = row.game_id.clone();
        if !self.require_game(&id)?.registered {
            return Ok(());
        }
        let mut shas: Vec<String> = row.files.iter().map(|f| f.sha256.clone()).collect();
        shas.sort();
        shas.dedup();
        let missing = client.missing(shas).await?;
        self.upload_blobs(client, &missing).await?;

        let req = CommitRequest {
            base_version: row.base_version,
            files: row.files.clone(),
            played_at: row.played_at,
            note: None,
        };
        let mut outcome = client.commit(&id, &req).await?;
        if let CommitOutcome::MissingBlobs(missing) = &outcome {
            self.upload_blobs(client, missing).await?;
            outcome = client.commit(&id, &req).await?;
        }

        let st = &self.inner.state;
        match outcome {
            CommitOutcome::Committed(resp) => {
                let version = resp.manifest.info.version;
                let mut game = self.require_game(&id)?;
                game.base_version = version;
                game.base_files = row.files;
                game.last_error = None;
                game.notified_version = game.notified_version.max(version);
                if game.staged.as_ref().is_some_and(|s| s.info.version <= version) {
                    game.staged = None;
                }
                st.save_game(&game)?;
                st.delete_outbox(&id)?;
                self.emit(EngineEvent::Uploaded { game_id: id, version, created: resp.created });
            }
            CommitOutcome::Conflict { current_version, .. } => {
                let remote = client.manifest(&id, current_version).await?;
                self.download(client, &remote).await?;
                row.blocked = true;
                st.put_outbox(&row)?;
                let mut game = self.require_game(&id)?;
                let conflict = ConflictInfo {
                    remote: remote.info.clone(),
                    local_played_at: row.played_at,
                    detected_at: now_ms(),
                };
                game.staged = Some(remote);
                game.conflict = Some(conflict.clone());
                st.save_game(&game)?;
                client.ack(&id, current_version, AckState::Delivered).await?;
                self.emit(EngineEvent::Conflict { game_id: id, game_name: game.config.name, conflict });
            }
            CommitOutcome::MissingBlobs(missing) => {
                return Err(EngineError::Other(format!(
                    "server still missing {} files after uploading them",
                    missing.len()
                )));
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ downloading

    async fn pull_pending(&self, client: &Client) -> Result<()> {
        for status in client.pending().await? {
            let Some(game) = self.inner.state.game(&status.id)? else { continue };
            let id = status.id.clone();
            let r = async {
                let have = game.base_version.max(game.staged.as_ref().map_or(0, |s| s.info.version));
                if status.current_version <= have {
                    // Already have it; the earlier ack must have been lost.
                    client.ack(&id, status.current_version, AckState::Delivered).await?;
                    return Ok(());
                }
                let manifest = client.manifest(&id, status.current_version).await?;
                self.receive(client, manifest).await
            }
            .await;
            self.per_game(&id, r)?;
        }
        Ok(())
    }

    async fn download(&self, client: &Client, manifest: &Manifest) -> Result<()> {
        validate_files(&manifest.files)?;
        let mut shas: Vec<&str> = manifest.files.iter().map(|f| f.sha256.as_str()).collect();
        shas.sort_unstable();
        shas.dedup();
        for sha in shas {
            if !self.inner.cache.has(sha) {
                let stream = client.download_blob(sha).await?;
                self.inner.cache.ingest_stream(stream, sha).await?;
            }
        }
        Ok(())
    }

    /// Downloads a version into the cache, stages it and tells the server it arrived.
    /// Doesn't touch the save folder; [`evaluate_staged`](Self::evaluate_staged) decides that.
    async fn receive(&self, client: &Client, manifest: Manifest) -> Result<()> {
        let id = manifest.info.game_id.clone();
        let version = manifest.info.version;
        let game = self.require_game(&id)?;
        let have = game.base_version.max(game.staged.as_ref().map_or(0, |s| s.info.version));
        if version > have {
            self.download(client, &manifest).await?;
            let mut game = self.require_game(&id)?;
            game.staged = Some(manifest);
            self.inner.state.save_game(&game)?;
        }
        client.ack(&id, version, AckState::Delivered).await?;
        Ok(())
    }

    /// Decides what to do with a staged download: hold it (game running), flag a
    /// conflict (local changes too), import it (auto policy) or ask the user.
    async fn evaluate_staged(&self, id: &str) -> Result<()> {
        let mut game = self.require_game(id)?;
        let Some(staged) = game.staged.clone() else { return Ok(()) };
        if self.in_session(id) || game.conflict.is_some() {
            return Ok(());
        }
        let st = &self.inner.state;
        let version = staged.info.version;
        if version <= game.base_version {
            game.staged = None;
            return st.save_game(&game);
        }

        let outbox = st.outbox(id)?;
        let local = self.capture(&game.config).await?;
        let local_changed = !local.is_empty() && !same_contents(&local, &game.base_files);
        if outbox.is_some() || local_changed {
            let local_played_at = match outbox {
                Some(mut o) => {
                    o.blocked = true;
                    st.put_outbox(&o)?;
                    o.played_at
                }
                None => {
                    let played_at = latest_mtime(&local);
                    st.put_outbox(&OutboxRow {
                        game_id: id.into(),
                        base_version: game.base_version,
                        files: local,
                        played_at,
                        created_at: now_ms(),
                        blocked: true,
                    })?;
                    played_at
                }
            };
            let conflict = ConflictInfo { remote: staged.info.clone(), local_played_at, detected_at: now_ms() };
            game.conflict = Some(conflict.clone());
            st.save_game(&game)?;
            self.emit(EngineEvent::Conflict { game_id: id.into(), game_name: game.config.name, conflict });
            return Ok(());
        }

        match game.config.import_policy {
            ImportPolicy::AutoWhenSafe => self.import_staged(id, staged).await,
            ImportPolicy::Ask => {
                if version > game.notified_version {
                    game.notified_version = version;
                    st.save_game(&game)?;
                    self.emit(EngineEvent::ImportAvailable {
                        game_id: id.into(),
                        game_name: game.config.name,
                        remote: staged.info,
                    });
                }
                Ok(())
            }
        }
    }

    // ------------------------------------------------------------ applying

    /// Makes the save folder match `files`, after backing up what's there.
    async fn apply(&self, id: &str, files: Vec<FileEntry>, reason: String) -> Result<()> {
        validate_files(&files)?;
        let game = self.require_game(id)?;
        if self.in_session(id) {
            return Err(EngineError::GameRunning(game.config.name));
        }
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.apply_blocking(&game.config, &files, &reason))
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?
    }

    async fn import_staged(&self, id: &str, staged: Manifest) -> Result<()> {
        let version = staged.info.version;
        self.apply(id, staged.files.clone(), format!("before importing v{version}")).await?;
        let st = &self.inner.state;
        let mut game = self.require_game(id)?;
        game.base_version = version;
        game.base_files = staged.files;
        game.staged = None;
        game.conflict = None;
        game.last_error = None;
        game.notified_version = game.notified_version.max(version);
        st.save_game(&game)?;
        // Anything queued was either already uploaded or the user chose the other save.
        st.delete_outbox(id)?;
        self.emit(EngineEvent::Imported { game_id: id.into(), version });
        if let Ok(client) = self.client()
            && let Err(e) = client.ack(id, version, AckState::Applied).await
        {
            tracing::debug!("couldn't report import of {id} v{version}: {e}");
        }
        Ok(())
    }

    /// Keeps the local save and uploads it on top of `remote`.
    async fn override_with_local(&self, id: &str, remote: Manifest) -> Result<()> {
        let version = remote.info.version;
        let st = &self.inner.state;
        let mut game = self.require_game(id)?;
        game.base_version = version;
        game.base_files = remote.files;
        game.staged = None;
        game.conflict = None;
        game.notified_version = game.notified_version.max(version);
        st.save_game(&game)?;
        if let Some(mut o) = st.outbox(id)? {
            o.base_version = version;
            o.blocked = false;
            st.put_outbox(&o)?;
        }
        self.snapshot(id, None).await?;
        if let Ok(client) = self.client() {
            let result = async {
                client.ack(id, version, AckState::Declined).await?;
                self.flush_outbox(&client).await
            }
            .await;
            self.ignore_offline(result)?;
        }
        Ok(())
    }
}

impl Inner {
    fn capture_blocking(&self, config: &GameConfig) -> Result<Vec<FileEntry>> {
        let storage = self.storage.open(&config.location)?;
        let ignore = build_ignore(&config.ignore)?;
        let include = build_globs(&config.include)?;
        let mut out = Vec::new();
        for f in storage.list()? {
            let included = config.include.is_empty() || include.is_match(&f.path);
            if !included || ignore.is_match(&f.path) || !valid_save_path(&f.path) {
                continue;
            }
            let cached = self
                .state
                .cached_hash(&config.id, &f.path, f.size, f.mtime)?
                .filter(|sha| self.cache.has(sha));
            let (sha, size) = match cached {
                Some(sha) => (sha, f.size),
                None => {
                    let mut reader = match storage.open_read(&f.path) {
                        Ok(r) => r,
                        // Deleted between listing and reading.
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(e.into()),
                    };
                    let (sha, size) = self.cache.ingest(&mut reader)?;
                    self.state.put_hash(&config.id, &f.path, f.size, f.mtime, &sha)?;
                    (sha, size)
                }
            };
            out.push(FileEntry { path: f.path, sha256: sha, size: size as i64, mtime: Some(f.mtime) });
        }
        let paths: HashSet<&str> = out.iter().map(|f| f.path.as_str()).collect();
        self.state.retain_hashes(&config.id, &paths)?;
        Ok(out)
    }

    fn apply_blocking(&self, config: &GameConfig, target: &[FileEntry], reason: &str) -> Result<()> {
        let current = self.capture_blocking(config)?;
        if !current.is_empty() && !same_contents(&current, target) {
            self.state.add_backup(&config.id, reason, &current, now_ms(), self.config.keep_backups)?;
        }
        let storage = self.storage.open(&config.location)?;
        let current_sha: HashMap<&str, &str> =
            current.iter().map(|f| (f.path.as_str(), f.sha256.as_str())).collect();
        for f in target {
            if current_sha.get(f.path.as_str()) != Some(&f.sha256.as_str()) {
                let mut src = self.cache.open_read(&f.sha256)?;
                storage.write_atomic(&f.path, &mut src)?;
            }
        }
        let target_by_path: HashMap<&str, &FileEntry> = target.iter().map(|f| (f.path.as_str(), f)).collect();
        for f in &current {
            if !target_by_path.contains_key(f.path.as_str()) {
                storage.remove(&f.path)?;
            }
        }
        // Remember the new files' hashes so the next scan doesn't re-read them.
        for lf in storage.list()? {
            if let Some(t) = target_by_path.get(lf.path.as_str())
                && t.size == lf.size as i64
            {
                self.state.put_hash(&config.id, &lf.path, lf.size, lf.mtime, &t.sha256)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, sha: &str) -> FileEntry {
        FileEntry { path: path.into(), sha256: sha.into(), size: 1, mtime: None }
    }

    #[test]
    fn rejects_hostile_file_lists() {
        let ok = "a".repeat(64);
        assert!(validate_files(&[entry("saves/game.srm", &ok)]).is_ok());
        for bad in [
            vec![entry("../outside", &ok)],
            vec![entry("/etc/passwd", &ok)],
            vec![entry("C:/Windows/x", &ok)],
            vec![entry("a\\..\\b", &ok)],
            vec![entry("game.srm", "../../../etc/passwd")],
            vec![entry("game.srm", "ab")],
            vec![entry("game.srm", &ok), entry("game.srm", &ok)],
        ] {
            assert!(validate_files(&bad).is_err(), "{bad:?}");
        }
    }
}
