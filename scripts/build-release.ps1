<#
.SYNOPSIS
  Builds the workspace in release mode WITHOUT machine-specific paths in the binaries, then checks them.

.DESCRIPTION
  Panics and dependency debug information embed absolute paths (Cargo's registry under the user's profile,
  the repository, and the toolchain). `--remap-path-prefix` replaces them, but the prefix depends on the machine and
  cannot be stored in `.cargo/config.toml` (it does not support variables or path expansion) or committed. This script
  calculates it AT RUNTIME and passes it through CARGO_ENCODED_RUSTFLAGS (which supports spaces in paths).
  Cargo's `trim-paths` is still unstable, so it is not used.

  CARGO_ENCODED_RUSTFLAGS takes precedence over `target.<cfg>.rustflags` in `.cargo/config.toml` (they are not combined), so
  static linking of the C runtime (`-Ctarget-feature=+crt-static`, also set in the config for
  development builds) is repeated here (as is `/Brepro`, see below): without it, the release would import VCRUNTIME140.dll and `api-ms-win-crt-*`. On Windows 10/11, the
  UCRT is part of the system; it is statically linked to avoid depending on the redistributable VC++ runtime (the
  installer is per-user and does not require administrator rights). The validation below checks this.

  The executables being checked come from Cargo's JSON output (`--message-format=json`, the `executable` field of each
  `compiler-artifact`), rather than a directory listing. This prevents accepting zero binaries, binaries for another
  architecture, or a stale binary from an earlier build. EXACTLY the expected set is required (`-ExpectedExes`, by
  default `tesela.exe` and `tesela-scan.exe`); the script fails if any are missing, another appears, or the release
  directory contains an .exe not produced by this build.

  After the build:
    1. searches each .exe (ASCII and UTF-16, with both `\` and `/`) for local values read from the environment—user name,
       computer name, profile, repository, CARGO_HOME, and toolchain—and fails if any are found. They are never written
       (see scripts/leak-scan.ps1);
    2. uses `scripts/check-pe.py` (Python standard library only) to read each .exe's PE import table and fails if it
       contains `VCRUNTIME*.dll`, `MSVCP*.dll`, or `api-ms-win-crt-*.dll`, or if its architecture is unexpected.

  CI builds releases with this same script.

.PARAMETER SkipCheck
  Does not search for local values or validate imports (the executable set is always checked).

.PARAMETER CheckOnly
  Does not build; checks existing release binaries (the workspace `bin` targets).

.PARAMETER CargoArgs
  Additional arguments for `cargo build --release` (default: --workspace).

.PARAMETER ExpectedExes
  Names of the executables the build must produce (default: tesela.exe and tesela-scan.exe).

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

# rustc applies remaps from LAST to FIRST (the last matching rule wins): put the most specific rule later. The sysroot is
# under RUSTUP_HOME, so `/rust-toolchain` comes AFTER `/rustup` (otherwise the entire sysroot would map to `/rustup/toolchains/...`).
$remaps = @(
    @($repo, '/tesela'),
    @($cargoHome, '/cargo'),
    @($rustupHome, '/rustup'),
    @($sysroot, '/rust-toolchain')
)
$flags = New-Object System.Collections.Generic.List[string]
foreach ($r in $remaps) {
    # Try the path as-is, with `/`, and with `\` (Git, Cargo, and Windows may use different separators).
    foreach ($form in @($r[0], $r[0].Replace('\', '/'), $r[0].Replace('/', '\')) | Select-Object -Unique) {
        if ($form) { $flags.Add("--remap-path-prefix=$form=$($r[1])") }
    }
}
# Statically linked C runtime (see above): must match .cargo/config.toml.
$flags.Add('-Ctarget-feature=+crt-static')
# Reproducible build: without `/Brepro`, the MSVC linker records a different timestamp (and PDB GUID) in each build.
# With it, builds of the same commit in different directories produce the SAME SHA-256 (paths are normalized by
# --remap-path-prefix). Must match .cargo/config.toml.
$flags.Add('-Clink-arg=/Brepro')
$env:CARGO_ENCODED_RUSTFLAGS = ($flags -join [string][char]0x1f)
$env:RUSTFLAGS = $null

# Expected binary architecture: the `--target` value if provided, otherwise the build machine's architecture.
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
        # `--locked`: un release no puede cambiar Cargo.lock (ni resolver dependencias nuevas) por su cuenta.
        $json = & cargo build --release --locked @CargoArgs --message-format=json-render-diagnostics
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
    # Normalize paths (Cargo can mix separators if CARGO_TARGET_DIR does) before comparing them.
    $artifacts = @($artifacts | ForEach-Object { [System.IO.Path]::GetFullPath($_) } | Sort-Object -Unique)

    # --- The executable set must be EXACTLY the expected set ----------------------------------------------------------
    $names = @($artifacts | ForEach-Object { Split-Path -Leaf $_ })
    $missing = @($ExpectedExes | Where-Object { $n = $_; -not ($names | Where-Object { $_ -ieq $n }) })
    $unexpected = @($names | Where-Object { $n = $_; -not ($ExpectedExes | Where-Object { $_ -ieq $n }) })
    $absent = @($artifacts | Where-Object { -not (Test-Path -LiteralPath $_ -PathType Leaf) })
    $setProblems = New-Object System.Collections.Generic.List[string]
    if ($artifacts.Count -eq 0) { $setProblems.Add('the build produced no executables') }
    foreach ($n in $missing) { $setProblems.Add("expected executable is missing: $n") }
    foreach ($n in $unexpected) { $setProblems.Add("unexpected executable (if new, add it to -ExpectedExes): $n") }
    foreach ($a in $absent) { $setProblems.Add("Cargo-reported executable does not exist: $(Split-Path -Leaf $a)") }
    if ($artifacts.Count -gt 0) {
        # An .exe in the release directory that this build did NOT produce belongs to another build (different name or
        # configuration); do not accept or publish it by mistake.
        $releaseDirs = @($artifacts | ForEach-Object { Split-Path -Parent $_ } | Sort-Object -Unique)
        foreach ($d in $releaseDirs) {
            foreach ($f in @(Get-ChildItem -LiteralPath $d -Filter '*.exe' -File -ErrorAction SilentlyContinue)) {
                $full = [System.IO.Path]::GetFullPath($f.FullName)
                if (-not ($artifacts | Where-Object { $_ -ieq $full })) {
                    $setProblems.Add("stale executable in the release directory (not produced by this build): $($f.Name)")
                }
            }
        }
    }
    if ($setProblems.Count -gt 0) {
        Write-Host 'build-release: executable set does not match the expected set:' -ForegroundColor Red
        $setProblems | ForEach-Object { Write-Host "  $_" }
        exit 1
    }
    foreach ($a in $artifacts) {
        Write-Host ('build-release: {0}: {1:N0} bytes ({2:N2} MiB)' -f (Split-Path -Leaf $a), (Get-Item -LiteralPath $a).Length, ((Get-Item -LiteralPath $a).Length / 1MB))
    }
    if ($SkipCheck) { exit 0 }

    # Check each .exe for local values (user, computer, profile, repository, CARGO_HOME, toolchain...) with both `\` and `/`.
    $needles = Get-LocalValueNeedles -Repo $repo -CargoHome $cargoHome -Sysroot $sysroot -RustupHome $rustupHome
    $leaks = New-Object System.Collections.Generic.List[string]
    foreach ($exePath in $artifacts) {
        $found = Find-LocalValueLeaks -Bytes ([System.IO.File]::ReadAllBytes($exePath)) -Needles $needles -Name (Split-Path -Leaf $exePath)
        foreach ($f in $found) { $leaks.Add($f) }
    }
    if ($leaks.Count -gt 0) {
        Write-Host 'build-release: binaries contain data from this machine:' -ForegroundColor Red
        $leaks | Sort-Object -Unique | ForEach-Object { Write-Host "  $_" }
        exit 1
    }

    # --- PE import table: no dynamic C runtime and expected architecture -----------------------------------------------
    $py = Get-Command python -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $py) { $py = Get-Command python3 -ErrorAction SilentlyContinue | Select-Object -First 1 }
    if (-not $py) {
        Write-Host 'build-release: Python 3 on PATH is required to read the .exe import table' -ForegroundColor Red
        exit 1
    }
    & $py.Source -I (Join-Path $repo 'scripts/check-pe.py') --machine $machine @artifacts
    if ($LASTEXITCODE -ne 0) {
        Write-Host 'build-release: binaries import the C runtime (VCRUNTIME*, MSVCP*, api-ms-win-crt-*) or have an unexpected architecture' -ForegroundColor Red
        exit 1
    }
    Write-Host "build-release: OK ($($artifacts.Count) $machine binaries without local values or the dynamic C runtime)"
} finally {
    Pop-Location
}
