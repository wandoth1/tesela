<#
.SYNOPSIS
  One PHASE of the MSI installation test as a STANDARD USER (no privileges or elevation). Launched by
  scripts/tests/msi-install.tests.ps1 on a GitHub Actions runner with a disposable local user; do not run manually.

.DESCRIPTION
  Runs AS that user (Start-Process -Credential -LoadUserProfile). Refuses to run if the process is an administrator or
  the marker created by msi-install.tests.ps1 is missing (only created on a GitHub Actions runner). Runs ONE phase,
  performs a real install or uninstall with msiexec /qn (no UI), and writes one result line per check to -ResultFile
  (the runner administrator cannot see this user's HKCU or profile, so the user reports its own state):
      ok|<texto>
      FAIL|<texto>|<detalle>
      INFO|<texto>
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
    throw 'msi-install-standard.tests.ps1 solo corre dentro de msi-install.tests.ps1, en un runner de GitHub Actions'
}
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($isAdmin) { throw 'esta prueba tiene que correr como usuario ESTANDAR, y este proceso es administrador' }

$UpgradeCode = '{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}'
$ExpectedFiles = @('tesela.exe', 'tesela-scan.exe', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md')
function Out-Result([string]$Kind, [string]$Text, [string]$Detail = '') {
    $line = if ($Detail) { "$Kind|$Text|$Detail" } else { "$Kind|$Text" }
    Add-Content -LiteralPath $ResultFile -Value $line -Encoding ASCII
}
# Process environment diagnostics (no personal data: this is a disposable user): with Start-Process -Credential, the
# profile and user variables may be missing, so record whether that happened.
Out-Result 'INFO' ("entorno: usuario=" + [Environment]::UserName + " USERPROFILE=[" + $env:USERPROFILE + "] LOCALAPPDATA=[" + $env:LOCALAPPDATA + "] APPDATA=[" + $env:APPDATA + "] HKCU=[" + [Microsoft.Win32.Registry]::CurrentUser.Name + "] GetFolderPath(LocalApplicationData)=[" + [Environment]::GetFolderPath('LocalApplicationData') + "]")
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

Out-Result 'INFO' "fase $Phase como usuario estandar ($([Environment]::UserName); administrador: $isAdmin; LocalAppData $localAppData)"
try {
switch ($Phase) {
    'machine-attempt' {
        # No UI: with credentials no one can enter, a per-machine install must fail rather than install.
        $r = Invoke-Msi @('/i', "`"$Msi`"", 'ALLUSERS=1') 'machine' 120
        if ($r.TimedOut) {
            Out-Result 'INFO' 'ALLUSERS=1 como estandar: Windows se quedo esperando y se corto a los 120 s (no se da por pasada: queda como no concluyente)'
            Out-Result 'FAIL' 'ALLUSERS=1 como usuario estandar no termino (se esperaba un error de privilegios, p. ej. 1925)' 'tiempo agotado'
        } else {
            Out-Result 'INFO' "ALLUSERS=1 como estandar termino con el codigo $($r.Code)"
            Check "ALLUSERS=1 /qn como usuario estandar FALLA por falta de privilegios (codigo $($r.Code); se espera 1925; 1625 seria la directiva DisableMsi del Windows Server, no una decision del paquete)" ($r.Code -ne 0 -and $r.Code -ne 1625) "codigo $($r.Code): 0 = instalo para todos sin ser administrador; 1625 = la directiva del sistema lo impidio antes"
            foreach ($l in @(Select-String -LiteralPath $r.Log -Pattern 'MSI_LUA|sufficient privileges|Elevation required|credential|1925|dual mode' | Select-Object -First 8)) { Out-Result 'INFO' ('registro: ' + $l.Line.Trim()) }
            Check 'el motor trato el paquete como de doble modo y pidio privilegios para todos (registro de msiexec)' (Select-String -LiteralPath $r.Log -Pattern 'sufficient privileges|Elevation required|credentials|1925' -Quiet) 'ninguna linea del registro habla de privilegios, elevacion o credenciales'
        }
        Check 'nada en Program Files' (-not (Test-Path -LiteralPath $machineDefault))
        Check 'nada en HKLM\SOFTWARE\Tesela' ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
        Check 'nada en la carpeta por usuario (no se instalo por usuario "de rebote")' (-not (Test-Path -LiteralPath $userDefault))
        Check 'ningun producto Tesela instalado' (@(Get-Products).Count -eq 0)
    }
    'user-install' {
        $r = Invoke-Msi @('/i', "`"$Msi`"") 'install' 600
        Check "instalar por usuario SIN elevacion termina con 0 (codigo $($r.Code))" ((-not $r.TimedOut) -and $r.Code -eq 0) "codigo $($r.Code), tiempo agotado: $($r.TimedOut)"
        Check 'los 5 archivos estan en su %LOCALAPPDATA%\Programs\Tesela' (Test-Files $userDefault)
        Check 'nada en Program Files' (-not (Test-Path -LiteralPath $machineDefault))
        Check 'nada en HKLM\SOFTWARE\Tesela' ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
        Check 'HKCU\Software\Tesela\InstallFolder es su carpeta' ((Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder') -ieq ($userDefault + '\'))
        Check 'acceso directo en su menu Inicio' (Test-Path -LiteralPath $startMenu)
        Check 'un solo producto para este usuario' (@(Get-Products).Count -eq 1)
    }
    'user-upgrade' {
        $before = @(Get-Products)
        Out-Result 'INFO' "productos visibles antes de actualizar: $($before.Count)"
        $r = Invoke-Msi @('/i', "`"$Msi`"") 'upgrade' 600
        Check "actualizar su instalacion por usuario SIN elevacion y con otra cuenta que instalo para todos termina con 0 (codigo $($r.Code))" ((-not $r.TimedOut) -and $r.Code -eq 0) "codigo $($r.Code), tiempo agotado: $($r.TimedOut); mira el registro std-user-upgrade-upgrade.log"
        Check 'los 5 archivos siguen en su carpeta por usuario' (Test-Files $userDefault)
        Check 'HKCU\Software\Tesela\InstallFolder sigue siendo su carpeta' ((Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder') -ieq ($userDefault + '\'))
        $after = @(Get-Products)
        Out-Result 'INFO' "productos visibles despues de actualizar: $($after.Count)"
    }
    'user-uninstall' {
        $prods = @(Get-Products)
        Check 'hay exactamente un producto que desinstalar para este usuario' ($prods.Count -eq 1) "hay $($prods.Count)"
        if ($prods.Count -eq 1) {
            $r = Invoke-Msi @('/x', $prods[0].Code) 'uninstall' 600
            Check "desinstalar SIN elevacion termina con 0 (codigo $($r.Code))" ((-not $r.TimedOut) -and $r.Code -eq 0) "codigo $($r.Code)"
        }
        Check 'su carpeta ya no existe' (-not (Test-Path -LiteralPath $userDefault))
        Check 'sin InstallFolder en su HKCU' ($null -eq (Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder'))
        Check 'no Tesela key remains in its HKCU' (-not (Test-Path -LiteralPath 'HKCU:\Software\Tesela'))
        Check 'sin acceso directo en su menu Inicio' (-not (Test-Path -LiteralPath $startMenu))
        Check 'ningun producto Tesela para este usuario' (@(Get-Products).Count -eq 0)
    }
}
} catch {
    Out-Result 'FAIL' "la fase $Phase fallo con una excepcion" ($_.Exception.Message + ' @' + $_.InvocationInfo.ScriptLineNumber)
}
Out-Result 'INFO' "fase $Phase terminada"
exit 0
