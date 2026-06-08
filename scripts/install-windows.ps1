# Install the AirPaste tray to a stable per-user location (%LOCALAPPDATA%\AirPaste) so that
# autostart (the HKCU Run key, toggled with the 开机自启 checkbox in the window) points at a path
# that survives rebuilds of target\release.
#
# Usage (set the WinLibs PATH first, as for the other build commands):
#   powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1
#   powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1 -NoBuild

param([switch]$NoBuild)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

if (-not $NoBuild) {
    Push-Location $root
    try {
        & cargo +stable-x86_64-pc-windows-gnu build --release -p airpaste-tray
        if ($LASTEXITCODE -ne 0) { throw "release build failed" }
    } finally {
        Pop-Location
    }
}

$src = Join-Path $root "target\release\airpaste-tray.exe"
if (-not (Test-Path -LiteralPath $src)) { throw "missing $src (build first)" }

$destDir = Join-Path $env:LOCALAPPDATA "AirPaste"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$dest = Join-Path $destDir "airpaste-tray.exe"
Copy-Item -Force -LiteralPath $src -Destination $dest

Write-Host "Installed to $dest"
Write-Host "Launch THIS copy, then tick 开机自启 in the window so autostart registers this stable path:"
Write-Host "  & `"$dest`""
