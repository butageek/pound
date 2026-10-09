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
    [switch]$NoPath,

    # Relaunch Pound after installing (used by the in-app updater).
    [switch]$Relaunch
)

$ErrorActionPreference = 'Stop'

# Windows PowerShell 5.1 defaults may not include TLS 1.2 (GitHub requires it).
[Net.ServicePointManager]::SecurityProtocol = `
    [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

# Canonicalize TEMP to its long form: profiles with dots in the username
# (C:\Users\hendry.chou) can get an 8.3 short-form TEMP
# (C:\Users\HENDRY~1.CHO\AppData\Local\Temp), which trips Remove-Item's
# path handling in Windows PowerShell 5.1.
$TempRoot = (Resolve-Path $env:TEMP).Path

# Same for the default install dir: LOCALAPPDATA can be short-formed too,
# and we don't want 8.3 paths baked into the registry or shortcuts.
if ($InstallDir -eq (Join-Path $env:LOCALAPPDATA 'Pound')) {
    $longLocal = [Environment]::GetFolderPath('LocalApplicationData')
    if ($longLocal) { $InstallDir = Join-Path $longLocal 'Pound' }
}

function Write-Step([string]$msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

# Close any Pound windows running from the given exe path so the file can
# be replaced (Windows locks a running executable). Graceful close first,
# force-terminate after a short grace period. Used by upgrades/uninstalls.
function Stop-PoundProcesses {
    param([string]$ExePath)
    $procs = @()
    try {
        $procs = @(Get-Process -Name 'pound' -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $ExePath })
    }
    catch { $procs = @() }
    if ($procs.Count -eq 0) { return }

    Write-Host "  closing running Pound ($($procs.Count) window(s))" -ForegroundColor DarkGray
    foreach ($p in $procs) {
        try { $null = $p.CloseMainWindow() } catch { }
    }
    $left = $procs.Count
    $deadline = (Get-Date).AddSeconds(5)
    while ($left -gt 0 -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 250
        try {
            $left = @(Get-Process -Name 'pound' -ErrorAction SilentlyContinue |
                Where-Object { $_.Path -eq $ExePath }).Count
        }
        catch { $left = 0 }
    }
    if ($left -gt 0) {
        try {
            Get-Process -Name 'pound' -ErrorAction SilentlyContinue |
                Where-Object { $_.Path -eq $ExePath } | Stop-Process -Force -ErrorAction SilentlyContinue
        }
        catch { }
    }
    Start-Sleep -Milliseconds 300 # let the OS release the file handle
}

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
    $tmp = Join-Path $TempRoot ("pound-install-" + [guid]::NewGuid().ToString('N'))
    Expand-Archive -Path $ArchivePath -DestinationPath $tmp -Force
    $exe = (Get-ChildItem -Path $tmp -Recurse -Filter 'pound.exe' | Select-Object -First 1).FullName
    if (-not $exe) { throw 'pound.exe not found inside the downloaded archive.' }
    return $exe
}

# ---------------------------------------------------------------- binary ---
if ($DownloadUrl -ne '') {
    Write-Step "Downloading $DownloadUrl"
    $archive = Join-Path $TempRoot ("pound-download-" + [guid]::NewGuid().ToString('N'))
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
    $archive = Join-Path $TempRoot ("pound-release-" + [guid]::NewGuid().ToString('N') + '.zip')
    Invoke-WebRequest -Uri $releaseUrl -OutFile $archive -UseBasicParsing
    $BinaryPath = Expand-PoundArchive $archive
}

if (-not (Test-Path $BinaryPath)) {
    throw "pound.exe not found at '$BinaryPath'. Pass -BinaryPath, -DownloadUrl, or build with: cargo build --release"
}
$BinaryPath = (Resolve-Path $BinaryPath).Path
Write-Step "Using binary: $BinaryPath"

# --------------------------------------------------------------- install ---
$isUpgrade = Test-Path -LiteralPath (Join-Path $InstallDir 'pound.exe')
if ($isUpgrade) {
    Write-Step 'Upgrading existing installation'
}
else {
    Write-Step "Installing to $InstallDir"
}
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
$targetExe = Join-Path $InstallDir 'pound.exe'
Stop-PoundProcesses -ExePath $targetExe
Copy-Item -Path $BinaryPath -Destination $targetExe -Force
# Future-proofing: if a release ever ships helper DLLs next to pound.exe,
# bring them along (MSVC builds link everything statically today).
$binDir = Split-Path -Parent $BinaryPath
if ($binDir) {
    Get-ChildItem -Path $binDir -Filter '*.dll' -ErrorAction SilentlyContinue | ForEach-Object {
        Copy-Item -Path $_.FullName -Destination $InstallDir -Force
    }
}

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
if (-not $NoRegister) {    Write-Step 'Registering Pound with Windows (HKCU only, no admin needed)'

    # pound.exe is a GUI-subsystem binary (no console flash when opening
    # .md files). PowerShell's call operator does NOT wait for GUI-subsystem
    # executables and leaves $LASTEXITCODE unset, so launch it through
    # Start-Process, wait for the real exit code, and capture its output
    # for diagnostics.
    $outLog = Join-Path $TempRoot ("pound-register-out-" + [guid]::NewGuid().ToString('N') + '.txt')
    $errLog = Join-Path $TempRoot ("pound-register-err-" + [guid]::NewGuid().ToString('N') + '.txt')
    $registerArgs = @('register')
    if ($SetDefault) { $registerArgs += '--default' }
    $proc = Start-Process -FilePath $targetExe -ArgumentList $registerArgs `
        -Wait -PassThru -RedirectStandardOutput $outLog -RedirectStandardError $errLog
    if ($proc.ExitCode -ne 0) {
        $detail = ''
        if (Test-Path -LiteralPath $errLog) { $detail += (Get-Content -LiteralPath $errLog -Raw -ErrorAction SilentlyContinue) }
        if (Test-Path -LiteralPath $outLog) { $detail += (Get-Content -LiteralPath $outLog -Raw -ErrorAction SilentlyContinue) }
        if ($detail) { $detail = "`npound said:`n$detail" }
        throw "pound register failed with exit code $($proc.ExitCode).$detail"
    }
    # Cleanup is cosmetic: -ErrorAction alone cannot suppress terminating
    # PSArgumentExceptions in PS 5.1, so guard with try/catch too.
    try { Remove-Item -LiteralPath $outLog, $errLog -ErrorAction SilentlyContinue } catch { }
}

