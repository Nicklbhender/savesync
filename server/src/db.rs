//! SQLite storage. A single connection behind a mutex is plenty for a handful of
//! devices, and it also serializes commits against blob garbage collection.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::{
    blobs::BlobStore,
    error::{AppError, AppResult},
    model::{AckState, DeviceInfo, FileEntry, GameStatus, Manifest, VersionInfo, same_contents},
};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE devices (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    platform      TEXT NOT NULL,
    token_hash    TEXT NOT NULL UNIQUE,
    push_endpoint TEXT,
    created_at    INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL,
    revoked       INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE games (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    current_version INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL
);

CREATE TABLE versions (
    game_id      TEXT NOT NULL REFERENCES games(id),
    version      INTEGER NOT NULL,
    base_version INTEGER NOT NULL,
    device_id    TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    played_at    INTEGER,
    total_size   INTEGER NOT NULL,
    file_count   INTEGER NOT NULL,
    note         TEXT,
    PRIMARY KEY (game_id, version)
);

CREATE TABLE version_files (
    game_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    path    TEXT NOT NULL,
    sha256  TEXT NOT NULL,
    size    INTEGER NOT NULL,
    mtime   INTEGER,
    PRIMARY KEY (game_id, version, path)
);
CREATE INDEX version_files_sha256 ON version_files(sha256);

-- One row per (device, game) the device syncs. Delivery tracking lives here.
CREATE TABLE subscriptions (
    device_id         TEXT NOT NULL REFERENCES devices(id),
    game_id           TEXT NOT NULL REFERENCES games(id),
    delivered_version INTEGER NOT NULL DEFAULT 0,
    applied_version   INTEGER NOT NULL DEFAULT 0,
    last_state        TEXT,
    updated_at        INTEGER NOT NULL,
    PRIMARY KEY (device_id, game_id)
);
"#;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if version > SCHEMA_VERSION {
            anyhow::bail!("database schema v{version} is newer than this server (v{SCHEMA_VERSION})");
        }
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Runs `f` with the connection on a blocking thread.
    pub async fn call<F, R>(&self, f: F) -> AppResult<R>
    where
        F: FnOnce(&mut Connection) -> AppResult<R> + Send + 'static,
        R: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            f(&mut guard)
        })
        .await
        .map_err(|e| AppError::Internal(format!("db task: {e}")))?
    }
}

// ---------------------------------------------------------------- devices

pub struct AuthedDevice {
    pub id: String,
    pub name: String,
    pub last_seen_at: i64,
}

pub fn create_device(c: &Connection, id: &str, name: &str, platform: &str, token_hash: &str, now: i64) -> AppResult<()> {
    c.execute(
        "INSERT INTO devices (id, name, platform, token_hash, created_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![id, name, platform, token_hash, now],
    )?;
    Ok(())
}

pub fn device_by_token(c: &Connection, token_hash: &str) -> AppResult<Option<AuthedDevice>> {
    Ok(c.query_row(
        "SELECT id, name, last_seen_at FROM devices WHERE token_hash = ?1 AND revoked = 0",
        [token_hash],
        |r| Ok(AuthedDevice { id: r.get(0)?, name: r.get(1)?, last_seen_at: r.get(2)? }),
    )
    .optional()?)
}

pub fn touch_device(c: &Connection, id: &str, now: i64) -> AppResult<()> {
    c.execute("UPDATE devices SET last_seen_at = ?2 WHERE id = ?1", params![id, now])?;
    Ok(())
}

pub fn set_push_endpoint(c: &Connection, id: &str, endpoint: Option<&str>) -> AppResult<()> {
    c.execute("UPDATE devices SET push_endpoint = ?2 WHERE id = ?1", params![id, endpoint])?;
    Ok(())
}

