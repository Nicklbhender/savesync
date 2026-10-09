//! Self-update from GitHub Releases.
//!
//! Every few hours the app looks at the latest release. If it's newer, it downloads
//! the asset for this platform plus `SHA256SUMS` and `SHA256SUMS.sig`, checks the
//! signature against the public key built into the app (the private key never
//! leaves the release machine), checks the asset's hash, then swaps itself out and
//! restarts once no game is being played.
//!
//! - Windows: the portable exe is renamed to `.old` (allowed while running) and the
//!   new exe takes its place.
//! - macOS: the `.app` bundle is replaced by the one in the release's `.app.tar.gz`.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::Duration,
};

use futures_util::StreamExt;
use ring::signature::{ED25519, UnparsedPublicKey};
use savesync_engine::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::AsyncWriteExt;

use crate::AppState;

/// Hex Ed25519 public key; the matching private key signs each release's SHA256SUMS.
const UPDATE_PUBLIC_KEY: &str = "826ec84e53fafbb876c2cffe72058955a013773672cf4b608eee0a9636a3dc9f";
/// Overridable at build time for testing against a local fake release.
const RELEASES_API: &str = match option_env!("SAVESYNC_UPDATE_API") {
    Some(url) => url,
    None => "https://api.github.com/repos/Nicklbhender/savesync/releases/latest",
};
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Passed to the new version so it waits for this one to exit (single-instance).
pub const WAIT_PID_ARG: &str = "--updated-wait-pid";
pub const UPDATED_FROM_ARG: &str = "--updated-from";

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Default, Serialize)]
pub struct UpdateStatus {
    pub current: &'static str,
    /// "idle", "checking", "up_to_date", "downloading", "ready", "installing", "error", "unsupported"
    pub state: &'static str,
    pub latest: Option<String>,
    pub message: Option<String>,
}

struct Ready {
    version: String,
    file: PathBuf,
}

pub struct Updater {
    status: Mutex<UpdateStatus>,
    ready: Mutex<Option<Ready>>,
    busy: tokio::sync::Mutex<()>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn platform_asset(version: &str) -> Option<String> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(format!("SaveSync-{version}-windows-x64.exe"))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(format!("SaveSync-{version}-macos-arm64.app.tar.gz"))
    } else {
        None
    }
}

/// `1.2.3` → (1, 2, 3); anything else (e.g. pre-releases) → None.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().trim_start_matches('v').split('.');
    let parsed = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    parts.next().is_none().then_some(parsed)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

fn verify_signature(data: &[u8], sig_hex: &str) -> Result<(), String> {
    let decode = |s: &str| -> Option<Vec<u8>> {
        s.len().is_multiple_of(2).then_some(())?;
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
    };
    let key = decode(UPDATE_PUBLIC_KEY).ok_or("bad built-in key")?;
    let sig = decode(sig_hex.trim()).ok_or("malformed signature")?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(data, &sig)
        .map_err(|_| "the update's signature is not valid; it was not installed".to_string())
}

/// The expected SHA-256 for `name` from a `sha256sum`-style file.
fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

fn http() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .user_agent(format!("SaveSync/{CURRENT}"))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()
        .expect("http client")
}

/// The folder an update is staged in: next to the exe / app bundle, so the final
/// swap is a same-volume rename.
fn install_target() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if cfg!(target_os = "macos") {
        exe.ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .map(Path::to_path_buf)
            .ok_or_else(|| "updates only work for the installed SaveSync.app".to_string())
    } else {
        Ok(exe)
    }
}

impl Updater {
    pub fn new() -> Self {
        Self {
            status: Mutex::new(UpdateStatus { current: CURRENT, state: "idle", ..Default::default() }),
            ready: Mutex::new(None),
            busy: tokio::sync::Mutex::new(()),
        }
    }

    pub fn status(&self) -> UpdateStatus {
        self.status.lock().unwrap().clone()
    }

    fn set(&self, app: &AppHandle, state: &'static str, latest: Option<String>, message: Option<String>) {
        *self.status.lock().unwrap() = UpdateStatus { current: CURRENT, state, latest, message };
        let _ = app.emit("update-status", self.status());
    }

