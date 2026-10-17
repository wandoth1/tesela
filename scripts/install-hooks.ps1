<#
.SYNOPSIS
  Install Git `pre-commit` and `commit-msg` hooks that run scripts/privacy-check.ps1.

.DESCRIPTION
  Hooks live in `.git/hooks/` (they are not versioned and are shared across worktrees of the same repository), so each
  clone must run this script once:
    pwsh -File scripts/install-hooks.ps1
  * `pre-commit` checks staged content and file names.
  * `commit-msg` checks the commit message using the same patterns.
  The hooks REQUIRE PowerShell 7 (`pwsh`): if it is not installed, the commit is blocked with a clear message (there is
  no fallback to Windows PowerShell 5.1). If scripts/privacy-check.ps1 is missing from the worktree, the commit is also
  blocked with a clear message. Existing `pre-commit` and `commit-msg` hooks are overwritten.

  Hooks pass -Local: a CI variable defined in the developer profile does not disable local value checks.

  Optional local denylist: `<git-common-dir>/privacy-denylist` (one term per line; see the README).
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$repoRoot = (& git rev-parse --show-toplevel).Trim()
$hooksDir = (& git rev-parse --git-path hooks).Trim()
if (-not [System.IO.Path]::IsPathRooted($hooksDir)) { $hooksDir = Join-Path $repoRoot $hooksDir }
New-Item -ItemType Directory -Force -Path $hooksDir | Out-Null

function New-Hook([string]$Name, [string]$Invocation) {
    $body = @'
#!/bin/sh
# Installed by scripts/install-hooks.ps1: ensure developer machine data is not committed.
root="$(git rev-parse --show-toplevel)"
script="$root/scripts/privacy-check.ps1"
if [ ! -f "$script" ]; then
  echo "privacy-check: $script is missing from this worktree; commit blocked (use --no-verify only if you know what you are doing)." >&2
  exit 1
fi
if ! command -v pwsh >/dev/null 2>&1; then
  echo "privacy-check: PowerShell 7 (pwsh) is required on PATH; commit blocked. Install it: winget install Microsoft.PowerShell" >&2
  exit 1
fi
@@INVOCATION@@
'@
    $content = ($body.Replace('@@INVOCATION@@', $Invocation)) -replace "`r`n", "`n"
    $path = Join-Path $hooksDir $Name
    [System.IO.File]::WriteAllText($path, $content, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "Hook installed at $path"
}

New-Hook 'pre-commit' 'exec pwsh -NoProfile -File "$script" -Local'
New-Hook 'commit-msg' @'
case "$1" in
  /*|?:*) msg="$1" ;;
  *) msg="$PWD/$1" ;;
esac
exec pwsh -NoProfile -File "$script" -Local -MessageFile "$msg"
'@
