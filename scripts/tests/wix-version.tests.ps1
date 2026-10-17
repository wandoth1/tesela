<#
.SYNOPSIS
  Comprueba que la version de WiX es la misma en scripts/build-msi.ps1, .github/workflows/ci.yml y
  .github/workflows/release.yml (y en installer/README.md), que el UpgradeCode del .wxs es el que espera msi-validate.ps1 y que
  scripts/install-wix.ps1 comprueba el SHA-512 del paquete ANTES de instalar (con un paquete incorrecto no se instala nada).

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
$installer = Read-Text 'scripts/install-wix.ps1'
Check "install-wix.ps1 conoce WiX $scriptVersion y su SHA-512" ($scriptVersion -and [regex]::IsMatch($installer, "'" + [regex]::Escape($scriptVersion) + "'\s*=\s*'[A-Za-z0-9+/]{86}=='"))
foreach ($f in '.github/workflows/ci.yml', '.github/workflows/release.yml') {
    $t = Read-Text $f
    Check "$f instala WiX con scripts/install-wix.ps1 y la variable WIX_VERSION, y nunca con otra orden" (
        $t.Contains('./scripts/install-wix.ps1 -Version $env:WIX_VERSION') -and -not $t.Contains('dotnet tool install'))
}
Check 'el .wxs cita la misma version de WiX en su cabecera' ((Read-Text 'installer/Tesela.wxs').Contains("WiX Toolset $scriptVersion"))

# UpgradeCode: el del .wxs y el que valida build-msi.ps1.
$wxs = Read-Text 'installer/Tesela.wxs'
$wxsCode = First-Match $wxs 'UpgradeCode="([0-9A-Fa-f-]{36})"'
$scriptCode = First-Match (Read-Text 'scripts/msi-validate.ps1') "^\`$MsiExpectedUpgradeCode\s*=\s*'\{([0-9A-Fa-f-]{36})\}'"
Check 'el .wxs tiene un UpgradeCode' ($null -ne $wxsCode)
Check "el UpgradeCode del .wxs ($wxsCode) es el que espera msi-validate.ps1 ($scriptCode)" ($wxsCode -and $scriptCode -and $wxsCode -ieq $scriptCode)
Check 'los GUID de los componentes del .wxs son todos distintos' (
    @([regex]::Matches($wxs, 'Guid="([0-9A-Fa-f-]{36})"') | ForEach-Object { $_.Groups[1].Value.ToUpperInvariant() } | Sort-Object -Unique).Count -eq
    [regex]::Matches($wxs, 'Guid="([0-9A-Fa-f-]{36})"').Count)

# --- install-wix.ps1: el SHA-512 se comprueba ANTES de instalar, y se instala desde el archivo verificado --------------------
$iw = $installer
$posHash = $iw.IndexOf('Assert-PackageHash $local')
$posInstall = $iw.IndexOf("'tool', 'install'")
Check 'install-wix.ps1 comprueba el hash del paquete descargado ANTES de la orden de instalar' ($posHash -ge 0 -and $posInstall -gt $posHash)
Check 'install-wix.ps1 instala desde la carpeta verificada como unica fuente (--source) y con installer/nuget.config' ($iw.Contains("'--source', `$work") -and $iw.Contains("'--configfile', `$config"))
Check 'installer/nuget.config sigue con <clear/> y solo nuget.org' (
    (Read-Text 'installer/nuget.config') -match '<clear\s*/>' -and @([regex]::Matches((Read-Text 'installer/nuget.config'), '<add\s')).Count -eq 1)

