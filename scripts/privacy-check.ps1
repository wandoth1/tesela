<#
.SYNOPSIS
  Fails if the repository contains data identifying the developer's machine or account.

.DESCRIPTION
  The repository is PUBLIC and must not contain information from any PC: hardware, OS, accounts, disks,
  personal drive paths, SIDs, etc. Compatible with PowerShell 7 (pwsh) and Windows PowerShell 5.1; the git hook
  requires pwsh. This script is ASCII-only by design (5.1 reads .ps1 files without a BOM as ANSI).

  Modes:
    * (default)      staged files (`git diff --cached`), using index contents. Used by the `pre-commit` hook.
    * -All            all tracked and non-ignored files (CI and full reviews).
    * -MessageFile    a commit message (`commit-msg` hook); checks the ENTIRE message without exceptions, including lines
                      beginning with `#` and content after the scissors line (`# ---- >8 ----`, the diff from
                      `git commit --verbose`). Git trims there only with `--cleanup=scissors`; with `-m`, `-F`,
                      `--cleanup=verbatim`, or `--cleanup=whitespace`, it keeps the content. If a verbose diff contains
                      private data, the commit is blocked (as intended).
    * -To [-From]     a commit range (CI: changes introduced by a push or pull request): every commit message and ALL blobs
                      it introduces, including blobs deleted by a later commit. `-From` is the previous SHA
                      (`github.event.before` or the PR base); if missing, all zeros, or absent from the clone, the full
                      history through `-To` is checked.

  What is checked:
    1. GENERIC patterns (always): SID (local and AzureAD accounts), real profile paths (Windows, `/c/Users/<user>`,
       `/mnt/c/Users/<user>`; legacy `<usuario>` placeholders are accepted too), drives D: through Z:, UNC paths (`\\<server>\<share>`, with any mix of `\` or `/`, including escaped forms and `\\?\UNC\...`),
       CPU/GPU/disk manufacturers and models, antivirus, Windows builds, and driver versions. Reasonable UNC false positives:
       documentation or examples with a network path and a URL WITHOUT a protocol (`//<host>/<path>`,
       which is indistinguishable from a network path when using normal slashes; `https://host/path` and `// comments` do
       not count); use a `<server>` placeholder instead of a name (angle-bracket placeholders are ignored), add the URL
       protocol, or construct the value in hexadecimal.
    2. LOCAL values (only on the developer's machine; never with -GenericOnly or in CI): the username, computer name,
       domain, and profile path, read at runtime from environment variables, plus an untracked local denylist at
       `<git-common-dir>/privacy-denylist` (one term per line, case-insensitive literal matching; blank lines and lines
       beginning with `#` are ignored).
    3. File NAMES use the same rules.
    4. All text is decoded before scanning (UTF-8, UTF-8 with BOM, UTF-16 with or without BOM and, if the bytes are not
       valid UTF-8, Latin-1/ANSI too; each decoding is scanned separately with its own line numbers, so a local value with
       accents is detected). Binaries are scanned for text strings (ASCII and ASCII surrounded by letters from any alphabet)
       in Latin-1 and UTF-16, both alignments (files over 8 MiB are rejected); images are rejected EXCEPT `docs/img/<name>.png`
       generated with --demo (see the exception below).
       OPAQUE or compressed formats (ZIP and derivatives such as docx/xlsx/jar/apk, gzip, 7z, rar, xz, bz2, zstd, lz4,
       lzip, compress, cab, OLE/msi, PDF, sqlite) are rejected by SIGNATURE regardless of extension (ZIP is also detected
       by its end record in the last 64 KiB): their contents cannot be scanned, and an SID inside a ZIP would pass.
       There is no allowlist (`$opaqueAllow` is intentionally empty).
       INHERENT LIMITS that no signature check detects: raw zlib, lzma-alone (`.lzma`), encrypted or obfuscated data, and
       base64 or otherwise transformed text. The only safeguards are that the repository rejects unknown binaries and that
       reviewers inspect changes.
    5. `docs/spike-results.md` MUST NOT exist (results are regenerated separately), and `spike/` is checked with ALL rules,
       including path rules. The only exception is two phase-0 files with synthetic paths since their first commit: only
       their path rules are skipped, and only while their content exactly matches the reviewed version (the closed
       `$spikePathAllow` list, keyed by SHA-256).

  Patterns use bracketed characters (e.g. `Ge[F]orce`) so this script does not match itself and can be checked like any
  other file.

.PARAMETER All
  Check all tracked and non-ignored files instead of only staged files.

.PARAMETER GenericOnly
  Do not use local values or the denylist. This is CI mode; it is also enabled whenever the CI variable is set.

.PARAMETER Local
  Always use local values and the denylist, even when the CI variable is set. Used by the hooks.

.PARAMETER MessageFile
  Check this file as a commit message instead of checking files.

.PARAMETER To
  Full SHA of the final commit in the range to check (messages and blobs it introduces).

.PARAMETER LegacyImageSha256
  Tests only: additional SHA-256 values accepted by -To as historical screenshots (in addition to the script's closed
  `$legacyDemoImages` list).

.PARAMETER From
  Full SHA before the range (`From..To` is checked). If missing, all zeros, or absent from the clone, check the full history
  through `To`.

.EXAMPLE
  pwsh -File scripts/privacy-check.ps1                 # staged files (pre-commit hook)
  pwsh -File scripts/privacy-check.ps1 -All            # entire tree
  pwsh -File scripts/privacy-check.ps1 -MessageFile .git/COMMIT_EDITMSG
#>
[CmdletBinding()]
param(
    [switch]$All,
    [switch]$GenericOnly,
    [switch]$Local,
    [string]$MessageFile,
    [string]$From,
    [string]$To,
    [string[]]$LegacyImageSha256 = @()
)

$ErrorActionPreference = 'Stop'

function Invoke-GitBytes {
    # Ejecuta git y devuelve su salida estandar como bytes SIN pasar por el pipeline de PowerShell (que recodifica).
    param([string]$GitArgs, [string]$WorkDir)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = 'git'
    $psi.Arguments = $GitArgs
    if ($WorkDir) { $psi.WorkingDirectory = $WorkDir }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    $errTask = $p.StandardError.ReadToEndAsync()
    $ms = New-Object System.IO.MemoryStream
    $p.StandardOutput.BaseStream.CopyTo($ms)
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { throw "git $GitArgs failed ($($p.ExitCode)): $($errTask.Result)" }
    return ,$ms.ToArray()
}

function Split-Nul([byte[]]$Bytes) {
    $s = [System.Text.Encoding]::UTF8.GetString($Bytes)
    return @($s.Split([char]0) | Where-Object { $_ -ne '' })
}

$cwd = (Get-Location).Path
$repoRoot = ([System.Text.Encoding]::UTF8.GetString((Invoke-GitBytes 'rev-parse --show-toplevel' $cwd))).Trim()
$commonDir = ([System.Text.Encoding]::UTF8.GetString((Invoke-GitBytes 'rev-parse --git-common-dir' $repoRoot))).Trim()
if (-not [System.IO.Path]::IsPathRooted($commonDir)) { $commonDir = Join-Path $repoRoot $commonDir }

# En CI (variable CI) solo se usan los patrones genericos, salvo que se pida -Local de forma explicita: los hooks lo
# pasan para que un CI definido en el perfil de un desarrollador no desactive en silencio los valores locales.
if ($env:CI -and -not $Local) { $GenericOnly = $true }
if ($Local -and $GenericOnly) {
    Write-Host 'privacy-check: -Local and -GenericOnly cannot be used together' -ForegroundColor Red
    exit 2
}

# --- Reglas ---------------------------------------------------------------------------------------------------------
# Path = $true: regla ligada a rutas. Se aplica en TODO el repositorio, spike/ incluido; la unica excepcion es la lista CERRADA
# `$spikePathAllow` (mas abajo). Los corchetes evitan que el script se detecte a si mismo.
$generic = @(
    @{ Name = 'Windows SID'; Path = $false; Pattern = '(?i)S-1-5-21-\d' },
    @{ Name = 'AzureAD account SID'; Path = $false; Pattern = '(?i)S-1-12-1-\d' },
    @{ Name = 'Real profile path (use <user>)'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_])[A-Z]:[\\/]{1,2}Users[\\/]{1,2}(?!<user>|<usuario>|%|\{|\$|Public\b|Default\b|\*)[^\\/\s''"`]+' },
    @{ Name = 'Real Unix or WSL profile path'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_.])/(mnt/)?[a-z]/Users/(?!<user>|<usuario>|Public\b|Default\b)[^/\s''"`]+' },
    @{ Name = 'Absolute drive path D: through Z:'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_])[D-Z]:[\\/]' },
    @{ Name = 'WSL drive path D: through Z:'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_])/mnt/[d-z](/|$)' },
    # UNC: las barras valen en cualquier mezcla (`\` y `/`). Las tres reglas piden la forma ANCLADA: dos barras seguidas, que
    # no vengan precedidas de otra barra, de `:` (una URL `https://...`), de `.` ni de una letra (`a//b`), y un servidor que
    # empiece por letra, digito o `_` (se excluyen `?`, `.` y `<`: `\\?\C:`, `\\.\pipe` y el marcador `<servidor>`).
    @{ Name = 'UNC network path'; Path = $false; Pattern = '(?<![A-Za-z0-9_\\.:/])[\\/]{2}(?![?.<])[A-Za-z0-9_][A-Za-z0-9_.-]*[\\/][A-Za-z0-9_$]' },
    # Escapada (un literal de Rust, JSON...): cada barra invertida va duplicada, la barra normal no. Hace falta al menos UNA barra
    # invertida duplicada (con solo barras normales ya vale la regla de arriba): dos barras normales al principio con una barra invertida duplicada despues del servidor tambien cuentan.
    @{ Name = 'Escaped UNC network path'; Path = $false; Pattern = '(?<![A-Za-z0-9_\\.:/])(?:(?:\\\\\\\\|\\\\/|/\\\\|//)(?![?.<])[A-Za-z0-9_][A-Za-z0-9_.-]*\\\\|(?:\\\\\\\\|\\\\/|/\\\\)(?![?.<])[A-Za-z0-9_][A-Za-z0-9_.-]*(?:\\\\|[\\/]))[A-Za-z0-9_$]' },
    # `\\?\UNC\` (verbatim) y `\\.\UNC\` (espacio de nombres de dispositivos): las dos formas.
    @{ Name = 'Verbatim UNC path'; Path = $false; Pattern = '(?i)(?:\\\\|[\\/]){2}[?.](?:\\\\|[\\/])UNC(?:\\\\|[\\/])[A-Za-z0-9_]' },
    @{ Name = 'Fabricante o modelo de GPU/CPU/disco'; Path = $false; Pattern = '(?i)\b(N[V]IDIA|Ge[F]orce|R[T]X\s?\d{3,4}\w*|G[T]X\s?\d{3,4}\w*|Rade[o]n|Ry[z]en|Threadrip[p]er|EP[Y]C|X[e]on|Inte[l]\s+Core|Inte[l]\(R\)\s*Core|Core\s+i[3579][ -]|Apple\s+M[1-9]|Sam[s]ung|WD_BL[A]CK|WD\s+Bl[a]ck|Western\s+Dig[i]tal|Sea[g]ate|Kings[t]on|Cruc[i]al|Cors[a]ir|SN8\d\d\w*|H[y]nix|Mic[r]on|Tos[h]iba|SanD[i]sk)\b' },
    @{ Name = 'Antivirus'; Path = $false; Pattern = '(?i)\b(Bitdef[e]nder|Kaspers[k]y|Nor[t]on|McA[f]ee|Av[a]st|AVG\s+Anti\w*|ES[E]T|Malwareby[t]es|Soph[o]s|CrowdStr[i]ke|Sentinel[O]ne|Trend\s*Mi[c]ro|Symant[e]c|F-Se[c]ure|Webr[o]ot|Cylan[c]e|Carbon\s*Bl[a]ck)\b' },
    @{ Name = 'Windows build'; Path = $false; Pattern = '(?i)\bbuild\s+(19|2\d)\d{3}\b' },
    @{ Name = 'Windows version with build'; Path = $false; Pattern = '\b10\.0\.[12]\d{4}\b' },
    @{ Name = 'Driver version'; Path = $false; Pattern = '(?i)\bdriver\s+(version\s+)?v?\d+(\.\d+)+' }
)

