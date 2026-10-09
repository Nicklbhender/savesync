# Installs what's needed to build SaveSync on Windows on ARM:
#   - Visual Studio 2022 Build Tools (C++ tools for ARM64 and x64, Windows SDK)
#   - Rust (native aarch64-pc-windows-msvc) in C:\rust, usable by every user
# Run as an administrator (or via scripts/windows/vm-run.sh). Safe to re-run.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
function Log($msg) { Write-Output "[$(Get-Date -Format HH:mm:ss)] $msg" }

$tmp = 'C:\savesync-tmp'
New-Item -ItemType Directory -Force $tmp, 'C:\rust' | Out-Null

# ---------------------------------------------------------------- Build Tools
# Clang is needed too: the `ring` crate builds its ARM64 Windows code with it.
$components = @(
    'Microsoft.VisualStudio.Workload.VCTools',
    'Microsoft.VisualStudio.Component.VC.Tools.ARM64',
    'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
    'Microsoft.VisualStudio.Component.VC.Llvm.Clang'
)
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$existing = if (Test-Path $vswhere) { & $vswhere -products * -property installationPath | Select-Object -First 1 }
$complete = $existing -and (& $vswhere -products * -requires $components -property installationPath)
if ($complete) {
    Log "Build Tools already installed with all components: $existing"
} else {
    $vs = "$tmp\vs_buildtools.exe"
    Log 'Downloading Visual Studio 2022 Build Tools bootstrapper'
    Invoke-WebRequest -UseBasicParsing https://aka.ms/vs/17/release/vs_buildtools.exe -OutFile $vs
    $sig = Get-AuthenticodeSignature $vs
    Log "Signature: $($sig.Status), $($sig.SignerCertificate.Subject)"
    if ($sig.Status -ne 'Valid' -or $sig.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
        throw 'vs_buildtools.exe is not validly signed by Microsoft'
    }
    $vsArgs = @('--quiet', '--wait', '--norestart', '--nocache', '--includeRecommended')
    foreach ($c in $components) { $vsArgs += @('--add', $c) }
    if ($existing) {
        Log "Adding missing components to $existing"
        # Start-Process joins arguments with spaces, so paths need their own quotes.
        $vsArgs = @('modify', '--installPath', "`"$existing`"") + $vsArgs
    } else {
        Log 'Installing C++ build tools (ARM64 + x64, Clang) and Windows SDK; this takes a while'
    }
    $p = Start-Process $vs -Wait -PassThru -ArgumentList $vsArgs
    Log "Build Tools installer exit code: $($p.ExitCode)"
    if ($p.ExitCode -notin 0, 3010) { throw "Build Tools install failed ($($p.ExitCode))" }
}

# ---------------------------------------------------------------- Rust
$env:RUSTUP_HOME = 'C:\rust\rustup'
$env:CARGO_HOME = 'C:\rust\cargo'
if (Test-Path 'C:\rust\cargo\bin\rustup.exe') {
    Log 'Rust already installed; updating'
    cmd /c "C:\rust\cargo\bin\rustup.exe update stable 2>&1" | Out-Null
} else {
    $url = 'https://static.rust-lang.org/rustup/dist/aarch64-pc-windows-msvc/rustup-init.exe'
    Log 'Downloading rustup-init (aarch64-pc-windows-msvc)'
    Invoke-WebRequest -UseBasicParsing $url -OutFile "$tmp\rustup-init.exe"
    Invoke-WebRequest -UseBasicParsing "$url.sha256" -OutFile "$tmp\rustup-init.sha256"
    $expected = ((Get-Content "$tmp\rustup-init.sha256" -Raw).Trim() -split '\s+')[0]
    $actual = (Get-FileHash "$tmp\rustup-init.exe" -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $expected) { throw "rustup-init checksum mismatch: $actual != $expected" }
    Log 'Checksum OK; installing stable toolchain'
    # Through cmd: Windows PowerShell treats a native tool's stderr output as an error.
    $out = cmd /c "$tmp\rustup-init.exe -y --profile minimal --default-host aarch64-pc-windows-msvc --default-toolchain stable --no-modify-path 2>&1"
    if ($LASTEXITCODE -ne 0) { $out | ForEach-Object { Log $_ }; throw "rustup-init failed ($LASTEXITCODE)" }
    $out | Select-String -Pattern 'installed' | ForEach-Object { Log $_.Line }
}

# Make it available to every user and new shells.
[Environment]::SetEnvironmentVariable('RUSTUP_HOME', 'C:\rust\rustup', 'Machine')
[Environment]::SetEnvironmentVariable('CARGO_HOME', 'C:\rust\cargo', 'Machine')
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
if ($machinePath -notlike '*C:\rust\cargo\bin*') {
    [Environment]::SetEnvironmentVariable('Path', "$machinePath;C:\rust\cargo\bin", 'Machine')
}
icacls C:\rust /grant '*S-1-5-32-545:(OI)(CI)M' /T /Q | Out-Null   # Users: modify

Log (& C:\rust\cargo\bin\rustc.exe -V)
Log (& C:\rust\cargo\bin\cargo.exe -V)
Log "Host: $((& C:\rust\cargo\bin\rustc.exe -vV | Select-String 'host').Line)"
Remove-Item -Recurse -Force $tmp
Log 'Done'
