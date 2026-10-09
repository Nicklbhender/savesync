#!/usr/bin/env bash
# Optional helper for a Windows VM in UTM on macOS: runs a PowerShell script in
# the VM through the QEMU guest agent (part of the UTM guest tools) and prints
# its output.
#
#   scripts/windows/vm-run.sh path/to/script.ps1 [vm-name]
#
# The script runs as SYSTEM. Exits with the script's exit code.
# VM_RUN_TIMEOUT (seconds, default 7200) bounds how long to wait.
set -euo pipefail

UTMCTL="${UTMCTL:-/Applications/UTM.app/Contents/MacOS/utmctl}"
VM="${2:-Windows 11}"
ID="savesync-$$-$RANDOM"
PS1="C:\\Windows\\Temp\\$ID.ps1"
LOG="C:\\Windows\\Temp\\$ID.log"
CODE="C:\\Windows\\Temp\\$ID.code"

if [ "$("$UTMCTL" status "$VM" 2>/dev/null)" != "started" ]; then
    echo "The VM \"$VM\" isn't running. Start it in UTM first." >&2
    exit 2
fi
"$UTMCTL" file push "$VM" "$PS1" < "$1"
# exec returns immediately, so the script records its exit code in a file
# when it finishes, and we wait for that file to appear.
"$UTMCTL" exec "$VM" --cmd cmd.exe /c \
    "powershell.exe -NoProfile -ExecutionPolicy Bypass -File $PS1 > $LOG 2>&1 & call echo %^errorlevel%> $CODE" \
    >/dev/null 2>&1 || true
deadline=$(( $(date +%s) + ${VM_RUN_TIMEOUT:-7200} ))
until code=$("$UTMCTL" file pull "$VM" "$CODE" 2>/dev/null | tr -dc '0-9') && [ -n "$code" ]; do
    if [ "$("$UTMCTL" status "$VM" 2>/dev/null)" != "started" ]; then
        echo "The VM stopped while the script was running." >&2
        exit 2
    fi
    if [ "$(date +%s)" -ge "$deadline" ]; then
        echo "timed out waiting for the script; partial output:" >&2
        "$UTMCTL" file pull "$VM" "$LOG" 2>/dev/null | tr -d '\r' || true
        exit 124
    fi
    sleep 3
done
"$UTMCTL" file pull "$VM" "$LOG" 2>/dev/null | tr -d '\r'
"$UTMCTL" exec "$VM" --cmd cmd.exe /c "del /q $PS1 $LOG $CODE" >/dev/null 2>&1 || true
exit "${code:-1}"
