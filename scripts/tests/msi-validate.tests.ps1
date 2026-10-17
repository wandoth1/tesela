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

function New-WordCountMutant([string]$Name, [int]$Value) {
    # Copia el MSI bueno y cambia el contador de palabras del Summary Information (el bit 8 = "no hace falta elevar").
    $path = Join-Path $tmp "$Name.msi"
    Copy-Item -LiteralPath $Msi -Destination $path
    $m = Open-MsiDatabase -Path $path -Write $true
    try {
        $si = [System.__ComObject].InvokeMember('SummaryInformation', 'GetProperty', $null, $m.Db, @(1))
        [System.__ComObject].InvokeMember('Property', 'SetProperty', $null, $si, @(15, $Value)) | Out-Null
        [System.__ComObject].InvokeMember('Persist', 'InvokeMethod', $null, $si, $null) | Out-Null
        [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($si)
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

    # --- Ambito dual: por usuario por defecto (ALLUSERS=2, MSIINSTALLPERUSER=1) y sin elevar -------------------------------
    $path = New-Mutant 'allusers1' @('UPDATE `Property` SET `Value`=''1'' WHERE `Property`=''ALLUSERS''')
    Check 'ALLUSERS=1 por defecto (por maquina)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*no es por usuario por defecto*')
    $path = New-Mutant 'allusers-none' @('DELETE FROM `Property` WHERE `Property`=''ALLUSERS''')
    Check 'sin ALLUSERS' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*no es por usuario por defecto*')
    $path = New-Mutant 'perusermode-none' @('DELETE FROM `Property` WHERE `Property`=''MSIINSTALLPERUSER''')
    Check 'sin MSIINSTALLPERUSER (ALLUSERS=2 solo daria para todos a un administrador)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*no es por usuario por defecto*')
    $path = New-Mutant 'perusermode-0' @('UPDATE `Property` SET `Value`=''0'' WHERE `Property`=''MSIINSTALLPERUSER''')
    Check 'MSIINSTALLPERUSER=0' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*no es por usuario por defecto*')
    $path = New-WordCountMutant 'wordcount-noelevation' 2
    Check 'sin el bit "no hace falta elevar" (Windows pediria administrador al abrir el MSI)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*no declara*no hace falta elevar*')
    $path = New-WordCountMutant 'wordcount-ok' 10
    Check 'contador de palabras 10 (con el bit) sigue siendo valido' (-not (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*no hace falta elevar*'))

    $path = New-Mutant 'upgradecode' @('UPDATE `Property` SET `Value`=''{11111111-2222-3333-4444-555555555555}'' WHERE `Property`=''UpgradeCode''')
    Check 'otro UpgradeCode' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*UpgradeCode*')

    $path = New-Mutant 'unknown-property' @('INSERT INTO `Property` (`Property`, `Value`) VALUES (''EVILPROP'', ''x'')')
    Check 'una propiedad que no esta en la lista cerrada' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*propiedad no esperada*EVILPROP*')

    # --- Registro: solo HKMU\Software\Tesela con los 8 valores esperados -----------------------------------------------------
    $path = New-Mutant 'hklm' @('INSERT INTO `Registry` (`Registry`, `Root`, `Key`, `Name`, `Value`, `Component_`) VALUES (''evilreg'', 2, ''Software\Evil'', ''n'', ''v'', ''TeselaExe'')')
    Check 'registro en HKLM fijo (no HKMU)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*registro fuera de HKMU*')
    $path = New-Mutant 'hkcu' @('UPDATE `Registry` SET `Root`=1 WHERE `Name`=''tesela.exe''')
    Check 'registro en HKCU fijo (no HKMU: una instalacion para todos escribiria en el usuario)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*registro fuera de HKMU*')
    $path = New-Mutant 'reg-otraclave' @('UPDATE `Registry` SET `Key`=''Software\Otra'' WHERE `Name`=''tesela.exe''')
    Check 'registro en otra clave' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*registro fuera de HKMU*')
    $path = New-Mutant 'reg-valor' @('UPDATE `Registry` SET `Value`=''C:\Otra\'' WHERE `Name`=''InstallFolder''')
    Check 'InstallFolder con otro valor' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*InstallFolder*se esperaba*[[]INSTALLFOLDER]*')
    $path = New-Mutant 'reg-extra' @('INSERT INTO `Registry` (`Registry`, `Root`, `Key`, `Name`, `Value`, `Component_`) VALUES (''extrareg'', -1, ''Software\Tesela'', ''Otro'', ''v'', ''TeselaExe'')')
    Check 'un valor de registro de mas' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*escribe 9 valores de registro*')
    $path = New-Mutant 'reg-nodesktop' @('DELETE FROM `Registry` WHERE `Name`=''DesktopShortcut''')
    Check 'sin el valor DesktopShortcut (el escritorio no se recordaria)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*DesktopShortcut: hay 0 filas*')

    $path = New-Mutant 'extrafile' @('INSERT INTO `File` (`File`, `Component_`, `FileName`, `FileSize`, `Sequence`) VALUES (''evil'', ''TeselaExe'', ''evil.exe'', 1, 99)')
    $p = Test-MsiPackage -Path $path -ExpectedVersion $version
    Check 'un archivo de mas' ((Has-Problem $p '*archivo inesperado*evil.exe*') -and (Has-Problem $p '*lleva 6 archivos*'))

    # --- SIN CODIGO: acciones personalizadas solo de tipo 51 / 35 y las esperadas ------------------------------------------
    $caCols = '`Action`, `Type`, `Source`, `Target`'
    $path = New-Mutant 'ca-dll' @("INSERT INTO ``CustomAction`` ($caCols) VALUES ('Evil', 1, 'Bin', 'x')")
    Check 'una accion personalizada de tipo 1 (DLL)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*accion personalizada de tipo 1*Evil*')
    $path = New-Mutant 'ca-exe' @("INSERT INTO ``CustomAction`` ($caCols) VALUES ('Evil', 2, 'Bin', 'x')")
    Check 'una accion personalizada de tipo 2 (EXE)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*accion personalizada de tipo 2*Evil*')
    $path = New-Mutant 'ca-script' @("INSERT INTO ``CustomAction`` ($caCols) VALUES ('Evil', 6, 'Bin', 'x')")
    Check 'una accion personalizada de tipo 6 (script)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*accion personalizada de tipo 6*Evil*')
    $path = New-Mutant 'ca-type34' @("INSERT INTO ``CustomAction`` ($caCols) VALUES ('Evil', 34, 'INSTALLFOLDER', 'x')")
    Check 'una accion personalizada de tipo 34 (EXE desde una carpeta)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*accion personalizada de tipo 34*Evil*')
    $path = New-Mutant 'ca-type51-extra' @("INSERT INTO ``CustomAction`` ($caCols) VALUES ('Evil', 51, 'INSTALLFOLDER', 'C:\Evil')")
    Check 'una accion de tipo 51 que no esta en la lista esperada' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*accion personalizada no esperada*Evil*')
    $path = New-Mutant 'ca-type35-extra' @("INSERT INTO ``CustomAction`` ($caCols) VALUES ('Evil', 35, 'INSTALLFOLDER', 'C:\Evil')")
    Check 'una accion de tipo 35 que no esta en la lista esperada' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*accion personalizada no esperada*Evil*')
    $path = New-Mutant 'ca-target' @('UPDATE `CustomAction` SET `Target`=''[TESELA_PREV_MACHINE]'' WHERE `Action`=''TeselaFolderPrevUser''')
    Check 'una accion esperada con otro destino' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TeselaFolderPrevUser*se esperaba tipo 51*')
    $path = New-Mutant 'ca-source' @('UPDATE `CustomAction` SET `Source`=''ALLUSERS'' WHERE `Action`=''TeselaRememberDesktop''')
    Check 'una accion esperada sobre otra propiedad' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TeselaRememberDesktop*se esperaba tipo 51*')
    $path = New-Mutant 'ca-missing' @('DELETE FROM `CustomAction` WHERE `Action`=''TeselaFolderDefaultUser''')
    Check 'falta una accion esperada' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*falta la accion TeselaFolderDefaultUser*')

    # Tablas extra aunque esten VACIAS: Environment, ServiceInstall, Class, Extension, ProgId, Binary, Error.
    $tables = [ordered]@{
        'Environment'    = 'CREATE TABLE `Environment` (`Environment` CHAR(72) NOT NULL, `Name` CHAR(255) NOT NULL, `Value` CHAR(255), `Component_` CHAR(72) NOT NULL PRIMARY KEY `Environment`)'
        'ServiceInstall' = 'CREATE TABLE `ServiceInstall` (`ServiceInstall` CHAR(72) NOT NULL, `Name` CHAR(255) NOT NULL, `Component_` CHAR(72) NOT NULL PRIMARY KEY `ServiceInstall`)'
        'Class'          = 'CREATE TABLE `Class` (`CLSID` CHAR(38) NOT NULL, `Context` CHAR(32) NOT NULL, `Component_` CHAR(72) NOT NULL PRIMARY KEY `CLSID`, `Context`)'
        'Extension'      = 'CREATE TABLE `Extension` (`Extension` CHAR(255) NOT NULL, `Component_` CHAR(72) NOT NULL PRIMARY KEY `Extension`, `Component_`)'
        'ProgId'         = 'CREATE TABLE `ProgId` (`ProgId` CHAR(255) NOT NULL, `Description` CHAR(255) PRIMARY KEY `ProgId`)'
        'Binary'         = 'CREATE TABLE `Binary` (`Name` CHAR(72) NOT NULL, `Data` OBJECT NOT NULL PRIMARY KEY `Name`)'
        'Error'          = 'CREATE TABLE `Error` (`Error` SHORT NOT NULL, `Message` CHAR(255) LOCALIZABLE PRIMARY KEY `Error`)'
        'Billboard'      = 'CREATE TABLE `Billboard` (`Billboard` CHAR(50) NOT NULL, `Feature_` CHAR(38) NOT NULL PRIMARY KEY `Billboard`)'
    }
    foreach ($t in $tables.Keys) {
        $path = New-Mutant ('table-' + $t) @($tables[$t])
        Check "tabla extra vacia: $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*tabla no permitida*$t*")
    }

    $path = New-Mutant 'noshortcut' @('DELETE FROM `Shortcut`')
    Check 'sin accesos directos' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*accesos directos*')
    $path = New-Mutant 'nodesktopshortcut' @('DELETE FROM `Shortcut` WHERE `Shortcut`=''DesktopShortcut''')
    Check 'sin el acceso directo del escritorio' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*acceso directo DesktopShortcut*DesktopFolder*')
    $path = New-Mutant 'desktopshortcut-dir' @('UPDATE `Shortcut` SET `Directory_`=''ProgramMenuFolder'' WHERE `Shortcut`=''DesktopShortcut''')
    Check 'acceso directo del escritorio en el menu Inicio' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*acceso directo DesktopShortcut*DesktopFolder*')

    # --- Busquedas de registro de instalaciones previas (cuatro, HKCU y HKLM) --------------------------------------------------
    $path = New-Mutant 'nosearch' @('DELETE FROM `AppSearch`')
    Check 'sin las busquedas de la instalacion previa' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*AppSearch*')
    $path = New-Mutant 'nosearch-machine' @('DELETE FROM `AppSearch` WHERE `Property`=''TESELA_PREV_MACHINE''')
    Check 'sin la busqueda de la instalacion para todos (el bloqueo del cambio de ambito no la veria)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TESELA_PREV_MACHINE*')
    $path = New-Mutant 'reglocator-type2' @('UPDATE `RegLocator` SET `Type`=2')
    Check 'RegLocator con Type=2 (vista de 32 bits) en lugar de 18' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*busqueda de registro*tipo 18*')
    $path = New-Mutant 'reglocator-root' @('UPDATE `RegLocator` SET `Root`=1 WHERE `Signature_`=''PrevMachineFolder''')
    Check 'la busqueda de la instalacion para todos mirando en HKCU' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*PrevMachineFolder*raiz 2*')
    $path = New-Mutant 'reglocator-name' @('UPDATE `RegLocator` SET `Name`=''Otro'' WHERE `Signature_`=''PrevUserFolder''')
    Check 'una busqueda con otro nombre de valor' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*PrevUserFolder*')

    # --- RemoveFile: una sola fila (INSTALLFOLDER, si queda vacia) ----------------------------------------------------------
    $path = New-Mutant 'removefile-extra' @('INSERT INTO `RemoveFile` (`FileKey`, `Component_`, `FileName`, `DirProperty`, `InstallMode`) VALUES (''evilrm'', ''TeselaExe'', ''*.*'', ''INSTALLFOLDER'', 2)')
    $p = Test-MsiPackage -Path $path -ExpectedVersion $version
    Check 'una fila de RemoveFile de mas (con *.*)' ((Has-Problem $p '*RemoveFile no esperada*evilrm*') -and (Has-Problem $p '*exactamente 1 fila*'))
    $path = New-Mutant 'removefile-otradir' @('UPDATE `RemoveFile` SET `DirProperty`=''ProgramMenuFolder'' WHERE `FileKey`=''RemoveInstallFolder''')
    Check 'RemoveFile sobre otra carpeta' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*RemoveFile no esperada*')
    $path = New-Mutant 'removefile-none' @('DELETE FROM `RemoveFile`')
    Check 'sin RemoveFile (la carpeta de instalacion quedaria al desinstalar)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*exactamente 1 fila*')

    # --- Arbol de carpetas EXACTO (Directory_Parent y DefaultDir) -------------------------------------------------------------
    foreach ($case in @(
            @('INSTALLFOLDER colgando de ProgramMenuFolder (padre equivocado)', 'INSTALLFOLDER', "UPDATE ``Directory`` SET ``Directory_Parent``='ProgramMenuFolder' WHERE ``Directory``='INSTALLFOLDER'", '*carpeta INSTALLFOLDER*Directory_Parent*TARGETDIR*'),
            @('INSTALLFOLDER sin padre', 'INSTALLFOLDER', "UPDATE ``Directory`` SET ``Directory_Parent``='' WHERE ``Directory``='INSTALLFOLDER'", '*carpeta INSTALLFOLDER*Directory_Parent*TARGETDIR*'),
            @('ProgramMenuFolder colgando de INSTALLFOLDER', 'ProgramMenuFolder', "UPDATE ``Directory`` SET ``Directory_Parent``='INSTALLFOLDER' WHERE ``Directory``='ProgramMenuFolder'", '*carpeta ProgramMenuFolder*Directory_Parent*TARGETDIR*'),
            @('DesktopFolder colgando de INSTALLFOLDER', 'DesktopFolder', "UPDATE ``Directory`` SET ``Directory_Parent``='INSTALLFOLDER' WHERE ``Directory``='DesktopFolder'", '*carpeta DesktopFolder*Directory_Parent*TARGETDIR*'),
            @('TARGETDIR con padre', 'TARGETDIR', "UPDATE ``Directory`` SET ``Directory_Parent``='INSTALLFOLDER' WHERE ``Directory``='TARGETDIR'", '*carpeta TARGETDIR*Directory_Parent*'),
            @('INSTALLFOLDER con otro nombre (DefaultDir)', 'INSTALLFOLDER', "UPDATE ``Directory`` SET ``DefaultDir``='OtraCarpeta' WHERE ``Directory``='INSTALLFOLDER'", '*carpeta INSTALLFOLDER*DefaultDir*Tesela*')
        )) {
        $path = New-Mutant ('dir-' + $case[1] + '-' + [guid]::NewGuid().ToString('N').Substring(0, 6)) @($case[2])
        Check ('carpetas: ' + $case[0]) (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) $case[3])
    }
    # DefaultDir en la forma "corto|largo": vale si el nombre largo es el esperado.
    $path = New-Mutant 'dir-shortlong' @("UPDATE ``Directory`` SET ``DefaultDir``='Tesela|Tesela' WHERE ``Directory``='INSTALLFOLDER'")
    Check 'carpetas: DefaultDir corto|largo con el nombre largo esperado es valido' (-not (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*DefaultDir*'))
    $path = New-Mutant 'dir-extra' @("INSERT INTO ``Directory`` (``Directory``, ``Directory_Parent``, ``DefaultDir``) VALUES ('EvilDir', 'TARGETDIR', 'Evil')")
    Check 'carpetas: una carpeta de mas' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*carpeta no esperada*EvilDir*')
    $path = New-Mutant 'dir-programfiles' @("INSERT INTO ``Directory`` (``Directory``, ``Directory_Parent``, ``DefaultDir``) VALUES ('ProgramFiles64Folder', 'TARGETDIR', '.')")
    Check 'carpetas: ProgramFiles64Folder fija en el arbol (el ambito lo decide la accion, no el arbol)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*carpeta no esperada*ProgramFiles64Folder*')
    $path = New-Mutant 'dir-missing' @("DELETE FROM ``Directory`` WHERE ``Directory``='DesktopFolder'")
    Check 'carpetas: falta una carpeta esperada' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*carpeta DesktopFolder*0 filas*')

    # --- Componentes: carpeta, atributos y la condicion del escritorio -------------------------------------------------------
    $path = New-Mutant 'component-nocond' @('UPDATE `Component` SET `Condition`='''' WHERE `Component`=''DesktopShortcut''')
    Check 'el acceso directo del escritorio sin condicion (se crearia siempre)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*componente DesktopShortcut*Condition*DESKTOPSHORTCUT*')
    $path = New-Mutant 'component-cond-exe' @('UPDATE `Component` SET `Condition`=''DESKTOPSHORTCUT="1"'' WHERE `Component`=''TeselaExe''')
    Check 'tesela.exe con una condicion (no se instalaria sin la casilla)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*componente TeselaExe*Condition*')
    $path = New-Mutant 'component-dir' @('UPDATE `Component` SET `Directory_`=''DesktopFolder'' WHERE `Component`=''TeselaExe''')
    Check 'tesela.exe en el escritorio' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*componente TeselaExe en DesktopFolder*')
    $path = New-Mutant 'component-attr' @('UPDATE `Component` SET `Attributes`=4 WHERE `Component`=''TeselaExe''')
    Check 'componente de 32 bits' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*componente TeselaExe*Attributes*260*')

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

    # --- LaunchCondition: las CUATRO exactas, cada una con mensaje (Condition es clave primaria: se borra y se inserta) ---------
    $lcMsg = 'Mensaje de prueba'
    $path = New-Mutant 'lc-nonot' @("DELETE FROM ``LaunchCondition`` WHERE ``Condition``='NOT WIX_DOWNGRADE_DETECTED'", "INSERT INTO ``LaunchCondition`` (``Condition``, ``Description``) VALUES ('WIX_DOWNGRADE_DETECTED', '$lcMsg')")
    Check 'LaunchCondition sin NOT (bloquearia a quien NO instala una anterior)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*exactamente estas 4 LaunchCondition*NOT WIX_DOWNGRADE_DETECTED*')
    $path = New-Mutant 'lc-other' @("DELETE FROM ``LaunchCondition`` WHERE ``Condition``='NOT WIX_DOWNGRADE_DETECTED'", "INSERT INTO ``LaunchCondition`` (``Condition``, ``Description``) VALUES ('NOT WIX_UPGRADE_DETECTED', '$lcMsg')")
    Check 'LaunchCondition sobre otra propiedad' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*exactamente estas 4 LaunchCondition*')
    $path = New-Mutant 'lc-none' @("DELETE FROM ``LaunchCondition``")
    Check 'sin LaunchCondition' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*exactamente estas 4 LaunchCondition*')
    $path = New-Mutant 'lc-extra' @("INSERT INTO ``LaunchCondition`` (``Condition``, ``Description``) VALUES ('1=1', '$lcMsg')")
    Check 'una LaunchCondition de mas' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*exactamente estas 4 LaunchCondition*')
    $path = New-Mutant 'lc-nomsg' @('UPDATE `LaunchCondition` SET `Description`='' '' WHERE `Condition`=''NOT WIX_DOWNGRADE_DETECTED''')
    Check 'LaunchCondition con el mensaje en blanco' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*LaunchCondition*no tiene mensaje*')
    # Las tres del ambito: borrar una cualquiera (Condition es clave: se borra por su valor exacto; MSI SQL no tiene LIKE).
    $lcScope = @(
        @('el cambio de ambito de por usuario a para todos', $MsiExpectedLaunchConditions[1]),
        @('el cambio de ambito de para todos a por usuario', $MsiExpectedLaunchConditions[2]),
        @('la carpeta protegida en una instalacion por usuario', $MsiExpectedLaunchConditions[3])
    )
    foreach ($c in $lcScope) {
        $path = New-Mutant ('lc-del-' + [guid]::NewGuid().ToString('N').Substring(0, 6)) @("DELETE FROM ``LaunchCondition`` WHERE ``Condition``='$($c[1])'")
        Check ('sin la LaunchCondition de ' + $c[0]) (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*exactamente estas 4 LaunchCondition*')
    }
    # Que la condicion deje pasar la interfaz completa y las reparaciones: sin "UILevel>=5" la condicion ya no es la exacta.
    $noUi = $MsiExpectedLaunchConditions[1].Replace(' OR UILevel>=5', '')
    $path = New-Mutant 'lc-nouilevel' @("DELETE FROM ``LaunchCondition`` WHERE ``Condition``='$($MsiExpectedLaunchConditions[1])'", "INSERT INTO ``LaunchCondition`` (``Condition``, ``Description``) VALUES ('$noUi', '$lcMsg')")
    Check 'LaunchCondition de cambio de ambito sin dejar pasar la interfaz completa' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*exactamente estas 4 LaunchCondition*')

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
    # FindRelatedProducts ANTES de LaunchConditions en las dos secuencias (si no, la propiedad esta vacia cuando se evalua la
    # condicion y el bloqueo no hace nada, aunque todas las filas esten).
    foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
        $path = New-Mutant "seq-order-frp-after-$t" @("UPDATE ``$t`` SET ``Sequence``=200 WHERE ``Action``='FindRelatedProducts'")
        Check "FindRelatedProducts (200) despues de LaunchConditions (100) en $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*FindRelatedProducts*antes de LaunchConditions*$t*")
        $path = New-Mutant "seq-order-lc-before-$t" @("UPDATE ``$t`` SET ``Sequence``=10 WHERE ``Action``='LaunchConditions'")
        Check "LaunchConditions (10) antes de FindRelatedProducts (25) en $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*FindRelatedProducts*antes de LaunchConditions*$t*")
        $path = New-Mutant "seq-order-equal-$t" @("UPDATE ``$t`` SET ``Sequence``=100 WHERE ``Action``='FindRelatedProducts'")
        Check "FindRelatedProducts y LaunchConditions con el mismo numero de secuencia en $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*FindRelatedProducts*antes de LaunchConditions*$t*")
    }

    # Identificadores y propiedades DISTINGUEN mayusculas (Windows Installer lo hace asi).
    $path = New-Mutant 'case-parent' @("UPDATE ``Directory`` SET ``Directory_Parent``='targetdir' WHERE ``Directory``='INSTALLFOLDER'")
    Check 'Directory_Parent en minusculas (targetdir)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*carpeta INSTALLFOLDER*Directory_Parent*')
    $path = New-Mutant 'case-actionproperty' @($delDown, (Down-Row $version '1033' 2 'wix_downgrade_detected'))
    Check 'ActionProperty del bloqueo en minusculas (wix_downgrade_detected)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*falta la fila de Upgrade*WIX_DOWNGRADE_DETECTED*')
    $path = New-Mutant 'case-launchcondition' @("DELETE FROM ``LaunchCondition`` WHERE ``Condition``='NOT WIX_DOWNGRADE_DETECTED'", "INSERT INTO ``LaunchCondition`` (``Condition``, ``Description``) VALUES ('NOT wix_downgrade_detected', '$lcMsg')")
    Check 'LaunchCondition con la propiedad en minusculas' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*exactamente estas 4 LaunchCondition*')
    $path = New-Mutant 'case-secure' @("UPDATE ``Property`` SET ``Value``='ALLUSERS;DESKTOPSHORTCUT;INSTALLFOLDER;MSIINSTALLPERUSER;wix_upgrade_detected;wix_downgrade_detected' WHERE ``Property``='SecureCustomProperties'")
    Check 'SecureCustomProperties en minusculas' ((Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*SecureCustomProperties no incluye WIX_DOWNGRADE_DETECTED*') -and (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*SecureCustomProperties no incluye WIX_UPGRADE_DETECTED*'))

    # --- Propiedades seguras: EXACTAMENTE las esperadas (INSTALLFOLDER y DESKTOPSHORTCUT tienen que ser seguras) -----------------
    $secureAll = 'ALLUSERS;DESKTOPSHORTCUT;INSTALLFOLDER;MSIINSTALLPERUSER;WIX_DOWNGRADE_DETECTED;WIX_UPGRADE_DETECTED'
    $path = New-Mutant 'secure-no-installfolder' @("UPDATE ``Property`` SET ``Value``='ALLUSERS;DESKTOPSHORTCUT;MSIINSTALLPERUSER;WIX_DOWNGRADE_DETECTED;WIX_UPGRADE_DETECTED' WHERE ``Property``='SecureCustomProperties'")
    Check 'INSTALLFOLDER sin ser propiedad segura (una instalacion para todos elevada la ignoraria)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*SecureCustomProperties no incluye INSTALLFOLDER*')
    $path = New-Mutant 'secure-no-desktop' @("UPDATE ``Property`` SET ``Value``='ALLUSERS;INSTALLFOLDER;MSIINSTALLPERUSER;WIX_DOWNGRADE_DETECTED;WIX_UPGRADE_DETECTED' WHERE ``Property``='SecureCustomProperties'")
    Check 'DESKTOPSHORTCUT sin ser propiedad segura' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*SecureCustomProperties no incluye DESKTOPSHORTCUT*')
    $path = New-Mutant 'secure-no-allusers' @("UPDATE ``Property`` SET ``Value``='DESKTOPSHORTCUT;INSTALLFOLDER;MSIINSTALLPERUSER;WIX_DOWNGRADE_DETECTED;WIX_UPGRADE_DETECTED' WHERE ``Property``='SecureCustomProperties'")
    Check 'ALLUSERS sin ser propiedad segura' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*SecureCustomProperties no incluye ALLUSERS*')
    $path = New-Mutant 'secure-extra' @("UPDATE ``Property`` SET ``Value``='$secureAll;TESELA_LANG' WHERE ``Property``='SecureCustomProperties'")
    Check 'una propiedad segura de mas' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*SecureCustomProperties incluye TESELA_LANG*')
    $path = New-Mutant 'secure-props' @("UPDATE ``Property`` SET ``Value``='WIX_UPGRADE_DETECTED' WHERE ``Property``='SecureCustomProperties'")
    Check 'SecureCustomProperties sin WIX_DOWNGRADE_DETECTED' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*SecureCustomProperties no incluye WIX_DOWNGRADE_DETECTED*')
    $path = New-Mutant 'default-installfolder' @('INSERT INTO `Property` (`Property`, `Value`) VALUES (''INSTALLFOLDER'', ''C:\Fija\'')')
    Check 'INSTALLFOLDER con valor por defecto en la tabla Property (pisaria el calculo por ambito)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*INSTALLFOLDER tiene un valor por defecto*')
    $path = New-Mutant 'default-desktop' @('INSERT INTO `Property` (`Property`, `Value`) VALUES (''DESKTOPSHORTCUT'', ''1'')')
    Check 'DESKTOPSHORTCUT marcada por defecto' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*DESKTOPSHORTCUT tiene un valor por defecto*')
    $path = New-Mutant 'default-scope' @('UPDATE `Property` SET `Value`=''machine'' WHERE `Property`=''TESELA_SCOPE''')
    Check 'el ambito de la ventana parte en "para todos"' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TESELA_SCOPE por defecto*user*')

    # --- Accesos directos: sin argumentos y con la carpeta de trabajo de la instalacion -----------------------------------------
    $path = New-Mutant 'shortcut-args' @("UPDATE ``Shortcut`` SET ``Arguments``='--demo'")
    Check 'acceso directo con argumentos' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*acceso directo*lleva argumentos*--demo*')
    $path = New-Mutant 'shortcut-args-desktop' @("UPDATE ``Shortcut`` SET ``Arguments``='--demo' WHERE ``Shortcut``='DesktopShortcut'")
    Check 'acceso directo del escritorio con argumentos' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*DesktopShortcut lleva argumentos*--demo*')
    $path = New-Mutant 'shortcut-wkdir' @("UPDATE ``Shortcut`` SET ``WkDir``='ProgramMenuFolder'")
    Check 'acceso directo con otra carpeta de trabajo' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*WkDir*ProgramMenuFolder*INSTALLFOLDER*')
    $path = New-Mutant 'shortcut-nowkdir' @("UPDATE ``Shortcut`` SET ``WkDir``=''")
    Check 'acceso directo sin carpeta de trabajo' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*WkDir*se esperaba*INSTALLFOLDER*')

    # --- GUID de componente fijos ----------------------------------------------------------------------------------------------
    $path = New-Mutant 'component-guid' @("UPDATE ``Component`` SET ``ComponentId``='{11111111-2222-3333-4444-555555555555}' WHERE ``Component``='StartMenuShortcut'")
    Check 'otro GUID en el componente del acceso directo' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*componente StartMenuShortcut*GUID*')
    $path = New-Mutant 'component-guid-exe' @("UPDATE ``Component`` SET ``ComponentId``='{11111111-2222-3333-4444-555555555555}' WHERE ``Component``='TeselaExe'")
    Check 'otro GUID en el componente de tesela.exe' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*componente TeselaExe*GUID*')
    $path = New-Mutant 'component-guid-desktop' @("UPDATE ``Component`` SET ``ComponentId``='{11111111-2222-3333-4444-555555555555}' WHERE ``Component``='DesktopShortcut'")
    Check 'otro GUID en el componente del acceso directo del escritorio' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*componente DesktopShortcut*GUID*')

    # --- AppSearch: la accion, sin condicion y antes de CostFinalize en las dos secuencias; Signature vacia -------------------------
    foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
        $path = New-Mutant "appsearch-none-$t" @("DELETE FROM ``$t`` WHERE ``Action``='AppSearch'")
        Check "sin la accion AppSearch en $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*falta AppSearch en $t*")
        $path = New-Mutant "appsearch-cond-$t" @("UPDATE ``$t`` SET ``Condition``='0' WHERE ``Action``='AppSearch'")
        Check "AppSearch con condicion en $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*AppSearch en $t lleva la condicion*")
        $path = New-Mutant "appsearch-late-$t" @("UPDATE ``$t`` SET ``Sequence``=2000 WHERE ``Action``='AppSearch'")
        Check "AppSearch despues de CostFinalize en $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*AppSearch*tiene que ir antes de CostFinalize*$t*")
    }
    $msiTables = Open-MsiDatabase $Msi
    $hasSignature = $msiTables.Tables -contains 'Signature'
    Close-Msi $msiTables
    $sigStatements = @()
    if (-not $hasSignature) {
        $sigStatements += 'CREATE TABLE `Signature` (`Signature` CHAR(72) NOT NULL, `FileName` CHAR(255) NOT NULL PRIMARY KEY `Signature`)'
    }
    $sigStatements += 'INSERT INTO `Signature` (`Signature`, `FileName`) VALUES (''EvilSearch'', ''evil.dll'')'
    $path = New-Mutant 'signature-row' $sigStatements
    Check 'una fila en la tabla Signature (busqueda de archivo no declarada)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*tabla Signature tiene 1 filas*')

    # --- Filas de las secuencias de la logica del ambito, la carpeta y el idioma ----------------------------------------------
    foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
        $path = New-Mutant "logic-nofolder-$t" @("DELETE FROM ``$t`` WHERE ``Action``='TeselaFolderPrevUser'")
        Check "sin TeselaFolderPrevUser en $t (una actualizacion no conservaria la carpeta)" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*falta TeselaFolderPrevUser en $t*")
        $path = New-Mutant "logic-cond-$t" @("UPDATE ``$t`` SET ``Condition``='1' WHERE ``Action``='TeselaFolderDefaultMachine'")
        Check "TeselaFolderDefaultMachine con otra condicion en $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) "*TeselaFolderDefaultMachine en $t lleva la condicion*")
        $path = New-Mutant "logic-seq-$t" @("UPDATE ``$t`` SET ``Sequence``=105 WHERE ``Action``='TeselaSetProtected64'")
        $p = Test-MsiPackage -Path $path -ExpectedVersion $version
        Check "TeselaSetProtected64 despues de LaunchConditions en $t" ((Has-Problem $p "*TeselaSetProtected64 en $t tiene la secuencia 105*") -and (Has-Problem $p '*TeselaSetProtected64*antes de LaunchConditions*'))
        $path = New-Mutant "logic-beforesearch-$t" @("UPDATE ``$t`` SET ``Sequence``=40 WHERE ``Action``='TeselaFolderPrevMachine'")
        Check "TeselaFolderPrevMachine antes de AppSearch en $t" (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TeselaFolderPrevMachine*despues de AppSearch*')
    }
    $path = New-Mutant 'logic-desktop-exec' @("UPDATE ``InstallExecuteSequence`` SET ``Condition``='DESKTOPSHORTCUT=""""' WHERE ``Action``='TeselaRememberDesktop'")
    Check 'TeselaRememberDesktop en el servidor sin la guarda UILevel (volveria a marcar la casilla que el usuario desmarco)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TeselaRememberDesktop en InstallExecuteSequence lleva la condicion*')
    $path = New-Mutant 'logic-scope-exec' @("INSERT INTO ``InstallExecuteSequence`` (``Action``, ``Condition``, ``Sequence``) VALUES ('TeselaLangEs1', '', 62)")
    Check 'el idioma elegido tambien en la secuencia del servidor' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TeselaLangEs1 esta en InstallExecuteSequence*')
    $path = New-Mutant 'lang-missing-id' @("UPDATE ``InstallUISequence`` SET ``Condition``='TESELA_LANG="""" AND (UserLanguageID=9226 OR UserLanguageID=10250)' WHERE ``Action``='TeselaLangEs2'")
    Check 'las acciones de idioma no cubren todos los LANGID del espanol' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TeselaLangEs1..3 no cubren exactamente*')
    $path = New-Mutant 'lang-extra-id' @("UPDATE ``InstallUISequence`` SET ``Condition``='TESELA_LANG="""" AND (UserLanguageID=1033)' WHERE ``Action``='TeselaLangEs3'")
    Check 'una accion de idioma con un LANGID que no es espanol (1033)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TeselaLangEs1..3 no cubren exactamente*')

    # --- Textos: cada Tx* en ingles tiene su version en espanol y se muestra -------------------------------------------------
    $path = New-Mutant 'tx-no-es' @('DELETE FROM `CustomAction` WHERE `Action`=''TeselaEs_TxNext''')
    Check 'un texto sin version en espanol' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TxNext no tiene su version en espanol*')
    $path = New-Mutant 'tx-no-seqrow' @('DELETE FROM `InstallUISequence` WHERE `Action`=''TeselaEs_TxNext''')
    Check 'un texto en espanol sin fila de secuencia' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*falta TeselaEs_TxNext en InstallUISequence*')
    $path = New-Mutant 'tx-bad-cond' @('UPDATE `InstallUISequence` SET `Condition`=''1'' WHERE `Action`=''TeselaEs_TxNext''')
    Check 'un texto en espanol que se aplica siempre (sin TESELA_LANG="es")' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TeselaEs_TxNext en InstallUISequence lleva la condicion*')
    $path = New-Mutant 'tx-in-exec' @("INSERT INTO ``InstallExecuteSequence`` (``Action``, ``Condition``, ``Sequence``) VALUES ('TeselaEs_TxNext', '', 63)")
    Check 'un texto en espanol en la secuencia del servidor' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TeselaEs_TxNext esta en InstallExecuteSequence*')
    $path = New-Mutant 'tx-bad-seq' @('UPDATE `InstallUISequence` SET `Sequence`=1500 WHERE `Action`=''TeselaEs_TxNext''')
    Check 'un texto en espanol despues de las ventanas' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TeselaEs_TxNext*antes de las ventanas*')
    $path = New-Mutant 'tx-brackets' @('UPDATE `Property` SET `Value`=''Siguiente [INSTALLFOLDER]'' WHERE `Property`=''TxNext''')
    Check 'un texto con una propiedad dentro (no se vuelve a formatear)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TxNext contiene corchetes*')
    $path = New-Mutant 'tx-es-brackets' @('UPDATE `CustomAction` SET `Target`=''Siguiente [INSTALLFOLDER]'' WHERE `Action`=''TeselaEs_TxNext''')
    Check 'un texto en espanol con una propiedad dentro' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*texto en espanol de TxNext contiene corchetes*')
    $path = New-Mutant 'tx-es-empty' @('UPDATE `CustomAction` SET `Target`='' '' WHERE `Action`=''TeselaEs_TxNext''')
    Check 'un texto en espanol vacio' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*TeselaEs_TxNext tiene que ser de tipo 51*con texto*')
    $path = New-Mutant 'tx-orphan' @('INSERT INTO `Property` (`Property`, `Value`) VALUES (''TxHuerfano'', ''Texto'')')
    $p = Test-MsiPackage -Path $path -ExpectedVersion $version
    Check 'un texto que no se muestra en ninguna ventana' ((Has-Problem $p '*TxHuerfano no se muestra*') -and (Has-Problem $p '*TxHuerfano no tiene su version en espanol*'))
    $path = New-Mutant 'tx-es-orphan' @("INSERT INTO ``CustomAction`` ($caCols) VALUES ('TeselaEs_TxSinIngles', 51, 'TxSinIngles', 'Texto')")
    Check 'una accion de texto sin propiedad en ingles' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*accion de texto sin propiedad en ingles*TeselaEs_TxSinIngles*')
    $path = New-Mutant 'tx-missing' @('UPDATE `Control` SET `Text`=''[TxNoExiste]'' WHERE `Dialog_`=''WelcomeDlg'' AND `Control`=''Body''')
    Check 'una ventana muestra un texto que no existe' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*muestra el texto [[]TxNoExiste]*')

    # --- La interfaz: ventanas, controles sin imagen y solo eventos nativos --------------------------------------------------
    $path = New-Mutant 'ui-extra-dialog' @('INSERT INTO `Dialog` (`Dialog`, `HCentering`, `VCentering`, `Width`, `Height`, `Attributes`, `Title`, `Control_First`) VALUES (''EvilDlg'', 50, 50, 100, 100, 3, ''x'', ''Ok'')')
    Check 'una ventana de mas' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*ventana no esperada*EvilDlg*')
    $path = New-Mutant 'ui-missing-dialog' @('DELETE FROM `Dialog` WHERE `Dialog`=''InvalidDirDlg''')
    Check 'falta una ventana' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*falta la ventana InvalidDirDlg*')
    $path = New-Mutant 'ui-bitmap' @('UPDATE `Control` SET `Type`=''Bitmap'' WHERE `Dialog_`=''WelcomeDlg'' AND `Control`=''Body''')
    Check 'un control de imagen (necesita Binary)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*control no permitido*WelcomeDlg.Body*Bitmap*')
    $path = New-Mutant 'ui-icon' @('UPDATE `Control` SET `Type`=''Icon'' WHERE `Dialog_`=''WelcomeDlg'' AND `Control`=''Body''')
    Check 'un control de icono' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*control no permitido*WelcomeDlg.Body*Icon*')
    $evCols = '`Dialog_`, `Control_`, `Event`, `Argument`, `Condition`, `Ordering`'
    $path = New-Mutant 'ui-doaction' @("INSERT INTO ``ControlEvent`` ($evCols) VALUES ('WelcomeDlg', 'Next', 'DoAction', 'TeselaFolderPrevUser', '1', 5)")
    Check 'un evento DoAction (llamaria a una accion personalizada)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*evento de control no permitido*WelcomeDlg.Next*DoAction*')
    $path = New-Mutant 'ui-badprop' @("INSERT INTO ``ControlEvent`` ($evCols) VALUES ('WelcomeDlg', 'Next', '[SecureCustomProperties]', 'x', '1', 5)")
    Check 'una ventana que cambia una propiedad que no debe' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*propiedad no permitida*SecureCustomProperties*')
    $path = New-Mutant 'ui-newdialog-missing' @("INSERT INTO ``ControlEvent`` ($evCols) VALUES ('WelcomeDlg', 'Next', 'NewDialog', 'NoExisteDlg', '1', 5)")
    Check 'un NewDialog a una ventana que no existe' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*abre la ventana*NoExisteDlg*no existe*')
    $path = New-Mutant 'ui-settargetpath' @("INSERT INTO ``ControlEvent`` ($evCols) VALUES ('WelcomeDlg', 'Next', 'SetTargetPath', 'ProgramMenuFolder', '1', 5)")
    Check 'un SetTargetPath sobre otra carpeta' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*SetTargetPath sobre*ProgramMenuFolder*')
    $path = New-Mutant 'ui-enddialog' @("INSERT INTO ``ControlEvent`` ($evCols) VALUES ('WelcomeDlg', 'Next', 'EndDialog', 'Raro', '1', 5)")
    Check 'un EndDialog con un valor que no es de la lista' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*EndDialog con el valor*Raro*')
    $path = New-Mutant 'ui-scope-missing-row' @('DELETE FROM `ControlEvent` WHERE `Dialog_`=''ScopeDlg'' AND `Control_`=''Next'' AND `Argument`=''[TESELA_PREV_USER]''')
    $p = Test-MsiPackage -Path $path -ExpectedVersion $version
    Check 'falta la fila de la ventana de ambito que conserva la carpeta de la instalacion por usuario' ((Has-Problem $p '*falta el evento de la interfaz*TESELA_PREV_USER*') -and (Has-Problem $p '*ScopeDlg.Next tiene que tener 9 eventos*'))
    $path = New-Mutant 'ui-scope-cond' @('DELETE FROM `ControlEvent` WHERE `Dialog_`=''ScopeDlg'' AND `Control_`=''Next'' AND `Argument`=''[TESELA_DEFAULT_USER]''', "INSERT INTO ``ControlEvent`` ($evCols) VALUES ('ScopeDlg', 'Next', '[INSTALLFOLDER]', '[TESELA_DEFAULT_USER]', '1', 8)")
    Check 'la fila de la carpeta por defecto por usuario pisaria la carpeta personalizada (sin condicion)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*falta el evento de la interfaz*TESELA_DEFAULT_USER*')
    $path = New-Mutant 'ui-scope-allusers' @('DELETE FROM `ControlEvent` WHERE `Dialog_`=''ScopeDlg'' AND `Control_`=''Next'' AND `Event`=''[ALLUSERS]'' AND `Argument`=''1''', "INSERT INTO ``ControlEvent`` ($evCols) VALUES ('ScopeDlg', 'Next', '[ALLUSERS]', '0', 'TESELA_SCOPE=""machine""', 1)")
    Check 'la ventana de ambito deja de poner ALLUSERS=1 para todos' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*falta el evento de la interfaz*ALLUSERS*')
    $path = New-Mutant 'ui-folder-notarget' @('DELETE FROM `ControlEvent` WHERE `Dialog_`=''FolderDlg'' AND `Control_`=''Next'' AND `Event`=''SetTargetPath''')
    Check 'FolderDlg.Next sin SetTargetPath' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*FolderDlg.Next tiene que hacer SetTargetPath*')
    $path = New-Mutant 'ui-folder-novalidate' @('DELETE FROM `ControlEvent` WHERE `Dialog_`=''FolderDlg'' AND `Control_`=''Next'' AND `Event`=''SpawnDialog''')
    Check 'FolderDlg.Next sin la ventana de carpeta no valida' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*FolderDlg.Next tiene que hacer SetTargetPath*')
    $path = New-Mutant 'ui-scope-nodisable' @('DELETE FROM `ControlCondition` WHERE `Dialog_`=''ScopeDlg'' AND `Control_`=''Next'' AND `Action`=''Disable''')
    Check 'la ventana de ambito deja pulsar Siguiente aunque el ambito choque con la instalacion existente' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*ScopeDlg.Next tiene que desactivarse*')
    $path = New-Mutant 'ui-browse-prop' @('UPDATE `Control` SET `Property`=''INSTALLFOLDER'' WHERE `Dialog_`=''BrowseDlg'' AND `Control`=''DirectoryList''')
    Check 'el dialogo de examinar trabaja sobre otra propiedad' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*BrowseDlg.DirectoryList*_BrowseProperty*')
    $path = New-Mutant 'ui-folderedit' @('UPDATE `Control` SET `Property`=''ALLUSERS'' WHERE `Dialog_`=''FolderDlg'' AND `Control`=''FolderEdit''')
    Check 'el campo de carpeta no es sobre INSTALLFOLDER' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*FolderDlg.FolderEdit tiene que ser un PathEdit sobre INSTALLFOLDER*')
    $path = New-Mutant 'ui-checkbox' @('UPDATE `CheckBox` SET `Value`=''2'' WHERE `Property`=''DESKTOPSHORTCUT''')
    Check 'la casilla del escritorio con otro valor' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*CheckBox*DESKTOPSHORTCUT*')
    $path = New-Mutant 'ui-errordialog' @('UPDATE `Property` SET `Value`=''WelcomeDlg'' WHERE `Property`=''ErrorDialog''')
    Check 'la propiedad ErrorDialog apunta a otra ventana' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*ErrorDialog*ErrorDlg*')
    $path = New-Mutant 'ui-radio-extra' @('INSERT INTO `RadioButton` (`Property`, `Order`, `Value`, `X`, `Y`, `Width`, `Height`, `Text`) VALUES (''TESELA_SCOPE'', 3, ''otro'', 0, 80, 100, 16, ''x'')')
    Check 'una tercera opcion de ambito' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*opciones ''user'' y ''machine''*')
    $path = New-Mutant 'ui-eventmapping' @('INSERT INTO `EventMapping` (`Dialog_`, `Control_`, `Event`, `Attribute`) VALUES (''WelcomeDlg'', ''Body'', ''ActionText'', ''Text'')')
    Check 'una suscripcion de evento de mas' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*suscripcion de evento no esperada*WelcomeDlg*')
    $path = New-Mutant 'ui-actiontext' @('INSERT INTO `ActionText` (`Action`, `Description`) VALUES (''EvilAction'', ''x'')')
    Check 'texto de progreso de una accion no esperada' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*texto de progreso*EvilAction*')
    $path = New-Mutant 'ui-exit-seq' @("UPDATE ``AdminUISequence`` SET ``Sequence``=-5 WHERE ``Action``='FatalError'")
    Check 'FatalError mal colocada en AdminUISequence (ICE20)' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*FatalError con la secuencia -3 en AdminUISequence*')
    $path = New-Mutant 'ui-welcome-cond' @("UPDATE ``InstallUISequence`` SET ``Condition``='' WHERE ``Action``='WelcomeDlg'")
    Check 'la bienvenida se mostraria tambien al reparar o desinstalar' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*WelcomeDlg en InstallUISequence lleva la condicion*')
    $path = New-Mutant 'ui-progress-late' @("UPDATE ``InstallUISequence`` SET ``Sequence``=1400 WHERE ``Action``='ProgressDlg'")
    Check 'ProgressDlg despues de ExecuteAction' (Has-Problem (Test-MsiPackage -Path $path -ExpectedVersion $version) '*ProgressDlg tiene que ir antes de ExecuteAction*')

    # --- Un error de la API no es una tabla vacia ------------------------------------------------------------------------
    $m = Open-MsiDatabase $Msi
    try {
        Check 'una tabla que no existe da cero filas' (@(Get-MsiRows $m 'SELECT `Action` FROM `NoExisteTabla`' 1).Count -eq 0)
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
# Salida EXPLICITA: un comando externo anterior (dotnet, git, wix...) deja su codigo en $LASTEXITCODE y GitHub Actions (pwsh -command)
# lo usa como codigo del paso aunque aqui todo haya ido bien.
exit 0
