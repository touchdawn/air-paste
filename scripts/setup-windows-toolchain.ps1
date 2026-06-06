param(
    [string]$Proxy = ""
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if ($Proxy) {
    $env:HTTP_PROXY = $Proxy
    $env:HTTPS_PROXY = $Proxy
}

if (!(Get-Command rustup -ErrorAction SilentlyContinue)) {
    curl.exe -L https://win.rustup.rs/x86_64 -o rustup-init.exe
    .\rustup-init.exe -y --default-toolchain stable-x86_64-pc-windows-gnu
}

rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
rustup component add rustfmt --toolchain stable-x86_64-pc-windows-gnu

$tools = Join-Path (Get-Location) "tools"
$mingwGcc = Join-Path $tools "winlibs\mingw64\bin\x86_64-w64-mingw32-gcc.exe"
if (!(Test-Path $mingwGcc)) {
    New-Item -ItemType Directory -Force $tools | Out-Null
    $url = "https://github.com/brechtsanders/winlibs_mingw/releases/download/16.1.0posix-14.0.0-ucrt-r2/winlibs-x86_64-posix-seh-gcc-16.1.0-mingw-w64ucrt-14.0.0-r2.zip"
    $zip = Join-Path $tools "winlibs-x86_64.zip"
    curl.exe -L $url -o $zip
    Expand-Archive -Force $zip (Join-Path $tools "winlibs")
}

Write-Host "Rust and portable MinGW are ready."
Write-Host "Add this to PATH before cargo build/run:"
Write-Host "$tools\winlibs\mingw64\bin"
