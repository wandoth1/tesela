//! Experimento 5 (PLAN.md §6, §2.10): fuentes variables del sistema y Mica sobre eframe.
//!
//! Comprueba:
//!   1. `SHGetKnownFolderPath(FOLDERID_Fonts)` + existencia de `SegUIVar.ttf`, `segoeui.ttf`, `segoeuib.ttf`.
//!   2. Que `SegUIVar.ttf` carga en egui 0.36 y que se aplican los ejes variables: se listan los ejes
//!      con `FontData::variation_axes()` y se mide el ANCHO del mismo texto con `wght` = 200..900 (si los ejes
//!      se aplican, los anchos cambian). Además se guarda una captura del framebuffer de egui (`--shot-egui`).
//!   3. Mica: `DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_MAINWINDOW)` sobre la ventana de eframe
//!      con viewport transparente. Se registra el HRESULT, se relee con `DwmGetWindowAttribute` y se hace una
//!      captura del ESCRITORIO COMPONIDO con GDI (`--shot-desktop`), única forma de ver el backdrop, que
//!      lo pinta el compositor (DWM) y no aparece en el framebuffer de la app.
//!
//! Uso:
//!   fonts-mica [--mica on|off] [--presentation visual|hwnd] (solo wgpu/DX12)
//!              [--shot-egui a.png] [--shot-desktop b.png] [--out resultados.json] [--hold-secs 3]
//! Compilar: cargo build --release -p fonts-mica [--no-default-features --features glow]

#[cfg(not(any(feature = "wgpu", feature = "glow")))]
compile_error!("Activa exactamente una feature: wgpu o glow");

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, FontTweak, RichText};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{FOLDERID_Fonts, SHGetKnownFolderPath};

#[cfg(feature = "wgpu")]
const BACKEND: &str = "wgpu";
#[cfg(not(feature = "wgpu"))]
const BACKEND: &str = "glow";

const WEIGHTS: [f32; 6] = [200.0, 300.0, 400.0, 600.0, 700.0, 900.0];
const SAMPLE: &str = "Tesela: Analizar disco 0123456789 áéíóú ñ";

/// Ruta de la carpeta Fonts vía SHGetKnownFolderPath.
fn fonts_dir() -> Result<std::path::PathBuf, String> {
    unsafe {
        let mut p: *mut u16 = std::ptr::null_mut();
        let hr = SHGetKnownFolderPath(&FOLDERID_Fonts, 0, std::ptr::null_mut(), &mut p);
        if hr != 0 || p.is_null() {
            return Err(format!("SHGetKnownFolderPath falló: HRESULT {hr:#x}"));
        }
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        CoTaskMemFree(p as *const _);
        Ok(s.into())
    }
}

fn hresult(hr: i32) -> String {
    format!("{:#010x} ({})", hr as u32, if hr >= 0 { "S_OK" } else { "FALLO" })
}

struct Opts {
    mica: bool,
    shot_egui: Option<String>,
    shot_desktop: Option<String>,
    out: String,
    hold_secs: f64,
    presentation: Option<String>,
}

fn parse() -> Opts {
    let a: Vec<String> = std::env::args().collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    Opts {
        mica: get("--mica").map(|s| s != "off").unwrap_or(true),
        shot_egui: get("--shot-egui"),
        shot_desktop: get("--shot-desktop"),
        out: get("--out").unwrap_or_else(|| format!("out/fonts-mica-{BACKEND}.json")),
        hold_secs: get("--hold-secs").and_then(|s| s.parse().ok()).unwrap_or(3.0),
        presentation: get("--presentation"),
    }
}

struct App {
    opts: Opts,
    start: Instant,
    result: serde_json::Map<String, Value>,
    dwm_done: bool,
    widths: Vec<(f32, f32)>,
    widths_done: bool,
    egui_shot_requested: bool,
    desktop_shot_done: bool,
    egui_shot_saved: bool,
    hwnd: isize,
    finished: bool,
}

