param(
    [string]$Bind = "127.0.0.1:18088",
    [string]$AuthToken = ""
)

# Interactive Windows harness for isolated-mode synthetic hotkeys (Ctrl+Shift+V / Ctrl+Shift+C).
# It sets everything up, then asks you to press the hotkeys in Notepad. It auto-checks the
# scriptable parts (the system clipboard is restored after paste; a new clip is published after
# copy); the pasted text appearing in Notepad is confirmed visually by you.
#
# Requires a real interactive desktop session. Over RDP, rdpclip can interfere with the
# clipboard save/restore; if the clipboard-restore check is flaky, disable clipboard
# redirection in mstsc (Local Resources -> uncheck Clipboard) and retry.

$ErrorActionPreference = "Stop"
if (-not $env:AIRPASTE_SMOKE_RUST_LOG) { $env:AIRPASTE_SMOKE_RUST_LOG = "airpaste_agent=info" }
$env:RUST_LOG = $env:AIRPASTE_SMOKE_RUST_LOG

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$serverExe = Join-Path $root "target\debug\airpaste-server.exe"
$agentExe = Join-Path $root "target\debug\airpaste-agent.exe"
$db = Join-Path $root "target\hotkey-iso-smoke.redb"
$senderState = Join-Path $root "target\hotkey-iso-sender.json"
$receiverState = Join-Path $root "target\hotkey-iso-receiver.json"
$receiverCache = Join-Path $root "target\hotkey-iso-receiver-cache"
$receiverOut = Join-Path $root "target\hotkey-iso-receiver.out.log"
$receiverErr = Join-Path $root "target\hotkey-iso-receiver.err.log"
$baseUrl = "http://$Bind"
$authArgs = @()
if ($AuthToken) { $authArgs = @("--auth-token", $AuthToken) }

function Get-AgentDeviceId([string]$StatePath) {
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path -LiteralPath $StatePath) {
            $state = Get-Content -LiteralPath $StatePath -Raw | ConvertFrom-Json
            if ($state.device_id) { return [string]$state.device_id }
        }
        Start-Sleep -Milliseconds 250
    }
    throw "agent did not write device_id to $StatePath"
}

function New-PairCode {
    $args = @(
        "--server-url", $baseUrl, "--state-path", $senderState, "--device-name", "iso-hotkey-sender",
        "--create-pair-code", "--pair-ttl-seconds", "600",
        "--publish-clipboard=false", "--apply-remote=false", "--remote-paste-hotkey=false"
    ) + $authArgs
    $json = & $agentExe @args
    if ($LASTEXITCODE -ne 0) { throw "failed to create pair code" }
    return $json | ConvertFrom-Json
}

