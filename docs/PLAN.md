# Plan: analizador visual de espacio en disco para Windows (Rust)

> **Estado:** diseño **cerrado y validado** (2 rondas entre diseño UX y arquitectura + validación del asesor; ver §9). **Fase 0 completada** ([conclusiones](fase-0-conclusiones.md)); sus números ya están incorporados aquí (ronda 4 en §9). Lo único abierto son las comprobaciones pendientes del usuario en §6. Siguiente: MVP v0.1.
> **Nombre de trabajo:** **Tesela**, la pieza de un mosaico, como cada bloque del treemap. Comprobado el 2026-10-15: no hay ningún crate `tesela` en crates.io, y en GitHub solo existe `raycoarana/tesela`, una librería Android de 2015 sin relación. Nombre libre en crates.io y winget (comprobado el 2026-10-15); marcas registradas sin revisar.

---

## 1. Decisiones clave

| Tema | Decisión | Por qué |
|---|---|---|
| Lenguaje | Rust estable; la MSRV la fija egui (hoy 1.95), con `rust-toolchain.toml` | Rápido, poca RAM y un único binario |
| UI | **egui/eframe 0.36** con tema propio. Backend **`wgpu` compilado solo con DX12** (WARP de respaldo para RDP y máquinas virtuales sin GPU). Vulkan se excluye. El treemap se dibuja como un **`Mesh` teselado una vez y cacheado**. `glow` es el plan B solo si la prueba de RDP/Hyper-V (§6) falla con wgpu y funciona con glow | Medido en la fase 0: exe ≈ 10,5 MB; ≈ 70 MiB de private working set con el mapa; con `Mesh`, ~1–2 ms por frame. MIT/Apache, compatible con SignPath; AccessKit. Slint queda descartado por licencia (GPL o no-OSI). WinUI vía `windows-reactor` necesita el Windows App Runtime y sigue en preview |
| Plan B de UI | Win32 + Direct2D | Más pequeño, pero supone meses de widgets, teclado y UIA |
| Visualización | **Treemap squarified + lista con barras**, sincronizados. Sin sunburst | El treemap muestra la forma del disco; la lista da cifras exactas y es la vista accesible |
| Tamaño que se muestra | **En disco** (`AllocationSize`) como valor principal; el lógico también aparece en el panel de detalles | Es lo que se libera de verdad. El Explorador muestra el lógico, así que se enseñan los dos |
| Escaneo en el MVP | Walker paralelo propio con `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` y `FindFirstFileExW` de respaldo | Obtiene `AllocationSize` y `FileId` por lotes. `walkdir` es de un solo hilo y `jwalk` está deprecado |
| Escaneo rápido (v0.3) | Lectura de `$MFT` con `ntfs-reader` (`MftScan` en streaming) detrás de `trait MftSource` | Estilo WizTree, pero requiere administrador |
| Elevación | Manifiesto `asInvoker`: **nunca** pide UAC al arrancar. Botón opcional "Modo rápido (administrador)" | Sin fricción. En v1.0, un helper elevado que solo escanea |
| Borrado | **Destino según cada regla, independiente del riesgo** (ver §2.5). Todo pasa por la cesta | Mandar temporales a la Papelera no libera nada |
| SO | Soporte oficial para Windows 10 22H2 y Windows 11; suelo técnico 1809. x64 desde el MVP; ARM64 se compila en CI desde el MVP y se publica en v1.0 | Rust exige Win10+. `aarch64-pc-windows-msvc` es Tier 1 |
| Entregable | **Instalador MSI por usuario desde la v0.1** (WiX): sin administrador, se instala en `%LOCALAPPDATA%\Programs\Tesela`, acceso en el menú Inicio, aparece en Aplicaciones instaladas, actualiza encima y se firma con SignPath. Zip portable como opción secundaria. MSIX descartado | Decisión del usuario (2026-10-15). Coherente con "nunca UAC". MSIX aísla el sistema de archivos, lo que choca con leer todo el disco y la MFT |
| Licencia | MIT / Apache-2.0 dual | Ecosistema Rust y requisito de SignPath |
| Fuera del alcance | Telemetría, cuentas, "optimizador de registro", desinstalador, sunburst, triturado, reglas que borren sin intervención | Ruido sin valor |

---

## 2. Producto y UX

### 2.1 Principios
1. **Tres clics para liberar espacio:** Analizar → "Limpiar lo seguro" → Confirmar.
2. **Primero la respuesta, luego el detalle.** En segundos aparece "Puedes liberar al menos X GB" y luego el mapa.
3. **Nunca se borra sin ver el total y el destino** (definitivo o Papelera).
4. **El riesgo se ve antes de leerse:** color con icono y forma, nunca solo color.
5. **Todo se explica en una frase:** cada sugerencia lleva su "por qué".
6. **Ligera y honesta:** si algo no es claramente seguro, **no se preselecciona**. Solo se cuenta como liberado lo que realmente se ha liberado.

Terminología: la UI dice siempre "archivos" y "Papelera de reciclaje", como Windows en español.

### 2.2 Flujo
1. **Inicio:** tarjetas por unidad con anillo de uso y la línea "Último análisis: hace 3 días · liberaste 8 GB", que solo aparece si hay historial.
2. **Escaneo:** dos zonas (§2.4).
3. **Exploración:** Mapa, Lista y Recomendaciones.
4. **Cesta** agrupada por destino.
5. **Confirmación:** "Liberar 11,0 GB y mover 3,2 GB a la Papelera".
6. **Resumen:** "Liberados ahora: 11,0 GB", seguido de "3,2 GB están en la Papelera · [Vaciar ahora] [Abrir la Papelera]". "Vaciar ahora" se convierte en el botón principal si lo que hay en la Papelera supera lo ya liberado.

Al terminar de limpiar **no se reescanea**: los bloques se encogen con una animación y los totales se recalculan (§3.6).

### 2.3 Ventana principal
```
┌──────────────────────────────────────────────────────────────────────────────┐
│ [≡] Tesela   C: › Users › ana › AppData     [Buscar… Ctrl+F] [Colorear: Tipo ▾] [⚙] │
├────────────┬─────────────────────────────────────────────┬───────────────────┤
│ UNIDADES   │   ┌─────────────┬──────────┬───────────┐   │ DETALLES          │
│ ▣ C: 400/500│   │ AppData     │ Videos   │ Descargas │   │ Videos            │
│ ▢ D:200/1000│   │  32 GB      │ 28 GB    │ 12 GB     │   │ En disco 28,3 GB  │
│ VISTAS     │   │             │          ├───────────┤   │ Tamaño   28,1 GB  │
│ ◉ Mapa     │   │             │          │ Docs 8 GB │   │ 142 archivos      │
│ ○ Lista    │   ├─────────┬───┴──────────┤           │   │ Mod.: 92 % < 2022 │
│ ○ Recomend.│   │ node_mod│ Windows y otros datos  ⓘ │   │ Riesgo: Revisar   │
│   (14 GB)  │   └─────────┴──────────────────────────┘   │ [Explorador][Ruta]│
├────────────┴─────────────────────────────────────────────┴───────────────────┤
│ CESTA: 23 elementos · 14,2 GB     [Ver] [Vaciar]          [Limpiar 14,2 GB ▸]│
├──────────────────────────────────────────────────────────────────────────────┤
│ 400 GB usados de 500 GB · 1,0 M archivos · escaneado hace 2 min  [Reescanear]│
└──────────────────────────────────────────────────────────────────────────────┘
```

