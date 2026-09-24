# maya_mcp build script
#
# Usage: powershell -ExecutionPolicy Bypass -File build.ps1 [-Clean]

param([switch]$Clean)

$ErrorActionPreference = 'Stop'
$ProjectRoot = $PSScriptRoot

if ($Clean) {
    Write-Host "[clean] cargo clean ..."
    cargo clean
    if ($LASTEXITCODE -ne 0) { throw "[clean] cargo clean failed" }
}

Write-Host "[build] cargo build --release ..."
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "[build] cargo build failed" }

$exe = Join-Path $ProjectRoot 'target\release\maya_mcp.exe'
if (-not (Test-Path $exe)) { throw "[build] binary not found: $exe" }
$size = (Get-Item $exe).Length / 1MB
Write-Host ("[build] OK: {0} ({1:N1} MB)" -f $exe, $size)
