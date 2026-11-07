<#
Packages a release build of the app for Windows, named as the Mac's disk
image is:

  Imago-Emulator-VERSION-windows-x86_64.exe           an NSIS installer
                                                     (installer.nsi)
  Imago-Emulator-VERSION-windows-x86_64-portable.zip  the same files, to
                                                     run from any folder

The installer's name says neither "setup" nor "install": Windows treats a
program so named as an installer and loads compatibility shims into it,
which NSIS warns is unsafe (warning 9000).

Both hold ds-emulator.exe, License.txt, Acknowledgements.html and
Source\seed_tables.rs. Neither is signed: there is no code-signing
certificate yet (packaging/README.md says what users see).

  pwsh packaging/windows/package.ps1 -Exe target/release/ds-emulator.exe `
      -Notices NOTICES_DIR [-Out target/dist] [-Version 0.1.0]

-Exe is a release build (cargo build --release --locked -p emu-app, on
Windows, which gives it its icon and GPUI its shaders). -Notices is the
folder packaging/notices.sh wrote for x86_64-pc-windows-msvc. -Version
defaults to the workspace's, from Cargo.toml.

Needs NSIS 3 (makensis on PATH or in Program Files; choco install nsis).
#>
param(
    [Parameter(Mandatory)] [string]$Exe,
    [Parameter(Mandatory)] [string]$Notices,
    [string]$Out = 'target/dist',
    [string]$Version = ''
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path

if (-not $Version) {
    $Version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.*)"$' |
        Select-Object -First 1).Matches[0].Groups[1].Value
}
# Windows keeps four numbers; a pre-release counts as its release.
$numbers = ($Version -split '[-+]')[0]
$version4 = "$numbers.0"
$name = "Imago-Emulator-$Version-windows-x86_64"

$Exe = (Resolve-Path $Exe).Path
$Notices = (Resolve-Path $Notices).Path
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Out = (Resolve-Path $Out).Path
foreach ($file in 'License.txt', 'Acknowledgements.html', 'Source\seed_tables.rs') {
    if (-not (Test-Path (Join-Path $Notices $file))) {
        throw "no $file in $Notices (packaging/notices.sh makes it)"
    }
}

$makensis = (Get-Command makensis -ErrorAction SilentlyContinue).Source
if (-not $makensis) {
    $makensis = @("${env:ProgramFiles(x86)}\NSIS\makensis.exe", "$env:ProgramFiles\NSIS\makensis.exe") |
        Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $makensis) { throw 'NSIS is not installed (choco install nsis)' }

Write-Host "== Imago Emulator $Version for Windows x86-64"
$scratch = Join-Path ([IO.Path]::GetTempPath()) "imago-package-$PID"
Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue
$stage = Join-Path $scratch 'Imago Emulator'
New-Item -ItemType Directory -Force -Path (Join-Path $stage 'Source') | Out-Null
try {
    Copy-Item $Exe (Join-Path $stage 'ds-emulator.exe')
    # Windows line endings, for Notepad and the installer's license page.
    $license = [IO.File]::ReadAllText((Join-Path $Notices 'License.txt')) -replace "`r?`n", "`r`n"
    [IO.File]::WriteAllText((Join-Path $stage 'License.txt'), $license)
    Copy-Item (Join-Path $Notices 'Acknowledgements.html') $stage
    Copy-Item (Join-Path $Notices 'Source\seed_tables.rs') (Join-Path $stage 'Source')

    Write-Host '== Making the zip'
    $zip = Join-Path $Out "$name-portable.zip"
    Remove-Item -Force $zip -ErrorAction SilentlyContinue
    # One folder inside, so unpacking it leaves the files together.
    Compress-Archive -Path $stage -DestinationPath $zip

    Write-Host '== Making the installer'
    $setup = Join-Path $Out "$name.exe"
    & $makensis /V2 /INPUTCHARSET UTF8 "/DVERSION=$Version" "/DVERSION4=$version4" `
        "/DSTAGE=$stage" "/DICON=$(Join-Path $root 'assets\app-icon.ico')" "/DOUTFILE=$setup" `
        (Join-Path $PSScriptRoot 'installer.nsi')
    if ($LASTEXITCODE -ne 0) { throw "makensis failed ($LASTEXITCODE)" }

    Write-Host '== Done'
    foreach ($file in $setup, $zip) {
        $hash = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLower()
        $size = [math]::Round((Get-Item $file).Length / 1MB, 1)
        Write-Host "$hash  $(Split-Path -Leaf $file)  ($size MB)"
    }
} finally {
    Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue
}
