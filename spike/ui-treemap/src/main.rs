//! Experimento 1 (PLAN.md §6): treemap sintético de ~30k rectángulos en egui, con backend
//! `wgpu` o `glow` (feature de Cargo, exclusivas). Mide RSS, tamaño del exe y ms/frame.
//!
//! Compilar (uno u otro, nunca los dos a la vez en el mismo binario):
//!   cargo build --release -p ui-treemap --no-default-features --features wgpu
//!   CARGO_TARGET_DIR=target-glow cargo build --release -p ui-treemap --no-default-features --features glow
//!
//! Uso:
//!   ui-treemap                       modo interactivo (hover real, FPS en pantalla)
//!   ui-treemap --bench [opciones]    se cierra solo y escribe JSON con los resultados
//!
//! Opciones del modo --bench:
//!   --cells 30000        número OBJETIVO de rectángulos dibujados (se ajusta el nº de archivos hasta alcanzarlo)
//!   --n <N>              fija el nº de archivos y desactiva el ajuste anterior
//!   --phase-secs 4       duración de cada fase (continua / hover) en segundos
//!   --idle-secs 4        duración de la fase de reposo (sin repintado) en segundos
//!   --vsync on|off       (por defecto on). Con off se mide el coste real por frame sin esperar a la pantalla
//!   --adapter <texto>    (solo wgpu) elegir el adaptador cuyo nombre contenga <texto> (p. ej. "Microsoft Basic" = WARP)
//!   --backend dx12|vulkan|gl   (solo wgpu) restringe los backends
//!   --mesh               dibuja todo con UN Mesh pretesselado reutilizado (alternativa a 30k RectShape)
//!   --topmost            ventana siempre encima (evita oclusión durante el bench)
//!   --label <texto>      etiqueta libre que se copia al JSON
//!   --out <ruta.json>    JSON de resultados (por defecto out/ui-<backend>-<label>.json)
//!   --frames-csv <ruta>  vuelca cada frame (fase, delta_ms, cpu_ms, ui_ms)
//!
//! Fases del bench: warmup (1 s, descartada) → continuous (se pide repintado en cada frame, contenido
//! estático: re-tesela los 30k rectángulos cada frame) → hover (puntero sintético que recorre el treemap:
//! hit-test + resaltado + etiqueta) → idle (no se pide repintado; la app queda dormida) → cierre.
//! Las instantáneas de memoria se toman con GetProcessMemoryInfo (working set y private bytes).

#[cfg(not(any(feature = "wgpu", feature = "glow")))]
compile_error!("Activa exactamente una feature: wgpu o glow");

use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::json;
use std::time::{Duration, Instant};

#[cfg(feature = "wgpu")]
const BACKEND: &str = "wgpu";
#[cfg(not(feature = "wgpu"))]
const BACKEND: &str = "glow";

