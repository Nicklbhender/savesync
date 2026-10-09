#!/usr/bin/env bash
# Builds the engine's tests for Android (arm64, no desktop features) and runs
# them on a connected device or a running emulator.
#
#   scripts/test-android.sh
#
# Needs: Android SDK + NDK, `rustup target add aarch64-linux-android` and
# `cargo install cargo-ndk`. See scripts/lib/env.sh for how they're found.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib/env.sh

"$ADB" get-state >/dev/null 2>&1 || { echo "No device or emulator connected (start an emulator or plug in a phone with USB debugging on)."; exit 1; }

cargo ndk -t arm64-v8a -P 26 test --no-run -p savesync-engine --no-default-features

DEPS=target/aarch64-linux-android/debug/deps
status=0
for name in savesync_engine sync; do
    bin=$(ls -t "$DEPS"/${name}-* | grep -vE '\.(d|rlib|rmeta|so)$' | head -1)
    "$ADB" push "$bin" "/data/local/tmp/$name-test" >/dev/null
    echo "== $name (on $("$ADB" shell getprop ro.product.model | tr -d '\r'), Android $("$ADB" shell getprop ro.build.version.release | tr -d '\r'))"
    "$ADB" shell "chmod 755 /data/local/tmp/$name-test && cd /data/local/tmp && TMPDIR=/data/local/tmp ./$name-test" || status=1
done
exit $status
