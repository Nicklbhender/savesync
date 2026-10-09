//! The engine's local SQLite database: pairing, games, the upload queue,
//! a hash cache for fast scans, and backups.

use std::{collections::HashSet, path::Path, sync::Mutex};

use rusqlite::{Connection, OptionalExtension, params};
use savesync_protocol::{FileEntry, Manifest};

use crate::{
    error::{EngineError, Result},
    types::{BackupInfo, ConflictInfo, GameConfig},
};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE games (
    id               TEXT PRIMARY KEY,
    config           TEXT NOT NULL,             -- GameConfig JSON
    registered       INTEGER NOT NULL DEFAULT 0,
    base_version     INTEGER NOT NULL DEFAULT 0,
    base_files       TEXT NOT NULL DEFAULT '[]',-- files as of the last sync
    staged           TEXT,                      -- Manifest JSON, downloaded and not yet imported
    conflict         TEXT,                      -- ConflictInfo JSON
    notified_version INTEGER NOT NULL DEFAULT 0,-- newest version the user was told about
    last_error       TEXT
);

-- At most one queued upload per game: newer snapshots replace older ones.
CREATE TABLE outbox (
    game_id      TEXT PRIMARY KEY,
    base_version INTEGER NOT NULL,
    files        TEXT NOT NULL,
    played_at    INTEGER,
    created_at   INTEGER NOT NULL,
    blocked      INTEGER NOT NULL DEFAULT 0     -- waiting on a conflict
);

CREATE TABLE file_hashes (
    game_id TEXT NOT NULL,
    path    TEXT NOT NULL,
    size    INTEGER NOT NULL,
    mtime   INTEGER NOT NULL,
    sha256  TEXT NOT NULL,
    PRIMARY KEY (game_id, path)
);

CREATE TABLE backups (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    reason     TEXT NOT NULL,
    files      TEXT NOT NULL
);
"#;

pub mod keys {
    pub const SERVER_URL: &str = "server_url";
    pub const DEVICE_ID: &str = "device_id";
    pub const TOKEN: &str = "token";
    pub const DEVICE_NAME: &str = "device_name";
}

#[derive(Clone, Debug)]
pub struct GameRow {
    pub config: GameConfig,
    pub registered: bool,
    pub base_version: i64,
    pub base_files: Vec<FileEntry>,
    pub staged: Option<Manifest>,
    pub conflict: Option<ConflictInfo>,
    pub notified_version: i64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OutboxRow {
    pub game_id: String,
    pub base_version: i64,
    pub files: Vec<FileEntry>,
    pub played_at: Option<i64>,
    pub created_at: i64,
    pub blocked: bool,
}

#[derive(Clone, Debug)]
pub struct BackupRow {
    pub info: BackupInfo,
    pub files: Vec<FileEntry>,
}

pub struct State {
    conn: Mutex<Connection>,
}

fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serializable")
}

fn from_json<T: serde::de::DeserializeOwned>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_str(s).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

fn opt_json<T: serde::de::DeserializeOwned>(s: Option<String>) -> rusqlite::Result<Option<T>> {
    s.map(|s| from_json(&s)).transpose()
}

