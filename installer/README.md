# Instalador MSI de Tesela

`Tesela.wxs` es el instalador **por usuario y sin administrador** (PLAN.md §1 y §3.11). Se compila con
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
completo.

## Qué instala

- `tesela.exe`, `tesela-scan.exe`, `LICENSE-MIT`, `LICENSE-APACHE` y `THIRD-PARTY-NOTICES.md` en
  `%LOCALAPPDATA%\Programs\Tesela` (o en la carpeta que se pida con `INSTALLFOLDER=`, ver abajo).
- Un acceso directo **"Tesela"** en el menú Inicio del usuario. Sin acceso en el escritorio y sin tocar el `PATH`.
- La entrada de **Aplicaciones instaladas** (icono, editor "Tesela contributors", versión y enlace al repositorio). Windows
  Installer escribe su clave de desinstalación en `HKLM\...\Uninstall\{ProductCode}` aunque el paquete sea por usuario (lo hace
  el servicio, sin pedir elevación); el icono va por `ProductIcon` en `%APPDATA%\Microsoft\Installer\{ProductCode}`.
  `ARPINSTALLLOCATION` queda vacío: rellenarlo exigiría una acción personalizada.
- Valores de `HKCU\Software\Tesela` (uno por componente, más `InstallFolder`: Windows Installer los necesita como *KeyPath* en
  una instalación por usuario) que se quitan al desinstalar.
- **Sin acciones personalizadas**, sin interfaz propia (la barra de progreso estándar) y sin elevación (`Scope="perUser"`).

### Carpeta personalizada

`msiexec /i tesela-<versión>-x64.msi INSTALLFOLDER="%LOCALAPPDATA%\Mis programas\Tesela\"` instala ahí (en `cmd`; `%LOCALAPPDATA%` lo expande el intérprete). **Elige una carpeta que solo tú puedas escribir** (ver la sección siguiente). La carpeta se **recuerda** en
`HKCU\Software\Tesela\InstallFolder` (un `RegistryValue` con `[INSTALLFOLDER]`) y se **recupera** con una `RegistrySearch`
hacia `INSTALLFOLDER`, que corre también al actualizar, reparar y desinstalar: así `msiexec /x` (o Aplicaciones instaladas)
quita los archivos de la carpeta personalizada sin necesidad de repetir el parámetro, y una actualización se queda donde
estaba. Consecuencia: **la carpeta de una instalación existente no se cambia pasando otro `INSTALLFOLDER`**; hay que
desinstalar e instalar de nuevo. Sin esto, los archivos de una instalación personalizada quedaban huérfanos al desinstalar.

**No edites `HKCU\Software\Tesela\InstallFolder`:** lo gestiona el instalador. Si se manipula a mano, el instalador no lo respeta como fuente de verdad ni lo quita al desinstalar de forma fiable (una instalación con el valor cambiado puede dejar archivos o la propia clave sin quitar). Para mover la instalación, desinstala y vuelve a instalar con `INSTALLFOLDER`.

### Seguridad de la carpeta de instalación (`INSTALLFOLDER`)

El MSI **acepta cualquier carpeta** en `INSTALLFOLDER`, también una que otros usuarios del equipo puedan escribir (la raíz de una
unidad, una carpeta compartida, `C:\Users\Public`...). El instalador es por usuario y no se eleva, así que solo puede escribir donde esa cuenta ya
puede escribir; el riesgo es de quien elige la carpeta: si **otro usuario puede escribir en ella**, podría dejar allí una DLL con el
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

**Decisión: no se añade ninguna regla al MSI; se documenta.** Se evaluó una `LaunchCondition` que rechazase un `INSTALLFOLDER` fuera del
perfil, solo con tablas y acciones estándar (el validador prohíbe la tabla `CustomAction` y debe seguir haciéndolo), y no se puede hacer
de forma robusta:

- La sintaxis de condiciones tiene "empieza por" (`<<`, y `~<<` sin distinguir mayúsculas) y variables de entorno (`%USERPROFILE`), pero el
  valor de una propiedad de la tabla `Property` no se formatea (según la documentación de Windows Installer y la práctica de WiX): no se puede construir `"<perfil>\"` con la barra final sin una acción
  personalizada de tipo 51. Sin la barra, un perfil `C:\Users\<usuario>` también aceptaría `C:\Users\<usuario>2`, el perfil de otra persona.
- `LocalAppDataFolder` y la ruta resuelta de `INSTALLFOLDER` no están garantizadas hasta `CostFinalize`, después de `LaunchConditions` (la documentación de Microsoft no fija el momento exacto); habría
  que recolocar la acción estándar, y eso cambia la secuencia que `msi-validate.ps1` fija.
- Aunque funcionase, una comparación de texto no ve `..` (`<perfil>\..` empieza por el perfil), ni un junction dentro del perfil que apunta fuera, ni
  perfiles redirigidos o con nombre corto 8.3, y rechazaría instalaciones legítimas en una carpeta propia fuera del perfil (otra unidad).
- Y **no se puede comprobar aquí**: validar la condición exige ejecutar `msiexec /i`, que no se hace en este repositorio (el MSI solo se
  valida leyéndolo).

