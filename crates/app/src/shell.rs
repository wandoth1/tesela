//! La ventana de `tesela.exe`: envuelve `TeselaUi` en `eframe`, guarda los ajustes y el tamaño de la ventana, y trae las
//! dos herramientas de desarrollo (`--bench-hover` y, con la feature `capture`, `--screenshot`).

use crate::config::Config;
use std::path::PathBuf;
use std::time::Instant;
use tesela_ui::TeselaUi;

/// Mide el coste de CPU por frame moviendo un puntero sintético sobre el mapa.
pub struct Bench {
    pub secs: f32,
    warm_start: Option<Instant>,
    start: Option<Instant>,
    samples_ms: Vec<f64>,
    frames: u64,
}

impl Bench {
    pub fn new(secs: f32) -> Bench {
        Bench {
            secs,
            warm_start: None,
            start: None,
            samples_ms: Vec::new(),
            frames: 0,
        }
    }

    /// Posición del puntero en el instante `t` (segundos): una curva de Lissajous que recorre la ventana.
    pub fn pointer_at(t: f32, w: f32, h: f32) -> (f32, f32) {
        let (cx, cy) = (w / 2.0, h / 2.0);
        (
            cx + (w * 0.36) * (t * 1.3).sin(),
            cy + (h * 0.36) * (t * 1.9 + 0.7).sin(),
        )
    }

    fn percentile(sorted: &[f64], p: f64) -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    /// Resumen de las muestras: (n, p50, p95, p99, máximo) en milisegundos.
    pub fn summary(samples: &[f64]) -> (usize, f64, f64, f64, f64) {
        let mut s = samples.to_vec();
        s.sort_by(|a, b| a.total_cmp(b));
        (
            s.len(),
            Bench::percentile(&s, 0.50),
            Bench::percentile(&s, 0.95),
            Bench::percentile(&s, 0.99),
            s.last().copied().unwrap_or(0.0),
        )
    }
}

pub struct Shell {
    pub ui: TeselaUi,
    pub config: Config,
    pub config_path: Option<PathBuf>,
    pub persist: bool,
    pub persist_size: bool,
    pub last_size: Option<(f32, f32)>,
    pub last_title: String,
    pub bench: Option<Bench>,
    #[cfg_attr(not(feature = "capture"), allow(dead_code))]
    pub screenshot: Option<PathBuf>,
    #[cfg_attr(not(feature = "capture"), allow(dead_code))]
    pub shot_frames: u32,
    #[cfg_attr(not(feature = "capture"), allow(dead_code))]
    pub shot_requested: bool,
    pub normalize_scale: Option<(f32, f32)>,
    pub norm_frames: u32,
    /// Con `--scan`: la captura espera a que el escaneo termine y aparezca el mapa.
    #[cfg_attr(not(feature = "capture"), allow(dead_code))]
    pub wait_explore: bool,
}

impl Shell {
    fn save_config(&mut self) {
        if !self.persist {
            return;
        }
        let s = self.ui.settings();
        self.config.theme = Some(s.theme);
        self.config.language = Some(s.language);
        if self.persist_size
            && let Some((w, h)) = self.last_size
        {
            self.config.window_width = Some(w);
            self.config.window_height = Some(h);
        }
        if let Some(p) = &self.config_path
            && let Err(e) = self.config.save(p)
        {
            crate::console::print(&format!("tesela: no se pudo guardar la configuración: {e}"));
        }
    }

    fn finish_bench(&mut self, ctx: &eframe::egui::Context) {
        if let Some(b) = self.bench.take() {
            let (n, p50, p95, p99, max) = Bench::summary(&b.samples_ms);
            let stats = self.ui.map_stats();
            crate::console::print(&format!(
                "bench-hover: {n} frames en {:.0} s; CPU por frame: p50 {p50:.2} ms, p95 {p95:.2} ms, p99 {p99:.2} ms, max {max:.2} ms; celdas {}, vértices {}",
                b.secs,
                stats.map_or(0, |s| s.cells),
                stats.map_or(0, |s| s.vertices),
            ));
        }
        ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
    }
}

impl eframe::App for Shell {
    fn raw_input_hook(&mut self, ctx: &eframe::egui::Context, raw: &mut eframe::egui::RawInput) {
        // Puntero sintético durante la medición, una vez que el mapa está en pantalla.
        if let Some(b) = &mut self.bench
            && b.start.is_some()
        {
            let t = b.start.unwrap().elapsed().as_secs_f32();
            let r = ctx.content_rect();
            let (x, y) = Bench::pointer_at(t, r.width(), r.height());
            raw.events
                .push(eframe::egui::Event::PointerMoved(eframe::egui::pos2(x, y)));
        }
    }

