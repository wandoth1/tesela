<#
.SYNOPSIS
  Construye el instalador MSI (por usuario o para todos, con interfaz), el zip portable y SHA256SUMS.txt de Tesela, y valida el
  resultado.

.DESCRIPTION
  Todo sale de lo que produce scripts/build-release.ps1 (CRT estatico, --remap-path-prefix, conjunto exacto de
  ejecutables, busqueda de valores locales e imports PE): el script lo llama primero (salvo -SkipBuild) y empaqueta
  EXACTAMENTE esos dos ejecutables mas LICENSE-MIT, LICENSE-APACHE y THIRD-PARTY-NOTICES.md.

  Herramienta: WiX Toolset $WixVersion (CLI `wix`, `dotnet tool`; MS-RL; solo compila, no se redistribuye). Se instala una
  vez con `pwsh -File scripts/install-wix.ps1 -Version 5.0.2` (usa installer/nuget.config, solo nuget.org, y comprueba el
  SHA-512 del paquete) y se quita con `dotnet tool uninstall --global wix`. La version esta FIJADA aqui, en ci.yml y en release.yml (la prueba
  scripts/tests/wix-version.tests.ps1 comprueba que las tres coinciden); el script se niega a usar otra.
  (Se fija la 5.0.2 y no la 6.x/7.x porque desde la 6 el binario va con el acuerdo OSMF y pide aceptarlo; la 5.0.2 es
  MS-RL sin aceptacion.)

  Salida (en -OutDir, por defecto <target de cargo>/dist; nada se escribe en el repositorio):
      tesela-<version>-x64.msi
      tesela-<version>-x64-portable.zip
      SHA256SUMS.txt

  Validacion del MSI (falla si algo no cuadra):
    * `wix msi validate` (ICE); solo se aceptan ICE61 (AllowSameVersionUpgrades) e ICE57 para los dos componentes de los
      accesos directos (ver $AcceptedIce).
    * tablas leidas con la API de Windows Installer en SOLO LECTURA (scripts/msi-validate.ps1): version, UpgradeCode, ambito
      dual (ALLUSERS=2 y MSIINSTALLPERUSER=1, y el bit "no hace falta elevar" del Summary Information), exactamente los 5
      archivos esperados, acciones personalizadas SOLO de tipo 51 y 35 (las esperadas, una a una), registro solo en
      HKMU\Software\Tesela, el arbol de carpetas, 2 accesos directos (menu Inicio y escritorio), icono, propiedades seguras,
      MajorUpgrade, bloqueo de versiones anteriores y del cambio de ambito, y la interfaz (ventanas, eventos nativos, textos
      en espanol e ingles).
    * los archivos que lleva el MSI (extraidos con `wix msi decompile -x`) son byte a byte los de la compilacion.
    * leak-scan (usuario, equipo, perfil, repositorio, CARGO_HOME y toolchain) sobre el MSI, sobre cada archivo que
      contiene y sobre cada archivo del zip YA DESCOMPRIMIDO, mas los nombres de las entradas del zip. (Los bytes crudos del
      MSI y del zip estan comprimidos y no se pueden buscar: por eso se buscan tambien los contenidos.)

.PARAMETER OutDir
  Carpeta de salida (se crea). Solo se sobrescriben los tres archivos que genera el script, con sus nombres exactos; usa una
  subcarpeta `_work` temporal suya (si ya existe, se niega a seguir) y la borra al terminar.

.PARAMETER SkipBuild
  No llama a build-release.ps1: usa los release que ya existan (solo para iterar en local; se rechaza en GitHub Actions).

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
# Avisos de ICE que se ACEPTAN (cualquier otro es un fallo). Cada uno: patron sobre la linea y por que.
#   * ICE61: AllowSameVersionUpgrades="yes" (reinstalar la MISMA version reemplaza a la anterior) hace que el maximo de la fila
#     de reemplazo no sea menor que la version del producto. Es lo que se quiere.
#   * ICE57 solo para StartMenuShortcut y DesktopShortcut: un acceso directo es dato "por usuario" y su KeyPath es un valor de
#     HKMU (HKCU por usuario, HKLM para todos), que ICE57 no sabe resolver. Es el diseno estandar de un paquete de doble
#     proposito (el acceso directo y su KeyPath van siempre en el mismo contexto: el del usuario o el comun del equipo).
$AcceptedIce = @(
    @{ Pattern = '\bICE61\b' },
    @{ Pattern = "\bICE57\b.*Component '(StartMenuShortcut|DesktopShortcut)'" }
)

