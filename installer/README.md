# Instalador MSI de Tesela

`Tesela.wxs` (el paquete) y `TeselaUI.wxs` (la interfaz) forman el instalador MSI de Tesela: **con interfaz**, **solo para tu
usuario (sin administrador) o para todos los usuarios** (PLAN.md §1 y §3.11, ronda 14). Se compilan juntos con
[`scripts/build-msi.ps1`](../scripts/build-msi.ps1), que parte siempre de lo que produce `scripts/build-release.ps1`.

## Herramienta

**WiX Toolset 5.0.2**, la CLI `wix` como `dotnet tool` global. Licencia **MS-RL**; solo es una herramienta de compilación:
nada de WiX se redistribuye dentro del MSI.

```powershell
pwsh -File scripts/install-wix.ps1 -Version 5.0.2    # instalar (y comprobar el SHA-512 del paquete)
dotnet tool uninstall --global wix                   # quitar
```

`install-wix.ps1` ejecuta `dotnet tool install --global wix --version 5.0.2 --configfile installer/nuget.config`: ese
`nuget.config` (`<clear/>` y solo nuget.org) hace que dotnet ignore cualquier otra fuente de NuGet de la máquina, y después se
compara el SHA-512 del `.nupkg` instalado con el que publica nuget.org (fijado en el script).

La versión está fijada en `scripts/build-msi.ps1`, `scripts/install-wix.ps1` (con su SHA-512), `.github/workflows/ci.yml` y
`.github/workflows/release.yml` (`scripts/tests/wix-version.tests.ps1` comprueba que coinciden). **Por qué la 5.0.2 y no la 6.x
o la 7.x:** desde la 6.0 el binario oficial se distribuye con el acuerdo OSMF (*Open Source Maintenance Fee*; solo cobra a
quien obtiene ingresos con WiX, y el código sigue siendo MS-RL) y la herramienta pide **aceptarlo**; la 5.0.2 es MS-RL pura y
no pide aceptar nada. Subir de versión es una decisión que se toma a propósito: cambia todo a la vez y vuelve a probar el ciclo
completo. **No se usa `WixToolset.UI.wixext`** (las ventanas `WixUI_*`): sus acciones personalizadas llevan DLL y chocan con la
regla de no llevar código propio; la interfaz se escribe a mano en `TeselaUI.wxs`.

## Qué instala

- `tesela.exe`, `tesela-scan.exe`, `LICENSE-MIT`, `LICENSE-APACHE` y `THIRD-PARTY-NOTICES.md` en la carpeta elegida. Por defecto:
  **solo para mí** en `%LOCALAPPDATA%\Programs\Tesela` (sin administrador) y **para todos los usuarios** en `C:\Program Files\Tesela`
  (pide administrador).
- Un acceso directo **"Tesela"** en el menú Inicio (el del usuario, o el común del equipo si es para todos). **Acceso directo del
  escritorio** solo si se pide (casilla de la interfaz o `DESKTOPSHORTCUT=1`); se recuerda en las actualizaciones y se quita al
  desinstalar. Sin tocar el `PATH`.
- La entrada de **Aplicaciones instaladas** (icono, editor "Tesela contributors", versión y enlace al repositorio). Windows
  Installer escribe su clave de desinstalación él mismo (en `HKCU` o en `HKLM` según el contexto que use; sin pedir elevación
  para una instalación por usuario); el icono va por `ProductIcon` en `%APPDATA%\Microsoft\Installer\{ProductCode}`.
  `ARPINSTALLLOCATION` queda vacío: rellenarlo exigiría una acción personalizada.
- Valores de `Software\Tesela` en `HKCU` (por usuario) o `HKLM` (para todos): uno por componente (Windows Installer los necesita como
  *KeyPath*), más `InstallFolder` y, si hay acceso directo del escritorio, `DesktopShortcut`. Se quitan al desinstalar.
