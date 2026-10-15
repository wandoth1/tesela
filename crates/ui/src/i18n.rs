//! Idiomas de la interfaz: español e inglés con `fluent-bundle` (PLAN.md §2.10). Ninguna cadena de la interfaz está
//! escrita en el código: todo sale de `i18n/es.ftl` y `i18n/en.ftl` por su identificador.
//!
//! * El idioma por defecto se detecta del sistema (`sys-locale`); cualquier configuración regional que no sea inglesa
//!   y no empiece por `es` cae en inglés (el idioma de reserva), y las `es-*` en español.
//! * `set_use_isolating(false)`: sin las marcas de aislamiento bidireccional (FSI/PDI), que egui dibuja como cajitas.
//! * Los números (tamaños, porcentajes, miles) se formatean aquí según el idioma: coma decimal y punto de miles en
//!   español; punto decimal y coma de miles en inglés.

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

const ES: &str = include_str!("../i18n/es.ftl");
const EN: &str = include_str!("../i18n/en.ftl");

/// Idioma de la interfaz.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Lang {
    #[serde(rename = "es")]
    Es,
    #[serde(rename = "en")]
    En,
}

impl Lang {
    /// Todos los idiomas.
    pub const ALL: [Lang; 2] = [Lang::Es, Lang::En];

    /// Código BCP 47.
    pub fn code(self) -> &'static str {
        match self {
            Lang::Es => "es",
            Lang::En => "en",
        }
    }

    /// Idioma para una configuración regional del sistema (`es-ES`, `en-US`, `fr-FR`...).
    pub fn from_locale(locale: &str) -> Lang {
        let l = locale.to_ascii_lowercase();
        if l == "es" || l.starts_with("es-") || l.starts_with("es_") {
            Lang::Es
        } else {
            Lang::En
        }
    }

    /// Idioma del sistema (inglés si no se puede saber).
    pub fn system() -> Lang {
        sys_locale::get_locale().map_or(Lang::En, |l| Lang::from_locale(&l))
    }
}

/// Textos de un idioma y formato de cifras.
pub struct Tr {
    lang: Lang,
    bundle: FluentBundle<FluentResource>,
}

impl std::fmt::Debug for Tr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tr({})", self.lang.code())
    }
}

impl Tr {
    /// Textos en `lang`.
    pub fn new(lang: Lang) -> Tr {
        let src = match lang {
            Lang::Es => ES,
            Lang::En => EN,
        };
        let res = FluentResource::try_new(src.to_string())
            .expect("el .ftl embebido es válido (lo prueba un test)");
        let id: LanguageIdentifier = lang.code().parse().expect("código de idioma válido");
        let mut bundle = FluentBundle::new(vec![id]);
        bundle.set_use_isolating(false);
        bundle
            .add_resource(res)
            .expect("sin identificadores repetidos (lo prueba un test)");
        Tr { lang, bundle }
    }

    /// El idioma activo.
    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// Texto sin argumentos. Si el identificador no existe devuelve el propio identificador (los tests lo impiden).
    pub fn t(&self, id: &str) -> String {
        self.format(id, None)
    }

    /// Texto con argumentos de cadena y numéricos.
    pub fn ta(&self, id: &str, args: &[(&str, Arg)]) -> String {
        let mut fa = FluentArgs::new();
        for (k, v) in args {
            match v {
                Arg::Str(s) => fa.set(*k, FluentValue::from(s.clone())),
                Arg::Num(n) => fa.set(*k, FluentValue::from(*n)),
            }
        }
        self.format(id, Some(&fa))
    }

    fn format(&self, id: &str, args: Option<&FluentArgs>) -> String {
        let Some(msg) = self.bundle.get_message(id) else {
            return id.to_string();
        };
        let Some(pattern) = msg.value() else {
            return id.to_string();
        };
        let mut errors = vec![];
        let s = self.bundle.format_pattern(pattern, args, &mut errors);
        debug_assert!(errors.is_empty(), "{id}: {errors:?}");
        s.into_owned()
    }

