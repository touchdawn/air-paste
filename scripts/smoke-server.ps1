param(
    [string]$BaseUrl = "http://127.0.0.1:8080",
    [string]$AuthToken = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$agentExe = Join-Path $root "target\debug\airpaste-agent.exe"
$trustedState = Join-Path $root "target\server-smoke-trusted.json"
$untrustedState = Join-Path $root "target\server-smoke-untrusted.json"
$untrustedError = Join-Path $root "target\server-smoke-untrusted.err"
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
    if (!(Test-Path -LiteralPath $StatePath)) {
        throw "agent state missing: $StatePath"
    }
    $state = Get-Content -LiteralPath $StatePath -Raw | ConvertFrom-Json
    if (!$state.device_id) {
        throw "agent state does not contain device_id: $StatePath"
    }
    return [string]$state.device_id
}

function Invoke-AgentJson([string[]]$ArgsList) {
    $json = & $agentExe @ArgsList
    if ($LASTEXITCODE -ne 0) {
        throw "agent command failed: $($ArgsList -join ' ')"
    }
    return $json | ConvertFrom-Json
}

function Invoke-AgentOneShot([string]$StatePath, [string[]]$ExtraArgs) {
    $args = @(
        "--server-url", $BaseUrl,
        "--state-path", $StatePath,
        "--publish-clipboard=false",
        "--apply-remote=false",
        "--remote-paste-hotkey=false"
    ) + $authArgs + $ExtraArgs
    return Invoke-AgentJson $args
}

Remove-Item -Force -ErrorAction SilentlyContinue $trustedState, $untrustedState, $untrustedError

Write-Host "Health"
Invoke-RestMethod "$BaseUrl/health" | ConvertTo-Json -Depth 20

if ($AuthToken) {
    Write-Host "Auth guard"
    Expect-HttpStatus 401 {
        Invoke-RestMethod "$BaseUrl/v1/devices" | Out-Null
    }
}

Write-Host "Register device"
Invoke-AgentOneShot $trustedState @("--print-latest-clip", "--device-name", "smoke-device") | ConvertTo-Json -Depth 20
$deviceId = Get-AgentDeviceId $trustedState

Write-Host "Trusted device guard"
Expect-HttpStatus 401 {
    Invoke-RestMethod "$BaseUrl/v1/clips/latest" -Headers $authHeaders | Out-Null
}

Write-Host "Start pairing"
$pair = Invoke-AgentOneShot $trustedState @("--create-pair-code", "--pair-ttl-seconds", "600")
$pair | ConvertTo-Json -Depth 20

Write-Host "Untrusted device guard"
try {
    $args = @(
        "--server-url", $BaseUrl,
        "--state-path", $untrustedState,
        "--publish-clipboard=false",
        "--apply-remote=false",
        "--remote-paste-hotkey=false",
        "--print-latest-clip",
        "--device-name", "smoke-untrusted"
    ) + $authArgs
    & $agentExe @args 2> $untrustedError | Out-Null
    if ($LASTEXITCODE -ne 0) {
        $errorText = Get-Content -LiteralPath $untrustedError -Raw
        if ($errorText -notmatch "403 Forbidden") {
            throw "expected untrusted device to fail with 403, got: $errorText"
        }
        throw "untrusted device was rejected"
    }
    throw "untrusted device unexpectedly read latest clip"
}
catch {
    if ($_.Exception.Message -like "untrusted device unexpectedly*") {
        throw
    }
}
$untrustedDeviceId = Get-AgentDeviceId $untrustedState

Write-Host "Confirm pairing"
Invoke-RestMethod "$BaseUrl/v1/pair/confirm" -Method Post -Headers $authHeaders -ContentType "application/json" -Body (@{
    code = $pair.code
    device_id = $untrustedDeviceId
} | ConvertTo-Json) | ConvertTo-Json -Depth 20
Invoke-AgentOneShot $untrustedState @("--print-latest-clip") | ConvertTo-Json -Depth 20

Write-Host "Create text clip"
$clip = Invoke-AgentOneShot $trustedState @("--publish-text-once", "hello smoke")
$clip | ConvertTo-Json -Depth 20

Write-Host "Latest clip"
$latestClip = Invoke-AgentOneShot $untrustedState @("--print-latest-clip")
$latestClip | ConvertTo-Json -Depth 20
if (!$latestClip.expires_at) {
    throw "text clip ttl smoke failed: latest text clip did not include expires_at"
}

Write-Host "Text TTL guard"
$shortTtlClip = Invoke-AgentOneShot $trustedState @("--publish-text-once", "short ttl smoke", "--text-clip-ttl-secs", "1")
Start-Sleep -Seconds 2
$latestAfterTtl = Invoke-AgentOneShot $untrustedState @("--print-latest-clip")
if ($latestAfterTtl.clip_id -eq $shortTtlClip.clip_id) {
    throw "text ttl smoke failed: expired text clip was still latest"
}

Write-Host "Replay guard"
Invoke-AgentOneShot $trustedState @("--replay-latest-clip-signature") | ConvertTo-Json -Depth 20

Write-Host "Create relay session"
Invoke-AgentOneShot $trustedState @(
    "--create-relay-for-clip", $clip.clip_id,
    "--relay-recipient-device-id", $untrustedDeviceId,
    "--relay-max-bytes", "1048576",
    "--relay-ttl-seconds", "600"
) | ConvertTo-Json -Depth 20
