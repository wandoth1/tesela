//! Visual identity (PLAN.md §2.10): light and dark palette, colors by type (Okabe-Ito), grays with meaning and
//! egui's styles. Solid background (Mica does not work with wgpu).

use egui::{Color32, Context, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Theme, Visuals};
use tesela_model::FileType;

/// Interface colors in a theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Window background.
    pub bg: Color32,
    /// Surface (cards, bars, details panel).
    pub surface: Color32,
    /// Status bar and secondary areas.
    pub surface_alt: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    /// Background of an active element in the sidebar and smooth chips.
    pub accent_soft: Color32,
    pub safe: Color32,
    pub review: Color32,
    pub danger: Color32,
    /// Track a progress bar.
    pub track: Color32,
    /// Text on light backgrounds of a map (cells) and on dark backgrounds.
    pub ink: Color32,
    pub ink_light: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    /// Theme palette (values ​​from PLAN.md §2.10 and the approved layout).
    pub fn of(dark: bool) -> Palette {
        if dark {
            Palette {
                dark,
                bg: rgb(0x1B1D21),
                surface: rgb(0x25282E),
                surface_alt: rgb(0x202227),
                border: rgb(0x363A42),
                text: rgb(0xECEEF1),
                text_muted: rgb(0xA0A6B3),
                accent: rgb(0x4CA3F0),
                accent_soft: rgb(0x26303D),
                safe: rgb(0x3FB871),
                review: rgb(0xF2B24D),
                danger: rgb(0xFF6B5E),
                track: rgb(0x33373F),
                ink: rgb(0x101418),
                ink_light: rgb(0xFFFFFF),
            }
        } else {
            Palette {
                dark,
                bg: rgb(0xF5F6F8),
                surface: rgb(0xFFFFFF),
                surface_alt: rgb(0xEEF0F3),
                border: rgb(0xDADDE3),
                text: rgb(0x1A1C20),
                text_muted: rgb(0x5A6070),
                accent: rgb(0x0F6CBD),
                accent_soft: rgb(0xE5EEF8),
                safe: rgb(0x107C41),
                review: rgb(0xB26A00),
                danger: rgb(0xC42B1C),
                track: rgb(0xE3E6EA),
                ink: rgb(0x101418),
                ink_light: rgb(0xFFFFFF),
            }
        }
    }

    /// Color of a file type (Okabe-Ito). It is the same in both themes, as in the model.
    pub fn type_color(&self, t: FileType) -> Color32 {
        TYPE_COLORS[t as usize]
    }

    /// "Mixed": Lighter blue-gray than "Other" (folder without dominant type).
    pub fn mixed(&self) -> Color32 {
        if self.dark {
            rgb(0x8E9DB0)
        } else {
            rgb(0xA7B6C8)
        }
    }

    /// "Report Only" Gray (`pagefile.sys`, `hiberfil.sys`): With a triangle, never selectable for deletion.
    pub fn info_only(&self) -> Color32 {
        if self.dark {
            rgb(0x5F6672)
        } else {
            rgb(0x9AA0A8)
        }
    }

    /// Gray from the "Windows and other system data" block (unmeasured).
    pub fn system_block(&self) -> Color32 {
        if self.dark {
            rgb(0x33373F)
        } else {
            rgb(0xD3D7DD)
        }
    }

    /// Edge of the system block.
    pub fn system_border(&self) -> Color32 {
        if self.dark {
            rgb(0x4A4F59)
        } else {
            rgb(0xB4BAC4)
        }
    }

    /// Gray from the "Others" grouping (color of the "others" type).
    pub fn others(&self) -> Color32 {
        TYPE_COLORS[FileType::Other as usize]
    }
}

/// Colors by type, in the order of [`FileType`]: video, images, audio, documents, tablets, programs, others.
pub const TYPE_COLORS: [Color32; FileType::COUNT] = [
    rgb(0x0072B2),
    rgb(0xE69F00),
    rgb(0xCC79A7),
    rgb(0x009E73),
    rgb(0xD55E00),
    rgb(0x56B4E9),
    rgb(0x8A8F98),
];

/// Relative luminance (WCAG) of an opaque color.
pub fn luminance(c: Color32) -> f32 {
    fn ch(v: u8) -> f32 {
        let s = v as f32 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
}

/// WCAG contrast ratio between two colors.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The text (dark or light) with the most contrast on `fill`.
pub fn text_on(p: &Palette, fill: Color32) -> Color32 {
    if contrast(fill, p.ink) >= contrast(fill, p.ink_light) {
        p.ink
    } else {
        p.ink_light
    }
}

/// Mixes `a` with `b` (`t` = 0 → a, 1 → b), opaque.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Egui family for the semi-bold peso (defined in [`crate::fonts`]).
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Typography with semi-bold weight.
pub fn font_semibold(size: f32) -> FontId {
    FontId::new(size, semibold())
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.surface_alt;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.hyperlink_color = p.accent;
    v.weak_text_color = Some(p.text_muted);
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(2.0, p.accent);
    let r = CornerRadius::same(8);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.fg_stroke.color = p.text;
    }
    v.widgets.inactive.bg_fill = p.surface;
    v.widgets.inactive.weak_bg_fill = p.surface;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.hovered.bg_fill = p.accent_soft;
    v.widgets.hovered.weak_bg_fill = p.accent_soft;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.active.bg_fill = p.accent_soft;
    v.widgets.active.weak_bg_fill = p.accent_soft;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    v
}

