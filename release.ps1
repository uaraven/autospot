# Builds autospot in release mode and packs the exe + config into a zip
# under dist/, named after the version in Cargo.toml. Also packs the companion
# microcontroller code into a separate companion-v{version}.zip.

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

$companionDir = "$root\companion"
$companionPaths = @(
    "$companionDir\boot.py",
    "$companionDir\code.py",
    "$companionDir\LICENSE",
    "$companionDir\install.txt",
    "$companionDir\board"
)
foreach ($path in $companionPaths) {
    if (-not (Test-Path $path)) { throw "Missing $path" }
}

$companionZipPath = "$distDir\companion-v$version.zip"
if (Test-Path $companionZipPath) { Remove-Item $companionZipPath -Force }

Compress-Archive -Path $companionPaths -DestinationPath $companionZipPath

Write-Host "Created $companionZipPath"
