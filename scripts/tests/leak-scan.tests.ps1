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

# --- UTF-16 con vecinos arbitrarios (los limites de palabra NO pueden depender de \p{L}) -----------------------------------------
# Un par de bytes cualquiera leido como UTF-16 casi siempre es una letra Unicode (CJK, hangul...): si el limite de palabra contase
# esas letras, un valor pegado a un vecino al azar no se veria nunca. Valor SINTETICO "Zorbaq" (no se parece a ningun usuario).
$zNeedles = @{ 'USERNAME' = 'Zorbaq'; 'COMPUTERNAME' = 'ZORBAQ-PC' }
function ZLeaks($Bytes, [string]$Key) {
    $l = Find-LocalValueLeaks -Bytes $Bytes -Needles $zNeedles -Name 'a.exe'
    return @($l | Where-Object { $_ -like "*'$Key'*" }).Count
}
$cjkL = [string][char]0x4E2D
$cjkR = [string][char]0x6587
foreach ($case in @(@('USERNAME', 'Zorbaq'), @('COMPUTERNAME', 'ZORBAQ-PC'))) {
    $u16 = [System.Text.Encoding]::Unicode.GetBytes($cjkL + $case[1] + $cjkR)
    Check "UTF-16 alineado: $($case[0]) entre dos caracteres CJK" ((ZLeaks (Pad $u16) $case[0]) -ge 1)
    Check "UTF-16 desalineado: $($case[0]) entre dos caracteres CJK" ((ZLeaks ([byte[]](7) + (Pad $u16)) $case[0]) -ge 1)
}
# Vecinos al azar (semilla fija: determinista): al menos el 95 % tiene que verse en UTF-16, alineado y desalineado.
$rnd = New-Object System.Random 20261016
$hits = 0
$tries = 400
for ($i = 0; $i -lt $tries; $i++) {
    $left = New-Object byte[] 2
    $right = New-Object byte[] 2
    $rnd.NextBytes($left)
    $rnd.NextBytes($right)
    $body = [System.Text.Encoding]::Unicode.GetBytes('Zorbaq')
    $bytes = $left + $body + $right
    if ($i % 2 -eq 1) { $bytes = [byte[]](7) + $bytes }
    if ((ZLeaks $bytes 'USERNAME') -ge 1) { $hits++ }
}
Check "UTF-16 con vecinos al azar: $hits de $tries" ($hits -ge ($tries * 0.95))
# Un valor ASCII con una letra latina acentuada DETRAS no cuenta en las decodificaciones nativas UTF-8 y UTF-16. Es lo unico que
# garantiza el limite: con la acentuada delante, o detras en Latin-1, SIGUE contando (falso positivo del lado seguro; ver el
# comentario de leak-scan.ps1), y por eso aqui solo se prueba UTF-8 y UTF-16.
$zAcc = 'Zorbaq' + $eAcute
foreach ($pair in @(@('UTF-8', (Pad ($utf8nb.GetBytes($zAcc)))), @('UTF-16', (Pad ([System.Text.Encoding]::Unicode.GetBytes($zAcc)))))) {
    Check "ASCII pegado a una letra acentuada no cuenta ($($pair[0]))" ((ZLeaks $pair[1] 'USERNAME') -eq 0)
}

