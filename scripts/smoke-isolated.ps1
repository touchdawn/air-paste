param(
    [string]$Bind = "127.0.0.1:18087",
    [string]$AuthToken = ""
)

# Windows isolated-clipboard-mode smoke (inbound half), the analog of
# scripts/smoke-isolated-macos.sh: a receiver runs with --clipboard-mode isolated, a remote
# text clip is published, and we assert the receiver stored it in its in-app inbox WITHOUT
# overwriting the system clipboard. The synthetic Ctrl+Shift+C / Ctrl+Shift+V hotkeys need a
# focused GUI app and are verified manually.

$ErrorActionPreference = "Stop"

# Ensure the agent emits the info-level lines this smoke greps for, regardless of any RUST_LOG
# already in the environment (e.g. RUST_LOG=warn would hide "stored remote text in isolated inbox").
if (-not $env:AIRPASTE_SMOKE_RUST_LOG) { $env:AIRPASTE_SMOKE_RUST_LOG = "airpaste_agent=info" }
$env:RUST_LOG = $env:AIRPASTE_SMOKE_RUST_LOG

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$serverExe = Join-Path $root "target\debug\airpaste-server.exe"
$agentExe = Join-Path $root "target\debug\airpaste-agent.exe"
$db = Join-Path $root "target\isolated-smoke.redb"
$bootstrapState = Join-Path $root "target\isolated-bootstrap.json"
$receiverState = Join-Path $root "target\isolated-receiver.json"
$receiverCache = Join-Path $root "target\isolated-receiver-cache"
$receiverOut = Join-Path $root "target\isolated-receiver.out.log"
$receiverErr = Join-Path $root "target\isolated-receiver.err.log"
$baseUrl = "http://$Bind"
$authHeaders = @{}
$authArgs = @()
if ($AuthToken) {
    $authHeaders["Authorization"] = "Bearer $AuthToken"
    $authArgs = @("--auth-token", $AuthToken)
}

function Get-AgentDeviceId([string]$StatePath) {
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path -LiteralPath $StatePath) {
            $state = Get-Content -LiteralPath $StatePath -Raw | ConvertFrom-Json
            if ($state.device_id) {
                return [string]$state.device_id
            }
        }
        Start-Sleep -Milliseconds 250
    }
    throw "agent did not write device_id to $StatePath"
}

function New-PairCode {
    $args = @(
        "--server-url", $baseUrl,
        "--state-path", $bootstrapState,
        "--device-name", "isolated-bootstrap",
        "--create-pair-code",
        "--pair-ttl-seconds", "600",
        "--publish-clipboard=false",
        "--apply-remote=false",
        "--remote-paste-hotkey=false"
    ) + $authArgs
    $json = & $agentExe @args
    if ($LASTEXITCODE -ne 0) {
        throw "failed to create pair code"
    }
    return $json | ConvertFrom-Json
}

function Test-ReceiverLog([string]$Pattern) {
    foreach ($log in @($receiverOut, $receiverErr)) {
        if ((Test-Path -LiteralPath $log) -and (Select-String -LiteralPath $log -Pattern $Pattern -Quiet)) {
            return $true
        }
    }
    return $false
}

Remove-Item -Force -Recurse -ErrorAction SilentlyContinue `
    $db, $bootstrapState, $receiverState, $receiverCache, $receiverOut, $receiverErr

$serverArgs = @("--bind", $Bind, "--db", $db) + $authArgs
$server = Start-Process -FilePath $serverExe -ArgumentList $serverArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2

$receiver = $null
try {
    Invoke-RestMethod "$baseUrl/health" | Out-Null

    # Seed the system clipboard with a sentinel; isolated mode must NOT overwrite it.
    $sentinel = "airpaste-isolated-sentinel-$(Get-Date -Format o)"
    Set-Clipboard -Value $sentinel
    if ((Get-Clipboard -Raw).TrimEnd() -ne $sentinel) {
        throw "could not seed the system clipboard with the sentinel"
    }

    # Bootstrap (first device, auto-trusted) creates a pair code for the receiver.
    $receiverPair = New-PairCode

    Write-Host "Start isolated receiver"
    $receiverArgs = @(
        "--server-url", $baseUrl,
        "--device-name", "isolated-receiver",
        "--state-path", $receiverState,
        "--pair-code", $receiverPair.code,
        "--peer-bind", "127.0.0.1:17394",
        "--cache-dir", $receiverCache,
        "--clipboard-mode", "isolated",
        "--publish-clipboard=false",
        "--remote-paste-hotkey=false"
    ) + $authArgs
    $receiver = Start-Process -FilePath $agentExe -ArgumentList $receiverArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru -RedirectStandardOutput $receiverOut -RedirectStandardError $receiverErr
    Get-AgentDeviceId $receiverState | Out-Null
    # Let the receiver's control WebSocket connect/subscribe before publishing, so it does not
    # miss the broadcast ClipCreated event.
    Start-Sleep -Seconds 2

    Write-Host "Publish remote text"
    $remoteText = "airpaste isolated remote $(Get-Date -Format o)"
    $publishArgs = @(
        "--server-url", $baseUrl,
        "--state-path", $bootstrapState,
        "--device-name", "isolated-bootstrap",
        "--publish-text-once", $remoteText,
        "--publish-clipboard=false",
        "--apply-remote=false",
        "--remote-paste-hotkey=false"
    ) + $authArgs
    & $agentExe @publishArgs | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "failed to publish remote text"
    }

    # The receiver must store it in the inbox (not the system clipboard).
    $deadline = (Get-Date).AddSeconds(10)
    $stored = $false
    while ((Get-Date) -lt $deadline) {
        if (Test-ReceiverLog "stored remote text in isolated inbox") {
            $stored = $true
            break
        }
        Start-Sleep -Milliseconds 250
    }
    if (-not $stored) {
        throw "isolated smoke failed: receiver did not store remote text in the inbox"
    }

    # Core assertion: the system clipboard was NOT overwritten.
    $current = (Get-Clipboard -Raw).TrimEnd()
    if ($current -ne $sentinel) {
        throw "isolated smoke failed: system clipboard was overwritten (expected sentinel, got '$current')"
    }

    # And it must not have taken the system-clipboard apply path.
    if (Test-ReceiverLog "applied remote text clip") {
        throw "isolated smoke failed: receiver took the system-clipboard apply path"
    }

    Write-Host "Isolated smoke passed"
    Write-Host "System clipboard preserved: $sentinel"
}
finally {
    if ($receiver) {
        Stop-Process -Id $receiver.Id -Force -ErrorAction SilentlyContinue
    }
    Get-Process airpaste-agent -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $agentExe } | Stop-Process -Force
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
}