- **Sin código propio:** las únicas acciones personalizadas son de **tipo 51** (fijar una propiedad: la carpeta, el ámbito
  preelegido, el idioma, los textos en español); no hay DLL, ejecutables ni tabla `Binary`. `msi-validate.ps1` compara la tabla
  `CustomAction` fila a fila.

## La interfaz (`TeselaUI.wxs`)

Bienvenida, **Ámbito**, **Carpeta y opciones**, Listo, Progreso y Fin, más Mantenimiento (abrir el MSI ya instalado: reparar o
desinstalar), Examinar y las obligatorias de Windows Installer (error, cancelación, interrupción, archivos en uso, esperar el
cálculo de espacio). Sin imágenes ni iconos (`Binary` está prohibida). Solo eventos de control **nativos** (`NewDialog`,
`SpawnDialog`, `EndDialog`, `SetTargetPath`, `Reinstall`, `Remove`...); el diálogo de Examinar es el nativo (`PathEdit`,
`DirectoryCombo` y `DirectoryList`) y la carpeta se valida con `SetTargetPath` y condiciones sobre la propiedad.

- **Ámbito.** «Solo para mí (recomendado)» o «Para todos los usuarios de este equipo». El texto dice, sin adornos: *instalar para
  todos NO da a Tesela más permisos para limpiar; Tesela siempre se ejecuta con permisos normales y se niega a limpiar si se
  ejecuta como administrador (decisión de seguridad E1)*. El paquete es de **doble modo** (`Scope="perUserOrMachine"`: `ALLUSERS=2` y `MSIINSTALLPERUSER=1`) y **no** se
  declara «compatible con UAC» (bit 8 del contador de palabras del Summary Information, que el validador exige LIMPIO): con ese bit, Windows Installer
  ignora `MSIINSTALLPERUSER` y «para todos» no pediría credenciales (error 1925). Sin él, abrir el MSI no pide nada, «solo para mí» no eleva nunca y
  «para todos» pide administrador **después** de la interfaz, al empezar la instalación (el botón Instalar lleva el escudo de UAC cuando se
  elige «para todos»). Eso último es lo documentado por Microsoft y **no está comprobado en una máquina real** (el CI no tiene escritorio donde
  mostrar el aviso): lo prueba una persona.
- **Carpeta.** Cambia con el ámbito, salvo que el usuario ya la haya personalizado. En una actualización la carpeta es la de la
  instalación existente y no se puede cambiar (campo y botón desactivados, con una nota). Si en «solo para mí» se elige una carpeta
  protegida (Archivos de programa, Windows), sale un aviso en el diálogo y Siguiente abre una ventana de «carpeta no válida» en vez de
  dejar seguir hasta un 1603 sin explicación (aunque quien instala sea administrador: ahí lo que toca es «para todos»).
- **Escritorio.** Casilla «Crear un acceso directo en el escritorio», desmarcada por defecto (`DESKTOPSHORTCUT`).
- **Anclar a la barra de tareas: no.** Windows prohíbe que un instalador lo haga. La pantalla final lo dice: *Para anclar Tesela a
  la barra de tareas: menú Inicio, clic derecho en Tesela, Anclar a la barra de tareas.*
- **Idioma.** UN solo MSI. Los textos en inglés son propiedades normales; si el idioma de usuario de Windows
  (`UserLanguageID`) es el español en cualquiera de sus variantes, una acción de tipo 51 por texto (solo en la secuencia de
  interfaz) los sustituye por el español. `TESELA_LANG=es` o `TESELA_LANG=en` en la línea de comandos lo fuerza (sirve para
  probar). `UserLanguageID` es el idioma que Windows da al usuario (formato regional): un Windows con la pantalla en inglés y el
  formato regional en español sale en español. **Lo que no se puede elegir por idioma sin código** son las tablas `ActionText` y
  `UIText` (los textos de la barra de progreso): van en español e inglés a la vez («Copiando archivos / Copying files»). **El idioma del
  instalador sigue el formato regional de Windows, no el idioma de la pantalla** (Windows Installer no expone el idioma de pantalla sin código;
  Tesela, la aplicación, sí usa el de pantalla): con la pantalla en español y el formato regional en inglés, el instalador sale en inglés y la
  aplicación en español, y al revés. Se acepta; `TESELA_LANG=es|en` lo fuerza. Los
  mensajes de error genéricos de Windows Installer salen en el idioma de Windows. Los mensajes de las condiciones de lanzamiento
  (modo sin interfaz) también van en los dos idiomas juntos.

