param(
    [string]$Bind = "127.0.0.1:18092",
    [string]$AuthToken = ""
)

# Verifies the tray's persisted-config path on Windows (phase 2): the tray reads server URL +
# pair code from %APPDATA%\AirPaste\tray-config.json with NO connection flags, connects, and
# clears the one-shot pair code from the config once trusted.
#
# It overrides %APPDATA% (and uses a temp --state-path) so it never touches your real config or
# identity. Asserts via the tray's embedded-agent log. Companion to smoke-tray-connect.ps1
# (which drives the same outcome via CLI flags instead).
#
# Saved as UTF-8 with a BOM so Windows PowerShell 5.x reads the Chinese correctly.

$ErrorActionPreference = "Stop"
if (-not $env:AIRPASTE_SMOKE_RUST_LOG) { $env:AIRPASTE_SMOKE_RUST_LOG = "airpaste_agent=info" }
$env:RUST_LOG = $env:AIRPASTE_SMOKE_RUST_LOG
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$serverExe = Join-Path $root "target\debug\airpaste-server.exe"
$agentExe  = Join-Path $root "target\debug\airpaste-agent.exe"
$trayExe   = Join-Path $root "target\release\airpaste-tray.exe"

$appData   = Join-Path $root "target\tray-config-appdata"
$cfgDir    = Join-Path $appData "AirPaste"
$cfgFile   = Join-Path $cfgDir "tray-config.json"
$db        = Join-Path $root "target\tray-config.redb"
$bootState = Join-Path $root "target\tray-config-bootstrap.json"
$trayState = Join-Path $root "target\tray-config-tray.json"
$trayErr   = Join-Path $root "target\tray-config-tray.err.log"

$baseUrl = "http://$Bind"
$authArgs = @()
if ($AuthToken) { $authArgs = @("--auth-token", $AuthToken) }

function Wait-Log([string]$Pattern, [int]$TimeoutSec) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if ((Test-Path -LiteralPath $trayErr) -and
            (Select-String -LiteralPath $trayErr -Pattern $Pattern -Quiet)) { return $true }
        Start-Sleep -Milliseconds 300
    }
    return $false
}

Write-Host "Building server + agent (debug) and tray (release)..."
& cargo +stable-x86_64-pc-windows-gnu build -p airpaste-server -p airpaste-agent
if ($LASTEXITCODE -ne 0) { throw "build of server/agent failed" }
& cargo +stable-x86_64-pc-windows-gnu build --release -p airpaste-tray
if ($LASTEXITCODE -ne 0) { throw "build of tray failed" }

Remove-Item -Force -Recurse -ErrorAction SilentlyContinue $appData, $db, $bootState, $trayState, $trayErr
New-Item -ItemType Directory -Force -Path $cfgDir | Out-Null

$serverArgs = @("--bind", $Bind, "--db", $db) + $authArgs
$server = Start-Process -FilePath $serverExe -ArgumentList $serverArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2

$tray = $null
$ok = $false
try {
    Invoke-RestMethod "$baseUrl/health" | Out-Null

    Write-Host "Minting pair code from the bootstrap device..."
    $pairArgs = @(
        "--server-url", $baseUrl, "--state-path", $bootState, "--device-name", "tray-config-bootstrap",
        "--create-pair-code", "--pair-ttl-seconds", "600",
        "--publish-clipboard=false", "--apply-remote=false", "--remote-paste-hotkey=false"
    ) + $authArgs
    $pair = (& $agentExe @pairArgs | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0 -or -not $pair.code) { throw "failed to create pair code" }

    # The thing under test: a tray-config.json with server + pair code, in an isolated APPDATA.
    $cfg = @{ server_url = $baseUrl; pair_code = $pair.code }
    if ($AuthToken) { $cfg.auth_token = $AuthToken }
    ($cfg | ConvertTo-Json) | Set-Content -LiteralPath $cfgFile -Encoding UTF8
    Write-Host "Wrote $cfgFile"

    # Launch the tray with the overridden APPDATA and NO connection flags -> it must read config.
    Write-Host "Launching the tray (reads config, no flags)..."
    $env:APPDATA = $appData
    $trayArgs = @("--state-path", $trayState, "--peer-bind", "127.0.0.1:17396")
    $tray = Start-Process -FilePath $trayExe -ArgumentList $trayArgs -WorkingDirectory $root -PassThru -RedirectStandardError $trayErr

    if (-not (Wait-Log "pairing confirmed" 25)) { throw "tray did not confirm pairing (see $trayErr)" }
    Start-Sleep -Seconds 3

    $sentinel = "AirPaste config smoke $(Get-Date -Format o)"
    Write-Host "Publishing text..."
    $pubArgs = @(
        "--server-url", $baseUrl, "--state-path", $bootState, "--device-name", "tray-config-bootstrap",
        "--publish-text-once", $sentinel,
        "--publish-clipboard=false", "--apply-remote=false", "--remote-paste-hotkey=false"
    ) + $authArgs
    & $agentExe @pubArgs | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "failed to publish text" }

    if (-not (Wait-Log "stored remote text in isolated inbox" 20)) {
        throw "tray did not store the published text (see $trayErr)"
    }

    # The one-shot pair code must be cleared from the config after a successful connect.
    Start-Sleep -Seconds 1
    $after = Get-Content -LiteralPath $cfgFile -Raw | ConvertFrom-Json
    if ($after.pair_code) { throw "pair_code was not cleared from the config after connect" }

    $ok = $true
    Write-Host ""
    Write-Host "PASS: tray read tray-config.json, paired, connected, received the clip, and the"
    Write-Host "      one-shot pair code was cleared from the config."
}
finally {
    if ($tray) { Stop-Process -Id $tray.Id -Force -ErrorAction SilentlyContinue }
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
    Get-Process airpaste-agent -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $agentExe } | Stop-Process -Force -ErrorAction SilentlyContinue
    if (-not $ok -and (Test-Path -LiteralPath $trayErr)) {
        Write-Host "FAIL — tray log:"; Get-Content -LiteralPath $trayErr
    }
    Remove-Item -Force -Recurse -ErrorAction SilentlyContinue $appData
}
