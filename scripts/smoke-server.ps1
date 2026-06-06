param(
    [string]$BaseUrl = "http://127.0.0.1:8080",
    [string]$AuthToken = ""
)

$ErrorActionPreference = "Stop"
$authHeaders = @{}
if ($AuthToken) {
    $authHeaders["Authorization"] = "Bearer $AuthToken"
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

Write-Host "Health"
Invoke-RestMethod "$BaseUrl/health" | ConvertTo-Json -Depth 20

if ($AuthToken) {
    Write-Host "Auth guard"
    Expect-HttpStatus 401 {
        Invoke-RestMethod "$BaseUrl/v1/devices" | Out-Null
    }
}

Write-Host "Register device"
$device = Invoke-RestMethod "$BaseUrl/v1/devices" -Method Post -Headers $authHeaders -ContentType "application/json" -Body (@{
    name = "smoke-device"
    public_key = "test-public-key"
} | ConvertTo-Json)
$device | ConvertTo-Json -Depth 20
$deviceHeaders = New-AirPasteHeaders $device.device.device_id

Write-Host "Trusted device guard"
Expect-HttpStatus 401 {
    Invoke-RestMethod "$BaseUrl/v1/clips/latest" -Headers $authHeaders | Out-Null
}
$untrusted = Invoke-RestMethod "$BaseUrl/v1/devices" -Method Post -Headers $authHeaders -ContentType "application/json" -Body (@{
    name = "smoke-untrusted"
    public_key = "test-untrusted-key"
} | ConvertTo-Json)
$untrustedHeaders = New-AirPasteHeaders $untrusted.device.device_id
Expect-HttpStatus 403 {
    Invoke-RestMethod "$BaseUrl/v1/clips/latest" -Headers $untrustedHeaders | Out-Null
}

Write-Host "Start pairing"
$pair = Invoke-RestMethod "$BaseUrl/v1/pair/start" -Method Post -Headers $deviceHeaders -ContentType "application/json" -Body (@{
    created_by = $device.device.device_id
    ttl_seconds = 600
} | ConvertTo-Json)
$pair | ConvertTo-Json -Depth 20

Write-Host "Confirm pairing"
Invoke-RestMethod "$BaseUrl/v1/pair/confirm" -Method Post -Headers $authHeaders -ContentType "application/json" -Body (@{
    code = $pair.code
    device_id = $untrusted.device.device_id
} | ConvertTo-Json) | ConvertTo-Json -Depth 20
$pairedHeaders = New-AirPasteHeaders $untrusted.device.device_id
Invoke-RestMethod "$BaseUrl/v1/devices" -Headers $pairedHeaders | ConvertTo-Json -Depth 20

Write-Host "Create text clip"
$clip = Invoke-RestMethod "$BaseUrl/v1/clips" -Method Post -Headers $deviceHeaders -ContentType "application/json" -Body (@{
    source_device_id = $device.device.device_id
    expires_at = $null
    kind = @{
        text = @{
            utf8_len = 11
            preview = $null
            encrypted_body_ref = @{
                id = "blob_smoke"
                byte_len = 32
            }
            encrypted_inline_body = "hello smoke"
        }
    }
    encryption = @{
        scheme = "mvp-placeholder"
        key_wrapped_for = @($device.device.device_id)
    }
} | ConvertTo-Json -Depth 20)
$clip | ConvertTo-Json -Depth 20

Write-Host "Latest clip"
Invoke-RestMethod "$BaseUrl/v1/clips/latest" -Headers $pairedHeaders | ConvertTo-Json -Depth 20

Write-Host "Create relay session"
Invoke-RestMethod "$BaseUrl/v1/relay/sessions" -Method Post -Headers $deviceHeaders -ContentType "application/json" -Body (@{
    clip_id = $clip.clip_id
    source_device_id = $device.device.device_id
    recipient_device_id = $untrusted.device.device_id
    max_bytes = 1048576
    ttl_seconds = 600
} | ConvertTo-Json) | ConvertTo-Json -Depth 20
