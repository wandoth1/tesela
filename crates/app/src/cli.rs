//! Línea de comandos de `tesela`.

use std::path::PathBuf;
use tesela_ui::{LangPref, ThemePref};

pub const USAGE: &str = "\
tesela: analizador visual de espacio en disco para Windows (solo lectura en esta version)

Uso:
  tesela                       abre la pantalla de inicio
  tesela --demo                modo demostracion: arbol SINTETICO y unidades ficticias; no lee tu disco
  tesela --scan <ruta>         analiza esa carpeta o unidad nada mas arrancar

Opciones de --demo:
  --stopped                    el arbol de la demostracion es el de un analisis detenido (carpetas incompletas)
  --stress                     (desarrollo) un mapa de ~20 000 celdas para medir el peor caso
  --screen home|scan|map|list  pantalla inicial (por defecto home)
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
  --screenshot <ruta.png>      guarda una captura de la ventana y sale (solo con la feature `capture`)
";

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Screen {
    #[default]
    Home,
    Scan,
    Map,
    List,
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
    pub theme: Option<ThemePref>,
    pub lang: Option<LangPref>,
    pub size: Option<(f32, f32)>,
    pub bench_hover: Option<f32>,
    pub screenshot: Option<PathBuf>,
    pub help: bool,
    pub version: bool,
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
            "--help" | "-h" | "-?" => cli.help = true,
            "--version" | "-V" => cli.version = true,
            "--screen" => {
                cli.screen = match need(&mut it, "--screen")?.as_str() {
                    "home" => Screen::Home,
                    "scan" => Screen::Scan,
                    "map" => Screen::Map,
                    "list" => Screen::List,
                    v => {
                        return Err(format!(
                            "--screen: valor desconocido `{v}` (home, scan, map o list)"
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
            "--screenshot" => cli.screenshot = Some(PathBuf::from(need(&mut it, "--screenshot")?)),
            other => return Err(format!("argumento desconocido: `{other}`")),
        }
    }
    if (cli.stopped
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
    if cli.scan.is_some() && cli.demo {
        return Err("--scan y --demo no se combinan".into());
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
    }

    #[test]
    fn ayuda_y_version() {
        assert!(p("--help").unwrap().help);
        assert!(p("-V").unwrap().version);
        assert_eq!(p("--scan X").unwrap().scan, Some(PathBuf::from("X")));
    }
}
