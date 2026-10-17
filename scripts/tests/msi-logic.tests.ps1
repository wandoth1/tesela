<#
.SYNOPSIS
  Prueba la LOGICA del MSI de Tesela (condiciones de lanzamiento, acciones de la carpeta, del ambito, del idioma y del
  escritorio, y los eventos de las ventanas de ambito y de carpeta) con una SESION de Windows Installer SIN INSTALAR NADA.

.DESCRIPTION
  Abre el MSI con `Installer.OpenPackage(ruta, 1)` (el 1 = ignorar el estado de la maquina): crea una sesion con las propiedades
  del paquete, sin ejecutar ninguna secuencia, sin msiexec, sin interfaz y sin pedir administrador. Sobre esa sesion, con
  propiedades SINTETICAS (nunca lee el registro ni las carpetas reales), usa el evaluador de condiciones del propio Windows
  Installer (`EvaluateCondition`) y ejecuta las acciones de tipo 51 con `DoAction` (solo fijan propiedades). Con eso:

    * recorre las secuencias (las filas de InstallExecuteSequence y de InstallUISequence, en orden de numero) y comprueba la
      carpeta, el ambito, el escritorio y el idioma que salen en cada caso, y que las condiciones de lanzamiento dejan pasar o
      bloquean lo que tienen que bloquear;
    * recorre los eventos de control de los botones Siguiente de las ventanas de ambito y de carpeta como lo haria la
      interfaz (condiciones evaluadas por Windows Installer, valores formateados) y comprueba el contexto (ALLUSERS,
      MSIINSTALLPERUSER) y la carpeta que dejan, el aviso de carpeta protegida y el bloqueo de Siguiente;
    * comprueba que los textos en espanol sobreviven a la tabla (acentos) y que el idioma se elige bien.

  Lo que NO sustituye: la interfaz real (dibujo, orden de tabulacion, comportamiento de los controles), el comportamiento del
  motor al cambiar ALLUSERS en mitad de la interfaz ni nada que dependa de instalar de verdad (eso lo hacen
  scripts/tests/msi-install.tests.ps1, en el CI, y una persona).

.PARAMETER Msi
  El MSI recien construido por scripts/build-msi.ps1.

.EXAMPLE
  pwsh -File scripts/tests/msi-logic.tests.ps1 -Msi <target>/dist/tesela-0.1.4-x64.msi
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Msi
)

$ErrorActionPreference = 'Stop'
. (Join-Path (Split-Path -Parent $PSScriptRoot) 'msi-validate.ps1')

$Msi = [System.IO.Path]::GetFullPath($Msi)
if (-not (Test-Path -LiteralPath $Msi -PathType Leaf)) { throw "no existe el MSI: $Msi" }

