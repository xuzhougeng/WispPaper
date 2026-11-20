# Build Wisp Study for Windows and package it twice, from the same folder:
# - dist/WispStudy-v<version>-windows-x64.zip, the portable one: WispStudy.exe
#   (the app) and wisp.exe (the command line) in a folder of the same name;
# - dist/WispStudy-v<version>-windows-x64-setup.exe, the setup that installs
#   those files for the current user (scripts/wisp-study.iss, Inno Setup 6).
#
#   pwsh -File scripts/package-windows.ps1               # unsigned, for a look on this machine
#   pwsh -File scripts/package-windows.ps1 -NoInstaller  # the zip only, where Inno Setup is not installed
#   pwsh -File scripts/package-windows.ps1 -Sign         # Authenticode-sign all three, as the release does
#
# -Sign goes through scripts/sign-windows-esigner.ps1 (SSL.com eSigner): run that
# script with -Install once before, and have the ESIGNER_* variables set. A
# signature that fails stops the packaging, so no half-signed package is written.
# The setup holds the signed exes and is signed itself; the uninstaller it
# writes is not.
#
# Environment: CARGO_TARGET_DIR is where cargo builds (default: target).

param(
    [switch]$Sign,
    [switch]$NoInstaller
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $root

# The version is written once, in the root Cargo.toml.
$match = Select-String -LiteralPath 'Cargo.toml' -Pattern '^version = "(.*)"$' | Select-Object -First 1
if (-not $match) { throw 'No version in Cargo.toml' }
$version = $match.Matches[0].Groups[1].Value

& cargo build --release -p wisp_study -p wisp_cli
if ($LASTEXITCODE -ne 0) { throw "cargo build exited $LASTEXITCODE" }

$targetDir = $env:CARGO_TARGET_DIR
if ([string]::IsNullOrWhiteSpace($targetDir)) { $targetDir = 'target' }
$release = Join-Path $targetDir 'release'

$name = "WispStudy-v$version-windows-x64"
$dist = Join-Path $root 'dist'
$folder = Join-Path $dist $name
$zip = Join-Path $dist "$name.zip"
$setup = Join-Path $dist "$name-setup.exe"

# Inno Setup's compiler: on the PATH, or where its own setup puts it.
function Find-InnoSetup {
    $command = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    foreach ($base in @(${env:ProgramFiles(x86)}, $env:ProgramFiles, "$env:LOCALAPPDATA\Programs")) {
        if ([string]::IsNullOrWhiteSpace($base)) { continue }
        $path = Join-Path $base 'Inno Setup 6\ISCC.exe'
        if (Test-Path -LiteralPath $path -PathType Leaf) { return $path }
    }
    return $null
}
# Asked before the build, which takes minutes.
if (-not $NoInstaller) {
    $iscc = Find-InnoSetup
    if (-not $iscc) { throw 'Inno Setup 6 (ISCC.exe) not found: winget install JRSoftware.InnoSetup, or pass -NoInstaller for the zip alone.' }
}

if (Test-Path -LiteralPath $setup) { Remove-Item -LiteralPath $setup -Force }
if (Test-Path -LiteralPath $folder) { Remove-Item -LiteralPath $folder -Recurse -Force }
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
New-Item -ItemType Directory -Force -Path $folder | Out-Null

# The app is a GUI-subsystem program; `wisp` beside it is the console one.
$app = Join-Path $folder 'WispStudy.exe'
$cli = Join-Path $folder 'wisp.exe'
Copy-Item -LiteralPath (Join-Path $release 'wisp_study.exe') -Destination $app
Copy-Item -LiteralPath (Join-Path $release 'wisp.exe') -Destination $cli
Copy-Item -LiteralPath 'LICENSE', 'THIRD_PARTY.md', 'README.md' -Destination $folder

function Invoke-Sign([string]$File) {
    & pwsh -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'sign-windows-esigner.ps1') -Path $File
    if ($LASTEXITCODE -ne 0) { throw "Signing $([IO.Path]::GetFileName($File)) failed (exit $LASTEXITCODE)" }
}

if ($Sign) {
    foreach ($file in @($app, $cli)) { Invoke-Sign $file }
} else {
    Write-Host 'Not signed: pass -Sign for a release.'
}

# The folder itself goes into the zip, so unpacking gives one folder.
Compress-Archive -LiteralPath $folder -DestinationPath $zip -CompressionLevel Optimal
Write-Host "folder: $folder"
Write-Host ("zip: $zip ({0:N1} MB)" -f ((Get-Item -LiteralPath $zip).Length / 1MB))

if ($NoInstaller) { return }
& $iscc /Qp "/DAppVersion=$version" "/DSource=$folder" "/DOutput=$dist" "/DBaseName=$name-setup" (Join-Path $PSScriptRoot 'wisp-study.iss')
if ($LASTEXITCODE -ne 0) { throw "ISCC exited $LASTEXITCODE" }
if (-not (Test-Path -LiteralPath $setup -PathType Leaf)) { throw "ISCC wrote no $setup" }
if ($Sign) { Invoke-Sign $setup }
Write-Host ("setup: $setup ({0:N1} MB)" -f ((Get-Item -LiteralPath $setup).Length / 1MB))
