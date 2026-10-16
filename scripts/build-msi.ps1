<#
.SYNOPSIS
  Construye el instalador MSI por usuario, el zip portable y SHA256SUMS.txt de Tesela, y valida el resultado.

.DESCRIPTION
  Todo sale de lo que produce scripts/build-release.ps1 (CRT estatico, --remap-path-prefix, conjunto exacto de
  ejecutables, busqueda de valores locales e imports PE): el script lo llama primero (salvo -SkipBuild) y empaqueta
  EXACTAMENTE esos dos ejecutables mas LICENSE-MIT, LICENSE-APACHE y THIRD-PARTY-NOTICES.md.

  Herramienta: WiX Toolset $WixVersion (CLI `wix`, `dotnet tool`; MS-RL; solo compila, no se redistribuye). Se instala una
  vez con:
      dotnet tool install --global wix --version 5.0.2 --add-source https://api.nuget.org/v3/index.json
  y se quita con `dotnet tool uninstall --global wix`. La version esta FIJADA aqui, en ci.yml y en release.yml (la prueba
  scripts/tests/wix-version.tests.ps1 comprueba que las tres coinciden); el script se niega a usar otra.
  (Se fija la 5.0.2 y no la 6.x/7.x porque desde la 6 el binario va con el acuerdo OSMF y pide aceptarlo; la 5.0.2 es
  MS-RL sin aceptacion.)

  Salida (en -OutDir, por defecto <target de cargo>/dist; nada se escribe en el repositorio):
      tesela-<version>-x64.msi
      tesela-<version>-x64-portable.zip
      SHA256SUMS.txt

  Validacion del MSI (falla si algo no cuadra):
    * `wix msi validate` (ICE); solo se aceptan ICE61 (AllowSameVersionUpgrades) e ICE91 (todo es por usuario).
    * tablas leidas con la API de Windows Installer en SOLO LECTURA: version, UpgradeCode, por usuario (sin ALLUSERS, o
      ALLUSERS=2 con MSIINSTALLPERUSER=1; y el bit "no hace falta elevar" del Summary Information), exactamente los 5 archivos
      esperados, ninguna accion personalizada, solo registro en HKCU, solo carpetas del perfil, 1 acceso directo del menu
      Inicio, icono, MajorUpgrade y bloqueo de versiones anteriores.
    * los archivos que lleva el MSI (extraidos con `wix msi decompile -x`) son byte a byte los de la compilacion.
    * leak-scan (usuario, equipo, perfil, repositorio, CARGO_HOME y toolchain) sobre el MSI, sobre cada archivo que
      contiene y sobre cada archivo del zip YA DESCOMPRIMIDO, mas los nombres de las entradas del zip. (Los bytes crudos del
      MSI y del zip estan comprimidos y no se pueden buscar: por eso se buscan tambien los contenidos.)

.PARAMETER OutDir
  Carpeta de salida (se crea; se borran de ella los tesela-*.msi / tesela-*.zip / SHA256SUMS.txt anteriores).

.PARAMETER SkipBuild
  No llama a build-release.ps1: usa los release que ya existan (solo para iterar en local; el CI y el release NO lo usan).

.PARAMETER Version
  SOLO PARA PRUEBAS LOCALES de actualizacion: sustituye la version del MSI y de los nombres (p. ej. 0.1.1 para probar que
  una version nueva sustituye a la anterior). Los ejecutables llevan la version del Cargo.toml, no esta. Se rechaza en
  GitHub Actions: un release SIEMPRE sale de la version de Cargo.toml.

.EXAMPLE
  pwsh -File scripts/build-msi.ps1
#>
[CmdletBinding()]
param(
    [string]$OutDir,
    [switch]$SkipBuild,
    [string]$Version
)

$ErrorActionPreference = 'Stop'

