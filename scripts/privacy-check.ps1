<#
.SYNOPSIS
  Falla si el repositorio contiene datos que identifiquen la maquina o al usuario que desarrolla.

.DESCRIPTION
  El repositorio es PUBLICO y no puede contener informacion de ningun PC: hardware, sistema, usuario, discos,
  rutas de unidades personales, SID, etc. Compatible con PowerShell 7 (pwsh) y con Windows PowerShell 5.1; el hook
  git exige pwsh. El script es solo ASCII a proposito (5.1 lee los .ps1 sin BOM como ANSI).

  Modos:
    * (por defecto)  archivos EN STAGING (`git diff --cached`), contenido del indice. Para el hook `pre-commit`.
    * -All           todos los archivos versionados y los no ignorados (CI y revisiones completas).
    * -MessageFile   un mensaje de commit (hook `commit-msg`); se revisan todas las lineas (tambien las que empiezan por `#`) hasta la linea de tijera `# ------------------------ >8`.

  Que se comprueba:
    1. Patrones GENERICOS (siempre): SID (cuentas locales y AzureAD), rutas de perfil reales (Windows, `/c/Users/<usuario>`,
       `/mnt/c/Users/<usuario>`), unidades D: a Z:, fabricantes y modelos de CPU/GPU/disco, antivirus, builds de Windows,
       versiones de driver.
    2. Valores LOCALES (solo en la maquina del desarrollador; nunca con -GenericOnly ni en CI): el nombre de usuario,
       el nombre del equipo, el dominio y la ruta del perfil, leidos EN TIEMPO DE EJECUCION de las variables de entorno, y una lista
       negra local NO versionada: `<git-common-dir>/privacy-denylist` (un termino por linea, sin distinguir
       mayusculas, coincidencia literal; las lineas vacias y las que empiezan por `#` se ignoran).
    3. Los NOMBRES de los archivos pasan por las mismas reglas.
    4. Cualquier texto se decodifica antes de revisarlo (UTF-8, UTF-8 con BOM, UTF-16 con o sin BOM). Los binarios se
       revisan buscando cadenas ASCII y UTF-16 (los de mas de 8 MiB se rechazan); las imagenes se rechazan SALVO `docs/img/<nombre>.png` generadas con --demo (ver la excepcion mas abajo).
    5. `docs/spike-results.md` NO puede existir (se rehace por separado) y `spike/` se revisa con las reglas que no
       dependen de rutas (SID, hardware, antivirus, builds, valores locales y lista negra).

  Los patrones estan escritos con corchetes (p. ej. `Ge[F]orce`) para que este script no se detecte a si mismo y se
  pueda revisar como cualquier otro archivo.

.PARAMETER All
  Revisa todo el arbol versionado/no ignorado en vez de solo lo que esta en staging.

.PARAMETER GenericOnly
  No usa los valores locales ni la lista negra. Es el modo de CI; tambien se activa si existe la variable CI.

.PARAMETER Local
  Usa SIEMPRE los valores locales y la lista negra aunque exista la variable CI. Lo pasan los hooks.

.PARAMETER MessageFile
  Revisa este archivo como mensaje de commit en vez de los archivos.

.EXAMPLE
  pwsh -File scripts/privacy-check.ps1                 # staging (hook pre-commit)
  pwsh -File scripts/privacy-check.ps1 -All            # todo el arbol
  pwsh -File scripts/privacy-check.ps1 -MessageFile .git/COMMIT_EDITMSG
#>
[CmdletBinding()]
param(
    [switch]$All,
    [switch]$GenericOnly,
    [switch]$Local,
    [string]$MessageFile
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
    if ($p.ExitCode -ne 0) { throw "git $GitArgs fallo ($($p.ExitCode)): $($errTask.Result)" }
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
    Write-Host 'privacy-check: -Local y -GenericOnly son incompatibles' -ForegroundColor Red
    exit 2
}