### Sin interfaz (`/qn`)

Igual que la v0.1.4: **por usuario y sin UAC por defecto**.

| Propiedad | Efecto |
|---|---|
| (ninguna) | Por usuario, `%LOCALAPPDATA%\Programs\Tesela`, sin acceso directo del escritorio. |
| `ALLUSERS=1` | Para todos (necesita administrador): `C:\Program Files\Tesela` y menú Inicio común. Es la **única** forma documentada de pedirlo. |
| `INSTALLFOLDER=<carpeta>\` | Otra carpeta (ver más abajo). |
| `DESKTOPSHORTCUT=1` | Crea el acceso directo del escritorio (`0` lo quita al actualizar; vacío lo mantiene si ya estaba). |
| `TESELA_LANG=es\|en` | Solo afecta a la interfaz. |

### Ámbito y actualizaciones

Windows Installer **no actualiza entre contextos**: un producto instalado por usuario no se sustituye desde una instalación para
todos, ni al revés. Se **detecta** la instalación previa (una clave con `InstallFolder` **y** `tesela.exe` en esa carpeta: una clave sin producto no cuenta; `HKCU\Software\Tesela\InstallFolder`, que ya escribía la v0.1.4, y
`HKLM\SOFTWARE\Tesela\InstallFolder`) y se **bloquea el cambio de ámbito** con un mensaje que dice que hay que desinstalar antes. **Excepción (varias cuentas):** si ESTE usuario tiene
su propia instalación por usuario, esa gana y puede actualizarla aunque otra cuenta haya instalado para todos (verá dos accesos directos «Tesela»);
la inversa, pedir «para todos» teniendo la suya por usuario, sigue bloqueada.
**Decisión y por qué:** se prefiere bloquear a *fijar* el ámbito en silencio. Sin interfaz (`/qn`) no hay a quien explicárselo y fijarlo
cambiaría lo que pide la línea de comandos; con interfaz, la ventana de ámbito **preelige** el de la instalación existente (actualizar
es pulsar Siguiente) y, si se elige el otro, muestra el aviso y desactiva Siguiente. Sin interfaz, una actualización de una
instalación para todos hay que lanzarla con `ALLUSERS=1`.

- v0.1.4 (por usuario) → v0.1.5 por usuario: actualiza en su sitio, conserva la carpeta personalizada y deja un solo producto.
- v0.1.4 (por usuario) → para todos: bloqueado; desinstala primero.
- Bajar de versión sigue bloqueado.

### Carpeta personalizada

`msiexec /i tesela-<versión>-x64.msi INSTALLFOLDER="%LOCALAPPDATA%\Mis programas\Tesela\"` instala ahí (en `cmd`; `%LOCALAPPDATA%` lo expande el intérprete). **Elige una carpeta que solo tú puedas escribir** (ver la sección siguiente). La carpeta se **recuerda** en
`Software\Tesela\InstallFolder` (HKCU o HKLM según el ámbito; un `RegistryValue` con `[INSTALLFOLDER]`) y se **recupera** con una
búsqueda de registro (`TESELA_PREV_USER` / `TESELA_PREV_MACHINE`) que corre también al actualizar, reparar y desinstalar: así `msiexec /x`
(o Aplicaciones instaladas) quita los archivos de la carpeta personalizada sin necesidad de repetir el parámetro, y una actualización se
queda donde estaba. Consecuencia: **la carpeta de una instalación existente no se cambia pasando otro `INSTALLFOLDER`**; hay que
desinstalar e instalar de nuevo. Sin esto, los archivos de una instalación personalizada quedaban huérfanos al desinstalar.

