# Fase 0: conclusiones del spike de medición

La fase 0 midió con binarios de prueba (`spike/`) las hipótesis técnicas del [plan](PLAN.md) antes de escribir el producto. Las decisiones ya están incorporadas en el plan (ronda 4 de §9). Este documento recoge **qué se comprobó y qué se concluyó**.

> **Privacidad.** Los datos brutos de las mediciones (JSON, CSV y capturas) **no se publican**, porque describen el equipo donde se tomaron. Aquí solo hay resultados a nivel de la aplicación (tamaño del ejecutable, memoria del proceso, coste por frame) y comparaciones relativas. Para reproducir las mediciones en tu equipo, ver [spike/README.md](../spike/README.md); los resultados se quedan en `spike/results/`, que está en `.gitignore`.

Todas las mediciones se hicieron en Windows, sin permisos de administrador, en un único equipo de pruebas. Son orientativas, no un benchmark.

## Resumen

| Tema | Resultado | Decisión |
|---|---|---|
| Backend de la UI | Exe: **glow 6,3 MB**, **wgpu solo DX12 10,5 MB**, wgpu con todos los backends 12,3 MB. Memoria del proceso (private working set) con un mapa de 30k celdas: glow ≈ 60–65 MiB, **wgpu DX12 ≈ 70 MiB**, wgpu Vulkan ≈ 110 MiB | **wgpu solo DX12** (WARP de respaldo para equipos sin GPU). Vulkan excluido. glow como plan B |
| Dibujar el treemap | Una `RectShape` por celda con 30k celdas: ~16 ms por frame, sin margen para 60 fps. **Un `Mesh` teselado una vez y reutilizado: ~1–2 ms por frame** (≈ 10× menos) | El treemap se dibuja como `Mesh` cacheado. Objetivo: p99 de CPU por frame < 8 ms |
| Layout squarified | 30k celdas en ~0,5 ms; construir el `Mesh`, ~0,4–0,8 ms | Suficiente para relayouts interactivos |
| Mica (fondo translúcido) | Funciona con glow; **no con wgpu** (DX12-HWND no admite transparencia; con DX12-Visual y Vulkan la ventana sale blanca) | Fondo sólido |
| Fuentes | `SegUIVar.ttf` se carga del sistema y egui aplica el eje `wght` (300–700); `opsz` no se aplica solo | Cargar las fuentes del sistema en tiempo de ejecución |
| Iconos | Fluent UI System Icons: licencia **MIT** | Se pueden embeber |
| API para leer carpetas | `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` va **igual o algo más rápida** que `FindFirstFileExW` y además da `AllocationSize` y `FileId`. `FindFirstFileExW` + abrir cada archivo para pedir el tamaño en disco es **~9× más lento** y falla en archivos del sistema | `FileIdExtdDirectoryInfo` como principal; `FindFirstFileExW` de respaldo, con tamaño en disco estimado |
| Paralelismo | La velocidad sube con los primeros hilos y después apenas mejora (varias veces más rápido que con 1 hilo) | Hilos ajustables midiendo |
| Tamaño en disco frente a tamaño lógico | Diferencia global despreciable; estimar redondeando al clúster se desvía de forma despreciable | Se muestra el tamaño en disco |
| Hard links | Sin deduplicar se cuenta de más: un porcentaje pequeño del volumen, pero una fracción considerable dentro de `C:\Windows`, donde los enlaces abundan. La ruta de respaldo `FindFirstFileExW` no da `FileId` y **no deduplica hard links**. Ordenar los FileId es despreciable frente al escaneo | Deduplicar siempre por FileId |
| Espacio no explicado | Lo enumerado sin administrador queda por debajo de `total − libre`, y **deduplicar los hard links AUMENTA el hueco no explicado** | Bloque "Windows y otros datos del sistema" |
| Metadatos NTFS | `$MFT` y compañía **no aparecen** al enumerar. `FSCTL_GET_NTFS_VOLUME_DATA` funciona sin administrador con un handle del directorio raíz | Se puede mostrar el tamaño de la MFT sin elevar |
| Papelera, 100k archivos | Mandar una carpeta de 100k archivos tarda **~2× el umbral de 10 s** del plan, es decir, 10 s alcanzan aproximadamente para la mitad de los 100k archivos (una sola medición). Archivo a archivo es inviable: decenas de veces más lento que mandar la carpeta entera | Carpetas de desarrollo: destino **diferido a la v0.2** (ver abajo) |
| Restaurar de la Papelera | `trash::os_limited::restore_all` restaura bien solo lo indicado. `list()` es lineal en el tamaño de **TODA la Papelera** (decenas de segundos con 100k elementos). `delete_all` puede devolver error habiendo movido casi todo | Deshacer verificado archivo a archivo; solo para lotes pequeños o carpetas enteras |
| Borrado POSIX (`FileDispositionInfoEx`) | Funciona, pero **no es más rápido** que `std::fs::remove_file`, que ya recurre a POSIX + `IGNORE_READONLY` con archivos de solo lectura | Descartado: se usa `remove_file` / `remove_dir` |
| Archivo abierto con `FILE_SHARE_DELETE` | Con **cualquier** método de borrado, el nombre desaparece pero **el espacio no se libera** hasta que el otro proceso cierra el archivo | Detectar el navegador abierto; contar solo lo realmente liberado |
| Borrar un junction | `remove_dir`, `remove_dir_all` y `FileDispositionInfoEx` con `FILE_FLAG_OPEN_REPARSE_POINT` borran **el enlace, no el destino** (destino intacto 4/4); `remove_file` y `DeleteFileW` fallan | Nunca se entra en un enlace |

