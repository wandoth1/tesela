<#
.SYNOPSIS
  Instala WiX Toolset (CLI `wix`, `dotnet tool` global) con la version FIJADA, comprobando el SHA-512 del paquete ANTES de
  instalar nada.

.DESCRIPTION
  Lo usan ci.yml y release.yml, y sirve en local. Hace estas cosas, por este orden:
    1. si `wix` ya esta instalado: comprueba que es la version fijada y que el SHA-512 de su paquete (el .nupkg que dotnet
       guardo en su almacen) es el esperado; no instala ni quita nada;
    2. si no lo esta: DESCARGA el .nupkg de nuget.org (flat container, https) a una carpeta temporal propia, calcula su SHA-512
       y lo compara con el valor fijado aqui (el mismo que publica nuget.org en el catalogo del paquete). Si no coincide, FALLA
       sin haber instalado nada;
    3. solo con el hash correcto instala DESDE ESE ARCHIVO LOCAL ya verificado: `dotnet tool install ... --source <carpeta>
       --configfile installer/nuget.config`. `--source` deja la carpeta como UNICA fuente (dotnet no vuelve a pedir el paquete a
       la red, asi que lo instalado es lo comprobado), y `installer/nuget.config` (`<clear/>` y solo nuget.org) hace que dotnet
       ignore cualquier otra configuracion de NuGet de la maquina. Despues vuelve a comprobar el hash del paquete instalado;
    4. en GitHub Actions anade la carpeta de las herramientas de dotnet al PATH de los pasos siguientes.
  Para quitarlo: `dotnet tool uninstall --global wix`.

  Para subir de version hay que cambiarla a la vez en scripts/build-msi.ps1, ci.yml, release.yml y aqui (con su SHA-512, que
  se saca de https://api.nuget.org/v3/catalog0/ -> packageHash del paquete), y repetir el ciclo de instalacion; la prueba
  scripts/tests/wix-version.tests.ps1 vigila que coincidan.

  El script es solo ASCII a proposito (Windows PowerShell 5.1 lee los .ps1 sin BOM como ANSI).

.PARAMETER Version
  Version de WiX. Solo se admiten las que figuran en la tabla de abajo.

.PARAMETER ToolPath
  Instala (y busca) en esta carpeta en vez de en el almacen global de herramientas de dotnet (`--tool-path`). Para las pruebas y
  para quien no quiera tocar el almacen global; no cambia el PATH.

.PARAMETER NupkgPath
  Usa este .nupkg local en vez de descargarlo (sin red): pasa por EXACTAMENTE la misma comprobacion de SHA-512. Para las pruebas.

.EXAMPLE
  pwsh -File scripts/install-wix.ps1 -Version 5.0.2
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$ToolPath,
    [string]$NupkgPath
)

$ErrorActionPreference = 'Stop'

# Version -> SHA-512 (base64) del .nupkg, tal como lo publica nuget.org (packageHash del catalogo del paquete).
$KnownHashes = @{
    '5.0.2' = 'z+fu1deTcaV+TRCo4+6HrJ48Sdnj+yeQjjMStTI3GW7NxFEPaknYCdxrf0qEvgdXrjYdjLnkZKCsNSqTTOo2OA=='
}
if (-not $KnownHashes.ContainsKey($Version)) {
    throw "WiX $Version no esta en la tabla de versiones conocidas de install-wix.ps1 (solo: $($KnownHashes.Keys -join ', '))"
}

function Get-Sha512Base64([string]$Path) {
    $sha = [System.Security.Cryptography.SHA512]::Create()
    $stream = [System.IO.File]::OpenRead($Path)
    try { return [Convert]::ToBase64String($sha.ComputeHash($stream)) } finally { $stream.Dispose(); $sha.Dispose() }
}

function Assert-PackageHash([string]$Path, [string]$What) {
    $actual = Get-Sha512Base64 $Path
    if ($actual -cne $KnownHashes[$Version]) {
        throw "el SHA-512 del paquete WiX $Version ($What) no es el esperado (esperado $($KnownHashes[$Version]), obtenido $actual)"
    }
}

