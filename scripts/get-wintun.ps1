# Скачивает официальный драйвер Wintun (https://www.wintun.net, авторы WireGuard)
# и кладёт wintun.dll рядом с p2pjack.exe.
param([string]$Dest = (Join-Path $PSScriptRoot "..\target\release"))

$ErrorActionPreference = "Stop"
$version = "0.14.1"
$url = "https://www.wintun.net/builds/wintun-$version.zip"
$tmp = Join-Path $env:TEMP "wintun-$version"
$zip = "$tmp.zip"

Write-Host "Скачиваем $url"
Invoke-WebRequest -Uri $url -OutFile $zip
Expand-Archive -Path $zip -DestinationPath $tmp -Force

$arch = switch ($env:PROCESSOR_ARCHITECTURE) { "ARM64" { "arm64" } "x86" { "x86" } default { "amd64" } }
New-Item -ItemType Directory -Force -Path $Dest | Out-Null
Copy-Item (Join-Path $tmp "wintun\bin\$arch\wintun.dll") $Dest -Force
Write-Host "Готово: $(Resolve-Path (Join-Path $Dest 'wintun.dll'))"
