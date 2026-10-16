<#
.SYNOPSIS
  Instala WiX Toolset (CLI `wix`, `dotnet tool` global) con la version FIJADA y comprueba el SHA-512 del paquete.

.DESCRIPTION
  Lo usan ci.yml y release.yml, y sirve en local. Hace tres cosas:
    1. instala `wix` con `dotnet tool install --global wix --version <v> --configfile installer/nuget.config`. Ese archivo
       (`<clear/>` y solo nuget.org) hace que dotnet ignore cualquier otra fuente o configuracion de NuGet de la maquina;
    2. calcula el SHA-512 del .nupkg que dotnet dejo en su almacen y lo compara con el valor fijado aqui (el mismo que
       publica nuget.org en el catalogo del paquete). Si no coincide, falla sin desinstalar nada: avisa y sale;
    3. en GitHub Actions anade la carpeta de las herramientas de dotnet al PATH de los pasos siguientes.
  Para quitarlo: `dotnet tool uninstall --global wix`.

  Para subir de version hay que cambiarla a la vez en scripts/build-msi.ps1, ci.yml, release.yml y aqui (con su SHA-512, que
  se saca de https://api.nuget.org/v3/catalog0/ -> packageHash del paquete), y repetir el ciclo de instalacion; la prueba
  scripts/tests/wix-version.tests.ps1 vigila que coincidan.

  El script es solo ASCII a proposito (Windows PowerShell 5.1 lee los .ps1 sin BOM como ANSI).

.PARAMETER Version
  Version de WiX. Solo se admiten las que figuran en la tabla de abajo.

.EXAMPLE
  pwsh -File scripts/install-wix.ps1 -Version 5.0.2
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Version
)

$ErrorActionPreference = 'Stop'

# Version -> SHA-512 (base64) del .nupkg, tal como lo publica nuget.org (packageHash del catalogo del paquete).
$KnownHashes = @{
    '5.0.2' = 'z+fu1deTcaV+TRCo4+6HrJ48Sdnj+yeQjjMStTI3GW7NxFEPaknYCdxrf0qEvgdXrjYdjLnkZKCsNSqTTOo2OA=='
}
if (-not $KnownHashes.ContainsKey($Version)) {
    throw "WiX $Version no esta en la tabla de versiones conocidas de install-wix.ps1 (solo: $($KnownHashes.Keys -join ', '))"
}

try {
    $repo = (& git rev-parse --show-toplevel).Trim()
    $config = Join-Path $repo 'installer/nuget.config'
    if (-not (Test-Path -LiteralPath $config)) { throw "falta $config" }

    $installed = @(@(& dotnet tool list --global) | Where-Object { $_ -match '^\s*wix\s+(\S+)' } | ForEach-Object { $Matches[1] })
    if ($installed.Count -gt 0 -and $installed[0] -ne $Version) {
        throw "ya hay WiX $($installed[0]) instalado y este repositorio fija la ${Version}: dotnet tool uninstall --global wix"
    }
    if ($installed.Count -eq 0) {
        Write-Host "install-wix: dotnet tool install --global wix --version $Version"
        & dotnet tool install --global wix --version $Version --configfile $config
        if ($LASTEXITCODE -ne 0) { throw 'dotnet tool install fallo' }
    } else {
        Write-Host "install-wix: WiX $Version ya esta instalado"
    }

    $home2 = $env:DOTNET_CLI_HOME
    if (-not $home2) { $home2 = $env:USERPROFILE }
    $toolsDir = Join-Path $home2 '.dotnet/tools'
    $nupkg = Join-Path $toolsDir ".store/wix/$Version/wix/$Version/wix.$Version.nupkg"
    if (-not (Test-Path -LiteralPath $nupkg)) { throw "no se encuentra el paquete instalado: $nupkg" }
    $sha = [System.Security.Cryptography.SHA512]::Create()
    $stream = [System.IO.File]::OpenRead($nupkg)
    try { $actual = [Convert]::ToBase64String($sha.ComputeHash($stream)) } finally { $stream.Dispose(); $sha.Dispose() }
    if ($actual -ne $KnownHashes[$Version]) {
        throw "el SHA-512 del paquete WiX $Version instalado no es el esperado (esperado $($KnownHashes[$Version]), obtenido $actual)"
    }
    Write-Host "install-wix: SHA-512 del paquete WiX $Version correcto"

    if ($env:GITHUB_PATH) { Add-Content -LiteralPath $env:GITHUB_PATH -Value $toolsDir }
    $wix = Get-Command wix -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $wix -and $env:GITHUB_PATH) {
        Write-Host "install-wix: wix estara en el PATH en los pasos siguientes ($toolsDir)"
    } elseif (-not $wix) {
        throw "wix no esta en el PATH: anade $toolsDir"
    }
    Write-Host 'install-wix: OK'
} catch {
    Write-Host "install-wix: ERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
