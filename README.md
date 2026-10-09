# SaveSync

Self-hosted game save sync between Windows, macOS and Android, using your own server (for example a NAS).

**[Download the latest release](https://github.com/Nicklbhender/savesync/releases/latest)**: Windows (portable exe),
macOS (dmg) and Android (apk). The apps update themselves when a new release is published.
When a game closes on one device, its save is uploaded; every other device gets it
automatically, including devices that were asleep or offline when it happened.

This repo holds the **server**, the shared **sync engine**, the **desktop app**
(Windows/macOS, Tauri) and the **Android app** (Kotlin + the same engine).

```
 Windows / macOS app ──┐   upload on game exit         ┌── WebSocket: instant notice
                       ├──────────────► SaveSync ──────┤
 Android app ──────────┘   (versioned, conflict-safe)  └── ntfy (UnifiedPush): wakes phones,
                                                            holds notices while they're off
```

## How it stays correct

- **Versioned commits.** Each upload names the version it was based on. If the server
  has moved on (you played on two devices while offline), the upload is rejected as a
  **conflict** instead of overwriting, and the app asks which save to keep.
- **Delivery tracking.** The server records which version each device has downloaded
  and which it has imported. Anything unacknowledged stays *pending* and is delivered
  the next time the device connects, so a phone that was off when you saved on the PC
  gets the save once it wakes.
- **History.** The last N versions per game are kept (default 20) for rollback. File
  contents are stored once by hash, so history is cheap.
- **Atomic.** A save made of several files becomes visible all at once, never half-uploaded.

## Layout

```
docker-compose.yml   Docker Compose project for the server (server + ntfy)
protocol/            Wire types shared by server and engine
server/              Rust server (axum + SQLite)
engine/              Client sync engine shared by the desktop and Android apps
desktop/             Desktop app (Tauri 2; plain HTML/CSS/JS UI in desktop/ui)
ffi/                 The engine exposed to Kotlin (UniFFI) for Android
android/             Android app (Jetpack Compose, foreground service, SAF folders)
```

## Building from source

This is a Cargo workspace. You need a recent stable [Rust](https://rustup.rs) toolchain.
From the repository root:

```bash
cargo test --workspace            # all tests
cargo run -p savesync-desktop     # the desktop app
```

The desktop app is built with Tauri; see Tauri's prerequisites for your platform.

**Android** (needs the Android SDK and NDK, a JDK 17+, `rustup target add aarch64-linux-android`
and `cargo install cargo-ndk`):

```bash
scripts/build-android.sh           # debug APK
scripts/build-android.sh install   # ...and install it on a connected device or emulator
scripts/test-android.sh            # run the engine's tests on a device or emulator
```

**Windows from macOS or Linux** (needs `rustup target add x86_64-pc-windows-msvc`,
`cargo install cargo-xwin`, and LLVM's `llvm-rc`, `lld-link` and `llvm-lib`). On Windows
itself, `cargo build --release -p savesync-desktop` is enough.

```bash
scripts/build-windows-exe.sh       # portable dist/SaveSync.exe
```

The scripts find the Android SDK, NDK and JDK in their usual places; see
`scripts/lib/env.sh` for the environment variables that override them.

### Releases

`scripts/release.sh` builds all three apps, signs the update checksums, tags the
version and publishes it to GitHub Releases. It runs on macOS and needs the update
signing key and Android keystore described at the top of the script. Write
the release notes to `dist/release-notes-<version>.md` first:

```bash
scripts/release.sh 1.2.3 --dry-run   # build only
scripts/release.sh 1.2.3
```

### Server

To run it for development:

```bash
SAVESYNC_ENROLL_KEY=dev-key-at-least-16-chars cargo run -p savesync-server
```

Server configuration (environment variables):

| Variable | Default | |
|---|---|---|
| `SAVESYNC_ENROLL_KEY` | required | Secret for adding/removing devices (16+ chars) |
| `SAVESYNC_BIND` | `0.0.0.0:8420` | Listen address |
| `SAVESYNC_DATA_DIR` | `./data` (`/data` in Docker) | Database and file contents |
| `SAVESYNC_KEEP_VERSIONS` | `20` | Versions kept per game |
| `SAVESYNC_MAX_FILE_MB` | `1024` | Largest single save file |
| `SAVESYNC_PUSH_REWRITE` | none | `public-url=internal-url` for reaching ntfy from inside Docker |
| `RUST_LOG` | `info,tower_http=warn` | Log level |
