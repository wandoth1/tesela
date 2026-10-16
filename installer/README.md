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

`msiexec /i tesela-<versión>-x64.msi INSTALLFOLDER="C:\Mi carpeta\"` instala ahí. La carpeta se **recuerda** en
`HKCU\Software\Tesela\InstallFolder` (un `RegistryValue` con `[INSTALLFOLDER]`) y se **recupera** con una `RegistrySearch`
hacia `INSTALLFOLDER`, que corre también al actualizar, reparar y desinstalar: así `msiexec /x` (o Aplicaciones instaladas)
quita los archivos de la carpeta personalizada sin necesidad de repetir el parámetro, y una actualización se queda donde
estaba. Consecuencia: **la carpeta de una instalación existente no se cambia pasando otro `INSTALLFOLDER`**; hay que
desinstalar e instalar de nuevo. Sin esto, los archivos de una instalación personalizada quedaban huérfanos al desinstalar.

**No edites `HKCU\Software\Tesela\InstallFolder`:** lo gestiona el instalador. Si se manipula a mano, el instalador no lo respeta como fuente de verdad ni lo quita al desinstalar de forma fiable (una instalación con el valor cambiado puede dejar archivos o la propia clave sin quitar). Para mover la instalación, desinstala y vuelve a instalar con `INSTALLFOLDER`.

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
un fallo), por usuario, archivos byte a byte los compilados, y ni el MSI ni el zip ni sus contenidos llevan datos de la
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
