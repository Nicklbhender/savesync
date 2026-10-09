// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod updater;

use std::{sync::Arc, time::Duration};

use savesync_engine::{Engine, EngineConfig, EngineEvent, FsProvider};
use tauri::{
    AppHandle, Emitter, Manager, RunEvent, WindowEvent,
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::broadcast::error::RecvError;

/// Passed by the login item so the app starts in the tray.
const HIDDEN_ARG: &str = "--hidden";

pub struct AppState {
    pub engine: Engine,
    pub updater: updater::Updater,
    status_item: MenuItem<tauri::Wry>,
}

pub fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

pub fn default_device_name() -> String {
    sysinfo::System::host_name()
        .map(|h| h.trim_end_matches(".local").to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "My computer".into())
}

pub fn show_main(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn hide_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    // Live in the menu bar only, without a Dock icon, while the window is closed.
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::warn!("notification failed: {e}");
    }
}

fn game_name(engine: &Engine, id: &str) -> String {
    engine.game(id).map(|g| g.config.name).unwrap_or_else(|_| id.to_string())
}

fn update_status(app: &AppHandle) {
    let state = app.state::<AppState>();
    let engine = &state.engine;
    let attention = engine
        .games()
        .map(|gs| gs.iter().filter(|g| g.staged.is_some() || g.conflict.is_some()).count())
        .unwrap_or(0);
    let text = match (engine.server().ok().flatten(), engine.is_online()) {
        (None, _) => "Not connected to a server".to_string(),
        (Some(_), _) if attention > 0 => {
            format!("{attention} save{} need{} attention", if attention == 1 { "" } else { "s" }, if attention == 1 { "s" } else { "" })
        }
        (Some(_), true) => "Online · up to date".to_string(),
        (Some(_), false) => "Offline · changes will sync later".to_string(),
    };
    let _ = state.status_item.set_text(&text);
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(format!("SaveSync: {text}")));
    }
}

/// Forwards engine events to the UI, shows notifications, and keeps the tray current.
async fn forward_events(app: AppHandle, engine: Engine) {
    let mut events = engine.events();
    loop {
        let event = match events.recv().await {
            Ok(e) => e,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        };
        let _ = app.emit("engine-event", &event);
        match &event {
            EngineEvent::ImportAvailable { game_name, remote, .. } => {
                let from = remote.device_name.as_deref().unwrap_or("another device");
                notify(&app, &format!("New save for {game_name}"), &format!("From {from}. Open SaveSync to import it."));
            }
            EngineEvent::Conflict { game_name, conflict, .. } => {
                let from = conflict.remote.device_name.as_deref().unwrap_or("another device");
                notify(
                    &app,
                    &format!("Save conflict: {game_name}"),
                    &format!("This computer and {from} both changed it. Open SaveSync to choose which to keep."),
                );
            }
            EngineEvent::Imported { game_id, .. } => {
                notify(&app, &format!("{} updated", game_name(&engine, game_id)), "Imported the latest save from your other device.");
            }
            _ => {}
        }
        update_status(&app);
    }
}

/// Timers don't run while the computer sleeps, so a tick that arrives much later
/// than scheduled means it just woke up: check the server right away.
async fn watch_for_wake(engine: Engine) {
    const TICK: Duration = Duration::from_secs(15);
    let mut last = std::time::SystemTime::now();
    loop {
        tokio::time::sleep(TICK).await;
        let now = std::time::SystemTime::now();
        if now.duration_since(last).unwrap_or_default() > TICK * 3 {
            tracing::info!("woke from sleep; syncing");
            engine.nudge();
        }
        last = now;
    }
}

fn build_tray(app: &AppHandle, status_item: &MenuItem<tauri::Wry>) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open SaveSync", true, None::<&str>)?;
    let sync = MenuItem::with_id(app, "sync", "Sync Now", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit SaveSync", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[status_item, &separator, &open, &sync, &separator, &quit])?;

    // macOS menu bar icons are monochrome "template" images the OS tints.
    let (icon, template) = if cfg!(target_os = "macos") {
        (Image::from_bytes(include_bytes!("../icons/tray-template.png"))?, true)
    } else {
        (Image::from_bytes(include_bytes!("../icons/tray.png"))?, false)
    };
    TrayIconBuilder::with_id("main")
        .icon(icon)
        .icon_as_template(template)
        .tooltip("SaveSync")
        .menu(&menu)
        // Windows convention: left-click opens the app, right-click shows the menu.
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "sync" => app.state::<AppState>().engine.nudge(),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event
                && !cfg!(target_os = "macos")
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn main() {
    // After a self-update, let the previous version exit before the single-instance check.
    updater::wait_for_previous_instance();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let app = tauri::Builder::default()
        // Must come first: a second launch just brings up the running app.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec![HIDDEN_ARG])))
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::pair,
            commands::unpair,
            commands::set_server_url,
            commands::add_game,
            commands::update_game,
            commands::remove_game,
            commands::import_save,
            commands::keep_local,
            commands::resolve_conflict,
            commands::list_backups,
            commands::restore_backup,
            commands::sync_now,
            commands::pick_folder,
            commands::pick_app,
            commands::running_processes,
            commands::set_autostart,
            commands::check_for_updates,
            commands::install_update,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = app.path().app_data_dir()?;
            let config = EngineConfig::new(data_dir.join("engine"), default_device_name(), platform());
            let engine = Engine::open(config, Arc::new(FsProvider))?;

            let status_item = MenuItem::with_id(app, "status", "Starting…", false, None::<&str>)?;
            build_tray(&handle, &status_item)?;
            app.manage(AppState { engine: engine.clone(), updater: updater::Updater::new(), status_item });
            update_status(&handle);
            updater::clean_up_previous();
            if let Some(from) = updater::updated_from() {
                notify(&handle, "SaveSync updated", &format!("Now on version {} (was {from}).", updater::CURRENT));
            }
            tauri::async_runtime::spawn(updater::run(handle.clone(), engine.clone()));

            tauri::async_runtime::spawn({
                let engine = engine.clone();
                async move { engine.run().await }
            });
            tauri::async_runtime::spawn({
                let engine = engine.clone();
                async move {
                    // Watches emulator processes and save folders; runs for the app's lifetime.
                    let _watchers = savesync_engine::desktop::spawn(&engine);
                    std::future::pending::<()>().await
                }
            });
            tauri::async_runtime::spawn(watch_for_wake(engine.clone()));
            tauri::async_runtime::spawn(forward_events(handle.clone(), engine.clone()));

            let paired = engine.server().ok().flatten().is_some();
            let started_hidden = std::env::args().any(|a| a == HIDDEN_ARG);
            if !paired || !started_hidden {
                show_main(&handle);
            } else {
                hide_main(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps SaveSync running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                hide_main(window.app_handle());
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to start SaveSync");

    app.run(|app, event| {
        match event {
            // Keep running with no windows open; only "Quit" exits.
            RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => show_main(app),
            _ => {}
        }
        let _ = app;
    });
}