function Get-LatestClip {
    $args = @(
        "--server-url", $baseUrl, "--state-path", $senderState, "--print-latest-clip",
        "--publish-clipboard=false", "--apply-remote=false", "--remote-paste-hotkey=false"
    ) + $authArgs
    $json = & $agentExe @args
    if ($LASTEXITCODE -ne 0) { throw "failed to read latest clip" }
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
    $db, $senderState, $receiverState, $receiverCache, $receiverOut, $receiverErr

$serverArgs = @("--bind", $Bind, "--db", $db) + $authArgs
$server = Start-Process -FilePath $serverExe -ArgumentList $serverArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2

$receiver = $null
$notepad = $null
try {
    Invoke-RestMethod "$baseUrl/health" | Out-Null

    $receiverPair = New-PairCode

    Write-Host "Starting isolated receiver with hotkeys..."
    $receiverArgs = @(
        "--server-url", $baseUrl, "--device-name", "iso-hotkey-receiver", "--state-path", $receiverState,
        "--pair-code", $receiverPair.code, "--peer-bind", "127.0.0.1:17395", "--cache-dir", $receiverCache,
        "--clipboard-mode", "isolated", "--publish-clipboard=false", "--remote-paste-hotkey=true"
    ) + $authArgs
    $receiver = Start-Process -FilePath $agentExe -ArgumentList $receiverArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru -RedirectStandardOutput $receiverOut -RedirectStandardError $receiverErr
    Get-AgentDeviceId $receiverState | Out-Null
    Start-Sleep -Seconds 2
    if (-not (Test-ReceiverLog "registered hotkeys Ctrl\+Shift\+V and Ctrl\+Shift\+C")) {
        Write-Warning "did not see the hotkey-registration log yet; continuing anyway"
    }

    $inboxText = "AIRPASTE HOTKEY PASTE $(Get-Date -Format o)"
    Write-Host "Publishing inbox text: $inboxText"
    $publishArgs = @(
        "--server-url", $baseUrl, "--state-path", $senderState, "--device-name", "iso-hotkey-sender",
        "--publish-text-once", $inboxText,
        "--publish-clipboard=false", "--apply-remote=false", "--remote-paste-hotkey=false"
    ) + $authArgs
    & $agentExe @publishArgs | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "failed to publish inbox text" }

    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline -and -not (Test-ReceiverLog "stored remote text in isolated inbox")) {
        Start-Sleep -Milliseconds 250
    }
    if (-not (Test-ReceiverLog "stored remote text in isolated inbox")) {
        throw "receiver did not store the inbox text"
    }

    $sentinel = "airpaste-hotkey-sentinel-$(Get-Date -Format o)"
    Set-Clipboard -Value $sentinel
    $notepad = Start-Process notepad -PassThru
    Start-Sleep -Seconds 1

    Write-Host ""
    Write-Host "==================== PASTE TEST (Ctrl+Shift+V) ===================="
    Write-Host "1. Click into the Notepad window so it has focus."
    Write-Host "2. Press Ctrl+Shift+V."
    Write-Host "   Expected: the line below is typed into Notepad:"
    Write-Host "     $inboxText"
    Read-Host "Press Enter here AFTER you have pressed Ctrl+Shift+V"

    $afterPaste = (Get-Clipboard -Raw).TrimEnd()
    if ($afterPaste -eq $sentinel) {
        Write-Host "PASS: system clipboard was restored to the sentinel after paste." -ForegroundColor Green
    }
    else {
        Write-Warning "system clipboard is '$afterPaste', expected sentinel '$sentinel' (rdpclip? see header note)"
    }
    Write-Host "(Visually confirm the inbox text appeared in Notepad.)"

    Write-Host ""
    Write-Host "==================== COPY TEST (Ctrl+Shift+C) ===================="
    $clipBefore = (Get-LatestClip).clip_id
    Set-Clipboard -Value $sentinel
    Write-Host "1. In Notepad, type some NEW text, then select it."
    Write-Host "2. Press Ctrl+Shift+C."
    Read-Host "Press Enter here AFTER you have selected text and pressed Ctrl+Shift+C"

    $deadline = (Get-Date).AddSeconds(8)
    $clipAfter = $clipBefore
    while ((Get-Date) -lt $deadline) {
        $clipAfter = (Get-LatestClip).clip_id
        if ($clipAfter -ne $clipBefore) { break }
        Start-Sleep -Milliseconds 250
    }
    if ($clipAfter -ne $clipBefore) {
        Write-Host "PASS: Ctrl+Shift+C published a new clip ($clipAfter)." -ForegroundColor Green
    }
    else {
        Write-Warning "no new clip was published after Ctrl+Shift+C (did you select text first?)"
    }
    $afterCopy = (Get-Clipboard -Raw).TrimEnd()
    if ($afterCopy -eq $sentinel) {
        Write-Host "PASS: system clipboard was restored to the sentinel after copy." -ForegroundColor Green
    }
    else {
        Write-Warning "system clipboard is '$afterCopy', expected sentinel '$sentinel' (rdpclip?)"
    }

    Write-Host ""
    Write-Host "Hotkey harness done. Review the PASS/WARN lines above."
}
finally {
    if ($notepad) { Stop-Process -Id $notepad.Id -Force -ErrorAction SilentlyContinue }
    if ($receiver) { Stop-Process -Id $receiver.Id -Force -ErrorAction SilentlyContinue }
    Get-Process airpaste-agent -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $agentExe } | Stop-Process -Force
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
}
