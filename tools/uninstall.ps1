# Pound uninstaller. Removes registry entries, shortcut, install dir, and
# the PATH entry created by install.ps1.
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Pound')
)
$ErrorActionPreference = 'Stop'

function Write-Step([string]$msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

$targetExe = Join-Path $InstallDir 'pound.exe'

# -------------------------------------------------- unregister (registry) --
if (Test-Path $targetExe) {
    Write-Step 'Removing registry entries'
    # GUI-subsystem exe: Start-Process -Wait, never the call operator
    # (PowerShell does not wait for GUI-subsystem binaries).
    $outLog = Join-Path $env:TEMP ("pound-unregister-out-" + [guid]::NewGuid().ToString('N') + '.txt')
    $errLog = Join-Path $env:TEMP ("pound-unregister-err-" + [guid]::NewGuid().ToString('N') + '.txt')
    $proc = Start-Process -FilePath $targetExe -ArgumentList @('unregister') `
        -Wait -PassThru -RedirectStandardOutput $outLog -RedirectStandardError $errLog
    if ($proc.ExitCode -ne 0) {
        Write-Warning "pound unregister exited with code $($proc.ExitCode); continuing."
    }
    Remove-Item $outLog, $errLog -ErrorAction SilentlyContinue
}
else {
    Write-Warning 'pound.exe not found; skipping registry cleanup (keys stay in HKCU).'
}

# ------------------------------------------------------------- shortcut ----
Write-Step 'Removing Start Menu shortcut'
$shortcutPath = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Pound.lnk'
if (Test-Path $shortcutPath) { Remove-Item $shortcutPath -Force }

# ------------------------------------------------------------ PATH entry ---
Write-Step 'Removing PATH entry'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($null -ne $userPath -and ($userPath -split ';') -contains $InstallDir) {
    $newPath = ($userPath -split ';' | Where-Object { $_ -ne $InstallDir -and $_ -ne '' }) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
}

# ---------------------------------------------------------- install dir ----
Write-Step "Removing $InstallDir"
if (Test-Path $InstallDir) { Remove-Item -Recurse -Force $InstallDir }

Write-Host 'Pound uninstalled.' -ForegroundColor Green
Write-Host 'Note: if Pound was set as the default .md app, Windows may keep the'
Write-Host 'choice in Explorer''s protected UserChoice key; pick another default via'
Write-Host 'Settings > Apps > Default apps if double-clicking .md complains.'