# --- Reglas ---------------------------------------------------------------------------------------------------------
# Path = $true: regla ligada a rutas (no se aplica en spike/). Los corchetes evitan que el script se detecte a si mismo.
$generic = @(
    @{ Name = 'SID de Windows'; Path = $false; Pattern = 'S-1-5-21-\d' },
    @{ Name = 'SID de cuenta AzureAD'; Path = $false; Pattern = 'S-1-12-1-\d' },
    @{ Name = 'Ruta de perfil real (use <usuario>)'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_])[A-Z]:[\\/]{1,2}Users[\\/]{1,2}(?!<usuario>|%|\{|\$|Public\b|Default\b|\*)[^\\/\s''"`]+' },
    @{ Name = 'Ruta de perfil real estilo Unix o WSL'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_.])/(mnt/)?[a-z]/Users/(?!<usuario>|Public\b|Default\b)[^/\s''"`]+' },
    @{ Name = 'Ruta absoluta de unidad D: a Z:'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_])[D-Z]:[\\/]' },
    @{ Name = 'Ruta WSL de unidad D a Z'; Path = $true; Pattern = '(?i)(?<![A-Za-z0-9_])/mnt/[d-z](/|$)' },
    @{ Name = 'Fabricante o modelo de GPU/CPU/disco'; Path = $false; Pattern = '(?i)\b(N[V]IDIA|Ge[F]orce|R[T]X\s?\d{3,4}\w*|G[T]X\s?\d{3,4}\w*|Rade[o]n|Ry[z]en|Threadrip[p]er|EP[Y]C|X[e]on|Inte[l]\s+Core|Inte[l]\(R\)\s*Core|Core\s+i[3579][ -]|Apple\s+M[1-9]|Sam[s]ung|WD_BL[A]CK|WD\s+Bl[a]ck|Western\s+Dig[i]tal|Sea[g]ate|Kings[t]on|Cruc[i]al|Cors[a]ir|SN8\d\d\w*|H[y]nix|Mic[r]on|Tos[h]iba|SanD[i]sk)\b' },
    @{ Name = 'Antivirus'; Path = $false; Pattern = '(?i)\b(Bitdef[e]nder|Kaspers[k]y|Nor[t]on|McA[f]ee|Av[a]st|AVG\s+Anti\w*|ES[E]T|Malwareby[t]es|Soph[o]s|CrowdStr[i]ke|Sentinel[O]ne|Trend\s*Mi[c]ro|Symant[e]c|F-Se[c]ure|Webr[o]ot|Cylan[c]e|Carbon\s*Bl[a]ck)\b' },
    @{ Name = 'Build de Windows'; Path = $false; Pattern = '(?i)\bbuild\s+(19|2\d)\d{3}\b' },
    @{ Name = 'Version de Windows con build'; Path = $false; Pattern = '\b10\.0\.[12]\d{4}\b' },
    @{ Name = 'Version de driver'; Path = $false; Pattern = '(?i)\bdriver\s+(version\s+)?v?\d+(\.\d+)+' }
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
                $pattern = "(?i)(?<![A-Za-z0-9_])$esc(?![A-Za-z0-9_])"
            }
            $localRules += @{ Name = "Valor local de `$env:$($pair[0])"; Path = $false; Pattern = $pattern }
        }
    }
    $deny = Join-Path $commonDir 'privacy-denylist'
    if (Test-Path -LiteralPath $deny -PathType Leaf) {
        $raw = [System.IO.File]::ReadAllBytes($deny)
        foreach ($line in ([System.Text.Encoding]::UTF8.GetString($raw) -split "`n")) {
            $t = $line.Trim()
            if ($t.Length -ge 2 -and -not $t.StartsWith('#')) {
                $localRules += @{ Name = 'Termino de la lista negra local'; Path = $false; Pattern = '(?i)' + [regex]::Escape($t) }
            }
        }
    }
}
$rules = @($generic) + @($localRules)

# --- Decodificacion de contenido ------------------------------------------------------------------------------------
function ConvertTo-ScanText {
    # Devuelve el texto a revisar para unos bytes, o $null si hay que rechazar (se indica en $reject).
    param([byte[]]$Bytes, [ref]$Reject)
    $Reject.Value = $null
    $n = $Bytes.Length
    if ($n -eq 0) { return '' }
    if ($n -ge 3 -and $Bytes[0] -eq 0xEF -and $Bytes[1] -eq 0xBB -and $Bytes[2] -eq 0xBF) { return [System.Text.Encoding]::UTF8.GetString($Bytes, 3, $n - 3) }
    if ($n -ge 2 -and $Bytes[0] -eq 0xFF -and $Bytes[1] -eq 0xFE) { return [System.Text.Encoding]::Unicode.GetString($Bytes, 2, $n - 2) }
    if ($n -ge 2 -and $Bytes[0] -eq 0xFE -and $Bytes[1] -eq 0xFF) { return [System.Text.Encoding]::BigEndianUnicode.GetString($Bytes, 2, $n - 2) }
    $zeros = 0
    $lim = [Math]::Min($n, 4096)
    for ($i = 0; $i -lt $lim; $i++) { if ($Bytes[$i] -eq 0) { $zeros++ } }
    if ($zeros -eq 0) { return [System.Text.Encoding]::UTF8.GetString($Bytes) }
    # Con NUL y sin BOM (UTF-16 sin BOM o binario): se extraen las cadenas ASCII y UTF-16 de ambas alineaciones.
    if ($n -gt 8MB) { $Reject.Value = 'binario de mas de 8 MiB (no se puede revisar)'; return $null }
    $sb = New-Object System.Text.StringBuilder
    $latin = [System.Text.Encoding]::GetEncoding(28591).GetString($Bytes)
    foreach ($m in [regex]::Matches($latin, '[\x20-\x7E]{6,}')) { [void]$sb.AppendLine($m.Value) }
    foreach ($off in 0, 1) {
        $u = [System.Text.Encoding]::Unicode.GetString($Bytes, $off, (($n - $off) -band -2))
        foreach ($m in [regex]::Matches($u, '[\x20-\x7E]{6,}')) { [void]$sb.AppendLine($m.Value) }
    }
    return $sb.ToString()
}

