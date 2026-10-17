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
    # Run Git and return stdout as bytes WITHOUT passing through the PowerShell pipeline (which re-encodes it).
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

# In CI (CI variable), use generic patterns only unless -Local is explicitly requested. Hooks pass -Local so a CI
# variable in a developer profile cannot silently disable local value checks.
if ($env:CI -and -not $Local) { $GenericOnly = $true }
if ($Local -and $GenericOnly) {
    Write-Host 'privacy-check: -Local and -GenericOnly cannot be used together' -ForegroundColor Red
    exit 2
}

# --- Rules -----------------------------------------------------------------------------------------------------------
# Path = $true: path-based rule. Applies to the ENTIRE repository, including spike/; the only exception is the CLOSED
# `$spikePathAllow` list below. Brackets prevent the script from matching itself.
$generic = @(
    @{ Name = 'Windows SID'; Path = $false; Pattern = '(?i)S-1-5-21-\d' },
    @{ Name = 'AzureAD account SID'; Path = $false; Pattern = '(?i)S-1-12-1-\d' },
    @{ Name = 'Real profile path (use <user>)'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_])[A-Z]:[\\/]{1,2}Users[\\/]{1,2}(?!<user>|<usuario>|%|\{|\$|Public\b|Default\b|\*)[^\\/\s''"`]+' },
    @{ Name = 'Real Unix or WSL profile path'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_.])/(mnt/)?[a-z]/Users/(?!<user>|<usuario>|Public\b|Default\b)[^/\s''"`]+' },
    @{ Name = 'Absolute drive path D: through Z:'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_])[D-Z]:[\\/]' },
    @{ Name = 'WSL drive path D: through Z:'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_])/mnt/[d-z](/|$)' },
    # UNC: separators may be mixed (`\` and `/`). All three rules require an ANCHORED form: two consecutive slashes not
    # preceded by another slash, `:` (a URL such as `https://...`), `.`, or a letter (`a//b`); the server must start with
    # a letter, digit, or `_`. Exclude `?`, `.`, and `<` (`\\?\C:`, `\\.\pipe`, and the `<server>` placeholder).
    @{ Name = 'UNC network path'; Path = $false; Pattern = '(?<![A-Za-z0-9_\\.:/])[\\/]{2}(?![?.<])[A-Za-z0-9_][A-Za-z0-9_.-]*[\\/][A-Za-z0-9_$]' },
    # Escaped form (a Rust or JSON literal, etc.): each backslash is doubled; forward slashes are not. Require at least
    # ONE doubled backslash (the rule above already covers forward slashes alone); two leading forward slashes followed
    # by a doubled backslash after the server also count.
    @{ Name = 'Escaped UNC network path'; Path = $false; Pattern = '(?<![A-Za-z0-9_\\.:/])(?:(?:\\\\\\\\|\\\\/|/\\\\|//)(?![?.<])[A-Za-z0-9_][A-Za-z0-9_.-]*\\\\|(?:\\\\\\\\|\\\\/|/\\\\)(?![?.<])[A-Za-z0-9_][A-Za-z0-9_.-]*(?:\\\\|[\\/]))[A-Za-z0-9_$]' },
    # Both `\\?\UNC\` (verbatim) and `\\.\UNC\` (device namespace) forms.
    @{ Name = 'Verbatim UNC path'; Path = $false; Pattern = '(?i)(?:\\\\|[\\/]){2}[?.](?:\\\\|[\\/])UNC(?:\\\\|[\\/])[A-Za-z0-9_]' },
    @{ Name = 'Fabricante o modelo de GPU/CPU/disco'; Path = $false; Pattern = '(?i)\b(N[V]IDIA|Ge[F]orce|R[T]X\s?\d{3,4}\w*|G[T]X\s?\d{3,4}\w*|Rade[o]n|Ry[z]en|Threadrip[p]er|EP[Y]C|X[e]on|Inte[l]\s+Core|Inte[l]\(R\)\s*Core|Core\s+i[3579][ -]|Apple\s+M[1-9]|Sam[s]ung|WD_BL[A]CK|WD\s+Bl[a]ck|Western\s+Dig[i]tal|Sea[g]ate|Kings[t]on|Cruc[i]al|Cors[a]ir|SN8\d\d\w*|H[y]nix|Mic[r]on|Tos[h]iba|SanD[i]sk)\b' },
    @{ Name = 'Antivirus'; Path = $false; Pattern = '(?i)\b(Bitdef[e]nder|Kaspers[k]y|Nor[t]on|McA[f]ee|Av[a]st|AVG\s+Anti\w*|ES[E]T|Malwareby[t]es|Soph[o]s|CrowdStr[i]ke|Sentinel[O]ne|Trend\s*Mi[c]ro|Symant[e]c|F-Se[c]ure|Webr[o]ot|Cylan[c]e|Carbon\s*Bl[a]ck)\b' },
    @{ Name = 'Windows build'; Path = $false; Pattern = '(?i)\bbuild\s+(19|2\d)\d{3}\b' },
    @{ Name = 'Windows version with build'; Path = $false; Pattern = '\b10\.0\.[12]\d{4}\b' },
    @{ Name = 'Driver version'; Path = $false; Pattern = '(?i)\bdriver\s+(version\s+)?v?\d+(\.\d+)+' }
)

