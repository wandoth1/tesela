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

function Run-Check([string[]]$CheckArgs) {
    Push-Location $repo
    try {
        $out = & $Shell -NoProfile -File $scriptPath @CheckArgs 2>&1
        return @{ Code = $LASTEXITCODE; Out = ($out | Out-String) }
    } finally { Pop-Location }
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
    [System.IO.File]::WriteAllText($msg, "feat: algo limpio`n`n# Please enter the commit message`n# $sid`n", $utf8)
    Expect 'mensaje limpio (los comentarios se ignoran)' $false @('-MessageFile', $msg)
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

    # --- Modo -All sobre lo versionado ---------------------------------------------------------------------------------------
    Reset-Repo
    Put 'v.txt' ($bad['perfil-windows'] + "`n")
    Stage-All
    Invoke-Git -C $repo commit -q -m malo
    Expect '-All encuentra lo ya commiteado' $true @('-All')
    Expect 'staging vacio no falla' $false
} finally {
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