    fn ui(&mut self, ui: &mut eframe::egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // En las capturas se normaliza la escala a 1 punto = 1 píxel, para que salgan iguales en cualquier pantalla.
        if let Some((w, h)) = self.normalize_scale {
            self.norm_frames += 1;
            if self.norm_frames == 1 {
                let native = ctx.native_pixels_per_point().unwrap_or(1.0);
                ctx.set_zoom_factor(1.0 / native);
                ctx.request_repaint();
            } else if self.norm_frames == 4 {
                // Con 1 punto = 1 píxel, el tamaño en puntos es el tamaño en píxeles.
                ctx.send_viewport_cmd(eframe::egui::ViewportCommand::InnerSize(
                    eframe::egui::vec2(w, h),
                ));
                self.normalize_scale = None;
            } else {
                ctx.request_repaint();
            }
        }
        self.ui.show(ui);

        // Título según la pantalla.
        let title = self.ui.window_title();
        if title != self.last_title {
            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        if let Some(s) = ctx
            .input(|i| i.viewport().inner_rect)
            .map(|r| (r.width(), r.height()))
        {
            self.last_size = Some(s);
        }
        if self.ui.take_settings_change().is_some() {
            self.save_config();
        }

        // Medición de CPU por frame con el puntero sintético.
        let mut done = false;
        let showing = self.ui.screen_name() == "explore";
        if let Some(b) = &mut self.bench {
            ctx.request_repaint();
            if b.start.is_none() && showing {
                // Calentamiento: el primer segundo y medio no cuenta (se construyen el layout y el mesh).
                let w = *b.warm_start.get_or_insert_with(Instant::now);
                if w.elapsed().as_secs_f32() > 1.5 {
                    b.start = Some(Instant::now());
                }
            }
            if let Some(s) = b.start {
                if let Some(cpu) = frame.info().cpu_usage {
                    b.samples_ms.push(cpu as f64 * 1000.0);
                    b.frames += 1;
                }
                if s.elapsed().as_secs_f32() >= b.secs {
                    done = true;
                }
            }
        }
        if done {
            self.finish_bench(&ctx);
        }

        #[cfg(feature = "capture")]
        self.capture(&ctx);
    }

    fn on_exit(&mut self) {
        self.save_config();
    }
}

#[cfg(feature = "capture")]
impl Shell {
    /// `--screenshot`: tras unos frames estables pide la captura de la VENTANA (no de la pantalla), la guarda como PNG
    /// sin metadatos y cierra.
    fn capture(&mut self, ctx: &eframe::egui::Context) {
        let Some(path) = self.screenshot.clone() else {
            return;
        };
        ctx.request_repaint();
        if self.wait_explore && self.ui.screen_name() != "explore" {
            self.shot_frames = 0;
            return;
        }
        self.shot_frames += 1;
        if !self.shot_requested && self.shot_frames > 50 {
            self.shot_requested = true;
            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Screenshot(
                eframe::egui::UserData::default(),
            ));
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                eframe::egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = image {
            let [w, h] = img.size;
            let mut rgba = Vec::with_capacity(w * h * 4);
            for p in &img.pixels {
                rgba.extend_from_slice(&p.to_array());
            }
            let file = std::fs::File::create(&path).expect("no se pudo crear la captura");
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            // Sin fragmentos de texto: ni software, ni autor, ni fecha.
            enc.write_header()
                .and_then(|mut wr| wr.write_image_data(&rgba))
                .expect("no se pudo escribir la captura");
            crate::console::print(&format!("captura guardada ({w}x{h})"));
            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_puntero_sintetico_se_queda_dentro_de_la_ventana() {
        for i in 0..2000 {
            let (x, y) = Bench::pointer_at(i as f32 * 0.05, 1280.0, 800.0);
            assert!(
                (0.0..=1280.0).contains(&x) && (0.0..=800.0).contains(&y),
                "{x},{y}"
            );
        }
        // Recorre una buena parte del área.
        let xs: Vec<f32> = (0..2000)
            .map(|i| Bench::pointer_at(i as f32 * 0.05, 1000.0, 600.0).0)
            .collect();
        assert!(xs.iter().cloned().fold(f32::MAX, f32::min) < 250.0);
        assert!(xs.iter().cloned().fold(f32::MIN, f32::max) > 750.0);
    }

    #[test]
    fn percentiles() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        let (n, p50, p95, p99, max) = Bench::summary(&v);
        assert_eq!(n, 100);
        assert!((p50 - 50.0).abs() <= 1.0);
        assert!((p95 - 95.0).abs() <= 1.0);
        assert!((p99 - 99.0).abs() <= 1.0);
        assert_eq!(max, 100.0);
        assert_eq!(Bench::summary(&[]).0, 0);
    }
}
