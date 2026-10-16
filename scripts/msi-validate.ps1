<#
.SYNOPSIS
  Lectura (SOLO LECTURA) y validacion de las tablas del MSI de Tesela con la API de Windows Installer.

.DESCRIPTION
  Funciones para dot-source (`. scripts/msi-validate.ps1`) desde scripts/build-msi.ps1 y desde
  scripts/tests/msi-validate.tests.ps1 (que las prueba con MSI manipulados, en %TEMP% y sin instalarlos). No usa CIM ni WMI:
  solo el objeto COM WindowsInstaller.Installer, como Orca.

  Test-MsiPackage devuelve la lista de problemas (vacia si el MSI es el esperado):
    * las tablas del MSI (`_Tables`) son EXACTAMENTE una lista cerrada ($MsiAllowedTables): cualquier tabla de mas (Binary,
      CustomAction, Environment, ServiceInstall, Class, Extension, ProgId, Verb, TypeLib, MIME, IniFile, ...) es un fallo;
    * version, UpgradeCode fijo, nombre y editor, propiedades de Aplicaciones instaladas;
    * por usuario: sin ALLUSERS (o ALLUSERS=2 con MSIINSTALLPERUSER=1) y bit "no hace falta elevar" del Summary Information;
    * exactamente los 5 archivos esperados y componentes solo en la carpeta de instalacion y el menu Inicio;
    * registro solo en HKCU\Software\Tesela; carpetas solo del perfil del usuario; RemoveFile solo con las dos filas
      esperadas y sin nombre de archivo; busqueda de registro de InstallFolder de tipo 18;
    * un acceso directo del menu Inicio, un icono, MajorUpgrade y bloqueo de versiones anteriores.

  El script es solo ASCII a proposito (Windows PowerShell 5.1 lee los .ps1 sin BOM como ANSI).
#>

# UpgradeCode de installer/Tesela.wxs: NO CAMBIAR NUNCA (ver installer/README.md). Aqui se repite para que un cambio
# accidental en el .wxs rompa la validacion.
$MsiExpectedUpgradeCode = '{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}'
$MsiExpectedFiles = @('tesela.exe', 'tesela-scan.exe', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md')
# Lista CERRADA de tablas de un MSI de Tesela. Una tabla que no esta aqui es un fallo, tenga o no filas.
$MsiAllowedTables = @(
    '_Validation', 'AdminExecuteSequence', 'AdminUISequence', 'AdvtExecuteSequence', 'AppSearch', 'Component', 'Directory', 'Feature',
    'FeatureComponents', 'File', 'Icon', 'InstallExecuteSequence', 'InstallUISequence', 'LaunchCondition', 'Media',
    'MsiFileHash', 'Property', 'RegLocator', 'Registry', 'RemoveFile', 'Shortcut', 'Signature', 'Upgrade'
)

function Open-MsiDatabase {
    # Abre el MSI. $Write = $false: solo lectura (lo que usa build-msi.ps1). Las pruebas abren COPIAS en modo escritura.
    param([string]$Path, [bool]$Write = $false)
    $installer = New-Object -ComObject WindowsInstaller.Installer
    # 0 = msiOpenDatabaseModeReadOnly, 1 = msiOpenDatabaseModeTransact
    $mode = if ($Write) { 1 } else { 0 }
    $db = [System.__ComObject].InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Path, $mode))
    $obj = [pscustomobject]@{ Installer = $installer; Db = $db; Tables = $null }
    $names = New-Object System.Collections.Generic.List[string]
    foreach ($r in (Get-MsiRows $obj 'SELECT `Name` FROM `_Tables`' 1 -NoTableCheck)) { $names.Add($r[0]) }
    $obj.Tables = $names.ToArray()
    return $obj
}