### 2.4 Pantalla de escaneo (veredicto temprano)
```
┌ Mientras analizamos tu disco… ───────────────────────────────────────┐
│ Puedes liberar al menos 9,0 GB              [  Limpiar lo seguro  ]  │
│ (se actualiza según avanza)   ◌ Revisando 2 de 5 sitios…             │
│  ✓ Temporales de Windows ........ 2,0 GB                             │
│  ✓ Caché de navegadores ......... 7,0 GB                             │
│  ✓ Papelera de reciclaje ........ 0,5 GB   (Revisar)                 │
│  ◌ Carpetas de proyectos ........ buscando…                          │
│ ──────────────────────────────────────────────────────────────────── │
│ Mapa: Users ████████ · Windows ██ · …               [Parar y ver]    │
└──────────────────────────────────────────────────────────────────────┘
```
- Las rutas conocidas se escanean primero, por una cola prioritaria (§3.3). Cada fila muestra su cifra final al terminar y no vuelve a cambiar.
- "Limpiar lo seguro" se activa cuando acaba el barrido de las rutas conocidas.
- El titular **suma solo las filas Seguro**, que es exactamente lo que hace "Limpiar lo seguro". En el ejemplo la Papelera (Revisar) se muestra, pero no cuenta.
- Mientras quedan filas pendientes el titular dice "**al menos**". Al terminar pasa a "Puedes liberar 14,2 GB" con una animación de contador. Si aparece algo más tarde, se muestra con un chip "+3,1 GB encontrados".
- **Parar y ver:** se construye el árbol con lo recibido hasta ese momento. Las carpetas que no se recorrieron enteras muestran "≥ X GB" con rayas diagonales tenues, la única trama activa por defecto. Si una de ellas va a la cesta, el `cleaner` la vuelve a medir antes de confirmar.

### 2.5 Riesgo y destino: dos ejes distintos
**Riesgo** (Seguro / Revisar / Solo informar):
- 🟢 Escudo con check: se puede preseleccionar y entra en "Limpiar lo seguro".
- 🟠 Ojo: nunca se preselecciona.
- 🔺 Triángulo "Solo informar": sin casilla.

**Destino** (icono propio, sin colores de riesgo):
- 🗑̸ **Definitivo:** solo lo regenerable **clasificado por una regla**. Frase de la cesta: "Se elimina y se vuelve a crear sola".
- 🗑 **Papelera:** todo lo demás. Frase: "Va a la Papelera de reciclaje · puedes recuperarlo".
- Si algo no cabe en la Papelera (supera el límite real del volumen, o es una unidad de red o extraíble), aparece un tercer grupo ámbar: "**Se eliminará definitivamente porque no cabe en la Papelera**", con una casilla explícita. Esto se recalcula **al ejecutar**, no al marcar; si cambia, se pide confirmar otra vez.
- Ajuste avanzado "**Enviar siempre todo a la Papelera**".
- **Orden de ejecución:** si en la cesta están juntos "Vaciar Papelera" y elementos con destino Papelera, la Papelera se vacía **primero** y después se mueven los elementos. Así lo que el usuario quiere conservar recuperable no se borra en el mismo paso.
- Lo que el usuario arrastra a mano o no está clasificado ("Tu archivo") tiene riesgo Revisar y destino Papelera, y **nunca** se borra definitivamente salvo por el caso de la casilla anterior. La confirmación muestra los 5 elementos más grandes y la frase "Son archivos tuyos; no se recrean".

### 2.6 Asistente de limpieza
| Categoría | Riesgo | Destino | Versión |
|---|---|---|---|
| Temporales (`%TEMP%`, `Windows\Temp` si hay acceso), **solo los de más de 24 h** (`min_age` en la regla; "por qué": "Archivos temporales de hace más de un día; los recientes se dejan por si un instalador los está usando") | 🟢 | Definitivo (se saltan los archivos en uso) | MVP |
| Caché de navegadores (Chrome, Edge, Firefox): **solo directorios concretos de caché** (Chromium: `Cache`, `Code Cache`, `GPUCache`, `Service Worker\CacheStorage` por perfil, y `ShaderCache`/`GrShaderCache`; Firefox: `cache2` y `startupCache` bajo `%LOCALAPPDATA%`). **El perfil del navegador nunca es caché**: historial, marcadores, cookies, credenciales y almacenamiento persistente jamás son candidatos | 🟢 | Definitivo | MVP |
| Papelera ("Vaciar") | 🟠 | Definitivo (es la acción en sí). **Alcance enumerado:** se purga solo la lista de elementos capturada al confirmar; lo que entre después no se toca y se informa | MVP |
| `pagefile.sys`, `hiberfil.sys` (con el consejo de `powercfg /h off`) | 🔺 | — | MVP |
| `node_modules`, `target`, `.venv`, `__pycache__`, `.gradle`, detectados por un archivo vecino (`package.json`, `Cargo.toml`, `pyvenv.cfg`…) | 🟢 | **Papelera** por defecto ("Se puede reinstalar con un comando. Va a la Papelera por si tenías cambios locales"). **Destino definitivo DIFERIDO a la v0.2:** la fase 0 midió ~2× el umbral de 10 s para 100k archivos, pero en una sola medición. En la v0.2 se repite ≥ 3 veces con un árbol realista, y se prueba **"renombrar primero"**: renombrar la carpeta a un nombre lápida hermano falla si hay archivos abiertos (cierra en seguro) y después se borra la renombrada | v0.2 |
| Descargas antiguas (> 6 meses), instaladores viejos | 🟠 | Papelera | v0.2 |
| Archivos grandes (> 500 MB) | 🟠 (solo se destacan) | Papelera | v0.2 |
| Duplicados (tamaño → hash parcial → hash completo; siempre queda una copia) | 🟠 | Papelera | v1.0 |
| WinSxS, puntos de restauración | 🔺 (consejo de DISM / configuración; la app no ejecuta nada) | — | v1.0 |

**Navegador abierto:** se detectan `chrome.exe`, `msedge.exe` y `firefox.exe`. Si el navegador está abierto, su fila **no se preselecciona** y no entra en "Limpiar lo seguro". La fila dice "**Cierra Chrome para liberar hasta 1,9 GB**" y ofrece [Volver a comprobar]. El motivo es que Chromium abre su caché con `FILE_SHARE_DELETE`: con el navegador abierto el borrado "funciona", pero no libera nada hasta que Chrome suelta los archivos y además le corrompe el índice. La fase 0 midió que esto pasa también con `std::fs::remove_file` y `DeleteFileW`, no solo con POSIX: el nombre desaparece y el espacio queda retenido. Lo de Chrome es una extrapolación de un handle sintético. Por eso no se limpia una caché en uso. La app nunca cierra el navegador. Antes de limpiar, las cifras se dan como "unos" o "hasta".

**OneDrive:** los archivos que solo están en la nube no se pueden seleccionar para liberar espacio. Si se borra un archivo sincronizado, la confirmación avisa: "Sincronizado con OneDrive: borrarlo también lo quita de la nube".

