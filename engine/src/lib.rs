//! The SaveSync client engine, shared by the desktop (Tauri) and Android apps.
//!
//! The platform layer gives the engine:
//! - a [`StorageProvider`] for save folders (filesystem on desktop, SAF on Android),
//! - session signals ([`Engine::session_started`] / [`Engine::session_ended`]) when an
//!   emulator opens and closes,
//! - nudges ([`Engine::nudge`]) on wake, unlock, network changes and push messages,
//!
//! and listens to [`Engine::events`] to show notifications and prompts.
//! On desktop, the `desktop` feature provides the process and folder watchers.

mod cache;
mod client;
#[cfg(feature = "desktop")]
pub mod desktop;
mod engine;
mod error;
mod state;
pub mod storage;
mod types;

pub use engine::{Engine, EngineConfig};
pub use error::{EngineError, Result};
pub use savesync_protocol::{FileEntry, VersionInfo};
pub use storage::{FsProvider, FsStorage, LocalFile, SaveStorage, StorageProvider};
pub use types::*;
