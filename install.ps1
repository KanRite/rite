# KanRite installer for Windows (ticket 282): install.sh's counterpart.
#
#   irm https://raw.githubusercontent.com/kanrite/rite/v5/install.ps1 | iex
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/kanrite/rite/v5/install.ps1))) -Version v5.0.0
#   ... -Uninstall [-All] [-Yes]          (or Settings > Apps > KanRite > Uninstall)
#
# Downloads the release zip for this machine (x64 or Arm64) and installs
# rite.exe, the command line interface, and riteui.exe, the desktop board, for
# this user only, with no administrator rights:
#
# - into %LOCALAPPDATA%\Programs\KanRite\bin (-Dir to choose another folder),
#   which is added to your PATH;
# - amd.exe and amdui.exe, copies under the 4.x names that keep working
#   through 5.x;
# - a Start menu shortcut, KanRite, for riteui;
# - an entry in Settings > Apps that runs the uninstaller;
# - rite-uninstall.ps1 beside the programs. It records that this is a release
#   install, which is what lets `rite update` and riteui's update bar replace
#   it with newer releases.
#
# Running it again updates an install in place. Building from source on
# Windows is `cargo xtask install` in a clone (CONTRIBUTING.md).
#
# Windows PowerShell 5.1 runs this, so nothing newer is used.

param(
    [string]$Dir = "",
    [string]$Version = "latest",
    # A release zip on disk instead of a download (scripts/test-install.ps1).
    [string]$Zip = "",
    [switch]$Uninstall,
    [switch]$All,
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is many times slower with it

$Repo = "kanrite/rite"
$AppName = "KanRite"
$UninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$AppName"

function Fail([string]$message) {
    Write-Host "install: $message" -ForegroundColor Red
    # `exit` would close the window of someone who ran `irm | iex`.
    throw "install: $message"
}

if ($Dir -eq "") {
    $Dir = Join-Path $env:LOCALAPPDATA "Programs\$AppName\bin"
}

# The uninstaller for an install into BIN. One script, written beside rite.exe
# as rite-uninstall.ps1, and also what `install.ps1 -Uninstall` runs. Its
# second line is the marker `rite update` looks for (update_apply::MARKER).
function Uninstaller([string]$bin, [string]$method) {
    $quoted = $bin.Replace("'", "''")
    $head = @"
# KanRite's uninstaller, written by install.ps1 (ticket 282).
# kanrite install: $method
#
#   rite-uninstall.ps1 [-All] [-Yes]
#
# Removes rite.exe, riteui.exe, this script, the Start menu shortcut, the
# Settings > Apps entry and the folder on your PATH. Then asks whether to
# remove your KanRite settings and data too (-All: yes without asking;
# -Yes: never asks, so with no -All they stay). Your projects (the tickets,
# documents and ideas in your git repositories) are never touched.
param([switch]`$All, [switch]`$Yes)
`$Bin = '$quoted'
`$AppName = '$AppName'
"@
    $body = @'
$ErrorActionPreference = 'Continue'
$removed = @()
$running = @()

# amd.exe and amdui.exe are the 4.x names, copies kept through 5.x (ticket 309).
foreach ($name in 'rite', 'riteui', 'amd', 'amdui') {
    $exe = Join-Path $Bin "$name.exe"
    if (Test-Path -LiteralPath $exe) {
        Remove-Item -LiteralPath $exe -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $exe) { $running += "$name.exe" } else { $removed += $exe }
    }
    # Kept by `rite update` for rollback, and by the installer while replacing.
    foreach ($extra in "$name.exe.previous", "$name.exe.old") {
        $path = Join-Path $Bin $extra
        if (Test-Path -LiteralPath $path) {
            Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
        }
    }
}
if ($running.Count -gt 0) {
    Write-Host "Couldn't remove $($running -join ' and '): close riteui (and any rite running), then uninstall again." -ForegroundColor Yellow
    return
}

$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) "$AppName.lnk"
if (Test-Path -LiteralPath $shortcut) { Remove-Item -LiteralPath $shortcut -Force; $removed += $shortcut }