fn write_png(path: &str, w: u32, h: u32, rgba: &[u8]) {
    if let Some(p) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let f = std::fs::File::create(path).expect("no se pudo crear el PNG");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
}

/// Captura de la pantalla (escritorio componido) en el rectángulo de la ventana.
fn capture_desktop_rect(rect: RECT) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let (w, h) = ((rect.right - rect.left).max(1), (rect.bottom - rect.top).max(1));
        let hdc = GetDC(std::ptr::null_mut());
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp as _);
        let ok = BitBlt(mem, 0, 0, w, h, hdc, rect.left, rect.top, SRCCOPY | CAPTUREBLT);
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h; // top-down
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let got = GetDIBits(mem, bmp, 0, h as u32, buf.as_mut_ptr() as *mut _, &mut bi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        DeleteObject(bmp as _);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), hdc);
        if ok == 0 || got == 0 {
            return None;
        }
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2); // BGRA -> RGBA
            px[3] = 255;
        }
        Some((w as u32, h as u32, buf))
    }
}

fn pixel_stats(rgba: &[u8]) -> Value {
    let n = (rgba.len() / 4) as f64;
    let (mut sr, mut sg, mut sb) = (0f64, 0f64, 0f64);
    for p in rgba.chunks_exact(4) {
        sr += p[0] as f64;
        sg += p[1] as f64;
        sb += p[2] as f64;
    }
    let (mr, mg, mb) = (sr / n, sg / n, sb / n);
    let mut var = 0f64;
    for p in rgba.chunks_exact(4) {
        var += (p[0] as f64 - mr).powi(2) + (p[1] as f64 - mg).powi(2) + (p[2] as f64 - mb).powi(2);
    }
    json!({"media_rgb": [mr.round(), mg.round(), mb.round()], "desviacion_tipica_global": (var / (3.0 * n)).sqrt()})
}

/// Valores RGBA de algunos puntos fijos (fracciones del ancho/alto) lejos de la tarjeta de texto:
/// sirven para ver el color del fondo (con Mica, un gris/tinte distinto del blanco o negro puros).
fn sample_points(w: u32, h: u32, rgba: &[u8]) -> Value {
    let pts = [(0.85, 0.15), (0.85, 0.5), (0.85, 0.9), (0.5, 0.9), (0.15, 0.9), (0.5, 0.06)];
    let v: Vec<Value> = pts
        .iter()
        .map(|(fx, fy)| {
            let (x, y) = ((w as f64 * fx) as u32, (h as f64 * fy) as u32);
            let i = ((y * w + x) * 4) as usize;
            json!({"x": x, "y": y, "rgba": [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]})
        })
        .collect();
    json!(v)
}

