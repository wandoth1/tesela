<#
.SYNOPSIS
  Test Tesela MSI installation (install, upgrade, repair, uninstall, per-machine install, scope changes...) without a
  UI (/qn). Runs ONLY on a GitHub Actions runner: it performs real installations and uninstalls.

.DESCRIPTION
  The `msi` job in .github/workflows/ci.yml runs this after building the MSI and downloading the published v0.1.4
  (verified with `gh attestation verify`). The runner is a disposable administrator with an elevated session. This test
  REFUSES to run on a development machine (checks GITHUB_ACTIONS and RUNNER_ENVIRONMENT=github-hosted); local installation
  testing is done manually with the user's awareness, never by an agent.

  Scenarios (each restores the machine to its prior state and verifies it):
    1. Per-user, silent, default location: files in %LOCALAPPDATA%\Programs\Tesela, a shortcut in the user's Start Menu,
       NOTHING in HKLM or Program Files, no desktop shortcut, `--version`, exactly one product and one Installed Apps entry;
       repair (restores a deleted file); uninstall leaves no leftovers.
    2. Upgrade from the published v0.1.4 (per-user), default location: one product after upgrading; downgrading by installing
       v0.1.4 over the newer version remains BLOCKED if the new version is higher (if equal, record and skip the downgrade).
    3. Upgrade from v0.1.4 installed in a CUSTOM location: the upgrade stays there, and uninstall removes it without repeating
       INSTALLFOLDER.
    4. Per-machine (ALLUSERS=1): Program Files\Tesela, HKLM, common Start Menu, `--version`, one product, and clean uninstall;
       custom per-machine location (INSTALLFOLDER reaches the server phase) and upgrade stays in place.
    5. Scope changes are BLOCKED in both directions with the expected message, without touching the existing installation.
    6. DESKTOPSHORTCUT: no shortcut by default; =1 creates it (per-user and per-machine), it is REMEMBERED on upgrade without
       repeating the property, =0 removes it, and uninstall deletes it.
    7. Protected folder (Program Files) in a per-user install: blocked with the expected message even though the runner is an
       administrator; allowed for a per-machine install.
    8. A registry key WITHOUT a product (stale data, no tesela.exe) in HKCU or HKLM does not block installation.
    9. Disposable STANDARD user (random password is never printed; user is removed even on failure), running
       scripts/tests/msi-install-standard.tests.ps1 AS that user without elevation: `ALLUSERS=1 /qn` must fail and leave
       nothing; a per-user install must succeed in that profile without touching Program Files or HKLM; then another account
       (the administrator) installs per-machine in between, and the standard user's per-user install still upgrades and uninstalls.
    A skipped test does NOT count as passed: skips are listed separately, minimum scenario and check counts are enforced,
    and no scenario may be skipped after a version newer than published v0.1.4. The job summary includes MSI_LUA lines
    from msiexec logs (how the installer engine chose scope and elevation).

  NOT tested (requires a person): the UI dialogs, the administrator (UAC) prompt when selecting "for everyone" (the runner
  has no interactive desktop), and secure properties when a standard user enters credentials for ANOTHER administrator.

.PARAMETER NewMsi
  The MSI just built by scripts/build-msi.ps1.

.PARAMETER OldMsi
  The published v0.1.4 MSI (downloaded and verified beforehand).

.PARAMETER WorkDir
  Working directory for logs and custom install directories (defaults to a new directory in RUNNER_TEMP). Must not contain spaces.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$NewMsi,
    [Parameter(Mandatory)][string]$OldMsi,
    [string]$WorkDir
)

$ErrorActionPreference = 'Stop'

# --- Safety: only on a disposable administrator GitHub Actions runner --------------------------------------------------
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'msi-install.tests.ps1 performs real installs and uninstalls; run it only on a GitHub-hosted Actions runner (GITHUB_ACTIONS=true and RUNNER_ENVIRONMENT=github-hosted).'
}
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { throw 'An administrator with an elevated session is required (the runner provides one).' }