Qué hacer: instala en la carpeta predeterminada (`%LOCALAPPDATA%\Programs\Tesela`, que es tuya) o en otra **a la que solo tu cuenta (y
los administradores) pueda escribir**. Para comprobarlo: `icacls "<carpeta>"` no debe mostrar `Users`, `Everyone` ni `Authenticated Users` con
permisos de escritura (`W`, `M`, `F`, `(OI)(CI)(W)`...). Lo mismo vale para el **zip portable**: descomprímelo en una carpeta de tu perfil, no en una
carpeta compartida ni en la raíz de una unidad. Si quieres reforzarlo en una instalación de empresa, hazlo con permisos NTFS de la carpeta.

## Identificadores (NO cambiar)

| Qué | Valor | Por qué |
|---|---|---|
| `UpgradeCode` | `{7E8F4B67-A7D4-415B-9F8F-2E0B88AA88BB}` | Identifica a Tesela como producto **para siempre**. Con otro valor, una versión nueva no reconocería a la anterior y quedarían las dos instaladas. `build-msi.ps1` falla si cambia. |
| GUID de cada componente | fijos en `Tesela.wxs` | Windows Installer reconoce los componentes entre versiones por su GUID. |
| `ProductCode` | nuevo en cada compilación (`Id="*"`) | Lo exige `MajorUpgrade`: cada versión es un producto distinto que sustituye al anterior. |

## Actualizar, reparar, desinstalar

- **Actualizar:** `MajorUpgrade` desinstala la versión anterior e instala la nueva (sin duplicados, en la misma carpeta).
  `AllowSameVersionUpgrades` permite también reinstalar encima una compilación de la *misma* versión. Instalar una versión
  **anterior** a la instalada se bloquea con el mensaje del `DowngradeErrorMessage` (error 1603 en el registro de `msiexec`).
- **Reparar:** `msiexec /fa` (o "Reparar" desde el MSI) restaura los archivos que falten o estén modificados.
- **Desinstalar:** quita carpeta, acceso directo, entrada de Aplicaciones instaladas y valores de `HKCU\Software\Tesela`.
  **No** borra `%APPDATA%\Tesela\config.toml` ni `%LOCALAPPDATA%\Tesela` (historial y registros de limpieza): el instalador
  no los conoce, son del usuario. Para borrarlos a mano, ver el README de la raíz ("Instalación").
- **Aplicación abierta durante una actualización:** Windows Installer usa el Administrador de reinicio (*Restart Manager*)
  para pedirle que se cierre; si no puede (o la aplicación se niega: Tesela bloquea el cierre mientras ejecuta una
  limpieza), los archivos en uso se sustituyen al reiniciar. *No está probado en una máquina real*; es el comportamiento
  esperado de Windows Installer, no uno propio.
- **Dos usuarios en el mismo equipo:** cada cuenta instala la suya (carpeta y menú Inicio propios); no se pisan ni piden
  administrador. *No está probado* (haría falta una segunda cuenta).

## Compilar y validar

Requisitos: Windows con la toolchain MSVC y el **Windows SDK** (`rc.exe`, que usa `winresource` para el icono y los datos de
versión), Python 3 y WiX 5.0.2 (arriba).

```powershell
pwsh -File scripts/build-msi.ps1
```

Compila los ejecutables (`build-release.ps1`: CRT estático, `--locked`, enlace reproducible, sin rutas de la máquina, imports
PE), empaqueta el MSI y el zip portable, escribe `SHA256SUMS.txt` y **valida** el MSI (`scripts/msi-validate.ps1`): ICE, lista
**cerrada** de tablas (cualquier `CustomAction`, `Environment`, `ServiceInstall`, `Class`, `Extension`, `ProgId`, `Binary`... es
un fallo), por usuario, el **árbol de carpetas exacto** (cada `Directory` con su `Directory_Parent`: `TARGETDIR` > `LocalAppDataFolder` >
`ProgramsFolder` > `INSTALLFOLDER`, y `ProgramMenuFolder` bajo `TARGETDIR`, con los nombres `Programs` y `Tesela`), las **dos filas
exactas de `Upgrade`** (reemplazar versiones anteriores, atributos 513, y detectar posteriores, atributos 2, con sus propiedades
`WIX_UPGRADE_DETECTED` y `WIX_DOWNGRADE_DETECTED`), la `LaunchCondition` `NOT WIX_DOWNGRADE_DETECTED`, que `FindRelatedProducts` se
ejecute antes que `LaunchConditions` y que `RemoveExistingProducts` esté entre `InstallValidate` e `InstallInitialize` (los
identificadores se comparan distinguiendo mayúsculas), archivos byte a byte los compilados, y ni el MSI ni el zip ni sus contenidos llevan datos de la
máquina. Salida en `<target de cargo>/dist`; nada se escribe en el repositorio. `scripts/tests/msi-validate.tests.ps1 -Msi
<msi>` prueba el validador con copias manipuladas del MSI (sin instalarlas).

El icono **no se versiona** (`privacy-check` rechaza las imágenes): `crates/app/build.rs` lo dibuja en cada compilación y lo
incrusta en `tesela.exe`; `scripts/extract-ico.py` lo saca del ejecutable para el MSI. (Apuntar la tabla `Icon` al `.exe`
metería una copia entera del ejecutable en el MSI.)

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
