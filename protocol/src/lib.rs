//! Wire types for the SaveSync API (v1), shared by the server and the sync engine
//! so the two can't drift apart.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- validation

/// Game ids are shared across devices, so they're restricted to a portable slug
/// like `my-game` or `game.v2`.
pub fn valid_game_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'))
}

pub fn valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Validates a path inside a save folder. Paths are `/`-separated and relative
/// to the game's save folder; anything that could escape that folder is rejected.
pub fn valid_save_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 1024 || path.contains(['\\', '\0']) || path.starts_with('/') {
        return false;
    }
    let mut components = path.split('/');
    // A drive letter like `C:` in the first component means an absolute Windows path.
    if components.clone().next().is_some_and(|first| first.contains(':')) {
        return false;
    }
    components.all(|c| !c.is_empty() && c != "." && c != ".." && c.len() <= 255)
}

// ---------------------------------------------------------------- core types

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    /// Path relative to the game's save folder, `/`-separated.
    pub path: String,
    pub sha256: String,
    pub size: i64,
    /// Client-side modification time (ms since epoch), informational only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
}

/// True when two file lists have the same paths with the same contents.
pub fn same_contents(a: &[FileEntry], b: &[FileEntry]) -> bool {
    fn key(files: &[FileEntry]) -> Vec<(&str, &str)> {
        let mut v: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.sha256.as_str())).collect();
        v.sort_unstable();
        v
    }
    key(a) == key(b)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VersionInfo {
    pub game_id: String,
    pub version: i64,
    pub base_version: i64,
    pub device_id: String,
    pub device_name: Option<String>,
    pub created_at: i64,
    /// When the play session ended on the client. Differs from `created_at`
    /// when a session ended offline and was uploaded later.
    pub played_at: Option<i64>,
    pub total_size: i64,
    pub file_count: i64,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    #[serde(flatten)]
    pub info: VersionInfo,
    pub files: Vec<FileEntry>,
}

/// A game as seen by one device.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GameStatus {
    pub id: String,
    pub name: String,
    pub current_version: i64,
    pub latest: Option<VersionInfo>,
    pub subscribed: bool,
    /// Newest version this device has downloaded and verified.
    pub delivered_version: i64,
    /// Newest version this device has imported into its save folder.
    pub applied_version: i64,
    pub last_state: Option<String>,
    /// True when the device is subscribed and hasn't received the latest version.
    pub pending: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub has_push: bool,
    pub created_at: i64,
    pub last_seen_at: i64,
    pub revoked: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AckState {
    /// Downloaded and verified; staged on the device, not yet imported.
    Delivered,
    /// Imported into the emulator's save folder.
    Applied,
    /// The user chose to keep their local save instead.
    Declined,
}

impl AckState {
    pub fn as_str(self) -> &'static str {
        match self {
            AckState::Delivered => "delivered",
            AckState::Applied => "applied",
            AckState::Declined => "declined",
        }
    }
}

// ---------------------------------------------------------------- requests / responses

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnrollRequest {
    pub name: String,
    /// Free-form, e.g. "windows", "macos", "android".
    pub platform: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnrollResponse {
    pub device_id: String,
    /// Shown once; the server only stores its hash.
    pub token: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushEndpointRequest {
    /// The UnifiedPush endpoint URL from the device's distributor, or null to disable.
    pub endpoint: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubscribeRequest {
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitRequest {
    /// The version this save was based on (0 for a game's first upload).
    pub base_version: i64,
    pub files: Vec<FileEntry>,
    /// When the play session ended on the device (ms since epoch).
    #[serde(default)]
    pub played_at: Option<i64>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitResponse {
    /// False when the upload matched the server's current version, so nothing changed.
    pub created: bool,
    #[serde(flatten)]
    pub manifest: Manifest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AckRequest {
    pub version: i64,
    pub state: AckState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissingRequest {
    pub sha256: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissingResponse {
    pub missing: Vec<String>,
}

/// Body of every error response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorBody {
    /// `unauthorized`, `not_found`, `bad_request`, `conflict`, `missing_blobs`, `too_large`, `internal`.
    pub error: String,
    pub message: String,
    /// `conflict` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<i64>,
    /// `conflict` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<VersionInfo>,
    /// `missing_blobs` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<Vec<String>>,
}

// ---------------------------------------------------------------- realtime

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Sent on connect: everything this device missed while it was away.
    Hello {
        device_id: String,
        server_time: i64,
        pending: Vec<GameStatus>,
    },
    /// A new version was uploaded by another device. Includes the manifest so the
    /// client can start downloading right away.
    VersionAvailable { game_name: String, manifest: Manifest },
    Acked { status: GameStatus },
    Error { message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Ack { game_id: String, version: i64, state: AckState },
}

/// The small payload sent through UnifiedPush. Push messages are size-limited,
/// so this only says what changed; the app then downloads it over HTTP.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushPayload {
    #[serde(rename = "type")]
    pub kind: String,
    pub game_id: String,
    pub game_name: String,
    pub version: i64,
    pub from_device: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_paths() {
        assert!(valid_save_path("My Game.sav"));
        assert!(valid_save_path("memcards/Mcd001.ps2"));
        assert!(valid_save_path(".hidden/state.dat"));
        assert!(!valid_save_path(""));
        assert!(!valid_save_path("/etc/passwd"));
        assert!(!valid_save_path("../escape"));
        assert!(!valid_save_path("a/../../b"));
        assert!(!valid_save_path("a/./b"));
        assert!(!valid_save_path("a//b"));
        assert!(!valid_save_path("a/"));
        assert!(!valid_save_path("C:/Users/x"));
        assert!(!valid_save_path("dir\\file"));
    }

    #[test]
    fn game_ids() {
        assert!(valid_game_id("my-game"));
        assert!(valid_game_id("game.v2_b"));
        assert!(!valid_game_id(""));
        assert!(!valid_game_id("-leading"));
        assert!(!valid_game_id("Upper"));
        assert!(!valid_game_id("has space"));
        assert!(!valid_game_id(&"a".repeat(65)));
    }

    #[test]
    fn sha() {
        assert!(valid_sha256(&"a".repeat(64)));
        assert!(!valid_sha256(&"A".repeat(64)));
        assert!(!valid_sha256(&"a".repeat(63)));
    }

    #[test]
    fn contents_ignore_order_and_mtime() {
        let f = |p: &str, s: &str, m| FileEntry { path: p.into(), sha256: s.into(), size: 1, mtime: m };
        assert!(same_contents(&[f("a", "1", None), f("b", "2", None)], &[f("b", "2", Some(5)), f("a", "1", None)]));
        assert!(!same_contents(&[f("a", "1", None)], &[f("a", "2", None)]));
        assert!(!same_contents(&[f("a", "1", None)], &[f("a", "1", None), f("b", "2", None)]));
    }
}
