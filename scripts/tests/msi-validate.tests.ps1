<#
.SYNOPSIS
  Prueba el validador de MSI (scripts/msi-validate.ps1) con copias MANIPULADAS de un MSI bueno, sin instalar nada.

.DESCRIPTION
  Parte de un MSI ya construido por scripts/build-msi.ps1 (-Msi). Cada caso copia ese MSI a una carpeta propia de %TEMP%,
  lo modifica con la API de Windows Installer (abrir la COPIA en modo escritura: no instala nada, no toca el original ni
  el sistema) y comprueba que el validador lo rechaza por el motivo esperado. Tambien comprueba que el MSI bueno pasa y
  que un error de la API NO se confunde con una tabla vacia.

.PARAMETER Msi
  Ruta de un MSI de Tesela bueno (el que acaba de generar build-msi.ps1).

.EXAMPLE
  pwsh -File scripts/tests/msi-validate.tests.ps1 -Msi <target>/dist/tesela-0.1.0-x64.msi
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Msi
)

$ErrorActionPreference = 'Stop'
. (Join-Path (Split-Path -Parent $PSScriptRoot) 'msi-validate.ps1')

$Msi = [System.IO.Path]::GetFullPath($Msi)
if (-not (Test-Path -LiteralPath $Msi -PathType Leaf)) { throw "no existe el MSI: $Msi" }

$failures = New-Object System.Collections.Generic.List[string]
$total = 0
function Check([string]$Case, [bool]$Condition) {
    $script:total++
    if (-not $Condition) { $script:failures.Add($Case) }
}

