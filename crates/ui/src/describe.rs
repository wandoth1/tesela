//! How a node is described in the interface: name, special class (system block, `pagefile`/`hiberfil`), phrases
//! of types and age. Pure: does not draw anything.

use crate::args;
use crate::i18n::Tr;
use tesela_model::{FileType, Hist, NodeId, Tree, flags};

/// System file that is reported only (🔺 "Report only": no checkbox, never deleted from the app).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfoKind {
    Pagefile,
    Hiberfil,
    Swapfile,
}

/// Nodes with their own treatment on the map (gray with meaning, PLAN.md §2.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Special {
    /// The synthetic node "Windows and other system data" (ⓘ).
    SystemBlock,
    /// `pagefile.sys`, `hiberfil.sys`, or `swapfile.sys` at the root of a volume ("report only" triangle).
    InfoOnly(InfoKind),
}

/// Is the name the root of a drive (`C:` or `C:\`)?
pub fn is_drive_root_name(name: &str) -> bool {
    let b = name.as_bytes();
    matches!(b.len(), 2 | 3)
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/')
}

/// `id` special class in `tree`. `pagefile` and company only count if they hang from the root of a drive
/// (the tree is an entire volume): a file with that name in any other folder is one more.
pub fn special(tree: &Tree, id: NodeId) -> Option<Special> {
    let n = tree.node(id);
    if n.flags & flags::SYSTEM_INFO != 0 {
        return Some(Special::SystemBlock);
    }
    if n.parent != 0 || n.is_dir() || !is_drive_root_name(&tree.name_lossy(0)) {
        return None;
    }
    let name = tree.name_lossy(id).to_ascii_lowercase();
    match name.as_str() {
        "pagefile.sys" => Some(Special::InfoOnly(InfoKind::Pagefile)),
        "hiberfil.sys" => Some(Special::InfoOnly(InfoKind::Hiberfil)),
        "swapfile.sys" => Some(Special::InfoOnly(InfoKind::Swapfile)),
        _ => None,
    }
}

/// Text to compare in searches: lowercase and without accents or umlauts (`Vídeos` matches `vid`).
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            other => other,
        })
        .collect()
}

/// "less than 1 min ago", "5 min ago", "3 hours ago", "2 days ago": the elapsed time in natural language.
pub fn ago_phrase(tr: &Tr, secs: u64) -> String {
    if secs < 60 {
        tr.t("ago-now")
    } else if secs < 3600 {
        tr.ta("ago-minutes", args!["n" => secs / 60])
    } else if secs < 86_400 {
        tr.ta("ago-hours", args!["n" => secs / 3600])
    } else {
        let n = secs / 86_400;
        tr.ta("ago-days", args!["n" => n, "count" => tr.int(n)])
    }
}

/// Is it an enterable directory, based on what is seen after cleaning (a cleared directory no longer has children)?
pub fn can_enter_view(view: &tesela_model::View, id: NodeId) -> bool {
    let n = view.tree().node(id);
    n.is_dir() && n.flags & flags::SYSTEM_INFO == 0 && !view.kids(id).is_empty()
}

/// Is it a directory that can be entered? (with children, it is not a mount point nor the system block).
pub fn can_enter(tree: &Tree, id: NodeId) -> bool {
    let n = tree.node(id);
    n.is_dir() && n.child_count > 0 && n.flags & flags::SYSTEM_INFO == 0
}

/// Is its size a minimum ("≥")? It happens in folders with the incomplete list or with something incomplete underneath.
pub fn is_lower_bound(tree: &Tree, id: NodeId) -> bool {
    tree.node(id).flags & (flags::INCOMPLETE | flags::INCOMPLETE_BELOW) != 0
}

/// Display name of `id`: the system block, translated; the root, its route; the rest, your name.
pub fn display_name(tree: &Tree, id: NodeId, tr: &Tr) -> String {
    match special(tree, id) {
        Some(Special::SystemBlock) => tr.t("system-block"),
        _ => tree.name_lossy(id).into_owned(),
    }
}

/// Short name for breadcrumb: the root of a drive is `C:`; the root of a scanned folder, its last component
/// (full route is in the details); the rest, your name.
pub fn crumb_name(tree: &Tree, id: NodeId, tr: &Tr) -> String {
    let name = display_name(tree, id, tr);
    if id != 0 {
        return name;
    }
    let trimmed = name.trim_end_matches(['\\', '/']);
    if is_drive_root_name(trimmed) {
        return trimmed.to_string();
    }
    trimmed
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

/// File type name, translated.
pub fn type_name(tr: &Tr, t: FileType) -> String {
    tr.t(match t {
        FileType::Video => "type-video",
        FileType::Image => "type-image",
        FileType::Audio => "type-audio",
        FileType::Document => "type-document",
        FileType::Archive => "type-archive",
        FileType::Program => "type-program",
        FileType::Other => "type-other",
    })
}

/// `n` types with more bytes (rounded percentage, largest first, without those worth 0).
pub fn top_types(h: &Hist, n: usize) -> Vec<(FileType, u8)> {
    let mut v: Vec<(FileType, u8)> = FileType::ALL
        .iter()
        .map(|&t| (t, h.type_pct[t as usize]))
        .filter(|(_, p)| *p > 0)
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then((a.0 as u8).cmp(&(b.0 as u8))));
    v.truncate(n);
    v
}

