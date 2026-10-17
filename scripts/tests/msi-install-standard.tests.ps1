<#
.SYNOPSIS
  Una FASE de la prueba de instalacion del MSI como USUARIO ESTANDAR (sin privilegios, sin elevacion). La lanza
  scripts/tests/msi-install.tests.ps1 en el runner de GitHub Actions, con un usuario local desechable; no se usa a mano.

.DESCRIPTION
  Corre COMO ese usuario (Start-Process -Credential -LoadUserProfile). Se niega a correr si el proceso es administrador o si no esta la marca
  que crea msi-install.tests.ps1 (solo en un runner de GitHub Actions). Hace UNA fase, instala o desinstala de verdad con msiexec /qn (sin interfaz) y escribe el resultado, una linea por
  comprobacion, en -ResultFile (el usuario administrador del runner no ve su HKCU ni su perfil, por eso lo cuenta el propio usuario):
      ok|<texto>
      FAIL|<texto>|<detalle>
      INFO|<texto>
  Fases:
    machine-attempt  `ALLUSERS=1 /qn` como estandar: tiene que FALLAR (no hay quien conceda administrador) y no dejar nada en
                     Program Files ni en HKLM. Con un limite de tiempo: si Windows esperase un consentimiento que no puede mostrar
                     en una sesion sin escritorio, se corta y se anota como INFO (no se da por pasada).
    user-install     instalar por usuario por defecto: tiene que terminar con 0, en %LOCALAPPDATA%\Programs\Tesela, con su menu Inicio y
                     su HKCU, sin tocar Program Files ni HKLM.
    user-upgrade     actualizar la instalacion por usuario con otro MSI (otro ProductCode, misma version). Se usa cuando OTRA cuenta
                     ha instalado para todos entre medias (M2): la suya manda y tiene que poder actualizarla.
    user-uninstall   desinstalar sin restos.

.PARAMETER Phase
  Una de las fases de arriba.

.PARAMETER Shared
  Carpeta compartida con el administrador (el usuario puede escribir en ella).

.PARAMETER Msi
  El MSI de la fase (no hace falta en user-uninstall).

.PARAMETER ResultFile
  Donde se escriben las lineas de resultado.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('machine-attempt', 'user-install', 'user-upgrade', 'user-uninstall')][string]$Phase,
    [Parameter(Mandatory)][string]$Shared,
    [string]$Msi,
    [Parameter(Mandatory)][string]$ResultFile
)

$ErrorActionPreference = 'Stop'
# Con Start-Process -Credential el proceso NO hereda las variables de entorno del runner (GITHUB_ACTIONS...), asi que la guardia es una
# marca que solo crea msi-install.tests.ps1 (que SI comprueba que esta en un runner hospedado de GitHub) en la carpeta compartida.
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
# Diagnostico del entorno del proceso (sin datos personales: es un usuario desechable): con Start-Process -Credential el perfil y las
# variables de usuario pueden faltar, y entonces hay que saberlo.
Out-Result 'INFO' ("entorno: usuario=" + [Environment]::UserName + " USERPROFILE=[" + $env:USERPROFILE + "] LOCALAPPDATA=[" + $env:LOCALAPPDATA + "] APPDATA=[" + $env:APPDATA + "] HKCU=[" + [Microsoft.Win32.Registry]::CurrentUser.Name + "] GetFolderPath(LocalApplicationData)=[" + [Environment]::GetFolderPath('LocalApplicationData') + "]")
function Get-UserFolder([string]$Special, [string]$Relative) {
    # La carpeta que da Windows y, si no hay perfil cargado (devuelve una cadena vacia), la misma desde el perfil por defecto de la cuenta.
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
        # Nada de interfaz: con credenciales que nadie puede teclear, "para todos" tiene que fallar, no instalar.
        $r = Invoke-Msi @('/i', "`"$Msi`"", 'ALLUSERS=1') 'machine' 120
        if ($r.TimedOut) {
            Out-Result 'INFO' 'ALLUSERS=1 como estandar: Windows se quedo esperando y se corto a los 120 s (no se da por pasada: queda como no concluyente)'
            Out-Result 'FAIL' 'ALLUSERS=1 como usuario estandar no termino (se esperaba un error de privilegios, p. ej. 1925)' 'tiempo agotado'
        } else {
            Out-Result 'INFO' "ALLUSERS=1 como estandar termino con el codigo $($r.Code)"
            Check "ALLUSERS=1 /qn como usuario estandar FALLA (codigo $($r.Code); se espera 1925, error de privilegios)" ($r.Code -ne 0) 'termino con 0: instalo para todos sin ser administrador'
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
        Check 'sin acceso directo en su menu Inicio' (-not (Test-Path -LiteralPath $startMenu))
        Check 'ningun producto Tesela para este usuario' (@(Get-Products).Count -eq 0)
    }
}
} catch {
    Out-Result 'FAIL' "la fase $Phase fallo con una excepcion" ($_.Exception.Message + ' @' + $_.InvocationInfo.ScriptLineNumber)
}
Out-Result 'INFO' "fase $Phase terminada"
exit 0