    /// ¿Existe el identificador?
    pub fn has(&self, id: &str) -> bool {
        self.bundle.has_message(id)
    }

    fn decimal(&self) -> char {
        match self.lang {
            Lang::Es => ',',
            Lang::En => '.',
        }
    }

    fn thousands(&self) -> char {
        match self.lang {
            Lang::Es => '.',
            Lang::En => ',',
        }
    }

    /// Entero con separador de miles (`1.214` en español, `1,214` en inglés).
    pub fn int(&self, n: u64) -> String {
        let digits = n.to_string();
        let mut out = String::with_capacity(digits.len() + digits.len() / 3);
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push(self.thousands());
            }
            out.push(c);
        }
        out
    }

    /// Decimal con `decimals` cifras y el separador del idioma.
    pub fn dec(&self, v: f64, decimals: usize) -> String {
        format!("{v:.decimals$}").replace('.', &self.decimal().to_string())
    }

    /// Tamaño en bytes con unidad base 1024 (`28,3 GB`). Por debajo de 1 KiB, bytes enteros.
    pub fn size(&self, bytes: u64) -> String {
        const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
        let mut v = bytes as f64;
        let mut i = 0;
        while v >= 1024.0 && i < UNITS.len() - 1 {
            v /= 1024.0;
            i += 1;
        }
        if i == 0 {
            format!("{bytes} B")
        } else {
            // Un decimal, salvo que al redondear salga 1024 (p. ej. 1023,96 KB): entonces ya es la unidad siguiente.
            let r = (v * 10.0).round() / 10.0;
            if r >= 1024.0 && i < UNITS.len() - 1 {
                format!("{} {}", self.dec(r / 1024.0, 1), UNITS[i + 1])
            } else {
                format!("{} {}", self.dec(v, 1), UNITS[i])
            }
        }
    }

    /// Tamaño con el prefijo "≥ " (un mínimo: el análisis no llegó a todo).
    pub fn size_min(&self, bytes: u64) -> String {
        format!("≥ {}", self.size(bytes))
    }

    /// Porcentaje entero (`19 %` en español, `19%` en inglés).
    pub fn pct(&self, p: f64) -> String {
        let n = p.round().clamp(0.0, 100.0) as u64;
        match self.lang {
            Lang::Es => format!("{n} %"),
            Lang::En => format!("{n}%"),
        }
    }

    /// Porcentaje con la parte decimal solo si es menor del 10 % (`3,4 %`, `27 %`); para cuotas pequeñas.
    pub fn pct_fine(&self, p: f64) -> String {
        if p > 0.0 && p < 10.0 {
            let s = self.dec(p, 1);
            match self.lang {
                Lang::Es => format!("{s} %"),
                Lang::En => format!("{s}%"),
            }
        } else {
            self.pct(p)
        }
    }
}

/// Argumento de un mensaje.
#[derive(Clone, Debug)]
pub enum Arg {
    Str(String),
    Num(i64),
}

impl From<String> for Arg {
    fn from(s: String) -> Arg {
        Arg::Str(s)
    }
}

impl From<&str> for Arg {
    fn from(s: &str) -> Arg {
        Arg::Str(s.to_string())
    }
}

impl From<i64> for Arg {
    fn from(n: i64) -> Arg {
        Arg::Num(n)
    }
}

impl From<u64> for Arg {
    fn from(n: u64) -> Arg {
        Arg::Num(n.min(i64::MAX as u64) as i64)
    }
}

impl From<usize> for Arg {
    fn from(n: usize) -> Arg {
        Arg::Num(n.min(i64::MAX as usize) as i64)
    }
}

