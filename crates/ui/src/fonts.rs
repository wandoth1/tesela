//! Fuentes del sistema cargadas en tiempo de ejecución (PLAN.md §2.10). No se redistribuye ninguna fuente de Windows.
//!
//! Orden de preferencia: `SegUIVar.ttf` (variable: egui aplica el eje `wght`; se cargan DOS pesos, 400 y 600, porque
//! cada peso copia los bytes y hay que cuidar la memoria), si no `segoeui.ttf` + `segoeuib.ttf`, y si no, las
//! fuentes de reserva de egui. Los símbolos que falten en la fuente elegida salen de las de egui (van detrás).
//! Para las rutas se usa Consolas si existe.

use crate::platform;
use egui::epaint::text::VariationCoords;
use egui::{Context, FontData, FontDefinitions, FontFamily, FontTweak};
use std::path::Path;
use std::sync::Arc;

/// Qué fuentes acabaron cargadas (para los tests y para el informe).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loaded {
    /// `SegUIVar.ttf`: pesos 400 y 600.
    SegoeVariable,
    /// `segoeui.ttf` y `segoeuib.ttf`.
    SegoeStatic,
    /// Solo las de egui.
    EguiDefault,
}

/// Construye las definiciones de fuentes a partir de la carpeta `dir` de fuentes del sistema (la inyecta el test).
pub fn definitions(dir: Option<&Path>) -> (FontDefinitions, Loaded) {
    let mut fd = FontDefinitions::default();
    let read = |name: &str| dir.and_then(|d| std::fs::read(d.join(name)).ok());
    let mut loaded = Loaded::EguiDefault;
    let mut semibold_set = false;

    if let Some(bytes) = read("SegUIVar.ttf") {
        for (key, w) in [("Tesela-regular", 400.0f32), ("Tesela-semibold", 600.0)] {
            let tweak = FontTweak {
                coords: VariationCoords::new([("wght", w)]),
                ..Default::default()
            };
            fd.font_data.insert(
                key.into(),
                Arc::new(FontData::from_owned(bytes.clone()).tweak(tweak)),
            );
        }
        loaded = Loaded::SegoeVariable;
    } else if let (Some(regular), Some(bold)) = (read("segoeui.ttf"), read("segoeuib.ttf")) {
        fd.font_data.insert(
            "Tesela-regular".into(),
            Arc::new(FontData::from_owned(regular)),
        );
        fd.font_data.insert(
            "Tesela-semibold".into(),
            Arc::new(FontData::from_owned(bold)),
        );
        loaded = Loaded::SegoeStatic;
    }

    if loaded != Loaded::EguiDefault {
        // Proporcional: la del sistema primero y detrás las de egui (símbolos, emoji).
        let fam = fd.families.entry(FontFamily::Proportional).or_default();
        fam.insert(0, "Tesela-regular".into());
        let mut semibold = vec!["Tesela-semibold".to_string()];
        semibold.extend(fam.iter().filter(|n| *n != "Tesela-regular").cloned());
        fd.families
            .insert(FontFamily::Name("semibold".into()), semibold);
        semibold_set = true;
    }
    if !semibold_set {
        let prop = fd
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default();
        fd.families
            .insert(FontFamily::Name("semibold".into()), prop);
    }
    if let Some(bytes) = read("consola.ttf") {
        fd.font_data
            .insert("Tesela-mono".into(), Arc::new(FontData::from_owned(bytes)));
        fd.families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "Tesela-mono".into());
    }
    (fd, loaded)
}

/// Carga las fuentes del sistema en `ctx` y devuelve cuáles.
pub fn install(ctx: &Context) -> Loaded {
    let dir = platform::fonts_dir();
    let (fd, loaded) = definitions(dir.as_deref());
    ctx.set_fonts(fd);
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_carpeta_de_fuentes_se_usan_las_de_egui_y_el_peso_semibold_existe() {
        let (fd, loaded) = definitions(None);
        assert_eq!(loaded, Loaded::EguiDefault);
        assert!(
            fd.families
                .contains_key(&FontFamily::Name("semibold".into()))
        );
        assert!(!fd.families[&FontFamily::Name("semibold".into())].is_empty());
        let (_, loaded) = definitions(Some(Path::new("no-existe-esta-carpeta")));
        assert_eq!(loaded, Loaded::EguiDefault);
    }

    #[test]
    fn con_las_fuentes_del_sistema_se_cargan_dos_pesos() {
        let dir = platform::fonts_dir().expect("carpeta de fuentes");
        let (fd, loaded) = definitions(Some(&dir));
        // En un Windows 10/11 normal existe SegUIVar (10 1709+) o, como mínimo, Segoe UI estática.
        assert_ne!(
            loaded,
            Loaded::EguiDefault,
            "ni SegUIVar.ttf ni segoeui.ttf"
        );
        assert_eq!(fd.families[&FontFamily::Proportional][0], "Tesela-regular");
        assert_eq!(
            fd.families[&FontFamily::Name("semibold".into())][0],
            "Tesela-semibold"
        );
        // Las fuentes de egui siguen detrás como reserva de símbolos.
        assert!(fd.families[&FontFamily::Proportional].len() > 1);
    }

    #[test]
    fn las_fuentes_instaladas_miden_texto_con_acentos() {
        let ctx = Context::default();
        let loaded = install(&ctx);
        let _ = loaded;
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let g = ui.painter().layout_no_wrap(
                "Vídeos · ≥ 12,3 GB · Descargas".into(),
                egui::FontId::proportional(14.0),
                egui::Color32::WHITE,
            );
            assert!(g.size().x > 50.0);
            let g2 = ui.painter().layout_no_wrap(
                "Vídeos · ≥ 12,3 GB · Descargas".into(),
                crate::theme::font_semibold(14.0),
                egui::Color32::WHITE,
            );
            assert!(g2.size().x > 50.0);
        });
        out.textures_delta.clear();
    }
}
