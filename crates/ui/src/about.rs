//! "About Tesela" window: version, license and third-party notices. The warnings (`THIRD-PARTY-NOTICES.md`, generated with
//! `cargo about`) go INSIDE the executable: so whoever receives only `tesela.exe` also has the licenses for what
//! includes, without depending on a file traveling next to it.

use crate::args;
use crate::i18n::Tr;
use crate::theme::Palette;
use egui::{FontId, Id, RichText};
use std::sync::OnceLock;

/// Full text of third party notices.
pub const NOTICES: &str = include_str!("../../../THIRD-PARTY-NOTICES.md");

/// Product version (that of the workspace).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Columns from which each line of the notices is divided to show them with rows of fixed height (and paint only those that are
/// see: the text has many thousands of lines).
const WRAP_COLUMNS: usize = 92;

/// The notices split into lines of at most `WRAP_COLUMNS` characters.
fn lines() -> &'static [String] {
    static LINES: OnceLock<Vec<String>> = OnceLock::new();
    LINES.get_or_init(|| wrap(NOTICES, WRAP_COLUMNS))
}

/// Part `text` in lines of at most `cols` characters, by words when possible (and dry if a word does not fit).
pub(crate) fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.chars().count() <= cols {
            out.push(line.to_owned());
            continue;
        }
        // The indentation of the original line is preserved.
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

/// Window status.
#[derive(Default)]
pub(crate) struct About {
    pub open: bool,
    notices: bool,
}

impl About {
    /// Escape closes the window. It's called BEFORE you paint the screens: with the window open, Escape is yours (if not, the
    /// exploration would waste it deselecting).
    pub fn handle_escape(&mut self, ctx: &egui::Context) {
        if self.open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.open = false;
            self.notices = false;
        }
    }

    /// Paint the window if it is open.
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
        assert!(NOTICES.contains("Third-party notices"));
        assert!(NOTICES.contains("MIT"));
        assert!(NOTICES.contains("Apache License"));
        // Direct product dependencies appear.
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
        // Same text without spaces or breaks.
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
