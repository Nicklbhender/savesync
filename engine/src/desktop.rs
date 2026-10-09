//! Desktop helpers: notice when an emulator opens or closes, and when a save
//! folder changes. Android provides its own equivalents (UsageStatsManager
//! foreground events, periodic scans) through the platform layer.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::{sync::broadcast::error::RecvError, task::JoinHandle};

use crate::{Engine, EngineEvent, storage::TEMP_PREFIX};

/// Starts both watchers. Drop or abort the handles to stop them.
pub fn spawn(engine: &Engine) -> Vec<JoinHandle<()>> {
    vec![
        spawn_process_watcher(engine.clone(), Duration::from_secs(2)),
        spawn_folder_watcher(engine.clone()),
    ]
}

/// Compares a configured process name with a running one, ignoring case and a
/// trailing `.exe`, so `Emulator` matches `emulator.exe` on Windows.
pub fn process_matches(pattern: &str, running: &str) -> bool {
    fn norm(s: &str) -> String {
        let s = s.trim().to_lowercase();
        s.strip_suffix(".exe").map(str::to_owned).unwrap_or(s)
    }
    !pattern.trim().is_empty() && norm(pattern) == norm(running)
}

fn running_process_names(sys: &mut System) -> HashSet<String> {
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::OnlyIfNotSet));
    let mut names = HashSet::new();
    for p in sys.processes().values() {
        names.insert(p.name().to_string_lossy().into_owned());
        // Process names can be truncated (macOS); the executable's file name isn't.
        if let Some(file) = p.exe().and_then(Path::file_name) {
            names.insert(file.to_string_lossy().into_owned());
        }
    }
    names
}

/// Polls the process list and turns emulator starts/exits into sessions.
pub fn spawn_process_watcher(engine: Engine, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut sys = Some(System::new());
        let mut active: HashSet<String> = HashSet::new();
        let mut tick = tokio::time::interval(interval);
        loop {
            tick.tick().await;
            let Ok(games) = engine.games() else { continue };
            if games.iter().all(|g| g.config.processes.is_empty()) {
                continue;
            }
            let mut s = sys.take().expect("system is returned after each refresh");
            let Ok((s, names)) = tokio::task::spawn_blocking(move || {
                let names = running_process_names(&mut s);
                (s, names)
            })
            .await
            else {
                sys = Some(System::new());
                continue;
            };
            sys = Some(s);

            let now_active: HashSet<String> = games
                .iter()
                .filter(|g| g.config.processes.iter().any(|p| names.iter().any(|n| process_matches(p, n))))
                .map(|g| g.config.id.clone())
                .collect();
            for id in now_active.difference(&active) {
                engine.session_started(id);
            }
            for id in active.difference(&now_active) {
                engine.session_ended(id);
            }
            active = now_active;
        }
    })
}

/// Watches every game's save folder and reports changes to the engine, which
/// captures them once the folder goes quiet.
pub fn spawn_folder_watcher(engine: Engine) -> JoinHandle<()> {
    tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                for path in event.paths {
                    let _ = tx.send(path);
                }
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("folder watching unavailable: {e}");
                return;
            }
        };
        let mut events = engine.events();
        // Folder -> games stored in it (several games can share one folder).
        let mut roots: HashMap<PathBuf, Vec<String>> = HashMap::new();
        refresh_watches(&engine, &mut watcher, &mut roots);
        loop {
            tokio::select! {
                Some(path) = rx.recv() => {
                    let is_temp = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(TEMP_PREFIX));
                    if is_temp {
                        continue;
                    }
                    for (root, ids) in &roots {
                        if path.starts_with(root) {
                            for id in ids {
                                engine.local_change(id);
                            }
                        }
                    }
                }
                event = events.recv() => match event {
                    Ok(EngineEvent::GamesChanged) | Err(RecvError::Lagged(_)) => {
                        refresh_watches(&engine, &mut watcher, &mut roots);
                    }
                    Err(RecvError::Closed) => break,
                    Ok(_) => {}
                },
            }
        }
    })
}

fn refresh_watches(engine: &Engine, watcher: &mut RecommendedWatcher, roots: &mut HashMap<PathBuf, Vec<String>>) {
    let mut wanted: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for g in engine.games().unwrap_or_default() {
        let root = PathBuf::from(&g.config.location);
        if root.is_absolute() && root.is_dir() {
            wanted.entry(root).or_default().push(g.config.id);
        }
    }
    for root in roots.keys() {
        if !wanted.contains_key(root) {
            let _ = watcher.unwatch(root);
        }
    }
    for root in wanted.keys() {
        if !roots.contains_key(root)
            && let Err(e) = watcher.watch(root, RecursiveMode::Recursive)
        {
            tracing::warn!("can't watch {}: {e}", root.display());
        }
    }
    *roots = wanted;
}

#[cfg(test)]
mod tests {
    use super::process_matches;

    #[test]
    fn matching() {
        assert!(process_matches("emulator.exe", "Emulator.exe"));
        assert!(process_matches("Emulator", "emulator.exe"));
        assert!(process_matches("Game", "Game"));
        assert!(!process_matches("game", "game-launcher"));
        assert!(!process_matches("", "anything"));
    }
}