    /// Checks GitHub and downloads + verifies a newer version if there is one.
    pub async fn check(&self, app: &AppHandle) {
        let Ok(_guard) = self.busy.try_lock() else { return };
        if self.ready.lock().unwrap().is_some() {
            return;
        }
        if platform_asset(CURRENT).is_none() || install_target().is_err() {
            self.set(app, "unsupported", None, Some("Automatic updates work in the released app.".into()));
            return;
        }
        self.set(app, "checking", None, None);
        match self.fetch(app).await {
            Ok(None) => self.set(app, "up_to_date", None, None),
            Ok(Some(ready)) => {
                let version = ready.version.clone();
                *self.ready.lock().unwrap() = Some(ready);
                self.set(app, "ready", Some(version), None);
            }
            Err(e) => {
                tracing::warn!("update check failed: {e}");
                self.set(app, "error", None, Some(e));
            }
        }
    }

    async fn fetch(&self, app: &AppHandle) -> Result<Option<Ready>, String> {
        let client = http();
        let resp = client.get(RELEASES_API).send().await.map_err(|e| format!("couldn't reach GitHub: {e}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None); // no releases yet
        }
        let release: Release = resp
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| format!("unexpected response from GitHub: {e}"))?;
        let version = release.tag_name.trim_start_matches('v').to_string();
        if release.draft || release.prerelease || !is_newer(&version, CURRENT) {
            return Ok(None);
        }
        let asset_name = platform_asset(&version).expect("checked by caller");
        let url_of = |name: &str| release.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
        let (Some(asset_url), Some(sums_url), Some(sig_url)) =
            (url_of(&asset_name), url_of("SHA256SUMS"), url_of("SHA256SUMS.sig"))
        else {
            return Err(format!("release {version} doesn't include {asset_name} and its checksums"));
        };

        let sums = client.get(&sums_url).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
        let sums = sums.text().await.map_err(|e| e.to_string())?;
        let sig = client.get(&sig_url).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
        let sig = sig.text().await.map_err(|e| e.to_string())?;
        verify_signature(sums.as_bytes(), &sig)?;
        let expected = expected_hash(&sums, &asset_name).ok_or("the release's checksums don't list this platform")?;

        self.set(app, "downloading", Some(version.clone()), None);
        let target = install_target()?;
        let dir = target.parent().ok_or("no parent folder")?;
        let file = dir.join(format!(".SaveSync-update-{version}.part"));
        let result = async {
            let resp = client.get(&asset_url).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
            let mut out = tokio::fs::File::create(&file).await.map_err(|e| {
                format!("SaveSync can't write to {} to update itself ({e}). Move it to a folder you own.", dir.display())
            })?;
            let mut hasher = Sha256::new();
            let mut body = resp.bytes_stream();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(|e| format!("download interrupted: {e}"))?;
                hasher.update(&chunk);
                out.write_all(&chunk).await.map_err(|e| e.to_string())?;
            }
            out.sync_all().await.map_err(|e| e.to_string())?;
            let actual = hex::encode(hasher.finalize());
            if actual != expected {
                return Err("the downloaded update didn't match its checksum; it was not installed".to_string());
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            let _ = std::fs::remove_file(&file);
            return Err(e);
        }
        Ok(Some(Ready { version, file }))
    }

    /// Swaps in the downloaded version and restarts into it.
    pub fn install(&self, app: &AppHandle) -> Result<(), String> {
        let Some(ready) = self.ready.lock().unwrap().take() else { return Err("no update is ready".into()) };
        self.set(app, "installing", Some(ready.version.clone()), None);
        let result = swap_in(&ready.file).and_then(|mut relaunch| {
            relaunch
                .arg(WAIT_PID_ARG)
                .arg(std::process::id().to_string())
                .arg(UPDATED_FROM_ARG)
                .arg(CURRENT)
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("couldn't restart SaveSync: {e}"))
        });
        match result {
            Ok(()) => {
                app.exit(0);
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&ready.file);
                self.set(app, "error", Some(ready.version), Some(e.clone()));
                Err(e)
            }
        }
    }
}

/// Replaces the installed app with the downloaded one; returns the command that
/// starts the new version.
#[cfg(target_os = "windows")]
fn swap_in(download: &Path) -> Result<Command, String> {
    let exe = install_target()?;
    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    // A running exe can be renamed (not overwritten), so move it aside first.
    std::fs::rename(&exe, &old).map_err(|e| format!("couldn't replace {}: {e}", exe.display()))?;
    if let Err(e) = std::fs::rename(download, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("couldn't install the update: {e}"));
    }
    Ok(Command::new(&exe))
}