function Test-Rules {
    # Devuelve la lista de (regla, linea) que coinciden en $Text. No devuelve la coincidencia (podria ser el dato).
    param([string]$Text, $RuleSet)
    $found = New-Object System.Collections.Generic.List[object]
    $Text = $Text.Replace("`r", '')
    foreach ($rule in $RuleSet) {
        if ([regex]::IsMatch($Text, $rule.Pattern, 'Multiline')) {
            $lines = $Text -split "`n"
            $any = $false
            for ($i = 0; $i -lt $lines.Count; $i++) {
                if ([regex]::IsMatch($lines[$i], $rule.Pattern)) { $found.Add(@($rule.Name, ($i + 1))); $any = $true }
            }
            if (-not $any) { $found.Add(@($rule.Name, 0)) }
        }
    }
    return ,$found
}

$problems = New-Object System.Collections.Generic.List[string]

# --- Modo mensaje de commit -----------------------------------------------------------------------------------------
if ($MessageFile) {
    $reject = $null
    $text = ConvertTo-ScanText ([System.IO.File]::ReadAllBytes($MessageFile)) ([ref]$reject)
    # Se revisan TODAS las lineas, tambien las que empiezan por `#` (git las conserva con -m y -F). Solo se descarta
    # lo que sigue a la linea de tijera que anade `git commit --verbose`.
    $kept = New-Object System.Collections.Generic.List[string]
    foreach ($line in ($text -split "`n")) {
        if ($line.TrimEnd("`r").StartsWith('# ------------------------ >8')) { break }
        $kept.Add($line)
    }
    $text = $kept -join "`n"
    foreach ($hit in (Test-Rules $text $rules)) { $problems.Add("mensaje de commit:$($hit[1]): $($hit[0])") }
    if ($problems.Count -gt 0) {
        Write-Host "privacy-check: el mensaje de commit contiene datos privados:" -ForegroundColor Red
        $problems | ForEach-Object { Write-Host "  $_" }
        exit 1
    }
    Write-Host 'privacy-check: OK (mensaje de commit)'
    exit 0
}

# --- Archivos a revisar ---------------------------------------------------------------------------------------------
if ($All) {
    $names = Split-Nul (Invoke-GitBytes '-c core.quotePath=false ls-files --cached --others --exclude-standard -z' $repoRoot)
} else {
    $names = Split-Nul (Invoke-GitBytes '-c core.quotePath=false diff --cached --name-only -z --diff-filter=ACMR' $repoRoot)
}
$names = @($names | Sort-Object -Unique)

$pathFree = @($rules | Where-Object { -not $_.Path })
$imageExt = '(?i)\.(png|jpe?g|gif|bmp|webp|tiff?|heic)$'