# Carpeta propia en %TEMP% (se borra al terminar, solo si tiene el nombre esperado).
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('tesela-test-' + [guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $tmp | Out-Null

function Invoke-Sql($db, [string]$Sql) {
    # Si la sentencia es invalida el error dice CUAL (un caso de prueba mal escrito no se confunde con un fallo del validador).
    try {
        $view = [System.__ComObject].InvokeMember('OpenView', 'InvokeMethod', $null, $db, @($Sql))
        [System.__ComObject].InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    } catch {
        throw "sentencia SQL invalida: $Sql ($($_.Exception.Message))"
    }
    [System.__ComObject].InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($view)
}

function New-Mutant([string]$Name, [string[]]$Statements) {
    # Copia el MSI bueno, le aplica las sentencias SQL y devuelve la ruta de la copia.
    $path = Join-Path $tmp "$Name.msi"
    Copy-Item -LiteralPath $Msi -Destination $path
    $m = Open-MsiDatabase -Path $path -Write $true
    try {
        foreach ($s in $Statements) { Invoke-Sql $m.Db $s }
        [System.__ComObject].InvokeMember('Commit', 'InvokeMethod', $null, $m.Db, $null) | Out-Null
    } finally {
        Close-Msi $m
    }
    return $path
}

function Get-GoodVersion {
    $m = Open-MsiDatabase $Msi
    try {
        $r = @(Get-MsiRows $m 'SELECT `Value` FROM `Property` WHERE `Property`=''ProductVersion''' 1)
        return $r[0][0]
    } finally { Close-Msi $m }
}

function Has-Problem($Problems, [string]$Pattern) {
    return @($Problems | Where-Object { $_ -like $Pattern }).Count -gt 0
}

try {
    $version = Get-GoodVersion

    # --- El MSI bueno pasa ----------------------------------------------------------------------------------------------
    $good = Test-MsiPackage -Path $Msi -ExpectedVersion $version
    Check "el MSI bueno no tiene problemas ($($good -join '; '))" ($good.Count -eq 0)

    # --- Cada manipulacion se rechaza por su motivo ---------------------------------------------------------------------
    $p = Test-MsiPackage -Path $Msi -ExpectedVersion '9.9.9'
    Check 'otra version de producto' (Has-Problem $p '*ProductVersion*')

    $path = New-Mutant 'allusers' @('INSERT INTO `Property` (`Property`, `Value`) VALUES (''ALLUSERS'', ''1'')')
    Check 'ALLUSERS=1 (por maquina)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*no es por usuario*')

    $path = New-Mutant 'upgradecode' @('UPDATE `Property` SET `Value`=''{11111111-2222-3333-4444-555555555555}'' WHERE `Property`=''UpgradeCode''')
    Check 'otro UpgradeCode' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*UpgradeCode*')

    $path = New-Mutant 'hklm' @('INSERT INTO `Registry` (`Registry`, `Root`, `Key`, `Name`, `Value`, `Component_`) VALUES (''evilreg'', 2, ''Software\Evil'', ''n'', ''v'', ''TeselaExe'')')
    Check 'registro en HKLM' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*registro fuera de HKCU*')

    $path = New-Mutant 'extrafile' @('INSERT INTO `File` (`File`, `Component_`, `FileName`, `FileSize`, `Sequence`) VALUES (''evil'', ''TeselaExe'', ''evil.exe'', 1, 99)')
    $p = Test-MsiPackage -Path $path -ExpectedVersion $version
    Check 'un archivo de mas' ((Has-Problem $p '*archivo inesperado*evil.exe*') -and (Has-Problem $p '*lleva 6 archivos*'))

    $path = New-Mutant 'customaction' @(
        'CREATE TABLE `CustomAction` (`Action` CHAR(72) NOT NULL, `Type` SHORT NOT NULL, `Source` CHAR(72), `Target` CHAR(255) PRIMARY KEY `Action`)',
        'INSERT INTO `CustomAction` (`Action`, `Type`, `Source`, `Target`) VALUES (''Evil'', 1, ''Bin'', ''x'')')
    $p = Test-MsiPackage -Path $path -ExpectedVersion $version
    Check 'una CustomAction (la tabla y la accion)' ((Has-Problem $p '*tabla no permitida*CustomAction*') -and (Has-Problem $p '*acciones personalizadas*Evil*'))

    # Tablas extra aunque esten VACIAS: Environment, ServiceInstall, Class, Extension, ProgId, Binary.
    $tables = [ordered]@{
        'Environment'    = 'CREATE TABLE `Environment` (`Environment` CHAR(72) NOT NULL, `Name` CHAR(255) NOT NULL, `Value` CHAR(255), `Component_` CHAR(72) NOT NULL PRIMARY KEY `Environment`)'
        'ServiceInstall' = 'CREATE TABLE `ServiceInstall` (`ServiceInstall` CHAR(72) NOT NULL, `Name` CHAR(255) NOT NULL, `Component_` CHAR(72) NOT NULL PRIMARY KEY `ServiceInstall`)'
        'Class'          = 'CREATE TABLE `Class` (`CLSID` CHAR(38) NOT NULL, `Context` CHAR(32) NOT NULL, `Component_` CHAR(72) NOT NULL PRIMARY KEY `CLSID`, `Context`)'
        'Extension'      = 'CREATE TABLE `Extension` (`Extension` CHAR(255) NOT NULL, `Component_` CHAR(72) NOT NULL PRIMARY KEY `Extension`, `Component_`)'
        'ProgId'         = 'CREATE TABLE `ProgId` (`ProgId` CHAR(255) NOT NULL, `Description` CHAR(255) PRIMARY KEY `ProgId`)'
        'Binary'         = 'CREATE TABLE `Binary` (`Name` CHAR(72) NOT NULL, `Data` OBJECT NOT NULL PRIMARY KEY `Name`)'
    }
    foreach ($t in $tables.Keys) {
        $path = New-Mutant ('table-' + $t) @($tables[$t])
        Check "tabla extra vacia: $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*tabla no permitida*$t*")
    }

    $path = New-Mutant 'noshortcut' @('DELETE FROM `Shortcut`')
    Check 'sin acceso directo' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*acceso directo*')

    $path = New-Mutant 'nosearch' @('DELETE FROM `AppSearch`')
    Check 'sin la busqueda de la carpeta de instalacion' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*AppSearch*')

    $path = New-Mutant 'removefile-extra' @('INSERT INTO `RemoveFile` (`FileKey`, `Component_`, `FileName`, `DirProperty`, `InstallMode`) VALUES (''evilrm'', ''TeselaExe'', ''*.*'', ''INSTALLFOLDER'', 2)')
    $p = Test-MsiPackage -Path $path -ExpectedVersion $version
    Check 'una fila de RemoveFile de mas (con *.*)' ((Has-Problem $p '*RemoveFile no esperada*evilrm*') -and (Has-Problem $p '*exactamente 2 filas*'))

    $path = New-Mutant 'removefile-otradir' @('UPDATE `RemoveFile` SET `DirProperty`=''LocalAppDataFolder'' WHERE `FileKey`=''RemoveProgramsFolder''')
    Check 'RemoveFile sobre otra carpeta' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*RemoveFile no esperada*')

    $path = New-Mutant 'reglocator-type2' @('UPDATE `RegLocator` SET `Type`=2')
    Check 'RegLocator con Type=2 (vista de 32 bits) en lugar de 18' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*busqueda de registro*tipo 18*')

    # --- Arbol de carpetas EXACTO (Directory_Parent y DefaultDir) -------------------------------------------------------------
    foreach ($case in @(
            @('INSTALLFOLDER colgando de TARGETDIR (padre equivocado)', 'INSTALLFOLDER', "UPDATE ``Directory`` SET ``Directory_Parent``='TARGETDIR' WHERE ``Directory``='INSTALLFOLDER'", '*carpeta INSTALLFOLDER*Directory_Parent*ProgramsFolder*'),
            @('ProgramsFolder colgando de TARGETDIR', 'ProgramsFolder', "UPDATE ``Directory`` SET ``Directory_Parent``='TARGETDIR' WHERE ``Directory``='ProgramsFolder'", '*carpeta ProgramsFolder*Directory_Parent*LocalAppDataFolder*'),
            @('LocalAppDataFolder colgando de ProgramMenuFolder', 'LocalAppDataFolder', "UPDATE ``Directory`` SET ``Directory_Parent``='ProgramMenuFolder' WHERE ``Directory``='LocalAppDataFolder'", '*carpeta LocalAppDataFolder*Directory_Parent*TARGETDIR*'),
            @('ProgramMenuFolder colgando de LocalAppDataFolder', 'ProgramMenuFolder', "UPDATE ``Directory`` SET ``Directory_Parent``='LocalAppDataFolder' WHERE ``Directory``='ProgramMenuFolder'", '*carpeta ProgramMenuFolder*Directory_Parent*TARGETDIR*'),
            @('TARGETDIR con padre', 'TARGETDIR', "UPDATE ``Directory`` SET ``Directory_Parent``='INSTALLFOLDER' WHERE ``Directory``='TARGETDIR'", '*carpeta TARGETDIR*Directory_Parent*'),
            @('INSTALLFOLDER con otro nombre (DefaultDir)', 'INSTALLFOLDER', "UPDATE ``Directory`` SET ``DefaultDir``='OtraCarpeta' WHERE ``Directory``='INSTALLFOLDER'", '*carpeta INSTALLFOLDER*DefaultDir*Tesela*'),
            @('ProgramsFolder con otro nombre (DefaultDir)', 'ProgramsFolder', "UPDATE ``Directory`` SET ``DefaultDir``='Prog' WHERE ``Directory``='ProgramsFolder'", '*carpeta ProgramsFolder*DefaultDir*Programs*')
        )) {
        $path = New-Mutant ('dir-' + $case[1] + '-' + [guid]::NewGuid().ToString('N').Substring(0, 6)) @($case[2])
        Check ('carpetas: ' + $case[0]) (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) $case[3])
    }
    # DefaultDir en la forma "corto|largo": vale si el nombre largo es el esperado.
    $path = New-Mutant 'dir-shortlong' @("UPDATE ``Directory`` SET ``DefaultDir``='Tesela|Tesela' WHERE ``Directory``='INSTALLFOLDER'")
    Check 'carpetas: DefaultDir corto|largo con el nombre largo esperado es valido' (-not (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*DefaultDir*'))
    $path = New-Mutant 'dir-extra' @("INSERT INTO ``Directory`` (``Directory``, ``Directory_Parent``, ``DefaultDir``) VALUES ('EvilDir', 'TARGETDIR', 'Evil')")
    Check 'carpetas: una carpeta de mas' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*carpeta fuera del perfil*EvilDir*')
    $path = New-Mutant 'dir-missing' @("DELETE FROM ``Directory`` WHERE ``Directory``='ProgramMenuFolder'")
    Check 'carpetas: falta una carpeta esperada' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*carpeta ProgramMenuFolder*0 filas*')

    # --- MajorUpgrade y bloqueo de versiones posteriores: valores exactos -----------------------------------------------------
    $upgCodeValue = '{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}'
    $upCols = '`UpgradeCode`, `VersionMin`, `VersionMax`, `Language`, `Attributes`, `Remove`, `ActionProperty`'
    $delDown = "DELETE FROM ``Upgrade`` WHERE ``ActionProperty``='WIX_DOWNGRADE_DETECTED'"
    $delUp = "DELETE FROM ``Upgrade`` WHERE ``ActionProperty``='WIX_UPGRADE_DETECTED'"
    function Down-Row([string]$Min, [string]$Lang, [int]$Attr, [string]$Prop = 'WIX_DOWNGRADE_DETECTED', [string]$UCode = $upgCodeValue) {
        return "INSERT INTO ``Upgrade`` ($upCols) VALUES ('$UCode', '$Min', '', '$Lang', $Attr, '', '$Prop')"
    }
    function Up-Row([string]$Max, [string]$Lang, [int]$Attr, [string]$Prop = 'WIX_UPGRADE_DETECTED') {
        return "INSERT INTO ``Upgrade`` ($upCols) VALUES ('$upgCodeValue', '', '$Max', '$Lang', $Attr, '', '$Prop')"
    }
    # Control: reponer las dos filas tal cual con estas funciones da un MSI valido (las manipulaciones de abajo son fiables).
    $path = New-Mutant 'upgrade-control' @($delDown, $delUp, (Down-Row $version '1033' 2), (Up-Row $version '1033' 513))
    Check 'Upgrade: reponer las dos filas tal cual sigue siendo valido' ((Test-MsiPackage -Path $path -ExpectedVersion $version).Count -eq 0)

    $path = New-Mutant 'upgrade-nodown' @($delDown)
    Check 'Upgrade: falta la fila de bloqueo de versiones posteriores' ((Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*falta la fila de Upgrade*bloquear*WIX_DOWNGRADE_DETECTED*') -and (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*tabla Upgrade tiene 1 filas*'))
    $path = New-Mutant 'upgrade-noup' @($delUp)
    Check 'Upgrade: falta la fila de reemplazo de versiones anteriores' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*falta la fila de Upgrade*reemplazar*WIX_UPGRADE_DETECTED*')
    $path = New-Mutant 'upgrade-noneatall' @($delDown, $delUp)
    Check 'Upgrade: sin MajorUpgrade (tabla vacia)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*tabla Upgrade tiene 0 filas*')
    $path = New-Mutant 'upgrade-down-attr0' @($delDown, (Down-Row $version '1033' 0))
    Check 'Upgrade: bloqueo sin OnlyDetect (Attributes = 0: desinstalaria la version posterior)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*bloquear*Attributes = ''0''*se esperaba ''2''*')
    $path = New-Mutant 'upgrade-down-attr258' @($delDown, (Down-Row $version '1033' 258))
    Check 'Upgrade: bloqueo con VersionMin inclusivo (Attributes = 258)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*bloquear*Attributes = ''258''*')
    $path = New-Mutant 'upgrade-up-attr1' @($delUp, (Up-Row $version '1033' 1))
    Check 'Upgrade: reemplazo sin VersionMax inclusivo (Attributes = 1)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*reemplazar*Attributes = ''1''*se esperaba ''513''*')
    $path = New-Mutant 'upgrade-up-attr512' @($delUp, (Up-Row $version '1033' 512))
    Check 'Upgrade: reemplazo sin MigrateFeatures (Attributes = 512)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*reemplazar*Attributes = ''512''*')
    $path = New-Mutant 'upgrade-down-min' @($delDown, (Down-Row '0.0.1' '1033' 2))
    Check 'Upgrade: bloqueo con otro VersionMin' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*bloquear*VersionMin = ''0.0.1''*')
    $path = New-Mutant 'upgrade-up-max' @($delUp, (Up-Row '99.0.0' '1033' 513))
    Check 'Upgrade: reemplazo con otro VersionMax' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*reemplazar*VersionMax = ''99.0.0''*')
    $path = New-Mutant 'upgrade-down-lang' @($delDown, (Down-Row $version '3082' 2))
    Check 'Upgrade: bloqueo con otro idioma' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*bloquear*Language = ''3082''*')
    $path = New-Mutant 'upgrade-down-prop' @($delDown, (Down-Row $version '1033' 2 'OTRA_PROPIEDAD'))
    Check 'Upgrade: bloqueo con otra ActionProperty' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*falta la fila de Upgrade*WIX_DOWNGRADE_DETECTED*')
    $path = New-Mutant 'upgrade-down-code' @($delDown, (Down-Row $version '1033' 2 'WIX_DOWNGRADE_DETECTED' '{11111111-2222-3333-4444-555555555555}'))
    Check 'Upgrade: bloqueo con otro UpgradeCode' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*bloquear*UpgradeCode*')
    $path = New-Mutant 'upgrade-extra' @((Down-Row '0.0.1' '1033' 2 'OTRA_PROPIEDAD'))
    Check 'Upgrade: una fila de mas' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*tabla Upgrade tiene 3 filas*')
    # Contra otra version esperada, VersionMin/VersionMax ya no cuadran.
    $p = Test-MsiPackage -Path $Msi -ExpectedVersion '9.9.9'
    Check 'Upgrade: otra version esperada (VersionMax y VersionMin siguen a la version del producto)' ((Has-Problem $p '*reemplazar*VersionMax*9.9.9*') -and (Has-Problem $p '*bloquear*VersionMin*9.9.9*'))

    # LaunchCondition: la condicion exacta, y UNA sola, con mensaje (Condition es clave primaria: se borra y se inserta).
    $lcMsg = 'Mensaje de prueba'
    $path = New-Mutant 'lc-nonot' @("DELETE FROM ``LaunchCondition``", "INSERT INTO ``LaunchCondition`` (``Condition``, ``Description``) VALUES ('WIX_DOWNGRADE_DETECTED', '$lcMsg')")
    Check 'LaunchCondition sin NOT (bloquearia a quien NO instala una anterior)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*UNA LaunchCondition*NOT WIX_DOWNGRADE_DETECTED*')
    $path = New-Mutant 'lc-other' @("DELETE FROM ``LaunchCondition``", "INSERT INTO ``LaunchCondition`` (``Condition``, ``Description``) VALUES ('NOT WIX_UPGRADE_DETECTED', '$lcMsg')")
    Check 'LaunchCondition sobre otra propiedad' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*UNA LaunchCondition*')
    $path = New-Mutant 'lc-none' @("DELETE FROM ``LaunchCondition``")
    Check 'sin LaunchCondition' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*UNA LaunchCondition*')
    $path = New-Mutant 'lc-extra' @("INSERT INTO ``LaunchCondition`` (``Condition``, ``Description``) VALUES ('1=1', '$lcMsg')")
    Check 'una LaunchCondition de mas' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*UNA LaunchCondition*')
    $path = New-Mutant 'lc-nomsg' @("DELETE FROM ``LaunchCondition``", "INSERT INTO ``LaunchCondition`` (``Condition``, ``Description``) VALUES ('NOT WIX_DOWNGRADE_DETECTED', ' ')")
    Check 'LaunchCondition con el mensaje en blanco' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*LaunchCondition*no tiene mensaje*')

    # Que todo esto SURTA EFECTO: las acciones que lo evaluan, presentes, incondicionales y bien colocadas.
    $path = New-Mutant 'seq-noremove' @("DELETE FROM ``InstallExecuteSequence`` WHERE ``Action``='RemoveExistingProducts'")
    Check 'sin RemoveExistingProducts en InstallExecuteSequence' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*falta RemoveExistingProducts*')
    $path = New-Mutant 'seq-removecond' @("UPDATE ``InstallExecuteSequence`` SET ``Condition``='0' WHERE ``Action``='RemoveExistingProducts'")
    Check 'RemoveExistingProducts con condicion' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*RemoveExistingProducts lleva la condicion*')
    $path = New-Mutant 'seq-removelate' @("UPDATE ``InstallExecuteSequence`` SET ``Sequence``=6500 WHERE ``Action``='RemoveExistingProducts'")
    Check 'RemoveExistingProducts fuera de InstallValidate..InstallInitialize' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*RemoveExistingProducts*entre InstallValidate e InstallInitialize*')
    foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
        foreach ($a in 'FindRelatedProducts', 'LaunchConditions') {
            $path = New-Mutant "seq-no-$a-$t" @("DELETE FROM ``$t`` WHERE ``Action``='$a'")
            Check "sin $a en $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*falta $a en $t*")
            $path = New-Mutant "seq-cond-$a-$t" @("UPDATE ``$t`` SET ``Condition``='0' WHERE ``Action``='$a'")
            Check "$a con condicion en $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*$a en $t lleva la condicion*")
        }
    }
    $path = New-Mutant 'secure-props' @("UPDATE ``Property`` SET ``Value``='WIX_UPGRADE_DETECTED' WHERE ``Property``='SecureCustomProperties'")
    Check 'SecureCustomProperties sin WIX_DOWNGRADE_DETECTED' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*SecureCustomProperties no incluye WIX_DOWNGRADE_DETECTED*')

    # --- Un error de la API no es una tabla vacia ------------------------------------------------------------------------
    $m = Open-MsiDatabase $Msi
    try {
        Check 'una tabla que no existe da cero filas' (@(Get-MsiRows $m 'SELECT `Action` FROM `CustomAction`' 1).Count -eq 0)
        $threw = $false
        try { Get-MsiRows $m 'SELECT `NoExiste` FROM `Property`' 1 | Out-Null } catch { $threw = $true }
        Check 'una columna que no existe NO se trata como tabla vacia (lanza)' $threw
        $threw = $false
        try { Get-MsiRows $m 'SELECT 1' 1 | Out-Null } catch { $threw = $true }
        Check 'una consulta sin tabla lanza' $threw
    } finally { Close-Msi $m }
} finally {
    $leaf = Split-Path -Leaf $tmp
    if ($leaf -like 'tesela-test-*' -and (Split-Path -Parent $tmp) -eq [System.IO.Path]::GetTempPath().TrimEnd('\')) {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($failures.Count -gt 0) {
    Write-Host "msi-validate.tests: $($failures.Count) de $total comprobaciones fallan:" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host "msi-validate.tests: OK ($total comprobaciones)"
