<#
.SYNOPSIS
  Prueba de INSTALACION del MSI de Tesela (instalar, actualizar, reparar, desinstalar, para todos, cambio de ambito...) sin
  interfaz (/qn). SOLO corre en un runner de GitHub Actions: instala y desinstala de verdad.

.DESCRIPTION
  El job `msi` de .github/workflows/ci.yml la lanza despues de construir el MSI y de bajar la v0.1.4 publicada (verificada con
  `gh attestation verify`). El runner es un administrador con la sesion elevada y desechable. En una maquina de desarrollo se
  NIEGA a correr (comprueba GITHUB_ACTIONS y RUNNER_ENVIRONMENT=github-hosted): la prueba de instalacion en local se hace a
  mano, avisando al usuario, nunca desde un agente.

  Escenarios (cada uno deja la maquina como estaba y lo comprueba):
    1. Por usuario, sin interfaz, por defecto: archivos en %LOCALAPPDATA%\Programs\Tesela, acceso directo del menu Inicio del
       usuario, NADA en HKLM ni en Program Files, sin acceso directo en el escritorio, `--version`, un solo producto y una sola
       entrada en Aplicaciones instaladas; reparar (restaura un archivo borrado); desinstalar sin restos.
    2. Actualizar desde la v0.1.4 publicada (por usuario), carpeta por defecto: un solo producto tras actualizar, y la
       bajada (instalar la v0.1.4 sobre la nueva) sigue BLOQUEADA si la version nueva es mayor (si es igual, se anota y no se
       aplica).
    3. Actualizar desde la v0.1.4 instalada en una carpeta PERSONALIZADA: la actualizacion se queda en ella y desinstalar sin
       repetir INSTALLFOLDER la quita.
    4. Para todos (ALLUSERS=1): Program Files\Tesela, HKLM, menu Inicio comun, `--version`, un solo producto y desinstalar
       sin restos; carpeta personalizada para todos (INSTALLFOLDER llega a la fase del servidor) y actualizacion en su sitio.
    5. Cambio de ambito BLOQUEADO en las dos direcciones, con su mensaje, sin tocar la instalacion existente.
    6. DESKTOPSHORTCUT: por defecto no hay acceso directo; =1 lo crea (por usuario y para todos), se RECUERDA al actualizar sin
       repetirlo, =0 lo quita y desinstalar lo elimina.
    7. Carpeta protegida (Archivos de programa) en una instalacion por usuario: bloqueada con su mensaje aunque el runner sea
       administrador; para todos, permitida.
    8. Una clave de registro SIN producto (restos, sin tesela.exe) en HKCU o en HKLM no bloquea nada.
    9. Usuario ESTANDAR desechable (contrasena aleatoria que no se imprime, borrado al final aunque falle), con
       scripts/tests/msi-install-standard.tests.ps1 lanzado COMO ese usuario, sin elevar: `ALLUSERS=1 /qn` tiene que fallar y no dejar
       nada; instalar por usuario tiene que terminar con 0 en su perfil sin tocar Program Files ni HKLM; y, con otra cuenta (el
       administrador) que instala para todos entre medias, su instalacion por usuario se puede actualizar (la suya gana) y desinstalar.
    Una prueba omitida NO cuenta como pasada: se lista aparte, hay un minimo de escenarios y de comprobaciones, y con una version
    posterior a la v0.1.4 publicada no puede quedar ninguna omitida. El resumen del job vuelca las lineas MSI_LUA de los registros de
    msiexec (como decidio el motor el ambito y la elevacion).

  Lo que NO prueba (no se puede sin una persona): la interfaz (ventanas), la peticion de administrador (UAC) al pulsar Instalar en
  "para todos" (el runner no tiene escritorio interactivo), y las propiedades seguras con un usuario estandar que da las credenciales de
  OTRO administrador.

.PARAMETER NewMsi
  El MSI recien construido por scripts/build-msi.ps1.

.PARAMETER OldMsi
  El MSI de la v0.1.4 publicada (descargado y verificado antes).

.PARAMETER WorkDir
  Carpeta de trabajo para registros y carpetas personalizadas (por defecto, una nueva en RUNNER_TEMP). No puede tener espacios.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$NewMsi,
    [Parameter(Mandatory)][string]$OldMsi,
    [string]$WorkDir
)

$ErrorActionPreference = 'Stop'

# --- Seguridad: solo en un runner de GitHub Actions, desechable y administrador -----------------------------------------------
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'msi-install.tests.ps1 instala y desinstala de verdad: solo corre en un runner hospedado de GitHub Actions (GITHUB_ACTIONS=true y RUNNER_ENVIRONMENT=github-hosted).'
}
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { throw 'hace falta un administrador con la sesion elevada (el runner lo es)' }

