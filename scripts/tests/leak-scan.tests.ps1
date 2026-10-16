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

# --- Valores con letras NO ASCII (nombres de usuario/equipo con acentos o enes) -----------------------------------------------
# Sinteticos y construidos con [char]0x...: este archivo es solo ASCII (Windows PowerShell 5.1 lee los .ps1 sin BOM como ANSI).
# Aqui no hay ningun dato de la maquina: el usuario "Inigo-Prueba" (I y n acentuadas) y el equipo "EQUIPO-NANDU" (N y U
# acentuadas) no existen.
$uUser = ([string][char]0x00CD) + ([string][char]0x00F1) + 'igo-Prueba'
$uPc = 'EQUIPO-' + ([string][char]0x00D1) + 'AND' + ([string][char]0x00DA)
$uProfile = ('Q' + ':\' + 'Users\') + $uUser
$eAcute = [string][char]0x00E9
$uNeedles = @{ 'USERNAME' = $uUser; 'COMPUTERNAME' = $uPc; 'USERPROFILE' = $uProfile }
$utf8nb = New-Object System.Text.UTF8Encoding($false)
function Leaks($Bytes, [string]$Key) {
    $l = Find-LocalValueLeaks -Bytes $Bytes -Needles $uNeedles -Name 'a.exe'
    return @($l | Where-Object { $_ -like "*'$Key'*" }).Count
}
# Cada texto se prueba en cuatro codificaciones: Latin-1 (ANSI de Windows), UTF-8 (lo que escribe el compilador de Rust),
# UTF-16 LE alineado y UTF-16 LE desalineado (un byte de mas delante).
function Encodings([string]$Text) {
    $u16 = [System.Text.Encoding]::Unicode.GetBytes($Text)
    return [ordered]@{
        'Latin-1'            = Pad (Latin1 $Text)
        'UTF-8'              = Pad ($utf8nb.GetBytes($Text))
        'UTF-16 alineado'    = Pad $u16
        'UTF-16 desalineado' = [byte[]](7) + (Pad $u16)
    }
}
foreach ($case in @(
        @('USERNAME', "usuario=$uUser;"),
        @('USERNAME', "x $uUser"),
        @('USERNAME', (('Q' + ':\' + 'Users\') + $uUser + '\.cargo\registry')),
        @('COMPUTERNAME', "host=$uPc;"),
        @('USERPROFILE', "ruta: $uProfile\src\main.rs"),
        @('USERPROFILE', ("ruta: $uProfile\src\main.rs").Replace('\', '/'))
    )) {
    $encs = Encodings $case[1]
    foreach ($e in $encs.Keys) {
        Check "no ASCII: $($case[0]) en $e ('$($case[1].Length) caracteres')" ((Leaks $encs[$e] $case[0]) -ge 1)
    }
}
# Sin distinguir mayusculas, tambien con letras acentuadas.
$encs = Encodings ("usuario=" + $uUser.ToLowerInvariant() + ';')
foreach ($e in $encs.Keys) { Check "no ASCII: minusculas en $e" ((Leaks $encs[$e] 'USERNAME') -ge 1) }
$encs = Encodings ("usuario=" + $uUser.ToUpperInvariant() + ';')
foreach ($e in $encs.Keys) { Check "no ASCII: mayusculas en $e" ((Leaks $encs[$e] 'USERNAME') -ge 1) }
# UTF-8 permisivo: bytes invalidos alrededor (y en medio de un binario) no impiden ver el valor ni inventan otro.
$bin = [byte[]](0xFF, 0xFE, 0xC3, 0x28, 0x80) + $utf8nb.GetBytes("host=$uPc;") + [byte[]](0xC3, 0xFF, 0x00)
Check 'UTF-8 con bytes invalidos alrededor sigue encontrando el equipo' ((Leaks $bin 'COMPUTERNAME') -ge 1)
# Negativos: pegado a otras letras o digitos NO cuenta (ASCII o acentuadas), con cualquier codificacion.
foreach ($glued in @(
        @('letras ASCII a los dos lados', ('zz' + $uUser + 'zz')),
        @('un digito delante', ('7' + $uUser)),
        @('una letra acentuada delante', ($eAcute + $uUser)),
        @('una letra acentuada detras', ($uUser + $eAcute)),
        @('el equipo con una letra ASCII detras', ($uPc + 'x')),
        @('el equipo con una letra acentuada delante', ($eAcute + $uPc))
    )) {
    $key = if ($glued[1].Contains($uPc)) { 'COMPUTERNAME' } else { 'USERNAME' }
    $encs = Encodings $glued[1]
    foreach ($e in $encs.Keys) { Check "no ASCII pegado ($($glued[0])) en $e no cuenta" ((Leaks $encs[$e] $key) -eq 0) }
}
# Separado por un signo o un espacio SI cuenta.
$encs = Encodings ("(" + $uUser + ")")
foreach ($e in $encs.Keys) { Check "no ASCII entre parentesis en $e" ((Leaks $encs[$e] 'USERNAME') -ge 1) }
# Un binario sin el valor no da fuga, con ninguna codificacion.
$encs = Encodings 'nada que ver con la maquina, ni con ninguna otra'
foreach ($e in $encs.Keys) { Check "no ASCII: texto limpio en $e" ((Leaks $encs[$e] 'USERNAME') -eq 0 -and (Leaks $encs[$e] 'COMPUTERNAME') -eq 0 -and (Leaks $encs[$e] 'USERPROFILE') -eq 0) }
# Y el valor "casi" (otra letra acentuada) tampoco.
$near = ([string][char]0x00D3) + ([string][char]0x00F1) + 'igo-Prueba'
$encs = Encodings ("usuario=" + $near + ';')
foreach ($e in $encs.Keys) { Check "no ASCII: valor parecido (otra letra) en $e no cuenta" ((Leaks $encs[$e] 'USERNAME') -eq 0) }

if ($failures.Count -gt 0) {
    Write-Host "leak-scan.tests: $($failures.Count) de $total pruebas FALLAN" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  - $_" }
    exit 1
}
Write-Host "leak-scan.tests: $total pruebas OK"
exit 0