### 2.7 Protección
- **Intocables como destino** (su contenido sí se puede limpiar): la raíz del perfil, `AppData`, `AppData\Local`, `Roaming`, `LocalLow`, `C:\`, `C:\Users` y los ancestros de rutas denegadas.
  - En el mapa llevan borde punteado y candado.
  - En el menú, "Añadir a la cesta" aparece desactivado con un texto que explica por qué; "Ver contenido" ocupa su lugar.
  - Si se arrastran a la cesta, rebotan.
- **Denegadas por completo:** `C:\Windows` (salvo los temporales conocidos), `Program Files`, `ProgramData` (salvo las cachés conocidas), `System Volume Information`, `$Recycle.Bin` (salvo la acción "Vaciar"), los perfiles de otros usuarios, `C:\Users\Default`, `C:\Users\Public`, los metadatos `$` y la carpeta de la propia app.
- **Raíces de carpetas conocidas** (Documentos, Escritorio, Vídeos…): se añaden como "**Contenido de Vídeos**", sin incluir la carpeta en sí. Riesgo Revisar, destino Papelera.
- **Carpetas que contienen algo protegido:** no se aceptan como destino entero (quedan "intocables": candado y rebote); se puede limpiar su contenido no protegido. Es más seguro que aceptarlas con nota, y es lo que hace el código auditado.
- **Cesta normalizada:** si se añade un nodo cuyo ancestro ya está en la cesta, no se duplica; si se añade un ancestro, absorbe a sus descendientes.
- **La defensa real es el crate `cleaner`, no la UI.** Calcula el conjunto efectivo y, justo antes de borrar cada elemento, vuelve a validar la ruta protegida, comprueba que no es un reparse point y que el tamaño no ha cambiado mucho (TOCTOU). Esto aplica tanto a la Papelera como al borrado definitivo.
- Los archivos en uso o sin permiso se saltan, se cuentan y se explican en el resumen.

### 2.8 Espacio no explicado
- Hay un bloque gris sintético, ordenado por tamaño como un hijo más de la raíz: "**Windows y otros datos del sistema · 40 GB**", con un icono ⓘ. Se calcula como `total − libre − escaneado` con magnitudes **del volumen** (`NtQueryVolumeInformationFile(FileFsFullSizeInformation)`), medido al terminar. La cuota del usuario se muestra aparte ("Disponible para tu cuenta") y nunca se mezcla en el residuo; sin la consulta y con cuota, no hay bloque.
- Se recorta a ≥ 0 y se oculta si es menor del 1 %. Solo aparece en el escaneo de un volumen completo y se marca "aproximado".
- Tooltip: "Incluye archivos de Windows, hibernación, puntos de restauración y carpetas que tu usuario no puede ver. Es normal y no se puede borrar desde aquí". Enlace "Ver el detalle (modo administrador)".
- Si el hueco supera el 5 %, la barra de estado muestra "El mapa explica 360 de 400 GB. ¿Y el resto?". Aparte se indica "N carpetas sin permiso".

### 2.9 Treemap y color
- Muestra un nivel de hijos y una insinuación tenue del siguiente, con esquinas de 4 px y separación de 2 px. La etiqueta solo aparece si cabe.
- **Dos capas distintas:**
  - Agrupación interactiva: los bloques de menos de 24×24 px se juntan en "Otros (1.000 elementos, 400 MB)", que se puede abrir. Es el sufijo del slice ordenado.
  - LOD de dibujo: no se subdivide nada por debajo de unos 3×3 px y hay un tope de ~15k rectángulos por frame.
- Navegación: zoom animado de 180 ms, breadcrumb, tooltip y menú contextual. En el MVP el menú tiene Abrir en el Explorador, Copiar ruta, Añadir a la cesta y Propiedades; "Excluir" llega en la v1.0.
- **Colorear por Tipo** (Okabe-Ito):
  - Vídeo `#0072B2`, imágenes `#E69F00`, audio `#CC79A7`, documentos `#009E73`, comprimidos `#D55E00`, programas `#56B4E9`, otros `#8A8F98`.
  - Una carpeta toma el color de su tipo dominante **solo si supera el 50 % de los bytes**. Si no, es "**Mixto**", un gris azulado más claro, distinto de "Otros".
  - Una franja inferior de 4 px muestra los 3 tipos principales cuando el bloque mide ≥ 48 px. El tooltip desglosa porcentajes.
- **Colorear por Antigüedad:** usa 5 tramos según mtime (< 1 mes, 6 meses, 1 año, 3 años, más). La carpeta toma el tramo donde cae el 80 % de sus bytes. Tooltip: "El 92 % de esto no se modifica desde 2022". Siempre dice "modificado", nunca "usado".
- **Colorear por Riesgo:** solo se colorean los elementos que tienen recomendación.
- **Grises con significado:**
  - "Solo informar": triángulo, no se puede seleccionar.
  - Bloque del sistema: ⓘ.
  - "Mixto": con su franja.
  - El bloque del sistema y los "solo informar" se quedan grises en todos los modos.
- Tras una limpieza, los histogramas se marcan como "≈ / Aproximado hasta reescanear".
- Tramas opcionales de accesibilidad por tipo: **puntos** (las rayas quedan reservadas para "incompleto").

### 2.10 Identidad, accesibilidad e idiomas
- **Paleta** (claro / oscuro):
  - Fondo `#F5F6F8` / `#1B1D21`
  - Superficie `#FFFFFF` / `#25282E`
  - Acento `#0F6CBD` / `#4CA3F0`
  - Seguro `#107C41` / `#3FB871`
  - Revisar `#B26A00` / `#F2B24D`
  - Solo informar / peligro `#C42B1C` / `#FF6B5E`
- **Fuentes:** se cargan del sistema en tiempo de ejecución, sin redistribuirlas: `SegUIVar.ttf` si existe (medido: egui aplica `wght` de 300 a 700; `opsz` no se aplica solo), si no `segoeui.ttf` + `segoeuib.ttf`. La ruta se obtiene con `SHGetKnownFolderPath(FOLDERID_Fonts)`. **No se embeben fuentes de egui** (`default_fonts` desactivado); las escrituras no latinas usan reservas del sistema. Si no hubiera ninguna fuente del sistema utilizable, la app avisa y sale.
- **Iconos:** un subconjunto de unos 30 de **Fluent UI System Icons** (MIT, © Microsoft, confirmado), embebidos. Se ven igual en Win10 y Win11. No se depende de `SegoeIcons.ttf`.
- **Fondo sólido.** Mica no funciona con wgpu (medido en 4 configuraciones). Solo volvería si se cambiara al plan B `glow`.
- **Idiomas:** español e inglés desde el día 1 (`fluent-bundle`), sin cadenas literales en el código.
- **Accesibilidad:**
  - DPI PerMonitorV2.
  - Foco visible de 2 px y treemap navegable con flechas.
  - AccessKit en la lista.
  - Se respeta "reducir animaciones" (`SPI_GETCLIENTAREAANIMATION`), también en el progreso indeterminado y los indicadores de espera.
  - Contraste AA.

### 2.11 Persistencia
- `%APPDATA%\Tesela\config.toml`: ajustes.
- `%LOCALAPPDATA%\Tesela\history.jsonl`: **solo resúmenes** (fecha, unidad, usado, liberado ahora, enviado a la Papelera). Se puede borrar desde Ajustes. Nunca se guarda el árbol.
- Cesta al relanzar como administrador: se guarda en un archivo de `%LOCALAPPDATA%` con una ACL restringida. El proceso nuevo la lee, vuelve a validar las rutas, recalcula los tamaños y la borra. Si la elevación es con **otra cuenta**, se avisa y no se recupera.

---

## 3. Arquitectura

### 3.1 Workspace
```
tesela/
├─ Cargo.toml              # [workspace], perfiles, [workspace.dependencies]
├─ rust-toolchain.toml, deny.toml, LICENSE-MIT, LICENSE-APACHE
├─ crates/
│  ├─ model/    # registros planos, TreeBuilder, Tree (CSR), overlay, histogramas (sin deps de Windows)
│  ├─ scanner/  # walker paralelo, cola prioritaria, MFT (trait MftSource)
│  ├─ layout/   # squarified, agrupación "Otros", LOD, hit-test (puro)
│  ├─ cleaner/  # reglas TOML (riesgo + destino), protección, borrado definitivo, Papelera, duplicados
│  ├─ ui/       # egui: vistas, tema, asistente, i18n
│  └─ app/      # main, manifiesto, elevación, persistencia
├─ tests/fixtures/  ├─ benches/  └─ .github/workflows/
```

### 3.2 Flujo de datos: un `TreeBuilder` para los dos escáneres
```
walker ─┐                         ┌─ agrega bottom-up (alloc, logical, files, histogramas)
        ├─► registros planos ─►  TreeBuilder ─┤─ dedup hard links por FileId
MFT ────┘   (Batch por dir)       └─ ordena hijos por tamaño ─► asigna ids finales en BFS ─► Arc<Tree> (CSR)
```
- **Registro plano:** `parent: u32`, `name_off: u32`, `name_len: u16`, `flags: u16`, `alloc: u64`, `logical: u64`, `mtime: u32` (32 B, campos reordenados para alinear). El `FileId` (u128, por ReFS) va aparte, en el `Batch`, y solo vive durante la construcción.
- **Orden:** se ordena **una sola vez** en el builder, después de agregar. Como los ids finales se asignan después de ordenar, no hace falta remapear `parent`.
- **Hard links:** se deduplican **siempre** por FileId en todo el volumen NTFS/ReFS, en el builder, de forma determinista. Se ordena un `Vec<(file_id, idx)>`. Dentro de cada grupo de colisión conserva el tamaño **el enlace con la ruta completa menor en orden lexicográfico**; reconstruir rutas sale barato porque solo se hace con las colisiones. No vale el índice plano, que depende de cómo se repartan los hilos y haría bailar los totales entre ejecuciones. Los demás enlaces se quedan con `alloc = logical = 0` y el flag `HARDLINK`. El dedup se hace **después del BFS**, solo entre entradas alcanzadas (un huérfano nunca le gana a un enlace real). Coste: +8 B por entrada y un sort despreciable frente al escaneo (medido). En la fase 0, sin dedup se contaba de más un porcentaje pequeño del volumen y una fracción considerable de `C:\Windows`. En FAT/exFAT no aplica. Dentro de una subcarpeta solo se deduplica lo que se ha escaneado.

