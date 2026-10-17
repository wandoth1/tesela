# Architecture

Tesela is a Windows disk analyzer and cleaner written in Rust. The Cargo workspace is split into small crates so the data model, filesystem traversal, visualization geometry, deletion engine, interface, and executable remain independently testable.

## Crates

- `crates/model` owns the scanned tree, identifiers, flags, and shared data types. It does not perform filesystem access or draw the interface.
- `crates/scanner` reads Windows filesystem metadata and builds the tree. It handles traversal, reparse points, cloud placeholders, volume measurements, progress, and cancellation. It is read-only.
- `crates/layout` computes treemap geometry, visible child ordering, hit testing, and cached layouts from model data.
- `crates/cleaner` classifies cleanup candidates and implements approval, basket, logging, permanent deletion, and Recycle Bin operations. It is the only crate that performs cleanup operations.
- `crates/ui` contains the egui interface, interaction state, Fluent resources in `i18n/`, and presentation of scanner and cleaner results.
- `crates/app` connects the UI to the scanner, cleaner, window shell, settings, and command-line options. The `tesela.exe` executable is built here.
- `crates/tesela-scan` provides the command-line scanner and JSON output without the graphical interface.

## Supporting files

- `installer/` contains the WiX MSI definition and installer-specific documentation and configuration.
- `scripts/` contains Windows build, release, privacy, PE, MSI, and installer tooling. Script tests are under `scripts/tests/`.
- `docs/` contains product plans, security findings, and synthetic demo screenshots.
- `spike/` is a separate, excluded Cargo workspace for disposable experiments; it is not part of the product build.

The root `Cargo.toml` is the authoritative workspace member list. Keep filesystem mutation in `crates/cleaner`; the scanner and model must remain read-only with respect to user data.
