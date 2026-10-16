# Code signing policy

*(Política de firma de código de Tesela.)*

> **Estado: la v0.1 se publica SIN firmar.** Este documento es la política que pide
> [SignPath Foundation](https://signpath.org/) en sus [términos](https://signpath.org/terms) para su programa gratuito para
> proyectos de código abierto; se enlaza desde el README como **"Code signing policy"** (inicio y sección de descargas) y rige
> desde que el proyecto sea aceptado (la solicitud la presenta el mantenedor, no el CI). Mientras tanto, la integridad de las
> descargas se comprueba con `SHA256SUMS.txt` y con la attestation de procedencia de GitHub (README, "Instalación").
>
> Marcas: **[A COMPLETAR]** y **[A CONFIRMAR]** son decisiones o comprobaciones del mantenedor antes de enviar la solicitud;
> **[A CONFIGURAR POR EL MANTENEDOR]** son ajustes del repositorio que hoy **NO están hechos** y no se pueden hacer desde el
> código (los decide el mantenedor). Este texto es una propuesta razonable, no un hecho verificado.

## Attribution

Cuando haya firma, se mostrará esta frase (README y página de descargas), tal cual la exige SignPath:

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
`.github/workflows/release.yml`; ahí se avisa además de que existe una v3 de la acción de SignPath).

No se firma ninguna dependencia de terceros por separado: se compilan dentro del ejecutable y sus licencias van en
`THIRD-PARTY-NOTICES.md`.

## Procedencia del build

- Los releases salen **solo** de `.github/workflows/release.yml`, que se dispara al empujar una etiqueta `vX.Y.Z` de un commit
  de `main`, en un runner alojado por GitHub (`windows-latest`). No se construye ni se firma nada en equipos personales.
- El build es el de `scripts/build-release.ps1` (sin rutas de la máquina, runtime de C estático, `--locked`, enlace
  reproducible, conjunto exacto de ejecutables, comprobación de imports) y `scripts/build-msi.ps1` (MSI por usuario validado,
  byte a byte contra lo compilado). Las acciones de GitHub están fijadas por SHA y WiX por versión y SHA-512.
- Cada release lleva una **attestation de procedencia** (SLSA, `actions/attest-build-provenance`) que se verifica con
  `gh attestation verify <archivo> --repo wandoth1/tesela --signer-workflow wandoth1/tesela/.github/workflows/release.yml`;
  el propio flujo la verifica antes de crear la release.
- El flujo crea la GitHub Release en **borrador**; nada se publica sin una persona.

## Roles y aprobación

Los tres roles de SignPath (ver sus [términos](https://signpath.org/terms)):

| Rol | Qué significa | Quién |
|---|---|---|
| **Authors** | De confianza para modificar el código sin revisión adicional | `@wandoth1` **[A COMPLETAR: lista de autores con permiso de escritura; hoy solo el propietario]** |
| **Reviewers** | Revisan todo cambio de quien no es author (pull requests) | **[A COMPLETAR: quién revisa; hoy, el propietario]** |
| **Approvers** | Aprueban **cada solicitud de firma**: deciden si una release puede firmarse | `@wandoth1` **[A COMPLETAR: nombre y cuenta de SignPath]** |

- **Cada release se aprueba a mano** en SignPath: la política de firma de producción exige aprobación manual, y la solicitud
  solo la acepta si procede del workflow `release.yml` de este repositorio sobre una etiqueta de `main` (verificación del
  origen del build que hace SignPath con su conector de GitHub). El mismo approver publica después el borrador de la release.
- **[A CONFIRMAR con SignPath]** Si el proyecto sigue con una sola persona, esa persona es a la vez author, reviewer y approver
  (con aprobación manual explícita y MFA); hay que comprobar que SignPath Foundation lo admite para un proyecto unipersonal.
  Si se suma otro miembro, lo razonable es que revise los cambios y apruebe él las firmas.
- **MFA obligatoria** para todos los miembros, **tanto en SignPath como en el repositorio de código** (GitHub), como exigen
  los términos de SignPath. **[A CONFIGURAR POR EL MANTENEDOR: activar y comprobar MFA en la cuenta de GitHub y en la de SignPath
  antes de la solicitud.]**
- Los secretos de SignPath (token y organización) vivirán solo en los secretos del repositorio de GitHub y no están en el código.

### Protección del repositorio: HOY NO ESTÁ CONFIGURADA

Hoy **no hay protección de `main`** ni de las etiquetas: cualquiera con permiso de escritura puede empujar a `main` o crear una
etiqueta `v*`, y eso dispararía el flujo de release. El flujo se ha endurecido por dentro (la etiqueta tiene que ser de `main` y
coincidir con `Cargo.toml`, permisos mínimos por job, borrador, attestation verificada), pero **eso son comprobaciones DENTRO del propio
workflow, no controles de seguridad**: quien empuja la etiqueta (y tiene permiso de escritura) puede editar el workflow en esa misma
etiqueta y saltárselas. Lo que de verdad lo impide es lo que sigue, que es del repositorio, no del código, y **hay que configurarlo antes de la primera release y antes de solicitar la firma**:

**[A CONFIGURAR POR EL MANTENEDOR]**
1. Ruleset de etiquetas `v*`: solo administradores pueden crearlas, sin force-push ni borrado.
2. Protección de `main`: pull request obligatorio, revisión de los code owners (`.github/CODEOWNERS`) y los checks del CI
   (`privacy`, `privacy-history`, `build`, `deny`, `msrv`, `aarch64`, `msi`) como requeridos.
3. Environment `release` con revisor requerido (y usarlo en el job `release`/`attest` cuando exista).
4. "Require SHA pinning" para las acciones (Settings > Actions).
5. Immutable releases (una release publicada no se puede modificar ni sustituir).
6. MFA en todas las cuentas con acceso (arriba).

La misma lista figura en `installer/README.md`, sección "Antes de la primera release".

## Privacy policy

This program will not transfer any information to other networked systems unless specifically requested by the user or the
person installing or operating it.

*(Este programa no transfiere información a ningún otro sistema conectado en red a menos que el usuario o quien lo instala u
opera lo solicite expresamente.)* Tesela no tiene telemetría ni cuentas; escanea solo la unidad o carpeta que eliges y guarda
sus ajustes y el registro de limpiezas **únicamente en tu equipo** (`%APPDATA%\Tesela` y `%LOCALAPPDATA%\Tesela`).
**[A CONFIRMAR: que ninguna dependencia hace peticiones de red en la versión que se firma; si alguna afecta a la privacidad,
SignPath pide enlazar su política.]**

## Reputación y contacto

- Proyecto de código abierto con licencia **MIT o Apache-2.0** (a elección de quien lo use), con historial público en
  [github.com/wandoth1/tesela](https://github.com/wandoth1/tesela) y CI público.
- Vulnerabilidades: ver [SECURITY.md](SECURITY.md).
- **[A COMPLETAR para la solicitud: dirección de contacto del proyecto, nombre legal o seudónimo con el que se solicita la
  firma, y que la página de inicio y la de descargas muestren esta política y la atribución.]**
- Requisitos de SignPath Foundation que **aún no se cumplen** y hay que vigilar antes de solicitar: reputación verificable
  (proyecto con uso y antigüedad suficientes) y que el repositorio no tenga trabajo de ingeniería inversa ni software
  potencialmente no deseado (Tesela borra archivos del usuario solo tras una confirmación explícita; conviene decirlo así en
  la solicitud).