# --- Constantes (cambiarlas es una decision deliberada) -------------------------------------------------------------------
$WixVersion = '5.0.2'
# UpgradeCode de installer/Tesela.wxs: NO CAMBIAR NUNCA (ver installer/README.md). Aqui se repite para que un cambio
# accidental en el .wxs rompa la validacion.
$ExpectedUpgradeCode = '{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}'
$ExpectedFiles = @('tesela.exe', 'tesela-scan.exe', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md')
$AcceptedIce = @('ICE61', 'ICE91')

. (Join-Path $PSScriptRoot 'leak-scan.ps1')

function Write-Step([string]$Text) { Write-Host "build-msi: $Text" }

function Invoke-Native {
    # Ejecuta un programa y devuelve su salida (lineas); falla si el codigo de salida no es 0 (o el permitido).
    param([string]$Exe, [string[]]$Arguments, [int[]]$OkCodes = @(0))
    $out = & $Exe @Arguments 2>&1 | ForEach-Object { "$_" }
    if ($OkCodes -notcontains $LASTEXITCODE) {
        $out | ForEach-Object { Write-Host "  $_" }
        throw "$([System.IO.Path]::GetFileName($Exe)) termino con el codigo $LASTEXITCODE"
    }
    return , @($out)
}

# --- Windows Installer (solo lectura) ------------------------------------------------------------------------------------
function Open-MsiDatabase([string]$Path) {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    # 0 = msiOpenDatabaseModeReadOnly
    $db = [System.__ComObject].InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Path, 0))
    return [pscustomobject]@{ Installer = $installer; Db = $db }
}

function Get-MsiRows($Msi, [string]$Sql, [int]$Columns) {
    # Devuelve una lista de filas (cada una, un array de cadenas). Una tabla que no existe da una lista vacia.
    $rows = New-Object System.Collections.Generic.List[object]
    $view = $null
    try {
        $view = [System.__ComObject].InvokeMember('OpenView', 'InvokeMethod', $null, $Msi.Db, @($Sql))
    } catch {
        return $rows.ToArray()
    }
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
        $props = @{}
        foreach ($r in (Get-MsiRows $msi 'SELECT `Property`, `Value` FROM `Property`' 2)) { $props[$r[0]] = $r[1] }
        if ($props['ProductVersion'] -ne $ExpectedVersion) { $problems.Add("ProductVersion = '$($props['ProductVersion'])', se esperaba '$ExpectedVersion'") }
        if ($props['UpgradeCode'] -ne $ExpectedUpgradeCode) { $problems.Add("UpgradeCode = '$($props['UpgradeCode'])': NO se puede cambiar (installer/README.md)") }
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
        foreach ($n in $ExpectedFiles) { if ($longNames -notcontains $n) { $problems.Add("falta el archivo $n") } }
        foreach ($n in $longNames) { if ($ExpectedFiles -notcontains $n) { $problems.Add("archivo inesperado en el MSI: $n") } }
        if ($files.Count -ne $ExpectedFiles.Count) { $problems.Add("el MSI lleva $($files.Count) archivos, se esperaban $($ExpectedFiles.Count)") }

        $ca = @(Get-MsiRows $msi 'SELECT `Action`, `Type` FROM `CustomAction`' 2)
        if ($ca.Count -gt 0) { $problems.Add("el MSI lleva acciones personalizadas: " + (($ca | ForEach-Object { $_[0] }) -join ', ')) }

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
        if (@($up | Where-Object { $_[0] -eq $ExpectedUpgradeCode }).Count -lt 2) { $problems.Add('falta MajorUpgrade (tabla Upgrade): una fila para las versiones anteriores y otra para detectar una anterior') }
        $lc = @(Get-MsiRows $msi 'SELECT `Condition`, `Description` FROM `LaunchCondition`' 2)
        if (-not @($lc | Where-Object { $_[0] -like '*WIX_DOWNGRADE_DETECTED*' -and $_[1] })) { $problems.Add('falta el bloqueo de versiones anteriores con su mensaje (LaunchCondition)') }
    } finally {
        Close-Msi $msi
    }
    return , $problems
}

