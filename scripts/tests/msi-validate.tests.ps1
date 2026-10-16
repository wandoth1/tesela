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
    $view = [System.__ComObject].InvokeMember('OpenView', 'InvokeMethod', $null, $db, @($Sql))
    [System.__ComObject].InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
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
