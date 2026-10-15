# Capturas del README

Todas las imágenes de esta carpeta salen **únicamente del modo `--demo`** de Tesela: un árbol sintético y determinista, con
carpetas de nombres inventados y unidades ficticias, sin leer nada del disco. Son capturas de la **ventana de la aplicación**
(el framebuffer de egui, no de la pantalla: no sale nada del escritorio) guardadas como PNG sin metadatos con la feature
`capture` de `tesela-app`.

`scripts/privacy-check.ps1` solo admite aquí `docs/img/<nombre>.png` (minúsculas, dígitos y guiones) que sean PNG sin
fragmentos de metadatos, de menos de 2 MiB, y exige que cada archivo tenga abajo una línea con su nombre y la orden `--demo`
que lo genera.

Para regenerarlas (desde la raíz del repositorio):

- mapa-claro.png: `cargo run -p tesela-app --features capture -- --demo --screen map --open Users/usuario --select Descargas --theme light --lang es --size 1280x800 --screenshot docs/img/mapa-claro.png`
- mapa-oscuro.png: `cargo run -p tesela-app --features capture -- --demo --screen map --theme dark --lang es --size 1280x800 --screenshot docs/img/mapa-oscuro.png`
