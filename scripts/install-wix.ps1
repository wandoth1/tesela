<#
.SYNOPSIS
  Installs WiX Toolset (the `wix` CLI, global `dotnet tool`) at the PINNED version, checking the package SHA-512 BEFORE
  installing anything.

.DESCRIPTION
  Used by ci.yml and release.yml, and available locally. It performs these steps in order:
    1. If `wix` is already installed, checks that it is the pinned version and that its package SHA-512 (the .nupkg in
       dotnet's package store) matches the expected value; it does not install or remove anything.
    2. Otherwise, DOWNLOADS the .nupkg from nuget.org (HTTPS flat container) to a dedicated temporary directory, computes
       its SHA-512, and compares it with the value pinned here (the value published by nuget.org in the package catalog).
       If they differ, it FAILS without installing anything.
    3. Only after the hash matches, installs FROM THAT VERIFIED LOCAL FILE: `dotnet tool install ... --source <directory>
       --configfile installer/nuget.config`. `--source` makes that directory the ONLY source (dotnet does not fetch the
       package again, so the installed file is the one checked), and `installer/nuget.config` (`<clear/>` and nuget.org
       only) makes dotnet ignore other machine NuGet configuration. It then checks the installed package hash again.
    4. In GitHub Actions, adds the dotnet tools directory to PATH for subsequent steps.
  To uninstall: `dotnet tool uninstall --global wix`.

  To upgrade, update the version in scripts/build-msi.ps1, ci.yml, release.yml, and this file (including its SHA-512 from
  https://api.nuget.org/v3/catalog0/ -> the package's packageHash), then repeat the installation cycle. The test
  scripts/tests/wix-version.tests.ps1 checks that they match.

  The script is ASCII-only by design (Windows PowerShell 5.1 reads .ps1 files without a BOM as ANSI).

.PARAMETER Version
  WiX version. Only versions listed in the table below are supported.

.PARAMETER ToolPath
  Installs (and searches) in this directory instead of dotnet's global tool store (`--tool-path`). For tests and anyone
  who does not want to modify the global store; does not change PATH.

.PARAMETER NupkgPath
  Uses this local .nupkg instead of downloading it (offline); it goes through EXACTLY the same SHA-512 check. For tests.

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

# Version -> SHA-512 (base64) of the .nupkg, as published by nuget.org (packageHash in the package catalog).
$KnownHashes = @{
    '5.0.2' = 'z+fu1deTcaV+TRCo4+6HrJ48Sdnj+yeQjjMStTI3GW7NxFEPaknYCdxrf0qEvgdXrjYdjLnkZKCsNSqTTOo2OA=='
}
if (-not $KnownHashes.ContainsKey($Version)) {
    throw "WiX $Version is not in the known-version table in install-wix.ps1 (supported: $($KnownHashes.Keys -join ', '))"
}

function Get-Sha512Base64([string]$Path) {
    $sha = [System.Security.Cryptography.SHA512]::Create()
    $stream = [System.IO.File]::OpenRead($Path)
    try { return [Convert]::ToBase64String($sha.ComputeHash($stream)) } finally { $stream.Dispose(); $sha.Dispose() }
}

function Assert-PackageHash([string]$Path, [string]$What) {
    $actual = Get-Sha512Base64 $Path
    if ($actual -cne $KnownHashes[$Version]) {
        throw "WiX $Version package SHA-512 ($What) does not match the expected value (expected $($KnownHashes[$Version]), got $actual)"
    }
}

$work = $null
$code = 0
try {
    $repo = (& git rev-parse --show-toplevel).Trim()
    $config = Join-Path $repo 'installer/nuget.config'
    if (-not (Test-Path -LiteralPath $config)) { throw "missing $config" }

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
        throw "WiX $($installed[0]) is already installed, but this repository pins ${Version}: dotnet tool uninstall --global wix"
    }

    $nupkg = Join-Path $toolsDir ".store/wix/$Version/wix/$Version/wix.$Version.nupkg"
    if ($installed.Count -eq 0) {
        # --- 1. Download the package to its own directory and verify its hash BEFORE installing anything -----------------
        $work = Join-Path ([System.IO.Path]::GetTempPath()) ('tesela-wix-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $work | Out-Null
        $local = Join-Path $work "wix.$Version.nupkg"
        if ($NupkgPath) {
            Write-Host "install-wix: using local package $NupkgPath"
            Copy-Item -LiteralPath $NupkgPath -Destination $local
        } else {
            $url = "https://api.nuget.org/v3-flatcontainer/wix/$Version/wix.$Version.nupkg"
            Write-Host "install-wix: downloading $url"
            $ProgressPreference = 'SilentlyContinue'
            Invoke-WebRequest -Uri $url -OutFile $local -UseBasicParsing
        }
        Assert-PackageHash $local 'to install'
        Write-Host "install-wix: WiX $Version package SHA-512 verified (before installation)"

        # --- 2. Install FROM that verified file (only source) ----------------------------------------------------------
        $installArgs = @('tool', 'install', 'wix', '--version', $Version, '--source', $work, '--configfile', $config)
        if ($ToolPath) { $installArgs += @('--tool-path', $ToolPath) } else { $installArgs += '--global' }
        Write-Host "install-wix: dotnet $($installArgs -join ' ')"
        & dotnet @installArgs
        if ($LASTEXITCODE -ne 0) { throw 'dotnet tool install failed' }
    } else {
        Write-Host "install-wix: WiX $Version is already installed"
    }

    # --- 3. Verify that the package in the store matches (also when it was already installed) ---------------------------
    if (-not (Test-Path -LiteralPath $nupkg)) { throw "installed package not found: $nupkg" }
    Assert-PackageHash $nupkg 'installed'
    Write-Host "install-wix: installed WiX $Version package SHA-512 verified"

    if (-not $ToolPath) {
        if ($env:GITHUB_PATH) { Add-Content -LiteralPath $env:GITHUB_PATH -Value $toolsDir }
        $wix = Get-Command wix -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $wix -and $env:GITHUB_PATH) {
            Write-Host "install-wix: wix will be on PATH in subsequent steps ($toolsDir)"
        } elseif (-not $wix) {
            throw "wix is not on PATH: add $toolsDir"
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
