$ErrorActionPreference = "Stop"

# --- Easily adjustable settings ---
$ZigRoot = "C:\tools\zig"
$Target = "aarch64-unknown-linux-gnu"
$SshUser = "cherry"
$SshHost = "192.168.0.31"
$RemoteCratondPath = "/tmp/cratond.new"
$RemoteCratonctlPath = "/tmp/cratonctl.new"
$RemoteInstallScriptPath = "/tmp/install-remote.sh"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$ReleaseDir = Join-Path $RepoRoot "target\$Target\release"
$Remote = "$SshUser@$SshHost"

Write-Host "[deploy] Repo root: $RepoRoot"

if (-not (Test-Path $ZigRoot)) {
    throw "[deploy] Zig path not found: $ZigRoot"
}

$env:PATH = "$ZigRoot;$env:PATH"
Write-Host "[deploy] Added Zig to PATH: $ZigRoot"

Write-Host "[deploy] Building cratond and cratonctl for $Target"
& cargo zigbuild --release --target $Target --bin cratond --bin cratonctl
if ($LASTEXITCODE -ne 0) {
    throw "[deploy] cargo zigbuild failed"
}

$CratondBinary = Join-Path $ReleaseDir "cratond"
$CratonctlBinary = Join-Path $ReleaseDir "cratonctl"
$RemoteScript = Join-Path $PSScriptRoot "install-remote.sh"

if (-not (Test-Path $CratondBinary)) {
    throw "[deploy] Missing binary: $CratondBinary"
}
if (-not (Test-Path $CratonctlBinary)) {
    throw "[deploy] Missing binary: $CratonctlBinary"
}
if (-not (Test-Path $RemoteScript)) {
    throw "[deploy] Missing remote install script: $RemoteScript"
}

Write-Host "[deploy] Uploading binaries to $Remote"
& scp $CratondBinary "${Remote}:${RemoteCratondPath}"
if ($LASTEXITCODE -ne 0) {
    throw "[deploy] Failed to upload cratond"
}

& scp $CratonctlBinary "${Remote}:${RemoteCratonctlPath}"
if ($LASTEXITCODE -ne 0) {
    throw "[deploy] Failed to upload cratonctl"
}

Write-Host "[deploy] Uploading remote install script"
& scp $RemoteScript "${Remote}:${RemoteInstallScriptPath}"
if ($LASTEXITCODE -ne 0) {
    throw "[deploy] Failed to upload remote install script"
}

Write-Host "[deploy] Running remote install script via SSH"
& ssh $Remote "sudo bash $RemoteInstallScriptPath"
if ($LASTEXITCODE -ne 0) {
    throw "[deploy] Remote install failed"
}

Write-Host "[deploy] Deploy completed successfully"