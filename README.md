# Tesela

**Mira qué ocupa tu disco y libera espacio sin miedo.**
Analizador visual de espacio en disco para Windows, escrito en Rust: rápido, ligero y claro.

> 🚧 **Estado: MVP v0.1 en construcción.** Hechos el modelo, el escáner y la **interfaz de solo lectura** (mapa, lista, detalles, escaneo con progreso real, es/en, claro/oscuro). **Todavía no borra nada:** la cesta y la limpieza llegan en el siguiente incremento. Conclusiones de la fase 0 en [docs/fase-0-conclusiones.md](docs/fase-0-conclusiones.md); el plan completo está en [docs/PLAN.md](docs/PLAN.md).

## Qué será

- **Mapa del disco (treemap) + lista.** Cada carpeta es un bloque proporcional a lo que ocupa, coloreado por tipo de archivo, antigüedad o riesgo.
- **Respuesta en segundos.** Mientras analiza ya te dice "puedes liberar al menos X GB", porque empieza por los sitios típicos de basura (temporales, cachés, Papelera).
- **Asistente de limpieza con riesgo visible.** Cada sugerencia va marcada como 🟢 Seguro, 🟠 Revisar o 🔺 Solo informar, y explica en una frase por qué.
- **Nada se borra a ciegas.** Todo pasa por una cesta de revisión. Lo regenerable (temporales, cachés) se borra; tus archivos van a la Papelera. Las carpetas del sistema están protegidas.
- **Honesto con los números.** Solo cuenta como liberado lo que realmente se ha liberado, y enseña qué parte del disco no ha podido medir.
- **Ligero.** Binario único, sin Electron ni WebView, sin telemetría ni cuentas. Funciona sin permisos de administrador y tiene un modo rápido opcional que lee directamente la tabla de archivos (MFT) de NTFS.

## Uso

Windows 10 22H2 y Windows 11. En esta versión la aplicación es de **solo lectura**: no borra, mueve ni escribe nada de tus archivos (solo guarda sus ajustes en `%APPDATA%\Tesela\config.toml`).

```powershell
tesela                      # pantalla de inicio: elige una unidad o "Analizar una carpeta…"
tesela --scan C:\           # analiza esa unidad (o carpeta) nada más arrancar
tesela --demo               # modo demostración: árbol SINTÉTICO y unidades ficticias; no lee tu disco
tesela --help
```

El modo `--demo` es el único con el que se hacen capturas y pruebas: todo lo que enseña (carpetas, tamaños, unidades) está inventado y es siempre el mismo. Admite `--screen home|scan|map|list`, `--open Users/usuario`, `--select Descargas`, `--stopped` (un análisis detenido, con carpetas incompletas), `--theme light|dark`, `--lang es|en` y `--size 1280x800`.

![Mapa de una carpeta de usuario en el modo demostración, tema claro](docs/img/mapa-claro.png)
![Mapa de una unidad en el modo demostración, tema oscuro](docs/img/mapa-oscuro.png)

- **Mapa:** un bloque por carpeta o archivo, proporcional a lo que ocupa **en disco**, coloreado por tipo (paleta apta para daltonismo). Se insinúa el nivel siguiente dentro de cada bloque. Los bloques demasiado pequeños se agrupan en **"Otros (N elementos, X)"**, que se abre con doble clic. Las carpetas que el análisis no llegó a recorrer entera llevan rayas diagonales y un "≥".
- **Grises con significado:** el bloque "Windows y otros datos del sistema" (lo que ocupa el disco pero tu usuario no puede ver), `pagefile.sys` y `hiberfil.sys` ("solo informar") y "Mixto" (carpetas sin un tipo dominante).
- **Lista:** las mismas carpetas con cifras exactas; es la vista accesible (lector de pantalla) y comparte la selección con el mapa.
- **Teclado:** doble clic o `Intro` entra en una carpeta; `Retroceso` sube; las flechas mueven la selección; `Ctrl+F` busca; `F5` reescanea. Clic derecho: abrir en el Explorador, copiar la ruta y Propiedades.
- **Ajustes (⚙):** tema (sistema, claro u oscuro) e idioma (sistema, español o English).

## Hoja de ruta

| Versión | Contenido |
|---|---|
| Fase 0 ✅ | Pruebas de medición: interfaz gráfica, API de lectura de carpetas, Papelera (hecha: [conclusiones](docs/fase-0-conclusiones.md)) |
| v0.1 | Escaneo, mapa + lista, cesta, limpieza de temporales, cachés y Papelera. Entregable: **instalador MSI por usuario** desde la v0.1 |
| v0.2 | Asistente completo (carpetas de proyectos, descargas antiguas, archivos grandes), deshacer |
| v0.3 | Modo rápido leyendo la MFT (administrador) |
| v1.0 | Duplicados, reescaneo incremental, instalador firmado, winget, ARM64 |