# --- Local values and denylist ---------------------------------------------------------------------------------------
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
                # Word boundaries: ASCII letters/digits (underscore is NOT a boundary: `user_x` and `x_user` count) plus
                # accented Latin letters (U+00C0-U+024F, excluding U+00D7 and U+00F7), NOT \p{L}\p{N}. In UTF-16 binary
                # strings, a neighboring byte pair is often a Unicode letter (CJK, etc.); using \p{L} would miss a value
                # adjacent to that neighbor. A value such as "Ren" does not match inside an accented Latin word. Accepted
                # safe-side tradeoff: a value adjacent to Cyrillic, Greek, or CJK letters DOES count as a match.
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

# --- Content decoding -----------------------------------------------------------------------------------------------
function Get-TextsWithAnsiFallback {
    # Return a LIST of text views to scan separately (each with its own line numbers): lenient UTF-8 and, if any bytes are
    # invalid UTF-8 (U+FFFD), Latin-1 decoded from the same bytes too. The file may use Latin-1/ANSI (an old text editor,
    # `>` from Windows PowerShell 5.1 with another code page, etc.); accented values must not hide behind U+FFFD. Both
    # views preserve line breaks, so line numbers align.
    param([byte[]]$Bytes, [int]$Offset)
    $count = $Bytes.Length - $Offset
    $text = [System.Text.Encoding]::UTF8.GetString($Bytes, $Offset, $count)
    if ($text.IndexOf([char]0xFFFD) -lt 0) { return , @($text) }
    return , @($text, [System.Text.Encoding]::GetEncoding(28591).GetString($Bytes, $Offset, $count))
}

