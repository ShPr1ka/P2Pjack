# Собирает P2PJack и установщик с вложенной программой: dist\P2PJack-Setup.exe
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

cargo build --release -p p2pjack
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Драйвер Wintun кладём в установщик, чтобы первый запуск не зависел от интернета.
if (-not (Test-Path "target\release\wintun.dll")) {
    & "$PSScriptRoot\get-wintun.ps1" -Dest "target\release"
}

cargo build --release -p p2pjack-setup
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

New-Item -ItemType Directory -Force -Path dist | Out-Null
Copy-Item target\release\P2PJack-Setup.exe dist\ -Force
Write-Host "Готово: $(Resolve-Path dist\P2PJack-Setup.exe)"
