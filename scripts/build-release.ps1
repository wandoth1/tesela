<#
.SYNOPSIS
  Compila el workspace en release SIN rutas de esta maquina en los binarios y lo comprueba.

.DESCRIPTION
  Los panics y la depuracion de las dependencias incrustan rutas absolutas (el registro de cargo bajo el perfil del
  usuario, el repositorio, el toolchain). `--remap-path-prefix` las sustituye, pero el prefijo a sustituir depende de la
  maquina y no se puede escribir en `.cargo/config.toml` (no admite variables ni expansion de rutas) ni versionar: este
  script lo calcula EN TIEMPO DE EJECUCION y lo pasa con CARGO_ENCODED_RUSTFLAGS (que admite espacios en las rutas).
  `trim-paths` de cargo sigue siendo inestable, por eso no se usa.

  CARGO_ENCODED_RUSTFLAGS tiene prioridad sobre `target.<cfg>.rustflags` de `.cargo/config.toml` (no se combinan), asi que
  el enlace estatico del runtime de C (`-Ctarget-feature=+crt-static`, que el config tambien fija para los builds de
  desarrollo) se repite aqui: sin el, el release importaria VCRUNTIME140.dll y las `api-ms-win-crt-*`, que no vienen en
  un Windows limpio (el instalador es por usuario y sin administrador). La validacion de abajo lo comprueba.

  Los ejecutables que se comprueban salen de la salida JSON de Cargo (`--message-format=json`, campo `executable` de
  cada `compiler-artifact`), no de un listado de la carpeta: asi no se aprueban 0 binarios, los de otra arquitectura ni
  uno antiguo que quedo de otra compilacion. Se exige EXACTAMENTE el conjunto esperado (`-ExpectedExes`, por defecto
  `tesela.exe` y `tesela-scan.exe`) y se falla si falta alguno, si aparece otro, o si en la carpeta de release queda un
  .exe que esta compilacion no ha producido.

  Despues de compilar:
    1. busca en cada .exe (ASCII y UTF-16, con ambas barras `\` y `/`) los valores locales -nombre de usuario, nombre del
       equipo, perfil, repositorio, CARGO_HOME y toolchain- leidos del entorno, y falla si aparece alguno. Nunca se
       escriben (ver scripts/leak-scan.ps1);
    2. con `scripts/check-pe.py` (Python, solo biblioteca estandar) lee la tabla de imports PE de cada .exe y falla si
       aparece `VCRUNTIME*.dll`, `MSVCP*.dll` o `api-ms-win-crt-*.dll` o si la arquitectura no es la esperada.

  El CI compila los releases con este mismo script.

.PARAMETER SkipCheck
  No busca valores locales ni valida los imports (la comprobacion del conjunto de ejecutables se hace siempre).

.PARAMETER CheckOnly
  No compila: comprueba los binarios de release que ya existan (los de los destinos `bin` del workspace).

.PARAMETER CargoArgs
  Argumentos extra para `cargo build --release` (por defecto: --workspace).

.PARAMETER ExpectedExes
  Nombres de los ejecutables que tiene que producir la compilacion (por defecto tesela.exe y tesela-scan.exe).

.EXAMPLE
  pwsh -File scripts/build-release.ps1
#>
[CmdletBinding()]
param(
    [switch]$SkipCheck,
    [switch]$CheckOnly,
    [string[]]$CargoArgs = @('--workspace'),
    [string[]]$ExpectedExes = @('tesela.exe', 'tesela-scan.exe')
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'leak-scan.ps1')
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
# Runtime de C enlazado de forma estatica (ver arriba): tiene que coincidir con .cargo/config.toml.
$flags.Add('-Ctarget-feature=+crt-static')
$env:CARGO_ENCODED_RUSTFLAGS = ($flags -join [string][char]0x1f)
$env:RUSTFLAGS = $null

# Arquitectura que se espera en los binarios: la de `--target` si se pasa, y si no la del equipo que compila.
$triple = $null
for ($i = 0; $i -lt $CargoArgs.Count; $i++) {
    if ($CargoArgs[$i] -eq '--target' -and ($i + 1) -lt $CargoArgs.Count) { $triple = $CargoArgs[$i + 1] }
    elseif ($CargoArgs[$i] -like '--target=*') { $triple = $CargoArgs[$i].Substring(9) }
}
if (-not $triple) {
    $hostLine = (& rustc -vV) | Where-Object { $_ -like 'host:*' } | Select-Object -First 1
    $triple = $hostLine.Substring(5).Trim()
}
$machine = if ($triple -like 'aarch64-*') { 'arm64' } elseif ($triple -like 'x86_64-*') { 'x64' } elseif ($triple -like 'i?86-*') { 'x86' } else { throw "destino desconocido: $triple" }

Push-Location $repo
try {
    $artifacts = @()
    if (-not $CheckOnly) {
        # La salida estandar es JSON (una linea por mensaje); los diagnosticos del compilador siguen yendo a la consola.
        $json = & cargo build --release @CargoArgs --message-format=json-render-diagnostics
        if ($LASTEXITCODE -ne 0) { throw 'cargo build --release fallo' }
        foreach ($line in $json) {
            if (-not $line -or -not $line.StartsWith('{')) { continue }
            $m = $line | ConvertFrom-Json
            if ($m.reason -eq 'compiler-artifact' -and $m.executable -and ($m.target.kind -contains 'bin')) {
                $artifacts += $m.executable
            }
        }
    }

    $meta = & cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    if ($CheckOnly) {
        $releaseDirMeta = Join-Path $meta.target_directory 'release'
        foreach ($pkg in $meta.packages) {
            foreach ($t in $pkg.targets) {
                if ($t.kind -contains 'bin') { $artifacts += (Join-Path $releaseDirMeta ($t.name + '.exe')) }
            }
        }
    }
    # Rutas normalizadas (cargo puede mezclar barras si CARGO_TARGET_DIR las mezcla) para compararlas sin sorpresas.
    $artifacts = @($artifacts | ForEach-Object { [System.IO.Path]::GetFullPath($_) } | Sort-Object -Unique)

    # --- El conjunto de ejecutables tiene que ser EXACTAMENTE el esperado ---------------------------------------------
    $names = @($artifacts | ForEach-Object { Split-Path -Leaf $_ })
    $missing = @($ExpectedExes | Where-Object { $n = $_; -not ($names | Where-Object { $_ -ieq $n }) })
    $unexpected = @($names | Where-Object { $n = $_; -not ($ExpectedExes | Where-Object { $_ -ieq $n }) })
    $absent = @($artifacts | Where-Object { -not (Test-Path -LiteralPath $_ -PathType Leaf) })
    $setProblems = New-Object System.Collections.Generic.List[string]
    if ($artifacts.Count -eq 0) { $setProblems.Add('la compilacion no produjo ningun ejecutable') }
    foreach ($n in $missing) { $setProblems.Add("falta el ejecutable esperado: $n") }
    foreach ($n in $unexpected) { $setProblems.Add("ejecutable inesperado (si es nuevo, anadelo a -ExpectedExes): $n") }
    foreach ($a in $absent) { $setProblems.Add("el ejecutable que dice cargo no existe: $(Split-Path -Leaf $a)") }
    if ($artifacts.Count -gt 0) {
        # Un .exe en la carpeta de release que esta compilacion NO ha producido es de otra compilacion (otro nombre, otra
        # configuracion): no se aprueba ni se publica por error.
        $releaseDirs = @($artifacts | ForEach-Object { Split-Path -Parent $_ } | Sort-Object -Unique)
        foreach ($d in $releaseDirs) {
            foreach ($f in @(Get-ChildItem -LiteralPath $d -Filter '*.exe' -File -ErrorAction SilentlyContinue)) {
                $full = [System.IO.Path]::GetFullPath($f.FullName)
                if (-not ($artifacts | Where-Object { $_ -ieq $full })) {
                    $setProblems.Add("ejecutable antiguo en la carpeta de release (no lo ha producido esta compilacion): $($f.Name)")
                }
            }
        }
    }
    if ($setProblems.Count -gt 0) {
        Write-Host 'build-release: el conjunto de ejecutables no es el esperado:' -ForegroundColor Red
        $setProblems | ForEach-Object { Write-Host "  $_" }
        exit 1
    }
    foreach ($a in $artifacts) {
        Write-Host ('build-release: {0}: {1:N0} bytes ({2:N2} MiB)' -f (Split-Path -Leaf $a), (Get-Item -LiteralPath $a).Length, ((Get-Item -LiteralPath $a).Length / 1MB))
    }
    if ($SkipCheck) { exit 0 }

    # Valores locales (usuario, equipo, perfil, repositorio, CARGO_HOME, toolchain...) en cada .exe, con `\` y con `/`.
    $needles = Get-LocalValueNeedles -Repo $repo -CargoHome $cargoHome -Sysroot $sysroot -RustupHome $rustupHome
    $leaks = New-Object System.Collections.Generic.List[string]
    foreach ($exePath in $artifacts) {
        $found = Find-LocalValueLeaks -Bytes ([System.IO.File]::ReadAllBytes($exePath)) -Needles $needles -Name (Split-Path -Leaf $exePath)
        foreach ($f in $found) { $leaks.Add($f) }
    }
    if ($leaks.Count -gt 0) {
        Write-Host 'build-release: los binarios contienen datos de esta maquina:' -ForegroundColor Red
        $leaks | Sort-Object -Unique | ForEach-Object { Write-Host "  $_" }
        exit 1
    }

    # --- Tabla de imports PE: sin runtime de C dinamico y de la arquitectura esperada ------------------------------------
    $py = Get-Command python -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $py) { $py = Get-Command python3 -ErrorAction SilentlyContinue | Select-Object -First 1 }
    if (-not $py) {
        Write-Host 'build-release: hace falta Python 3 en el PATH para leer la tabla de imports de los .exe' -ForegroundColor Red
        exit 1
    }
    & $py.Source -I (Join-Path $repo 'scripts/check-pe.py') --machine $machine @artifacts
    if ($LASTEXITCODE -ne 0) {
        Write-Host 'build-release: los binarios importan el runtime de C (VCRUNTIME*, MSVCP*, api-ms-win-crt-*) o no son de la arquitectura esperada' -ForegroundColor Red
        exit 1
    }
    Write-Host "build-release: OK ($($artifacts.Count) binarios $machine sin valores locales y sin runtime de C dinamico)"
} finally {
    Pop-Location
}
