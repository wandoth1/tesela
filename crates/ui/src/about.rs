//! Ventana "Acerca de Tesela": versión, licencia y avisos de terceros. Los avisos (`THIRD-PARTY-NOTICES.md`, generado con
//! `cargo about`) van DENTRO del ejecutable: así quien reciba solo `tesela.exe` también tiene las licencias de lo que
//! incluye, sin depender de que un archivo viaje al lado.

use crate::args;
use crate::i18n::Tr;
use crate::theme::Palette;
use egui::{FontId, Id, RichText};
use std::sync::OnceLock;

/// Texto completo de los avisos de terceros.
pub const NOTICES: &str = include_str!("../../../THIRD-PARTY-NOTICES.md");

/// Versión del producto (la del espacio de trabajo).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Columnas a las que se parte cada línea de los avisos para mostrarlos con filas de altura fija (y pintar solo las que se
/// ven: el texto tiene muchos miles de líneas).
const WRAP_COLUMNS: usize = 92;

/// Los avisos partidos en líneas de como mucho `WRAP_COLUMNS` caracteres.
fn lines() -> &'static [String] {
    static LINES: OnceLock<Vec<String>> = OnceLock::new();
    LINES.get_or_init(|| wrap(NOTICES, WRAP_COLUMNS))
}

/// Parte `text` en líneas de como mucho `cols` caracteres, por palabras cuando se puede (y en seco si una palabra no cabe).
pub(crate) fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.chars().count() <= cols {
            out.push(line.to_owned());
            continue;
        }
        // Se conserva la sangría de la línea original.
        let indent: String = line.chars().take_while(|c| *c == ' ').collect();
        let mut cur = indent.clone();
        for word in line.split_whitespace() {
            let mut word = word;
            while word.chars().count() > cols.saturating_sub(indent.len()).max(1) {
                let take = cols.saturating_sub(indent.len()).max(1);
                let cut = word.char_indices().nth(take).map_or(word.len(), |(i, _)| i);
                if cur.trim().is_empty() {
                    cur.push_str(&word[..cut]);
                } else {
                    out.push(std::mem::replace(&mut cur, indent.clone()));
                    cur.push_str(&word[..cut]);
                }
                out.push(std::mem::replace(&mut cur, indent.clone()));
                word = &word[cut..];
            }
            let sep = usize::from(!cur.trim().is_empty());
            if cur.chars().count() + sep + word.chars().count() > cols && !cur.trim().is_empty() {
                out.push(std::mem::replace(&mut cur, indent.clone()));
            }
            if !cur.trim().is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        if !cur.trim().is_empty() {
            out.push(cur);
        }
    }
    out
}

/// Estado de la ventana.
#[derive(Default)]
pub(crate) struct About {
    pub open: bool,
    notices: bool,
}

impl About {
    /// Escape cierra la ventana. Se llama ANTES de pintar las pantallas: con la ventana abierta, Escape es suyo (si no, la
    /// exploración lo gastaría quitando la selección).
    pub fn handle_escape(&mut self, ctx: &egui::Context) {
        if self.open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.open = false;
            self.notices = false;
        }
    }

    /// Pinta la ventana si está abierta.
    pub fn show(&mut self, ctx: &egui::Context, tr: &Tr, pal: &Palette) {
        if !self.open {
            return;
        }
        let mut open = true;
        let mut show_notices = self.notices;
        egui::Window::new(tr.t("about"))
            .id(Id::new("tesela-about"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(700.0)
            .show(ctx, |ui| {
                ui.label(RichText::new(tr.t("app-name")).size(22.0).color(pal.text));
                ui.label(tr.ta("about-version", args!["v" => VERSION]));
                ui.add_space(6.0);
                ui.label(tr.t("about-description"));
                ui.label(RichText::new(tr.t("about-license")).color(pal.text_muted));
                ui.add_space(8.0);
                let label = tr.t("about-notices");
                if ui.selectable_label(show_notices, label).clicked() {
                    show_notices = !show_notices;
                }
                if show_notices {
                    ui.add_space(4.0);
                    let font = FontId::monospace(11.0);
                    let row_h = ui.fonts_mut(|f| f.row_height(&font));
                    egui::ScrollArea::vertical()
                        .id_salt("tesela-about-notices")
                        .max_height(380.0)
                        .auto_shrink([false, true])
                        .show_rows(ui, row_h, lines().len(), |ui, range| {
                            for line in &lines()[range] {
                                ui.label(
                                    RichText::new(line.as_str())
                                        .font(font.clone())
                                        .color(pal.text),
                                );
                            }
                        });
                }
            });
        self.notices = show_notices;
        if !open {
            self.open = false;
            self.notices = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn los_avisos_estan_incrustados_y_listan_las_dependencias_y_sus_licencias() {
        assert!(NOTICES.contains("Avisos de terceros"));
        assert!(NOTICES.contains("MIT"));
        assert!(NOTICES.contains("Apache License"));
        // Las dependencias directas del producto aparecen.
        for c in [
            "egui ",
            "eframe ",
            "wgpu ",
            "fluent-bundle ",
            "rfd ",
            "windows-sys ",
        ] {
            assert!(NOTICES.contains(&format!("- {c}")), "falta {c}");
        }
        assert!(NOTICES.contains("Fluent UI System Icons"));
    }

    #[test]
    fn el_ajuste_de_lineas_no_pierde_texto_ni_pasa_de_las_columnas() {
        let long = format!("  {}", "palabra ".repeat(40));
        let text = format!("corta\n{long}\n{}\nfin", "x".repeat(250));
        let w = wrap(&text, 40);
        assert!(w.iter().all(|l| l.chars().count() <= 40), "{w:?}");
        // Mismo texto sin espacios ni saltos.
        let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        assert_eq!(squash(&w.join("\n")), squash(&text));
        assert_eq!(wrap("a\n\nb", 40), vec!["a", "", "b"]);
    }

    #[test]
    fn los_avisos_enteros_se_ajustan_y_la_version_es_la_del_producto() {
        let w = lines();
        assert!(w.len() > 100);
        assert!(w.iter().all(|l| l.chars().count() <= WRAP_COLUMNS));
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
