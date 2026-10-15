<#
.SYNOPSIS
  Compila el workspace en release SIN rutas de esta maquina en los binarios y lo comprueba.

.DESCRIPTION
  Los panics y la depuracion de las dependencias incrustan rutas absolutas (el registro de cargo bajo el perfil del
  usuario, el repositorio, el toolchain). `--remap-path-prefix` las sustituye, pero el prefijo a sustituir depende de la
  maquina y no se puede escribir en `.cargo/config.toml` (no admite variables ni expansion de rutas) ni versionar: este
  script lo calcula EN TIEMPO DE EJECUCION y lo pasa con CARGO_ENCODED_RUSTFLAGS (que admite espacios en las rutas).
  `trim-paths` de cargo sigue siendo inestable, por eso no se usa.

  Despues de compilar busca en cada .exe (ASCII y UTF-16, con ambas barras) los valores locales -nombre de usuario,
  perfil, repositorio, CARGO_HOME y toolchain- leidos del entorno, y falla si aparece alguno. Nunca se escriben.

  El CI compila los releases con este mismo script.

.PARAMETER SkipCheck
  No busca valores locales en los binarios.

.PARAMETER CheckOnly
  No compila: solo busca valores locales en los binarios de release que ya existan.

.PARAMETER CargoArgs
  Argumentos extra para `cargo build --release` (por defecto: --workspace).

.EXAMPLE
  pwsh -File scripts/build-release.ps1
#>
[CmdletBinding()]
param(
    [switch]$SkipCheck,
    [switch]$CheckOnly,
    [string[]]$CargoArgs = @('--workspace')
)

$ErrorActionPreference = 'Stop'
$repo = (& git rev-parse --show-toplevel).Trim()
$cargoHome = $env:CARGO_HOME
if (-not $cargoHome) { $cargoHome = Join-Path $env:USERPROFILE '.cargo' }
$sysroot = (& rustc --print sysroot).Trim()
$rustupHome = $env:RUSTUP_HOME
if (-not $rustupHome) { $rustupHome = Join-Path $env:USERPROFILE '.rustup' }

$remaps = @(
    @($repo, '/tesela'),
    @($cargoHome, '/cargo'),
    @($sysroot, '/rust-toolchain'),
    @($rustupHome, '/rustup')
)
$flags = New-Object System.Collections.Generic.List[string]
foreach ($r in $remaps) {
    foreach ($form in @($r[0], $r[0].Replace('\', '/'))) {
        if ($form) { $flags.Add("--remap-path-prefix=$form=$($r[1])") }
    }
}
$env:CARGO_ENCODED_RUSTFLAGS = ($flags -join [string][char]0x1f)
$env:RUSTFLAGS = $null

Push-Location $repo
try {
    if (-not $CheckOnly) {
        & cargo build --release @CargoArgs
        if ($LASTEXITCODE -ne 0) { throw 'cargo build --release fallo' }
    }
    if ($SkipCheck) { exit 0 }

    $meta = & cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    $releaseDir = Join-Path $meta.target_directory 'release'
    $needles = @{
        'USERNAME'    = $env:USERNAME
        'USERPROFILE' = $env:USERPROFILE
        'repositorio' = $repo
        'CARGO_HOME'  = $cargoHome
        'toolchain'   = $sysroot
        'RUSTUP_HOME' = $rustupHome
    }
    $leaks = New-Object System.Collections.Generic.List[string]
    $exes = @(Get-ChildItem -LiteralPath $releaseDir -Filter '*.exe' -File)
    foreach ($exe in $exes) {
        $bytes = [System.IO.File]::ReadAllBytes($exe.FullName)
        $texts = @(
            [System.Text.Encoding]::GetEncoding(28591).GetString($bytes),
            [System.Text.Encoding]::Unicode.GetString($bytes),
            [System.Text.Encoding]::Unicode.GetString($bytes, 1, ($bytes.Length - 1) -band -2)
        )
        foreach ($k in $needles.Keys) {
            $v = $needles[$k]
            if (-not $v -or $v.Length -lt 3) { continue }
            foreach ($form in @($v, $v.Replace('\', '/'))) {
                foreach ($t in $texts) {
                    if ($t.IndexOf($form, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
                        $leaks.Add("$($exe.Name): contiene el valor local '$k'")
                        break
                    }
                }
            }
        }
    }
    if ($leaks.Count -gt 0) {
        Write-Host 'build-release: los binarios contienen datos de esta maquina:' -ForegroundColor Red
        $leaks | Sort-Object -Unique | ForEach-Object { Write-Host "  $_" }
        exit 1
    }
    Write-Host "build-release: OK ($($exes.Count) binarios sin valores locales)"
} finally {
    Pop-Location
}
