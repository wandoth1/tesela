<#
.SYNOPSIS
  Test Tesela MSI LOGIC (launch conditions, directory, scope, language, and desktop actions, plus scope and directory
  dialog events) in a Windows Installer SESSION WITHOUT INSTALLING ANYTHING.

.DESCRIPTION
  Opens the MSI with `Installer.OpenPackage(path, 1)` (1 = ignore machine state), creating a session with package
  properties without running a sequence, msiexec, UI, or requesting administrator rights. Using SYNTHETIC properties
  (never reads the registry or real directories), it calls Windows Installer's condition evaluator (`EvaluateCondition`)
  and runs type 51 actions with `DoAction` (they only set properties). It then:

    * walks the sequences (InstallExecuteSequence and InstallUISequence rows, in sequence-number order) and checks the
      directory, scope, desktop shortcut, and language selected in each case, as well as which launch conditions pass or fail;
    * walks the Next-button control events in the scope and directory dialogs as the UI would (conditions evaluated by
      Windows Installer, formatted values) and checks the context (ALLUSERS, MSIINSTALLPERUSER), resulting directory,
      protected-directory warning, and whether Next is blocked;
    * checks that Spanish UI strings (including accents) survive the MSI tables and that the language is selected correctly.

  Does NOT replace testing the real UI (rendering, tab order, control behavior), the engine's behavior when ALLUSERS
  changes mid-UI, or anything that requires a real installation (covered by scripts/tests/msi-install.tests.ps1 in CI
  and by a person).

.PARAMETER Msi
  The MSI just built by scripts/build-msi.ps1.

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
if (-not (Test-Path -LiteralPath $Msi -PathType Leaf)) { throw "MSI does not exist: $Msi" }

# Work on a COPY under %TEMP% (the session does not modify the MSI, but this ensures the original is never touched).
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

