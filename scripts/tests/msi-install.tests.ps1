<#
.SYNOPSIS
  Test Tesela MSI installation (install, upgrade, repair, uninstall, per-machine install, scope changes...) without a
  UI (/qn). Runs ONLY on a GitHub Actions runner: it performs real installations and uninstalls.

.DESCRIPTION
  The `msi` job in .github/workflows/ci.yml runs this after building the MSI and downloading the published v0.1.4
  (verified with `gh attestation verify`). The runner is a disposable administrator with an elevated session. This test
  REFUSES to run on a development machine (checks GITHUB_ACTIONS and RUNNER_ENVIRONMENT=github-hosted); local installation
  testing is done manually with the user's awareness, never by an agent.

  Scenarios (each restores the machine to its prior state and verifies it):
    1. Por usuario, sin interfaz, por defecto: archivos en %LOCALAPPDATA%\Programs\Tesela, acceso directo del menu Inicio del
       usuario, NADA en HKLM ni en Program Files, sin acceso directo en el escritorio, `--version`, un solo producto y una sola
       entrada en Aplicaciones instaladas; reparar (restaura un archivo borrado); desinstalar sin restos.
    2. Actualizar desde la v0.1.4 publicada (por usuario), carpeta por defecto: un solo producto tras actualizar, y la
       bajada (instalar la v0.1.4 sobre la nueva) sigue BLOQUEADA si la version nueva es mayor (si es igual, se anota y no se
       aplica).
    3. Actualizar desde la v0.1.4 instalada en una carpeta PERSONALIZADA: la actualizacion se queda en ella y desinstalar sin
       repetir INSTALLFOLDER la quita.
    4. Para todos (ALLUSERS=1): Program Files\Tesela, HKLM, menu Inicio comun, `--version`, un solo producto y desinstalar
       sin restos; carpeta personalizada para todos (INSTALLFOLDER llega a la fase del servidor) y actualizacion en su sitio.
    5. Cambio de ambito BLOQUEADO en las dos direcciones, con su mensaje, sin tocar la instalacion existente.
    6. DESKTOPSHORTCUT: por defecto no hay acceso directo; =1 lo crea (por usuario y para todos), se RECUERDA al actualizar sin
       repetirlo, =0 lo quita y desinstalar lo elimina.
    7. Carpeta protegida (Archivos de programa) en una instalacion por usuario: bloqueada con su mensaje aunque el runner sea
       administrador; para todos, permitida.
    8. Una clave de registro SIN producto (restos, sin tesela.exe) en HKCU o en HKLM no bloquea nada.
    9. Usuario ESTANDAR desechable (contrasena aleatoria que no se imprime, borrado al final aunque falle), con
       scripts/tests/msi-install-standard.tests.ps1 lanzado COMO ese usuario, sin elevar: `ALLUSERS=1 /qn` tiene que fallar y no dejar
       nada; instalar por usuario tiene que terminar con 0 en su perfil sin tocar Program Files ni HKLM; y, con otra cuenta (el
       administrador) que instala para todos entre medias, su instalacion por usuario se puede actualizar (la suya gana) y desinstalar.
    A skipped test does NOT count as passed: skips are listed separately, minimum scenario and check counts are enforced,
    and no scenario may be skipped after a version newer than published v0.1.4. The job summary includes MSI_LUA lines
    from msiexec logs (how the installer engine chose scope and elevation).

  NOT tested (requires a person): the UI dialogs, the administrator (UAC) prompt when selecting "for everyone" (the runner
  has no interactive desktop), and secure properties when a standard user enters credentials for ANOTHER administrator.

.PARAMETER NewMsi
  The MSI just built by scripts/build-msi.ps1.

.PARAMETER OldMsi
  The published v0.1.4 MSI (downloaded and verified beforehand).

.PARAMETER WorkDir
  Working directory for logs and custom install directories (defaults to a new directory in RUNNER_TEMP). Must not contain spaces.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$NewMsi,
    [Parameter(Mandatory)][string]$OldMsi,
    [string]$WorkDir
)

$ErrorActionPreference = 'Stop'

# --- Safety: only on a disposable administrator GitHub Actions runner --------------------------------------------------
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'msi-install.tests.ps1 instala y desinstala de verdad: solo corre en un runner hospedado de GitHub Actions (GITHUB_ACTIONS=true y RUNNER_ENVIRONMENT=github-hosted).'
}
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { throw 'hace falta un administrador con la sesion elevada (el runner lo es)' }