$NewMsi = [System.IO.Path]::GetFullPath($NewMsi)
$OldMsi = [System.IO.Path]::GetFullPath($OldMsi)
foreach ($f in $NewMsi, $OldMsi) { if (-not (Test-Path -LiteralPath $f -PathType Leaf)) { throw "File does not exist: $f" } }
if (-not $WorkDir) { $WorkDir = Join-Path $env:RUNNER_TEMP ('tesela-install-tests-' + [guid]::NewGuid().ToString('N').Substring(0, 8)) }
$WorkDir = [System.IO.Path]::GetFullPath($WorkDir)
if ($WorkDir -match '\s') { throw "WorkDir cannot contain spaces (INSTALLFOLDER is passed without quotes): $WorkDir" }
$tempRoot = [System.IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\')
if (-not $WorkDir.StartsWith($tempRoot + '\', [System.StringComparison]::OrdinalIgnoreCase)) { throw "WorkDir must be inside RUNNER_TEMP ($tempRoot)" }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

$UpgradeCode = '{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}'
$ExpectedFiles = @('tesela.exe', 'tesela-scan.exe', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md')

# --- Checks ----------------------------------------------------------------------------------------------------------------------
$passed = New-Object System.Collections.Generic.List[string]
$failed = New-Object System.Collections.Generic.List[string]
# Checks that did NOT run (not counted as passed): printed separately; the job can require zero skips.
$skipped = New-Object System.Collections.Generic.List[string]
$script:stepsRun = 0
$script:createdInstallerPolicy = $null
function Check([string]$Case, [bool]$Condition, [string]$Detail = '') {
    if ($Condition) { $script:passed.Add($Case); Write-Host "  ok   $Case" }
    else { $script:failed.Add($(if ($Detail) { "$Case ($Detail)" } else { $Case })); Write-Host "  FALLA $Case $Detail" -ForegroundColor Red }
}
function Step([string]$Text) { $script:stepsRun++; Write-Host ''; Write-Host "== $Text" }

# --- Utilities ------------------------------------------------------------------------------------------------------------------
$script:logIndex = 0
function Invoke-Msi {
    # Run msiexec without UI and return @{ Code; Log; Text }. Remove NULs from the log (it may be UTF-16) so ASCII text can
    # be searched.
    param([string[]]$MsiArgs, [string]$Name)
    $script:logIndex++
    $log = Join-Path $WorkDir ('{0:D2}-{1}.log' -f $script:logIndex, $Name)
    $argText = ($MsiArgs -join ' ') + " /qn /norestart /l*v `"$log`""
    Write-Host "  msiexec $($MsiArgs -join ' ')"
    $p = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\msiexec.exe') -ArgumentList $argText -Wait -PassThru
    $text = ''
    if (Test-Path -LiteralPath $log) {
        $bytes = [System.IO.File]::ReadAllBytes($log)
        $text = [System.Text.Encoding]::Latin1.GetString(@($bytes | Where-Object { $_ -ne 0 }))
    }
    return [pscustomobject]@{ Code = $p.ExitCode; Log = $log; Text = $text }
}
function Install-Msi([string]$Msi, [string[]]$Props, [string]$Name) { return Invoke-Msi -MsiArgs (@('/i', "`"$Msi`"") + $Props) -Name $Name }
function Uninstall-Msi([string]$Msi, [string]$Name) { return Invoke-Msi -MsiArgs @('/x', "`"$Msi`"") -Name $Name }
function Repair-Msi([string]$Msi, [string]$Name) { return Invoke-Msi -MsiArgs @('/fa', "`"$Msi`"") -Name $Name }

$installer = New-Object -ComObject WindowsInstaller.Installer
function Get-Products {
# Tesela products visible to Windows Installer by UpgradeCode (per-user and per-machine).
    $list = [System.__ComObject].InvokeMember('RelatedProducts', 'GetProperty', $null, $installer, @([string]$UpgradeCode))
    $res = New-Object System.Collections.Generic.List[object]
    foreach ($pc in $list) {
        $code = [string]$pc
        $ver = [string][System.__ComObject].InvokeMember('ProductInfo', 'GetProperty', $null, $installer, @($code, 'VersionString'))
        $res.Add([pscustomobject]@{ Code = $code; Version = $ver })
    }
    return $res.ToArray()
}
function Get-UninstallEntries {
# "Tesela" entries in Installed Apps (HKCU and HKLM, 64-bit and 32-bit views).
    $keys = @('HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall', 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall')
    $n = 0
    foreach ($k in $keys) {
        if (-not (Test-Path -LiteralPath $k)) { continue }
        foreach ($sub in Get-ChildItem -LiteralPath $k) {
            $dn = (Get-ItemProperty -LiteralPath $sub.PSPath -ErrorAction SilentlyContinue).DisplayName
            if ($dn -eq 'Tesela') { $n++ }
        }
    }
    return $n
}
function Get-MsiVersion([string]$Msi) {
    $db = [System.__ComObject].InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Msi, 0))
    $view = [System.__ComObject].InvokeMember('OpenView', 'InvokeMethod', $null, $db, @('SELECT `Value` FROM `Property` WHERE `Property`=''ProductVersion'''))
    [System.__ComObject].InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    $rec = [System.__ComObject].InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
    $v = [string][System.__ComObject].InvokeMember('StringData', 'GetProperty', $null, $rec, @(1))
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($rec)
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($view)
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($db)
    return $v
}
function New-SiblingMsi([string]$Src, [string]$Dst) {
    # Copy the MSI with a DIFFERENT ProductCode and package code (the Summary Information revision counter), as in a later
    # build of the same version. Installing it over the original is an UPGRADE (same UpgradeCode, different product); using
    # the same file would only maintain the same product. Modify the copy only.
    Copy-Item -LiteralPath $Src -Destination $Dst
    $newProduct = '{' + [guid]::NewGuid().ToString().ToUpperInvariant() + '}'
    $newPackage = '{' + [guid]::NewGuid().ToString().ToUpperInvariant() + '}'
    $db = [System.__ComObject].InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Dst, 1))
    $sql = "UPDATE ``Property`` SET ``Value``='$newProduct' WHERE ``Property``='ProductCode'"
    $view = [System.__ComObject].InvokeMember('OpenView', 'InvokeMethod', $null, $db, @($sql))
    [System.__ComObject].InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    [System.__ComObject].InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($view)
    $si = [System.__ComObject].InvokeMember('SummaryInformation', 'GetProperty', $null, $db, @(1))
    [System.__ComObject].InvokeMember('Property', 'SetProperty', $null, $si, @(9, $newPackage)) | Out-Null
    [System.__ComObject].InvokeMember('Persist', 'InvokeMethod', $null, $si, $null) | Out-Null
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($si)
    [System.__ComObject].InvokeMember('Commit', 'InvokeMethod', $null, $db, $null) | Out-Null
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($db)
}
function Uninstall-Installed([string]$Name) {
    # Uninstall the ONLY installed Tesela product by ProductCode, regardless of which MSI installed it.
    $prods = @(Get-Products)
    if ($prods.Count -ne 1) { throw "Expected exactly one installed product to uninstall; found $($prods.Count)." }
    return Invoke-Msi -MsiArgs @('/x', $prods[0].Code) -Name $Name
}
function Test-Files([string]$Dir) {
    foreach ($f in $ExpectedFiles) { if (-not (Test-Path -LiteralPath (Join-Path $Dir $f) -PathType Leaf)) { return $false } }
    return $true
}
function Get-RegValue([string]$Key, [string]$Name) {
    if (-not (Test-Path -LiteralPath $Key)) { return $null }
    return (Get-ItemProperty -LiteralPath $Key -ErrorAction SilentlyContinue).$Name
}
function Test-ReparseFree([string]$Dir) {
    # No links (junctions or symbolic links) anywhere in the tree: only then is recursive deletion allowed.
    if (-not (Test-Path -LiteralPath $Dir)) { return $true }
    $items = @(Get-Item -LiteralPath $Dir -Force) + @(Get-ChildItem -LiteralPath $Dir -Recurse -Force -ErrorAction SilentlyContinue)
    return @($items | Where-Object { $_.Attributes -band [System.IO.FileAttributes]::ReparsePoint }).Count -eq 0
}
function Remove-TestDir([string]$Dir) {
    # Only directories under WorkDir or the per-machine test directory, and only when no links exist.
    if (-not (Test-Path -LiteralPath $Dir)) { return }
    $full = [System.IO.Path]::GetFullPath($Dir).TrimEnd('\')
    $ok = $full.StartsWith($WorkDir + '\', [System.StringComparison]::OrdinalIgnoreCase) -or $full -ieq $machineCustomRoot -or $full -ieq $stdShared
    if (-not $ok) { throw "Refusing to delete outside the test area: $full" }
    if (-not (Test-ReparseFree $full)) { throw "Refusing to delete ${full}: it contains links (reparse points)." }
    Remove-Item -LiteralPath $full -Recurse -Force
}
function Assert-Clean([string]$When) {
    # The machine has no Tesela installation (products, entries, registry values, or directories).
    $prods = @(Get-Products)
    Check "$When`: no Tesela products are installed" ($prods.Count -eq 0) "found $($prods.Count)"
    Check "$When`: no Tesela entries in Installed Apps" ((Get-UninstallEntries) -eq 0)
    Check "$When`: no Program Files directory" (-not (Test-Path -LiteralPath $machineDefault))
    Check "$When`: no per-user directory" (-not (Test-Path -LiteralPath $userDefault))
    Check "$When`: no InstallFolder in HKCU" ($null -eq (Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder'))
    Check "$When`: no InstallFolder in HKLM" ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
    Check "$When`: no Tesela key remains in HKCU" (-not (Test-Path -LiteralPath 'HKCU:\Software\Tesela'))
    Check "$When`: no Tesela key remains in HKLM" (-not (Test-Path -LiteralPath 'HKLM:\SOFTWARE\Tesela'))
    Check "$When`: no shortcut in the user's Start Menu" (-not (Test-Path -LiteralPath $userStartMenu))
    Check "$When`: no shortcut in the common Start Menu" (-not (Test-Path -LiteralPath $commonStartMenu))
    Check "$When`: no shortcut on the user's desktop" (-not (Test-Path -LiteralPath $userDesktop))
    Check "$When`: no shortcut on the common desktop" (-not (Test-Path -LiteralPath $commonDesktop))
}
function Reset-Machine {
    # If a scenario fails partway through, uninstall any Tesela product (the runner is disposable and contains only our test install).
    foreach ($p in @(Get-Products)) { [void](Invoke-Msi -MsiArgs @('/x', $p.Code) -Name 'reset') }
}
function Test-TeselaVersion([string]$Exe, [string]$Label) {
    # tesela.exe is a GUI executable: --version is written to STDERR without opening a window.
    $err = Join-Path $WorkDir 'version.txt'
    $p = Start-Process -FilePath $Exe -ArgumentList '--version' -Wait -PassThru -RedirectStandardError $err
    $out = if (Test-Path -LiteralPath $err) { Get-Content -LiteralPath $err -Raw } else { '' }
    Check "$Label`: tesela.exe --version responds" ($p.ExitCode -eq 0 -and $out -match '^tesela \d+\.\d+\.\d+') "exit code $($p.ExitCode), output '$out'"
}
function Show-LogTail([object]$R) {
    Write-Host "  --- last lines of $($R.Log) ---"
    ($R.Text -split "`n") | Select-Object -Last 25 | ForEach-Object { Write-Host "  | $_" }
}

# --- Disposable STANDARD user (scenario 9) ------------------------------------------------------------------------------------------
$stdShared = 'C:\TeselaCiStd'
$stdName = 'teselacistd'
$script:stdUser = $null
function New-RandomPassword {
    # Generate a random password here; never print it (and mask it in the job log as a safeguard).
    $b = New-Object byte[] 24
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $rng.GetBytes($b)
    $rng.Dispose()
    return 'Aa1!' + [Convert]::ToBase64String($b).Replace('=', 'x')
}
function New-StandardUser {
    if (Get-LocalUser -Name $stdName -ErrorAction SilentlyContinue) { throw "Local user ${stdName} already exists; aborting without touching it." }
    $pw = New-RandomPassword
    Write-Host "::add-mask::$pw"
    $sec = ConvertTo-SecureString $pw -AsPlainText -Force
    New-LocalUser -Name $stdName -Password $sec -FullName 'Tesela CI standard user' -Description 'Disposable user of the Tesela CI install tests' -AccountNeverExpires -PasswordNeverExpires | Out-Null
    # Only the Users group (S-1-5-32-545): standard user, no administrator rights.
    Add-LocalGroupMember -SID 'S-1-5-32-545' -Member $stdName
    $script:stdUser = [pscustomobject]@{ Name = $stdName; Cred = (New-Object System.Management.Automation.PSCredential(".\$stdName", $sec)) }
    $isAdminMember = @(Get-LocalGroupMember -SID 'S-1-5-32-544' -ErrorAction SilentlyContinue | Where-Object { $_.Name -like "*\$stdName" }).Count -gt 0
    Check 'Disposable account is a STANDARD user (not in Administrators)' (-not $isAdminMember)
}
function Remove-StandardUser {
    # Delete the user even if the test fails (leave the profile directory because it contains Windows-owned links and the runner is disposable).
    if (Get-LocalUser -Name $stdName -ErrorAction SilentlyContinue) { Remove-LocalUser -Name $stdName }
    $script:stdUser = $null
}
function Invoke-StdPhase([string]$Phase, [string]$MsiPath) {
    # Launch msi-install-standard.tests.ps1 AS the standard user (without elevation) and add its result lines to the checks.
    $result = Join-Path $stdShared "result-$Phase.txt"
    if (Test-Path -LiteralPath $result) { Remove-Item -LiteralPath $result -Force }
    $stdArgs = @('-NoProfile', '-File', "`"$stdScriptCopy`"", '-Phase', $Phase, '-Shared', "`"$stdShared`"", '-ResultFile', "`"$result`"")
    if ($MsiPath) { $stdArgs += @('-Msi', "`"$MsiPath`"") }
    $pwshExe = (Get-Process -Id $PID).Path
    Write-Host "  (as $stdName) pwsh -File msi-install-standard.tests.ps1 -Phase $Phase"
    try {
        $outFile = Join-Path $stdShared "std-$Phase.out.txt"
        $errFile = Join-Path $stdShared "std-$Phase.err.txt"
        $p = Start-Process -FilePath $pwshExe -ArgumentList ($stdArgs -join ' ') -Credential $script:stdUser.Cred -LoadUserProfile -WorkingDirectory $stdShared -UseNewEnvironment -PassThru -RedirectStandardOutput $outFile -RedirectStandardError $errFile
    } catch {
        Check "can launch a process as the standard user (phase $Phase)" $false "Start-Process -Credential failed: $($_.Exception.Message)"
        return
    }
    Wait-Process -Id $p.Id -Timeout 900 -ErrorAction SilentlyContinue
    if (Get-Process -Id $p.Id -ErrorAction SilentlyContinue) {
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        Check "standard-user phase $Phase completed on time" $false 'timed out (15 minutes)'
        return
    }
    foreach ($f in $outFile, $errFile) {
        if ((Test-Path -LiteralPath $f) -and (Get-Item -LiteralPath $f).Length -gt 0) { Write-Host "  [standard user] contents of $(Split-Path -Leaf $f):"; Get-Content -LiteralPath $f -TotalCount 40 | ForEach-Object { Write-Host "    | $_" } }
    }
    if (-not (Test-Path -LiteralPath $result)) {
        Check "standard-user phase $Phase produced results" $false 'result file is missing: the process did not write it (profile, permissions, or startup error)'
        return
    }
    $lines = @(Get-Content -LiteralPath $result)
    $count = 0
    foreach ($l in $lines) {
        $parts = $l -split '\|', 3
        switch ($parts[0]) {
            'ok' { $count++; Check "(standard user) $($parts[1])" $true }
            'FAIL' { $count++; Check "(standard user) $($parts[1])" $false $(if ($parts.Count -gt 2) { $parts[2] } else { '' }) }
            'INFO' { Write-Host "  [standard user] $($parts[1])" }
        }
    }
    Check "standard-user phase $Phase ran checks" ($count -gt 0)
    if (@($lines | Where-Object { $_ -like 'FAIL|*' }).Count -gt 0) { Show-StdLogs $Phase }
}
function Show-StdLogs([string]$Phase) {
    # If a phase fails, show the most relevant standard-user msiexec log lines (errors, policies, scope, elevation).
    foreach ($log in Get-ChildItem -LiteralPath $stdShared -Filter "std-$Phase-*.log" -ErrorAction SilentlyContinue) {
        $text = [System.Text.Encoding]::Latin1.GetString(@([System.IO.File]::ReadAllBytes($log.FullName) | Where-Object { $_ -ne 0 }))
        Write-Host "  [standard user] $($log.Name): relevant lines"
        ($text -split "`r?`n") | Where-Object { $_ -match 'Error|error|1625|1925|policy|Policy|DisableMSI|DisableUserInstalls|MSI_LUA|dual mode|Return value|Elevat|privilege|not allowed|forbidden|rejected|Product:|Note:' } | Select-Object -First 40 | ForEach-Object { Write-Host "    | $($_.Trim())" }
    }
}
function Show-PolicyDiagnostics {
    # Read-only runner policies that may determine whether a standard user can install; include them in the job log.
    foreach ($k in 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Installer', 'HKCU:\SOFTWARE\Policies\Microsoft\Windows\Installer', 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Safer\CodeIdentifiers', 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System', 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\SrpV2') {
        if (Test-Path -LiteralPath $k) {
            Write-Host "  [policy] $k"
            (Get-ItemProperty -LiteralPath $k | Get-Member -MemberType NoteProperty | Where-Object { $_.Name -notlike 'PS*' } | ForEach-Object { "$($_.Name)=$((Get-ItemProperty -LiteralPath $k).($_.Name))" }) -join '; ' | ForEach-Object { Write-Host "    $_" }
        } else { Write-Host "  [policy] $k : does not exist" }
    }
    $svc = Get-Service -Name seclogon -ErrorAction SilentlyContinue
    Write-Host "  [service] seclogon (Secondary Logon): $($svc.Status) / $($svc.StartType)"
}
function Get-LuaEvidence {
    # msiexec log lines showing how the engine chose scope and elevation (MSI_LUA, dual mode, 1925...).
    $out = New-Object System.Collections.Generic.List[string]
    foreach ($dir in $WorkDir, $stdShared) {
        if (-not (Test-Path -LiteralPath $dir)) { continue }
        # Standard-user lines (std-*) first: they are the most relevant evidence.
        foreach ($log in (Get-ChildItem -LiteralPath $dir -Filter '*.log' -ErrorAction SilentlyContinue | Sort-Object { if ($_.Name -like 'std-*') { 0 } else { 1 } }, Name)) {
            $text = [System.Text.Encoding]::Latin1.GetString(@([System.IO.File]::ReadAllBytes($log.FullName) | Where-Object { $_ -ne 0 }))
            foreach ($line in ($text -split "`r?`n")) {
                if ($line -match 'MSI_LUA|dual mode|Elevation required|sufficient privileges|MSIINSTALLPERUSER') { $out.Add("$($log.Name): $($line.Trim())") }
            }
        }
    }
    return @($out | Select-Object -Unique | Select-Object -First 80)
}

# --- Paths ---------------------------------------------------------------------------------------------------------------------
$userDefault = Join-Path $env:LOCALAPPDATA 'Programs\Tesela'
$machineDefault = Join-Path $env:ProgramFiles 'Tesela'
$userStartMenu = Join-Path ([Environment]::GetFolderPath('Programs')) 'Tesela.lnk'
$commonStartMenu = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'Tesela.lnk'
$userDesktop = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Tesela.lnk'
$commonDesktop = Join-Path ([Environment]::GetFolderPath('CommonDesktopDirectory')) 'Tesela.lnk'
$userCustom = Join-Path $WorkDir 'custom-user\Tesela'
$machineCustomRoot = 'C:\TeselaCiMachine'
$machineCustom = Join-Path $machineCustomRoot 'Tesela'
$SiblingMsi = Join-Path $WorkDir 'sibling.msi'
New-SiblingMsi $NewMsi $SiblingMsi
$newVersion = Get-MsiVersion $NewMsi
$oldVersion = Get-MsiVersion $OldMsi
Write-Host "New MSI $newVersion, old MSI $oldVersion, working directory: $WorkDir"

try {
    Step '0. Starting state: no Tesela installation is present'
    Assert-Clean 'inicio'
    if ($failed.Count -gt 0) { throw 'The runner already had Tesela installed; aborting without touching anything.' }
    Check 'Previous MSI is v0.1.4' ($oldVersion -eq '0.1.4') "found $oldVersion"

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '1. Per-user, silent, default location'
    $r = Install-Msi $NewMsi @() 'user-install'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'Install (/qn) exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'all 5 files are in %LOCALAPPDATA%\Programs\Tesela' (Test-Files $userDefault)
    Check 'nothing is in Program Files' (-not (Test-Path -LiteralPath $machineDefault))
    Check 'HKCU\Software\Tesela\InstallFolder points to the per-user folder' ((Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder') -ieq ($userDefault + '\'))
    Check 'nothing is in HKLM\SOFTWARE\Tesela' ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
    Check 'shortcut is in the user Start Menu' (Test-Path -LiteralPath $userStartMenu)
    Check 'no shortcut is in the common Start Menu' (-not (Test-Path -LiteralPath $commonStartMenu))
    Check 'no desktop shortcut by default' (-not (Test-Path -LiteralPath $userDesktop) -and -not (Test-Path -LiteralPath $commonDesktop))
    Check 'exactly one product with the MSI version' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Check 'exactly one Installed Apps entry' ((Get-UninstallEntries) -eq 1)
    Test-TeselaVersion (Join-Path $userDefault 'tesela.exe') 'per-user'
    # Repair: restore a deleted file.
    Remove-Item -LiteralPath (Join-Path $userDefault 'tesela-scan.exe') -Force
    $r = Repair-Msi $NewMsi 'user-repair'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'Repair (/fa) exits with 0 and restores the deleted file' ($r.Code -eq 0 -and (Test-Files $userDefault)) "exit code $($r.Code)"
    $r = Uninstall-Msi $NewMsi 'user-uninstall'
    Check 'Uninstall exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Assert-Clean 'after per-user uninstall'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '2. Upgrade from published v0.1.4 (per-user, default location)'
    $r = Install-Msi $OldMsi @() 'old-install'
    Check 'Install v0.1.4 exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'v0.1.4 is in the default folder and is the only product' ((Test-Files $userDefault) -and (@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq '0.1.4')
    $r = Install-Msi $NewMsi @() 'upgrade-default'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'Upgrade to the new version exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'exactly ONE product remains after upgrade, with the new version' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Check 'exactly ONE Installed Apps entry remains after upgrade' ((Get-UninstallEntries) -eq 1)
    Check 'the upgrade stays in the default folder with all 5 files' (Test-Files $userDefault)
    Check 'the Start Menu shortcut remains' (Test-Path -LiteralPath $userStartMenu)
    if ([version]$newVersion -gt [version]$oldVersion) {
        $r = Install-Msi $OldMsi @() 'downgrade'
        Check 'Downgrade (installing v0.1.4 over the new version) remains BLOCKED (exit code 1603)' ($r.Code -eq 1603) "exit code $($r.Code)"
        Check 'Downgrade is blocked with the expected message' ($r.Text -match 'newer version of Tesela is already installed') 'message is missing from the log'
        Check 'the new version remains installed after the blocked downgrade' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    } else {
        Write-Host "  (downgrade not applicable: new version ($newVersion) is not newer than published version ($oldVersion); it will be tested when the version is bumped)"
        $skipped.Add('downgrade: NOT RUN (new version is not newer than published version); test it when the version is bumped')
    }
    $r = Uninstall-Installed 'upgrade-uninstall'
    Check 'Uninstall after upgrade exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Assert-Clean 'after uninstalling the upgrade'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '3. Upgrade from v0.1.4 in a CUSTOM per-user folder'
    $r = Install-Msi $OldMsi @("INSTALLFOLDER=$userCustom\") 'old-custom'
    Check 'Install v0.1.4 in a custom folder exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'v0.1.4 is in the custom folder and not the default folder' ((Test-Files $userCustom) -and -not (Test-Path -LiteralPath $userDefault))
    $r = Install-Msi $NewMsi @() 'upgrade-custom'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'Upgrade without repeating INSTALLFOLDER exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'upgrade keeps the custom folder (5 files) and does not create the default folder' ((Test-Files $userCustom) -and -not (Test-Path -LiteralPath $userDefault))
    Check 'exactly one product remains, with the new version' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Test-TeselaVersion (Join-Path $userCustom 'tesela.exe') 'custom folder'
    $r = Uninstall-Installed 'custom-uninstall'
    Check 'Uninstall without repeating INSTALLFOLDER exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'uninstall removes the entire custom folder' (-not (Test-Path -LiteralPath (Join-Path $userCustom 'tesela.exe')) -and -not (Test-Path -LiteralPath $userCustom))
    Assert-Clean 'after uninstalling the custom-folder install'
    Remove-TestDir (Join-Path $WorkDir 'custom-user')

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '4. Per-machine (ALLUSERS=1)'
    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'machine-install'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'Per-machine install exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'all 5 files are in Program Files\Tesela' (Test-Files $machineDefault)
    Check 'nothing is in the per-user folder' (-not (Test-Path -LiteralPath $userDefault))
    Check 'HKLM\SOFTWARE\Tesela\InstallFolder points to Program Files\Tesela' ((Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder') -ieq ($machineDefault + '\'))
    Check 'nothing is in HKCU\Software\Tesela' ($null -eq (Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder'))
    Check 'shortcut is in the common Start Menu, not the user Start Menu' ((Test-Path -LiteralPath $commonStartMenu) -and -not (Test-Path -LiteralPath $userStartMenu))
    Check 'exactly one product with the MSI version' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Check 'exactly one Installed Apps entry' ((Get-UninstallEntries) -eq 1)
    Test-TeselaVersion (Join-Path $machineDefault 'tesela.exe') 'per-machine'
    $r = Uninstall-Msi $NewMsi 'machine-uninstall'
    Check 'Per-machine uninstall exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Assert-Clean 'after per-machine uninstall'

    Write-Host '  -- per-machine custom folder (INSTALLFOLDER reaches the server phase) and upgrade stays in place'
    $r = Install-Msi $NewMsi @('ALLUSERS=1', "INSTALLFOLDER=$machineCustom\") 'machine-custom'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'Per-machine install with INSTALLFOLDER exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'all 5 files are in the custom per-machine folder, not Program Files' ((Test-Files $machineCustom) -and -not (Test-Path -LiteralPath $machineDefault))
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'machine-custom-upgrade'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'Per-machine reinstall without INSTALLFOLDER exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'per-machine upgrade stays in its folder and leaves one product' ((Test-Files $machineCustom) -and -not (Test-Path -LiteralPath $machineDefault) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Installed 'machine-custom-uninstall'
    Check 'Uninstall without repeating INSTALLFOLDER exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'uninstall removes the custom per-machine folder' (-not (Test-Path -LiteralPath $machineCustom))
    Remove-TestDir $machineCustomRoot
    Assert-Clean 'after uninstalling the custom per-machine install'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '5. Scope changes are blocked (existing installation remains untouched)'
    $r = Install-Msi $NewMsi @() 'scope-user'
    Check 'Per-user install exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'scope-user-to-machine'
    Check 'Per-user to per-machine switch (ALLUSERS=1) is BLOCKED (exit code 1603)' ($r.Code -eq 1603) "exit code $($r.Code)"
    Check 'blocked switch says to uninstall first (per-user -> per-machine)' ($r.Text -match 'already installed for your user only') 'expected message is missing from the log'
    Check 'per-user install remains intact; nothing is in Program Files or HKLM' ((Test-Files $userDefault) -and -not (Test-Path -LiteralPath $machineDefault) -and ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder')) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Msi $NewMsi 'scope-user-uninstall'
    Check 'Per-user uninstall exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Assert-Clean 'after blocked per-user -> per-machine switch'

    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'scope-machine'
    Check 'Per-machine install exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    $r = Install-Msi $SiblingMsi @() 'scope-machine-to-user'
    Check 'Per-machine to per-user switch (without ALLUSERS) is BLOCKED (exit code 1603)' ($r.Code -eq 1603) "exit code $($r.Code)"
    Check 'blocked switch says to use ALLUSERS=1 or uninstall first (per-machine -> per-user)' ($r.Text -match 'already installed for all users') 'expected message is missing from the log'
    Check 'per-machine install remains intact; nothing is in the per-user folder or HKCU' ((Test-Files $machineDefault) -and -not (Test-Path -LiteralPath $userDefault) -and ($null -eq (Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder')) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Msi $NewMsi 'scope-machine-uninstall'
    Check 'Per-machine uninstall exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Assert-Clean 'after blocked per-machine -> per-user switch'

    # A per-user v0.1.4 install also blocks switching to per-machine with the new MSI.
    $r = Install-Msi $OldMsi @() 'scope-old-user'
    Check 'Install v0.1.4 per-user exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'scope-old-to-machine'
    Check 'switching v0.1.4 from per-user to per-machine is BLOCKED (exit code 1603)' ($r.Code -eq 1603) "exit code $($r.Code)"
    $r = Install-Msi $NewMsi @() 'scope-old-upgrade'
    Check 'per-user upgrade from v0.1.4 succeeds' ($r.Code -eq 0 -and (@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion) "exit code $($r.Code)"
    $r = Uninstall-Installed 'scope-old-uninstall'
    Check 'Uninstall exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Assert-Clean 'after blocked scope switch from v0.1.4'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '6. Desktop shortcut (DESKTOPSHORTCUT)'
    $r = Install-Msi $NewMsi @('DESKTOPSHORTCUT=1') 'desktop-user'
    Check 'Per-user install with DESKTOPSHORTCUT=1 exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'shortcut is on the user desktop, not the common desktop' ((Test-Path -LiteralPath $userDesktop) -and -not (Test-Path -LiteralPath $commonDesktop))
    $r = Install-Msi $SiblingMsi @() 'desktop-user-remembered'
    Check 'Upgrade without repeating DESKTOPSHORTCUT exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'desktop shortcut is REMEMBERED on upgrade' (Test-Path -LiteralPath $userDesktop)
    $r = Install-Msi $NewMsi @('DESKTOPSHORTCUT=0') 'desktop-user-off'
    Check 'Upgrade with DESKTOPSHORTCUT=0 exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'DESKTOPSHORTCUT=0 removes the desktop shortcut' (-not (Test-Path -LiteralPath $userDesktop))
    $r = Install-Msi $SiblingMsi @('DESKTOPSHORTCUT=1') 'desktop-user-on'
    Check 'DESKTOPSHORTCUT=1 restores the shortcut' ($r.Code -eq 0 -and (Test-Path -LiteralPath $userDesktop))
    $r = Uninstall-Installed 'desktop-user-uninstall'
    Check 'Uninstall exits with 0 and removes the desktop shortcut' ($r.Code -eq 0 -and -not (Test-Path -LiteralPath $userDesktop)) "exit code $($r.Code)"
    Assert-Clean 'after uninstalling with a desktop shortcut'

    $r = Install-Msi $NewMsi @('ALLUSERS=1', 'DESKTOPSHORTCUT=1') 'desktop-machine'
    Check 'Per-machine install with DESKTOPSHORTCUT=1 exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'shortcut is on the COMMON desktop, not the user desktop' ((Test-Path -LiteralPath $commonDesktop) -and -not (Test-Path -LiteralPath $userDesktop))
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'desktop-machine-remembered'
    Check 'Per-machine upgrade without repeating DESKTOPSHORTCUT remembers it' ($r.Code -eq 0 -and (Test-Path -LiteralPath $commonDesktop)) "exit code $($r.Code)"
    $r = Uninstall-Installed 'desktop-machine-uninstall'
    Check 'Per-machine uninstall exits with 0 and removes the common shortcut' ($r.Code -eq 0 -and -not (Test-Path -LiteralPath $commonDesktop)) "exit code $($r.Code)"
    Assert-Clean 'after per-machine uninstall with a desktop shortcut'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '7. Protected folder in a per-user install (clear message, not a silent 1603)'
    # The runner is an administrator, but "just for me" still rejects Program Files (this does not depend on Privileged).
    $protectedFolder = Join-Path $env:ProgramFiles 'TeselaCiProtegida'
    $r = Install-Msi $NewMsi @("INSTALLFOLDER=`"$protectedFolder`"") 'protected-user'
    Check 'Per-user install with INSTALLFOLDER in Program Files is BLOCKED (exit code 1603)' ($r.Code -eq 1603) "exit code $($r.Code)"
    Check 'blocked install says the folder is protected and suggests ALLUSERS=1' ($r.Text -match 'INSTALLFOLDER is in a protected location') 'expected message is missing from the log'
    Check 'nothing was installed and the folder was not created' (-not (Test-Path -LiteralPath $protectedFolder) -and (@(Get-Products)).Count -eq 0)
    $r = Install-Msi $NewMsi @('ALLUSERS=1', "INSTALLFOLDER=`"$protectedFolder`"") 'protected-machine'
    Check 'Per-machine install with INSTALLFOLDER in Program Files succeeds' ($r.Code -eq 0 -and (Test-Files $protectedFolder)) "exit code $($r.Code)"
    $r = Uninstall-Installed 'protected-machine-uninstall'
    Check 'Uninstall exits with 0 and removes the folder' ($r.Code -eq 0 -and -not (Test-Path -LiteralPath $protectedFolder)) "exit code $($r.Code)"
    Assert-Clean 'after protected-folder scenario'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '8. A registry key WITHOUT a product (stale data) does not block installation'
    # A key with InstallFolder but no tesela.exe in that directory (left by an old version or manually cleaned registry...).
    New-Item -Path 'HKCU:\Software\Tesela' -Force | Out-Null
    Set-ItemProperty -LiteralPath 'HKCU:\Software\Tesela' -Name 'InstallFolder' -Value ($WorkDir + '\resto-usuario\')
    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'stale-user-to-machine'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'HKCU key without a product does not block per-machine install (exit code 0)' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'installed in Program Files' (Test-Files $machineDefault)
    $r = Uninstall-Installed 'stale-user-uninstall'
    Check 'Uninstall exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Remove-Item -LiteralPath 'HKCU:\Software\Tesela' -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -Path 'HKLM:\SOFTWARE\Tesela' -Force | Out-Null
    Set-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Tesela' -Name 'InstallFolder' -Value ($WorkDir + '\resto-equipo\')
    $r = Install-Msi $NewMsi @() 'stale-machine-to-user'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'HKLM key without a product does not block per-user install (exit code 0)' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'installed in the per-user folder' (Test-Files $userDefault)
    $r = Uninstall-Installed 'stale-machine-uninstall'
    Check 'Uninstall exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    Remove-Item -LiteralPath 'HKLM:\SOFTWARE\Tesela' -Recurse -Force -ErrorAction SilentlyContinue
    Assert-Clean 'after stale registry keys without a product'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '9. STANDARD user (without privileges): per-user install without elevation, per-machine install fails, and their install takes precedence over another account'
    # Disposable local standard user, with a random unprinted password. The user reports its own actions (its HKCU and
    # profile are separate from the runner administrator's).
    New-Item -ItemType Directory -Force -Path $stdShared | Out-Null
    Show-PolicyDiagnostics
    # EVIDENCE (run 39030236010 for this PR): windows-latest is Windows SERVER, where the effective Windows Installer
    # policy is DisableMsi=1 ("for non-managed applications only"): a standard user cannot install ANY MSI, and msiexec
    # exits with 1625 ("This installation is forbidden by system policy") BEFORE choosing scope, including per-user dual-mode packages.
    # (the log says: "MSI_LUA: Package is a dual mode package and is being installed in Per-User mode. No elevation is required.").
    # This is not a package issue: the default on Windows 10 and 11 client is 0. To test the intended behavior, set this
    # DISPOSABLE machine to the Windows client default (DisableMsi=0) during the scenario, then remove the setting afterward.
    $installerPolicy = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Installer'
    if (Test-Path -LiteralPath $installerPolicy) { throw 'Windows Installer policy key already exists; aborting without changing it.' }
    New-Item -Path $installerPolicy -Force | Out-Null
    New-ItemProperty -LiteralPath $installerPolicy -Name 'DisableMSI' -Value 0 -PropertyType DWord -Force | Out-Null
    $script:createdInstallerPolicy = $installerPolicy
    Write-Host "  [policy] DisableMSI=0 at $installerPolicy (Windows client default; removed at the end)"
    New-StandardUser
    & icacls.exe $stdShared '/grant' "${stdName}:(OI)(CI)M" | Out-Null
    Set-Content -LiteralPath (Join-Path $stdShared '.tesela-ci-marker') -Value 'created by msi-install.tests.ps1 in a GitHub-hosted runner' -Encoding ASCII
    $stdScriptCopy = Join-Path $stdShared 'msi-install-standard.tests.ps1'
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'msi-install-standard.tests.ps1') -Destination $stdScriptCopy
    $stdNew = Join-Path $stdShared 'new.msi'
    $stdSibling2 = Join-Path $stdShared 'sibling2.msi'
    Copy-Item -LiteralPath $NewMsi -Destination $stdNew
    New-SiblingMsi $NewMsi $stdSibling2
    # (a) As a standard user, a per-machine install must FAIL and leave nothing behind.
    Invoke-StdPhase 'machine-attempt' $stdNew
    # (b) Per-user without elevation: install in the user's profile without touching Program Files or HKLM.
    Invoke-StdPhase 'user-install' $stdNew
    Check 'runner administrator cannot see the per-user product (each account has its own)' ((@(Get-Products)).Count -eq 0)
    Check 'nothing in Program Files after the standard user per-user install' (-not (Test-Path -LiteralPath $machineDefault))
    Check 'nothing in HKLM after the standard user per-user install' ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
    # (c) M2: another account (administrator) installs per-machine using another build; the standard user can still upgrade THEIR install.
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'std-machine-by-admin'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'another account (administrator) installs per-machine while the standard user has a per-user install (exit code 0)' ($r.Code -eq 0) "exit code $($r.Code)"
    Check 'per-machine install is in Program Files' (Test-Files $machineDefault)
    Invoke-StdPhase 'user-upgrade' $stdSibling2
    Check "the other account's per-machine install remains intact after the standard user upgrade" ((Test-Files $machineDefault) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Installed 'std-machine-uninstall'
    Check 'Uninstalling the per-machine install exits with 0' ($r.Code -eq 0) "exit code $($r.Code)"
    # (d) The standard user uninstalls their product without leftovers.
    Invoke-StdPhase 'user-uninstall' $null
    Remove-StandardUser
    Check 'disposable user was removed' (-not (Get-LocalUser -Name $stdName -ErrorAction SilentlyContinue))
    Assert-Clean 'after standard-user scenario'
} catch {
    $failed.Add("test interrupted: $($_.Exception.Message)")
    Write-Host "ERROR: $($_.Exception.Message)" -ForegroundColor Red
} finally {
    # Cleanup: remove any remaining Tesela product and test directories (the runner is disposable, but leave no debris).
    try { Reset-Machine } catch { Write-Host "cleanup: $($_.Exception.Message)" }
    # Save standard-user logs and results with the other logs before deleting the shared directory.
    try { if (Test-Path -LiteralPath $stdShared) { Get-ChildItem -LiteralPath $stdShared -File | Where-Object { $_.Extension -in '.log', '.txt' } | Copy-Item -Destination $WorkDir -Force } } catch { Write-Host "copying logs: $($_.Exception.Message)" }
    if ($script:createdInstallerPolicy) { Remove-Item -LiteralPath $script:createdInstallerPolicy -Recurse -Force -ErrorAction SilentlyContinue }
    try { Remove-StandardUser } catch { Write-Host "standard-user cleanup: $($_.Exception.Message)" }
    foreach ($k in 'HKCU:\Software\Tesela', 'HKLM:\SOFTWARE\Tesela') { Remove-Item -LiteralPath $k -Recurse -Force -ErrorAction SilentlyContinue }
    foreach ($d in (Join-Path $WorkDir 'custom-user'), $machineCustomRoot, $stdShared) {
        try { Remove-TestDir $d } catch { Write-Host "cleanup: $($_.Exception.Message)" }
    }
}

# --- Summary -------------------------------------------------------------------------------------------------------------------
# A test that did not run is not a passing test: enforce minimum scenario and check counts, and report skipped checks separately.
$ExpectedSteps = 10
$MinChecks = 220
if ($script:stepsRun -ne $ExpectedSteps) { $failed.Add("$($script:stepsRun) scenarios ran; expected $ExpectedSteps (a scenario did not run)") }
if ($passed.Count + $failed.Count -lt $MinChecks) { $failed.Add("only $($passed.Count + $failed.Count) checks ran; expected at least $MinChecks") }
$cargoMajor = [version]$newVersion
if ($skipped.Count -gt 0 -and $cargoMajor -gt [version]'0.1.4') { $failed.Add("tests were skipped for version $newVersion, which is later than published v0.1.4: $($skipped -join '; ')") }
$lua = @(Get-LuaEvidence)
Write-Host ''
Write-Host "msi-install.tests: $($passed.Count) checks passed, $($failed.Count) failed, $($skipped.Count) skipped, $($script:stepsRun) scenarios"
$skipped | ForEach-Object { Write-Host "  SKIPPED: $_" -ForegroundColor Yellow }
if ($lua.Count -gt 0) { Write-Host '  Installer-engine evidence (scope and elevation):'; $lua | ForEach-Object { Write-Host "    $_" } }
if ($env:GITHUB_STEP_SUMMARY) {
    $md = New-Object System.Collections.Generic.List[string]
    $md.Add("### MSI installation tests (version $newVersion; previous $oldVersion)")
    $md.Add('')
    $md.Add("**$($passed.Count) checks passed, $($failed.Count) failed, $($skipped.Count) skipped, $($script:stepsRun) scenarios.**")
    $md.Add('')
    foreach ($f in $failed) { $md.Add("- FAILED: $f") }
    foreach ($s in $skipped) { $md.Add("- SKIPPED (not counted as passed): $s") }
    if ($lua.Count -gt 0) {
        $md.Add('')
        $md.Add('Windows Installer engine evidence (msiexec logs: MSI_LUA, scope, and elevation):')
        $md.Add('')
        $md.Add('```')
        foreach ($l in $lua) { $md.Add($l) }
        $md.Add('```')
    }
    $md.Add('')
    $md.Add('<details><summary>Checks that passed</summary>')
    $md.Add('')
    foreach ($ok in $passed) { $md.Add("- ok: $ok") }
    $md.Add('')
    $md.Add('</details>')
    [System.IO.File]::AppendAllText($env:GITHUB_STEP_SUMMARY, (($md -join "`n") + "`n"))
}
if ($failed.Count -gt 0) {
    $failed | ForEach-Object { Write-Host "  FAILED: $_" -ForegroundColor Red }
    exit 1
}
exit 0
