param(
    [string]$Toolchain = "1.88.0-x86_64-pc-windows-msvc",
    [string]$Proxy = "",
    [string]$OutputDir = "",
    [switch]$NoBuild,
    [switch]$IncludeCli
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

function Invoke-Checked([scriptblock]$Command, [string]$FailureMessage) {
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw $FailureMessage
    }
}

Push-Location $root
try {
    if ($Proxy) {
        $env:HTTP_PROXY = $Proxy
        $env:HTTPS_PROXY = $Proxy
    }
    if (-not $env:CARGO_REGISTRIES_CRATES_IO_PROTOCOL) {
        $env:CARGO_REGISTRIES_CRATES_IO_PROTOCOL = "sparse"
    }

    $hash = (git rev-parse --short HEAD).Trim()
    $version = "portable-$hash"
    $distRoot = if ($OutputDir) { $OutputDir } else { Join-Path $root "dist" }
    $packageDir = Join-Path $distRoot "AirPaste-$version-windows"
    $zipPath = "$packageDir.zip"

    if (-not $NoBuild) {
        Write-Host "Building release tray (toolchain $Toolchain)..."
        Invoke-Checked { cargo "+$Toolchain" build --release -p airpaste-tray } "tray release build failed"

        if ($IncludeCli) {
            Write-Host "Building release CLI tools..."
            Invoke-Checked { cargo "+$Toolchain" build --release -p airpaste-agent -p airpaste-server } "CLI release build failed"
        }
    }

    $trayExe = Join-Path $root "target\release\airpaste-tray.exe"
    if (-not (Test-Path -LiteralPath $trayExe)) {
        throw "missing $trayExe (build first, or omit -NoBuild)"
    }

    Remove-Item -Force -Recurse -ErrorAction SilentlyContinue $packageDir, $zipPath
    New-Item -ItemType Directory -Force -Path $packageDir | Out-Null

    Copy-Item -LiteralPath $trayExe -Destination (Join-Path $packageDir "AirPaste.exe") -Force

    if ($IncludeCli) {
        foreach ($name in @("airpaste-agent.exe", "airpaste-server.exe")) {
            $src = Join-Path $root "target\release\$name"
            if (-not (Test-Path -LiteralPath $src)) {
                throw "missing $src"
            }
            Copy-Item -LiteralPath $src -Destination (Join-Path $packageDir $name) -Force
        }
    }

    @"
AirPaste Windows portable build
================================

Run:
  AirPaste.exe

Notes:
  - This is the tray UI build. It embeds the desktop agent.
  - Settings and device state are stored under:
      %APPDATA%\AirPaste
  - To upgrade, quit AirPaste from the tray menu, then replace this folder with a newer build.
  - To start with Windows, open AirPaste and enable the startup checkbox in Settings.

Build:
  Commit: $hash
  Date:   $(Get-Date -Format "yyyy-MM-dd HH:mm:ss zzz")

"@ | Set-Content -LiteralPath (Join-Path $packageDir "README.txt") -Encoding UTF8

    if (Test-Path -LiteralPath (Join-Path $root "README.md")) {
        Copy-Item -LiteralPath (Join-Path $root "README.md") -Destination (Join-Path $packageDir "PROJECT_README.md") -Force
    }

    Compress-Archive -Path (Join-Path $packageDir "*") -DestinationPath $zipPath -Force

    Write-Host "Portable package:"
    Write-Host "  $packageDir"
    Write-Host "  $zipPath"
} finally {
    Pop-Location
}
