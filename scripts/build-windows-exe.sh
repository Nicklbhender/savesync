#!/usr/bin/env bash
# Cross-compiles the desktop app into a portable Windows x64 exe from macOS or Linux:
#
#   scripts/build-windows-exe.sh      →  dist/SaveSync.exe
#
# One-time setup:
#   rustup target add x86_64-pc-windows-msvc
#   cargo install cargo-xwin --locked     (downloads the Windows SDK and C runtime, under their license)
#   llvm-rc, lld-link and llvm-lib from LLVM, on PATH or in $LLVM_BIN
#     (llvm-rc embeds the icon and the manifest the file picker needs)
# (On Windows itself, just run `cargo build --release -p savesync-desktop`.)
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib/env.sh

cargo xwin build --release -p savesync-desktop --target x86_64-pc-windows-msvc
mkdir -p dist
cp target/x86_64-pc-windows-msvc/release/savesync-desktop.exe dist/SaveSync.exe
echo "Built dist/SaveSync.exe ($(du -h dist/SaveSync.exe | cut -f1))"