$key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$AppName"
if (Test-Path $key) { Remove-Item $key -Recurse -Force; $removed += 'the Settings > Apps entry' }

# The PATH entry, read and written unexpanded so other entries keep their %VARS%.
$envKey = Get-Item 'HKCU:\Environment'
$path = $envKey.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
$kept = @($path -split ';' | Where-Object { $_ -ne '' -and $_.TrimEnd('\') -ne $Bin.TrimEnd('\') })
if ($kept.Count -ne @($path -split ';' | Where-Object { $_ -ne '' }).Count) {
    Set-ItemProperty 'HKCU:\Environment' -Name Path -Value ($kept -join ';') -Type ExpandString
    # Tells Explorer, so new terminals see the change.
    [Environment]::SetEnvironmentVariable('AMD_INSTALL_REFRESH', 'x', 'User')
    [Environment]::SetEnvironmentVariable('AMD_INSTALL_REFRESH', $null, 'User')
    $removed += "$Bin from your PATH"
}

if ($removed.Count -gt 0) {
    Write-Host 'Removed:'
    $removed | ForEach-Object { Write-Host "  $_" }
} else {
    Write-Host "KanRite's programs were not in $Bin."
}

# Your own settings and data: kept unless you say. On Windows rite keeps them
# in %APPDATA% and %LOCALAPPDATA% (ticket 278); 5.0.0 used $HOME or your user
# folder, as Unix does, and an XDG_* folder wins when set. All are looked for.
$homeDir = if ($env:HOME) { $env:HOME } else { $env:USERPROFILE }
# Called kanrite since 5.0, agile-md before (ticket 310).
function Place([string]$xdg, [string]$windows, [string]$fallback, [string]$name) {
    if ($xdg -and [IO.Path]::IsPathRooted($xdg)) { Join-Path $xdg $name }
    elseif ($windows -and [IO.Path]::IsPathRooted($windows)) { Join-Path $windows $name }
    else { Join-Path $homeDir "$fallback\$name" }
}
$present = @(foreach ($name in 'kanrite', 'agile-md') {
    (Place $env:XDG_CONFIG_HOME $env:APPDATA '.config' $name)
    (Place $env:XDG_STATE_HOME $env:LOCALAPPDATA '.local\state' $name)
    (Place $env:XDG_DATA_HOME $env:LOCALAPPDATA '.local\share' $name)
    # 5.0.0's folders, until rite has moved them.
    (Join-Path $homeDir ".config\$name")
    (Join-Path $homeDir ".local\state\$name")
    (Join-Path $homeDir ".local\share\$name")
}) | Select-Object -Unique | Where-Object { Test-Path -LiteralPath $_ }
if ($present.Count -gt 0) {
    $removeData = [bool]$All
    if (-not $All -and -not $Yes) {
        Write-Host ''
        Write-Host 'Your KanRite settings and data are still here:'
        $present | ForEach-Object { Write-Host "  $_" }
        $recovered = $present | ForEach-Object { Join-Path $_ 'recovered' } | Where-Object { Test-Path -LiteralPath $_ }
        foreach ($r in $recovered) {
            Write-Host "  ($r holds tickets saved after a push conflict: look before removing.)"
        }
        $answer = Read-Host 'Remove them too? [y/N]'
        $removeData = $answer -match '^(y|yes)$'
    }
    if ($removeData) {
        foreach ($d in $present) { Remove-Item -LiteralPath $d -Recurse -Force; Write-Host "Removed $d" }
    } else {
        Write-Host 'Kept your settings and data (rite-uninstall.ps1 -All removes them):'
        $present | ForEach-Object { Write-Host "  $_" }
    }
}

Remove-Item -LiteralPath (Join-Path $Bin 'rite-uninstall.ps1') -Force -ErrorAction SilentlyContinue
# The folders the installer made, when nothing else is in them.
foreach ($folder in $Bin, (Split-Path $Bin)) {
    if ((Split-Path $folder -Leaf) -in 'bin', $AppName -and (Test-Path -LiteralPath $folder) -and
        -not (Get-ChildItem -LiteralPath $folder -Force | Select-Object -First 1)) {
        Remove-Item -LiteralPath $folder -Force
    }
}
Write-Host 'KanRite is uninstalled. Your projects, in your git repositories, were not touched.'
'@
    return $head + "`r`n" + $body
}

if ($Uninstall) {
    $script = Join-Path $Dir 'rite-uninstall.ps1'
    if (-not (Test-Path -LiteralPath $script)) {
        # No uninstaller there (an install from before this one, say): a
        # fresh copy of it does the same.
        $script = Join-Path ([IO.Path]::GetTempPath()) "rite-uninstall-$PID.ps1"
        Set-Content -LiteralPath $script -Value (Uninstaller $Dir 'unknown') -Encoding UTF8
    }
    & $script -All:$All -Yes:$Yes
    return
}

# Says plainly, before anything is downloaded or changed, when this machine
# isn't one the release covers, and why.
$arch = $null
try {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
} catch {
    # Older .NET: the processor, even from x64 PowerShell emulated on Arm.
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
}
switch -Regex ($arch) {
    '^(X64|AMD64)$' { $target = 'x86_64-pc-windows-msvc' }
    '^(Arm64|ARM64)$' { $target = 'aarch64-pc-windows-msvc' }
    default { Fail "KanRite's downloads are for x64 and Arm64 processors; this one is $arch." }
}

$git = Get-Command git -ErrorAction SilentlyContinue
if (-not $git) {
    Fail "KanRite needs git 2.29 or later, and git isn't installed. Install Git for Windows (https://git-scm.com/download/win, or: winget install Git.Git), then run this again."
}
if ((& git --version) -match '(\d+)\.(\d+)') {
    if ([int]$Matches[1] -lt 2 -or ([int]$Matches[1] -eq 2 -and [int]$Matches[2] -lt 29)) {
        Fail "KanRite needs git 2.29 or later; this machine has $(& git --version). Update git, then run this again."
    }
}

# Windows PowerShell 5.1 may not offer TLS 1.2 by default; GitHub needs it.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

if ($Version -eq 'latest') {
    $url = "https://github.com/$Repo/releases/latest/download/amd-$target.zip"
} else {
    $url = "https://github.com/$Repo/releases/download/$Version/amd-$target.zip"
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) "amd-install-$PID"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
    if ($Zip -ne "") {
        $url = $Zip
        Copy-Item -LiteralPath $Zip -Destination (Join-Path $tmp 'rite.zip')
    } else {
        Write-Host "Downloading $url"
        try {
            Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp 'rite.zip') -UseBasicParsing
        } catch {
            Fail "couldn't download KanRite $Version for $target from https://github.com/$Repo/releases: check your connection, or that the version exists. ($($_.Exception.Message))"
        }
    }
    Expand-Archive -LiteralPath (Join-Path $tmp 'rite.zip') -DestinationPath $tmp -Force
    if (-not (Test-Path -LiteralPath (Join-Path $tmp 'rite.exe'))) {
        Fail "the download has no rite.exe in it: $url"
    }

    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    $restart = @()
    # Each program, and a copy under its 4.x name, which keeps working
    # through 5.x and says once a day that it's now rite (ticket 309).
    # Windows has no symlinks without developer mode, so it's a copy.
    $installs = @()
    foreach ($pair in @(@('rite', 'amd'), @('riteui', 'amdui'))) {
        $new = Join-Path $tmp "$($pair[0]).exe"
        if (-not (Test-Path -LiteralPath $new)) { continue }
        # Downloaded files are marked as from the internet; these are trusted now.
        Unblock-File -LiteralPath $new
        Copy-Item -LiteralPath $new -Destination (Join-Path $tmp "$($pair[1]).exe")
        $installs += $pair[0], $pair[1]
    }
    foreach ($name in $installs) {
        $new = Join-Path $tmp "$name.exe"
        $dest = Join-Path $Dir "$name.exe"
        $old = "$dest.old"
        # Windows won't overwrite a running .exe but will rename it: the old
        # one goes aside, keeps running until it exits, and the next launch
        # gets the new one. A leftover .old from last time goes first.
        Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $dest) {
            if (Get-Process -Name $name -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $dest }) {
                $restart += $name
            }
            Move-Item -LiteralPath $dest -Destination $old -Force
        }
        Move-Item -LiteralPath $new -Destination $dest -Force
        Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
    }
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$rite = Join-Path $Dir 'rite.exe'
$riteui = Join-Path $Dir 'riteui.exe'
$versionOut = & $rite --version 2>&1
if ($LASTEXITCODE -ne 0) {
    Fail "rite.exe was copied to $Dir but does not run on this machine: $versionOut"
}
Write-Host "Installed rite -> $rite"
Write-Host $versionOut

