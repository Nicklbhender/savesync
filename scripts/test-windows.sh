#!/usr/bin/env bash
# Optional helper for developing on macOS with a Windows VM in UTM: runs the
# whole workspace's tests natively in that VM. (On a Windows machine, just run
# `cargo test --workspace`.)
#
#   scripts/test-windows.sh [vm-name]
#
# Copies the current working tree (tracked + new files, minus ignored ones)
# into the VM, then runs `cargo test --workspace` there. One-time setup:
# UTM guest tools in the VM, then scripts/windows/vm-run.sh scripts/windows/install-toolchain.ps1
set -euo pipefail
cd "$(dirname "$0")/.."

UTMCTL="${UTMCTL:-/Applications/UTM.app/Contents/MacOS/utmctl}"
VM="${1:-Windows 11}"

archive=$(mktemp -t savesync-src).tar
trap 'rm -f "$archive"' EXIT
git ls-files -z --cached --others --exclude-standard | xargs -0 tar -cf "$archive"
"$UTMCTL" exec "$VM" --cmd cmd.exe /c "mkdir C:\\savesync 2>nul" >/dev/null 2>&1 || true
"$UTMCTL" file push "$VM" 'C:\savesync\src.tar' < "$archive"

script=$(mktemp -t savesync-test).ps1
trap 'rm -f "$archive" "$script"' EXIT
cat > "$script" <<'EOF'
$env:RUSTUP_HOME = 'C:\rust\rustup'
$env:CARGO_HOME = 'C:\rust\cargo'
$env:Path = "C:\rust\cargo\bin;$env:Path"
$env:CARGO_TARGET_DIR = 'C:\savesync\target'
# Visual Studio's clang, needed by `ring` on ARM64 Windows.
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -property installationPath
foreach ($arch in 'ARM64', 'x64') {
    if (Test-Path "$vs\VC\Tools\Llvm\$arch\bin\clang.exe") { $env:Path = "$vs\VC\Tools\Llvm\$arch\bin;$env:Path"; break }
}
$src = 'C:\savesync\src'
if (Test-Path $src) { Remove-Item -Recurse -Force $src }
New-Item -ItemType Directory -Force $src | Out-Null
tar -xf C:\savesync\src.tar -C $src
Set-Location $src
$os = Get-CimInstance Win32_OperatingSystem
Write-Output "== $($os.Caption) build $($os.BuildNumber); $(rustc -V); host $((rustc -vV | Select-String 'host').Line -replace 'host: ','')"
cmd /c "cargo test --workspace --color never 2>&1"
exit $LASTEXITCODE
EOF
VM_RUN_TIMEOUT=3600 scripts/windows/vm-run.sh "$script" "$VM"
