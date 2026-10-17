# Cambios

Formato basado en [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/). Las versiones siguen [SemVer](https://semver.org/lang/es/)
(`major.minor.patch`, sin sufijos: un MSI no admite `-rc1`). `release.yml` copia la sección de la versión de la etiqueta a las
notas de la GitHub Release, así que cada versión necesita su sección aquí.

## [0.1.4] - 2026-10-17

Cuarta ronda de correcciones, tras dos auditorías externas de la 0.1.3, y dos mejoras de uso pedidas en las pruebas.
**Actualiza si usas la limpieza.** Sigue **sin firmar**: Windows SmartScreen puede avisar.

### Añadido

- Botón **Inicio** (y `Alt+Inicio`) en la barra superior de la exploración: vuelve a la pantalla de elegir disco. Está
  desactivado mientras hay una limpieza en curso.
- Las unidades de la barra lateral se ven pulsables: se resaltan al pasar el ratón, cambian el cursor y explican que
  analizan esa unidad.

### Seguridad

- Una carpeta que se envía a la Papelera queda **vigilada** desde antes de medirla hasta el último momento: si algo
  cambia dentro (también si encoge), no se envía y vuelve a pedir confirmación. Solo se envía con el mismo contenido
  que aprobaste.
- El espacio disponible en la Papelera se lee antes y después de medir, y manda la lectura peor.
- Los enlaces y archivos de nube que hay dentro de una carpeta cuentan contra el espacio de la Papelera.
- Al aprobar, se rechaza lo que cuelga de un enlace (junction, symlink o volumen montado en una carpeta); las
  carpetas sincronizadas con la nube (OneDrive) siguen admitidas con su aviso.
- Vaciar la Papelera salta, con su motivo, las carpetas borradas que llevan enlaces dentro, y vacía el resto. Un
  elemento de la Papelera ilegible se salta y consta en el registro.
- Discos montados sin letra y rutas de volumen reconocidas en la protección del sistema y en el análisis.
- El análisis prioritario ya no sigue enlaces en ningún punto de la ruta ni entra en carpetas de nube.
- El modo demostración y las herramientas de medición ya no leen tu historial ni preparan una limpieza real.
- Endurecimiento extra contra DLL plantadas (la carpeta actual sale de la búsqueda).

### Corregido

- Tamaños correctos al eliminar dentro de una carpeta que había encogido; medidas del mapa que no se actualizaban.
- La herramienta `tesela-scan` acepta rutas con cualquier nombre y no culpa al sistema de espacio que no pudo leer.
- Instalador: el acceso directo tiene un identificador fijo y el validador del MSI comprueba más cosas.
- Herramientas del proyecto: la comprobación de privacidad cubre más codificaciones y también autor y committer; las
  pruebas que lanzan copias del programa solo corren en el CI.

## [0.1.3] - 2026-10-16

Tercera ronda de correcciones de la limpieza, tras una nueva auditoría externa de la 0.1.2. **Actualiza si usas la
limpieza.** El análisis del disco no cambia. Sigue **sin firmar**: Windows SmartScreen puede avisar.

### Seguridad

- Cada carpeta se envía a la Papelera **sola**, medida y comprobada contra el espacio disponible justo antes. Los
  archivos se siguen enviando juntos, pero Windows vuelve a comprobar el tamaño de cada uno en el último momento y, si
  creció, no se envía.
- Si Windows devuelve un error en mitad de un envío, el resto de ese lote ya no se procesa.
- El registro de una limpieza en curso ya no se puede borrar mientras está abierto (por ejemplo, desde otra limpieza
  simultánea).
- Cuando Tesela no puede comprobar qué pasó con una carpeta, lo dice así ("no se pudo comprobar") en lugar de darla por
  eliminada; y cuando una carpeta cambió mientras Windows la procesaba, dice que se eliminó o cambió, sin afirmar más
  de lo que sabe. En los dos casos el envío se detiene.

### Cambiado

- Una cesta con muchas carpetas tarda algo más en enviarse a la Papelera (cada carpeta va en su propia operación).

## [0.1.2] - 2026-10-16

Segunda ronda de correcciones tras dos auditorías externas de la 0.1.1 (una de la limpieza y otra del resto).
**Actualiza si usas la limpieza.** El análisis del disco no cambia en lo esencial. Sigue **sin firmar**: Windows
SmartScreen puede avisar.

### Seguridad

- Envío a la Papelera más estricto: cada archivo y carpeta se vuelve a medir justo antes de enviarlo, y se recalcula si
  cabe. Lo que creció desde la confirmación, o lo que no se pudo medir entero, no se envía.
- Si Windows falla después de haber borrado parte de algo, se cuenta como borrado y el envío se detiene. Una carpeta
  solo se da por intacta si su contenido coincide (huella de cada elemento, no solo el tamaño total).
- Vaciar la Papelera comprueba la identidad de cada elemento en el último momento. Un sustituto se veta y lo que venía
  detrás no se toca.
- **No limpia si se está ejecutando como administrador** (por ejemplo, abierta desde una consola elevada). Analizar y
  simular siguen funcionando.
- Endurecimiento contra DLL plantadas: el programa solo busca sus DLL en System32 y ya no intenta cargar
  `dxcompiler.dll`.
- El escáner ya no sigue un enlace (junction o symlink) elegido como carpeta raíz: lo rechaza con un mensaje claro.
- El registro local escribe cada resultado en el momento, y su poda ya no puede borrar el registro recién abierto.
- El modo demostración (`--demo`) y las herramientas de medición ya no leen ni guardan la configuración en tu perfil.

### Cambiado

- Textos que no prometen de más: "Hasta X en candidatos de bajo riesgo (se comprueba al limpiar)", "Liberar hasta X" y
  "Cambio medido del espacio libre" (otros programas también lo modifican).
- Los temporales se limpian si llevan **más de 7 días** sin modificarse (antes, 24 h), como hace Windows.
- La nota de `hiberfil.sys` explica que hace falta un símbolo del sistema como administrador, qué se pierde con
  `powercfg /h off` y la alternativa `powercfg /h /type reduced`.
- Una carpeta con partes ilegibles se rechaza al aprobarla, con el motivo.
- Nuevo lema: *"Mira qué ocupa tu disco y decide qué liberar, sabiendo qué se borra y comprobando lo que se libera."*
- Herramientas del proyecto: la comprobación de privacidad detecta más formas de ruta y valores partidos; el SHA-512 de
  WiX se comprueba antes de instalarlo; el validador del MSI y las pruebas son más estrictos.
- Nueva hoja de ruta de producto en `docs/ROADMAP.md`.

## [0.1.1] - 2026-10-16

Correcciones de seguridad de la limpieza tras una auditoría externa de la 0.1.0. **Actualiza si usas la limpieza**: el
análisis del disco (mapa, lista y detalles) no cambia. Sigue **sin firmar**: Windows SmartScreen puede avisar.

### Seguridad

- Un *junction* por encima de una carpeta de caché o de temporales ya no puede volver borrable una carpeta protegida. La
  ruta de cada categoría se comprueba pieza a pieza, sin aceptar ningún enlace, y cada excepción solo levanta su propia
  zona. Si una categoría se descarta por eso (por ejemplo, la carpeta del navegador movida a otro disco con un enlace),
  Recomendaciones la muestra como "no disponible" con el motivo.
- El envío a la Papelera falla cerrado: si no se sabe cuánto cabe, no se envía. Las carpetas se vuelven a medir justo
  antes de enviarlas, y si crecieron desde la confirmación no se envían.
- Justo antes de que Windows actúe se comprueba que cada elemento sigue siendo el aprobado; un sustituto con el mismo
  nombre no se envía. El envío se detiene en cuanto Windows elimina algo de forma definitiva, no al final del lote, y ya
  no arrastra archivos asociados (la carpeta `_files` de una página guardada).
- Si Windows dice que ha eliminado algo, Tesela lo comprueba sobre el propio objeto y su contenido: un borrado real,
  aunque sea parcial, nunca se anota como "no se tocó nada".
- Vaciar la Papelera comprueba la identidad de cada elemento antes de purgarlo y solo limpia metadatos huérfanos con
  pruebas completas.
- Un archivo al que se le añade o quita un enlace duro después de la confirmación se rechaza.

### Cambiado

- **Sin registro no hay limpieza**: si no se puede crear o escribir el registro local, la limpieza no empieza o se
  detiene. Antes de cada operación se anota qué se va a hacer, y vaciar la Papelera anota cada elemento.
- En un disco **sin tamaño máximo de Papelera configurado**, lo que iría a la Papelera pasa al grupo de borrado
  definitivo, que solo se ejecuta si lo marcas expresamente; la confirmación explica cómo fijar ese tamaño.
- Los elementos que cambiaron desde la confirmación salen como "cambió desde la confirmación" y no se tocan.
- Una simulación (`--dry-run`) ya no borra registros de limpiezas reales; cada clase de registro tiene su propio límite.
- `leak-scan` y `privacy-check` detectan también nombres con letras no ASCII (UTF-8, Latin-1 y UTF-16 sin BOM), y el
  validador del MSI comprueba el árbol de carpetas, las reglas de actualización y su orden exactos.
- Avisos de terceros regenerados: faltaba `getrandom` 0.4.

## [0.1.0] - 2026-10-16

Primera versión (MVP). **Sin firmar**: Windows SmartScreen puede avisar (ver el README, "Instalación").

### Añadido

- **Escáner** de solo lectura de unidades y carpetas (walker paralelo con `AllocationSize` real, hard links sin
  sobrecontar, carpetas de la nube sin descargar), y la herramienta de consola `tesela-scan`.
- **Interfaz**: mapa del disco (treemap) y lista sincronizados, detalles, pantalla de escaneo con progreso real y
  veredicto temprano, español e inglés, tema claro y oscuro, accesibilidad (DPI por monitor, foco visible, AccessKit en la
  lista) y modo `--demo` con datos sintéticos.
- **Limpieza segura**: Recomendaciones (temporales, cachés de navegadores, Descargas y Papelera), cesta de revisión,
  confirmación agrupada por destino y resumen con lo liberado de verdad. Borrado por handle sin seguir puntos de reparse,
  envío a la Papelera verificado por identidad, protección de carpetas del sistema, simulación (`--dry-run`) y registro
  de cada limpieza en tu equipo.
- **Instalador MSI por usuario** (WiX 5.0.2): sin administrador, en `%LOCALAPPDATA%\Programs\Tesela`, con acceso en el
  menú Inicio y entrada en Aplicaciones instaladas; una versión nueva sustituye a la anterior y una anterior queda
  bloqueada. Al desinstalar **no** se borran tu configuración ni tu historial. **Zip portable** y `SHA256SUMS.txt`.
- **Releases automáticas**: al empujar una etiqueta `vX.Y.Z` se construyen el MSI y el zip, se atesta su procedencia
  (`gh attestation verify`) y se crea una GitHub Release en **borrador**, que publica una persona tras revisarla.
- Icono de mosaico y datos de versión (producto, editor) en `tesela.exe` y `tesela-scan.exe`.

### Cambiado

- Las pruebas del `cleaner` que usan la Papelera real solo se ejecutan con `TESELA_REAL_RECYCLE_TESTS=1` (el CI de
  GitHub no tiene una Papelera de usuario fiable).