# --- Excepcion ESTRECHA para las capturas de --demo ------------------------------------------------------------------
# Las imagenes se rechazan en todo el repositorio SALVO `docs/img/<nombre>.png` (minusculas, digitos y guiones) cuando:
#   1. es un PNG de verdad y de como mucho 2 MiB, con SOLO los fragmentos IHDR, PLTE, tRNS, IDAT, IEND, sRGB, gAMA, cHRM y
#      pHYs (nada de tEXt/iTXt/zTXt/eXIf/tIME/iCCP: ahi viajan software, fecha, camara o el perfil de color del monitor);
#   2. `docs/img/README.md` tiene una linea con su nombre Y con `--demo` (la orden que la genera): solo se admiten
#      capturas de la ventana en modo demostracion (arbol sintetico, unidades ficticias);
#   3. pasa ademas el escaneo de cadenas ASCII/UTF-16 como cualquier binario.
# Lo que el codigo no puede comprobar (que los pixeles sean solo de la app) lo revisa quien audita el cambio.
$demoImgPattern = '^docs/img/[a-z0-9-]+\.png$'
$demoImgMaxBytes = 2MB
$pngAllowedChunks = @('IHDR', 'PLTE', 'tRNS', 'IDAT', 'IEND', 'sRGB', 'gAMA', 'cHRM', 'pHYs')

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
    while ($pos -lt $Bytes.Length) {
        if ($sawEnd) { return 'hay datos despues del fragmento IEND' }
        if ($pos + 12 -gt $Bytes.Length) { return 'PNG truncado' }
        $len = ([int]$Bytes[$pos] -shl 24) -bor ([int]$Bytes[$pos + 1] -shl 16) -bor ([int]$Bytes[$pos + 2] -shl 8) -bor [int]$Bytes[$pos + 3]
        if ($len -lt 0 -or ($pos + 12 + [long]$len) -gt $Bytes.Length) { return 'PNG con un fragmento fuera de rango' }
        $type = [System.Text.Encoding]::ASCII.GetString($Bytes, $pos + 4, 4)
        if ($first -and $type -cne 'IHDR') { return 'el primer fragmento del PNG no es IHDR' }
        $first = $false
        if ($pngAllowedChunks -cnotcontains $type) { return "fragmento PNG no permitido ($type): puede llevar metadatos del equipo" }
        if ($type -ceq 'IEND') { $sawEnd = $true }
        $pos += 12 + $len
    }
    if (-not $sawEnd) { return 'PNG sin fragmento IEND' }
    return $null
}

function Get-ImgReadme {
    # Texto de docs/img/README.md (disco con -All; indice en modo staging), o $null si no existe.
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

foreach ($name in $names) {
    $f = $name.Replace('\', '/')
    $inSpike = $f.StartsWith('spike/')
    $ruleSet = if ($inSpike) { $pathFree } else { $rules }

    if ($f -eq 'docs/spike-results.md') {
        $problems.Add("${f}: archivo prohibido (los resultados del spike se rehacen por separado)")
        continue
    }
    # El nombre del archivo tambien se revisa.
    foreach ($hit in (Test-Rules $f $ruleSet)) { $problems.Add("${f}: NOMBRE del archivo: $($hit[0])") }

    $isDemoImg = $false
    if ($f -match $imageExt) {
        if ($f -cnotmatch $demoImgPattern) {
            $problems.Add("${f}: imagen versionada (no se permiten capturas; solo docs/img/<nombre>.png generadas con --demo)")
            continue
        }
        $isDemoImg = $true
    }
    $full = Join-Path $repoRoot $name
    if ($All) {
        if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { continue }
        $bytes = [System.IO.File]::ReadAllBytes($full)
    } else {
        # Contenido del INDICE, leido como bytes (sin recodificar).
        $bytes = Invoke-GitBytes "cat-file blob `":$f`"" $repoRoot
    }
    if ($isDemoImg) {
        $why = Test-DemoPng $bytes
        if ($why) { $problems.Add("${f}: $why"); continue }
        $readme = Get-ImgReadme
        $leaf = $f.Substring($f.LastIndexOf('/') + 1)
        $listed = $false
        if ($readme) {
            foreach ($line in ($readme -split "`n")) {
                if ($line.Contains($leaf) -and $line.Contains('--demo')) { $listed = $true; break }
            }
        }
        if (-not $listed) {
            $problems.Add("${f}: falta una linea en docs/img/README.md con el nombre del archivo y la orden --demo que lo genera")
            continue
        }
    }
    $reject = $null
    $text = ConvertTo-ScanText $bytes ([ref]$reject)
    if ($reject) { $problems.Add("${f}: $reject"); continue }
    foreach ($hit in (Test-Rules $text $ruleSet)) { $problems.Add("${f}:$($hit[1]): $($hit[0])") }
}

if ($problems.Count -gt 0) {
    Write-Host "privacy-check: FALLO ($($problems.Count) coincidencias). Quita esos datos antes de commitear:" -ForegroundColor Red
    $problems | Select-Object -First 50 | ForEach-Object { Write-Host "  $_" }
    if ($problems.Count -gt 50) { Write-Host "  ... y $($problems.Count - 50) mas" }
    exit 1
}

$mode = if ($All) { 'todo el arbol' } else { 'staging' }
$scope = if ($GenericOnly) { 'patrones genericos' } else { 'patrones genericos + valores locales' }
Write-Host "privacy-check: OK ($($names.Count) archivos, $mode, $scope)"
exit 0
