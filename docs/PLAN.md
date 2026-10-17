# Plan: visual disk space analyzer for Windows (Rust)

> **Status:** design **closed and validated** (2 rounds between UX design and architecture + validation by the advisor; see §9). **Phase 0 completed** ([findings](phase-0-findings.md)); its numbers are already incorporated here (round 4 in §9). The only open items are the user's pending checks in §6. Next: MVP v0.1.
> **Working name:** **Tesela**, the single piece of a mosaic, like each block of the treemap. Checked on 2026-10-15: there is no `tesela` crate on crates.io, and on GitHub the only match is `raycoarana/tesela`, an unrelated Android library from 2015. The name is free on crates.io and winget (checked on 2026-10-15); registered trademarks not reviewed.

---

## 1. Key decisions

| Topic | Decision | Why |
|---|---|---|
| Language | Stable Rust; the MSRV is set by egui (today 1.95), with `rust-toolchain.toml` | Fast, low RAM and a single binary |
| UI | **egui/eframe 0.36** with a custom theme. **`wgpu` backend compiled with DX12 only** (WARP as the fallback for RDP and virtual machines without a GPU). Vulkan is excluded. The treemap is drawn as a **`Mesh` tessellated once and cached**. `glow` is plan B only if the RDP/Hyper-V test (§6) fails with wgpu and works with glow | Measured in phase 0: exe ≈ 10.5 MB; ≈ 70 MiB private working set with the map; with `Mesh`, ~1–2 ms per frame. MIT/Apache, compatible with SignPath; AccessKit. Slint is ruled out because of its license (GPL or non-OSI). WinUI via `windows-reactor` needs the Windows App Runtime and is still in preview |
| UI plan B | Win32 + Direct2D | Smaller, but it means months of widgets, keyboard and UIA work |
| Visualization | **Squarified treemap + list with bars**, kept in sync. No sunburst | The treemap shows the shape of the disk; the list gives exact figures and is the accessible view |
| Size shown | **On disk** (`AllocationSize`) as the main value; the logical size also appears in the details panel | It is what is actually freed. Explorer shows the logical size, so both are shown |
| Scan in the MVP | Own parallel walker with `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` and `FindFirstFileExW` as the fallback | Gets `AllocationSize` and `FileId` in batches. `walkdir` is single-threaded and `jwalk` is deprecated |
| Fast scan (v0.3) | `$MFT` read with `ntfs-reader` (streaming `MftScan`) behind a `trait MftSource` | WizTree style, but it requires administrator rights |
| Elevation | `asInvoker` manifest: it **never** asks for UAC at startup. Optional "Fast mode (administrator)" button | No friction. In v1.0, an elevated helper that only scans |
| Deletion | **Destination set by each rule, independent of risk** (see §2.5). Everything goes through the basket | Sending temporary files to the Recycle Bin frees nothing |
| OS | Official support for Windows 10 22H2 and Windows 11; technical floor 1809. x64 from the MVP; ARM64 is built in CI from the MVP and published in v1.0 | Rust requires Win10+. `aarch64-pc-windows-msvc` is Tier 1 |
| Deliverable | **Per-user MSI installer from v0.1** (WiX): no administrator, installs to `%LOCALAPPDATA%\Programs\Tesela`, Start menu shortcut, shows up in Installed apps, updates in place and is signed with SignPath. Portable zip as a secondary option. MSIX ruled out | User decision (2026-10-15). Consistent with "never UAC". MSIX isolates the file system, which clashes with reading the whole disk and the MFT |
| License | MIT / Apache-2.0 dual | Rust ecosystem and a SignPath requirement |
| Out of scope | Telemetry, accounts, "registry optimizer", uninstaller, sunburst, shredding, rules that delete without intervention | Noise with no value |

---

## 2. Product and UX

### 2.1 Principles
1. **Three clicks to free space:** Analyze → "Clean the safe stuff" → Confirm.
2. **The answer first, then the detail.** Within seconds "You can free at least X GB" appears, and then the map.
3. **Nothing is deleted without seeing the total and the destination** (permanent or Recycle Bin).
4. **Risk is seen before it is read:** color with an icon and a shape, never color alone.
5. **Everything is explained in one sentence:** every suggestion carries its "why".
6. **Light and honest:** if something is not clearly safe, it is **not preselected**. Only what has really been freed is counted as freed.

Terminology: the UI always says "files" and "Recycle Bin", as Windows does.

### 2.2 Flow
1. **Home:** one card per drive with a usage ring and the line "Last cleanup: 3 days ago · measured change in free space: 8 GB", which only appears if there is history.
2. **Scan:** two zones (§2.4).
3. **Exploration:** Map, List and Recommendations.
4. **Basket** grouped by destination.
5. **Confirmation:** "Free up to 11.0 GB and move 3.2 GB to the Recycle Bin".
6. **Summary:** "Freed now: 11.0 GB", followed by "3.2 GB is in the Recycle Bin · [Empty now] [Open the Recycle Bin]". "Empty now" becomes the primary button if what is in the Recycle Bin exceeds what has already been freed.

When cleaning finishes **there is no rescan**: the blocks shrink with an animation and the totals are recalculated (§3.6).