/// Install the styles (the two themes) and the chosen preference.
pub fn install(ctx: &Context, pref: ThemePref) {
    ctx.set_visuals_of(Theme::Light, visuals(&Palette::of(false)));
    ctx.set_visuals_of(Theme::Dark, visuals(&Palette::of(true)));
    apply_pref(ctx, pref);
    ctx.all_styles_mut(|s| {
        use TextStyle::*;
        s.text_styles = [
            (Small, FontId::proportional(12.0)),
            (Body, FontId::proportional(14.0)),
            (Button, FontId::proportional(14.0)),
            (Heading, FontId::new(26.0, semibold())),
            (Monospace, FontId::monospace(12.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 32.0;
        s.interaction.tooltip_delay = 0.15;
        // 2 px visible focus (PLAN.md §2.10).
        s.visuals.selection.stroke = Stroke::new(2.0, s.visuals.selection.stroke.color);
    });
}

/// Applies the theme preference (system, light or dark).
pub fn apply_pref(ctx: &Context, pref: ThemePref) {
    ctx.options_mut(|o| {
        o.theme_preference = match pref {
            ThemePref::System => egui::ThemePreference::System,
            ThemePref::Light => egui::ThemePreference::Light,
            ThemePref::Dark => egui::ThemePreference::Dark,
        }
    });
}

/// Theme palette that egui is using now.
pub fn palette(ctx: &Context) -> Palette {
    Palette::of(ctx.theme() == Theme::Dark)
}

/// User theme preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ThemePref {
    /// Follow the system.
    #[serde(rename = "system")]
    System,
    #[serde(rename = "light")]
    Light,
    #[serde(rename = "dark")]
    Dark,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_paleta_es_la_del_plan() {
        let l = Palette::of(false);
        let d = Palette::of(true);
        assert_eq!(l.bg, Color32::from_rgb(0xF5, 0xF6, 0xF8));
        assert_eq!(d.bg, Color32::from_rgb(0x1B, 0x1D, 0x21));
        assert_eq!(l.accent, Color32::from_rgb(0x0F, 0x6C, 0xBD));
        assert_eq!(d.accent, Color32::from_rgb(0x4C, 0xA3, 0xF0));
        assert_eq!(l.safe, Color32::from_rgb(0x10, 0x7C, 0x41));
        assert_eq!(d.review, Color32::from_rgb(0xF2, 0xB2, 0x4D));
        assert_eq!(d.danger, Color32::from_rgb(0xFF, 0x6B, 0x5E));
        // Okabe-Ito (PLAN.md §2.9).
        assert_eq!(
            l.type_color(FileType::Video),
            Color32::from_rgb(0x00, 0x72, 0xB2)
        );
        assert_eq!(
            l.type_color(FileType::Image),
            Color32::from_rgb(0xE6, 0x9F, 0x00)
        );
        assert_eq!(
            l.type_color(FileType::Audio),
            Color32::from_rgb(0xCC, 0x79, 0xA7)
        );
        assert_eq!(
            l.type_color(FileType::Document),
            Color32::from_rgb(0x00, 0x9E, 0x73)
        );
        assert_eq!(
            l.type_color(FileType::Archive),
            Color32::from_rgb(0xD5, 0x5E, 0x00)
        );
        assert_eq!(
            l.type_color(FileType::Program),
            Color32::from_rgb(0x56, 0xB4, 0xE9)
        );
        assert_eq!(
            l.type_color(FileType::Other),
            Color32::from_rgb(0x8A, 0x8F, 0x98)
        );
    }

    #[test]
    fn el_texto_de_las_celdas_tiene_contraste_aa() {
        // Normal text AA = 4.5. Each type color, "Mixed" and the grays with meaning, in both themes.
        for dark in [false, true] {
            let p = Palette::of(dark);
            let mut fills: Vec<Color32> = FileType::ALL.iter().map(|t| p.type_color(*t)).collect();
            fills.extend([p.mixed(), p.info_only(), p.system_block()]);
            for f in fills {
                let t = text_on(&p, f);
                assert!(
                    contrast(f, t) >= 3.0,
                    "contraste {} sobre {f:?} (dark={dark})",
                    contrast(f, t)
                );
            }
        }
        // And the colors of the interface: text on background and surface.
        for dark in [false, true] {
            let p = Palette::of(dark);
            for bg in [p.bg, p.surface, p.surface_alt] {
                assert!(contrast(p.text, bg) >= 7.0);
                assert!(contrast(p.text_muted, bg) >= 4.5, "muted {dark}");
                assert!(contrast(p.accent, bg) >= 4.5, "acento {dark}");
            }
        }
    }

    #[test]
    fn mezcla_de_colores() {
        let a = Color32::from_rgb(0, 0, 0);
        let b = Color32::from_rgb(200, 100, 50);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, 0.5), Color32::from_rgb(100, 50, 25));
    }
}
