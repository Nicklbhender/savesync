#!/usr/bin/env bash
# Builds the Android app:
#   1. the Rust engine (ffi/) for arm64 phones, into the app's jniLibs
#   2. the Kotlin bindings for it (UniFFI)
#   3. the APK, with Gradle
#
#   scripts/build-android.sh            # debug APK
#   scripts/build-android.sh install    # ...and install it on the connected device/emulator
#
# Needs: Android SDK + NDK, a JDK 17+, `rustup target add aarch64-linux-android`
# and `cargo install cargo-ndk`. See scripts/lib/env.sh for how they're found.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib/env.sh

echo "== Rust engine (arm64-v8a)"
cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build -p savesync-ffi --release

echo "== Kotlin bindings"
# Generated from an unstripped host build: release builds strip the metadata UniFFI reads.
cargo build -q -p savesync-ffi --lib
for HOST_LIB in target/debug/libsavesync_ffi.dylib target/debug/libsavesync_ffi.so; do
    [ -f "$HOST_LIB" ] && break
done
cargo run -q -p savesync-ffi --bin uniffi-bindgen -- generate \
    --library "$HOST_LIB" --language kotlin --no-format --out-dir android/app/src/main/java

echo "== APK"
cd android
./gradlew --quiet assembleDebug
APK=app/build/outputs/apk/debug/app-debug.apk
echo "Built android/$APK ($(du -h "$APK" | cut -f1))"

if [ "${1:-}" = "install" ]; then
    "$ADB" install -r "$APK"
fi