### 2.3 Main window
```
┌──────────────────────────────────────────────────────────────────────────────┐
│ [≡] Tesela   C: › Users › ana › AppData     [Search… Ctrl+F] [Color by: Type ▾] [⚙] │
├────────────┬─────────────────────────────────────────────┬───────────────────┤
│ DRIVES     │   ┌─────────────┬──────────┬───────────┐   │ DETAILS           │
│ ▣ C: 400/500│   │ AppData     │ Videos   │ Downloads │   │ Videos            │
│ ▢ D:200/1000│   │  32 GB      │ 28 GB    │ 12 GB     │   │ On disk  28.3 GB  │
│ VIEWS      │   │             │          ├───────────┤   │ Size     28.1 GB  │
│ ◉ Map      │   │             │          │ Docs 8 GB │   │ 142 files         │
│ ○ List     │   ├─────────┬───┴──────────┤           │   │ Mod.: 92 % < 2022 │
│ ○ Recomm.  │   │ node_mod│ Windows and other data ⓘ │   │ Risk: Review      │
│   (14 GB)  │   └─────────┴──────────────────────────┘   │ [Explorer][Path]  │
├────────────┴─────────────────────────────────────────────┴───────────────────┤
│ BASKET: 23 items · 14.2 GB       [View] [Empty]          [Clean 14.2 GB ▸]   │
├──────────────────────────────────────────────────────────────────────────────┤
│ 400 GB used of 500 GB · 1.0 M files · scanned 2 min ago         [Rescan]     │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 2.4 Scan screen (early verdict)
```
┌ While we scan your disk… ────────────────────────────────────────────┐
│ You can free at least 9.0 GB             [ Clean the safe stuff ]    │
│ (updates as it goes)          ◌ Checking 2 of 5 places…              │
│  ✓ Windows temporary files ...... 2.0 GB                             │
│  ✓ Browser cache ................ 7.0 GB                             │
│  ✓ Recycle Bin .................. 0.5 GB   (Review)                  │
│  ◌ Project folders .............. searching…                         │
│ ──────────────────────────────────────────────────────────────────── │
│ Map: Users ████████ · Windows ██ · …               [Stop and view]   │
└──────────────────────────────────────────────────────────────────────┘
```
- Known paths are scanned first, through a priority queue (§3.3). Each row shows its final figure when it finishes and does not change again.
- "Clean the safe stuff" becomes active when the sweep of the known paths ends.
- The headline **adds up only the Safe rows**, which is exactly what "Clean the safe stuff" does. In the example the Recycle Bin (Review) is shown but does not count.
- While rows are still pending, the headline says "**at least**". When it finishes it changes to "You can free 14.2 GB" with a counter animation. If something turns up later, it is shown with a "+3.1 GB found" chip.
- **Stop and view:** the tree is built with what has been received up to that moment. Folders that were not walked in full show "≥ X GB" with faint diagonal stripes, the only pattern active by default. If one of them goes to the basket, the `cleaner` measures it again before confirming.

### 2.5 Risk and destination: two separate axes
**Risk** (Safe / Review / Info only):
- 🟢 Shield with a check: can be preselected and is included in "Clean the safe stuff".
- 🟠 Eye: never preselected.
- 🔺 "Info only" triangle: no checkbox.

**Destination** (its own icon, no risk colors):
- 🗑̸ **Permanent:** only regenerable items **classified by a rule**. Basket phrase: "Deleted, and it is recreated on its own".
- 🗑 **Recycle Bin:** everything else. Phrase: "Goes to the Recycle Bin · you can recover it".
- If something does not fit in the Recycle Bin (it exceeds the volume's real limit, or it is on a network or removable drive), a third amber group appears: "**Will be deleted permanently because it cannot be sent to the Recycle Bin**", with an explicit checkbox. This is recalculated **at execution time**, not when it is checked; if it changes, confirmation is requested again.
- Advanced setting "**Always send everything to the Recycle Bin**".
- **Execution order:** if "Empty the Recycle Bin" and items with a Recycle Bin destination are in the basket together, the Recycle Bin is emptied **first** and then the items are moved. That way what the user wants to keep recoverable is not deleted in the same step.
- What the user drags in by hand or is not classified ("Your file") has Review risk and a Recycle Bin destination, and is **never** deleted permanently except through the checkbox case above. The confirmation shows the 5 largest items and the phrase "These are your own files; they are not recreated".

### 2.6 Cleanup assistant
| Category | Risk | Destination | Version |
|---|---|---|---|
| Temporary files (`%TEMP%`, `Windows\Temp` if accessible), **only those not modified in the last 7 days** (`min_age_hours = 168`: the most recent of the creation, write and change dates; it is the `cleanmgr` criterion, "not modified in the last week"; "why": "Temporary files not modified in the last week; recent ones are left in case an installer is using them") | 🟢 | Permanent (files in use are skipped) | MVP |
| Browser cache (Chrome, Edge, Firefox): **only specific cache directories** (Chromium: `Cache`, `Code Cache`, `GPUCache`, `ShaderCache`/`GrShaderCache`; Firefox: `cache2` and `startupCache` under `%LOCALAPPDATA%`). **The browser profile is never cache**: history, bookmarks, cookies, credentials and persistent storage are never candidates | 🟢 | Permanent | MVP |
| Recycle Bin ("Empty") | 🟠 | Permanent (it is the action itself). **Enumerated scope:** only the list of items captured at confirmation is purged; what arrives afterwards is not touched and is reported | MVP |
| `pagefile.sys`, `hiberfil.sys` (explained only: shrinking or disabling it requires a command prompt **as administrator**; `powercfg /h off` also removes hybrid sleep and fast startup; `powercfg /h /type reduced` shrinks it and keeps fast startup; Tesela runs nothing) | 🔺 | — | MVP |
| `node_modules`, `target`, `.venv`, `__pycache__`, `.gradle`, detected by a neighboring file (`package.json`, `Cargo.toml`, `pyvenv.cfg`…) | 🟢 | **Recycle Bin** by default ("It can be reinstalled with one command. It goes to the Recycle Bin in case you had local changes"). **Permanent destination DEFERRED to v0.2:** phase 0 measured ~2× the 10 s threshold for 100k files, but in a single measurement. In v0.2 it is repeated ≥ 3 times with a realistic tree, and **"rename first"** is tested: renaming the folder to a sibling tombstone name fails if there are open files (it stops safely) and then the renamed folder is deleted | v0.2 |
| Old downloads (> 6 months), old installers | 🟠 | Recycle Bin | v0.2 |
| Large files (> 500 MB) | 🟠 (only highlighted) | Recycle Bin | v0.2 |
| Duplicates (size → partial hash → full hash; one copy always remains) | 🟠 | Recycle Bin | v1.0 |
| WinSxS, restore points | 🔺 (DISM / settings advice; the app runs nothing) | — | v1.0 |

**Browser open:** `chrome.exe`, `msedge.exe` and `firefox.exe` are detected. If the browser is open, its row is **not preselected** and is not included in "Clean the safe stuff". The row says "**Close Chrome to free up to 1.9 GB**" and offers [Check again]. The reason is that Chromium opens its cache with `FILE_SHARE_DELETE`: with the browser open the deletion "works", but frees nothing until Chrome releases the files, and it also corrupts the browser's index. Phase 0 measured that this also happens with `std::fs::remove_file` and `DeleteFileW`, not just with POSIX: the name disappears and the space stays held. The Chrome case is an extrapolation from a synthetic handle. That is why a cache in use is not cleaned. The app never closes the browser. Before cleaning, figures are given as "about" or "up to".

**OneDrive:** files that exist only in the cloud cannot be selected to free space. If a synced file is deleted, the confirmation warns: "Synced with OneDrive: deleting it also removes it from the cloud".

### 2.7 Protection
- **Untouchable as a target** (their contents can be cleaned): the profile root, `AppData`, `AppData\Local`, `Roaming`, `LocalLow`, `C:\`, `C:\Users` and the ancestors of denied paths.
  - On the map they have a dotted border and a padlock.
  - In the menu, "Add to basket" appears disabled with a text explaining why; "View contents" takes its place.
  - If they are dragged to the basket, they bounce back.
- **Denied entirely:** `C:\Windows` (except the known temporary files), `Program Files`, `ProgramData` (except the known caches), `System Volume Information`, `$Recycle.Bin` (except the "Empty" action), other users' profiles, `C:\Users\Default`, `C:\Users\Public`, the `$` metadata files and the app's own folder.
- **Known-folder roots** (Documents, Desktop, Videos…): they are added as "**Contents of Videos**", without including the folder itself. Review risk, Recycle Bin destination.
- **Folders that contain something protected:** they are not accepted as a whole target (they stay "untouchable": padlock and bounce); their unprotected contents can be cleaned. This is safer than accepting them with a note, and it is what the audited code does.
- **Normalized basket:** if a node is added whose ancestor is already in the basket, it is not duplicated; if an ancestor is added, it absorbs its descendants.
- **The real defense is the `cleaner` crate, not the UI.** It computes the effective set and, right before deleting each item, validates the protected path again, checks that it is not a reparse point and that the size has not changed much (TOCTOU). This applies to both the Recycle Bin and permanent deletion.
- Files in use or without permission are skipped, counted and explained in the summary.

### 2.8 Unexplained space
- There is a synthetic gray block, sorted by size like one more child of the root: "**Windows and other system data · 40 GB**", with an ⓘ icon. It is computed as `total − free − scanned` using magnitudes **of the volume** (`NtQueryVolumeInformationFile(FileFsFullSizeInformation)`), measured when the scan finishes. The user's quota is shown separately ("Available to your account") and is never mixed into the remainder; without the query and with a quota, there is no block.
- It is clamped to ≥ 0 and hidden if it is under 1 %. It only appears in the scan of a whole volume and is marked "approximate".
- Tooltip: "Includes Windows files, hibernation, restore points and folders your user cannot see. This is normal and cannot be deleted from here". Link "See the detail (administrator mode)".
- If the gap exceeds 5 %, the status bar shows "The map explains 360 of 400 GB. What about the rest?". Separately, "N folders without permission" is shown.

### 2.9 Treemap and color
- It shows one level of children and a faint hint of the next, with 4 px corners and 2 px spacing. The label only appears if it fits.
- **Two distinct layers:**
  - Interactive grouping: blocks smaller than 24×24 px are merged into "Others (1,000 items, 400 MB)", which can be opened. It is the suffix of the sorted slice.
  - Drawing LOD: nothing is subdivided below about 3×3 px and there is a cap of ~15k rectangles per frame.
- Navigation: 180 ms animated zoom, breadcrumb, tooltip and context menu. In the MVP the menu has Open in Explorer, Copy path, Add to basket and Properties; "Exclude" arrives in v1.0.
- **Color by Type** (Okabe-Ito):
  - Video `#0072B2`, images `#E69F00`, audio `#CC79A7`, documents `#009E73`, archives `#D55E00`, programs `#56B4E9`, other `#8A8F98`.
  - A folder takes the color of its dominant type **only if it exceeds 50 % of the bytes**. Otherwise it is "**Mixed**", a lighter bluish gray, distinct from "Other".
  - A 4 px bottom strip shows the top 3 types when the block is ≥ 48 px. The tooltip breaks down the percentages.
