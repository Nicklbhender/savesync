//! Access to a game's save folder. Desktop uses the filesystem directly; Android
//! implements these traits over the Storage Access Framework (content URIs), since
//! scoped storage doesn't allow plain paths.

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use savesync_protocol::valid_save_path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFile {
    /// Relative to the save folder, `/`-separated.
    pub path: String,
    pub size: u64,
    /// Modification time, ms since epoch.
    pub mtime: i64,
}

pub trait SaveStorage: Send + Sync {
    /// Every file in the folder, recursively. Must fail (not return an empty list)
    /// when the folder itself is missing, e.g. an unmounted SD card.
    fn list(&self) -> io::Result<Vec<LocalFile>>;
    fn open_read(&self, path: &str) -> io::Result<Box<dyn Read + Send>>;
    /// Creates or replaces a file so readers never see it half-written.
    fn write_atomic(&self, path: &str, contents: &mut dyn Read) -> io::Result<()>;
    fn remove(&self, path: &str) -> io::Result<()>;
    /// The folder's filesystem path, when it has one (lets desktop watch it).
    fn local_root(&self) -> Option<&Path> {
        None
    }
}

pub trait StorageProvider: Send + Sync {
    fn open(&self, location: &str) -> io::Result<Arc<dyn SaveStorage>>;
}

/// Prefix for in-progress writes; always ignored by scans.
pub const TEMP_PREFIX: &str = ".savesync-";

// ---------------------------------------------------------------- filesystem

/// `location` is an absolute folder path.
#[derive(Default)]
pub struct FsProvider;

impl StorageProvider for FsProvider {
    fn open(&self, location: &str) -> io::Result<Arc<dyn SaveStorage>> {
        let root = PathBuf::from(location);
        if !root.is_absolute() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "save folder must be an absolute path"));
        }
        Ok(Arc::new(FsStorage { root }))
    }
}

pub struct FsStorage {
    root: PathBuf,
}

impl FsStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn resolve(&self, path: &str) -> io::Result<PathBuf> {
        if !valid_save_path(path) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("invalid save path: {path}")));
        }
        Ok(path.split('/').fold(self.root.clone(), |p, c| p.join(c)))
    }

    fn walk(&self, dir: &Path, prefix: &str, out: &mut Vec<LocalFile>) -> io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                tracing::warn!("skipping non-UTF-8 file name in {}", dir.display());
                continue;
            };
            let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                self.walk(&entry.path(), &rel, out)?;
            } else if file_type.is_file() {
                let meta = entry.metadata()?;
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                out.push(LocalFile { path: rel, size: meta.len(), mtime });
            }
            // Symlinks and other special files are skipped.
        }
        Ok(())
    }
}

impl SaveStorage for FsStorage {
    fn list(&self) -> io::Result<Vec<LocalFile>> {
        if !self.root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("save folder not found: {}", self.root.display()),
            ));
        }
        let mut out = Vec::new();
        self.walk(&self.root, "", &mut out)?;
        Ok(out)
    }

    fn open_read(&self, path: &str) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(std::fs::File::open(self.resolve(path)?)?))
    }

    fn write_atomic(&self, path: &str, contents: &mut dyn Read) -> io::Result<()> {
        let dest = self.resolve(path)?;
        let dir = dest.parent().expect("resolved paths have a parent");
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{TEMP_PREFIX}{}.tmp", hex::encode(rand::random::<[u8; 8]>())));
        let result = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            io::copy(contents, &mut file)?;
            file.flush()?;
            file.sync_all()?;
            // Replaces an existing file on Windows too.
            std::fs::rename(&tmp, &dest)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    fn remove(&self, path: &str) -> io::Result<()> {
        let target = self.resolve(path)?;
        match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Tidy up folders the removal emptied, but never the save folder itself.
        let mut dir = target.parent();
        while let Some(d) = dir {
            if d == self.root || std::fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
        Ok(())
    }

    fn local_root(&self) -> Option<&Path> {
        Some(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = FsStorage::new(dir.path());
        s.write_atomic("a/b/save.srm", &mut &b"hello"[..]).unwrap();
        s.write_atomic("top.dat", &mut &b"x"[..]).unwrap();
        let mut files = s.list().unwrap();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a/b/save.srm", "top.dat"]);
        assert_eq!(files[0].size, 5);

        let mut buf = String::new();
        s.open_read("a/b/save.srm").unwrap().read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "hello");

        s.remove("a/b/save.srm").unwrap();
        assert!(!dir.path().join("a").exists(), "emptied folders are removed");
        assert!(dir.path().exists());
        assert!(s.write_atomic("../escape", &mut &b""[..]).is_err());
    }

    #[test]
    fn missing_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let s = FsStorage::new(dir.path().join("nope"));
        assert_eq!(s.list().unwrap_err().kind(), io::ErrorKind::NotFound);
    }
}