/// Composition phrase: "55% compressed · 30% programs · 15% other" (empty if there are no bytes).
pub fn type_phrase(tr: &Tr, h: &Hist) -> String {
    top_types(h, 3)
        .iter()
        .map(|(t, p)| {
            tr.ta(
                "type-share",
                args!["pct" => tr.pct(*p as f64), "type" => type_name(tr, *t).to_lowercase()],
            )
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Seniority phrase (always "modified", never "used", PLAN.md §2.9). `None` if there is no date data.
pub fn age_phrase(tr: &Tr, h: &Hist) -> Option<String> {
    let total: u32 = h.age_pct.iter().map(|&p| p as u32).sum();
    if total == 0 {
        return None;
    }
    match h.dominant_age() {
        Some(b) if b >= 1 => {
            let older: u32 = h.age_pct[b..].iter().map(|&p| p as u32).sum();
            let key = [
                "",
                "age-over-month",
                "age-over-6months",
                "age-over-year",
                "age-over-3years",
            ][b];
            Some(tr.ta(key, args!["pct" => tr.pct(older as f64)]))
        }
        _ => {
            let recent = h.age_pct[0] as u32;
            if recent >= 50 {
                Some(tr.ta("age-recent", args!["pct" => tr.pct(recent as f64)]))
            } else {
                Some(tr.t("age-mixed"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::{self, Variant};
    use crate::i18n::Lang;

    #[test]
    fn la_busqueda_ignora_mayusculas_y_acentos() {
        assert_eq!(fold("Vídeos"), "videos");
        assert_eq!(fold("MÚSICA ñandú"), "musica nandu");
        assert!(fold("Imágenes").contains(&fold("IMAGEN")));
    }

    #[test]
    fn nombre_corto_de_la_raiz_para_el_breadcrumb() {
        let t = demo::tree(Variant::Complete);
        let tr = Tr::new(Lang::Es);
        assert_eq!(crumb_name(&t, 0, &tr), "C:");
        let users = t.child_by_name(0, b"Users").unwrap();
        assert_eq!(crumb_name(&t, users, &tr), "Users");
    }

    #[test]
    fn raices_de_unidad() {
        assert!(is_drive_root_name("C:\\"));
        assert!(is_drive_root_name("e:"));
        assert!(!is_drive_root_name("C:\\Users"));
        assert!(!is_drive_root_name("Users"));
        assert!(!is_drive_root_name(""));
    }

    #[test]
    fn pagefile_hiberfil_y_bloque_del_sistema_son_especiales_solo_en_la_raiz() {
        let t = demo::tree(Variant::Complete);
        let find = |name: &str| t.child_by_name(0, name.as_bytes()).unwrap();
        assert_eq!(
            special(&t, find("pagefile.sys")),
            Some(Special::InfoOnly(InfoKind::Pagefile))
        );
        assert_eq!(
            special(&t, find("hiberfil.sys")),
            Some(Special::InfoOnly(InfoKind::Hiberfil))
        );
        assert_eq!(
            special(&t, find("swapfile.sys")),
            Some(Special::InfoOnly(InfoKind::Swapfile))
        );
        assert_eq!(special(&t, find("Users")), None);
        let sys = t.children(0).last().unwrap();
        assert_eq!(special(&t, sys), Some(Special::SystemBlock));
        assert!(!can_enter(&t, sys));
        assert!(can_enter(&t, find("Users")));
        assert!(!can_enter(&t, find("pagefile.sys")));
        // A file with that name in another folder is not special.
        assert_eq!(special(&t, 0), None);
    }

    #[test]
    fn el_tamano_minimo_se_marca_en_las_carpetas_incompletas() {
        let t = demo::tree(Variant::Stopped);
        let win = t.child_by_name(0, b"Windows").unwrap();
        assert!(
            is_lower_bound(&t, win),
            "Windows tiene algo incompleto por debajo"
        );
        assert!(is_lower_bound(&t, 0));
        let users = t.child_by_name(0, b"Users").unwrap();
        assert!(!is_lower_bound(&t, users));
    }

    #[test]
    fn las_frases_de_tipos_y_antiguedad() {
        let t = demo::tree(Variant::Complete);
        let es = Tr::new(Lang::Es);
        let users = t.child_by_name(0, b"Users").unwrap();
        let me = t.child_by_name(users, demo::USER.as_bytes()).unwrap();
        let videos = t.child_by_name(me, "Vídeos".as_bytes()).unwrap();
        let tp = type_phrase(&es, t.hist(videos));
        assert!(tp.contains("vídeo"), "{tp}");
        assert!(tp.contains('%'));
        let top = top_types(t.hist(videos), 3);
        assert_eq!(top[0].0, FileType::Video);
        assert!(top.windows(2).all(|w| w[0].1 >= w[1].1));
        // An old files folder says "not modified for more than...".
        let docs = t.child_by_name(me, b"Documentos").unwrap();
        let ap = age_phrase(&es, t.hist(docs)).expect("fechas");
        assert!(ap.contains("modifica") || ap.contains("modific"), "{ap}");
        assert!(age_phrase(&es, &Hist::EMPTY).is_none());
    }
}