- **Color by Age:** uses 5 bands by mtime (< 1 month, 6 months, 1 year, 3 years, older). A folder takes the band where 80 % of its bytes fall. Tooltip: "92 % of this has not been modified since 2022". It always says "modified", never "used".
- **Color by Risk:** only items that have a recommendation are colored.
- **Grays with meaning:**
  - "Info only": triangle, cannot be selected.
  - System block: ⓘ.
  - "Mixed": with its strip.
  - The system block and the "info only" items stay gray in all modes.
- After a cleanup, the histograms are marked "≈ / Approximate until rescan".
- Optional accessibility patterns by type: **dots** (stripes remain reserved for "incomplete").

### 2.10 Identity, accessibility and languages
- **Palette** (light / dark):
  - Background `#F5F6F8` / `#1B1D21`
  - Surface `#FFFFFF` / `#25282E`
  - Accent `#0F6CBD` / `#4CA3F0`
  - Safe `#107C41` / `#3FB871`
  - Review `#B26A00` / `#F2B24D`
  - Info only / danger `#C42B1C` / `#FF6B5E`
- **Fonts:** loaded from the system at runtime, without redistributing them: `SegUIVar.ttf` if it exists (measured: egui applies `wght` from 300 to 700; `opsz` is not applied on its own), otherwise `segoeui.ttf` + `segoeuib.ttf`. The path is obtained with `SHGetKnownFolderPath(FOLDERID_Fonts)`. **No egui fonts are embedded** (`default_fonts` disabled); non-Latin scripts use system fallbacks. If there were no usable system font at all, the app warns and exits.
- **Icons:** a subset of about 30 **Fluent UI System Icons** (MIT, © Microsoft, confirmed), embedded. They look the same on Win10 and Win11. It does not depend on `SegoeIcons.ttf`.
- **Solid background.** Mica does not work with wgpu (measured in 4 configurations). It would only come back if we switched to plan B `glow`.
- **Languages:** Spanish and English from day 1 (`fluent-bundle`), with no literal strings in the code.
- **Accessibility:**
  - PerMonitorV2 DPI.
  - 2 px visible focus and a treemap navigable with the arrow keys.
  - AccessKit in the list.
  - "Reduce animations" is respected (`SPI_GETCLIENTAREAANIMATION`), also in the indeterminate progress and the wait indicators.
  - AA contrast.

