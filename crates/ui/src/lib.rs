//! Interfaz de Tesela (egui). SOLO LECTURA en este incremento: no borra ni mueve nada.
//!
//! * [`TeselaUi`]: la interfaz completa; pinta contra un `egui::Ui` (sin depender de `eframe`).
//! * Pantallas: [`home`] (inicio), [`scan`] (escaneo), [`explore`] (mapa, lista y detalles).
//! * [`map`]: el treemap como `Mesh` teselado una vez y cacheado.
//! * [`demo`]: árbol sintético y determinista para `tesela --demo`.

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
mod widgets;

pub use app::{
    BackendRef, CleanStart, DemoOptions, LangPref, MapStart, Options, Settings, StartScreen,
    TeselaUi,
};
pub use i18n::{Lang, Tr};
pub use theme::ThemePref;

#[cfg(test)]
mod tests;
