use serde::{Deserialize, Serialize};

use savesync_protocol::VersionInfo;

/// A game the user has set up on this device.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GameConfig {
    /// Shared across devices, e.g. `my-game`. Must be the same on every device.
    pub id: String,
    pub name: String,
    /// Where the save folder is: a filesystem path on desktop, a SAF tree URI on Android.
    pub location: String,
    /// Glob patterns (relative to the save folder) selecting which files belong to
    /// this game. Empty means everything. Needed when several games share one
    /// folder, e.g. one emulator's `saves/` folder: `["my-game.*"]`.
    #[serde(default)]
    pub include: Vec<String>,
    /// Glob patterns (relative to the save folder) that are never synced, e.g. `*.bak`.
    /// Common junk like `.DS_Store` and `Thumbs.db` is always ignored.
    #[serde(default)]
    pub ignore: Vec<String>,
    /// Process names (desktop) or package names (Android) that mean the game is
    /// being played, e.g. `emulator.exe` or `com.example.emulator`. While one is running,
    /// incoming saves are held and uploads wait until it closes.
    #[serde(default)]
    pub processes: Vec<String>,
    #[serde(default)]
    pub import_policy: ImportPolicy,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportPolicy {
    /// Download new saves automatically, but ask before importing them.
    #[default]
    Ask,
    /// Import automatically when the local save has no unsynced changes.
    /// Still asks when there's a conflict.
    AutoWhenSafe,
}

/// The user's choice when both this device and another changed the save.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// Upload this device's save as the newest version.
    KeepLocal,
    /// Replace this device's save with the server's. The local save is backed up first.
    KeepRemote,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ConflictInfo {
    /// The server's version that conflicts with this device's save.
    pub remote: VersionInfo,
    /// When this device's conflicting play session ended.
    pub local_played_at: Option<i64>,
    pub detected_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct QueuedUpload {
    pub played_at: Option<i64>,
    pub created_at: i64,
    /// Waiting on a conflict to be resolved.
    pub blocked: bool,
}

/// Everything the UI needs to show one game.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GameView {
    pub config: GameConfig,
    /// The server version this device's save is based on (0 = never synced).
    pub base_version: i64,
    /// Registered with the server (happens automatically once online).
    pub registered: bool,
    pub in_session: bool,
    /// A local save waiting to be uploaded (e.g. played offline).
    pub queued_upload: Option<QueuedUpload>,
    /// A newer save from another device, downloaded and ready to import.
    pub staged: Option<VersionInfo>,
    pub conflict: Option<ConflictInfo>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BackupInfo {
    pub id: i64,
    pub game_id: String,
    pub created_at: i64,
    /// Why it was made, e.g. "before importing v8".
    pub reason: String,
    pub file_count: usize,
    pub total_size: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ServerInfo {
    pub url: String,
    pub device_id: String,
    pub device_name: String,
}

/// Things the UI (tray icon, notifications) should react to.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    Connection { online: bool },
    GamesChanged,
    SessionStarted { game_id: String },
    SessionEnded { game_id: String },
    /// A local save was captured and is waiting to upload.
    Queued { game_id: String },
    /// `created` is false when the server already had exactly this save.
    Uploaded { game_id: String, version: i64, created: bool },
    /// A newer save from another device is ready. Show "Import / Keep mine".
    ImportAvailable { game_id: String, game_name: String, remote: VersionInfo },
    Imported { game_id: String, version: i64 },
    /// Both this device and another changed the save. Show "Keep mine / Use theirs".
    Conflict { game_id: String, game_name: String, conflict: ConflictInfo },
    Error { game_id: Option<String>, message: String },
}
