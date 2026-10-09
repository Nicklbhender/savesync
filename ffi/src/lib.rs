//! The SaveSync engine for the Android app, exposed to Kotlin with UniFFI.
//!
//! Kept deliberately small: rich values (game lists, configs, events) cross the
//! boundary as JSON matching the engine's serde types, so the Kotlin side just
//! mirrors those types. Save folders are Storage Access Framework tree URIs,
//! read and written by Kotlin through [`FolderAccess`].
//!
//! Threading: methods block until done; call them off the main thread. The
//! [`EventListener`] is called on an engine thread and must return quickly
//! (hand the event to a coroutine) and must not call back into blocking methods.

use std::{
    io::{self, Cursor, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use savesync_engine::{
    Engine, EngineConfig, EngineError, GameConfig, LocalFile, Resolution, SaveStorage, StorageProvider,
};

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("{detail}")]
    Failed { detail: String },
    /// Missing file or folder (e.g. the folder was deleted or access was revoked).
    #[error("{detail}")]
    NotFound { detail: String },
}

impl From<EngineError> for FfiError {
    fn from(e: EngineError) -> Self {
        FfiError::Failed { detail: e.to_string() }
    }
}

impl From<serde_json::Error> for FfiError {
    fn from(e: serde_json::Error) -> Self {
        FfiError::Failed { detail: format!("invalid data: {e}") }
    }
}

type FfiResult<T> = Result<T, FfiError>;

// ---------------------------------------------------------------- foreign interfaces

#[derive(uniffi::Record)]
pub struct FolderEntry {
    /// Relative to the save folder, `/`-separated.
    pub path: String,
    pub size: u64,
    /// Last modified, ms since epoch.
    pub mtime: i64,
}

/// Save-folder access, implemented in Kotlin over the Storage Access Framework.
/// `location` is the folder's tree URI.
#[uniffi::export(with_foreign)]
pub trait FolderAccess: Send + Sync {
    fn list(&self, location: String) -> FfiResult<Vec<FolderEntry>>;
    fn read(&self, location: String, path: String) -> FfiResult<Vec<u8>>;
    /// Create or replace a file, creating parent folders as needed.
    fn write(&self, location: String, path: String, data: Vec<u8>) -> FfiResult<()>;
    fn remove(&self, location: String, path: String) -> FfiResult<()>;
}

#[uniffi::export(with_foreign)]
pub trait EventListener: Send + Sync {
    /// An engine event as JSON (`{"type": "imported", ...}`).
    fn on_event(&self, event_json: String);
}

fn to_io(e: FfiError) -> io::Error {
    match e {
        FfiError::NotFound { detail } => io::Error::new(io::ErrorKind::NotFound, detail),
        FfiError::Failed { detail } => io::Error::other(detail),
    }
}

struct ForeignProvider {
    access: Arc<dyn FolderAccess>,
}

struct ForeignStorage {
    access: Arc<dyn FolderAccess>,
    location: String,
}

impl StorageProvider for ForeignProvider {
    fn open(&self, location: &str) -> io::Result<Arc<dyn SaveStorage>> {
        Ok(Arc::new(ForeignStorage { access: self.access.clone(), location: location.to_string() }))
    }
}

impl SaveStorage for ForeignStorage {
    fn list(&self) -> io::Result<Vec<LocalFile>> {
        let entries = self.access.list(self.location.clone()).map_err(to_io)?;
        Ok(entries.into_iter().map(|e| LocalFile { path: e.path, size: e.size, mtime: e.mtime }).collect())
    }

    fn open_read(&self, path: &str) -> io::Result<Box<dyn Read + Send>> {
        let data = self.access.read(self.location.clone(), path.to_string()).map_err(to_io)?;
        Ok(Box::new(Cursor::new(data)))
    }

    fn write_atomic(&self, path: &str, contents: &mut dyn Read) -> io::Result<()> {
        let mut data = Vec::new();
        contents.read_to_end(&mut data)?;
        self.access.write(self.location.clone(), path.to_string(), data).map_err(to_io)
    }

    fn remove(&self, path: &str) -> io::Result<()> {
        self.access.remove(self.location.clone(), path.to_string()).map_err(to_io)
    }
}

// ---------------------------------------------------------------- engine

#[derive(uniffi::Object)]
pub struct SaveSync {
    engine: Engine,
    rt: tokio::runtime::Runtime,
    started: AtomicBool,
    listening: AtomicBool,
}

#[uniffi::export]
impl SaveSync {
    #[uniffi::constructor]
    pub fn new(data_dir: String, device_name: String, folders: Arc<dyn FolderAccess>) -> FfiResult<Arc<Self>> {
        init_logging();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("savesync")
            .enable_all()
            .build()
            .map_err(|e| FfiError::Failed { detail: e.to_string() })?;
        let config = EngineConfig::new(data_dir, device_name, "android");
        let engine = {
            let _enter = rt.enter();
            Engine::open(config, Arc::new(ForeignProvider { access: folders }))?
        };
        Ok(Arc::new(Self { engine, rt, started: AtomicBool::new(false), listening: AtomicBool::new(false) }))
    }