# Best-effort version confirmation (never fails the install).
try {
    $verOut = Join-Path $TempRoot ("pound-version-" + [guid]::NewGuid().ToString('N') + '.txt')
    $verErr = Join-Path $TempRoot ("pound-version-err-" + [guid]::NewGuid().ToString('N') + '.txt')
    $vp = Start-Process -FilePath $targetExe -ArgumentList @('--version') `
        -Wait -PassThru -RedirectStandardOutput $verOut -RedirectStandardError $verErr
    $versionText = ''
    if (Test-Path -LiteralPath $verOut) { $versionText = (Get-Content -LiteralPath $verOut -Raw -ErrorAction SilentlyContinue) }
    try { Remove-Item -LiteralPath $verOut, $verErr -ErrorAction SilentlyContinue } catch { }
    if ($vp.ExitCode -eq 0 -and $versionText) {
        Write-Host ("Installed: " + $versionText.Trim()) -ForegroundColor Green
    }
}
catch { }

Write-Host ''
if ($isUpgrade) {
    Write-Host 'Pound upgraded.' -ForegroundColor Green
}
else {
    Write-Host 'Pound installed.' -ForegroundColor Green
}
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

if ($Relaunch) {
    Write-Step 'Relaunching Pound'
    Start-Process -FilePath $targetExe
}
