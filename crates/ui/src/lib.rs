//! Interfaz de Tesela (egui). Todo lo que borra o mueve va por el `Backend` del `cleaner` (el real, o el SIMULADO de
//! `--demo`, que nunca toca el disco) y solo después de la confirmación.
//!
//! * [`TeselaUi`]: la interfaz completa; pinta contra un `egui::Ui` (sin depender de `eframe`).
//! * Pantallas: [`home`] (inicio), [`scan`] (escaneo), [`explore`] (mapa, lista y detalles).
//! * [`map`]: el treemap como `Mesh` teselado una vez y cacheado.
//! * [`demo`] y [`demo_backend`]: árbol sintético y determinista y respaldo de limpieza simulado para `tesela --demo`.
//! * `clean`, `basket` y `recs`: el flujo de limpieza (cesta, Recomendaciones, confirmación, ejecución y resumen).

mod about;
pub mod app;
mod basket;
mod clean;
pub mod demo;
pub mod demo_backend;
pub mod describe;
mod details;
pub mod error;
pub mod explore;
pub mod fonts;
mod home;
pub mod i18n;
pub mod icons;
mod list;
pub mod map;
pub mod platform;
mod recs;
mod scan;
pub mod theme;
mod treepath;
mod update;
mod widgets;

pub use app::{
    BackendRef, CleanStart, DemoOptions, LangPref, MapStart, Options, Settings, StartScreen,
    TeselaUi, TransportRef, UpdatePrefs,
};
pub use i18n::{Lang, Tr};
pub use theme::ThemePref;

#[cfg(test)]
mod testdir;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod update_tests;