### 2.11 Persistence
- `%APPDATA%\Tesela\config.toml`: settings.
- `%LOCALAPPDATA%\Tesela\history.jsonl`: **summaries only** (date, drive, used, freed now, sent to the Recycle Bin). It can be deleted from Settings. The tree is never stored.
- Basket when relaunching as administrator: it is saved to a file in `%LOCALAPPDATA%` with a restricted ACL. The new process reads it, validates the paths again, recalculates the sizes and deletes it. If the elevation is with **another account**, a warning is shown and it is not recovered.

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
3. **Ancestros y recursión.** Windows no tiene `openat`, pero `NtCreateFile` con `OBJECT_ATTRIBUTES.RootDirectory = <handle del padre>` es la apertura relativa. Se abre por handle la raíz de la categoría (`%TEMP%`, el directorio de caché…), se verifica su FileId contra el capturado, y los hijos se enumeran y abren **relativos a ese handle**, cada uno con `FILE_OPEN_REPARSE_POINT`, rechazando cualquier reparse point. Es el patrón con el que se reescribió `std::fs::remove_dir_all` tras CVE-2022-21658. **Frontera de confianza:** los ancestros por encima de la raíz de categoría son del perfil del propio usuario; cambiarlos exige sus propios privilegios, y la app nunca se ejecuta elevada en la v0.1. **Cadena de la raíz (v0.1.1):** una raíz de categoría solo se acepta tras recorrer su ruta, tal como se escribió y desde la raíz del volumen, componente a componente con aperturas relativas (`FILE_OPEN_REPARSE_POINT`, solo atributos), sin ningún reparse point y con la ruta resuelta por el handle igual a la canónica; el recorrido devuelve la identidad del último componente y la aprobación, al abrir la raíz para listarla, exige la misma identidad y la misma ruta (si no, `ReparseAncestor`: un ancestro cambiado por un junction entre los dos pasos no aprueba el objeto equivocado). Se repite en cada aprobación; una raíz descartada no es categoría: consta en el plan, en el registro (nota `rejected`) y la interfaz la muestra como "no disponible" con su motivo. Residual: los ancestros de `%LOCALAPPDATA%` se resuelven antes (carpetas conocidas canónicas, perfil propio).
4. **Papelera (sin lápida).** Comprobación previa con apertura exclusiva (identidad y "en uso"); después se abre un **handle de guarda** `DELETE | READ_ATTRIBUTES` compartido que se mantiene durante toda la operación de la shell y hasta juzgar el resultado (bloquea renombrar sus **ancestros**, error 5, sin impedir que la shell recicle; **no** impide renombrar el propio elemento ni poner otro con su nombre, porque comparte `DELETE`). Se recicla **por la ruta original** con `IFileOperation` (`FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_NO_CONNECTED_ELEMENTS`: sin la última, una página `.html` arrastraba su carpeta `_files`) y un `IFileOperationProgressSink` propio: en `PostDeleteItem`, `psiNewlyCreated` es el elemento ya en la Papelera (nulo si la shell lo eliminó definitivamente). `Recycled` exige: misma identidad (FileId del `$R`), el nombre aprobado ya no existe, la ruta final de la guarda está en `$Recycle.Bin` y los enlaces no cambiaron; si no, `RecycledOther` (recuperable, se informa y no se aplica al árbol). Nulo con éxito → "eliminado definitivamente (no cabía)". **Receptor (v0.1.1):** `PreDeleteItem` abre el elemento por su ruta sin seguir enlaces y compara volumen + FileId con lo aprobado para esa ruta exacta; un sustituto, un hijo o cualquier otra ruta se vetan con `E_ABORT` (la shell cancela lo pendiente). Residual, dicho con precisión: entre esa comprobación y el movimiento la shell actúa por ruta, y un sustituto puesto exactamente ahí llega a la shell; solo se garantiza que se **detecta** después (si cabe y la shell da su ruta, `RecycledOther`, recuperable; si no cabe, la shell lo elimina y consta como `DeletedUnexpectedly`; si no da ruta o falla, `Unverified`). **v0.1.2:** "no enviado" (`NotRecycled`) solo se afirma con el objeto aprobado intacto en su sitio y, en un directorio, con la **huella** de su contenido idéntica (suma y XOR de un hash por entrada de FileId, tamaños, enlaces y clase, independiente del orden); vale también para resultados de **fallo** de la shell y para "sin aviso"; una pérdida parcial es `DeletedUnexpectedly` y corta el envío. Una medida de directorio solo vale si es **completa** (sin `denied`, `in_use`, `other_errors`, `gone` ni `changed`); lo que creció frente a lo aprobado o a la última medida no se envía. **v0.1.3:** un **directorio va siempre solo** en su operación: se lee la ocupación de la Papelera, se mide el directorio y se aplica `take_op` con esa medida y esa ocupación, y se entrega sin nada de por medio salvo la línea de intención del registro. Los **archivos contiguos de la cola** (el grupo se corta en el primer directorio) pueden viajar juntos porque `PreDeleteItem` vuelve a medir cada uno **por su handle de guarda** (`FileStandardInfo`, el mayor entre bytes en disco y tamaño lógico, sin tolerancia) y, si ocupa más que lo contado para el presupuesto, veta con `E_ABORT` ("cambió desde la confirmación"). Coste medido con la shell real y 500 archivos pequeños: ≈ 7 s en una operación con la comprobación (indistinguible sin ella) frente a ≈ 70 s con una operación por archivo; un directorio por operación cuesta del orden de 0,15 s. Residuales de la v0.1.3: lo que crece mientras corre la propia operación de la shell o entre `PreDeleteItem` y el movimiento; en un directorio, entre el recorrido y la entrega; y la ocupación de la Papelera, leída antes del recorrido. La v0.1.4 cierra los dos últimos con la vigilancia de cambios y la doble lectura de la ocupación (abajo), salvo el límite de NTFS con escritores abiertos. La comprobación del contenido de un directorio no enviado **corre primero y manda**, con tres estados: intacto (medida completa e igual), cambiado (medida completa y distinta: `DeletedUnexpectedly`, que afirma la **diferencia** —"se eliminó definitivamente, o su contenido cambió o perdió una parte"—, no un borrado seguro) y desconocido (medida incompleta, fallida o cancelada: `Unverified`, sin afirmar pérdida); cambiado y desconocido detienen el envío. **Un resultado de fallo (`hrDelete < 0`) al enviar corta el lote** (el receptor devuelve `E_ABORT`); la purga no cambia. `ShellDeclined` solo se da con el contenido intacto y pendientes cancelados; un fallo de un archivo con pendientes en su lote detiene todo el envío (conservador). El `E_ABORT` del propio receptor no consta como error de Windows. Una carpeta con destino Papelera que ya al aprobar no se pudo leer entera se rechaza ("no se pudo leer entera: N elementos sin acceso"). `PreDeleteItem`/`PostDeleteItem` reciben la ruta **larga** (`SIGDN_DESKTOPABSOLUTEPARSING`): toda comparación se hace en forma larga canónica (en CI `%TEMP%` es 8.3). `PostDeleteItem` registra el evento y devuelve `E_ABORT` si la shell eliminó definitivamente o no dio la ruta: el corte ya no espera al final del lote, y `Unverified` también corta. El juicio de "eliminado" es **por objeto y por contenido, no por nombre**: con la guarda abierta, un archivo eliminado sigue visible como pendiente de borrar (`DeletePending`; medido `hr = 0x00270008`), así que solo es "la shell no tocó nada" (`ShellDeclined`, medido `hr = 0x00270003` con una carpeta que contiene un disperso enorme) si la guarda no está pendiente de borrar, reabrir la ruta da la misma identidad y, en un directorio, la medida (tamaño y número de entradas) es idéntica a la tomada justo antes del envío; cualquier otro caso es borrado definitivo, aunque sea parcial. La lógica del receptor es código puro (`SinkCore`, `post_delete_aborts`) probado sin COM. **Reparto por operaciones** (`take_op`): cada operación ≤ 90 % de `MaxCapacity` menos la ocupación actual (`SHQueryRecycleBin`); lo grande (> 10 %) va solo; sin capacidad u ocupación conocidas, `NoRoom` (falla cerrado); cada directorio se vuelve a medir justo antes de la operación que lo incluye (si creció no se envía, "cambió desde la confirmación"); lo que no cabe queda `NoRoom` sin llegar a la shell; **corte de seguridad** tras el primer borrado inesperado (el resto queda donde estaba). Sin `MaxCapacity` no hay capacidad que garantizar (`Unfit(CapacityUnknown)`: solo con confirmación expresa de borrado definitivo, con la pista de fijar un tamaño máximo en las propiedades de la Papelera); la directiva `NoRecycleFiles` y cualquier duda = "no cabe" (falla cerrado). La lápida renombrada del diseño anterior se retiró porque el usuario veía y restauraba el nombre lápida y podían quedar huérfanas. Medido: `FOF_WANTNUKEWARNING` con `FOF_NO_UI` se cuelga (su prueba corre en un proceso hijo con timeout).
   - **Vigilancia de cambios (v0.1.4).** Un directorio lleva una vigilancia (`ReadDirectoryChangesW` de subárbol, sobre un segundo handle con identidad verificada) abierta **antes** del recorrido de medida y viva hasta su `PreDeleteItem`, donde se **para** tras la última comprobación. Cualquier notificación, desbordamiento o error invalida la medida: antes de enviar, no se envía (se comprueba antes y después de escribir `recycle-begin`); en `PreDeleteItem`, veto con `E_ABORT`; en ambos casos "cambió desde la confirmación". Una carpeta sin vigilancia no se envía. Límite de NTFS: las notificaciones de tamaño y última escritura de un archivo que otro proceso mantiene abierto llegan al volcar o cerrar; un escritor así (con `FILE_SHARE_DELETE`) que lo agrande dentro de la ventana puede llegar a la shell con más de lo medido: la shell decide, si no cabe elimina, Tesela lo detecta (`DeletedUnexpectedly`) y detiene el envío. Altas, bajas y renombrados notifican al instante. La ventana residual empieza en `PreDeleteItem`. Coste ≈ 170 µs por directorio (debug) frente a ≈ 1,2 ms del recorrido de medida. Si el usuario mete `%LOCALAPPDATA%\Tesela` (o un ancestro) en la cesta, el propio registro cae en el subárbol vigilado y el envío se veta (falla cerrado). La solución estructural (que la shell **pregunte** antes de eliminar, con `FOF_WANTNUKEWARNING` y una ventana propietaria) es un spike de la v0.2.
   - **Huella (v0.1.4).** Un directorio solo se envía con el **mismo** contenido que se aprobó: huella, recuentos, ruta descendiente más larga y tamaño de los reparse points idénticos, sin más entradas protegidas, reparse points ni archivos de la nube; cualquier cambio, también encoger, vuelve a la confirmación. Cargo contra la Papelera: `max(bytes + compartidos + reparse; lógico + lógico de reparse)`; los reparse points de dentro cuentan su propio tamaño según el listado (un placeholder de nube no hidratado cuenta su tamaño lógico completo, a propósito) y nunca se mide su destino. Restricción: ninguna regla con destino Papelera puede llevar `min_age > 0` sin revisar esta comparación. En la aprobación, un ancestro enlazado (junction, symlink, punto de montaje, enlace de WSL) se rechaza como `ReparseAncestor`; las carpetas de sincronización de la nube se toleran como ancestro (un archivo suelto bajo OneDrive sigue llegando a `confirm-cloud`); los hijos se abren relativos al directorio listado con el FileId y el volumen del listado.
   - **Ocupación (v0.1.4).** La ocupación de la Papelera se lee antes y después del recorrido y manda la peor (mayor ocupación, menor capacidad); es una heurística, no una garantía. El 90 % es el mismo cálculo exacto en `u128` en el encaje y en el reparto.
