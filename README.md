# Tesela

**Mira qué ocupa tu disco y libera espacio sin miedo.**
Analizador visual de espacio en disco para Windows, escrito en Rust: rápido, ligero y claro.

> 🚧 **Estado: Fase 0 (spike de medición) completada; MVP pendiente.** Conclusiones en [docs/fase-0-conclusiones.md](docs/fase-0-conclusiones.md); el plan completo está en [docs/PLAN.md](docs/PLAN.md).

## Qué será

- **Mapa del disco (treemap) + lista.** Cada carpeta es un bloque proporcional a lo que ocupa, coloreado por tipo de archivo, antigüedad o riesgo.
- **Respuesta en segundos.** Mientras analiza ya te dice "puedes liberar al menos X GB", porque empieza por los sitios típicos de basura (temporales, cachés, Papelera).
- **Asistente de limpieza con riesgo visible.** Cada sugerencia va marcada como 🟢 Seguro, 🟠 Revisar o 🔺 Solo informar, y explica en una frase por qué.
- **Nada se borra a ciegas.** Todo pasa por una cesta de revisión. Lo regenerable (temporales, cachés) se borra; tus archivos van a la Papelera. Las carpetas del sistema están protegidas.
- **Honesto con los números.** Solo cuenta como liberado lo que realmente se ha liberado, y enseña qué parte del disco no ha podido medir.
- **Ligero.** Binario único, sin Electron ni WebView, sin telemetría ni cuentas. Funciona sin permisos de administrador y tiene un modo rápido opcional que lee directamente la tabla de archivos (MFT) de NTFS.

## Hoja de ruta

| Versión | Contenido |
|---|---|
| Fase 0 ✅ | Pruebas de medición: interfaz gráfica, API de lectura de carpetas, Papelera (hecha: [conclusiones](docs/fase-0-conclusiones.md)) |
| v0.1 | Escaneo, mapa + lista, cesta, limpieza de temporales, cachés y Papelera. Entregable: **instalador MSI por usuario** desde la v0.1 |
| v0.2 | Asistente completo (carpetas de proyectos, descargas antiguas, archivos grandes), deshacer |
| v0.3 | Modo rápido leyendo la MFT (administrador) |
| v1.0 | Duplicados, reescaneo incremental, instalador firmado, winget, ARM64 |

## Tecnología

Rust · [egui/eframe](https://github.com/emilk/egui) · [windows-rs](https://github.com/microsoft/windows-rs) · Windows 10 22H2 y Windows 11.

---

**English:** Tesela is a visual disk space analyzer and cleaner for Windows, written in Rust. Phase 0 (measurement spike) is done and the MVP is pending; see [docs/PLAN.md](docs/PLAN.md) and [docs/fase-0-conclusiones.md](docs/fase-0-conclusiones.md) (in Spanish).

## Desarrollo

Workspace de Cargo en la raíz (`crates/`); `spike/` es el código de medición de la fase 0, con su propio workspace.
Estado del MVP v0.1: hecho el modelo (`tesela-model`) y el escáner de solo lectura (`tesela-scanner`) con su CLI
`tesela-scan`; la interfaz, la cesta y la limpieza llegan en los siguientes incrementos.

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace            # los tests solo usan árboles sintéticos en %TEMP%	esela-test-<uuid>
cargo deny check
cargo run --release -p tesela-scan -- <ruta> --top 20   # solo lectura; opciones en la cabecera de crates/tesela-scan/src/main.rs
```

**Privacidad:** el repositorio es público y no puede contener datos de ninguna máquina (hardware, usuario, rutas, tamaños de
discos reales, resultados de escaneos). Tras clonar, instala el hook que lo comprueba antes de cada commit:
`pwsh -File scripts/install-hooks.ps1` (`scripts/privacy-check.ps1 -All` revisa todo el árbol; el CI lo ejecuta).

## Licencia

Licencia dual a tu elección:

- MIT ([LICENSE-MIT](LICENSE-MIT))
- Apache 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

Salvo que digas lo contrario, cualquier contribución que envíes para incluirla en el proyecto queda bajo esa misma licencia dual, sin términos adicionales.
