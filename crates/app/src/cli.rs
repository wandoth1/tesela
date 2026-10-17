//! Línea de comandos de `tesela`.

use std::path::PathBuf;
use tesela_ui::{LangPref, ThemePref};

pub const USAGE: &str = "\
tesela: analizador visual de espacio en disco para Windows, con limpieza segura

Uso:
  tesela                       abre la pantalla de inicio
  tesela --demo                modo demostracion: arbol SINTETICO y unidades ficticias; no lee tu disco y la limpieza
                               es SIMULADA (nunca toca el disco)
  tesela --scan <ruta>         analiza esa carpeta o unidad nada mas arrancar
  tesela --dry-run             modo simulacion: la limpieza hace todo salvo borrar o mover y registra las identidades
                               que habria tocado (log local en %LOCALAPPDATA%\\Tesela); se combina con --scan

Opciones de --demo:
  --stopped                    el arbol de la demostracion es el de un analisis detenido (carpetas incompletas)
  --stress                     (desarrollo) un mapa de ~20 000 celdas para medir el peor caso
  --screen home|scan|map|list|recs|confirm|summary
                               pantalla inicial (por defecto home); recs, confirm y summary son del flujo de limpieza
  --open <carpeta/carpeta>     entra por esas carpetas (con map o list)
  --select <nombre>            deja seleccionado ese elemento de la carpeta (con map o list)
  --progress <0..1>            fraccion fija del escaneo simulado (con --screen scan)

Opciones generales:
  --theme system|light|dark    tema para esta sesion (no se guarda)
  --lang system|es|en          idioma para esta sesion (no se guarda)
  --size <ANCHOxALTO>          tamano de ventana en puntos (no se guarda)
  --help                       esta ayuda
  --version                    version

Herramientas de desarrollo (no guardan nada):
  --bench-hover <segundos>     mueve un puntero sintetico sobre el mapa e imprime el coste de CPU por frame
                               (con --demo abre el mapa; sin --demo exige --scan)
  --idle-memory <segundos>     espera con la ventana en reposo e imprime el private working set del proceso
  --screenshot <ruta.png>      guarda una captura de la ventana y sale (solo con --demo y la feature `capture`);
                               NO pisa un archivo que ya exista
  --overwrite                  con --screenshot: reemplaza el archivo si ya existe
";

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Screen {
    #[default]
    Home,
    Scan,
    Map,
    List,
    /// Recomendaciones.
    Recs,
    /// La confirmacion de la limpieza con una cesta de ejemplo.
    Confirm,
    /// El resumen tras limpiar esa cesta.
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
    pub scan: Option<PathBuf>,
    /// Modo simulacion: la limpieza no toca nada.
    pub dry_run: bool,
    pub theme: Option<ThemePref>,
    pub lang: Option<LangPref>,
    pub size: Option<(f32, f32)>,
    pub bench_hover: Option<f32>,
    pub idle_memory: Option<f32>,
    pub screenshot: Option<PathBuf>,
    /// Con `--screenshot`: reemplazar el archivo si ya existe (por defecto no se pisa nada).
    pub overwrite: bool,
    pub help: bool,
    pub version: bool,
}

impl Cli {
    /// ¿Es una herramienta de medición o de captura (`--bench-hover`, `--screenshot`, `--idle-memory`)?
    pub fn tool_mode(&self) -> bool {
        self.bench_hover.is_some() || self.screenshot.is_some() || self.idle_memory.is_some()
    }

    /// ¿Sesión aislada? La demostración y TODAS las herramientas de medición, con o sin `--demo`: no leen ni guardan la
    /// configuración, no leen el historial de limpiezas y no inicializan las raíces reales de la limpieza.
    pub fn isolated(&self) -> bool {
        self.demo || self.tool_mode()
    }
}

