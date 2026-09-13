# Builds autospot in release mode and packs the exe + config into a zip
# under dist/, named after the version in Cargo.toml.

$ErrorActionPreference = "Stop"

$root = $PSScriptRoot
Set-Location $root

Write-Host "Building release..."
cargo build --release
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE"
}

$cargoToml = Get-Content "$root\Cargo.toml" -Raw
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    throw "Could not find version in Cargo.toml"
}
$version = $Matches[1]

$exePath = "$root\target\release\autospot.exe"
$tomlPath = "$root\autospot.toml"
if (-not (Test-Path $exePath)) { throw "Missing $exePath" }
if (-not (Test-Path $tomlPath)) { throw "Missing $tomlPath" }

$distDir = "$root\dist"
New-Item -ItemType Directory -Force -Path $distDir | Out-Null

$zipPath = "$distDir\autospot-v$version.zip"
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }

Compress-Archive -Path $exePath, $tomlPath -DestinationPath $zipPath

Write-Host "Created $zipPath"
