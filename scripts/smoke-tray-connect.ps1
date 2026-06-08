param(
    [string]$Bind = "127.0.0.1:18089",
    [string]$AuthToken = ""
)

# Visual end-to-end check for the Windows tray UI (crates/airpaste-tray).
#
# Unlike the headless smokes, this drives the *tray* as the receiver so you can eyeball the
# window: it should flip to "● 已连接", show the device id, and populate "最近收到 (隔离收件箱)"
# with the text published below. The script also asserts the same outcome automatically by
# grepping the tray's embedded-agent log (stderr -> file), so it prints PASS/FAIL too.
#
# Flow: fresh server -> a CLI bootstrap device (first, auto-trusted) mints a pair code ->
# the tray starts with that pair code (so it becomes trusted, required before E2EE publish) ->
# the bootstrap publishes a text clip -> the tray decrypts it into its isolated inbox.
#
# On success the server + tray are LEFT RUNNING so you can look at the window. Tear them down
# with the command printed at the end (or just close the tray window and stop the server).

$ErrorActionPreference = "Stop"

# Make the embedded agent log at info so the asserts below can see the key lines.
if (-not $env:RUST_LOG) { $env:RUST_LOG = "airpaste_agent=info" }

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$serverExe = Join-Path $root "target\debug\airpaste-server.exe"
$agentExe  = Join-Path $root "target\debug\airpaste-agent.exe"
$trayExe   = Join-Path $root "target\release\airpaste-tray.exe"

$db             = Join-Path $root "target\tray-connect.redb"
$bootstrapState = Join-Path $root "target\tray-connect-bootstrap.json"
$trayState      = Join-Path $root "target\tray-connect-tray.json"
$trayCache      = Join-Path $root "target\tray-connect-tray-cache"
$trayErr        = Join-Path $root "target\tray-connect-tray.err.log"

$baseUrl = "http://$Bind"
$authArgs = @()
if ($AuthToken) { $authArgs = @("--auth-token", $AuthToken) }

function Wait-Log([string]$Pattern, [int]$TimeoutSec) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if ((Test-Path -LiteralPath $trayErr) -and
            (Select-String -LiteralPath $trayErr -Pattern $Pattern -Quiet)) {
            return $true
        }
        Start-Sleep -Milliseconds 300
    }
    return $false
}

# Build what we need (cached builds are fast). Tray is the release exe (no console window).
Write-Host "Building server + agent (debug) and tray (release)..."
& cargo +stable-x86_64-pc-windows-gnu build -p airpaste-server -p airpaste-agent
if ($LASTEXITCODE -ne 0) { throw "build of server/agent failed" }
& cargo +stable-x86_64-pc-windows-gnu build --release -p airpaste-tray
if ($LASTEXITCODE -ne 0) { throw "build of tray failed" }

Remove-Item -Force -Recurse -ErrorAction SilentlyContinue `
    $db, $bootstrapState, $trayState, $trayCache, $trayErr

$serverArgs = @("--bind", $Bind, "--db", $db) + $authArgs
$server = Start-Process -FilePath $serverExe -ArgumentList $serverArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2

$tray = $null
$ok = $false
try {
    Invoke-RestMethod "$baseUrl/health" | Out-Null

    # Bootstrap device (first in a fresh DB -> auto-trusted) mints a pair code for the tray.
    Write-Host "Minting pair code from the bootstrap device..."
    $pairArgs = @(
        "--server-url", $baseUrl,
        "--state-path", $bootstrapState,
        "--device-name", "tray-connect-bootstrap",
        "--create-pair-code",
        "--pair-ttl-seconds", "600",
        "--publish-clipboard=false",
        "--apply-remote=false",
        "--remote-paste-hotkey=false"
    ) + $authArgs
    $pair = (& $agentExe @pairArgs | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0 -or -not $pair.code) { throw "failed to create pair code" }
    Write-Host "Pair code: $($pair.code)"

    # Start the tray as the receiver. It defaults to isolated mode, so inbound text lands in the
    # in-app inbox shown in the window.
    Write-Host "Launching the tray (watch the window)..."
    $trayArgs = @(
        "--server-url", $baseUrl,
        "--state-path", $trayState,
        "--device-name", "tray-connect-ui",
        "--pair-code", $pair.code,
        "--peer-bind", "127.0.0.1:17395",
        "--cache-dir", $trayCache,
        "--publish-clipboard=false"
    ) + $authArgs
    $tray = Start-Process -FilePath $trayExe -ArgumentList $trayArgs -WorkingDirectory $root -PassThru -RedirectStandardError $trayErr

    # The tray must be trusted (paired) before the bootstrap publishes, or the E2EE wrap won't
    # include it as a recipient. Wait for "pairing confirmed", then give the control WebSocket a
    # moment to subscribe so it does not miss the ClipCreated broadcast.
    if (-not (Wait-Log "pairing confirmed" 20)) {
        throw "tray did not confirm pairing (see $trayErr)"
    }
    Write-Host "Tray paired + trusted. Letting the control WebSocket subscribe..."
    Start-Sleep -Seconds 3

    # Publish a recognizable text clip from the bootstrap device.
    $sentinel = "AirPaste 托盘连接测试 $(Get-Date -Format o)"
    Write-Host "Publishing text: $sentinel"
    $pubArgs = @(
        "--server-url", $baseUrl,
        "--state-path", $bootstrapState,
        "--device-name", "tray-connect-bootstrap",
        "--publish-text-once", $sentinel,
        "--publish-clipboard=false",
        "--apply-remote=false",
        "--remote-paste-hotkey=false"
    ) + $authArgs
    & $agentExe @pubArgs | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "failed to publish text" }

    if (-not (Wait-Log "stored remote text in isolated inbox" 20)) {
        throw "tray did not store the published text in its inbox (see $trayErr)"
    }

    $ok = $true
    Write-Host ""
    Write-Host "PASS: the tray connected, paired, and received the published text into its inbox."
    Write-Host "Now LOOK at the tray window — it should show:"
    Write-Host "  - ● 已连接 (green)"
    Write-Host "  - 设备 / 设备 ID populated"
    Write-Host "  - 最近收到 (隔离收件箱): $sentinel"
    Write-Host ""
    Write-Host "The server + tray are left running for you to inspect. Tear down with:"
    Write-Host "  Stop-Process -Id $($tray.Id) -ErrorAction SilentlyContinue   # tray"
    Write-Host "  Stop-Process -Id $($server.Id) -ErrorAction SilentlyContinue # server"
}
finally {
    # Bootstrap exited after --publish-text-once. On failure, tear everything down; on success,
    # leave the server + tray up for visual inspection.
    if (-not $ok) {
        if ($tray)   { Stop-Process -Id $tray.Id   -Force -ErrorAction SilentlyContinue }
        Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
        Write-Host "FAIL — tore down processes. Tray log follows:"
        if (Test-Path -LiteralPath $trayErr) { Get-Content -LiteralPath $trayErr }
    }
    Get-Process airpaste-agent -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $agentExe } | Stop-Process -Force -ErrorAction SilentlyContinue
}
