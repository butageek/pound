# Pound installer (per-user, no admin rights required).
#
# Easiest install or upgrade, in PowerShell 7 (downloads the latest
# release build from GitHub):
#   irm https://raw.githubusercontent.com/butageek/pound/main/tools/install.ps1 | iex
# Windows PowerShell 5.1 cannot iex a script with a param block (and
# -SetDefault needs the file form anyway) - there, download it and run it
# as a file:
#   powershell -c "irm https://raw.githubusercontent.com/butageek/pound/main/tools/install.ps1 -OutFile $env:TEMP\pound-install.ps1; powershell -ExecutionPolicy Bypass -File $env:TEMP\pound-install.ps1"
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
#   1. Asks any running Pound to close (Windows locks a running exe) and
#      stops if one stays open — it is most likely showing the
#      unsaved-changes prompt; -Force terminates it instead (edits lost).
#   2. Copies pound.exe to $env:LOCALAPPDATA\Pound
#   3. Adds a Start Menu shortcut and (optionally) the user PATH — the
#      PATH keeps its registry kind and unexpanded %VAR% entries
#   4. Runs `pound.exe register [--default]` so Windows knows about the app
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
    [switch]$Relaunch,

    # File to reopen on the relaunched Pound (used with -Relaunch).
    [string]$OpenPath = '',

    # Terminate a Pound that did not close gracefully within the grace
    # period. Off by default: a Pound still alive after CloseMainWindow is
    # usually showing the unsaved-changes prompt, and killing it would
    # silently discard the user's edits.
    [switch]$Force
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

# The user PATH, read raw and written back with its registry kind intact.
# [Environment]::GetEnvironmentVariable('Path','User') hands back the
# EXPANDED value and SetEnvironmentVariable writes REG_SZ, so that pair
# silently downgraded a REG_EXPAND_SZ PATH and froze every %VAR% entry in
# it (the classic setx pitfall). The registry API can do both right.
function Get-UserPath {
    # Always answers: a missing PATH comes back empty + ExpandString (how
    # Windows creates a new one), so callers need no $null handling.
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
    if (-not $key -or $key.GetValueNames() -notcontains 'Path') {
        if ($key) { $key.Close() }
        return [pscustomobject]@{ Value = ''; Kind = 'ExpandString' }
    }
    try {
        $raw = $key.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($raw -is [array]) { $raw = $raw -join ';' } # a REG_MULTI_SZ PATH
        $kind = 'String'
        if ($key.GetValueKind('Path') -eq [Microsoft.Win32.RegistryValueKind]::ExpandString) { $kind = 'ExpandString' }
        return [pscustomobject]@{ Value = [string]$raw; Kind = $kind }
    }
    finally { $key.Close() }
}
function Set-UserPath([string]$value, [string]$kind) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if (-not $key) { throw 'cannot open HKCU\Environment for writing' }
    try {
        $vk = [Microsoft.Win32.RegistryValueKind]::String
        if ($kind -eq 'ExpandString') { $vk = [Microsoft.Win32.RegistryValueKind]::ExpandString }
        $key.SetValue('Path', $value, $vk)
    }
    finally { $key.Close() }
    # Broadcast WM_SETTINGCHANGE the way SetEnvironmentVariable did, so
    # new processes (anything launched from Explorer) see the new PATH
    # without a logoff. HWND_BROADCAST=0xFFFF, WM_SETTINGCHANGE=0x1A,
    # SMTO_ABORTIFHUNG=2.
    if (-not ('Pound.NativeMethods' -as [type])) {
        Add-Type -Namespace Pound -Name NativeMethods -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
'@
    }
    $result = [UIntPtr]::Zero
    [void][Pound.NativeMethods]::SendMessageTimeout([IntPtr]0xFFFF, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
}

# Download and extraction artifacts in TEMP; removed once the exe is in
# place (every install used to leave a zip and an extracted copy behind).
$script:downloadFile = ''
$script:extractDir = ''
function Remove-TempArtifacts {
    foreach ($path in @($script:downloadFile, $script:extractDir)) {
        if ($path -and (Test-Path -LiteralPath $path)) {
            try { Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction SilentlyContinue } catch { }
        }
    }
}

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
        if ($Force) {
            Write-Host '  still running: terminating (unsaved edits are lost)' -ForegroundColor Yellow
            try {
                Get-Process -Name 'pound' -ErrorAction SilentlyContinue |
                    Where-Object { $_.Path -eq $ExePath } | Stop-Process -Force -ErrorAction SilentlyContinue
            }
            catch { }
        }
        else {
            # Graceful close declined or waiting on the unsaved-changes
            # prompt: killing it would silently discard the user's edits.
            # (The in-app updater never needs this: it saves or discards
            # before spawning the installer.)
            throw "Pound is still running - it may be showing the unsaved-changes prompt. Save or discard your edits, close it, and run this again (or pass -Force to discard them)."
        }
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
    $script:extractDir = $tmp
    Expand-Archive -Path $ArchivePath -DestinationPath $tmp -Force
    $exe = (Get-ChildItem -Path $tmp -Recurse -Filter 'pound.exe' | Select-Object -First 1).FullName
    if (-not $exe) { throw 'pound.exe not found inside the downloaded archive.' }
    return $exe
}

# ---------------------------------------------------------------- binary ---
if ($DownloadUrl -ne '') {
    Write-Step "Downloading $DownloadUrl"
    $archive = Join-Path $TempRoot ("pound-download-" + [guid]::NewGuid().ToString('N'))
    $script:downloadFile = $archive
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
    $script:downloadFile = $archive
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
Remove-TempArtifacts

# Start Menu shortcut (puts Pound in the system app list for the Start menu).
Write-Step 'Creating Start Menu shortcut'
$shortcutPath = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Pound.lnk'
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $targetExe
$shortcut.WorkingDirectory = $InstallDir
$shortcut.IconLocation = "$targetExe,0"
$shortcut.Description = 'Pound — file viewer'
$shortcut.Save()

# User PATH (lets you run `pound file.md` from anywhere). The value keeps
# its registry kind and its unexpanded %VAR% entries (see Get-UserPath).
if (-not $NoPath) {
    $userPath = Get-UserPath
    if (($userPath.Value -split ';') -notcontains $InstallDir) {
        Write-Step "Adding $InstallDir to the user PATH"
        $newPath = ($userPath.Value.TrimEnd(';'), $InstallDir) -join ';'
        Set-UserPath $newPath $userPath.Kind
    }
}

# ------------------------------------------------------------ register ----
if (-not $NoRegister) {
    Write-Step 'Registering Pound with Windows (HKCU only, no admin needed)'

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
    Write-Host 'Next: right-click a .md, .csv or text file > "Open with" > choose Pound (tick "Always").'
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
    # ProcessStartInfo.Arguments is the RAW command line. Start-Process
    # re-quotes -ArgumentList elements and eats pre-embedded quotes, so
    # the path arrived with literal quote characters and pound
    # relaunched without its file. The .NET API passes it through.
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $targetExe
    if ($OpenPath -ne '') { $psi.Arguments = '"' + $OpenPath + '"' }
    [void][System.Diagnostics.Process]::Start($psi)
}