#[cfg(target_os = "macos")]
fn swap_in(download: &Path) -> Result<Command, String> {
    let bundle = install_target()?;
    let dir = bundle.parent().ok_or("no parent folder")?;
    let staging = dir.join(format!(".SaveSync-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let status = Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(download)
        .arg("-C")
        .arg(&staging)
        .status()
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(download);
    let new_bundle = staging.join("SaveSync.app");
    if !status.success() || !new_bundle.join("Contents/MacOS").is_dir() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("the update archive is damaged".into());
    }
    let old = dir.join(format!(".SaveSync-old-{}.app", std::process::id()));
    std::fs::rename(&bundle, &old).map_err(|e| format!("couldn't replace {}: {e}", bundle.display()))?;
    if let Err(e) = std::fs::rename(&new_bundle, &bundle) {
        let _ = std::fs::rename(&old, &bundle);
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("couldn't install the update: {e}"));
    }
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_dir_all(&old);
    let mut cmd = Command::new("/usr/bin/open");
    cmd.arg("-n").arg(&bundle).arg("--args");
    Ok(cmd)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn swap_in(_download: &Path) -> Result<Command, String> {
    Err("automatic updates aren't supported on this platform".into())
}

/// Removes leftovers from a previous update.
pub fn clean_up_previous() {
    let Ok(target) = install_target() else { return };
    if cfg!(target_os = "windows") {
        let _ = std::fs::remove_file(target.with_extension("exe.old"));
    }
    if let Some(dir) = target.parent()
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".SaveSync-update-") || name.starts_with(".SaveSync-old-") {
                let path = entry.path();
                let _ = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
            }
        }
    }
}

/// When started by an update, waits for the previous version to exit so the
/// single-instance check doesn't hand off to it.
pub fn wait_for_previous_instance() {
    let args: Vec<String> = std::env::args().collect();
    let Some(pid) = args.iter().position(|a| a == WAIT_PID_ARG).and_then(|i| args.get(i + 1)).and_then(|p| p.parse::<u32>().ok())
    else {
        return;
    };
    let pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    for _ in 0..100 {
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        if sys.process(pid).is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn updated_from() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == UPDATED_FROM_ARG).and_then(|i| args.get(i + 1)).cloned()
}

/// Checks shortly after start and every few hours; installs when no game is running.
pub async fn run(app: AppHandle, engine: Engine) {
    tokio::time::sleep(Duration::from_secs(20)).await;
    let mut last_check = None::<std::time::Instant>;
    loop {
        let updater = &app.state::<AppState>().updater;
        if last_check.is_none_or(|t| t.elapsed() >= CHECK_INTERVAL) {
            updater.check(&app).await;
            last_check = Some(std::time::Instant::now());
        }
        let ready = updater.status().state == "ready";
        let playing = engine.games().map(|g| g.iter().any(|g| g.in_session)).unwrap_or(true);
        if ready
            && !playing
            && let Err(e) = updater.install(&app)
        {
            tracing::warn!("update install failed: {e}");
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9.9", "1.0.0"));
        assert!(!is_newer("1.1.0-beta", "1.0.0"), "pre-releases are skipped");
        assert!(!is_newer("garbage", "1.0.0"));
    }

    #[test]
    fn checksum_lookup() {
        let sums = "abc123  SaveSync-1.0.1-windows-x64.exe\nDEF456 *SaveSync-1.0.1-macos-arm64.app.tar.gz\n";
        assert_eq!(expected_hash(sums, "SaveSync-1.0.1-windows-x64.exe").as_deref(), Some("abc123"));
        assert_eq!(expected_hash(sums, "SaveSync-1.0.1-macos-arm64.app.tar.gz").as_deref(), Some("def456"));
        assert_eq!(expected_hash(sums, "other"), None);
    }

    #[test]
    fn rejects_bad_signatures() {
        assert!(verify_signature(b"data", &"00".repeat(64)).is_err());
        assert!(verify_signature(b"data", "not hex").is_err());
    }
}