# --- Valores partidos por bytes NUL sueltos: se busca ADEMAS en los bytes sin NUL ----------------------------------------------
function Nul([byte[]]$a, [byte[]]$b) { return [byte[]]($a + [byte[]](0) + $b) }
$zAscii = [System.Text.Encoding]::ASCII
# El usuario sintetico "Zorbaq" partido en cualquier punto, en ASCII, con el NUL en medio.
foreach ($cut in 1, 3, 5) {
    $bytes = Pad (Nul $zAscii.GetBytes('usuario=' + 'Zorbaq'.Substring(0, $cut)) $zAscii.GetBytes('Zorbaq'.Substring($cut) + ';'))
    Check "NUL suelto dentro del usuario (corte $cut)" ((ZLeaks $bytes 'USERNAME') -ge 1)
}
# Varios NUL repartidos, y el equipo.
$bytes = Pad ($zAscii.GetBytes('x ') + [byte[]](0) + $zAscii.GetBytes('ZOR') + [byte[]](0, 0) + $zAscii.GetBytes('BAQ-') + [byte[]](0) + $zAscii.GetBytes('PC y'))
Check 'varios NUL dentro del nombre del equipo' ((ZLeaks $bytes 'COMPUTERNAME') -ge 1)
# Una ruta (con `/` y con `\`) partida por un NUL.
foreach ($form in @($repoSlash, $repoBack)) {
    $half = [int]($form.Length / 2)
    $bytes = Pad (Nul (Latin1 ('ruta: ' + $form.Substring(0, $half))) (Latin1 ($form.Substring($half) + '\src')))
    $l = Find-LocalValueLeaks -Bytes $bytes -Needles $needles -Name 'a.exe'
    Check "ruta partida por un NUL ('$form')" ($l.Count -eq 1 -and $l[0] -like "*'repositorio'*")
}
# Un valor con letras acentuadas en UTF-8 partido por un NUL: solo se ve con el UTF-8 de los bytes sin NUL (un NUL entre dos
# valores ASCII no hace falta decodificarlo asi, pero uno entre los dos bytes de una letra acentuada, si).
$uBytes = $utf8nb.GetBytes('usuario=' + $uUser + ';')
$at = 9   # tras "usuario=" y el primer byte (0xC3) de la primera letra acentuada
$cutBytes = [byte[]]($uBytes[0..$at]) + [byte[]](0) + [byte[]]($uBytes[($at + 1)..($uBytes.Length - 1)])
Check 'valor con acentos en UTF-8 partido por un NUL' ((Leaks (Pad $cutBytes) 'USERNAME') -ge 1)
# El mismo valor en Latin-1/ANSI (una letra acentuada es UN byte) partido por un NUL, en tres puntos: no es UTF-8 valido, asi que solo
# lo ve el Latin-1 de los bytes sin NUL.
foreach ($cut in 1, 2, 5) {
    $latCut = Pad (Nul (Latin1 ('usuario=' + $uUser.Substring(0, $cut))) (Latin1 ($uUser.Substring($cut) + ';')))
    Check "valor con acentos en Latin-1 partido por un NUL (corte $cut)" ((Leaks $latCut 'USERNAME') -ge 1)
}
$latCutPc = Pad (Nul (Latin1 ('host=' + $uPc.Substring(0, 9))) (Latin1 ($uPc.Substring(9) + ';')))
Check 'equipo con acentos en Latin-1 partido por un NUL' ((Leaks $latCutPc 'COMPUTERNAME') -ge 1)
# El UTF-16 sigue funcionando con sus NUL (el texto sin NUL es ADICIONAL): alineado, desalineado y con NUL extra dentro del valor.
$u16z = [System.Text.Encoding]::Unicode.GetBytes('usuario=Zorbaq;')
Check 'UTF-16 alineado sigue viendose' ((ZLeaks (Pad $u16z) 'USERNAME') -ge 1)
Check 'UTF-16 desalineado sigue viendose' ((ZLeaks ([byte[]](7) + (Pad $u16z)) 'USERNAME') -ge 1)
$u16cut = [System.Text.Encoding]::Unicode.GetBytes('usuario=Zor' + [string][char]0 + 'baq;')
Check 'UTF-16 con un NUL dentro del valor' ((ZLeaks (Pad $u16cut) 'USERNAME') -ge 1)
# Negativos: texto limpio con NUL, y el valor pegado a otra letra tras quitar los NUL (mismo criterio de limites de palabra).
$bytes = Pad (Nul $zAscii.GetBytes('nada que ver ') $zAscii.GetBytes('con la maquina'))
Check 'texto limpio con un NUL' ((ZLeaks $bytes 'USERNAME') -eq 0 -and (ZLeaks $bytes 'COMPUTERNAME') -eq 0)
$bytes = Pad (Nul $zAscii.GetBytes('xZ') $zAscii.GetBytes('orbaqx'))
Check 'valor pegado a letras tras quitar los NUL no cuenta' ((ZLeaks $bytes 'USERNAME') -eq 0)
# Un binario grande de 6 MiB con muchos NUL se revisa en tiempo razonable (el texto sin NUL es un texto mas).
$big = New-Object byte[] (6MB)
for ($i = 0; $i -lt $big.Length; $i += 3) { $big[$i] = 0x41 }
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$l = Find-LocalValueLeaks -Bytes $big -Needles $needles -Name 'grande.exe'
$sw.Stop()
Check "binario de 6 MiB con muchos NUL: sin falsos positivos ($($l.Count))" ($l.Count -eq 0)
Check "binario de 6 MiB con muchos NUL: $([int]$sw.Elapsed.TotalSeconds) s (maximo 60)" ($sw.Elapsed.TotalSeconds -lt 60)

if ($failures.Count -gt 0) {
    Write-Host "leak-scan.tests: $($failures.Count) de $total pruebas FALLAN" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  - $_" }
    exit 1
}
Write-Host "leak-scan.tests: $total pruebas OK"
exit 0
