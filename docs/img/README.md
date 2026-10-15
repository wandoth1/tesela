# Capturas del README

Todas las imágenes de esta carpeta salen **únicamente del modo `--demo`** de Tesela: un árbol sintético y determinista, con
carpetas de nombres inventados y unidades ficticias, sin leer nada del disco. Son capturas de la **ventana de la aplicación**
(el framebuffer de egui, no de la pantalla: no sale nada del escritorio) guardadas como PNG sin metadatos con la feature
`capture` de `tesela-app`. La opción `--screenshot` solo se admite junto a `--demo`.

`scripts/privacy-check.ps1` solo admite aquí `docs/img/<nombre>.png` (minúsculas, dígitos y guiones) que sean PNG de menos de
2 MiB con una lista cerrada de fragmentos y longitudes (sin metadatos), y exige que **este archivo** tenga, para cada imagen,
una línea con exactamente esta forma:

    - <nombre>.png | sha256: <SHA-256 de sus bytes> | <orden con --demo que la genera>

El hash va con la imagen: si cambia un solo byte hay que volver a registrarla (`sha256sum docs/img/<nombre>.png`).

## Imágenes registradas

- mapa-claro.png | sha256: adf4969582fbb559236a8f910d9eb967e8460f655280a148c2860a45c8711938 | `cargo run -p tesela-app --features capture -- --demo --screen map --open Users/usuario --select Descargas --theme light --lang es --size 1280x800 --screenshot docs/img/mapa-claro.png`
- mapa-oscuro.png | sha256: a0609c142a2b8391cada1795b200991e9f854b24a5c9b156aed3fd1d747f43b0 | `cargo run -p tesela-app --features capture -- --demo --screen map --theme dark --lang es --size 1280x800 --screenshot docs/img/mapa-oscuro.png`