**No edites `Software\Tesela\InstallFolder`:** lo gestiona el instalador. Si se manipula a mano, el instalador no lo respeta como fuente de verdad ni lo quita al desinstalar de forma fiable (una instalación con el valor cambiado puede dejar archivos o la propia clave sin quitar). Para mover la instalación, desinstala y vuelve a instalar con `INSTALLFOLDER`.

### Propiedades seguras y `AlwaysInstallElevated` (revierte la decisión R-S3 de la v0.1.4)

`INSTALLFOLDER` y `DESKTOPSHORTCUT` son ahora **propiedades seguras** (`ALLUSERS` y `MSIINSTALLPERUSER` no hace falta marcarlas: `ALLUSERS` pasa siempre
al servidor y `MSIINSTALLPERUSER` solo se lee con `ALLUSERS=2`) (`SecureCustomProperties`; `INSTALLFOLDER` y `DESKTOPSHORTCUT` más las dos de MajorUpgrade). La v0.1.4
no tenía interfaz y era solo por usuario, así que no había nada que traspasar al servidor y se dejó sin `Secure`. Ahora la interfaz fija la
carpeta y el acceso directo en la fase de interfaz y, en una instalación **para todos** elevada, Windows Installer solo traspasa al
servidor las propiedades seguras: sin `Secure`, «para todos» ignoraría en silencio lo elegido. **Riesgo con `AlwaysInstallElevated`** (directiva
de HKLM y HKCU, desactivada por defecto): con ella cualquier usuario instala **cualquier** `.msi` con permisos de SYSTEM (la escalada es de la
directiva, no del paquete) y `Secure` le deja además elegir la carpeta donde el servidor escribe. **Se acepta** porque no añade nada: el
contenido lo fija el MSI (5 archivos de Tesela), con esa directiva el usuario ya podría instalar su propio MSI como SYSTEM, `RemoveFolder` solo
quita carpetas vacías y la lista de carpetas protegidas solo sirve para avisar. La alternativa (una acción personalizada que releyera el valor)
está prohibida por la regla de no llevar código. `msi-validate.ps1` exige que estén en `SecureCustomProperties` (y solo esas).

### Seguridad de la carpeta de instalación (`INSTALLFOLDER`)

El MSI **acepta cualquier carpeta** en `INSTALLFOLDER`, también una que otros usuarios del equipo puedan escribir (la raíz de una
unidad, una carpeta compartida, `C:\Users\Public`...). Por usuario, el instalador no se eleva, así que solo puede escribir donde esa cuenta ya
puede escribir (en «para todos» sí se eleva y escribe donde diga `INSTALLFOLDER`, ver la sección de propiedades seguras); el riesgo es de
quien elige la carpeta: si **otro usuario puede escribir en ella**, podría dejar allí una DLL con el
nombre de una que `tesela.exe` carga por nombre, y se cargaría con los permisos de quien ejecute Tesela. Lo que carga el programa
por nombre (antes del endurecimiento de abajo; revisado en el código de `wgpu-hal` 30.0.1 y `libloading` 0.8.9, que usa para ello `LoadLibraryExW(nombre, NULL, 0)`: orden de
búsqueda **predeterminado**, con la carpeta del `.exe` la primera y sin `LOAD_LIBRARY_SEARCH_SYSTEM32`): `d3d12.dll`, `dxgi.dll`,
`dcomp.dll`, `d3dcompiler_47.dll` y **`dxcompiler.dll`**; ninguna está en la lista `KnownDLLs` de Windows. La que más importa es `dxcompiler.dll`: hasta la 0.1.2 `tesela.exe` no fijaba el compilador de sombreadores de DX12 y `wgpu` usa por defecto `Dx12Compiler::Auto`, que **intenta cargar `dxcompiler.dll` por nombre antes de caer a `d3dcompiler_47.dll`**; esa DLL **no existe en una instalación normal de Windows**, así que una copia dejada junto al `.exe` (o en una carpeta del `PATH`) no compite con ninguna de `System32` (DXC carga además `dxil.dll`). La variable de entorno `WGPU_DX12_COMPILER` también cambia esta elección. `dbghelp.dll` no la carga `wgpu`: la carga, si hace falta,
la biblioteca estándar de Rust al generar un *backtrace* (esto último no se ha comprobado en el código fuente de Rust). **Es un riesgo sospechado, no demostrado en ejecución.**

