use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, bail};

/// Server configuration, read from `SAVESYNC_*` environment variables.
#[derive(Clone, Debug)]
pub struct Config {
    /// Address to listen on. `SAVESYNC_BIND`, default `0.0.0.0:8420`.
    pub bind: SocketAddr,
    /// Where the database and blobs live. `SAVESYNC_DATA_DIR`, default `./data`.
    pub data_dir: PathBuf,
    /// Shared secret used to enroll new devices and for admin calls. `SAVESYNC_ENROLL_KEY`, required.
    pub enroll_key: String,
    /// How many versions of each game to keep. `SAVESYNC_KEEP_VERSIONS`, default 20.
    pub keep_versions: u32,
    /// Largest single save file accepted, in bytes. `SAVESYNC_MAX_FILE_MB`, default 1024.
    pub max_file_bytes: u64,
    /// Rewrites push endpoint URLs before posting, as `from=to`. Lets the server reach
    /// ntfy by its container name when phones know it by a public or Tailscale URL,
    /// e.g. `http://your-nas:8421=http://ntfy`. `SAVESYNC_PUSH_REWRITE`, optional.
    pub push_rewrite: Option<(String, String)>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = env_or("SAVESYNC_BIND", "0.0.0.0:8420")
            .parse()
            .context("SAVESYNC_BIND must be host:port")?;
        let data_dir = PathBuf::from(env_or("SAVESYNC_DATA_DIR", "./data"));
        let enroll_key = std::env::var("SAVESYNC_ENROLL_KEY").unwrap_or_default();
        if enroll_key.len() < 16 {
            bail!("SAVESYNC_ENROLL_KEY must be set to a secret of at least 16 characters");
        }
        let keep_versions: u32 = env_or("SAVESYNC_KEEP_VERSIONS", "20")
            .parse()
            .context("SAVESYNC_KEEP_VERSIONS must be a number")?;
        let max_file_mb: u64 = env_or("SAVESYNC_MAX_FILE_MB", "1024")
            .parse()
            .context("SAVESYNC_MAX_FILE_MB must be a number")?;
        let push_rewrite = match std::env::var("SAVESYNC_PUSH_REWRITE").ok().filter(|v| !v.is_empty()) {
            None => None,
            Some(v) => {
                let (from, to) = v
                    .split_once('=')
                    .context("SAVESYNC_PUSH_REWRITE must look like http://public-url=http://internal-url")?;
                Some((from.trim().to_string(), to.trim().to_string()))
            }
        };
        Ok(Self {
            bind,
            data_dir,
            enroll_key,
            keep_versions: keep_versions.max(1),
            max_file_bytes: max_file_mb * 1024 * 1024,
            push_rewrite,
        })
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}
