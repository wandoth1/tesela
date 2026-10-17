//! Interface languages: Spanish and English with `fluent-bundle` (PLAN.md §2.10). No interface string is
//! written in the code: everything comes out of `i18n/es.ftl` and `i18n/en.ftl` by its identifier.
//!
//! * The default language comes from the system (`sys-locale`). Any locale that is neither English nor starts with `es`
//!   defaults to English; `es-*` selects Spanish.
//! * `set_use_isolating(false)`: without the bidirectional isolation marks (FSI/PDI), which egui draws as little boxes.
//! * Numbers (sizes, percentages, thousands) are formatted here depending on the language: decimal point and thousands point in
//!   Spanish; a decimal point and comma thousands separator in English.

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

const ES: &str = include_str!("../i18n/es.ftl");
const EN: &str = include_str!("../i18n/en.ftl");

/// Interface language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Lang {
    #[serde(rename = "es")]
    Es,
    #[serde(rename = "en")]
    En,
}

impl Lang {
    /// All languages.
    pub const ALL: [Lang; 2] = [Lang::Es, Lang::En];

    /// BCP code 47.
    pub fn code(self) -> &'static str {
        match self {
            Lang::Es => "es",
            Lang::En => "en",
        }
    }

    /// Language for a system locale (`es-ES`, `en-US`, `fr-FR`...).
    pub fn from_locale(locale: &str) -> Lang {
        let l = locale.to_ascii_lowercase();
        if l == "es" || l.starts_with("es-") || l.starts_with("es_") {
            Lang::Es
        } else {
            Lang::En
        }
    }

    /// System language (English if you can't know).
    pub fn system() -> Lang {
        sys_locale::get_locale().map_or(Lang::En, |l| Lang::from_locale(&l))
    }
}

/// Texts of one language and format of figures.
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
    /// Texts in `lang`.
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

    /// The active language.
    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// Text without arguments. If the identifier does not exist, it returns the identifier itself (the tests prevent this).
    pub fn t(&self, id: &str) -> String {
        self.format(id, None)
    }

    /// Text with string and numeric arguments.
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

    /// Does the identifier exist?
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

    /// Integer with thousands separator (`1.214` in Spanish, `1,214` in English).
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

    /// Decimal with `decimals` figures and the language separator.
    pub fn dec(&self, v: f64, decimals: usize) -> String {
        format!("{v:.decimals$}").replace('.', &self.decimal().to_string())
    }

    /// Size in bytes with base unit 1024 (`28,3 GB`). Below 1 KiB, integer bytes.
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
            // A decimal, unless rounding results in 1024 (e.g. 1023.96 KB): then it is the next unit.
            let r = (v * 10.0).round() / 10.0;
            if r >= 1024.0 && i < UNITS.len() - 1 {
                format!("{} {}", self.dec(r / 1024.0, 1), UNITS[i + 1])
            } else {
                format!("{} {}", self.dec(v, 1), UNITS[i])
            }
        }
    }

    /// Size with the prefix "≥ " (a minimum: the analysis did not reach everything).
    pub fn size_min(&self, bytes: u64) -> String {
        format!("≥ {}", self.size(bytes))
    }

    /// Integer percentage (`19 %` in Spanish, `19%` in English).
    pub fn pct(&self, p: f64) -> String {
        let n = p.round().clamp(0.0, 100.0) as i64;
        self.ta("percent", &[("n", Arg::Num(n))])
    }

    /// Percentage with decimal part only if less than 10% (`3,4 %`, `27 %`); for small installments.
    pub fn pct_fine(&self, p: f64) -> String {
        if p > 0.0 && p < 10.0 {
            self.ta("percent-fine", &[("v", Arg::Str(self.dec(p, 1)))])
        } else {
            self.pct(p)
        }
    }
}

