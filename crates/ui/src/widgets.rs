//! Widgets propios con el estilo de la maqueta: botones con icono, chips, tarjetas y barras. Todos registran su
//! etiqueta accesible (AccessKit) y dan una pista cuando están deshabilitados.

use crate::app::Env;
use crate::icons::Icon;
use crate::theme::{self, Palette};
use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    Vec2, WidgetInfo, WidgetType, pos2, vec2,
};

/// Anillo de foco de teclado (2 px, color de acento, por dentro de `rect`): lo llevan todos los elementos que se
/// alcanzan con Tab.
pub fn focus_ring(painter: &egui::Painter, pal: &Palette, rect: Rect, radius: u8) {
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius),
        Stroke::new(2.0, pal.accent),
        StrokeKind::Inside,
    );
}

/// Aspecto de un botón.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonStyle {
    /// Relleno de acento: la acción principal de la pantalla.
    Primary,
    /// Borde y fondo de superficie.
    Secondary,
    /// Sin borde ni fondo hasta pasar el ratón.
    Ghost,
}

/// Color del texto y del icono de un botón: el normal si está habilitado y uno atenuado (menos contraste, pero legible)
/// si no.
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
        // Sobre el fondo gris de un principal deshabilitado ([`button_disabled_fill`]).
        theme::mix(pal.text_muted, pal.surface, 0.25)
    } else {
        theme::mix(pal.text_muted, pal.surface, 0.35)
    }
}

/// Fondo de un botón principal deshabilitado.
pub fn button_disabled_fill(pal: &Palette) -> Color32 {
    pal.track
}

/// Botón con icono opcional y texto. Devuelve la respuesta; si está deshabilitado no reacciona al clic.
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
    // El color del texto se decide al pintar (habilitado o no): el galley va con `PLACEHOLDER` y `galley(.., fg)` lo rellena.
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

/// Botón cuadrado de 36 px con solo un icono. `label` es el texto accesible y el de la pista.
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

/// Chip redondeado con icono opcional (riesgo, "Aproximado", tamaños de la cesta...).
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

/// Rótulo de sección en mayúsculas pequeñas (UNIDADES, VISTAS...).
pub fn section_label(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add_space(2.0);
    let t = egui::RichText::new(text.to_uppercase())
        .font(theme::font_semibold(11.0))
        .color(pal.text_muted);
    ui.label(t);
}

/// Indicador de "ocupado". Con animaciones gira (`egui::Spinner`, que además pide repintar sin parar); con "reducir
/// animaciones" (`animate == false`) es un arco ESTÁTICO y no pide ningún repintado.
pub fn busy(ui: &mut Ui, animate: bool, size: f32, color: Color32) -> Response {
    if animate {
        return ui.add(egui::Spinner::new().size(size).color(color));
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    if ui.is_rect_visible(rect) {
        let c = rect.center();
        let r = size * 0.5 - 2.0;
        // Tres cuartos de circunferencia (un "cargando" congelado).
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

/// Tramo del progreso indeterminado ("sin total conocido") dentro de la barra `bar`. Con animaciones va y viene según
/// `t` (segundos); sin ellas es un tramo FIJO en el centro (y quien llama no tiene que pedir repintado).
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

/// Barra de progreso redondeada. Con `hatch`, el relleno lleva rayas diagonales tenues ("aún contando").
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

/// Rayas diagonales (135°) dentro de `rect`: cada línea se recorta al rectángulo. Las usan "incompleto" y las barras
/// de progreso.
pub fn hatch_rect(painter: &egui::Painter, rect: Rect, color: Color32, spacing: f32, width: f32) {
    for s in hatch_segments(rect, spacing) {
        painter.line_segment([s.0, s.1], Stroke::new(width, color));
    }
}

/// Segmentos de las rayas diagonales `x - y = c` dentro de `rect`, separadas `spacing` px en horizontal.
pub fn hatch_segments(rect: Rect, spacing: f32) -> Vec<(Pos2, Pos2)> {
    let mut out = Vec::new();
    if rect.width() < 1.0 || rect.height() < 1.0 || spacing < 1.0 {
        return out;
    }
    // Línea x = y + c, con c desde -h hasta w.
    let (w, h) = (rect.width(), rect.height());
    let mut c = -h;
    while c < w {
        // Intersección con el rectángulo local [0,w] × [0,h].
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

/// Texto de una sola línea recortado con "…" al ancho `max_w`, pintado con su ancla. Devuelve el rectángulo usado.
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

    /// Color con que se PINTA de verdad el texto `text` en la salida de un pase: el de la sección del galley o, si es
    /// `PLACEHOLDER`, el de reserva del `TextShape`.
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
                    // Dos pases: el primero aplica las fuentes del sistema.
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
                                modal_open: false,
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
            // 45 grados: dx == dy.
            assert!(((b.x - a.x) - (b.y - a.y)).abs() < 1e-3);
        }
        assert!(hatch_segments(Rect::from_min_size(Pos2::ZERO, vec2(0.0, 5.0)), 8.0).is_empty());
    }
}