// ------------------------------------------------------------------------------------------------
// RNG determinista (xorshift64*) y datos sintéticos
// ------------------------------------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Normal estándar (Box-Muller).
    fn normal(&mut self) -> f64 {
        let (u1, u2) = (self.f64().max(1e-12), self.f64());
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Paleta Okabe-Ito de PLAN.md §2.9 (vídeo, imágenes, audio, documentos, comprimidos, programas, otros).
const PALETTE: [Color32; 7] = [
    Color32::from_rgb(0x00, 0x72, 0xB2),
    Color32::from_rgb(0xE6, 0x9F, 0x00),
    Color32::from_rgb(0xCC, 0x79, 0xA7),
    Color32::from_rgb(0x00, 0x9E, 0x73),
    Color32::from_rgb(0xD5, 0x5E, 0x00),
    Color32::from_rgb(0x56, 0xB4, 0xE9),
    Color32::from_rgb(0x8A, 0x8F, 0x98),
];

struct Folder {
    size: f64,
    files: Vec<(f64, u8)>, // (tamaño, índice de color), ordenado de mayor a menor
}

/// `n` archivos repartidos en ~n/100 carpetas con tamaños lognormales (cola pesada, como un disco real).
fn synth_tree(n: usize, seed: u64) -> Vec<Folder> {
    let mut rng = Rng(seed);
    let n_folders = (n / 100).max(1);
    let mut folders: Vec<Folder> = (0..n_folders).map(|_| Folder { size: 0.0, files: vec![] }).collect();
    let dominant: Vec<u8> = (0..n_folders).map(|_| (rng.next_u64() % 7) as u8).collect();
    for _ in 0..n {
        // Reparto sesgado: unas carpetas tienen muchísimos archivos y otras pocos.
        let f = ((n_folders as f64) * rng.f64().powf(1.6)) as usize;
        let f = f.min(n_folders - 1);
        let size = (rng.normal() * 1.8 + 11.0).exp(); // mediana ~60 KB, cola pesada
        let color = if rng.f64() < 0.65 { dominant[f] } else { (rng.next_u64() % 7) as u8 };
        folders[f].size += size;
        folders[f].files.push((size, color));
    }
    folders.retain(|f| !f.files.is_empty());
    for f in &mut folders {
        f.files.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    }
    folders.sort_by(|a, b| b.size.partial_cmp(&a.size).unwrap());
    folders
}

// ------------------------------------------------------------------------------------------------
// Squarified treemap (Bruls et al.). Entrada ordenada de mayor a menor → lineal.
// ------------------------------------------------------------------------------------------------

/// Devuelve un rectángulo `(x, y, w, h)` por cada valor de `values` (en el mismo orden).
fn squarify(values: &[f64], x: f64, y: f64, w: f64, h: f64, out: &mut Vec<[f64; 4]>) {
    let total: f64 = values.iter().sum();
    if values.is_empty() || total <= 0.0 || w <= 0.0 || h <= 0.0 {
        out.extend(values.iter().map(|_| [x, y, 0.0, 0.0]));
        return;
    }
    let scale = (w * h) / total;
    let (mut x, mut y, mut w, mut h) = (x, y, w, h);
    let mut i = 0;
    while i < values.len() {
        let short = w.min(h);
        // Crecemos la fila mientras mejore (o mantenga) la peor relación de aspecto.
        let mut j = i;
        let mut sum = 0.0;
        let mut worst_prev = f64::INFINITY;
        let (mut rmax, mut rmin) = (0.0f64, f64::INFINITY);
        while j < values.len() {
            let a = values[j] * scale;
            let s2 = sum + a;
            let (mx, mn) = (rmax.max(a), rmin.min(a));
            let worst = ((short * short * mx) / (s2 * s2)).max((s2 * s2) / (short * short * mn));
            if j > i && worst > worst_prev {
                break;
            }
            sum = s2;
            rmax = mx;
            rmin = mn;
            worst_prev = worst;
            j += 1;
        }
        // Colocamos la fila [i, j) a lo largo del lado corto.
        let thickness = sum / short;
        let mut pos = 0.0;
        for k in i..j {
            let len = values[k] * scale / thickness;
            if w >= h {
                // Fila vertical a la izquierda: apilada en y.
                out.push([x, y + pos, thickness, len]);
            } else {
                out.push([x + pos, y, len, thickness]);
            }
            pos += len;
        }
        if w >= h {
            x += thickness;
            w -= thickness;
        } else {
            y += thickness;
            h -= thickness;
        }
        i = j;
    }
}

struct Cell {
    rect: Rect,
    color: Color32,
    folder: u32,
    file: u32, // u32::MAX = es una carpeta
    size: f64,
}

/// Layout de dos niveles (carpetas → archivos) con separación de 2 px entre carpetas y 1 px entre archivos.
fn layout(folders: &[Folder], area: Rect) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(folders.iter().map(|f| f.files.len() + 1).sum());
    let sizes: Vec<f64> = folders.iter().map(|f| f.size).collect();
    let mut frects = Vec::with_capacity(folders.len());
    squarify(&sizes, area.min.x as f64, area.min.y as f64, area.width() as f64, area.height() as f64, &mut frects);
    let mut tmp = Vec::new();
    for (fi, (f, r)) in folders.iter().zip(&frects).enumerate() {
        let fr = Rect::from_min_size(Pos2::new(r[0] as f32, r[1] as f32), Vec2::new(r[2] as f32, r[3] as f32)).shrink(1.0);
        if fr.width() < 1.0 || fr.height() < 1.0 {
            continue;
        }
        cells.push(Cell { rect: fr, color: Color32::from_gray(0x2a), folder: fi as u32, file: u32::MAX, size: f.size });
        let inner = fr.shrink(1.0);
        if inner.width() < 1.0 || inner.height() < 1.0 {
            continue;
        }
        let vals: Vec<f64> = f.files.iter().map(|x| x.0).collect();
        tmp.clear();
        squarify(&vals, inner.min.x as f64, inner.min.y as f64, inner.width() as f64, inner.height() as f64, &mut tmp);
        for (k, (file, t)) in f.files.iter().zip(&tmp).enumerate() {
            let rect = Rect::from_min_size(Pos2::new(t[0] as f32, t[1] as f32), Vec2::new(t[2] as f32, t[3] as f32)).shrink(0.5);
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            cells.push(Cell { rect, color: PALETTE[file.1 as usize], folder: fi as u32, file: k as u32, size: file.0 });
        }
    }
    cells
}