## Tecnología

Rust · [egui/eframe](https://github.com/emilk/egui) · [windows-rs](https://github.com/microsoft/windows-rs) · Windows 10 22H2 y Windows 11.

---

**English:** Tesela is a visual disk space analyzer and cleaner for Windows, written in Rust. Phase 0 (measurement spike) is done and the MVP is pending; see [docs/PLAN.md](docs/PLAN.md) and [docs/fase-0-conclusiones.md](docs/fase-0-conclusiones.md) (in Spanish).

## Desarrollo

Workspace de Cargo en la raíz (`crates/`); `spike/` es el código de medición de la fase 0, con su propio workspace.
Estado del MVP v0.1: hechos el modelo (`tesela-model`), el escáner de solo lectura (`tesela-scanner`) con su CLI
`tesela-scan`, el layout del treemap (`tesela-layout`), la interfaz (`tesela-ui`) y el ejecutable (`tesela-app`, binario
`tesela`); la cesta y la limpieza (`tesela-cleaner`) llegan en el siguiente incremento.

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace            # los tests solo usan árboles sintéticos en %TEMP%\tesela-test-<uuid>
cargo deny check
cargo run --release -p tesela-scan -- <ruta> --top 20   # solo lectura; opciones en la cabecera de crates/tesela-scan/src/main.rs
cargo run -p tesela-app -- --demo                        # la interfaz con datos sintéticos
cargo test -p tesela-layout --release -- --ignored relayout   # benchmark del layout (20k rectángulos)
pwsh -File scripts/build-release.ps1                     # release SIN rutas de la máquina en los binarios (y lo comprueba)
```

Capturas del README (solo del modo `--demo`; ver [docs/img/README.md](docs/img/README.md)):
`cargo run -p tesela-app --features capture -- --demo --size 1280x800 --screenshot salida.png` (no pisa un archivo que ya exista: `--overwrite` lo reemplaza). La feature `capture` no forma
parte del ejecutable que se publica. `--bench-hover <segundos>` mueve un puntero sintético sobre el mapa e imprime el coste de
CPU por frame y `--idle-memory <segundos>` deja la ventana en reposo e imprime el private working set del proceso (sin PowerShell, CIM ni WMI); los resultados de cualquier medición se quedan fuera del repositorio.

### Privacidad

El repositorio es público y **no puede contener datos de ninguna máquina**: hardware, sistema, usuario, rutas, tamaños de
discos reales ni resultados de escaneos (los ejemplos, capturas y fixtures son siempre sintéticos).

- **Instala los hooks tras clonar:** `pwsh -File scripts/install-hooks.ps1`. `pre-commit` revisa lo que está en staging
  (contenido del índice y nombres de archivo) y `commit-msg` revisa el mensaje. Exigen **PowerShell 7 (`pwsh`)**; sin él, o
  sin `scripts/privacy-check.ps1` en el worktree, el commit se bloquea con un mensaje claro.
- `scripts/privacy-check.ps1 -All` revisa todo el árbol (el CI lo ejecuta con `-GenericOnly`). Busca SID, rutas de perfil
  reales, unidades D:–Z:, rutas UNC de red (`\\<servidor>\<recurso>`: en la documentación usa un marcador entre `<>`), hardware,
  antivirus y builds de Windows; en local añade el usuario, el equipo y el perfil leídos
  del entorno en tiempo de ejecución. Se decodifica UTF-8 y UTF-16, se revisan cadenas de binarios, se rechazan **por su firma** los
  formatos opacos o comprimidos (ZIP y derivados, gzip, 7z, rar, xz, bz2, zstd, cab, msi, pdf, sqlite: su contenido no se puede revisar) y se rechazan las imágenes, con UNA excepción estrecha: `docs/img/<nombre>.png` si es un PNG sin metadatos (solo fragmentos IHDR, PLTE, tRNS, IDAT, IEND, sRGB, gAMA, cHRM y pHYs), de menos de 2 MiB, y `docs/img/README.md` tiene una línea con su nombre y la orden `--demo` que lo genera.
  `docs/spike-results.md` no puede existir y `spike/` pasa las reglas no ligadas a rutas.
- **Mensajes y rangos:** el mensaje de commit se revisa entero (también las líneas con `#` y lo que sigue a la línea de tijera
  de `git commit --verbose`, aunque git lo recortaría: el hook ve el archivo antes). `privacy-check.ps1 -From <sha> -To <sha>`
  revisa el mensaje de cada commit y TODOS los blobs que introduce el rango, también los que un commit posterior borra; el CI lo
  ejecuta en cada push (`before..sha`; con `before` a ceros o ausente, toda la historia) y pull request (`base..head`).
