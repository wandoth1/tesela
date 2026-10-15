//! Fuentes del sistema cargadas en tiempo de ejecución (PLAN.md §2.10). NO se redistribuye ninguna fuente: ni las de
//! Windows ni las de egui (se compila sin `default_fonts`, así que en el `.exe` no hay fuentes OFL ni Ubuntu).
//!
//! * Principal: `SegUIVar.ttf` (variable: egui aplica el eje `wght`; DOS pesos, 400 y 600, compartiendo los bytes), si
//!   no `segoeui.ttf` + `segoeuib.ttf`, si no Tahoma o Arial. Segoe UI Variable cubre latino, griego y cirílico; árabe y hebreo salen de `segoeui.ttf` bajo demanda.
//! * Reserva POR ESCRITURA y bajo demanda: las fuentes CJK pesan 14-20 MB y se leen a memoria, así que solo se cargan
//!   cuando un nombre (o el buscador) trae esa escritura: Yu Gothic (kana y kanji), YaHei/JhengHei (hanzi), Malgun Gothic
//!   (hangul), Leelawadee UI (tailandés y lao), Nirmala UI (escrituras índicas), Segoe UI Symbol (símbolos; los emoji en color no se pueden dibujar).
//!   Van al final de las familias Proporcional y Monoespaciada, y las que no existan se omiten.
//! * Para las rutas se usa Consolas si existe.
//!
//! Si no hay NINGUNA fuente utilizable ([`available`] devuelve `false`) la aplicación no puede dibujar texto: el
//! ejecutable lo avisa y sale en vez de colgarse en un pánico de egui.

use crate::platform;
use egui::epaint::text::VariationCoords;
use egui::{Context, FontData, FontDefinitions, FontFamily, FontTweak};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Bytes de una fuente del sistema, compartidos por todos los pesos y por todas las llamadas: se leen y se "fijan" una
/// sola vez por archivo (`Box::leak`, de duración igual a la del proceso, que es lo que dura una fuente cargada). Sin
/// esto cada peso de `SegUIVar.ttf` (1,8 MB) tendría su propia copia en memoria.
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

/// Qué fuente principal acabó cargada (para los tests y para el informe).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loaded {
    /// `SegUIVar.ttf`: pesos 400 y 600.
    SegoeVariable,
    /// `segoeui.ttf` y `segoeuib.ttf`.
    SegoeStatic,
    /// Tahoma o Arial (regular y negrita).
    OtherSystem,
    /// Ninguna: no se puede dibujar texto.
    None,
}

/// Escrituras que necesitan una fuente de reserva (conjunto de banderas).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scripts(u8);

