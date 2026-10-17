//! `tesela`: Tesela's executable. An `eframe` window with `wgpu` built for DX12 ONLY (Windows WARP fallback on machines
//! without a GPU), an `asInvoker` manifest with long paths and PerMonitorV2, and minimal configuration in
//! `%APPDATA%\Tesela\config.toml`. Cleanup (basket, confirmation, deletion, and Recycle Bin) lives in `tesela-cleaner`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
mod config;
mod console;
mod dll_hardening;
mod memory;
mod output;
mod shell;

use cli::Screen;
use config::MIN_SIZE;
use tesela_ui::{CleanStart, DemoOptions, MapStart, Options, StartScreen, TeselaUi, TransportRef};

fn main() -> eframe::Result {
    // FIRST, before parsing arguments and before eframe or wgpu loads anything by name: restrict DLL search to System32
    // (see `dll_hardening`). If Windows rejects this, continue with the default search order.
    let _ = dll_hardening::restrict_dll_search();
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
        console::print("tesela: --screenshot requires building with the `capture` feature");
        std::process::exit(2);
    }

    // Detect an unsavable screenshot BEFORE opening the window, rather than after rendering it.
    if let Some(path) = &cli.screenshot
        && let Err(e) = output::preflight(path, cli.overwrite)
    {
        console::print(&format!("tesela: {e}"));
        std::process::exit(1);
    }

    // Without system fonts, text cannot be rendered (none are bundled): report the problem and exit instead of panicking.
    if !tesela_ui::fonts::available() {
        console::print(
            "tesela: no Windows fonts (Segoe UI, Tahoma, or Arial) were found in the system fonts directory",
        );
        std::process::exit(3);
    }

    // The demo ("nothing is read from your disk") and measurement tools do NOT read or save configuration: they use
    // in-memory defaults and do not access %APPDATA%.
    let tool_mode = cli.tool_mode();
    let isolated = cli.isolated();
    let session = config::Session::new(isolated);
    let persist = session.persists();
    let config::Session {
        config,
        path: config_path,
    } = session;
    // Arguments for this session override saved settings but are not saved themselves.
    let mut settings = config.settings();
    if let Some(t) = cli.theme {
        settings.theme = t;
    }
    if let Some(l) = cli.lang {
        settings.language = l;
    }
    let (w, h) = cli.size.unwrap_or_else(|| config.window_size());
    let normalize = cli.screenshot.is_some().then_some((w, h));

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([w, h])
        .with_min_inner_size([MIN_SIZE.0, MIN_SIZE.1])
        .with_title("Tesela")
        .with_app_id("Tesela");
    if tool_mode {
        // Measurements and screenshots: keep the window on top so another window cannot hide it and stop eframe rendering.
        viewport = viewport.with_always_on_top();
    }
    let mut native = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut native.wgpu_options.wgpu_setup {
        // DX12 only (PLAN.md §1): if there is no GPU, wgpu offers the Windows WARP adapter.
        c.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
        // FIXED shader compiler: FXC (`d3dcompiler_47.dll`, from the system). With the default `Auto`, wgpu first tries to
        // load `dxcompiler.dll` by name, which is absent on a standard Windows installation; this also ignores the
        // `WGPU_DX12_COMPILER` environment variable.
        c.instance_descriptor.backend_options.dx12.shader_compiler =
            eframe::wgpu::Dx12Compiler::Fxc;
    }

    let demo = cli.demo.then_some(DemoOptions {
        stopped: cli.stopped,
        stress: cli.stress,
    });
    let start = match (cli.demo, cli.screen) {
        // `--stress` and `--bench-hover` measure the map: with `--demo` and no `--screen`, open it directly.
        (true, Screen::Home) if cli.stress || cli.bench_hover.is_some() => {
            StartScreen::Map(MapStart::default())
        }
        (true, Screen::Scan) => StartScreen::Scan(cli.progress.unwrap_or(0.45)),
        (true, Screen::Recs) => StartScreen::Clean(CleanStart::Recommendations),
        (true, Screen::Confirm) => StartScreen::Clean(CleanStart::Confirm),
        (true, Screen::Summary) => StartScreen::Clean(CleanStart::Summary),
        (true, Screen::Map | Screen::List) => StartScreen::Map(MapStart {
            list: cli.screen == Screen::List,
            open: cli.open.clone(),
            select: cli.select.clone(),
            marks: cli.marks.clone(),
        }),
        _ => StartScreen::Home,
    };
    let animate = !tool_mode && tesela_ui::platform::client_area_animation_enabled();
    let opts = Options {
        settings,
        demo,
        animate,
        start,
        dry_run: cli.dry_run,
        backend: None,
        // Demo and measurement tools: nothing is read from the user's disk (no history or cleanup roots).
        isolated,
        // Update checking is OPTIONAL (disabled by default). The demo and measurement tools do not receive the transport,
        // so they never check for updates, regardless of the setting.
        update_transport: (!isolated)
            .then(|| TransportRef(std::sync::Arc::new(tesela_update::winhttp::WinHttp))),
    };
    let scan = cli.scan.clone();
    let wait_explore = scan.is_some();
    let bench = cli.bench_hover.map(shell::Bench::new);
    if let Some(secs) = cli.bench_hover {
        // With `--scan`, also allow time for the scan to finish.
        let extra = if cli.scan.is_some() { 600.0 } else { 0.0 };
        // If the window stops rendering (minimized or hidden), the measurement would never advance: abort with a message.
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs_f32(secs + 40.0 + extra));
            console::print(
                "tesela: --bench-hover produced no results: the window stopped rendering or never reached the map",
            );
            std::process::exit(3);
        });
    }
    let idle_mem = cli.idle_memory.map(shell::IdleMemory::new);
    if let Some(secs) = cli.idle_memory {
        // Do not wait forever if the window never renders.
        let extra = if cli.scan.is_some() { 600.0 } else { 0.0 };
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs_f32(secs + 40.0 + extra));
            console::print(
                "tesela: --idle-memory produced no results: the window stopped rendering",
            );
            std::process::exit(3);
        });
    }
    let screenshot = cli.screenshot.clone();
    let screenshot_overwrite = cli.overwrite;
    let persist_size = cli.size.is_none();

    let result = eframe::run_native(
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
                session_settings: settings,
                last_title: String::new(),
                bench,
                idle_mem,
                screenshot,
                screenshot_overwrite,
                shot_frames: 0,
                shot_requested: false,
                normalize_scale: normalize,
                norm_frames: 0,
                wait_explore,
            }))
        }),
    );
    // A requested screenshot that could not be saved is a failure: exit with a nonzero code.
    if output::failed() {
        std::process::exit(1);
    }
    result
}
