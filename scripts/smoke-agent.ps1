param(
    [string]$Bind = "127.0.0.1:18081",
    [string]$AuthToken = ""
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$serverExe = Join-Path $root "target\debug\airpaste-server.exe"
$agentExe = Join-Path $root "target\debug\airpaste-agent.exe"
$db = Join-Path $root "target\agent-smoke.redb"
$publishState = Join-Path $root "target\agent-publish.json"
$applyState = Join-Path $root "target\agent-apply.json"
$filePublishState = Join-Path $root "target\agent-file-publish.json"
$fileReceiveState = Join-Path $root "target\agent-file-receive.json"
$fileReceiveCache = Join-Path $root "target\agent-file-cache"
$baseUrl = "http://$Bind"
$authHeaders = @{}
$authArgs = @()
if ($AuthToken) {
    $authHeaders["Authorization"] = "Bearer $AuthToken"
    $authArgs = @("--auth-token", $AuthToken)
}

function New-AirPasteHeaders([string]$DeviceId) {
    $headers = @{}
    foreach ($key in $authHeaders.Keys) {
        $headers[$key] = $authHeaders[$key]
    }
    if ($DeviceId) {
        $headers["x-airpaste-device-id"] = $DeviceId
    }
    return $headers
}

function Expect-HttpStatus([int]$ExpectedStatus, [scriptblock]$Action) {
    try {
        & $Action
        throw "request unexpectedly succeeded"
    }
    catch {
        $statusCode = $null
        if ($_.Exception.Response -and $_.Exception.Response.StatusCode) {
            $statusCode = [int]$_.Exception.Response.StatusCode
        }
        if ($statusCode -ne $ExpectedStatus) {
            throw
        }
    }
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

function New-PairCode([string]$TrustedDeviceId) {
    $headers = New-AirPasteHeaders $TrustedDeviceId
    return Invoke-RestMethod "$baseUrl/v1/pair/start" -Method Post -Headers $headers -ContentType "application/json" -Body (@{
        created_by = $TrustedDeviceId
        ttl_seconds = 600
    } | ConvertTo-Json)
}

Remove-Item -Force -Recurse -ErrorAction SilentlyContinue $db, $publishState, $applyState, $filePublishState, $fileReceiveState, $fileReceiveCache

$serverArgs = @("--bind", $Bind, "--db", $db) + $authArgs
$server = Start-Process -FilePath $serverExe -ArgumentList $serverArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2

try {
    Invoke-RestMethod "$baseUrl/health" | Out-Null
    if ($AuthToken) {
        Write-Host "Auth guard"
        Expect-HttpStatus 401 {
            Invoke-RestMethod "$baseUrl/v1/devices" | Out-Null
        }
    }

    Write-Host "Publish path"
    $publisherArgs = @(
        "--server-url", $baseUrl,
        "--device-name", "smoke-publisher",
        "--state-path", $publishState,
        "--apply-remote=false",
        "--remote-paste-hotkey=false"
    ) + $authArgs
    $publisher = Start-Process -FilePath $agentExe -ArgumentList $publisherArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2
    $publisherDeviceId = Get-AgentDeviceId $publishState
    $publisherHeaders = New-AirPasteHeaders $publisherDeviceId
    Expect-HttpStatus 401 {
        Invoke-RestMethod "$baseUrl/v1/clips/latest" -Headers $authHeaders | Out-Null
    }
    $publishText = "airpaste publish smoke $(Get-Date -Format o)"
    Set-Clipboard -Value $publishText
    Start-Sleep -Seconds 2
    $latest = Invoke-RestMethod "$baseUrl/v1/clips/latest" -Headers $publisherHeaders
    if ($latest.kind.text.encrypted_inline_body -ne $publishText) {
        throw "publish smoke failed"
    }
    Stop-Process -Id $publisher.Id -Force

    Write-Host "Apply path"
    $applierPair = New-PairCode $publisherDeviceId
    $applierArgs = @(
        "--server-url", $baseUrl,
        "--device-name", "smoke-applier",
        "--state-path", $applyState,
        "--pair-code", $applierPair.code,
        "--publish-clipboard=false",
        "--remote-paste-hotkey=false"
    ) + $authArgs
    $applier = Start-Process -FilePath $agentExe -ArgumentList $applierArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2
    $applierDeviceId = Get-AgentDeviceId $applyState
    $applierHeaders = New-AirPasteHeaders $applierDeviceId

    $source = Invoke-RestMethod "$baseUrl/v1/devices" -Method Post -Headers $authHeaders -ContentType "application/json" -Body (@{
        name = "smoke-remote-source"
        public_key = "smoke-key"
    } | ConvertTo-Json)
    $sourceHeaders = New-AirPasteHeaders $source.device.device_id
    Expect-HttpStatus 403 {
        Invoke-RestMethod "$baseUrl/v1/clips/latest" -Headers $sourceHeaders | Out-Null
    }
    $sourcePair = New-PairCode $publisherDeviceId
    Invoke-RestMethod "$baseUrl/v1/pair/confirm" -Method Post -Headers $authHeaders -ContentType "application/json" -Body (@{
        code = $sourcePair.code
        device_id = $source.device.device_id
    } | ConvertTo-Json) | Out-Null

    $applyText = "airpaste apply smoke $(Get-Date -Format o)"
    Invoke-RestMethod "$baseUrl/v1/clips" -Method Post -Headers $sourceHeaders -ContentType "application/json" -Body (@{
        source_device_id = $source.device.device_id
        expires_at = $null
        kind = @{
            text = @{
                utf8_len = [Text.Encoding]::UTF8.GetByteCount($applyText)
                preview = $null
                encrypted_body_ref = @{
                    id = "inline:smoke"
                    byte_len = [Text.Encoding]::UTF8.GetByteCount($applyText)
                }
                encrypted_inline_body = $applyText
            }
        }
        encryption = @{
            scheme = "mvp-inline-placeholder"
            key_wrapped_for = @()
        }
    } | ConvertTo-Json -Depth 20) | Out-Null

    Start-Sleep -Seconds 2
    $clipboard = Get-Clipboard -Raw
    if ($clipboard.TrimEnd() -ne $applyText) {
        throw "apply smoke failed: clipboard was '$clipboard'"
    }
    Stop-Process -Id $applier.Id -Force

    Write-Host "File manifest path"
    $fileReceiverPair = New-PairCode $publisherDeviceId
    $fileReceiverArgs = @(
        "--server-url", $baseUrl,
        "--device-name", "smoke-file-receiver",
        "--state-path", $fileReceiveState,
        "--pair-code", $fileReceiverPair.code,
        "--publish-clipboard=false",
        "--auto-apply-files=true",
        "--remote-paste-hotkey=false",
        "--peer-bind", "127.0.0.1:17392",
        "--cache-dir", $fileReceiveCache
    ) + $authArgs
    $fileReceiver = Start-Process -FilePath $agentExe -ArgumentList $fileReceiverArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru

    $filePublisherPair = New-PairCode $publisherDeviceId
    $filePublisherArgs = @(
        "--server-url", $baseUrl,
        "--device-name", "smoke-file-publisher",
        "--state-path", $filePublishState,
        "--pair-code", $filePublisherPair.code,
        "--apply-remote=false",
        "--remote-paste-hotkey=false",
        "--peer-bind", "127.0.0.1:17391",
        "--peer-public-url", "http://127.0.0.1:17391"
    ) + $authArgs
    $filePublisher = Start-Process -FilePath $agentExe -ArgumentList $filePublisherArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2

    $sampleFile = Join-Path $root "target\agent-smoke-file.txt"
    $sampleContent = "airpaste file smoke $(Get-Date -Format o)"
    Set-Content -LiteralPath $sampleFile -Value $sampleContent -NoNewline
    Set-Clipboard -LiteralPath $sampleFile
    Start-Sleep -Seconds 2
    $fileReceiverDeviceId = Get-AgentDeviceId $fileReceiveState
    $fileReceiverHeaders = New-AirPasteHeaders $fileReceiverDeviceId
    $fileClip = Invoke-RestMethod "$baseUrl/v1/clips/latest" -Headers $fileReceiverHeaders
    if ($null -eq $fileClip.kind.files) {
        throw "file manifest smoke failed: latest clip was not files"
    }
    if ($fileClip.kind.files.files[0].display_name -ne "agent-smoke-file.txt") {
        throw "file manifest smoke failed: unexpected display name"
    }
    $downloaded = Join-Path $fileReceiveCache (Join-Path $fileClip.kind.files.transfer_token "agent-smoke-file.txt")
    $deadline = (Get-Date).AddSeconds(10)
    while (!(Test-Path -LiteralPath $downloaded) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 250
    }
    if (!(Test-Path -LiteralPath $downloaded)) {
        throw "file transfer smoke failed: downloaded file missing"
    }
    $downloadedContent = Get-Content -LiteralPath $downloaded -Raw
    if ($downloadedContent -ne $sampleContent) {
        throw "file transfer smoke failed: downloaded content mismatch"
    }
    $peerFileUrl = "http://127.0.0.1:17391/v1/files/$($fileClip.kind.files.transfer_token)/0"
    try {
        Invoke-WebRequest $peerFileUrl -UseBasicParsing | Out-Null
        throw "file transfer smoke failed: unauthenticated peer download succeeded"
    }
    catch {
        $statusCode = $null
        if ($_.Exception.Response -and $_.Exception.Response.StatusCode) {
            $statusCode = [int]$_.Exception.Response.StatusCode
        }
        if ($statusCode -ne 401) {
            throw
        }
    }
    $clipboardFiles = @(Get-Clipboard -Format FileDropList)
    if ($clipboardFiles.Count -lt 1) {
        throw "file clipboard smoke failed: clipboard has no file drop list"
    }
    if ($clipboardFiles[0].FullName -ne $downloaded) {
        throw "file clipboard smoke failed: expected '$downloaded', got '$($clipboardFiles[0].FullName)'"
    }
    Stop-Process -Id $filePublisher.Id -Force
    Stop-Process -Id $fileReceiver.Id -Force

    Write-Host "Agent smoke passed"
}
finally {
    Get-Process airpaste-agent -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $agentExe } | Stop-Process -Force
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
}