impl App {
    fn apply_dwm(&mut self, frame: &eframe::Frame) {
        self.dwm_done = true;
        let hwnd = match frame.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            other => {
                self.result.insert("mica".into(), json!({"error": format!("no hay HWND Win32: {other:?}")}));
                return;
            }
        };
        self.hwnd = hwnd;
        let h = hwnd as HWND;
        unsafe {
            let mut os: windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW = std::mem::zeroed();
            os.dwOSVersionInfoSize = std::mem::size_of_val(&os) as u32;
            windows_sys::Wdk::System::SystemServices::RtlGetVersion(&mut os);
            // Experimento (--null-brush): quitar el pincel de fondo de la clase de ventana (por si el blanco que se ve con wgpu
            // es el fondo que pinta el sistema bajo la cadena de intercambio).
            if std::env::args().any(|a| a == "--null-brush") {
                use windows_sys::Win32::UI::WindowsAndMessaging::{GCLP_HBRBACKGROUND, SetClassLongPtrW};
                SetClassLongPtrW(h, GCLP_HBRBACKGROUND, GetStockObject(NULL_BRUSH) as isize);
            }
            // Extiende el marco al área cliente (lámina de cristal) y fija el backdrop.
            let margins = windows_sys::Win32::UI::Controls::MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
            let hr_extend = DwmExtendFrameIntoClientArea(h, &margins);
            let dark: i32 = 1;
            let hr_dark = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, &dark as *const _ as *const _, 4);
            let backdrop: i32 = if self.opts.mica { DWMSBT_MAINWINDOW } else { DWMSBT_NONE };
            let hr_set = DwmSetWindowAttribute(h, DWMWA_SYSTEMBACKDROP_TYPE as u32, &backdrop as *const _ as *const _, 4);
            let mut readback: i32 = -1;
            let hr_get = DwmGetWindowAttribute(h, DWMWA_SYSTEMBACKDROP_TYPE as u32, &mut readback as *mut _ as *mut _, 4);
            self.result.insert(
                "mica".into(),
                json!({
                    "os_build": format!("{}.{}.{}", os.dwMajorVersion, os.dwMinorVersion, os.dwBuildNumber),
                    "backdrop_pedido": if self.opts.mica { "DWMSBT_MAINWINDOW (2) = Mica" } else { "DWMSBT_NONE (1)" },
                    "DwmExtendFrameIntoClientArea": hresult(hr_extend),
                    "DwmSetWindowAttribute_USE_IMMERSIVE_DARK_MODE": hresult(hr_dark),
                    "DwmSetWindowAttribute_SYSTEMBACKDROP_TYPE": hresult(hr_set),
                    "DwmGetWindowAttribute_SYSTEMBACKDROP_TYPE": hresult(hr_get),
                    "valor_releido": readback,
                    "hwnd": format!("{hwnd:#x}"),
                }),
            );
        }
    }

    fn desktop_shot(&mut self) {
        self.desktop_shot_done = true;
        let Some(path) = self.opts.shot_desktop.clone() else { return };
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            let hr = DwmGetWindowAttribute(self.hwnd as HWND, DWMWA_EXTENDED_FRAME_BOUNDS as u32, &mut r as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32);
            let info = if hr < 0 { json!({"error": hresult(hr)}) } else {
                match capture_desktop_rect(r) {
                    Some((w, h, rgba)) => {
                        write_png(&path, w, h, &rgba);
                        json!({"png": path, "rect_pantalla": [r.left, r.top, r.right, r.bottom], "estadisticas_pixeles": pixel_stats(&rgba), "puntos_de_fondo": sample_points(w, h, &rgba)})
                    }
                    None => json!({"error": "BitBlt/GetDIBits falló"}),
                }
            };
            self.result.insert("captura_escritorio".into(), info);
        }
    }

    fn finish(&mut self, ctx: &egui::Context) {
        if self.finished {
            return;
        }
        self.finished = true;
        let doc = Value::Object(std::mem::take(&mut self.result));
        if let Some(p) = std::path::Path::new(&self.opts.out).parent() {
            let _ = std::fs::create_dir_all(p);
        }
        std::fs::write(&self.opts.out, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        eprintln!("{}", serde_json::to_string_pretty(&doc).unwrap());
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for App {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // totalmente transparente: así el backdrop de DWM se ve a través
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.dwm_done {
            self.apply_dwm(frame);
        }
        // Eventos de captura de egui
        let shots: Vec<_> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None })
                .collect()
        });
        for img in shots {
            if let Some(p) = &self.opts.shot_egui {
                let mut rgba = Vec::with_capacity(img.pixels.len() * 4);
                for c in &img.pixels {
                    rgba.extend_from_slice(&[c.r(), c.g(), c.b(), c.a()]);
                }
                write_png(p, img.size[0] as u32, img.size[1] as u32, &rgba);
                self.result.insert("captura_egui".into(), json!({"png": p, "tam": img.size, "estadisticas_pixeles": pixel_stats(&rgba), "puntos_de_fondo": sample_points(img.size[0] as u32, img.size[1] as u32, &rgba)}));
                self.egui_shot_saved = true;
            }
        }

        // UI: fondo TRANSPARENTE + tarjeta semitransparente + texto con distintos pesos.
        egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(24.0).show(ui, |ui| {
            ui.label(RichText::new(format!("Tesela · spike fuentes + Mica · backend {BACKEND}")).size(22.0).color(Color32::WHITE).family(FontFamily::Name("w600".into())));
            ui.add_space(8.0);
            egui::Frame::new().fill(Color32::from_rgba_unmultiplied(32, 32, 32, 90)).corner_radius(8.0).inner_margin(16.0).show(ui, |ui| {
                for w in WEIGHTS {
                    let fam = FontFamily::Name(format!("w{}", w as u32).into());
                    let galley = ui.painter().layout_no_wrap(SAMPLE.to_string(), FontId::new(20.0, fam.clone()), Color32::WHITE);
                    if !self.widths_done {
                        self.widths.push((w, galley.size().x));
                    }
                    ui.label(RichText::new(format!("wght={w:.0}  {SAMPLE}")).font(FontId::new(20.0, fam)).color(Color32::WHITE));
                }
                self.widths_done = true;
                ui.add_space(8.0);
                ui.label(RichText::new("Fuente por defecto de egui (Ubuntu-Light/Hack) para contraste: Tesela 0123456789 áéíóú ñ").size(16.0).family(FontFamily::Name("egui_default".into())).color(Color32::LIGHT_GRAY));
            });
        });

        let t = self.start.elapsed().as_secs_f64();
        if t > 1.5 && self.opts.shot_egui.is_some() && !self.egui_shot_requested {
            self.egui_shot_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        if t > 2.0 && !self.desktop_shot_done {
            self.desktop_shot();
        }
        let ws: Vec<Value> = self.widths.iter().map(|(w, x)| json!({"wght": w, "ancho_px": x})).collect();
        if t > self.opts.hold_secs && (self.egui_shot_saved || self.opts.shot_egui.is_none()) {
            let distinct = {
                let mut v: Vec<i64> = self.widths.iter().map(|(_, x)| (*x * 100.0).round() as i64).collect();
                v.dedup();
                v.len()
            };
            self.result.insert(
                "anchos_mismo_texto_por_wght".into(),
                json!({"texto": SAMPLE, "medidas": ws, "valores_distintos": distinct,
                       "los_ejes_variables_se_aplican": distinct > 1,
                       "monotonico_creciente": self.widths.windows(2).all(|p| p[1].1 >= p[0].1)}),
            );
            self.result.insert("backend".into(), json!(BACKEND));
            self.finish(&ctx);
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

fn main() -> eframe::Result {
    // Logger a stderr: con RUST_LOG=egui_wgpu=debug,wgpu_hal=warn se ven los avisos de egui-wgpu (p. ej. si la
    // superficie no admite un CompositeAlphaMode con transparencia).
    env_logger::init();
    let opts = parse();
    let mut result = serde_json::Map::new();

    // 1) Carpeta de fuentes y archivos
    let dir = fonts_dir();
    let mut fonts_json = serde_json::Map::new();
    let mut fd = FontDefinitions::default();
    match &dir {
        Ok(d) => {
            fonts_json.insert("SHGetKnownFolderPath(FOLDERID_Fonts)".into(), json!(d.display().to_string()));
            for name in ["SegUIVar.ttf", "segoeui.ttf", "segoeuib.ttf"] {
                let p = d.join(name);
                fonts_json.insert(name.into(), json!({"existe": p.exists(), "bytes": std::fs::metadata(&p).map(|m| m.len()).ok()}));
            }
        }
        Err(e) => {
            fonts_json.insert("error".into(), json!(e));
        }
    }
    // 2) Cargar SegUIVar.ttf en egui, una familia por peso (cada una con su propia FontTweak.coords)
    let mut axes_json = json!(null);
    let mut loaded = false;
    if let Ok(d) = &dir {
        if let Ok(bytes) = std::fs::read(d.join("SegUIVar.ttf")) {
            let base = FontData::from_owned(bytes);
            let axes = base.variation_axes();
            axes_json = json!(axes.iter().map(|a| json!({"tag": format!("{:?}", a.tag), "nombre": a.name, "min": a.range.min, "max": a.range.max, "defecto": a.default, "oculto": a.hidden})).collect::<Vec<_>>());
            for w in WEIGHTS {
                let key = format!("SegUIVar-w{}", w as u32);
                let tweak = FontTweak { coords: egui::epaint::text::VariationCoords::new([("wght", w)]), ..Default::default() };
                fd.font_data.insert(key.clone(), std::sync::Arc::new(base.clone().tweak(tweak)));
                fd.families.insert(FontFamily::Name(format!("w{}", w as u32).into()), vec![key]);
            }
            loaded = true;
        }
    }
    if !loaded {
        // Sin SegUIVar: se usa la fuente por defecto de egui para que la app arranque igualmente.
        for w in WEIGHTS {
            fd.families.insert(FontFamily::Name(format!("w{}", w as u32).into()), vec!["Ubuntu-Light".to_string()]);
        }
    }
    let prop = fd.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    fd.families.insert(FontFamily::Name("egui_default".into()), prop);
    fonts_json.insert("SegUIVar_cargada_en_egui".into(), json!(loaded));
    fonts_json.insert("ejes_variables".into(), axes_json.clone());
    result.insert("fuentes".into(), Value::Object(fonts_json));

    let mut native = eframe::NativeOptions::default();
    native.viewport = egui::ViewportBuilder::default()
        .with_inner_size([900.0, 520.0])
        .with_position([120.0, 120.0])
        .with_transparent(true)
        .with_always_on_top()
        .with_title("Tesela spike: fuentes + Mica");
    #[cfg(feature = "wgpu")]
    {
        native.renderer = eframe::Renderer::Wgpu;
        if let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut native.wgpu_options.wgpu_setup {
            if let Some(p) = &opts.presentation {
                c.instance_descriptor.backend_options.dx12.presentation_system = match p.as_str() {
                    "visual" => eframe::wgpu::Dx12SwapchainKind::DxgiFromVisual,
                    _ => eframe::wgpu::Dx12SwapchainKind::DxgiFromHwnd,
                };
            }
            // DX12 explícito: es donde el spike espera que funcione la transparencia con Visual
            if let Ok(b) = std::env::var("SPIKE_BACKEND") {
                c.instance_descriptor.backends = match b.as_str() {
                    "vulkan" => eframe::wgpu::Backends::VULKAN,
                    "gl" => eframe::wgpu::Backends::GL,
                    _ => eframe::wgpu::Backends::DX12,
                };
            }
        }
    }
    #[cfg(not(feature = "wgpu"))]
    {
        native.renderer = eframe::Renderer::Glow;
    }
    let presentation = opts.presentation.clone();
    eframe::run_native(
        "tesela-spike-fonts-mica",
        native,
        Box::new(move |cc| {
            cc.egui_ctx.set_fonts(fd);
            #[cfg(feature = "wgpu")]
            if let Some(rs) = &cc.wgpu_render_state {
                let i = rs.adapter.get_info();
                result.insert("gpu".into(), json!({"adaptador": i.name, "backend_wgpu": format!("{:?}", i.backend), "presentation_system_pedido": presentation, "formato": format!("{:?}", rs.target_format)}));
                // Modos alfa que soporta la superficie no son accesibles aquí; se infiere del resultado visual.
            }
            #[cfg(not(feature = "wgpu"))]
            {
                let _ = &presentation;
                result.insert("gpu".into(), json!({"backend": "glow"}));
            }
            Ok(Box::new(App {
                opts,
                start: Instant::now(),
                result,
                dwm_done: false,
                widths: vec![],
                widths_done: false,
                egui_shot_requested: false,
                desktop_shot_done: false,
                egui_shot_saved: false,
                hwnd: 0,
                finished: false,
            }))
        }),
    )
}
