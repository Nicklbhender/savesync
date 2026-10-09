//! Commands the UI calls. Errors are returned as display strings.

use std::collections::BTreeSet;

use savesync_engine::{BackupInfo, EngineError, GameConfig, GameView, Resolution, ServerInfo};
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;

use crate::{AppState, default_device_name, platform, updater::UpdateStatus};

type CmdResult<T> = Result<T, String>;

fn err(e: EngineError) -> String {
    e.to_string()
}

#[derive(Serialize)]
pub struct Snapshot {
    server: Option<ServerInfo>,
    online: bool,
    suggested_device_name: String,
    platform: &'static str,
    autostart: bool,
    version: &'static str,
    update: UpdateStatus,
    games: Vec<GameView>,
}

#[tauri::command]
pub fn get_snapshot(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Snapshot> {
    let engine = &state.engine;
    Ok(Snapshot {
        server: engine.server().map_err(err)?,
        online: engine.is_online(),
        suggested_device_name: default_device_name(),
        platform: platform(),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        version: crate::updater::CURRENT,
        update: state.updater.status(),
        games: engine.games().map_err(err)?,
    })
}

#[tauri::command]
pub async fn pair(
    state: State<'_, AppState>,
    server_url: String,
    enroll_key: String,
    device_name: String,
) -> CmdResult<ServerInfo> {
    let url = server_url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("The server address should start with http:// (for example http://192.168.1.50:8420)".into());
    }
    state.engine.pair_as(url, enroll_key.trim(), &device_name).await.map_err(err)
}

#[tauri::command]
pub fn unpair(state: State<'_, AppState>) -> CmdResult<()> {
    state.engine.unpair().map_err(err)
}

#[tauri::command]
pub fn set_server_url(state: State<'_, AppState>, url: String) -> CmdResult<()> {
    state.engine.set_server_url(&url).map_err(err)
}

#[tauri::command]
pub fn add_game(state: State<'_, AppState>, config: GameConfig) -> CmdResult<GameView> {
    state.engine.add_game(config).map_err(err)
}

#[tauri::command]
pub fn update_game(state: State<'_, AppState>, config: GameConfig) -> CmdResult<GameView> {
    state.engine.update_game(config).map_err(err)
}

#[tauri::command]
pub async fn remove_game(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.engine.remove_game(&id).await.map_err(err)
}

#[tauri::command]
pub async fn import_save(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.engine.import(&id).await.map_err(err)
}

#[tauri::command]
pub async fn keep_local(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    state.engine.keep_local(&id).await.map_err(err)
}

#[tauri::command]
pub async fn resolve_conflict(state: State<'_, AppState>, id: String, resolution: Resolution) -> CmdResult<()> {
    state.engine.resolve_conflict(&id, resolution).await.map_err(err)
}

#[tauri::command]
pub fn list_backups(state: State<'_, AppState>, id: String) -> CmdResult<Vec<BackupInfo>> {
    state.engine.backups(&id).map_err(err)
}

#[tauri::command]
pub async fn restore_backup(state: State<'_, AppState>, backup_id: i64) -> CmdResult<()> {
    state.engine.restore_backup(backup_id).await.map_err(err)
}

#[tauri::command]
pub async fn sync_now(state: State<'_, AppState>) -> CmdResult<()> {
    state.engine.sync_now().await.map_err(err)
}

#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> CmdResult<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().set_title("Choose the save folder").pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx.await.map_err(|e| e.to_string())?;
    Ok(folder.and_then(|f| f.into_path().ok()).map(|p| p.to_string_lossy().into_owned()))
}

/// Lets the user pick the program they play a game in. Only programs are offered:
/// `.exe` files on Windows, apps on macOS. Returns the process name to watch for.
#[tauri::command]
pub async fn pick_app(app: AppHandle) -> CmdResult<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let dialog = app.dialog().file().set_title("Choose the app or game");
    #[cfg(target_os = "windows")]
    let dialog = dialog.add_filter("Programs", &["exe"]);
    #[cfg(target_os = "macos")]
    let dialog = dialog.add_filter("Applications", &["app"]).set_directory("/Applications");
    dialog.pick_file(move |file| {
        let _ = tx.send(file);
    });
    let picked = rx.await.map_err(|e| e.to_string())?;
    Ok(picked.and_then(|f| f.into_path().ok()).map(|p| process_name(&p)))
}

/// The name the running process will have: the exe's file name on Windows, the
/// bundle's executable (CFBundleExecutable) for a macOS app.
fn process_name(path: &std::path::Path) -> String {
    #[cfg(target_os = "macos")]
    if path.extension().is_some_and(|e| e == "app") {
        let exe = std::process::Command::new("/usr/bin/defaults")
            .arg("read")
            .arg(path.join("Contents/Info"))
            .arg("CFBundleExecutable")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(exe) = exe {
            return exe;
        }
        return path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    }
    path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Names of running programs, for picking the app/game.
#[tauri::command]
pub fn running_processes() -> Vec<String> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let names: BTreeSet<String> = sys
        .processes()
        .values()
        .map(|p| p.name().to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .collect();
    names.into_iter().collect()
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> CmdResult<()> {
    let launcher = app.autolaunch();
    let result = if enabled { launcher.enable() } else { launcher.disable() };
    result.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle, state: State<'_, AppState>) -> CmdResult<UpdateStatus> {
    state.updater.check(&app).await;
    Ok(state.updater.status())
}

/// Installs a downloaded update now and restarts (otherwise it waits until no game is running).
#[tauri::command]
pub fn install_update(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    state.updater.install(&app)
}