- **Lista negra local (opcional, no versionada):** crea `<git-common-dir>/privacy-denylist` (en un clon normal,
  `.git/privacy-denylist`; en un worktree, el `.git` del repositorio principal; `git rev-parse --git-common-dir` lo dice) con
  **un término por línea**. Coincidencia literal sin distinguir mayúsculas; se ignoran las líneas vacías y las que empiezan por
  `#`. Sirve para nombres propios (carpetas, programas, equipos) que ningún patrón genérico puede conocer.
- Pruebas del propio check, con `pwsh` y con Windows PowerShell 5.1:
  `pwsh -File scripts/tests/privacy-check.tests.ps1` y `pwsh -File scripts/tests/privacy-check.tests.ps1 -Shell powershell`.
- `scripts/build-release.ps1` compila con `--remap-path-prefix` calculado en tiempo de ejecución (`trim-paths` de cargo sigue
  siendo inestable y el prefijo depende de la máquina, así que no cabe en `.cargo/config.toml`) y falla si el `.exe` contiene el
  usuario, el perfil, el repositorio o el toolchain.

### Notas de diseño del escáner

- Carpetas de la nube (OneDrive y similares): las **ya pobladas** (reparse point con etiqueta de nube) se recorren; las **no
  pobladas** (atributos `RECALL_ON_OPEN` o `RECALL_ON_DATA_ACCESS`) **no se recorren**, para no provocar descargas por red: se
  cuentan con su `AllocationSize` y quedan marcadas `CLOUD | INCOMPLETE` (la CLI las muestra como `NUBE`). El resto de reparse
  points de directorio (junctions, symlinks, montajes) no se siguen. No hay forma de crear placeholders reales en el CI: solo se
  prueban la clasificación y el builder con atributos sintéticos.
- El `mtime` de un directorio en el listado de su padre se refresca de forma diferida en NTFS: justo tras escribir puede ir
  atrasado. Su tamaño propio es 0 en el listado del padre (el espacio real que ocupa el directorio va al bloque de datos del
  sistema, es decir, al "espacio no explicado").

- **Espacio del volumen y cuotas de disco:** se distinguen tres magnitudes de ámbitos distintos (`NtQueryVolumeInformationFile`
  con `FileFsFullSizeInformation`; `GetDiskFreeSpaceExW` mezcla la cuota del usuario con el libre del volumen y solo es el
  respaldo cuando no hay cuota): la **capacidad** y el **libre del volumen**, y lo **disponible para el usuario** (menor si hay una cuota).
  La interfaz enseña el anillo de uso, "X libres de Y" y el "espacio no explicado" SOLO con las dos del volumen
  (`total - libre - escaneado`); si hay cuota añade una línea aparte "Disponible para tu cuenta (cuota)". Si no se puede
  conocer la capacidad real del volumen (cuota y consulta no soportada), no se calcula el espacio no explicado.

### Licencias de terceros

- **Dependencias de Rust:** [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) lista cada componente que se compila en el `.exe` con el texto de su licencia. Se genera con [`cargo about`](https://github.com/EmbarkStudios/cargo-about) y la configuración de `about.toml` y `about.hbs`; no se edita a mano. Para regenerarlo tras cambiar dependencias: `cargo install cargo-about --locked --features cli` (una vez) y `cargo about generate --locked -o THIRD-PARTY-NOTICES.md about.hbs`. El archivo va **incrustado en el ejecutable** y se puede leer desde la propia aplicación: Ajustes, "Acerca de Tesela", "Avisos de terceros".
- **Iconos:** un subconjunto de 30 de [Fluent UI System Icons](https://github.com/microsoft/fluentui-system-icons) (© Microsoft, MIT), tal como salen de su repositorio oficial, con su `LICENSE` en `crates/ui/assets/icons/`.
- **Fuentes:** la interfaz usa Segoe UI (Variable) del propio Windows, que se carga en tiempo de ejecución y **no se redistribuye**. El `.exe` **no lleva ninguna fuente** (egui se compila sin `default_fonts`, así que tampoco hay fuentes OFL ni Ubuntu). Los nombres en chino, japonés, coreano, tailandés, índico, árabe, hebreo o con símbolos cargan, solo si aparecen, la fuente de reserva del sistema que corresponda (Microsoft YaHei o MS Gothic, Malgun Gothic, Tahoma, Nirmala UI, Segoe UI y Segoe UI Symbol). Si Windows no tuviera ninguna fuente utilizable, la aplicación lo dice y se cierra. Los emoji en color no se pueden dibujar (egui no los admite).

## Licencia

Licencia dual a tu elección:

- MIT ([LICENSE-MIT](LICENSE-MIT))
- Apache 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

Salvo que digas lo contrario, cualquier contribución que envíes para incluirla en el proyecto queda bajo esa misma licencia dual, sin términos adicionales.