**Endurecimiento en el propio programa** (`tesela.exe` y `tesela-scan.exe`), que reduce el riesgo aunque la carpeta sea escribible por otros sin sustituir la recomendación de abajo:

- `main` llama lo primero a `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` (módulo `dll_hardening`): lo que se carga después por nombre (`d3d12.dll`, `dxgi.dll`, `dcomp.dll`, `d3dcompiler_47.dll`...) se busca solo en `System32`, nunca junto al `.exe`.
- El compilador de sombreadores de DX12 queda fijado a FXC (`d3dcompiler_47.dll`, del sistema): ya no se sondea `dxcompiler.dll`, ni la variable `WGPU_DX12_COMPILER` lo cambia.
- Las importaciones **estáticas** del `.exe` (`dxgi.dll`, `dwmapi.dll`, `uxtheme.dll`, `propsys.dll`, `uiautomationcore.dll`, `bcryptprimitives.dll`) las resuelve el cargador antes de `main`, así que `SetDefaultDllDirectories` no llega a ellas: se enlazan con `/DEPENDENTLOADFLAG:0x800` (`build.rs`), que las busca solo en `System32`. Comprobado con archivos vacíos con esos nombres junto a una copia del `.exe`: sin el endurecimiento el proceso ni arranca (basta un `dxgi.dll` falso); con él arranca, pinta y todos esos módulos salen de `System32`.
- Límites: no protege de lo que ya cargó el cargador antes de `main` por otros medios ni de DLL que el propio Windows cargue con otros indicadores (p. ej. `LOAD_LIBRARY_SEARCH_DEFAULT_DIRS`, que incluye la carpeta de la aplicación y usa `windows-core` para activar clases WinRT que `combase` no resuelve), ni de un `.exe` sustituido. Las DLL de los controladores de la GPU se cargan con ruta absoluta y no se ven afectadas.

**Decisión: no se añade ninguna barrera al MSI; se documenta, y se avisa solo de lo evidente.** Se evaluó una `LaunchCondition` que
rechazase un `INSTALLFOLDER` fuera del perfil, y no se puede hacer de forma robusta (el MSI ya lleva acciones de tipo 51, pero eso no cambia lo
de fondo):

- Una comparación de texto no ve `..` (`<perfil>\..` empieza por el perfil), ni un junction dentro del perfil que apunta fuera, ni
  perfiles redirigidos o con nombre corto 8.3, y rechazaría instalaciones legítimas en una carpeta propia fuera del perfil (otra unidad).
- Sin la barra final, un perfil `C:\Users\<usuario>` también aceptaría `C:\Users\<usuario>2`, el perfil de otra persona.
- Y **no se puede comprobar aquí**: validar la condición exige ejecutar `msiexec /i`, que en local no se hace desde un agente (las pruebas de
  instalación corren en el CI: `scripts/tests/msi-install.tests.ps1`).

Lo que sí hay, **solo para dar un mensaje legible y no como barrera**: en «solo para mí», una carpeta dentro de Archivos de programa o de
Windows (comparación por prefijo y sin barra final, sin distinguir mayúsculas: cubre `Program Files`, `Program Files (x86)` y `Program Files (Arm)` en ARM64,
y trata también como protegida una carpeta cuyo nombre empiece igual, como `Program Files2`) se avisa en el diálogo (y con `/qn`, una condición de lanzamiento con mensaje que sugiere `ALLUSERS=1`), en vez de acabar en un
1603 sin explicación. No depende de `Privileged`: según la documentación de Windows Installer vale lo mismo que `AdminUser` (el usuario es
administrador), no que el proceso esté elevado, y un administrador sin elevar tampoco puede escribir en Archivos de programa.
Además se rechazan los formatos de ruta que esquivarían el aviso: tiene que ser una ruta completa con unidad (`C:\...`) y sin `/`, `..`, `?` ni
ruta de red (en el diálogo, la ventana de «carpeta no válida»; con `/qn`, una condición de lanzamiento). **No se detectan** los nombres cortos
8.3 (`C:\PROGRA~1`) ni un enlace dentro del perfil que apunte fuera: sin código no hay forma; es un aviso, no una barrera.

