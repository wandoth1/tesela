<#
.SYNOPSIS
  Comprueba que la version de WiX es la misma en scripts/build-msi.ps1, .github/workflows/ci.yml y
  .github/workflows/release.yml (y en installer/README.md), y que el UpgradeCode del .wxs es el que espera build-msi.ps1.

.DESCRIPTION
  Los tres sitios fijan la version (`$WixVersion = 'x.y.z'` en el script; `WIX_VERSION: "x.y.z"` en los workflows) y un
  cambio en uno solo romperia el build del MSI en el CI o, peor, usaria otra herramienta en el release.

.EXAMPLE
  pwsh -File scripts/tests/wix-version.tests.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$failures = New-Object System.Collections.Generic.List[string]
$total = 0
function Check([string]$Case, [bool]$Condition) {
    $script:total++
    if (-not $Condition) { $script:failures.Add($Case) }
}
function Read-Text([string]$Rel) { return [System.IO.File]::ReadAllText((Join-Path $repo $Rel)) }
function First-Match([string]$Text, [string]$Pattern) {
    $m = [regex]::Match($Text, $Pattern, [System.Text.RegularExpressions.RegexOptions]::Multiline)
    if ($m.Success) { return $m.Groups[1].Value } else { return $null }
}

$script = Read-Text 'scripts/build-msi.ps1'
$scriptVersion = First-Match $script "^\`$WixVersion\s*=\s*'([^']+)'"
$ciVersion = First-Match (Read-Text '.github/workflows/ci.yml') '^\s*WIX_VERSION:\s*"([^"]+)"'
$relVersion = First-Match (Read-Text '.github/workflows/release.yml') '^\s*WIX_VERSION:\s*"([^"]+)"'
$readme = Read-Text 'installer/README.md'

Check 'build-msi.ps1 fija la version de WiX' ($null -ne $scriptVersion -and $scriptVersion -match '^\d+\.\d+\.\d+$')
Check "ci.yml fija WIX_VERSION ($ciVersion) igual que build-msi.ps1 ($scriptVersion)" ($ciVersion -eq $scriptVersion)
Check "release.yml fija WIX_VERSION ($relVersion) igual que build-msi.ps1 ($scriptVersion)" ($relVersion -eq $scriptVersion)
Check 'installer/README.md cita la version fijada' ($scriptVersion -and $readme.Contains("--version $scriptVersion"))
foreach ($f in '.github/workflows/ci.yml', '.github/workflows/release.yml') {
    Check "$f instala WiX con la variable WIX_VERSION y nunca otra version a mano" (
        (Read-Text $f).Contains('--version $env:WIX_VERSION') -and ([regex]::Matches((Read-Text $f), 'dotnet tool install')).Count -eq 1)
}
Check 'el .wxs cita la misma version de WiX en su cabecera' ((Read-Text 'installer/Tesela.wxs').Contains("WiX Toolset $scriptVersion"))

# UpgradeCode: el del .wxs y el que valida build-msi.ps1.
$wxs = Read-Text 'installer/Tesela.wxs'
$wxsCode = First-Match $wxs 'UpgradeCode="([0-9A-Fa-f-]{36})"'
$scriptCode = First-Match $script "^\`$ExpectedUpgradeCode\s*=\s*'\{([0-9A-Fa-f-]{36})\}'"
Check 'el .wxs tiene un UpgradeCode' ($null -ne $wxsCode)
Check "el UpgradeCode del .wxs ($wxsCode) es el que espera build-msi.ps1 ($scriptCode)" ($wxsCode -and $scriptCode -and $wxsCode -ieq $scriptCode)
Check 'los GUID de los componentes del .wxs son todos distintos' (
    @([regex]::Matches($wxs, 'Guid="([0-9A-Fa-f-]{36})"') | ForEach-Object { $_.Groups[1].Value.ToUpperInvariant() } | Sort-Object -Unique).Count -eq
    [regex]::Matches($wxs, 'Guid="([0-9A-Fa-f-]{36})"').Count)

if ($failures.Count -gt 0) {
    Write-Host "wix-version.tests: $($failures.Count) de $total comprobaciones fallan:" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host "wix-version.tests: OK ($total comprobaciones)"
