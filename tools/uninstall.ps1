# Pound uninstaller. Removes registry entries, shortcut, Pound's own files
# in the install dir (the folder itself only if nothing else lives there)
# and the PATH entry created by install.ps1 (its registry kind intact).
#
# In PowerShell 7:
#   irm https://raw.githubusercontent.com/butageek/pound/main/tools/uninstall.ps1 | iex
# Windows PowerShell 5.1 cannot iex a script with a param block - there,
# download it and run it as a file:
#   powershell -c "irm https://raw.githubusercontent.com/butageek/pound/main/tools/uninstall.ps1 -OutFile $env:TEMP\pound-uninstall.ps1; powershell -ExecutionPolicy Bypass -File $env:TEMP\pound-uninstall.ps1"
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Pound'),

    # Terminate a Pound that did not close gracefully within the grace
    # period. Off by default: a Pound still alive after CloseMainWindow is
    # usually showing the unsaved-changes prompt, and killing it would
    # silently discard the user's edits.
    [switch]$Force
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
            throw "Pound is still running - it may be showing the unsaved-changes prompt. Save or discard your edits, close it, and run this again (or pass -Force to discard them)."
        }
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
# The value keeps its registry kind and its unexpanded %VAR% entries (see
# Get-UserPath).
Write-Step 'Removing PATH entry'
$userPath = Get-UserPath
if (($userPath.Value -split ';') -contains $InstallDir) {
    $newPath = ($userPath.Value -split ';' | Where-Object { $_ -ne $InstallDir -and $_ -ne '' }) -join ';'
    Set-UserPath $newPath $userPath.Kind
}

# ---------------------------------------------------------- install dir ----
Write-Step "Removing $InstallDir"
if (Test-Path -LiteralPath $InstallDir) {
    # Delete only what the installer put there — pound.exe and any helper
    # DLLs it copied along — never -Recurse into the folder itself: a user
    # who installed with -InstallDir C:\Tools must not lose that folder.
    $installed = @('pound.exe') + @(Get-ChildItem -LiteralPath $InstallDir -Filter '*.dll' -File -ErrorAction SilentlyContinue | ForEach-Object { $_.Name })
    foreach ($name in $installed) {
        $file = Join-Path $InstallDir $name
        if (Test-Path -LiteralPath $file) {
            try { Remove-Item -LiteralPath $file -Force } catch { Write-Warning "could not remove ${file}: $_" }
        }
    }
    # The folder itself goes only when nothing else is left in it.
    $left = @(Get-ChildItem -LiteralPath $InstallDir -Force -ErrorAction SilentlyContinue)
    if ($left.Count -eq 0) {
        try { Remove-Item -LiteralPath $InstallDir -Force } catch { Write-Warning "could not remove $InstallDir (delete it manually): $_" }
    }
    else {
        Write-Host "  kept $InstallDir ($($left.Count) other item(s) in it)" -ForegroundColor DarkGray
    }
}

Write-Host 'Pound uninstalled.' -ForegroundColor Green
Write-Host 'Note: if Pound was set as the default .md app, Windows may keep the'
Write-Host 'choice in Explorer''s protected UserChoice key; pick another default via'
Write-Host 'Settings > Apps > Default apps if double-clicking .md complains.'