function ConvertTo-ScanText {
    # Return a LIST of text views to scan separately, or $null when the bytes must be rejected (reason in $reject). ADD
    # NUL-free text views to the existing ones (never replace them: UTF-16 must still be read as-is). A SID or profile
    # path split by stray NUL bytes (`S-1<NUL>-5...`) matches no decoding, but removing NULs restores the value. Line
    # breaks are unchanged, so line numbers still align.
    param([byte[]]$Bytes, [ref]$Reject)
    $texts = ConvertTo-ScanTextCore $Bytes $Reject
    if ($null -eq $texts) { return $null }
    $nul = [string][char]0
    $extra = New-Object System.Collections.Generic.List[string]
    foreach ($t in $texts) {
        if ($t.IndexOf([char]0) -ge 0) { $extra.Add($t.Replace($nul, '')) }
    }
    # Also scan UTF-8 decoded from bytes WITHOUT NULs (as in leak-scan.ps1): removing a NUL from decoded text differs from
    # removing it from the bytes. A multibyte character split by a stray NUL (`C3 00 A9`) decodes as two U+FFFD characters
    # with U+0000 between them; it can be restored only by removing the NUL before decoding.
    if ([System.Array]::IndexOf($Bytes, [byte]0) -ge 0) {
        # Latin-1 maps bytes one-to-one: removing NULs from its text removes NUL bytes (a bulk operation, avoiding a
        # byte-by-byte PowerShell loop, which is slow on large binaries in 5.1).
        $latin1 = [System.Text.Encoding]::GetEncoding(28591)
        $noNulBytes = $latin1.GetBytes($latin1.GetString($Bytes).Replace($nul, ''))
        $extra.Add([System.Text.Encoding]::UTF8.GetString($noNulBytes))
    }
    if ($extra.Count -eq 0) { return , @($texts) }
    return , @(@($texts) + $extra.ToArray())
}