/// Argument of a message.
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

    const SOURCES: [(&str, &str); 19] = [
        ("app.rs", include_str!("app.rs")),
        ("basket.rs", include_str!("basket.rs")),
        ("clean.rs", include_str!("clean.rs")),
        ("demo.rs", include_str!("demo.rs")),
        ("demo_backend.rs", include_str!("demo_backend.rs")),
        ("describe.rs", include_str!("describe.rs")),
        ("details.rs", include_str!("details.rs")),
        ("explore.rs", include_str!("explore.rs")),
        ("fonts.rs", include_str!("fonts.rs")),
        ("home.rs", include_str!("home.rs")),
        ("icons.rs", include_str!("icons.rs")),
        ("list.rs", include_str!("list.rs")),
        ("map.rs", include_str!("map.rs")),
        ("multisel.rs", include_str!("multisel.rs")),
        ("platform.rs", include_str!("platform.rs")),
        ("recs.rs", include_str!("recs.rs")),
        ("scan.rs", include_str!("scan.rs")),
        ("theme.rs", include_str!("theme.rs")),
        ("widgets.rs", include_str!("widgets.rs")),
    ];

    /// The interface sources, plus the NON-testing part of this same file (use `percent`).
    fn all_sources() -> Vec<(&'static str, &'static str)> {
        let mut v = SOURCES.to_vec();
        // `about.rs` without its evidence (which quotes text from the notices).
        v.push((
            "about.rs",
            include_str!("about.rs")
                .split("#[cfg(test)]")
                .next()
                .unwrap(),
        ));
        // `error.rs` without its proofs (which cite the texts in both languages).
        v.push((
            "error.rs",
            include_str!("error.rs")
                .split("#[cfg(test)]")
                .next()
                .unwrap(),
        ));
        v.push((
            "i18n.rs",
            include_str!("i18n.rs")
                .split("#[cfg(test)]")
                .next()
                .unwrap(),
        ));
        v
    }

    /// Message identifiers for a `.ftl`: lines starting with `id =` (without indentation).
    fn ids(src: &str) -> BTreeSet<String> {
        src.lines()
            .filter(|l| !l.starts_with([' ', '#', '}', '\t']) && l.contains('='))
            .map(|l| l.split('=').next().unwrap().trim().to_string())
            .collect()
    }

    /// String literals from a source (no complicated escaping: key texts don't have them).
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
        // Both load without errors (repeated identifiers, syntax).
        for lang in Lang::ALL {
            let tr = Tr::new(lang);
            for id in &es {
                assert!(tr.has(id), "{lang:?}: {id}");
                let all = crate::args![
                    "name" => "x", "files" => "x", "size" => "x", "total" => "x", "time" => "x", "pct" => "x", "free" => "x", "avail" => "x", "detail" => "x",
                    "used" => "x", "ago" => "x", "explained" => "x", "done" => 1u64, "n" => 2u64, "count" => "2",
                    "parent" => "x", "error" => "x", "type" => "x", "label" => "x", "v" => "x", "bin" => "x", "folder" => "x", "code" => "x"
                ];
                assert!(!tr.ta(id, all).is_empty());
            }
        }
    }

    /// No percentage is written by hand: the space before `%` changes with the language (`19 %` in es, `19%` in en) and
    /// decides Fluent (`Tr::pct` and `Tr::pct_fine`). Any string literal in the code with a `%` fails.
    #[test]
    fn ningun_porcentaje_se_escribe_a_mano_en_el_codigo() {
        let mut bad = Vec::new();
        for (file, src) in all_sources() {
            // Only production code (tests for each module can quote already formatted texts).
            let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
            let prod: String = prod
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            for lit in literals(&prod) {
                if lit.contains('%') {
                    bad.push(format!("{file}: {lit:?}"));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "usa `tr.pct(..)` o `tr.pct_fine(..)` en vez de escribir `%`: {bad:?}"
        );
    }

    #[test]
    fn el_codigo_solo_usa_identificadores_que_existen_y_todos_se_usan() {
        let known = ids(ES);
        let mut used: BTreeSet<String> = BTreeSet::new();
        for (file, src) in all_sources() {
            if file == "icons.rs" {
                continue;
            }
            // `.t("id")` and `.ta("id", ...)`.
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
            // Indirect identifiers (key tables): Any literal that is a catalog identifier.
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
        // Calls that receive the text the user will see: Their first argument cannot be a lettered literal.
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
        for (file, src) in all_sources() {
            if file == "icons.rs" || file == "platform.rs" {
                continue;
            }
            // Outside the test blocks of each file.
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
                        // `selectable_label(selected, tr.t(...))`: The second argument must come out of the catalog.
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
        // Without bidirectional isolation marks (I would draw them as little boxes).
        let s = es.ta("analyze-drive", crate::args!["name" => "Sistema (C:)"]);
        assert_eq!(s, "Analizar Sistema (C:)");
        assert!(!s.contains('\u{2068}') && !s.contains('\u{2069}'));
    }
}