$dotnet = Get-Command dotnet -ErrorAction SilentlyContinue | Select-Object -First 1
if ($dotnet) {
    $self = (Get-Process -Id $PID).Path
    $iwScript = Join-Path $repo 'scripts/install-wix.ps1'
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('tesela-test-' + [guid]::NewGuid().ToString())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    function Run-Install([string[]]$Extra) {
        $prev = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $out = & $self -NoProfile -File $iwScript @Extra 2>&1 | Out-String
            return @{ Code = $LASTEXITCODE; Out = $out }
        } finally { $ErrorActionPreference = $prev }
    }
    function Installed-In([string]$Dir) { return (Test-Path -LiteralPath (Join-Path $Dir 'wix.exe')) -or (Test-Path -LiteralPath (Join-Path $Dir '.store')) }
    try {
        # Un paquete que no es el esperado: ni se instala nada ni se crea la carpeta de herramientas.
        $junk = Join-Path $tmp 'junk.nupkg'
        [System.IO.File]::WriteAllBytes($junk, [byte[]](1, 2, 3, 4, 5))
        $tp = Join-Path $tmp 'bad-hash'
        $r = Run-Install @('-Version', '5.0.2', '-ToolPath', $tp, '-NupkgPath', $junk)
        Check "hash incorrecto (basura): falla ($($r.Code))" ($r.Code -ne 0 -and $r.Out -like '*SHA-512*')
        Check 'hash incorrecto (basura): no se instalo nada' (-not (Installed-In $tp))
        # Una version fuera de la tabla.
        $r = Run-Install @('-Version', '9.9.9', '-ToolPath', (Join-Path $tmp 'unknown'))
        Check 'version fuera de la tabla: falla' ($r.Code -ne 0)
        # Con el paquete REAL de este equipo (si hay un WiX 5.0.2 global instalado): el mismo, con un byte cambiado, no se instala; el
        # intacto si, desde una carpeta de herramientas temporal (el WiX global no se toca).
        $home2 = $env:DOTNET_CLI_HOME
        if (-not $home2) { $home2 = $env:USERPROFILE }
        $cached = Join-Path $home2 '.dotnet/tools/.store/wix/5.0.2/wix/5.0.2/wix.5.0.2.nupkg'
        if ((Test-Path -LiteralPath $cached) -and $scriptVersion -eq '5.0.2') {
            $bytes = [System.IO.File]::ReadAllBytes($cached)
            $bytes[$bytes.Length - 100] = [byte]($bytes[$bytes.Length - 100] -bxor 1)
            $tampered = Join-Path $tmp 'tampered.nupkg'
            [System.IO.File]::WriteAllBytes($tampered, $bytes)
            $tp = Join-Path $tmp 'tampered'
            $r = Run-Install @('-Version', '5.0.2', '-ToolPath', $tp, '-NupkgPath', $tampered)
            Check "paquete real con UN byte cambiado: falla ($($r.Code))" ($r.Code -ne 0 -and $r.Out -like '*SHA-512*')
            Check 'paquete real con UN byte cambiado: no se instalo nada' (-not (Installed-In $tp))
            $tp = Join-Path $tmp 'good'
            $r = Run-Install @('-Version', '5.0.2', '-ToolPath', $tp, '-NupkgPath', $cached)
            Check "paquete real intacto: se instala desde el archivo verificado ($($r.Code))" ($r.Code -eq 0 -and (Test-Path -LiteralPath (Join-Path $tp 'wix.exe')))
            $r = Run-Install @('-Version', '5.0.2', '-ToolPath', $tp)
            Check 'una segunda ejecucion detecta la instalacion existente y no reinstala' ($r.Code -eq 0 -and $r.Out -like '*is already installed*')
            # Ya hay un WiX 5.0.2 instalado pero con OTRO contenido (el .nupkg del almacen no es el fijado): la comprobacion POSTERIOR lo
            # rechaza. Se manipula la copia de la carpeta de herramientas temporal, nunca el WiX global.
            $store = Join-Path $tp '.store/wix/5.0.2/wix/5.0.2/wix.5.0.2.nupkg'
            Check 'la instalacion de prueba tiene su .nupkg en el almacen' (Test-Path -LiteralPath $store)
            [System.IO.File]::WriteAllBytes($store, [byte[]](9, 8, 7, 6, 5))
            $r = Run-Install @('-Version', '5.0.2', '-ToolPath', $tp)
            Check "WiX ya instalado con un paquete de otro contenido: falla ($($r.Code))" ($r.Code -ne 0 -and $r.Out -like '*SHA-512*installed*')
        } else {
            Write-Host 'wix-version.tests: sin el paquete WiX 5.0.2 en el almacen local, se omiten las pruebas de instalacion real'
        }
    } finally {
        if ((Split-Path -Parent $tmp) -eq [System.IO.Path]::GetTempPath().TrimEnd('\') -and (Split-Path -Leaf $tmp).StartsWith('tesela-test-')) {
            Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
} else {
    Write-Host 'wix-version.tests: sin dotnet, se omiten las pruebas dinamicas de install-wix.ps1'
}

if ($failures.Count -gt 0) {
    Write-Host "wix-version.tests: $($failures.Count) de $total comprobaciones fallan:" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host "wix-version.tests: OK ($total comprobaciones)"
# Salida EXPLICITA: un comando externo anterior (dotnet, git, wix...) deja su codigo en $LASTEXITCODE y GitHub Actions (pwsh -command)
# lo usa como codigo del paso aunque aqui todo haya ido bien.
exit 0
