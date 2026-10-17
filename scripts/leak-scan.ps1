<#
.SYNOPSIS
  Functions that search binary bytes for machine-local values (user, computer, profile, and paths).

.DESCRIPTION
  Used by scripts/build-release.ps1 (dot-sourced with `. ./scripts/leak-scan.ps1`) and tested by
  scripts/tests/leak-scan.tests.ps1. Values are read from the environment AT RUNTIME and are never written. Each value
  is searched in three path forms (as-is, with `/`, and with `\`); bytes are decoded as Latin-1 (ASCII and Windows ANSI),
  UTF-8 (used by the Rust compiler in embedded paths; accented values match only in this form), and UTF-16 (both
  alignments), case-insensitively. If NUL bytes are present, the Latin-1 and UTF-8 views of the bytes WITHOUT NULs are
  also searched (to find values split by stray NULs). Documented limit: base64-encoded or otherwise transformed text
  cannot be detected. This script is ASCII-only by design (Windows PowerShell 5.1 reads .ps1 files without a BOM as ANSI).
#>

function Get-LocalValueNeedles {
    # Local values to search: key = description (shown in the report), value = search text.
    param([string]$Repo, [string]$CargoHome, [string]$Sysroot, [string]$RustupHome)
    return @{
        'USERNAME'     = $env:USERNAME
        'USERPROFILE'  = $env:USERPROFILE
        'COMPUTERNAME' = $env:COMPUTERNAME
        'repository'   = $Repo
        'CARGO_HOME'   = $CargoHome
        'toolchain'    = $Sysroot
        'RUSTUP_HOME'  = $RustupHome
    }
}

function Find-LocalValueLeaks {
    # Return messages in the form "<name>: contains local value '<key>'" (empty when there are no leaks).
    param([byte[]]$Bytes, [hashtable]$Needles, [string]$Name)
    $leaks = New-Object System.Collections.Generic.List[string]
    if ($Bytes.Length -lt 2) { return , $leaks }
    $texts = @(
        [System.Text.Encoding]::GetEncoding(28591).GetString($Bytes),
        # Lenient UTF-8: each invalid sequence becomes U+FFFD (not a letter in the boundary class below). This only
        # affects this decoding, which is native to UTF-8 text; the other decodings have no such ambiguity.
        (New-Object System.Text.UTF8Encoding($false, $false)).GetString($Bytes),
        [System.Text.Encoding]::Unicode.GetString($Bytes, 0, ($Bytes.Length -band -2)),
        [System.Text.Encoding]::Unicode.GetString($Bytes, 1, (($Bytes.Length - 1) -band -2))
    )
    # A value split by stray NUL bytes (`Zo\0rbaq`) matches none of the decodings above. Also add (never replace them:
    # UTF-16 is still read with its NULs) Latin-1 and UTF-8 views of the bytes WITHOUT NULs. Latin-1 maps bytes one to
    # one, so removing NULs from the text removes NUL bytes; those bytes are then decoded as UTF-8.
    $noNul = $texts[0].Replace([string][char]0, '')
    $utf8NoNul = $null
    if ($noNul.Length -lt $texts[0].Length) {
        $texts += $noNul
        $utf8NoNul = (New-Object System.Text.UTF8Encoding($false, $false)).GetString([System.Text.Encoding]::GetEncoding(28591).GetBytes($noNul))
    }
    foreach ($k in $Needles.Keys) {
        $v = $Needles[$k]
        if (-not $v -or $v.Length -lt 3) { continue }
        # Search both separators: `git rev-parse --show-toplevel` returns `/`, while the compiler may write `\`.
        $forms = @($v, $v.Replace('\', '/'), $v.Replace('/', '\')) | Select-Object -Unique
        foreach ($form in $forms) {
            # User and computer names are short standalone strings: without word boundaries, they could match part of a
            # license author name (e.g. "Alexander" for a user named "Alex"). Count them as leaks only when they are not
            # adjacent to other letters or digits (as in a path, email address, or directory name).
            # The "letter or digit" class is ASCII plus accented Latin letters (U+00C0-U+024F, excluding multiplication
            # and division), NOT \p{L}\p{N}. The same boundaries apply to UTF-16 binary decodings, where a neighboring
            # pair of bytes is often a Unicode letter (CJK, Hangul, etc.); using \p{L} lost almost all UTF-16 detection
            # (99.9% before versus 5.9%, measured with random neighbors).
            # Guarantee: an ASCII value followed by an accented Latin letter (e.g. "Ren" followed by an accented e) is NOT
            # a false leak in native UTF-8 and UTF-16 text decoding. Everything else remains a safe-side false positive
            # (as before): an accented letter BEFORE the value in Latin-1 and UTF-8, an accented letter AFTER the value
            # in Latin-1 (the UTF-8 decoding of that stray byte is U+FFFD, which is not a letter), and generally any value
            # that Latin-1 decoding of valid UTF-8 leaves next to a continuation byte (such as the second byte of an
            # accented e in UTF-8, which is a symbol in Latin-1). A value adjacent to Cyrillic, Greek, or CJK letters,
            # and neighbors that are NULs, symbols, or U+FFFD, also count as leaks.
            $pattern = if ($k -eq 'USERNAME' -or $k -eq 'COMPUTERNAME') { '(?<![0-9A-Za-z\u00C0-\u00D6\u00D8-\u00F6\u00F8-\u024F])' + [regex]::Escape($form) + '(?![0-9A-Za-z\u00C0-\u00D6\u00D8-\u00F6\u00F8-\u024F])' } else { [regex]::Escape($form) }
            # CultureInvariant: case-insensitive matching must not depend on the user's locale (e.g. Turkish i).
            $options = [System.Text.RegularExpressions.RegexOptions]::IgnoreCase -bor [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
            $hit = $false
            foreach ($t in $texts) {
                if ([regex]::IsMatch($t, $pattern, $options)) { $hit = $true; break }
            }
            if (-not $hit -and $null -ne $utf8NoNul) {
                # In UTF-8 decoded from bytes without NULs, a stray accented byte (such as the accented e from UTF-16
                # after removing its NULs) becomes U+FFFD. Treat it as a letter here to avoid inventing a leak that native
                # UTF-16 decoding above would discard.
                $pattern8 = $pattern.Replace('\u00F8-\u024F]', '\u00F8-\u024F\uFFFD]')
                $hit = [regex]::IsMatch($utf8NoNul, $pattern8, $options)
            }
            if ($hit) { $leaks.Add("${Name}: contains local value '$k'") }
        }
    }
    return , $leaks
}