/// `tr.ta(id, args!("clave" => valor, ...))`.
#[macro_export]
macro_rules! args {
    ($($k:literal => $v:expr),* $(,)?) => {
        &[$(($k, $crate::i18n::Arg::from($v))),*]
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_idioma_sale_de_la_configuracion_regional() {
        assert_eq!(Lang::from_locale("es-ES"), Lang::Es);
        assert_eq!(Lang::from_locale("es_MX"), Lang::Es);
        assert_eq!(Lang::from_locale("es"), Lang::Es);
        assert_eq!(Lang::from_locale("en-US"), Lang::En);
        assert_eq!(Lang::from_locale("fr-FR"), Lang::En);
        assert_eq!(Lang::from_locale(""), Lang::En);
        let _ = Lang::system();
    }

    #[test]
    fn los_numeros_siguen_el_idioma() {
        let es = Tr::new(Lang::Es);
        let en = Tr::new(Lang::En);
        assert_eq!(es.int(1_214), "1.214");
        assert_eq!(en.int(1_214), "1,214");
        assert_eq!(es.int(1_234_567), "1.234.567");
        assert_eq!(es.int(999), "999");
        assert_eq!(es.int(0), "0");
        assert_eq!(es.size(512), "512 B");
        assert_eq!(es.size(30_399_297_536), "28,3 GB");
        assert_eq!(en.size(30_399_297_536), "28.3 GB");
        assert_eq!(es.size(1024 * 1024), "1,0 MB");
        assert_eq!(es.size(1024 * 1024 * 1024 - 1), "1,0 GB");
        assert_eq!(es.size_min(1 << 30), "≥ 1,0 GB");
        assert_eq!(es.pct(19.4), "19 %");
        assert_eq!(en.pct(19.6), "20%");
        assert_eq!(es.pct(250.0), "100 %");
        assert_eq!(es.pct_fine(3.44), "3,4 %");
        assert_eq!(en.pct_fine(3.44), "3.4%");
        assert_eq!(es.pct_fine(27.0), "27 %");
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use std::collections::BTreeSet;

    const SOURCES: [(&str, &str); 14] = [
        ("app.rs", include_str!("app.rs")),
        ("demo.rs", include_str!("demo.rs")),
        ("describe.rs", include_str!("describe.rs")),
        ("details.rs", include_str!("details.rs")),
        ("explore.rs", include_str!("explore.rs")),
        ("fonts.rs", include_str!("fonts.rs")),
        ("home.rs", include_str!("home.rs")),
        ("icons.rs", include_str!("icons.rs")),
        ("list.rs", include_str!("list.rs")),
        ("map.rs", include_str!("map.rs")),
        ("platform.rs", include_str!("platform.rs")),
        ("scan.rs", include_str!("scan.rs")),
        ("theme.rs", include_str!("theme.rs")),
        ("widgets.rs", include_str!("widgets.rs")),
    ];

    /// Identificadores de mensaje de un `.ftl`: las líneas que empiezan por `id =` (sin sangría).
    fn ids(src: &str) -> BTreeSet<String> {
        src.lines()
            .filter(|l| !l.starts_with([' ', '#', '}', '\t']) && l.contains('='))
            .map(|l| l.split('=').next().unwrap().trim().to_string())
            .collect()
    }

    /// Literales de cadena de una fuente (sin escapes complicados: los textos de las claves no los llevan).
    fn literals(src: &str) -> Vec<String> {
        let mut out = vec![];
        let b = src.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'"' {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j] != b'"' {
                    if b[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                out.push(src[start..j.min(b.len())].to_string());
                i = j + 1;
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn los_dos_catalogos_tienen_exactamente_los_mismos_identificadores() {
        let (es, en) = (ids(ES), ids(EN));
        assert!(es.len() > 100);
        let only_es: Vec<_> = es.difference(&en).collect();
        let only_en: Vec<_> = en.difference(&es).collect();
        assert!(only_es.is_empty(), "solo en es.ftl: {only_es:?}");
        assert!(only_en.is_empty(), "solo en en.ftl: {only_en:?}");
        // Los dos se cargan sin errores (identificadores repetidos, sintaxis).
        for lang in Lang::ALL {
            let tr = Tr::new(lang);
            for id in &es {
                assert!(tr.has(id), "{lang:?}: {id}");
                let all = crate::args![
                    "name" => "x", "files" => "x", "size" => "x", "total" => "x", "time" => "x", "pct" => "x", "free" => "x",
                    "used" => "x", "ago" => "x", "explained" => "x", "done" => 1u64, "n" => 2u64, "count" => "2",
                    "parent" => "x", "error" => "x", "type" => "x", "label" => "x"
                ];
                assert!(!tr.ta(id, all).is_empty());
            }
        }
    }

    #[test]
    fn el_codigo_solo_usa_identificadores_que_existen_y_todos_se_usan() {
        let known = ids(ES);
        let mut used: BTreeSet<String> = BTreeSet::new();
        for (file, src) in SOURCES {
            if file == "icons.rs" {
                continue;
            }
            // `.t("id")` y `.ta("id", ...)`.
            for needle in [".t(\"", ".ta(\""] {
                let mut rest = src;
                while let Some(p) = rest.find(needle) {
                    let after = &rest[p + needle.len()..];
                    let id = &after[..after.find('"').unwrap()];
                    assert!(
                        known.contains(id),
                        "{file}: identificador inexistente `{id}`"
                    );
                    used.insert(id.to_string());
                    rest = after;
                }
            }
            // Identificadores indirectos (tablas de claves): cualquier literal que sea un identificador del catálogo.
            for lit in literals(src) {
                if known.contains(&lit) {
                    used.insert(lit);
                }
            }
        }
        let unused: Vec<_> = known.difference(&used).collect();
        assert!(unused.is_empty(), "identificadores sin usar: {unused:?}");
    }

    #[test]
    fn no_hay_cadenas_literales_de_interfaz_en_el_codigo() {
        // Llamadas que reciben el texto que verá el usuario: su primer argumento no puede ser un literal con letras.
        let calls = [
            "RichText::new(\"",
            "Label::new(\"",
            "Button::new(\"",
            ".label(\"",
            ".heading(\"",
            ".on_hover_text(\"",
            ".on_disabled_hover_text(\"",
            ".hint_text(\"",
            ".link(\"",
            ".selectable_label(",
            ".checkbox(",
            "section_label(ui, &pal, \"",
        ];
        for (file, src) in SOURCES {
            if file == "icons.rs" || file == "platform.rs" {
                continue;
            }
            // Fuera de los bloques de tests de cada archivo.
            let body = src.split("#[cfg(test)]").next().unwrap();
            for c in calls {
                let mut rest = body;
                while let Some(p) = rest.find(c) {
                    let after = &rest[p + c.len()..];
                    if c.ends_with('"') {
                        let lit = &after[..after.find('"').unwrap()];
                        assert!(
                            !lit.chars().any(char::is_alphabetic),
                            "{file}: texto de interfaz escrito en el código: {c}{lit}\""
                        );
                    } else {
                        // `selectable_label(selected, tr.t(...))`: el segundo argumento debe salir del catálogo.
                        let line = &after[..after.find('\n').unwrap_or(after.len())];
                        assert!(!line.contains(", \""), "{file}: literal en {c}{line}");
                    }
                    rest = after;
                }
            }
        }
    }

    #[test]
    fn los_plurales_y_los_argumentos_funcionan_en_los_dos_idiomas() {
        let es = Tr::new(Lang::Es);
        let en = Tr::new(Lang::En);
        let one = |tr: &Tr| tr.ta("files-count", crate::args!["n" => 1u64, "count" => "1"]);
        let many = |tr: &Tr| tr.ta("files-count", crate::args!["n" => 142u64, "count" => "142"]);
        assert_eq!(one(&es), "1 archivo");
        assert_eq!(many(&es), "142 archivos");
        assert_eq!(one(&en), "1 file");
        assert_eq!(many(&en), "142 files");
        assert_eq!(
            es.ta(
                "tip-pct-of",
                crate::args!["pct" => "19 %", "parent" => "usuario"]
            ),
            "19 % de usuario"
        );
        // Sin marcas de aislamiento bidireccional (egui las dibujaría como cajitas).
        let s = es.ta("analyze-drive", crate::args!["name" => "Sistema (C:)"]);
        assert_eq!(s, "Analizar Sistema (C:)");
        assert!(!s.contains('\u{2068}') && !s.contains('\u{2069}'));
    }
}
