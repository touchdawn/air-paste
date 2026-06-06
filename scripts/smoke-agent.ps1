param(
    [string]$Bind = "127.0.0.1:18081"
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$serverExe = Join-Path $root "target\debug\airpaste-server.exe"
$agentExe = Join-Path $root "target\debug\airpaste-agent.exe"
$db = Join-Path $root "target\agent-smoke.redb"
$publishState = Join-Path $root "target\agent-publish.json"
$applyState = Join-Path $root "target\agent-apply.json"
$baseUrl = "http://$Bind"

Remove-Item -Force -ErrorAction SilentlyContinue $db, $publishState, $applyState

$server = Start-Process -FilePath $serverExe -ArgumentList "--bind", $Bind, "--db", $db -WorkingDirectory $root -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2

try {
    Invoke-RestMethod "$baseUrl/health" | Out-Null

    Write-Host "Publish path"
    $publisher = Start-Process -FilePath $agentExe -ArgumentList `
        "--server-url", $baseUrl, `
        "--device-name", "smoke-publisher", `
        "--state-path", $publishState, `
        "--apply-remote=false" `
        -WorkingDirectory $root -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2
    $publishText = "airpaste publish smoke $(Get-Date -Format o)"
    Set-Clipboard -Value $publishText
    Start-Sleep -Seconds 2
    $latest = Invoke-RestMethod "$baseUrl/v1/clips/latest"
    if ($latest.kind.text.encrypted_inline_body -ne $publishText) {
        throw "publish smoke failed"
    }
    Stop-Process -Id $publisher.Id -Force

    Write-Host "Apply path"
    $applier = Start-Process -FilePath $agentExe -ArgumentList `
        "--server-url", $baseUrl, `
        "--device-name", "smoke-applier", `
        "--state-path", $applyState, `
        "--publish-clipboard=false" `
        -WorkingDirectory $root -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2

    $source = Invoke-RestMethod "$baseUrl/v1/devices" -Method Post -ContentType "application/json" -Body (@{
        name = "smoke-remote-source"
        public_key = "smoke-key"
    } | ConvertTo-Json)

    $applyText = "airpaste apply smoke $(Get-Date -Format o)"
    Invoke-RestMethod "$baseUrl/v1/clips" -Method Post -ContentType "application/json" -Body (@{
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
    $filePublisher = Start-Process -FilePath $agentExe -ArgumentList `
        "--server-url", $baseUrl, `
        "--device-name", "smoke-file-publisher", `
        "--state-path", (Join-Path $root "target\agent-file-publish.json"), `
        "--apply-remote=false" `
        -WorkingDirectory $root -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2
    $sampleFile = Join-Path $root "target\agent-smoke-file.txt"
    Set-Content -LiteralPath $sampleFile -Value "airpaste file smoke"
    Set-Clipboard -LiteralPath $sampleFile
    Start-Sleep -Seconds 2
    $fileClip = Invoke-RestMethod "$baseUrl/v1/clips/latest"
    if ($null -eq $fileClip.kind.files) {
        throw "file manifest smoke failed: latest clip was not files"
    }
    if ($fileClip.kind.files.files[0].display_name -ne "agent-smoke-file.txt") {
        throw "file manifest smoke failed: unexpected display name"
    }
    Stop-Process -Id $filePublisher.Id -Force

    Write-Host "Agent smoke passed"
}
finally {
    Get-Process airpaste-agent -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $agentExe } | Stop-Process -Force
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
}