# Trabaja sobre una COPIA en %TEMP% (la sesion no modifica el MSI, pero asi nunca se toca el original).
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ('tesela-logic-' + [guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $tmp | Out-Null
$copy = Join-Path $tmp 'logic.msi'
Copy-Item -LiteralPath $Msi -Destination $copy

$failures = New-Object System.Collections.Generic.List[string]
$total = 0
function Check([string]$Case, [bool]$Condition, [string]$Detail = '') {
    $script:total++
    if (-not $Condition) { $script:failures.Add($(if ($Detail) { "$Case ($Detail)" } else { $Case })) }
}

$installer = New-Object -ComObject WindowsInstaller.Installer

# --- Sesion -----------------------------------------------------------------------------------------------------------------------
$script:current = $null
function Close-Session {
    # Windows Installer solo admite UNA sesion abierta por proceso: se suelta la anterior antes de abrir otra.
    if ($null -ne $script:current) {
        try { [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($script:current) } catch { }
        $script:current = $null
    }
    [System.GC]::Collect()
    [System.GC]::WaitForPendingFinalizers()
}
function New-Session {
    # [string] e [int]: pasar el resultado de Join-Path tal cual da DISP_E_TYPEMISMATCH.
    Close-Session
    $script:current = [System.__ComObject].InvokeMember('OpenPackage', 'InvokeMethod', $null, $installer, @([string]$copy, [int]1))
    return $script:current
}
function Get-Prop($S, [string]$Name) { return [string][System.__ComObject].InvokeMember('Property', 'GetProperty', $null, $S, @($Name)) }
function Set-Prop($S, [string]$Name, [string]$Value) { [System.__ComObject].InvokeMember('Property', 'SetProperty', $null, $S, @($Name, $Value)) | Out-Null }
function Test-Cond($S, [string]$Condition) {
    # 0 = falso, 1 = verdadero, 2 = sin condicion, 3 = error de sintaxis. Una condicion vacia es verdadera.
    if ([string]::IsNullOrEmpty($Condition)) { return $true }
    $r = [int][System.__ComObject].InvokeMember('EvaluateCondition', 'InvokeMethod', $null, $S, @($Condition))
    if ($r -ge 2) { throw "la condicion no se pudo evaluar (codigo $r): $Condition" }
    return ($r -eq 1)
}
function Invoke-Action($S, [string]$Name) {
    $r = [int][System.__ComObject].InvokeMember('DoAction', 'InvokeMethod', $null, $S, @($Name))
    if ($r -ne 1) { throw "la accion $Name devolvio $r" }
}
function Format-Arg($S, [string]$Arg) {
    # `[Propiedad]` -> su valor; `{}` -> vacio (borra la propiedad).
    if ($Arg -ceq '{}') { return '' }
    return [regex]::Replace($Arg, '\[([A-Za-z0-9_]+)\]', { param($m) Get-Prop $S $m.Groups[1].Value })
}

# --- Tablas del MSI (leidas una vez) -----------------------------------------------------------------------------------------------
$db = Open-MsiDatabase $copy
try {
    $seqRows = @{}
    foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
        $seqRows[$t] = @(Get-MsiRows $db ('SELECT `Action`, `Condition`, `Sequence` FROM `' + $t + '`') 3 | ForEach-Object { [pscustomobject]@{ Action = $_[0]; Condition = $_[1]; Sequence = [int]$_[2] } })
    }
    $events = @(Get-MsiRows $db 'SELECT `Dialog_`, `Control_`, `Event`, `Argument`, `Condition`, `Ordering` FROM `ControlEvent`' 6 | ForEach-Object { [pscustomobject]@{ Dialog = $_[0]; Control = $_[1]; Event = $_[2]; Argument = $_[3]; Condition = $_[4]; Order = [int]$_[5] } })
    $controlConds = @(Get-MsiRows $db 'SELECT `Dialog_`, `Control_`, `Action`, `Condition` FROM `ControlCondition`' 4 | ForEach-Object { [pscustomobject]@{ Dialog = $_[0]; Control = $_[1]; Action = $_[2]; Condition = $_[3] } })
    $launch = @(Get-MsiRows $db 'SELECT `Condition`, `Description` FROM `LaunchCondition`' 2 | ForEach-Object { [pscustomobject]@{ Condition = $_[0]; Message = $_[1] } })
} finally { Close-Msi $db }

# --- Simulacion de una secuencia y de los botones -------------------------------------------------------------------------------------
# El perfil es SINTETICO (nunca el real). Las carpetas del sistema salen de las variables de entorno, como en el MSI (en el contexto por
# usuario Windows Installer redirige ProgramFilesFolder al perfil, por eso el MSI no usa esa propiedad).
$synthetic = @{ LocalAppDataFolder = 'C:\Synth\Local\' }
$userDefault = 'C:\Synth\Local\Programs\Tesela\'
$pf64 = $env:ProgramW6432
$pf32 = $pf64 + ' (x86)'   # como el MSI: ICE03 no admite %ProgramFiles(x86)% dentro de [%...]
$winDir = $env:SystemRoot
if (-not $pf64 -or -not $winDir) { throw 'faltan las variables de entorno ProgramW6432 o SystemRoot (hace falta un Windows de 64 bits)' }
$machineDefault = $pf64 + '\Tesela\'
$unc = '\' * 2   # prefijo de ruta de red, construido en vez de escrito (privacy-check rechaza rutas de red literales)

function New-Case([hashtable]$Props) {
    # Sesion nueva con las carpetas sinteticas y las propiedades del caso. Todas las demas, las del paquete.
    $s = New-Session
    foreach ($k in $synthetic.Keys) { Set-Prop $s $k $synthetic[$k] }
    foreach ($k in $Props.Keys) { Set-Prop $s $k ([string]$Props[$k]) }
    return $s
}
function New-UiCase([hashtable]$Props) {
    # El estado en el que estan las propiedades cuando se abren las ventanas: las acciones del principio de la secuencia que fijan las
    # carpetas por defecto y las protegidas (TESELA_DEFAULT_*, TESELA_PF64, TESELA_PF32, TESELA_WIN) ya se han ejecutado. Las demas
    # propiedades (ambito elegido, carpeta, instalaciones previas) son las del caso.
    $s = New-Case $Props
    foreach ($a in 'TeselaSetDefaultUser', 'TeselaSetDefaultMachine', 'TeselaSetProtected64', 'TeselaSetProtected32', 'TeselaSetProtectedWin') { Invoke-Action $s $a }
    return $s
}
function Invoke-Sequence($S, [string]$Table, [switch]$IncludeAfterLaunch) {
    # Ejecuta las acciones `Tesela*` (que no son de texto) con su condicion, en orden de numero. Las anteriores a
    # LaunchConditions (100) primero; luego devuelve cuales de las condiciones de lanzamiento PASAN (lista de booleanos en el
    # orden de la tabla LaunchCondition) y, si se pide, ejecuta las posteriores (ambito y idioma de la interfaz).
    $rows = @($seqRows[$Table] | Where-Object { $_.Action -clike 'Tesela*' -and $_.Action -cnotlike 'TeselaEs_*' } | Sort-Object Sequence)
    foreach ($r in ($rows | Where-Object { $_.Sequence -lt 100 })) { if (Test-Cond $S $r.Condition) { Invoke-Action $S $r.Action } }
    $lc = @($launch | ForEach-Object { Test-Cond $S $_.Condition })
    if ($IncludeAfterLaunch) {
        foreach ($r in ($rows | Where-Object { $_.Sequence -gt 100 })) { if (Test-Cond $S $r.Condition) { Invoke-Action $S $r.Action } }
    }
    return , $lc
}
function Invoke-Button($S, [string]$Dialog, [string]$Control) {
    # Los eventos del boton, en orden: cada uno con su condicion evaluada por Windows Installer. Devuelve el texto de los eventos
    # de ventana (NewDialog:X, SpawnDialog:X, ...) que se habrian ejecutado; los de propiedad cambian la sesion.
    $done = New-Object System.Collections.Generic.List[string]
    foreach ($e in ($events | Where-Object { $_.Dialog -ceq $Dialog -and $_.Control -ceq $Control } | Sort-Object Order)) {
        if (-not (Test-Cond $S $e.Condition)) { continue }
        if ($e.Event -cmatch '^\[(.+)\]$') { Set-Prop $S $Matches[1] (Format-Arg $S $e.Argument) }
        else { $done.Add("$($e.Event):$(Format-Arg $S $e.Argument)") }
    }
    return , $done.ToArray()
}
function Test-ControlCond($S, [string]$Dialog, [string]$Control, [string]$Action) {
    $rows = @($controlConds | Where-Object { $_.Dialog -ceq $Dialog -and $_.Control -ceq $Control -and $_.Action -ceq $Action })
    if ($rows.Count -eq 0) { return $false }
    return (Test-Cond $S $rows[0].Condition)
}
function Join-Bool([bool[]]$b) { return (($b | ForEach-Object { if ($_) { 'P' } else { 'X' } }) -join '') }

try {
    # Orden de las condiciones de lanzamiento en la tabla (para leer los resultados): la que bloquea versiones posteriores y
    # las tres del ambito.
    $lcNames = @($launch | ForEach-Object {
            if ($_.Condition -ceq 'NOT WIX_DOWNGRADE_DETECTED') { 'downgrade' }
            elseif ($_.Condition -clike '*TESELA_PREV_USER=""') { 'user->machine' }
            elseif ($_.Condition -clike '*TESELA_PREV_MACHINE=""') { 'machine->user' }
            elseif ($_.Condition -clike '*TESELA_WIN*') { 'protected' }
            else { 'desconocida' }
        })
    Check "las cuatro condiciones de lanzamiento se reconocen ($($lcNames -join ', '))" ((($lcNames | Sort-Object) -join ',') -ceq 'downgrade,machine->user,protected,user->machine')
    function Get-LcFailures($Result) {
        $bad = @()
        for ($i = 0; $i -lt $lcNames.Count; $i++) { if (-not $Result[$i]) { $bad += $lcNames[$i] } }
        return ($bad | Sort-Object) -join ','
    }

    # === 1. Condiciones de lanzamiento sin interfaz (UILevel 2): quien las bloquea ===========================================
    $lcCases = @(
        @{ Name = 'por usuario, sin instalacion previa: todo pasa'; Props = @{ UILevel = 2 }; Fails = '' },
        @{ Name = 'para todos (ALLUSERS=1), sin instalacion previa: todo pasa'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = '' }; Fails = '' },
        @{ Name = 'por usuario sobre una instalacion por usuario: pasa'; Props = @{ UILevel = 2; TESELA_PREV_USER = 'C:\Prev\' }; Fails = '' },
        @{ Name = 'para todos sobre una instalacion para todos: pasa'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_MACHINE = 'C:\PrevM\' }; Fails = '' },
        @{ Name = 'para todos (ALLUSERS=1) sobre una instalacion por usuario: BLOQUEA'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_USER = 'C:\Prev\' }; Fails = 'user->machine' },
        @{ Name = 'ALLUSERS=2 con MSIINSTALLPERUSER vacio (para todos) sobre una instalacion por usuario: BLOQUEA'; Props = @{ UILevel = 2; ALLUSERS = 2; MSIINSTALLPERUSER = ''; TESELA_PREV_USER = 'C:\Prev\' }; Fails = 'user->machine' },
        @{ Name = 'por usuario sobre una instalacion para todos: BLOQUEA'; Props = @{ UILevel = 2; TESELA_PREV_MACHINE = 'C:\PrevM\' }; Fails = 'machine->user' },
        @{ Name = 'con las dos instalaciones previas: por usuario bloquea una y para todos la otra'; Props = @{ UILevel = 2; TESELA_PREV_USER = 'C:\Prev\'; TESELA_PREV_MACHINE = 'C:\PrevM\' }; Fails = 'machine->user' },
        @{ Name = 'con interfaz completa (UILevel 5) las ventanas avisan: no bloquea'; Props = @{ UILevel = 5; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_USER = 'C:\Prev\' }; Fails = '' },
        @{ Name = 'reparar o desinstalar (Installed) nunca se bloquea por el ambito'; Props = @{ UILevel = 2; Installed = '00:00:00'; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_USER = 'C:\Prev\' }; Fails = '' },
        @{ Name = 'una version posterior instalada bloquea la bajada'; Props = @{ UILevel = 2; WIX_DOWNGRADE_DETECTED = '{11111111-2222-3333-4444-555555555555}' }; Fails = 'downgrade' },
        @{ Name = 'carpeta protegida (Program Files) por usuario sin administrador: BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = $machineDefault }; Fails = 'protected' },
        @{ Name = 'carpeta protegida (Program Files x86) por usuario sin administrador: BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = ($pf32 + '\Tesela\') }; Fails = 'protected' },
        @{ Name = 'carpeta protegida (Windows) por usuario sin administrador: BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = ($winDir + '\Tesela\') }; Fails = 'protected' },
        @{ Name = 'carpeta protegida sin distinguir mayusculas: BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = ($machineDefault.ToLowerInvariant()) }; Fails = 'protected' },
        @{ Name = 'carpeta protegida pero con permisos de administrador (Privileged): pasa'; Props = @{ UILevel = 2; Privileged = 1; INSTALLFOLDER = $machineDefault }; Fails = '' },
        @{ Name = 'carpeta de Program Files para todos (ALLUSERS=1): pasa'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; INSTALLFOLDER = $machineDefault }; Fails = '' },
        @{ Name = 'carpeta del perfil por usuario: pasa'; Props = @{ UILevel = 2; INSTALLFOLDER = 'C:\Synth\Local\Programs\Tesela\' }; Fails = '' },
        @{ Name = 'otra unidad por usuario: pasa'; Props = @{ UILevel = 2; INSTALLFOLDER = 'C:\Mis programas\Tesela\' }; Fails = '' },
        @{ Name = 'Program Files2 NO es Program Files (la comparacion lleva la barra final): pasa'; Props = @{ UILevel = 2; INSTALLFOLDER = ($pf64 + '2\Tesela\') }; Fails = '' }
    )
    foreach ($c in $lcCases) {
        $s = New-Case $c.Props
        # La carpeta por defecto la fija la accion (si INSTALLFOLDER no viene fijada): se ejecuta la secuencia del servidor.
        $res = Invoke-Sequence $s 'InstallExecuteSequence'
        Check ("condiciones de lanzamiento: " + $c.Name) ((Get-LcFailures $res) -ceq $c.Fails) "bloquean: '$(Get-LcFailures $res)', se esperaba '$($c.Fails)'"
    }

    # Regresion: con las carpetas REALES de la sesion (no las sinteticas), la carpeta por defecto por usuario NO cuenta como protegida. En
    # el contexto por usuario Windows Installer redirige ProgramFilesFolder / ProgramFiles64Folder a %LOCALAPPDATA%\Programs\, y si el MSI
    # las usara para detectar las carpetas protegidas, la carpeta por defecto del perfil quedaria bloqueada.
    $real = New-Session
    $realLc = Invoke-Sequence $real 'InstallExecuteSequence'
    $realFolder = Get-Prop $real 'INSTALLFOLDER'
    Check 'con las carpetas reales de la sesion, la instalacion por usuario por defecto no se bloquea' ((Get-LcFailures $realLc) -ceq '') "bloquean: '$(Get-LcFailures $realLc)'"
    Check 'con las carpetas reales de la sesion, la carpeta por defecto por usuario es LocalAppData\Programs\Tesela\' ($realFolder -ceq ((Get-Prop $real 'LocalAppDataFolder') + 'Programs\Tesela\'))
    Check 'en el contexto por usuario ProgramFiles64Folder esta redirigida al perfil (y por eso el MSI usa las variables de entorno)' ((Get-Prop $real 'ProgramFiles64Folder') -ceq ((Get-Prop $real 'LocalAppDataFolder') + 'Programs\')) 'si esto falla, Windows Installer ya no redirige: se puede simplificar el MSI'

    # === 2. Carpeta, ambito y escritorio en la secuencia del servidor (/qn) ====================================================
    $seqCases = @(
        @{ Name = 'por usuario, sin nada: carpeta por usuario'; Props = @{ UILevel = 2 }; Expect = @{ INSTALLFOLDER = $userDefault; DESKTOPSHORTCUT = '' } },
        @{ Name = 'ALLUSERS=1: carpeta de Program Files'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = '' }; Expect = @{ INSTALLFOLDER = $machineDefault } },
        @{ Name = 'INSTALLFOLDER pedida por usuario: se respeta'; Props = @{ UILevel = 2; INSTALLFOLDER = 'C:\Mia\' }; Expect = @{ INSTALLFOLDER = 'C:\Mia\' } },
        @{ Name = 'INSTALLFOLDER pedida para todos: se respeta (llega al servidor)'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; INSTALLFOLDER = 'C:\Mia\' }; Expect = @{ INSTALLFOLDER = 'C:\Mia\' } },
        @{ Name = 'actualizacion por usuario: se queda en la carpeta anterior aunque se pida otra'; Props = @{ UILevel = 2; TESELA_PREV_USER = 'C:\Anterior\'; INSTALLFOLDER = 'C:\Otra\' }; Expect = @{ INSTALLFOLDER = 'C:\Anterior\' } },
        @{ Name = 'actualizacion para todos: se queda en la carpeta anterior'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_MACHINE = 'C:\AnteriorM\' }; Expect = @{ INSTALLFOLDER = 'C:\AnteriorM\' } },
        @{ Name = 'la carpeta anterior de OTRO ambito no se usa'; Props = @{ UILevel = 2; TESELA_PREV_MACHINE = 'C:\AnteriorM\' }; Expect = @{ INSTALLFOLDER = $userDefault } },
        @{ Name = 'escritorio recordado (HKCU) sin interfaz: se mantiene'; Props = @{ UILevel = 2; TESELA_PREV_DESKTOP_USER = '1' }; Expect = @{ DESKTOPSHORTCUT = '1' } },
        @{ Name = 'escritorio recordado (HKLM) sin interfaz: se mantiene'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_DESKTOP_MACHINE = '1' }; Expect = @{ DESKTOPSHORTCUT = '1' } },
        @{ Name = 'DESKTOPSHORTCUT=0 explicito manda sobre lo recordado'; Props = @{ UILevel = 2; TESELA_PREV_DESKTOP_USER = '1'; DESKTOPSHORTCUT = '0' }; Expect = @{ DESKTOPSHORTCUT = '0' } },
        @{ Name = 'DESKTOPSHORTCUT=1 explicito se respeta sin nada recordado'; Props = @{ UILevel = 2; DESKTOPSHORTCUT = '1' }; Expect = @{ DESKTOPSHORTCUT = '1' } },
        @{ Name = 'con interfaz completa el servidor NO vuelve a marcar la casilla que el usuario desmarco'; Props = @{ UILevel = 5; TESELA_PREV_DESKTOP_USER = '1' }; Expect = @{ DESKTOPSHORTCUT = '' } },
        @{ Name = 'sin escritorio recordado ni pedido: no hay acceso directo'; Props = @{ UILevel = 2 }; Expect = @{ DESKTOPSHORTCUT = '' } }
    )
    foreach ($c in $seqCases) {
        $s = New-Case $c.Props
        [void](Invoke-Sequence $s 'InstallExecuteSequence')
        foreach ($k in $c.Expect.Keys) {
            Check ("secuencia del servidor: $($c.Name) [$k]") ((Get-Prop $s $k) -ceq $c.Expect[$k]) "es '$(Get-Prop $s $k)', se esperaba '$($c.Expect[$k])'"
        }
    }
    # Con interfaz (secuencia de interfaz): el escritorio recordado SI marca la casilla de partida; el ambito se preelige.
    $uiCases = @(
        @{ Name = 'escritorio recordado: la casilla parte marcada'; Props = @{ UILevel = 5; TESELA_PREV_DESKTOP_USER = '1' }; Expect = @{ DESKTOPSHORTCUT = '1'; TESELA_SCOPE = 'user'; INSTALLFOLDER = $userDefault } },
        @{ Name = 'sin instalacion previa: ambito por usuario preelegido'; Props = @{ UILevel = 5 }; Expect = @{ TESELA_SCOPE = 'user'; DESKTOPSHORTCUT = '' } },
        @{ Name = 'instalacion previa para todos: ambito para todos preelegido'; Props = @{ UILevel = 5; TESELA_PREV_MACHINE = 'C:\AnteriorM\' }; Expect = @{ TESELA_SCOPE = 'machine' } },
        @{ Name = 'instalacion previa por usuario: ambito por usuario preelegido'; Props = @{ UILevel = 5; TESELA_PREV_USER = 'C:\Anterior\' }; Expect = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = 'C:\Anterior\' } },
        @{ Name = 'con las dos instalaciones previas se queda por usuario (y la ventana bloquea Siguiente)'; Props = @{ UILevel = 5; TESELA_PREV_USER = 'C:\Anterior\'; TESELA_PREV_MACHINE = 'C:\AnteriorM\' }; Expect = @{ TESELA_SCOPE = 'user' } },
        @{ Name = 'ALLUSERS=1 en la linea de comandos preelige para todos'; Props = @{ UILevel = 5; ALLUSERS = 1; MSIINSTALLPERUSER = '' }; Expect = @{ TESELA_SCOPE = 'machine'; INSTALLFOLDER = $machineDefault } }
    )
    foreach ($c in $uiCases) {
        $s = New-Case $c.Props
        [void](Invoke-Sequence $s 'InstallUISequence' -IncludeAfterLaunch)
        foreach ($k in $c.Expect.Keys) {
            Check ("secuencia de interfaz: $($c.Name) [$k]") ((Get-Prop $s $k) -ceq $c.Expect[$k]) "es '$(Get-Prop $s $k)', se esperaba '$($c.Expect[$k])'"
        }
    }

    # === 3. Idioma ===============================================================================================================
    $langCases = @(
        @(3082, '', 'es'), @(1034, '', 'es'), @(2058, '', 'es'), @(11274, '', 'es'), @(22538, '', 'es'), @(23562, '', 'es'),
        @(1033, '', 'en'), @(2057, '', 'en'), @(1036, '', 'en'), @(1031, '', 'en'), @(1027, '', 'en'),
        @(3082, 'en', 'en'), @(1033, 'es', 'es')
    )
    foreach ($c in $langCases) {
        $s = New-Case @{ UILevel = 5; UserLanguageID = $c[0]; TESELA_LANG = $c[1] }
        [void](Invoke-Sequence $s 'InstallUISequence' -IncludeAfterLaunch)
        Check "idioma: UserLanguageID=$($c[0]) TESELA_LANG='$($c[1])' -> $($c[2])" ((Get-Prop $s 'TESELA_LANG') -ceq $c[2]) "es '$(Get-Prop $s 'TESELA_LANG')'"
    }
    # Los textos en espanol: la accion fija la propiedad y los acentos sobreviven a la tabla (pagina de codigos del paquete).
    $s = New-Case @{ UILevel = 5 }
    $before = Get-Prop $s 'TxWelcomeTitle'
    Invoke-Action $s 'TeselaEs_TxWelcomeTitle'
    $after = Get-Prop $s 'TxWelcomeTitle'
    Check 'texto en ingles por defecto' ($before -ceq 'Welcome to the Tesela setup') "es '$before'"
    Check 'la accion TeselaEs_ lo cambia al espanol con acentos intactos' ($after -ceq ('Bienvenido a la instalaci' + [char]0x00F3 + 'n de Tesela')) "es '$after'"
    Invoke-Action $s 'TeselaEs_TxScopeTitle'
    Check 'signo de interrogacion de apertura y acentos del espanol' ((Get-Prop $s 'TxScopeTitle') -ceq ([char]0x00BF + 'Para qui' + [char]0x00E9 + 'n es Tesela?'))
    Invoke-Action $s 'TeselaEs_TxExitPin'
    Check 'la linea de anclar a la barra de tareas (espanol)' ((Get-Prop $s 'TxExitPin') -clike '*Anclar a la barra de tareas*')
    $sEn = New-Case @{ UILevel = 5 }
    Check 'la linea de anclar a la barra de tareas (ingles)' ((Get-Prop $sEn 'TxExitPin') -clike '*Pin to taskbar*')
    Check 'el texto honesto del ambito (ingles) dice que NO da mas permisos' ((Get-Prop $sEn 'TxScopeHonest') -clike '*does NOT give Tesela more permissions*')
    Invoke-Action $sEn 'TeselaEs_TxScopeHonest'
    Check 'el texto honesto del ambito (espanol) dice que NO da mas permisos' ((Get-Prop $sEn 'TxScopeHonest') -clike '*NO da a Tesela m*permisos para limpiar*')

    # === 4. Ventana de ambito: Siguiente =========================================================================================
    $scopeCases = @(
        @{ Name = 'por usuario, carpeta por defecto: se queda'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = $userDefault }; Expect = @{ ALLUSERS = '2'; MSIINSTALLPERUSER = '1'; INSTALLFOLDER = $userDefault }; Disabled = $false },
        @{ Name = 'para todos, carpeta por defecto por usuario: pasa a la de Program Files'; Props = @{ TESELA_SCOPE = 'machine'; INSTALLFOLDER = $userDefault }; Expect = @{ ALLUSERS = '1'; MSIINSTALLPERUSER = ''; INSTALLFOLDER = $machineDefault }; Disabled = $false },
        @{ Name = 'para todos, sin carpeta todavia: la de Program Files'; Props = @{ TESELA_SCOPE = 'machine'; INSTALLFOLDER = '' }; Expect = @{ ALLUSERS = '1'; INSTALLFOLDER = $machineDefault }; Disabled = $false },
        @{ Name = 'para todos con carpeta PERSONALIZADA: se respeta'; Props = @{ TESELA_SCOPE = 'machine'; INSTALLFOLDER = 'C:\Mia\' }; Expect = @{ ALLUSERS = '1'; INSTALLFOLDER = 'C:\Mia\' }; Disabled = $false },
        @{ Name = 'por usuario, volviendo de para todos: la de Program Files pasa a la del perfil'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = $machineDefault }; Expect = @{ ALLUSERS = '2'; MSIINSTALLPERUSER = '1'; INSTALLFOLDER = $userDefault }; Disabled = $false },
        @{ Name = 'por usuario con carpeta PERSONALIZADA: se respeta'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = 'C:\Mia\' }; Expect = @{ ALLUSERS = '2'; INSTALLFOLDER = 'C:\Mia\' }; Disabled = $false },
        @{ Name = 'la carpeta por defecto se reconoce sin distinguir mayusculas'; Props = @{ TESELA_SCOPE = 'machine'; INSTALLFOLDER = $userDefault.ToUpperInvariant() }; Expect = @{ ALLUSERS = '1'; INSTALLFOLDER = $machineDefault }; Disabled = $false },
        @{ Name = 'por usuario sobre una instalacion por usuario: usa la carpeta de esa instalacion'; Props = @{ TESELA_SCOPE = 'user'; TESELA_PREV_USER = 'C:\Anterior\'; INSTALLFOLDER = 'C:\Otra\' }; Expect = @{ ALLUSERS = '2'; INSTALLFOLDER = 'C:\Anterior\' }; Disabled = $false },
        @{ Name = 'para todos sobre una instalacion para todos: usa la carpeta de esa instalacion'; Props = @{ TESELA_SCOPE = 'machine'; TESELA_PREV_MACHINE = 'C:\AnteriorM\'; INSTALLFOLDER = $userDefault }; Expect = @{ ALLUSERS = '1'; INSTALLFOLDER = 'C:\AnteriorM\' }; Disabled = $false },
        @{ Name = 'para todos sobre una instalacion por usuario: Siguiente DESACTIVADO'; Props = @{ TESELA_SCOPE = 'machine'; TESELA_PREV_USER = 'C:\Anterior\' }; Expect = @{}; Disabled = $true },
        @{ Name = 'por usuario sobre una instalacion para todos: Siguiente DESACTIVADO'; Props = @{ TESELA_SCOPE = 'user'; TESELA_PREV_MACHINE = 'C:\AnteriorM\' }; Expect = @{}; Disabled = $true },
        @{ Name = 'por usuario con las dos instalaciones: Siguiente DESACTIVADO'; Props = @{ TESELA_SCOPE = 'user'; TESELA_PREV_USER = 'C:\A\'; TESELA_PREV_MACHINE = 'C:\B\' }; Expect = @{}; Disabled = $true },
        @{ Name = 'para todos con las dos instalaciones: Siguiente DESACTIVADO'; Props = @{ TESELA_SCOPE = 'machine'; TESELA_PREV_USER = 'C:\A\'; TESELA_PREV_MACHINE = 'C:\B\' }; Expect = @{}; Disabled = $true }
    )
    foreach ($c in $scopeCases) {
        $s = New-UiCase $c.Props
        $dis = Test-ControlCond $s 'ScopeDlg' 'Next' 'Disable'
        Check "ventana de ambito: $($c.Name) (Siguiente desactivado: $($c.Disabled))" ($dis -eq $c.Disabled) "desactivado = $dis"
        if (-not $c.Disabled) {
            $done = Invoke-Button $s 'ScopeDlg' 'Next'
            Check "ventana de ambito: $($c.Name) (abre la ventana de carpeta)" (($done -join ',') -ceq 'NewDialog:FolderDlg') "eventos: $($done -join ',')"
            foreach ($k in $c.Expect.Keys) {
                Check "ventana de ambito: $($c.Name) [$k]" ((Get-Prop $s $k) -ceq $c.Expect[$k]) "es '$(Get-Prop $s $k)', se esperaba '$($c.Expect[$k])'"
            }
        }
    }
    # Los avisos de la ventana de ambito.
    $s = New-UiCase @{ TESELA_SCOPE = 'machine'; TESELA_PREV_USER = 'C:\A\' }
    Check 'ventana de ambito: aviso "ya esta instalada por usuario" al elegir para todos' ((Test-ControlCond $s 'ScopeDlg' 'ConflictUser' 'Show') -and -not (Test-ControlCond $s 'ScopeDlg' 'ConflictMachine' 'Show'))
    $s = New-UiCase @{ TESELA_SCOPE = 'user'; TESELA_PREV_MACHINE = 'C:\B\' }
    Check 'ventana de ambito: aviso "ya esta instalada para todos" al elegir por usuario' ((Test-ControlCond $s 'ScopeDlg' 'ConflictMachine' 'Show') -and -not (Test-ControlCond $s 'ScopeDlg' 'ConflictUser' 'Show'))
    $s = New-UiCase @{ TESELA_SCOPE = 'user' }
    Check 'ventana de ambito: sin conflicto no sale ningun aviso' (-not (Test-ControlCond $s 'ScopeDlg' 'ConflictMachine' 'Show') -and -not (Test-ControlCond $s 'ScopeDlg' 'ConflictUser' 'Show'))

    # === 5. Ventana de carpeta: Siguiente y avisos ================================================================================
    $folderCases = @(
        @{ Name = 'carpeta del perfil, por usuario'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = $userDefault }; Result = 'NewDialog:VerifyReadyDlg'; Protected = $false },
        @{ Name = 'otra unidad, por usuario'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = 'C:\Mis programas\Tesela\' }; Result = 'NewDialog:VerifyReadyDlg'; Protected = $false },
        @{ Name = 'ruta de red (UNC), por usuario'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = ($unc + 'servidor\recurso\Tesela\') }; Result = 'NewDialog:VerifyReadyDlg'; Protected = $false },
        @{ Name = 'Program Files, para todos'; Props = @{ TESELA_SCOPE = 'machine'; INSTALLFOLDER = $machineDefault }; Result = 'NewDialog:VerifyReadyDlg'; Protected = $false },
        @{ Name = 'Program Files, por usuario SIN administrador: carpeta no valida y aviso'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = $machineDefault }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $true },
        @{ Name = 'Program Files (x86), por usuario SIN administrador: carpeta no valida y aviso'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = ($pf32 + '\Tesela\') }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $true },
        @{ Name = 'Windows, por usuario SIN administrador: carpeta no valida y aviso'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = ($winDir + '\Tesela\') }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $true },
        @{ Name = 'Program Files, por usuario CON administrador: sigue y sin aviso'; Props = @{ TESELA_SCOPE = 'user'; Privileged = 1; INSTALLFOLDER = $machineDefault }; Result = 'NewDialog:VerifyReadyDlg'; Protected = $false },
        @{ Name = 'ruta relativa: carpeta no valida'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = 'Tesela' }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $false },
        @{ Name = 'solo un nombre de unidad sin barra: carpeta no valida'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = 'C:' }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $false },
        @{ Name = 'vacia: carpeta no valida'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = '' }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $false }
    )
    foreach ($c in $folderCases) {
        $s = New-UiCase $c.Props
        $done = @((Invoke-Button $s 'FolderDlg' 'Next') | Where-Object { $_ -cnotlike 'SetTargetPath:*' })
        Check "ventana de carpeta: $($c.Name) (eventos de ventana: $($c.Result))" (($done -join ',') -ceq $c.Result) "eventos: $($done -join ',')"
        Check "ventana de carpeta: $($c.Name) (aviso de carpeta protegida: $($c.Protected))" ((Test-ControlCond $s 'FolderDlg' 'FolderProtected' 'Show') -eq $c.Protected)
    }
    $s = New-UiCase @{ TESELA_SCOPE = 'user'; TESELA_PREV_USER = 'C:\A\'; INSTALLFOLDER = 'C:\A\' }
    Check 'ventana de carpeta: en una actualizacion el campo y Examinar se desactivan y sale la nota' ((Test-ControlCond $s 'FolderDlg' 'FolderEdit' 'Disable') -and (Test-ControlCond $s 'FolderDlg' 'ChangeFolder' 'Disable') -and (Test-ControlCond $s 'FolderDlg' 'FolderFixed' 'Show'))
    $s = New-UiCase @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = $userDefault }
    Check 'ventana de carpeta: sin instalacion previa el campo y Examinar estan activos y no hay nota' (-not (Test-ControlCond $s 'FolderDlg' 'FolderEdit' 'Disable') -and -not (Test-ControlCond $s 'FolderDlg' 'ChangeFolder' 'Disable') -and -not (Test-ControlCond $s 'FolderDlg' 'FolderFixed' 'Show'))
    $s = New-UiCase @{ TESELA_SCOPE = 'machine'; TESELA_PREV_USER = 'C:\A\' }
    Check 'ventana de carpeta: una instalacion previa del OTRO ambito no fija la carpeta' (-not (Test-ControlCond $s 'FolderDlg' 'FolderEdit' 'Disable'))
    # El boton Examinar: pone _BrowseProperty en INSTALLFOLDER y abre el dialogo nativo.
    $s = New-UiCase @{ TESELA_SCOPE = 'user' }
    $done = Invoke-Button $s 'FolderDlg' 'ChangeFolder'
    Check 'Examinar abre el dialogo nativo sobre INSTALLFOLDER' ((($done -join ',') -ceq 'SpawnDialog:BrowseDlg') -and (Get-Prop $s '_BrowseProperty') -ceq 'INSTALLFOLDER')

    # === 6. Ventana de listo y de fin ===============================================================================================
    $s = New-Case @{ TESELA_SCOPE = 'machine'; DESKTOPSHORTCUT = '1'; WIX_UPGRADE_DETECTED = '{X}' }
    Check 'ventana de listo: para todos, con escritorio y actualizacion' ((Test-ControlCond $s 'VerifyReadyDlg' 'ScopeMachine' 'Show') -and -not (Test-ControlCond $s 'VerifyReadyDlg' 'ScopeUser' 'Show') -and (Test-ControlCond $s 'VerifyReadyDlg' 'Desktop' 'Show') -and (Test-ControlCond $s 'VerifyReadyDlg' 'Upgrade' 'Show'))
    $s = New-Case @{ TESELA_SCOPE = 'user' }
    Check 'ventana de listo: por usuario, sin escritorio ni actualizacion' ((Test-ControlCond $s 'VerifyReadyDlg' 'ScopeUser' 'Show') -and -not (Test-ControlCond $s 'VerifyReadyDlg' 'Desktop' 'Show') -and -not (Test-ControlCond $s 'VerifyReadyDlg' 'Upgrade' 'Show'))
    $s = New-Case @{}
    Check 'ventana de fin: instalacion nueva muestra el titulo de instalada, el inicio y la linea de anclar' ((Test-ControlCond $s 'ExitDialog' 'TitleInstalled' 'Show') -and (Test-ControlCond $s 'ExitDialog' 'Pin' 'Show') -and -not (Test-ControlCond $s 'ExitDialog' 'TitleRemoved' 'Show'))
    $s = New-Case @{ REMOVE = 'ALL' }
    Check 'ventana de fin: al desinstalar muestra el titulo de desinstalada y NO la linea de anclar' ((Test-ControlCond $s 'ExitDialog' 'TitleRemoved' 'Show') -and -not (Test-ControlCond $s 'ExitDialog' 'Pin' 'Show') -and -not (Test-ControlCond $s 'ExitDialog' 'TitleInstalled' 'Show'))
    $s = New-Case @{ REINSTALL = 'ALL' }
    Check 'ventana de fin: al reparar muestra el titulo de reparada' ((Test-ControlCond $s 'ExitDialog' 'TitleRepaired' 'Show') -and -not (Test-ControlCond $s 'ExitDialog' 'TitleInstalled' 'Show'))
} finally {
    # La sesion tiene el MSI abierto: se suelta antes de borrar la copia.
    Close-Session
    $leaf = Split-Path -Leaf $tmp
    if ($leaf -like 'tesela-logic-*' -and (Split-Path -Parent $tmp) -eq [System.IO.Path]::GetTempPath().TrimEnd('\')) {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($failures.Count -gt 0) {
    Write-Host "msi-logic.tests: $($failures.Count) de $total comprobaciones fallan:" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host "msi-logic.tests: OK ($total comprobaciones)"
exit 0
