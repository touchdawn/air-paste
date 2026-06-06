param(
    [string]$BaseUrl = "http://127.0.0.1:8080"
)

$ErrorActionPreference = "Stop"

Write-Host "Health"
Invoke-RestMethod "$BaseUrl/health" | ConvertTo-Json -Depth 20

Write-Host "Register device"
$device = Invoke-RestMethod "$BaseUrl/v1/devices" -Method Post -ContentType "application/json" -Body (@{
    name = "smoke-device"
    public_key = "test-public-key"
} | ConvertTo-Json)
$device | ConvertTo-Json -Depth 20

Write-Host "Start pairing"
$pair = Invoke-RestMethod "$BaseUrl/v1/pair/start" -Method Post -ContentType "application/json" -Body (@{
    created_by = $null
    ttl_seconds = 600
} | ConvertTo-Json)
$pair | ConvertTo-Json -Depth 20

Write-Host "Confirm pairing"
Invoke-RestMethod "$BaseUrl/v1/pair/confirm" -Method Post -ContentType "application/json" -Body (@{
    code = $pair.code
    device_id = $device.device.device_id
} | ConvertTo-Json) | ConvertTo-Json -Depth 20

Write-Host "Create text clip"
$clip = Invoke-RestMethod "$BaseUrl/v1/clips" -Method Post -ContentType "application/json" -Body (@{
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
Invoke-RestMethod "$BaseUrl/v1/clips/latest" | ConvertTo-Json -Depth 20

Write-Host "Create relay session"
Invoke-RestMethod "$BaseUrl/v1/relay/sessions" -Method Post -ContentType "application/json" -Body (@{
    clip_id = $clip.clip_id
    source_device_id = $device.device.device_id
    recipient_device_id = $device.device.device_id
    max_bytes = 1048576
    ttl_seconds = 600
} | ConvertTo-Json) | ConvertTo-Json -Depth 20
