<#
.SYNOPSIS
  Tests for scripts/privacy-check.ps1 using a test repository under %TEMP%\tesela-test-<uuid>.

.DESCRIPTION
  Creates a disposable Git repository, copies the script, and checks that "bad" fixtures fail and "good" fixtures pass
  in staging, -All, -GenericOnly, and -MessageFile modes. Bad fixtures are stored as hexadecimal in `fixtures.json`
  (they contain exactly what the checker rejects, so this directory can be reviewed like the rest of the repository).

.PARAMETER Shell
  Executable used to run privacy-check.ps1: `pwsh` (default) or `powershell` (Windows PowerShell 5.1).

.EXAMPLE
  pwsh -File scripts/tests/privacy-check.tests.ps1
  pwsh -File scripts/tests/privacy-check.tests.ps1 -Shell powershell
#>
[CmdletBinding()]
param([string]$Shell = 'pwsh')

$ErrorActionPreference = 'Stop'
$srcScript = Join-Path (Split-Path -Parent $PSScriptRoot) 'privacy-check.ps1'
if (-not (Test-Path -LiteralPath $srcScript)) { throw "cannot find $srcScript" }

function Decode([string]$Hex) {
    $bytes = New-Object byte[] ($Hex.Length / 2)
    for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = [System.Convert]::ToByte($Hex.Substring(2 * $i, 2), 16) }
    return [System.Text.Encoding]::UTF8.GetString($bytes)
}
$raw = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'fixtures.json') -Raw | ConvertFrom-Json
$bad = @{}
foreach ($p in $raw.bad.PSObject.Properties) { $bad[$p.Name] = Decode $p.Value }
$good = @($raw.good | ForEach-Object { Decode $_ })
$misc = @{}
foreach ($p in $raw.misc.PSObject.Properties) { $misc[$p.Name] = Decode $p.Value }
$opaqueHex = @{}
foreach ($p in $raw.opaque.PSObject.Properties) { $opaqueHex[$p.Name] = $p.Value }
$lookalike = @($raw.'opaque-lookalike' | ForEach-Object { Decode $_ })

# --- No versioned text file contains NUL bytes (except docs/img/*.png screenshots) --------------------------------------
# A stray NUL makes grep and other tools treat a file as binary and hide its contents or diff.
$realRepo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$nulFiles = New-Object System.Collections.Generic.List[string]
if (Test-Path -LiteralPath (Join-Path $realRepo '.git')) {
    $lsPsi = New-Object System.Diagnostics.ProcessStartInfo
    $lsPsi.FileName = 'git'
    $lsPsi.Arguments = '-c core.quotePath=false ls-files --cached -z'
    $lsPsi.WorkingDirectory = $realRepo
    $lsPsi.UseShellExecute = $false
    $lsPsi.RedirectStandardOutput = $true
    $lsPsi.CreateNoWindow = $true
    $lsProc = [System.Diagnostics.Process]::Start($lsPsi)
    $lsMs = New-Object System.IO.MemoryStream
    $lsProc.StandardOutput.BaseStream.CopyTo($lsMs)
    $lsProc.WaitForExit()
    foreach ($name in ([System.Text.Encoding]::UTF8.GetString($lsMs.ToArray()).Split([char]0) | Where-Object { $_ -ne '' })) {
        if ($name -cmatch '^docs/img/[a-z0-9-]+\.png$') { continue }
        $full = Join-Path $realRepo $name
        if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { continue }
        if ([System.Array]::IndexOf([System.IO.File]::ReadAllBytes($full), [byte]0) -ge 0) { $nulFiles.Add($name) }
    }
}