function Get-MsiRows {
    # Devuelve las filas (cada una, un array de cadenas) de la consulta. Una tabla que NO existe en el MSI (no figura en
    # `_Tables`) da cero filas; CUALQUIER otro error de la API se propaga: un fallo de lectura no puede pasar por "tabla
    # vacia".
    param($Msi, [string]$Sql, [int]$Columns, [switch]$NoTableCheck)
    if (-not $NoTableCheck) {
        $m = [regex]::Match($Sql, 'FROM\s+`([^`]+)`')
        if (-not $m.Success) { throw "consulta sin tabla: $Sql" }
        if ($Msi.Tables -notcontains $m.Groups[1].Value) { return @() }
    }
    $rows = New-Object System.Collections.Generic.List[object]
    $view = [System.__ComObject].InvokeMember('OpenView', 'InvokeMethod', $null, $Msi.Db, @($Sql))
    [System.__ComObject].InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    while ($true) {
        $rec = [System.__ComObject].InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
        if ($null -eq $rec) { break }
        $vals = @()
        for ($i = 1; $i -le $Columns; $i++) {
            $vals += [string][System.__ComObject].InvokeMember('StringData', 'GetProperty', $null, $rec, @($i))
        }
        $rows.Add($vals)
        [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($rec)
    }
    [System.__ComObject].InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($view)
    return $rows.ToArray()
}

function Get-MsiSummaryProperty($Msi, [int]$Id) {
    $si = [System.__ComObject].InvokeMember('SummaryInformation', 'GetProperty', $null, $Msi.Db, @(0))
    $v = [System.__ComObject].InvokeMember('Property', 'GetProperty', $null, $si, @($Id))
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($si)
    return $v
}

function Close-Msi($Msi) {
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($Msi.Db)
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($Msi.Installer)
    [System.GC]::Collect()
    [System.GC]::WaitForPendingFinalizers()
}

function Test-MsiPackage {
    # Devuelve la lista de problemas del MSI (vacia si esta bien).
    param([string]$Path, [string]$ExpectedVersion)
    $problems = New-Object System.Collections.Generic.List[string]
    $msi = Open-MsiDatabase $Path
    try {
        # Tablas: lista cerrada.
        foreach ($t in $msi.Tables) {
            if ($MsiAllowedTables -notcontains $t) { $problems.Add("tabla no permitida en el MSI: $t") }
        }

        $props = @{}
        foreach ($r in (Get-MsiRows $msi 'SELECT `Property`, `Value` FROM `Property`' 2)) { $props[$r[0]] = $r[1] }
        if ($props['ProductVersion'] -ne $ExpectedVersion) { $problems.Add("ProductVersion = '$($props['ProductVersion'])', se esperaba '$ExpectedVersion'") }
        if ($props['UpgradeCode'] -ne $MsiExpectedUpgradeCode) { $problems.Add("UpgradeCode = '$($props['UpgradeCode'])': NO se puede cambiar (installer/README.md)") }
        if ($props['ProductName'] -ne 'Tesela') { $problems.Add("ProductName = '$($props['ProductName'])'") }
        if ($props['Manufacturer'] -ne 'Tesela contributors') { $problems.Add("Manufacturer = '$($props['Manufacturer'])'") }
        foreach ($p in 'ARPPRODUCTICON', 'ARPURLINFOABOUT') {
            if (-not $props[$p]) { $problems.Add("falta la propiedad $p (Aplicaciones instaladas)") }
        }
        if ($props['ARPURLINFOABOUT'] -ne 'https://github.com/wandoth1/tesela') { $problems.Add("ARPURLINFOABOUT = '$($props['ARPURLINFOABOUT'])'") }

        # Por usuario: sin ALLUSERS (o ALLUSERS=2 con MSIINSTALLPERUSER=1, segun la version de WiX) y, en el Summary
        # Information, el bit 3 del contador de palabras ("no hace falta elevar").
        $allUsers = $props['ALLUSERS']
        $perUserProps = (-not $allUsers) -or ($allUsers -eq '2' -and $props['MSIINSTALLPERUSER'] -eq '1')
        if (-not $perUserProps) { $problems.Add("no es por usuario: ALLUSERS='$allUsers' MSIINSTALLPERUSER='$($props['MSIINSTALLPERUSER'])'") }
        $wordCount = [int](Get-MsiSummaryProperty $msi 15)
        if (($wordCount -band 8) -eq 0) { $problems.Add("el Summary Information no declara 'no hace falta elevar' (contador de palabras = $wordCount)") }

        $files = @(Get-MsiRows $msi 'SELECT `File`, `FileName`, `FileSize` FROM `File`' 3)
        $longNames = @($files | ForEach-Object { ($_[1] -split '\|')[-1] })
        foreach ($n in $MsiExpectedFiles) { if ($longNames -notcontains $n) { $problems.Add("falta el archivo $n") } }
        foreach ($n in $longNames) { if ($MsiExpectedFiles -notcontains $n) { $problems.Add("archivo inesperado en el MSI: $n") } }
        if ($files.Count -ne $MsiExpectedFiles.Count) { $problems.Add("el MSI lleva $($files.Count) archivos, se esperaban $($MsiExpectedFiles.Count)") }

        $ca = @(Get-MsiRows $msi 'SELECT `Action`, `Type` FROM `CustomAction`' 2)
        if ($ca.Count -gt 0) { $problems.Add('el MSI lleva acciones personalizadas: ' + (($ca | ForEach-Object { $_[0] }) -join ', ')) }

        $reg = @(Get-MsiRows $msi 'SELECT `Registry`, `Root`, `Key` FROM `Registry`' 3)
        foreach ($r in $reg) {
            # 1 = HKEY_CURRENT_USER
            if ($r[1] -ne '1' -or $r[2] -notlike 'Software\Tesela*') { $problems.Add("registro fuera de HKCU\Software\Tesela: raiz $($r[1]), clave '$($r[2])'") }
        }
        if ($reg.Count -eq 0) { $problems.Add('no hay valores de registro (los componentes por usuario los necesitan como KeyPath)') }

        $allowedDirs = @('TARGETDIR', 'LocalAppDataFolder', 'ProgramsFolder', 'INSTALLFOLDER', 'ProgramMenuFolder')
        foreach ($d in (Get-MsiRows $msi 'SELECT `Directory`, `Directory_Parent` FROM `Directory`' 2)) {
            if ($allowedDirs -notcontains $d[0]) { $problems.Add("carpeta fuera del perfil del usuario: $($d[0])") }
        }
        foreach ($c in (Get-MsiRows $msi 'SELECT `Component`, `Directory_` FROM `Component`' 2)) {
            if (@('INSTALLFOLDER', 'ProgramMenuFolder') -notcontains $c[1]) { $problems.Add("componente $($c[0]) en $($c[1])") }
        }

        $sc = @(Get-MsiRows $msi 'SELECT `Shortcut`, `Directory_`, `Name`, `Target`, `Icon_` FROM `Shortcut`' 5)
        if ($sc.Count -ne 1 -or $sc[0][1] -ne 'ProgramMenuFolder' -or $sc[0][3] -ne '[INSTALLFOLDER]tesela.exe') {
            $problems.Add("se esperaba exactamente 1 acceso directo en el menu Inicio hacia tesela.exe; hay $($sc.Count)")
        }
        $icons = @(Get-MsiRows $msi 'SELECT `Name` FROM `Icon`' 1)
        if ($icons.Count -ne 1) { $problems.Add("se esperaba 1 icono en el MSI; hay $($icons.Count)") }

        $up = @(Get-MsiRows $msi 'SELECT `UpgradeCode`, `VersionMin`, `VersionMax`, `Attributes`, `ActionProperty` FROM `Upgrade`' 5)
        if (@($up | Where-Object { $_[0] -eq $MsiExpectedUpgradeCode }).Count -lt 2) { $problems.Add('falta MajorUpgrade (tabla Upgrade): una fila para las versiones anteriores y otra para detectar una anterior') }
        $lc = @(Get-MsiRows $msi 'SELECT `Condition`, `Description` FROM `LaunchCondition`' 2)
        if (-not @($lc | Where-Object { $_[0] -like '*WIX_DOWNGRADE_DETECTED*' -and $_[1] })) { $problems.Add('falta el bloqueo de versiones anteriores con su mensaje (LaunchCondition)') }

        # RemoveFile: SOLO las dos filas esperadas (quitar INSTALLFOLDER y ProgramsFolder si quedan vacias), sin nombre de
        # archivo: una fila con FileName (p. ej. *.*) borraria archivos del usuario al desinstalar.
        $rm = @(Get-MsiRows $msi 'SELECT `FileKey`, `Component_`, `FileName`, `DirProperty`, `InstallMode` FROM `RemoveFile`' 5)
        foreach ($r in $rm) {
            if ($r[2] -ne '' -or @('INSTALLFOLDER', 'ProgramsFolder') -notcontains $r[3] -or $r[4] -ne '2') {
                $problems.Add("fila de RemoveFile no esperada: '$($r[0])' FileName='$($r[2])' DirProperty='$($r[3])' InstallMode='$($r[4])'")
            }
        }
        $rmDirs = @($rm | ForEach-Object { $_[3] } | Sort-Object -Unique)
        if ($rm.Count -ne 2 -or $rmDirs.Count -ne 2) { $problems.Add("RemoveFile tiene que tener exactamente 2 filas (INSTALLFOLDER y ProgramsFolder); hay $($rm.Count)") }

        # La carpeta de instalacion se recuerda en HKCU\Software\Tesela\InstallFolder (valor `[INSTALLFOLDER]`) y se recupera
        # con UNA busqueda de registro hacia INSTALLFOLDER (sin accion personalizada): sin eso, al desinstalar un MSI que se
        # instalo en una carpeta personalizada los archivos quedarian huerfanos.
        $loc = @(Get-MsiRows $msi 'SELECT `Signature_`, `Root`, `Key`, `Name`, `Type` FROM `RegLocator`' 5)
        # Type 18 = 2 (valor crudo, raw) + 16 (vista de 64 bits: el paquete es x64).
        if ($loc.Count -ne 1 -or $loc[0][1] -ne '1' -or $loc[0][2] -ne 'Software\Tesela' -or $loc[0][3] -ne 'InstallFolder' -or $loc[0][4] -ne '18') {
            $problems.Add("se esperaba UNA busqueda de registro de HKCU\Software\Tesela\InstallFolder (tipo 18); hay $($loc.Count), tipo '$($loc | ForEach-Object { $_[4] })'")
        }
        $app = @(Get-MsiRows $msi 'SELECT `Property`, `Signature_` FROM `AppSearch`' 2)
        if ($app.Count -ne 1 -or $app[0][0] -ne 'INSTALLFOLDER') { $problems.Add('AppSearch tiene que asignar solo INSTALLFOLDER') }
        $saved = @(Get-MsiRows $msi 'SELECT `Name`, `Value` FROM `Registry`' 2 | Where-Object { $_[0] -eq 'InstallFolder' })
        if ($saved.Count -ne 1 -or $saved[0][1] -ne '[INSTALLFOLDER]') { $problems.Add('falta el valor de registro InstallFolder = [INSTALLFOLDER]') }
    } finally {
        Close-Msi $msi
    }
    return , $problems
}