5. **Hard links.** No hace falta el FileId en el modelo: si `NumberOfLinks > 1` al aprobar, el elemento se marca en la confirmación ("compartido con otro archivo: no liberará espacio") y se excluye de la estimación de bytes liberados. El `NumberOfLinks` aprobado se guarda exacto: si cambia al ejecutar, se rechaza ("cambió desde la confirmación").
6. **Vaciar la Papelera:** alcance enumerado; se purga por los elementos de la carpeta shell de la Papelera (`DeleteItem` con el mismo sink). En esta build de Windows la shell deja el `$I` huérfano, así que `remove_orphan_meta` borra el `$I` hermano exacto de un elemento recién purgado (solo bajo la carpeta del SID propio, con el `$R` ya inexistente, contenido `$I` válido que casa con el elemento, apertura exclusiva y lectura/borrado por el mismo handle). El verbo `delete` del menú contextual de la Papelera se cuelga y no se usa. **v0.1.1:** al enumerar se capturan las identidades del `$R` y del `$I` y al purgar se vuelven a comprobar (si no coinciden, el elemento se salta); el `$I` huérfano solo se borra si el `$R` está *no encontrado* (cualquier otro error es "se desconoce") y nombre, carpeta de origen, tamaño y momento del borrado coinciden; sin identidad (FAT/exFAT) no se purga y se cuenta aparte. Purga con `FOF_NO_UI | FOF_NO_CONNECTED_ELEMENTS`. **v0.1.2:** cada trozo de 500 abre una guarda por `$R` (verificada por identidad) y la operación lleva un receptor obligatorio **por tipo** (`Operation::new` no admite `None`) cuyo `PreDeleteItem` vuelve a comprobar por ruta el `$R` y el `$I` (los hijos de una carpeta de la Papelera, por la identidad de su `$R`) y veta con `E_ABORT`. Medido con la shell real: el vetado recibe `E_ABORT` (`0x80004004`) y su `$R` sigue; con el veto en el medio de un lote, el anterior se purga y el siguiente no recibe aviso; un veto corta también los trozos restantes. El veto se atribuye al elemento de la lista (`Veto::id()`) y consta como "cambiado". Al terminar, la guarda demuestra si lo borrado era el objeto aprobado (`delete_pending`); si no, "sustituido". Residual: entre la comprobación por ruta y el borrado real; un sustituto puesto ahí se borra (se detecta justo después).
7. **Tres contadores.** "Eliminado definitivamente" y "movido a la Papelera" son sumas de tamaños; **"Liberado ahora" es la diferencia medida de `GetDiskFreeSpaceEx` antes y después** (incluye de forma automática hard links, handles abiertos y borrados definitivos inesperados). Si divergen, se muestran ambos con el motivo en una línea.
8. **Navegador.** Detectar el proceso es UX ("cierra Chrome"); la garantía es el punto 2: si el navegador arranca a mitad, los archivos restantes fallan la apertura exclusiva y se saltan.
9. **Paralelismo y progreso.** Archivos en un pool dedicado y directorios después, de abajo arriba; progreso con atómicos y cancelación entre entradas. Se saltan y cuentan `SHARING_VIOLATION` y `ACCESS_DENIED`.
10. **Modo simulación (`--dry-run`)** que registra las identidades aprobadas sin tocar nada, y log local de lo borrado. Nombres `limpieza-<AAAAMMDD>-<HHMMSS>-<secuencia de 3 cifras>[-simulacion].jsonl`; la retención es por clase (30 limpiezas reales, 10 simulaciones), ordena por fecha, hora y secuencia como números (también los nombres de la v0.1.1), nunca borra el registro que acaba de abrir y comprueba que existe tras crearlo.
11. **Tests obligatorios del `cleaner`** sobre árboles en `%TEMP%`, que **compitan con la operación**: sustituir un archivo por otro del mismo tamaño, cambiar un ancestro por un junction, abrir un archivo en exclusiva desde otro hilo… y comprobar rechazo o salto. Las decisiones del receptor de la shell se prueban también sin COM (el CI no tiene Papelera de escritorio); con `TESELA_REAL_RECYCLE_TESTS=1` se confirman con la shell real y elementos propios. Los callbacks COM del receptor se prueban también directamente (sin operación de la shell). Los restos de esas pruebas los limpia una red de seguridad que solo toca elementos de una prueba (`tesela-test-<uuid>`, forma de UUID exigida, borrados hace más de 2 minutos), los borra por ruta solo bajo la Papelera propia y sin reparse points, falla cerrado y hace fallar la prueba si queda algo. Toda comparación de rutas, sobre formas canónicas largas (en CI `%TEMP%` es 8.3).
12. **Solo unidades locales:** la ruta final resuelta debe ser de una unidad local con letra (fija o extraíble); las rutas UNC (incluido el recurso administrativo de la propia máquina) y las unidades de red se rechazan (`NotLocal`) ya en la cesta. `TEMP` solo se acepta si es `%LOCALAPPDATA%\Temp` o cuelga de él, y una exención nunca levanta una denegación por debajo de ella; desde la v0.1.1 cada exención solo levanta la **única zona denegada que declara** (su `User Data`, `Mozilla\Firefox`, la carpeta de Windows; `%TEMP%` ninguna), nunca "esa zona o cualquiera por encima".
13. **Cierre de la ventana bloqueado** mientras se ejecuta una limpieza (desde cualquier pantalla); registro local con `start` y `recycle-begin` volcados antes de cada operación para poder reconstruir qué se movió tras un cierre forzoso. **v0.1.1:** sin registro utilizable no hay ninguna acción destructiva (la simulación puede ir sin él); un error de escritura cancela la limpieza en el siguiente punto seguro; antes de cada operación se escribe y vuelca una línea de intención (`delete-begin`, `recycle-begin`, `bin-begin`) y vaciar la Papelera anota cada elemento (`bin-item`); `flush` tras cada línea y `sync_all` al crear y al terminar (el riesgo modelado es el cierre forzoso). El informe dice si el registro se truncó y distingue la cancelación del usuario del fallo del registro. Un registro solo se descarta si no se escribió ninguna operación. `open_rel` rechaza nombres con `:` (flujos alternativos). **v0.1.2:** el resultado de cada entrada se escribe y se vuelca al sistema en cuanto se produce, con su error comprobado; el volcado es **al sistema, no a disco** (`sync_all` solo al crear y al terminar). Tras un cierre forzoso lo único que puede faltar es lo que estaba en vuelo: una entrada por hilo de trabajo (hasta 32) en el borrado definitivo, o el lote entero de una operación de la shell (su lista consta en su intención). Coste medido: de 6 a 13 µs por resultado en el escritor; borrando 100 000 archivos propios, +6 % con 1 hilo y +2 % con 4. Tope por registro: 250 000 líneas o 64 MiB (pasado el tope, `log_truncated`). **v0.1.3:** el registro abierto está protegido: se abre con `FILE_SHARE_READ | FILE_SHARE_WRITE`, sin `FILE_SHARE_DELETE`, así que mientras un escritor lo tiene abierto nadie puede borrarlo ni renombrarlo (sí leerlo); la garantía vale entre escritores ≥ 0.1.3. La poda salta el registro abierto (la retención puede pasar del límite mientras haya un escritor vivo); `discard` cierra antes de borrar y deja el escritor inutilizable; "Borrar el historial" no borra el registro abierto.
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
- **La aplicación no limpia elevada (v0.1.2).** El manifiesto es `asInvoker`, pero un proceso elevado que lance Tesela le pasa sus privilegios: el `cleaner` consulta `TokenElevation` y, si está elevado o no se puede consultar (falla cerrado), `execute` devuelve `ExecError::Elevated` antes de tocar nada y vaciar la Papelera no se hace; el análisis y la simulación siguen permitidos. Las funciones que borran son `pub(crate)`: no se puede saltar desde otro crate. Límite conocido: con UAC desactivado o con la cuenta Administrador integrada todo proceso es elevado y Tesela no limpia (README y `SECURITY.md` lo explican).
- **Carga de DLL (v0.1.2; v0.1.4 añade `SetDllDirectoryW("")`, que saca la carpeta actual de la búsqueda).** `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` como primera sentencia de `main` (en `tesela` y `tesela-scan`) y `/DEPENDENTLOADFLAG:0x800` en el enlazado (las importaciones estáticas no KnownDLL, como `dxgi`, `dwmapi`, `uxtheme`, `propsys`, `uiautomationcore`, se resuelven antes de `main`). El compilador de shaders DX12 se fija a FXC (no se sondea `dxcompiler.dll`). Probado: con archivos con nombre de DLL junto a una copia del exe, arranca, pinta y no carga ninguno; COM/Shell, UIA y el controlador de la GPU siguen funcionando.

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