// ------------------------------------------------------------------------------------------------
// Bench
// ------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Warmup,
    Continuous,
    Hover,
    Idle,
    Done,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Warmup => "warmup",
            Phase::Continuous => "continuous",
            Phase::Hover => "hover",
            Phase::Idle => "idle",
            Phase::Done => "done",
        }
    }
}

#[derive(Clone)]
struct Opts {
    bench: bool,
    n: Option<usize>,
    cells: usize,
    phase_secs: f64,
    idle_secs: f64,
    vsync: bool,
    #[cfg_attr(not(feature = "wgpu"), allow(dead_code))] // solo los usa la build wgpu
    adapter: Option<String>,
    #[cfg_attr(not(feature = "wgpu"), allow(dead_code))]
    backend: Option<String>,
    label: String,
    out: Option<String>,
    frames_csv: Option<String>,
    mesh: bool,
}

fn parse_opts() -> Opts {
    let a: Vec<String> = std::env::args().collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    Opts {
        bench: a.iter().any(|x| x == "--bench"),
        n: get("--n").and_then(|s| s.parse().ok()),
        cells: get("--cells").and_then(|s| s.parse().ok()).unwrap_or(30_000),
        phase_secs: get("--phase-secs").and_then(|s| s.parse().ok()).unwrap_or(4.0),
        idle_secs: get("--idle-secs").and_then(|s| s.parse().ok()).unwrap_or(4.0),
        vsync: get("--vsync").map(|s| s != "off").unwrap_or(true),
        adapter: get("--adapter"),
        backend: get("--backend"),
        label: get("--label").unwrap_or_else(|| "default".into()),
        out: get("--out"),
        frames_csv: get("--frames-csv"),
        mesh: a.iter().any(|x| x == "--mesh"),
    }
}

#[derive(Default)]
struct Samples {
    delta_ms: Vec<f64>, // intervalo entre llamadas consecutivas a ui() (incluye espera de vsync)
    cpu_ms: Vec<f64>,   // frame.info().cpu_usage: CPU de eframe en el frame anterior (ui + tesela + render, sin vsync)
    ui_ms: Vec<f64>,    // solo el cuerpo de nuestro ui(): layout-cache + generar shapes + hit-test
}

struct MemSnap {
    at: &'static str,
    ws: u64,
    peak_ws: u64,
    private: u64,
    pws: u64,
}