Qué hacer: instala en la carpeta predeterminada (`%LOCALAPPDATA%\Programs\Tesela`, que es tuya) o en otra **a la que solo tu cuenta (y
los administradores) pueda escribir**. Para comprobarlo: `icacls "<carpeta>"` no debe mostrar `Users`, `Everyone` ni `Authenticated Users` con
permisos de escritura (`W`, `M`, `F`, `(OI)(CI)(W)`...). Lo mismo vale para el **zip portable**: descomprímelo en una carpeta de tu perfil, no en una
carpeta compartida ni en la raíz de una unidad. Si quieres reforzarlo en una instalación de empresa, hazlo con permisos NTFS de la carpeta.

## Identificadores (NO cambiar)

| Qué | Valor | Por qué |
|---|---|---|
| `UpgradeCode` | `{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}` | Identifica a Tesela como producto **para siempre**. Con otro valor, una versión nueva no reconocería a la anterior y quedarían las dos instaladas. `build-msi.ps1` falla si cambia. |
| GUID de cada componente | fijos en `Tesela.wxs` (7: los 6 de la v0.1.4 y el del acceso directo del escritorio) | Windows Installer reconoce los componentes entre versiones por su GUID. |
| `ProductCode` | nuevo en cada compilación (`Id="*"`) | Lo exige `MajorUpgrade`: cada versión es un producto distinto que sustituye al anterior. |

## Actualizar, reparar, desinstalar

- **Actualizar:** `MajorUpgrade` desinstala la versión anterior e instala la nueva (sin duplicados, en la misma carpeta y en el mismo
  ámbito). `AllowSameVersionUpgrades` permite también reinstalar encima una compilación de la *misma* versión. Instalar una versión
  **anterior** a la instalada se bloquea con el mensaje del `DowngradeErrorMessage` (error 1603 en el registro de `msiexec`). Cambiar de
  ámbito se bloquea (ver arriba).
- **Reparar:** `msiexec /fa` (o «Reparar» al abrir el MSI ya instalado) restaura los archivos que falten o estén modificados.
- **Desinstalar:** quita carpeta, accesos directos (menú Inicio y escritorio), entrada de Aplicaciones instaladas y valores de
  `Software\Tesela` (HKCU o HKLM). Deja la carpeta padre (por ejemplo `%LOCALAPPDATA%\Programs`): ya no cuelga de una carpeta conocida del
  perfil, así que `RemoveFolder` solo quita la de Tesela, y solo si queda vacía. **No** borra `%APPDATA%\Tesela\config.toml` ni
  `%LOCALAPPDATA%\Tesela` (historial y registros de limpieza): el instalador no los conoce, son del usuario. Para borrarlos a mano, ver el
  README de la raíz ("Instalación").
- **Aplicación abierta durante una actualización:** Windows Installer usa el Administrador de reinicio (*Restart Manager*)
  para pedirle que se cierre (la interfaz trae las ventanas `FilesInUse` y `MsiRMFilesInUse` para avisar); si no puede (o la aplicación
  se niega: Tesela bloquea el cierre mientras ejecuta una limpieza), los archivos en uso se sustituyen al reiniciar. *No está probado en
  una máquina real*; es el comportamiento esperado de Windows Installer, no uno propio.
- **Dos usuarios en el mismo equipo:** por usuario, cada cuenta instala la suya (carpeta y menú Inicio propios); no se pisan ni piden
  administrador. Para todos, una sola instalación sirve a todas las cuentas. *No está probado* (haría falta una segunda cuenta).

