# Spike de la fase 0 de Tesela

Código **desechable de medición** que convierte en datos las hipótesis de `docs/PLAN.md` §6. No es el producto: no
reutilices nada de aquí sin releerlo. Las conclusiones están en `docs/fase-0-conclusiones.md`. Los datos brutos de
cada medición **no se versionan** (describen el equipo donde se toman): se quedan en local, en `spike/results/` (en `.gitignore`).

Es un workspace de Cargo **propio** (`spike/Cargo.toml`), independiente del workspace del producto. Requisitos:
Windows 10/11 x64, Rust estable (edición 2024) y el toolchain `x86_64-pc-windows-msvc`.
Todas las versiones de dependencias se verificaron con `cargo search` el 2026-10-15 y coinciden con PLAN.md §3.10.

```text
spike/
├─ common/        utilidades: GetProcessMemoryInfo, percentiles, espacio libre del volumen
├─ enum-bench/    experimento 2: enumeración paralela de C:\ (solo lectura) + sondeo de $MFT / FSCTL
├─ trash-bench/   experimento 3: Papelera con el crate `trash`
├─ delete-bench/  experimento 4: borrado POSIX con FileDispositionInfoEx
├─ ui-treemap/    experimento 1: treemap sintético de ~30k rectángulos en egui (wgpu o glow)
├─ fonts-mica/    experimento 5: SegUIVar.ttf con ejes variables y Mica sobre eframe
├─ scripts/       run-enum.sh, run-ui.sh, run-trash.sh: baterías completas; collect-results.sh + redact.py: copian y (lista negra local opcional en spike/.denylist, no versionada)
│                 redactan los datos a results/; summarize.py: tablas Markdown a partir de los JSON
└─ results/       (local, no versionado) JSON/CSV/PNG de tus mediciones
```

## Lista negra local (`spike/.denylist`)

`scripts/collect-results.sh` copia los resultados de `out/` a `results/` redactando datos personales con
`scripts/redact.py`. La protección **principal** es la **lista blanca** de `redact.py` (solo pasa lo que reconoce). Como
comprobación adicional, el script busca en lo ya redactado una **lista negra local**: `spike/.denylist`.

- **Es un archivo por máquina y NO se versiona** (está en `.gitignore`): contiene justo los nombres privados que no
  deben publicarse, así que subirlo al repositorio los filtraría.
- Si no existe, el script avisa (`AVISO: no existe spike/.denylist`) y solo comprueba el nombre de usuario de Windows y los
  SID.
- **Cómo recrearlo** en otra máquina o tras clonar: crea `spike/.denylist` con **una expresión regular (`grep -E`) por
  línea**, sin comentarios (cada línea no vacía es un patrón; las vacías se ignoran). Pon nombres de carpetas personales,
  de programas instalados que no quieras revelar, el nombre del equipo, etc. Forma del archivo, con valores genéricos:

  ```text
  nombre-de-una-carpeta-privada
  nombre-de-un-programa-instalado
  nombre-del-equipo
  ```

- Antes de publicar `results/`, ejecuta `collect-results.sh` con la lista negra creada: aborta si queda alguna coincidencia
  y no modifica `results/`.

## Compilar

Desde `spike/`:

```powershell
# Experimentos de sistema de archivos
cargo build --release -p enum-bench -p trash-bench -p delete-bench

# UI: el backend se elige con una feature y NO se pueden mezclar en el mismo binario.
cargo build --release -p ui-treemap --no-default-features --features wgpu-all   # wgpu con todos los backends
cargo build --release -p fonts-mica --no-default-features --features wgpu
$env:CARGO_TARGET_DIR = "target-dx12"      # wgpu SOLO con el backend DX12 (como PLAN.md §1)
cargo build --release -p ui-treemap --no-default-features --features wgpu-dx12
$env:CARGO_TARGET_DIR = "target-glow"
cargo build --release -p ui-treemap -p fonts-mica --no-default-features --features glow
Remove-Item Env:CARGO_TARGET_DIR
```

