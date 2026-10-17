//! Command-line interface for `tesela`.

use std::path::PathBuf;
use tesela_ui::{LangPref, ThemePref};

pub const USAGE: &str = "\
tesela: visual disk space analyzer for Windows with safe cleanup

Usage:
  tesela                       open the home screen
  tesela --demo                demo mode: SYNTHETIC tree and fictional drives; does not read your disk and cleanup is
                               SIMULATED (never touches the disk)
  tesela --scan <path>         scan that folder or drive on startup
  tesela --dry-run             simulation mode: cleanup does everything except deleting or moving items, and logs the
                               identities it would have touched (local log in %LOCALAPPDATA%\\Tesela); combine with --scan

Options for --demo:
  --stopped                    use a demo tree from a stopped scan (incomplete folders)
  --stress                     (development) a map with ~20,000 cells for worst-case measurements
  --screen home|scan|map|list|recs|confirm|summary
                               initial screen (default: home); recs, confirm, and summary are cleanup flow screens
  --open <folder/folder>       navigate into those folders (with map or list)
  --select <name>              select that item in the folder (with map or list)
  --mark <name>                mark a visible item in the folder for the selection bar; may be repeated
  --progress <0..1>            fixed fraction of the simulated scan (with --screen scan)

General options:
  --theme system|light|dark    theme for this session (not saved)
  --lang system|es|en          language for this session (not saved)
  --size <WIDTHxHEIGHT>        window size in points (not saved)
  --help                       show this help
  --version                    show version

Developer tools (do not save anything):
  --bench-hover <seconds>      moves a synthetic pointer over the map and prints CPU cost per frame
                               (opens the map with --demo; requires --scan without --demo)
  --idle-memory <seconds>      waits with the window idle and prints the process private working set
  --screenshot <path.png>      saves a window screenshot and exits (only with --demo and the `capture` feature);
                               NEVER overwrites an existing file
  --overwrite                  with --screenshot: replace the file if it already exists