### 3.3 Walker
- Pools dedicados para escaneo, borrado y hashing, para que no compitan entre sí.
- **Work-stealing** (`crossbeam-deque`), con una tarea por directorio.
  - El worker enumera el directorio y obtiene `base_id` con un `AtomicU32::fetch_add(n)`, así los subdirectorios ya tienen id antes de encolarse.
  - Envía el `Batch { base_id, entries, names }` a un hilo colector que solo hace `push`.
  - Sin `unsafe` ni arena compartida. Es un mensaje por directorio (~0,3 M por cada millón de archivos).
- **Cola prioritaria:** un `Injector` que los workers consultan antes de robar trabajo. Contiene `%TEMP%`, `Windows\Temp`, **directorios concretos de caché de los navegadores (nunca el perfil)**, `$Recycle.Bin\<SID>` y Descargas. La clasificación exacta y conservadora vive en `classify_browser_path` (`scanner/src/known.rs`) y la reutilizará el `cleaner`; los nombres de caché deben estar verificados contra el código fuente oficial de Chromium y Firefox antes de borrar nada.
  - Sus ids se reservan de antemano, **junto con la cadena de ancestros** (p. ej. `C:\Users\<usuario>\AppData\Local`), creada como nodos `INCOMPLETE`. Así "Parar y ver" nunca enseña huérfanos.
  - Cuando el recorrido general llega a uno de esos directorios, **adopta** el id existente en vez de volver a escanearlo. La comparación se hace **por `FileId`** del directorio y no por la ruta en minúsculas, porque NTFS admite carpetas que distinguen mayúsculas.