Los binarios quedan en `target\release\` (wgpu, todos los backends), `target-dx12\release\` (wgpu solo DX12) y
`target-glow\release\` (glow). Esos directorios y `out\` están en `.gitignore`. Perfil release: `lto = "fat"`, `codegen-units = 1`, `strip = "symbols"`, como en el plan.

## Ejecutar cada experimento

Todos escriben sus resultados como JSON en `out/` (local). **No subas esos resultados ni capturas al repositorio**:
contienen datos de tu equipo. El repositorio solo publica conclusiones (`docs/fase-0-conclusiones.md`).

### 1. `ui-treemap` (UI: RSS, exe, ms/frame)

```powershell
# Interactivo (hover real, FPS en pantalla):
target\release\ui-treemap.exe
# Medición sin interacción: se cierra solo y escribe el JSON (+ CSV de frames):
target\release\ui-treemap.exe --bench --label prueba --out out\ui\prueba.json --frames-csv out\ui\prueba.csv
# Opciones: --cells 30000  --phase-secs 4  --idle-secs 4  --vsync on|off  --mesh (un Mesh pretesselado)  --topmost
#           --backend dx12|vulkan|gl  --adapter "Microsoft Basic"   (solo wgpu; "Microsoft Basic" = WARP; forzarlo en un equipo con GPU no sustituye la prueba de RDP/Hyper-V: ver la sección Pendiente de docs/fase-0-conclusiones.md)
# Batería completa:
bash scripts/run-ui.sh
```

Fases del `--bench`: calentamiento (1 s, descartada) → continua (repinta todo cada frame) → hover (puntero sintético que
recorre el mapa: hit-test + resaltado) → reposo (sin repintado; ahí se mide el RSS "en reposo"). **No toques el ratón ni
tapes la ventana** durante la medición (una ventana tapada deja de pintar).

### 2. `enum-bench` (enumeración de C:\\, solo lectura)

```powershell
target\release\enum-bench.exe --api extd|find|find-alloc|std --root 'C:\' --threads 32 --runs 3 --json out\x.json
target\release\enum-bench.exe --probe --json out\probe.json   # $MFT y FSCTL_GET_NTFS_VOLUME_DATA
bash scripts/run-enum.sh                                       # serie completa (3 rondas intercaladas + escalado de hilos)
```

`--api std` es una referencia mono-hilo con `std::fs` para validar recuentos en árboles pequeños
(p. ej. `--root 'C:\Windows\System32\DriverStore'`). La enumeración nunca escribe nada.

### 3. `trash-bench` (Papelera)

```powershell
target\release\trash-bench.exe run --n 100000 --loose-mode one --json out\trash.json
target\release\trash-bench.exe count      # nº de elementos de la Papelera y tiempo de list()
target\release\trash-bench.exe cleanup        # solo LISTA lo que haría (recuperación tras una ejecución muerta)
target\release\trash-bench.exe cleanup --yes  # purga y borra de verdad; solo con ninguna otra ejecución viva
```

> **AVISO: `trash-bench run` mete hasta ~100 000 elementos transitorios en la Papelera REAL del usuario durante
> mucho tiempo** (decenas de minutos con `--n 100000`; con `--n 1000` es rápido). Todos se purgan al final. No lo ejecutes si no quieres
> eso, y no interrumpas la ejecución con un `kill`: la guarda interna solo cubre pánicos y errores (para el resto está
> `cleanup`).

Trabaja en `%TEMP%\tesela-spike-<uuid>`. Solo restaura o purga elementos cuyo origen está **bajo `%TEMP%` y cuyo primer
componente tras `%TEMP%` empieza por `tesela-spike-<uuid>` completo** (en `cleanup`: por `tesela-spike-`), y al final
comprueba que el conjunto de ids de la Papelera es idéntico al de antes. Valida todos los argumentos antes de crear
nada. **Nunca vacía la Papelera.**
`--loose-mode` admite `one` (una llamada `delete_all` con todo), `batch=1000` (lotes) y `par=8` (hilos en paralelo).

### 4. `delete-bench` (borrado POSIX)

```powershell
target\release\delete-bench.exe functional   --json out\a.json   # flags, solo lectura
target\release\delete-bench.exe share-delete --json out\b.json   # §2.6: archivo abierto por otro handle (escribe 8x512 MiB en %TEMP%)
target\release\delete-bench.exe reparse      --json out\r.json   # borrar un junction/symlink de directorio: ¿se respeta el destino?
target\release\delete-bench.exe speed --files 50000 --runs 3 --threads 16 --dirs 100 --json out\c.json
```

### 5. `fonts-mica` (fuentes variables y Mica)

```powershell
target\release\fonts-mica.exe --mica on --shot-egui out\f-egui.png --shot-desktop out\f-desktop.png --out out\fm.json
# --mica off (control), --presentation visual|hwnd (DX12), SPIKE_BACKEND=dx12|vulkan|gl (solo wgpu)
```

La ventana es transparente, siempre encima y se coloca en (120,120): **no la tapes** mientras se hace la captura del
escritorio (GDI, `BitBlt` del escritorio componido, única forma de ver el backdrop que pinta DWM).

## Qué NO se puede medir aquí (pendiente para el usuario)

RDP y Hyper-V sin GPU, y Windows 10 22H2. Las instrucciones están en `docs/fase-0-conclusiones.md` (sección Pendiente).
