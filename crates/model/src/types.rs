//! Tipos de archivo (por extensión) y tramos de antigüedad, usados por los histogramas.

use rustc_hash::FxHashMap;
use serde::Deserialize;
use std::sync::OnceLock;

/// Tipo de archivo. El orden de las variantes es el de `Hist::type_pct`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum FileType {
    Video = 0,
    Image = 1,
    Audio = 2,
    Document = 3,
    Archive = 4,
    Program = 5,
    Other = 6,
}

impl FileType {
    /// Número de tipos (longitud de `Hist::type_pct`).
    pub const COUNT: usize = 7;
    /// Todos los tipos, en orden de índice.
    pub const ALL: [FileType; Self::COUNT] = [
        FileType::Video,
        FileType::Image,
        FileType::Audio,
        FileType::Document,
        FileType::Archive,
        FileType::Program,
        FileType::Other,
    ];

    /// Tipo a partir de su índice (0..7).
    pub fn from_index(i: u8) -> Option<FileType> {
        Self::ALL.get(i as usize).copied()
    }

    /// Clasifica un nombre de archivo (bytes WTF-8) por su extensión, sin distinguir mayúsculas.
    /// Sin extensión (o solo un punto inicial, como `.gitignore`) es [`FileType::Other`].
    pub fn classify(name: &[u8]) -> FileType {
        let Some(dot) = name.iter().rposition(|&b| b == b'.') else {
            return FileType::Other;
        };
        if dot == 0 {
            return FileType::Other;
        }
        let ext = &name[dot + 1..];
        if ext.is_empty() || ext.len() > MAX_EXT {
            return FileType::Other;
        }
        let mut buf = [0u8; MAX_EXT];
        for (d, s) in buf.iter_mut().zip(ext) {
            *d = s.to_ascii_lowercase();
        }
        table()
            .get(&buf[..ext.len()])
            .copied()
            .unwrap_or(FileType::Other)
    }
}

/// Las extensiones más largas que esto se consideran "otros" (ninguna de la tabla se acerca).
const MAX_EXT: usize = 15;

#[derive(Deserialize)]
struct TypesFile {
    video: Vec<String>,
    image: Vec<String>,
    audio: Vec<String>,
    document: Vec<String>,
    archive: Vec<String>,
    program: Vec<String>,
}

/// Fuente embebida de la tabla extensión -> tipo.
pub const TYPES_TOML: &str = include_str!("../data/types.toml");

/// Interpreta una tabla TOML. Devuelve el mapa o un mensaje de error (extensión repetida entre tipos, TOML inválido).
pub fn parse_table(src: &str) -> Result<FxHashMap<Box<[u8]>, FileType>, String> {
    let f: TypesFile = toml::from_str(src).map_err(|e| e.to_string())?;
    let mut map: FxHashMap<Box<[u8]>, FileType> = FxHashMap::default();
    let groups = [
        (FileType::Video, f.video),
        (FileType::Image, f.image),
        (FileType::Audio, f.audio),
        (FileType::Document, f.document),
        (FileType::Archive, f.archive),
        (FileType::Program, f.program),
    ];
    for (ty, exts) in groups {
        for e in exts {
            if e.is_empty() || e.len() > MAX_EXT || e != e.to_ascii_lowercase() || e.contains('.') {
                return Err(format!("extensión inválida: {e:?}"));
            }
            if let Some(prev) = map.insert(e.as_bytes().into(), ty) {
                return Err(format!("extensión repetida: {e:?} ({prev:?} y {ty:?})"));
            }
        }
    }
    Ok(map)
}

fn table() -> &'static FxHashMap<Box<[u8]>, FileType> {
    static TABLE: OnceLock<FxHashMap<Box<[u8]>, FileType>> = OnceLock::new();
    TABLE.get_or_init(|| {
        parse_table(TYPES_TOML).expect("types.toml embebido inválido (lo cubre un test)")
    })
}

/// Número de tramos de antigüedad (longitud de `Hist::age_pct`).
pub const AGE_BUCKETS: usize = 5;

const DAY: u32 = 86_400;

/// Tramo de antigüedad de un `mtime` (segundos Unix) respecto a `now` (segundos Unix):
/// 0 = menos de 1 mes (30 días), 1 = menos de 6 meses (180), 2 = menos de 1 año (365), 3 = menos de 3 años (1095),
/// 4 = más. Un `mtime` en el futuro cuenta como reciente.
pub fn age_bucket(now: u32, mtime: u32) -> usize {
    let age = now.saturating_sub(mtime);
    match age {
        a if a < 30 * DAY => 0,
        a if a < 180 * DAY => 1,
        a if a < 365 * DAY => 2,
        a if a < 1095 * DAY => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_tabla_embebida_es_valida_y_sin_repetidos() {
        let m = parse_table(TYPES_TOML).expect("tabla válida");
        assert!(m.len() > 100);
    }

    #[test]
    fn extension_repetida_se_rechaza() {
        let src = r#"
            video = ["mp4"]
            image = ["mp4"]
            audio = []
            document = []
            archive = []
            program = []
        "#;
        assert!(parse_table(src).unwrap_err().contains("repetida"));
    }

    #[test]
    fn clasifica_por_extension() {
        assert_eq!(FileType::classify(b"pelicula.MKV"), FileType::Video);
        assert_eq!(FileType::classify(b"foto.JpG"), FileType::Image);
        assert_eq!(FileType::classify(b"a.b.c.zip"), FileType::Archive);
        assert_eq!(FileType::classify(b"setup.exe"), FileType::Program);
        assert_eq!(FileType::classify(b"cancion.flac"), FileType::Audio);
        assert_eq!(FileType::classify(b"informe.pdf"), FileType::Document);
    }

    #[test]
    fn sin_extension_o_desconocida_es_otros() {
        assert_eq!(FileType::classify(b"Makefile"), FileType::Other);
        assert_eq!(FileType::classify(b".gitignore"), FileType::Other);
        assert_eq!(FileType::classify(b"final."), FileType::Other);
        assert_eq!(FileType::classify(b"x.desconocida"), FileType::Other);
        assert_eq!(
            FileType::classify(b"x.extensionmuylargaquenoexiste"),
            FileType::Other
        );
        assert_eq!(FileType::classify("año.mp4".as_bytes()), FileType::Video);
    }

    #[test]
    fn tramos_de_antiguedad() {
        let now = 4_000_000_000u32;
        let d = |days: u32| now - days * DAY;
        assert_eq!(age_bucket(now, d(0)), 0);
        assert_eq!(age_bucket(now, d(29)), 0);
        assert_eq!(age_bucket(now, d(30)), 1);
        assert_eq!(age_bucket(now, d(179)), 1);
        assert_eq!(age_bucket(now, d(180)), 2);
        assert_eq!(age_bucket(now, d(364)), 2);
        assert_eq!(age_bucket(now, d(365)), 3);
        assert_eq!(age_bucket(now, d(1094)), 3);
        assert_eq!(age_bucket(now, d(1095)), 4);
        assert_eq!(age_bucket(now, 0), 4);
        assert_eq!(age_bucket(100, 5000), 0, "futuro = reciente");
    }
}
