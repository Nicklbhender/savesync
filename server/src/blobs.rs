//! Content-addressed file storage. Each save file is stored once under its SHA-256,
//! so unchanged files are shared between versions and uploads are idempotent.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::{
    error::{AppError, AppResult},
    model::random_hex,
};

#[derive(Clone, Debug)]
pub struct BlobStore {
    root: PathBuf,
    tmp: PathBuf,
    max_bytes: u64,
}

impl BlobStore {
    pub fn open(data_dir: &Path, max_bytes: u64) -> std::io::Result<Self> {
        let root = data_dir.join("blobs");
        let tmp = data_dir.join("tmp");
        std::fs::create_dir_all(&root)?;
        // Leftovers from interrupted uploads.
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        Ok(Self { root, tmp, max_bytes })
    }

    pub fn path(&self, sha: &str) -> PathBuf {
        self.root.join(&sha[..2]).join(sha)
    }

    pub fn size(&self, sha: &str) -> Option<u64> {
        std::fs::metadata(self.path(sha)).ok().map(|m| m.len())
    }

    /// Refreshes a blob's mtime so the orphan sweep won't delete a blob a client
    /// was just told it doesn't need to upload.
    pub fn touch(&self, sha: &str) {
        if let Ok(f) = std::fs::File::options().write(true).open(self.path(sha)) {
            let _ = f.set_modified(SystemTime::now());
        }
    }

    pub fn remove(&self, sha: &str) {
        let _ = std::fs::remove_file(self.path(sha));
    }

    /// Streams an upload to disk, verifying it hashes to `sha`.
    /// Returns `false` if the blob already existed.
    pub async fn put<S, E>(&self, sha: &str, mut body: S) -> AppResult<bool>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let dest = self.path(sha);
        if dest.exists() {
            return Ok(false);
        }
        let tmp_path = self.tmp.join(format!("{}.part", random_hex(8)));
        let result = self.write_verified(sha, &mut body, &tmp_path).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return result.map(|_| false);
        }
        tokio::fs::create_dir_all(dest.parent().expect("blob path has a parent")).await?;
        tokio::fs::rename(&tmp_path, &dest).await?;
        Ok(true)
    }

    async fn write_verified<S, E>(&self, sha: &str, body: &mut S, tmp_path: &Path) -> AppResult<()>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let mut file = tokio::fs::File::create(tmp_path).await?;
        let mut hasher = Sha256::new();
        let mut written: u64 = 0;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| AppError::BadRequest(format!("upload interrupted: {e}")))?;
            written += chunk.len() as u64;
            if written > self.max_bytes {
                return Err(AppError::TooLarge);
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        let actual = hex::encode(hasher.finalize());
        if actual != sha {
            return Err(AppError::BadRequest(format!(
                "content hash {actual} does not match {sha}"
            )));
        }
        Ok(())
    }

    /// Every stored blob with its modification time, for the orphan sweep.
    pub fn list(&self) -> Vec<(String, SystemTime)> {
        let mut out = Vec::new();
        let Ok(shards) = std::fs::read_dir(&self.root) else { return out };
        for shard in shards.flatten() {
            let Ok(entries) = std::fs::read_dir(shard.path()) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                    out.push((name, modified));
                }
            }
        }
        out
    }
}