function ConvertTo-ScanTextCore {
    # The original text views (without the NUL-free variant added by ConvertTo-ScanText).
    param([byte[]]$Bytes, [ref]$Reject)
    $Reject.Value = $null
    $n = $Bytes.Length
    # Apply the size limit FIRST for every input: a large file with a BOM (or no NULs) used to be decoded several times
    # before rejection, and a limit on only one decoding path could be bypassed with a BOM.
    if ($n -gt 8MB) { $Reject.Value = 'archivo de mas de 8 MiB (no se puede revisar)'; return $null }
    if ($n -eq 0) { return , @('') }
    # A BOM alone does NOT determine how the file is read: add its decoding to the original views (the whole file as UTF-8,
    # and binary strings if NULs exist). A UTF-16 BOM before UTF-8 text, or vice versa, hides nothing. (UTF-8 with a BOM
    # and no NULs uses only its own decoding, which already covers the whole file.)
    $texts = New-Object System.Collections.Generic.List[string]
    $hasNul = [System.Array]::IndexOf($Bytes, [byte]0) -ge 0
    if ($n -ge 3 -and $Bytes[0] -eq 0xEF -and $Bytes[1] -eq 0xBB -and $Bytes[2] -eq 0xBF) {
        foreach ($t in (Get-TextsWithAnsiFallback $Bytes 3)) { $texts.Add($t) }
        if (-not $hasNul) { return , $texts.ToArray() }
    }
    # UTF-32 with BOM (little-endian FF FE 00 00 starts like UTF-16, so check it FIRST).
    if ($n -ge 4 -and $Bytes[0] -eq 0xFF -and $Bytes[1] -eq 0xFE -and $Bytes[2] -eq 0 -and $Bytes[3] -eq 0) { $texts.Add((New-Object System.Text.UTF32Encoding($false, $false)).GetString($Bytes, 4, (($n - 4) -band -4))) }
    elseif ($n -ge 4 -and $Bytes[0] -eq 0 -and $Bytes[1] -eq 0 -and $Bytes[2] -eq 0xFE -and $Bytes[3] -eq 0xFF) { $texts.Add((New-Object System.Text.UTF32Encoding($true, $false)).GetString($Bytes, 4, (($n - 4) -band -4))) }
    elseif ($n -ge 2 -and $Bytes[0] -eq 0xFF -and $Bytes[1] -eq 0xFE) { $texts.Add([System.Text.Encoding]::Unicode.GetString($Bytes, 2, $n - 2)) }
    elseif ($n -ge 2 -and $Bytes[0] -eq 0xFE -and $Bytes[1] -eq 0xFF) { $texts.Add([System.Text.Encoding]::BigEndianUnicode.GetString($Bytes, 2, $n - 2)) }
    # UTF-16 without BOM or binary: detect any NUL in the ENTIRE file (not just the first 4 KiB; long leading ASCII text
    # must not hide content that follows).
    if (-not $hasNul) {
        foreach ($t in (Get-TextsWithAnsiFallback $Bytes 0)) { $texts.Add($t) }
        return , $texts.ToArray()
    }
    # With NULs and no BOM: scan the whole file as UTF-8 (including accented text) PLUS Latin-1 and UTF-16 strings (both
    # alignments) from the remaining bytes. Emit two classes of strings, each at least 6 characters, from every decoding:
    # (1) printable ASCII only, which avoids contamination from a neighboring UTF-16 character (an arbitrary byte pair
    # often reads as CJK and would break `\b`); (2) ASCII plus letters and digits from any alphabet (\p{L}\p{N}), so
    # accented local values in Latin-1 or UTF-16 without BOM are not split apart. Also emit strings of 2+ characters
    # (previously 6) to detect short local values and fragments.
    $runClasses = @('[\x20-\x7E]{2,}', '[\x20-\x7E\p{L}\p{N}]{2,}')
    $sb = New-Object System.Text.StringBuilder
    $latin = [System.Text.Encoding]::GetEncoding(28591).GetString($Bytes)
    # Latin-1 maps bytes one-to-one, so removing NULs from its text removes NUL bytes. Emit its strings too (a value split
    # by stray NULs may not otherwise form a six-character string).
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
    # Return matching (rule, line) pairs for $Text (one string or a list of text views checked SEPARATELY, each with its
    # own line numbers). Do not return the matched text; it may contain private data.
    param([string[]]$Text, $RuleSet)
    $found = New-Object System.Collections.Generic.List[object]
    $seen = New-Object System.Collections.Generic.HashSet[string]
    # CultureInvariant: (?i) must not depend on process culture (Turkish dotted and dotless i).
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

# --- Commit message mode ---------------------------------------------------------------------------------------------
if ($MessageFile) {
    $reject = $null
    $text = ConvertTo-ScanText ([System.IO.File]::ReadAllBytes($MessageFile)) ([ref]$reject)
    # Check the ENTIRE message with no exceptions, including lines starting with `#` and content after the scissors line
    # (`# ---- >8 ----`). Git trims it only with `--cleanup=scissors` (or editor plus --verbose); with `-m`, `-F`,
    # `--cleanup=verbatim`, or `--cleanup=whitespace`, it is retained, and the hook sees the file BEFORE cleanup.
    # If the --verbose diff contains private data, blocking the commit is correct.
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

# --- NARROW exception to path rules under spike/ ----------------------------------------------------------------------
# Apply ALL rules under spike/, including path rules. Two phase-0 files have contained SYNTHETIC paths in a test and a
# comment since their first commit (an invented `D:` drive, fake user directory, and redact.py example): these are test
# data, not machine data, and are in published history (which is not rewritten). Skip path rules for those files ONLY
# while their contents EXACTLY match the reviewed versions (SHA-256 of their bytes); changing one byte restores all path
# checks. This is a closed list; no new files may be added here.
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

# --- NARROW exception for --demo screenshots --------------------------------------------------------------------------
# Reject images throughout the repository regardless of extension (detect by SIGNATURE: a .png renamed to .dat or .txt
# still fails), EXCEPT `docs/img/<name>.png` (lowercase letters, digits, and hyphens) when:
#   1. It is a real PNG no larger than 2 MiB and contains ONLY IHDR (13 bytes), IDAT, IEND, and fixed-length sRGB (1),
#      gAMA (4), cHRM (32), and pHYs (9) chunks; PLTE (<= 768, multiple of 3) and tRNS (<= 256) are allowed only for
#      indexed PNGs (color type 3). Disallow tEXt/iTXt/zTXt/eXIf/tIME/iCCP, which may expose software, date, camera, or
#      monitor profile. Fixed lengths prevent hiding plain text in allowed chunks.
#   2. `docs/img/README.md` contains a line in EXACTLY this anchored form (no substring matches):
#          - <name>.png | sha256: <64 hex> | <command with --demo>
#      with the filename and SHA-256 of its bytes. Changing one image byte requires a new entry; a similar name (`o.png`
#      versus `mapa-claro.png`) does not count.
#   3. Do NOT scan it as text: IDAT data is compressed, so readable strings are hidden and random printable bytes can
#      cause false positives (this happened with a real screenshot). Protection comes from the closed chunk list and
#      lengths in item 1.
# Pixel content (whether it shows only the app) must be reviewed by the person auditing the change.
$demoImgPattern = '^docs/img/[a-z0-9-]+\.png$'
$demoImgMaxBytes = 2MB
$pngFixedChunks = @{ 'sRGB' = 1; 'gAMA' = 4; 'cHRM' = 32; 'pHYs' = 9 }

function Get-ImageMagic {
    # Return the image format indicated by the byte SIGNATURE, or $null if it does not look like an image.
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
        # BMP: "BM" + size + reserved + offset + known DIB header size (avoid false positives for text starting with "BM").
        $dib = [int]$Bytes[14] + 256 * [int]$Bytes[15] + 65536 * [int]$Bytes[16] + 16777216 * [int]$Bytes[17]
        if (@(12, 40, 52, 56, 64, 108, 124) -contains $dib) { return 'BMP' }
    }
    return $null
}

function Test-DemoPng {
    # Return $null if the bytes form an allowed PNG; otherwise return the reason.
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

# Source of docs/img/README.md: `$null` = working tree (-All) or index (staging); a SHA = that commit (-To mode).
$script:ImgReadmeRef = $null
# HISTORICAL --demo SCREENSHOTS (only in -To mode): two images added in commit dcebdb2 BEFORE the README recorded each
# image's SHA-256, then replaced by e1d1903 (so no later README lists their hashes). Manually reviewed as demo-mode images
# (synthetic data and the "Demo mode" label). They still pass structural checks (closed chunk list), docs/img/ path
# pattern, and size limit. CLOSED list: new screenshots must be registered in the README instead.
$legacyDemoImages = @(
    'df18aa75fd3a2318db99a5324cc9b7824042226997f12802199b5173aaa18cbe',
    '2fb2810bad36cdfe13b36379c8de5c0a6f13eaac38278f58338ba51b78445aa3',
    # Older --demo screenshots that WERE registered in their own commit README (e1d1903, c12434b, and 81f1417) but are no
    # longer listed today (they were replaced). Since -To mode accepts only the `To` commit's README (an intermediate
    # commit cannot vouch for its own image), these published historical images need this list. CLOSED list: hashes of
    # existing public blobs that passed structural checks; no new screenshots may be added here.
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
# -To mode: the README from commit `To` is authoritative.
$script:ImgReadmeExtraRef = $null

function Get-ImgReadme {
    # Return docs/img/README.md text from the working tree (-All), index (staging), or commit (-To), or $null if absent.
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
    # Return docs/img/README.md text from commit `$Ref`, or $null.
    param([string]$Ref)
    try {
        return [System.Text.Encoding]::UTF8.GetString((Invoke-GitBytes ('cat-file blob "' + $Ref + ':docs/img/README.md"') $repoRoot))
    } catch {
        return $null
    }
}

function Test-ImgRegistered {
    # Check for an anchored docs/img/README.md entry `- <leaf> | sha256: <hash> | ...--demo...` matching the SHA-256 of $Bytes.
    # In -To mode, inspect the image commit's README AND the `To` commit's README.
    param([string]$Leaf, [byte[]]$Bytes)
    # In -To mode, ONLY the `To` commit's README counts (the current registration). The image commit's own README does not
    # count because an image may have been registered and later unregistered, and an intermediate commit cannot vouch for
    # itself. In other modes, use the working tree or index.
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

# --- Opaque or compressed formats: reject by SIGNATURE, regardless of extension -------------------------------------
# Their contents cannot be inspected (an SID inside a ZIP or gzip would pass), and a code repository does not need them:
# registered docs/img/ images already have their own exception. There is no allowlist today. If one is ever needed, it
# must be NARROW: exact path and SHA-256 of the bytes (like docs/img/ screenshots).
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
    # Return the opaque format indicated by the byte SIGNATURE, or $null.
    param([byte[]]$Bytes)
    foreach ($fmt in $opaqueFormats) {
        if (Test-Signature $Bytes $fmt.Sig) { return $fmt.Name }
    }
    # Detect ZIP by its END record (`PK 05 06`, at least 22 bytes) in the last 64 KiB: a ZIP with prepended data (a
    # self-extracting archive) or a changed local header may not start with `PK 03 04`.
    if ($Bytes.Length -ge 22) {
        $tail = [Math]::Min($Bytes.Length, 65536 + 22)
        $start = $Bytes.Length - $tail
        $chunk = [System.Text.Encoding]::GetEncoding(28591).GetString($Bytes, $start, $tail)
        $sig = ([char]0x50).ToString() + ([char]0x4B).ToString() + ([char]5).ToString() + ([char]6).ToString()
        $at = $chunk.LastIndexOf($sig, [System.StringComparison]::Ordinal)
        if ($at -ge 0 -and ($at + 22) -le $chunk.Length) { return 'ZIP (registro de fin)' }
    }
    # bzip2: "BZh", level 1-9, and the first block's pi marker (avoid matching text that merely starts with "BZh").
    if ($Bytes.Length -ge 10 -and $Bytes[0] -eq 0x42 -and $Bytes[1] -eq 0x5A -and $Bytes[2] -eq 0x68 -and $Bytes[3] -ge 0x31 -and $Bytes[3] -le 0x39) {
        if (Test-Signature ([byte[]]($Bytes[4..9])) @(0x31, 0x41, 0x59, 0x26, 0x53, 0x59)) { return 'bzip2' }
    }
    return $null
}

# --- Check ONE file (name and contents); used for staging, -All, and range mode ---------------------------------------
function Test-FileBytes {
    param([string]$F, [byte[]]$Bytes, [string]$Where)
    # Apply every rule everywhere; only the two reviewed files in $spikePathAllow skip path rules.
    $ruleSet = $rules
    if ($spikePathAllow.ContainsKey($F) -and (Get-Sha256Hex $Bytes) -ceq $spikePathAllow[$F]) { $ruleSet = $pathFree }

    if ($F -eq 'docs/spike-results.md') {
        $problems.Add("${Where}${F}: forbidden file (spike results are regenerated separately)")
        return
    }
    # File names are checked too.
    foreach ($hit in (Test-Rules $F $ruleSet)) { $problems.Add("${Where}${F}: file NAME: $($hit[0])") }

    # Images are detected by extension AND signature (a .png renamed to .dat, .bin, or .txt is still a screenshot).
    $magic = Get-ImageMagic $Bytes
    $isDemoImg = $false
    if ($F -match $imageExt -or $magic) {
        if ($F -cmatch $demoImgPattern) {
            $isDemoImg = $true
        } else {
            $kind = if ($magic) { $magic } else { 'by extension' }
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
    # Opaque or compressed formats cannot be inspected, so reject them (there is no allowlist today).
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

# --- Range mode: messages and blobs introduced by a push or pull request ----------------------------------------------
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
        # Inspect author and committer (name and email) as separate labeled fields, plus the full message (header and body,
        # including scissors and `#` lines) using its own line numbers.
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
        # Files introduced (added, changed, copied, or renamed; with -m, include merge changes too).
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

# --- Files to inspect -----------------------------------------------------------------------------------------------
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
        # Read the INDEX contents as bytes (without re-encoding).
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