## Carpetas de desarrollo (`node_modules`, `target`, `.venv`…)

La regla del plan decía: si mandar 100k archivos a la Papelera tarda más de 10 s, pasan a borrado definitivo. La medición lo superó, pero se tomó **una sola vez y con interferencias del sistema**. Además, el borrado definitivo "sale bien" aunque haya archivos abiertos (`FILE_SHARE_DELETE`), y eso es justo lo que pasa en un proyecto abierto en un editor.

Decisión: **diferido a la v0.2**, que es cuando estas carpetas entran en el producto. Allí se hará:
- repetir la medición ≥ 3 veces con un árbol realista;
- probar **"renombrar primero"**: renombrar la carpeta a un nombre lápida hermano falla si hay cualquier archivo abierto (cierra en seguro), y después se borra la carpeta renombrada. En una comprobación aparte, no versionada, renombrar falló con archivos abiertos con o sin `FILE_SHARE_DELETE`.

## Pendiente

Pruebas que no se pudieron hacer en el equipo de medición:
- **RDP y Hyper-V sin GPU.** Comprobar que la build wgpu DX12 arranca y dibuja (y glow como comparación). Forzar WARP en un equipo con GPU no sirve como sustituto.
  1. Compila las dos builds (ver `spike/README.md`). Copia `ui-treemap.exe` de `target-dx12` y de `target-glow` a la máquina remota y **renómbralos** a `ui-treemap-wgpu.exe` y `ui-treemap-glow.exe`, porque los dos se llaman igual.
  2. Ejecuta en cada entorno: una sesión RDP sin GPU, y la consola de una VM Hyper-V sin GPU, tanto en **sesión básica** como en **sesión mejorada**:
     ```powershell
     .\ui-treemap-wgpu.exe --bench --topmost --label rdp-wgpu --out rdp-wgpu.json
     .\ui-treemap-wgpu.exe --bench --topmost --mesh --label rdp-wgpu-mesh --out rdp-wgpu-mesh.json
     .\ui-treemap-glow.exe --bench --topmost --label rdp-glow --out rdp-glow.json
     .\ui-treemap-glow.exe --bench --topmost --mesh --label rdp-glow-mesh --out rdp-glow-mesh.json
     ```
  3. Anota, para cada ejecución: si arranca y dibuja o, si falla, el mensaje de error; el adaptador y el backend que figuran en el JSON; p50 y p99 de CPU por frame; fps; y la memoria en reposo (private working set).
  4. Criterio: si wgpu arranca y dibuja en todos los entornos, se queda wgpu. Si falla y glow funciona, se cambia a glow. Si **fallan los dos**, es un problema mayor y se decide aparte.
  5. Esos JSON contienen datos del equipo (adaptador, driver, build…): **no se suben al repositorio**; solo se anotan las conclusiones.
- **Windows 10 22H2:** `FileIdExtdDirectoryInfo` y `SegUIVar.ttf`.
- **Symlinks de directorio** al borrar. Crearlos necesita el Modo de desarrollador o permisos de administrador.
- `FOF_WANTNUKEWARNING` junto con `FOF_NO_UI`, y el límite real de la Papelera aplicado por Windows.
