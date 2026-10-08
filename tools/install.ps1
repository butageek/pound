# Pound installer (per-user, no admin rights required).
#
# Easiest install (downloads the latest release build from GitHub):
#   powershell -c "irm https://raw.githubusercontent.com/butageek/pound/main/tools/install.ps1 | iex"
#
# More control:
#   powershell -ExecutionPolicy Bypass -File tools\install.ps1 -SetDefault
#   powershell -ExecutionPolicy Bypass -File tools\install.ps1 -BinaryPath .\target\release\pound.exe
#   powershell -ExecutionPolicy Bypass -File tools\install.ps1 -DownloadUrl https://github.com/butageek/pound/releases/latest/download/pound-0.1.0-win64.zip
#
# Binary resolution order:
#   1. -BinaryPath / -DownloadUrl parameters
#   2. pound.exe next to this script or in target\release (local builds)
#   3. the latest pound-*-win64.zip asset of the GitHub releases of -Repo
#
# What it does:
#   1. Copies pound.exe to $env:LOCALAPPDATA\Pound
#   2. Adds a Start Menu shortcut and (optionally) the user PATH
#   3. Runs `pound.exe register [--default]` so Windows knows about the app
#      and its .md association (registry only, HKCU)
#Requires -Version 5.1
[CmdletBinding()]
param(
    # Path to a built pound.exe. Default: searched next to the script, in
    # target\release, then downloaded from GitHub releases.
    [string]$BinaryPath = '',

    # Where to install. Default: %LOCALAPPDATA%\Pound
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Pound'),

    # Optional direct URL to pound.exe or a .zip containing it.
    [string]$DownloadUrl = '',

    # GitHub repo (owner/name) to fetch the latest release from when no
    # local binary is found.
    [string]$Repo = 'butageek/pound',

    # Also make Pound the default handler for .md files.
    [switch]$SetDefault,

    # Skip the registry registration step entirely.
    [switch]$NoRegister,

    # Do not add the install dir to the user PATH.
    [switch]$NoPath
)

$ErrorActionPreference = 'Stop'

# Windows PowerShell 5.1 defaults may not include TLS 1.2 (GitHub requires it).
[Net.ServicePointManager]::SecurityProtocol = `
    [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

function Write-Step([string]$msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

function Get-GithubLatestAsset {
    param([string]$OwnerRepo)
    Write-Step "Looking up the latest release of $OwnerRepo"
    $headers = @{ 'User-Agent' = 'pound-installer' }
    if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "token $env:GITHUB_TOKEN" }
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$OwnerRepo/releases/latest" -Headers $headers
    $asset = $release.assets |
        Where-Object { $_.name -match '^pound-[\w.\-]+-win64\.zip$' } |
        Select-Object -First 1
    if (-not $asset) {
        throw "No pound-*-win64.zip asset found on the latest release of $OwnerRepo ($($release.tag_name))."
    }
    return $asset.browser_download_url
}

function Expand-PoundArchive {
    param([string]$ArchivePath)
    $tmp = Join-Path $env:TEMP ("pound-install-" + [guid]::NewGuid().ToString('N'))
    Expand-Archive -Path $ArchivePath -DestinationPath $tmp -Force
    $exe = (Get-ChildItem -Path $tmp -Recurse -Filter 'pound.exe' | Select-Object -First 1).FullName
    if (-not $exe) { throw 'pound.exe not found inside the downloaded archive.' }
    return $exe
}

# ---------------------------------------------------------------- binary ---
if ($DownloadUrl -ne '') {
    Write-Step "Downloading $DownloadUrl"
    $archive = Join-Path $env:TEMP ("pound-download-" + [guid]::NewGuid().ToString('N'))
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $archive -UseBasicParsing
    if ($DownloadUrl -like '*.zip') {
        $BinaryPath = Expand-PoundArchive $archive
    }
    else {
        $BinaryPath = $archive
    }
}

if ($BinaryPath -eq '' -and $PSScriptRoot) {
    $candidates = @(
        (Join-Path $PSScriptRoot 'pound.exe'),
        (Join-Path $PSScriptRoot '..\target\release\pound.exe'),
        (Join-Path $PSScriptRoot '..\..\target\release\pound.exe')
    )
    $found = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if ($found) { $BinaryPath = (Resolve-Path $found).Path }
}

if ($BinaryPath -eq '') {
    # No local build: fetch the latest release zip from GitHub.
    $releaseUrl = Get-GithubLatestAsset $Repo
    Write-Step "Downloading $releaseUrl"
    $archive = Join-Path $env:TEMP ("pound-release-" + [guid]::NewGuid().ToString('N') + '.zip')
    Invoke-WebRequest -Uri $releaseUrl -OutFile $archive -UseBasicParsing
    $BinaryPath = Expand-PoundArchive $archive
}

if (-not (Test-Path $BinaryPath)) {
    throw "pound.exe not found at '$BinaryPath'. Pass -BinaryPath, -DownloadUrl, or build with: cargo build --release"
}
$BinaryPath = (Resolve-Path $BinaryPath).Path
Write-Step "Using binary: $BinaryPath"

# --------------------------------------------------------------- install ---
Write-Step "Installing to $InstallDir"
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
$targetExe = Join-Path $InstallDir 'pound.exe'
Copy-Item -Path $BinaryPath -Destination $targetExe -Force

# Start Menu shortcut (puts Pound in the system app list for the Start menu).
Write-Step 'Creating Start Menu shortcut'
$shortcutPath = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Pound.lnk'
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $targetExe
$shortcut.WorkingDirectory = $InstallDir
$shortcut.IconLocation = "$targetExe,0"
$shortcut.Description = 'Pound — markdown reader'
$shortcut.Save()

# User PATH (lets you run `pound file.md` from anywhere).
if (-not $NoPath) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($null -eq $userPath) { $userPath = '' }
    if (($userPath -split ';') -notcontains $InstallDir) {
        Write-Step "Adding $InstallDir to the user PATH"
        $newPath = ($userPath.TrimEnd(';'), $InstallDir) -join ';'
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    }
}

# ------------------------------------------------------------ register ----
if (-not $NoRegister) {
    Write-Step 'Registering Pound with Windows (HKCU only, no admin needed)'
    $registerArgs = @('register')
    if ($SetDefault) { $registerArgs += '--default' }
    & $targetExe @registerArgs
    if ($LASTEXITCODE -ne 0) { throw "pound register failed with exit code $LASTEXITCODE." }
}

Write-Host ''
Write-Host 'Pound installed.' -ForegroundColor Green
if (-not $NoRegister -and -not $SetDefault) {
    Write-Host 'Next: right-click a .md file > "Open with" > choose Pound (tick "Always").'
    Write-Host '      Or re-run the installer with -SetDefault.'
}
if ($NoPath) {
    Write-Host "Run it from: $targetExe"
}
else {
    Write-Host 'Open a new terminal and try:  pound README.md'
}