impl State {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if version > SCHEMA_VERSION {
            return Err(EngineError::Other(format!(
                "local database v{version} is newer than this app (v{SCHEMA_VERSION})"
            )));
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ------------------------------------------------------------ settings

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        let conn = self.conn();
        match value {
            Some(v) => conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, v],
            )?,
            None => conn.execute("DELETE FROM settings WHERE key = ?1", [key])?,
        };
        Ok(())
    }

    // ------------------------------------------------------------ games

    const GAME_COLUMNS: &str =
        "config, registered, base_version, base_files, staged, conflict, notified_version, last_error";

    fn game_from_row(r: &rusqlite::Row) -> rusqlite::Result<GameRow> {
        Ok(GameRow {
            config: from_json(&r.get::<_, String>(0)?)?,
            registered: r.get(1)?,
            base_version: r.get(2)?,
            base_files: from_json(&r.get::<_, String>(3)?)?,
            staged: opt_json(r.get(4)?)?,
            conflict: opt_json(r.get(5)?)?,
            notified_version: r.get(6)?,
            last_error: r.get(7)?,
        })
    }

    pub fn games(&self) -> Result<Vec<GameRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {} FROM games ORDER BY id", Self::GAME_COLUMNS))?;
        let rows = stmt.query_map([], Self::game_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn game(&self, id: &str) -> Result<Option<GameRow>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {} FROM games WHERE id = ?1", Self::GAME_COLUMNS),
                [id],
                Self::game_from_row,
            )
            .optional()?)
    }

    pub fn insert_game(&self, config: &GameConfig) -> Result<()> {
        let changed = self.conn().execute(
            "INSERT INTO games (id, config) VALUES (?1, ?2) ON CONFLICT(id) DO NOTHING",
            params![config.id, to_json(config)],
        )?;
        if changed == 0 {
            return Err(EngineError::Invalid(format!("a game with id {} already exists", config.id)));
        }
        Ok(())
    }

    pub fn update_config(&self, config: &GameConfig) -> Result<()> {
        let changed = self
            .conn()
            .execute("UPDATE games SET config = ?2 WHERE id = ?1", params![config.id, to_json(config)])?;
        if changed == 0 {
            return Err(EngineError::UnknownGame(config.id.clone()));
        }
        Ok(())
    }

    /// Persists everything about a game except its config.
    pub fn save_game(&self, g: &GameRow) -> Result<()> {
        self.conn().execute(
            "UPDATE games SET registered = ?2, base_version = ?3, base_files = ?4, staged = ?5, conflict = ?6,
                notified_version = ?7, last_error = ?8 WHERE id = ?1",
            params![
                g.config.id,
                g.registered,
                g.base_version,
                to_json(&g.base_files),
                g.staged.as_ref().map(to_json),
                g.conflict.as_ref().map(to_json),
                g.notified_version,
                g.last_error
            ],
        )?;
        Ok(())
    }

    pub fn set_registered_all(&self, registered: bool) -> Result<()> {
        self.conn().execute("UPDATE games SET registered = ?1", [registered])?;
        Ok(())
    }

    pub fn delete_game(&self, id: &str) -> Result<()> {
        let conn = self.conn();
        for table in ["games", "outbox", "file_hashes", "backups"] {
            let column = if table == "games" { "id" } else { "game_id" };
            conn.execute(&format!("DELETE FROM {table} WHERE {column} = ?1"), [id])?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ outbox

    fn outbox_from_row(r: &rusqlite::Row) -> rusqlite::Result<OutboxRow> {
        Ok(OutboxRow {
            game_id: r.get(0)?,
            base_version: r.get(1)?,
            files: from_json(&r.get::<_, String>(2)?)?,
            played_at: r.get(3)?,
            created_at: r.get(4)?,
            blocked: r.get(5)?,
        })
    }

    pub fn outbox(&self, game_id: &str) -> Result<Option<OutboxRow>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT game_id, base_version, files, played_at, created_at, blocked FROM outbox WHERE game_id = ?1",
                [game_id],
                Self::outbox_from_row,
            )
            .optional()?)
    }

    pub fn outbox_all(&self) -> Result<Vec<OutboxRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT game_id, base_version, files, played_at, created_at, blocked FROM outbox ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], Self::outbox_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn put_outbox(&self, row: &OutboxRow) -> Result<()> {
        self.conn().execute(
            "INSERT INTO outbox (game_id, base_version, files, played_at, created_at, blocked)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(game_id) DO UPDATE SET base_version = excluded.base_version, files = excluded.files,
                played_at = excluded.played_at, created_at = excluded.created_at, blocked = excluded.blocked",
            params![row.game_id, row.base_version, to_json(&row.files), row.played_at, row.created_at, row.blocked],
        )?;
        Ok(())
    }

    pub fn delete_outbox(&self, game_id: &str) -> Result<()> {
        self.conn().execute("DELETE FROM outbox WHERE game_id = ?1", [game_id])?;
        Ok(())
    }

    // ------------------------------------------------------------ hash cache

    pub fn cached_hash(&self, game_id: &str, path: &str, size: u64, mtime: i64) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT sha256 FROM file_hashes WHERE game_id = ?1 AND path = ?2 AND size = ?3 AND mtime = ?4",
                params![game_id, path, size as i64, mtime],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn put_hash(&self, game_id: &str, path: &str, size: u64, mtime: i64, sha: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO file_hashes (game_id, path, size, mtime, sha256) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(game_id, path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, sha256 = excluded.sha256",
            params![game_id, path, size as i64, mtime, sha],
        )?;
        Ok(())
    }

    /// Forgets hashes for files that no longer exist.
    pub fn retain_hashes(&self, game_id: &str, paths: &HashSet<&str>) -> Result<()> {
        let conn = self.conn();
        let existing: Vec<String> = {
            let mut stmt = conn.prepare("SELECT path FROM file_hashes WHERE game_id = ?1")?;
            stmt.query_map([game_id], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        for path in existing.iter().filter(|p| !paths.contains(p.as_str())) {
            conn.execute("DELETE FROM file_hashes WHERE game_id = ?1 AND path = ?2", params![game_id, path])?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ backups

    pub fn add_backup(&self, game_id: &str, reason: &str, files: &[FileEntry], now: i64, keep: usize) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO backups (game_id, created_at, reason, files) VALUES (?1, ?2, ?3, ?4)",
            params![game_id, now, reason, to_json(&files)],
        )?;
        let id = conn.last_insert_rowid();
        conn.execute(
            "DELETE FROM backups WHERE game_id = ?1 AND id NOT IN
                (SELECT id FROM backups WHERE game_id = ?1 ORDER BY id DESC LIMIT ?2)",
            params![game_id, keep as i64],
        )?;
        Ok(id)
    }

    fn backup_from_row(r: &rusqlite::Row) -> rusqlite::Result<BackupRow> {
        let files: Vec<FileEntry> = from_json(&r.get::<_, String>(4)?)?;
        Ok(BackupRow {
            info: BackupInfo {
                id: r.get(0)?,
                game_id: r.get(1)?,
                created_at: r.get(2)?,
                reason: r.get(3)?,
                file_count: files.len(),
                total_size: files.iter().map(|f| f.size).sum(),
            },
            files,
        })
    }

    pub fn backups(&self, game_id: &str) -> Result<Vec<BackupRow>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, game_id, created_at, reason, files FROM backups WHERE game_id = ?1 ORDER BY id DESC")?;
        let rows = stmt.query_map([game_id], Self::backup_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn backup(&self, id: i64) -> Result<Option<BackupRow>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, game_id, created_at, reason, files FROM backups WHERE id = ?1",
                [id],
                Self::backup_from_row,
            )
            .optional()?)
    }

    // ------------------------------------------------------------ cache GC

    /// Every blob something still points at; the rest of the cache can go.
    pub fn referenced_blobs(&self) -> Result<HashSet<String>> {
        let mut keep = HashSet::new();
        for g in self.games()? {
            keep.extend(g.base_files.into_iter().map(|f| f.sha256));
            if let Some(m) = g.staged {
                keep.extend(m.files.into_iter().map(|f| f.sha256));
            }
        }
        for o in self.outbox_all()? {
            keep.extend(o.files.into_iter().map(|f| f.sha256));
        }
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT files FROM backups")?;
        for files in stmt.query_map([], |r| r.get::<_, String>(0))? {
            let files: Vec<FileEntry> = from_json(&files?)?;
            keep.extend(files.into_iter().map(|f| f.sha256));
        }
        Ok(keep)
    }
}
