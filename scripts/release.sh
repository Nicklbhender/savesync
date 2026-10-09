#!/usr/bin/env bash
# Builds and publishes a SaveSync release to GitHub:
#
#   scripts/release.sh 1.2.3            # build, sign, tag, publish
#   scripts/release.sh 1.2.3 --dry-run  # build and sign only (dist/release-1.2.3)
#
# Produces, in dist/release-<version>/:
#   SaveSync-<v>-macos-arm64.dmg            first install on a Mac
#   SaveSync-<v>-macos-arm64.app.tar.gz     what the Mac app downloads to update itself
#   SaveSync-<v>-windows-x64.exe            portable Windows app (also its own updater asset)
#   SaveSync-<v>-android.apk                Android app (release-signed)
#   SHA256SUMS, SHA256SUMS.sig              checksums, signed with the update key
#
# Runs on macOS (it builds the macOS app and cross-compiles the others). Needs
# the toolchains from scripts/build-android.sh and scripts/build-windows-exe.sh,
# `cargo tauri`, `gh` logged in, and two private keys kept outside the repo:
#   SAVESYNC_UPDATE_KEY        update signing key (default ~/.savesync/update-signing.pk8),
#                              created with tools/release-sign
#   SAVESYNC_ANDROID_SIGNING   Android keystore properties (default
#                              ~/.savesync/android-release.properties; see android/app/build.gradle.kts)
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:?usage: scripts/release.sh <version> [--dry-run]}"
DRY_RUN="${2:-}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "version must look like 1.2.3"; exit 1; }

source scripts/lib/env.sh
SIGN_KEY="${SAVESYNC_UPDATE_KEY:-$HOME/.savesync/update-signing.pk8}"
ANDROID_SIGNING="${SAVESYNC_ANDROID_SIGNING:-$HOME/.savesync/android-release.properties}"
export SAVESYNC_ANDROID_SIGNING="$ANDROID_SIGNING"
SIGNER=tools/release-sign/target/release/release-sign
OUT="dist/release-$VERSION"

[ -f "$SIGN_KEY" ] || { echo "missing update signing key $SIGN_KEY"; exit 1; }
[ -f "$ANDROID_SIGNING" ] || { echo "missing Android signing config $ANDROID_SIGNING"; exit 1; }
if [ -z "$DRY_RUN" ]; then
    git diff --quiet && git diff --cached --quiet || { echo "commit or stash your changes first"; exit 1; }
    ! git rev-parse "v$VERSION" >/dev/null 2>&1 || { echo "tag v$VERSION already exists"; exit 1; }
fi

step() { printf '\n== %s\n' "$*"; }

step "Version $VERSION"
sed -i '' -E "s/^version = \"[0-9]+\.[0-9]+\.[0-9]+\"/version = \"$VERSION\"/" Cargo.toml
sed -i '' -E "s/(as String\?\) \?: )\"[0-9]+\.[0-9]+\.[0-9]+\"/\1\"$VERSION\"/" android/app/build.gradle.kts
cargo update -q --workspace

step "Tests"
cargo test -q --workspace 2>&1 | grep -E "^test result|FAILED|panicked" | sort | uniq -c

rm -rf "$OUT" && mkdir -p "$OUT"

step "macOS app"
(cd desktop && cargo tauri build --bundles app 2>&1 | grep -E "Finished|Error|error" || true)
APP=target/release/bundle/macos/SaveSync.app
[ "$(defaults read "$PWD/$APP/Contents/Info.plist" CFBundleShortVersionString)" = "$VERSION" ] || { echo "macOS build has the wrong version"; exit 1; }
tar -czf "$OUT/SaveSync-$VERSION-macos-arm64.app.tar.gz" -C target/release/bundle/macos SaveSync.app
DMG_DIR=$(mktemp -d)
cp -R "$APP" "$DMG_DIR/" && ln -s /Applications "$DMG_DIR/Applications"
hdiutil create -quiet -volname "SaveSync $VERSION" -srcfolder "$DMG_DIR" -ov -format UDZO "$OUT/SaveSync-$VERSION-macos-arm64.dmg"
rm -rf "$DMG_DIR"

step "Windows app"
cargo xwin build -q --release -p savesync-desktop --target x86_64-pc-windows-msvc 2>&1 | grep -E "^error" || true
cp target/x86_64-pc-windows-msvc/release/savesync-desktop.exe "$OUT/SaveSync-$VERSION-windows-x64.exe"

step "Android app"
cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build -q -p savesync-ffi --release
cargo build -q -p savesync-ffi --lib
for HOST_LIB in target/debug/libsavesync_ffi.dylib target/debug/libsavesync_ffi.so; do [ -f "$HOST_LIB" ] && break; done
cargo run -q -p savesync-ffi --bin uniffi-bindgen -- generate --library "$HOST_LIB" --language kotlin --no-format --out-dir android/app/src/main/java
(cd android && ./gradlew --quiet assembleRelease -PsavesyncVersion="$VERSION")
cp android/app/build/outputs/apk/release/app-release.apk "$OUT/SaveSync-$VERSION-android.apk"

step "Checksums and signature"
(cd tools/release-sign && cargo build -q --release)
(cd "$OUT" && shasum -a 256 SaveSync-* > SHA256SUMS)
"$SIGNER" sign "$SIGN_KEY" "$OUT/SHA256SUMS"
"$SIGNER" verify "$("$SIGNER" pubkey "$SIGN_KEY")" "$OUT/SHA256SUMS"
ls -lh "$OUT" | awk 'NR>1 {print $5, $9}'

if [ -n "$DRY_RUN" ]; then
    echo; echo "Dry run: built $OUT (not committed, tagged or published)."
    exit 0
fi

step "Publish"
NOTES="${SAVESYNC_RELEASE_NOTES:-dist/release-notes-$VERSION.md}"
[ -f "$NOTES" ] || { echo "write $NOTES first"; exit 1; }
git add Cargo.toml Cargo.lock android/app/build.gradle.kts
git diff --cached --quiet || git commit -q -m "Release v$VERSION"
git tag -a "v$VERSION" -m "SaveSync $VERSION"
git push -q origin HEAD "v$VERSION"
gh release create "v$VERSION" "$OUT"/* --title "SaveSync $VERSION" --notes-file "$NOTES"
echo "Published https://github.com/Nicklbhender/savesync/releases/tag/v$VERSION"
