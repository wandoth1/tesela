<#
.SYNOPSIS
  Pruebas de scripts/privacy-check.ps1 sobre un repositorio de pruebas en %TEMP%\tesela-test-<uuid>.

.DESCRIPTION
  Crea un repositorio git desechable, copia el script y comprueba que los fixtures "malos" hacen fallar al check y los
  "buenos" lo pasan, en modo staging, -All, -GenericOnly y -MessageFile. Los fixtures malos viven en hexadecimal en
  `fixtures.json` (contienen justo lo que el check rechaza, y asi este directorio se puede revisar como el resto del
  repositorio).

.PARAMETER Shell
  Ejecutable con el que se lanza privacy-check.ps1: `pwsh` (por defecto) o `powershell` (Windows PowerShell 5.1).

.EXAMPLE
  pwsh -File scripts/tests/privacy-check.tests.ps1
  pwsh -File scripts/tests/privacy-check.tests.ps1 -Shell powershell
#>
[CmdletBinding()]
param([string]$Shell = 'pwsh')

$ErrorActionPreference = 'Stop'
$srcScript = Join-Path (Split-Path -Parent $PSScriptRoot) 'privacy-check.ps1'
if (-not (Test-Path -LiteralPath $srcScript)) { throw "no encuentro $srcScript" }

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

# --- Repositorio de pruebas (solo bajo %TEMP%, con guarda al borrar) -------------------------------------------------
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

# Un PNG minimo (sin CRC validos: el check solo mira la estructura, los tipos y las longitudes de los fragmentos).
function New-Chunk([string]$Type, [byte[]]$Data) {
    $len = $Data.Length
    $b = New-Object System.Collections.Generic.List[byte]
    $b.AddRange([byte[]]((($len -shr 24) -band 255), (($len -shr 16) -band 255), (($len -shr 8) -band 255), ($len -band 255)))
    $b.AddRange([System.Text.Encoding]::ASCII.GetBytes($Type))
    $b.AddRange($Data)
    $b.AddRange([byte[]](0, 0, 0, 0))
    return , $b.ToArray()
}

# Fragmento `$Type` con `$N` bytes de datos (por defecto, los 4 de gAMA...).
function Fill([string]$Type, [int]$N) { return @{ T = $Type; D = (New-Object byte[] $N) } }

# `-Color` es el tipo de color del IHDR (6 = RGBA, 3 = indexado); `-Chunks` son fragmentos extra antes de IDAT.
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

# Linea del README de docs/img con el SHA-256 de los bytes de la imagen.
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
        $want = if ($ShouldFail) { 'FALLO' } else { 'OK' }
        $script:failures.Add("$Case (esperado: $want, codigo $($r.Code)) $($r.Out)")
    }
}

