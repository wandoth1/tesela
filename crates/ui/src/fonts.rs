//! System fonts loaded at runtime (PLAN.md §2.10). NO sources are redistributed: not even those from
//! Windows nor those from egui (it is compiled without `default_fonts`, so in `.exe` there are no OFL or Ubuntu sources).
//!
//! * Main font: `SegUIVar.ttf` (variable; egui applies the `wght` axis, with weights 400 and 600 in one file),
//!   falling back to Tahoma or Arial if unavailable. Segoe UI Variable covers Latin, Greek, and Cyrillic; Arabic and
//!   Hebrew are loaded from `segoeui.ttf` on demand.
//! * CJK and other fallback fonts are loaded on demand because they are 14–20 MB and read into memory. When a name or
//!   search result uses that script, the UI loads Yu Gothic (kana and kanji), YaHei/JhengHei (hanzi), Malgun Gothic
//!   (Hangul), Leelawadee UI (Thai and Lao), Nirmala UI (Indic scripts), or Segoe UI Symbol (symbols; color emoji are
//!   not supported). Available fallback fonts are appended to the Proportional and Monospaced families.
//! * For routes, Consolas is used if it exists.
//!
//! If there is NO usable font ([`available`] returns `false`) the application cannot draw text: the
//! executable warns you and exits instead of hanging in a panic.

use crate::platform;
use egui::epaint::text::VariationCoords;
use egui::{Context, FontData, FontDefinitions, FontFamily, FontTweak};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Bytes from a system source, shared by all weights and by all calls: they are read and "set" a
/// only once per file (`Box::leak`, duration equal to the duration of the process, which is the duration of a loaded source). Without
/// this each weight of `SegUIVar.ttf` (1.8 MB) would have its own copy in memory.
fn shared_bytes(path: &Path) -> Option<&'static [u8]> {
    static CACHE: Mutex<Vec<(PathBuf, &'static [u8])>> = Mutex::new(Vec::new());
    let mut cache = CACHE.lock().ok()?;
    if let Some((_, b)) = cache.iter().find(|(p, _)| p == path) {
        return Some(b);
    }
    let bytes: &'static [u8] = Box::leak(std::fs::read(path).ok()?.into_boxed_slice());
    cache.push((path.to_path_buf(), bytes));
    Some(bytes)
}

/// Which main source ended up loaded (for the tests and for the report).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loaded {
    /// `SegUIVar.ttf`: weights 400 and 600.
    SegoeVariable,
    /// `segoeui.ttf` and `segoeuib.ttf`.
    SegoeStatic,
    /// Tahoma or Arial (regular and bold).
    OtherSystem,
    /// None: Text cannot be drawn.
    None,
}

/// Scripts that need a fallback source (flag set).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scripts(u8);

impl Scripts {
    pub const NONE: Scripts = Scripts(0);
    /// Hanzi/kanji and CJK punctuation.
    pub const HAN: Scripts = Scripts(1);
    pub const KANA: Scripts = Scripts(2);
    pub const HANGUL: Scripts = Scripts(4);
    /// Thai and Lao.
    pub const THAI: Scripts = Scripts(8);
    /// Devanagari, Bengali, Tamil, Telugu... (even Sinhala).
    pub const INDIC: Scripts = Scripts(16);
    /// Arrows, mathematical symbols and various.
    pub const SYMBOLS: Scripts = Scripts(32);
    pub const EMOJI: Scripts = Scripts(64);
    /// Arabic and Hebrew (Segoe UI Variable does not have them; `segoeui.ttf` does).
    pub const RTL: Scripts = Scripts(128);