/// Interpreta los argumentos (sin el nombre del programa).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut it = args.into_iter();
    fn need(it: &mut dyn Iterator<Item = String>, flag: &str) -> Result<String, String> {
        it.next().ok_or_else(|| format!("{flag} necesita un valor"))
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
                            "--screen: valor desconocido `{v}` (home, scan, map, list, recs, confirm o summary)"
                        ));
                    }
                }
            }
            "--progress" => {
                let v = need(&mut it, "--progress")?;
                let p: f32 = v
                    .parse()
                    .map_err(|_| format!("--progress: `{v}` no es un numero"))?;
                if !(0.0..=1.0).contains(&p) {
                    return Err("--progress debe estar entre 0 y 1".into());
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
            "--scan" => cli.scan = Some(PathBuf::from(need(&mut it, "--scan")?)),
            "--theme" => {
                cli.theme = Some(match need(&mut it, "--theme")?.as_str() {
                    "system" => ThemePref::System,
                    "light" => ThemePref::Light,
                    "dark" => ThemePref::Dark,
                    v => return Err(format!("--theme: valor desconocido `{v}`")),
                })
            }
            "--lang" => {
                cli.lang = Some(match need(&mut it, "--lang")?.as_str() {
                    "system" => LangPref::System,
                    "es" => LangPref::Es,
                    "en" => LangPref::En,
                    v => return Err(format!("--lang: valor desconocido `{v}`")),
                })
            }
            "--size" => {
                let v = need(&mut it, "--size")?;
                let (w, h) = v
                    .split_once(['x', 'X'])
                    .and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))
                    .ok_or_else(|| format!("--size: `{v}` no tiene la forma ANCHOxALTO"))?;
                if !(200.0..=8192.0).contains(&w) || !(200.0..=8192.0).contains(&h) {
                    return Err("--size: el ancho y el alto deben estar entre 200 y 8192".into());
                }
                cli.size = Some((w, h));
            }
            "--bench-hover" => {
                let v = need(&mut it, "--bench-hover")?;
                let s: f32 = v
                    .parse()
                    .map_err(|_| format!("--bench-hover: `{v}` no es un numero"))?;
                if !(1.0..=600.0).contains(&s) {
                    return Err("--bench-hover: entre 1 y 600 segundos".into());
                }
                cli.bench_hover = Some(s);
            }
            "--idle-memory" => {
                let v = need(&mut it, "--idle-memory")?;
                let s: f32 = v
                    .parse()
                    .map_err(|_| format!("--idle-memory: `{v}` no es un numero"))?;
                if !(1.0..=600.0).contains(&s) {
                    return Err("--idle-memory: entre 1 y 600 segundos".into());
                }
                cli.idle_memory = Some(s);
            }
            "--screenshot" => cli.screenshot = Some(PathBuf::from(need(&mut it, "--screenshot")?)),
            "--overwrite" => cli.overwrite = true,
            other => return Err(format!("argumento desconocido: `{other}`")),
        }
    }
    if (cli.stopped
        || cli.stress
        || cli.screen != Screen::Home
        || cli.progress.is_some()
        || !cli.open.is_empty()
        || cli.select.is_some())
        && !cli.demo
    {
        return Err(
            "--stopped, --stress, --screen, --progress, --open y --select solo tienen sentido con --demo"
                .into(),
        );
    }
    if cli.screenshot.is_some() && !cli.demo {
        return Err("--screenshot solo se admite con --demo: una captura de un disco real mostraría datos del equipo".into());
    }
    if cli.overwrite && cli.screenshot.is_none() {
        return Err("--overwrite solo tiene sentido con --screenshot".into());
    }
    if cli.scan.is_some() && cli.demo {
        return Err("--scan y --demo no se combinan".into());
    }
    // La medición mueve el puntero sobre el MAPA: sin él (inicio sin --scan, o escaneo simulado) no habría nada que medir.
    if cli.bench_hover.is_some() && cli.scan.is_none() && !cli.demo {
        return Err("--bench-hover necesita --demo o --scan: mide el mapa, que la pantalla de inicio no tiene".into());
    }
    if cli.bench_hover.is_some() && !matches!(cli.screen, Screen::Home | Screen::Map | Screen::List)
    {
        return Err("--bench-hover mide el mapa o la lista: usa --screen map o --screen list (o nada: con --demo abre el mapa)".into());
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
    }

    #[test]
    fn errores_claros() {
        assert!(p("--screen map").unwrap_err().contains("--demo"));
        assert!(p("--demo --scan x").unwrap_err().contains("no se combinan"));
        assert!(p("--theme naranja").is_err());
        assert!(p("--size 10x10").is_err());
        assert!(p("--size 1280").is_err());
        assert!(p("--progress 2 --demo").is_err());
        assert!(p("--scan").unwrap_err().contains("necesita un valor"));
        assert!(p("--cosa").unwrap_err().contains("desconocido"));
        assert!(p("--bench-hover 0").is_err());
        // Medir el mapa exige llegar a él: sin --demo ni --scan, o con el escaneo simulado, no hay mapa.
        assert!(
            p("--bench-hover 5")
                .unwrap_err()
                .contains("--demo o --scan")
        );
        assert!(p("--demo --screen scan --bench-hover 5").is_err());
        // GM-m8: tampoco las pantallas de la limpieza (recs, confirm, summary): no hay mapa que medir.
        for s in ["recs", "confirm", "summary"] {
            assert!(
                p(&format!("--demo --screen {s} --bench-hover 5"))
                    .unwrap_err()
                    .contains("mide el mapa"),
                "{s}"
            );
        }
        for s in ["map", "list"] {
            assert!(
                p(&format!("--demo --screen {s} --bench-hover 5")).is_ok(),
                "{s}"
            );
        }
        // GM-m8: `--stress` solo tiene sentido con `--demo`.
        assert!(p("--stress").unwrap_err().contains("--demo"));
        assert!(p("--demo --stress").unwrap().stress);
        assert!(p("--demo --bench-hover 5").is_ok());
        assert!(p("--scan X --bench-hover 5").is_ok());
        assert!(p("--idle-memory 0").is_err());
        assert_eq!(
            p("--demo --idle-memory 12").unwrap().idle_memory,
            Some(12.0)
        );
        // Una captura de un disco real mostraría datos del equipo: solo con --demo.
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
        // `--overwrite` solo acompaña a `--screenshot`.
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
            // Sin --demo mostrarian una limpieza que no es la del disco real.
            assert!(p(&format!("--screen {v}")).unwrap_err().contains("--demo"));
        }
        assert!(
            p("--screen otra --demo")
                .unwrap_err()
                .contains("recs, confirm o summary")
        );
        assert!(p("--dry-run").unwrap().dry_run);
        assert!(!p("--demo").unwrap().dry_run);
        // La simulacion se combina con el resto (escanear y limpiar sin tocar nada).
        let c = p("--scan X --dry-run").unwrap();
        assert!(c.dry_run && c.scan == Some(PathBuf::from("X")));
        assert!(p("--demo --screen confirm --dry-run").unwrap().dry_run);
    }

    #[test]
    fn la_sesion_aislada_es_la_demo_y_todas_las_herramientas_de_medicion() {
        // GM-M4: `--idle-memory` y `--bench-hover` SIN `--demo` también están aisladas (sin historial ni raíces reales).
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
        // Las herramientas de medición lo son aunque no haya demostración; la demostración no es una herramienta.
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
