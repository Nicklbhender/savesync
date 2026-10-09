#!/usr/bin/env bash
# Packages what's needed to build and run the server with Docker (for example
# in a NAS's container app), with your settings filled into docker-compose.yml:
#
#   scripts/make-nas-bundle.sh      →  dist/savesync-nas.zip
#
# Generates a new enroll key and prints it once. Save it in your password manager.
set -euo pipefail
cd "$(dirname "$0")/.."

ask() {
    local value=""
    while [ -z "$value" ]; do read -rp "$1: " value; done
    printf '%s' "$value"
}
NAS=$(ask "Address your devices will use to reach the server (LAN IP or Tailscale name, e.g. 192.168.1.50)")
SAVES=$(ask "Path of the saves folder on the server (e.g. /path/to/saves)")
KEY=$(openssl rand -hex 32)

OUT=dist/savesync-nas
rm -rf "$OUT" dist/savesync-nas.zip
mkdir -p "$OUT/engine" "$OUT/desktop" "$OUT/ffi" "$OUT/ntfy"
# Some systems don't create missing bind-mount folders, so ship ntfy's cache folder.
touch "$OUT/ntfy/.keep"
cp -R Cargo.toml Cargo.lock .dockerignore protocol server "$OUT/"
rm -rf "$OUT/server/tests"
# The server build only needs these crates' manifests (they're workspace members).
cp engine/Cargo.toml "$OUT/engine/"
cp desktop/Cargo.toml "$OUT/desktop/"
cp ffi/Cargo.toml "$OUT/ffi/"

sed -e "s|SAVESYNC_ENROLL_KEY: \".*\"|SAVESYNC_ENROLL_KEY: \"$KEY\"|" \
    -e "s|SAVESYNC_PUSH_REWRITE: \".*\"|SAVESYNC_PUSH_REWRITE: \"http://$NAS:8421=http://ntfy\"|" \
    -e "s|NTFY_BASE_URL: \".*\"|NTFY_BASE_URL: \"http://$NAS:8421\"|" \
    -e "s|- ./data:/data|- $SAVES:/data|" \
    docker-compose.yml > "$OUT/docker-compose.yml"

(cd "$OUT" && zip -rqX ../savesync-nas.zip . -x '.DS_Store')
rm -rf "$OUT"

cat <<EOF

Created dist/savesync-nas.zip for http://$NAS:8420 (saves in $SAVES).

Your enroll key (save it in your password manager; it's also inside the zip's docker-compose.yml):

    $KEY

EOF
open -R dist/savesync-nas.zip 2>/dev/null || true
