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
    * registro solo en HKCU\Software\Tesela; RemoveFile solo con las dos filas esperadas y sin nombre de archivo;
      busqueda de registro de InstallFolder de tipo 18;
    * el arbol de carpetas EXACTO ($MsiExpectedDirectories: cada Directory con su Directory_Parent y, donde se fija el
      nombre, su DefaultDir) y los componentes solo en la carpeta de instalacion y el menu Inicio;
    * un acceso directo del menu Inicio, un icono;
    * MajorUpgrade y bloqueo de versiones anteriores, con valores exactos (ver $MsiExpectedUpgradeRows): las dos filas de la
      tabla Upgrade (UpgradeCode, VersionMin/VersionMax, idioma, Attributes y ActionProperty), la LaunchCondition
      `NOT WIX_DOWNGRADE_DETECTED` con su mensaje, FindRelatedProducts y LaunchConditions sin condicion en las dos
      secuencias de instalacion y RemoveExistingProducts (sin condicion) en InstallExecuteSequence.

  El script es solo ASCII a proposito (Windows PowerShell 5.1 lee los .ps1 sin BOM como ANSI).
#>

# UpgradeCode de installer/Tesela.wxs: NO CAMBIAR NUNCA (ver installer/README.md). Aqui se repite para que un cambio
# accidental en el .wxs rompa la validacion.
$MsiExpectedUpgradeCode = '{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}'
$MsiExpectedFiles = @('tesela.exe', 'tesela-scan.exe', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md')
# Arbol de carpetas EXACTO de un MSI de Tesela (Directory -> Directory_Parent), tal como lo genera installer/Tesela.wxs
# (StandardDirectory LocalAppDataFolder > Directory ProgramsFolder "Programs" > Directory INSTALLFOLDER "Tesela" y
# StandardDirectory ProgramMenuFolder). Todo cuelga de TARGETDIR por el perfil del usuario: nada de ProgramFiles ni de otra
# unidad. DefaultDir solo se fija donde el .wxs pone el nombre ($null = no se comprueba: son los nombres cortos que WiX da a
# las carpetas estandar, `LocalApp`, `PMenu`, `SourceDir`, y no son decision de Tesela).
$MsiExpectedDirectories = @(
    @{ Id = 'TARGETDIR'; Parent = ''; DefaultDir = $null },
    @{ Id = 'LocalAppDataFolder'; Parent = 'TARGETDIR'; DefaultDir = $null },
    @{ Id = 'ProgramsFolder'; Parent = 'LocalAppDataFolder'; DefaultDir = 'Programs' },
    @{ Id = 'INSTALLFOLDER'; Parent = 'ProgramsFolder'; DefaultDir = 'Tesela' },
    @{ Id = 'ProgramMenuFolder'; Parent = 'TARGETDIR'; DefaultDir = $null }
)

# Filas EXACTAS de la tabla Upgrade que genera <MajorUpgrade AllowSameVersionUpgrades="yes"> de installer/Tesela.wxs. `V` es
# la version del producto (ProductVersion del MSI), por eso VersionMin/VersionMax no son una cadena fija. Las dos filas llevan
# UpgradeCode = $MsiExpectedUpgradeCode y Language = 1033 (el idioma del paquete: solo se detectan versiones en ese idioma).
#   * Reemplazar (ActionProperty WIX_UPGRADE_DETECTED): VersionMin vacio y VersionMax = V, Attributes = 513 = 1 + 512:
#       1   msidbUpgradeAttributesMigrateFeatures     conserva la seleccion de caracteristicas de la version vieja;
#       512 msidbUpgradeAttributesVersionMaxInclusive el limite superior V cuenta (AllowSameVersionUpgrades: reinstalar la
#           MISMA version tambien reemplaza; por eso build-msi.ps1 acepta el aviso ICE61).
#     Es decir: toda version <= V se desinstala al instalar esta (RemoveExistingProducts).
#   * Bloquear versiones posteriores (ActionProperty WIX_DOWNGRADE_DETECTED): VersionMin = V y VersionMax vacio, Attributes =
#     2 = msidbUpgradeAttributesOnlyDetect (solo marca la propiedad, no desinstala NADA). Sin el bit 256
#     (VersionMinInclusive) el limite inferior no cuenta: version > V estricta. La LaunchCondition `NOT WIX_DOWNGRADE_DETECTED`
#     convierte esa deteccion en un error con mensaje.
# Ninguna lleva Remove (que borraria caracteristicas concretas) ni el bit 1024 (idiomas excluyentes).
$MsiExpectedUpgradeRows = @(
    @{ Name = 'reemplazar versiones anteriores'; ActionProperty = 'WIX_UPGRADE_DETECTED'; Min = ''; MaxIsVersion = $true; Attributes = '513' },
    @{ Name = 'bloquear versiones posteriores'; ActionProperty = 'WIX_DOWNGRADE_DETECTED'; Min = 'V'; MaxIsVersion = $false; Attributes = '2' }
)
$MsiExpectedLanguage = '1033'
$MsiExpectedLaunchCondition = 'NOT WIX_DOWNGRADE_DETECTED'
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

        # Carpetas: el arbol EXACTO (cada Directory con su Directory_Parent) y, donde el .wxs fija el nombre, su DefaultDir
        # (que puede ser "corto|largo": el nombre largo es el ultimo trozo).
        $dirs = @(Get-MsiRows $msi 'SELECT `Directory`, `Directory_Parent`, `DefaultDir` FROM `Directory`' 3)
        $expectedIds = @($MsiExpectedDirectories | ForEach-Object { $_.Id })
        foreach ($d in $dirs) {
            if ($expectedIds -notcontains $d[0]) { $problems.Add("carpeta fuera del perfil del usuario: $($d[0])") }
        }
        foreach ($e in $MsiExpectedDirectories) {
            $found = @($dirs | Where-Object { $_[0] -eq $e.Id })
            if ($found.Count -ne 1) { $problems.Add("carpeta $($e.Id): hay $($found.Count) filas en la tabla Directory, se esperaba 1"); continue }
            if ($found[0][1] -ne $e.Parent) { $problems.Add("carpeta $($e.Id): Directory_Parent = '$($found[0][1])', se esperaba '$($e.Parent)'") }
            $leafName = ($found[0][2] -split '\|')[-1]
            if ($null -ne $e.DefaultDir -and $leafName -ne $e.DefaultDir) { $problems.Add("carpeta $($e.Id): DefaultDir = '$($found[0][2])', se esperaba '$($e.DefaultDir)'") }
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

        # MajorUpgrade: las dos filas EXACTAS de la tabla Upgrade (ver $MsiExpectedUpgradeRows y su explicacion).
        $up = @(Get-MsiRows $msi 'SELECT `UpgradeCode`, `VersionMin`, `VersionMax`, `Language`, `Attributes`, `Remove`, `ActionProperty` FROM `Upgrade`' 7)
        if ($up.Count -ne $MsiExpectedUpgradeRows.Count) { $problems.Add("la tabla Upgrade tiene $($up.Count) filas, se esperaban $($MsiExpectedUpgradeRows.Count) (MajorUpgrade)") }
        foreach ($e in $MsiExpectedUpgradeRows) {
            $rows = @($up | Where-Object { $_[6] -eq $e.ActionProperty })
            if ($rows.Count -ne 1) { $problems.Add("falta la fila de Upgrade para $($e.Name) (ActionProperty $($e.ActionProperty)): hay $($rows.Count)"); continue }
            $r = $rows[0]
            $min = if ($e.Min -eq 'V') { $ExpectedVersion } else { $e.Min }
            $max = if ($e.MaxIsVersion) { $ExpectedVersion } else { '' }
            if ($r[0] -ne $MsiExpectedUpgradeCode) { $problems.Add("Upgrade ($($e.Name)): UpgradeCode = '$($r[0])'") }
            if ($r[1] -ne $min) { $problems.Add("Upgrade ($($e.Name)): VersionMin = '$($r[1])', se esperaba '$min'") }
            if ($r[2] -ne $max) { $problems.Add("Upgrade ($($e.Name)): VersionMax = '$($r[2])', se esperaba '$max'") }
            if ($r[3] -ne $MsiExpectedLanguage) { $problems.Add("Upgrade ($($e.Name)): Language = '$($r[3])', se esperaba '$MsiExpectedLanguage'") }
            if ($r[4] -ne $e.Attributes) { $problems.Add("Upgrade ($($e.Name)): Attributes = '$($r[4])', se esperaba '$($e.Attributes)'") }
            if ($r[5] -ne '') { $problems.Add("Upgrade ($($e.Name)): Remove = '$($r[5])', se esperaba vacio") }
        }
        # Bloqueo de versiones posteriores: UNA LaunchCondition exacta, con su mensaje.
        $lc = @(Get-MsiRows $msi 'SELECT `Condition`, `Description` FROM `LaunchCondition`' 2)
        if ($lc.Count -ne 1 -or $lc[0][0] -ne $MsiExpectedLaunchCondition) {
            $problems.Add("se esperaba UNA LaunchCondition '$MsiExpectedLaunchCondition'; hay: " + (($lc | ForEach-Object { "'$($_[0])'" }) -join ', '))
        } elseif ([string]::IsNullOrWhiteSpace($lc[0][1])) {
            $problems.Add('la LaunchCondition de versiones posteriores no tiene mensaje')
        }
        # Para que lo anterior SURTA EFECTO: FindRelatedProducts (rellena las propiedades de la tabla Upgrade) y LaunchConditions
        # (evalua la condicion) tienen que estar, sin condicion, en las dos secuencias de instalacion, y RemoveExistingProducts
        # (la desinstalacion de la version anterior) sin condicion en la de ejecucion y entre InstallValidate e InstallInitialize
        # (el programa "afterInstallValidate" de MajorUpgrade).
        $seq = @{}
        foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
            $seq[$t] = @{}
            foreach ($r in (Get-MsiRows $msi ('SELECT `Action`, `Condition`, `Sequence` FROM `' + $t + '`') 3)) { $seq[$t][$r[0]] = $r }
        }
        foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
            foreach ($a in 'FindRelatedProducts', 'LaunchConditions') {
                if (-not $seq[$t].ContainsKey($a)) { $problems.Add("falta $a en $t (sin el, el bloqueo o la deteccion de versiones no se evalua)") }
                elseif ($seq[$t][$a][1] -ne '') { $problems.Add("$a en ${t} lleva la condicion '$($seq[$t][$a][1])' (tiene que ser incondicional)") }
            }
        }
        $ies = $seq['InstallExecuteSequence']
        if (-not $ies.ContainsKey('RemoveExistingProducts')) {
            $problems.Add('falta RemoveExistingProducts en InstallExecuteSequence (MajorUpgrade no desinstalaria la version anterior)')
        } else {
            $rep = $ies['RemoveExistingProducts']
            if ($rep[1] -ne '') { $problems.Add("RemoveExistingProducts lleva la condicion '$($rep[1])' (tiene que ser incondicional)") }
            $lo = if ($ies.ContainsKey('InstallValidate')) { [int]$ies['InstallValidate'][2] } else { $null }
            $hi = if ($ies.ContainsKey('InstallInitialize')) { [int]$ies['InstallInitialize'][2] } else { $null }
            if ($null -eq $lo -or $null -eq $hi -or [int]$rep[2] -le $lo -or [int]$rep[2] -ge $hi) {
                $problems.Add("RemoveExistingProducts (secuencia $($rep[2])) tiene que ir entre InstallValidate e InstallInitialize")
            }
        }
        # Las dos propiedades que rellena la tabla Upgrade tienen que figurar en SecureCustomProperties (propiedades publicas
        # restringidas: sin ello Windows Installer no las conserva al pasar de la fase de interfaz a la de ejecucion).
        $secure = @(($props['SecureCustomProperties'] -split ';') | Where-Object { $_ })
        foreach ($a in 'WIX_UPGRADE_DETECTED', 'WIX_DOWNGRADE_DETECTED') {
            if ($secure -notcontains $a) { $problems.Add("SecureCustomProperties no incluye $a (tiene: '$($props['SecureCustomProperties'])')") }
        }

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