$work = $null
$code = 0
try {
    $repo = (& git rev-parse --show-toplevel).Trim()
    $config = Join-Path $repo 'installer/nuget.config'
    if (-not (Test-Path -LiteralPath $config)) { throw "falta $config" }

    if ($ToolPath) {
        $toolsDir = $ToolPath
        $listArgs = @('tool', 'list', '--tool-path', $ToolPath)
    } else {
        $home2 = $env:DOTNET_CLI_HOME
        if (-not $home2) { $home2 = $env:USERPROFILE }
        $toolsDir = Join-Path $home2 '.dotnet/tools'
        $listArgs = @('tool', 'list', '--global')
    }
    $installed = @()
    if (-not $ToolPath -or (Test-Path -LiteralPath $ToolPath -PathType Container)) {
        $installed = @(@(& dotnet @listArgs) | Where-Object { $_ -match '^\s*wix\s+(\S+)' } | ForEach-Object { $Matches[1] })
    }
    if ($installed.Count -gt 0 -and $installed[0] -ne $Version) {
        throw "ya hay WiX $($installed[0]) instalado y este repositorio fija la ${Version}: dotnet tool uninstall --global wix"
    }

    $nupkg = Join-Path $toolsDir ".store/wix/$Version/wix/$Version/wix.$Version.nupkg"
    if ($installed.Count -eq 0) {
        # --- 1. Obtener el paquete a una carpeta propia y comprobar su hash ANTES de instalar nada -----------------------
        $work = Join-Path ([System.IO.Path]::GetTempPath()) ('tesela-wix-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $work | Out-Null
        $local = Join-Path $work "wix.$Version.nupkg"
        if ($NupkgPath) {
            Write-Host "install-wix: usando el paquete local $NupkgPath"
            Copy-Item -LiteralPath $NupkgPath -Destination $local
        } else {
            $url = "https://api.nuget.org/v3-flatcontainer/wix/$Version/wix.$Version.nupkg"
            Write-Host "install-wix: descargando $url"
            $ProgressPreference = 'SilentlyContinue'
            Invoke-WebRequest -Uri $url -OutFile $local -UseBasicParsing
        }
        Assert-PackageHash $local 'a instalar'
        Write-Host "install-wix: SHA-512 del paquete WiX $Version correcto (antes de instalar)"

        # --- 2. Instalar DESDE ese archivo verificado (unica fuente) ----------------------------------------------------
        $installArgs = @('tool', 'install', 'wix', '--version', $Version, '--source', $work, '--configfile', $config)
        if ($ToolPath) { $installArgs += @('--tool-path', $ToolPath) } else { $installArgs += '--global' }
        Write-Host "install-wix: dotnet $($installArgs -join ' ')"
        & dotnet @installArgs
        if ($LASTEXITCODE -ne 0) { throw 'dotnet tool install fallo' }
    } else {
        Write-Host "install-wix: WiX $Version ya esta instalado"
    }

    # --- 3. El paquete que quedo en el almacen es el verificado (tambien cuando ya estaba instalado) ---------------------
    if (-not (Test-Path -LiteralPath $nupkg)) { throw "no se encuentra el paquete instalado: $nupkg" }
    Assert-PackageHash $nupkg 'instalado'
    Write-Host "install-wix: SHA-512 del paquete WiX $Version instalado correcto"

    if (-not $ToolPath) {
        if ($env:GITHUB_PATH) { Add-Content -LiteralPath $env:GITHUB_PATH -Value $toolsDir }
        $wix = Get-Command wix -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $wix -and $env:GITHUB_PATH) {
            Write-Host "install-wix: wix estara en el PATH en los pasos siguientes ($toolsDir)"
        } elseif (-not $wix) {
            throw "wix no esta en el PATH: anade $toolsDir"
        }
    }
    Write-Host 'install-wix: OK'
} catch {
    Write-Host "install-wix: ERROR: $($_.Exception.Message)" -ForegroundColor Red
    $code = 1
} finally {
    if ($work -and (Test-Path -LiteralPath $work)) {
        $leaf = Split-Path -Leaf $work
        if ($leaf.StartsWith('tesela-wix-') -and (Split-Path -Parent $work) -eq [System.IO.Path]::GetTempPath().TrimEnd('\')) {
            Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}
exit $code
