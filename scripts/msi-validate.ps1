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
    * exactamente los 5 archivos esperados;
    * registro solo en HKCU\Software\Tesela; RemoveFile solo con las dos filas esperadas y sin nombre de archivo;
      busqueda de registro de InstallFolder de tipo 18;
    * el arbol de carpetas EXACTO ($MsiExpectedDirectories: cada Directory con su Directory_Parent y, donde se fija el
      nombre, su DefaultDir) y los componentes solo en la carpeta de instalacion y el menu Inicio;
    * un acceso directo del menu Inicio (sin Arguments y con WkDir = INSTALLFOLDER), un icono;
    * los GUID fijos de los 6 componentes (Windows Installer identifica el componente por su GUID entre versiones);
    * INSTALLFOLDER en SecureCustomProperties, AppSearch sin condicion y ANTES de CostFinalize en las dos secuencias de
      instalacion, y la tabla Signature vacia (no hay busquedas de archivos ni carpetas);
    * MajorUpgrade y bloqueo de versiones anteriores, con valores exactos (ver $MsiExpectedUpgradeRows): las dos filas de la
      tabla Upgrade (UpgradeCode, VersionMin/VersionMax, idioma, Attributes y ActionProperty), la LaunchCondition
      `NOT WIX_DOWNGRADE_DETECTED` con su mensaje, FindRelatedProducts y LaunchConditions sin condicion en las dos
      secuencias de instalacion y FindRelatedProducts ANTES de LaunchConditions en cada una (al reves, la condicion se
      evaluaria con la propiedad aun sin rellenar y el bloqueo no haria nada), y RemoveExistingProducts (sin condicion)
      en InstallExecuteSequence.
    * los identificadores y las propiedades se comparan DISTINGUIENDO mayusculas (Windows Installer lo hace asi: una
      ActionProperty `wix_downgrade_detected` no es la propiedad que la LaunchCondition lee).

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
# GUID de componente FIJOS de installer/Tesela.wxs (en mayusculas y entre llaves, como los guarda la tabla Component). Un cambio
# accidental dejaria dos componentes distintos para el mismo archivo entre versiones.
$MsiExpectedComponentIds = @{
    'TeselaExe'         = '{B684DA7F-4E1F-49DD-9ED7-A7D2C72A1B22}'
    'TeselaScanExe'     = '{F6AC4BAC-3E83-4784-ACA7-50ED49A1D3D9}'
    'LicenseMit'        = '{1B50C86F-50F5-4EEF-B97B-BE62D82BBB9D}'
    'LicenseApache'     = '{100B9CE0-2501-4884-8A2D-FE074CF71F06}'
    'ThirdPartyNotices' = '{0FFBF222-BD3C-4B09-A7A1-97CA3D105F13}'
    'StartMenuShortcut' = '{089416B7-8B88-4F2D-9636-96931BE9FCC0}'
}
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
            if ($MsiAllowedTables -cnotcontains $t) { $problems.Add("tabla no permitida en el MSI: $t") }
        }

        # Hashtable que distingue mayusculas (@{} no lo hace): los nombres de propiedad de Windows Installer si.
        $props = New-Object System.Collections.Hashtable ([System.StringComparer]::Ordinal)
        foreach ($r in (Get-MsiRows $msi 'SELECT `Property`, `Value` FROM `Property`' 2)) { $props[$r[0]] = $r[1] }
        if ($props['ProductVersion'] -cne $ExpectedVersion) { $problems.Add("ProductVersion = '$($props['ProductVersion'])', se esperaba '$ExpectedVersion'") }
        if ($props['UpgradeCode'] -cne $MsiExpectedUpgradeCode) { $problems.Add("UpgradeCode = '$($props['UpgradeCode'])': NO se puede cambiar (installer/README.md)") }
        if ($props['ProductName'] -cne 'Tesela') { $problems.Add("ProductName = '$($props['ProductName'])'") }
        if ($props['Manufacturer'] -cne 'Tesela contributors') { $problems.Add("Manufacturer = '$($props['Manufacturer'])'") }
        foreach ($p in 'ARPPRODUCTICON', 'ARPURLINFOABOUT') {
            if (-not $props[$p]) { $problems.Add("falta la propiedad $p (Aplicaciones instaladas)") }
        }
        if ($props['ARPURLINFOABOUT'] -cne 'https://github.com/wandoth1/tesela') { $problems.Add("ARPURLINFOABOUT = '$($props['ARPURLINFOABOUT'])'") }

        # Por usuario: sin ALLUSERS (o ALLUSERS=2 con MSIINSTALLPERUSER=1, segun la version de WiX) y, en el Summary
        # Information, el bit 3 del contador de palabras ("no hace falta elevar").
        $allUsers = $props['ALLUSERS']
        $perUserProps = (-not $allUsers) -or ($allUsers -ceq '2' -and $props['MSIINSTALLPERUSER'] -ceq '1')
        if (-not $perUserProps) { $problems.Add("no es por usuario: ALLUSERS='$allUsers' MSIINSTALLPERUSER='$($props['MSIINSTALLPERUSER'])'") }
        $wordCount = [int](Get-MsiSummaryProperty $msi 15)
        if (($wordCount -band 8) -ceq 0) { $problems.Add("el Summary Information no declara 'no hace falta elevar' (contador de palabras = $wordCount)") }

        $files = @(Get-MsiRows $msi 'SELECT `File`, `FileName`, `FileSize` FROM `File`' 3)
        $longNames = @($files | ForEach-Object { ($_[1] -split '\|')[-1] })
        foreach ($n in $MsiExpectedFiles) { if ($longNames -cnotcontains $n) { $problems.Add("falta el archivo $n") } }
        foreach ($n in $longNames) { if ($MsiExpectedFiles -cnotcontains $n) { $problems.Add("archivo inesperado en el MSI: $n") } }
        if ($files.Count -cne $MsiExpectedFiles.Count) { $problems.Add("el MSI lleva $($files.Count) archivos, se esperaban $($MsiExpectedFiles.Count)") }

        $ca = @(Get-MsiRows $msi 'SELECT `Action`, `Type` FROM `CustomAction`' 2)
        if ($ca.Count -gt 0) { $problems.Add('el MSI lleva acciones personalizadas: ' + (($ca | ForEach-Object { $_[0] }) -join ', ')) }

        $reg = @(Get-MsiRows $msi 'SELECT `Registry`, `Root`, `Key` FROM `Registry`' 3)
        foreach ($r in $reg) {
            # 1 = HKEY_CURRENT_USER
            if ($r[1] -cne '1' -or $r[2] -notlike 'Software\Tesela*') { $problems.Add("registro fuera de HKCU\Software\Tesela: raiz $($r[1]), clave '$($r[2])'") }
        }
        if ($reg.Count -ceq 0) { $problems.Add('no hay valores de registro (los componentes por usuario los necesitan como KeyPath)') }

        # Carpetas: el arbol EXACTO (cada Directory con su Directory_Parent) y, donde el .wxs fija el nombre, su DefaultDir
        # (que puede ser "corto|largo": el nombre largo es el ultimo trozo).
        $dirs = @(Get-MsiRows $msi 'SELECT `Directory`, `Directory_Parent`, `DefaultDir` FROM `Directory`' 3)
        $expectedIds = @($MsiExpectedDirectories | ForEach-Object { $_.Id })
        foreach ($d in $dirs) {
            if ($expectedIds -cnotcontains $d[0]) { $problems.Add("carpeta fuera del perfil del usuario: $($d[0])") }
        }
        foreach ($e in $MsiExpectedDirectories) {
            $found = @($dirs | Where-Object { $_[0] -ceq $e.Id })
            if ($found.Count -cne 1) { $problems.Add("carpeta $($e.Id): hay $($found.Count) filas en la tabla Directory, se esperaba 1"); continue }
            if ($found[0][1] -cne $e.Parent) { $problems.Add("carpeta $($e.Id): Directory_Parent = '$($found[0][1])', se esperaba '$($e.Parent)'") }
            $leafName = ($found[0][2] -split '\|')[-1]
            if ($null -cne $e.DefaultDir -and $leafName -cne $e.DefaultDir) { $problems.Add("carpeta $($e.Id): DefaultDir = '$($found[0][2])', se esperaba '$($e.DefaultDir)'") }
        }
        $comps = @(Get-MsiRows $msi 'SELECT `Component`, `Directory_`, `ComponentId` FROM `Component`' 3)
        foreach ($c in $comps) {
            if (@('INSTALLFOLDER', 'ProgramMenuFolder') -cnotcontains $c[1]) { $problems.Add("componente $($c[0]) en $($c[1])") }
            if (-not $MsiExpectedComponentIds.ContainsKey($c[0])) { $problems.Add("componente inesperado: $($c[0])") }
            elseif ($c[2] -cne $MsiExpectedComponentIds[$c[0]]) { $problems.Add("componente $($c[0]): GUID = '$($c[2])', se esperaba '$($MsiExpectedComponentIds[$c[0]])' (los GUID de componente son fijos)") }
        }
        foreach ($k in $MsiExpectedComponentIds.Keys) {
            if (@($comps | Where-Object { $_[0] -ceq $k }).Count -cne 1) { $problems.Add("falta el componente $k") }
        }

        $sc = @(Get-MsiRows $msi 'SELECT `Shortcut`, `Directory_`, `Name`, `Target`, `Icon_`, `Arguments`, `WkDir` FROM `Shortcut`' 7)
        if ($sc.Count -cne 1 -or $sc[0][1] -cne 'ProgramMenuFolder' -or $sc[0][3] -cne '[INSTALLFOLDER]tesela.exe') {
            $problems.Add("se esperaba exactamente 1 acceso directo en el menu Inicio hacia tesela.exe; hay $($sc.Count)")
        } else {
            # Sin argumentos (un acceso directo con `--scan <ruta>` o `--demo` cambiaria lo que hace Tesela al abrirse) y con la
            # carpeta de trabajo en la de instalacion.
            if ($sc[0][5] -cne '') { $problems.Add("el acceso directo lleva argumentos: '$($sc[0][5])'") }
            if ($sc[0][6] -cne 'INSTALLFOLDER') { $problems.Add("el acceso directo tiene WkDir = '$($sc[0][6])', se esperaba 'INSTALLFOLDER'") }
        }
        $icons = @(Get-MsiRows $msi 'SELECT `Name` FROM `Icon`' 1)
        if ($icons.Count -cne 1) { $problems.Add("se esperaba 1 icono en el MSI; hay $($icons.Count)") }

        # MajorUpgrade: las dos filas EXACTAS de la tabla Upgrade (ver $MsiExpectedUpgradeRows y su explicacion).
        $up = @(Get-MsiRows $msi 'SELECT `UpgradeCode`, `VersionMin`, `VersionMax`, `Language`, `Attributes`, `Remove`, `ActionProperty` FROM `Upgrade`' 7)
        if ($up.Count -cne $MsiExpectedUpgradeRows.Count) { $problems.Add("la tabla Upgrade tiene $($up.Count) filas, se esperaban $($MsiExpectedUpgradeRows.Count) (MajorUpgrade)") }
        foreach ($e in $MsiExpectedUpgradeRows) {
            $rows = @($up | Where-Object { $_[6] -ceq $e.ActionProperty })
            if ($rows.Count -cne 1) { $problems.Add("falta la fila de Upgrade para $($e.Name) (ActionProperty $($e.ActionProperty)): hay $($rows.Count)"); continue }
            $r = $rows[0]
            $min = if ($e.Min -ceq 'V') { $ExpectedVersion } else { $e.Min }
            $max = if ($e.MaxIsVersion) { $ExpectedVersion } else { '' }
            if ($r[0] -cne $MsiExpectedUpgradeCode) { $problems.Add("Upgrade ($($e.Name)): UpgradeCode = '$($r[0])'") }
            if ($r[1] -cne $min) { $problems.Add("Upgrade ($($e.Name)): VersionMin = '$($r[1])', se esperaba '$min'") }
            if ($r[2] -cne $max) { $problems.Add("Upgrade ($($e.Name)): VersionMax = '$($r[2])', se esperaba '$max'") }
            if ($r[3] -cne $MsiExpectedLanguage) { $problems.Add("Upgrade ($($e.Name)): Language = '$($r[3])', se esperaba '$MsiExpectedLanguage'") }
            if ($r[4] -cne $e.Attributes) { $problems.Add("Upgrade ($($e.Name)): Attributes = '$($r[4])', se esperaba '$($e.Attributes)'") }
            if ($r[5] -cne '') { $problems.Add("Upgrade ($($e.Name)): Remove = '$($r[5])', se esperaba vacio") }
        }
        # Bloqueo de versiones posteriores: UNA LaunchCondition exacta, con su mensaje.
        $lc = @(Get-MsiRows $msi 'SELECT `Condition`, `Description` FROM `LaunchCondition`' 2)
        if ($lc.Count -cne 1 -or $lc[0][0] -cne $MsiExpectedLaunchCondition) {
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
            $seq[$t] = New-Object System.Collections.Hashtable ([System.StringComparer]::Ordinal)
            foreach ($r in (Get-MsiRows $msi ('SELECT `Action`, `Condition`, `Sequence` FROM `' + $t + '`') 3)) { $seq[$t][$r[0]] = $r }
        }
        foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
            foreach ($a in 'FindRelatedProducts', 'LaunchConditions') {
                if (-not $seq[$t].ContainsKey($a)) { $problems.Add("falta $a en $t (sin el, el bloqueo o la deteccion de versiones no se evalua)") }
                elseif ($seq[$t][$a][1] -cne '') { $problems.Add("$a en ${t} lleva la condicion '$($seq[$t][$a][1])' (tiene que ser incondicional)") }
            }
        }
        # FindRelatedProducts tiene que ejecutarse ANTES de LaunchConditions: es la que pone WIX_DOWNGRADE_DETECTED. Con el orden
        # al reves, la condicion `NOT WIX_DOWNGRADE_DETECTED` se evalua con la propiedad vacia (siempre verdadera) y el
        # bloqueo no hace nada, aunque todas las filas esten.
        foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
            if ($seq[$t].ContainsKey('FindRelatedProducts') -and $seq[$t].ContainsKey('LaunchConditions')) {
                $frp = [int]$seq[$t]['FindRelatedProducts'][2]
                $lcs = [int]$seq[$t]['LaunchConditions'][2]
                if ($frp -ge $lcs) { $problems.Add("FindRelatedProducts (secuencia $frp) tiene que ir antes de LaunchConditions (secuencia $lcs) en ${t}: si no, el bloqueo de versiones posteriores no surte efecto") }
            }
        }
        $ies = $seq['InstallExecuteSequence']
        if (-not $ies.ContainsKey('RemoveExistingProducts')) {
            $problems.Add('falta RemoveExistingProducts en InstallExecuteSequence (MajorUpgrade no desinstalaria la version anterior)')
        } else {
            $rep = $ies['RemoveExistingProducts']
            if ($rep[1] -cne '') { $problems.Add("RemoveExistingProducts lleva la condicion '$($rep[1])' (tiene que ser incondicional)") }
            $lo = if ($ies.ContainsKey('InstallValidate')) { [int]$ies['InstallValidate'][2] } else { $null }
            $hi = if ($ies.ContainsKey('InstallInitialize')) { [int]$ies['InstallInitialize'][2] } else { $null }
            if ($null -ceq $lo -or $null -ceq $hi -or [int]$rep[2] -le $lo -or [int]$rep[2] -ge $hi) {
                $problems.Add("RemoveExistingProducts (secuencia $($rep[2])) tiene que ir entre InstallValidate e InstallInitialize")
            }
        }
        # Las dos propiedades que rellena la tabla Upgrade tienen que figurar en SecureCustomProperties (propiedades publicas
        # restringidas: sin ello Windows Installer no las conserva al pasar de la fase de interfaz a la de ejecucion).
        $secure = @(($props['SecureCustomProperties'] -split ';') | Where-Object { $_ })
        # INSTALLFOLDER tambien (`<Property Secure="yes">`): la carpeta de la linea de comandos y la de la busqueda de registro.
        foreach ($a in 'WIX_UPGRADE_DETECTED', 'WIX_DOWNGRADE_DETECTED', 'INSTALLFOLDER') {
            if ($secure -cnotcontains $a) { $problems.Add("SecureCustomProperties no incluye $a (tiene: '$($props['SecureCustomProperties'])')") }
        }

        # RemoveFile: SOLO las dos filas esperadas (quitar INSTALLFOLDER y ProgramsFolder si quedan vacias), sin nombre de
        # archivo: una fila con FileName (p. ej. *.*) borraria archivos del usuario al desinstalar.
        $rm = @(Get-MsiRows $msi 'SELECT `FileKey`, `Component_`, `FileName`, `DirProperty`, `InstallMode` FROM `RemoveFile`' 5)
        foreach ($r in $rm) {
            if ($r[2] -cne '' -or @('INSTALLFOLDER', 'ProgramsFolder') -cnotcontains $r[3] -or $r[4] -cne '2') {
                $problems.Add("fila de RemoveFile no esperada: '$($r[0])' FileName='$($r[2])' DirProperty='$($r[3])' InstallMode='$($r[4])'")
            }
        }
        $rmDirs = @($rm | ForEach-Object { $_[3] } | Sort-Object -Unique)
        if ($rm.Count -cne 2 -or $rmDirs.Count -cne 2) { $problems.Add("RemoveFile tiene que tener exactamente 2 filas (INSTALLFOLDER y ProgramsFolder); hay $($rm.Count)") }

        # La carpeta de instalacion se recuerda en HKCU\Software\Tesela\InstallFolder (valor `[INSTALLFOLDER]`) y se recupera
        # con UNA busqueda de registro hacia INSTALLFOLDER (sin accion personalizada): sin eso, al desinstalar un MSI que se
        # instalo en una carpeta personalizada los archivos quedarian huerfanos.
        $loc = @(Get-MsiRows $msi 'SELECT `Signature_`, `Root`, `Key`, `Name`, `Type` FROM `RegLocator`' 5)
        # Type 18 = 2 (valor crudo, raw) + 16 (vista de 64 bits: el paquete es x64).
        if ($loc.Count -cne 1 -or $loc[0][1] -cne '1' -or $loc[0][2] -cne 'Software\Tesela' -or $loc[0][3] -cne 'InstallFolder' -or $loc[0][4] -cne '18') {
            $problems.Add("se esperaba UNA busqueda de registro de HKCU\Software\Tesela\InstallFolder (tipo 18); hay $($loc.Count), tipo '$($loc | ForEach-Object { $_[4] })'")
        }
        $app = @(Get-MsiRows $msi 'SELECT `Property`, `Signature_` FROM `AppSearch`' 2)
        if ($app.Count -cne 1 -or $app[0][0] -cne 'INSTALLFOLDER') { $problems.Add('AppSearch tiene que asignar solo INSTALLFOLDER') }
        # La accion AppSearch (la que ejecuta la busqueda) tiene que estar, sin condicion, ANTES de CostFinalize (que calcula las
        # rutas de instalacion con INSTALLFOLDER) en las dos secuencias de instalacion: si no, la carpeta recordada no se recupera
        # al actualizar, reparar o desinstalar y los archivos de una carpeta personalizada quedarian huerfanos.
        foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
            if (-not $seq[$t].ContainsKey('AppSearch')) { $problems.Add("falta AppSearch en $t"); continue }
            if ($seq[$t]['AppSearch'][1] -cne '') { $problems.Add("AppSearch en ${t} lleva la condicion '$($seq[$t]['AppSearch'][1])' (tiene que ser incondicional)") }
            if (-not $seq[$t].ContainsKey('CostFinalize')) { $problems.Add("falta CostFinalize en $t"); continue }
            if ([int]$seq[$t]['AppSearch'][2] -ge [int]$seq[$t]['CostFinalize'][2]) {
                $problems.Add("AppSearch (secuencia $($seq[$t]['AppSearch'][2])) tiene que ir antes de CostFinalize (secuencia $($seq[$t]['CostFinalize'][2])) en ${t}")
            }
        }
        # Sin busquedas de archivos ni de carpetas: la tabla Signature (si existe) esta vacia. RegLocator no usa filas de
        # Signature; una fila de mas seria una busqueda que el .wxs no declara.
        $sig = @(Get-MsiRows $msi 'SELECT `Signature` FROM `Signature`' 1)
        if ($sig.Count -cne 0) { $problems.Add("la tabla Signature tiene $($sig.Count) filas, tiene que estar vacia (busquedas de archivos o carpetas no declaradas)") }
        $saved = @(Get-MsiRows $msi 'SELECT `Name`, `Value` FROM `Registry`' 2 | Where-Object { $_[0] -ceq 'InstallFolder' })
        if ($saved.Count -cne 1 -or $saved[0][1] -cne '[INSTALLFOLDER]') { $problems.Add('falta el valor de registro InstallFolder = [INSTALLFOLDER]') }
    } finally {
        Close-Msi $msi
    }
    return , $problems
}