> **Vigente desde el 2026-10-16: [ROADMAP.md](ROADMAP.md)** (tesis de producto, segmento, reglas, catálogo, fase de validación de 3 meses y su métrica de decisión). La tabla de abajo queda como histórico del diseño original; lo posterior a la v0.1 lo decide ROADMAP.md.

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
Con la **v0.1.4** publicada (cuarta ronda de correcciones y vigilancia de cambios en el envío a la Papelera), empezar la **fase de validación de 3 meses** de [ROADMAP.md](ROADMAP.md) §5: evidencia pública de seguridad, recorridos completos a partir de casos reales y un piloto sin ayuda del autor. El resto de la hoja de ruta depende de su métrica de decisión. Después de la primera versión estable, el repositorio pasa entero al inglés (la aplicación sigue en español e inglés).

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
2. `%TEMP%` solo borra lo de más de 7 días (`min_age`).
3. "Vaciar Papelera" se ejecuta antes de mover elementos a la Papelera.
4. Hard links: el que conserva el tamaño es el de ruta menor en orden lexicográfico (determinista), no el de id menor.
5. Menores:
   - Histogramas para todos los nodos (12 B).
   - Cadena de ancestros `INCOMPLETE` en la cola prioritaria y adopción por `FileId`.
   - `$Recycle.Bin`, `Default` y `Public` denegadas.
   - El titular suma solo lo Seguro.
   - El MVP borra con `remove_file` y deja POSIX para la v0.2.