# --- Session ---------------------------------------------------------------------------------------------------------------------
$script:current = $null
function Close-Session {
    # Windows Installer allows only ONE open session per process: release the previous one before opening another.
    if ($null -ne $script:current) {
        try { [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($script:current) } catch { }
        $script:current = $null
    }
    [System.GC]::Collect()
    [System.GC]::WaitForPendingFinalizers()
}
function New-Session {
    # [string] and [int]: passing the Join-Path result directly causes DISP_E_TYPEMISMATCH.
    Close-Session
    $script:current = [System.__ComObject].InvokeMember('OpenPackage', 'InvokeMethod', $null, $installer, @([string]$copy, [int]1))
    return $script:current
}
function Get-Prop($S, [string]$Name) { return [string][System.__ComObject].InvokeMember('Property', 'GetProperty', $null, $S, @($Name)) }
function Set-Prop($S, [string]$Name, [string]$Value) { [System.__ComObject].InvokeMember('Property', 'SetProperty', $null, $S, @($Name, $Value)) | Out-Null }
function Test-Cond($S, [string]$Condition) {
    # 0 = false, 1 = true, 2 = no condition, 3 = syntax error. An empty condition is true.
    if ([string]::IsNullOrEmpty($Condition)) { return $true }
    $r = [int][System.__ComObject].InvokeMember('EvaluateCondition', 'InvokeMethod', $null, $S, @($Condition))
    if ($r -ge 2) { throw "Could not evaluate condition (code $r): $Condition" }
    return ($r -eq 1)
}
function Invoke-Action($S, [string]$Name) {
    $r = [int][System.__ComObject].InvokeMember('DoAction', 'InvokeMethod', $null, $S, @($Name))
    if ($r -ne 1) { throw "Action $Name returned $r" }
}
function Format-Arg($S, [string]$Arg) {
    # `[Property]` -> its value; `{}` -> empty (clears the property).
    if ($Arg -ceq '{}') { return '' }
    return [regex]::Replace($Arg, '\[([A-Za-z0-9_]+)\]', { param($m) Get-Prop $S $m.Groups[1].Value })
}

# --- MSI tables (read once) -------------------------------------------------------------------------------------------------------
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

# --- Simulate a sequence and dialog buttons ---------------------------------------------------------------------------------------
# The profile is SYNTHETIC (never the real one). System directories come from environment variables, as in the MSI. In
# per-user context Windows Installer redirects ProgramFilesFolder to the profile, which is why the MSI does not use it.
$synthetic = @{ LocalAppDataFolder = 'C:\Synth\Local\' }
$userDefault = 'C:\Synth\Local\Programs\Tesela\'
$pf64 = $env:ProgramW6432
$pf32 = $pf64 + ' (x86)'   # As in the MSI: ICE03 does not allow %ProgramFiles(x86)% inside [%...].
$winDir = $env:SystemRoot
if (-not $pf64 -or -not $winDir) { throw 'Missing ProgramW6432 or SystemRoot environment variables (64-bit Windows is required)' }
$machineDefault = $pf64 + '\Tesela\'
$unc = '\' * 2   # Network-path prefix, constructed rather than written (privacy-check rejects literal network paths).

function New-Case([hashtable]$Props) {
    # New session with synthetic directories and case-specific properties. All other properties come from the package.
    $s = New-Session
    foreach ($k in $synthetic.Keys) { Set-Prop $s $k $synthetic[$k] }
    foreach ($k in $Props.Keys) { Set-Prop $s $k ([string]$Props[$k]) }
    # A real previous install has tesela.exe in its directory (checked by TESELA_PREV_*_EXE); a case can set
    # TESELA_PREV_USER_EXE='' to test a registry key with NO product.
    foreach ($pair in @(@('TESELA_PREV_USER', 'TESELA_PREV_USER_EXE'), @('TESELA_PREV_MACHINE', 'TESELA_PREV_MACHINE_EXE'))) {
        if ($Props.ContainsKey($pair[0]) -and $Props[$pair[0]] -and -not $Props.ContainsKey($pair[1])) { Set-Prop $s $pair[1] ([string]$Props[$pair[0]] + 'tesela.exe') }
    }
    return $s
}
function New-UiCase([hashtable]$Props) {
    # Property state when dialogs open: startup actions that set default and protected directories (TESELA_DEFAULT_*,
    # TESELA_PF64, TESELA_WIN) have already run. Other properties (selected scope, directory, previous installs) come from the case.
    $s = New-Case $Props
    foreach ($a in 'TeselaSetDefaultUser', 'TeselaSetDefaultMachine', 'TeselaSetProtected64', 'TeselaSetProtectedWin') { Invoke-Action $s $a }
    return $s
}
function Invoke-Sequence($S, [string]$Table, [switch]$IncludeAfterLaunch) {
    # Run non-text `Tesela*` actions whose conditions are true, in sequence order. Run actions before LaunchConditions
    # (sequence 100) first; then return which launch conditions PASS (booleans in LaunchCondition table order) and, when
    # requested, run later actions (UI scope and language).
    $rows = @($seqRows[$Table] | Where-Object { $_.Action -clike 'Tesela*' -and $_.Action -cnotlike 'TeselaEs_*' } | Sort-Object Sequence)
    foreach ($r in ($rows | Where-Object { $_.Sequence -lt 100 })) { if (Test-Cond $S $r.Condition) { Invoke-Action $S $r.Action } }
    $lc = @($launch | ForEach-Object { Test-Cond $S $_.Condition })
    if ($IncludeAfterLaunch) {
        foreach ($r in ($rows | Where-Object { $_.Sequence -gt 100 })) { if (Test-Cond $S $r.Condition) { Invoke-Action $S $r.Action } }
    }
    return , $lc
}
function Invoke-Button($S, [string]$Dialog, [string]$Control) {
    # Button events in order, each with its condition evaluated by Windows Installer. Return the dialog events
    # (NewDialog:X, SpawnDialog:X, ...) that would run; property events update the session.
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
    # Launch condition table order (for reading results): the newer-version block followed by the three scope checks.
    $lcNames = @($launch | ForEach-Object {
            if ($_.Condition -ceq 'NOT WIX_DOWNGRADE_DETECTED') { 'downgrade' }
            elseif ($_.Condition -clike '*TESELA_PREV_USER=""') { 'user->machine' }
            elseif ($_.Condition -clike '*TESELA_PREV_MACHINE=""*') { 'machine->user' }
            elseif ($_.Condition -clike '*TESELA_WIN*') { 'protected' }
            elseif ($_.Condition -clike '*INSTALLFOLDER><":*') { 'format' }
            else { 'unrecognized' }
        })
    Check "all five launch conditions are recognized ($($lcNames -join ', '))" ((($lcNames | Sort-Object) -join ',') -ceq 'downgrade,format,machine->user,protected,user->machine')
    function Get-LcFailures($Result) {
        $bad = @()
        for ($i = 0; $i -lt $lcNames.Count; $i++) { if (-not $Result[$i]) { $bad += $lcNames[$i] } }
        return ($bad | Sort-Object) -join ','
    }

    # === 1. Launch conditions without UI (UILevel 2): what blocks installation ========================================
    $lcCases = @(
        @{ Name = 'por usuario, sin instalacion previa: todo pasa'; Props = @{ UILevel = 2 }; Fails = '' },
        @{ Name = 'para todos (ALLUSERS=1), sin instalacion previa: todo pasa'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = '' }; Fails = '' },
        @{ Name = 'por usuario sobre una instalacion por usuario: pasa'; Props = @{ UILevel = 2; TESELA_PREV_USER = 'C:\Prev\' }; Fails = '' },
        @{ Name = 'para todos sobre una instalacion para todos: pasa'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_MACHINE = 'C:\PrevM\' }; Fails = '' },
        @{ Name = 'para todos (ALLUSERS=1) sobre una instalacion por usuario: BLOQUEA'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_USER = 'C:\Prev\' }; Fails = 'user->machine' },
        @{ Name = 'ALLUSERS=2 con MSIINSTALLPERUSER vacio (para todos) sobre una instalacion por usuario: BLOQUEA'; Props = @{ UILevel = 2; ALLUSERS = 2; MSIINSTALLPERUSER = ''; TESELA_PREV_USER = 'C:\Prev\' }; Fails = 'user->machine' },
        @{ Name = 'por usuario sobre una instalacion para todos: BLOQUEA'; Props = @{ UILevel = 2; TESELA_PREV_MACHINE = 'C:\PrevM\' }; Fails = 'machine->user' },
        @{ Name = 'M2: con las dos (la suya por usuario y la de otra cuenta para todos), por usuario PUEDE actualizar la suya'; Props = @{ UILevel = 2; TESELA_PREV_USER = 'C:\Prev\'; TESELA_PREV_MACHINE = 'C:\PrevM\' }; Fails = '' },
        @{ Name = 'con las dos, pedir para todos (ALLUSERS=1) sigue bloqueado por la suya por usuario'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_USER = 'C:\Prev\'; TESELA_PREV_MACHINE = 'C:\PrevM\' }; Fails = 'user->machine' },
        @{ Name = 'm6: una clave de HKCU sin tesela.exe (restos) NO bloquea pasar a para todos'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_USER = 'C:\Resto\'; TESELA_PREV_USER_EXE = '' }; Fails = '' },
        @{ Name = 'm6: una clave de HKLM sin tesela.exe (restos) NO bloquea la instalacion por usuario'; Props = @{ UILevel = 2; TESELA_PREV_MACHINE = 'C:\Resto\'; TESELA_PREV_MACHINE_EXE = '' }; Fails = '' },
        @{ Name = 'con interfaz completa (UILevel 5) las ventanas avisan: no bloquea'; Props = @{ UILevel = 5; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_USER = 'C:\Prev\' }; Fails = '' },
        @{ Name = 'reparar o desinstalar (Installed) nunca se bloquea por el ambito'; Props = @{ UILevel = 2; Installed = '00:00:00'; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_USER = 'C:\Prev\' }; Fails = '' },
        @{ Name = 'una version posterior instalada bloquea la bajada'; Props = @{ UILevel = 2; WIX_DOWNGRADE_DETECTED = '{11111111-2222-3333-4444-555555555555}' }; Fails = 'downgrade' },
        @{ Name = 'carpeta protegida (Program Files) por usuario: BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = $machineDefault }; Fails = 'protected' },
        @{ Name = 'carpeta protegida (Program Files x86) por usuario: BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = ($pf32 + '\Tesela\') }; Fails = 'protected' },
        @{ Name = 'carpeta protegida (Windows) por usuario: BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = ($winDir + '\Tesela\') }; Fails = 'protected' },
        @{ Name = 'carpeta protegida sin distinguir mayusculas: BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = ($machineDefault.ToLowerInvariant()) }; Fails = 'protected' },
        @{ Name = 'carpeta protegida por usuario BLOQUEA tambien con permisos de administrador (Privileged): se usa ALLUSERS=1'; Props = @{ UILevel = 2; Privileged = 1; INSTALLFOLDER = $machineDefault }; Fails = 'protected' },
        @{ Name = 'carpeta de Program Files para todos (ALLUSERS=1): pasa'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; INSTALLFOLDER = $machineDefault }; Fails = '' },
        @{ Name = 'carpeta del perfil por usuario: pasa'; Props = @{ UILevel = 2; INSTALLFOLDER = 'C:\Synth\Local\Programs\Tesela\' }; Fails = '' },
        @{ Name = 'otra unidad por usuario: pasa'; Props = @{ UILevel = 2; INSTALLFOLDER = 'C:\Mis programas\Tesela\' }; Fails = '' },
        @{ Name = 'm1: Program Files SIN barra final: BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = $pf64 }; Fails = 'protected' },
        @{ Name = 'm1: Windows SIN barra final: BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = $winDir }; Fails = 'protected' },
        @{ Name = 'm1: Program Files (Arm) (ARM64): BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = ($pf64 + ' (Arm)\Tesela\') }; Fails = 'protected' },
        @{ Name = 'm1: Program Files en mayusculas: BLOQUEA'; Props = @{ UILevel = 2; INSTALLFOLDER = ($pf64.ToUpperInvariant() + '\TESELA\') }; Fails = 'protected' },
        @{ Name = 'm1 (coste aceptado): una carpeta cuyo NOMBRE empieza por Program Files (Program Files2) tambien se trata como protegida'; Props = @{ UILevel = 2; INSTALLFOLDER = ($pf64 + '2\Tesela\') }; Fails = 'protected' },
        @{ Name = 'm1: barras normales (C:/Program Files/...): formato no valido'; Props = @{ UILevel = 2; INSTALLFOLDER = ($pf64.Replace('\', '/') + '/Tesela/') }; Fails = 'format' },
        @{ Name = 'm1: con .. (C:\Datos\x\..\..\Program Files): formato no valido'; Props = @{ UILevel = 2; INSTALLFOLDER = 'C:\Datos\Uno\..\..\Program Files\Tesela\' }; Fails = 'format' },
        @{ Name = 'm1: ruta con prefijo ?: formato no valido'; Props = @{ UILevel = 2; INSTALLFOLDER = ($unc + '?\C:\Program Files\Tesela\') }; Fails = 'format' },
        @{ Name = 'm1: ruta de red (UNC): formato no valido'; Props = @{ UILevel = 2; INSTALLFOLDER = ($unc + 'localhost\C$\Program Files\Tesela\') }; Fails = 'format' },
        @{ Name = 'm1: ruta relativa: formato no valido'; Props = @{ UILevel = 2; INSTALLFOLDER = 'Tesela' }; Fails = 'format' },
        @{ Name = 'm1: tambien se valida el formato para todos (ALLUSERS=1) con /'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; INSTALLFOLDER = 'C:/Tesela/' }; Fails = 'format' },
        @{ Name = 'm1 (limite documentado): el nombre corto 8.3 (C:\PROGRA~1) NO se detecta sin codigo'; Props = @{ UILevel = 2; INSTALLFOLDER = 'C:\PROGRA~1\Tesela\' }; Fails = '' },
        @{ Name = 'el formato de la ruta no se exige al reparar o desinstalar (Installed)'; Props = @{ UILevel = 2; Installed = '00:00:00'; INSTALLFOLDER = 'C:/Tesela/' }; Fails = '' }
    )
    foreach ($c in $lcCases) {
        $s = New-Case $c.Props
        # The action sets the default directory (if INSTALLFOLDER is not preset): run the server sequence.
        $res = Invoke-Sequence $s 'InstallExecuteSequence'
        Check ("launch conditions: " + $c.Name) ((Get-LcFailures $res) -ceq $c.Fails) "blocked: '$(Get-LcFailures $res)', expected '$($c.Fails)'"
    }

    # Regression: with the session's REAL directories (not synthetic ones), the per-user default directory must NOT be
    # considered protected. In per-user context Windows Installer redirects ProgramFilesFolder / ProgramFiles64Folder to
    # %LOCALAPPDATA%\Programs\; if the MSI used those properties to detect protected directories, the profile default would be blocked.
    $real = New-Session
    $realLc = Invoke-Sequence $real 'InstallExecuteSequence'
    $realFolder = Get-Prop $real 'INSTALLFOLDER'
    Check 'with the session''s real directories, the default per-user install is not blocked' ((Get-LcFailures $realLc) -ceq '') "blocked: '$(Get-LcFailures $realLc)'"
    Check 'with the session''s real directories, the default per-user directory is LocalAppData\Programs\Tesela\' ($realFolder -ceq ((Get-Prop $real 'LocalAppDataFolder') + 'Programs\Tesela\'))
    Check 'in per-user context, ProgramFiles64Folder is redirected to the profile (so the MSI uses environment variables)' ((Get-Prop $real 'ProgramFiles64Folder') -ceq ((Get-Prop $real 'LocalAppDataFolder') + 'Programs\')) 'if this fails, Windows Installer no longer redirects it: the MSI can be simplified'

    # === 2. Directory, scope, and desktop shortcut in the server sequence (/qn) =======================================
    $seqCases = @(
        @{ Name = 'por usuario, sin nada: carpeta por usuario'; Props = @{ UILevel = 2 }; Expect = @{ INSTALLFOLDER = $userDefault; DESKTOPSHORTCUT = '' } },
        @{ Name = 'ALLUSERS=1: carpeta de Program Files'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = '' }; Expect = @{ INSTALLFOLDER = $machineDefault } },
        @{ Name = 'INSTALLFOLDER pedida por usuario: se respeta'; Props = @{ UILevel = 2; INSTALLFOLDER = 'C:\Mia\' }; Expect = @{ INSTALLFOLDER = 'C:\Mia\' } },
        @{ Name = 'INSTALLFOLDER pedida para todos: se respeta (llega al servidor)'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; INSTALLFOLDER = 'C:\Mia\' }; Expect = @{ INSTALLFOLDER = 'C:\Mia\' } },
        @{ Name = 'actualizacion por usuario: se queda en la carpeta anterior aunque se pida otra'; Props = @{ UILevel = 2; TESELA_PREV_USER = 'C:\Anterior\'; INSTALLFOLDER = 'C:\Otra\' }; Expect = @{ INSTALLFOLDER = 'C:\Anterior\' } },
        @{ Name = 'actualizacion para todos: se queda en la carpeta anterior'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_MACHINE = 'C:\AnteriorM\' }; Expect = @{ INSTALLFOLDER = 'C:\AnteriorM\' } },
        @{ Name = 'la carpeta anterior de OTRO ambito no se usa'; Props = @{ UILevel = 2; TESELA_PREV_MACHINE = 'C:\AnteriorM\' }; Expect = @{ INSTALLFOLDER = $userDefault } },
        @{ Name = 'escritorio recordado (HKCU) sin interfaz: se mantiene'; Props = @{ UILevel = 2; TESELA_PREV_USER = 'C:\Prev\'; TESELA_PREV_DESKTOP_USER = '1' }; Expect = @{ DESKTOPSHORTCUT = '1' } },
        @{ Name = 'm6: marca de escritorio de una clave SIN producto (sin tesela.exe): no se recuerda'; Props = @{ UILevel = 2; TESELA_PREV_USER = 'C:\Resto\'; TESELA_PREV_USER_EXE = ''; TESELA_PREV_DESKTOP_USER = '1' }; Expect = @{ DESKTOPSHORTCUT = ''; INSTALLFOLDER = $userDefault } },
        @{ Name = 'm6: una clave de carpeta SIN tesela.exe no fija la carpeta de la instalacion (se vacia y vale la de por defecto)'; Props = @{ UILevel = 2; TESELA_PREV_USER = 'C:\Resto\'; TESELA_PREV_USER_EXE = '' }; Expect = @{ INSTALLFOLDER = $userDefault; TESELA_PREV_USER = '' } },
        @{ Name = 'm6: igual para una clave de HKLM sin tesela.exe (para todos)'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_MACHINE = 'C:\Resto\'; TESELA_PREV_MACHINE_EXE = '' }; Expect = @{ INSTALLFOLDER = $machineDefault; TESELA_PREV_MACHINE = '' } },
        @{ Name = 'escritorio recordado (HKLM) sin interfaz: se mantiene'; Props = @{ UILevel = 2; ALLUSERS = 1; MSIINSTALLPERUSER = ''; TESELA_PREV_MACHINE = 'C:\PrevM\'; TESELA_PREV_DESKTOP_MACHINE = '1' }; Expect = @{ DESKTOPSHORTCUT = '1' } },
        @{ Name = 'DESKTOPSHORTCUT=0 explicito manda sobre lo recordado'; Props = @{ UILevel = 2; TESELA_PREV_USER = 'C:\Prev\'; TESELA_PREV_DESKTOP_USER = '1'; DESKTOPSHORTCUT = '0' }; Expect = @{ DESKTOPSHORTCUT = '0' } },
        @{ Name = 'DESKTOPSHORTCUT=1 explicito se respeta sin nada recordado'; Props = @{ UILevel = 2; DESKTOPSHORTCUT = '1' }; Expect = @{ DESKTOPSHORTCUT = '1' } },
        @{ Name = 'con interfaz completa el servidor NO vuelve a marcar la casilla que el usuario desmarco'; Props = @{ UILevel = 5; TESELA_PREV_USER = 'C:\Prev\'; TESELA_PREV_DESKTOP_USER = '1' }; Expect = @{ DESKTOPSHORTCUT = '' } },
        @{ Name = 'sin escritorio recordado ni pedido: no hay acceso directo'; Props = @{ UILevel = 2 }; Expect = @{ DESKTOPSHORTCUT = '' } }
    )
    foreach ($c in $seqCases) {
        $s = New-Case $c.Props
        [void](Invoke-Sequence $s 'InstallExecuteSequence')
        foreach ($k in $c.Expect.Keys) {
            Check ("server sequence: $($c.Name) [$k]") ((Get-Prop $s $k) -ceq $c.Expect[$k]) "is '$(Get-Prop $s $k)', expected '$($c.Expect[$k])'"
        }
    }
    # With UI (UI sequence): a remembered desktop shortcut DOES check the box by default; scope is preselected.
    $uiCases = @(
        @{ Name = 'escritorio recordado: la casilla parte marcada'; Props = @{ UILevel = 5; TESELA_PREV_USER = 'C:\Prev\'; TESELA_PREV_DESKTOP_USER = '1' }; Expect = @{ DESKTOPSHORTCUT = '1'; TESELA_SCOPE = 'user'; INSTALLFOLDER = 'C:\Prev\' } },
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
            Check ("UI sequence: $($c.Name) [$k]") ((Get-Prop $s $k) -ceq $c.Expect[$k]) "is '$(Get-Prop $s $k)', expected '$($c.Expect[$k])'"
        }
    }

    # === 3. Language =============================================================================================================
    $langCases = @(
        @(3082, '', 'es'), @(1034, '', 'es'), @(2058, '', 'es'), @(11274, '', 'es'), @(22538, '', 'es'), @(23562, '', 'es'),
        @(1033, '', 'en'), @(2057, '', 'en'), @(1036, '', 'en'), @(1031, '', 'en'), @(1027, '', 'en'),
        @(3082, 'en', 'en'), @(1033, 'es', 'es')
    )
    foreach ($c in $langCases) {
        $s = New-Case @{ UILevel = 5; UserLanguageID = $c[0]; TESELA_LANG = $c[1] }
        [void](Invoke-Sequence $s 'InstallUISequence' -IncludeAfterLaunch)
        Check "language: UserLanguageID=$($c[0]) TESELA_LANG='$($c[1])' -> $($c[2])" ((Get-Prop $s 'TESELA_LANG') -ceq $c[2]) "is '$(Get-Prop $s 'TESELA_LANG')'"
    }
    # Spanish UI strings: the action sets the property, and accented characters survive the package code page.
    $s = New-Case @{ UILevel = 5 }
    $before = Get-Prop $s 'TxWelcomeTitle'
    Invoke-Action $s 'TeselaEs_TxWelcomeTitle'
    $after = Get-Prop $s 'TxWelcomeTitle'
    Check 'default English text' ($before -ceq 'Welcome to the Tesela setup') "is '$before'"
    Check 'TeselaEs_ action switches it to Spanish with accents intact' ($after -ceq ('Bienvenido a la instalaci' + [char]0x00F3 + 'n de Tesela')) "is '$after'"
    Invoke-Action $s 'TeselaEs_TxScopeTitle'
    Check 'Spanish opening question mark and accents' ((Get-Prop $s 'TxScopeTitle') -ceq ([char]0x00BF + 'Para qui' + [char]0x00E9 + 'n es Tesela?'))
    Invoke-Action $s 'TeselaEs_TxExitPin'
    Check 'taskbar pinning line (Spanish)' ((Get-Prop $s 'TxExitPin') -clike '*Anclar a la barra de tareas*')
    $sEn = New-Case @{ UILevel = 5 }
    Check 'taskbar pinning line (English)' ((Get-Prop $sEn 'TxExitPin') -clike '*Pin to taskbar*')
    Check 'honest scope text (English) says it does NOT grant more permissions' ((Get-Prop $sEn 'TxScopeHonest') -clike '*does NOT give Tesela more permissions*')
    Invoke-Action $sEn 'TeselaEs_TxScopeHonest'
    Check 'honest scope text (Spanish) says it does NOT grant more permissions' ((Get-Prop $sEn 'TxScopeHonest') -clike '*NO da a Tesela m*permisos para limpiar*')

    # === 4. Scope dialog: Next ================================================================================================
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
        @{ Name = 'M2: por usuario con las dos instalaciones (la suya por usuario y otra para todos): Siguiente ACTIVO, la suya gana'; Props = @{ TESELA_SCOPE = 'user'; TESELA_PREV_USER = 'C:\A\'; TESELA_PREV_MACHINE = 'C:\B\' }; Expect = @{ ALLUSERS = '2'; INSTALLFOLDER = 'C:\A\' }; Disabled = $false },
        @{ Name = 'para todos con las dos instalaciones: Siguiente DESACTIVADO'; Props = @{ TESELA_SCOPE = 'machine'; TESELA_PREV_USER = 'C:\A\'; TESELA_PREV_MACHINE = 'C:\B\' }; Expect = @{}; Disabled = $true }
    )
    foreach ($c in $scopeCases) {
        $s = New-UiCase $c.Props
        $dis = Test-ControlCond $s 'ScopeDlg' 'Next' 'Disable'
        Check "scope dialog: $($c.Name) (Next disabled: $($c.Disabled))" ($dis -eq $c.Disabled) "disabled = $dis"
        if (-not $c.Disabled) {
            $done = Invoke-Button $s 'ScopeDlg' 'Next'
            Check "scope dialog: $($c.Name) (opens the directory dialog)" (($done -join ',') -ceq 'NewDialog:FolderDlg') "events: $($done -join ',')"
            foreach ($k in $c.Expect.Keys) {
                Check "scope dialog: $($c.Name) [$k]" ((Get-Prop $s $k) -ceq $c.Expect[$k]) "is '$(Get-Prop $s $k)', expected '$($c.Expect[$k])'"
            }
        }
    }
    # Scope dialog warnings.
    $s = New-UiCase @{ TESELA_SCOPE = 'machine'; TESELA_PREV_USER = 'C:\A\' }
    Check 'scope dialog: warns "already installed per-user" when choosing per-machine' ((Test-ControlCond $s 'ScopeDlg' 'ConflictUser' 'Show') -and -not (Test-ControlCond $s 'ScopeDlg' 'ConflictMachine' 'Show'))
    $s = New-UiCase @{ TESELA_SCOPE = 'user'; TESELA_PREV_MACHINE = 'C:\B\' }
    Check 'scope dialog: warns "already installed per-machine" when choosing per-user' ((Test-ControlCond $s 'ScopeDlg' 'ConflictMachine' 'Show') -and -not (Test-ControlCond $s 'ScopeDlg' 'ConflictUser' 'Show'))
    $s = New-UiCase @{ TESELA_SCOPE = 'user' }
    $s = New-UiCase @{ TESELA_SCOPE = 'user'; TESELA_PREV_USER = 'C:\A\'; TESELA_PREV_MACHINE = 'C:\B\' }
    Check 'scope dialog: M2, no per-machine warning when the per-user install belongs to this user' (-not (Test-ControlCond $s 'ScopeDlg' 'ConflictMachine' 'Show') -and -not (Test-ControlCond $s 'ScopeDlg' 'ConflictUser' 'Show'))
    $s = New-UiCase @{ TESELA_SCOPE = 'user' }
    Check 'scope dialog: no warning when there is no conflict' (-not (Test-ControlCond $s 'ScopeDlg' 'ConflictMachine' 'Show') -and -not (Test-ControlCond $s 'ScopeDlg' 'ConflictUser' 'Show'))

    # === 5. Directory dialog: Next and warnings ============================================================================
    $folderCases = @(
        @{ Name = 'carpeta del perfil, por usuario'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = $userDefault }; Result = 'NewDialog:VerifyReadyDlg'; Protected = $false },
        @{ Name = 'otra unidad, por usuario'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = 'C:\Mis programas\Tesela\' }; Result = 'NewDialog:VerifyReadyDlg'; Protected = $false },
        @{ Name = 'ruta de red (UNC), por usuario: ya no se admite (se esquivaria el aviso de carpeta protegida con un recurso administrativo)'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = ($unc + 'servidor\recurso\Tesela\') }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $false },
        @{ Name = 'Program Files, para todos'; Props = @{ TESELA_SCOPE = 'machine'; INSTALLFOLDER = $machineDefault }; Result = 'NewDialog:VerifyReadyDlg'; Protected = $false },
        @{ Name = 'Program Files, por usuario: carpeta no valida y aviso'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = $machineDefault }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $true },
        @{ Name = 'Program Files (x86), por usuario: carpeta no valida y aviso'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = ($pf32 + '\Tesela\') }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $true },
        @{ Name = 'Windows, por usuario: carpeta no valida y aviso'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = ($winDir + '\Tesela\') }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $true },
        @{ Name = 'm1: Program Files sin barra final, por usuario'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = $pf64 }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $true },
        @{ Name = 'm1: Program Files (Arm), por usuario'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = ($pf64 + ' (Arm)\Tesela\') }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $true },
        @{ Name = 'm1: Program Files en mayusculas, por usuario'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = ($pf64.ToUpperInvariant() + '\X\') }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $true },
        @{ Name = 'm1: barras normales: carpeta no valida'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = 'C:/Datos/x/Tesela/' }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $false },
        @{ Name = 'm1: con ..: carpeta no valida'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = 'C:\Datos\..\Tesela\' }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $false },
        @{ Name = 'm1: con ? (prefijo de ruta larga): carpeta no valida'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = ($unc + '?\C:\Tesela\') }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $false },
        @{ Name = 'm1: tambien para todos se rechaza el formato raro'; Props = @{ TESELA_SCOPE = 'machine'; INSTALLFOLDER = 'C:/Tesela/' }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $false },
        @{ Name = 'Program Files, por usuario CON administrador (Privileged): tambien carpeta no valida y aviso'; Props = @{ TESELA_SCOPE = 'user'; Privileged = 1; INSTALLFOLDER = $machineDefault }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $true },
        @{ Name = 'ruta relativa: carpeta no valida'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = 'Tesela' }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $false },
        @{ Name = 'solo un nombre de unidad sin barra: carpeta no valida'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = 'C:' }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $false },
        @{ Name = 'vacia: carpeta no valida'; Props = @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = '' }; Result = 'SpawnDialog:InvalidDirDlg'; Protected = $false }
    )
    foreach ($c in $folderCases) {
        $s = New-UiCase $c.Props
        $done = @((Invoke-Button $s 'FolderDlg' 'Next') | Where-Object { $_ -cnotlike 'SetTargetPath:*' })
        Check "directory dialog: $($c.Name) (dialog events: $($c.Result))" (($done -join ',') -ceq $c.Result) "events: $($done -join ',')"
        Check "directory dialog: $($c.Name) (protected-directory warning: $($c.Protected))" ((Test-ControlCond $s 'FolderDlg' 'FolderProtected' 'Show') -eq $c.Protected)
    }
    $s = New-UiCase @{ TESELA_SCOPE = 'user'; TESELA_PREV_USER = 'C:\A\'; INSTALLFOLDER = 'C:\A\' }
    Check 'directory dialog: during an upgrade, the field and Browse button are disabled and the note appears' ((Test-ControlCond $s 'FolderDlg' 'FolderEdit' 'Disable') -and (Test-ControlCond $s 'FolderDlg' 'ChangeFolder' 'Disable') -and (Test-ControlCond $s 'FolderDlg' 'FolderFixed' 'Show'))
    $s = New-UiCase @{ TESELA_SCOPE = 'user'; INSTALLFOLDER = $userDefault }
    Check 'directory dialog: without a previous install, the field and Browse button are enabled and no note appears' (-not (Test-ControlCond $s 'FolderDlg' 'FolderEdit' 'Disable') -and -not (Test-ControlCond $s 'FolderDlg' 'ChangeFolder' 'Disable') -and -not (Test-ControlCond $s 'FolderDlg' 'FolderFixed' 'Show'))
    $s = New-UiCase @{ TESELA_SCOPE = 'machine'; TESELA_PREV_USER = 'C:\A\' }
    Check 'directory dialog: a previous install in the OTHER scope does not lock the directory' (-not (Test-ControlCond $s 'FolderDlg' 'FolderEdit' 'Disable'))
    # Browse button: sets _BrowseProperty to INSTALLFOLDER and opens the native dialog.
    $s = New-UiCase @{ TESELA_SCOPE = 'user' }
    $done = Invoke-Button $s 'FolderDlg' 'ChangeFolder'
    Check 'Browse opens the native dialog for INSTALLFOLDER' ((($done -join ',') -ceq 'SpawnDialog:BrowseDlg') -and (Get-Prop $s '_BrowseProperty') -ceq 'INSTALLFOLDER')

    # === 6. Ready and completion dialogs =====================================================================================
    $s = New-Case @{ TESELA_SCOPE = 'machine'; DESKTOPSHORTCUT = '1'; WIX_UPGRADE_DETECTED = '{X}' }
    Check 'ready dialog: per-machine, with desktop shortcut and upgrade' ((Test-ControlCond $s 'VerifyReadyDlg' 'ScopeMachine' 'Show') -and -not (Test-ControlCond $s 'VerifyReadyDlg' 'ScopeUser' 'Show') -and (Test-ControlCond $s 'VerifyReadyDlg' 'Desktop' 'Show') -and (Test-ControlCond $s 'VerifyReadyDlg' 'Upgrade' 'Show'))
    $s = New-Case @{ TESELA_SCOPE = 'user' }
    Check 'ready dialog: per-user, without desktop shortcut or upgrade' ((Test-ControlCond $s 'VerifyReadyDlg' 'ScopeUser' 'Show') -and -not (Test-ControlCond $s 'VerifyReadyDlg' 'Desktop' 'Show') -and -not (Test-ControlCond $s 'VerifyReadyDlg' 'Upgrade' 'Show'))
    $s = New-Case @{ TESELA_SCOPE = 'machine' }
    Check 'ready dialog: per-machine shows the Install button WITH the UAC shield and hides the regular button' ((Test-ControlCond $s 'VerifyReadyDlg' 'InstallElevated' 'Show') -and (Test-ControlCond $s 'VerifyReadyDlg' 'Install' 'Hide'))
    $s = New-Case @{ TESELA_SCOPE = 'user' }
    Check 'ready dialog: per-user shows the regular Install button (without shield)' (-not (Test-ControlCond $s 'VerifyReadyDlg' 'InstallElevated' 'Show') -and -not (Test-ControlCond $s 'VerifyReadyDlg' 'Install' 'Hide'))
    $s = New-Case @{}
    Check 'exit dialog: a new install shows the installed title, Start option, and pinning line' ((Test-ControlCond $s 'ExitDialog' 'TitleInstalled' 'Show') -and (Test-ControlCond $s 'ExitDialog' 'Pin' 'Show') -and -not (Test-ControlCond $s 'ExitDialog' 'TitleRemoved' 'Show'))
    $s = New-Case @{ REMOVE = 'ALL' }
    Check 'exit dialog: uninstall shows the removed title and NOT the pinning line' ((Test-ControlCond $s 'ExitDialog' 'TitleRemoved' 'Show') -and -not (Test-ControlCond $s 'ExitDialog' 'Pin' 'Show') -and -not (Test-ControlCond $s 'ExitDialog' 'TitleInstalled' 'Show'))
    $s = New-Case @{ REINSTALL = 'ALL' }
    Check 'exit dialog: repair shows the repaired title' ((Test-ControlCond $s 'ExitDialog' 'TitleRepaired' 'Show') -and -not (Test-ControlCond $s 'ExitDialog' 'TitleInstalled' 'Show'))
} finally {
    # The session has the MSI open: release it before deleting the copy.
    Close-Session
    $leaf = Split-Path -Leaf $tmp
    if ($leaf -like 'tesela-logic-*' -and (Split-Path -Parent $tmp) -eq [System.IO.Path]::GetTempPath().TrimEnd('\')) {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# Keep the established coverage floor: deleting cases must not silently produce a green run.
$MinChecks = 187
if ($total -lt $MinChecks) { $failures.Add("only $total checks ran; expected at least $MinChecks") }
if ($failures.Count -gt 0) {
    Write-Host "msi-logic.tests: $($failures.Count) of $total checks failed:" -ForegroundColor Red
    $failures | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host "msi-logic.tests: OK ($total checks)"
exit 0
