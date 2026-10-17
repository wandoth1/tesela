//! Tesela interface (egui). Everything you delete or move goes through the `Backend` of `cleaner` (the real one or the SIMULATED one in
//! `--demo`, which never touches the disk) and only after confirmation.
//!
//! * [`TeselaUi`]: the complete interface; paints against a `egui::Ui` (without depending on `eframe`).
//! * Screens: [`home`] (home), [`scan`] (scan), [`explore`] (map, list and details).
//! * [`map`]: The treemap as `Mesh` tiled once and cached.
//! * [`demo`] and [`demo_backend`]: Synthetic, deterministic tree and mock cleanup support for `tesela --demo`.
//! * `clean`, `basket` and `recs`: the cleanup flow (basket, Recommendations, confirmation, execution and summary).

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
mod multisel;
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
