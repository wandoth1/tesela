<#
.SYNOPSIS
  Instala los hooks de git `pre-commit` y `commit-msg` que ejecutan scripts/privacy-check.ps1.

.DESCRIPTION
  Los hooks viven en `.git/hooks/` (no se versionan, y se comparten entre los worktrees del mismo repositorio), asi que
  cada clon debe ejecutar este script una vez:
    pwsh -File scripts/install-hooks.ps1
  * `pre-commit` revisa el contenido en staging y los nombres de archivo.
  * `commit-msg` revisa el mensaje del commit con los mismos patrones.
  Los hooks EXIGEN PowerShell 7 (`pwsh`): si no esta instalado, el commit se bloquea con un mensaje claro (no se cae a
  Windows PowerShell 5.1). Si falta scripts/privacy-check.ps1 en el worktree, tambien se bloquea con un mensaje claro.
  Sobrescribe los hooks `pre-commit` y `commit-msg` anteriores.

  Los hooks pasan -Local: un CI definido en el perfil del desarrollador no desactiva los valores locales.

  Lista negra local opcional: `<git-common-dir>/privacy-denylist` (un termino por linea; ver el README).
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
# Instalado por scripts/install-hooks.ps1: comprueba que no se commitean datos de la maquina del desarrollador.
root="$(git rev-parse --show-toplevel)"
script="$root/scripts/privacy-check.ps1"
if [ ! -f "$script" ]; then
  echo "privacy-check: no existe $script en este worktree; commit bloqueado (usa --no-verify solo si sabes lo que haces)." >&2
  exit 1
fi
if ! command -v pwsh >/dev/null 2>&1; then
  echo "privacy-check: hace falta PowerShell 7 (pwsh) en el PATH; commit bloqueado. Instalalo: winget install Microsoft.PowerShell" >&2
  exit 1
fi
@@INVOCATION@@
'@
    $content = ($body.Replace('@@INVOCATION@@', $Invocation)) -replace "`r`n", "`n"
    $path = Join-Path $hooksDir $Name
    [System.IO.File]::WriteAllText($path, $content, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "Hook instalado en $path"
}

New-Hook 'pre-commit' 'exec pwsh -NoProfile -File "$script" -Local'
New-Hook 'commit-msg' @'
case "$1" in
  /*|?:*) msg="$1" ;;
  *) msg="$PWD/$1" ;;
esac
exec pwsh -NoProfile -File "$script" -Local -MessageFile "$msg"
'@
