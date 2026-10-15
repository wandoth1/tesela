//! Manifiesto de Windows (PLAN.md §3.9): `asInvoker` (nunca pide UAC al arrancar), rutas largas, DPI PerMonitorV2 y
//! Windows 10/11. SIN `activeCodePage UTF-8`: la app usa las APIs W y el valor por defecto de `embed-manifest` lo
//! activaría, así que se desactiva a propósito (`ActiveCodePage::System` no escribe el elemento).

use embed_manifest::manifest::{
    ActiveCodePage, DpiAwareness, ExecutionLevel, Setting, SupportedOS,
};
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = new_manifest("Tesela")
            .requested_execution_level(ExecutionLevel::AsInvoker)
            .long_path_aware(Setting::Enabled)
            .dpi_awareness(DpiAwareness::PerMonitorV2)
            .active_code_page(ActiveCodePage::System)
            .supported_os(SupportedOS::Windows10..=SupportedOS::Windows10);
        embed_manifest(manifest).expect("no se pudo incrustar el manifiesto");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