**Ronda 13** (auditorías externas de Astra y Gemini sobre `v0.1.3`, verificadas por el coordinador; dos construcciones Sonnet en paralelo con alcances disjuntos —limpieza y resto—, auditorías Sonnet independientes con corrección de sus hallazgos, revisión con el asesor). Correcciones de la v0.1.4:
- **Envío a la Papelera (cuarta ronda de la misma familia de concurrencia):** vigilancia de cambios desde antes del recorrido hasta `PreDeleteItem`, huella idéntica (también encoger vuelve a la confirmación), ocupación leída dos veces y cargo con los reparse points de dentro (§3.7.4). Se corrige la afirmación de que el margen del 90 % absorbía los cambios de ocupación.
- **Aprobación:** un ancestro enlazado se rechaza (`ReparseAncestor`, también bajo un volumen montado en una carpeta); OneDrive se tolera como ancestro (la primera versión lo rechazaba y la auditoría lo detectó). Hijos abiertos relativos al directorio listado. `C:` sin barra deja de ser raíz de unidad (clave inválida, denegada).
- **Vaciar la Papelera:** un `$R` con enlaces dentro, o que no se pudo comprobar entero, se excluye antes de entregarlo ("no purgado", registro `linked`) sin cortar el lote; el veto del receptor queda como última defensa. Un `$I` ilegible se salta, consta (`bin-unlisted`) y no entra en lo prometido. Eventos y vetos se casan por nombre exacto antes que por `PathKey` (directorios sensibles a mayúsculas). Volúmenes montados en carpetas reconocidos en la protección, la Papelera y el tipo de unidad.
- **Escáner:** la cola prioritaria valida cada candidata componente a componente desde la raíz (sin seguir enlaces, solo `DirAction::Walk`, nombres 8.3 expandidos, `..` descartado); raíz `\\?\Volume{GUID}\` sin letra como volumen completo; `reparse_tag` solo con `REPARSE_POINT`; sin lote con los ids agotados; cachés de navegador verificadas para el escáner y candidatas para el `cleaner`. Efecto aceptado: una carpeta conocida bajo una carpeta de nube poblada deja de ser prioritaria.
- **Modelo y UI:** `Overlay::add_up` resta a los ancestros lo realmente restado; sumas saturadas; WTF-8 rechaza los 3 bytes sobrelargos; medidas obsoletas descartadas tras limpiar durante el escaneo. Sesión aislada (`--idle-memory`, `--bench-hover`, con o sin `--demo`) sin historial ni raíces de limpieza reales, y siempre simulación si no es la demo (M4 solo se cubre con pruebas unitarias: `SHGetKnownFolderPath` ignora las variables de entorno).
- **Pedido en las pruebas del usuario:** las unidades de la barra lateral se ven pulsables (fondo, cursor, tooltip; la actual no se anuncia como botón) y hay botón **Inicio** con `Alt+Inicio` en la barra superior, deshabilitado durante una limpieza. "Analizar una carpeta…" en la barra lateral se descartó: rompía la leyenda a 1280×800; se llega por Inicio.
- **Herramientas e instalador:** `privacy-check` (límite de tamaño primero, BOM acumulados, UTF-8 sin NUL, cadenas de 2+, autor y committer con su etiqueta; al regenerar una captura, el hash de la anterior va a `$legacyDemoImages`); `check-pe.py` exige `DependentLoadFlags` 0x800 y REPRO; GUID fijo del acceso directo; validaciones nuevas del MSI. `INSTALLFOLDER` **no** es propiedad segura: el MSI no tiene interfaz y es por usuario; marcarla segura solo valdría en una instalación elevada, donde dejaría al usuario dirigir dónde escribe el servidor. CODEOWNERS ampliado y `cargo deny --locked` con `all-features`. Para quien active SignPath: firmar los `.exe` **antes** de empaquetar el MSI y el zip.
- **Incidente y regla:** el antivirus del equipo volvió a retirar `cargo.exe` mientras otro auditor ejecutaba las pruebas que lanzan copias del programa con archivos con nombre de DLL al lado. Esas pruebas solo corren con `TESELA_PROCESS_TESTS=1`, que define el CI; en CI faltar la variable es un fallo, no un salto. En local se trabaja con la toolchain anterior hasta que el usuario restaure la actual; el CI confirma con la fijada.
- **Pendiente:** repetir `medicion_a_m3_*` (las cifras del registro son de la v0.1.3, antes de que `over_cap` tomase el cerrojo del escritor); spike de `FOF_WANTNUKEWARNING` con ventana propietaria (v0.2).
- **Pasada real única** (Papelera real, solo elementos propios, una vez y en serie, en el árbol fusionado): las 279 pruebas del `cleaner` en verde, incluidas las tres nuevas con la shell real (una carpeta de tres niveles queda `Recycled`, lo que confirma que la shell no toca el árbol antes de `PreDeleteItem`; un cambio justo antes de `PreDeleteItem` se veta; un `$R` propio con un junction dentro se excluye y el resto se purga). Papelera del usuario intacta.

**Ronda 12** (tercera auditoría externa de Astra sobre `v0.1.2`, verificada por el coordinador; construcción Sonnet, auditoría Sonnet con re-verificación, revisión con el asesor). Correcciones de la v0.1.3:
- **A2-B1:** dentro de una misma operación el presupuesto podía quedar obsoleto (una carpeta ya medida crecía mientras se medía otra). Ahora un directorio va siempre solo y los archivos del lote se vuelven a medir por su guarda en `PreDeleteItem` (§3.7.4).
- **A2-M1:** un fallo de la shell tras una pérdida parcial dejaba seguir el resto del lote; ahora un resultado de fallo corta el lote.
- **A2-M2:** la poda de un escritor podía borrar el registro abierto de otro; ahora el registro abierto no admite borrado compartido (§3.7.13).
- **A2-m1:** una comprobación imposible se presentaba como pérdida; ahora hay tres estados (intacto, cambiado, desconocido).
- La auditoría interna añadió pruebas negativas para el presupuesto de un directorio al entregarlo y para la comprobación de tamaño sin tolerancia, y textos que afirman la diferencia y no un borrado seguro.
- **Incidente:** una campaña de mutantes del auditor ejecutó el binario de pruebas del borrado en bucle y el antivirus del equipo lo marcó como malware (bloqueó 19 aplicaciones, retiró `cargo.exe` y reinició el equipo). Regla nueva: en la máquina del desarrollador no se ejecutan mutantes en bucle ni pruebas con la Papelera real repetidas; eso va al CI o a una VM desechable. Riesgo de producto asociado: un antivirus podría desconfiar de `tesela.exe` al limpiar (ROADMAP).

**Ronda 11** (auditorías externas de Astra —borrado— y Grok —resto— sobre `v0.1.1`, verificadas por el coordinador; dos construcciones Sonnet en paralelo con alcances disjuntos, auditorías Sonnet con re-auditoría de cada corrección, revisión con el asesor). Correcciones de la v0.1.2:
- **Borrado (Astra):** A-B1 los archivos se vuelven a medir por su guarda justo antes de cada operación y se recomprueba el presupuesto; A-B2 una medida de directorio solo vale si es completa; A-M1 los fallos de la shell y "sin aviso" se juzgan como un posible borrado, con huella del contenido; A-M4 vaciar la Papelera lleva guardas por `$R` y un receptor que veta sustitutos; A-M2 la retención no puede borrar el registro recién abierto; A-M3 cada resultado se vuelca y comprueba; A-m1 los callbacks COM se prueban directamente. Además: no se limpia elevado (E1), textos que no prometen cifras ni ausencia de riesgo (T1), temporales a 7 días (E2), nota de `hiberfil` honesta (E3), las funciones que borran son `pub(crate)` y la red de seguridad de las pruebas reales solo toca elementos con forma `tesela-test-<uuid>`.
- **Resto (Grok):** G-m1 la raíz y las rutas prioritarias del escáner no siguen un reparse point (raíz-enlace → `ScanError::RootIsLink`); G-m2 `privacy-check` detecta rutas UNC con `/`; G-m3 las reglas de ruta rigen en `spike/` (dos archivos históricos con rutas sintéticas permitidos por SHA-256 exacto); G-S1 valores partidos por NUL; G-S2 `install-wix.ps1` comprueba el SHA-512 **antes** de instalar; G-S3 documentado (no hay forma robusta de limitar `INSTALLFOLDER` sin acción personalizada).
- **Endurecimiento de DLL** (hallado al investigar G-S3): `dxcompiler.dll` se sondeaba por nombre y varias importaciones estáticas se buscaban primero en la carpeta del exe (§3.9).
- Incidente de proceso: un auditor creó junctions a la raíz de una unidad y lanzó `rm -rf` sobre su carpeta (no borró nada; comprobado). Regla nueva para todos los agentes: nunca borrado recursivo sobre carpetas con enlaces, quitar los enlaces como enlaces y no crear enlaces a raíces de unidad.
- Producto: estudio de diferenciación y dos revisiones externas → [ROADMAP.md](ROADMAP.md).

**Ronda 10** (auditoría externa de Astra sobre `v0.1.0`, verificada por el coordinador; construcción Sonnet, auditorías Sonnet independientes con re-auditoría de cada corrección, revisión con el asesor). Correcciones de la v0.1.1:
- **B1:** un reparse point por encima de una raíz de categoría (`User Data`, `%TEMP%`) volvía borrable contenido protegido, porque la raíz canónica se convertía en exención y levantaba cualquier denegación por encima. Ahora la cadena se recorre por componentes sin reparse points, la aprobación exige la misma identidad que vio la cadena y cada exención levanta solo su zona declarada (§3.7.3, §3.7.12). Una categoría descartada se muestra como "no disponible" con su motivo.
- **B2:** el reparto de la Papelera falla cerrado (sin capacidad u ocupación, `NoRoom`); se elimina el 5 % asumido; los directorios se miden justo antes de su operación.
- **M1, M2, S1:** `PreDeleteItem` veta sustitutos; `PostDeleteItem` corta el lote en el primer borrado definitivo o sin ruta; `FOF_NO_CONNECTED_ELEMENTS`. Se corrige la afirmación de que la guarda impedía renombrar el propio elemento.
- **M3:** vaciar la Papelera comprueba la identidad de `$R` y `$I`; el `$I` huérfano solo se borra con pruebas completas.
- **M4, m2:** sin registro no hay limpieza; intención antes de cada operación; la simulación no poda registros reales.
- **m1:** `NumberOfLinks` exacto. **m3:** `leak-scan` y `privacy-check` detectan valores locales no ASCII (UTF-8, Latin-1, UTF-16 sin BOM) sin perder la detección UTF-16. **S2:** `msi-validate` fija el árbol de carpetas, las filas de Upgrade, el orden `FindRelatedProducts` < `LaunchConditions` y compara distinguiendo mayúsculas.
- Las auditorías internas añadieron: la aprobación de una raíz compara la identidad con la de la cadena; el juicio de "eliminado" pasa a ser por objeto y por contenido (una primera versión por nombre ocultaba borrados reales con la guarda abierta, reproducido con la shell real); pruebas sin COM del receptor; retención de registros por clase; nombres con `:` rechazados.
- **M5** (protecciones del repositorio: rulesets, protección de `main`, environment de release, releases inmutables) es configuración del mantenedor y queda en su lista.
- Comportamiento visible nuevo: en un volumen sin tamaño máximo de Papelera configurado, lo que iría a la Papelera pide borrado definitivo expreso.

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