## Compilar y validar

Requisitos: Windows con la toolchain MSVC y el **Windows SDK** (`rc.exe`, que usa `winresource` para el icono y los datos de
versión), Python 3 y WiX 5.0.2 (arriba).

```powershell
pwsh -File scripts/build-msi.ps1
```

Compila los ejecutables (`build-release.ps1`: CRT estático, `--locked`, enlace reproducible, sin rutas de la máquina, imports
PE), empaqueta el MSI (los dos `.wxs`) y el zip portable, escribe `SHA256SUMS.txt` y **valida** el MSI (`scripts/msi-validate.ps1`):
ICE (se aceptan solo los mensajes exactos de ICE61 y los dos de ICE57 de los componentes de los accesos directos: su `KeyPath` es un valor de HKMU,
el diseño estándar de un paquete de doble propósito), lista **cerrada** de tablas (la de la instalación y las de la interfaz; `Environment`, `ServiceInstall`,
`Class`, `Extension`, `ProgId`, `Binary`... son un fallo), la tabla `CustomAction` **fila a fila** (solo tipos 51 y 35), ámbito dual
(`ALLUSERS=2`, `MSIINSTALLPERUSER=1` y el bit «no hace falta elevar» LIMPIO), el **árbol de carpetas exacto** (`TARGETDIR` > `INSTALLFOLDER`, más
`ProgramMenuFolder` y `DesktopFolder` bajo `TARGETDIR`), los 8 valores de registro en HKMU, las seis búsquedas de instalaciones previas (cuatro valores y la comprobación de `tesela.exe`),
las **dos filas exactas de `Upgrade`**, las **cinco `LaunchCondition`** (versiones posteriores, cambio de ámbito en las dos direcciones, carpeta protegida y formato de la ruta), las **propiedades seguras** exactas, las secuencias (`FindRelatedProducts` antes de `LaunchConditions`,
`RemoveExistingProducts` entre `InstallValidate` e `InstallInitialize`, las acciones de carpeta/idioma/escritorio con su condición y su número),
la **interfaz** (ventanas esperadas, solo controles sin imagen, solo eventos nativos, la ventana de ámbito y la de carpeta fila a fila, y cada
texto en inglés **y** en español), archivos byte a byte los compilados, y ni el MSI ni el zip ni sus contenidos llevan datos de la
máquina (los identificadores se comparan distinguiendo mayúsculas). Salida en `<target de cargo>/dist`; nada se escribe en el repositorio.
`scripts/tests/msi-validate.tests.ps1 -Msi <msi>` prueba el validador con copias manipuladas del MSI (sin instalarlas).
`scripts/tests/msi-logic.tests.ps1 -Msi <msi>` prueba la **lógica** (condiciones de lanzamiento, carpeta, ámbito, idioma, escritorio y los
botones Siguiente de las ventanas de ámbito y de carpeta) con una **sesión** de Windows Installer sin instalar nada (`OpenPackage` con
propiedades sintéticas: el evaluador de condiciones es el del propio Windows Installer, y nunca lee el registro ni las carpetas reales).
Se puede correr en local sin tocar la máquina.

