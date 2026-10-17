<#
.SYNOPSIS
  Read-only validation of Tesela MSI tables using the Windows Installer API.

.DESCRIPTION
  Functions to dot-source (`. scripts/msi-validate.ps1`) from scripts/build-msi.ps1 and
  scripts/tests/msi-validate.tests.ps1 (which tests them against modified MSI files in %TEMP% without installing them).
  Does not use CIM or WMI; it uses only the WindowsInstaller.Installer COM object, like Orca.

  Test-MsiPackage returns a list of problems (empty when the MSI matches expectations). It checks:
    * The MSI tables (`_Tables`) are EXACTLY the closed list in $MsiAllowedTables: installation and UI tables
      (Dialog, Control, ControlEvent...), and NO others (Binary, Environment, ServiceInstall, Class, Extension, ProgId,
      Verb, TypeLib, MIME, IniFile, Error, etc. are failures whether or not they contain rows).
    * Version, fixed UpgradeCode, name and publisher, Installed Applications properties, and the closed property list.
    * DUAL SCOPE (Scope="perUserOrMachine"): ALLUSERS=2 and MSIINSTALLPERUSER=1 (per-user unless requested otherwise),
      with the "no elevation required" bit in Summary Information CLEAR. If set, Windows Installer treats the package as
      UAC-compatible, ignores MSIINSTALLPERUSER, and does not require admin for a per-machine install (error 1925).
    * Exactly the five expected files.
    * Registry entries ONLY under HKMU (root -1) \Software\Tesela, with the eight expected values; one RemoveFile row
      for the install directory only when empty, without a file name.
    * The six registry searches for previous installs (HKCU and HKLM: InstallFolder and DesktopShortcut, type 18, plus
      the type 16 Signature check that tesela.exe exists in the remembered directory): an orphaned key does not count.
    * The EXACT directory tree ($MsiExpectedDirectories) and components (directory, fixed GUID, attributes, condition).
    * Two shortcuts (Start menu and desktop, no arguments, WkDir = INSTALLFOLDER) and one icon.
    * SECURE PROPERTIES (see installer/Tesela.wxs): SecureCustomProperties is EXACTLY DESKTOPSHORTCUT, INSTALLFOLDER,
      WIX_DOWNGRADE_DETECTED, and WIX_UPGRADE_DETECTED (ALLUSERS always reaches the server, and MSIINSTALLPERUSER is read
      only with ALLUSERS=2); INSTALLFOLDER and DESKTOPSHORTCUT have no default value (the desktop checkbox starts unchecked).
    * NO CODE: CustomAction is EXACTLY the expected list (all type 51, setting a property; type 35, setting a directory,
      is also allowed but unused), plus one `TeselaEs_<Text>` action for each UI string. No DLLs, executables, or Binary
      rows. InstallUISequence and InstallExecuteSequence entries for these actions (action, condition, and sequence) match.
    * MajorUpgrade and blocking of newer versions, with exact values (see $MsiExpectedUpgradeRows).
    * Exactly FIVE LaunchConditions: blocking newer versions, scope checks (both directions, with the current user's
      per-user install taking precedence), and directory checks (protected for per-user installs, valid path format), each with a message.
    * UI: expected dialogs, control types without images, native control events only (NewDialog, SpawnDialog, EndDialog,
      SetTargetPath, Reinstall...), properties that can change dialogs, valid NewDialog/SpawnDialog targets, exact rows
      for the scope and directory dialogs, and every displayed `[Tx...]` string present in both English and Spanish
      (property + type 51 action + sequence row).
    * Identifiers and properties are compared CASE-SENSITIVELY (Windows Installer is case-sensitive: an ActionProperty
      `wix_downgrade_detected` is not the property read by the LaunchCondition).

  This script is ASCII-only by design (Windows PowerShell 5.1 reads .ps1 files without a BOM as ANSI).
#>

