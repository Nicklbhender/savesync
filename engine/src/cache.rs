//! Local content-addressed copies of save files. Snapshots are copied here before
//! uploading (so later play can't change a queued upload), downloads land here
//! before importing, and backups live here.

use std::{
    collections::HashSet,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use savesync_protocol::valid_sha256;

use crate::error::{EngineError, Result};

#[derive(Clone, Debug)]
pub struct BlobCache {
    root: PathBuf,
    tmp: PathBuf,
}

impl BlobCache {
    pub fn open(dir: &Path) -> io::Result<Self> {
        let root = dir.join("blobs");
        let tmp = dir.join("tmp");
        std::fs::create_dir_all(&root)?;
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        Ok(Self { root, tmp })
    }

    /// Where a blob lives. Hashes come from the server too, so anything that isn't a
    /// well-formed SHA-256 maps to a path that can't exist instead of escaping the cache.
    pub fn path(&self, sha: &str) -> PathBuf {
        if valid_sha256(sha) {
            self.root.join(&sha[..2]).join(sha)
        } else {
            self.root.join("invalid-hash")
        }
    }

    pub fn has(&self, sha: &str) -> bool {
        valid_sha256(sha) && self.path(sha).is_file()
    }

    pub fn open_read(&self, sha: &str) -> io::Result<std::fs::File> {
        if !valid_sha256(sha) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid file hash"));
        }
        std::fs::File::open(self.path(sha))
    }

    fn temp_path(&self) -> PathBuf {
        self.tmp.join(format!("{}.part", hex::encode(rand::random::<[u8; 8]>())))
    }

    fn commit(&self, tmp: &Path, sha: &str) -> io::Result<()> {
        let dest = self.path(sha);
        std::fs::create_dir_all(dest.parent().expect("blob path has a parent"))?;
        std::fs::rename(tmp, dest)
    }

    /// Copies `src` into the cache, returning its hash and size.
    pub fn ingest(&self, src: &mut dyn Read) -> io::Result<(String, u64)> {
        let tmp = self.temp_path();
        let result = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 64 * 1024];
            let mut size = 0u64;
            loop {
                let n = src.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                file.write_all(&buf[..n])?;
                size += n as u64;
            }
            file.sync_all()?;
            let sha = hex::encode(hasher.finalize());
            if !self.has(&sha) {
                self.commit(&tmp, &sha)?;
            }
            Ok((sha, size))
        })();
        let _ = std::fs::remove_file(&tmp);
        result
    }

    /// Stores a download, verifying it hashes to `expected`.
    pub async fn ingest_stream<S, E>(&self, mut body: S, expected: &str) -> Result<()>
    where
        S: Stream<Item = std::result::Result<Bytes, E>> + Unpin,
        E: Into<EngineError>,
    {
        if !valid_sha256(expected) {
            return Err(EngineError::Other(format!("invalid file hash from server: {expected:?}")));
        }
        let tmp = self.temp_path();
        let result = async {
            let mut file = tokio::fs::File::create(&tmp).await?;
            let mut hasher = Sha256::new();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(Into::into)?;
                hasher.update(&chunk);
                file.write_all(&chunk).await?;
            }
            file.sync_all().await?;
            let actual = hex::encode(hasher.finalize());
            if actual != expected {
                return Err(EngineError::Other(format!("downloaded file hash {actual} != {expected}")));
            }
            self.commit(&tmp, expected)?;
            Ok(())
        }
        .await;
        let _ = tokio::fs::remove_file(&tmp).await;
        result
    }

    /// Deletes every blob not in `keep`. Returns how many were removed.
    pub fn retain(&self, keep: &HashSet<String>) -> usize {
        let mut removed = 0;
        let Ok(shards) = std::fs::read_dir(&self.root) else { return 0 };
        for shard in shards.flatten() {
            let Ok(entries) = std::fs::read_dir(shard.path()) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !keep.contains(&name) && std::fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_dedupes_and_retain_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(dir.path()).unwrap();
        let (a, size) = cache.ingest(&mut &b"save data"[..]).unwrap();
        assert_eq!(size, 9);
        assert_eq!(a, hex::encode(Sha256::digest(b"save data")));
        let (a2, _) = cache.ingest(&mut &b"save data"[..]).unwrap();
        assert_eq!(a, a2);
        let (b, _) = cache.ingest(&mut &b"other"[..]).unwrap();
        assert!(cache.has(&a) && cache.has(&b));

        assert_eq!(cache.retain(&HashSet::from([a.clone()])), 1);
        assert!(cache.has(&a) && !cache.has(&b));
    }

    #[test]
    fn hostile_hashes_stay_inside_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(dir.path()).unwrap();
        for bad in ["../../../etc/passwd", "ab", "", &"Z".repeat(64), "é"] {
            assert!(cache.path(bad).starts_with(dir.path().join("blobs")), "{bad}");
            assert!(!cache.has(bad));
            assert!(cache.open_read(bad).is_err());
        }
    }

    #[tokio::test]
    async fn stream_verifies_hash() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::open(dir.path()).unwrap();
        let good = hex::encode(Sha256::digest(b"abc"));
        let chunks = || futures_util::stream::iter(vec![Ok::<_, EngineError>(Bytes::from_static(b"ab")), Ok(Bytes::from_static(b"c"))]);
        cache.ingest_stream(chunks(), &good).await.unwrap();
        assert!(cache.has(&good));
        let bad = "0".repeat(64);
        assert!(cache.ingest_stream(chunks(), &bad).await.is_err());
        assert!(!cache.has(&bad));
    }
}