    pub fn contains(self, other: Scripts) -> bool {
        self.0 & other.0 == other.0
    }
    pub fn union(self, other: Scripts) -> Scripts {
        Scripts(self.0 | other.0)
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Scripts {
    type Output = Scripts;
    fn bitor(self, o: Scripts) -> Scripts {
        self.union(o)
    }
}

/// Writing a character that the main font does NOT cover (Latin, Greek and Cyrillic do).
pub fn script_of(c: char) -> Scripts {
    match c as u32 {
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => Scripts::KANA,
        0x2E80..=0x303F
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFF65
        | 0x20000..=0x2FFFF => Scripts::HAN,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => Scripts::HANGUL,
        0x0590..=0x06FF | 0x0750..=0x077F | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF => Scripts::RTL,
        0x0E00..=0x0EFF => Scripts::THAI,
        0x0900..=0x0DFF => Scripts::INDIC,
        0x2190..=0x2BFF => Scripts::SYMBOLS,
        0x1F000..=0x1FAFF => Scripts::EMOJI,
        _ => Scripts::NONE,
    }
}

/// Writings of a text.
pub fn scripts_of_str(s: &str) -> Scripts {
    s.chars().fold(Scripts::NONE, |a, c| a | script_of(c))
}

/// Writes a name arena in WTF-8 (what keeps the tree). Loops through the bytes once and only decodes the
/// which are not ASCII, so a tree of millions of Latin names costs a few tens of milliseconds.
pub fn scripts_of_wtf8(bytes: &[u8]) -> Scripts {
    let mut acc = Scripts::NONE;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            i += 1;
            continue;
        }
        let (len, init) = match b {
            0xC0..=0xDF => (2, (b & 0x1F) as u32),
            0xE0..=0xEF => (3, (b & 0x0F) as u32),
            0xF0..=0xF7 => (4, (b & 0x07) as u32),
            _ => {
                i += 1;
                continue;
            }
        };
        if i + len > bytes.len() {
            break;
        }
        let cp = bytes[i + 1..i + len]
            .iter()
            .fold(init, |a, &c| (a << 6) | (c & 0x3F) as u32);
        if let Some(c) = char::from_u32(cp) {
            acc = acc | script_of(c);
        }
        i += len;
    }
    acc
}

/// Files that, even if they exist, egui 0.36 cannot use (they do not provide any glyphs). The tests do not have them: a team
/// If you only have those fonts (for example a Windows Server without MS Gothic or YaHei) you should skip the font check.
/// writing, not failing.
#[cfg(test)]
pub(crate) const UNUSABLE_IN_EGUI: [&str; 3] = ["YuGothR.ttc", "LeelawUI.ttf", "leelawad.ttf"];

/// `s` fallback groups for which `dir` does not have any egui-usable sources (empty = all covered).
#[cfg(test)]
pub(crate) fn missing_groups(dir: &Path, s: Scripts) -> Vec<&'static str> {
    fallback_groups(s)
        .iter()
        .filter(|(_, c)| {
            !c.iter()
                .any(|f| dir.join(f).exists() && !UNUSABLE_IN_EGUI.contains(f))
        })
        .map(|(g, _)| *g)
        .collect()
}