struct App {
    opts: Opts,
    folders: Vec<Folder>,
    n_files: usize,
    cells: Vec<Cell>,
    layout_area: Rect,
    gpu: serde_json::Value,
    // bench
    phase: Phase,
    phase_start: Instant,
    last_frame: Option<Instant>,
    frame_idx: u64,
    samples: std::collections::BTreeMap<&'static str, Samples>,
    csv: Vec<String>,
    mems: Vec<MemSnap>,
    idle_frames: u64,
    layout_ms: Vec<f64>,
    start: Instant,
    hover_hits: u64,
    finished: bool,
    last_logic_log: Option<Instant>,
    restores: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Modo --mesh: todos los rectángulos pretesselados en UN solo Mesh, reutilizado entre frames.
    mesh: Option<std::sync::Arc<egui::epaint::Mesh>>,
    mesh_build_ms: Vec<f64>,
    idle_done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    idle_snaps: std::sync::Arc<std::sync::Mutex<Vec<MemSnap>>>,
}

impl App {
    fn snap(&mut self, at: &'static str) {
        let m = spike_common::mem_info();
        self.mems.push(MemSnap { at, ws: m.working_set, peak_ws: m.peak_working_set, private: m.private_bytes, pws: m.private_working_set });
    }

    fn set_phase(&mut self, p: Phase) {
        match self.phase {
            Phase::Continuous => self.snap("fin_continuous"),
            Phase::Hover => self.snap("fin_hover"),
            _ => {}
        }
        if std::env::var_os("SPIKE_TRACE").is_some() {
            eprintln!("[trace] t={:.2}s fase {:?} -> {:?} (frame {})", self.start.elapsed().as_secs_f64(), self.phase, p, self.frame_idx);
        }
        self.phase = p;
        self.phase_start = Instant::now();
    }

    fn finish(&mut self, ctx: &egui::Context) {
        if self.finished {
            return;
        }
        self.finished = true;
        let exe = std::env::current_exe().ok();
        let exe_size = exe.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len());
        let mut phases = serde_json::Map::new();
        for (name, s) in &self.samples {
            let secs = match *name {
                "continuous" | "hover" => self.opts.phase_secs,
                "idle" => self.opts.idle_secs,
                _ => 1.0,
            };
            let d = spike_common::stats(&s.delta_ms);
            let c = spike_common::stats(&s.cpu_ms);
            let u = spike_common::stats(&s.ui_ms);
            let f = |x: spike_common::Stats| json!({"n": x.n, "mean_ms": x.mean, "p50_ms": x.p50, "p95_ms": x.p95, "p99_ms": x.p99, "max_ms": x.max});
            phases.insert(
                name.to_string(),
                json!({"frames": s.delta_ms.len(), "fps_medio": s.delta_ms.len() as f64 / secs,
                       "intervalo_entre_frames": f(d), "cpu_eframe_por_frame": f(c), "ui_propio": f(u)}),
            );
        }
        let mem: Vec<_> = self
            .mems
            .iter()
            .map(|m| json!({"momento": m.at, "working_set_mib": m.ws as f64 / 1048576.0, "private_bytes_mib": m.private as f64 / 1048576.0, "private_working_set_mib": m.pws as f64 / 1048576.0, "peak_working_set_mib": m.peak_ws as f64 / 1048576.0}))
            .collect();
        let lm = spike_common::stats(&self.layout_ms);
        let doc = json!({
            "backend": BACKEND,
            "label": self.opts.label,
            "vsync": self.opts.vsync,
            "modo_dibujo": if self.opts.mesh { "mesh unico pretesselado (sin esquinas redondeadas)" } else { "un RectShape por celda (esquinas redondeadas)" },
            "mesh_build_ms": spike_common::stats(&self.mesh_build_ms).p50,
            "gpu": self.gpu,
            "n_archivos": self.n_files,
            "n_celdas_dibujadas": self.cells.len(),
            "ventana_px": [self.layout_area.width(), self.layout_area.height()],
            "exe": exe.map(|p| p.display().to_string()),
            "exe_bytes": exe_size,
            "layout_squarified_ms": {"repeticiones": lm.n, "mean": lm.mean, "p50": lm.p50, "p95": lm.p95, "max": lm.max},
            "fases": phases,
            "memoria": mem,
            "frames_en_idle": self.idle_frames,
            "hover_hits_con_celda": self.hover_hits,
            "restauraciones_de_ventana_minimizada_por_el_entorno": self.restores.load(std::sync::atomic::Ordering::Relaxed),
            "duracion_total_s": self.start.elapsed().as_secs_f64(),
            "nota": "intervalo_entre_frames incluye la espera de vsync si vsync=true; cpu_eframe_por_frame NO la incluye",
        });
        let out = self.opts.out.clone().unwrap_or_else(|| format!("out/ui-{}-{}.json", BACKEND, self.opts.label));
        if let Some(parent) = std::path::Path::new(&out).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&out, serde_json::to_string_pretty(&doc).unwrap()).expect("no se pudo escribir el JSON");
        if let Some(p) = &self.opts.frames_csv {
            let mut s = String::from("fase,delta_ms,cpu_ms,ui_ms\n");
            for l in &self.csv {
                s.push_str(l);
                s.push('\n');
            }
            std::fs::write(p, s).expect("no se pudo escribir el CSV");
        }
        eprintln!("resultados en {out}");
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