# En GitHub Actions CI=true: el script pasaria a modo generico y los casos con valores locales fallarian. Las pruebas
# fijan su propio entorno y lo restauran al terminar.
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

    # --- Fixtures malos ------------------------------------------------------------------------------------------------
    foreach ($k in ($bad.Keys | Sort-Object)) {
        Reset-Repo
        Put 'notas.txt' ("linea limpia`n" + $bad[$k] + "`notra linea limpia`n")
        Stage-All
        Expect "malo (staging): $k" $true
        Expect "malo (-All): $k" $true @('-All')
    }

    # --- Fixtures buenos -----------------------------------------------------------------------------------------------
    $i = 0
    foreach ($g in $good) {
        Reset-Repo
        $i++
        Put 'ok.txt' ($g + "`n")
        Stage-All
        Expect "bueno (staging) #${i}: $g" $false
    }

    # --- Codificaciones y binarios ---------------------------------------------------------------------------------------
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
    Reset-Repo
    PutBytes 'c.bin' ([byte[]](0, 1, 2, 0, 255, 7, 8, 9, 0, 0, 1))
    Stage-All
    Expect 'binario limpio' $false
    Reset-Repo
    PutBytes 'captura.png' ([byte[]](0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0))
    Stage-All
    Expect 'imagen (captura)' $true

    # --- Excepcion de docs/img/ (capturas de --demo) -------------------------------------------------------------------
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
    # La linea del README tiene que ser ANCLADA y con el hash exacto.
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
    # Un nombre que es SUBCADENA del de otra imagen registrada no vale (`o.png` frente a `mapa-demo.png`).
    Reset-Repo
    PutBytes 'docs/img/o.png' $png
    Put 'docs/img/README.md' $readmeOk
    Stage-All
    Expect 'docs/img: nombre que es subcadena de otro registrado (o.png)' $true
    # Cambiar un solo byte de la imagen la invalida hasta registrarla de nuevo.
    Reset-Repo
    PutBytes $img (New-Png -Idat ([byte[]](1, 2, 3, 4, 5, 6, 7, 9)))
    Put 'docs/img/README.md' $readmeOk
    Stage-All
    Expect 'docs/img: imagen distinta de la registrada (un byte)' $true

    # --- Fragmentos PNG: lista cerrada y longitudes cerradas -----------------------------------------------------------------
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
        # Con UN byte mas (texto en claro escondido en un fragmento permitido) falla.
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
    # PLTE y tRNS solo en PNG indexados y con longitudes maximas.
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

    # --- Rutas no permitidas --------------------------------------------------------------------------------------------------
    foreach ($badPath in 'docs/img/Mapa.png', 'docs/img/sub/mapa.png', 'docs/mapa.png', 'img/mapa.png', 'docs/img/mapa.jpg', 'docs/img/mapa_demo.png', 'docs/img/mapa.PNG') {
        Reset-Repo
        PutBytes $badPath $png
        $leafBad = $badPath.Substring($badPath.LastIndexOf('/') + 1)
        Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $leafBad $png))
        Stage-All
        Expect "docs/img: ruta no permitida $badPath" $true
    }

    # --- Imagenes disfrazadas: se detectan por la FIRMA, con cualquier extension -------------------------------------------------
    $magic = @{}
    foreach ($pr in $raw.magic.PSObject.Properties) { $magic[$pr.Name] = HexBytes $pr.Value }
    foreach ($k in ($magic.Keys | Sort-Object)) {
        foreach ($ext in 'dat', 'bin', 'txt', 'md', 'json', 'rs') {
            Reset-Repo
            PutBytes "docs/notas.$ext" ($magic[$k] + [byte[]](0, 0, 0, 0))
            Stage-All
            Expect "imagen $k disfrazada de .$ext" $true
        }
        # Tambien dentro de docs/img/ con un nombre que no es .png.
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
    # Un JPEG con nombre .png en docs/img/ no es un PNG.
    Reset-Repo
    PutBytes $img ($magic['jpeg'] + (New-Object byte[] 40))
    Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $imgLeaf ($magic['jpeg'] + (New-Object byte[] 40))))
    Stage-All
    Expect 'docs/img: un JPEG con extension .png' $true
    # Los textos que solo EMPIEZAN como una firma no son imagenes.
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
    # Truncado, datos tras IEND, no PNG, demasiado grande y el contenido comprimido NO se escanea como texto.
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

    # --- Nombres de archivo ----------------------------------------------------------------------------------------------
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
    Reset-Repo
    Put 'spike/notas.txt' ($misc['spike-paths'] + "`n")
    Stage-All
    Expect 'spike/: las reglas de rutas no se aplican' $false
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

    # Cambio de TIPO en staging: un archivo que pasa a enlace simbolico (su destino es su contenido) se revisa; antes quedaba
    # en "0 archivos, OK".
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

    # --- El contenido que cuenta es el del indice ----------------------------------------------------------------------
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

    # --- Mensajes de commit ------------------------------------------------------------------------------------------------
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

    # --- Formatos opacos o comprimidos: se rechazan por la firma, con cualquier extension ------------------------------------
    foreach ($k in ($opaqueHex.Keys | Sort-Object)) {
        Reset-Repo
        PutBytes 'datos/archivo.dat' ((HexBytes $opaqueHex[$k]) + (New-Object byte[] 64))
        Stage-All
        Expect "formato opaco $k (.dat)" $true
    }
    # Con otras extensiones, en -All y dentro de spike/ (basta un formato: la regla es por firma).
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
    # Un ZIP y un gzip REALES con texto comprimido que contiene un SID: el texto comprimido no se ve, la firma si.
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
    # ZIP por el registro de FIN: con datos delante (un autoextraible) o con la cabecera local alterada no empieza por `PK 03 04`.
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
    # Un texto normal que solo menciona "PK" no es un ZIP.
    Reset-Repo
    Put 'notas.txt' ("PK 05 06 y PK\n en un texto`n" + ('limpio ' * 50) + "`n")
    Stage-All
    Expect 'texto que menciona PK sin ser un ZIP' $false

    # --- Codificaciones: UTF-32 con BOM y UTF-16 sin BOM detras de un texto largo ---------------------------------------------
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
    # Texto ASCII de mas de 4096 bytes y DESPUES texto en UTF-16 sin BOM: antes solo se miraban los primeros 4 KiB.
    $longAscii = $utf8.GetBytes(('a' * 6000) + "`n")
    Reset-Repo
    PutBytes 'largo.txt' ($longAscii + [System.Text.Encoding]::Unicode.GetBytes($secretLine))
    Stage-All
    Expect 'UTF-16 sin BOM tras 6000 bytes de ASCII con SID' $true
    Reset-Repo
    PutBytes 'largo.txt' ($longAscii + [System.Text.Encoding]::Unicode.GetBytes("todo limpio`n"))
    Stage-All
    Expect 'UTF-16 sin BOM tras 6000 bytes de ASCII, limpio' $false
    # Y un NUL suelto tampoco esconde un SID en UTF-8 (con acentos) del mismo archivo.
    Reset-Repo
    PutBytes 'mixto.txt' ($utf8.GetBytes("canci" + [char]0xF3 + "n $sid`n") + [byte[]](0, 1, 2) + $utf8.GetBytes("fin`n"))
    Stage-All
    Expect 'NUL suelto con SID en UTF-8 del mismo archivo' $true
    # Un PNG registrado de docs/img/ sigue permitido (tiene su excepcion propia, antes que la regla de opacos).
    Reset-Repo
    PutBytes $img $png
    Put 'docs/img/README.md' $readmeOk
    Stage-All
    Expect 'docs/img: el PNG registrado sigue permitido con la regla de formatos opacos' $false

    # --- Commits reales con los hooks instalados: el mensaje se revisa ENTERO, con cualquier limpieza de git -------------------
    Reset-Repo
    Copy-Item -LiteralPath (Join-Path (Split-Path -Parent $srcScript) 'install-hooks.ps1') -Destination (Join-Path $repo 'scripts\install-hooks.ps1')
    Push-Location $repo
    try {
        $hookOut = & pwsh -NoProfile -File scripts/install-hooks.ps1 2>&1
        if ($LASTEXITCODE -ne 0) { throw "install-hooks fallo: $hookOut" }
    } finally { Pop-Location }
    function Try-Commit([string[]]$GitArgs) {
        # Devuelve $true si el commit se hizo (el hook lo dejo pasar). Siempre con --allow-empty y sin editor.
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
    # Tras la linea de tijera: con --cleanup=scissors git LO RECORTARIA, pero el hook ve el archivo antes y bloquea.
    $scissorsMsg = "feat: limpio`n`n# ------------------------ >8 ------------------------`n# Do not modify or remove the line above.`n+$sid`n"
    Expect-Commit 'commit real: --cleanup=scissors con SID tras la tijera' $false @('--cleanup=scissors', '-m', $scissorsMsg)
    Expect-Commit 'commit real: --cleanup=verbatim con SID tras la tijera' $false @('--cleanup=verbatim', '-m', $scissorsMsg)
    Expect-Commit 'commit real: --cleanup=whitespace con SID tras la tijera' $false @('--cleanup=whitespace', '-m', $scissorsMsg)
    [System.IO.File]::WriteAllText($msgFile, $scissorsMsg, $utf8)
    Expect-Commit 'commit real: -F con SID tras la tijera' $false @('-F', $msgFile)
    Remove-Item $msgFile -Force
    Remove-Item -LiteralPath (Join-Path $repo '.git\hooks\commit-msg'), (Join-Path $repo '.git\hooks\pre-commit') -Force -ErrorAction SilentlyContinue

    # --- Rango de commits (CI: lo que introduce un push o un PR) --------------------------------------------------------------
    # Historia: base limpia; un commit que anade un SID con un mensaje limpio; otro que BORRA el archivo; uno con SID en el
    # mensaje (commit --allow-empty); uno final limpio. El arbol final esta limpio: solo el rango lo detecta.
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
    Expect 'historia hasta la base (limpia) pasa' $false @('-To', $base)
    Expect 'rango: -To con un valor que no es un SHA (sin inyeccion)' $true @('-To', 'HEAD; calc')
    Expect 'rango: -From con un valor que no es un SHA' $true @('-From', '--all', '-To', $tip)
    # Rama con un merge: el blob malo llega por la segunda rama.
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
    # Archivos de rango: formatos opacos y nombres.
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
    # La excepcion de docs/img/ funciona en el rango (README del propio commit).
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
    # Una captura de --demo que se registro en un commit POSTERIOR (el README del commit `To` la lista): el rango pasa.
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
    # Registrar OTRA imagen (otro hash) no sirve para esta.
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B imagenes4 $base
    PutBytes $img $png
    Put 'docs/img/README.md' ("# Capturas`n`n" + (ReadmeLine $imgLeaf ([byte[]](1, 2, 3))))
    Stage-All
    New-Commit 'docs: captura con el hash de otra'
    Expect 'el README de To con el hash de otra imagen no vale' $true @('-From', $base, '-To', (Sha 'HEAD'))

    # Capturas HISTORICAS de --demo: su hash esta en la lista cerrada del script (solo en modo -To), no en ningun README.
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B historica $base
    PutBytes $img $png
    Stage-All
    New-Commit 'docs: captura historica'
    $legacyTip = Sha 'HEAD'
    Expect 'rango: una captura sin registrar y fuera de la lista historica se rechaza' $true @('-From', $base, '-To', $legacyTip)
    Expect 'rango: una captura historica (hash de la lista cerrada) se acepta' $false @('-From', $base, '-To', $legacyTip, '-LegacyImageSha256', (Sha256Hex $png))
    Expect 'la lista historica no vale en staging ni en -All' $true @('-All', '-LegacyImageSha256', (Sha256Hex $png))
    # Una imagen historica con metadatos sigue rechazada (la comprobacion estructural no se salta).
    $pngMeta = New-Png -Chunks @((Fill 'tEXt' 4))
    Reset-Repo
    Invoke-Git -C $repo checkout -q -f -B historica2 $base
    PutBytes $img $pngMeta
    Stage-All
    New-Commit 'docs: captura historica con metadatos'
    Expect 'rango: la captura historica con un fragmento no permitido se rechaza' $true @('-From', $base, '-To', (Sha 'HEAD'), '-LegacyImageSha256', (Sha256Hex $pngMeta))

    # Cambio de TIPO (un archivo que pasa a enlace simbolico): el destino del enlace es su contenido y se revisa.
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

    # --- Modo -All sobre lo versionado ---------------------------------------------------------------------------------------
    Reset-Repo
    Put 'v.txt' ($bad['perfil-windows'] + "`n")
    Stage-All
    Invoke-Git -C $repo commit -q -m malo
    Expect '-All encuentra lo ya commiteado' $true @('-All')
    Expect 'staging vacio no falla' $false
    # --- Valores locales con letras NO ASCII (acentos, enes) en cualquier codificacion -----------------------------------------
    # El usuario y el equipo son SINTETICOS y se fijan solo para los procesos hijo (se restauran al terminar); se construyen con
    # [char]0x...: este archivo es solo ASCII (Windows PowerShell 5.1 lee los .ps1 sin BOM como ANSI).
    $uUser = ([string][char]0x00CD) + ([string][char]0x00F1) + 'igo-Prueba'
    $uPc = 'EQUIPO-' + ([string][char]0x00D1) + 'AND' + ([string][char]0x00DA)
    $eAcute = [string][char]0x00E9
    $latin1 = [System.Text.Encoding]::GetEncoding(28591)
    $oldUser = $env:USERNAME
    $oldPc = $env:COMPUTERNAME
    $env:USERNAME = $uUser
    $env:COMPUTERNAME = $uPc
    try {
        # Cada contenido se escribe en: UTF-8, UTF-8 con BOM, UTF-16 con BOM, UTF-16 SIN BOM, Latin-1 y dos binarios (con
        # NUL y sin BOM: una cadena Latin-1 y una cadena UTF-16 dentro).
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
        # Un archivo por vez (asi se sabe cual falla): se deja solo ese en staging.
        function Expect-Each([string]$Case, [string]$Text, [bool]$ShouldFail, [string[]]$CheckArgs = @()) {
            foreach ($n in $encNames) {
                Reset-Repo
                Put-Encodings $Text
                foreach ($other in $encNames) { if ($other -ne $n) { Remove-Item -LiteralPath (Join-Path $repo "e\$other") -Force } }
                Stage-All
                Expect "$Case [$n]" $ShouldFail $CheckArgs
            }
        }
        Expect-Each 'usuario local con letras no ASCII, suelto' ("hola`nusuario $uUser fin`n") $true
        Expect-Each 'usuario local con letras no ASCII, entre parentesis' ("(" + $uUser + ")`n") $true
        Expect-Each 'usuario local con letras no ASCII, en mayusculas' ("hola " + $uUser.ToUpperInvariant() + " fin`n") $true
        Expect-Each 'equipo local con letras no ASCII' ("host=" + $uPc + ";`n") $true
        # Pegado a otras letras o digitos NO es una fuga (ni ASCII ni acentuadas).
        Expect-Each 'usuario pegado a letras ASCII no cuenta' ("zz" + $uUser + "zz`n") $false
        Expect-Each 'usuario con una letra acentuada delante no cuenta' ($eAcute + $uUser + "`n") $false
        Expect-Each 'usuario con una letra acentuada detras no cuenta' ($uUser + $eAcute + "`n") $false
        Expect-Each 'equipo con una letra acentuada delante no cuenta' ($eAcute + $uPc + "`n") $false
        Expect-Each 'otro valor parecido (otra letra acentuada) no cuenta' (([string][char]0x00D3) + ([string][char]0x00F1) + "igo-Prueba`n") $false
        Expect-Each 'contenido limpio en cualquier codificacion' "nada que ver con la maquina`n" $false
        # Con -GenericOnly (CI) los valores locales no rigen.
        Expect-Each 'usuario con letras no ASCII con -GenericOnly' ("hola " + $uUser + " fin`n") $false @('-GenericOnly')
        # El mensaje de commit pasa por la misma decodificacion.
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

if ($failures.Count -gt 0) {
    Write-Host "privacy-check.tests ($Shell): $($failures.Count) de $total pruebas FALLAN" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  - $_" }
    exit 1
}
Write-Host "privacy-check.tests ($Shell): $total pruebas OK"
exit 0