function Get-FileSha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# --- Inicio ---------------------------------------------------------------------------------------------------------------
$repo = (& git rev-parse --show-toplevel).Trim()
if ($LASTEXITCODE -ne 0 -or -not $repo) { throw 'no se pudo localizar el repositorio (git rev-parse)' }
Push-Location $repo
try {
    $meta = & cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata fallo' }
    $versions = @($meta.packages | ForEach-Object { $_.version } | Sort-Object -Unique)
    if ($versions.Count -ne 1) { throw "los crates del workspace no comparten version: $($versions -join ', ')" }
    $cargoVersion = $versions[0]
    $msiVersion = $cargoVersion
    if ($Version) {
        if ($env:GITHUB_ACTIONS -eq 'true') { throw '-Version solo es para pruebas locales: un release sale de la version de Cargo.toml' }
        $msiVersion = $Version
        Write-Host "build-msi: AVISO: -Version $Version solo para pruebas locales de actualizacion (los ejecutables llevan $cargoVersion). NO publiques esto." -ForegroundColor Yellow
    }
    if ($msiVersion -notmatch '^(\d+)\.(\d+)\.(\d+)$' -or [int]$Matches[1] -gt 255 -or [int]$Matches[2] -gt 255 -or [int]$Matches[3] -gt 65535) {
        throw "la version '$msiVersion' no cabe en un MSI (major.minor.build con numeros; sin sufijos como -rc1)"
    }

    # WiX con la version fijada.
    $wix = Get-Command wix -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $wix) {
        throw "no se encuentra 'wix'. Instalalo con: dotnet tool install --global wix --version $WixVersion --add-source https://api.nuget.org/v3/index.json"
    }
    $wixOut = (Invoke-Native $wix.Source @('--version')) -join ' '
    if (-not $wixOut.StartsWith("$WixVersion+") -and $wixOut.Trim() -ne $WixVersion) {
        throw "WiX '$wixOut': este repositorio fija la $WixVersion (dotnet tool update --global wix --version $WixVersion)"
    }
    Write-Step "WiX $wixOut"

    $targetDir = $meta.target_directory
    if (-not $OutDir) { $OutDir = Join-Path $targetDir 'dist' }
    $OutDir = [System.IO.Path]::GetFullPath($OutDir)
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    Get-ChildItem -LiteralPath $OutDir -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like 'tesela-*.msi' -or $_.Name -like 'tesela-*.zip' -or $_.Name -eq 'SHA256SUMS.txt' } |
        Remove-Item -Force
    $work = Join-Path $OutDir '_work'
    if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $work | Out-Null

    # --- 1. Release (el unico origen de los ejecutables) -----------------------------------------------------------------
    if (-not $SkipBuild) {
        Write-Step 'scripts/build-release.ps1'
        $self = (Get-Process -Id $PID).MainModule.FileName
        & $self -NoProfile -File (Join-Path $PSScriptRoot 'build-release.ps1')
        if ($LASTEXITCODE -ne 0) { throw 'build-release.ps1 fallo' }
    }
    $releaseDir = Join-Path $targetDir 'release'
    $exes = @('tesela.exe', 'tesela-scan.exe')
    foreach ($e in $exes) {
        if (-not (Test-Path -LiteralPath (Join-Path $releaseDir $e) -PathType Leaf)) { throw "falta $e en la carpeta de release (corre build-release.ps1)" }
    }
    # Los ejecutables llevan la version de Cargo.toml en su VERSIONINFO.
    foreach ($e in $exes) {
        $pv = (Get-Item -LiteralPath (Join-Path $releaseDir $e)).VersionInfo.ProductVersion
        if ($pv -ne $cargoVersion) { throw "$e lleva la version '$pv' y Cargo.toml dice '$cargoVersion' (compilacion antigua: corre build-release.ps1)" }
    }

    # --- 2. Preparar el contenido (los mismos 5 archivos para el MSI y el zip) ------------------------------------------
    $stage = Join-Path $work 'stage'
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    foreach ($e in $exes) { Copy-Item -LiteralPath (Join-Path $releaseDir $e) -Destination $stage }
    foreach ($d in 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md') { Copy-Item -LiteralPath (Join-Path $repo $d) -Destination $stage }
    $stagedNames = @(Get-ChildItem -LiteralPath $stage -File | ForEach-Object { $_.Name })
    foreach ($n in $ExpectedFiles) { if ($stagedNames -notcontains $n) { throw "falta $n en el contenido" } }
    if ($stagedNames.Count -ne $ExpectedFiles.Count) { throw "el contenido tiene archivos de mas: $($stagedNames -join ', ')" }

    $py = Get-Command python -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $py) { $py = Get-Command python3 -ErrorAction SilentlyContinue | Select-Object -First 1 }
    if (-not $py) { throw 'hace falta Python 3 en el PATH (scripts/extract-ico.py)' }
    $ico = Join-Path $work 'tesela.ico'
    Invoke-Native $py.Source @('-I', (Join-Path $repo 'scripts/extract-ico.py'), (Join-Path $stage 'tesela.exe'), $ico) | ForEach-Object { Write-Host "  $_" }

    # --- 3. MSI ---------------------------------------------------------------------------------------------------------
    $msiName = "tesela-$msiVersion-x64.msi"
    $msiPath = Join-Path $OutDir $msiName
    Write-Step "wix build -> $msiName"
    $buildOut = Invoke-Native $wix.Source @(
        'build', (Join-Path $repo 'installer/Tesela.wxs'), '-arch', 'x64', '-nologo', '-wx',
        '-d', "Version=$msiVersion", '-d', "SourceDir=$stage", '-d', "IconFile=$ico",
        '-pdbtype', 'none', '-o', $msiPath)
    $buildOut | ForEach-Object { if ($_) { Write-Host "  $_" } }

    # --- 4. Validacion del MSI -------------------------------------------------------------------------------------------
    Write-Step 'wix msi validate (ICE)'
    $iceOut = & $wix.Source msi validate $msiPath 2>&1 | ForEach-Object { "$_" }
    $iceCode = $LASTEXITCODE
    $bad = New-Object System.Collections.Generic.List[string]
    foreach ($line in $iceOut) {
        if (-not $line.Trim()) { continue }
        $accepted = $false
        foreach ($i in $AcceptedIce) { if ($line -match "\b$i\b") { $accepted = $true } }
        if ($accepted) { Write-Host "  (aceptado) $($line.Substring([Math]::Max(0, $line.IndexOf('warning'))))" } else { $bad.Add($line) }
    }
    if ($iceCode -ne 0 -or $bad.Count -gt 0) {
        $bad | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
        throw "wix msi validate: codigo $iceCode, $($bad.Count) mensajes sin aceptar"
    }

    Write-Step 'tablas del MSI (Windows Installer, solo lectura)'
    $problems = Test-MsiPackage -Path $msiPath -ExpectedVersion $msiVersion
    if ($problems.Count -gt 0) {
        $problems | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
        throw 'el MSI no cumple lo esperado'
    }

    Write-Step 'contenido del MSI frente a lo compilado'
    $extract = Join-Path $work 'msi-files'
    Invoke-Native $wix.Source @('msi', 'decompile', '-x', $extract, '-o', (Join-Path $work 'decompiled.wxs'), $msiPath) | Out-Null
    $mapping = @{
        'TeselaExe' = 'tesela.exe'; 'TeselaScanExe' = 'tesela-scan.exe'; 'LicenseMit' = 'LICENSE-MIT'
        'LicenseApache' = 'LICENSE-APACHE'; 'ThirdPartyNotices' = 'THIRD-PARTY-NOTICES.md'
    }
    $inMsi = @(Get-ChildItem -LiteralPath (Join-Path $extract 'File') -File)
    if ($inMsi.Count -ne $ExpectedFiles.Count) { throw "el MSI contiene $($inMsi.Count) archivos, se esperaban $($ExpectedFiles.Count)" }
    foreach ($f in $inMsi) {
        $name = $mapping[$f.Name]
        if (-not $name) { throw "archivo inesperado en el MSI: $($f.Name)" }
        if ((Get-FileSha256 $f.FullName) -ne (Get-FileSha256 (Join-Path $stage $name))) { throw "$name del MSI no coincide con el compilado" }
    }

    # --- 5. Zip portable -------------------------------------------------------------------------------------------------
    $zipName = "tesela-$msiVersion-x64-portable.zip"
    $zipPath = Join-Path $OutDir $zipName
    Write-Step "zip portable -> $zipName"
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $rootInZip = "tesela-$msiVersion-x64-portable"
    $zip = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($n in $ExpectedFiles) {
            [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, (Join-Path $stage $n), "$rootInZip/$n", [System.IO.Compression.CompressionLevel]::Optimal)
        }
    } finally {
        $zip.Dispose()
    }

    # --- 6. Fugas de valores locales: MSI, contenido del MSI, contenido del zip y nombres de sus entradas ------------------
    Write-Step 'busqueda de valores locales (MSI, zip y sus contenidos descomprimidos)'
    $cargoHome = $env:CARGO_HOME
    if (-not $cargoHome) { $cargoHome = Join-Path $env:USERPROFILE '.cargo' }
    $sysroot = (& rustc --print sysroot).Trim()
    $rustupHome = $env:RUSTUP_HOME
    if (-not $rustupHome) { $rustupHome = Join-Path $env:USERPROFILE '.rustup' }
    $needles = Get-LocalValueNeedles -Repo $repo -CargoHome $cargoHome -Sysroot $sysroot -RustupHome $rustupHome
    $leaks = New-Object System.Collections.Generic.List[string]
    foreach ($l in (Find-LocalValueLeaks -Bytes ([System.IO.File]::ReadAllBytes($msiPath)) -Needles $needles -Name $msiName)) { $leaks.Add($l) }
    foreach ($f in $inMsi) {
        foreach ($l in (Find-LocalValueLeaks -Bytes ([System.IO.File]::ReadAllBytes($f.FullName)) -Needles $needles -Name "$msiName/$($mapping[$f.Name])")) { $leaks.Add($l) }
    }
    foreach ($l in (Find-LocalValueLeaks -Bytes ([System.IO.File]::ReadAllBytes($ico)) -Needles $needles -Name "$msiName/icono")) { $leaks.Add($l) }
    $zipRead = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
    try {
        $entryNames = @($zipRead.Entries | ForEach-Object { $_.FullName })
        $expectedEntries = @($ExpectedFiles | ForEach-Object { "$rootInZip/$_" })
        if ($entryNames.Count -ne $expectedEntries.Count -or @($entryNames | Where-Object { $expectedEntries -notcontains $_ }).Count -gt 0) {
            throw "el zip no contiene exactamente los archivos esperados: $($entryNames -join ', ')"
        }
        foreach ($l in (Find-LocalValueLeaks -Bytes ([System.Text.Encoding]::UTF8.GetBytes(($entryNames -join "`n"))) -Needles $needles -Name "$zipName (nombres)")) { $leaks.Add($l) }
        foreach ($entry in $zipRead.Entries) {
            $ms = New-Object System.IO.MemoryStream
            $s = $entry.Open()
            try { $s.CopyTo($ms) } finally { $s.Dispose() }
            $bytes = $ms.ToArray()
            foreach ($l in (Find-LocalValueLeaks -Bytes $bytes -Needles $needles -Name "$zipName/$($entry.FullName)")) { $leaks.Add($l) }
            $leaf = $entry.FullName.Substring($rootInZip.Length + 1)
            if ((Get-FileSha256 (Join-Path $stage $leaf)) -ne ([System.BitConverter]::ToString([System.Security.Cryptography.SHA256]::Create().ComputeHash($bytes)).Replace('-', '').ToLowerInvariant())) {
                throw "$leaf del zip no coincide con el compilado"
            }
        }
    } finally {
        $zipRead.Dispose()
    }
    if ($leaks.Count -gt 0) {
        Write-Host 'build-msi: los paquetes contienen datos de esta maquina:' -ForegroundColor Red
        $leaks | Sort-Object -Unique | ForEach-Object { Write-Host "  $_" }
        throw 'fuga de valores locales'
    }

    # --- 7. Sumas de verificacion ----------------------------------------------------------------------------------------
    $sums = @($msiPath, $zipPath) | ForEach-Object { '{0}  {1}' -f (Get-FileSha256 $_), (Split-Path -Leaf $_) }
    [System.IO.File]::WriteAllText((Join-Path $OutDir 'SHA256SUMS.txt'), (($sums -join "`n") + "`n"), (New-Object System.Text.UTF8Encoding($false)))

    Remove-Item -LiteralPath $work -Recurse -Force
    Write-Host ''
    Write-Host ('build-msi: {0}: {1:N0} bytes ({2:N2} MiB)' -f $msiName, (Get-Item -LiteralPath $msiPath).Length, ((Get-Item -LiteralPath $msiPath).Length / 1MB))
    Write-Host ('build-msi: {0}: {1:N0} bytes ({2:N2} MiB)' -f $zipName, (Get-Item -LiteralPath $zipPath).Length, ((Get-Item -LiteralPath $zipPath).Length / 1MB))
    Write-Host "build-msi: SHA256SUMS.txt:"
    $sums | ForEach-Object { Write-Host "  $_" }
    Write-Host "build-msi: OK ($OutDir)"
} catch {
    Write-Host "build-msi: ERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
} finally {
    Pop-Location
}
