param(
    [string]$Remote = "origin",
    [string]$Branch = "main",
    [string]$Toolchain = "1.88.0-x86_64-pc-windows-msvc",
    [string]$Proxy = "",
    [switch]$Release,
    [switch]$NoPull,
    [switch]$NoStart,
    [switch]$KeepExisting
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$profile = if ($Release) { "release" } else { "debug" }

function Invoke-Checked([scriptblock]$Command, [string]$FailureMessage) {
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw $FailureMessage
    }
}

Push-Location $root
try {
    if ($Proxy) {
        $env:HTTP_PROXY = $Proxy
        $env:HTTPS_PROXY = $Proxy
    }
    if (-not $env:CARGO_REGISTRIES_CRATES_IO_PROTOCOL) {
        $env:CARGO_REGISTRIES_CRATES_IO_PROTOCOL = "sparse"
    }

    if (-not $KeepExisting) {
        $processes = Get-Process -ErrorAction SilentlyContinue |
            Where-Object { $_.ProcessName -in @("airpaste-tray", "airpaste-agent", "airpaste-server") }
        foreach ($process in $processes) {
            Write-Host "Stopping $($process.ProcessName) (PID $($process.Id))..."
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        }
        if ($processes) {
            Start-Sleep -Seconds 1
        }
    }

    if (-not $NoPull) {
        Write-Host "Pulling $Remote/$Branch..."
        Invoke-Checked { git pull --ff-only $Remote $Branch } "git pull failed"
    }

    Write-Host "Building workspace ($profile, toolchain $Toolchain)..."
    if ($Release) {
        Invoke-Checked { cargo "+$Toolchain" build --workspace --release } "cargo build failed"
    } else {
        Invoke-Checked { cargo "+$Toolchain" build --workspace } "cargo build failed"
    }

    $trayExe = Join-Path $root "target\$profile\airpaste-tray.exe"
    if (-not (Test-Path -LiteralPath $trayExe)) {
        throw "missing tray executable: $trayExe"
    }

    if (-not $NoStart) {
        Write-Host "Starting tray..."
        $startInfo = @{
            FilePath = $trayExe
            WorkingDirectory = $root
            PassThru = $true
        }
        if ($Release) {
            $startInfo.WindowStyle = "Hidden"
        } else {
            $startInfo.WindowStyle = "Normal"
        }
        $process = Start-Process @startInfo
        Start-Sleep -Seconds 2
        if (Get-Process -Id $process.Id -ErrorAction SilentlyContinue) {
            Write-Host "Started airpaste-tray PID $($process.Id)"
        } else {
            throw "airpaste-tray exited immediately"
        }
    }

    Write-Host "Done."
} finally {
    Pop-Location
}
