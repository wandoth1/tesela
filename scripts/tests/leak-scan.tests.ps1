<#
.SYNOPSIS
  Pruebas de scripts/leak-scan.ps1 (la busqueda de valores locales en los binarios de build-release.ps1).

.DESCRIPTION
  Los valores se construyen en tiempo de ejecucion o salen del entorno: ningun dato de la maquina vive en este archivo.

.EXAMPLE
  pwsh -File scripts/tests/leak-scan.tests.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
. (Join-Path (Split-Path -Parent $PSScriptRoot) 'leak-scan.ps1')

$failures = New-Object System.Collections.Generic.List[string]
$total = 0
function Check([string]$Case, [bool]$Condition) {
    $script:total++
    if (-not $Condition) { $script:failures.Add($Case) }
}

function Latin1([string]$s) { return [System.Text.Encoding]::GetEncoding(28591).GetBytes($s) }
function Pad([byte[]]$b) { return [byte[]](0, 1, 2, 3, 255) + $b + [byte[]](0, 9, 8, 7) }

# Valores inventados, partidos para que ninguna regla de privacidad vea una ruta literal.
$repoSlash = ('Q' + ':/' + 'trabajo/' + 'proyecto-x')
$repoBack = $repoSlash.Replace('/', '\')
$cargo = ('Q' + ':\' + 'cargo-home')
$needles = @{ 'repositorio' = $repoSlash; 'CARGO_HOME' = $cargo }

# --- Rutas con `/` y con `\`: se encuentran las dos formas, cualquiera que sea la que el valor trae --------------------------
foreach ($form in @($repoSlash, $repoBack)) {
    $bytes = Pad (Latin1 "ruta: $form\src\main.rs")
    $l = Find-LocalValueLeaks -Bytes $bytes -Needles $needles -Name 'a.exe'
    Check "repositorio con barras del tipo '$form' en ASCII" ($l.Count -eq 1 -and $l[0] -like "*'repositorio'*")
    $u16 = Pad ([System.Text.Encoding]::Unicode.GetBytes("ruta: $form\src\main.rs"))
    $l = Find-LocalValueLeaks -Bytes $u16 -Needles $needles -Name 'a.exe'
    Check "repositorio con barras del tipo '$form' en UTF-16" ($l.Count -eq 1)
    $u16b = [byte[]](7) + $u16
    $l = Find-LocalValueLeaks -Bytes $u16b -Needles $needles -Name 'a.exe'
    Check "repositorio en UTF-16 con la otra alineacion ('$form')" ($l.Count -eq 1)
}
# Sin mayusculas exactas tambien.
$l = Find-LocalValueLeaks -Bytes (Pad (Latin1 $repoBack.ToUpperInvariant())) -Needles $needles -Name 'a.exe'
Check 'sin distinguir mayusculas' ($l.Count -eq 1)
# Un valor que no esta no da fuga, y un binario limpio tampoco.
$l = Find-LocalValueLeaks -Bytes (Pad (Latin1 'nada que ver con la maquina')) -Needles $needles -Name 'a.exe'
Check 'binario limpio' ($l.Count -eq 0)
$l = Find-LocalValueLeaks -Bytes ([byte[]](1)) -Needles $needles -Name 'a.exe'
Check 'binario diminuto no falla' ($l.Count -eq 0)

# --- El nombre del equipo del entorno -----------------------------------------------------------------------------------
$pc = $env:COMPUTERNAME
if ($pc -and $pc.Length -ge 3) {
    $env_needles = Get-LocalValueNeedles -Repo $repoSlash -CargoHome $cargo -Sysroot $null -RustupHome $null
    Check 'las claves incluyen COMPUTERNAME' ($env_needles.ContainsKey('COMPUTERNAME') -and $env_needles['COMPUTERNAME'] -eq $pc)
    $l = Find-LocalValueLeaks -Bytes (Pad (Latin1 "host=$pc;")) -Needles $env_needles -Name 'a.exe'
    Check 'el nombre del equipo suelto en ASCII' (@($l | Where-Object { $_ -like "*'COMPUTERNAME'*" }).Count -eq 1)
    $l = Find-LocalValueLeaks -Bytes (Pad ([System.Text.Encoding]::Unicode.GetBytes("host=$pc;"))) -Needles $env_needles -Name 'a.exe'
    Check 'el nombre del equipo en UTF-16' (@($l | Where-Object { $_ -like "*'COMPUTERNAME'*" }).Count -eq 1)
    # Pegado a letras o digitos NO cuenta (un autor en el texto de una licencia).
    $l = Find-LocalValueLeaks -Bytes (Pad (Latin1 "zz${pc}zz")) -Needles $env_needles -Name 'a.exe'
    Check 'el nombre del equipo pegado a letras no cuenta' (@($l | Where-Object { $_ -like "*'COMPUTERNAME'*" }).Count -eq 0)
}
$user = $env:USERNAME
if ($user -and $user.Length -ge 3) {
    $env_needles = Get-LocalValueNeedles -Repo $repoSlash -CargoHome $cargo -Sysroot $null -RustupHome $null
    $l = Find-LocalValueLeaks -Bytes (Pad (Latin1 "usuario=$user;")) -Needles $env_needles -Name 'a.exe'
    Check 'el usuario suelto' (@($l | Where-Object { $_ -like "*'USERNAME'*" }).Count -eq 1)
}

if ($failures.Count -gt 0) {
    Write-Host "leak-scan.tests: $($failures.Count) de $total pruebas FALLAN" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  - $_" }
    exit 1
}
Write-Host "leak-scan.tests: $total pruebas OK"
exit 0