- **Hilos:** por defecto `min(16, 2 × núcleos)`; los ociosos esperan con backoff creciente. Se ajustan midiendo el rendimiento real; `IncursSeekPenalty` es solo una pista. Observación de la fase 0 (un solo equipo de pruebas, una ejecución por valor): la mejora se satura pronto y es de varias veces frente a 1 hilo. HDD, USB y red siguen sin medir.
- **Reparse points:** no se siguen (tamaño 0 y un flag).
- **Carpetas de la nube (OneDrive y similares):** si están **pobladas** se recorren (se abren dejando actuar al filtro de la nube; medido en el inc. 1: sin tráfico de red ni hidratación). Si **no están pobladas** (`RECALL_ON_OPEN` / `RECALL_ON_DATA_ACCESS`) no se recorren: cuentan su `AllocationSize` y quedan `CLOUD|INCOMPLETE`, para no provocar descargas.
- **Modo de respaldo (`FindFirstFileExW`):** sin `FileId`, así que no hay dedup ni cola prioritaria; `alloc` de archivos estimado (EOF redondeado al clúster) y de directorios 0. Los hard links se cuentan N veces y el hueco puede recortarse a 0: la UI marca ese escaneo como **"aproximado"**.
- **Datos de directorio del listado del padre:** el `mtime` puede ir atrasado ~1 s justo tras escribir (no sirve para un reescaneo incremental) y el tamaño propio del directorio sale 0 (va al bloque de datos del sistema).
- **Mayúsculas:** la cola prioritaria y la búsqueda de entradas comparan nombres exactos; la comparación insensible solo es respaldo si es única.
- **Rutas largas:** `\\?\` + `longPathAware` desde el primer commit. Los nombres se guardan en WTF-8.
- Cancelación con un `AtomicBool` que nace **antes** del hilo de arranque y se consulta entre fases. Los errores de permiso se cuentan sin abortar. Los errores son tipados y la UI los traduce con Fluent.
- La raíz pasa por la misma clasificación de nube y reparse que los descendientes; su `FileIdInfo` es opcional (si el volumen no lo soporta se sigue con el respaldo).
- **Progreso en vivo:** contadores atómicos más acumuladores atómicos por carpeta de primer nivel.
- **"Parar y ver":** el builder trabaja con los batches recibidos y marca `INCOMPLETE` los directorios que aún tenían tareas pendientes, e `INCOMPLETE_BELOW` en todos sus ancestros (para el "≥" de la UI). Si hay directorios incompletos el escaneo cuenta como detenido y **no se calcula el bloque de espacio no explicado**.
- Un pánico en un hilo activa la cancelación (no se cuelga el coordinador); agotar los ids cancela y se informa.

### 3.4 MFT (v0.3)
- `ntfs-reader::MftScan` en streaming → registros planos → el mismo `TreeBuilder`.
- Un registro MFT es un nodo. Se ignoran los nombres DOS-only.
- Se suma `allocated_size` de todos los streams, ADS incluidos. Los archivos residentes cuentan 0 clusters.
- Se excluye `$BadClus:$Bad`. El mtime sale de `$STANDARD_INFORMATION`.
- En ReFS o con BitLocker bloqueado se vuelve al walker.
- Opción de escanear sobre una instantánea VSS.

### 3.5 Modelo final
```rust
#[repr(C)]
struct Node {            // 40 B; hijos contiguos y ordenados por tamaño (lo garantiza el TreeBuilder)
    alloc: u64, logical: u64,
    parent: u32, first_child: u32, child_count: u32,
    name_off: u32, name_len: u16, flags: u16,   // DIR, REPARSE, CLOUD, HARDLINK, INCOMPLETE, INCOMPLETE_BELOW, SYSTEM_INFO, ERROR…
    files_below: u32,
}
// Columnas laterales (todos los nodos, indexadas por id; los ids BFS mezclan carpetas y archivos):
//   mtime: Vec<u32>
//   hist:  Vec<Hist { type_pct: [u8; 7], age_pct: [u8; 5], dominant_type: u8, dominant_age: u8 }>  (14 B)
```
- Los umbrales se calculan **sobre los acumuladores `u64`** (con suma saturada) en el builder y se guardan en `dominant_type` / `dominant_age`. Tipo dominante: estrictamente > 50 % de los bytes. Antigüedad: el tramo más antiguo tal que ≥ 80 % de los bytes están en él o en uno más antiguo; un resultado "reciente" puede significar también "mixto". Un `mtime` desconocido (0) no cuenta para la antigüedad. Pesos = `alloc` tras el dedup. Los `u8` solo se usan para dibujar y para el tooltip.
- La tabla extensión → tipo es un TOML embebido.
- **RAM**, estimación que se valida en la fase 0:
  - En régimen: unos 78 B por entrada (40 del nodo, 4 de mtime, 14 de histogramas y el nombre), medido en el inc. 1; más con nombres largos.
  - Pico durante la construcción: **≈ 2× el régimen** (registros planos + temporales + árbol final coexisten); depende de la longitud de los nombres.
  - La UI añade unos 65–70 MiB de private working set (medido con wgpu DX12 y el mapa dibujado).
  - **Métrica de memoria:** siempre **private working set** (la columna "Memoria" del Administrador de tareas). El working set total incluye DLL compartidas del driver y da 82–174 MiB con cualquier backend.

### 3.6 Escaneo ↔ UI y mutación tras limpiar
- La UI lee los contadores atómicos entre 15 y 30 Hz.
- Al terminar se publica un `Arc<Tree>` inmutable con `arc-swap`. La UI no toma locks.
- `crossbeam-channel` lleva los eventos.
- **Overlay de limpieza** (no se usa `Arc::make_mut`, que copiaría más de 64 MB):
  - Un bitset de nodos eliminados (125 KB por millón).
  - Un `FxHashMap<u32, Delta>` para los ancestros: al borrar se resta el delta a lo largo de los ~30 ancestros como máximo.
  - Tamaño visible = base + delta. El layout reordena los hijos visibles.
  - Se compacta al reescanear.
- **Deshacer (v0.2):** `trash::os_limited::list/restore_all` (restauran bien, medido en la fase 0). Se casa por `original_path` y hora. Revierte el delta. Solo se ofrece para lo que fue a la Papelera. `list()` es lineal en el tamaño de **TODA la Papelera** (decenas de segundos con 100k elementos), y `delete_all` puede devolver error habiendo movido casi todo. Por eso el resultado se **verifica archivo a archivo**, y Deshacer se ofrece para lotes pequeños o carpetas enteras.

### 3.7 Borrado seguro (incremento 3)
Principio: **verificar y actuar sobre el mismo handle**, nunca "comprobar por ruta y borrar por ruta" (cierra la carrera TOCTOU señalada por la auditoría externa).

1. **La aprobación es una identidad.** Al confirmar la cesta, el `cleaner` reabre cada elemento con `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS` y guarda `(número de serie del volumen, FileId de 128 bits)` (`FileIdInfo`) y `NumberOfLinks` (`FileStandardInfo`). Las rutas solo sirven para volver a encontrar los objetos.
2. **Borrado definitivo por handle.** Se abre con acceso `DELETE` y **`dwShareMode = 0`**. La apertura exclusiva es a la vez la comprobación de "en uso" (`SHARING_VIOLATION` → se salta y se cuenta), cierra el caso `FILE_SHARE_DELETE` (nadie más lo tiene abierto) e impide la sustitución entre comprobar y actuar. Si el `FileIdInfo` del handle no coincide con lo aprobado → se rechaza ("cambió desde la confirmación"). Después, `SetFileInformationByHandle(FileDispositionInfoEx, DELETE | POSIX_SEMANTICS | IGNORE_READONLY)` sobre **ese mismo handle**.
3. **Ancestros y recursión.** Windows no tiene `openat`, pero `NtCreateFile` con `OBJECT_ATTRIBUTES.RootDirectory = <handle del padre>` es la apertura relativa. Se abre por handle la raíz de la categoría (`%TEMP%`, el directorio de caché…), se verifica su FileId contra el capturado, y los hijos se enumeran y abren **relativos a ese handle**, cada uno con `FILE_OPEN_REPARSE_POINT`, rechazando cualquier reparse point. Es el patrón con el que se reescribió `std::fs::remove_dir_all` tras CVE-2022-21658. **Frontera de confianza:** los ancestros por encima de la raíz de categoría son del perfil del propio usuario; cambiarlos exige sus propios privilegios, y la app nunca se ejecuta elevada en la v0.1.
4. **Papelera (sin lápida).** Comprobación previa con apertura exclusiva (identidad y "en uso"); después se abre un **handle de guarda** `DELETE | READ_ATTRIBUTES` compartido que se mantiene durante toda la operación de la shell y hasta juzgar el resultado (bloquea renombrar el elemento y sus ancestros, error 5, sin impedir que la shell recicle). Se recicla **por la ruta original** con `IFileOperation` (`FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE`) y un `IFileOperationProgressSink` propio: en `PostDeleteItem`, `psiNewlyCreated` es el elemento ya en la Papelera (nulo si la shell lo eliminó definitivamente). `Recycled` exige: misma identidad (FileId del `$R`), el nombre aprobado ya no existe, la ruta final de la guarda está en `$Recycle.Bin` y los enlaces no cambiaron; si no, `RecycledOther` (recuperable, se informa y no se aplica al árbol). Nulo con éxito → "eliminado definitivamente (no cabía)". **Reparto por operaciones** (`take_op`): cada operación ≤ 90 % de `MaxCapacity` menos la ocupación actual (`SHQueryRecycleBin`); lo grande (> 10 %) o de capacidad desconocida va solo; lo que no cabe queda `NoRoom` sin llegar a la shell; **corte de seguridad** tras el primer borrado inesperado (el resto queda donde estaba). Sin `MaxCapacity` se asume el 5 % del volumen; la directiva `NoRecycleFiles` y cualquier duda = "no cabe" (falla cerrado). La lápida renombrada del diseño anterior se retiró porque el usuario veía y restauraba el nombre lápida y podían quedar huérfanas. Medido: `FOF_WANTNUKEWARNING` con `FOF_NO_UI` se cuelga (su prueba corre en un proceso hijo con timeout).
5. **Hard links.** No hace falta el FileId en el modelo: si `NumberOfLinks > 1` al aprobar, el elemento se marca en la confirmación ("compartido con otro archivo: no liberará espacio") y se excluye de la estimación de bytes liberados.
6. **Vaciar la Papelera:** alcance enumerado; se purga por los elementos de la carpeta shell de la Papelera (`DeleteItem` con el mismo sink). En esta build de Windows la shell deja el `$I` huérfano, así que `remove_orphan_meta` borra el `$I` hermano exacto de un elemento recién purgado (solo bajo la carpeta del SID propio, con el `$R` ya inexistente, contenido `$I` válido que casa con el elemento, apertura exclusiva y lectura/borrado por el mismo handle). El verbo `delete` del menú contextual de la Papelera se cuelga y no se usa.
7. **Tres contadores.** "Eliminado definitivamente" y "movido a la Papelera" son sumas de tamaños; **"Liberado ahora" es la diferencia medida de `GetDiskFreeSpaceEx` antes y después** (incluye de forma automática hard links, handles abiertos y borrados definitivos inesperados). Si divergen, se muestran ambos con el motivo en una línea.
8. **Navegador.** Detectar el proceso es UX ("cierra Chrome"); la garantía es el punto 2: si el navegador arranca a mitad, los archivos restantes fallan la apertura exclusiva y se saltan.
9. **Paralelismo y progreso.** Archivos en un pool dedicado y directorios después, de abajo arriba; progreso con atómicos y cancelación entre entradas. Se saltan y cuentan `SHARING_VIOLATION` y `ACCESS_DENIED`.
10. **Modo simulación (`--dry-run`)** que registra las identidades aprobadas sin tocar nada, y log local de lo borrado.
11. **Tests obligatorios del `cleaner`** sobre árboles en `%TEMP%`, que **compitan con la operación**: sustituir un archivo por otro del mismo tamaño, cambiar un ancestro por un junction, abrir un archivo en exclusiva desde otro hilo… y comprobar rechazo o salto. Toda comparación de rutas, sobre formas canónicas largas (en CI `%TEMP%` es 8.3).
12. **Solo unidades locales:** la ruta final resuelta debe ser de una unidad local con letra (fija o extraíble); las rutas UNC (incluido el recurso administrativo de la propia máquina) y las unidades de red se rechazan (`NotLocal`) ya en la cesta. `TEMP` solo se acepta si es `%LOCALAPPDATA%\Temp` o cuelga de él, y una exención nunca levanta una denegación por debajo de ella.
13. **Cierre de la ventana bloqueado** mientras se ejecuta una limpieza (desde cualquier pantalla); registro local con `start` y `recycle-begin` volcados antes de cada operación para poder reconstruir qué se movió tras un cierre forzoso.
14. **Reparse points:** medido con junctions: el borrado relativo nunca entra en ellos. Los symlinks de directorio quedan por medir (§6).

### 3.8 Treemap
- Algoritmo squarified de Bruls et al., lineal porque los hijos ya vienen ordenados.
- Layout de profundidad 2 (un nivel con etiquetas y uno insinuado).
- Caché por `(raíz, viewport, profundidad, versión del overlay)`. El hover se pinta como overlay y la detección usa una rejilla espacial.
- **Presupuesto de celdas:** `max_rects` es único para los dos niveles y se aplica antes del primer squarify (el exceso va a un "Otros" sufijo).
- **Dibujo:** el layout se tesela una vez en un `Mesh` que se reutiliza mientras no cambie la clave de caché. Objetivo: **p99 de CPU por frame < 8 ms**. Medido: con `Mesh`, ~1–2 ms por frame; con una `RectShape` por celda no se cumple ni con 15k celdas.

### 3.9 Manifiesto y elevación
- Manifiesto con `embed-manifest`: `asInvoker`, `longPathAware`, PerMonitorV2 y `supportedOS` Win10/11. **Sin** `activeCodePage UTF-8`, porque se usan APIs W.
- Relanzar con `ShellExecuteExW("runas", "--scan C:\\")` solo si es el mismo usuario (ver §2.11).
- En v1.0, un helper elevado `--mft-helper` que devuelve el resultado compacto por una named pipe con ACL limitada al SID del usuario. Así la UI sigue sin privilegios.

### 3.10 Dependencias (versiones del 2026-10-15; **volver a verificarlas al escribir `Cargo.toml`**)
`eframe/egui 0.36.2` · `windows 0.62.2` / `windows-sys 0.61.2` (con `windows-core` 0.100 por separado; tolerar duplicados en `cargo-deny`) · `crossbeam-deque 0.8.8` · `crossbeam-channel 0.5.17` · `arc-swap 1.9.2` · `rayon 1.12` · `trash 5.2.9` · `xxhash-rust 0.8.19` · `blake3 1.8.7` · `rustc-hash 2.1.3` · `thiserror 2.0.21` · `dunce 1.0.5` · `embed-manifest 1.5.1` · `rfd 0.17.2` · `serde` + `toml` · `fluent-bundle` · `criterion 0.8.2` · (v0.3) `ntfs-reader 0.6.0` · (v1.0) `usn-journal-rs 0.5.0`.

### 3.11 Calidad y distribución
- **CI** en `windows-latest` (acciones fijadas por SHA, `timeout-minutes`, Dependabot): `fmt --check`, `clippy -D warnings`, `cargo test`, `cargo deny` (también semanal), MSRV 1.95, `cargo check` ARM64, build release y **job de privacidad** (`scripts/privacy-check.ps1 -All -GenericOnly` y sus pruebas). En local, hooks `pre-commit` y `commit-msg` con el mismo check y valores locales.
- **Todo binario publicado se compila con `scripts/build-release.ps1`** (`--remap-path-prefix` y **CRT estático**, `+crt-static` en `.cargo/config.toml`), que exige el conjunto exacto de ejecutables (salida JSON de Cargo), verifica que no contienen rutas ni nombres de la máquina y, con `scripts/check-pe.py`, que la tabla de imports no depende del runtime de C (`VCRUNTIME*`, `MSVCP*`, `api-ms-win-crt-*`).
- El CI revisa además el mensaje y **todos los blobs de cada commit** del push o PR (job `privacy-history`); `privacy-check` rechaza por firma los formatos opacos o comprimidos y las rutas UNC. Las herramientas `--json` y `--screenshot` no pisan archivos sin `--overwrite`.
- "Abrir en el Explorador" usa `SHParseDisplayName` con UTF-16 exacto + `SHOpenFolderAndSelectItems`; ninguna ruta pasa por `str`/`display()` (regla también para el `cleaner`).
- **Tests:**
  - El builder (agregación, orden, dedup de hard links, histogramas y umbrales), el layout y las reglas, todo puro.
  - Árboles sintéticos con semilla.
  - **Tests del `cleaner` sobre carpetas temporales**: nunca borra nada fuera del conjunto permitido, absorbe descendientes y pasa la revalidación TOCTOU.
  - Test diferencial MFT/walker sobre un VHDX creado en CI. Deben coincidir salvo los metadatos NTFS y las carpetas denegadas.
  - Fuzzing del parser MFT antes de la 1.0.
- **Release:** `lto = "fat"`, `codegen-units = 1`, `strip`. `panic = "unwind"` en las 0.x y `abort` solo tras el fuzzing.
- **Firma:** SignPath Foundation (licencia OSI, build automatizado, política de firma publicada en el README).
- **Entrega:** **MSI por usuario desde la v0.1**, con **WiX 5.0.2** (`wix` CLI como `dotnet tool`, versión y SHA-512 fijados; `.wxs` escrito a mano, sin acciones personalizadas; desde WiX 6 el paquete exige aceptar el acuerdo OSMF), más un zip portable y `SHA256SUMS.txt`. `scripts/build-msi.ps1` parte siempre de `build-release.ps1` (binarios reproducibles con `/Brepro`) y valida el MSI con `scripts/msi-validate.ps1` (tablas en lista cerrada). `release.yml` (etiqueta `v*`): jobs `build` → `attest` (procedencia) → `verify` → `release` en **borrador**; un ensayo `workflow_dispatch` (`dry_run`) solo desde `main`. La v0.1 sale **sin firmar**; SignPath Foundation (gratis para open source) cuando el mantenedor lo solicite (`CODE_SIGNING.md`). winget y ARM64 publicado, en la v1.0.

---

## 4. Hoja de ruta

Referencia de diseño: un SSD con un volumen de ~1 M de archivos. La memoria se mide siempre como **private working set** (§3.5).

| Fase | Contenido | Hito medible |
|---|---|---|
| **0. Spike ✅** | Hecho: ver [fase-0-conclusiones.md](fase-0-conclusiones.md). Backend wgpu DX12 + `Mesh`; API `FileIdExtdDirectoryInfo`; POSIX descartado; Mica descartado; destino de las carpetas de desarrollo diferido a la v0.2 | Cerrado (ronda 4, §9) |
| **1. MVP v0.1** | ✅ Inc. 1 (modelo + escáner + CLI `tesela-scan` + CI). ✅ Inc. 2 (layout + interfaz de solo lectura + `--demo`). ✅ Inc. 3 (borrado seguro, §3.7). ✅ Inc. 4 (MSI por usuario + release en borrador). Walker + cola prioritaria + TreeBuilder (con dedup de hard links), inicio, pantalla de escaneo con veredicto, treemap (`Mesh` cacheado) + lista + detalles, bloque del sistema, cesta por destino, borrado definitivo + Papelera, protección, Temp/cachés/Papelera/pagefile/hiberfil, resumen, historial de resúmenes, es/en, claro/oscuro. **Entregable: instalador MSI por usuario** + zip portable, generados en CI | 1 M de archivos en < 30 s en caliente; veredicto de rutas conocidas en < 5 s; private working set ≤ 75 MiB en reposo con el mapa dibujado y ~85 B por entrada en régimen (≈ 155 MiB con 1 M; aceptado por el usuario); p99 de CPU por frame < 8 ms con hover; **0 borrados fuera del conjunto permitido** en los tests; el MSI instala, actualiza y desinstala sin administrador |
| **2. v0.2** | Carpetas de desarrollo (con la decisión de destino y "renombrar primero", §2.6), descargas e instaladores viejos, archivos grandes, colorear por antigüedad y riesgo, filtros, Deshacer + `Ctrl+Z`, historial visible, atajos, flechas en el treemap | Relayout de 20k rectángulos < 5 ms |
| **3. v0.3** | MFT + "Reiniciar como administrador" (con la cesta persistida) + test diferencial | 1 M < 3 s; 10 M < 30 s; totales iguales al walker salvo metadatos NTFS y carpetas denegadas |
| **4. v1.0** | Duplicados, WinSxS / puntos de restauración, helper elevado, USN incremental, Excluir + exclusiones persistentes, exportar CSV/HTML, menú contextual del Explorador (HKCU), winget, ARM64 publicado, fuzzing, auditoría con lector de pantalla | Reescaneo incremental < 2 s; exe ≲ 16 MB (wgpu DX12 ya ocupa ≈ 10,5 MB); paquetes firmados |

---

## 5. Riesgos
| Riesgo | Mitigación |
|---|---|
| Borrar algo indebido (sobre todo en definitivo) | Definitivo solo con reglas; `cleaner` como defensa; revalidación justo antes de borrar; sin seguir reparse points; tests sobre carpetas temporales; simulación y log |
| La Papelera no cabe, o la unidad es de red o extraíble | Límite real recalculado al ejecutar y casilla explícita |
| OneDrive: borrar un archivo sincronizado lo quita de la nube | Aviso en la confirmación; los archivos solo en la nube no se pueden seleccionar |
| Hard links sobrecontados | Dedup por FileId en el builder |
| La UI no arranca en RDP o en VMs sin GPU | `wgpu` DX12 con WARP por defecto; `glow` como plan B. Pendiente de la prueba del usuario (§6) |
| Archivos abiertos con `FILE_SHARE_DELETE`: el borrado parece salir bien pero no libera espacio | Detectar el proceso del navegador; "renombrar primero" en la v0.2; contar solo lo realmente liberado |
| Casos raros del parser MFT; `ntfs-reader` joven | Trait, test diferencial, fuzzing, parser propio de reserva |
| Cifras que luego no se cumplen (navegador abierto, archivos en uso) | "al menos", "unos", "hasta"; el resumen da la cifra real |
| SmartScreen o antivirus | SignPath |
| Accesibilidad del treemap | La lista equivalente con AccessKit |
| Elevar con otra cuenta | Aviso, la cesta no se recupera; helper en v1.0 |

---

## 6. Verificaciones (fase 0)
Hechas (detalle en [fase-0-conclusiones.md](fase-0-conclusiones.md)):
- [x] `FILE_ID_EXTD_DIR_INFO` en Win11: funciona y es la API principal.
- [x] Los metadatos NTFS (`$MFT`…) no aparecen al enumerar. `FSCTL_GET_NTFS_VOLUME_DATA` funciona sin administrador con un handle del directorio raíz.
- [x] Memoria, exe y ms/frame de egui-`wgpu` frente a `glow`.
- [x] 100k archivos a la Papelera: ~2× el umbral (una sola medición); decisión diferida a la v0.2.
- [x] `restore_all` correcto; `list()` es lineal en el tamaño de toda la Papelera.
- [x] `FileDispositionInfoEx` + POSIX funciona, pero no es más rápido: descartado.
- [x] Reparse points (junction): el borrado elimina el enlace, no el destino.
- [x] Mica (solo con glow), `SegUIVar.ttf` (`wght` de 300 a 700), licencia de Fluent (MIT), versiones de los crates.
- [x] Nombre "Tesela": libre en crates.io y winget (2026-10-15).

Pendientes (las tres primeras las hace el usuario):
- [ ] **RDP y Hyper-V sin GPU** con la build wgpu DX12 (y glow como comparación). Instrucciones en fase-0-conclusiones.md (Pendiente).
- [ ] **Windows 10 22H2:** `FILE_ID_EXTD_DIR_INFO` y `SegUIVar.ttf`.
- [ ] **Symlinks de directorio** al borrar. No necesita administrador si se activa el Modo de desarrollador.
- [ ] `FOF_WANTNUKEWARNING` junto con `FOF_NO_UI` y el límite real de la Papelera aplicado: **test obligatorio del incremento 3** (§3.7).
- [x] Incremento 4 (MSI): CRT estático sin `VCRUNTIME140.dll` (verificado en los imports); ciclo instalar/actualizar/bloquear versión anterior/reparar/carpeta personalizada/desinstalar sin elevación, con la máquina idéntica al final; validación del paquete (ejecutables, avisos, sin rutas locales).
- [ ] MSI pendiente de probar: dos usuarios, aplicación abierta durante la actualización (Restart Manager), rollback de una instalación fallida, firmas EXE/MSI.
- [x] WiX 5.0.2 en GitHub Actions (sin `cargo-wix`). Pendientes: `winget-releaser` y runner ARM64 (v1.0).
- [ ] **Configuración del repo (la hace el mantenedor):** reporte privado de vulnerabilidades, ruleset de etiquetas `v*`, protección de `main` (PR + code owners + checks), environment `release` con revisor, "Require SHA pinning", immutable releases y MFA. Lista detallada en `installer/README.md`.
- [ ] Marcas registradas del nombre "Tesela" sin revisar.

## 7. Siguiente paso
**Publicar la v0.1.0** (etiqueta, verificación del borrador y publicación) y, después, la **auditoría externa** que pedirá el usuario; luego, la v0.2 (§4).

## 8. Fuera del alcance (explícito)
Limpiador de registro, "optimizar" o desfragmentar, desinstalar programas, análisis de red o nube en v1, cuentas, telemetría o anuncios, sunburst o 3D, triturado o recuperación, editor o gestor de archivos completo, reglas automáticas sin intervención del usuario.

## 9. Registro de revisión del diseño
**Ronda 1** (revisión del plan unificado; 19 puntos más 14 que añadieron los agentes):

| Problema | Resolución |
|---|---|
| "Siempre a la Papelera" contradecía "liberar espacio" | Riesgo y destino pasan a ser ejes separados; definitivo solo para lo regenerable por regla |
| CSR "ordenado sin ordenar después" era imposible | Se ordena una vez en el TreeBuilder y luego se asignan los ids |
| Escritura concurrente en el arena | Batches por directorio + `fetch_add` de `base_id` + colector |
| MFT y walker construían el modelo por caminos distintos | Registros planos → un único TreeBuilder |
| Hard links sobrecontados en el walker (WinSxS) | Dedup por FileId en el builder |
| La suma escaneada ≠ el espacio usado | Bloque "Windows y otros datos del sistema" |
| Árbol inmutable frente a limpieza | Overlay con bitset + deltas |
| Veredicto en 5 s frente a escaneo de 30 s | Cola prioritaria + "al menos X GB" |
| Fuentes e iconos de Microsoft no redistribuibles / solo Win11 | Fuentes del sistema en tiempo de ejecución + Fluent UI System Icons |
| Faltaba proteger dentro del propio perfil | Intocables como destino + "Contenido de …" |
| Color de las carpetas por tipo y antigüedad sin definir | Histogramas de 12 B por directorio, con umbrales del 50 % y el 80 % |
| Alcance incoherente (Excluir, Deshacer, historial) | Reordenado por versión (§4) |
| Navegador abierto | Cifras honestas y "Volver a comprobar" |
| Selecciones solapadas en la cesta | Normalización por ancestros |
| `glow` en RDP | `wgpu` por defecto (hipótesis) |
| Slint en el spike | Eliminado |
| `activeCodePage UTF-8` | Eliminado |
| Principio 6 frente a las categorías "Revisar" | "No se preselecciona" |
| OneDrive sincronizado | Aviso + no seleccionable si solo está en la nube |

**Ronda 3** (validación del asesor, Fable) — diseño validado con estas correcciones aplicadas:
1. Caché de un navegador abierto: no se preselecciona y se pide "Cierra Chrome para liberar hasta X GB". El borrado POSIX con `FILE_SHARE_DELETE` daba falsos "liberados" y corrompía el índice de la caché.
2. `%TEMP%` solo borra lo de más de 24 h (`min_age`).
3. "Vaciar Papelera" se ejecuta antes de mover elementos a la Papelera.
4. Hard links: el que conserva el tamaño es el de ruta menor en orden lexicográfico (determinista), no el de id menor.
5. Menores:
   - Histogramas para todos los nodos (12 B).
   - Cadena de ancestros `INCOMPLETE` en la cola prioritaria y adopción por `FileId`.
   - `$Recycle.Bin`, `Default` y `Public` denegadas.
   - El titular suma solo lo Seguro.
   - El MVP borra con `remove_file` y deja POSIX para la v0.2.

**Ronda 9** (incremento 4, instalador y release: construcción Sonnet, 2 auditorías Sonnet, revisión con el asesor):
- WiX 5.0.2 (no 6/7, para no aceptar el acuerdo OSMF en nombre del usuario), por usuario, sin acciones personalizadas; `INSTALLFOLDER` persistido en HKCU para que actualizar y desinstalar respeten una carpeta personalizada.
- Icono dibujado por `build.rs` (el repo no versiona imágenes fuera de `docs/img/`) e incrustado con `winresource`, con VERSIONINFO.
- Cadena de suministro: acciones por SHA, permisos mínimos por job, attestation en un job aislado, verificación antes del borrador, `--locked`, WiX con `nuget.config` limpio y SHA-512 comprobado, binarios reproducibles.
- Los tests con la Papelera real van detrás de `TESELA_REAL_RECYCLE_TESTS=1` (los runners de GitHub no tienen Papelera de escritorio); en local se lanzan con la variable.
- La seguridad real de la publicación hoy es el borrador que verifica una persona: la configuración del repo (rulesets, protección de `main`, environment) queda para el mantenedor.

**Ronda 8** (incremento 3, el borrado: construcción Sonnet, 3 auditorías Sonnet con sondas de ataque, revisión con el asesor):
- El diseño "verificar y actuar sobre el mismo handle" resistió todas las carreras probadas (sustitución ×300, junctions en hijo/ancestro/raíz, hard links, renombrados, cancelación, matar el proceso).
- Bloqueantes corregidos: verificación de la Papelera por identidad (no por nombre; fallaba con `.lnk`/`.url` y con extensiones ocultas), solo unidades locales (el recurso administrativo UNC de la propia máquina esquivaba la protección), `TEMP` y exenciones (podían borrar datos protegidos).
- Cambio de diseño: se retira la lápida (§3.7.4); Papelera por ruta original con sink de eventos y handle de guarda; reparto por operaciones con presupuesto y corte de seguridad (una operación con un elemento que no cabía eliminaba definitivamente todo el lote).
- Pendiente de prueba manual antes/después de la v0.1: `HideFileExt=1` (por construcción no afecta), volumen sin `MaxCapacity`, directiva `NoRecycleFiles`, symlinks de directorio, unidades de red mapeadas. Diferido a la v0.2: coste cuadrático con árboles de miles de niveles y protección de carpetas de sistema en otras unidades.

**Ronda 7** (auditoría externa de Astra/ChatGPT sobre 6781a03, triada con el asesor):
- **B1 (diseño):** el borrado se rediseña como "identidad aprobada + actuar sobre el mismo handle" (§3.7): apertura exclusiva, FileId+volumen, recursión relativa a handles (patrón CVE-2022-21658), Papelera vía lápida renombrada por handle y verificación por elemento, Vaciar con alcance enumerado, hard links detectados por `NumberOfLinks`, "liberado" = diferencia medida del espacio libre.
- **M2:** las cachés de navegador son solo directorios concretos de caché; el perfil nunca lo es (§2.6).
- **M1:** `--json` y `--screenshot` no sobrescriben sin `--overwrite`. **M3:** los atajos respetan el control enfocado. **M5/M6:** `privacy-check` revisa el mensaje completo, rechaza formatos opacos y el CI revisa todos los commits del push/PR. **m1–m11 y N1:** en el ciclo de endurecimiento.
- **M4:** se generalizan las menciones al equipo de medición en la documentación (versión del SO y tipo de disco). No se reescribe el historial: no identifican el equipo.
- **N2:** el bloque de espacio no explicado se ordena por tamaño; no hay fuentes de reserva de egui.
- **Incremento 4:** CRT estático (`+crt-static`) y requisitos de MSI/SignPath añadidos a §6.
- **Ciclo de endurecimiento** (13 commits, construcción Sonnet + auditoría Sonnet + revisión con el asesor): M1–M6, m1–m11 y N1 corregidos; exe ≈ 11,0 MiB con el CRT estático. Por decisión del usuario NO se añaden patrones genéricos de versión de Windows ni de capacidad de disco: lo que importa es la información personal (nombres de carpetas y archivos, usuario, rutas del perfil, capturas reales).
- **Distribución:** decisión del usuario: **solo MSI** (no MSIX ni Microsoft Store). Firma con SignPath Foundation (gratis para open source) cuando el usuario la solicite; la v0.1 sale sin firmar con el aviso de SmartScreen explicado.

**Ronda 6** (incremento 2 del MVP: construcción Sonnet, 2 auditorías Sonnet, verificación propia y asesor):
- Memoria: el objetivo pasa a ~85 B por entrada en régimen (≈ 155 MiB con 1 M); **decisión del usuario**, no se recorta. En reposo con el mapa: ~68 MiB.
- Fuentes: sin `default_fonts` de egui (no se redistribuyen fuentes OFL/UFL); reservas DEL SISTEMA por escritura (CJK, tailandés, índicas, árabe, hebreo, símbolos), con kana y han siempre juntos. Las pruebas de fuentes se saltan lo que el equipo no tiene (CI en Windows Server).
- Avisos de terceros: `THIRD-PARTY-NOTICES.md` generado con `cargo about` (no se edita a mano), incrustado en el `.exe` y visible en Ajustes › Acerca de.
- "Otros": paginado cuando ningún hijo cabe, de modo que abrirlo siempre avanza. Esquinas de 4 px solo en celdas ≥ 16 px. Relayout de 20k celdas ≈ 1 ms (hito de la v0.2 ya cumplido).
- Capturas: `--screenshot` **exige `--demo`** (en código); `docs/img/` solo admite PNG registrados con su SHA-256 en `docs/img/README.md`, con fragmentos y longitudes acotados.
- Hooks sin `-ExecutionPolicy Bypass` (heurística del antivirus).
- egui 0.36.2 se mantiene (0.37 existe; subir es tarea de la v0.2). Mientras no haya riesgo (inc. 3), la cabecera del escaneo dice "X GB en sitios conocidos". Tabla de rutas conocidas con `KnownKind`.
- Pendiente para la v0.2: profundidad de "Otros" anidados en colas muy pesadas (R4) y validar el zlib de `IDAT` y los data URI en `privacy-check` (R6). Las herramientas de medición (`--bench-hover`, `--idle-memory`) pueden fallar si la ventana queda tapada: es del entorno, no de la app.

**Ronda 5** (incremento 1 del MVP: construcción Sonnet, 2 auditorías Sonnet, revisión con el asesor):
- `Hist` de 14 B con `dominant_type` / `dominant_age`; regla del 80 % precisada; `mtime` 0 fuera de la antigüedad.
- Memoria: régimen ~78 B por entrada; pico ≈ 2× el régimen.
- `Record` de 32 B con el `FileId` aparte; dedup tras el BFS y también con `logical = 0`.
- Respaldo `FindFirstFileExW`: sin dedup ni cola prioritaria, escaneo "aproximado".
- `INCOMPLETE_BELOW`; sin bloque de espacio no explicado si hay directorios incompletos.
- Carpetas de la nube: pobladas se recorren, no pobladas se cuentan sin entrar.
- Hilos `min(16, 2 × núcleos)`; `cargo test` en vez de nextest; binarios publicados solo con `build-release.ps1`.
- Privacidad: `privacy-check.ps1` con hooks `pre-commit` y `commit-msg`, lista negra local en `<git-common-dir>/privacy-denylist`, pruebas propias y job de CI.

**Ronda 4** (resultados de la fase 0: construcción Sonnet, auditoría Sonnet, revisión final con el asesor):
- **UI:** wgpu solo DX12, treemap como `Mesh` cacheado (objetivo p99 < 8 ms), fondo sólido (sin Mica).
- **Métricas:** memoria = private working set (≤ 75 MiB en reposo con el mapa); exe ≲ 16 MB.
- **Borrado:** POSIX propio descartado; la nota de `FILE_SHARE_DELETE` se amplía a `remove_file`/`DeleteFileW`; junctions medidos.
- **Carpetas de desarrollo:** destino diferido a la v0.2, con "renombrar primero".
- **Hilos:** se sigue ajustando midiendo (la mejora se satura pronto).
- **Deshacer:** verificar archivo a archivo.
- **Entregable:** MSI por usuario desde la v0.1 (decisión del usuario).

**Ronda 2** (cierre de las decisiones cruzadas): carpetas de desarrollo a la Papelera por defecto (la medición de la fase 0 puede cambiarlo), histogramas, pagefile/hiberfil como nodos "solo informar", `wgpu` por defecto y cesta persistida al elevar. Ni diseño ni arquitectura tienen objeciones bloqueantes.
