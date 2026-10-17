//! Own widgets in the style of the mockup: buttons with icons, chips, cards and bars. Everyone registers their
//! accessible tag (AccessKit) and give a hint when they are disabled.

use crate::app::Env;
use crate::icons::Icon;
use crate::theme::{self, Palette};
use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    Vec2, WidgetInfo, WidgetType, pos2, vec2,
};

/// Keyboard focus ring (2 px, accent color, inside `rect`): carried by all elements that are
/// are enough with Tab.
pub fn focus_ring(painter: &egui::Painter, pal: &Palette, rect: Rect, radius: u8) {
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius),
        Stroke::new(2.0, pal.accent),
        StrokeKind::Inside,
    );
}

/// Appearance of a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonStyle {
    /// Accent fill - the main action of the screen.
    Primary,
    /// Surface edge and background.
    Secondary,
    /// No border or background until mouseover.
    Ghost,
}

/// Color of the text and icon of a button: normal if enabled and grayed out (less contrast, but legible)
/// but.
pub fn button_fg(pal: &Palette, style: ButtonStyle, enabled: bool) -> Color32 {
    let normal = match style {
        ButtonStyle::Primary if pal.dark => pal.ink,
        ButtonStyle::Primary => Color32::WHITE,
        ButtonStyle::Secondary => pal.text,
        ButtonStyle::Ghost => pal.text_muted,
    };
    if enabled {
        normal
    } else if style == ButtonStyle::Primary {
        // On the gray background of a disabled main ([`button_disabled_fill`]).
        theme::mix(pal.text_muted, pal.surface, 0.25)
    } else {
        theme::mix(pal.text_muted, pal.surface, 0.35)
    }
}

/// Background of a disabled main button.
pub fn button_disabled_fill(pal: &Palette) -> Color32 {
    pal.track
}

/// Button with optional icon and text. Returns the response; If it is disabled it does not react to the click.
pub fn button(
    ui: &mut Ui,
    env: &mut Env,
    style: ButtonStyle,
    icon: Option<Icon>,
    text: &str,
    enabled: bool,
) -> Response {
    let pal = env.pal;
    let (bg, stroke) = match style {
        ButtonStyle::Primary => (pal.accent, Stroke::NONE),
        ButtonStyle::Secondary => (pal.surface, Stroke::new(1.0, pal.border)),
        ButtonStyle::Ghost => (Color32::TRANSPARENT, Stroke::NONE),
    };
    // The text color is decided when painting (enabled or not): the galley goes with `PLACEHOLDER` and `galley(.., fg)` fills it.
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        theme::font_semibold(14.0),
        Color32::PLACEHOLDER,
    );
    let icon_w = if icon.is_some() { 18.0 + 8.0 } else { 0.0 };
    let w = 12.0 * 2.0 + icon_w + galley.size().x;
    let h = if style == ButtonStyle::Primary {
        40.0
    } else {
        34.0
    };
    let (rect, resp) = ui.allocate_exact_size(
        vec2(w.max(40.0), h),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, text));
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let pressed = enabled && resp.is_pointer_button_down_on();
        let fill = match (style, hovered, pressed) {
            (ButtonStyle::Primary, _, true) => theme::mix(bg, pal.ink, 0.25),
            (ButtonStyle::Primary, true, _) => theme::mix(bg, pal.ink, 0.12),
            (ButtonStyle::Primary, _, _) => bg,
            (_, _, true) => pal.accent_soft,
            (ButtonStyle::Ghost, true, _) | (ButtonStyle::Secondary, true, _) => pal.accent_soft,
            _ => bg,
        };
        let fill = if enabled || style == ButtonStyle::Ghost {
            fill
        } else if style == ButtonStyle::Primary {
            button_disabled_fill(&pal)
        } else {
            pal.surface_alt
        };
        let fg = button_fg(&pal, style, enabled);
        ui.painter().rect(
            rect,
            CornerRadius::same(8),
            fill,
            stroke,
            StrokeKind::Inside,
        );
        let mut x = rect.left() + 12.0;
        if let Some(i) = icon {
            let r = Rect::from_min_size(pos2(x, rect.center().y - 9.0), Vec2::splat(18.0));
            env.icons.paint(ui.painter(), r, i, fg);
            x += icon_w;
        }
        ui.painter()
            .galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, fg);
        if resp.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(1.0),
                CornerRadius::same(9),
                Stroke::new(2.0, pal.accent),
                StrokeKind::Outside,
            );
        }
    }
    resp
}

