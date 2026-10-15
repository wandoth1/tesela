//! `tesela`: el ejecutable de Tesela. Ventana `eframe` con `wgpu` compilado SOLO con DX12 (WARP de respaldo en
//! máquinas sin GPU), manifiesto `asInvoker` + rutas largas + PerMonitorV2, y configuración mínima en
//! `%APPDATA%\Tesela\config.toml`. En este incremento la aplicación es de SOLO LECTURA.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
mod config;
mod console;
mod shell;

use cli::Screen;
use config::{Config, MIN_SIZE};
use tesela_ui::{DemoOptions, MapStart, Options, StartScreen, TeselaUi};

fn main() -> eframe::Result {
    let cli = match cli::parse(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            console::print(&format!("tesela: {e}\n\n{}", cli::USAGE));
            std::process::exit(2);
        }
    };
    if cli.help {
        console::print(cli::USAGE);
        return Ok(());
    }
    if cli.version {
        console::print(&format!("tesela {}", env!("CARGO_PKG_VERSION")));
        return Ok(());
    }
    #[cfg(not(feature = "capture"))]
    if cli.screenshot.is_some() {
        console::print("tesela: --screenshot necesita compilar con la feature `capture`");
        std::process::exit(2);
    }

    let config_path = config::config_path();
    let config = config_path.as_deref().map(Config::load).unwrap_or_default();
    // Los argumentos de esta sesión mandan sobre lo guardado, pero no se guardan.
    let mut settings = config.settings();
    if let Some(t) = cli.theme {
        settings.theme = t;
    }
    if let Some(l) = cli.lang {
        settings.language = l;
    }
    let tool_mode = cli.bench_hover.is_some() || cli.screenshot.is_some();
    let (w, h) = cli.size.unwrap_or_else(|| config.window_size());
    let normalize = cli.screenshot.is_some().then_some((w, h));

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([w, h])
        .with_min_inner_size([MIN_SIZE.0, MIN_SIZE.1])
        .with_title("Tesela")
        .with_app_id("Tesela");
    if tool_mode {
        // Mediciones y capturas: siempre encima, para que otra ventana no la oculte y eframe deje de pintar.
        viewport = viewport.with_always_on_top();
    }
    let mut native = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut native.wgpu_options.wgpu_setup {
        // Solo DX12 (PLAN.md §1): si no hay GPU, wgpu ofrece el adaptador WARP de Windows.
        c.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    }

    let demo = cli.demo.then_some(DemoOptions {
        stopped: cli.stopped,
        stress: cli.stress,
    });
    let start = match (cli.demo, cli.screen) {
        (true, Screen::Home) if cli.stress => StartScreen::Map(MapStart::default()),
        (true, Screen::Scan) => StartScreen::Scan(cli.progress.unwrap_or(0.45)),
        (true, Screen::Map | Screen::List) => StartScreen::Map(MapStart {
            list: cli.screen == Screen::List,
            open: cli.open.clone(),
            select: cli.select.clone(),
        }),
        _ => StartScreen::Home,
    };
    let animate = !tool_mode && tesela_ui::platform::client_area_animation_enabled();
    let opts = Options {
        settings,
        demo,
        animate,
        start,
    };
    let scan = cli.scan.clone();
    let bench = cli.bench_hover.map(shell::Bench::new);
    if let Some(secs) = cli.bench_hover {
        // Con `--scan` hay que esperar además al escaneo.
        let extra = if cli.scan.is_some() { 600.0 } else { 0.0 };
        // Si la ventana deja de pintar (minimizada u oculta) la medición no avanzaría nunca: se aborta con un aviso.
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs_f32(secs + 40.0 + extra));
            console::print("tesela: --bench-hover sin resultados: la ventana dejó de pintar");
            std::process::exit(3);
        });
    }
    let screenshot = cli.screenshot.clone();
    let persist = !tool_mode;
    let persist_size = cli.size.is_none();

    eframe::run_native(
        "Tesela",
        native,
        Box::new(move |cc| {
            let mut ui = TeselaUi::new(&cc.egui_ctx, opts);
            if let Some(root) = scan {
                ui.request_scan(&cc.egui_ctx, root);
            }
            Ok(Box::new(shell::Shell {
                ui,
                config,
                config_path,
                persist,
                persist_size,
                last_size: None,
                last_title: String::new(),
                bench,
                screenshot,
                shot_frames: 0,
                shot_requested: false,
                normalize_scale: normalize,
                norm_frames: 0,
            }))
        }),
    )
}