# UpgradeCode de installer/Tesela.wxs: NO CAMBIAR NUNCA (ver installer/README.md). Aqui se repite para que un cambio
# accidental en el .wxs rompa la validacion.
$MsiExpectedUpgradeCode = '{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}'
$MsiExpectedFiles = @('tesela.exe', 'tesela-scan.exe', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY-NOTICES.md')
# Arbol de carpetas EXACTO de un MSI de Tesela (Directory -> Directory_Parent), tal como lo genera installer/Tesela.wxs.
# INSTALLFOLDER cuelga de TARGETDIR y su ruta REAL la fijan las acciones de tipo 51 segun el ambito (por usuario
# [LocalAppDataFolder]Programs\Tesela\, para todos [%ProgramW6432]\Tesela\ (variable de entorno: en el contexto por usuario Windows
# Installer redirige [ProgramFiles64Folder] al perfil), o la de la instalacion anterior); el nombre
# `Tesela` solo vale si nada la fija. ProgramMenuFolder y DesktopFolder dependen de ALLUSERS (el del usuario o el comun). DefaultDir
# solo se fija donde el .wxs pone el nombre ($null = no se comprueba: son los nombres cortos que WiX da a las carpetas
# estandar, `PMenu`, `Desktop`, `SourceDir`, y no son decision de Tesela).
$MsiExpectedDirectories = @(
    @{ Id = 'TARGETDIR'; Parent = ''; DefaultDir = $null },
    @{ Id = 'INSTALLFOLDER'; Parent = 'TARGETDIR'; DefaultDir = 'Tesela' },
    @{ Id = 'ProgramMenuFolder'; Parent = 'TARGETDIR'; DefaultDir = $null },
    @{ Id = 'DesktopFolder'; Parent = 'TARGETDIR'; DefaultDir = $null }
)

# Filas EXACTAS de la tabla Upgrade que genera <MajorUpgrade AllowSameVersionUpgrades="yes"> de installer/Tesela.wxs. `V` es
# la version del producto (ProductVersion del MSI), por eso VersionMin/VersionMax no son una cadena fija. Las dos filas llevan
# UpgradeCode = $MsiExpectedUpgradeCode y Language = 1033 (el idioma del paquete: solo se detectan versiones en ese idioma).
#   * Reemplazar (ActionProperty WIX_UPGRADE_DETECTED): VersionMin vacio y VersionMax = V, Attributes = 513 = 1 + 512:
#       1   msidbUpgradeAttributesMigrateFeatures     conserva la seleccion de caracteristicas de la version vieja;
#       512 msidbUpgradeAttributesVersionMaxInclusive el limite superior V cuenta (AllowSameVersionUpgrades: reinstalar la
#           MISMA version tambien reemplaza; por eso build-msi.ps1 acepta el aviso ICE61).
#     Es decir: toda version <= V se desinstala al instalar esta (RemoveExistingProducts).
#   * Bloquear versiones posteriores (ActionProperty WIX_DOWNGRADE_DETECTED): VersionMin = V y VersionMax vacio, Attributes =
#     2 = msidbUpgradeAttributesOnlyDetect (solo marca la propiedad, no desinstala NADA). Sin el bit 256
#     (VersionMinInclusive) el limite inferior no cuenta: version > V estricta. La LaunchCondition `NOT WIX_DOWNGRADE_DETECTED`
#     convierte esa deteccion en un error con mensaje.
# Ninguna lleva Remove (que borraria caracteristicas concretas) ni el bit 1024 (idiomas excluyentes).
$MsiExpectedUpgradeRows = @(
    @{ Name = 'reemplazar versiones anteriores'; ActionProperty = 'WIX_UPGRADE_DETECTED'; Min = ''; MaxIsVersion = $true; Attributes = '513' },
    @{ Name = 'bloquear versiones posteriores'; ActionProperty = 'WIX_DOWNGRADE_DETECTED'; Min = 'V'; MaxIsVersion = $false; Attributes = '2' }
)
$MsiExpectedLanguage = '1033'
# GUID de componente FIJOS de installer/Tesela.wxs (en mayusculas y entre llaves, como los guarda la tabla Component). Un cambio
# accidental dejaria dos componentes distintos para el mismo archivo entre versiones. Los seis primeros son los de la v0.1.4.
$MsiExpectedComponentIds = @{
    'TeselaExe'         = '{B684DA7F-4E1F-49DD-9ED7-A7D2C72A1B22}'
    'TeselaScanExe'     = '{F6AC4BAC-3E83-4784-ACA7-50ED49A1D3D9}'
    'LicenseMit'        = '{1B50C86F-50F5-4EEF-B97B-BE62D82BBB9D}'
    'LicenseApache'     = '{100B9CE0-2501-4884-8A2D-FE074CF71F06}'
    'ThirdPartyNotices' = '{0FFBF222-BD3C-4B09-A7A1-97CA3D105F13}'
    'StartMenuShortcut' = '{089416B7-8B88-4F2D-9636-96931BE9FCC0}'
    'DesktopShortcut'   = '{5C2D6C0B-7C3E-4B6B-9B7A-3E0D1A9F6D41}'
}
# Carpeta de cada componente, y condicion (solo el acceso directo del escritorio la lleva: DESKTOPSHORTCUT="1").
$MsiExpectedComponentDirs = @{
    'TeselaExe' = 'INSTALLFOLDER'; 'TeselaScanExe' = 'INSTALLFOLDER'; 'LicenseMit' = 'INSTALLFOLDER'
    'LicenseApache' = 'INSTALLFOLDER'; 'ThirdPartyNotices' = 'INSTALLFOLDER'
    'StartMenuShortcut' = 'ProgramMenuFolder'; 'DesktopShortcut' = 'DesktopFolder'
}
$MsiExpectedDesktopCondition = 'DESKTOPSHORTCUT="1"'
# Atributos de cada componente: 260 = 4 (la clave de registro es el KeyPath) + 256 (64 bits: el paquete es x64).
$MsiExpectedComponentAttributes = '260'

# Valores de registro EXACTOS (Raiz -1 = HKMU: HKCU en una instalacion por usuario, HKLM en una para todos).
$MsiExpectedRegistryKey = 'Software\Tesela'
$MsiExpectedRegistry = @(
    @{ Name = 'tesela.exe'; Value = '#1'; Component = 'TeselaExe' },
    @{ Name = 'InstallFolder'; Value = '[INSTALLFOLDER]'; Component = 'TeselaExe' },
    @{ Name = 'tesela-scan.exe'; Value = '#1'; Component = 'TeselaScanExe' },
    @{ Name = 'LICENSE-MIT'; Value = '#1'; Component = 'LicenseMit' },
    @{ Name = 'LICENSE-APACHE'; Value = '#1'; Component = 'LicenseApache' },
    @{ Name = 'THIRD-PARTY-NOTICES.md'; Value = '#1'; Component = 'ThirdPartyNotices' },
    @{ Name = 'StartMenuShortcut'; Value = '#1'; Component = 'StartMenuShortcut' },
    @{ Name = 'DesktopShortcut'; Value = '1'; Component = 'DesktopShortcut' }
)
# Las dos acciones de carpeta recuerdan donde se instalo: InstallFolder; el acceso directo del escritorio se recuerda con su
# propio valor DesktopShortcut="1" (que es KeyPath del componente: aparece y desaparece con el).

# Busquedas de registro de instalaciones previas: raiz 1 = HKCU, 2 = HKLM; tipo 18 = 2 (valor crudo) + 16 (vista de 64 bits). Las dos
# *Exe comprueban que tesela.exe EXISTE en la carpeta recordada (tipo 16 = directorio en la vista de 64 bits, con una fila de la tabla
# Signature que busca tesela.exe): una clave sin producto (restos de versiones antiguas) no cuenta.
$MsiExpectedRegLocators = @(
    @{ Id = 'PrevUserFolder'; Root = '1'; Name = 'InstallFolder'; Property = 'TESELA_PREV_USER'; Type = '18'; File = $null },
    @{ Id = 'PrevMachineFolder'; Root = '2'; Name = 'InstallFolder'; Property = 'TESELA_PREV_MACHINE'; Type = '18'; File = $null },
    @{ Id = 'PrevUserDesktop'; Root = '1'; Name = 'DesktopShortcut'; Property = 'TESELA_PREV_DESKTOP_USER'; Type = '18'; File = $null },
    @{ Id = 'PrevMachineDesktop'; Root = '2'; Name = 'DesktopShortcut'; Property = 'TESELA_PREV_DESKTOP_MACHINE'; Type = '18'; File = $null },
    @{ Id = 'PrevUserExe'; Root = '1'; Name = 'InstallFolder'; Property = 'TESELA_PREV_USER_EXE'; Type = '16'; File = 'tesela.exe' },
    @{ Id = 'PrevMachineExe'; Root = '2'; Name = 'InstallFolder'; Property = 'TESELA_PREV_MACHINE_EXE'; Type = '16'; File = 'tesela.exe' }
)

# Condiciones de lanzamiento EXACTAS (la primera la anade MajorUpgrade). "Para todos" = ALLUSERS=1, o ALLUSERS=2 con
# MSIINSTALLPERUSER vacio. Las cuatro ultimas dejan pasar siempre Installed (reparar/desinstalar) y la interfaz completa (UILevel 5:
# las propias ventanas avisan y no dejan seguir). La 3 (para todos instalada, por usuario pedido) no bloquea si ESTE usuario ya tiene
# su instalacion por usuario: la suya gana. La 5 exige una ruta completa con unidad y sin /, .., ? ni ruta de red. Ver installer/Tesela.wxs.
$MsiExpectedLaunchConditions = @(
    'NOT WIX_DOWNGRADE_DETECTED',
    'Installed OR UILevel>=5 OR NOT (ALLUSERS=1 OR (ALLUSERS=2 AND NOT MSIINSTALLPERUSER)) OR TESELA_PREV_USER=""',
    'Installed OR UILevel>=5 OR (ALLUSERS=1 OR (ALLUSERS=2 AND NOT MSIINSTALLPERUSER)) OR TESELA_PREV_MACHINE="" OR TESELA_PREV_USER<>""',
    'Installed OR UILevel>=5 OR ALLUSERS=1 OR (ALLUSERS=2 AND NOT MSIINSTALLPERUSER) OR NOT (INSTALLFOLDER~<<TESELA_PF64 OR INSTALLFOLDER~<<TESELA_WIN)',
    'Installed OR UILevel>=5 OR (INSTALLFOLDER><":\" AND NOT (INSTALLFOLDER<<"\\" OR INSTALLFOLDER><"/" OR INSTALLFOLDER><".." OR INSTALLFOLDER><"?"))'
)

# Propiedades seguras: las que de verdad llegan al servidor en una instalacion elevada. EXACTAMENTE estas.
$MsiExpectedSecure = @('DESKTOPSHORTCUT', 'INSTALLFOLDER', 'WIX_DOWNGRADE_DETECTED', 'WIX_UPGRADE_DETECTED')
# Propiedades que SI pueden estar en la tabla Property (ademas de las que empiezan por `Tx`: los textos en ingles).
$MsiAllowedProperties = @(
    'UpgradeCode', 'ARPPRODUCTICON', 'ARPURLINFOABOUT', 'ARPHELPLINK', 'ARPNOMODIFY', 'ALLUSERS', 'MSIINSTALLPERUSER', 'Manufacturer',
    'ProductCode', 'ProductLanguage', 'ProductName', 'ProductVersion', 'SecureCustomProperties', 'TESELA_SCOPE',
    'RestartManagerOption', 'DefaultUIFont', 'ErrorDialog'
)

# Lista CERRADA de tablas de un MSI de Tesela. Una tabla que no esta aqui es un fallo, tenga o no filas.
$MsiAllowedTables = @(
    '_Validation', 'AdminExecuteSequence', 'AdminUISequence', 'AdvtExecuteSequence', 'AppSearch', 'CheckBox', 'Component',
    'Control', 'ControlCondition', 'ControlEvent', 'CustomAction', 'Dialog', 'Directory', 'EventMapping', 'Feature',
    'FeatureComponents', 'File', 'Icon', 'InstallExecuteSequence', 'InstallUISequence', 'LaunchCondition', 'ListBox',
    'Media', 'MsiFileHash', 'Property', 'RadioButton', 'RegLocator', 'Registry', 'RemoveFile', 'Shortcut', 'Signature',
    'TextStyle', 'UIText', 'ActionText', 'Upgrade'
)

# --- Acciones personalizadas: SOLO de tipo 51 (fijar una propiedad) o 35 (fijar una carpeta). Estas son las de la logica
# (ambito, carpeta, escritorio, idioma); las de los textos son `TeselaEs_<Texto>` y se validan aparte. -------------------------
$MsiExpectedCustomActions = @(
    @{ Action = 'TeselaStaleUser'; Type = '51'; Source = 'TESELA_PREV_USER'; Target = '' },
    @{ Action = 'TeselaStaleMachine'; Type = '51'; Source = 'TESELA_PREV_MACHINE'; Target = '' },
    @{ Action = 'TeselaSetDefaultUser'; Type = '51'; Source = 'TESELA_DEFAULT_USER'; Target = '[LocalAppDataFolder]Programs\Tesela\' },
    @{ Action = 'TeselaSetDefaultMachine'; Type = '51'; Source = 'TESELA_DEFAULT_MACHINE'; Target = '[%ProgramW6432]\Tesela\' },
    @{ Action = 'TeselaSetProtected64'; Type = '51'; Source = 'TESELA_PF64'; Target = '[%ProgramW6432]' },
    @{ Action = 'TeselaSetProtectedWin'; Type = '51'; Source = 'TESELA_WIN'; Target = '[%SystemRoot]' },
    @{ Action = 'TeselaFolderPrevUser'; Type = '51'; Source = 'INSTALLFOLDER'; Target = '[TESELA_PREV_USER]' },
    @{ Action = 'TeselaFolderPrevMachine'; Type = '51'; Source = 'INSTALLFOLDER'; Target = '[TESELA_PREV_MACHINE]' },
    @{ Action = 'TeselaFolderDefaultUser'; Type = '51'; Source = 'INSTALLFOLDER'; Target = '[TESELA_DEFAULT_USER]' },
    @{ Action = 'TeselaFolderDefaultMachine'; Type = '51'; Source = 'INSTALLFOLDER'; Target = '[TESELA_DEFAULT_MACHINE]' },
    @{ Action = 'TeselaRememberDesktop'; Type = '51'; Source = 'DESKTOPSHORTCUT'; Target = '1' },
    @{ Action = 'TeselaScopeMachine'; Type = '51'; Source = 'TESELA_SCOPE'; Target = 'machine' },
    @{ Action = 'TeselaLangEs1'; Type = '51'; Source = 'TESELA_LANG'; Target = 'es' },
    @{ Action = 'TeselaLangEs2'; Type = '51'; Source = 'TESELA_LANG'; Target = 'es' },
    @{ Action = 'TeselaLangEs3'; Type = '51'; Source = 'TESELA_LANG'; Target = 'es' },
    @{ Action = 'TeselaLangEn'; Type = '51'; Source = 'TESELA_LANG'; Target = 'en' }
)
$MsiAllowedCustomActionTypes = @('51', '35')

# Filas de las secuencias (accion, condicion y numero). Las dos secuencias comparten las de la carpeta y el escritorio; solo
# la de interfaz lleva el ambito preelegido y el idioma. En la del servidor, TeselaRememberDesktop solo se aplica si NO hay
# interfaz completa (UILevel<5): con interfaz, la casilla ya parte del valor recordado y el servidor no debe volver a ponerlo si
# el usuario la desmarco. `Sequence` entre CostFinalize (1000) y MigrateFeatureStates (1200) para lo de la interfaz.
$MsiCtxMachine = '(ALLUSERS=1 OR (ALLUSERS=2 AND NOT MSIINSTALLPERUSER))'
$MsiRememberCond = 'DESKTOPSHORTCUT="" AND ((TESELA_PREV_DESKTOP_USER="1" AND TESELA_PREV_USER<>"") OR (TESELA_PREV_DESKTOP_MACHINE="1" AND TESELA_PREV_MACHINE<>""))'
$MsiExpectedLogicRows = @(
    @{ Action = 'TeselaStaleUser'; Seq = 51; Cond = 'TESELA_PREV_USER<>"" AND TESELA_PREV_USER_EXE=""'; Where = 'both' },
    @{ Action = 'TeselaStaleMachine'; Seq = 52; Cond = 'TESELA_PREV_MACHINE<>"" AND TESELA_PREV_MACHINE_EXE=""'; Where = 'both' },
    @{ Action = 'TeselaSetDefaultUser'; Seq = 53; Cond = ''; Where = 'both' },
    @{ Action = 'TeselaSetDefaultMachine'; Seq = 54; Cond = ''; Where = 'both' },
    @{ Action = 'TeselaSetProtected64'; Seq = 55; Cond = ''; Where = 'both' },
    @{ Action = 'TeselaSetProtectedWin'; Seq = 56; Cond = ''; Where = 'both' },
    @{ Action = 'TeselaFolderPrevUser'; Seq = 57; Cond = ('TESELA_PREV_USER<>"" AND NOT ' + $MsiCtxMachine); Where = 'both' },
    @{ Action = 'TeselaFolderPrevMachine'; Seq = 58; Cond = ('TESELA_PREV_MACHINE<>"" AND ' + $MsiCtxMachine); Where = 'both' },
    @{ Action = 'TeselaFolderDefaultUser'; Seq = 59; Cond = ('INSTALLFOLDER="" AND NOT ' + $MsiCtxMachine); Where = 'both' },
    @{ Action = 'TeselaFolderDefaultMachine'; Seq = 60; Cond = ('INSTALLFOLDER="" AND ' + $MsiCtxMachine); Where = 'both' },
    @{ Action = 'TeselaRememberDesktop'; Seq = 61; Cond = $MsiRememberCond; Where = 'ui' },
    @{ Action = 'TeselaRememberDesktop'; Seq = 61; Cond = ('UILevel<5 AND ' + $MsiRememberCond); Where = 'exec' },
    @{ Action = 'TeselaScopeMachine'; Seq = 1001; Cond = 'TESELA_SCOPE="user" AND (ALLUSERS=1 OR (TESELA_PREV_MACHINE<>"" AND TESELA_PREV_USER=""))'; Where = 'ui' },
    @{ Action = 'TeselaLangEn'; Seq = 1006; Cond = 'TESELA_LANG=""'; Where = 'ui' }
)
# LANGID del espanol (cualquier variante): las tres acciones TeselaLangEs1..3 juntas los cubren todos (la condicion admite
# 255 caracteres, por eso van en tres).
$MsiSpanishLangIds = @(1034, 3082, 2058, 4106, 5130, 6154, 7178, 8202, 9226, 10250, 11274, 12298, 13322, 14346, 15370, 16394, 17418, 18442, 19466, 20490, 21514, 22538, 23562)
$MsiLangEsSeq = @{ 'TeselaLangEs1' = 1003; 'TeselaLangEs2' = 1004; 'TeselaLangEs3' = 1005 }

# --- Interfaz -------------------------------------------------------------------------------------------------------------------
$MsiExpectedDialogs = @(
    'WelcomeDlg', 'ScopeDlg', 'FolderDlg', 'InvalidDirDlg', 'BrowseDlg', 'VerifyReadyDlg', 'WaitForCostingDlg', 'MaintenanceDlg',
    'ProgressDlg', 'ExitDialog', 'FatalError', 'UserExit', 'CancelDlg', 'ErrorDlg', 'FilesInUse', 'MsiRMFilesInUse'
)
# Solo controles SIN imagen (los iconos y los mapas de bits necesitan la tabla Binary).
$MsiAllowedControlTypes = @('Text', 'Line', 'PushButton', 'PathEdit', 'CheckBox', 'RadioButtonGroup', 'ProgressBar', 'ListBox', 'DirectoryCombo', 'DirectoryList')
# Eventos de control NATIVOS de Windows Installer. Nada de DoAction (llamaria a una accion personalizada).
$MsiAllowedControlEvents = @('NewDialog', 'SpawnDialog', 'SpawnWaitDialog', 'EndDialog', 'SetTargetPath', 'DirectoryListUp', 'DirectoryListNew',
    'Reset', 'Reinstall', 'ReinstallMode', 'Remove', 'RMShutdownAndRestart')
# Propiedades que las ventanas pueden cambiar con un evento `[Propiedad]`.
$MsiAllowedPublishedProperties = @('ALLUSERS', 'MSIINSTALLPERUSER', 'INSTALLFOLDER', '_BrowseProperty', 'TESELA_FOLDER_OK')
$MsiAllowedEndDialogValues = @('Return', 'Exit', 'Retry', 'Ignore', 'ErrorYes', 'ErrorNo', 'ErrorAbort', 'ErrorCancel', 'ErrorIgnore', 'ErrorOk', 'ErrorRetry')
$MsiAllowedSetTargetPath = @('INSTALLFOLDER', '[_BrowseProperty]')
$MsiExpectedEventMappings = @('BrowseDlg|DirectoryCombo|IgnoreChange|IgnoreChange', 'ProgressDlg|ActionText|ActionText|Text',
    'ProgressDlg|ActionData|ActionData|Text', 'ProgressDlg|ProgressBar|SetProgress|Progress')
# Acciones con texto de progreso (en espanol e ingles a la vez: ActionText es una sola tabla).
$MsiAllowedActionText = @('InstallInitialize', 'RemoveExistingProducts', 'ProcessComponents', 'InstallFiles', 'RemoveFiles', 'CreateFolders',
    'RemoveFolders', 'CreateShortcuts', 'RemoveShortcuts', 'WriteRegistryValues', 'RemoveRegistryValues', 'RegisterProduct',
    'PublishProduct', 'InstallFinalize')
# Filas de la ventana de ambito y de la de carpeta (el corazon de la logica de la interfaz), una a una:
# Dialog|Control|Evento|Argumento|Condicion|Orden
$MsiExpectedScopeEvents = @(
    'ScopeDlg|Next|[ALLUSERS]|1|TESELA_SCOPE="machine"|1',
    'ScopeDlg|Next|[MSIINSTALLPERUSER]|{}|TESELA_SCOPE="machine"|2',
    'ScopeDlg|Next|[ALLUSERS]|2|TESELA_SCOPE="user"|3',
    'ScopeDlg|Next|[MSIINSTALLPERUSER]|1|TESELA_SCOPE="user"|4',
    'ScopeDlg|Next|[INSTALLFOLDER]|[TESELA_PREV_MACHINE]|TESELA_SCOPE="machine" AND TESELA_PREV_MACHINE<>""|5',
    'ScopeDlg|Next|[INSTALLFOLDER]|[TESELA_PREV_USER]|TESELA_SCOPE="user" AND TESELA_PREV_USER<>""|6',
    'ScopeDlg|Next|[INSTALLFOLDER]|[TESELA_DEFAULT_MACHINE]|TESELA_SCOPE="machine" AND TESELA_PREV_MACHINE="" AND (INSTALLFOLDER="" OR INSTALLFOLDER~=TESELA_DEFAULT_USER)|7',
    'ScopeDlg|Next|[INSTALLFOLDER]|[TESELA_DEFAULT_USER]|TESELA_SCOPE="user" AND TESELA_PREV_USER="" AND (INSTALLFOLDER="" OR INSTALLFOLDER~=TESELA_DEFAULT_MACHINE)|8',
    'ScopeDlg|Next|NewDialog|FolderDlg|1|9',
    'FolderDlg|Next|SetTargetPath|INSTALLFOLDER|1|1'
)
$MsiExpectedScopeDisable = '(TESELA_SCOPE="machine" AND TESELA_PREV_USER<>"") OR (TESELA_SCOPE="user" AND TESELA_PREV_MACHINE<>"" AND TESELA_PREV_USER="")'
# TODOS los eventos de propiedad (`[X]`) de TODAS las ventanas, exactos: ningun otro boton puede cambiar el ambito, la carpeta ni nada.
# Dialog|Control|Evento|Argumento|Condicion|Orden
$MsiFolderValid = 'INSTALLFOLDER><":\" AND NOT (INSTALLFOLDER<<"\\" OR INSTALLFOLDER><"/" OR INSTALLFOLDER><".." OR INSTALLFOLDER><"?")'
$MsiExpectedPropertyEvents = @(
    $MsiExpectedScopeEvents | Where-Object { $_ -clike 'ScopeDlg|Next|[[]*' }
    'FolderDlg|ChangeFolder|[_BrowseProperty]|INSTALLFOLDER|1|1',
    'FolderDlg|Next|[TESELA_FOLDER_OK]|{}|1|2',
    ('FolderDlg|Next|[TESELA_FOLDER_OK]|1|' + $MsiFolderValid + '|3'),
    'FolderDlg|Next|[TESELA_FOLDER_OK]|{}|TESELA_SCOPE="user" AND (INSTALLFOLDER~<<TESELA_PF64 OR INSTALLFOLDER~<<TESELA_WIN)|4'
)
# TODOS los EndDialog (Dialogo|Boton|Valor), exactos: un EndDialog de mas en la bienvenida se saltaria todas las ventanas.
$MsiExpectedEndDialogs = @(
    'ErrorDlg|Y|ErrorYes', 'ErrorDlg|A|ErrorAbort', 'ErrorDlg|C|ErrorCancel', 'ErrorDlg|I|ErrorIgnore', 'ErrorDlg|N|ErrorNo', 'ErrorDlg|O|ErrorOk',
    'ErrorDlg|R|ErrorRetry', 'InvalidDirDlg|OK|Return', 'BrowseDlg|OK|Return', 'BrowseDlg|Cancel|Return', 'VerifyReadyDlg|Install|Return',
    'VerifyReadyDlg|InstallElevated|Return', 'WaitForCostingDlg|Return|Exit', 'MaintenanceDlg|Repair|Return', 'MaintenanceDlg|Remove|Return',
    'ExitDialog|Finish|Return', 'FatalError|Finish|Exit', 'UserExit|Finish|Exit', 'CancelDlg|Yes|Exit', 'CancelDlg|No|Return',
    'FilesInUse|Retry|Retry', 'FilesInUse|Ignore|Ignore', 'FilesInUse|Exit|Exit', 'MsiRMFilesInUse|OK|Return', 'MsiRMFilesInUse|Cancel|Exit'
)

function Open-MsiDatabase {
    # Abre el MSI. $Write = $false: solo lectura (lo que usa build-msi.ps1). Las pruebas abren COPIAS en modo escritura.
    param([string]$Path, [bool]$Write = $false)
    $installer = New-Object -ComObject WindowsInstaller.Installer
    # 0 = msiOpenDatabaseModeReadOnly, 1 = msiOpenDatabaseModeTransact
    $mode = if ($Write) { 1 } else { 0 }
    $db = [System.__ComObject].InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @($Path, $mode))
    $obj = [pscustomobject]@{ Installer = $installer; Db = $db; Tables = $null }
    $names = New-Object System.Collections.Generic.List[string]
    foreach ($r in (Get-MsiRows $obj 'SELECT `Name` FROM `_Tables`' 1 -NoTableCheck)) { $names.Add($r[0]) }
    $obj.Tables = $names.ToArray()
    return $obj
}

function Get-MsiRows {
    # Devuelve las filas (cada una, un array de cadenas) de la consulta. Una tabla que NO existe en el MSI (no figura en
    # `_Tables`) da cero filas; CUALQUIER otro error de la API se propaga: un fallo de lectura no puede pasar por "tabla
    # vacia".
    param($Msi, [string]$Sql, [int]$Columns, [switch]$NoTableCheck)
    if (-not $NoTableCheck) {
        $m = [regex]::Match($Sql, 'FROM\s+`([^`]+)`')
        if (-not $m.Success) { throw "consulta sin tabla: $Sql" }
        if ($Msi.Tables -notcontains $m.Groups[1].Value) { return @() }
    }
    $rows = New-Object System.Collections.Generic.List[object]
    $view = [System.__ComObject].InvokeMember('OpenView', 'InvokeMethod', $null, $Msi.Db, @($Sql))
    [System.__ComObject].InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    while ($true) {
        $rec = [System.__ComObject].InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
        if ($null -eq $rec) { break }
        $vals = @()
        for ($i = 1; $i -le $Columns; $i++) {
            $vals += [string][System.__ComObject].InvokeMember('StringData', 'GetProperty', $null, $rec, @($i))
        }
        $rows.Add($vals)
        [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($rec)
    }
    [System.__ComObject].InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($view)
    return $rows.ToArray()
}

function Get-MsiSummaryProperty($Msi, [int]$Id) {
    $si = [System.__ComObject].InvokeMember('SummaryInformation', 'GetProperty', $null, $Msi.Db, @(0))
    $v = [System.__ComObject].InvokeMember('Property', 'GetProperty', $null, $si, @($Id))
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($si)
    return $v
}

function Close-Msi($Msi) {
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($Msi.Db)
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($Msi.Installer)
    [System.GC]::Collect()
    [System.GC]::WaitForPendingFinalizers()
}

function Test-MsiUserInterface {
    # Valida la interfaz y los textos (ver la cabecera). Recibe las propiedades ya leidas (distinguen mayusculas) y las dos
    # secuencias; anade los problemas a $Problems.
    param($Msi, $Props, $Seq, $Problems)
    $dlgRows = @(Get-MsiRows $Msi 'SELECT `Dialog`, `Attributes`, `Title` FROM `Dialog`' 3)
    $dlgIds = @($dlgRows | ForEach-Object { $_[0] })
    foreach ($d in $MsiExpectedDialogs) { if ($dlgIds -cnotcontains $d) { $Problems.Add("falta la ventana $d") } }
    foreach ($d in $dlgIds) { if ($MsiExpectedDialogs -cnotcontains $d) { $Problems.Add("ventana no esperada: $d") } }
    if ($Props['ErrorDialog'] -cne 'ErrorDlg') { $Problems.Add("la propiedad ErrorDialog es '$($Props['ErrorDialog'])', se esperaba 'ErrorDlg'") }

    # Controles: solo tipos sin imagen. Textos referenciados: `[Tx...]`.
    $ctl = @(Get-MsiRows $Msi 'SELECT `Dialog_`, `Control`, `Type`, `Property`, `Text`, `Attributes` FROM `Control`' 6)
    $referenced = New-Object System.Collections.Generic.HashSet[string] ([System.StringComparer]::Ordinal)
    foreach ($c in $ctl) {
        if ($MsiAllowedControlTypes -cnotcontains $c[2]) { $Problems.Add("control no permitido: $($c[0]).$($c[1]) es de tipo $($c[2]) (solo controles sin imagen)") }
        foreach ($m in [regex]::Matches($c[4], '\[([A-Za-z0-9_]+)\]')) {
            $n = $m.Groups[1].Value
            if ($n -clike 'Tx*') { [void]$referenced.Add($n) }
        }
    }
    foreach ($d in $dlgRows) {
        foreach ($m in [regex]::Matches($d[2], '\[([A-Za-z0-9_]+)\]')) { if ($m.Groups[1].Value -clike 'Tx*') { [void]$referenced.Add($m.Groups[1].Value) } }
    }
    $rb = @(Get-MsiRows $Msi 'SELECT `Property`, `Value`, `Text` FROM `RadioButton`' 3)
    foreach ($r in $rb) {
        foreach ($m in [regex]::Matches($r[2], '\[([A-Za-z0-9_]+)\]')) { if ($m.Groups[1].Value -clike 'Tx*') { [void]$referenced.Add($m.Groups[1].Value) } }
    }
    # El boton Instalar de "para todos" lleva el escudo de UAC (ElevationShield, 0x00800000): Windows pedira administrador al seguir.
    $shield = @($ctl | Where-Object { $_[0] -ceq 'VerifyReadyDlg' -and $_[1] -ceq 'InstallElevated' })
    if ($shield.Count -cne 1 -or (([int]$shield[0][5]) -band 8388608) -eq 0) { $Problems.Add('VerifyReadyDlg.InstallElevated (el boton Instalar de "para todos") tiene que llevar el escudo de UAC (ElevationShield)') }
    # La casilla del escritorio: propiedad DESKTOPSHORTCUT con valor 1.
    $cb = @(Get-MsiRows $Msi 'SELECT `Property`, `Value` FROM `CheckBox`' 2)
    if ($cb.Count -cne 1 -or $cb[0][0] -cne 'DESKTOPSHORTCUT' -or $cb[0][1] -cne '1') {
        $Problems.Add('la tabla CheckBox tiene que tener exactamente una casilla: DESKTOPSHORTCUT = 1')
    }
    $fold = @($ctl | Where-Object { $_[0] -ceq 'FolderDlg' -and $_[1] -ceq 'FolderEdit' })
    if ($fold.Count -cne 1 -or $fold[0][2] -cne 'PathEdit' -or $fold[0][3] -cne 'INSTALLFOLDER') { $Problems.Add('FolderDlg.FolderEdit tiene que ser un PathEdit sobre INSTALLFOLDER') }
    $scope = @($ctl | Where-Object { $_[0] -ceq 'ScopeDlg' -and $_[1] -ceq 'ScopeGroup' })
    if ($scope.Count -cne 1 -or $scope[0][2] -cne 'RadioButtonGroup' -or $scope[0][3] -cne 'TESELA_SCOPE') { $Problems.Add('ScopeDlg.ScopeGroup tiene que ser un grupo de opciones sobre TESELA_SCOPE') }
    $scopeValues = @($rb | Where-Object { $_[0] -ceq 'TESELA_SCOPE' } | ForEach-Object { $_[1] } | Sort-Object)
    if (($scopeValues -join ',') -cne 'machine,user') { $Problems.Add("el grupo de ambito tiene que tener las opciones 'user' y 'machine'; tiene: $($scopeValues -join ',')") }
    if ($Props['TESELA_SCOPE'] -cne 'user') { $Problems.Add("TESELA_SCOPE por defecto = '$($Props['TESELA_SCOPE'])', se esperaba 'user' (por usuario como predeterminado)") }

    # Eventos de control: solo nativos, y cada ventana a la que apuntan existe.
    $ev = @(Get-MsiRows $Msi 'SELECT `Dialog_`, `Control_`, `Event`, `Argument`, `Condition`, `Ordering` FROM `ControlEvent`' 6)
    foreach ($e in $ev) {
        $name = "$($e[0]).$($e[1])"
        $isProp = $e[2] -cmatch '^\[(.+)\]$'
        if ($isProp) {
            $pn = $Matches[1]
            if ($MsiAllowedPublishedProperties -cnotcontains $pn) { $Problems.Add("evento de control sobre una propiedad no permitida: $name cambia [$pn]") }
        } elseif ($MsiAllowedControlEvents -cnotcontains $e[2]) {
            $Problems.Add("evento de control no permitido: $name usa $($e[2]) (solo eventos nativos de Windows Installer)")
        }
        if (@('NewDialog', 'SpawnDialog', 'SpawnWaitDialog') -ccontains $e[2] -and $dlgIds -cnotcontains $e[3]) { $Problems.Add("$name abre la ventana '$($e[3])', que no existe") }
        if ($e[2] -ceq 'EndDialog' -and $MsiAllowedEndDialogValues -cnotcontains $e[3]) { $Problems.Add("${name}: EndDialog con el valor '$($e[3])'") }
        if ($e[2] -ceq 'SetTargetPath' -and $MsiAllowedSetTargetPath -cnotcontains $e[3]) { $Problems.Add("${name}: SetTargetPath sobre '$($e[3])'") }
    }
    # Ambito y carpeta, fila a fila.
    $got = @($ev | Where-Object { $_[0] -ceq 'ScopeDlg' -or ($_[0] -ceq 'FolderDlg' -and $_[1] -ceq 'Next' -and $_[2] -ceq 'SetTargetPath') } | ForEach-Object { $_ -join '|' })
    foreach ($x in $MsiExpectedScopeEvents) { if ($got -cnotcontains $x) { $Problems.Add("falta el evento de la interfaz: $x") } }
    $scopeNext = @($ev | Where-Object { $_[0] -ceq 'ScopeDlg' -and $_[1] -ceq 'Next' })
    if ($scopeNext.Count -cne 9) { $Problems.Add("ScopeDlg.Next tiene que tener 9 eventos; tiene $($scopeNext.Count)") }
    # TODOS los eventos de propiedad de TODAS las ventanas: exactamente los esperados (ni uno mas, ni uno menos).
    $propEvents = @($ev | Where-Object { $_[2] -cmatch '^\[.+\]$' } | ForEach-Object { $_ -join '|' })
    foreach ($x in $MsiExpectedPropertyEvents) { if ($propEvents -cnotcontains $x) { $Problems.Add("falta el evento de propiedad esperado: $x") } }
    foreach ($x in $propEvents) { if ($MsiExpectedPropertyEvents -cnotcontains $x) { $Problems.Add("evento de propiedad NO esperado (una ventana cambia una propiedad que no debe): $x") } }
    # TODOS los EndDialog: exactamente los esperados.
    $ends = @($ev | Where-Object { $_[2] -ceq 'EndDialog' } | ForEach-Object { "$($_[0])|$($_[1])|$($_[3])" })
    foreach ($x in $MsiExpectedEndDialogs) { if ($ends -cnotcontains $x) { $Problems.Add("falta el EndDialog esperado: $x") } }
    foreach ($x in $ends) { if ($MsiExpectedEndDialogs -cnotcontains $x) { $Problems.Add("EndDialog NO esperado (cerraria una ventana donde no toca): $x") } }
    # FolderDlg.Next: SetTargetPath (1), la bandera TESELA_FOLDER_OK (2-4), NewDialog VerifyReadyDlg (5) y SpawnDialog InvalidDirDlg (6).
    $fn = @($ev | Where-Object { $_[0] -ceq 'FolderDlg' -and $_[1] -ceq 'Next' } | Sort-Object { [int]$_[5] })
    if ($fn.Count -cne 6 -or $fn[0][2] -cne 'SetTargetPath' -or $fn[4][2] -cne 'NewDialog' -or $fn[4][3] -cne 'VerifyReadyDlg' -or $fn[4][4] -cne 'TESELA_FOLDER_OK=1' -or $fn[5][2] -cne 'SpawnDialog' -or $fn[5][3] -cne 'InvalidDirDlg' -or $fn[5][4] -cne 'NOT TESELA_FOLDER_OK') {
        $Problems.Add('FolderDlg.Next tiene que hacer SetTargetPath, calcular TESELA_FOLDER_OK, abrir VerifyReadyDlg si es 1 y abrir InvalidDirDlg si no')
    }
    # Examinar usa el dialogo nativo (PathEdit, DirectoryCombo y DirectoryList sobre _BrowseProperty).
    $browse = @($ctl | Where-Object { $_[0] -ceq 'BrowseDlg' -and @('PathEdit', 'DirectoryCombo', 'DirectoryList') -ccontains $_[2] })
    if ($browse.Count -cne 3) { $Problems.Add('BrowseDlg tiene que tener PathEdit, DirectoryCombo y DirectoryList') }
    foreach ($b in $browse) { if ($b[3] -cne '_BrowseProperty') { $Problems.Add("BrowseDlg.$($b[1]) trabaja sobre '$($b[3])', se esperaba _BrowseProperty") } }
    # El boton Siguiente de la ventana de ambito se desactiva si el ambito choca con la instalacion existente.
    $cc = @(Get-MsiRows $Msi 'SELECT `Dialog_`, `Control_`, `Action`, `Condition` FROM `ControlCondition`' 4)
    $dis = @($cc | Where-Object { $_[0] -ceq 'ScopeDlg' -and $_[1] -ceq 'Next' -and $_[2] -ceq 'Disable' })
    if ($dis.Count -cne 1 -or $dis[0][3] -cne $MsiExpectedScopeDisable) { $Problems.Add('ScopeDlg.Next tiene que desactivarse cuando el ambito elegido choca con una instalacion del otro ambito') }
    $erDlg = @(Get-MsiRows $Msi 'SELECT `Dialog`, `Attributes` FROM `Dialog`' 2 | Where-Object { $_[0] -ceq 'ErrorDlg' })
    if ($erDlg.Count -eq 1 -and (([int]$erDlg[0][1]) -band 65536) -eq 0) { $Problems.Add('ErrorDlg no esta marcada como ventana de errores') }
    $em = @(Get-MsiRows $Msi 'SELECT `Dialog_`, `Control_`, `Event`, `Attribute` FROM `EventMapping`' 4 | ForEach-Object { $_ -join '|' })
    foreach ($x in $MsiExpectedEventMappings) { if ($em -cnotcontains $x) { $Problems.Add("falta la suscripcion de evento: $x") } }
    foreach ($x in $em) { if ($MsiExpectedEventMappings -cnotcontains $x) { $Problems.Add("suscripcion de evento no esperada: $x") } }
    $at = @(Get-MsiRows $Msi 'SELECT `Action` FROM `ActionText`' 1)
    foreach ($a in $at) { if ($MsiAllowedActionText -cnotcontains $a[0]) { $Problems.Add("texto de progreso de una accion no esperada: $($a[0])") } }
    $ui = @(Get-MsiRows $Msi 'SELECT `Key`, `Text` FROM `UIText`' 2)
    if ($ui.Count -cne 1 -or $ui[0][0] -cne 'NewFolder') { $Problems.Add('la tabla UIText tiene que tener solo NewFolder') }

    # Textos: cada propiedad Tx* (ingles) tiene su accion de tipo 51 TeselaEs_<id> (espanol), su fila en InstallUISequence con
    # TESELA_LANG="es" entre CostFinalize y las ventanas, y ninguna en InstallExecuteSequence; y todo texto mostrado existe.
    $ca = @(Get-MsiRows $Msi 'SELECT `Action`, `Type`, `Source`, `Target` FROM `CustomAction`' 4)
    $tx = @($Props.Keys | Where-Object { $_ -clike 'Tx*' } | Sort-Object)
    $costFinal = if ($Seq['InstallUISequence'].ContainsKey('CostFinalize')) { [int]$Seq['InstallUISequence']['CostFinalize'][2] } else { 1000 }
    $migrate = if ($Seq['InstallUISequence'].ContainsKey('MigrateFeatureStates')) { [int]$Seq['InstallUISequence']['MigrateFeatureStates'][2] } else { 1200 }
    $seqNums = New-Object System.Collections.Generic.HashSet[int]
    foreach ($id in $tx) {
        if ($Props[$id] -cmatch '[\[\]]') { $Problems.Add("el texto $id contiene corchetes (el valor de una propiedad no se vuelve a formatear): '$($Props[$id])'") }
        if ([string]::IsNullOrWhiteSpace($Props[$id])) { $Problems.Add("el texto $id esta vacio") }
        if (-not $referenced.Contains($id)) { $Problems.Add("el texto $id no se muestra en ninguna ventana (sobra)") }
        $es = @($ca | Where-Object { $_[0] -ceq "TeselaEs_$id" })
        if ($es.Count -cne 1) { $Problems.Add("el texto $id no tiene su version en espanol (accion TeselaEs_$id)"); continue }
        if ($es[0][1] -cne '51' -or $es[0][2] -cne $id -or [string]::IsNullOrWhiteSpace($es[0][3])) { $Problems.Add("TeselaEs_$id tiene que ser de tipo 51 sobre la propiedad $id y con texto") }
        if ($es[0][3] -cmatch '[\[\]]') { $Problems.Add("el texto en espanol de $id contiene corchetes") }
        $row = $Seq['InstallUISequence']["TeselaEs_$id"]
        if ($null -eq $row) { $Problems.Add("falta TeselaEs_$id en InstallUISequence"); continue }
        if ($row[1] -cne 'TESELA_LANG="es"') { $Problems.Add("TeselaEs_$id en InstallUISequence lleva la condicion '$($row[1])', se esperaba TESELA_LANG=""es""") }
        $n = [int]$row[2]
        if ($n -le 1006 -or $n -ge $migrate) { $Problems.Add("TeselaEs_$id (secuencia $n) tiene que ir despues de la deteccion de idioma (1006) y antes de las ventanas (MigrateFeatureStates, $migrate)") }
        if (-not $seqNums.Add($n)) { $Problems.Add("TeselaEs_$id comparte el numero de secuencia $n con otra accion de texto") }
        if ($Seq['InstallExecuteSequence'].ContainsKey("TeselaEs_$id")) { $Problems.Add("TeselaEs_$id esta en InstallExecuteSequence (los textos solo se cambian en la interfaz)") }
    }
    foreach ($c in $ca) {
        if ($c[0] -clike 'TeselaEs_*' -and $tx -cnotcontains $c[0].Substring(9)) { $Problems.Add("accion de texto sin propiedad en ingles: $($c[0])") }
    }
    foreach ($id in $referenced) { if (-not $Props.ContainsKey($id)) { $Problems.Add("una ventana muestra el texto [$id], que no esta en la tabla Property") } }
    if ($tx.Count -lt 10) { $Problems.Add("la interfaz tiene solo $($tx.Count) textos") }
}

function Test-MsiPackage {
    # Devuelve la lista de problemas del MSI (vacia si esta bien).
    param([string]$Path, [string]$ExpectedVersion)
    $problems = New-Object System.Collections.Generic.List[string]
    $msi = Open-MsiDatabase $Path
    try {
        # Tablas: lista cerrada.
        foreach ($t in $msi.Tables) {
            if ($MsiAllowedTables -cnotcontains $t) { $problems.Add("tabla no permitida en el MSI: $t") }
        }

        # Hashtable que distingue mayusculas (@{} no lo hace): los nombres de propiedad de Windows Installer si.
        $props = New-Object System.Collections.Hashtable ([System.StringComparer]::Ordinal)
        foreach ($r in (Get-MsiRows $msi 'SELECT `Property`, `Value` FROM `Property`' 2)) { $props[$r[0]] = $r[1] }
        if ($props['ProductVersion'] -cne $ExpectedVersion) { $problems.Add("ProductVersion = '$($props['ProductVersion'])', se esperaba '$ExpectedVersion'") }
        if ($props['UpgradeCode'] -cne $MsiExpectedUpgradeCode) { $problems.Add("UpgradeCode = '$($props['UpgradeCode'])': NO se puede cambiar (installer/README.md)") }
        if ($props['ProductName'] -cne 'Tesela') { $problems.Add("ProductName = '$($props['ProductName'])'") }
        if ($props['Manufacturer'] -cne 'Tesela contributors') { $problems.Add("Manufacturer = '$($props['Manufacturer'])'") }
        foreach ($p in 'ARPPRODUCTICON', 'ARPURLINFOABOUT') {
            if (-not $props[$p]) { $problems.Add("falta la propiedad $p (Aplicaciones instaladas)") }
        }
        if ($props['ARPURLINFOABOUT'] -cne 'https://github.com/wandoth1/tesela') { $problems.Add("ARPURLINFOABOUT = '$($props['ARPURLINFOABOUT'])'") }
        # Lista cerrada de propiedades (los textos de la interfaz empiezan por Tx).
        foreach ($k in $props.Keys) {
            if ($MsiAllowedProperties -cnotcontains $k -and $k -cnotlike 'Tx*') { $problems.Add("propiedad no esperada en la tabla Property: $k = '$($props[$k])'") }
        }

        # AMBITO DUAL: por usuario por defecto (ALLUSERS=2 y MSIINSTALLPERUSER=1; ALLUSERS=1 en la linea de comandos da la
        # instalacion para todos) y, en el Summary Information, el bit 3 del contador de palabras ("no hace falta elevar").
        $allUsers = $props['ALLUSERS']
        if (-not ($allUsers -ceq '2' -and $props['MSIINSTALLPERUSER'] -ceq '1')) { $problems.Add("no es por usuario por defecto: ALLUSERS='$allUsers' MSIINSTALLPERUSER='$($props['MSIINSTALLPERUSER'])' (se esperaba 2 y 1)") }
        $wordCount = [int](Get-MsiSummaryProperty $msi 15)
        if (($wordCount -band 8) -cne 0) { $problems.Add("el Summary Information declara 'no hace falta elevar' (bit 8 del contador de palabras = $wordCount): un paquete de doble modo no puede llevarlo, el motor ignora MSIINSTALLPERUSER y 'para todos' no pediria administrador (error 1925)") }
        # La carpeta y la casilla del escritorio parten SIN valor: por usuario y desmarcada por defecto.
        foreach ($p in 'INSTALLFOLDER', 'DESKTOPSHORTCUT') {
            if ($props.ContainsKey($p) -and $props[$p] -cne '') { $problems.Add("la propiedad $p tiene un valor por defecto en la tabla Property ('$($props[$p])'): tiene que partir vacia") }
        }

        $files = @(Get-MsiRows $msi 'SELECT `File`, `FileName`, `FileSize` FROM `File`' 3)
        $longNames = @($files | ForEach-Object { ($_[1] -split '\|')[-1] })
        foreach ($n in $MsiExpectedFiles) { if ($longNames -cnotcontains $n) { $problems.Add("falta el archivo $n") } }
        foreach ($n in $longNames) { if ($MsiExpectedFiles -cnotcontains $n) { $problems.Add("archivo inesperado en el MSI: $n") } }
        if ($files.Count -cne $MsiExpectedFiles.Count) { $problems.Add("el MSI lleva $($files.Count) archivos, se esperaban $($MsiExpectedFiles.Count)") }

        # SIN CODIGO: la tabla CustomAction es EXACTAMENTE la lista esperada mas una accion TeselaEs_<Texto> por texto; todas de
        # tipo 51 (propiedad) o 35 (carpeta). Nada de DLL, EXE, scripts ni Binary.
        $ca = @(Get-MsiRows $msi 'SELECT `Action`, `Type`, `Source`, `Target` FROM `CustomAction`' 4)
        $expectedCa = @{}
        foreach ($e in $MsiExpectedCustomActions) { $expectedCa[$e.Action] = $e }
        foreach ($c in $ca) {
            if ($MsiAllowedCustomActionTypes -cnotcontains $c[1]) { $problems.Add("accion personalizada de tipo $($c[1]) ($($c[0])): solo se admiten los tipos 51 (fijar una propiedad) y 35 (fijar una carpeta)"); continue }
            if ($c[0] -clike 'TeselaEs_*') { continue }
            if (-not $expectedCa.ContainsKey($c[0])) { $problems.Add("accion personalizada no esperada: $($c[0]) (tipo $($c[1]), origen '$($c[2])', destino '$($c[3])')"); continue }
            $e = $expectedCa[$c[0]]
            if ($c[1] -cne $e.Type -or $c[2] -cne $e.Source -or $c[3] -cne $e.Target) { $problems.Add("la accion $($c[0]) es tipo $($c[1]) origen '$($c[2])' destino '$($c[3])'; se esperaba tipo $($e.Type) origen '$($e.Source)' destino '$($e.Target)'") }
        }
        foreach ($e in $MsiExpectedCustomActions) {
            if (@($ca | Where-Object { $_[0] -ceq $e.Action }).Count -cne 1) { $problems.Add("falta la accion $($e.Action)") }
        }

        # Registro: SOLO HKMU\Software\Tesela con los 8 valores esperados.
        $reg = @(Get-MsiRows $msi 'SELECT `Registry`, `Root`, `Key`, `Name`, `Value`, `Component_` FROM `Registry`' 6)
        foreach ($r in $reg) {
            # -1 = HKEY_CURRENT_USER o HKEY_LOCAL_MACHINE segun el ambito (HKMU)
            if ($r[1] -cne '-1' -or $r[2] -cne $MsiExpectedRegistryKey) { $problems.Add("registro fuera de HKMU\Software\Tesela: raiz $($r[1]), clave '$($r[2])'") }
        }
        if ($reg.Count -ceq 0) { $problems.Add('no hay valores de registro (los componentes los necesitan como KeyPath)') }
        foreach ($e in $MsiExpectedRegistry) {
            $found = @($reg | Where-Object { $_[3] -ceq $e.Name })
            if ($found.Count -cne 1) { $problems.Add("el valor de registro $($e.Name): hay $($found.Count) filas, se esperaba 1"); continue }
            if ($found[0][4] -cne $e.Value -or $found[0][5] -cne $e.Component) { $problems.Add("el valor de registro $($e.Name) es '$($found[0][4])' en el componente '$($found[0][5])'; se esperaba '$($e.Value)' en '$($e.Component)'") }
        }
        if ($reg.Count -cne $MsiExpectedRegistry.Count) { $problems.Add("el MSI escribe $($reg.Count) valores de registro, se esperaban $($MsiExpectedRegistry.Count)") }

        # Carpetas: el arbol EXACTO (cada Directory con su Directory_Parent) y, donde el .wxs fija el nombre, su DefaultDir
        # (que puede ser "corto|largo": el nombre largo es el ultimo trozo).
        $dirs = @(Get-MsiRows $msi 'SELECT `Directory`, `Directory_Parent`, `DefaultDir` FROM `Directory`' 3)
        $expectedIds = @($MsiExpectedDirectories | ForEach-Object { $_.Id })
        foreach ($d in $dirs) {
            if ($expectedIds -cnotcontains $d[0]) { $problems.Add("carpeta no esperada: $($d[0])") }
        }
        foreach ($e in $MsiExpectedDirectories) {
            $found = @($dirs | Where-Object { $_[0] -ceq $e.Id })
            if ($found.Count -cne 1) { $problems.Add("carpeta $($e.Id): hay $($found.Count) filas en la tabla Directory, se esperaba 1"); continue }
            if ($found[0][1] -cne $e.Parent) { $problems.Add("carpeta $($e.Id): Directory_Parent = '$($found[0][1])', se esperaba '$($e.Parent)'") }
            $leafName = ($found[0][2] -split '\|')[-1]
            if ($null -cne $e.DefaultDir -and $leafName -cne $e.DefaultDir) { $problems.Add("carpeta $($e.Id): DefaultDir = '$($found[0][2])', se esperaba '$($e.DefaultDir)'") }
        }
        $comps = @(Get-MsiRows $msi 'SELECT `Component`, `Directory_`, `ComponentId`, `Attributes`, `Condition`, `KeyPath` FROM `Component`' 6)
        $regIds = @($reg | ForEach-Object { $_[0] })
        foreach ($c in $comps) {
            if (-not $MsiExpectedComponentDirs.ContainsKey($c[0])) { $problems.Add("componente inesperado: $($c[0])"); continue }
            if ($c[1] -cne $MsiExpectedComponentDirs[$c[0]]) { $problems.Add("componente $($c[0]) en $($c[1]), se esperaba $($MsiExpectedComponentDirs[$c[0]])") }
            if ($c[2] -cne $MsiExpectedComponentIds[$c[0]]) { $problems.Add("componente $($c[0]): GUID = '$($c[2])', se esperaba '$($MsiExpectedComponentIds[$c[0]])' (los GUID de componente son fijos)") }
            if ($c[3] -cne $MsiExpectedComponentAttributes) { $problems.Add("componente $($c[0]): Attributes = '$($c[3])', se esperaba '$MsiExpectedComponentAttributes' (KeyPath de registro, 64 bits)") }
            $wantCond = if ($c[0] -ceq 'DesktopShortcut') { $MsiExpectedDesktopCondition } else { '' }
            if ($c[4] -cne $wantCond) { $problems.Add("componente $($c[0]): Condition = '$($c[4])', se esperaba '$wantCond'") }
            if ($regIds -cnotcontains $c[5]) { $problems.Add("componente $($c[0]): su KeyPath '$($c[5])' no es un valor de registro") }
        }
        foreach ($k in $MsiExpectedComponentIds.Keys) {
            if (@($comps | Where-Object { $_[0] -ceq $k }).Count -cne 1) { $problems.Add("falta el componente $k") }
        }

        # Accesos directos: menu Inicio y escritorio, hacia tesela.exe, sin argumentos (un acceso directo con `--scan <ruta>` o
        # `--demo` cambiaria lo que hace Tesela al abrirse) y con la carpeta de trabajo en la de instalacion.
        $sc = @(Get-MsiRows $msi 'SELECT `Shortcut`, `Directory_`, `Name`, `Target`, `Icon_`, `Arguments`, `WkDir`, `Component_` FROM `Shortcut`' 8)
        $wantSc = @(@{ Id = 'StartMenuShortcut'; Dir = 'ProgramMenuFolder' }, @{ Id = 'DesktopShortcut'; Dir = 'DesktopFolder' })
        if ($sc.Count -cne 2) { $problems.Add("se esperaban exactamente 2 accesos directos (menu Inicio y escritorio) hacia tesela.exe; hay $($sc.Count)") }
        foreach ($w in $wantSc) {
            $s = @($sc | Where-Object { $_[0] -ceq $w.Id })
            if ($s.Count -cne 1 -or $s[0][1] -cne $w.Dir -or $s[0][3] -cne '[INSTALLFOLDER]tesela.exe' -or $s[0][7] -cne $w.Id) {
                $problems.Add("se esperaba el acceso directo $($w.Id) en $($w.Dir) hacia [INSTALLFOLDER]tesela.exe")
                continue
            }
            if ($s[0][5] -cne '') { $problems.Add("el acceso directo $($w.Id) lleva argumentos: '$($s[0][5])'") }
            if ($s[0][6] -cne 'INSTALLFOLDER') { $problems.Add("el acceso directo $($w.Id) tiene WkDir = '$($s[0][6])', se esperaba 'INSTALLFOLDER'") }
        }
        $icons = @(Get-MsiRows $msi 'SELECT `Name` FROM `Icon`' 1)
        if ($icons.Count -cne 1) { $problems.Add("se esperaba 1 icono en el MSI; hay $($icons.Count)") }

        # MajorUpgrade: las dos filas EXACTAS de la tabla Upgrade (ver $MsiExpectedUpgradeRows y su explicacion).
        $up = @(Get-MsiRows $msi 'SELECT `UpgradeCode`, `VersionMin`, `VersionMax`, `Language`, `Attributes`, `Remove`, `ActionProperty` FROM `Upgrade`' 7)
        if ($up.Count -cne $MsiExpectedUpgradeRows.Count) { $problems.Add("la tabla Upgrade tiene $($up.Count) filas, se esperaban $($MsiExpectedUpgradeRows.Count) (MajorUpgrade)") }
        foreach ($e in $MsiExpectedUpgradeRows) {
            $rows = @($up | Where-Object { $_[6] -ceq $e.ActionProperty })
            if ($rows.Count -cne 1) { $problems.Add("falta la fila de Upgrade para $($e.Name) (ActionProperty $($e.ActionProperty)): hay $($rows.Count)"); continue }
            $r = $rows[0]
            $min = if ($e.Min -ceq 'V') { $ExpectedVersion } else { $e.Min }
            $max = if ($e.MaxIsVersion) { $ExpectedVersion } else { '' }
            if ($r[0] -cne $MsiExpectedUpgradeCode) { $problems.Add("Upgrade ($($e.Name)): UpgradeCode = '$($r[0])'") }
            if ($r[1] -cne $min) { $problems.Add("Upgrade ($($e.Name)): VersionMin = '$($r[1])', se esperaba '$min'") }
            if ($r[2] -cne $max) { $problems.Add("Upgrade ($($e.Name)): VersionMax = '$($r[2])', se esperaba '$max'") }
            if ($r[3] -cne $MsiExpectedLanguage) { $problems.Add("Upgrade ($($e.Name)): Language = '$($r[3])', se esperaba '$MsiExpectedLanguage'") }
            if ($r[4] -cne $e.Attributes) { $problems.Add("Upgrade ($($e.Name)): Attributes = '$($r[4])', se esperaba '$($e.Attributes)'") }
            if ($r[5] -cne '') { $problems.Add("Upgrade ($($e.Name)): Remove = '$($r[5])', se esperaba vacio") }
        }
        # Condiciones de lanzamiento: CUATRO exactas, cada una con su mensaje (el bloqueo de versiones posteriores y las tres del
        # ambito).
        $lc = @(Get-MsiRows $msi 'SELECT `Condition`, `Description` FROM `LaunchCondition`' 2)
        $lcGot = @($lc | ForEach-Object { $_[0] })
        $lcOk = $lc.Count -ceq $MsiExpectedLaunchConditions.Count
        foreach ($x in $MsiExpectedLaunchConditions) { if ($lcGot -cnotcontains $x) { $lcOk = $false } }
        if (-not $lcOk) {
            $problems.Add("se esperaban exactamente estas $($MsiExpectedLaunchConditions.Count) LaunchCondition: " + (($MsiExpectedLaunchConditions | ForEach-Object { "'$_'" }) -join ', ') + '; hay: ' + (($lcGot | ForEach-Object { "'$_'" }) -join ', '))
        } else {
            foreach ($r in $lc) { if ([string]::IsNullOrWhiteSpace($r[1])) { $problems.Add("la LaunchCondition '$($r[0])' no tiene mensaje") } }
        }
        # Para que lo anterior SURTA EFECTO: FindRelatedProducts (rellena las propiedades de la tabla Upgrade) y LaunchConditions
        # (evalua la condicion) tienen que estar, sin condicion, en las dos secuencias de instalacion, y RemoveExistingProducts
        # (la desinstalacion de la version anterior) sin condicion en la de ejecucion y entre InstallValidate e InstallInitialize
        # (el programa "afterInstallValidate" de MajorUpgrade).
        $seq = @{}
        foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
            $seq[$t] = New-Object System.Collections.Hashtable ([System.StringComparer]::Ordinal)
            foreach ($r in (Get-MsiRows $msi ('SELECT `Action`, `Condition`, `Sequence` FROM `' + $t + '`') 3)) { $seq[$t][$r[0]] = $r }
        }
        foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
            foreach ($a in 'FindRelatedProducts', 'LaunchConditions') {
                if (-not $seq[$t].ContainsKey($a)) { $problems.Add("falta $a en $t (sin el, el bloqueo o la deteccion de versiones no se evalua)") }
                elseif ($seq[$t][$a][1] -cne '') { $problems.Add("$a en ${t} lleva la condicion '$($seq[$t][$a][1])' (tiene que ser incondicional)") }
            }
        }
        # FindRelatedProducts tiene que ejecutarse ANTES de LaunchConditions: es la que pone WIX_DOWNGRADE_DETECTED. Con el orden
        # al reves, la condicion `NOT WIX_DOWNGRADE_DETECTED` se evalua con la propiedad vacia (siempre verdadera) y el
        # bloqueo no hace nada, aunque todas las filas esten.
        foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
            if ($seq[$t].ContainsKey('FindRelatedProducts') -and $seq[$t].ContainsKey('LaunchConditions')) {
                $frp = [int]$seq[$t]['FindRelatedProducts'][2]
                $lcs = [int]$seq[$t]['LaunchConditions'][2]
                if ($frp -ge $lcs) { $problems.Add("FindRelatedProducts (secuencia $frp) tiene que ir antes de LaunchConditions (secuencia $lcs) en ${t}: si no, el bloqueo de versiones posteriores no surte efecto") }
            }
        }
        $ies = $seq['InstallExecuteSequence']
        if (-not $ies.ContainsKey('RemoveExistingProducts')) {
            $problems.Add('falta RemoveExistingProducts en InstallExecuteSequence (MajorUpgrade no desinstalaria la version anterior)')
        } else {
            $rep = $ies['RemoveExistingProducts']
            if ($rep[1] -cne '') { $problems.Add("RemoveExistingProducts lleva la condicion '$($rep[1])' (tiene que ser incondicional)") }
            $lo = if ($ies.ContainsKey('InstallValidate')) { [int]$ies['InstallValidate'][2] } else { $null }
            $hi = if ($ies.ContainsKey('InstallInitialize')) { [int]$ies['InstallInitialize'][2] } else { $null }
            if ($null -ceq $lo -or $null -ceq $hi -or [int]$rep[2] -le $lo -or [int]$rep[2] -ge $hi) {
                $problems.Add("RemoveExistingProducts (secuencia $($rep[2])) tiene que ir entre InstallValidate e InstallInitialize")
            }
        }
        # PROPIEDADES SEGURAS: EXACTAMENTE las esperadas. Las dos de la tabla Upgrade (WIX_*) para que sobrevivan al paso de la
        # interfaz al servidor, ALLUSERS y MSIINSTALLPERUSER (el ambito elegido) e INSTALLFOLDER y DESKTOPSHORTCUT (la carpeta y
        # el acceso directo elegidos en la interfaz: sin Secure, una instalacion PARA TODOS elevada los ignoraria; revierte la
        # decision R-S3 de la v0.1.4, ver installer/Tesela.wxs, que tambien explica el riesgo con AlwaysInstallElevated).
        $secure = @(($props['SecureCustomProperties'] -split ';') | Where-Object { $_ })
        foreach ($a in $MsiExpectedSecure) {
            if ($secure -cnotcontains $a) { $problems.Add("SecureCustomProperties no incluye $a (tiene: '$($props['SecureCustomProperties'])')") }
        }
        foreach ($a in $secure) {
            if ($MsiExpectedSecure -cnotcontains $a) { $problems.Add("SecureCustomProperties incluye $a, que no es una propiedad segura esperada (tiene: '$($props['SecureCustomProperties'])')") }
        }

        # RemoveFile: UNA sola fila (quitar INSTALLFOLDER si queda vacia), sin nombre de archivo: una fila con FileName (p. ej.
        # *.*) borraria archivos del usuario al desinstalar. Ya no se quita ProgramsFolder: la carpeta padre de la de
        # instalacion no es una carpeta conocida (la ruta la fijan las acciones de tipo 51).
        $rm = @(Get-MsiRows $msi 'SELECT `FileKey`, `Component_`, `FileName`, `DirProperty`, `InstallMode` FROM `RemoveFile`' 5)
        foreach ($r in $rm) {
            if ($r[2] -cne '' -or $r[3] -cne 'INSTALLFOLDER' -or $r[4] -cne '2') {
                $problems.Add("fila de RemoveFile no esperada: '$($r[0])' FileName='$($r[2])' DirProperty='$($r[3])' InstallMode='$($r[4])'")
            }
        }
        if ($rm.Count -cne 1) { $problems.Add("RemoveFile tiene que tener exactamente 1 fila (INSTALLFOLDER, solo si queda vacia); hay $($rm.Count)") }

        # Instalaciones previas: CUATRO busquedas de registro (HKCU y HKLM, InstallFolder y DesktopShortcut; sin accion
        # personalizada) hacia TESELA_PREV_*. Las usan el bloqueo del cambio de ambito, la carpeta de las actualizaciones y el
        # acceso directo del escritorio recordado. Se buscan las DOS raices porque AppSearch corre antes de que la interfaz
        # decida el ambito.
        $loc = @(Get-MsiRows $msi 'SELECT `Signature_`, `Root`, `Key`, `Name`, `Type` FROM `RegLocator`' 5)
        if ($loc.Count -cne $MsiExpectedRegLocators.Count) { $problems.Add("RegLocator tiene $($loc.Count) busquedas de registro, se esperaban $($MsiExpectedRegLocators.Count)") }
        $app = @(Get-MsiRows $msi 'SELECT `Property`, `Signature_` FROM `AppSearch`' 2)
        if ($app.Count -cne $MsiExpectedRegLocators.Count) { $problems.Add("AppSearch tiene $($app.Count) filas, se esperaban $($MsiExpectedRegLocators.Count)") }
        foreach ($e in $MsiExpectedRegLocators) {
            $l = @($loc | Where-Object { $_[0] -ceq $e.Id })
            # Type 18 = 2 (valor crudo, raw) + 16 (vista de 64 bits: el paquete es x64); 16 = directorio (con tesela.exe) en la vista de 64 bits.
            if ($l.Count -cne 1 -or $l[0][1] -cne $e.Root -or $l[0][2] -cne $MsiExpectedRegistryKey -or $l[0][3] -cne $e.Name -or $l[0][4] -cne $e.Type) {
                $problems.Add("se esperaba UNA busqueda de registro $($e.Id): raiz $($e.Root), Software" + [char]92 + "Tesela, valor $($e.Name), tipo $($e.Type)")
            }
            $a = @($app | Where-Object { $_[1] -ceq $e.Id })
            if ($a.Count -cne 1 -or $a[0][0] -cne $e.Property) { $problems.Add("AppSearch tiene que asignar $($e.Property) con la busqueda $($e.Id)") }
        }
        # La accion AppSearch (la que ejecuta la busqueda) tiene que estar, sin condicion, ANTES de CostFinalize en las dos
        # secuencias de instalacion: si no, la carpeta recordada no se recupera al actualizar, reparar o desinstalar y los
        # archivos de una carpeta personalizada quedarian huerfanos.
        foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
            if (-not $seq[$t].ContainsKey('AppSearch')) { $problems.Add("falta AppSearch en $t"); continue }
            if ($seq[$t]['AppSearch'][1] -cne '') { $problems.Add("AppSearch en ${t} lleva la condicion '$($seq[$t]['AppSearch'][1])' (tiene que ser incondicional)") }
            if (-not $seq[$t].ContainsKey('CostFinalize')) { $problems.Add("falta CostFinalize en $t"); continue }
            if ([int]$seq[$t]['AppSearch'][2] -ge [int]$seq[$t]['CostFinalize'][2]) {
                $problems.Add("AppSearch (secuencia $($seq[$t]['AppSearch'][2])) tiene que ir antes de CostFinalize (secuencia $($seq[$t]['CostFinalize'][2])) en ${t}")
            }
        }
        # La tabla Signature solo lleva las dos filas de comprobacion de tesela.exe (PrevUserExe y PrevMachineExe); ninguna busqueda
        # de archivo o carpeta mas.
        $sig = @(Get-MsiRows $msi 'SELECT `Signature`, `FileName`, `MinVersion`, `MaxVersion`, `MinSize`, `MaxSize`, `MinDate`, `MaxDate`, `Languages` FROM `Signature`' 9)
        $sigWant = @($MsiExpectedRegLocators | Where-Object { $_.File })
        if ($sig.Count -cne $sigWant.Count) { $problems.Add("la tabla Signature tiene $($sig.Count) filas, tiene que tener exactamente $($sigWant.Count) (la comprobacion de tesela.exe)") }
        foreach ($e in $sigWant) {
            $s = @($sig | Where-Object { $_[0] -ceq $e.Id })
            if ($s.Count -cne 1 -or $s[0][1] -cne $e.File -or (($s[0][2..8] -join '') -cne '')) { $problems.Add("Signature $($e.Id) tiene que buscar solo el archivo $($e.File) (sin version, tamano ni fecha)") }
        }

        # Filas de las secuencias de las acciones de la logica (accion, condicion y numero exactos) y su orden respecto a las
        # acciones estandar: despues de AppSearch (rellena TESELA_PREV_*), antes de LaunchConditions (las condiciones de
        # lanzamiento leen TESELA_PF* e INSTALLFOLDER) y de CostInitialize (INSTALLFOLDER tiene que estar fijada antes de que se
        # calculen las rutas).
        foreach ($t in 'InstallExecuteSequence', 'InstallUISequence') {
            $isUi = $t -ceq 'InstallUISequence'
            foreach ($e in $MsiExpectedLogicRows) {
                if ($e.Where -ceq 'ui' -and -not $isUi) { continue }
                if ($e.Where -ceq 'exec' -and $isUi) { continue }
                if (-not $seq[$t].ContainsKey($e.Action)) { $problems.Add("falta $($e.Action) en $t"); continue }
                $r = $seq[$t][$e.Action]
                if ($r[1] -cne $e.Cond) { $problems.Add("$($e.Action) en ${t} lleva la condicion '$($r[1])', se esperaba '$($e.Cond)'") }
                if ([int]$r[2] -cne [int]$e.Seq) { $problems.Add("$($e.Action) en ${t} tiene la secuencia $($r[2]), se esperaba $($e.Seq)") }
            }
            foreach ($e in $MsiExpectedLogicRows) {
                if ($e.Seq -gt 100) { continue }
                if (-not $seq[$t].ContainsKey($e.Action) -or -not $seq[$t].ContainsKey('AppSearch') -or -not $seq[$t].ContainsKey('LaunchConditions')) { continue }
                $n = [int]$seq[$t][$e.Action][2]
                if ($n -le [int]$seq[$t]['AppSearch'][2] -or $n -ge [int]$seq[$t]['LaunchConditions'][2]) {
                    $problems.Add("$($e.Action) (secuencia $n) tiene que ir despues de AppSearch y antes de LaunchConditions en ${t}")
                }
            }
        }
        foreach ($k in $MsiLangEsSeq.Keys) {
            if (-not $seq['InstallUISequence'].ContainsKey($k)) { $problems.Add("falta $k en InstallUISequence"); continue }
            if ([int]$seq['InstallUISequence'][$k][2] -cne $MsiLangEsSeq[$k]) { $problems.Add("$k tiene la secuencia $($seq['InstallUISequence'][$k][2]), se esperaba $($MsiLangEsSeq[$k])") }
            if ($seq['InstallExecuteSequence'].ContainsKey($k)) { $problems.Add("$k esta en InstallExecuteSequence (el idioma solo se elige en la interfaz)") }
        }
        # Las tres acciones de idioma cubren TODOS los LANGID del espanol y solo ellos (cada condicion: TESELA_LANG="" AND (...)).
        $ids = New-Object System.Collections.Generic.List[int]
        foreach ($k in $MsiLangEsSeq.Keys) {
            if (-not $seq['InstallUISequence'].ContainsKey($k)) { continue }
            $cond = $seq['InstallUISequence'][$k][1]
            if ($cond -cnotmatch '^TESELA_LANG="" AND \(UserLanguageID=\d+( OR UserLanguageID=\d+)*\)$') { $problems.Add("la condicion de $k no tiene la forma esperada: '$cond'"); continue }
            foreach ($m in [regex]::Matches($cond, 'UserLanguageID=(\d+)')) { $ids.Add([int]$m.Groups[1].Value) }
        }
        if ((($ids | Sort-Object) -join ',') -cne (($MsiSpanishLangIds | Sort-Object) -join ',')) { $problems.Add('las acciones TeselaLangEs1..3 no cubren exactamente los LANGID del espanol') }
        # Las ventanas de la secuencia de interfaz: bienvenida (instalacion nueva) o mantenimiento (ya instalada), progreso antes
        # de ExecuteAction y las tres de salida (-1 exito, -2 el usuario cancela, -3 error) en la de interfaz y la administrativa.
        $ui = $seq['InstallUISequence']
        $showRows = @(
            @{ Action = 'WelcomeDlg'; Cond = 'NOT Installed' },
            @{ Action = 'MaintenanceDlg'; Cond = 'Installed AND NOT RESUME AND NOT Preselected AND NOT PATCH' },
            @{ Action = 'ProgressDlg'; Cond = '' }
        )
        foreach ($s in $showRows) {
            if (-not $ui.ContainsKey($s.Action)) { $problems.Add("falta $($s.Action) en InstallUISequence"); continue }
            if ($ui[$s.Action][1] -cne $s.Cond) { $problems.Add("$($s.Action) en InstallUISequence lleva la condicion '$($ui[$s.Action][1])', se esperaba '$($s.Cond)'") }
            if ($ui.ContainsKey('MigrateFeatureStates') -and [int]$ui[$s.Action][2] -le [int]$ui['MigrateFeatureStates'][2]) { $problems.Add("$($s.Action) tiene que ir despues de MigrateFeatureStates (despues de calcular los costes)") }
            if ($ui.ContainsKey('ExecuteAction') -and [int]$ui[$s.Action][2] -ge [int]$ui['ExecuteAction'][2]) { $problems.Add("$($s.Action) tiene que ir antes de ExecuteAction") }
        }
        if ($ui.ContainsKey('ProgressDlg') -and $ui.ContainsKey('WelcomeDlg') -and [int]$ui['ProgressDlg'][2] -le [int]$ui['WelcomeDlg'][2]) { $problems.Add('ProgressDlg tiene que ir despues de WelcomeDlg') }
        if (-not $ui.ContainsKey('ExecuteAction')) { $problems.Add('falta ExecuteAction en InstallUISequence') }
        $exitRows = @(@{ Action = 'ExitDialog'; Seq = -1 }, @{ Action = 'UserExit'; Seq = -2 }, @{ Action = 'FatalError'; Seq = -3 })
        $adm = New-Object System.Collections.Hashtable ([System.StringComparer]::Ordinal)
        foreach ($r in (Get-MsiRows $msi 'SELECT `Action`, `Condition`, `Sequence` FROM `AdminUISequence`' 3)) { $adm[$r[0]] = $r }
        foreach ($x in $exitRows) {
            if (-not $ui.ContainsKey($x.Action) -or [int]$ui[$x.Action][2] -cne $x.Seq) { $problems.Add("falta $($x.Action) con la secuencia $($x.Seq) en InstallUISequence") }
            if (-not $adm.ContainsKey($x.Action) -or [int]$adm[$x.Action][2] -cne $x.Seq) { $problems.Add("falta $($x.Action) con la secuencia $($x.Seq) en AdminUISequence (ICE20)") }
        }

        # La interfaz (ventanas, controles, eventos y textos).
        Test-MsiUserInterface -Msi $msi -Props $props -Seq $seq -Problems $problems
    } finally {
        Close-Msi $msi
    }
    return , $problems
}
