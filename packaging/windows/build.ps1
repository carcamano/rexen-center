# Builds the Windows package (not built or run on the development machine;
# see README.md):
#   <out>\xenadmin-rs-<version>-windows-<arch>.zip
#
# Usage: powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1 [-Out <dir>]
#
# Build needs: Rust with the MSVC toolchain (Visual Studio Build Tools).
# The zip holds xenadmin.exe, LICENSE and README.md; unzip anywhere and run
# xenadmin.exe. Put an empty file named "portable" next to it for portable
# mode (settings and logs beside the executable). Not signed.
param([string]$Out = "")
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
if ($Out -eq "") { $Out = Join-Path $root "target\dist" }
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$name = "xenadmin-rs"
$version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version = "(.*)"' |
    Select-Object -First 1).Matches.Groups[1].Value
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }

cargo build --manifest-path (Join-Path $root "Cargo.toml") --release --locked -p xenadmin
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("$name-" + [guid]::NewGuid())
$dir = Join-Path $stage "$name-$version"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Copy-Item (Join-Path $root "target\release\xenadmin.exe") $dir
Copy-Item (Join-Path $root "LICENSE") $dir
Copy-Item (Join-Path $root "README.md") $dir

$zip = Join-Path $Out "$name-$version-windows-$arch.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path $dir -DestinationPath $zip
Remove-Item -Recurse -Force $stage
Write-Output $zip