fn fmt_size(b: f64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

impl eframe::App for App {
    /// Solo diagnóstico: eframe llama a `logic` incluso con la ventana oculta/ocluida (ver docs de `App::logic`).
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if std::env::var_os("SPIKE_TRACE").is_some() {
            let now = Instant::now();
            if self.last_logic_log.map(|l| (now - l).as_secs_f64() >= 1.0).unwrap_or(true) {
                self.last_logic_log = Some(now);
                let (occ, min, foc, vis) = ctx.input(|i| {
                    let v = i.viewport();
                    (v.occluded, v.minimized, v.focused, v.visible())
                });
                eprintln!("[trace] t={:.2}s logic(): occluded={occ:?} minimized={min:?} focused={foc:?} visible={vis:?} frame={}", self.start.elapsed().as_secs_f64(), self.frame_idx);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let t_frame = Instant::now();
        let ctx = ui.ctx().clone();
        let now = t_frame;
        let delta = self.last_frame.map(|l| (now - l).as_secs_f64() * 1000.0);
        self.last_frame = Some(now);
        self.frame_idx += 1;
        if std::env::var_os("SPIKE_TRACE").is_some() && (self.frame_idx <= 3 || self.frame_idx % 60 == 0) {
            eprintln!("[trace] t={:.2}s ui() frame {}", self.start.elapsed().as_secs_f64(), self.frame_idx);
        }
        let cpu = frame.info().cpu_usage.map(|c| c as f64 * 1000.0);
        // Guardián anti-minimizado (solo --bench): en las pruebas, algo ajeno a la app (el entorno que lanzaba las
        // mediciones) minimizaba la ventana a los pocos segundos y eframe deja de pintar con la ventana minimizada
        // (parecía un cuelgue de glow). Un hilo sondea IsIconic y restaura la ventana SIN pedir repintados a egui,
        // para no falsear la fase de reposo. Se cuenta cuántas veces ocurre y se informa en el JSON.
        if self.frame_idx == 1 && self.opts.bench && !std::env::args().any(|a| a == "--no-guard") {
            if let Ok(RawWindowHandle::Win32(h)) = frame.window_handle().map(|h| h.as_raw()) {
                let hwnd = h.hwnd.get();
                let counter = self.restores.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_millis(100));
                    unsafe {
                        use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, ShowWindow};
                        if IsIconic(hwnd as _) != 0 {
                            ShowWindow(hwnd as _, SW_RESTORE);
                            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                });
            }
        }
        // Diagnóstico de los cuelgues de glow observados en las pruebas: no se usan en las mediciones finales.
        if self.frame_idx == 1 && std::env::args().any(|a| a == "--diag-focus") {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if self.frame_idx == 1 && std::env::args().any(|a| a == "--diag-heartbeat") {
            let c = ctx.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(4));
                c.request_repaint();
            });
        }

        // ---- Máquina de fases (solo --bench) ----
        if self.opts.bench {
            let el = self.phase_start.elapsed().as_secs_f64();
            match self.phase {
                Phase::Warmup if el >= 1.0 => {
                    self.snap("tras_warmup");
                    self.set_phase(Phase::Continuous)
                }
                Phase::Continuous if el >= self.opts.phase_secs => self.set_phase(Phase::Hover),
                Phase::Hover if el >= self.opts.phase_secs => {
                    self.set_phase(Phase::Idle);
                    // Reposo: NO se piden repintados. Un hilo temporizador toma las instantáneas de memoria
                    // (GetProcessMemoryInfo vale desde cualquier hilo) y, al terminar, despierta a egui con un
                    // request_repaint desde fuera. Antes se usaba request_repaint_after, que en las pruebas llegaba
                    // con retrasos de varios segundos o no llegaba.
                    let idle = self.opts.idle_secs;
                    let c = ctx.clone();
                    let done = self.idle_done.clone();
                    let mids = self.idle_snaps.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs_f64(idle / 2.0));
                        let m = spike_common::mem_info();
                        mids.lock().unwrap().push(MemSnap { at: "idle_mitad", ws: m.working_set, peak_ws: m.peak_working_set, private: m.private_bytes, pws: m.private_working_set });
                        std::thread::sleep(Duration::from_secs_f64(idle / 2.0));
                        let m = spike_common::mem_info();
                        mids.lock().unwrap().push(MemSnap { at: "idle_final", ws: m.working_set, peak_ws: m.peak_working_set, private: m.private_bytes, pws: m.private_working_set });
                        done.store(true, std::sync::atomic::Ordering::SeqCst);
                        loop {
                            c.request_repaint();
                            std::thread::sleep(Duration::from_millis(500));
                        }
                    });
                }
                Phase::Idle => {
                    self.idle_frames += 1;
                    if self.idle_done.load(std::sync::atomic::Ordering::SeqCst) {
                        let snaps: Vec<MemSnap> = std::mem::take(&mut *self.idle_snaps.lock().unwrap());
                        self.mems.extend(snaps);
                        self.set_phase(Phase::Done);
                        self.finish(&ctx);
                    }
                }
                _ => {}
            }
            if self.phase == Phase::Done {
                return;
            }
        }

        // ---- Layout cacheado por tamaño de ventana ----
        let area = ui.max_rect();
        if self.cells.is_empty() || (area.size() - self.layout_area.size()).abs().max_elem() > 0.5 {
            let t = Instant::now();
            self.cells = layout(&self.folders, area);
            self.layout_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            self.layout_area = area;
            if self.opts.mesh {
                let t = Instant::now();
                let mut m = egui::epaint::Mesh::default();
                m.reserve_triangles(self.cells.len() * 2);
                m.reserve_vertices(self.cells.len() * 4);
                for c in &self.cells {
                    m.add_colored_rect(c.rect, c.color);
                }
                self.mesh = Some(std::sync::Arc::new(m));
                self.mesh_build_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }

        // ---- Puntero (real o sintético) ----
        let t_ui = Instant::now();
        let synthetic = self.opts.bench && self.phase == Phase::Hover;
        let pointer: Option<Pos2> = if synthetic {
            let t = self.frame_idx as f32 * 0.037;
            let c = area.center();
            Some(Pos2::new(
                c.x + (area.width() * 0.45) * (t * 1.31).sin(),
                c.y + (area.height() * 0.45) * (t * 0.87 + 0.5).cos(),
            ))
        } else if self.opts.bench {
            None
        } else {
            ui.input(|i| i.pointer.hover_pos())
        };

        // ---- Dibujo: todos los rectángulos, cada frame (modo inmediato) ----
        let (_resp, painter) = ui.allocate_painter(area.size(), egui::Sense::hover());
        painter.rect_filled(area, 0.0, Color32::from_rgb(0x1B, 0x1D, 0x21));
        if let Some(m) = &self.mesh {
            painter.add(egui::Shape::mesh(m.clone())); // Arc: no se copia el mesh en cada frame
        } else {
            for c in &self.cells {
                painter.rect_filled(c.rect, if c.file == u32::MAX { 3.0 } else { 1.5 }, c.color);
            }
        }
        // Hit-test lineal desde el final (los archivos se dibujan después que su carpeta).
        let mut hit: Option<&Cell> = None;
        if let Some(p) = pointer {
            hit = self.cells.iter().rev().find(|c| c.file != u32::MAX && c.rect.contains(p));
            if let Some(c) = hit {
                painter.rect_stroke(c.rect.expand(1.0), 1.5, egui::Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Outside);
                let txt = format!("carpeta_{}/archivo_{} · {}", c.folder, c.file, fmt_size(c.size));
                let galley = painter.layout_no_wrap(txt, egui::FontId::proportional(13.0), Color32::WHITE);
                let r = Rect::from_min_size(p + Vec2::new(14.0, 14.0), galley.size() + Vec2::splat(8.0));
                painter.rect_filled(r, 4.0, Color32::from_black_alpha(220));
                painter.galley(r.min + Vec2::splat(4.0), galley, Color32::WHITE);
            }
        }
        if synthetic && hit.is_some() {
            self.hover_hits += 1;
        }
        if !self.opts.bench {
            let s = format!(
                "{BACKEND} · {} celdas · frame {:.2} ms (CPU eframe {:.2} ms)",
                self.cells.len(),
                delta.unwrap_or(0.0),
                cpu.unwrap_or(0.0)
            );
            painter.text(area.left_top() + Vec2::new(8.0, 8.0), egui::Align2::LEFT_TOP, s, egui::FontId::monospace(13.0), Color32::WHITE);
        }
        let ui_ms = t_ui.elapsed().as_secs_f64() * 1000.0;

        // ---- Registro de muestras ----
        if self.opts.bench {
            if let (Some(d), Some(c)) = (delta, cpu) {
                let name = self.phase.name();
                {
                    let s = self.samples.entry(name).or_default();
                    s.delta_ms.push(d);
                    s.cpu_ms.push(c);
                    s.ui_ms.push(ui_ms);
                    if self.opts.frames_csv.is_some() {
                        self.csv.push(format!("{name},{d:.4},{c:.4},{ui_ms:.4}"));
                    }
                }
            }
            if self.frame_idx == 3 {
                self.snap("tras_3_frames");
            }
            // Mantener el repintado continuo salvo en reposo.
            if matches!(self.phase, Phase::Warmup | Phase::Continuous | Phase::Hover) {
                ctx.request_repaint();
            }
        } else {
            ctx.request_repaint(); // modo interactivo: animación continua para ver el FPS
        }
        let _ = t_frame;
    }
}