/// 36 px square button with only one icon. `label` is the accessible text and the hint text.
pub fn icon_button(ui: &mut Ui, env: &mut Env, icon: Icon, label: &str, enabled: bool) -> Response {
    let pal = env.pal;
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::splat(36.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let fill = if enabled && resp.hovered() {
            pal.accent_soft
        } else {
            pal.surface
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(8),
            fill,
            Stroke::new(1.0, pal.border),
            StrokeKind::Inside,
        );
        let fg = if enabled {
            pal.text
        } else {
            theme::mix(pal.text_muted, pal.surface, 0.4)
        };
        env.icons.paint(
            ui.painter(),
            Rect::from_center_size(rect.center(), Vec2::splat(18.0)),
            icon,
            fg,
        );
        if resp.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(1.0),
                CornerRadius::same(9),
                Stroke::new(2.0, pal.accent),
                StrokeKind::Outside,
            );
        }
    }
    if enabled {
        resp.on_hover_text(label)
    } else {
        resp
    }
}

/// Rounded chip with optional icon (risk, "Approximate", basket sizes...).
pub fn chip(
    ui: &mut Ui,
    env: &mut Env,
    icon: Option<Icon>,
    text: &str,
    fg: Color32,
    bg: Color32,
) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), theme::font_semibold(12.0), fg);
    let icon_w = if icon.is_some() { 12.0 + 4.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 20.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, CornerRadius::same(12), bg);
        let mut x = rect.left() + 10.0;
        if let Some(i) = icon {
            env.icons.paint(
                ui.painter(),
                Rect::from_min_size(pos2(x, rect.center().y - 6.0), Vec2::splat(12.0)),
                i,
                fg,
            );
            x += icon_w;
        }
        ui.painter()
            .galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, fg);
    }
    resp
}

/// Section label in small capitals (UNITS, VIEWS...).
pub fn section_label(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add_space(2.0);
    let t = egui::RichText::new(text.to_uppercase())
        .font(theme::font_semibold(11.0))
        .color(pal.text_muted);
    ui.label(t);
}