**Las pruebas de instalación solo corren en el CI** (`scripts/tests/msi-install.tests.ps1`, job `msi` de `ci.yml`: el runner es un
administrador desechable, y el script se niega a correr fuera de un runner de GitHub). Bajan la v0.1.4 publicada, la verifican con
`gh attestation verify` y la suma SHA-256, y prueban sin interfaz: instalar por usuario, reparar y desinstalar sin restos; actualizar desde la
v0.1.4 (carpeta por defecto y personalizada, un solo producto, la bajada sigue bloqueada); para todos (`Program Files`, `HKLM`, menú Inicio
común, carpeta personalizada, actualización y desinstalación sin restos); el cambio de ámbito bloqueado en las dos direcciones; y
`DESKTOPSHORTCUT` (por usuario y para todos, recordado al actualizar, quitado al desinstalar); la carpeta protegida; una clave de registro sin producto; y
un **usuario estándar** desechable (contraseña aleatoria generada en el job y nunca impresa, borrado al final aunque falle): `ALLUSERS=1 /qn` sin
privilegios tiene que fallar sin dejar nada, instalar por usuario sin elevar tiene que terminar con 0 en su perfil sin tocar `Program Files` ni
`HKLM`, y su instalación por usuario se puede actualizar aunque otra cuenta haya instalado para todos (M2). **El runner es Windows Server**, donde la directiva
efectiva de Windows Installer es `DisableMsi=1`: un usuario estándar no puede instalar ningún MSI (error 1625, sea el paquete que sea, antes de
decidir el ámbito). Para probar lo que importa en un Windows de cliente (cuyo valor por defecto es 0), la máquina desechable recibe la
directiva `DisableMSI=0` mientras dura ese escenario y se le quita al terminar. Con ella, el registro de msiexec dice «dual mode package ...
installed in Per-User mode. No elevation is required»; sin elevar, `ALLUSERS=1 /qn` termina con 1603 sin instalar nada. El resumen del job lista cada
comprobación, lo omitido (que no cuenta como pasado, y hace fallar el job si la versión ya es posterior a la v0.1.4 publicada) y las líneas
`MSI_LUA` de los registros de msiexec (cómo decidió el motor el ámbito y la elevación).
**Lo que no prueba ningún agente ni el CI:** la interfaz (ventanas: aspecto, textos, flujo), la petición de administrador (UAC) al pulsar
Instalar en «para todos» (el runner no tiene escritorio interactivo para mostrarla), el idioma de la interfaz con un Windows en español, las
propiedades seguras con un usuario **sin** privilegios que da credenciales de otro administrador, y la ventana de carpeta con su aviso de carpeta
protegida. Eso lo prueba una persona, con el usuario avisado.

El icono **no se versiona** (`privacy-check` rechaza las imágenes): `crates/app/build.rs` lo dibuja en cada compilación y lo
incrusta en `tesela.exe`; `scripts/extract-ico.py` lo saca del ejecutable para el MSI. (Apuntar la tabla `Icon` al `.exe`
metería una copia entera del ejecutable en el MSI.) La interfaz tampoco lleva imágenes.

El build es **reproducible**: `-Clink-arg=/Brepro` y `--remap-path-prefix` hacen que el mismo commit dé el mismo `.exe` byte a
byte en carpetas distintas. (El MSI y el zip no lo son: llevan marcas de tiempo y el `ProductCode` es nuevo en cada
compilación.)

## Antes de la primera release

**[A CONFIGURAR POR EL MANTENEDOR]** Son ajustes del repositorio que hoy **no están hechos** (no se pueden hacer desde el
código ni los toca el CI). Hasta entonces, cualquiera con permiso de escritura puede empujar a `main` o crear una etiqueta
`v*` que dispara `release.yml`:

1. **Ruleset de etiquetas `v*`:** solo administradores, sin force-push ni borrado.
2. **Protección de `main`:** pull request obligatorio, revisión de los code owners (`.github/CODEOWNERS`) y los checks del CI
   (`privacy`, `privacy-history`, `build`, `deny`, `msrv`, `aarch64`, `msi`) requeridos.
3. **Environment `release`** con revisor requerido.
4. **"Require SHA pinning"** para las acciones (Settings > Actions).
5. **Immutable releases.**
6. **MFA** en todas las cuentas con acceso (GitHub y, cuando exista, SignPath).
7. Activar **"Private vulnerability reporting"** (Settings > Code security) para que funcione el botón "Report a vulnerability"
   de `SECURITY.md` (hoy está desactivado; mientras tanto vale el canal de respaldo que describe).
8. (Opcional) Ensayar el flujo: Actions > Release > Run workflow en `main` (ensayo, sin publicar nada).
9. Poner fecha a la sección de `CHANGELOG.md` y comprobar que la etiqueta es la versión de `Cargo.toml`.
