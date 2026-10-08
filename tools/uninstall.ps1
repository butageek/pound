# Pound uninstaller. Removes registry entries, shortcut, install dir, and
# the PATH entry created by install.ps1.
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Pound')
)
$ErrorActionPreference = 'Stop'

# Canonicalize to long paths: profiles with dots in the username get 8.3
# short-form env dirs (C:\Users\HENDRY~1.CHO\...), which trip Remove-Item
# in Windows PowerShell 5.1.
$TempRoot = (Resolve-Path $env:TEMP).Path
if (-not (Test-Path -LiteralPath $InstallDir)) {
    # Default may have been given in short form; resolve via LOCALAPPDATA.
    $long = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Pound'
    if (Test-Path -LiteralPath $long) { $InstallDir = $long }
}

function Write-Step([string]$msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

# Close any Pound windows running from the given exe path so the install
# folder can be removed (Windows locks a running executable).
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

$targetExe = Join-Path $InstallDir 'pound.exe'
Stop-PoundProcesses -ExePath $targetExe

# -------------------------------------------------- unregister (registry) --
if (Test-Path $targetExe) {
    Write-Step 'Removing registry entries'
    # GUI-subsystem exe: Start-Process -Wait, never the call operator
    # (PowerShell does not wait for GUI-subsystem binaries).
    $outLog = Join-Path $TempRoot ("pound-unregister-out-" + [guid]::NewGuid().ToString('N') + '.txt')
    $errLog = Join-Path $TempRoot ("pound-unregister-err-" + [guid]::NewGuid().ToString('N') + '.txt')
    $proc = Start-Process -FilePath $targetExe -ArgumentList @('unregister') `
        -Wait -PassThru -RedirectStandardOutput $outLog -RedirectStandardError $errLog
    if ($proc.ExitCode -ne 0) {
        Write-Warning "pound unregister exited with code $($proc.ExitCode); continuing."
    }
    try { Remove-Item -LiteralPath $outLog, $errLog -ErrorAction SilentlyContinue } catch { }
}
else {
    Write-Warning 'pound.exe not found; skipping registry cleanup (keys stay in HKCU).'
}

# ------------------------------------------------------------- shortcut ----
Write-Step 'Removing Start Menu shortcut'
$shortcutPath = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'Microsoft\Windows\Start Menu\Programs\Pound.lnk'
if (Test-Path -LiteralPath $shortcutPath) {
    try { Remove-Item -LiteralPath $shortcutPath -Force } catch { Write-Warning "could not remove the Start Menu shortcut: $_" }
}

# ------------------------------------------------------------ PATH entry ---
Write-Step 'Removing PATH entry'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($null -ne $userPath -and ($userPath -split ';') -contains $InstallDir) {
    $newPath = ($userPath -split ';' | Where-Object { $_ -ne $InstallDir -and $_ -ne '' }) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
}

# ---------------------------------------------------------- install dir ----
Write-Step "Removing $InstallDir"
if (Test-Path -LiteralPath $InstallDir) {
    try { Remove-Item -LiteralPath $InstallDir -Recurse -Force } catch { Write-Warning "could not remove $InstallDir (delete it manually): $_" }
}

Write-Host 'Pound uninstalled.' -ForegroundColor Green
Write-Host 'Note: if Pound was set as the default .md app, Windows may keep the'
Write-Host 'choice in Explorer''s protected UserChoice key; pick another default via'
Write-Host 'Settings > Apps > Default apps if double-clicking .md complains.'