/// "Busy" indicator. With rotating animations (`egui::Spinner`, which also asks to repaint without stopping); with "reduce"
/// animations" (`animate == false`) is a STATIC arc and does not ask for any repainting.
pub fn busy(ui: &mut Ui, animate: bool, size: f32, color: Color32) -> Response {
    if animate {
        return ui.add(egui::Spinner::new().size(size).color(color));
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    if ui.is_rect_visible(rect) {
        let c = rect.center();
        let r = size * 0.5 - 2.0;
        // Three-quarters of a circumference (a frozen "loading").
        let points: Vec<Pos2> = (0..=24)
            .map(|i| {
                let a =
                    -0.5 * std::f32::consts::PI + (i as f32 / 24.0) * 1.5 * std::f32::consts::PI;
                c + vec2(a.cos(), a.sin()) * r
            })
            .collect();
        ui.painter()
            .add(egui::Shape::line(points, Stroke::new(2.0, color)));
    }
    resp
}

/// Indeterminate progress section ("no known total") within the `bar` bar. With animations it comes and goes according to
/// `t` (seconds); Without them it is a FIXED section in the center (and whoever calls does not have to ask for repainting).
pub fn indeterminate_segment(bar: Rect, t: f32, animate: bool) -> Rect {
    let w = bar.width() * 0.25;
    let k = if animate {
        0.5 + 0.5 * (t * 2.0).sin()
    } else {
        0.5
    };
    let x = bar.left() + (bar.width() - w) * k;
    Rect::from_min_size(Pos2::new(x, bar.top()), vec2(w, bar.height()))
}

/// Rounded progress bar. With `hatch`, the padding carries faint ("still counting") diagonal stripes.
pub fn progress_bar(
    ui: &mut Ui,
    pal: &Palette,
    frac: f32,
    color: Color32,
    height: f32,
    hatch: bool,
    width: Option<f32>,
) -> Response {
    let w = width.unwrap_or_else(|| ui.available_width());
    let (rect, resp) = ui.allocate_exact_size(vec2(w, height), Sense::hover());
    if ui.is_rect_visible(rect) {
        let r = CornerRadius::same((height / 2.0).round().clamp(2.0, 255.0) as u8);
        ui.painter().rect_filled(rect, r, pal.track);
        let fw = (rect.width() * frac.clamp(0.0, 1.0)).max(0.0);
        if fw > 0.5 {
            let fill = Rect::from_min_size(rect.min, vec2(fw, rect.height()));
            ui.painter().rect_filled(fill, r, color);
            if hatch {
                hatch_rect(ui.painter(), fill, Color32::from_white_alpha(70), 8.0, 1.5);
            }
        }
    }
    resp
}

/// Diagonal stripes (135°) inside `rect`: each line is clipped to the rectangle. They use "incomplete" and bars
/// of progress.
pub fn hatch_rect(painter: &egui::Painter, rect: Rect, color: Color32, spacing: f32, width: f32) {
    for s in hatch_segments(rect, spacing) {
        painter.line_segment([s.0, s.1], Stroke::new(width, color));
    }
}

/// Segments of `x - y = c` diagonal stripes within `rect`, separated `spacing` px horizontally.
pub fn hatch_segments(rect: Rect, spacing: f32) -> Vec<(Pos2, Pos2)> {
    let mut out = Vec::new();
    if rect.width() < 1.0 || rect.height() < 1.0 || spacing < 1.0 {
        return out;
    }
    // Line x = y + c, with c from -h to w.
    let (w, h) = (rect.width(), rect.height());
    let mut c = -h;
    while c < w {
        // Intersection with the local rectangle [0,w] × [0,h].
        let (y0, y1) = ((-c).max(0.0), (w - c).min(h));
        if y1 > y0 {
            out.push((
                pos2(rect.left() + y0 + c, rect.top() + y0),
                pos2(rect.left() + y1 + c, rect.top() + y1),
            ));
        }
        c += spacing;
    }
    out
}

/// Single line text trimmed with "…" to width `max_w`, painted with its anchor. Returns the rectangle used.
pub fn clipped_text(
    painter: &egui::Painter,
    pos: Pos2,
    anchor: Align2,
    text: &str,
    font: FontId,
    color: Color32,
    max_w: f32,
) -> Rect {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(1.0));
    let galley = painter.layout_job(job);
    let rect = anchor.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, color);
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::app::{Action, Settings};
    use crate::i18n::{Lang, Tr};
    use crate::icons::IconCache;
    use egui::epaint::Shape;

    /// Color with which the text `text` is actually PAINTED at the output of a pass: that of the gallery section or, if
    /// `PLACEHOLDER`, the reserve for `TextShape`.
    fn painted_color(shapes: &[egui::epaint::ClippedShape], text: &str) -> Color32 {
        for cs in shapes {
            if let Shape::Text(ts) = &cs.shape
                && ts.galley.text() == text
            {
                let c = ts.galley.job.sections[0].format.color;
                return if c == Color32::PLACEHOLDER {
                    ts.override_text_color.unwrap_or(ts.fallback_color)
                } else {
                    c
                };
            }
        }
        panic!("no se pintó el texto {text:?}");
    }

    #[test]
    fn el_texto_de_un_boton_deshabilitado_se_pinta_atenuado() {
        let ctx = egui::Context::default();
        crate::fonts::install_now(&ctx, crate::fonts::Scripts::NONE);
        let tr = Tr::new(Lang::Es);
        for dark in [false, true] {
            let pal = Palette::of(dark);
            for style in [
                ButtonStyle::Primary,
                ButtonStyle::Secondary,
                ButtonStyle::Ghost,
            ] {
                let mut colors = Vec::new();
                for enabled in [true, false] {
                    let label = format!("boton-{style:?}-{enabled}");
                    let mut icons = IconCache::default();
                    let backend: std::sync::Arc<dyn tesela_cleaner::Backend> = std::sync::Arc::new(
                        crate::demo_backend::DemoBackend::new(std::time::Duration::ZERO),
                    );
                    let mut actions: Vec<Action> = Vec::new();
                    let mut painted = Color32::PLACEHOLDER;
                    // Two passes: the first applies system fonts.
                    for _ in 0..2 {
                        let mut out = ctx.run_ui(Default::default(), |ui| {
                            let mut env = Env {
                                tr: &tr,
                                pal,
                                icons: &mut icons,
                                demo: false,
                                animate: false,
                                drives: &[],
                                drives_loading: false,
                                settings: Settings::default(),
                                backend: &backend,
                                dry_run: false,
                                history: &[],
                                modal_open: false,
                                update: Default::default(),
                                actions: &mut actions,
                            };
                            button(ui, &mut env, style, None, &label, enabled);
                        });
                        if let Some(Shape::Text(_)) = out
                            .shapes
                            .iter()
                            .map(|s| &s.shape)
                            .find(|s| matches!(s, Shape::Text(_)))
                        {
                            painted = painted_color(&out.shapes, &label);
                        }
                        out.textures_delta.clear();
                    }
                    assert_eq!(
                        painted,
                        button_fg(&pal, style, enabled),
                        "{style:?} enabled={enabled} dark={dark}"
                    );
                    colors.push(painted);
                }
                assert_ne!(
                    colors[0], colors[1],
                    "{style:?} dark={dark}: el deshabilitado debe verse distinto"
                );
            }
        }
    }

    #[test]
    fn el_texto_deshabilitado_sigue_legible_pero_con_menos_contraste() {
        for dark in [false, true] {
            let pal = Palette::of(dark);
            for style in [
                ButtonStyle::Primary,
                ButtonStyle::Secondary,
                ButtonStyle::Ghost,
            ] {
                let (on, off) = (button_fg(&pal, style, true), button_fg(&pal, style, false));
                assert_ne!(on, off, "{style:?} dark={dark}");
                let bg = match style {
                    ButtonStyle::Primary => button_disabled_fill(&pal),
                    _ => pal.surface,
                };
                assert!(
                    theme::contrast(off, bg) >= 2.0,
                    "demasiado apagado: {style:?} dark={dark}: {}",
                    theme::contrast(off, bg)
                );
            }
        }
    }

    #[test]
    fn las_rayas_quedan_dentro_del_rectangulo() {
        let r = Rect::from_min_size(pos2(10.0, 20.0), vec2(60.0, 30.0));
        let segs = hatch_segments(r, 8.0);
        assert!(!segs.is_empty());
        for (a, b) in segs {
            for p in [a, b] {
                assert!(p.x >= r.left() - 1e-3 && p.x <= r.right() + 1e-3, "{p:?}");
                assert!(p.y >= r.top() - 1e-3 && p.y <= r.bottom() + 1e-3, "{p:?}");
            }
            // 45 degrees: dx == dy.
            assert!(((b.x - a.x) - (b.y - a.y)).abs() < 1e-3);
        }
        assert!(hatch_segments(Rect::from_min_size(Pos2::ZERO, vec2(0.0, 5.0)), 8.0).is_empty());
    }
}