";

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Screen {
    #[default]
    Home,
    Scan,
    Map,
    List,
    /// Recommendations.
    Recs,
    /// The cleanup confirmation screen with a sample basket.
    Confirm,
    /// The summary after cleaning that basket.
    Summary,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cli {
    pub demo: bool,
    pub stopped: bool,
    pub stress: bool,
    pub screen: Screen,
    pub progress: Option<f32>,
    pub open: Vec<String>,
    pub select: Option<String>,
    pub marks: Vec<String>,
    pub scan: Option<PathBuf>,
    /// Simulation mode: cleanup does not touch anything.
    pub dry_run: bool,
    pub theme: Option<ThemePref>,
    pub lang: Option<LangPref>,
    pub size: Option<(f32, f32)>,
    pub bench_hover: Option<f32>,
    pub idle_memory: Option<f32>,
    pub screenshot: Option<PathBuf>,
    /// With `--screenshot`: replace the file if it already exists (by default, nothing is overwritten).
    pub overwrite: bool,
    pub help: bool,
    pub version: bool,
}

impl Cli {
    /// Is this a measurement or capture tool (`--bench-hover`, `--screenshot`, `--idle-memory`)?
    pub fn tool_mode(&self) -> bool {
        self.bench_hover.is_some() || self.screenshot.is_some() || self.idle_memory.is_some()
    }

    /// Is this an isolated session? The demo and ALL measurement tools, with or without `--demo`, do not read or save
    /// configuration, read cleanup history, or initialize real cleanup roots.
    pub fn isolated(&self) -> bool {
        self.demo || self.tool_mode()
    }
}

/// Parses arguments (excluding the program name).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut it = args.into_iter();
    fn need(it: &mut dyn Iterator<Item = String>, flag: &str) -> Result<String, String> {
        it.next().ok_or_else(|| format!("{flag} requires a value"))
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--demo" => cli.demo = true,
            "--stopped" => cli.stopped = true,
            "--stress" => cli.stress = true,
            "--dry-run" => cli.dry_run = true,
            "--help" | "-h" | "-?" => cli.help = true,
            "--version" | "-V" => cli.version = true,
            "--screen" => {
                cli.screen = match need(&mut it, "--screen")?.as_str() {
                    "home" => Screen::Home,
                    "scan" => Screen::Scan,
                    "map" => Screen::Map,
                    "list" => Screen::List,
                    "recs" => Screen::Recs,
                    "confirm" => Screen::Confirm,
                    "summary" => Screen::Summary,
                    v => {
                        return Err(format!(
                            "--screen: unknown value `{v}` (expected home, scan, map, list, recs, confirm, or summary)"
                        ));
                    }
                }
            }
            "--progress" => {
                let v = need(&mut it, "--progress")?;
                let p: f32 = v
                    .parse()
                    .map_err(|_| format!("--progress: `{v}` is not a number"))?;
                if !(0.0..=1.0).contains(&p) {
                    return Err("--progress must be between 0 and 1".into());
                }
                cli.progress = Some(p);
            }
            "--open" => {
                cli.open = need(&mut it, "--open")?
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            }
            "--select" => cli.select = Some(need(&mut it, "--select")?),
            "--mark" => cli.marks.push(need(&mut it, "--mark")?),
            "--scan" => cli.scan = Some(PathBuf::from(need(&mut it, "--scan")?)),
            "--theme" => {
                cli.theme = Some(match need(&mut it, "--theme")?.as_str() {
                    "system" => ThemePref::System,
                    "light" => ThemePref::Light,
                    "dark" => ThemePref::Dark,
                    v => return Err(format!("--theme: unknown value `{v}`")),
                })
            }
            "--lang" => {
                cli.lang = Some(match need(&mut it, "--lang")?.as_str() {
                    "system" => LangPref::System,
                    "es" => LangPref::Es,
                    "en" => LangPref::En,
                    v => return Err(format!("--lang: unknown value `{v}`")),
                })
            }
            "--size" => {
                let v = need(&mut it, "--size")?;
                let (w, h) = v
                    .split_once(['x', 'X'])
                    .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
                    .ok_or_else(|| format!("--size: `{v}` must have the form WIDTHxHEIGHT"))?;
                if !(200.0..=8192.0).contains(&w) || !(200.0..=8192.0).contains(&h) {
                    return Err("--size: width and height must be between 200 and 8192".into());
                }
                cli.size = Some((w, h));
            }
            "--bench-hover" => {
                let v = need(&mut it, "--bench-hover")?;
                let s: f32 = v
                    .parse()
                    .map_err(|_| format!("--bench-hover: `{v}` is not a number"))?;
                if !(1.0..=600.0).contains(&s) {
                    return Err("--bench-hover: value must be between 1 and 600 seconds".into());
                }
                cli.bench_hover = Some(s);
            }
            "--idle-memory" => {
                let v = need(&mut it, "--idle-memory")?;
                let s: f32 = v
                    .parse()
                    .map_err(|_| format!("--idle-memory: `{v}` is not a number"))?;
                if !(1.0..=600.0).contains(&s) {
                    return Err("--idle-memory: value must be between 1 and 600 seconds".into());
                }
                cli.idle_memory = Some(s);
            }
            "--screenshot" => cli.screenshot = Some(PathBuf::from(need(&mut it, "--screenshot")?)),
            "--overwrite" => cli.overwrite = true,
            other => return Err(format!("unknown argument: `{other}`")),
        }
    }
    if (cli.stopped
        || cli.stress
        || cli.screen != Screen::Home
        || cli.progress.is_some()
        || !cli.open.is_empty()
        || cli.select.is_some()
        || !cli.marks.is_empty())
        && !cli.demo
    {
        return Err(
            "--stopped, --stress, --screen, --progress, --open, --select, and --mark are only valid with --demo"
                .into(),
        );
    }
    if cli.screenshot.is_some() && !cli.demo {
        return Err("--screenshot is only available with --demo: a screenshot of a real disk could expose your data".into());
    }
    if !cli.marks.is_empty() && !matches!(cli.screen, Screen::Map | Screen::List) {
        return Err("--mark requires --screen map or --screen list".into());
    }
    if cli.overwrite && cli.screenshot.is_none() {
        return Err("--overwrite is only valid with --screenshot".into());
    }
    if cli.scan.is_some() && cli.demo {
        return Err("--scan and --demo cannot be combined".into());
    }
    // Measurement moves the pointer over the MAP: without one (startup without --scan or a simulated scan), there is
    // nothing to measure.
    if cli.bench_hover.is_some() && cli.scan.is_none() && !cli.demo {
        return Err("--bench-hover requires --demo or --scan: it measures the map, which is not available on the home screen".into());
    }
    if cli.bench_hover.is_some() && !matches!(cli.screen, Screen::Home | Screen::Map | Screen::List)
    {
        return Err("--bench-hover measures the map or list: use --screen map or --screen list (or omit it: --demo opens the map)".into());
    }
    Ok(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Cli, String> {
        parse(s.split_whitespace().map(String::from))
    }

    #[test]
    fn sin_argumentos_es_el_inicio_normal() {
        assert_eq!(p("").unwrap(), Cli::default());
    }

    #[test]
    fn demo_y_sus_opciones() {
        let c = p("--demo --stopped --screen map --theme dark --lang en --size 1280x800").unwrap();
        assert!(c.demo && c.stopped);
        assert_eq!(c.screen, Screen::Map);
        assert_eq!(c.theme, Some(ThemePref::Dark));
        assert_eq!(c.lang, Some(LangPref::En));
        assert_eq!(c.size, Some((1280.0, 800.0)));
        let c = p("--demo --screen scan --progress 0.4").unwrap();
        assert_eq!((c.screen, c.progress), (Screen::Scan, Some(0.4)));
        let c = p("--demo --screen map --mark Descargas --mark Proyectos").unwrap();
        assert_eq!(c.marks, ["Descargas", "Proyectos"]);
        assert!(
            p("--mark Descargas --screen map")
                .unwrap_err()
                .contains("--demo")
        );
        assert!(
            p("--demo --mark Descargas")
                .unwrap_err()
                .contains("--screen map")
        );
    }

    #[test]
    fn errores_claros() {
        assert!(p("--screen map").unwrap_err().contains("--demo"));
        assert!(
            p("--demo --scan x")
                .unwrap_err()
                .contains("cannot be combined")
        );
        assert!(p("--theme naranja").is_err());
        assert!(p("--size 10x10").is_err());
        assert!(p("--size 1280").is_err());
        assert!(p("--progress 2 --demo").is_err());
        assert!(p("--scan").unwrap_err().contains("requires a value"));
        assert!(p("--cosa").unwrap_err().contains("unknown argument"));
        assert!(p("--bench-hover 0").is_err());
        // Measuring the map requires reaching it: without --demo or --scan, or with a simulated scan, there is no map.
        assert!(
            p("--bench-hover 5")
                .unwrap_err()
                .contains("--demo or --scan")
        );
        assert!(p("--demo --screen scan --bench-hover 5").is_err());
        // GM-m8: cleanup screens (recs, confirm, summary) have no map to measure either.
        for s in ["recs", "confirm", "summary"] {
            assert!(
                p(&format!("--demo --screen {s} --bench-hover 5"))
                    .unwrap_err()
                    .contains("measures the map"),
                "{s}"
            );
        }
        for s in ["map", "list"] {
            assert!(
                p(&format!("--demo --screen {s} --bench-hover 5")).is_ok(),
                "{s}"
            );
        }
        // GM-m8: `--stress` only makes sense with `--demo`.
        assert!(p("--stress").unwrap_err().contains("--demo"));
        assert!(p("--demo --stress").unwrap().stress);
        assert!(p("--demo --bench-hover 5").is_ok());
        assert!(p("--scan X --bench-hover 5").is_ok());
        assert!(p("--idle-memory 0").is_err());
        assert_eq!(
            p("--demo --idle-memory 12").unwrap().idle_memory,
            Some(12.0)
        );
        // Capturing a real disk would expose machine data: allow it only with --demo.
        assert!(p("--screenshot x.png").unwrap_err().contains("--demo"));
        assert!(
            p("--scan C --screenshot x.png")
                .unwrap_err()
                .contains("--demo")
        );
        assert_eq!(
            p("--demo --screenshot x.png").unwrap().screenshot,
            Some(PathBuf::from("x.png"))
        );
        // `--overwrite` is only valid with `--screenshot`.
        assert!(
            p("--demo --overwrite")
                .unwrap_err()
                .contains("--screenshot")
        );
        let c = p("--demo --screenshot x.png --overwrite").unwrap();
        assert!(c.overwrite);
        assert!(!p("--demo --screenshot x.png").unwrap().overwrite);
    }

    #[test]
    fn limpieza_pantallas_y_simulacion() {
        for (v, sc) in [
            ("recs", Screen::Recs),
            ("confirm", Screen::Confirm),
            ("summary", Screen::Summary),
        ] {
            let c = p(&format!("--demo --screen {v}")).unwrap();
            assert_eq!(c.screen, sc);
            // Without --demo, these would show a cleanup that does not reflect the real disk.
            assert!(p(&format!("--screen {v}")).unwrap_err().contains("--demo"));
        }
        assert!(
            p("--screen otra --demo")
                .unwrap_err()
                .contains("recs, confirm, or summary")
        );
        assert!(p("--dry-run").unwrap().dry_run);
        assert!(!p("--demo").unwrap().dry_run);
        // Simulation can be combined with the rest (scan and clean without touching anything).
        let c = p("--scan X --dry-run").unwrap();
        assert!(c.dry_run && c.scan == Some(PathBuf::from("X")));
        assert!(p("--demo --screen confirm --dry-run").unwrap().dry_run);
    }

    #[test]
    fn la_sesion_aislada_es_la_demo_y_todas_las_herramientas_de_medicion() {
        // GM-M4: `--idle-memory` and `--bench-hover` are isolated even WITHOUT `--demo` (no history or real roots).
        assert!(!p("").unwrap().isolated());
        assert!(!p("--scan X").unwrap().isolated());
        assert!(!p("--dry-run").unwrap().isolated());
        for a in [
            "--demo",
            "--idle-memory 5",
            "--bench-hover 5 --scan X",
            "--demo --bench-hover 5",
            "--demo --screenshot x.png",
        ] {
            assert!(p(a).unwrap().isolated(), "{a}");
        }
        // Measurement tools are isolated even without the demo; the demo itself is not a measurement tool.
        assert!(p("--idle-memory 5").unwrap().tool_mode());
        assert!(!p("--demo").unwrap().tool_mode());
    }

    #[test]
    fn ayuda_y_version() {
        assert!(p("--help").unwrap().help);
        assert!(p("-V").unwrap().version);
        assert_eq!(p("--scan X").unwrap().scan, Some(PathBuf::from("X")));
    }
}