# --- Valores locales y lista negra ----------------------------------------------------------------------------------
$localRules = @()
if (-not $GenericOnly) {
    foreach ($pair in @(@('USERNAME', $env:USERNAME), @('COMPUTERNAME', $env:COMPUTERNAME), @('USERDOMAIN', $env:USERDOMAIN), @('USERPROFILE', $env:USERPROFILE))) {
        $val = $pair[1]
        if ($val -and $val.Trim().Length -ge 3) {
            $esc = [regex]::Escape($val.Trim())
            if ($pair[0] -eq 'USERPROFILE') {
                $alt = [regex]::Escape($val.Trim().Replace('\', '/'))
                $pattern = "(?i)($esc|$alt)"
            } else {
                # Limites de palabra: ASCII (letras y digitos; el guion bajo NO es un limite: `usuario_x` y `x_usuario` cuentan) y letras latinas acentuadas (U+00C0-U+024F sin U+00D7 ni U+00F7), NO
                # \p{L}\p{N}: en las cadenas UTF-16 de un binario el vecino de una cadena es un par de bytes cualquiera, que casi
                # siempre es una letra Unicode (CJK...), y con \p{L} el valor pegado a ese vecino no se veria. Un valor como "Ren"
                # no coincide con una palabra latina con acentos que lo contiene. Coste aceptado y del lado seguro: un valor pegado a
                # una letra cirilica, griega o CJK SI cuenta como coincidencia.
                $pattern = "(?i)(?<![0-9A-Za-z\u00C0-\u00D6\u00D8-\u00F6\u00F8-\u024F])$esc(?![0-9A-Za-z\u00C0-\u00D6\u00D8-\u00F6\u00F8-\u024F])"
            }
            $localRules += @{ Name = "Local value of `$env:$($pair[0])"; Path = $false; Pattern = $pattern }
        }
    }
    $deny = Join-Path $commonDir 'privacy-denylist'
    if (Test-Path -LiteralPath $deny -PathType Leaf) {
        $raw = [System.IO.File]::ReadAllBytes($deny)
        foreach ($line in ([System.Text.Encoding]::UTF8.GetString($raw) -split "`n")) {
            $t = $line.Trim()
            if ($t.Length -ge 2 -and -not $t.StartsWith('#')) {
                $localRules += @{ Name = 'Local denylist term'; Path = $false; Pattern = '(?i)' + [regex]::Escape($t) }
            }
        }
    }
}
$rules = @($generic) + @($localRules)

# --- Decodificacion de contenido ------------------------------------------------------------------------------------
function Get-TextsWithAnsiFallback {
    # Devuelve una LISTA de textos a revisar por separado (cada uno con sus propios numeros de linea): la decodificacion UTF-8
    # (permisiva) y, si hay bytes que no son UTF-8 valido (U+FFFD), tambien la de Latin-1 de los mismos bytes: el archivo puede
    # estar en Latin-1/ANSI (un bloc de notas antiguo, un `>` de Windows PowerShell 5.1 con otra pagina de codigos...) y un valor
    # con acentos no puede esconderse tras el U+FFFD. Los saltos de linea son los mismos en las dos: las lineas coinciden.
    param([byte[]]$Bytes, [int]$Offset)
    $count = $Bytes.Length - $Offset
    $text = [System.Text.Encoding]::UTF8.GetString($Bytes, $Offset, $count)
    if ($text.IndexOf([char]0xFFFD) -lt 0) { return , @($text) }
    return , @($text, [System.Text.Encoding]::GetEncoding(28591).GetString($Bytes, $Offset, $count))
}

function ConvertTo-ScanText {
    # Devuelve la LISTA de textos a revisar para unos bytes (cada uno se revisa por separado), o $null si hay que rechazar (se
    # indica en $reject). A los textos de siempre se ANADEN (nunca los sustituyen: UTF-16 sigue leyendose igual) los mismos
    # textos sin NUL: un SID o una ruta de perfil partidos por bytes NUL sueltos (`S-1<NUL>-5...`) no coinciden con nada en ninguna
    # decodificacion, pero al quitar los NUL vuelven a ser el valor. Los saltos de linea no cambian: las lineas coinciden.
    param([byte[]]$Bytes, [ref]$Reject)
    $texts = ConvertTo-ScanTextCore $Bytes $Reject
    if ($null -eq $texts) { return $null }
    $nul = [string][char]0
    $extra = New-Object System.Collections.Generic.List[string]
    foreach ($t in $texts) {
        if ($t.IndexOf([char]0) -ge 0) { $extra.Add($t.Replace($nul, '')) }
    }
    # Ademas, el UTF-8 de los BYTES sin NUL (como leak-scan.ps1): quitar el NUL de un texto ya decodificado no es lo mismo que
    # quitarlo de los bytes. Un caracter de varios bytes partido por un NUL suelto (`C3 00 A9`) se decodifica como dos U+FFFD
    # con un U+0000 en medio y asi nunca vuelve a ser la letra; sin el NUL antes de decodificar, si.
    if ([System.Array]::IndexOf($Bytes, [byte]0) -ge 0) {
        # Latin-1 es una biyeccion de bytes: quitar los NUL del texto es quitar los bytes NUL (operaciones en bloque, sin recorrer
        # byte a byte en PowerShell, que en 5.1 es lento con binarios grandes).
        $latin1 = [System.Text.Encoding]::GetEncoding(28591)
        $noNulBytes = $latin1.GetBytes($latin1.GetString($Bytes).Replace($nul, ''))
        $extra.Add([System.Text.Encoding]::UTF8.GetString($noNulBytes))
    }
    if ($extra.Count -eq 0) { return , @($texts) }
    return , @(@($texts) + $extra.ToArray())
}

function ConvertTo-ScanTextCore {
    # Los textos "de siempre" (sin la variante sin NUL, que anade ConvertTo-ScanText).
    param([byte[]]$Bytes, [ref]$Reject)
    $Reject.Value = $null
    $n = $Bytes.Length
    # El limite de tamano va PRIMERO, para todo: con una marca BOM delante (o sin NUL) un archivo enorme se decodificaba entero
    # varias veces antes de rechazarlo, y un limite que solo cubria una de las rutas se sorteaba con un BOM.
    if ($n -gt 8MB) { $Reject.Value = 'archivo de mas de 8 MiB (no se puede revisar)'; return $null }
    if ($n -eq 0) { return , @('') }
    # Una marca BOM NO decide por si sola como se lee el archivo: sus decodificaciones se ACUMULAN (el texto segun el BOM) y, ademas,
    # se sigue con las de siempre (el archivo entero como UTF-8, y las cadenas de un binario si hay NUL). Quien pone un BOM de
    # UTF-16 delante de un texto UTF-8, o al reves, no esconde nada. (Solo un UTF-8 con BOM y sin NUL se queda con su propia
    # lectura: ya es la del archivo entero.)
    $texts = New-Object System.Collections.Generic.List[string]
    $hasNul = [System.Array]::IndexOf($Bytes, [byte]0) -ge 0
    if ($n -ge 3 -and $Bytes[0] -eq 0xEF -and $Bytes[1] -eq 0xBB -and $Bytes[2] -eq 0xBF) {
        foreach ($t in (Get-TextsWithAnsiFallback $Bytes 3)) { $texts.Add($t) }
        if (-not $hasNul) { return , $texts.ToArray() }
    }
    # UTF-32 con BOM (el de little endian, FF FE 00 00, empieza como el de UTF-16: se comprueba ANTES).
    if ($n -ge 4 -and $Bytes[0] -eq 0xFF -and $Bytes[1] -eq 0xFE -and $Bytes[2] -eq 0 -and $Bytes[3] -eq 0) { $texts.Add((New-Object System.Text.UTF32Encoding($false, $false)).GetString($Bytes, 4, (($n - 4) -band -4))) }
    elseif ($n -ge 4 -and $Bytes[0] -eq 0 -and $Bytes[1] -eq 0 -and $Bytes[2] -eq 0xFE -and $Bytes[3] -eq 0xFF) { $texts.Add((New-Object System.Text.UTF32Encoding($true, $false)).GetString($Bytes, 4, (($n - 4) -band -4))) }
    elseif ($n -ge 2 -and $Bytes[0] -eq 0xFF -and $Bytes[1] -eq 0xFE) { $texts.Add([System.Text.Encoding]::Unicode.GetString($Bytes, 2, $n - 2)) }
    elseif ($n -ge 2 -and $Bytes[0] -eq 0xFE -and $Bytes[1] -eq 0xFF) { $texts.Add([System.Text.Encoding]::BigEndianUnicode.GetString($Bytes, 2, $n - 2)) }
    # UTF-16 sin BOM o binario: cualquier NUL en TODO el archivo (no solo en los primeros 4 KiB: un texto ASCII largo
    # delante no puede esconder lo que viene despues).
    if (-not $hasNul) {
        foreach ($t in (Get-TextsWithAnsiFallback $Bytes 0)) { $texts.Add($t) }
        return , $texts.ToArray()
    }
    # Con NUL y sin BOM: el archivo entero como UTF-8 (las partes de texto con acentos) MAS las cadenas de texto en Latin-1
    # y en UTF-16 (de ambas alineaciones) del resto, en un segundo texto. De cada decodificacion se sacan DOS clases de
    # cadena de 6 o mas caracteres, y se emiten las dos: (1) solo ASCII imprimible: es la de siempre y no la contamina el
    # vecino de la cadena (un par de bytes cualquiera leido como UTF-16 suele ser una letra CJK, que pegada a la cadena haria
    # fallar un `\b`); (2) ASCII mas letras y digitos de cualquier alfabeto (\p{L}\p{N}): con solo la (1), un valor local con
    # acentos (en Latin-1 o en UTF-16 sin BOM) partiria la cadena y no coincidiria con nada.
    # Cadenas de 2 o mas caracteres (antes 6): un valor local corto (3 letras) o un fragmento suelto no llegaba a formar cadena.
    $runClasses = @('[\x20-\x7E]{2,}', '[\x20-\x7E\p{L}\p{N}]{2,}')
    $sb = New-Object System.Text.StringBuilder
    $latin = [System.Text.Encoding]::GetEncoding(28591).GetString($Bytes)
    # Latin-1 es una biyeccion de bytes: quitar los NUL del texto es quitar los bytes NUL. Sus cadenas se emiten tambien (un
    # valor partido por NUL sueltos no forma ninguna cadena de 6 caracteres sin esto).
    $decodes = @($latin, $latin.Replace([string][char]0, ''))
    foreach ($off in 0, 1) { $decodes += [System.Text.Encoding]::Unicode.GetString($Bytes, $off, (($n - $off) -band -2)) }
    foreach ($d in $decodes) {
        foreach ($cls in $runClasses) {
            foreach ($m in [regex]::Matches($d, $cls)) { [void]$sb.AppendLine($m.Value) }
        }
    }
    $texts.Add([System.Text.Encoding]::UTF8.GetString($Bytes))
    $texts.Add($sb.ToString())
    return , $texts.ToArray()
}

function Test-Rules {
    # Devuelve la lista de (regla, linea) que coinciden en $Text (un texto, o una lista de textos que se revisan POR SEPARADO:
    # cada uno con sus numeros de linea). No devuelve la coincidencia (podria ser el dato).
    param([string[]]$Text, $RuleSet)
    $found = New-Object System.Collections.Generic.List[object]
    $seen = New-Object System.Collections.Generic.HashSet[string]
    # CultureInvariant: el (?i) no puede depender de la cultura del proceso (la i turca con y sin punto).
    $multi = [System.Text.RegularExpressions.RegexOptions]::Multiline -bor [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
    $single = [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
    foreach ($one in $Text) {
        $one = $one.Replace("`r", '')
        foreach ($rule in $RuleSet) {
            if ([regex]::IsMatch($one, $rule.Pattern, $multi)) {
                $lines = $one -split "`n"
                $any = $false
                for ($i = 0; $i -lt $lines.Count; $i++) {
                    if ([regex]::IsMatch($lines[$i], $rule.Pattern, $single)) {
                        if ($seen.Add("$($rule.Name)|$($i + 1)")) { $found.Add(@($rule.Name, ($i + 1))) }
                        $any = $true
                    }
                }
                if (-not $any -and $seen.Add("$($rule.Name)|0")) { $found.Add(@($rule.Name, 0)) }
            }
        }
    }
    return ,$found
}

$problems = New-Object System.Collections.Generic.List[string]

# --- Modo mensaje de commit -----------------------------------------------------------------------------------------
if ($MessageFile) {
    $reject = $null
    $text = ConvertTo-ScanText ([System.IO.File]::ReadAllBytes($MessageFile)) ([ref]$reject)
    # Se revisa el mensaje ENTERO, sin excepcion: tambien las lineas que empiezan por `#` y lo que sigue a la linea de
    # tijera (`# ---- >8 ----`). Git solo lo recorta con `--cleanup=scissors` (o con el editor y --verbose); con `-m`,
    # `-F`, `--cleanup=verbatim` o `--cleanup=whitespace` lo conserva, y el hook ve el archivo ANTES de la limpieza.
    # Si el diff de --verbose lleva datos privados, bloquear es lo correcto.
    foreach ($hit in (Test-Rules $text $rules)) { $problems.Add("mensaje de commit:$($hit[1]): $($hit[0])") }
    if ($problems.Count -gt 0) {
        Write-Host "privacy-check: commit message contains private data:" -ForegroundColor Red
        $problems | ForEach-Object { Write-Host "  $_" }
        exit 1
    }
    Write-Host 'privacy-check: OK (commit message)'
    exit 0
}

$pathFree = @($rules | Where-Object { -not $_.Path })

# --- Excepcion ESTRECHA de las reglas de rutas en spike/ ------------------------------------------------------------
# spike/ se revisa con TODAS las reglas, las de rutas incluidas. Dos archivos de la fase 0 llevan, desde su primer commit, rutas
# SINTETICAS en un test y en un comentario (una unidad `D:` inventada, una carpeta de usuario inventada y el ejemplo de lo que
# redacta redact.py): son datos de prueba, no de ninguna maquina, y estan en la historia (que no se reescribe). Para ellos y SOLO
# para ellos las reglas de rutas se saltan, y solo mientras el contenido sea EXACTAMENTE el revisado (SHA-256 de sus bytes):
# cambiar un solo byte los devuelve a las reglas completas. Lista cerrada: nada nuevo entra por aqui.
$spikePathAllow = @{
    'spike/scripts/redact.py'       = '65ab9bc8af78a0d82bb9b84bda44cf7c9e95a896f59bd552c38239fc7b298346'
    'spike/trash-bench/src/main.rs' = '801482ae217ac6a09cebfef0b7904e3dc792f59545983a9d58f205d7bcaa2f77'
}

function Get-Sha256Hex {
    param([byte[]]$Bytes)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try { return ([System.BitConverter]::ToString($sha.ComputeHash($Bytes))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose() }
}
$imageExt = '(?i)\.(png|jpe?g|gif|bmp|webp|tiff?|heic)$'

# --- Excepcion ESTRECHA para las capturas de --demo ------------------------------------------------------------------
# Las imagenes se rechazan en todo el repositorio, con la extension que tengan (se detectan por su FIRMA: un .png
# renombrado a .dat o .txt tambien falla), SALVO `docs/img/<nombre>.png` (minusculas, digitos y guiones) cuando:
#   1. es un PNG de verdad y de como mucho 2 MiB, con SOLO los fragmentos IHDR (13 bytes), IDAT, IEND y los de tamano
#      fijo sRGB (1), gAMA (4), cHRM (32) y pHYs (9); PLTE (<= 768, multiplo de 3) y tRNS (<= 256) solo en PNG indexados
#      (tipo de color 3). Nada de tEXt/iTXt/zTXt/eXIf/tIME/iCCP: ahi viajan software, fecha, camara o el perfil del
#      monitor. Las longitudes cerradas impiden esconder texto en claro en los fragmentos permitidos;
#   2. `docs/img/README.md` tiene una linea con EXACTAMENTE esta forma (anclada, nada de subcadenas):
#          - <nombre>.png | sha256: <64 hex> | <orden con --demo>
#      con el nombre del archivo y el SHA-256 de sus bytes: cambiar un solo byte de la imagen obliga a registrarla de
#      nuevo, y un nombre parecido (`o.png` frente a `mapa-claro.png`) no vale;
#   3. NO se escanea como texto: sus datos van comprimidos (IDAT), donde una cadena legible no se ve y, en cambio, una
#      secuencia casual de bytes imprimibles da falsos positivos (ocurrio con una captura real). Lo que protege es la
#      lista cerrada de fragmentos y longitudes del punto 1.
# Lo que el codigo no puede comprobar (que los pixeles sean solo de la app) lo revisa quien audita el cambio.
$demoImgPattern = '^docs/img/[a-z0-9-]+\.png$'
$demoImgMaxBytes = 2MB
$pngFixedChunks = @{ 'sRGB' = 1; 'gAMA' = 4; 'cHRM' = 32; 'pHYs' = 9 }

function Get-ImageMagic {
    # Devuelve el nombre del formato de imagen que indica la FIRMA de los bytes, o $null si no parece una imagen.
    param([byte[]]$Bytes)
    $n = $Bytes.Length
    if ($n -ge 8 -and $Bytes[0] -eq 0x89 -and $Bytes[1] -eq 0x50 -and $Bytes[2] -eq 0x4E -and $Bytes[3] -eq 0x47) { return 'PNG' }
    if ($n -ge 3 -and $Bytes[0] -eq 0xFF -and $Bytes[1] -eq 0xD8 -and $Bytes[2] -eq 0xFF) { return 'JPEG' }
    if ($n -ge 6) {
        $h6 = [System.Text.Encoding]::ASCII.GetString($Bytes, 0, 6)
        if ($h6 -ceq 'GIF87a' -or $h6 -ceq 'GIF89a') { return 'GIF' }
    }
    if ($n -ge 12) {
        $riff = [System.Text.Encoding]::ASCII.GetString($Bytes, 0, 4)
        $webp = [System.Text.Encoding]::ASCII.GetString($Bytes, 8, 4)
        if ($riff -ceq 'RIFF' -and $webp -ceq 'WEBP') { return 'WEBP' }
        $ftyp = [System.Text.Encoding]::ASCII.GetString($Bytes, 4, 4)
        if ($ftyp -ceq 'ftyp') {
            $brand = [System.Text.Encoding]::ASCII.GetString($Bytes, 8, 4)
            if (@('avif', 'avis', 'heic', 'heix', 'hevc', 'hevx', 'heim', 'heis', 'mif1', 'msf1') -ccontains $brand) { return 'AVIF/HEIF' }
        }
    }
    if ($n -ge 8) {
        $t = ([int]$Bytes[0] -shl 24) -bor ([int]$Bytes[1] -shl 16) -bor ([int]$Bytes[2] -shl 8) -bor [int]$Bytes[3]
        if ($t -eq 0x49492A00 -or $t -eq 0x4D4D002A) { return 'TIFF' }
    }
    if ($n -ge 6 -and $Bytes[0] -eq 0 -and $Bytes[1] -eq 0 -and ($Bytes[2] -eq 1 -or $Bytes[2] -eq 2) -and $Bytes[3] -eq 0) {
        $count = [int]$Bytes[4] + 256 * [int]$Bytes[5]
        if ($count -ge 1 -and $count -le 256 -and $n -ge (6 + 16 * $count)) { return 'ICO' }
    }
    if ($n -ge 18 -and $Bytes[0] -eq 0x42 -and $Bytes[1] -eq 0x4D) {
        # BMP: "BM" + tamano + reservado + desplazamiento + tamano de la cabecera DIB conocida (evita falsos positivos con
        # un texto que empiece por "BM").
        $dib = [int]$Bytes[14] + 256 * [int]$Bytes[15] + 65536 * [int]$Bytes[16] + 16777216 * [int]$Bytes[17]
        if (@(12, 40, 52, 56, 64, 108, 124) -contains $dib) { return 'BMP' }
    }
    return $null
}

function Test-DemoPng {
    # Devuelve $null si los bytes son un PNG admitido; si no, el motivo.
    param([byte[]]$Bytes)
    if ($Bytes.Length -gt $demoImgMaxBytes) { return 'imagen de mas de 2 MiB' }
    if ($Bytes.Length -lt 33) { return 'no es un PNG valido (demasiado corto)' }
    $sig = 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A
    for ($i = 0; $i -lt 8; $i++) { if ($Bytes[$i] -ne $sig[$i]) { return 'no es un PNG (firma incorrecta)' } }
    $pos = 8
    $first = $true
    $sawEnd = $false
    $sawIdat = $false
    $colorType = -1
    while ($pos -lt $Bytes.Length) {
        if ($sawEnd) { return 'data appears after the IEND chunk' }
        if ($pos + 12 -gt $Bytes.Length) { return 'PNG truncado' }
        $len = ([int]$Bytes[$pos] -shl 24) -bor ([int]$Bytes[$pos + 1] -shl 16) -bor ([int]$Bytes[$pos + 2] -shl 8) -bor [int]$Bytes[$pos + 3]
        if ($len -lt 0 -or ($pos + 12 + [long]$len) -gt $Bytes.Length) { return 'PNG con un fragmento fuera de rango' }
        $type = [System.Text.Encoding]::ASCII.GetString($Bytes, $pos + 4, 4)
        if ($first -and $type -cne 'IHDR') { return 'el primer fragmento del PNG no es IHDR' }
        if (-not $first -and $type -ceq 'IHDR') { return 'IHDR repetido' }
        if ($type -ceq 'IHDR') {
            if ($len -ne 13) { return "IHDR de $len bytes (deben ser 13)" }
            $colorType = [int]$Bytes[$pos + 8 + 9]
        } elseif ($type -ceq 'IDAT') {
            $sawIdat = $true
        } elseif ($type -ceq 'IEND') {
            if ($len -ne 0) { return 'IEND contains data' }
            $sawEnd = $true
        } elseif ($pngFixedChunks.ContainsKey($type)) {
            if ($len -ne $pngFixedChunks[$type]) { return "fragmento $type de $len bytes (deben ser $($pngFixedChunks[$type])): ahi cabria texto" }
        } elseif ($type -ceq 'PLTE') {
            if ($colorType -ne 3) { return 'PLTE en un PNG no indexado' }
            if ($len -gt 768 -or ($len % 3) -ne 0) { return "PLTE de $len bytes (maximo 768 y multiplo de 3)" }
        } elseif ($type -ceq 'tRNS') {
            if ($colorType -ne 3) { return 'tRNS en un PNG no indexado' }
            if ($len -gt 256) { return "tRNS de $len bytes (maximo 256)" }
        } else {
            return "disallowed PNG chunk ($type): it may contain machine metadata"
        }
        $first = $false
        $pos += 12 + $len
    }
    if (-not $sawEnd) { return 'PNG sin fragmento IEND' }
    if (-not $sawIdat) { return 'PNG has no image data (IDAT)' }
    return $null
}

# Origen de docs/img/README.md: `$null` = disco (-All) o indice (staging); un SHA = ese commit (modo -To).
$script:ImgReadmeRef = $null
# CAPTURAS HISTORICAS de --demo (solo modo -To): las dos que entraron en el commit dcebdb2 ANTES de que el README
# registrara el SHA-256 de cada imagen, y que e1d1903 sustituyo por otras (por eso ningun README posterior lista su hash).
# Se revisaron a ojo: son del modo demostracion (datos inventados y el rotulo "Modo demostracion"). Siguen pasando la
# comprobacion estructural (lista cerrada de fragmentos), el patron de ruta de docs/img/ y el limite de tamano. Lista
# CERRADA: ninguna captura nueva entra por aqui, se registra en el README.
$legacyDemoImages = @(
    'df18aa75fd3a2318db99a5324cc9b7824042226997f12802199b5173aaa18cbe',
    '2fb2810bad36cdfe13b36379c8de5c0a6f13eaac38278f58338ba51b78445aa3',
    # Capturas de --demo de versiones anteriores, que SI estaban registradas en el README de SU commit (e1d1903, c12434b y
    # 81f1417) y que el README de hoy ya no lista (se sustituyeron por las nuevas). Desde que en modo -To solo vale el README del
    # commit `To` (un commit intermedio no puede avalar su propia imagen), la historia ya publicada las necesita aqui. Lista
    # CERRADA: son los hashes de blobs que ya estan en la historia publica y que pasaron la comprobacion estructural; ninguna
    # captura nueva entra por aqui.
    'adf4969582fbb559236a8f910d9eb967e8460f655280a148c2860a45c8711938',
    'a0609c142a2b8391cada1795b200991e9f854b24a5c9b156aed3fd1d747f43b0',
    'df8dd2a6d419bbe661a161e0c8b0d9002fde9e84d8f0edcbfbddebf41f02ed3c',
    '53875e4cd8c00b57935cbf10a614a9a3f49430c663aefbeade01cf72ec10067c',
    'b3a431728b860853484cb70a339f5398fb11165cb3cb46d0193262e96e7f9c66',
    '1392d5ff921dba0138300cb4755f2edb8333502ba95dfe3e719f44483e43dac1',
    '64f7d14a990044197b08095f1eb6e8323be322bf178838c4cd0568e3454406bf',
    '08db178df8af19d6c64480e6f7e0752f3885e9f92b1b3ce9f688dfdb1c6e7cc0',
    '4114a563bbbf12aa10d8c57cf1138f0df723efa259feea22a0b40a33796ad067'
)
$script:RangeMode = $false
# Modo -To: el README que vale es el del commit `To`.
$script:ImgReadmeExtraRef = $null

function Get-ImgReadme {
    # Texto de docs/img/README.md (disco con -All; indice en modo staging; el commit en modo -To), o $null si no existe.
    if ($script:ImgReadmeRef) {
        try {
            return [System.Text.Encoding]::UTF8.GetString((Invoke-GitBytes ('cat-file blob "' + $script:ImgReadmeRef + ':docs/img/README.md"') $repoRoot))
        } catch {
            return $null
        }
    }
    if ($All) {
        $p = Join-Path $repoRoot 'docs/img/README.md'
        if (-not (Test-Path -LiteralPath $p -PathType Leaf)) { return $null }
        return [System.Text.Encoding]::UTF8.GetString([System.IO.File]::ReadAllBytes($p))
    }
    try {
        return [System.Text.Encoding]::UTF8.GetString((Invoke-GitBytes 'cat-file blob ":docs/img/README.md"' $repoRoot))
    } catch {
        return $null
    }
}

function Get-ImgReadmeAt {
    # Texto de docs/img/README.md en el commit `$Ref`, o $null.
    param([string]$Ref)
    try {
        return [System.Text.Encoding]::UTF8.GetString((Invoke-GitBytes ('cat-file blob "' + $Ref + ':docs/img/README.md"') $repoRoot))
    } catch {
        return $null
    }
}

function Test-ImgRegistered {
    # Hay en docs/img/README.md una linea anclada `- <leaf> | sha256: <hash> | ...--demo...` con el SHA-256 de $Bytes.
    # En modo -To se mira el README del commit de la imagen Y el del commit `To`.
    param([string]$Leaf, [byte[]]$Bytes)
    # En modo -To, SOLO vale el README del commit `To` (el registro vigente): el README del propio commit de la imagen no cuenta
    # (una imagen se puede haber registrado y desregistrado despues; y un commit intermedio no puede avalarse a si mismo). En
    # los demas modos, el del disco o el del indice.
    if ($script:RangeMode) { $readmes = @(Get-ImgReadmeAt $script:ImgReadmeExtraRef) } else { $readmes = @(Get-ImgReadme) }
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try { $hex = ([System.BitConverter]::ToString($sha.ComputeHash($Bytes))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose() }
    if ($script:RangeMode -and (($legacyDemoImages + $LegacyImageSha256) -contains $hex)) { return $true }
    $pattern = '^[-*]\s+' + [regex]::Escape($Leaf) + '\s*\|\s*sha256:\s*' + $hex + '\s*\|.*--demo'
    foreach ($readme in $readmes) {
        if (-not $readme) { continue }
        foreach ($line in ($readme -split "`n")) {
            if ($line.TrimEnd("`r") -match $pattern) { return $true }
        }
    }
    return $false
}

# --- Formatos OPACOS o comprimidos: se rechazan por la FIRMA, con cualquier extension ------------------------------
# Su contenido no se puede revisar (un SID dentro de un ZIP o de un gzip pasaria) y un repositorio de codigo no los
# necesita: las imagenes registradas de docs/img/ ya tienen su excepcion propia. Lista blanca: ninguna hoy. Si algun
# dia hiciera falta, tendria que ser ESTRECHA: ruta exacta y SHA-256 de los bytes (como las capturas de docs/img/).
$opaqueAllow = @()
$opaqueFormats = @(
    @{ Name = 'ZIP (y docx, xlsx, pptx, jar, apk, nupkg, whl...)'; Sig = @(0x50, 0x4B, 0x03, 0x04) },
    @{ Name = 'ZIP vacio'; Sig = @(0x50, 0x4B, 0x05, 0x06) },
    @{ Name = 'ZIP por tramos'; Sig = @(0x50, 0x4B, 0x07, 0x08) },
    @{ Name = 'gzip'; Sig = @(0x1F, 0x8B) },
    @{ Name = 'compress (.Z)'; Sig = @(0x1F, 0x9D) },
    @{ Name = '7z'; Sig = @(0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C) },
    @{ Name = 'rar'; Sig = @(0x52, 0x61, 0x72, 0x21, 0x1A, 0x07) },
    @{ Name = 'xz'; Sig = @(0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00) },
    @{ Name = 'zstd'; Sig = @(0x28, 0xB5, 0x2F, 0xFD) },
    @{ Name = 'lz4'; Sig = @(0x04, 0x22, 0x4D, 0x18) },
    @{ Name = 'lzip'; Sig = @(0x4C, 0x5A, 0x49, 0x50, 0x01) },
    @{ Name = 'cab'; Sig = @(0x4D, 0x53, 0x43, 0x46, 0x00, 0x00, 0x00, 0x00) },
    @{ Name = 'OLE (msi, doc, xls, ppt)'; Sig = @(0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1) },
    @{ Name = 'pdf'; Sig = @(0x25, 0x50, 0x44, 0x46, 0x2D) },
    @{ Name = 'sqlite'; Sig = @(0x53, 0x51, 0x4C, 0x69, 0x74, 0x65, 0x20, 0x66, 0x6F, 0x72, 0x6D, 0x61, 0x74, 0x20, 0x33, 0x00) }
)

function Test-Signature {
    param([byte[]]$Bytes, [int[]]$Sig)
    if ($Bytes.Length -lt $Sig.Length) { return $false }
    for ($i = 0; $i -lt $Sig.Length; $i++) { if ($Bytes[$i] -ne $Sig[$i]) { return $false } }
    return $true
}

function Get-OpaqueFormat {
    # Nombre del formato opaco que indica la FIRMA de los bytes, o $null.
    param([byte[]]$Bytes)
    foreach ($fmt in $opaqueFormats) {
        if (Test-Signature $Bytes $fmt.Sig) { return $fmt.Name }
    }
    # ZIP por el registro de FIN (`PK 05 06`, 22 bytes como minimo) en los ultimos 64 KiB: un ZIP con datos delante (un
    # autoextraible) o con la cabecera local alterada no empieza por `PK 03 04`.
    if ($Bytes.Length -ge 22) {
        $tail = [Math]::Min($Bytes.Length, 65536 + 22)
        $start = $Bytes.Length - $tail
        $chunk = [System.Text.Encoding]::GetEncoding(28591).GetString($Bytes, $start, $tail)
        $sig = ([char]0x50).ToString() + ([char]0x4B).ToString() + ([char]5).ToString() + ([char]6).ToString()
        $at = $chunk.LastIndexOf($sig, [System.StringComparison]::Ordinal)
        if ($at -ge 0 -and ($at + 22) -le $chunk.Length) { return 'ZIP (registro de fin)' }
    }
    # bzip2: "BZh", nivel 1-9 y el numero pi del primer bloque (evita un texto que empiece por "BZh").
    if ($Bytes.Length -ge 10 -and $Bytes[0] -eq 0x42 -and $Bytes[1] -eq 0x5A -and $Bytes[2] -eq 0x68 -and $Bytes[3] -ge 0x31 -and $Bytes[3] -le 0x39) {
        if (Test-Signature ([byte[]]($Bytes[4..9])) @(0x31, 0x41, 0x59, 0x26, 0x53, 0x59)) { return 'bzip2' }
    }
    return $null
}

# --- Revision de UN archivo (nombre y contenido); sirve para staging, -All y el modo de rango -------------------------
function Test-FileBytes {
    param([string]$F, [byte[]]$Bytes, [string]$Where)
    # Todas las reglas en todas partes; solo los dos archivos de $spikePathAllow, con el contenido revisado, saltan las de rutas.
    $ruleSet = $rules
    if ($spikePathAllow.ContainsKey($F) -and (Get-Sha256Hex $Bytes) -ceq $spikePathAllow[$F]) { $ruleSet = $pathFree }

    if ($F -eq 'docs/spike-results.md') {
        $problems.Add("${Where}${F}: forbidden file (spike results are regenerated separately)")
        return
    }
    # El nombre del archivo tambien se revisa.
    foreach ($hit in (Test-Rules $F $ruleSet)) { $problems.Add("${Where}${F}: file NAME: $($hit[0])") }

    # Imagenes: por extension Y por firma (un .png renombrado a .dat, .bin o .txt tambien es una captura).
    $magic = Get-ImageMagic $Bytes
    $isDemoImg = $false
    if ($F -match $imageExt -or $magic) {
        if ($F -cmatch $demoImgPattern) {
            $isDemoImg = $true
        } else {
            $kind = if ($magic) { $magic } else { 'por la extension' }
            $problems.Add("${Where}${F}: versioned image ($kind); screenshots are not allowed except docs/img/<name>.png generated with --demo")
            return
        }
    }
    if ($isDemoImg) {
        $why = Test-DemoPng $Bytes
        if ($why) { $problems.Add("${Where}${F}: $why"); return }
        $leaf = $F.Substring($F.LastIndexOf('/') + 1)
        if (-not (Test-ImgRegistered $leaf $Bytes)) {
            $problems.Add("${Where}${F}: missing its anchored entry in docs/img/README.md (- <name>.png | sha256: <hash of its bytes> | --demo command)")
        }
        return
    }
    # Formatos opacos o comprimidos: no se pueden revisar, se rechazan (sin lista blanca hoy).
    $opaque = Get-OpaqueFormat $Bytes
    if ($opaque -and ($opaqueAllow -notcontains $F)) {
        $problems.Add("${Where}${F}: opaque or compressed format ($opaque): its contents cannot be scanned; commit uncompressed text instead")
        return
    }
    $reject = $null
    $text = ConvertTo-ScanText $Bytes ([ref]$reject)
    if ($reject) { $problems.Add("${Where}${F}: $reject"); return }
    foreach ($hit in (Test-Rules $text $ruleSet)) { $problems.Add("${Where}${F}:$($hit[1]): $($hit[0])") }
}

# --- Modo de rango: mensajes y blobs de lo que introduce un push o un pull request ----------------------------------
if ($To) {
    if ($To -notmatch '^[0-9a-f]{40}$') {
        Write-Host 'privacy-check: -To must be a full 40-character hexadecimal SHA' -ForegroundColor Red
        exit 2
    }
    if ($From -and $From -notmatch '^[0-9a-f]{40}$') {
        Write-Host 'privacy-check: -From must be a full 40-character hexadecimal SHA' -ForegroundColor Red
        exit 2
    }
    $revSpec = $To
    $whole = 'toda la historia'
    if ($From -and $From -notmatch '^0{40}$') {
        $exists = $true
        try { [void](Invoke-GitBytes ('cat-file -e ' + $From + '^{commit}') $repoRoot) } catch { $exists = $false }
        if ($exists) { $revSpec = "$From..$To"; $whole = "$From..$To" }
    }
    $commits = @((Split-Nul (Invoke-GitBytes "rev-list -z --reverse $revSpec" $repoRoot)) | ForEach-Object { $_.Trim() } | Where-Object { $_ -ne '' })
    $seen = @{}
    $blobCount = 0
    $script:ImgReadmeExtraRef = $To
    $script:RangeMode = $true
    foreach ($c in $commits) {
        $short = $c.Substring(0, 10)
        $script:ImgReadmeRef = $c
        # Autor y committer (nombre y correo de cada uno), cada campo por separado y con su etiqueta, y el mensaje completo (cabecera
        # y cuerpo; sin excepcion de tijera ni de lineas con `#`) con sus propios numeros de linea.
        $people = [System.Text.Encoding]::UTF8.GetString((Invoke-GitBytes "log -1 --format=%an%x00%ae%x00%cn%x00%ce $c" $repoRoot)).TrimEnd("`n").Split([char]0)
        $personLabels = @('autor (nombre)', 'autor (correo)', 'committer (nombre)', 'committer (correo)')
        for ($k = 0; $k -lt $personLabels.Count; $k++) {
            $field = if ($k -lt $people.Count) { $people[$k] } else { '' }
            $rejectP = $null
            $personText = ConvertTo-ScanText ([System.Text.Encoding]::UTF8.GetBytes($field)) ([ref]$rejectP)
            foreach ($hit in (Test-Rules $personText $rules)) { $problems.Add("commit ${short}: $($personLabels[$k]): $($hit[0])") }
        }
        $reject = $null
        $msgText = ConvertTo-ScanText (Invoke-GitBytes "log -1 --format=%B $c" $repoRoot) ([ref]$reject)
        foreach ($hit in (Test-Rules $msgText $rules)) { $problems.Add("commit ${short}: mensaje:$($hit[1]): $($hit[0])") }
        # Archivos que introduce (altas, cambios, copias y renombrados; con -m tambien los de los merges).
        $raw = Split-Nul (Invoke-GitBytes "diff-tree -r -m --root --no-commit-id --no-renames --diff-filter=ACMRT --raw -z $c" $repoRoot)
        for ($i = 0; $i -lt $raw.Count; $i++) {
            if (-not $raw[$i].StartsWith(':')) { continue }
            $fields = $raw[$i].Split(' ')
            $i++
            if ($i -ge $raw.Count) { break }
            $f = $raw[$i].Replace('\', '/')
            $mode = $fields[1]
            $oid = $fields[3]
            if ($mode -eq '160000' -or $oid -match '^0+$') { continue }
            $key = "$oid|$f"
            if ($seen.ContainsKey($key)) { continue }
            $seen[$key] = $true
            $blobCount++
            $bytes = Invoke-GitBytes "cat-file blob $oid" $repoRoot
            Test-FileBytes $f $bytes "commit ${short}: "
        }
    }
    if ($problems.Count -gt 0) {
        Write-Host "privacy-check: FAILED ($($problems.Count) matches in $whole). Remove this data (including from history) before publishing:" -ForegroundColor Red
        $problems | Select-Object -First 50 | ForEach-Object { Write-Host "  $_" }
        if ($problems.Count -gt 50) { Write-Host "  ... and $($problems.Count - 50) more" }
        exit 1
    }
    $scope = if ($GenericOnly) { 'generic patterns' } else { 'generic patterns + local values' }
    Write-Host "privacy-check: OK ($($commits.Count) commits and $blobCount blobs in $whole, $scope)"
    exit 0
}

# --- Archivos a revisar ---------------------------------------------------------------------------------------------
if ($All) {
    $names = Split-Nul (Invoke-GitBytes '-c core.quotePath=false ls-files --cached --others --exclude-standard -z' $repoRoot)
} else {
    $names = Split-Nul (Invoke-GitBytes '-c core.quotePath=false diff --cached --name-only -z --diff-filter=ACMRT' $repoRoot)
}
$names = @($names | Sort-Object -Unique)

foreach ($name in $names) {
    $f = $name.Replace('\', '/')
    $full = Join-Path $repoRoot $name
    if ($All) {
        if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { continue }
        $bytes = [System.IO.File]::ReadAllBytes($full)
    } else {
        # Contenido del INDICE, leido como bytes (sin recodificar).
        $bytes = Invoke-GitBytes "cat-file blob `":$f`"" $repoRoot
    }
    Test-FileBytes $f $bytes ''
}

if ($problems.Count -gt 0) {
    Write-Host "privacy-check: FAILED ($($problems.Count) matches). Remove this data before committing:" -ForegroundColor Red
    $problems | Select-Object -First 50 | ForEach-Object { Write-Host "  $_" }
    if ($problems.Count -gt 50) { Write-Host "  ... and $($problems.Count - 50) more" }
    exit 1
}

$mode = if ($All) { 'entire tree' } else { 'staged files' }
$scope = if ($GenericOnly) { 'generic patterns' } else { 'generic patterns + local values' }
Write-Host "privacy-check: OK ($($names.Count) files, $mode, $scope)"
exit 0
