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
$fileLimitState = Join-Path $root "target\agent-file-limit.json"
$fileReceiveCache = Join-Path $root "target\agent-file-cache"
$baseUrl = "http://$Bind"
$authHeaders = @{}
$authArgs = @()
if ($AuthToken) {
    $authHeaders["Authorization"] = "Bearer $AuthToken"
    $authArgs = @("--auth-token", $AuthToken)
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

function New-PairCode {
    $args = @(
        "--server-url", $baseUrl,
        "--state-path", $publishState,
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

function Get-LatestClip() {
    $args = @(
        "--server-url", $baseUrl,
        "--state-path", $publishState,
        "--print-latest-clip",
        "--publish-clipboard=false",
        "--apply-remote=false",
        "--remote-paste-hotkey=false"
    ) + $authArgs
    $json = & $agentExe @args
    if ($LASTEXITCODE -ne 0) {
        throw "failed to read latest clip"
    }
    return $json | ConvertFrom-Json
}

Remove-Item -Force -Recurse -ErrorAction SilentlyContinue $db, $publishState, $applyState, $filePublishState, $fileReceiveState, $fileLimitState, $fileReceiveCache

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
        "--remote-paste-hotkey=false",
        "--peer-bind", "127.0.0.1:17388"
    ) + $authArgs
    $publisher = Start-Process -FilePath $agentExe -ArgumentList $publisherArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2
    $publisherDeviceId = Get-AgentDeviceId $publishState
    Expect-HttpStatus 401 {
        Invoke-RestMethod "$baseUrl/v1/clips/latest" -Headers $authHeaders | Out-Null
    }
    $publishText = "airpaste publish smoke $(Get-Date -Format o)"
    Set-Clipboard -Value $publishText
    Start-Sleep -Seconds 2
    $latest = Get-LatestClip
    if ($latest.kind.text.encrypted_inline_body -ne $publishText) {
        throw "publish smoke failed"
    }
    if (!$latest.expires_at) {
        throw "publish smoke failed: text clip did not include expires_at"
    }
    $sensitiveText = "DATABASE_PASSWORD=airpaste-smoke-secret"
    Set-Clipboard -Value $sensitiveText
    Start-Sleep -Seconds 2
    $latestAfterSensitive = Get-LatestClip
    if ($latestAfterSensitive.kind.text.encrypted_inline_body -eq $sensitiveText) {
        throw "sensitive text filter smoke failed: secret-like text was published"
    }

    Write-Host "Apply path"
    $applierPair = New-PairCode
    $applierArgs = @(
        "--server-url", $baseUrl,
        "--device-name", "smoke-applier",
        "--state-path", $applyState,
        "--pair-code", $applierPair.code,
        "--publish-clipboard=false",
        "--remote-paste-hotkey=false",
        "--peer-bind", "127.0.0.1:17389"
    ) + $authArgs
    $applier = Start-Process -FilePath $agentExe -ArgumentList $applierArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2
    $applierDeviceId = Get-AgentDeviceId $applyState

    $applyText = "airpaste apply smoke $(Get-Date -Format o)"
    Set-Clipboard -Value $applyText

    Start-Sleep -Seconds 2
    $clipboard = Get-Clipboard -Raw
    if ($clipboard.TrimEnd() -ne $applyText) {
        throw "apply smoke failed: clipboard was '$clipboard'"
    }
    Stop-Process -Id $applier.Id -Force -ErrorAction SilentlyContinue
    Stop-Process -Id $publisher.Id -Force -ErrorAction SilentlyContinue

    Write-Host "File manifest path"
    $fileReceiverPair = New-PairCode
    $fileReceiverArgs = @(
        "--server-url", $baseUrl,
        "--device-name", "smoke-file-receiver",
        "--state-path", $fileReceiveState,
        "--pair-code", $fileReceiverPair.code,
        "--publish-clipboard=false",
        "--auto-apply-files=true",
        "--remote-paste-hotkey=false",
        "--max-single-file-bytes", "1048576",
        "--peer-bind", "127.0.0.1:17392",
        "--cache-dir", $fileReceiveCache
    ) + $authArgs
    $fileReceiver = Start-Process -FilePath $agentExe -ArgumentList $fileReceiverArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru

    $filePublisherPair = New-PairCode
    $filePublisherArgs = @(
        "--server-url", $baseUrl,
        "--device-name", "smoke-file-publisher",
        "--state-path", $filePublishState,
        "--pair-code", $filePublisherPair.code,
        "--apply-remote=false",
        "--remote-paste-hotkey=false",
        "--max-single-file-bytes", "1048576",
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
    $fileClip = Get-LatestClip
    if ($null -eq $fileClip.kind.files) {
        throw "file manifest smoke failed: latest clip was not files"
    }
    if ($fileClip.kind.files.files[0].display_name -ne "agent-smoke-file.txt") {
        throw "file manifest smoke failed: unexpected display name"
    }
    $manifestSha256 = [string]$fileClip.kind.files.files[0].sha256
    if (![regex]::IsMatch($manifestSha256, '^[0-9a-f]{64}$')) {
        throw "file manifest smoke failed: sha256 was not 64-char lowercase hex"
    }
    $sampleSha256 = (Get-FileHash -LiteralPath $sampleFile -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($manifestSha256 -ne $sampleSha256) {
        throw "file manifest smoke failed: sha256 did not match source file"
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
    $downloadedSha256 = (Get-FileHash -LiteralPath $downloaded -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($downloadedSha256 -ne $manifestSha256) {
        throw "file transfer smoke failed: downloaded sha256 mismatch"
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
    Stop-Process -Id $filePublisher.Id -Force -ErrorAction SilentlyContinue
    Stop-Process -Id $fileReceiver.Id -Force -ErrorAction SilentlyContinue

    Write-Host "Single-file limit guard"
    $latestBeforeLimit = Get-LatestClip
    $fileLimitPair = New-PairCode
    $fileLimitArgs = @(
        "--server-url", $baseUrl,
        "--device-name", "smoke-file-limit",
        "--state-path", $fileLimitState,
        "--pair-code", $fileLimitPair.code,
        "--apply-remote=false",
        "--remote-paste-hotkey=false",
        "--max-single-file-bytes", "1",
        "--peer-bind", "127.0.0.1:17393",
        "--peer-public-url", "http://127.0.0.1:17393"
    ) + $authArgs
    $fileLimitPublisher = Start-Process -FilePath $agentExe -ArgumentList $fileLimitArgs -WorkingDirectory $root -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2

    $oversizedFile = Join-Path $root "target\agent-smoke-file-limit.txt"
    Set-Content -LiteralPath $oversizedFile -Value "xx" -NoNewline
    Set-Clipboard -LiteralPath $oversizedFile
    Start-Sleep -Seconds 2
    $latestAfterLimit = Get-LatestClip
    if ($latestAfterLimit.clip_id -ne $latestBeforeLimit.clip_id) {
        throw "single-file limit smoke failed: oversized file was published"
    }
    Stop-Process -Id $fileLimitPublisher.Id -Force -ErrorAction SilentlyContinue

    Write-Host "Agent smoke passed"
}
finally {
    Get-Process airpaste-agent -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $agentExe } | Stop-Process -Force
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
}
