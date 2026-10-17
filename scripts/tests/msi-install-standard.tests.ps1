<#
.SYNOPSIS
  One PHASE of the MSI installation test as a STANDARD USER (no privileges or elevation). Launched by
  scripts/tests/msi-install.tests.ps1 on a GitHub Actions runner with a disposable local user; do not run manually.

.DESCRIPTION
  Runs AS that user (Start-Process -Credential -LoadUserProfile). Refuses to run if the process is an administrator or
  the marker created by msi-install.tests.ps1 is missing (only created on a GitHub Actions runner). Runs ONE phase,
  performs a real install or uninstall with msiexec /qn (no UI), and writes one result line per check to -ResultFile
  (the runner administrator cannot see this user's HKCU or profile, so the user reports its own state):
      ok|<text>
      FAIL|<text>|<detail>
      INFO|<text>
  Phases:
    machine-attempt  `ALLUSERS=1 /qn` as standard user: must FAIL (no one can grant administrator rights) and leave nothing
                     in Program Files or HKLM. Has a timeout: if Windows waits for consent it cannot display in a headless
                     session, the process is stopped and recorded as INFO (not counted as passed).
    user-install     install per-user by default: must exit 0 under %LOCALAPPDATA%\Programs\Tesela, with its Start menu
                     entry and HKCU state, without touching Program Files or HKLM.
    user-upgrade     upgrade the per-user install with another MSI (different ProductCode, same version). Used when ANOTHER
                     account installs per-machine in between (M2): the current user's install wins and must be upgradeable.
    user-uninstall   uninstall without leftovers.

.PARAMETER Phase
  One of the phases above.

.PARAMETER Shared
  Directory shared with the administrator (the user can write to it).

.PARAMETER Msi
  The MSI for this phase (not needed for user-uninstall).

.PARAMETER ResultFile
  Where result lines are written.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('machine-attempt', 'user-install', 'user-upgrade', 'user-uninstall')][string]$Phase,
    [Parameter(Mandatory)][string]$Shared,
    [string]$Msi,
    [Parameter(Mandatory)][string]$ResultFile
)

$ErrorActionPreference = 'Stop'
# With Start-Process -Credential, the process does NOT inherit runner environment variables (GITHUB_ACTIONS...), so the
# guard is a marker that only msi-install.tests.ps1 creates in the shared directory after verifying a GitHub-hosted runner.
if ($Shared -ne 'C:\TeselaCiStd' -or -not (Test-Path -LiteralPath (Join-Path $Shared '.tesela-ci-marker'))) {
    throw 'msi-install-standard.tests.ps1 runs only from msi-install.tests.ps1 on a GitHub Actions runner'
}
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($isAdmin) { throw 'this test must run as a STANDARD USER, but this process is an administrator' }