$NewMsi = [System.IO.Path]::GetFullPath($NewMsi)
$OldMsi = [System.IO.Path]::GetFullPath($OldMsi)
foreach ($f in $NewMsi, $OldMsi) { if (-not (Test-Path -LiteralPath $f -PathType Leaf)) { throw "no existe $f" } }
if (-not $WorkDir) { $WorkDir = Join-Path $env:RUNNER_TEMP ('tesela-install-tests-' + [guid]::NewGuid().ToString('N').Substring(0, 8)) }
$WorkDir = [System.IO.Path]::GetFullPath($WorkDir)
if ($WorkDir -match '\s') { throw "WorkDir no puede tener espacios (INSTALLFOLDER va sin comillas): $WorkDir" }
$tempRoot = [System.IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\')
if (-not $WorkDir.StartsWith($tempRoot + '\', [System.StringComparison]::OrdinalIgnoreCase)) { throw "WorkDir tiene que estar dentro de RUNNER_TEMP ($tempRoot)" }
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null

$UpgradeCode = '{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}'
$ExpectedFiles = @('tesela.exe', 'tesela-scan.exe', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md')

# --- Comprobaciones ------------------------------------------------------------------------------------------------------------
$passed = New-Object System.Collections.Generic.List[string]
$failed = New-Object System.Collections.Generic.List[string]
# Checks that did NOT run (not counted as passed): printed separately; the job can require zero skips.
$skipped = New-Object System.Collections.Generic.List[string]
$script:stepsRun = 0
$script:createdInstallerPolicy = $null
function Check([string]$Case, [bool]$Condition, [string]$Detail = '') {
    if ($Condition) { $script:passed.Add($Case); Write-Host "  ok   $Case" }
    else { $script:failed.Add($(if ($Detail) { "$Case ($Detail)" } else { $Case })); Write-Host "  FALLA $Case $Detail" -ForegroundColor Red }
}
function Step([string]$Text) { $script:stepsRun++; Write-Host ''; Write-Host "== $Text" }

# --- Utilidades ---------------------------------------------------------------------------------------------------------------
$script:logIndex = 0
function Invoke-Msi {
    # Run msiexec without UI and return @{ Code; Log; Text }. Remove NULs from the log (it may be UTF-16) so ASCII text can
    # be searched.
    param([string[]]$MsiArgs, [string]$Name)
    $script:logIndex++
    $log = Join-Path $WorkDir ('{0:D2}-{1}.log' -f $script:logIndex, $Name)
    $argText = ($MsiArgs -join ' ') + " /qn /norestart /l*v `"$log`""
    Write-Host "  msiexec $($MsiArgs -join ' ')"
    $p = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\msiexec.exe') -ArgumentList $argText -Wait -PassThru
    $text = ''
    if (Test-Path -LiteralPath $log) {
        $bytes = [System.IO.File]::ReadAllBytes($log)
        $text = [System.Text.Encoding]::Latin1.GetString(@($bytes | Where-Object { $_ -ne 0 }))
    }
    return [pscustomobject]@{ Code = $p.ExitCode; Log = $log; Text = $text }
}
function Install-Msi([string]$Msi, [string[]]$Props, [string]$Name) { return Invoke-Msi -MsiArgs (@('/i', "`"$Msi`"") + $Props) -Name $Name }
function Uninstall-Msi([string]$Msi, [string]$Name) { return Invoke-Msi -MsiArgs @('/x', "`"$Msi`"") -Name $Name }
function Repair-Msi([string]$Msi, [string]$Name) { return Invoke-Msi -MsiArgs @('/fa', "`"$Msi`"") -Name $Name }

$installer = New-Object -ComObject WindowsInstaller.Installer
function Get-Products {
    # Tesela products visible to Windows Installer by UpgradeCode (current user and per-machine).
    $list = [System.__ComObject].InvokeMember('RelatedProducts', 'GetProperty', $null, $installer, @([string]$UpgradeCode))
    $res = New-Object System.Collections.Generic.List[object]
    foreach ($pc in $list) {
        $code = [string]$pc
        $ver = [string][System.__ComObject].InvokeMember('ProductInfo', 'GetProperty', $null, $installer, @($code, 'VersionString'))
        $res.Add([pscustomobject]@{ Code = $code; Version = $ver })
    }
    return $res.ToArray()
}
function Get-UninstallEntries {
    # "Tesela" entries in Installed Applications (HKCU and HKLM, 64-bit and 32-bit views).
    $keys = @('HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall', 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall')
    $n = 0
    foreach ($k in $keys) {
        if (-not (Test-Path -LiteralPath $k)) { continue }
        foreach ($sub in Get-ChildItem -LiteralPath $k) {
            $dn = (Get-ItemProperty -LiteralPath $sub.PSPath -ErrorAction SilentlyContinue).DisplayName
            if ($dn -eq 'Tesela') { $n++ }
        }
    }
    return $n
}
function Get-MsiVersion([string]$Msi) {
    $db = [System.__ComObject].InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Msi, 0))
    $view = [System.__ComObject].InvokeMember('OpenView', 'InvokeMethod', $null, $db, @('SELECT `Value` FROM `Property` WHERE `Property`=''ProductVersion'''))
    [System.__ComObject].InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    $rec = [System.__ComObject].InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
    $v = [string][System.__ComObject].InvokeMember('StringData', 'GetProperty', $null, $rec, @(1))
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($rec)
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($view)
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($db)
    return $v
}
function New-SiblingMsi([string]$Src, [string]$Dst) {
    # Copy the MSI with a DIFFERENT ProductCode and package code (the Summary Information revision counter), as in a later
    # build of the same version. Installing it over the original is an UPGRADE (same UpgradeCode, different product); using
    # the same file would only maintain the same product. Modify the copy only.
    Copy-Item -LiteralPath $Src -Destination $Dst
    $newProduct = '{' + [guid]::NewGuid().ToString().ToUpperInvariant() + '}'
    $newPackage = '{' + [guid]::NewGuid().ToString().ToUpperInvariant() + '}'
    $db = [System.__ComObject].InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Dst, 1))
    $sql = "UPDATE ``Property`` SET ``Value``='$newProduct' WHERE ``Property``='ProductCode'"
    $view = [System.__ComObject].InvokeMember('OpenView', 'InvokeMethod', $null, $db, @($sql))
    [System.__ComObject].InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    [System.__ComObject].InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($view)
    $si = [System.__ComObject].InvokeMember('SummaryInformation', 'GetProperty', $null, $db, @(1))
    [System.__ComObject].InvokeMember('Property', 'SetProperty', $null, $si, @(9, $newPackage)) | Out-Null
    [System.__ComObject].InvokeMember('Persist', 'InvokeMethod', $null, $si, $null) | Out-Null
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($si)
    [System.__ComObject].InvokeMember('Commit', 'InvokeMethod', $null, $db, $null) | Out-Null
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($db)
}
function Uninstall-Installed([string]$Name) {
    # Uninstall the ONLY installed Tesela product by ProductCode, regardless of which MSI installed it.
    $prods = @(Get-Products)
    if ($prods.Count -ne 1) { throw "se esperaba un unico producto instalado para desinstalar; hay $($prods.Count)" }
    return Invoke-Msi -MsiArgs @('/x', $prods[0].Code) -Name $Name
}
function Test-Files([string]$Dir) {
    foreach ($f in $ExpectedFiles) { if (-not (Test-Path -LiteralPath (Join-Path $Dir $f) -PathType Leaf)) { return $false } }
    return $true
}
function Get-RegValue([string]$Key, [string]$Name) {
    if (-not (Test-Path -LiteralPath $Key)) { return $null }
    return (Get-ItemProperty -LiteralPath $Key -ErrorAction SilentlyContinue).$Name
}
function Test-ReparseFree([string]$Dir) {
    # No links (junctions or symbolic links) anywhere in the tree: only then is recursive deletion allowed.
    if (-not (Test-Path -LiteralPath $Dir)) { return $true }
    $items = @(Get-Item -LiteralPath $Dir -Force) + @(Get-ChildItem -LiteralPath $Dir -Recurse -Force -ErrorAction SilentlyContinue)
    return @($items | Where-Object { $_.Attributes -band [System.IO.FileAttributes]::ReparsePoint }).Count -eq 0
}
function Remove-TestDir([string]$Dir) {
    # Only directories under WorkDir or the per-machine test directory, and only when no links exist.
    if (-not (Test-Path -LiteralPath $Dir)) { return }
    $full = [System.IO.Path]::GetFullPath($Dir).TrimEnd('\')
    $ok = $full.StartsWith($WorkDir + '\', [System.StringComparison]::OrdinalIgnoreCase) -or $full -ieq $machineCustomRoot -or $full -ieq $stdShared
    if (-not $ok) { throw "se niega a borrar fuera de la zona de pruebas: $full" }
    if (-not (Test-ReparseFree $full)) { throw "se niega a borrar ${full}: tiene enlaces (reparse points)" }
    Remove-Item -LiteralPath $full -Recurse -Force
}
function Assert-Clean([string]$When) {
    # The machine has no Tesela installation (products, entries, registry values, or directories).
    $prods = @(Get-Products)
    Check "$When`: ningun producto Tesela instalado" ($prods.Count -eq 0) "hay $($prods.Count)"
    Check "$When`: ninguna entrada de Tesela en Aplicaciones instaladas" ((Get-UninstallEntries) -eq 0)
    Check "$When`: sin carpeta en Program Files" (-not (Test-Path -LiteralPath $machineDefault))
    Check "$When`: sin carpeta por usuario" (-not (Test-Path -LiteralPath $userDefault))
    Check "$When`: sin InstallFolder en HKCU" ($null -eq (Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder'))
    Check "$When`: sin InstallFolder en HKLM" ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
    Check "$When`: no Tesela key remains in HKCU" (-not (Test-Path -LiteralPath 'HKCU:\Software\Tesela'))
    Check "$When`: no Tesela key remains in HKLM" (-not (Test-Path -LiteralPath 'HKLM:\SOFTWARE\Tesela'))
    Check "$When`: sin acceso directo en el menu Inicio del usuario" (-not (Test-Path -LiteralPath $userStartMenu))
    Check "$When`: sin acceso directo en el menu Inicio comun" (-not (Test-Path -LiteralPath $commonStartMenu))
    Check "$When`: sin acceso directo en el escritorio del usuario" (-not (Test-Path -LiteralPath $userDesktop))
    Check "$When`: sin acceso directo en el escritorio comun" (-not (Test-Path -LiteralPath $commonDesktop))
}
function Reset-Machine {
    # If a scenario fails partway through, uninstall any Tesela product (the runner is disposable and contains only our test install).
    foreach ($p in @(Get-Products)) { [void](Invoke-Msi -MsiArgs @('/x', $p.Code) -Name 'reset') }
}
function Test-TeselaVersion([string]$Exe, [string]$Label) {
    # tesela.exe is a GUI executable: --version is written to STDERR without opening a window.
    $err = Join-Path $WorkDir 'version.txt'
    $p = Start-Process -FilePath $Exe -ArgumentList '--version' -Wait -PassThru -RedirectStandardError $err
    $out = if (Test-Path -LiteralPath $err) { Get-Content -LiteralPath $err -Raw } else { '' }
    Check "$Label`: tesela.exe --version responde" ($p.ExitCode -eq 0 -and $out -match '^tesela \d+\.\d+\.\d+') "codigo $($p.ExitCode), salida '$out'"
}
function Show-LogTail([object]$R) {
    Write-Host "  --- ultimas lineas de $($R.Log) ---"
    ($R.Text -split "`n") | Select-Object -Last 25 | ForEach-Object { Write-Host "  | $_" }
}

# --- Usuario ESTANDAR desechable (escenario 9) --------------------------------------------------------------------------------------
$stdShared = 'C:\TeselaCiStd'
$stdName = 'teselacistd'
$script:stdUser = $null
function New-RandomPassword {
    # Generate a random password here; never print it (and mask it in the job log as a safeguard).
    $b = New-Object byte[] 24
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $rng.GetBytes($b)
    $rng.Dispose()
    return 'Aa1!' + [Convert]::ToBase64String($b).Replace('=', 'x')
}
function New-StandardUser {
    if (Get-LocalUser -Name $stdName -ErrorAction SilentlyContinue) { throw "ya existe el usuario local ${stdName}: se aborta sin tocarlo" }
    $pw = New-RandomPassword
    Write-Host "::add-mask::$pw"
    $sec = ConvertTo-SecureString $pw -AsPlainText -Force
    New-LocalUser -Name $stdName -Password $sec -FullName 'Tesela CI standard user' -Description 'Disposable user of the Tesela CI install tests' -AccountNeverExpires -PasswordNeverExpires | Out-Null
    # Only the Users group (S-1-5-32-545): standard user, no administrator rights.
    Add-LocalGroupMember -SID 'S-1-5-32-545' -Member $stdName
    $script:stdUser = [pscustomobject]@{ Name = $stdName; Cred = (New-Object System.Management.Automation.PSCredential(".\$stdName", $sec)) }
    $isAdminMember = @(Get-LocalGroupMember -SID 'S-1-5-32-544' -ErrorAction SilentlyContinue | Where-Object { $_.Name -like "*\$stdName" }).Count -gt 0
    Check 'el usuario desechable es ESTANDAR (no esta en Administradores)' (-not $isAdminMember)
}
function Remove-StandardUser {
    # Delete the user even if the test fails (leave the profile directory because it contains Windows-owned links and the runner is disposable).
    if (Get-LocalUser -Name $stdName -ErrorAction SilentlyContinue) { Remove-LocalUser -Name $stdName }
    $script:stdUser = $null
}
function Invoke-StdPhase([string]$Phase, [string]$MsiPath) {
    # Launch msi-install-standard.tests.ps1 AS the standard user (without elevation) and add its result lines to the checks.
    $result = Join-Path $stdShared "result-$Phase.txt"
    if (Test-Path -LiteralPath $result) { Remove-Item -LiteralPath $result -Force }
    $stdArgs = @('-NoProfile', '-File', "`"$stdScriptCopy`"", '-Phase', $Phase, '-Shared', "`"$stdShared`"", '-ResultFile', "`"$result`"")
    if ($MsiPath) { $stdArgs += @('-Msi', "`"$MsiPath`"") }
    $pwshExe = (Get-Process -Id $PID).Path
    Write-Host "  (como $stdName) pwsh -File msi-install-standard.tests.ps1 -Phase $Phase"
    try {
        $outFile = Join-Path $stdShared "std-$Phase.out.txt"
        $errFile = Join-Path $stdShared "std-$Phase.err.txt"
        $p = Start-Process -FilePath $pwshExe -ArgumentList ($stdArgs -join ' ') -Credential $script:stdUser.Cred -LoadUserProfile -WorkingDirectory $stdShared -UseNewEnvironment -PassThru -RedirectStandardOutput $outFile -RedirectStandardError $errFile
    } catch {
        Check "se puede lanzar un proceso como el usuario estandar (fase $Phase)" $false "Start-Process -Credential fallo: $($_.Exception.Message)"
        return
    }
    Wait-Process -Id $p.Id -Timeout 900 -ErrorAction SilentlyContinue
    if (Get-Process -Id $p.Id -ErrorAction SilentlyContinue) {
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        Check "la fase $Phase del usuario estandar termino a tiempo" $false 'tiempo agotado (15 min)'
        return
    }
    foreach ($f in $outFile, $errFile) {
        if ((Test-Path -LiteralPath $f) -and (Get-Item -LiteralPath $f).Length -gt 0) { Write-Host "  [estandar] contenido de $(Split-Path -Leaf $f):"; Get-Content -LiteralPath $f -TotalCount 40 | ForEach-Object { Write-Host "    | $_" } }
    }
    if (-not (Test-Path -LiteralPath $result)) {
        Check "la fase $Phase del usuario estandar produjo resultados" $false 'no hay archivo de resultados: el proceso no llego a escribirlos (perfil, permisos o error al arrancar)'
        return
    }
    $lines = @(Get-Content -LiteralPath $result)
    $count = 0
    foreach ($l in $lines) {
        $parts = $l -split '\|', 3
        switch ($parts[0]) {
            'ok' { $count++; Check "(usuario estandar) $($parts[1])" $true }
            'FAIL' { $count++; Check "(usuario estandar) $($parts[1])" $false $(if ($parts.Count -gt 2) { $parts[2] } else { '' }) }
            'INFO' { Write-Host "  [estandar] $($parts[1])" }
        }
    }
    Check "la fase $Phase del usuario estandar hizo comprobaciones" ($count -gt 0)
    if (@($lines | Where-Object { $_ -like 'FAIL|*' }).Count -gt 0) { Show-StdLogs $Phase }
}
function Show-StdLogs([string]$Phase) {
    # If a phase fails, show the most relevant standard-user msiexec log lines (errors, policies, scope, elevation).
    foreach ($log in Get-ChildItem -LiteralPath $stdShared -Filter "std-$Phase-*.log" -ErrorAction SilentlyContinue) {
        $text = [System.Text.Encoding]::Latin1.GetString(@([System.IO.File]::ReadAllBytes($log.FullName) | Where-Object { $_ -ne 0 }))
        Write-Host "  [estandar] $($log.Name): lineas relevantes"
        ($text -split "`r?`n") | Where-Object { $_ -match 'Error|error|1625|1925|policy|Policy|DisableMSI|DisableUserInstalls|MSI_LUA|dual mode|Return value|Elevat|privilege|not allowed|forbidden|rejected|Product:|Note:' } | Select-Object -First 40 | ForEach-Object { Write-Host "    | $($_.Trim())" }
    }
}
function Show-PolicyDiagnostics {
    # Read-only runner policies that may determine whether a standard user can install; include them in the job log.
    foreach ($k in 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Installer', 'HKCU:\SOFTWARE\Policies\Microsoft\Windows\Installer', 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Safer\CodeIdentifiers', 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System', 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\SrpV2') {
        if (Test-Path -LiteralPath $k) {
            Write-Host "  [directiva] $k"
            (Get-ItemProperty -LiteralPath $k | Get-Member -MemberType NoteProperty | Where-Object { $_.Name -notlike 'PS*' } | ForEach-Object { "$($_.Name)=$((Get-ItemProperty -LiteralPath $k).($_.Name))" }) -join '; ' | ForEach-Object { Write-Host "    $_" }
        } else { Write-Host "  [directiva] $k : no existe" }
    }
    $svc = Get-Service -Name seclogon -ErrorAction SilentlyContinue
    Write-Host "  [servicio] seclogon (inicio de sesion secundario): $($svc.Status) / $($svc.StartType)"
}
function Get-LuaEvidence {
    # msiexec log lines showing how the engine chose scope and elevation (MSI_LUA, dual mode, 1925...).
    $out = New-Object System.Collections.Generic.List[string]
    foreach ($dir in $WorkDir, $stdShared) {
        if (-not (Test-Path -LiteralPath $dir)) { continue }
        # Standard-user lines (std-*) first: they are the most relevant evidence.
        foreach ($log in (Get-ChildItem -LiteralPath $dir -Filter '*.log' -ErrorAction SilentlyContinue | Sort-Object { if ($_.Name -like 'std-*') { 0 } else { 1 } }, Name)) {
            $text = [System.Text.Encoding]::Latin1.GetString(@([System.IO.File]::ReadAllBytes($log.FullName) | Where-Object { $_ -ne 0 }))
            foreach ($line in ($text -split "`r?`n")) {
                if ($line -match 'MSI_LUA|dual mode|Elevation required|sufficient privileges|MSIINSTALLPERUSER') { $out.Add("$($log.Name): $($line.Trim())") }
            }
        }
    }
    return @($out | Select-Object -Unique | Select-Object -First 80)
}

# --- Rutas ---------------------------------------------------------------------------------------------------------------------
$userDefault = Join-Path $env:LOCALAPPDATA 'Programs\Tesela'
$machineDefault = Join-Path $env:ProgramFiles 'Tesela'
$userStartMenu = Join-Path ([Environment]::GetFolderPath('Programs')) 'Tesela.lnk'
$commonStartMenu = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'Tesela.lnk'
$userDesktop = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Tesela.lnk'
$commonDesktop = Join-Path ([Environment]::GetFolderPath('CommonDesktopDirectory')) 'Tesela.lnk'
$userCustom = Join-Path $WorkDir 'custom-user\Tesela'
$machineCustomRoot = 'C:\TeselaCiMachine'
$machineCustom = Join-Path $machineCustomRoot 'Tesela'
$SiblingMsi = Join-Path $WorkDir 'sibling.msi'
New-SiblingMsi $NewMsi $SiblingMsi
$newVersion = Get-MsiVersion $NewMsi
$oldVersion = Get-MsiVersion $OldMsi
Write-Host "MSI nuevo $newVersion, MSI anterior $oldVersion, trabajo en $WorkDir"

try {
    Step '0. Punto de partida: la maquina no tiene Tesela'
    Assert-Clean 'inicio'
    if ($failed.Count -gt 0) { throw 'el runner ya tenia Tesela instalada: se aborta sin tocar nada' }
    Check 'el MSI anterior es la v0.1.4' ($oldVersion -eq '0.1.4') "es $oldVersion"

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '1. Por usuario, sin interfaz, por defecto'
    $r = Install-Msi $NewMsi @() 'user-install'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'instalar (/qn) termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'los 5 archivos estan en %LOCALAPPDATA%\Programs\Tesela' (Test-Files $userDefault)
    Check 'nada en Program Files' (-not (Test-Path -LiteralPath $machineDefault))
    Check 'HKCU\Software\Tesela\InstallFolder es la carpeta por usuario' ((Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder') -ieq ($userDefault + '\'))
    Check 'nada en HKLM\SOFTWARE\Tesela' ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
    Check 'acceso directo en el menu Inicio del usuario' (Test-Path -LiteralPath $userStartMenu)
    Check 'ningun acceso directo en el menu Inicio comun' (-not (Test-Path -LiteralPath $commonStartMenu))
    Check 'SIN acceso directo en el escritorio (desmarcado por defecto)' (-not (Test-Path -LiteralPath $userDesktop) -and -not (Test-Path -LiteralPath $commonDesktop))
    Check 'un solo producto, con la version del MSI' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Check 'una sola entrada en Aplicaciones instaladas' ((Get-UninstallEntries) -eq 1)
    Test-TeselaVersion (Join-Path $userDefault 'tesela.exe') 'por usuario'
    # Repair: restore a deleted file.
    Remove-Item -LiteralPath (Join-Path $userDefault 'tesela-scan.exe') -Force
    $r = Repair-Msi $NewMsi 'user-repair'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'reparar (/fa) termina con 0 y restaura el archivo borrado' ($r.Code -eq 0 -and (Test-Files $userDefault)) "codigo $($r.Code)"
    $r = Uninstall-Msi $NewMsi 'user-uninstall'
    Check 'desinstalar termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras desinstalar por usuario'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '2. Actualizar desde la v0.1.4 publicada (por usuario, carpeta por defecto)'
    $r = Install-Msi $OldMsi @() 'old-install'
    Check 'instalar la v0.1.4 termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la v0.1.4 esta en la carpeta por defecto y es el unico producto' ((Test-Files $userDefault) -and (@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq '0.1.4')
    $r = Install-Msi $NewMsi @() 'upgrade-default'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'actualizar a la version nueva termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'tras actualizar queda UN solo producto, con la version nueva' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Check 'tras actualizar queda UNA sola entrada en Aplicaciones instaladas' ((Get-UninstallEntries) -eq 1)
    Check 'la actualizacion se queda en la carpeta por defecto, con los 5 archivos' (Test-Files $userDefault)
    Check 'el acceso directo del menu Inicio sigue ahi' (Test-Path -LiteralPath $userStartMenu)
    if ([version]$newVersion -gt [version]$oldVersion) {
        $r = Install-Msi $OldMsi @() 'downgrade'
        Check 'la bajada (instalar la v0.1.4 sobre la nueva) sigue BLOQUEADA (codigo 1603)' ($r.Code -eq 1603) "codigo $($r.Code)"
        Check 'la bajada se bloquea con su mensaje' ($r.Text -match 'newer version of Tesela is already installed') 'no sale el mensaje en el registro'
        Check 'tras la bajada bloqueada sigue instalada la version nueva' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    } else {
        Write-Host "  (bajada NO aplicable: la version nueva ($newVersion) no es mayor que la publicada ($oldVersion); se probara cuando se suba la version)"
        $skipped.Add('bajada: NO ejecutada (la version nueva no es mayor que la publicada); se prueba al subir la version')
    }
    $r = Uninstall-Installed 'upgrade-uninstall'
    Check 'desinstalar tras actualizar termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras desinstalar la actualizacion'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '3. Actualizar desde la v0.1.4 en una carpeta PERSONALIZADA (por usuario)'
    $r = Install-Msi $OldMsi @("INSTALLFOLDER=$userCustom\") 'old-custom'
    Check 'instalar la v0.1.4 en una carpeta personalizada termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la v0.1.4 esta en la carpeta personalizada y no en la de por defecto' ((Test-Files $userCustom) -and -not (Test-Path -LiteralPath $userDefault))
    $r = Install-Msi $NewMsi @() 'upgrade-custom'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'actualizar sin repetir INSTALLFOLDER termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la actualizacion conserva la carpeta personalizada (5 archivos) y no crea la de por defecto' ((Test-Files $userCustom) -and -not (Test-Path -LiteralPath $userDefault))
    Check 'un solo producto, con la version nueva' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Test-TeselaVersion (Join-Path $userCustom 'tesela.exe') 'carpeta personalizada'
    $r = Uninstall-Installed 'custom-uninstall'
    Check 'desinstalar sin repetir INSTALLFOLDER termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la carpeta personalizada se quita entera al desinstalar' (-not (Test-Path -LiteralPath (Join-Path $userCustom 'tesela.exe')) -and -not (Test-Path -LiteralPath $userCustom))
    Assert-Clean 'tras desinstalar la carpeta personalizada'
    Remove-TestDir (Join-Path $WorkDir 'custom-user')

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '4. Para todos (ALLUSERS=1)'
    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'machine-install'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'instalar para todos termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'los 5 archivos estan en Program Files\Tesela' (Test-Files $machineDefault)
    Check 'nada en la carpeta por usuario' (-not (Test-Path -LiteralPath $userDefault))
    Check 'HKLM\SOFTWARE\Tesela\InstallFolder es Program Files\Tesela' ((Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder') -ieq ($machineDefault + '\'))
    Check 'nada en HKCU\Software\Tesela' ($null -eq (Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder'))
    Check 'acceso directo en el menu Inicio COMUN y no en el del usuario' ((Test-Path -LiteralPath $commonStartMenu) -and -not (Test-Path -LiteralPath $userStartMenu))
    Check 'un solo producto, con la version del MSI' ((@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion)
    Check 'una sola entrada en Aplicaciones instaladas' ((Get-UninstallEntries) -eq 1)
    Test-TeselaVersion (Join-Path $machineDefault 'tesela.exe') 'para todos'
    $r = Uninstall-Msi $NewMsi 'machine-uninstall'
    Check 'desinstalar la instalacion para todos termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras desinstalar para todos'

    Write-Host '  -- para todos con carpeta personalizada (INSTALLFOLDER llega a la fase del servidor) y actualizacion en su sitio'
    $r = Install-Msi $NewMsi @('ALLUSERS=1', "INSTALLFOLDER=$machineCustom\") 'machine-custom'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'instalar para todos con INSTALLFOLDER termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'los 5 archivos estan en la carpeta personalizada para todos y no en Program Files' ((Test-Files $machineCustom) -and -not (Test-Path -LiteralPath $machineDefault))
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'machine-custom-upgrade'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'reinstalar para todos sin INSTALLFOLDER termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la actualizacion para todos se queda en su carpeta y deja un solo producto' ((Test-Files $machineCustom) -and -not (Test-Path -LiteralPath $machineDefault) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Installed 'machine-custom-uninstall'
    Check 'desinstalar sin repetir INSTALLFOLDER termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la carpeta personalizada para todos se quita al desinstalar' (-not (Test-Path -LiteralPath $machineCustom))
    Remove-TestDir $machineCustomRoot
    Assert-Clean 'tras desinstalar para todos con carpeta personalizada'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '5. Cambio de ambito bloqueado (sin tocar la instalacion existente)'
    $r = Install-Msi $NewMsi @() 'scope-user'
    Check 'instalar por usuario termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'scope-user-to-machine'
    Check 'pasar de por usuario a para todos (ALLUSERS=1) se BLOQUEA (codigo 1603)' ($r.Code -eq 1603) "codigo $($r.Code)"
    Check 'el bloqueo dice que hay que desinstalar antes (por usuario -> para todos)' ($r.Text -match 'already installed for your user only') 'no sale el mensaje en el registro'
    Check 'la instalacion por usuario sigue intacta y no hay nada en Program Files ni en HKLM' ((Test-Files $userDefault) -and -not (Test-Path -LiteralPath $machineDefault) -and ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder')) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Msi $NewMsi 'scope-user-uninstall'
    Check 'desinstalar la instalacion por usuario termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras el bloqueo user -> machine'

    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'scope-machine'
    Check 'instalar para todos termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    $r = Install-Msi $SiblingMsi @() 'scope-machine-to-user'
    Check 'pasar de para todos a por usuario (sin ALLUSERS) se BLOQUEA (codigo 1603)' ($r.Code -eq 1603) "codigo $($r.Code)"
    Check 'el bloqueo dice que hay que usar ALLUSERS=1 o desinstalar antes (para todos -> por usuario)' ($r.Text -match 'already installed for all users') 'no sale el mensaje en el registro'
    Check 'la instalacion para todos sigue intacta y no hay nada en la carpeta por usuario ni en HKCU' ((Test-Files $machineDefault) -and -not (Test-Path -LiteralPath $userDefault) -and ($null -eq (Get-RegValue 'HKCU:\Software\Tesela' 'InstallFolder')) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Msi $NewMsi 'scope-machine-uninstall'
    Check 'desinstalar la instalacion para todos termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras el bloqueo machine -> user'

    # A per-user v0.1.4 install also blocks switching to per-machine with the new MSI.
    $r = Install-Msi $OldMsi @() 'scope-old-user'
    Check 'instalar la v0.1.4 por usuario termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'scope-old-to-machine'
    Check 'desde la v0.1.4 por usuario, pasar a para todos se BLOQUEA (codigo 1603)' ($r.Code -eq 1603) "codigo $($r.Code)"
    $r = Install-Msi $NewMsi @() 'scope-old-upgrade'
    Check 'desde la v0.1.4 por usuario, actualizar por usuario SI funciona' ($r.Code -eq 0 -and (@(Get-Products)).Count -eq 1 -and @(Get-Products)[0].Version -eq $newVersion) "codigo $($r.Code)"
    $r = Uninstall-Installed 'scope-old-uninstall'
    Check 'desinstalar termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Assert-Clean 'tras el bloqueo desde la v0.1.4'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '6. Acceso directo del escritorio (DESKTOPSHORTCUT)'
    $r = Install-Msi $NewMsi @('DESKTOPSHORTCUT=1') 'desktop-user'
    Check 'instalar por usuario con DESKTOPSHORTCUT=1 termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'el acceso directo esta en el escritorio del usuario (y no en el comun)' ((Test-Path -LiteralPath $userDesktop) -and -not (Test-Path -LiteralPath $commonDesktop))
    $r = Install-Msi $SiblingMsi @() 'desktop-user-remembered'
    Check 'actualizar sin repetir DESKTOPSHORTCUT termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'el acceso directo del escritorio se RECUERDA al actualizar' (Test-Path -LiteralPath $userDesktop)
    $r = Install-Msi $NewMsi @('DESKTOPSHORTCUT=0') 'desktop-user-off'
    Check 'actualizar con DESKTOPSHORTCUT=0 termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'DESKTOPSHORTCUT=0 quita el acceso directo del escritorio' (-not (Test-Path -LiteralPath $userDesktop))
    $r = Install-Msi $SiblingMsi @('DESKTOPSHORTCUT=1') 'desktop-user-on'
    Check 'volver a ponerlo con DESKTOPSHORTCUT=1' ($r.Code -eq 0 -and (Test-Path -LiteralPath $userDesktop))
    $r = Uninstall-Installed 'desktop-user-uninstall'
    Check 'desinstalar termina con 0 y quita el acceso directo del escritorio' ($r.Code -eq 0 -and -not (Test-Path -LiteralPath $userDesktop)) "codigo $($r.Code)"
    Assert-Clean 'tras desinstalar con acceso directo del escritorio'

    $r = Install-Msi $NewMsi @('ALLUSERS=1', 'DESKTOPSHORTCUT=1') 'desktop-machine'
    Check 'instalar para todos con DESKTOPSHORTCUT=1 termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'el acceso directo esta en el escritorio COMUN (y no en el del usuario)' ((Test-Path -LiteralPath $commonDesktop) -and -not (Test-Path -LiteralPath $userDesktop))
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'desktop-machine-remembered'
    Check 'actualizar para todos sin repetir DESKTOPSHORTCUT lo recuerda' ($r.Code -eq 0 -and (Test-Path -LiteralPath $commonDesktop)) "codigo $($r.Code)"
    $r = Uninstall-Installed 'desktop-machine-uninstall'
    Check 'desinstalar para todos termina con 0 y quita el acceso directo comun' ($r.Code -eq 0 -and -not (Test-Path -LiteralPath $commonDesktop)) "codigo $($r.Code)"
    Assert-Clean 'tras desinstalar para todos con acceso directo del escritorio'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '7. Carpeta protegida en una instalacion por usuario (mensaje legible, no un 1603 mudo)'
    # The runner is an administrator, but "just for me" still rejects Program Files (this does not depend on Privileged).
    $protectedFolder = Join-Path $env:ProgramFiles 'TeselaCiProtegida'
    $r = Install-Msi $NewMsi @("INSTALLFOLDER=`"$protectedFolder`"") 'protected-user'
    Check 'por usuario con INSTALLFOLDER en Archivos de programa se BLOQUEA (codigo 1603)' ($r.Code -eq 1603) "codigo $($r.Code)"
    Check 'el bloqueo dice que la carpeta esta protegida y sugiere ALLUSERS=1' ($r.Text -match 'INSTALLFOLDER is in a protected location') 'no sale el mensaje en el registro'
    Check 'no se instalo nada ni se creo la carpeta' (-not (Test-Path -LiteralPath $protectedFolder) -and (@(Get-Products)).Count -eq 0)
    $r = Install-Msi $NewMsi @('ALLUSERS=1', "INSTALLFOLDER=`"$protectedFolder`"") 'protected-machine'
    Check 'para todos con INSTALLFOLDER en Archivos de programa SI se instala' ($r.Code -eq 0 -and (Test-Files $protectedFolder)) "codigo $($r.Code)"
    $r = Uninstall-Installed 'protected-machine-uninstall'
    Check 'desinstalar termina con 0 y quita la carpeta' ($r.Code -eq 0 -and -not (Test-Path -LiteralPath $protectedFolder)) "codigo $($r.Code)"
    Assert-Clean 'tras la carpeta protegida'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '8. Una clave de registro SIN producto (restos) no bloquea nada'
    # A key with InstallFolder but no tesela.exe in that directory (left by an old version or manually cleaned registry...).
    New-Item -Path 'HKCU:\Software\Tesela' -Force | Out-Null
    Set-ItemProperty -LiteralPath 'HKCU:\Software\Tesela' -Name 'InstallFolder' -Value ($WorkDir + '\resto-usuario\')
    $r = Install-Msi $NewMsi @('ALLUSERS=1') 'stale-user-to-machine'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'con una clave de HKCU sin producto, instalar para todos NO se bloquea (codigo 0)' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'se instalo en Program Files' (Test-Files $machineDefault)
    $r = Uninstall-Installed 'stale-user-uninstall'
    Check 'desinstalar termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Remove-Item -LiteralPath 'HKCU:\Software\Tesela' -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -Path 'HKLM:\SOFTWARE\Tesela' -Force | Out-Null
    Set-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Tesela' -Name 'InstallFolder' -Value ($WorkDir + '\resto-equipo\')
    $r = Install-Msi $NewMsi @() 'stale-machine-to-user'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'con una clave de HKLM sin producto, instalar por usuario NO se bloquea (codigo 0)' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'se instalo en la carpeta por usuario' (Test-Files $userDefault)
    $r = Uninstall-Installed 'stale-machine-uninstall'
    Check 'desinstalar termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    Remove-Item -LiteralPath 'HKLM:\SOFTWARE\Tesela' -Recurse -Force -ErrorAction SilentlyContinue
    Assert-Clean 'tras las claves sin producto'

    # ---------------------------------------------------------------------------------------------------------------------------
    Step '9. Usuario ESTANDAR (sin privilegios): por usuario sin elevar, "para todos" falla y la suya gana a la de otra cuenta'
    # Disposable local standard user, with a random unprinted password. The user reports its own actions (its HKCU and
    # profile are separate from the runner administrator's).
    New-Item -ItemType Directory -Force -Path $stdShared | Out-Null
    Show-PolicyDiagnostics
    # EVIDENCE (run 39030236010 for this PR): windows-latest is Windows SERVER, where the effective Windows Installer
    # policy is DisableMsi=1 ("for non-managed applications only"): a standard user cannot install ANY MSI, and msiexec
    # exits with 1625 ("This installation is forbidden by system policy") BEFORE choosing scope, including per-user dual-mode packages.
    # (the log says: "MSI_LUA: Package is a dual mode package and is being installed in Per-User mode. No elevation is required.").
    # This is not a package issue: the default on Windows 10 and 11 client is 0. To test the intended behavior, set this
    # DISPOSABLE machine to the Windows client default (DisableMsi=0) during the scenario, then remove the setting afterward.
    $installerPolicy = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Installer'
    if (Test-Path -LiteralPath $installerPolicy) { throw 'ya existe la clave de directivas de Windows Installer: se aborta sin tocarla' }
    New-Item -Path $installerPolicy -Force | Out-Null
    New-ItemProperty -LiteralPath $installerPolicy -Name 'DisableMSI' -Value 0 -PropertyType DWord -Force | Out-Null
    $script:createdInstallerPolicy = $installerPolicy
    Write-Host "  [directiva] DisableMSI=0 en $installerPolicy (valor por defecto de un Windows cliente; se quita al terminar)"
    New-StandardUser
    & icacls.exe $stdShared '/grant' "${stdName}:(OI)(CI)M" | Out-Null
    Set-Content -LiteralPath (Join-Path $stdShared '.tesela-ci-marker') -Value 'created by msi-install.tests.ps1 in a GitHub-hosted runner' -Encoding ASCII
    $stdScriptCopy = Join-Path $stdShared 'msi-install-standard.tests.ps1'
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'msi-install-standard.tests.ps1') -Destination $stdScriptCopy
    $stdNew = Join-Path $stdShared 'new.msi'
    $stdSibling2 = Join-Path $stdShared 'sibling2.msi'
    Copy-Item -LiteralPath $NewMsi -Destination $stdNew
    New-SiblingMsi $NewMsi $stdSibling2
    # (a) As a standard user, a per-machine install must FAIL and leave nothing behind.
    Invoke-StdPhase 'machine-attempt' $stdNew
    # (b) Per-user without elevation: install in the user's profile without touching Program Files or HKLM.
    Invoke-StdPhase 'user-install' $stdNew
    Check 'el administrador del runner no ve ese producto por usuario (cada cuenta la suya)' ((@(Get-Products)).Count -eq 0)
    Check 'nada en Program Files tras la instalacion por usuario del estandar' (-not (Test-Path -LiteralPath $machineDefault))
    Check 'nada en HKLM tras la instalacion por usuario del estandar' ($null -eq (Get-RegValue 'HKLM:\SOFTWARE\Tesela' 'InstallFolder'))
    # (c) M2: another account (administrator) installs per-machine using another build; the standard user can still upgrade THEIR install.
    $r = Install-Msi $SiblingMsi @('ALLUSERS=1') 'std-machine-by-admin'
    if ($r.Code -ne 0) { Show-LogTail $r }
    Check 'otra cuenta (administrador) instala para todos mientras el estandar tiene la suya por usuario (codigo 0)' ($r.Code -eq 0) "codigo $($r.Code)"
    Check 'la instalacion para todos esta en Program Files' (Test-Files $machineDefault)
    Invoke-StdPhase 'user-upgrade' $stdSibling2
    Check 'la instalacion para todos de la otra cuenta sigue intacta tras la actualizacion del estandar' ((Test-Files $machineDefault) -and (@(Get-Products)).Count -eq 1)
    $r = Uninstall-Installed 'std-machine-uninstall'
    Check 'desinstalar la instalacion para todos termina con 0' ($r.Code -eq 0) "codigo $($r.Code)"
    # (d) The standard user uninstalls their product without leftovers.
    Invoke-StdPhase 'user-uninstall' $null
    Remove-StandardUser
    Check 'el usuario desechable se borro' (-not (Get-LocalUser -Name $stdName -ErrorAction SilentlyContinue))
    Assert-Clean 'tras el escenario del usuario estandar'
} catch {
    $failed.Add("la prueba se interrumpio: $($_.Exception.Message)")
    Write-Host "ERROR: $($_.Exception.Message)" -ForegroundColor Red
} finally {
    # Cleanup: remove any remaining Tesela product and test directories (the runner is disposable, but leave no debris).
    try { Reset-Machine } catch { Write-Host "limpieza: $($_.Exception.Message)" }
    # Save standard-user logs and results with the other logs before deleting the shared directory.
    try { if (Test-Path -LiteralPath $stdShared) { Get-ChildItem -LiteralPath $stdShared -File | Where-Object { $_.Extension -in '.log', '.txt' } | Copy-Item -Destination $WorkDir -Force } } catch { Write-Host "copia de registros: $($_.Exception.Message)" }
    if ($script:createdInstallerPolicy) { Remove-Item -LiteralPath $script:createdInstallerPolicy -Recurse -Force -ErrorAction SilentlyContinue }
    try { Remove-StandardUser } catch { Write-Host "limpieza del usuario estandar: $($_.Exception.Message)" }
    foreach ($k in 'HKCU:\Software\Tesela', 'HKLM:\SOFTWARE\Tesela') { Remove-Item -LiteralPath $k -Recurse -Force -ErrorAction SilentlyContinue }
    foreach ($d in (Join-Path $WorkDir 'custom-user'), $machineCustomRoot, $stdShared) {
        try { Remove-TestDir $d } catch { Write-Host "limpieza: $($_.Exception.Message)" }
    }
}

# --- Resumen -------------------------------------------------------------------------------------------------------------------
# A test that did not run is not a passing test: enforce minimum scenario and check counts, and report skipped checks separately.
$ExpectedSteps = 10
$MinChecks = 220
if ($script:stepsRun -ne $ExpectedSteps) { $failed.Add("se ejecutaron $($script:stepsRun) escenarios y se esperaban $ExpectedSteps (un escenario no se llego a ejecutar)") }
if ($passed.Count + $failed.Count -lt $MinChecks) { $failed.Add("solo se ejecutaron $($passed.Count + $failed.Count) comprobaciones y se esperaban al menos $MinChecks") }
$cargoMajor = [version]$newVersion
if ($skipped.Count -gt 0 -and $cargoMajor -gt [version]'0.1.4') { $failed.Add("hay pruebas omitidas con la version $newVersion, que ya es posterior a la v0.1.4 publicada: $($skipped -join '; ')") }
$lua = @(Get-LuaEvidence)
Write-Host ''
Write-Host "msi-install.tests: $($passed.Count) comprobaciones bien, $($failed.Count) fallan, $($skipped.Count) omitidas, $($script:stepsRun) escenarios"
$skipped | ForEach-Object { Write-Host "  OMITIDA: $_" -ForegroundColor Yellow }
if ($lua.Count -gt 0) { Write-Host '  Evidencia del motor (ambito y elevacion):'; $lua | ForEach-Object { Write-Host "    $_" } }
if ($env:GITHUB_STEP_SUMMARY) {
    $md = New-Object System.Collections.Generic.List[string]
    $md.Add("### Pruebas de instalacion del MSI (version $newVersion; anterior $oldVersion)")
    $md.Add('')
    $md.Add("**$($passed.Count) comprobaciones bien, $($failed.Count) fallan, $($skipped.Count) omitidas, $($script:stepsRun) escenarios.**")
    $md.Add('')
    foreach ($f in $failed) { $md.Add("- FALLA: $f") }
    foreach ($s in $skipped) { $md.Add("- OMITIDA (no cuenta como pasada): $s") }
    if ($lua.Count -gt 0) {
        $md.Add('')
        $md.Add('Evidencia del motor de Windows Installer (registros de msiexec: MSI_LUA, ambito y elevacion):')
        $md.Add('')
        $md.Add('```')
        foreach ($l in $lua) { $md.Add($l) }
        $md.Add('```')
    }
    $md.Add('')
    $md.Add('<details><summary>Comprobaciones que pasaron</summary>')
    $md.Add('')
    foreach ($ok in $passed) { $md.Add("- ok: $ok") }
    $md.Add('')
    $md.Add('</details>')
    [System.IO.File]::AppendAllText($env:GITHUB_STEP_SUMMARY, (($md -join "`n") + "`n"))
}
if ($failed.Count -gt 0) {
    $failed | ForEach-Object { Write-Host "  FALLA: $_" -ForegroundColor Red }
    exit 1
}
exit 0