impl Scripts {
    pub const NONE: Scripts = Scripts(0);
    /// Hanzi/kanji y la puntuación CJK.
    pub const HAN: Scripts = Scripts(1);
    pub const KANA: Scripts = Scripts(2);
    pub const HANGUL: Scripts = Scripts(4);
    /// Tailandés y lao.
    pub const THAI: Scripts = Scripts(8);
    /// Devanagari, bengalí, tamil, telugu... (hasta el cingalés).
    pub const INDIC: Scripts = Scripts(16);
    /// Flechas, símbolos matemáticos y varios.
    pub const SYMBOLS: Scripts = Scripts(32);
    pub const EMOJI: Scripts = Scripts(64);
    /// Árabe y hebreo (Segoe UI Variable no los lleva; `segoeui.ttf` sí).
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

/// Escritura de un carácter que la fuente principal NO cubre (latino, griego y cirílico sí).
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

/// Escrituras de un texto.
pub fn scripts_of_str(s: &str) -> Scripts {
    s.chars().fold(Scripts::NONE, |a, c| a | script_of(c))
}

/// Escrituras de un arena de nombres en WTF-8 (lo que guarda el árbol). Recorre los bytes una vez y solo decodifica los
/// que no son ASCII, así que un árbol de millones de nombres latinos cuesta unas decenas de milisegundos.
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

/// Archivos que, aunque existan, egui 0.36 no consigue usar (no dan ningún glifo). Los tests no cuentan con ellos: un equipo
/// que solo tenga esas fuentes (por ejemplo un Windows Server sin MS Gothic ni YaHei) debe saltarse la comprobación de la
/// escritura, no fallar.
#[cfg(test)]
pub(crate) const UNUSABLE_IN_EGUI: [&str; 3] = ["YuGothR.ttc", "LeelawUI.ttf", "leelawad.ttf"];

/// Grupos de reserva de `s` para los que `dir` no tiene ninguna fuente utilizable por egui (vacío = todo cubierto).
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

/// Archivos de reserva (en la carpeta de fuentes) para un conjunto de escrituras, por orden de preferencia; de cada
/// grupo se usa el primero que exista. `(nombre de la fuente en egui, candidatos)`.
pub(crate) fn fallback_groups(s: Scripts) -> Vec<(&'static str, &'static [&'static str])> {
    let mut v: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if s.contains(Scripts::KANA) {
        // Yu Gothic (`YuGothR.ttc`) NO se carga en egui 0.36 (no da ningún glifo): se usa si no hay otra. MS Gothic y Meiryo
        // sí; Microsoft YaHei (que está en toda instalación) trae también kana.
        v.push((
            "Tesela-kana",
            &["meiryo.ttc", "msgothic.ttc", "YuGothR.ttc", "msyh.ttc"],
        ));
    }
    if s.contains(Scripts::HAN) {
        // SIEMPRE después de la japonesa (si la hay): MS Gothic o Meiryo cubren los kanji, pero no los caracteres solo del
        // chino simplificado (como 夹), que saldrían como recuadro en un árbol con japonés y chino a la vez.
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
    // Emoji: egui no dibuja fuentes de color (Segoe UI Emoji), así que solo se obtiene lo que Segoe UI Symbol trae en
    // monocromo; el resto de emoji de un nombre sale como recuadro (limitación conocida, ver el informe).
    if s.contains(Scripts::SYMBOLS) || s.contains(Scripts::EMOJI) {
        v.push(("Tesela-symbols", &["seguisym.ttf"]));
    }
    v
}

/// Construye las definiciones de fuentes a partir de la carpeta `dir` de fuentes del sistema (la inyecta el test) y de
/// las escrituras que hay que cubrir además de la principal.
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

    // Reservas por escritura (las que existan).
    let mut fallback_names: Vec<String> = Vec::new();
    let mut chosen: Vec<&str> = Vec::new();
    for (name, candidates) in fallback_groups(scripts) {
        // El mismo archivo no se carga dos veces (YaHei puede ser a la vez la reserva de kana y la de han).
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

/// ¿Hay alguna fuente de sistema utilizable?
pub fn available() -> bool {
    let dir = platform::fonts_dir();
    definitions(dir.as_deref(), Scripts::NONE).1 != Loaded::None
}

/// Carga las fuentes del sistema (y las de `scripts`) en `ctx` y devuelve cuál es la principal. Los cambios valen desde
/// el SIGUIENTE pase de egui.
pub fn install(ctx: &Context, scripts: Scripts) -> Loaded {
    let dir = platform::fonts_dir();
    let (fd, loaded) = definitions(dir.as_deref(), scripts);
    if loaded != Loaded::None {
        ctx.set_fonts(fd);
    }
    loaded
}

/// Como [`install`], y además aplica las fuentes ya (con un pase vacío). Para los tests y para quien necesite medir
/// texto de inmediato.
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
        // Sin escrituras pedidas no se carga ninguna reserva (memoria).
        assert_eq!(fd.families[&FontFamily::Proportional].len(), 1);
        // Con ellas, van detrás de la principal en las tres familias (si el equipo tiene alguna de esas fuentes: un Windows
        // Server mínimo puede no tener ninguna, y entonces no hay nada que comprobar).
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
        // Carpeta de fuentes FALSA (solo %TEMP%	esela-test-<uuid>): un Windows Server mínimo.
        let dir = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let all = Scripts::KANA
            | Scripts::HANGUL
            | Scripts::THAI
            | Scripts::INDIC
            | Scripts::RTL
            | Scripts::SYMBOLS;
        // Vacía: faltan todos los grupos.
        assert_eq!(missing_groups(&dir, all).len(), fallback_groups(all).len());
        // Con solo las fuentes que egui no puede usar, siguen faltando (no se cuentan).
        for f in UNUSABLE_IN_EGUI {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        assert_eq!(missing_groups(&dir, all).len(), fallback_groups(all).len());
        // Con una utilizable por grupo, no falta ninguno.
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
        // Sin la fuente de símbolos, solo falta ese grupo.
        std::fs::remove_file(dir.join("seguisym.ttf")).unwrap();
        assert_eq!(missing_groups(&dir, all), vec!["Tesela-symbols"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn con_japones_y_chino_a_la_vez_se_cargan_las_dos_reservas_y_sin_duplicar_el_archivo() {
        let dir = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["segoeui.ttf", "segoeuib.ttf", "msgothic.ttc", "msyh.ttc"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let both = Scripts::KANA | Scripts::HAN;
        let (fd, _) = definitions(Some(&dir), both);
        let prop = &fd.families[&FontFamily::Proportional];
        // La japonesa primero y la china después (para los caracteres solo simplificados).
        let kana = prop.iter().position(|n| n == "Tesela-kana").expect("kana");
        let han = prop.iter().position(|n| n == "Tesela-han").expect("han");
        assert!(kana < han, "{prop:?}");
        // Si la única fuente CJK es YaHei, sirve para las dos y no se carga dos veces.
        // (otra carpeta: los bytes de cada ruta se comparten y se recuerdan durante el proceso)
        let dir2 = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir2).unwrap();
        for f in ["segoeui.ttf", "segoeuib.ttf", "msyh.ttc"] {
            std::fs::write(dir2.join(f), b"x").unwrap();
        }
        let (fd, _) = definitions(Some(&dir2), both);
        let prop = &fd.families[&FontFamily::Proportional];
        assert_eq!(
            prop.iter()
                .filter(|n| n.starts_with("Tesela-k") || n.starts_with("Tesela-h"))
                .count(),
            1,
            "{prop:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
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
        // El arena WTF-8 da lo mismo que el texto.
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
        // Bytes sueltos o truncados no hacen daño.
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