$UpgradeCode = '{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}'
$ExpectedFiles = @('tesela.exe', 'tesela-scan.exe', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md')
function Out-Result([string]$Kind, [string]$Text, [string]$Detail = '') {
    $line = if ($Detail) { "$Kind|$Text|$Detail" } else { "$Kind|$Text" }
    Add-Content -LiteralPath $ResultFile -Value $line -Encoding ASCII
}
# Process environment diagnostics (no personal data: this is a disposable user): with Start-Process -Credential, the
# profile and user variables may be missing, so record whether that happened.
Out-Result 'INFO' ("environment: user=" + [Environment]::UserName + " USERPROFILE=[" + $env:USERPROFILE + "] LOCALAPPDATA=[" + $env:LOCALAPPDATA + "] APPDATA=[" + $env:APPDATA + "] HKCU=[" + [Microsoft.Win32.Registry]::CurrentUser.Name + "] GetFolderPath(LocalApplicationData)=[" + [Environment]::GetFolderPath('LocalApplicationData') + "]")
function Get-UserFolder([string]$Special, [string]$Relative) {
    # The directory returned by Windows, or, if no profile is loaded (returns an empty string), the same path under the account's default profile.
    $f = [Environment]::GetFolderPath($Special)
    if ($f) { return $f }
    $root = if ($env:USERPROFILE) { $env:USERPROFILE } else { Join-Path 'C:\Users' ([Environment]::UserName) }
    return (Join-Path $root $Relative)
}
$localAppData = Get-UserFolder 'LocalApplicationData' 'AppData\Local'
$userDefault = Join-Path $localAppData 'Programs\Tesela'
$machineDefault = Join-Path $env:ProgramFiles 'Tesela'
$startMenu = Join-Path (Get-UserFolder 'Programs' 'AppData\Roaming\Microsoft\Windows\Start Menu\Programs') 'Tesela.lnk'

function Check([string]$Text, [bool]$Condition, [string]$Detail = '') {
    if ($Condition) { Out-Result 'ok' $Text } else { Out-Result 'FAIL' $Text $Detail }
}
$installer = New-Object -ComObject WindowsInstaller.Installer
function Get-Products {
    $list = [System.__ComObject].InvokeMember('RelatedProducts', 'GetProperty', $null, $installer, @([string]$UpgradeCode))
    $res = New-Object System.Collections.Generic.List[object]
    foreach ($pc in $list) {
        $code = [string]$pc
        $ver = [string][System.__ComObject].InvokeMember('ProductInfo', 'GetProperty', $null, $installer, @($code, 'VersionString'))
        $res.Add([pscustomobject]@{ Code = $code; Version = $ver })
    }
    return $res.ToArray()
}
function Test-Files([string]$Dir) {
    foreach ($f in $ExpectedFiles) { if (-not (Test-Path -LiteralPath (Join-Path $Dir $f) -PathType Leaf)) { return $false } }
    return $true
}
function Get-RegValue([string]$Key, [string]$Name) {
    if (-not (Test-Path -LiteralPath $Key)) { return $null }
    return (Get-ItemProperty -LiteralPath $Key -ErrorAction SilentlyContinue).$Name
}
function Invoke-Msi([string[]]$MsiArgs, [string]$Name, [int]$TimeoutSeconds = 600) {
    $log = Join-Path $Shared ("std-$Phase-$Name.log")
    $argText = ($MsiArgs -join ' ') + " /qn /norestart /l*v `"$log`""
    Out-Result 'INFO' "msiexec $($MsiArgs -join ' ')"
    $p = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\msiexec.exe') -ArgumentList $argText -PassThru
    if (-not $p.WaitForExit($TimeoutSeconds * 1000)) {
        try { $p.Kill() } catch { }
        return [pscustomobject]@{ Code = $null; TimedOut = $true; Log = $log }
    }
    return [pscustomobject]@{ Code = $p.ExitCode; TimedOut = $false; Log = $log }
}

Out-Result 'INFO' "phase $Phase as standard user ($([Environment]::UserName); administrator: $isAdmin; LocalAppData $localAppData)"
try {
switch ($Phase) {
    'machine-attempt' {
        # No UI: with credentials no one can enter, a per-machine install must fail rather than install.
        $r = Invoke-Msi @('/i', "`"$Msi`"", 'ALLUSERS=1') 'machine' 120
        if ($r.TimedOut) {
            Out-Result 'INFO' 'ALLUSERS=1 as standard user: Windows waited until the 120 s timeout (inconclusive, not a pass)'
            Out-Result 'FAIL' 'ALLUSERS=1 as standard user did not finish (expected a privilege error, e.g. 1925)' 'timed out'
        } else {
            Out-Result 'INFO' "ALLUSERS=1 as standard user exited with code $($r.Code)"
            Check "ALLUSERS=1 /qn as standard user FAILS for insufficient privileges (code $($r.Code); expected 1925; 1625 would be Windows Server DisableMsi policy, not a package decision)" ($r.Code -ne 0 -and $r.Code -ne 1625) "code $($r.Code): 0 = installed for everyone without administrator rights; 1625 = blocked earlier by system policy"
            foreach ($l in @(Select-String -LiteralPath $r.Log -Pattern 'MSI_LUA|sufficient privileges|Elevation required|credential|1925|dual mode' | Select-Object -First 8)) { Out-Result 'INFO' ('log: ' + $l.Line.Trim()) }
            Check 'MSI engine treated the package as dual mode and requested machine privileges (msiexec log)' (Select-String -LiteralPath $r.Log -Pattern 'sufficient privileges|Elevation required|credentials|1925' -Quiet) 'no log line mentions privileges, elevation, or credentials'
        }
        Check 'nothing in Program Files' (-not (Test-Path -LiteralPath $machineDefault))
        Check 'nothing in HKLM\SOFTWARE\Tesela' ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
        Check 'nothing in the per-user folder (no fallback per-user installation)' (-not (Test-Path -LiteralPath $userDefault))
        Check 'no Tesela product installed' (@(Get-Products).Count -eq 0)
    }
    'user-install' {
        $r = Invoke-Msi @('/i', "`"$Msi`"") 'install' 600
        Check "per-user installation WITHOUT elevation exits 0 (code $($r.Code))" ((-not $r.TimedOut) -and $r.Code -eq 0) "code $($r.Code), timed out: $($r.TimedOut)"
        Check 'all 5 files are under the user %LOCALAPPDATA%\Programs\Tesela' (Test-Files $userDefault)
        Check 'nothing in Program Files' (-not (Test-Path -LiteralPath $machineDefault))
        Check 'nothing in HKLM\SOFTWARE\Tesela' ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
        Check 'HKCU\Software\Tesela\InstallFolder points to the user folder' ((Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder') -ieq ($userDefault + '\'))
        Check 'shortcut exists in the user Start menu' (Test-Path -LiteralPath $startMenu)
        Check 'exactly one product for this user' (@(Get-Products).Count -eq 1)
    }
    'user-upgrade' {
        $before = @(Get-Products)
        Out-Result 'INFO' "products visible before upgrade: $($before.Count)"
        $r = Invoke-Msi @('/i', "`"$Msi`"") 'upgrade' 600
        Check "upgrading the per-user installation WITHOUT elevation while another account installed for everyone exits 0 (code $($r.Code))" ((-not $r.TimedOut) -and $r.Code -eq 0) "code $($r.Code), timed out: $($r.TimedOut); see std-user-upgrade-upgrade.log"
        Check 'all 5 files remain in the per-user folder' (Test-Files $userDefault)
        Check 'HKCU\Software\Tesela\InstallFolder still points to the user folder' ((Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder') -ieq ($userDefault + '\'))
        $after = @(Get-Products)
        Out-Result 'INFO' "products visible after upgrade: $($after.Count)"
    }
    'user-uninstall' {
        $prods = @(Get-Products)
        Check 'exactly one product is available to uninstall for this user' ($prods.Count -eq 1) "found $($prods.Count)"
        if ($prods.Count -eq 1) {
            $r = Invoke-Msi @('/x', $prods[0].Code) 'uninstall' 600
            Check "uninstall WITHOUT elevation exits 0 (code $($r.Code))" ((-not $r.TimedOut) -and $r.Code -eq 0) "code $($r.Code)"
        }
        Check 'the per-user folder no longer exists' (-not (Test-Path -LiteralPath $userDefault))
        Check 'no InstallFolder in the user HKCU' ($null -eq (Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder'))
        Check 'no Tesela key remains in its HKCU' (-not (Test-Path -LiteralPath 'HKCU:\Software\Tesela'))
        Check 'no shortcut in the user Start menu' (-not (Test-Path -LiteralPath $startMenu))
        Check 'no Tesela product for this user' (@(Get-Products).Count -eq 0)
    }
}
} catch {
    Out-Result 'FAIL' "phase $Phase raised an exception" ($_.Exception.Message + ' @' + $_.InvocationInfo.ScriptLineNumber)
}
Out-Result 'INFO' "phase $Phase completed"
exit 0
