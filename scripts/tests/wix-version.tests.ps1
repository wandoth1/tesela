<#
.SYNOPSIS
  Check that the WiX version matches in scripts/build-msi.ps1, .github/workflows/ci.yml, .github/workflows/release.yml
  (and installer/README.md), that the .wxs UpgradeCode matches msi-validate.ps1, and that scripts/install-wix.ps1 checks
  the package SHA-512 BEFORE installation (an incorrect package is never installed).

.DESCRIPTION
  All three locations pin the version (`$WixVersion = 'x.y.z'` in the script; `WIX_VERSION: "x.y.z"` in the workflows).
  A mismatch would break the MSI build in CI or, worse, use a different tool for the release.

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

Check 'build-msi.ps1 pins the WiX version' ($null -ne $scriptVersion -and $scriptVersion -match '^\d+\.\d+\.\d+$')
Check "ci.yml pins WIX_VERSION ($ciVersion) to match build-msi.ps1 ($scriptVersion)" ($ciVersion -eq $scriptVersion)
Check "release.yml pins WIX_VERSION ($relVersion) to match build-msi.ps1 ($scriptVersion)" ($relVersion -eq $scriptVersion)
Check 'installer/README.md documents the pinned version' ($scriptVersion -and $readme.Contains("--version $scriptVersion"))
$installer = Read-Text 'scripts/install-wix.ps1'
Check "install-wix.ps1 has WiX $scriptVersion and its SHA-512" ($scriptVersion -and [regex]::IsMatch($installer, "'" + [regex]::Escape($scriptVersion) + "'\s*=\s*'[A-Za-z0-9+/]{86}=='"))
foreach ($f in '.github/workflows/ci.yml', '.github/workflows/release.yml') {
    $t = Read-Text $f
    Check "$f installs WiX with scripts/install-wix.ps1 and WIX_VERSION, never with another command" (
        $t.Contains('./scripts/install-wix.ps1 -Version $env:WIX_VERSION') -and -not $t.Contains('dotnet tool install'))
}
Check 'the .wxs header specifies the same WiX version' ((Read-Text 'installer/Tesela.wxs').Contains("WiX Toolset $scriptVersion"))

# UpgradeCode: the value in the .wxs and the one checked by build-msi.ps1.
$wxs = Read-Text 'installer/Tesela.wxs'
$wxsCode = First-Match $wxs 'UpgradeCode="([0-9A-Fa-f-]{36})"'
$scriptCode = First-Match (Read-Text 'scripts/msi-validate.ps1') "^\`$MsiExpectedUpgradeCode\s*=\s*'\{([0-9A-Fa-f-]{36})\}'"
Check 'the .wxs has an UpgradeCode' ($null -ne $wxsCode)
Check "the .wxs UpgradeCode ($wxsCode) matches the one expected by msi-validate.ps1 ($scriptCode)" ($wxsCode -and $scriptCode -and $wxsCode -ieq $scriptCode)
Check 'all component GUIDs in the .wxs are unique' (
    @([regex]::Matches($wxs, 'Guid="([0-9A-Fa-f-]{36})"') | ForEach-Object { $_.Groups[1].Value.ToUpperInvariant() } | Sort-Object -Unique).Count -eq
    [regex]::Matches($wxs, 'Guid="([0-9A-Fa-f-]{36})"').Count)

# --- install-wix.ps1: check SHA-512 BEFORE installation and install from the verified file -----------------------------
$iw = $installer
$posHash = $iw.IndexOf('Assert-PackageHash $local')
$posInstall = $iw.IndexOf("'tool', 'install'")
Check 'install-wix.ps1 checks the downloaded package hash BEFORE the install command' ($posHash -ge 0 -and $posInstall -gt $posHash)
Check 'install-wix.ps1 installs only from the verified folder (--source) and uses installer/nuget.config' ($iw.Contains("'--source', `$work") -and $iw.Contains("'--configfile', `$config"))
Check 'installer/nuget.config retains <clear/> and uses only nuget.org' (
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
        # An unexpected package: do not install anything or create the tools directory.
        $junk = Join-Path $tmp 'junk.nupkg'
        [System.IO.File]::WriteAllBytes($junk, [byte[]](1, 2, 3, 4, 5))
        $tp = Join-Path $tmp 'bad-hash'
        $r = Run-Install @('-Version', '5.0.2', '-ToolPath', $tp, '-NupkgPath', $junk)
        Check "invalid hash (junk file): fails ($($r.Code))" ($r.Code -ne 0 -and $r.Out -like '*SHA-512*')
        Check 'invalid hash (junk file): nothing was installed' (-not (Installed-In $tp))
        # A version not listed in the table.
        $r = Run-Install @('-Version', '9.9.9', '-ToolPath', (Join-Path $tmp 'unknown'))
        Check 'version not in the table: fails' ($r.Code -ne 0)
        # With this machine's REAL package (if WiX 5.0.2 is installed globally): a copy with one byte changed must not be
        # installed; the intact package must install from a temporary tools directory (the global WiX is untouched).
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
            Check "real package with ONE byte changed: fails ($($r.Code))" ($r.Code -ne 0 -and $r.Out -like '*SHA-512*')
            Check 'real package with ONE byte changed: nothing was installed' (-not (Installed-In $tp))
            $tp = Join-Path $tmp 'good'
            $r = Run-Install @('-Version', '5.0.2', '-ToolPath', $tp, '-NupkgPath', $cached)
            Check "intact real package: installs from the verified file ($($r.Code))" ($r.Code -eq 0 -and (Test-Path -LiteralPath (Join-Path $tp 'wix.exe')))
            $r = Run-Install @('-Version', '5.0.2', '-ToolPath', $tp)
            Check 'a second run detects the existing installation and does not reinstall' ($r.Code -eq 0 -and $r.Out -like '*is already installed*')
            # WiX 5.0.2 is installed but has DIFFERENT contents (the stored .nupkg is not the pinned package): the
            # post-install check rejects it. Only the temporary tools directory copy is modified, never global WiX.
            $store = Join-Path $tp '.store/wix/5.0.2/wix/5.0.2/wix.5.0.2.nupkg'
            Check 'the test installation has its .nupkg in the store' (Test-Path -LiteralPath $store)
            [System.IO.File]::WriteAllBytes($store, [byte[]](9, 8, 7, 6, 5))
            $r = Run-Install @('-Version', '5.0.2', '-ToolPath', $tp)
            Check "WiX already installed with a package of different contents: fails ($($r.Code))" ($r.Code -ne 0 -and $r.Out -like '*SHA-512*installed*')
        } else {
            Write-Host 'wix-version.tests: skipping install tests; WiX 5.0.2 package is not in the local cache'
        }
    } finally {
        if ((Split-Path -Parent $tmp) -eq [System.IO.Path]::GetTempPath().TrimEnd('\') -and (Split-Path -Leaf $tmp).StartsWith('tesela-test-')) {
            Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
} else {
    Write-Host 'wix-version.tests: skipping dynamic install-wix.ps1 tests; dotnet is not available'
}

if ($failures.Count -gt 0) {
    Write-Host "wix-version.tests: $($failures.Count) of $total checks failed:" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host "wix-version.tests: OK ($total checks)"
# Set the exit code EXPLICITLY: an earlier external command (dotnet, git, wix...) may leave its code in $LASTEXITCODE,
# which GitHub Actions (pwsh -command) uses as the step exit code even if this script succeeded.
exit 0
