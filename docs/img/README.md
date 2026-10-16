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

- mapa-claro.png | sha256: 86c3498396bfe928dc2f44a94a495764dd42b9131b298f3b9e70f0fd84185455 | `cargo run -p tesela-app --features capture -- --demo --screen map --open Users/usuario --select Descargas --theme light --lang es --size 1280x800 --screenshot docs/img/mapa-claro.png --overwrite`
- mapa-oscuro.png | sha256: 0daf7d45071889963e0b5fa510cddd739d0df806bd890a4ac94789c349a5ec3d | `cargo run -p tesela-app --features capture -- --demo --screen map --theme dark --lang es --size 1280x800 --screenshot docs/img/mapa-oscuro.png --overwrite`
- recomendaciones.png | sha256: 7190f6cd883ca083b3d99fd728f9f5e74e087def35d7761f0116babd1de5c50c | `cargo run -p tesela-app --features capture -- --demo --screen recs --theme light --lang es --size 1280x800 --screenshot docs/img/recomendaciones.png`
- confirmacion.png | sha256: 9e22cfe21c7fa317fb7ce440398f21a700418c86dff752144b14b78c8c0871ad | `cargo run -p tesela-app --features capture -- --demo --screen confirm --theme light --lang es --size 1280x800 --screenshot docs/img/confirmacion.png`
- resumen.png | sha256: 2a1b8337900915992cb6aa928b4f13823e9202536b09452de0ca77a9eb6fdfc0 | `cargo run -p tesela-app --features capture -- --demo --screen summary --theme light --lang es --size 1280x800 --screenshot docs/img/resumen.png`