if (Test-Path -LiteralPath $riteui) {
    Write-Host "Installed riteui -> $riteui   (the desktop board)"
    $shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) "$AppName.lnk"
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($shortcut)
    $link.TargetPath = $riteui
    $link.WorkingDirectory = [Environment]::GetFolderPath('UserProfile')
    $link.IconLocation = "$riteui,0"
    $link.Description = 'KanRite: the desktop board'
    $link.Save()
    Write-Host "Added $AppName to the Start menu"
}

$uninstallScript = Join-Path $Dir 'rite-uninstall.ps1'
Set-Content -LiteralPath $uninstallScript -Value (Uninstaller $Dir 'release') -Encoding UTF8

# Settings > Apps. No DisplayVersion: `rite update` changes the version later,
# and a stale one there would be wrong.
New-Item -Path $UninstallKey -Force | Out-Null
$uninstallCommand = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$uninstallScript`""
$sizeKb = [int]((Get-ChildItem -LiteralPath $Dir -File | Measure-Object Length -Sum).Sum / 1KB)
$entry = @{
    DisplayName = $AppName
    Publisher = $AppName
    InstallLocation = $Dir
    UninstallString = $uninstallCommand
    QuietUninstallString = "$uninstallCommand -Yes"
    URLInfoAbout = "https://github.com/$Repo"
}
if (Test-Path -LiteralPath $riteui) { $entry.DisplayIcon = "$riteui,0" }
foreach ($name in $entry.Keys) {
    New-ItemProperty -Path $UninstallKey -Name $name -Value $entry[$name] -PropertyType String -Force | Out-Null
}
foreach ($name in 'NoModify', 'NoRepair') {
    New-ItemProperty -Path $UninstallKey -Name $name -Value 1 -PropertyType DWord -Force | Out-Null
}
New-ItemProperty -Path $UninstallKey -Name EstimatedSize -Value $sizeKb -PropertyType DWord -Force | Out-Null
Write-Host "To uninstall later: Settings > Apps > $AppName, or $uninstallScript"

# The user's PATH, read and written unexpanded so other entries keep their %VARS%.
$envKey = Get-Item 'HKCU:\Environment'
$userPath = $envKey.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
$entries = @($userPath -split ';' | Where-Object { $_ -ne '' })
if (-not ($entries | Where-Object { $_.TrimEnd('\') -eq $Dir.TrimEnd('\') })) {
    Set-ItemProperty 'HKCU:\Environment' -Name Path -Value (($entries + $Dir) -join ';') -Type ExpandString
    # Tells Explorer, so new terminals see the change.
    [Environment]::SetEnvironmentVariable('AMD_INSTALL_REFRESH', 'x', 'User')
    [Environment]::SetEnvironmentVariable('AMD_INSTALL_REFRESH', $null, 'User')
    Write-Host "Added $Dir to your PATH: open a new terminal to use rite."
}
if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -eq $Dir.TrimEnd('\') })) {
    $env:Path = "$env:Path;$Dir"
}

Write-Host 'Then start a project, from the folder you keep projects in:'
Write-Host '  rite init my-project      (or rite join my-project for a team''s)'
if (Test-Path -LiteralPath $riteui) {
    Write-Host "For the desktop board: $AppName in the Start menu, or riteui (or rite gui)"
}
foreach ($name in $restart) {
    Write-Host "Note: $name was running while it was replaced; restart it to use the new version."
}