/// Backup font files (in the `fonts` folder) for a set of scripts, in order of preference;
/// the first available font in each group is used. `(egui font family name, candidate fonts)`.
pub(crate) fn fallback_groups(s: Scripts) -> Vec<(&'static str, &'static [&'static str])> {
    let mut v: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if s.contains(Scripts::KANA) {
        // Yu Gothic (`YuGothR.ttc`) is NOT loaded in egui 0.36 (does not give any glyph): it is used if there is no other. MS Gothic and Meiryo
        // Yeah; Microsoft YaHei (which is in every installation) also comes with kana.
        v.push((
            "Tesela-kana",
            &["meiryo.ttc", "msgothic.ttc", "YuGothR.ttc", "msyh.ttc"],
        ));
    }
    if s.contains(Scripts::HAN) {
        // ALWAYS after the Japanese (if there is one): MS Gothic or Meiryo cover the kanji, but not the characters only of the
        // simplified Chinese (like 夹), which would appear as a box in a tree with Japanese and Chinese at the same time.
        v.push((
            "Tesela-han",
            &["msyh.ttc", "msjh.ttc", "YuGothR.ttc", "simsun.ttc"],
        ));
    }
    if s.contains(Scripts::HANGUL) {
        v.push(("Tesela-hangul", &["malgun.ttf", "batang.ttc"]));
    }
    if s.contains(Scripts::THAI) {
        v.push((
            "Tesela-thai",
            &["tahoma.ttf", "LeelawUI.ttf", "leelawad.ttf"],
        ));
    }
    if s.contains(Scripts::INDIC) {
        v.push((
            "Tesela-indic",
            &["Nirmala.ttc", "Nirmala.ttf", "mangal.ttf"],
        ));
    }
    if s.contains(Scripts::RTL) {
        v.push(("Tesela-rtl", &["segoeui.ttf", "tahoma.ttf", "arial.ttf"]));
    }
    // Emoji: egui does not draw color fonts (Segoe UI Emoji), so you only get what Segoe UI Symbol brings in
    // monochrome; the rest of the emoji of a name appear as a box (known limitation, see the report).
    if s.contains(Scripts::SYMBOLS) || s.contains(Scripts::EMOJI) {
        v.push(("Tesela-symbols", &["seguisym.ttf"]));
    }
    v
}

/// Builds font definitions from the system font folder `dir` (injected by the test) and
/// the writings that must be covered in addition to the main one.
pub fn definitions(dir: Option<&Path>, scripts: Scripts) -> (FontDefinitions, Loaded) {
    let mut fd = FontDefinitions::empty();
    fd.families.insert(FontFamily::Proportional, Vec::new());
    fd.families.insert(FontFamily::Monospace, Vec::new());
    let read = |name: &str| dir.and_then(|d| shared_bytes(&d.join(name)));
    let mut loaded = Loaded::None;

    if let Some(bytes) = read("SegUIVar.ttf") {
        for (key, w) in [("Tesela-regular", 400.0f32), ("Tesela-semibold", 600.0)] {
            let tweak = FontTweak {
                coords: VariationCoords::new([("wght", w)]),
                ..Default::default()
            };
            fd.font_data.insert(
                key.into(),
                Arc::new(FontData::from_static(bytes).tweak(tweak)),
            );
        }
        loaded = Loaded::SegoeVariable;
    } else {
        for (reg, bold, kind) in [
            ("segoeui.ttf", "segoeuib.ttf", Loaded::SegoeStatic),
            ("tahoma.ttf", "tahomabd.ttf", Loaded::OtherSystem),
            ("arial.ttf", "arialbd.ttf", Loaded::OtherSystem),
        ] {
            if let (Some(r), Some(b)) = (read(reg), read(bold)) {
                fd.font_data
                    .insert("Tesela-regular".into(), Arc::new(FontData::from_static(r)));
                fd.font_data
                    .insert("Tesela-semibold".into(), Arc::new(FontData::from_static(b)));
                loaded = kind;
                break;
            }
        }
    }
    if loaded == Loaded::None {
        return (fd, loaded);
    }

    // Deed reserves (those that exist).
    let mut fallback_names: Vec<String> = Vec::new();
    let mut chosen: Vec<&str> = Vec::new();
    for (name, candidates) in fallback_groups(scripts) {
        // The same file is not loaded twice (YaHei can be both kana's and han's reservation).
        if let Some((file, bytes)) = candidates.iter().find_map(|c| read(c).map(|b| (*c, b)))
            && !chosen.contains(&file)
        {
            chosen.push(file);
            fd.font_data
                .insert(name.into(), Arc::new(FontData::from_static(bytes)));
            fallback_names.push(name.to_string());
        }
    }
    if let Some(bytes) = read("consola.ttf") {
        fd.font_data
            .insert("Tesela-mono".into(), Arc::new(FontData::from_static(bytes)));
    }

    let mut prop = vec!["Tesela-regular".to_string()];
    prop.extend(fallback_names.iter().cloned());
    let mut semibold = vec!["Tesela-semibold".to_string()];
    semibold.extend(fallback_names.iter().cloned());
    let mut mono = Vec::new();
    if fd.font_data.contains_key("Tesela-mono") {
        mono.push("Tesela-mono".to_string());
    }
    mono.push("Tesela-regular".to_string());
    mono.extend(fallback_names.iter().cloned());
    fd.families.insert(FontFamily::Proportional, prop);
    fd.families
        .insert(FontFamily::Name("semibold".into()), semibold);
    fd.families.insert(FontFamily::Monospace, mono);
    (fd, loaded)
}

/// Is there any usable system font?
pub fn available() -> bool {
    let dir = platform::fonts_dir();
    definitions(dir.as_deref(), Scripts::NONE).1 != Loaded::None
}

/// Loads the system fonts (and those from `scripts`) into `ctx` and returns which one is the primary one. The changes are valid from
/// egui's NEXT pass.
pub fn install(ctx: &Context, scripts: Scripts) -> Loaded {
    let dir = platform::fonts_dir();
    let (fd, loaded) = definitions(dir.as_deref(), scripts);
    if loaded != Loaded::None {
        ctx.set_fonts(fd);
    }
    loaded
}

/// Like [`install`], and also applies the sources already (with an empty pass). For tests and for whoever needs to measure
/// text immediately.
pub fn install_now(ctx: &Context, scripts: Scripts) -> Loaded {
    let loaded = install(ctx, scripts);
    let mut out = ctx.run_ui(Default::default(), |_| {});
    out.textures_delta.clear();
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::FontId;

    #[test]
    fn sin_carpeta_de_fuentes_no_hay_fuente_y_no_se_inventa_ninguna() {
        let (fd, loaded) = definitions(None, Scripts::NONE);
        assert_eq!(loaded, Loaded::None);
        assert!(fd.font_data.is_empty(), "no se embebe ninguna fuente");
        let (_, loaded) = definitions(Some(Path::new("no-existe-esta-carpeta")), Scripts::NONE);
        assert_eq!(loaded, Loaded::None);
    }

    #[test]
    fn con_las_fuentes_del_sistema_se_cargan_dos_pesos_y_las_reservas_van_detras() {
        let dir = platform::fonts_dir().expect("carpeta de fuentes");
        let (fd, loaded) = definitions(Some(&dir), Scripts::NONE);
        assert_ne!(
            loaded,
            Loaded::None,
            "ni SegUIVar, ni segoeui, ni Tahoma, ni Arial"
        );
        assert_eq!(fd.families[&FontFamily::Proportional][0], "Tesela-regular");
        assert_eq!(
            fd.families[&FontFamily::Name("semibold".into())][0],
            "Tesela-semibold"
        );
        assert!(available());
        // Without requested writes, no reservation (memory) is loaded.
        assert_eq!(fd.families[&FontFamily::Proportional].len(), 1);
        // With them, they go after the main one in the three families (if the computer has any of those sources: a Windows
        // Minimal Server may not have any, and then there is nothing to check).
        let wanted = Scripts::KANA | Scripts::THAI | Scripts::SYMBOLS;
        let (fd, _) = definitions(Some(&dir), wanted);
        if !fallback_groups(wanted)
            .iter()
            .any(|(_, c)| c.iter().any(|f| dir.join(f).exists()))
        {
            eprintln!(
                "sin ninguna fuente de reserva para kana, tailandés o símbolos en este equipo: se omite"
            );
            return;
        }
        for fam in [
            FontFamily::Proportional,
            FontFamily::Name("semibold".into()),
            FontFamily::Monospace,
        ] {
            let names = &fd.families[&fam];
            assert!(names.len() > 1, "{fam:?}: {names:?}");
            assert!(names.iter().all(|n| fd.font_data.contains_key(n)));
        }
        assert!(
            !fd.font_data
                .keys()
                .any(|k| k.contains("Ubuntu") || k.contains("Hack"))
        );
    }

    #[test]
    fn un_equipo_sin_las_fuentes_de_reserva_las_declara_ausentes_para_saltarse_la_prueba() {
        // FAKE Fonts Folder (`%TEMP%\tesela-test-<uuid>` only, guarded): A minimal Windows Server.
        let dir = crate::testdir::TestDir::new();
        let all = Scripts::KANA
            | Scripts::HANGUL
            | Scripts::THAI
            | Scripts::INDIC
            | Scripts::RTL
            | Scripts::SYMBOLS;
        // Empty: all groups are missing.
        assert_eq!(missing_groups(&dir, all).len(), fallback_groups(all).len());
        // With only the fonts that egui cannot use, they are still missing (not counted).
        for f in UNUSABLE_IN_EGUI {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        assert_eq!(missing_groups(&dir, all).len(), fallback_groups(all).len());
        // With one usable per group, none are missing.
        for f in [
            "msyh.ttc",
            "malgun.ttf",
            "tahoma.ttf",
            "Nirmala.ttc",
            "segoeui.ttf",
            "seguisym.ttf",
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        assert!(missing_groups(&dir, all).is_empty());
        // Without the symbol font, only that group is missing.
        std::fs::remove_file(dir.join("seguisym.ttf")).unwrap();
        assert_eq!(missing_groups(&dir, all), vec!["Tesela-symbols"]);
    }

    #[test]
    fn con_japones_y_chino_a_la_vez_se_cargan_las_dos_reservas_y_sin_duplicar_el_archivo() {
        let dir = crate::testdir::TestDir::new();
        for f in ["segoeui.ttf", "segoeuib.ttf", "msgothic.ttc", "msyh.ttc"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let both = Scripts::KANA | Scripts::HAN;
        let (fd, _) = definitions(Some(dir.path()), both);
        let prop = &fd.families[&FontFamily::Proportional];
        // Japanese first and Chinese later (for simplified characters only).
        let kana = prop.iter().position(|n| n == "Tesela-kana").expect("kana");
        let han = prop.iter().position(|n| n == "Tesela-han").expect("han");
        assert!(kana < han, "{prop:?}");
        // If the only CJK source is YaHei, it works for both and is not loaded twice.
        // (another folder: the bytes of each path are shared and remembered during the process)
        let dir2 = crate::testdir::TestDir::new();
        for f in ["segoeui.ttf", "segoeuib.ttf", "msyh.ttc"] {
            std::fs::write(dir2.join(f), b"x").unwrap();
        }
        let (fd, _) = definitions(Some(dir2.path()), both);
        let prop = &fd.families[&FontFamily::Proportional];
        assert_eq!(
            prop.iter()
                .filter(|n| n.starts_with("Tesela-k") || n.starts_with("Tesela-h"))
                .count(),
            1,
            "{prop:?}"
        );
    }

    #[test]
    fn deteccion_de_escrituras() {
        assert_eq!(
            scripts_of_str("Documentos ñ é ß Ελληνικά Кириллица"),
            Scripts::NONE
        );
        assert!(scripts_of_str("العربية").contains(Scripts::RTL));
        assert!(scripts_of_str("עברית").contains(Scripts::RTL));
        assert!(scripts_of_str("日本語").contains(Scripts::HAN));
        assert!(scripts_of_str("ひらがな").contains(Scripts::KANA));
        assert!(scripts_of_str("カタカナ").contains(Scripts::KANA));
        assert!(scripts_of_str("한국어").contains(Scripts::HANGUL));
        assert!(scripts_of_str("สวัสดี").contains(Scripts::THAI));
        assert!(scripts_of_str("नमस्ते").contains(Scripts::INDIC));
        assert!(scripts_of_str("→ ★").contains(Scripts::SYMBOLS));
        assert!(scripts_of_str("📁").contains(Scripts::EMOJI));
        // The WTF-8 arena is the same as the text.
        for s in [
            "日本語 y ひらがな",
            "العربية",
            "한국어",
            "สวัสดี",
            "नमस्ते",
            "plano ascii",
            "📁 → mezcla ñ",
        ] {
            assert_eq!(scripts_of_wtf8(s.as_bytes()), scripts_of_str(s), "{s}");
        }
        // Loose or truncated bytes don't hurt.
        let _ = scripts_of_wtf8(&[0xE6, 0x97]);
        let _ = scripts_of_wtf8(&[0xFF, 0x80, 0xC3]);
    }

    #[test]
    fn las_fuentes_instaladas_miden_texto_con_acentos() {
        let ctx = Context::default();
        let loaded = install_now(&ctx, Scripts::NONE);
        assert_ne!(loaded, Loaded::None);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let g = ui.painter().layout_no_wrap(
                "Vídeos · ≥ 12,3 GB · Descargas".into(),
                FontId::proportional(14.0),
                egui::Color32::WHITE,
            );
            assert!(g.size().x > 50.0);
            let g2 = ui.painter().layout_no_wrap(
                "Vídeos · ≥ 12,3 GB · Descargas".into(),
                crate::theme::font_semibold(14.0),
                egui::Color32::WHITE,
            );
            assert!(g2.size().x > 50.0);
            let g3 = ui.painter().layout_no_wrap(
                "C:\\ruta\\ñ".into(),
                FontId::monospace(12.0),
                egui::Color32::WHITE,
            );
            assert!(g3.size().x > 20.0);
        });
        out.textures_delta.clear();
    }
}