pub fn list_devices(c: &Connection) -> AppResult<Vec<DeviceInfo>> {
    let mut stmt = c.prepare(
        "SELECT id, name, platform, push_endpoint IS NOT NULL, created_at, last_seen_at, revoked
         FROM devices ORDER BY created_at",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(DeviceInfo {
            id: r.get(0)?,
            name: r.get(1)?,
            platform: r.get(2)?,
            has_push: r.get(3)?,
            created_at: r.get(4)?,
            last_seen_at: r.get(5)?,
            revoked: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn device_info(c: &Connection, id: &str) -> AppResult<DeviceInfo> {
    list_devices(c)?
        .into_iter()
        .find(|d| d.id == id)
        .ok_or(AppError::NotFound("device"))
}

/// Revoked devices keep their rows so version history still shows who uploaded what.
pub fn revoke_device(c: &Connection, id: &str) -> AppResult<bool> {
    let changed = c.execute(
        "UPDATE devices SET revoked = 1, push_endpoint = NULL WHERE id = ?1 AND revoked = 0",
        [id],
    )?;
    c.execute("DELETE FROM subscriptions WHERE device_id = ?1", [id])?;
    Ok(changed > 0)
}

// ---------------------------------------------------------------- games

/// Creates the game if needed and subscribes the device to it. A device that
/// subscribes to a game that already has saves sees it as pending right away.
pub fn subscribe(c: &mut Connection, game_id: &str, name: &str, device_id: &str, now: i64) -> AppResult<GameStatus> {
    let tx = c.transaction()?;
    tx.execute(
        "INSERT INTO games (id, name, created_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name",
        params![game_id, name, now],
    )?;
    tx.execute(
        "INSERT INTO subscriptions (device_id, game_id, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(device_id, game_id) DO NOTHING",
        params![device_id, game_id, now],
    )?;
    tx.commit()?;
    game_status(c, device_id, game_id)
}

pub fn unsubscribe(c: &Connection, device_id: &str, game_id: &str) -> AppResult<bool> {
    Ok(c.execute(
        "DELETE FROM subscriptions WHERE device_id = ?1 AND game_id = ?2",
        params![device_id, game_id],
    )? > 0)
}

const STATUS_SELECT: &str = "
    SELECT g.id, g.name, g.current_version,
           s.device_id IS NOT NULL, COALESCE(s.delivered_version, 0), COALESCE(s.applied_version, 0), s.last_state,
           v.game_id, v.version, v.base_version, v.device_id, d.name, v.created_at, v.played_at,
           v.total_size, v.file_count, v.note
    FROM games g
    LEFT JOIN subscriptions s ON s.game_id = g.id AND s.device_id = ?1
    LEFT JOIN versions v ON v.game_id = g.id AND v.version = g.current_version
    LEFT JOIN devices d ON d.id = v.device_id";

fn status_from_row(r: &Row) -> rusqlite::Result<GameStatus> {
    let current_version: i64 = r.get(2)?;
    let subscribed: bool = r.get(3)?;
    let delivered_version: i64 = r.get(4)?;
    let latest = match r.get::<_, Option<String>>(7)? {
        Some(_) => Some(version_from_row(r, 7)?),
        None => None,
    };
    Ok(GameStatus {
        id: r.get(0)?,
        name: r.get(1)?,
        current_version,
        latest,
        subscribed,
        delivered_version,
        applied_version: r.get(5)?,
        last_state: r.get(6)?,
        pending: subscribed && current_version > delivered_version,
    })
}

/// Reads a `VersionInfo` from 10 consecutive columns starting at `at`:
/// game_id, version, base_version, device_id, device_name, created_at, played_at,
/// total_size, file_count, note.
fn version_from_row(r: &Row, at: usize) -> rusqlite::Result<VersionInfo> {
    Ok(VersionInfo {
        game_id: r.get(at)?,
        version: r.get(at + 1)?,
        base_version: r.get(at + 2)?,
        device_id: r.get(at + 3)?,
        device_name: r.get(at + 4)?,
        created_at: r.get(at + 5)?,
        played_at: r.get(at + 6)?,
        total_size: r.get(at + 7)?,
        file_count: r.get(at + 8)?,
        note: r.get(at + 9)?,
    })
}

pub fn game_statuses(c: &Connection, device_id: &str) -> AppResult<Vec<GameStatus>> {
    let mut stmt = c.prepare(&format!("{STATUS_SELECT} ORDER BY g.name COLLATE NOCASE"))?;
    let rows = stmt.query_map([device_id], status_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn game_status(c: &Connection, device_id: &str, game_id: &str) -> AppResult<GameStatus> {
    c.query_row(&format!("{STATUS_SELECT} WHERE g.id = ?2"), params![device_id, game_id], status_from_row)
        .optional()?
        .ok_or(AppError::NotFound("game"))
}

pub fn pending(c: &Connection, device_id: &str) -> AppResult<Vec<GameStatus>> {
    Ok(game_statuses(c, device_id)?.into_iter().filter(|g| g.pending).collect())
}

// ---------------------------------------------------------------- versions

const VERSION_SELECT: &str = "
    SELECT v.game_id, v.version, v.base_version, v.device_id, d.name, v.created_at, v.played_at,
           v.total_size, v.file_count, v.note
    FROM versions v LEFT JOIN devices d ON d.id = v.device_id";

fn current_version(c: &Connection, game_id: &str) -> AppResult<i64> {
    c.query_row("SELECT current_version FROM games WHERE id = ?1", [game_id], |r| r.get(0))
        .optional()?
        .ok_or(AppError::NotFound("game"))
}

pub fn list_versions(c: &Connection, game_id: &str) -> AppResult<Vec<VersionInfo>> {
    current_version(c, game_id)?;
    let mut stmt = c.prepare(&format!("{VERSION_SELECT} WHERE v.game_id = ?1 ORDER BY v.version DESC"))?;
    let rows = stmt.query_map([game_id], |r| version_from_row(r, 0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn version_info(c: &Connection, game_id: &str, version: i64) -> AppResult<Option<VersionInfo>> {
    Ok(c.query_row(
        &format!("{VERSION_SELECT} WHERE v.game_id = ?1 AND v.version = ?2"),
        params![game_id, version],
        |r| version_from_row(r, 0),
    )
    .optional()?)
}

fn version_files(c: &Connection, game_id: &str, version: i64) -> AppResult<Vec<FileEntry>> {
    let mut stmt = c.prepare(
        "SELECT path, sha256, size, mtime FROM version_files WHERE game_id = ?1 AND version = ?2 ORDER BY path",
    )?;
    let rows = stmt.query_map(params![game_id, version], |r| {
        Ok(FileEntry { path: r.get(0)?, sha256: r.get(1)?, size: r.get(2)?, mtime: r.get(3)? })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The manifest for `version`, or for the current version when `None`.
pub fn manifest(c: &Connection, game_id: &str, version: Option<i64>) -> AppResult<Manifest> {
    let version = match version {
        Some(v) => v,
        None => current_version(c, game_id)?,
    };
    let info = version_info(c, game_id, version)?.ok_or(AppError::NotFound("version"))?;
    let files = version_files(c, game_id, version)?;
    Ok(Manifest { info, files })
}

pub struct NewVersion {
    pub base_version: i64,
    pub files: Vec<FileEntry>,
    pub played_at: Option<i64>,
    pub note: Option<String>,
}

pub struct Recipient {
    pub device_id: String,
    pub push_endpoint: Option<String>,
}

pub struct CommitOutcome {
    pub manifest: Manifest,
    pub game_name: String,
    /// False when the upload matched the current version exactly, so nothing new was stored.
    pub created: bool,
    /// Other subscribed devices that should hear about the new version.
    pub recipients: Vec<Recipient>,
}

/// Stores a new version if `base_version` is still the game's current version.
///
/// - Upload identical to the current version: no new version; the device is
///   marked as up to date (avoids false conflicts when two devices already match).
/// - `base_version` is stale: `AppError::Conflict`, so the client can ask the user.
pub fn commit(
    c: &mut Connection,
    blobs: &BlobStore,
    keep_versions: u32,
    game_id: &str,
    device_id: &str,
    req: NewVersion,
    now: i64,
) -> AppResult<CommitOutcome> {
    let mut missing = Vec::new();
    for f in &req.files {
        match blobs.size(&f.sha256) {
            None => missing.push(f.sha256.clone()),
            Some(size) if size as i64 != f.size => {
                return Err(AppError::BadRequest(format!(
                    "{}: size {} does not match stored content ({size} bytes)",
                    f.path, f.size
                )));
            }
            Some(_) => {}
        }
    }
    if !missing.is_empty() {
        missing.sort();
        missing.dedup();
        return Err(AppError::MissingBlobs(missing));
    }

    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (current, game_name): (i64, String) = tx
        .query_row("SELECT current_version, name FROM games WHERE id = ?1", [game_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?
        .ok_or(AppError::NotFound("game"))?;

    if current > 0 && same_contents(&version_files(&tx, game_id, current)?, &req.files) {
        mark_up_to_date(&tx, device_id, game_id, current, "uploaded", now)?;
        tx.commit()?;
        return Ok(CommitOutcome {
            manifest: manifest(c, game_id, Some(current))?,
            game_name,
            created: false,
            recipients: Vec::new(),
        });
    }

    if req.base_version != current {
        let latest = version_info(&tx, game_id, current)?.map(Box::new);
        return Err(AppError::Conflict { current_version: current, latest });
    }

    let version = current + 1;
    let total_size: i64 = req.files.iter().map(|f| f.size).sum();
    tx.execute(
        "INSERT INTO versions (game_id, version, base_version, device_id, created_at, played_at, total_size, file_count, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            game_id,
            version,
            req.base_version,
            device_id,
            now,
            req.played_at,
            total_size,
            req.files.len() as i64,
            req.note
        ],
    )?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO version_files (game_id, version, path, sha256, size, mtime) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for f in &req.files {
            insert.execute(params![game_id, version, f.path, f.sha256, f.size, f.mtime])?;
        }
    }
    tx.execute("UPDATE games SET current_version = ?2 WHERE id = ?1", params![game_id, version])?;
    mark_up_to_date(&tx, device_id, game_id, version, "uploaded", now)?;

    let recipients = {
        let mut stmt = tx.prepare(
            "SELECT s.device_id, d.push_endpoint FROM subscriptions s JOIN devices d ON d.id = s.device_id
             WHERE s.game_id = ?1 AND s.device_id != ?2 AND d.revoked = 0",
        )?;
        let rows = stmt.query_map(params![game_id, device_id], |r| {
            Ok(Recipient { device_id: r.get(0)?, push_endpoint: r.get(1)? })
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let pruned = prune(&tx, game_id, keep_versions)?;
    tx.commit()?;

    // Still holding the connection lock, so no commit can start referencing these meanwhile.
    for sha in pruned {
        if !blob_referenced(c, &sha)? {
            blobs.remove(&sha);
        }
    }

    Ok(CommitOutcome {
        manifest: manifest(c, game_id, Some(version))?,
        game_name,
        created: true,
        recipients,
    })
}

fn mark_up_to_date(c: &Connection, device_id: &str, game_id: &str, version: i64, state: &str, now: i64) -> AppResult<()> {
    c.execute(
        "INSERT INTO subscriptions (device_id, game_id, delivered_version, applied_version, last_state, updated_at)
         VALUES (?1, ?2, ?3, ?3, ?4, ?5)
         ON CONFLICT(device_id, game_id) DO UPDATE SET
            delivered_version = max(delivered_version, excluded.delivered_version),
            applied_version   = max(applied_version, excluded.applied_version),
            last_state = excluded.last_state, updated_at = excluded.updated_at",
        params![device_id, game_id, version, state, now],
    )?;
    Ok(())
}

/// Deletes all but the newest `keep` versions; returns hashes that may now be unreferenced.
fn prune(c: &Connection, game_id: &str, keep: u32) -> AppResult<Vec<String>> {
    let cutoff: Option<i64> = c
        .query_row(
            "SELECT version FROM versions WHERE game_id = ?1 ORDER BY version DESC LIMIT 1 OFFSET ?2",
            params![game_id, keep],
            |r| r.get(0),
        )
        .optional()?;
    let Some(cutoff) = cutoff else { return Ok(Vec::new()) };
    let shas = {
        let mut stmt =
            c.prepare("SELECT DISTINCT sha256 FROM version_files WHERE game_id = ?1 AND version <= ?2")?;
        let rows = stmt.query_map(params![game_id, cutoff], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    c.execute("DELETE FROM version_files WHERE game_id = ?1 AND version <= ?2", params![game_id, cutoff])?;
    c.execute("DELETE FROM versions WHERE game_id = ?1 AND version <= ?2", params![game_id, cutoff])?;
    Ok(shas)
}

pub fn blob_referenced(c: &Connection, sha: &str) -> AppResult<bool> {
    Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM version_files WHERE sha256 = ?1)", [sha], |r| r.get(0))?)
}

// ---------------------------------------------------------------- delivery

/// Records that a device received, imported or declined a version. Any of these
/// clears the "pending" flag for that version.
pub fn ack(c: &Connection, device_id: &str, game_id: &str, version: i64, state: AckState, now: i64) -> AppResult<GameStatus> {
    let current = current_version(c, game_id)?;
    if version < 1 || version > current {
        return Err(AppError::BadRequest(format!("version must be between 1 and {current}")));
    }
    let changed = c.execute(
        "UPDATE subscriptions SET
            delivered_version = max(delivered_version, ?3),
            applied_version = CASE WHEN ?4 = 'applied' THEN max(applied_version, ?3) ELSE applied_version END,
            last_state = ?4, updated_at = ?5
         WHERE device_id = ?1 AND game_id = ?2",
        params![device_id, game_id, version, state.as_str(), now],
    )?;
    if changed == 0 {
        return Err(AppError::NotFound("subscription"));
    }
    game_status(c, device_id, game_id)
}