. (Join-Path $PSScriptRoot 'leak-scan.ps1')
. (Join-Path $PSScriptRoot 'msi-validate.ps1')
$ExpectedFiles = $MsiExpectedFiles

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

function Get-FileSha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# --- Inicio ---------------------------------------------------------------------------------------------------------------
$repo = (& git rev-parse --show-toplevel).Trim()
if ($LASTEXITCODE -ne 0 -or -not $repo) { throw 'no se pudo localizar el repositorio (git rev-parse)' }
$workCreated = $false
Push-Location $repo
try {
    if ($SkipBuild -and $env:GITHUB_ACTIONS -eq 'true') { throw '-SkipBuild solo es para iterar en local: en GitHub Actions el release siempre se construye' }
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
        throw "no se encuentra 'wix'. Instalalo con: pwsh -File scripts/install-wix.ps1 -Version $WixVersion"
    }
    $wixOut = (Invoke-Native $wix.Source @('--version')) -join ' '
    if (-not $wixOut.StartsWith("$WixVersion+") -and $wixOut.Trim() -ne $WixVersion) {
        throw "WiX '$wixOut': este repositorio fija la $WixVersion (dotnet tool uninstall --global wix; pwsh -File scripts/install-wix.ps1 -Version $WixVersion)"
    }
    Write-Step "WiX $wixOut"

    $targetDir = $meta.target_directory
    if (-not $OutDir) { $OutDir = Join-Path $targetDir 'dist' }
    $OutDir = [System.IO.Path]::GetFullPath($OutDir)
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    # Solo se tocan los archivos que este script genera, con sus nombres EXACTOS (nunca se borra otra cosa de -OutDir).
    $msiName = "tesela-$msiVersion-x64.msi"
    $zipName = "tesela-$msiVersion-x64-portable.zip"
    $msiPath = Join-Path $OutDir $msiName
    $zipPath = Join-Path $OutDir $zipName
    foreach ($f in @($msiPath, $zipPath, (Join-Path $OutDir 'SHA256SUMS.txt'))) {
        if (Test-Path -LiteralPath $f) { Remove-Item -LiteralPath $f -Force }
    }
    $work = Join-Path $OutDir '_work'
    if (Test-Path -LiteralPath $work) { throw "ya existe $work (resto de otra ejecucion, o carpeta ajena): borrala a mano o usa otro -OutDir" }
    New-Item -ItemType Directory -Path $work | Out-Null
    $workCreated = $true

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
    Write-Step "wix build -> $msiName"
    $buildOut = Invoke-Native $wix.Source @(
        'build', (Join-Path $repo 'installer/Tesela.wxs'), (Join-Path $repo 'installer/TeselaUI.wxs'), '-arch', 'x64', '-nologo', '-wx',
        '-d', "Version=$msiVersion", '-d', "SourceDir=$stage", '-d', "IconFile=$ico",
        '-pdbtype', 'none', '-o', $msiPath)
    $buildOut | ForEach-Object { if ($_) { Write-Host "  $_" } }

    # --- 4. Validacion del MSI -------------------------------------------------------------------------------------------
    Write-Step 'wix msi validate (ICE)'
    $iceOut = & $wix.Source msi validate $msiPath 2>&1 | ForEach-Object { "$_" }
    $iceCode = $LASTEXITCODE
    $bad = New-Object System.Collections.Generic.List[string]
    $acceptedErrors = 0
    foreach ($line in $iceOut) {
        if (-not $line.Trim()) { continue }
        $accepted = $false
        foreach ($i in $AcceptedIce) { if ($line -match $i.Pattern) { $accepted = $true } }
        if ($accepted) {
            if ($line -match ' error ') { $acceptedErrors++ }
            Write-Host "  (aceptado) $($line.Substring([Math]::Max(0, $line.IndexOf('warning'))))"
        } else { $bad.Add($line) }
    }
    # Un codigo de salida distinto de 0 solo se tolera si lo explican avisos ACEPTADOS que ICE da como error (ICE57 de los accesos
    # directos): con cualquier otra causa, o sin ninguna linea, es un fallo.
    if (($iceCode -ne 0 -and $acceptedErrors -eq 0) -or $bad.Count -gt 0) {
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
    if ($workCreated -and (Test-Path -LiteralPath $work)) { Remove-Item -LiteralPath $work -Recurse -Force }
    Pop-Location
}
