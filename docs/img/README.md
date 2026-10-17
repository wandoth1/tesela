# README screenshots

All the images in this folder come **only from Tesela's `--demo` mode**: a synthetic, deterministic tree, with
made-up folder names and fictional drives, without reading anything from the disk. They are screenshots of the **application window**
(the egui framebuffer, not the screen: nothing from the desktop gets in) saved as PNG without metadata with the `capture`
feature of `tesela-app`. The `--screenshot` option is only accepted together with `--demo`. (They use the Spanish interface, `--lang es`.)

`scripts/privacy-check.ps1` only admits here `docs/img/<name>.png` (lowercase, digits and hyphens) that are PNGs under
2 MiB with a closed list of chunks and lengths (no metadata), and requires that **this file** has, for each image,
a line with exactly this form:

    - <name>.png | sha256: <SHA-256 of its bytes> | <--demo command that generates it>

The hash goes with the image: if a single byte changes it has to be registered again (`sha256sum docs/img/<name>.png`).

## When regenerating a screenshot

Register the new one (hash and command) **and** add the SHA-256 of the previous one to `$legacyDemoImages` in `scripts/privacy-check.ps1`: in
range mode (`-To`, the one that checks the whole history when a tag is published) only this file's registration at the `To`
commit counts, and the old image is still in the public history; without its hash in that list, the privacy job of the tag push fails.
(`sha256sum` of the old image, before replacing it.)

## Registered images

- mapa-claro.png | sha256: 86c3498396bfe928dc2f44a94a495764dd42b9131b298f3b9e70f0fd84185455 | `cargo run -p tesela-app --features capture -- --demo --screen map --open Users/usuario --select Descargas --theme light --lang es --size 1280x800 --screenshot docs/img/mapa-claro.png --overwrite`
- mapa-oscuro.png | sha256: 0daf7d45071889963e0b5fa510cddd739d0df806bd890a4ac94789c349a5ec3d | `cargo run -p tesela-app --features capture -- --demo --screen map --theme dark --lang es --size 1280x800 --screenshot docs/img/mapa-oscuro.png --overwrite`
- seleccion.png | sha256: 7383f13f91acbf313fd07f4cb128ca046f6764942af8b4fbf730a9f8870ea357 | `cargo run -p tesela-app --features capture -- --demo --screen map --open Users/usuario --mark Descargas --mark Proyectos --theme light --lang es --size 1280x800 --screenshot docs/img/seleccion.png`
- recomendaciones.png | sha256: 7190f6cd883ca083b3d99fd728f9f5e74e087def35d7761f0116babd1de5c50c | `cargo run -p tesela-app --features capture -- --demo --screen recs --theme light --lang es --size 1280x800 --screenshot docs/img/recomendaciones.png`
- confirmacion.png | sha256: 9e22cfe21c7fa317fb7ce440398f21a700418c86dff752144b14b78c8c0871ad | `cargo run -p tesela-app --features capture -- --demo --screen confirm --theme light --lang es --size 1280x800 --screenshot docs/img/confirmacion.png`
- resumen.png | sha256: 2a1b8337900915992cb6aa928b4f13823e9202536b09452de0ca77a9eb6fdfc0 | `cargo run -p tesela-app --features capture -- --demo --screen summary --theme light --lang es --size 1280x800 --screenshot docs/img/resumen.png`