    /// Starts the background connection loop. Safe to call repeatedly.
    pub fn start(&self) {
        if !self.started.swap(true, Ordering::SeqCst) {
            let engine = self.engine.clone();
            self.rt.spawn(async move { engine.run().await });
        }
    }

    /// Forwards every engine event to `listener`. Only the first listener is used.
    pub fn set_listener(&self, listener: Arc<dyn EventListener>) {
        if self.listening.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut events = self.engine.events();
        self.rt.spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if let Ok(json) = serde_json::to_string(&event) {
                            listener.on_event(json);
                        }
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }

    /// `{"server": ServerInfo|null, "online": bool, "games": [GameView]}`
    pub fn snapshot(&self) -> FfiResult<String> {
        let snapshot = serde_json::json!({
            "server": self.engine.server()?,
            "online": self.engine.is_online(),
            "games": self.engine.games()?,
        });
        Ok(snapshot.to_string())
    }

    /// Returns ServerInfo JSON.
    pub fn pair(&self, server_url: String, enroll_key: String, device_name: String) -> FfiResult<String> {
        let info = self.rt.block_on(self.engine.pair_as(server_url.trim(), enroll_key.trim(), &device_name))?;
        Ok(serde_json::to_string(&info)?)
    }

    pub fn unpair(&self) -> FfiResult<()> {
        Ok(self.engine.unpair()?)
    }

    pub fn set_server_url(&self, url: String) -> FfiResult<()> {
        Ok(self.engine.set_server_url(&url)?)
    }

    /// Android: the UnifiedPush endpoint from the distributor, or None to stop pushes.
    pub fn set_push_endpoint(&self, endpoint: Option<String>) -> FfiResult<()> {
        Ok(self.rt.block_on(self.engine.set_push_endpoint(endpoint))?)
    }

    /// `config_json` is a GameConfig; returns GameView JSON.
    pub fn add_game(&self, config_json: String) -> FfiResult<String> {
        let config: GameConfig = serde_json::from_str(&config_json)?;
        let view = {
            let _enter = self.rt.enter();
            self.engine.add_game(config)?
        };
        Ok(serde_json::to_string(&view)?)
    }

    pub fn update_game(&self, config_json: String) -> FfiResult<String> {
        let config: GameConfig = serde_json::from_str(&config_json)?;
        let view = {
            let _enter = self.rt.enter();
            self.engine.update_game(config)?
        };
        Ok(serde_json::to_string(&view)?)
    }

    pub fn remove_game(&self, id: String) -> FfiResult<()> {
        Ok(self.rt.block_on(self.engine.remove_game(&id))?)
    }

    pub fn import_save(&self, id: String) -> FfiResult<()> {
        Ok(self.rt.block_on(self.engine.import(&id))?)
    }

    pub fn keep_local(&self, id: String) -> FfiResult<()> {
        Ok(self.rt.block_on(self.engine.keep_local(&id))?)
    }

    /// `resolution` is "keep_local" or "keep_remote".
    pub fn resolve_conflict(&self, id: String, resolution: String) -> FfiResult<()> {
        let resolution: Resolution = serde_json::from_value(serde_json::Value::String(resolution))?;
        Ok(self.rt.block_on(self.engine.resolve_conflict(&id, resolution))?)
    }

    /// BackupInfo list as JSON.
    pub fn backups(&self, id: String) -> FfiResult<String> {
        Ok(serde_json::to_string(&self.engine.backups(&id)?)?)
    }

    pub fn restore_backup(&self, backup_id: i64) -> FfiResult<()> {
        Ok(self.rt.block_on(self.engine.restore_backup(backup_id))?)
    }

    /// Full sync now; returns when finished (used by background workers too).
    pub fn sync_now(&self) -> FfiResult<()> {
        Ok(self.rt.block_on(self.engine.sync_now())?)
    }

    pub fn session_started(&self, game_id: String) {
        let _enter = self.rt.enter();
        self.engine.session_started(&game_id);
    }

    pub fn session_ended(&self, game_id: String) {
        let _enter = self.rt.enter();
        self.engine.session_ended(&game_id);
    }

    /// Woke up, unlocked, network changed, frontend opened: check the server now.
    pub fn nudge(&self) {
        self.engine.nudge();
    }

    pub fn handle_push(&self, payload: String) {
        self.engine.handle_push(&payload);
    }
}

fn init_logging() {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_tag("SaveSync").with_max_level(log::LevelFilter::Info),
    );
}
