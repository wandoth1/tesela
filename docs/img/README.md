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

- mapa-claro.png | sha256: 53875e4cd8c00b57935cbf10a614a9a3f49430c663aefbeade01cf72ec10067c | `cargo run -p tesela-app --features capture -- --demo --screen map --open Users/usuario --select Descargas --theme light --lang es --size 1280x800 --screenshot docs/img/mapa-claro.png --overwrite`
- mapa-oscuro.png | sha256: b3a431728b860853484cb70a339f5398fb11165cb3cb46d0193262e96e7f9c66 | `cargo run -p tesela-app --features capture -- --demo --screen map --theme dark --lang es --size 1280x800 --screenshot docs/img/mapa-oscuro.png --overwrite`
- recomendaciones.png | sha256: 1392d5ff921dba0138300cb4755f2edb8333502ba95dfe3e719f44483e43dac1 | `cargo run -p tesela-app --features capture -- --demo --screen recs --theme light --lang es --size 1280x800 --screenshot docs/img/recomendaciones.png`
- confirmacion.png | sha256: 08db178df8af19d6c64480e6f7e0752f3885e9f92b1b3ce9f688dfdb1c6e7cc0 | `cargo run -p tesela-app --features capture -- --demo --screen confirm --theme light --lang es --size 1280x800 --screenshot docs/img/confirmacion.png`
- resumen.png | sha256: 4114a563bbbf12aa10d8c57cf1138f0df723efa259feea22a0b40a33796ad067 | `cargo run -p tesela-app --features capture -- --demo --screen summary --theme light --lang es --size 1280x800 --screenshot docs/img/resumen.png`
