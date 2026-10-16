# Política de firma de código

> **Estado: la v0.1 se publica SIN firmar.** Este documento es la política que pide
> [SignPath Foundation](https://signpath.org/) para su programa gratuito para proyectos de código abierto; se envía con la
> solicitud (que presenta el mantenedor, no el CI) y rige desde que el proyecto sea aceptado. Mientras tanto, la integridad de
> las descargas se comprueba con `SHA256SUMS.txt` y con la attestation de procedencia de GitHub (ver el README, "Instalación").
>
> Lo que está marcado como **[A COMPLETAR]** o **[A CONFIRMAR]** lo tiene que decidir o comprobar el mantenedor antes de
> enviar la solicitud: este texto es una propuesta razonable, no un hecho verificado.

## Atribución

Cuando haya firma, se mostrará esta frase (README y página de descargas):

> Free code signing provided by [SignPath.io](https://about.signpath.io/), certificate by
> [SignPath Foundation](https://signpath.org/).

## Qué se firma

Solo artefactos **construidos por el flujo de release de este repositorio** a partir del código fuente público, sin firmar
nada compilado fuera de él:

| Artefacto | Firma |
|---|---|
| `tesela.exe`, `tesela-scan.exe` | Authenticode |
| `tesela-<versión>-x64.msi` (instalador por usuario) | Authenticode |
| `tesela-<versión>-x64-portable.zip`, `SHA256SUMS.txt` | No se firman (el zip contiene los mismos `.exe` ya firmados; las sumas y la attestation de GitHub cubren el resto) |

Los ejecutables llevan los atributos de producto que exige SignPath: producto "Tesela", editor "Tesela contributors" y la
versión del `Cargo.toml` (recurso `VERSIONINFO`, el mismo en el MSI). **Orden previsto:** firmar primero los dos `.exe`,
reconstruir el MSI con ellos y firmar después el MSI. Hoy `build-msi.ps1` empaqueta lo que sale de `build-release.ps1`;
reordenarlo para firmar entre los dos pasos es trabajo pendiente del proceso de alta (el paso está preparado, desactivado, en
`.github/workflows/release.yml`).

No se firma ninguna dependencia de terceros por separado: se compilan dentro del ejecutable y sus licencias van en
`THIRD-PARTY-NOTICES.md`.

## Procedencia del build

- Los releases salen **solo** de `.github/workflows/release.yml`, que se dispara al empujar una etiqueta `vX.Y.Z` de un commit
  de `main`, en un runner alojado por GitHub (`windows-latest`). No se construye ni se firma nada en equipos personales.
- El build es el de `scripts/build-release.ps1` (sin rutas de la máquina, runtime de C estático, conjunto exacto de
  ejecutables, comprobación de imports) y `scripts/build-msi.ps1` (MSI por usuario validado, byte a byte contra lo
  compilado). Las acciones de GitHub están fijadas por SHA y las herramientas por versión.
- Cada release lleva una **attestation de procedencia** (SLSA, `actions/attest-build-provenance`) que se verifica con
  `gh attestation verify <archivo> --repo wandoth1/tesela`.
- El flujo crea la GitHub Release en **borrador**; nada se publica sin una persona.

## Roles y aprobación

| Rol | Quién | Qué puede hacer |
|---|---|---|
| **Propietario / Approver** | `@wandoth1` **[A CONFIRMAR: nombre y cuenta de SignPath]** | Es el único que **aprueba manualmente cada solicitud de firma** en SignPath y publica cada release (quita el borrador). Administra el repositorio y la configuración de SignPath. |
| **Mantenedores / Reviewers** | **[A COMPLETAR: otros mantenedores con permiso de escritura, si los hay; hoy ninguno]** | Revisan y fusionan cambios en `main`; no aprueban firmas. |
| **Colaboradores** | Cualquiera | Proponen cambios por pull request; no pueden firmar ni publicar. |

- **Cada release se aprueba a mano** en SignPath: la política de firma de producción exige aprobación manual, y la solicitud
  solo la acepta si procede del workflow `release.yml` de este repositorio sobre una etiqueta de `main` (verificación del
  origen del build que hace SignPath con su conector de GitHub).
- **[A CONFIRMAR con SignPath]** Si el proyecto sigue con una sola persona, esa persona es a la vez propietaria y aprobadora
  (con aprobación manual explícita y MFA); hay que comprobar que SignPath Foundation lo admite para un proyecto unipersonal.
  Si se suma otro mantenedor, lo razonable es que revise los cambios y apruebe él las firmas.
- **Autenticación en dos pasos (MFA) obligatoria** para todas las cuentas con algún rol: GitHub (propietario y mantenedores),
  y SignPath. **[A CONFIRMAR: activar y comprobar MFA en la cuenta de GitHub y en la de SignPath antes de la solicitud.]**
- La rama `main` exige revisión de los propietarios de código (`.github/CODEOWNERS`) para `.github/`, los scripts de build
  y de privacidad, `installer/` y este documento **[A CONFIRMAR: "Require review from Code Owners" activado en la regla de
  protección de `main`]**.
- Los secretos de SignPath (token y organización) viven solo en los secretos del repositorio de GitHub y no están en el código.

## Política de privacidad

Esta aplicación no transfiere información a ningún otro sistema conectado en red a menos que el usuario o quien la instala u
opera lo solicite expresamente. Tesela no tiene telemetría ni cuentas; escanea solo la unidad o carpeta que eliges y guarda
sus ajustes y el registro de limpiezas **únicamente en tu equipo** (`%APPDATA%\Tesela` y `%LOCALAPPDATA%\Tesela`).
**[A CONFIRMAR: que ninguna dependencia hace peticiones de red en la versión que se firma.]**

## Reputación y contacto

- Proyecto de código abierto con licencia **MIT o Apache-2.0** (a elección de quien lo use), con historial público en
  [github.com/wandoth1/tesela](https://github.com/wandoth1/tesela) y CI público.
- Vulnerabilidades: ver [SECURITY.md](SECURITY.md).
- **[A COMPLETAR para la solicitud: dirección de contacto del proyecto, nombre legal o seudónimo con el que se solicita la
  firma, y página del proyecto/descargas que muestre la atribución.]**
- Requisitos de SignPath Foundation que **aún no se cumplen** y hay que vigilar antes de solicitar: reputación verificable
  (proyecto con uso y antigüedad suficientes) y que el repositorio no tenga trabajo de ingeniería inversa ni software
  potencialmente no deseado (Tesela borra archivos del usuario solo tras una confirmación explícita; conviene decirlo así en
  la solicitud).
