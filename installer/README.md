# Instalador MSI de Tesela

`Tesela.wxs` es el instalador **por usuario y sin administrador** (PLAN.md §1 y §3.11). Se compila con
[`scripts/build-msi.ps1`](../scripts/build-msi.ps1), que parte siempre de lo que produce `scripts/build-release.ps1`.

## Herramienta

**WiX Toolset 5.0.2**, la CLI `wix` como `dotnet tool` global. Licencia **MS-RL**; solo es una herramienta de compilación:
nada de WiX se redistribuye dentro del MSI.

```powershell
dotnet tool install --global wix --version 5.0.2 --add-source https://api.nuget.org/v3/index.json   # instalar
dotnet tool uninstall --global wix                                                                # quitar
```

La versión está fijada en `scripts/build-msi.ps1`, en `.github/workflows/ci.yml` y en `.github/workflows/release.yml`
(`scripts/tests/wix-version.tests.ps1` comprueba que las tres coinciden). **Por qué la 5.0.2 y no la 6.x o la 7.x:**
desde la 6.0 el binario oficial se distribuye con el acuerdo OSMF (*Open Source Maintenance Fee*; solo cobra a quien
obtiene ingresos con WiX, y el código sigue siendo MS-RL) y la herramienta pide **aceptarlo**; la 5.0.2 es MS-RL pura y no
pide aceptar nada. Subir de versión es una decisión que se toma a propósito: cambia las tres a la vez y vuelve a probar el
ciclo completo.

## Qué instala

- `tesela.exe`, `tesela-scan.exe`, `LICENSE-MIT`, `LICENSE-APACHE` y `THIRD-PARTY-NOTICES.md` en
  `%LOCALAPPDATA%\Programs\Tesela`.
- Un acceso directo **"Tesela"** en el menú Inicio del usuario. Sin acceso en el escritorio y sin tocar el `PATH`.
- La entrada de **Aplicaciones instaladas** (icono, editor "Tesela contributors", versión y enlace al repositorio).
- Valores de `HKCU\Software\Tesela` (uno por componente: Windows Installer los necesita como *KeyPath* en una instalación por
  usuario) que se quitan al desinstalar.
- **Sin acciones personalizadas**, sin interfaz propia (la barra de progreso estándar) y sin elevación (`Scope="perUser"`).

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

```powershell
pwsh -File scripts/build-msi.ps1
```

Compila los ejecutables (`build-release.ps1`: CRT estático, sin rutas de la máquina, imports PE), empaqueta el MSI y el zip
portable, escribe `SHA256SUMS.txt` y **valida** el MSI (ICE, tablas, que es por usuario, que no lleva acciones
personalizadas, que sus archivos son byte a byte los compilados y que ni el MSI ni el zip ni sus contenidos llevan datos de
la máquina). Salida en `<target de cargo>/dist`; nada se escribe en el repositorio.

El icono **no se versiona** (`privacy-check` rechaza las imágenes): `crates/app/build.rs` lo dibuja en cada compilación y lo
incrusta en `tesela.exe`; `scripts/extract-ico.py` lo saca del ejecutable para el MSI. (Apuntar la tabla `Icon` al `.exe`
metería una copia entera del ejecutable en el MSI.)