fn gpu_info(cc: &eframe::CreationContext<'_>) -> serde_json::Value {
    #[cfg(feature = "wgpu")]
    {
        if let Some(rs) = &cc.wgpu_render_state {
            let i = rs.adapter.get_info();
            let all: Vec<String> = rs
                .available_adapters
                .iter()
                .map(|a| {
                    let x = a.get_info();
                    format!("{} [{:?}, {:?}]", x.name, x.backend, x.device_type)
                })
                .collect();
            return json!({"adaptador": i.name, "backend_wgpu": format!("{:?}", i.backend), "tipo": format!("{:?}", i.device_type),
                          "driver": i.driver, "driver_info": i.driver_info, "adaptadores_disponibles": all,
                          "formato_superficie": format!("{:?}", rs.target_format)});
        }
        json!(null)
    }
    #[cfg(not(feature = "wgpu"))]
    {
        use eframe::glow::HasContext;
        if let Some(gl) = &cc.gl {
            unsafe {
                return json!({"renderer": gl.get_parameter_string(eframe::glow::RENDERER),
                              "version": gl.get_parameter_string(eframe::glow::VERSION),
                              "vendor": gl.get_parameter_string(eframe::glow::VENDOR)});
            }
        }
        json!(null)
    }
}

fn main() -> eframe::Result {
    let opts = parse_opts();
    let t0 = Instant::now();
    let mem0 = spike_common::mem_info();

    // Se preparan los datos y el layout (fuera de la ventana) para medir el coste puro del squarified a este tamaño.
    let probe_area = Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 800.0));
    // Si no se fija --n, se sube el número de archivos hasta que el layout produzca >= --cells rectángulos
    // (los archivos de área < 1 px² no generan celda, así que n > cells).
    let mut n_files = opts.n.unwrap_or(opts.cells);
    let mut folders = synth_tree(n_files, 0xC0FFEE);
    if opts.n.is_none() {
        while layout(&folders, probe_area).len() < opts.cells {
            n_files = (n_files as f64 * 1.05) as usize + 1;
            folders = synth_tree(n_files, 0xC0FFEE);
        }
    }
    let mut layout_ms = Vec::new();
    let mut n_cells = 0;
    for _ in 0..30 {
        let t = Instant::now();
        let c = layout(&folders, probe_area);
        layout_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        n_cells = c.len();
    }
    eprintln!(
        "{BACKEND}: {} archivos en {} carpetas -> {n_cells} celdas; layout mediana {:.2} ms",
        n_files,
        folders.len(),
        spike_common::stats(&layout_ms).p50
    );

    let mut native = eframe::NativeOptions::default();
    native.viewport = egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]).with_title("Tesela spike: treemap");
    if std::env::args().any(|a| a == "--topmost") {
        // Evita que otra ventana tape la del bench (una ventana tapada puede dejar de recibir frames).
        native.viewport = native.viewport.with_always_on_top();
    }
    #[cfg(feature = "wgpu")]
    {
        native.renderer = eframe::Renderer::Wgpu;
        native.wgpu_options.surface.present_mode =
            if opts.vsync { eframe::wgpu::PresentMode::AutoVsync } else { eframe::wgpu::PresentMode::AutoNoVsync };
        if let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut native.wgpu_options.wgpu_setup {
            if let Some(b) = &opts.backend {
                c.instance_descriptor.backends = match b.as_str() {
                    "dx12" => eframe::wgpu::Backends::DX12,
                    "vulkan" => eframe::wgpu::Backends::VULKAN,
                    "gl" => eframe::wgpu::Backends::GL,
                    x => panic!("--backend desconocido: {x}"),
                };
            }
            if let Some(name) = opts.adapter.clone() {
                c.native_adapter_selector = Some(std::sync::Arc::new(move |adapters, _surface| {
                    adapters
                        .iter()
                        .find(|a| a.get_info().name.to_lowercase().contains(&name.to_lowercase()))
                        .cloned()
                        .ok_or_else(|| {
                            format!(
                                "ningún adaptador contiene '{name}'; disponibles: {:?}",
                                adapters.iter().map(|a| a.get_info().name).collect::<Vec<_>>()
                            )
                        })
                }));
            }
        }
    }
    #[cfg(not(feature = "wgpu"))]
    {
        native.renderer = eframe::Renderer::Glow;
        native.glow_options.vsync = opts.vsync;
    }

    let opts2 = opts.clone();
    eframe::run_native(
        "tesela-spike-treemap",
        native,
        Box::new(move |cc| {
            let gpu = gpu_info(cc);
            eprintln!("GPU: {gpu}");
            let mut app = App {
                opts: opts2,
                folders,
                n_files,
                cells: Vec::new(),
                layout_area: Rect::NOTHING,
                gpu,
                phase: Phase::Warmup,
                phase_start: Instant::now(),
                last_frame: None,
                frame_idx: 0,
                samples: Default::default(),
                csv: Vec::new(),
                mems: Vec::new(),
                idle_frames: 0,
                layout_ms,
                start: t0,
                hover_hits: 0,
                finished: false,
                last_logic_log: None,
                restores: Default::default(),
                mesh: None,
                mesh_build_ms: Vec::new(),
                idle_done: Default::default(),
                idle_snaps: Default::default(),
            };
            app.mems.push(MemSnap { at: "antes_de_ventana", ws: mem0.working_set, peak_ws: mem0.peak_working_set, private: mem0.private_bytes, pws: mem0.private_working_set });
            app.snap("tras_crear_ventana_y_gpu");
            Ok(Box::new(app))
        }),
    )
}