# --- Test repository (only under %TEMP%, with a deletion guard) ---------------------------------------------------------
$temp = [System.IO.Path]::GetTempPath().TrimEnd('\')
$repo = Join-Path $temp ('tesela-test-' + [guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $repo | Out-Null
$scriptPath = Join-Path $repo 'scripts\privacy-check.ps1'
$failures = New-Object System.Collections.Generic.List[string]
$total = 0
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Remove-Repo {
    $leaf = Split-Path -Leaf $repo
    if ((Split-Path -Parent $repo) -eq $temp -and $leaf.StartsWith('tesela-test-')) {
        Remove-Item -LiteralPath $repo -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-Git {
    & git @args 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "git $args fallo" }
}

function Put([string]$Rel, [string]$Text) {
    $p = Join-Path $repo $Rel
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $p) | Out-Null
    [System.IO.File]::WriteAllText($p, $Text, $utf8)
}

function PutBytes([string]$Rel, [byte[]]$Bytes) {
    $p = Join-Path $repo $Rel
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $p) | Out-Null
    [System.IO.File]::WriteAllBytes($p, $Bytes)
}

function Reset-Repo {
    Invoke-Git -C $repo reset -q
    Get-ChildItem -LiteralPath $repo -Force | Where-Object { $_.Name -ne '.git' -and $_.Name -ne 'scripts' } |
        Remove-Item -Recurse -Force
    $deny = Join-Path $repo '.git\privacy-denylist'
    if (Test-Path $deny) { Remove-Item $deny -Force }
}

function Stage-All { Invoke-Git -C $repo add -A -f }

# A minimal PNG (no valid CRCs: the checker only inspects chunk structure, types, and lengths).
function New-Chunk([string]$Type, [byte[]]$Data) {
    $len = $Data.Length
    $b = New-Object System.Collections.Generic.List[byte]
    $b.AddRange([byte[]]((($len -shr 24) -band 255), (($len -shr 16) -band 255), (($len -shr 8) -band 255), ($len -band 255)))
    $b.AddRange([System.Text.Encoding]::ASCII.GetBytes($Type))
    $b.AddRange($Data)
    $b.AddRange([byte[]](0, 0, 0, 0))
    return , $b.ToArray()
}

# A `$Type` chunk with `$N` data bytes (default: four for gAMA...).
function Fill([string]$Type, [int]$N) { return @{ T = $Type; D = (New-Object byte[] $N) } }

# `-Color` is the IHDR color type (6 = RGBA, 3 = indexed); `-Chunks` are extra chunks before IDAT.
function New-Png([object[]]$Chunks = @(), [byte[]]$Idat = [byte[]](1, 2, 3, 4, 5, 6, 7, 8), [int]$Color = 6) {
    $b = New-Object System.Collections.Generic.List[byte]
    $b.AddRange([byte[]](0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A))
    $b.AddRange((New-Chunk 'IHDR' ([byte[]](0, 0, 0, 4, 0, 0, 0, 4, 8, $Color, 0, 0, 0))))
    foreach ($c in $Chunks) { $b.AddRange((New-Chunk $c.T $c.D)) }
    $b.AddRange((New-Chunk 'IDAT' $Idat))
    $b.AddRange((New-Chunk 'IEND' ([byte[]]@())))
    return , $b.ToArray()
}

function Sha256Hex([byte[]]$Bytes) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try { return ([System.BitConverter]::ToString($sha.ComputeHash($Bytes))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose() }
}

# docs/img README entry with the image bytes' SHA-256.
function ReadmeLine([string]$Leaf, [byte[]]$Bytes, [string]$Tail = 'tesela --demo --screen map') {
    return ('- ' + $Leaf + ' | sha256: ' + (Sha256Hex $Bytes) + ' | ' + $Tail + "`n")
}

function HexBytes([string]$Hex) {
    $bytes = New-Object byte[] ($Hex.Length / 2)
    for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = [System.Convert]::ToByte($Hex.Substring(2 * $i, 2), 16) }
    return , $bytes
}

function Run-Check([string[]]$CheckArgs) {
    Push-Location $repo
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'   # en 5.1 el stderr de un nativo con 2>&1 seria un error terminante
    try {
        $out = & $Shell -NoProfile -File $scriptPath @CheckArgs 2>&1
        return @{ Code = $LASTEXITCODE; Out = ($out | Out-String) }
    } finally {
        $ErrorActionPreference = $prevEap
        Pop-Location
    }
}

function Expect([string]$Case, [bool]$ShouldFail, [string[]]$CheckArgs = @()) {
    $script:total++
    $r = Run-Check $CheckArgs
    $failed = ($r.Code -ne 0)
    if ($failed -ne $ShouldFail) {
        $want = if ($ShouldFail) { 'FAILED' } else { 'OK' }
        $script:failures.Add("$Case (esperado: $want, codigo $($r.Code)) $($r.Out)")
    }
}

# In GitHub Actions, CI=true makes the script use generic mode and local-value cases would fail. Tests set their own
# environment and restore it on exit.
$oldCI = $env:CI
$env:CI = $null

try {
    Invoke-Git -C $repo init -q
    Invoke-Git -C $repo config user.email test@example.invalid
    Invoke-Git -C $repo config user.name test
    Invoke-Git -C $repo config core.autocrlf false
    New-Item -ItemType Directory -Path (Split-Path -Parent $scriptPath) | Out-Null
    Copy-Item -LiteralPath $srcScript -Destination $scriptPath
    Invoke-Git -C $repo add -A
    Invoke-Git -C $repo commit -q -m init

    # --- Bad fixtures --------------------------------------------------------------------------------------------------
    foreach ($k in ($bad.Keys | Sort-Object)) {
        Reset-Repo
        Put 'notas.txt' ("linea limpia`n" + $bad[$k] + "`notra linea limpia`n")
        Stage-All
        Expect "malo (staging): $k" $true
        Expect "malo (-All): $k" $true @('-All')
    }

    # --- Good fixtures -------------------------------------------------------------------------------------------------
    $i = 0
    foreach ($g in $good) {
        Reset-Repo
        $i++
        Put 'ok.txt' ($g + "`n")
        Stage-All
        Expect "bueno (staging) #${i}: $g" $false
    }

    # --- Encodings and binaries -----------------------------------------------------------------------------------------
    $sid = $misc['sid']
    Reset-Repo
    PutBytes 'u16bom.txt' ([System.Text.Encoding]::Unicode.GetPreamble() + [System.Text.Encoding]::Unicode.GetBytes("hola`n$sid`n"))
    Stage-All
    Expect 'UTF-16 con BOM con SID' $true
    Reset-Repo
    PutBytes 'u16.txt' ([System.Text.Encoding]::Unicode.GetBytes("hola`n$sid`n"))
    Stage-All
    Expect 'UTF-16 sin BOM con SID' $true
    Reset-Repo
    PutBytes 'u16ok.txt' ([System.Text.Encoding]::Unicode.GetPreamble() + [System.Text.Encoding]::Unicode.GetBytes("todo limpio`n"))
    Stage-All
    Expect 'UTF-16 limpio' $false
    Reset-Repo
    PutBytes 'u8bom.txt' ([byte[]](0xEF, 0xBB, 0xBF) + $utf8.GetBytes("hola $sid`n"))
    Stage-All
    Expect 'UTF-8 con BOM con SID' $true

    $profilePath = $misc['profile-path']
    Reset-Repo
    PutBytes 'a.bin' ([byte[]](0, 1, 2, 0, 255) + [System.Text.Encoding]::ASCII.GetBytes($profilePath) + [byte[]](0, 0, 9))
    Stage-All
    Expect 'binario con cadena ASCII de perfil' $true
    Reset-Repo
    PutBytes 'b.bin' ([byte[]](0, 1, 2, 0, 255) + [System.Text.Encoding]::Unicode.GetBytes($profilePath) + [byte[]](0, 0, 9))
    Stage-All
    Expect 'binario con cadena UTF-16 de perfil' $true
    # A UTF-16 hardware-vendor string (generic rule, including -GenericOnly/CI) with a neighbor that reads as a letter in
    # UTF-16: U+4E2D before it, or two aligned ASCII letters ('ab' = U+6261). The neighbor must not hide the match.
    $gpuU16 = [System.Text.Encoding]::Unicode.GetBytes($misc['gpu-word'])
    foreach ($case in @(
            @('precedido de U+4E2D', [System.Text.Encoding]::Unicode.GetBytes([string][char]0x4E2D)),
            @('precedido de dos letras ASCII alineadas', [System.Text.Encoding]::ASCII.GetBytes('ab')),
            @('precedido de U+4E2D y seguido de U+6587', [System.Text.Encoding]::Unicode.GetBytes([string][char]0x4E2D))
        )) {
        $tail = if ($case[0] -like '*seguido*') { [System.Text.Encoding]::Unicode.GetBytes([string][char]0x6587) } else { [byte[]]@() }
        Reset-Repo
        PutBytes 'g.bin' ([byte[]](0, 1, 2, 0, 255, 7, 0) + $case[1] + $gpuU16 + $tail + [byte[]](0, 0, 9))
        Stage-All
        Expect "binario con cadena UTF-16 de hardware $($case[0])" $true
        Expect "binario con cadena UTF-16 de hardware $($case[0]) con -GenericOnly" $true @('-GenericOnly')
    }
    Reset-Repo
    PutBytes 'c.bin' ([byte[]](0, 1, 2, 0, 255, 7, 8, 9, 0, 0, 1))
    Stage-All
    Expect 'binario limpio' $false
    Reset-Repo
    PutBytes 'captura.png' ([byte[]](0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0))
    Stage-All
    Expect 'imagen (captura)' $true

    # --- docs/img/ exception (--demo screenshots) ----------------------------------------------------------------------
    $img = 'docs/img/mapa-demo.png'
    $imgLeaf = 'mapa-demo.png'
    $png = New-Png
    $readmeOk = "# Capturas`n`n" + (ReadmeLine $imgLeaf $png)
    Reset-Repo
    PutBytes $img $png
    Put 'docs/img/README.md' $readmeOk
    Stage-All
    Expect 'docs/img: PNG valido con su linea anclada (nombre + sha256 + --demo)' $false
    Expect 'docs/img: PNG valido con su linea anclada (-All)' $false @('-All')
    Expect 'docs/img: PNG valido con -GenericOnly' $false @('-GenericOnly')
    Reset-Repo
    PutBytes $img $png
    Stage-All
    Expect 'docs/img: sin README.md' $true
    # The README entry must be anchored and contain the exact hash.
    foreach ($case in @(
            @('sin --demo', ('- ' + $imgLeaf + ' | sha256: ' + (Sha256Hex $png) + " | captura de mi escritorio`n")),
            @('hash de otra imagen', ('- ' + $imgLeaf + ' | sha256: ' + (Sha256Hex ([byte[]](1, 2, 3))) + " | tesela --demo`n")),
            @('sin hash', ('- ' + $imgLeaf + ": tesela --demo`n")),
            @('hash truncado', ('- ' + $imgLeaf + ' | sha256: ' + (Sha256Hex $png).Substring(0, 60) + " | tesela --demo`n")),
            @('otro archivo', ('- otra.png | sha256: ' + (Sha256Hex $png) + " | tesela --demo`n")),
            @('nombre como sufijo (xmapa-demo.png)', ('- x' + $imgLeaf + ' | sha256: ' + (Sha256Hex $png) + " | tesela --demo`n")),
            @('linea dentro de otra (no empieza por guion)', ('texto - ' + $imgLeaf + ' | sha256: ' + (Sha256Hex $png) + " | tesela --demo`n"))
        )) {
        Reset-Repo
        PutBytes $img $png
        Put 'docs/img/README.md' ("# Capturas`n`n" + $case[1])
        Stage-All
        Expect ('docs/img: README.md invalido: ' + $case[0]) $true
    }
    # A name that is a SUBSTRING of another registered image name is not valid (`o.png` versus `mapa-demo.png`).
    Reset-Repo
    PutBytes 'docs/img/o.png' $png
    Put 'docs/img/README.md' $readmeOk
    Stage-All
    Expect 'docs/img: nombre que es subcadena de otro registrado (o.png)' $true
    # Changing one image byte invalidates it until it is registered again.
    Reset-Repo
    PutBytes $img (New-Png -Idat ([byte[]](1, 2, 3, 4, 5, 6, 7, 9)))
    Put 'docs/img/README.md' $readmeOk
    Stage-All
    Expect 'docs/img: imagen distinta de la registrada (un byte)' $true

    # --- PNG chunks: closed list and fixed lengths ----------------------------------------------------------------------
    foreach ($t in 'tEXt', 'iTXt', 'zTXt', 'eXIf', 'tIME', 'iCCP', 'sBIT', 'bKGD', 'hIST', 'sPLT', 'zzZz') {
        $p = New-Png -Chunks @((Fill $t 4))
        Reset-Repo
        PutBytes $img $p
        Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $imgLeaf $p))
        Stage-All
        Expect "docs/img: fragmento PNG no permitido $t" $true
    }
    foreach ($c in @(@('sRGB', 1), @('gAMA', 4), @('cHRM', 32), @('pHYs', 9))) {
        $p = New-Png -Chunks @((Fill $c[0] $c[1]))
        Reset-Repo
        PutBytes $img $p
        Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $imgLeaf $p))
        Stage-All
        Expect ("docs/img: fragmento PNG permitido " + $c[0] + ' con su longitud') $false
        # ONE extra byte (plain text hidden in an allowed chunk) must fail.
        $p = New-Png -Chunks @((Fill $c[0] ($c[1] + 1)))
        Reset-Repo
        PutBytes $img $p
        Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $imgLeaf $p))
        Stage-All
        Expect ("docs/img: " + $c[0] + ' con longitud de mas') $true
    }
    $hidden = [System.Text.Encoding]::ASCII.GetBytes($profilePath + ' notas privadas ' + $profilePath)
    $p = New-Png -Chunks @(@{ T = 'gAMA'; D = $hidden })
    Reset-Repo
    PutBytes $img $p
    Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $imgLeaf $p))
    Stage-All
    Expect 'docs/img: texto en claro dentro de gAMA' $true
    # PLTE and tRNS are allowed only in indexed PNGs and have maximum lengths.
    foreach ($case in @(
            @('PLTE en RGBA', 6, (Fill 'PLTE' 12), $true),
            @('tRNS en RGBA', 6, (Fill 'tRNS' 2), $true),
            @('PLTE indexado 768', 3, (Fill 'PLTE' 768), $false),
            @('PLTE indexado 771 (de mas)', 3, (Fill 'PLTE' 771), $true),
            @('PLTE indexado no multiplo de 3', 3, (Fill 'PLTE' 100), $true),
            @('tRNS indexado 256', 3, (Fill 'tRNS' 256), $false),
            @('tRNS indexado 257 (de mas)', 3, (Fill 'tRNS' 257), $true)
        )) {
        $chunks = @($case[2])
        if ($case[0] -like 'tRNS indexado*') { $chunks = @((Fill 'PLTE' 6), $case[2]) }
        $p = New-Png -Chunks $chunks -Color $case[1]
        Reset-Repo
        PutBytes $img $p
        Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $imgLeaf $p))
        Stage-All
        Expect ('docs/img: ' + $case[0]) $case[3]
    }
    $p = New-Png -Chunks @((Fill 'IHDR' 13))
    Reset-Repo
    PutBytes $img $p
    Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $imgLeaf $p))
    Stage-All
    Expect 'docs/img: IHDR repetido' $true

    # --- Disallowed paths ------------------------------------------------------------------------------------------------
    foreach ($badPath in 'docs/img/Mapa.png', 'docs/img/sub/mapa.png', 'docs/mapa.png', 'img/mapa.png', 'docs/img/mapa.jpg', 'docs/img/mapa_demo.png', 'docs/img/mapa.PNG') {
        Reset-Repo
        PutBytes $badPath $png
        $leafBad = $badPath.Substring($badPath.LastIndexOf('/') + 1)
        Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $leafBad $png))
        Stage-All
        Expect "docs/img: ruta no permitida $badPath" $true
    }

    # --- Disguised images: detect by SIGNATURE, regardless of extension ------------------------------------------------
    $magic = @{}
    foreach ($pr in $raw.magic.PSObject.Properties) { $magic[$pr.Name] = HexBytes $pr.Value }
    foreach ($k in ($magic.Keys | Sort-Object)) {
        foreach ($ext in 'dat', 'bin', 'txt', 'md', 'json', 'rs') {
            Reset-Repo
            PutBytes "docs/notas.$ext" ($magic[$k] + [byte[]](0, 0, 0, 0))
            Stage-All
            Expect "imagen $k disfrazada de .$ext" $true
        }
        # Also under docs/img/ with a non-.png name.
        Reset-Repo
        PutBytes 'docs/img/captura.dat' $magic[$k]
        Stage-All
        Expect "imagen $k en docs/img/captura.dat" $true
    }
    foreach ($ext in 'dat', 'bin', 'txt') {
        Reset-Repo
        PutBytes "docs/notas.$ext" $png
        Stage-All
        Expect "PNG disfrazado de .$ext" $true
        Expect "PNG disfrazado de .$ext (-All)" $true @('-All')
    }
    # A JPEG named .png under docs/img/ is not a PNG.
    Reset-Repo
    PutBytes $img ($magic['jpeg'] + (New-Object byte[] 40))
    Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $imgLeaf ($magic['jpeg'] + (New-Object byte[] 40))))
    Stage-All
    Expect 'docs/img: un JPEG con extension .png' $true
    # Text that only STARTS with an image signature is not an image.
    foreach ($txt in 'BMW y GIF89 y RIFF notas', 'GIF', 'II*', 'RIFF WEBP', 'ftyp avif', 'PNG sin firma', 'BM') {
        Reset-Repo
        Put 'docs/notas.txt' ($txt + "`n")
        Stage-All
        Expect "texto que se parece a una firma: $txt" $false
    }
    Reset-Repo
    PutBytes 'docs/img/mapa-demo.png' $png
    Put 'docs/img/README.md' $readmeOk
    Stage-All
    Invoke-Git -C $repo commit -q -m imagen
    Expect 'docs/img: -All sobre lo ya commiteado' $false @('-All')
    # Truncation, data after IEND, non-PNG, oversized files, and compressed contents are NOT scanned as text.
    foreach ($case in @(
            @('datos despues de IEND', ($png + [byte[]](1, 2, 3)), $true),
            @('PNG truncado', ([byte[]](0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0)), $true),
            @('mas de 2 MiB', (New-Png -Idat (New-Object byte[] (2MB + 10))), $true),
            @('contenido comprimido con una cadena de perfil (no se escanea: sin falsos positivos)', (New-Png -Idat ([byte[]](0, 0) + [System.Text.Encoding]::ASCII.GetBytes($profilePath) + [byte[]](0, 0))), $false)
        )) {
        Reset-Repo
        PutBytes $img $case[1]
        Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $imgLeaf $case[1]))
        Stage-All
        Expect ('docs/img: ' + $case[0]) $case[2]
    }

    # --- UNC paths with normal slashes: URL WITHOUT a protocol is a DOCUMENTED false positive; with a protocol it is not --
    foreach ($case in @(
            @('URL sin protocolo (barras, host y ruta) cuenta como ruta de red (falso positivo documentado)', ('ver /' + '/cdn.ejemplo.invalid/lib.js'), $true),
            @('la misma URL con protocolo no cuenta', 'ver https://cdn.ejemplo.invalid/lib.js', $false),
            @('comentario con espacio no cuenta', 'x = 1; // lo que sea/otra cosa', $false),
            @('el marcador <servidor> con barras normales no cuenta', 'ver //<servidor>/<recurso>', $false)
        )) {
        Reset-Repo
        Put 'u.txt' ($case[1] + "`n")
        Stage-All
        Expect ('UNC: ' + $case[0]) $case[2]
    }
    foreach ($k in 'unc-barras-normales', 'unc-verbatim-barras-normales', 'unc-barras-mezcladas-a', 'unc-escapada-con-barra-normal', 'unc-verbatim-escapada') {
        Reset-Repo
        Put 'u.txt' ($bad[$k] + "`n")
        Stage-All
        Expect "UNC (-GenericOnly): $k" $true @('-GenericOnly')
        $msgU = Join-Path $repo 'MSGU.txt'
        [System.IO.File]::WriteAllText($msgU, ('feat: ' + $bad[$k] + "`n"), $utf8)
        Expect "UNC (mensaje de commit): $k" $true @('-MessageFile', $msgU)
        Remove-Item -LiteralPath $msgU -Force
    }

    # --- File names -----------------------------------------------------------------------------------------------------
    Reset-Repo
    Put $misc['name-hw'] "limpio`n"
    Stage-All
    Expect 'nombre de archivo con hardware' $true
    Reset-Repo
    Put $misc['name-sid'] "limpio`n"
    Stage-All
    Expect 'nombre de directorio con SID' $true
    $nonAscii = 'ca' + [char]0xF1 + 'on-' + [char]0xE9 + 'xito.txt'
    Reset-Repo
    Put $nonAscii ("hola $sid`n")
    Stage-All
    Expect 'nombre no ASCII con contenido malo' $true
    Reset-Repo
    Put $nonAscii "limpio`n"
    Stage-All
    Expect 'nombre no ASCII limpio' $false

    # --- spike/ y docs/spike-results.md -----------------------------------------------------------------------------------
    # PATH rules also apply under spike/ (previously skipped): drive, profile, WSL, and UNC patterns in staging and -All.
    Reset-Repo
    Put 'spike/notas.txt' ($misc['spike-paths'] + "`n")
    Stage-All
    Expect 'spike/: las reglas de rutas SI se aplican (perfil y unidad)' $true
    Expect 'spike/: las reglas de rutas SI se aplican (-All)' $true @('-All')
    foreach ($k in 'perfil-windows', 'perfil-barras', 'perfil-unix', 'perfil-wsl', 'unidad-e', 'unidad-wsl', 'unc-clasica', 'unc-barras-normales') {
        Reset-Repo
        Put 'spike/notas.txt' ($bad[$k] + "`n")
        Stage-All
        Expect "spike/: $k falla" $true
    }
    foreach ($placeholder in '<user>', '<usuario>') {
        Reset-Repo
        $unixPath = '/mnt/c/' + 'Users/' + $placeholder + '/x'
        Put 'spike/notas.txt' ("paths C:\Users\$placeholder\x and $unixPath; system paths C:\Windows\Temp and %USERPROFILE%\x; https://example.invalid/x`n")
        Stage-All
        Expect "spike/: placeholder $placeholder and system paths pass" $false
    }
    # CLOSED list of two phase-0 files with synthetic paths: pass ONLY with the exact reviewed contents.
    foreach ($rel in 'spike/scripts/redact.py', 'spike/trash-bench/src/main.rs') {
        $real = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) $rel
        if (-not (Test-Path -LiteralPath $real)) { continue }
        $realBytes = [System.IO.File]::ReadAllBytes($real)
        Reset-Repo
        PutBytes $rel $realBytes
        Stage-All
        Expect "spike/: $rel con el contenido revisado pasa (lista cerrada)" $false
        Expect "spike/: $rel con el contenido revisado pasa en -All" $false @('-All')
        Reset-Repo
        PutBytes $rel ($realBytes + [byte[]](0x0A))
        Stage-All
        Expect "spike/: $rel con UN byte de mas vuelve a las reglas completas" $true
        # Same contents under another name: the exception is tied to BOTH path and hash.
        Reset-Repo
        PutBytes 'spike/otro.txt' $realBytes
        Stage-All
        Expect "spike/: el contenido de $rel en otro archivo falla" $true
    }
    Reset-Repo
    Put 'spike/notas.txt' ('tarjeta ' + $misc['gpu-word'] + "`n")
    Stage-All
    Expect 'spike/: hardware si falla' $true
    Reset-Repo
    Put 'spike/notas.txt' ("cuenta $sid`n")
    Stage-All
    Expect 'spike/: SID si falla' $true
    Reset-Repo
    Put 'docs/spike-results.md' "limpio`n"
    Stage-All
    Expect 'docs/spike-results.md ya no esta exento (existir es un fallo)' $true
    Expect 'docs/spike-results.md en -All' $true @('-All')

    # --- Valores locales y lista negra -------------------------------------------------------------------------------------
    if ($env:USERNAME -and $env:USERNAME.Length -ge 3) {
        Reset-Repo
        Put 'u.txt' ('hola ' + $env:USERNAME + " adios`n")
        Stage-All
        Expect 'valor local USERNAME (local)' $true
        Expect 'valor local USERNAME con -GenericOnly' $false @('-GenericOnly')
    }
    if ($env:COMPUTERNAME -and $env:COMPUTERNAME.Length -ge 3) {
        Reset-Repo
        Put 'u.txt' ('equipo ' + $env:COMPUTERNAME + "`n")
        Stage-All
        Expect 'valor local COMPUTERNAME (local)' $true
    }
    Reset-Repo
    Put 'd.txt' ('contiene ' + $misc['denylist-term'].ToUpper() + " en mayusculas`n")
    Stage-All
    Expect 'sin lista negra no falla' $false
    [System.IO.File]::WriteAllText((Join-Path $repo '.git\privacy-denylist'), ("# comentario`n`n" + $misc['denylist-term'] + "`n"), $utf8)
    Expect 'termino de la lista negra (insensible a mayusculas)' $true
    Expect 'la lista negra no se usa con -GenericOnly' $false @('-GenericOnly')

    # Staging TYPE change: a file changed to a symbolic link (its target is its contents) must be checked; previously it
    # was reported as "0 files, OK".
    Reset-Repo
    Put 'enlace.txt' "texto limpio`n"
    Stage-All
    Invoke-Git -C $repo commit -q -m 'archivo normal'
    $symOid = ((("cuenta $sid`n") | & git -C $repo hash-object -w --stdin) | Out-String).Trim()
    Invoke-Git -C $repo update-index --cacheinfo "120000,$symOid,enlace.txt"
    Expect 'staging: un archivo que pasa a enlace simbolico con el SID en su destino (cambio de tipo)' $true
    $symClean = ((("destino limpio`n") | & git -C $repo hash-object -w --stdin) | Out-String).Trim()
    Invoke-Git -C $repo update-index --cacheinfo "120000,$symClean,enlace.txt"
    Expect 'staging: un enlace simbolico con un destino limpio pasa' $false
    Invoke-Git -C $repo reset -q
    Invoke-Git -C $repo checkout -q -- .

    # --- The index contents are authoritative ---------------------------------------------------------------------------
    Reset-Repo
    Put 'x.txt' "limpio`n"
    Stage-All
    Put 'x.txt' ("sucio $sid`n")   # sin hacer `git add`
    Expect 'staging limpio aunque el archivo de trabajo este sucio' $false
    Reset-Repo
    Put 'x.txt' ("sucio $sid`n")
    Stage-All
    Put 'x.txt' "limpio`n"          # arreglado sin `git add`
    Expect 'staging sucio aunque el archivo de trabajo ya este limpio' $true

    # --- Commit messages -----------------------------------------------------------------------------------------------
    $msg = Join-Path $repo 'MSG.txt'
    [System.IO.File]::WriteAllText($msg, "feat: algo limpio`n`n# Please enter the commit message`n# On branch x`n", $utf8)
    Expect 'mensaje limpio con lineas de comentario limpias' $false @('-MessageFile', $msg)
    [System.IO.File]::WriteAllText($msg, "feat: limpio`n`n# $sid`n", $utf8)
    Expect 'SID en una linea que empieza por # (git la conserva con -m y -F)' $true @('-MessageFile', $msg)
    [System.IO.File]::WriteAllText($msg, "feat: limpio`n# $sid`n# ------------------------ >8`nlimpio`n", $utf8)
    Expect 'comentario con SID ANTES de la linea de tijera' $true @('-MessageFile', $msg)
    [System.IO.File]::WriteAllText($msg, "feat: limpio`n`n# ------------------------ >8`n# $sid`ndiff --git a/x b/x`n+$sid`n", $utf8)
    Expect 'lo que sigue a la linea de tijera (diff de commit --verbose) SE REVISA tambien' $true @('-MessageFile', $msg)
    [System.IO.File]::WriteAllText($msg, "feat: limpio`n`n# ------------------------ >8`n# Do not modify or remove the line above.`ndiff --git a/x b/x`n+limpio`n", $utf8)
    Expect 'tijera con un diff limpio pasa' $false @('-MessageFile', $msg)
    [System.IO.File]::WriteAllText($msg, ('feat: probado en ' + $misc['gpu-word'] + "`n"), $utf8)
    Expect 'mensaje con hardware' $true @('-MessageFile', $msg)
    if ($env:USERNAME -and $env:USERNAME.Length -ge 3) {
        [System.IO.File]::WriteAllText($msg, ('fix: caso de ' + $env:USERNAME + "`n"), $utf8)
        Expect 'mensaje con el usuario local' $true @('-MessageFile', $msg)
        Expect 'mensaje con el usuario local con -GenericOnly' $false @('-GenericOnly', '-MessageFile', $msg)
    }
    [System.IO.File]::WriteAllText($msg, "fix: $sid`n", $utf8)
    Expect 'mensaje con SID' $true @('-MessageFile', $msg)
    Remove-Item $msg -Force

    # --- Opaque or compressed formats: reject by signature, regardless of extension ------------------------------------
    foreach ($k in ($opaqueHex.Keys | Sort-Object)) {
        Reset-Repo
        PutBytes 'datos/archivo.dat' ((HexBytes $opaqueHex[$k]) + (New-Object byte[] 64))
        Stage-All
        Expect "formato opaco $k (.dat)" $true
    }
    # Also with other extensions, under -All and inside spike/ (one format is enough: the rule uses signatures).
    foreach ($ext in 'txt', 'json', 'rs', 'docx') {
        Reset-Repo
        PutBytes "datos/archivo.$ext" ((HexBytes $opaqueHex['zip']) + (New-Object byte[] 64))
        Stage-All
        Expect "formato opaco zip disfrazado de .$ext" $true
    }
    Expect 'formato opaco zip (-All)' $true @('-All')
    Reset-Repo
    PutBytes 'spike/archivo.dat' ((HexBytes $opaqueHex['gzip']) + (New-Object byte[] 64))
    Stage-All
    Expect 'formato opaco gzip dentro de spike/' $true
    $j = 0
    foreach ($t in $lookalike) {
        $j++
        Reset-Repo
        Put 'docs/notas.txt' ($t + "`n")
        Stage-All
        Expect "texto que solo se parece a una firma opaca #$j" $false
    }
    # Real ZIP and gzip files whose compressed contents contain a SID: compressed text is hidden, but the signature is visible.
    Add-Type -AssemblyName System.IO.Compression
    $secret = "cuenta $sid`n"
    $ms = New-Object System.IO.MemoryStream
    $zip = New-Object System.IO.Compression.ZipArchive($ms, [System.IO.Compression.ZipArchiveMode]::Create, $true)
    $entry = $zip.CreateEntry('notas.txt')
    $es = $entry.Open()
    $sw = New-Object System.IO.StreamWriter($es, $utf8)
    $sw.Write($secret * 20)
    $sw.Dispose()
    $zip.Dispose()
    $zipBytes = $ms.ToArray()
    Reset-Repo
    PutBytes 'datos.bin' $zipBytes
    Stage-All
    Expect 'ZIP real con texto comprimido que contiene un SID (.bin)' $true
    Reset-Repo
    PutBytes 'informe.docx' $zipBytes
    Stage-All
    Expect 'ZIP real con texto comprimido que contiene un SID (.docx)' $true
    Reset-Repo
    PutBytes 'informe.docx' $zipBytes
    Stage-All
    Expect 'ZIP real con texto comprimido que contiene un SID (-All)' $true @('-All')
    $gms = New-Object System.IO.MemoryStream
    $gz = New-Object System.IO.Compression.GZipStream($gms, [System.IO.Compression.CompressionMode]::Compress, $true)
    $gb = $utf8.GetBytes($secret * 20)
    $gz.Write($gb, 0, $gb.Length)
    $gz.Dispose()
    Reset-Repo
    PutBytes 'datos.txt' $gms.ToArray()
    Stage-All
    Expect 'gzip real con texto comprimido que contiene un SID' $true
    # Detect ZIP by its END record: prepended data (a self-extracting archive) or a changed local header may not start with `PK 03 04`.
    Reset-Repo
    PutBytes 'datos.bin' ($utf8.GetBytes(('x' * 3000) + "`n") + $zipBytes)
    Stage-All
    Expect 'ZIP con datos delante (se reconoce por su registro de fin)' $true
    $brokenZip = [byte[]]$zipBytes.Clone()
    $brokenZip[2] = 0x99
    Reset-Repo
    PutBytes 'datos.bin' $brokenZip
    Stage-All
    Expect 'ZIP con la cabecera local alterada (registro de fin intacto)' $true
    # Ordinary text that only mentions "PK" is not a ZIP.
    Reset-Repo
    Put 'notas.txt' ("PK 05 06 y PK\n en un texto`n" + ('limpio ' * 50) + "`n")
    Stage-All
    Expect 'texto que menciona PK sin ser un ZIP' $false

    # --- Encodings: UTF-32 with BOM and UTF-16 without BOM after long text -----------------------------------------------
    $secretLine = "cuenta $sid`n"
    $utf32le = New-Object System.Text.UTF32Encoding($false, $true)
    $utf32be = New-Object System.Text.UTF32Encoding($true, $true)
    Reset-Repo
    PutBytes 'u32le.txt' ($utf32le.GetPreamble() + $utf32le.GetBytes($secretLine))
    Stage-All
    Expect 'UTF-32 LE con BOM con SID' $true
    Reset-Repo
    PutBytes 'u32be.txt' ($utf32be.GetPreamble() + $utf32be.GetBytes($secretLine))
    Stage-All
    Expect 'UTF-32 BE con BOM con SID' $true
    Reset-Repo
    PutBytes 'u32ok.txt' ($utf32le.GetPreamble() + $utf32le.GetBytes("todo limpio`n"))
    Stage-All
    Expect 'UTF-32 con BOM limpio' $false
    # More than 4096 bytes of ASCII followed by UTF-16 without a BOM: previously only the first 4 KiB were checked.
    $longAscii = $utf8.GetBytes(('a' * 6000) + "`n")
    Reset-Repo
    PutBytes 'largo.txt' ($longAscii + [System.Text.Encoding]::Unicode.GetBytes($secretLine))
    Stage-All
    Expect 'UTF-16 sin BOM tras 6000 bytes de ASCII con SID' $true
    Reset-Repo
    PutBytes 'largo.txt' ($longAscii + [System.Text.Encoding]::Unicode.GetBytes("todo limpio`n"))
    Stage-All
    Expect 'UTF-16 sin BOM tras 6000 bytes de ASCII, limpio' $false
    # A stray NUL must not hide a UTF-8 SID (with accented characters) in the same file either.
    Reset-Repo
    PutBytes 'mixto.txt' ($utf8.GetBytes("canci" + [char]0xF3 + "n $sid`n") + [byte[]](0, 1, 2) + $utf8.GetBytes("fin`n"))
    Stage-All
    Expect 'NUL suelto con SID en UTF-8 del mismo archivo' $true
    # --- Values split by stray NUL bytes: also search text with NULs removed --------------------------------------------
    function Join-Nul([string[]]$Pieces) {
        # UTF-8 pieces joined by a stray NUL: no decoder sees them together.
        $b = New-Object System.Collections.Generic.List[byte]
        for ($k = 0; $k -lt $Pieces.Count; $k++) {
            if ($k -gt 0) { $b.Add(0) }
            $b.AddRange($utf8.GetBytes($Pieces[$k]))
        }
        return , $b.ToArray()
    }
    $sidPieces = @($sid.Substring(0, 3), $sid.Substring(3, 4), $sid.Substring(7, 6), $sid.Substring(13))
    $msg = Join-Path $repo 'MSG.txt'
    Reset-Repo
    PutBytes 'nulos.txt' (Join-Nul (@('cuenta ') + $sidPieces + @("`n")))
    Stage-All
    Expect 'SID partido por NUL sueltos (UTF-8)' $true
    Expect 'SID partido por NUL sueltos (UTF-8, -All)' $true @('-All')
    Reset-Repo
    PutBytes 'nulos.txt' ([byte[]](0xEF, 0xBB, 0xBF) + (Join-Nul (@('cuenta ') + $sidPieces + @("`n"))))
    Stage-All
    Expect 'SID partido por NUL sueltos (UTF-8 con BOM)' $true
    Reset-Repo
    PutBytes 'nulos.bin' ([byte[]](0, 1, 2, 0, 255, 7) + (Join-Nul (@('cuenta ') + $sidPieces)) + [byte[]](0, 0, 9))
    Stage-All
    Expect 'SID partido por NUL sueltos dentro de un binario' $true
    Reset-Repo
    PutBytes 'nulos.txt' (Join-Nul @('ruta C:\Us', 'ers\bob\x', "`n"))
    Stage-All
    Expect 'ruta de perfil partida por NUL sueltos' $true
    Reset-Repo
    PutBytes 'nulos.txt' ([System.Text.Encoding]::Unicode.GetPreamble() + [System.Text.Encoding]::Unicode.GetBytes("cuenta " + $sid.Insert(9, [string][char]0) + "`n"))
    Stage-All
    Expect 'SID partido por un NUL en UTF-16 con BOM' $true
    Reset-Repo
    PutBytes 'nulos.txt' ([System.Text.Encoding]::Unicode.GetBytes("cuenta " + $sid.Insert(9, [string][char]0) + "`n"))
    Stage-All
    Expect 'SID partido por un NUL en UTF-16 sin BOM' $true
    Reset-Repo
    PutBytes 'nulos.txt' (Join-Nul @('nada que ver ', 'con la ', 'maquina', "`n"))
    Stage-All
    Expect 'texto limpio partido por NUL sueltos' $false
    Reset-Repo
    PutBytes 'nulos.bin' ([byte[]](0, 1, 2, 0, 255, 7) + (Join-Nul @('algo ', 'inocuo ', 'de ', 'relleno')) + [byte[]](0, 0, 9))
    Stage-All
    Expect 'binario limpio con NUL sueltos' $false
    # A large binary with many NULs is still scanned in reasonable time (the NUL-free text is an additional view).
    $big = New-Object byte[] (3MB)
    for ($k = 0; $k -lt $big.Length; $k += 3) { $big[$k] = 0x41 }
    Reset-Repo
    PutBytes 'grande.bin' $big
    Stage-All
    $swBig = [System.Diagnostics.Stopwatch]::StartNew()
    Expect 'binario de 3 MiB con muchos NUL' $false
    $script:total++
    if ($swBig.Elapsed.TotalSeconds -gt 60) { $script:failures.Add("binario de 3 MiB con muchos NUL: tardo $([int]$swBig.Elapsed.TotalSeconds) s") }
    # Commit messages too.
    [System.IO.File]::WriteAllBytes($msg, (Join-Nul (@('fix: ') + $sidPieces + @("`n"))))
    Expect 'mensaje con un SID partido por NUL sueltos' $true @('-MessageFile', $msg)
    [System.IO.File]::WriteAllBytes($msg, (Join-Nul @('fix: ', 'limpio', "`n")))
    Expect 'mensaje limpio partido por NUL sueltos' $false @('-MessageFile', $msg)
    Remove-Item -LiteralPath $msg -Force
    # --- GM-M8: case-insensitive SID, size limit, cumulative BOM handling, UTF-8 without NULs, \\.\UNC\ ------------------
    Reset-Repo
    Put 'minusculas.txt' ("cuenta " + $sid.ToLowerInvariant() + "`n")
    Stage-All
    Expect 'SID en minusculas' $true
    # The 8 MiB limit applies to ALL decoding paths, including a BOM prefix or no NULs (previously only binary decoding used it).
    $bigText = New-Object byte[] (8MB + 1)
    for ($k = 0; $k -lt $bigText.Length; $k++) { $bigText[$k] = 0x61 }
    foreach ($case in @(
            @('sin BOM', [byte[]]@()),
            @('con BOM de UTF-8', [byte[]](0xEF, 0xBB, 0xBF)),
            @('con BOM de UTF-16', [byte[]](0xFF, 0xFE)),
            @('con BOM de UTF-32', [byte[]](0xFF, 0xFE, 0, 0))
        )) {
        Reset-Repo
        PutBytes 'enorme.txt' ($case[1] + $bigText)
        Stage-All
        Expect ('archivo de texto de mas de 8 MiB ' + $case[0] + ' se rechaza') $true
    }
    # A BOM alone does not determine how a file is read: decodings are cumulative.
    Reset-Repo
    PutBytes 'bom-cruzado.txt' ([byte[]](0xFF, 0xFE) + $utf8.GetBytes("hola $sid`n"))
    Stage-All
    Expect 'BOM de UTF-16 delante de un texto UTF-8 con SID' $true
    Reset-Repo
    PutBytes 'bom-cruzado.txt' ([byte[]](0xFE, 0xFF) + $utf8.GetBytes("hola $sid`n"))
    Stage-All
    Expect 'BOM de UTF-16 BE delante de un texto UTF-8 con SID' $true
    Reset-Repo
    PutBytes 'bom-cruzado.txt' ([byte[]](0xEF, 0xBB, 0xBF) + [System.Text.Encoding]::Unicode.GetBytes("hola $sid`n"))
    Stage-All
    Expect 'BOM de UTF-8 delante de un texto UTF-16 con SID' $true
    Reset-Repo
    PutBytes 'bom-cruzado.txt' ([System.Text.Encoding]::Unicode.GetPreamble() + $utf8.GetBytes("hola`n") + [System.Text.Encoding]::Unicode.GetBytes("`n$sid`n"))
    Stage-All
    Expect 'UTF-16 con BOM con un tramo UTF-8 limpio y otro UTF-16 con SID' $true
    Reset-Repo
    PutBytes 'bom-limpio.txt' ([byte[]](0xFF, 0xFE) + $utf8.GetBytes("todo limpio`n"))
    Stage-All
    Expect 'BOM de UTF-16 delante de un texto UTF-8 limpio' $false
    Reset-Repo
    PutBytes 'bom-limpio.txt' ([byte[]](0xEF, 0xBB, 0xBF) + [System.Text.Encoding]::Unicode.GetBytes("todo limpio`n"))
    Stage-All
    Expect 'BOM de UTF-8 delante de un texto UTF-16 limpio' $false
    # UTF-8 from bytes WITHOUT NULs: a multibyte character split by a stray NUL (C3 00 B3) becomes the letter again only
    # when the NUL is removed BEFORE decoding. Use an accented denylist term (constructed with [char]0x00F3).
    $oTilde = [string][char]0x00F3
    $deny = Join-Path $repo '.git\privacy-denylist'
    [System.IO.File]::WriteAllText($deny, ("canci" + $oTilde + "n`n"), $utf8)
    Reset-Repo
    [System.IO.File]::WriteAllText($deny, ("canci" + $oTilde + "n`n"), $utf8)
    PutBytes 'partido.txt' ([byte[]](0x63, 0x61, 0x6E, 0x63, 0x69, 0xC3, 0x00, 0xB3, 0x6E, 0x0A))
    Stage-All
    Expect 'termino con acento partido por un NUL dentro de sus bytes UTF-8' $true
    Reset-Repo
    [System.IO.File]::WriteAllText($deny, ("canci" + $oTilde + "n`n"), $utf8)
    PutBytes 'partido.txt' ([byte[]](0x63, 0x61, 0x6E, 0x63, 0x69, 0xC3, 0x00, 0xA9, 0x6E, 0x0A))
    Stage-All
    Expect 'el mismo caso con otra letra acentuada (no es el termino) pasa' $false
    Remove-Item -LiteralPath $deny -Force -ErrorAction SilentlyContinue
    # UNC paths in both namespace forms: `\\?\UNC\` and `\\.\UNC\` (constructed without writing them literally).
    $bsl = [string][char]92
    foreach ($ns in @('?', '.')) {
        Reset-Repo
        Put 'unc.txt' ("ruta " + $bsl + $bsl + $ns + $bsl + 'UNC' + $bsl + 'srv' + $bsl + 'share' + $bsl + "x`n")
        Stage-All
        Expect ("ruta UNC verbatim con $ns") $true
    }
    # A registered docs/img/ PNG remains allowed (its exception is checked before the opaque-format rule).
    Reset-Repo
    PutBytes $img $png
    Put 'docs/img/README.md' $readmeOk
    Stage-All
    Expect 'docs/img: el PNG registrado sigue permitido con la regla de formatos opacos' $false

    # --- Real commits with hooks installed: inspect the ENTIRE message, regardless of Git cleanup mode -------------------
    Reset-Repo
    Copy-Item -LiteralPath (Join-Path (Split-Path -Parent $srcScript) 'install-hooks.ps1') -Destination (Join-Path $repo 'scripts\install-hooks.ps1')
    Push-Location $repo
    try {
        $hookOut = & pwsh -NoProfile -File scripts/install-hooks.ps1 2>&1
        if ($LASTEXITCODE -ne 0) { throw "install-hooks fallo: $hookOut" }
    } finally { Pop-Location }
    function Try-Commit([string[]]$GitArgs) {
        # Return $true if the commit succeeded (the hook allowed it). Always use --allow-empty and no editor.
        $before = (& git -C $repo rev-parse HEAD)
        $prev = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try { & git -C $repo commit -q --allow-empty @GitArgs 2>&1 | Out-Null } finally { $ErrorActionPreference = $prev }
        $after = (& git -C $repo rev-parse HEAD)
        return ($before -ne $after)
    }
    function Expect-Commit([string]$Case, [bool]$ShouldPass, [string[]]$GitArgs) {
        $script:total++
        $ok = Try-Commit $GitArgs
        if ($ok -ne $ShouldPass) {
            $want = if ($ShouldPass) { 'COMMIT' } else { 'BLOQUEO' }
            $script:failures.Add("$Case (esperado: $want, resultado: $(if ($ok) { 'commit hecho' } else { 'bloqueado' }))")
        }
    }
    $msgFile = Join-Path $repo 'MSGF.txt'
    Expect-Commit 'commit real: -m limpio' $true @('-m', 'feat: limpio')
    Expect-Commit 'commit real: -m con SID' $false @('-m', "feat: x $sid")
    Expect-Commit 'commit real: -m con SID tras una linea con #' $false @('-m', "feat: x`n`n# $sid")
    [System.IO.File]::WriteAllText($msgFile, "feat: limpio`n`n# nota`n", $utf8)
    Expect-Commit 'commit real: -F limpio' $true @('-F', $msgFile)
    [System.IO.File]::WriteAllText($msgFile, "feat: x`n`n# $sid`n", $utf8)
    Expect-Commit 'commit real: -F con SID en linea #' $false @('-F', $msgFile)
    foreach ($mode in 'verbatim', 'whitespace', 'scissors', 'strip', 'default') {
        Expect-Commit "commit real: --cleanup=$mode con SID en linea #" $false @("--cleanup=$mode", '-m', "feat: x`n`n# $sid")
        Expect-Commit "commit real: --cleanup=$mode con SID en el cuerpo" $false @("--cleanup=$mode", '-m', "feat: x`n`n$sid")
        Expect-Commit "commit real: --cleanup=$mode limpio" $true @("--cleanup=$mode", '-m', "feat: limpio con $mode")
    }
    # After the scissors line: --cleanup=scissors would TRUNCATE this in Git, but the hook sees the file first and blocks it.
    $scissorsMsg = "feat: limpio`n`n# ------------------------ >8 ------------------------`n# Do not modify or remove the line above.`n+$sid`n"
    Expect-Commit 'commit real: --cleanup=scissors con SID tras la tijera' $false @('--cleanup=scissors', '-m', $scissorsMsg)
    Expect-Commit 'commit real: --cleanup=verbatim con SID tras la tijera' $false @('--cleanup=verbatim', '-m', $scissorsMsg)
    Expect-Commit 'commit real: --cleanup=whitespace con SID tras la tijera' $false @('--cleanup=whitespace', '-m', $scissorsMsg)
    [System.IO.File]::WriteAllText($msgFile, $scissorsMsg, $utf8)
    Expect-Commit 'commit real: -F con SID tras la tijera' $false @('-F', $msgFile)
    Remove-Item $msgFile -Force
    Remove-Item -LiteralPath (Join-Path $repo '.git\hooks\commit-msg'), (Join-Path $repo '.git\hooks\pre-commit') -Force -ErrorAction SilentlyContinue

    # --- Commit range (CI: changes introduced by a push or PR) ----------------------------------------------------------
    # History: clean base; commit adds an SID with a clean message; another DELETES the file; one adds an SID to the
    # message (--allow-empty); final clean commit. The final tree is clean: only range scanning finds the leak.
    function Sha([string]$Rev) { return ((& git -C $repo rev-parse $Rev) | Out-String).Trim() }
    function New-Commit([string]$Message) {
        $prev = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try { & git -C $repo commit -q --allow-empty --no-verify -m $Message 2>&1 | Out-Null } finally { $ErrorActionPreference = $prev }
    }
    Reset-Repo
    Put 'base.txt' "base limpia`n"
    Stage-All
    New-Commit 'chore: base limpia'
    $base = Sha 'HEAD'
    Put 'filtrado.txt' ("cuenta $sid`n")
    Stage-All
    New-Commit 'feat: archivo (limpio en apariencia)'
    $leak = Sha 'HEAD'
    Invoke-Git -C $repo rm -q -f filtrado.txt
    New-Commit 'chore: se quita el archivo'
    $removed = Sha 'HEAD'
    New-Commit "docs: mensaje con $sid"
    $msgLeak = Sha 'HEAD'
    Put 'final.txt' "final limpio`n"
    Stage-All
    New-Commit 'feat: final limpio'
    $tip = Sha 'HEAD'
    Expect '-All sobre el arbol final limpio pasa (el SID solo esta en la historia)' $false @('-All')
    Expect 'rango base..tip: detecta el blob introducido y luego borrado y el mensaje con SID' $true @('-From', $base, '-To', $tip)
    Expect 'rango: el blob con SID de un commit intermedio (base..removed)' $true @('-From', $base, '-To', $removed)
    Expect 'rango: un solo commit con el SID (base..leak)' $true @('-From', $base, '-To', $leak)
    Expect 'rango: solo el mensaje con SID (removed..msgLeak)' $true @('-From', $removed, '-To', $msgLeak)
    Expect 'rango posterior a lo malo (msgLeak..tip) pasa' $false @('-From', $msgLeak, '-To', $tip)
    Expect 'rango vacio (tip..tip) pasa' $false @('-From', $tip, '-To', $tip)
    $zeros = '0' * 40
    Expect 'primer push (before = ceros): se revisa toda la historia' $true @('-From', $zeros, '-To', $tip)
    Expect 'before ausente: se revisa toda la historia' $true @('-To', $tip)
    $ghost = 'a' * 40
    Expect 'before que no existe en el clon: se revisa toda la historia' $true @('-From', $ghost, '-To', $tip)
    # Test-repository history includes an old `image` commit that registered its PNG in its README. With the current `To`
    # README (the base commit, without the image), it passes only if its hash is in the closed historical list (provided here).
    Expect 'historia hasta la base (limpia) pasa' $false @('-To', $base, '-LegacyImageSha256', (Sha256Hex $png))
    Expect 'historia hasta la base: la captura de un commit antiguo ya no esta registrada en To' $true @('-To', $base)
    Expect 'rango: -To con un valor que no es un SHA (sin inyeccion)' $true @('-To', 'HEAD; calc')
    Expect 'rango: -From con un valor que no es un SHA' $true @('-From', '--all', '-To', $tip)
    # Merge branch: the bad blob arrives through the second branch.
    Invoke-Git -C $repo checkout -q -b lateral $base
    Put 'lateral.txt' ("cuenta $sid`n")
    Stage-All
    New-Commit 'feat: lateral'
    Invoke-Git -C $repo checkout -q -
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & git -C $repo merge -q --no-ff --no-verify -m 'merge: lateral' lateral 2>&1 | Out-Null } finally { $ErrorActionPreference = $prev }
    $merged = Sha 'HEAD'
    Expect 'rango con un merge: el blob de la rama lateral se revisa' $true @('-From', $tip, '-To', $merged)
    # Range files: opaque formats and file names.
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B limpia $base
    PutBytes 'datos/contenido.dat' ($zipBytes)
    Stage-All
    New-Commit 'feat: un zip'
    $zipTip = Sha 'HEAD'
    Expect 'rango: un ZIP introducido se rechaza' $true @('-From', $base, '-To', $zipTip)
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B limpia2 $base
    Put 'ok.txt' "limpio`n"
    Stage-All
    New-Commit 'feat: limpio'
    $okTip = Sha 'HEAD'
    Expect 'rango limpio pasa' $false @('-From', $base, '-To', $okTip)
    # The docs/img/ exception works in range mode (README from the relevant commit).
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B imagenes $base
    PutBytes $img $png
    Put 'docs/img/README.md' $readmeOk
    Stage-All
    New-Commit 'docs: captura'
    $imgTip = Sha 'HEAD'
    Expect 'rango: PNG de docs/img registrado en su commit pasa' $false @('-From', $base, '-To', $imgTip)
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B imagenes2 $base
    PutBytes $img $png
    Stage-All
    New-Commit 'docs: captura sin registrar'
    $img2Tip = Sha 'HEAD'
    Expect 'rango: PNG de docs/img SIN registrar se rechaza' $true @('-From', $base, '-To', $img2Tip)
    # A --demo screenshot registered in a LATER commit (listed by the `To` README): range validation passes.
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B imagenes3 $base
    PutBytes $img $png
    Stage-All
    New-Commit 'docs: captura (aun sin registrar)'
    $pngEarly = Sha 'HEAD'
    Put 'docs/img/README.md' $readmeOk
    Stage-All
    New-Commit 'docs: se registra la captura'
    $pngLate = Sha 'HEAD'
    Expect 'rango que empieza antes del registro del PNG de --demo pasa (README del commit To)' $false @('-From', $base, '-To', $pngLate)
    Expect 'toda la historia con el PNG registrado despues pasa (from a ceros)' $false @('-From', ('0' * 40), '-To', $pngLate)
    Expect 'sin el registro en To, el mismo PNG se rechaza' $true @('-From', $base, '-To', $pngEarly)
    # Registering a DIFFERENT image (different hash) does not cover this one.
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B imagenes4 $base
    PutBytes $img $png
    Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $imgLeaf ([byte[]](1, 2, 3))))
    Stage-All
    New-Commit 'docs: captura con el hash de otra'
    Expect 'el README de To con el hash de otra imagen no vale' $true @('-From', $base, '-To', (Sha 'HEAD'))

    # GM-M8: in -To mode, ONLY the `To` commit's README counts. An image registered in its own commit but removed later
    # must fail (previously its own README vouched for it).
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B desregistrada $base
    PutBytes $img $png
    Put 'docs/img/README.md' $readmeOk
    Stage-All
    New-Commit 'docs: captura registrada'
    $regTip = Sha 'HEAD'
    Put 'docs/img/README.md' "# Capturas`n"
    Stage-All
    New-Commit 'docs: se quita el registro'
    $unregTip = Sha 'HEAD'
    Expect 'rango: la captura registrada en su commit pasa con To = ese commit' $false @('-From', $base, '-To', $regTip)
    Expect 'rango: la captura registrada en su commit pero desregistrada en To se rechaza' $true @('-From', $base, '-To', $unregTip)

    # GM-M8: also inspect each range commit's author and committer (name and email).
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        Reset-Repo
        Invoke-Git -C $repo checkout -q -f -B autores $base
        & git -C $repo commit -q --allow-empty --no-verify --author "Ana $sid <ana@example.invalid>" -m 'chore: autor con SID' 2>&1 | Out-Null
        $authorTip = Sha 'HEAD'
        & git -C $repo commit -q --allow-empty --no-verify --author "Ana <$($sid.ToLowerInvariant())@example.invalid>" -m 'chore: correo del autor con SID' 2>&1 | Out-Null
        $authorMailTip = Sha 'HEAD'
        $env:GIT_COMMITTER_NAME = "Luis $sid"
        & git -C $repo commit -q --allow-empty --no-verify -m 'chore: committer con SID' 2>&1 | Out-Null
        $committerTip = Sha 'HEAD'
        Remove-Item Env:GIT_COMMITTER_NAME -ErrorAction SilentlyContinue
        $env:GIT_COMMITTER_EMAIL = "$($sid.ToLowerInvariant())@example.invalid"
        & git -C $repo commit -q --allow-empty --no-verify -m 'chore: correo del committer con SID' 2>&1 | Out-Null
        $committerMailTip = Sha 'HEAD'
        Remove-Item Env:GIT_COMMITTER_EMAIL -ErrorAction SilentlyContinue
        & git -C $repo commit -q --allow-empty --no-verify -m 'chore: ya limpio' 2>&1 | Out-Null
        $cleanTip = Sha 'HEAD'
    } finally {
        Remove-Item Env:GIT_COMMITTER_NAME -ErrorAction SilentlyContinue
        Remove-Item Env:GIT_COMMITTER_EMAIL -ErrorAction SilentlyContinue
        $ErrorActionPreference = $prevEap
    }
    Expect 'rango: el nombre del autor con un SID se rechaza' $true @('-From', $base, '-To', $authorTip)
    Expect 'rango: el correo del autor con un SID (minusculas) se rechaza' $true @('-From', $authorTip, '-To', $authorMailTip)
    Expect 'rango: el nombre del committer con un SID se rechaza' $true @('-From', $authorMailTip, '-To', $committerTip)
    Expect 'rango: el correo del committer con un SID se rechaza' $true @('-From', $committerTip, '-To', $committerMailTip)
    Expect 'rango: un commit limpio despues de los malos pasa' $false @('-From', $committerMailTip, '-To', $cleanTip)
    # R-S8: findings identify their field (not `message:1..4`), and the body retains its own line numbers.
    foreach ($case in @(
            @($authorTip, $base, 'autor (nombre)'),
            @($authorMailTip, $authorTip, 'autor (correo)'),
            @($committerTip, $authorMailTip, 'committer (nombre)'),
            @($committerMailTip, $committerTip, 'committer (correo)')
        )) {
        $script:total++
        $out = (Run-Check @('-From', $case[1], '-To', $case[0])).Out
        if ($out -notlike ('*' + $case[2] + ': SID*') -or $out -like '*mensaje:*') { $script:failures.Add("etiqueta del campo $($case[2]): $out") }
    }
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B cuerpo $base
    $prevEap2 = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & git -C $repo commit -q --allow-empty --no-verify -m "chore: linea 1`n`nlinea 3`n$sid" 2>&1 | Out-Null } finally { $ErrorActionPreference = $prevEap2 }
    $script:total++
    $out = (Run-Check @('-From', $base, '-To', (Sha 'HEAD'))).Out
    if ($out -notlike '*mensaje:4: SID*') { $script:failures.Add("numero de linea del cuerpo (se esperaba mensaje:4): $out") }

    # HISTORICAL --demo screenshots: their hashes are in the script's closed list (only in -To mode), not in any README.
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B historica $base
    PutBytes $img $png
    Stage-All
    New-Commit 'docs: captura historica'
    $legacyTip = Sha 'HEAD'
    Expect 'rango: una captura sin registrar y fuera de la lista historica se rechaza' $true @('-From', $base, '-To', $legacyTip)
    Expect 'rango: una captura historica (hash de la lista cerrada) se acepta' $false @('-From', $base, '-To', $legacyTip, '-LegacyImageSha256', (Sha256Hex $png))
    Expect 'la lista historica no vale en staging ni en -All' $true @('-All', '-LegacyImageSha256', (Sha256Hex $png))
    # A historical image with metadata is still rejected (structural checks are not skipped).
    $pngMeta = New-Png -Chunks @((Fill 'tEXt' 4))
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B historica2 $base
    PutBytes $img $pngMeta
    Stage-All
    New-Commit 'docs: captura historica con metadatos'
    Expect 'rango: la captura historica con un fragmento no permitido se rechaza' $true @('-From', $base, '-To', (Sha 'HEAD'), '-LegacyImageSha256', (Sha256Hex $pngMeta))

    # TYPE change (a file becomes a symbolic link): the link target is its content and is checked.
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B tipo $base
    Put 'enlace.txt' "texto limpio`n"
    Stage-All
    New-Commit 'feat: archivo normal'
    $beforeType = Sha 'HEAD'
    $targetOid = (($secretLine) | & git -C $repo hash-object -w --stdin | Out-String).Trim()
    Invoke-Git -C $repo update-index --cacheinfo "120000,$targetOid,enlace.txt"
    New-Commit 'feat: ahora es un enlace'
    $afterType = Sha 'HEAD'
    Expect 'rango: un archivo que pasa a enlace simbolico con el SID en su destino (cambio de tipo)' $true @('-From', $beforeType, '-To', $afterType)
    Expect 'rango anterior al cambio de tipo pasa' $false @('-From', $base, '-To', $beforeType)

    # --- -All mode over versioned files ----------------------------------------------------------------------------------
    Reset-Repo
    Put 'v.txt' ($bad['perfil-windows'] + "`n")
    Stage-All
    Invoke-Git -C $repo commit -q -m malo
    Expect '-All encuentra lo ya commiteado' $true @('-All')
    Expect 'staging vacio no falla' $false
    # --- Local values with NON-ASCII letters (accents, tildes) in every encoding -----------------------------------------
    # User and computer values are SYNTHETIC and set only for child processes (restored on exit); they are constructed with
    # [char]0x... so this file remains ASCII-only (Windows PowerShell 5.1 reads .ps1 files without a BOM as ANSI).
    $uUser = ([string][char]0x00CD) + ([string][char]0x00F1) + 'igo-Prueba'
    $uPc = 'EQUIPO-' + ([string][char]0x00D1) + 'AND' + ([string][char]0x00DA)
    $eAcute = [string][char]0x00E9
    $latin1 = [System.Text.Encoding]::GetEncoding(28591)
    $oldUser = $env:USERNAME
    $oldPc = $env:COMPUTERNAME
    $env:USERNAME = $uUser
    $env:COMPUTERNAME = $uPc
    try {
        # Write each case in UTF-8, UTF-8 with BOM, UTF-16 with BOM, UTF-16 WITHOUT BOM, Latin-1, and two binaries (with
        # NULs and no BOM: one contains a Latin-1 string and one a UTF-16 string).
        function Put-Encodings([string]$Text) {
            $u16 = [System.Text.Encoding]::Unicode.GetBytes($Text)
            PutBytes 'e/utf8.txt' ($utf8.GetBytes($Text))
            PutBytes 'e/utf8bom.txt' ([byte[]](0xEF, 0xBB, 0xBF) + $utf8.GetBytes($Text))
            PutBytes 'e/utf16bom.txt' ([System.Text.Encoding]::Unicode.GetPreamble() + $u16)
            PutBytes 'e/utf16.txt' $u16
            PutBytes 'e/latin1.txt' ($latin1.GetBytes($Text))
            PutBytes 'e/bin-latin1.bin' ([byte[]](0, 1, 2, 0, 255) + $latin1.GetBytes($Text) + [byte[]](0, 0, 9))
            PutBytes 'e/bin-utf16.bin' ([byte[]](0, 1, 2, 0, 255, 7) + $u16 + [byte[]](0, 0, 9))
        }
        $encNames = @('utf8.txt', 'utf8bom.txt', 'utf16bom.txt', 'utf16.txt', 'latin1.txt', 'bin-latin1.bin', 'bin-utf16.bin')
        # All seven files for a case run TOGETHER (one process per format took minutes): checker output lists each problem
        # file ("e/<file>:<line>: <rule>"), so each encoding is asserted separately (positive requires ITS file; negative
        # requires NONE) and failures identify the encoding.
        function Expect-Each([string]$Case, [string]$Text, [bool]$ShouldFail, [string[]]$CheckArgs = @()) {
            Reset-Repo
            Put-Encodings $Text
            Stage-All
            $r = Run-Check $CheckArgs
            foreach ($n in $encNames) {
                $script:total++
                $listed = [regex]::IsMatch($r.Out, '(?m)^\s*' + [regex]::Escape("e/$n") + ':')
                if ($listed -ne $ShouldFail) {
                    $want = if ($ShouldFail) { 'DETECTADO' } else { 'LIMPIO' }
                    $script:failures.Add("$Case [$n] (esperado: $want, codigo $($r.Code))")
                }
            }
            $script:total++
            if (($r.Code -ne 0) -ne $ShouldFail) { $script:failures.Add("$Case (codigo de salida $($r.Code), esperado " + $(if ($ShouldFail) { 'distinto de 0' } else { '0' }) + ")") }
        }
        Expect-Each 'usuario local con letras no ASCII, suelto' ("hola`nusuario $uUser fin`n") $true
        Expect-Each 'usuario local con letras no ASCII, entre parentesis' ("(" + $uUser + ")`n") $true
        Expect-Each 'usuario local con letras no ASCII, en mayusculas' ("hola " + $uUser.ToUpperInvariant() + " fin`n") $true
        Expect-Each 'equipo local con letras no ASCII' ("host=" + $uPc + ";`n") $true
        # Adjacent to other letters or digits is NOT a leak (ASCII or accented).
        Expect-Each 'usuario pegado a letras ASCII no cuenta' ("zz" + $uUser + "zz`n") $false
        Expect-Each 'usuario con una letra acentuada delante no cuenta' ($eAcute + $uUser + "`n") $false
        Expect-Each 'usuario con una letra acentuada detras no cuenta' ($uUser + $eAcute + "`n") $false
        Expect-Each 'equipo con una letra acentuada delante no cuenta' ($eAcute + $uPc + "`n") $false
        Expect-Each 'otro valor parecido (otra letra acentuada) no cuenta' (([string][char]0x00D3) + ([string][char]0x00F1) + "igo-Prueba`n") $false
        Expect-Each 'contenido limpio en cualquier codificacion' "nada que ver con la maquina`n" $false
        # Local values do not apply with -GenericOnly (CI).
        Expect-Each 'usuario con letras no ASCII con -GenericOnly' ("hola " + $uUser + " fin`n") $false @('-GenericOnly')
        # Line numbers: a finding present only in Latin-1 text must report ITS line (not shifted by text concatenated
        # before it). Three-line file with the user on line 2.
        Reset-Repo
        PutBytes 'ln/lat.txt' ($latin1.GetBytes("uno`nusuario $uUser fin`ntres`n"))
        Stage-All
        $r = Run-Check @()
        $lineNos = @([regex]::Matches($r.Out, 'ln/lat\.txt:(\d+):') | ForEach-Object { $_.Groups[1].Value })
        $script:total++
        if ($r.Code -eq 0 -or $lineNos.Count -lt 1 -or @($lineNos | Where-Object { $_ -ne '2' }).Count -gt 0) {
            $script:failures.Add("numero de linea de un hallazgo solo Latin-1: se esperaba solo la linea 2, salio: $($lineNos -join ',') ($($r.Out))")
        }
        # A synthetic ASCII value between two CJK characters in UTF-16 WITHOUT BOM (binary): neighbors must not hide it.
        $env:USERNAME = 'Zorbaq'
        $cjkL = [string][char]0x4E2D
        $cjkR = [string][char]0x6587
        Reset-Repo
        PutBytes 'ln/cjk.bin' ([byte[]](0, 1, 2, 0, 255, 7) + [System.Text.Encoding]::Unicode.GetBytes($cjkL + 'Zorbaq' + $cjkR) + [byte[]](0, 0, 9))
        Stage-All
        Expect 'valor local en UTF-16 de un binario entre dos caracteres CJK' $true
        Reset-Repo
        PutBytes 'ln/cjk-ok.bin' ([byte[]](0, 1, 2, 0, 255, 7) + [System.Text.Encoding]::Unicode.GetBytes($cjkL + 'Zorbaqx' + $cjkR) + [byte[]](0, 0, 9))
        Stage-All
        Expect 'valor local pegado a una letra ASCII en UTF-16 de un binario no cuenta' $false
        # Case-insensitive matching must not depend on process culture (Turkish i): under tr-TR, 'IQZORB' and 'iqzorb'
        # must still match.
        $env:USERNAME = 'IQZORB'
        Reset-Repo
        Put 'ln/tr.txt' ("hola iqzorb fin`n")
        Stage-All
        Expect 'IQZORB frente a iqzorb con la cultura por defecto' $true
        $script:total++
        $cmd = "[System.Threading.Thread]::CurrentThread.CurrentCulture = [System.Globalization.CultureInfo]'tr-TR'; & '$scriptPath'; exit `$LASTEXITCODE"
        Push-Location $repo
        $prevEap = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try { $null = & $Shell -NoProfile -Command $cmd 2>&1; $trCode = $LASTEXITCODE } finally { $ErrorActionPreference = $prevEap; Pop-Location }
        if ($trCode -eq 0) { $script:failures.Add('IQZORB frente a iqzorb con la cultura tr-TR: no se detecto (la i turca)') }
        $env:USERNAME = $uUser
        # Commit messages use the same decoding.
        foreach ($case in @(
                @('UTF-8', $utf8.GetBytes("fix: caso de $uUser`n")),
                @('Latin-1', $latin1.GetBytes("fix: caso de $uUser`n")),
                @('UTF-16 sin BOM', [System.Text.Encoding]::Unicode.GetBytes("fix: caso de $uUser`n"))
            )) {
            [System.IO.File]::WriteAllBytes($msg, $case[1])
            Expect "mensaje con el usuario local no ASCII ($($case[0]))" $true @('-MessageFile', $msg)
            Expect "mensaje con el usuario local no ASCII ($($case[0])) con -GenericOnly" $false @('-GenericOnly', '-MessageFile', $msg)
        }
        Remove-Item -LiteralPath $msg -Force
    } finally {
        $env:USERNAME = $oldUser
        $env:COMPUTERNAME = $oldPc
    }
    # --- CI definido y -Local, USERDOMAIN --------------------------------------------------------------------------------
    if ($env:USERNAME -and $env:USERNAME.Length -ge 3) {
        Reset-Repo
        Put 'u.txt' ('hola ' + $env:USERNAME + " adios`n")
        Stage-All
        $env:CI = 'true'
        try {
            Expect 'con CI definido solo rigen los patrones genericos' $false
            Expect 'con CI definido y -Local los valores locales siguen vigentes' $true @('-Local')
        } finally {
            $env:CI = $null
        }
        Expect '-Local con CI sin definir' $true @('-Local')
        Expect '-Local y -GenericOnly son incompatibles' $true @('-Local', '-GenericOnly')
    }
    if ($env:USERDOMAIN -and $env:USERDOMAIN.Length -ge 3) {
        Reset-Repo
        Put 'u.txt' ('dominio ' + $env:USERDOMAIN + "`n")
        Stage-All
        Expect 'valor local USERDOMAIN (local)' $true
        Expect 'valor local USERDOMAIN con -GenericOnly' $false @('-GenericOnly')
    }
} finally {
    if ($null -eq $oldCI) { Remove-Item Env:CI -ErrorAction SilentlyContinue } else { $env:CI = $oldCI }
    Set-Location $PSScriptRoot
    Remove-Repo
}

if ($nulFiles.Count -gt 0) {
    $total++
    $failures.Add("archivos de texto versionados con bytes NUL (grep los tomaria por binarios): $($nulFiles -join ', ')")
} else {
    $total++
}

if ($failures.Count -gt 0) {
    Write-Host "privacy-check.tests ($Shell): $($failures.Count) de $total pruebas FALLAN" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  - $_" }
    exit 1
}
Write-Host "privacy-check.tests ($Shell): $total pruebas OK"
exit 0