$NewMsi = [System.IO.Path]::GetFullPath($NewMsi)
$OldMsi = [System.IO.Path]::GetFullPath($OldMsi)
foreach ($f in $NewMsi, $OldMsi) { if (-not (Test-Path -LiteralPath $f -PathType Leaf)) { throw "no existe $f" } }
if (-not $WorkDir) { $WorkDir = Join-Path $env:RUNNER_TEMP ('tesela-install-tests-' + [guid]::NewGuid().ToString('N').Substring(0, 8)) }
$WorkDir = [System.IO.Path]::GetFullPath($WorkDir)
if ($WorkDir -match '\s') { throw "WorkDir no puede tener espacios (INSTALLFOLDER va sin comillas): $WorkDir" }
$tempRoot = [System.IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\')
if (-not $WorkDir.StartsWith($tempRoot + '\', [System.StringComparison]::OrdinalIgnoreCase)) { throw "WorkDir tiene que estar dentro de RUNNER_TEMP ($tempRoot)" }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

$UpgradeCode = '{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}'
$ExpectedFiles = @('tesela.exe', 'tesela-scan.exe', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md')

# --- Comprobaciones ------------------------------------------------------------------------------------------------------------
$passed = New-Object System.Collections.Generic.List[string]
$failed = New-Object System.Collections.Generic.List[string]
# Lo que NO se ejecuto (no cuenta como pasado): se imprime aparte y el job puede exigir que no haya ninguna.
$skipped = New-Object System.Collections.Generic.List[string]
$script:stepsRun = 0
function Check([string]$Case, [bool]$Condition, [string]$Detail = '') {
    if ($Condition) { $script:passed.Add($Case); Write-Host "  ok   $Case" }
    else { $script:failed.Add($(if ($Detail) { "$Case ($Detail)" } else { $Case })); Write-Host "  FALLA $Case $Detail" -ForegroundColor Red }
}
function Step([string]$Text) { $script:stepsRun++; Write-Host ''; Write-Host "== $Text" }

# --- Utilidades ---------------------------------------------------------------------------------------------------------------
$script:logIndex = 0
function Invoke-Msi {
    # Ejecuta msiexec sin interfaz y devuelve @{ Code; Log; Text }. El registro se lee SIN los NUL (puede ser UTF-16) para poder
    # buscar texto ASCII en el.
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
    # Productos con el UpgradeCode de Tesela que ve Windows Installer (por usuario del usuario actual y para todos).
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
    # Entradas "Tesela" de Aplicaciones instaladas (HKCU y HKLM, vistas de 64 y 32 bits).
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
    # Copia del MSI con OTRO ProductCode y otro codigo de paquete (el contador de revision del Summary Information): lo mismo que
    # una compilacion posterior de la misma version. Instalarlo sobre la instalacion del MSI original es una ACTUALIZACION (el
    # mismo UpgradeCode, otro producto); con el mismo archivo seria solo mantenimiento del mismo producto. Solo en la copia.
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
    # Desinstala el UNICO producto Tesela instalado, por su codigo de producto (valga el MSI que valga el que lo instalo).
    $prods = @(Get-Products)
    if ($prods.Count -ne 1) { throw "se esperaba un unico producto instalado para desinstalar; hay $($prods.Count)" }
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
    # Ningun enlace (junction, enlace simbolico) en el arbol: solo entonces se borra de forma recursiva.
    if (-not (Test-Path -LiteralPath $Dir)) { return $true }
    $items = @(Get-Item -LiteralPath $Dir -Force) + @(Get-ChildItem -LiteralPath $Dir -Recurse -Force -ErrorAction SilentlyContinue)
    return @($items | Where-Object { $_.Attributes -band [System.IO.FileAttributes]::ReparsePoint }).Count -eq 0
}
function Remove-TestDir([string]$Dir) {
    # Solo carpetas dentro de WorkDir o la carpeta de la prueba para todos, y solo si no hay enlaces.
    if (-not (Test-Path -LiteralPath $Dir)) { return }
    $full = [System.IO.Path]::GetFullPath($Dir).TrimEnd('\')
    $ok = $full.StartsWith($WorkDir + '\', [System.StringComparison]::OrdinalIgnoreCase) -or $full -ieq $machineCustomRoot -or $full -ieq $stdShared
    if (-not $ok) { throw "se niega a borrar fuera de la zona de pruebas: $full" }
    if (-not (Test-ReparseFree $full)) { throw "se niega a borrar ${full}: tiene enlaces (reparse points)" }
    Remove-Item -LiteralPath $full -Recurse -Force
}
function Assert-Clean([string]$When) {
    # La maquina no tiene Tesela instalada (ni productos, ni entradas, ni valores de registro, ni carpetas).
    $prods = @(Get-Products)
    Check "$When`: ningun producto Tesela instalado" ($prods.Count -eq 0) "hay $($prods.Count)"
    Check "$When`: ninguna entrada de Tesela en Aplicaciones instaladas" ((Get-UninstallEntries) -eq 0)
    Check "$When`: sin carpeta en Program Files" (-not (Test-Path -LiteralPath $machineDefault))
    Check "$When`: sin carpeta por usuario" (-not (Test-Path -LiteralPath $userDefault))
    Check "$When`: sin InstallFolder en HKCU" ($null -eq (Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder'))
    Check "$When`: sin InstallFolder en HKLM" ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
    Check "$When`: sin acceso directo en el menu Inicio del usuario" (-not (Test-Path -LiteralPath $userStartMenu))
    Check "$When`: sin acceso directo en el menu Inicio comun" (-not (Test-Path -LiteralPath $commonStartMenu))
    Check "$When`: sin acceso directo en el escritorio del usuario" (-not (Test-Path -LiteralPath $userDesktop))
    Check "$When`: sin acceso directo en el escritorio comun" (-not (Test-Path -LiteralPath $commonDesktop))
}
function Reset-Machine {
    # Por si un escenario falla a medias: desinstala lo que haya de Tesela (el runner es desechable y solo hay lo nuestro).
    foreach ($p in @(Get-Products)) { [void](Invoke-Msi -MsiArgs @('/x', $p.Code) -Name 'reset') }
}
function Test-TeselaVersion([string]$Exe, [string]$Label) {
    # tesela.exe es de interfaz grafica: --version sale por la salida de ERRORES, sin abrir la ventana.
    $err = Join-Path $WorkDir 'version.txt'
    $p = Start-Process -FilePath $Exe -ArgumentList '--version' -Wait -PassThru -RedirectStandardError $err
    $out = if (Test-Path -LiteralPath $err) { Get-Content -LiteralPath $err -Raw } else { '' }
    Check "$Label`: tesela.exe --version responde" ($p.ExitCode -eq 0 -and $out -match '^tesela \d+\.\d+\.\d+') "codigo $($p.ExitCode), salida '$out'"
}
function Show-LogTail([object]$R) {
    Write-Host "  --- ultimas lineas de $($R.Log) ---"
    ($R.Text -split "`n") | Select-Object -Last 25 | ForEach-Object { Write-Host "  | $_" }
}

# --- Usuario ESTANDAR desechable (escenario 9) --------------------------------------------------------------------------------------
$stdShared = 'C:\TeselaCiStd'
$stdName = 'teselacistd'
$script:stdUser = $null
function New-RandomPassword {
    # Contrasena aleatoria generada aqui, que nunca se imprime (y se enmascara en el registro del job por si acaso).
    $b = New-Object byte[] 24
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $rng.GetBytes($b)
    $rng.Dispose()
    return 'Aa1!' + [Convert]::ToBase64String($b).Replace('=', 'x')
}
function New-StandardUser {
    if (Get-LocalUser -Name $stdName -ErrorAction SilentlyContinue) { throw "ya existe el usuario local ${stdName}: se aborta sin tocarlo" }
    $pw = New-RandomPassword
    Write-Host "::add-mask::$pw"
    $sec = ConvertTo-SecureString $pw -AsPlainText -Force
    New-LocalUser -Name $stdName -Password $sec -FullName 'Tesela CI standard user' -Description 'Disposable user of the Tesela CI install tests' -AccountNeverExpires -PasswordNeverExpires | Out-Null
    # Solo el grupo Usuarios (S-1-5-32-545): estandar, sin administrador.
    Add-LocalGroupMember -SID 'S-1-5-32-545' -Member $stdName
    $script:stdUser = [pscustomobject]@{ Name = $stdName; Cred = (New-Object System.Management.Automation.PSCredential(".\$stdName", $sec)) }
    $isAdminMember = @(Get-LocalGroupMember -SID 'S-1-5-32-544' -ErrorAction SilentlyContinue | Where-Object { $_.Name -like "*\$stdName" }).Count -gt 0
    Check 'el usuario desechable es ESTANDAR (no esta en Administradores)' (-not $isAdminMember)
}
function Remove-StandardUser {
    # Se borra el usuario aunque la prueba falle (la carpeta de su perfil se deja: tiene enlaces propios de Windows y el runner es
    # desechable).
    if (Get-LocalUser -Name $stdName -ErrorAction SilentlyContinue) { Remove-LocalUser -Name $stdName }
    $script:stdUser = $null
}
function Invoke-StdPhase([string]$Phase, [string]$MsiPath) {
    # Lanza msi-install-standard.tests.ps1 COMO el usuario estandar (sin elevar) y vuelca sus lineas de resultado en las comprobaciones.
    $result = Join-Path $stdShared "result-$Phase.txt"
    if (Test-Path -LiteralPath $result) { Remove-Item -LiteralPath $result -Force }
    $stdArgs = @('-NoProfile', '-File', "`"$stdScriptCopy`"", '-Phase', $Phase, '-Shared', "`"$stdShared`"", '-ResultFile', "`"$result`"")
    if ($MsiPath) { $stdArgs += @('-Msi', "`"$MsiPath`"") }
    $pwshExe = (Get-Process -Id $PID).Path
    Write-Host "  (como $stdName) pwsh -File msi-install-standard.tests.ps1 -Phase $Phase"
    try {
        $outFile = Join-Path $stdShared "std-$Phase.out.txt"
        $errFile = Join-Path $stdShared "std-$Phase.err.txt"
        $p = Start-Process -FilePath $pwshExe -ArgumentList ($stdArgs -join ' ') -Credential $script:stdUser.Cred -LoadUserProfile -WorkingDirectory $stdShared -UseNewEnvironment -PassThru -RedirectStandardOutput $outFile -RedirectStandardError $errFile
    } catch {
        Check "se puede lanzar un proceso como el usuario estandar (fase $Phase)" $false "Start-Process -Credential fallo: $($_.Exception.Message)"
        return
    }
    Wait-Process -Id $p.Id -Timeout 900 -ErrorAction SilentlyContinue
    if (Get-Process -Id $p.Id -ErrorAction SilentlyContinue) {
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        Check "la fase $Phase del usuario estandar termino a tiempo" $false 'tiempo agotado (15 min)'
        return
    }
    foreach ($f in $outFile, $errFile) {
        if ((Test-Path -LiteralPath $f) -and (Get-Item -LiteralPath $f).Length -gt 0) { Write-Host "  [estandar] contenido de $(Split-Path -Leaf $f):"; Get-Content -LiteralPath $f -TotalCount 40 | ForEach-Object { Write-Host "    | $_" } }
    }
    if (-not (Test-Path -LiteralPath $result)) {
        Check "la fase $Phase del usuario estandar produjo resultados" $false 'no hay archivo de resultados: el proceso no llego a escribirlos (perfil, permisos o error al arrancar)'
        return
    }
    $lines = @(Get-Content -LiteralPath $result)
    $count = 0
    foreach ($l in $lines) {
        $parts = $l -split '\|', 3
        switch ($parts[0]) {
            'ok' { $count++; Check "(usuario estandar) $($parts[1])" $true }
            'FAIL' { $count++; Check "(usuario estandar) $($parts[1])" $false $(if ($parts.Count -gt 2) { $parts[2] } else { '' }) }
            'INFO' { Write-Host "  [estandar] $($parts[1])" }
        }
    }
    Check "la fase $Phase del usuario estandar hizo comprobaciones" ($count -gt 0)
    if (@($lines | Where-Object { $_ -like 'FAIL|*' }).Count -gt 0) { Show-StdLogs $Phase }
}
function Show-StdLogs([string]$Phase) {
    # Si una fase falla: lo mas relevante de los registros de msiexec del usuario estandar (errores, directivas, ambito, elevacion).
    foreach ($log in Get-ChildItem -LiteralPath $stdShared -Filter "std-$Phase-*.log" -ErrorAction SilentlyContinue) {
        $text = [System.Text.Encoding]::Latin1.GetString(@([System.IO.File]::ReadAllBytes($log.FullName) | Where-Object { $_ -ne 0 }))
        Write-Host "  [estandar] $($log.Name): lineas relevantes"
        ($text -split "`r?`n") | Where-Object { $_ -match 'Error|error|1625|1925|policy|Policy|DisableMSI|DisableUserInstalls|MSI_LUA|dual mode|Return value|Elevat|privilege|not allowed|forbidden|rejected|Product:|Note:' } | Select-Object -First 40 | ForEach-Object { Write-Host "    | $($_.Trim())" }
    }
}
function Show-PolicyDiagnostics {
    # Directivas del runner que pueden decidir si un usuario estandar puede instalar (solo lectura): sale en el registro del job.
    foreach ($k in 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Installer', 'HKCU:\SOFTWARE\Policies\Microsoft\Windows\Installer', 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Safer\CodeIdentifiers', 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System', 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\SrpV2') {
        if (Test-Path -LiteralPath $k) {
            Write-Host "  [directiva] $k"
            (Get-ItemProperty -LiteralPath $k | Get-Member -MemberType NoteProperty | Where-Object { $_.Name -notlike 'PS*' } | ForEach-Object { "$($_.Name)=$((Get-ItemProperty -LiteralPath $k).($_.Name))" }) -join '; ' | ForEach-Object { Write-Host "    $_" }
        } else { Write-Host "  [directiva] $k : no existe" }
    }
    $svc = Get-Service -Name seclogon -ErrorAction SilentlyContinue
    Write-Host "  [servicio] seclogon (inicio de sesion secundario): $($svc.Status) / $($svc.StartType)"
}
function Get-LuaEvidence {
    # Lineas del registro de msiexec que dicen como decidio el motor el ambito y la elevacion (MSI_LUA, dual mode, 1925...).
    $out = New-Object System.Collections.Generic.List[string]
    foreach ($dir in $WorkDir, $stdShared) {
        if (-not (Test-Path -LiteralPath $dir)) { continue }
        foreach ($log in Get-ChildItem -LiteralPath $dir -Filter '*.log' -ErrorAction SilentlyContinue) {
            $text = [System.Text.Encoding]::Latin1.GetString(@([System.IO.File]::ReadAllBytes($log.FullName) | Where-Object { $_ -ne 0 }))
            foreach ($line in ($text -split "`r?`n")) {
                if ($line -match 'MSI_LUA|dual mode|Elevation required|sufficient privileges|MSIINSTALLPERUSER') { $out.Add("$($log.Name): $($line.Trim())") }
            }
        }
    }
    return @($out | Select-Object -Unique | Select-Object -First 60)
}

# --- Rutas ---------------------------------------------------------------------------------------------------------------------
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
Write-Host "MSI nuevo $newVersion, MSI anterior $oldVersion, trabajo en $WorkDir"

try {
    Step '0. Punto de partida: la maquina no tiene Tesela'
    Assert-Clean 'inicio'
    if ($failed.Count -gt 0) { throw 'el runner ya tenia Tesela instalada: se aborta sin tocar nada' }
    Check 'el MSI anterior es la v0.1.4' ($oldVersion -eq '0.1.4') "es $oldVersion"

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '1. Por usuario, sin interfaz, por defecto'
    $r = Install-Msi $NewMsi @() 'user-install'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'instalar (/qn) termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'los 5 archivos estan en %LOCALAPPDATA%\Programs\Tesela' (Test-Files $userDefault)
    Check 'nada en Program Files' (-not (Test-Path -LiteralPath $machineDefault))
    Check 'HKCU\Software\Tesela\InstallFolder es la carpeta por usuario' ((Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder') -ieq ($userDefault + '\'))
    Check 'nada en HKLM\SOFTWARE\Tesela' ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
    Check 'acceso directo en el menu Inicio del usuario' (Test-Path -LiteralPath $userStartMenu)
    Check 'ningun acceso directo en el menu Inicio comun' (-not (Test-Path -LiteralPath $commonStartMenu))
    Check 'SIN acceso directo en el escritorio (desmarcado por defecto)' (-not (Test-Path -LiteralPath $userDesktop) -and -not (Test-Path -LiteralPath $commonDesktop))
    Check 'un solo producto, con la version del MSI' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Check 'una sola entrada en Aplicaciones instaladas' ((Get-UninstallEntries) -eq 1)
    Test-TeselaVersion (Join-Path $userDefault 'tesela.exe') 'por usuario'
    # Reparar: restaura un archivo borrado.
    Remove-Item -LiteralPath (Join-Path $userDefault 'tesela-scan.exe') -Force
    $r = Repair-Msi $NewMsi 'user-repair'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'reparar (/fa) termina con 0 y restaura el archivo borrado' ($r.Code -eq 0 -and (Test-Files $userDefault)) "codigo $($r.Code)"
    $r = Uninstall-Msi $NewMsi 'user-uninstall'
    Check 'desinstalar termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras desinstalar por usuario'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '2. Actualizar desde la v0.1.4 publicada (por usuario, carpeta por defecto)'
    $r = Install-Msi $OldMsi @() 'old-install'
    Check 'instalar la v0.1.4 termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la v0.1.4 esta en la carpeta por defecto y es el unico producto' ((Test-Files $userDefault) -and (@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq '0.1.4')
    $r = Install-Msi $NewMsi @() 'upgrade-default'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'actualizar a la version nueva termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'tras actualizar queda UN solo producto, con la version nueva' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Check 'tras actualizar queda UNA sola entrada en Aplicaciones instaladas' ((Get-UninstallEntries) -eq 1)
    Check 'la actualizacion se queda en la carpeta por defecto, con los 5 archivos' (Test-Files $userDefault)
    Check 'el acceso directo del menu Inicio sigue ahi' (Test-Path -LiteralPath $userStartMenu)
    if ([version]$newVersion -gt [version]$oldVersion) {
        $r = Install-Msi $OldMsi @() 'downgrade'
        Check 'la bajada (instalar la v0.1.4 sobre la nueva) sigue BLOQUEADA (codigo 1603)' ($r.Code -eq 1603) "codigo $($r.Code)"
        Check 'la bajada se bloquea con su mensaje' ($r.Text -match 'newer version of Tesela is already installed') 'no sale el mensaje en el registro'
        Check 'tras la bajada bloqueada sigue instalada la version nueva' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    } else {
        Write-Host "  (bajada NO aplicable: la version nueva ($newVersion) no es mayor que la publicada ($oldVersion); se probara cuando se suba la version)"
        $skipped.Add('bajada: NO ejecutada (la version nueva no es mayor que la publicada); se prueba al subir la version')
    }
    $r = Uninstall-Installed 'upgrade-uninstall'
    Check 'desinstalar tras actualizar termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras desinstalar la actualizacion'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '3. Actualizar desde la v0.1.4 en una carpeta PERSONALIZADA (por usuario)'
    $r = Install-Msi $OldMsi @("INSTALLFOLDER=$userCustom\") 'old-custom'
    Check 'instalar la v0.1.4 en una carpeta personalizada termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la v0.1.4 esta en la carpeta personalizada y no en la de por defecto' ((Test-Files $userCustom) -and -not (Test-Path -LiteralPath $userDefault))
    $r = Install-Msi $NewMsi @() 'upgrade-custom'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'actualizar sin repetir INSTALLFOLDER termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la actualizacion conserva la carpeta personalizada (5 archivos) y no crea la de por defecto' ((Test-Files $userCustom) -and -not (Test-Path -LiteralPath $userDefault))
    Check 'un solo producto, con la version nueva' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Test-TeselaVersion (Join-Path $userCustom 'tesela.exe') 'carpeta personalizada'
    $r = Uninstall-Installed 'custom-uninstall'
    Check 'desinstalar sin repetir INSTALLFOLDER termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la carpeta personalizada se quita entera al desinstalar' (-not (Test-Path -LiteralPath (Join-Path $userCustom 'tesela.exe')) -and -not (Test-Path -LiteralPath $userCustom))
    Assert-Clean 'tras desinstalar la carpeta personalizada'
    Remove-TestDir (Join-Path $WorkDir 'custom-user')

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '4. Para todos (ALLUSERS=1)'
    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'machine-install'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'instalar para todos termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'los 5 archivos estan en Program Files\Tesela' (Test-Files $machineDefault)
    Check 'nada en la carpeta por usuario' (-not (Test-Path -LiteralPath $userDefault))
    Check 'HKLM\SOFTWARE\Tesela\InstallFolder es Program Files\Tesela' ((Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder') -ieq ($machineDefault + '\'))
    Check 'nada en HKCU\Software\Tesela' ($null -eq (Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder'))
    Check 'acceso directo en el menu Inicio COMUN y no en el del usuario' ((Test-Path -LiteralPath $commonStartMenu) -and -not (Test-Path -LiteralPath $userStartMenu))
    Check 'un solo producto, con la version del MSI' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Check 'una sola entrada en Aplicaciones instaladas' ((Get-UninstallEntries) -eq 1)
    Test-TeselaVersion (Join-Path $machineDefault 'tesela.exe') 'para todos'
    $r = Uninstall-Msi $NewMsi 'machine-uninstall'
    Check 'desinstalar la instalacion para todos termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras desinstalar para todos'

    Write-Host '  -- para todos con carpeta personalizada (INSTALLFOLDER llega a la fase del servidor) y actualizacion en su sitio'
    $r = Install-Msi $NewMsi @('ALLUSERS=1', "INSTALLFOLDER=$machineCustom\") 'machine-custom'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'instalar para todos con INSTALLFOLDER termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'los 5 archivos estan en la carpeta personalizada para todos y no en Program Files' ((Test-Files $machineCustom) -and -not (Test-Path -LiteralPath $machineDefault))
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'machine-custom-upgrade'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'reinstalar para todos sin INSTALLFOLDER termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la actualizacion para todos se queda en su carpeta y deja un solo producto' ((Test-Files $machineCustom) -and -not (Test-Path -LiteralPath $machineDefault) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Installed 'machine-custom-uninstall'
    Check 'desinstalar sin repetir INSTALLFOLDER termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la carpeta personalizada para todos se quita al desinstalar' (-not (Test-Path -LiteralPath $machineCustom))
    Remove-TestDir $machineCustomRoot
    Assert-Clean 'tras desinstalar para todos con carpeta personalizada'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '5. Cambio de ambito bloqueado (sin tocar la instalacion existente)'
    $r = Install-Msi $NewMsi @() 'scope-user'
    Check 'instalar por usuario termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'scope-user-to-machine'
    Check 'pasar de por usuario a para todos (ALLUSERS=1) se BLOQUEA (codigo 1603)' ($r.Code -eq 1603) "codigo $($r.Code)"
    Check 'el bloqueo dice que hay que desinstalar antes (por usuario -> para todos)' ($r.Text -match 'already installed for your user only') 'no sale el mensaje en el registro'
    Check 'la instalacion por usuario sigue intacta y no hay nada en Program Files ni en HKLM' ((Test-Files $userDefault) -and -not (Test-Path -LiteralPath $machineDefault) -and ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder')) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Msi $NewMsi 'scope-user-uninstall'
    Check 'desinstalar la instalacion por usuario termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras el bloqueo user -> machine'

    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'scope-machine'
    Check 'instalar para todos termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    $r = Install-Msi $SiblingMsi @() 'scope-machine-to-user'
    Check 'pasar de para todos a por usuario (sin ALLUSERS) se BLOQUEA (codigo 1603)' ($r.Code -eq 1603) "codigo $($r.Code)"
    Check 'el bloqueo dice que hay que usar ALLUSERS=1 o desinstalar antes (para todos -> por usuario)' ($r.Text -match 'already installed for all users') 'no sale el mensaje en el registro'
    Check 'la instalacion para todos sigue intacta y no hay nada en la carpeta por usuario ni en HKCU' ((Test-Files $machineDefault) -and -not (Test-Path -LiteralPath $userDefault) -and ($null -eq (Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder')) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Msi $NewMsi 'scope-machine-uninstall'
    Check 'desinstalar la instalacion para todos termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras el bloqueo machine -> user'

    # La v0.1.4 por usuario tambien bloquea el paso a para todos con el MSI nuevo.
    $r = Install-Msi $OldMsi @() 'scope-old-user'
    Check 'instalar la v0.1.4 por usuario termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'scope-old-to-machine'
    Check 'desde la v0.1.4 por usuario, pasar a para todos se BLOQUEA (codigo 1603)' ($r.Code -eq 1603) "codigo $($r.Code)"
    $r = Install-Msi $NewMsi @() 'scope-old-upgrade'
    Check 'desde la v0.1.4 por usuario, actualizar por usuario SI funciona' ($r.Code -eq 0 -and (@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion) "codigo $($r.Code)"
    $r = Uninstall-Installed 'scope-old-uninstall'
    Check 'desinstalar termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras el bloqueo desde la v0.1.4'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '6. Acceso directo del escritorio (DESKTOPSHORTCUT)'
    $r = Install-Msi $NewMsi @('DESKTOPSHORTCUT=1') 'desktop-user'
    Check 'instalar por usuario con DESKTOPSHORTCUT=1 termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'el acceso directo esta en el escritorio del usuario (y no en el comun)' ((Test-Path -LiteralPath $userDesktop) -and -not (Test-Path -LiteralPath $commonDesktop))
    $r = Install-Msi $SiblingMsi @() 'desktop-user-remembered'
    Check 'actualizar sin repetir DESKTOPSHORTCUT termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'el acceso directo del escritorio se RECUERDA al actualizar' (Test-Path -LiteralPath $userDesktop)
    $r = Install-Msi $NewMsi @('DESKTOPSHORTCUT=0') 'desktop-user-off'
    Check 'actualizar con DESKTOPSHORTCUT=0 termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'DESKTOPSHORTCUT=0 quita el acceso directo del escritorio' (-not (Test-Path -LiteralPath $userDesktop))
    $r = Install-Msi $SiblingMsi @('DESKTOPSHORTCUT=1') 'desktop-user-on'
    Check 'volver a ponerlo con DESKTOPSHORTCUT=1' ($r.Code -eq 0 -and (Test-Path -LiteralPath $userDesktop))
    $r = Uninstall-Installed 'desktop-user-uninstall'
    Check 'desinstalar termina con 0 y quita el acceso directo del escritorio' ($r.Code -eq 0 -and -not (Test-Path -LiteralPath $userDesktop)) "codigo $($r.Code)"
    Assert-Clean 'tras desinstalar con acceso directo del escritorio'

    $r = Install-Msi $NewMsi @('ALLUSERS=1', 'DESKTOPSHORTCUT=1') 'desktop-machine'
    Check 'instalar para todos con DESKTOPSHORTCUT=1 termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'el acceso directo esta en el escritorio COMUN (y no en el del usuario)' ((Test-Path -LiteralPath $commonDesktop) -and -not (Test-Path -LiteralPath $userDesktop))
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'desktop-machine-remembered'
    Check 'actualizar para todos sin repetir DESKTOPSHORTCUT lo recuerda' ($r.Code -eq 0 -and (Test-Path -LiteralPath $commonDesktop)) "codigo $($r.Code)"
    $r = Uninstall-Installed 'desktop-machine-uninstall'
    Check 'desinstalar para todos termina con 0 y quita el acceso directo comun' ($r.Code -eq 0 -and -not (Test-Path -LiteralPath $commonDesktop)) "codigo $($r.Code)"
    Assert-Clean 'tras desinstalar para todos con acceso directo del escritorio'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '7. Carpeta protegida en una instalacion por usuario (mensaje legible, no un 1603 mudo)'
    # El runner es administrador, y aun asi "solo para mi" rechaza Archivos de programa (no depende de Privileged).
    $protectedFolder = Join-Path $env:ProgramFiles 'TeselaCiProtegida'
    $r = Install-Msi $NewMsi @("INSTALLFOLDER=`"$protectedFolder`"") 'protected-user'
    Check 'por usuario con INSTALLFOLDER en Archivos de programa se BLOQUEA (codigo 1603)' ($r.Code -eq 1603) "codigo $($r.Code)"
    Check 'el bloqueo dice que la carpeta esta protegida y sugiere ALLUSERS=1' ($r.Text -match 'INSTALLFOLDER is in a protected location') 'no sale el mensaje en el registro'
    Check 'no se instalo nada ni se creo la carpeta' (-not (Test-Path -LiteralPath $protectedFolder) -and (@(Get-Products)).Count -eq 0)
    $r = Install-Msi $NewMsi @('ALLUSERS=1', "INSTALLFOLDER=`"$protectedFolder`"") 'protected-machine'
    Check 'para todos con INSTALLFOLDER en Archivos de programa SI se instala' ($r.Code -eq 0 -and (Test-Files $protectedFolder)) "codigo $($r.Code)"
    $r = Uninstall-Installed 'protected-machine-uninstall'
    Check 'desinstalar termina con 0 y quita la carpeta' ($r.Code -eq 0 -and -not (Test-Path -LiteralPath $protectedFolder)) "codigo $($r.Code)"
    Assert-Clean 'tras la carpeta protegida'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '8. Una clave de registro SIN producto (restos) no bloquea nada'
    # Una clave con InstallFolder pero sin tesela.exe en esa carpeta (restos de una version antigua, un registro limpiado a mano...).
    New-Item -Path 'HKCU:\Software\Tesela' -Force | Out-Null
    Set-ItemProperty -LiteralPath 'HKCU:\Software\Tesela' -Name 'InstallFolder' -Value ($WorkDir + '\resto-usuario\')
    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'stale-user-to-machine'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'con una clave de HKCU sin producto, instalar para todos NO se bloquea (codigo 0)' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'se instalo en Program Files' (Test-Files $machineDefault)
    $r = Uninstall-Installed 'stale-user-uninstall'
    Check 'desinstalar termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Remove-Item -LiteralPath 'HKCU:\Software\Tesela' -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -Path 'HKLM:\SOFTWARE\Tesela' -Force | Out-Null
    Set-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Tesela' -Name 'InstallFolder' -Value ($WorkDir + '\resto-equipo\')
    $r = Install-Msi $NewMsi @() 'stale-machine-to-user'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'con una clave de HKLM sin producto, instalar por usuario NO se bloquea (codigo 0)' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'se instalo en la carpeta por usuario' (Test-Files $userDefault)
    $r = Uninstall-Installed 'stale-machine-uninstall'
    Check 'desinstalar termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Remove-Item -LiteralPath 'HKLM:\SOFTWARE\Tesela' -Recurse -Force -ErrorAction SilentlyContinue
    Assert-Clean 'tras las claves sin producto'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '9. Usuario ESTANDAR (sin privilegios): por usuario sin elevar, "para todos" falla y la suya gana a la de otra cuenta'
    # Usuario local desechable, sin administrador, con contrasena aleatoria que no se imprime. Todo lo que hace el lo cuenta el mismo
    # (su HKCU y su perfil no son los del administrador del runner).
    New-Item -ItemType Directory -Force -Path $stdShared | Out-Null
    Show-PolicyDiagnostics
    New-StandardUser
    & icacls.exe $stdShared '/grant' "${stdName}:(OI)(CI)M" | Out-Null
    Set-Content -LiteralPath (Join-Path $stdShared '.tesela-ci-marker') -Value 'created by msi-install.tests.ps1 in a GitHub-hosted runner' -Encoding ASCII
    $stdScriptCopy = Join-Path $stdShared 'msi-install-standard.tests.ps1'
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'msi-install-standard.tests.ps1') -Destination $stdScriptCopy
    $stdNew = Join-Path $stdShared 'new.msi'
    $stdSibling2 = Join-Path $stdShared 'sibling2.msi'
    Copy-Item -LiteralPath $NewMsi -Destination $stdNew
    New-SiblingMsi $NewMsi $stdSibling2
    # (a) Como estandar, "para todos" tiene que FALLAR y no dejar nada.
    Invoke-StdPhase 'machine-attempt' $stdNew
    # (b) Por usuario, sin elevar: instala en su perfil sin tocar Program Files ni HKLM.
    Invoke-StdPhase 'user-install' $stdNew
    Check 'el administrador del runner no ve ese producto por usuario (cada cuenta la suya)' ((@(Get-Products)).Count -eq 0)
    Check 'nada en Program Files tras la instalacion por usuario del estandar' (-not (Test-Path -LiteralPath $machineDefault))
    Check 'nada en HKLM tras la instalacion por usuario del estandar' ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
    # (c) M2: otra cuenta (el administrador) instala para todos con otra compilacion; el estandar sigue pudiendo actualizar la SUYA.
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'std-machine-by-admin'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'otra cuenta (administrador) instala para todos mientras el estandar tiene la suya por usuario (codigo 0)' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la instalacion para todos esta en Program Files' (Test-Files $machineDefault)
    Invoke-StdPhase 'user-upgrade' $stdSibling2
    Check 'la instalacion para todos de la otra cuenta sigue intacta tras la actualizacion del estandar' ((Test-Files $machineDefault) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Installed 'std-machine-uninstall'
    Check 'desinstalar la instalacion para todos termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    # (d) El estandar desinstala la suya sin restos.
    Invoke-StdPhase 'user-uninstall' $null
    Remove-StandardUser
    Check 'el usuario desechable se borro' (-not (Get-LocalUser -Name $stdName -ErrorAction SilentlyContinue))
    Assert-Clean 'tras el escenario del usuario estandar'
} catch {
    $failed.Add("la prueba se interrumpio: $($_.Exception.Message)")
    Write-Host "ERROR: $($_.Exception.Message)" -ForegroundColor Red
} finally {
    # Limpieza: lo que quede de Tesela (el runner es desechable, pero no se deja basura) y las carpetas de la prueba.
    try { Reset-Machine } catch { Write-Host "limpieza: $($_.Exception.Message)" }
    # Los registros y resultados del usuario estandar se guardan junto a los demas antes de borrar la carpeta compartida.
    try { if (Test-Path -LiteralPath $stdShared) { Get-ChildItem -LiteralPath $stdShared -File | Where-Object { $_.Extension -in '.log', '.txt' } | Copy-Item -Destination $WorkDir -Force } } catch { Write-Host "copia de registros: $($_.Exception.Message)" }
    try { Remove-StandardUser } catch { Write-Host "limpieza del usuario estandar: $($_.Exception.Message)" }
    foreach ($k in 'HKCU:\Software\Tesela', 'HKLM:\SOFTWARE\Tesela') { Remove-Item -LiteralPath $k -Recurse -Force -ErrorAction SilentlyContinue }
    foreach ($d in (Join-Path $WorkDir 'custom-user'), $machineCustomRoot, $stdShared) {
        try { Remove-TestDir $d } catch { Write-Host "limpieza: $($_.Exception.Message)" }
    }
}

# --- Resumen -------------------------------------------------------------------------------------------------------------------
# Una prueba que no se ejecuta no es una prueba pasada: hay un minimo de escenarios y de comprobaciones, y lo omitido se cuenta aparte.
$ExpectedSteps = 10
$MinChecks = 220
if ($script:stepsRun -ne $ExpectedSteps) { $failed.Add("se ejecutaron $($script:stepsRun) escenarios y se esperaban $ExpectedSteps (un escenario no se llego a ejecutar)") }
if ($passed.Count + $failed.Count -lt $MinChecks) { $failed.Add("solo se ejecutaron $($passed.Count + $failed.Count) comprobaciones y se esperaban al menos $MinChecks") }
$cargoMajor = [version]$newVersion
if ($skipped.Count -gt 0 -and $cargoMajor -gt [version]'0.1.4') { $failed.Add("hay pruebas omitidas con la version $newVersion, que ya es posterior a la v0.1.4 publicada: $($skipped -join '; ')") }
$lua = @(Get-LuaEvidence)
Write-Host ''
Write-Host "msi-install.tests: $($passed.Count) comprobaciones bien, $($failed.Count) fallan, $($skipped.Count) omitidas, $($script:stepsRun) escenarios"
$skipped | ForEach-Object { Write-Host "  OMITIDA: $_" -ForegroundColor Yellow }
if ($lua.Count -gt 0) { Write-Host '  Evidencia del motor (ambito y elevacion):'; $lua | ForEach-Object { Write-Host "    $_" } }
if ($env:GITHUB_STEP_SUMMARY) {
    $md = New-Object System.Collections.Generic.List[string]
    $md.Add("### Pruebas de instalacion del MSI (version $newVersion; anterior $oldVersion)")
    $md.Add('')
    $md.Add("**$($passed.Count) comprobaciones bien, $($failed.Count) fallan, $($skipped.Count) omitidas, $($script:stepsRun) escenarios.**")
    $md.Add('')
    foreach ($f in $failed) { $md.Add("- FALLA: $f") }
    foreach ($s in $skipped) { $md.Add("- OMITIDA (no cuenta como pasada): $s") }
    if ($lua.Count -gt 0) {
        $md.Add('')
        $md.Add('Evidencia del motor de Windows Installer (registros de msiexec: MSI_LUA, ambito y elevacion):')
        $md.Add('')
        $md.Add('```')
        foreach ($l in $lua) { $md.Add($l) }
        $md.Add('```')
    }
    $md.Add('')
    $md.Add('<details><summary>Comprobaciones que pasaron</summary>')
    $md.Add('')
    foreach ($ok in $passed) { $md.Add("- ok: $ok") }
    $md.Add('')
    $md.Add('</details>')
    [System.IO.File]::AppendAllText($env:GITHUB_STEP_SUMMARY, (($md -join "`n") + "`n"))
}
if ($failed.Count -gt 0) {
    $failed | ForEach-Object { Write-Host "  FALLA: $_" -ForegroundColor Red }
    exit 1
}
exit 0
