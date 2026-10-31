# Installs, updates, rolls back and uninstalls a real Windows install
# (ticket 282), the way a user's machine would see it. For CI's Windows job.
#
#   scripts/test-install.ps1 [-Bin target\debug]
#
# install.ps1 installs a zip of the rite.exe and riteui.exe in -Bin. Then the
# installed rite.exe updates itself to a pretend 9.9.9 (two tiny programs
# that only print their version, built with rustc), which replaces rite.exe
# while it is the program running, rolls back, and the uninstaller removes
# it all. It writes to this user's PATH, Start menu and Settings > Apps, so
# it is for a throwaway CI runner, not your own machine.

param([string]$Bin = "target\debug")

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$work = Join-Path ([IO.Path]::GetTempPath()) "amd-test-install-$PID"
$dir = Join-Path $work 'KanRite\bin'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$failures = 0

function Check([string]$name, [scriptblock]$test) {
    # Windows PowerShell 5.1 turns a program's stderr, redirected, into
    # errors, which 'Stop' would throw on.
    $ErrorActionPreference = 'Continue'
    $ok = $false
    try { $ok = [bool](& $test) } catch { Write-Host "    $($_.Exception.Message)" }
    if ($ok) { Write-Host "  ok    $name" } else { Write-Host "  FAIL  $name" -ForegroundColor Red; $script:failures++ }
}

function Zip([string]$from, [string]$to) {
    Compress-Archive -Path (Join-Path $from 'rite.exe'), (Join-Path $from 'riteui.exe') -DestinationPath $to -Force
}

Write-Host 'install.ps1:'
$built = Join-Path $root $Bin
$zip = Join-Path $work 'rite.zip'
Zip $built $zip
& (Join-Path $root 'install.ps1') -Dir $dir -Zip $zip
$rite = Join-Path $dir 'rite.exe'
$uninstaller = Join-Path $dir 'rite-uninstall.ps1'
$key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\KanRite'
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'KanRite.lnk'
function UserPath { (Get-Item 'HKCU:\Environment').GetValue('Path', '', 'DoNotExpandEnvironmentNames') -split ';' }

Check 'rite.exe and riteui.exe are installed and run' {
    (& $rite --version) -match '^rite ' -and (& (Join-Path $dir 'riteui.exe') --version) -match '^riteui '
}
Check 'amd.exe, the 4.x name, runs rite and says it is now rite' {
    $out = & (Join-Path $dir 'amd.exe') --version 2>&1 | Out-String
    $out -match 'amd is now rite' -and $out -match 'rite '
}
Check 'the uninstaller marks it a release install' {
    (Get-Content -LiteralPath $uninstaller) -contains '# kanrite install: release'
}
Check 'the folder is on the user PATH' { (UserPath) -contains $dir }
Check 'there is a Start menu shortcut to riteui' {
    (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut).TargetPath -eq (Join-Path $dir 'riteui.exe')
}
Check 'Settings > Apps lists it, uninstalling with rite-uninstall.ps1' {
    (Get-ItemProperty $key).UninstallString -like "*$uninstaller*"
}

Write-Host 'rite update:'
# A pretend 9.9.9, zipped as the release workflow does, and the release list
# naming its checksum.
$fake = Join-Path $work 'fake'
New-Item -ItemType Directory -Force -Path $fake | Out-Null
foreach ($name in 'rite', 'riteui') {
    Set-Content -LiteralPath (Join-Path $fake "$name.rs") -Value "fn main() { println!(`"$name 9.9.9`"); }"
    & rustc -O -o (Join-Path $fake "$name.exe") (Join-Path $fake "$name.rs")
    if ($LASTEXITCODE -ne 0) { throw "rustc couldn't build the pretend $name" }
}
$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$target = if ($arch -eq 'Arm64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
$release = Join-Path $work "amd-$target.zip"
Zip $fake $release
$sha = (Get-FileHash -LiteralPath $release -Algorithm SHA256).Hash.ToLower()
$url = 'file://' + ($release -replace '\\', '/')
$list = Join-Path $work 'releases.json'
Set-Content -LiteralPath $list -Value ('[{"tag_name":"v9.9.9","html_url":"https://example.test/9.9.9","draft":false,"prerelease":false,"assets":[{"name":"amd-' + $target + '.zip","browser_download_url":"' + $url + '","digest":"sha256:' + $sha + '"}]}]')
$env:AMD_UPDATE_RELEASES = 'file://' + ($list -replace '\\', '/')

Check 'rite update replaces the running rite.exe and riteui.exe, keeping the ones before, and refreshes the amd.exe copy' {
    $out = & $rite update 2>&1 | Out-String
    Write-Host $out.Trim()
    (& $rite --version) -eq 'rite 9.9.9' -and (& (Join-Path $dir 'riteui.exe') --version) -eq 'riteui 9.9.9' -and
        (Test-Path -LiteralPath "$rite.previous") -and
        (& (Join-Path $dir 'amd.exe') --version 2>$null) -eq 'rite 9.9.9'
}
Check 'rite update --rollback puts the previous rite.exe back' {
    # Run from the kept copy, as a user stuck on a broken 9.9.9 would; under
    # a .exe name, which Windows wants to start a program.
    $kept = Join-Path $dir 'amd-kept.exe'
    Copy-Item -LiteralPath "$rite.previous" -Destination $kept
    $out = & $kept update --rollback 2>&1 | Out-String
    Write-Host $out.Trim()
    Remove-Item -LiteralPath $kept
    (& $rite --version) -match '^rite ' -and (& $rite --version) -ne 'rite 9.9.9' -and
        -not (Test-Path -LiteralPath "$rite.previous")
}
Check 'the amd.exe copy follows: the update, then the rollback (ticket 311)' {
    $copy = Join-Path $dir 'amd.exe'
    (& $copy --version 2>$null) -eq (& $rite --version) -and (& $copy --version 2>$null) -ne 'rite 9.9.9'
}
Remove-Item Env:\AMD_UPDATE_RELEASES

Write-Host 'install.ps1 again, over the install:'
& (Join-Path $root 'install.ps1') -Dir $dir -Zip $zip
Check 'the PATH has the folder once' { @((UserPath) | Where-Object { $_ -eq $dir }).Count -eq 1 }

Write-Host 'rite-uninstall.ps1:'
& $uninstaller -Yes
Check 'the programs, uninstaller and folder are gone' { -not (Test-Path -LiteralPath $dir) }
Check 'the Start menu shortcut is gone' { -not (Test-Path -LiteralPath $shortcut) }
Check 'Settings > Apps no longer lists it' { -not (Test-Path $key) }
Check 'the folder is off the user PATH' { -not ((UserPath) -contains $dir) }

Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
if ($failures -gt 0) {
    Write-Host "$failures failed" -ForegroundColor Red
    exit 1
}
Write-Host 'all passed'
