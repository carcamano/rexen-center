# Builds the Windows packages (the release workflow runs it on
# windows-latest):
#   <out>\rexen-center-<version>-windows-<arch>.zip   always
#   <out>\rexen-center-<version>-windows-<arch>.msi   when the WiX Toolset 5 is installed
# (the names come from crates\xen-core\src\branding.rs).
#
# Usage: powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1 [-Out <dir>]
#
# Build needs: Rust with the MSVC toolchain (Visual Studio Build Tools).
# The icon (packaging\icons\icon.ico) is embedded in rexencenter.exe by the
# crate's build script, with the resource compiler of the Windows SDK.
# The zip holds rexencenter.exe, LICENSE and README.md; unzip anywhere and run
# rexencenter.exe. Put an empty file named "portable" next to it for portable
# mode (settings and logs beside the executable). The installer
# (rexen-center.wxs) needs the `wix` command line:
#   dotnet tool install --global wix --version 5.0.2
# Neither is signed.
param([string]$Out = "")
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
if ($Out -eq "") { $Out = Join-Path $root "target\dist" }
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Out = Resolve-Path $Out
$branding = Get-Content -Encoding UTF8 (Join-Path $root "crates\xen-core\src\branding.rs")
function Get-Brand([string]$const) {
    $m = $branding | Select-String -Pattern ('^pub const ' + $const + ': &str = "(.*)";$') | Select-Object -First 1
    if (-not $m) { throw "cannot read $const from crates\xen-core\src\branding.rs" }
    $m.Matches.Groups[1].Value
}
$name = Get-Brand "APP_ID"
$bin = Get-Brand "BINARY_NAME"
$productName = Get-Brand "PRODUCT_NAME"
$version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version = "(.*)"' |
    Select-Object -First 1).Matches.Groups[1].Value
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }

cargo build --manifest-path (Join-Path $root "Cargo.toml") --release --locked -p xenadmin
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
$exe = Join-Path $targetDir "release\$bin.exe"

$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("$name-" + [guid]::NewGuid())
$dir = Join-Path $stage "$name-$version"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Copy-Item $exe $dir
Copy-Item (Join-Path $root "LICENSE") $dir
Copy-Item (Join-Path $root "README.md") $dir

$zip = Join-Path $Out "$name-$version-windows-$arch.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path $dir -DestinationPath $zip
Write-Output $zip

if (Get-Command wix -ErrorAction SilentlyContinue) {
    $msi = Join-Path $Out "$name-$version-windows-$arch.msi"
    if (Test-Path $msi) { Remove-Item $msi }
    $built = Join-Path $stage "$name.msi"
    wix build -arch $arch `
        -d "ProductName=$productName" `
        -d "Version=$version" `
        -d "BinFile=$exe" `
        -d "IconFile=$(Join-Path $root 'packaging\icons\icon.ico')" `
        -d "LicenseFile=$(Join-Path $root 'LICENSE')" `
        -o $built (Join-Path $root "packaging\windows\rexen-center.wxs")
    if ($LASTEXITCODE -ne 0) { throw "wix build failed" }
    Copy-Item $built $msi
    Write-Output $msi
}

Remove-Item -Recurse -Force $stage
