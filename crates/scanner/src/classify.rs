//! Entry classification and time conversion (pure functions).

use tesela_model::flags;

const ATTR_DIRECTORY: u32 = 0x0000_0010;
const ATTR_REPARSE_POINT: u32 = 0x0000_0400;
/// `FILE_ATTRIBUTE_RECALL_ON_OPEN`.
const ATTR_RECALL_ON_OPEN: u32 = 0x0004_0000;
/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`: the content is not on disk and is downloaded when accessed.
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// Is this a directory?
pub fn is_dir(attrs: u32) -> bool {
    attrs & ATTR_DIRECTORY != 0
}

/// Is this a reparse point (junction, symlink, mount point, placeholder...)?
pub fn is_reparse(attrs: u32) -> bool {
    attrs & ATTR_REPARSE_POINT != 0
}

/// Is this a cloud item (a OneDrive placeholder or one from another Cloud Files provider)?
/// The `IO_REPARSE_TAG_CLOUD` (`0x9000001A`) and `IO_REPARSE_TAG_CLOUD_1..F` tags (the 12..15 nibble varies), or the
/// "recall on open / recall on data access" attributes.
pub fn is_cloud(attrs: u32, reparse_tag: u32) -> bool {
    let tag_is_cloud = is_reparse(attrs) && (reparse_tag & !0xF000) == 0x9000_001A;
    tag_is_cloud || attrs & (ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0
}

/// Is this a cloud reparse point (an `IO_REPARSE_TAG_CLOUD*` tag)? These directories ARE traversed (their filter
/// allows them to be listed); other directory reparse points (junctions, symlinks, mount points) are not followed.
pub fn is_cloud_reparse(attrs: u32, reparse_tag: u32) -> bool {
    is_reparse(attrs) && (reparse_tag & !0xF000) == 0x9000_001A
}

/// What the scanner does with a directory found in its parent's listing. The decision uses the reparse TAG and
/// recall attributes; the builder represents this with flags (see `dir_flags`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirAction {
    /// Normal directory: traverse it.
    Walk,
    /// Populated cloud reparse point: traverse it using its cloud filter.
    WalkCloud,
    /// Junction, symlink, or mount point: do not follow it; count zero bytes.
    SkipReparse,
    /// Unpopulated cloud folder (`RECALL_ON_OPEN` or `RECALL_ON_DATA_ACCESS`): listing it could trigger network
    /// downloads, so do not traverse it; count its `AllocationSize` and mark it `CLOUD | INCOMPLETE`.
    SkipUnpopulatedCloud,
}

/// Decide what to do with a directory.
pub fn dir_action(attrs: u32, reparse_tag: u32) -> DirAction {
    let cloud_tag = is_cloud_reparse(attrs, reparse_tag);
    if is_reparse(attrs) && !cloud_tag {
        DirAction::SkipReparse
    } else if attrs & (ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0 {
        DirAction::SkipUnpopulatedCloud
    } else if cloud_tag {
        DirAction::WalkCloud
    } else {
        DirAction::Walk
    }
}

/// What the scanner does with the scan ROOT: the same classification used for descendants (`dir_action`), using
/// the root's own attributes and reparse tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootPlan {
    /// Flags for the root node (`DIR`, and `REPARSE`/`CLOUD` as applicable).
    pub flags: u16,
    /// Should the root be listed? No, if it is a junction/symlink/mount point (do not follow it) or an unpopulated cloud
    /// folder (listing it could trigger network downloads).
    pub walk: bool,
    /// Open the root and let the cloud filter handle it (a populated cloud folder).
    pub open_cloud: bool,
    /// Count zero bytes (unfollowed reparse point). An unpopulated cloud folder retains its `AllocationSize`.
    pub zero_size: bool,
}

/// Decide what to do with the root from its attributes and reparse tag.
pub fn root_plan(attrs: u32, reparse_tag: u32) -> RootPlan {
    let action = dir_action(attrs, reparse_tag);
    RootPlan {
        flags: dir_flags(attrs, reparse_tag) | flags::DIR,
        walk: matches!(action, DirAction::Walk | DirAction::WalkCloud),
        open_cloud: action == DirAction::WalkCloud,
        zero_size: action == DirAction::SkipReparse,
    }
}

/// Flags for a directory based on its action. A reparse point that is NOT from the cloud never has `CLOUD` (even if it has
/// recall attributes): this lets the builder distinguish "not followed" (`REPARSE` without `CLOUD`) from "traversable"
/// (`REPARSE | CLOUD`, or no `REPARSE`) using only the flags, with the same rule as the scanner.
pub fn dir_flags(attrs: u32, reparse_tag: u32) -> u16 {
    let f = entry_flags(attrs, reparse_tag);
    if dir_action(attrs, reparse_tag) == DirAction::SkipReparse {
        f & !flags::CLOUD
    } else {
        f
    }
}

/// Flags for a listed entry.
pub fn entry_flags(attrs: u32, reparse_tag: u32) -> u16 {
    let mut f = 0;
    if is_dir(attrs) {
        f |= flags::DIR;
    }
    if is_reparse(attrs) {
        f |= flags::REPARSE;
    }
    if is_cloud(attrs, reparse_tag) {
        f |= flags::CLOUD;
    }
    f
}

/// Convert FILETIME (100 ns intervals since 1601) to Unix seconds in `u32`: values before 1970 become 0, and those after
/// 2106 are saturated.
pub fn filetime_to_unix(ft: i64) -> u32 {
    const UNIX_EPOCH_SECS: i64 = 11_644_473_600;
    let secs = ft / 10_000_000 - UNIX_EPOCH_SECS;
    secs.clamp(0, u32::MAX as i64) as u32
}

/// Estimated `alloc` when only `EndOfFile` is available (the `FindFirstFileExW` fallback): EOF rounded up to a cluster.
pub fn estimate_alloc(logical: u64, cluster: u64) -> u64 {
    logical.div_ceil(cluster) * cluster
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_de_directorios_y_reparse() {
        assert_eq!(entry_flags(0x10, 0), flags::DIR);
        assert_eq!(
            entry_flags(0x10 | 0x400, 0xA000_0003),
            flags::DIR | flags::REPARSE
        ); // junction
        assert_eq!(entry_flags(0x20, 0), 0);
        assert_eq!(entry_flags(0x20 | 0x400, 0xA000_000C), flags::REPARSE); // symlink de archivo
    }

    #[test]
    fn placeholders_de_la_nube() {
        // OneDrive: reparse con la etiqueta CLOUD (varios nibbles posibles).
        assert_eq!(
            entry_flags(0x20 | 0x400, 0x9000_001A),
            flags::REPARSE | flags::CLOUD
        );
        assert_eq!(
            entry_flags(0x20 | 0x400, 0x9000_901A),
            flags::REPARSE | flags::CLOUD
        );
        assert_eq!(
            entry_flags(0x20 | 0x400, 0x9000_F01A),
            flags::REPARSE | flags::CLOUD
        );
        // Solo atributos de recuperación (sin reparse).
        assert_eq!(entry_flags(0x20 | 0x0040_0000, 0), flags::CLOUD);
        assert_eq!(entry_flags(0x20 | 0x0004_0000, 0), flags::CLOUD);
        // Otras etiquetas de reparse no son de la nube, y la etiqueta sin el atributo REPARSE se ignora.
        assert_eq!(entry_flags(0x20 | 0x400, 0x8000_0017), flags::REPARSE); // WOF
        assert_eq!(entry_flags(0x20, 0x9000_001A), 0);
    }

    #[test]
    fn solo_los_reparse_de_la_nube_se_recorren() {
        assert!(is_cloud_reparse(0x10 | 0x400, 0x9000_001A));
        assert!(is_cloud_reparse(0x10 | 0x400, 0x9000_B01A));
        assert!(!is_cloud_reparse(0x10 | 0x400, 0xA000_0003), "junction");
        assert!(!is_cloud_reparse(0x10 | 0x400, 0xA000_000C), "symlink");
        assert!(
            !is_cloud_reparse(0x10, 0x9000_001A),
            "sin el atributo REPARSE_POINT la etiqueta no vale"
        );
        // Un atributo de recuperación sin reparse es CLOUD pero no un reparse de la nube.
        assert!(!is_cloud_reparse(0x10 | 0x0040_0000, 0));
    }

    #[test]
    fn que_se_hace_con_cada_directorio_segun_su_etiqueta_y_atributos() {
        const DIR: u32 = 0x10;
        const REPARSE: u32 = 0x400;
        const RECALL_OPEN: u32 = 0x0004_0000;
        const RECALL_DATA: u32 = 0x0040_0000;
        assert_eq!(dir_action(DIR, 0), DirAction::Walk);
        assert_eq!(
            dir_action(DIR | REPARSE, 0xA000_0003),
            DirAction::SkipReparse
        ); // junction
        assert_eq!(
            dir_action(DIR | REPARSE, 0xA000_000C),
            DirAction::SkipReparse
        ); // symlink
        // Carpeta de la nube poblada: se recorre con su filtro.
        assert_eq!(dir_action(DIR | REPARSE, 0x9000_001A), DirAction::WalkCloud);
        // No poblada (cualquiera de los dos atributos), con o sin reparse: NO se recorre.
        assert_eq!(
            dir_action(DIR | REPARSE | RECALL_OPEN, 0x9000_001A),
            DirAction::SkipUnpopulatedCloud
        );
        assert_eq!(
            dir_action(DIR | REPARSE | RECALL_DATA, 0x9000_001A),
            DirAction::SkipUnpopulatedCloud
        );
        assert_eq!(
            dir_action(DIR | RECALL_DATA, 0),
            DirAction::SkipUnpopulatedCloud
        );
        // Un junction con atributos de recuperación sigue siendo un reparse sin seguir.
        assert_eq!(
            dir_action(DIR | REPARSE | RECALL_DATA, 0xA000_0003),
            DirAction::SkipReparse
        );
    }

    #[test]
    fn los_flags_de_directorio_distinguen_no_seguido_de_recorrible() {
        const DIR: u32 = 0x10;
        const REPARSE: u32 = 0x400;
        const RECALL_DATA: u32 = 0x0040_0000;
        let f = |attrs, tag| dir_flags(attrs, tag);
        assert_eq!(f(DIR, 0), flags::DIR);
        assert_eq!(f(DIR | REPARSE, 0xA000_0003), flags::DIR | flags::REPARSE);
        // Junction con atributo de recuperación: sin CLOUD (no es recorrible).
        assert_eq!(
            f(DIR | REPARSE | RECALL_DATA, 0xA000_0003),
            flags::DIR | flags::REPARSE
        );
        assert_eq!(
            f(DIR | REPARSE, 0x9000_001A),
            flags::DIR | flags::REPARSE | flags::CLOUD
        );
        assert_eq!(f(DIR | RECALL_DATA, 0), flags::DIR | flags::CLOUD);
    }

    #[test]
    fn la_raiz_se_clasifica_igual_que_los_descendientes() {
        const DIR: u32 = 0x10;
        const REPARSE: u32 = 0x400;
        const RECALL_OPEN: u32 = 0x0004_0000;
        const RECALL_DATA: u32 = 0x0040_0000;
        // Raíz normal.
        assert_eq!(
            root_plan(DIR, 0),
            RootPlan {
                flags: flags::DIR,
                walk: true,
                open_cloud: false,
                zero_size: false
            }
        );
        // Carpeta de la nube ya poblada: se recorre abriéndola con su filtro.
        assert_eq!(
            root_plan(DIR | REPARSE, 0x9000_001A),
            RootPlan {
                flags: flags::DIR | flags::REPARSE | flags::CLOUD,
                walk: true,
                open_cloud: true,
                zero_size: false
            }
        );
        // Carpeta de la nube NO poblada (cualquiera de los dos atributos): no se lista y conserva su tamaño.
        for recall in [RECALL_OPEN, RECALL_DATA] {
            assert_eq!(
                root_plan(DIR | REPARSE | recall, 0x9000_001A),
                RootPlan {
                    flags: flags::DIR | flags::REPARSE | flags::CLOUD,
                    walk: false,
                    open_cloud: false,
                    zero_size: false
                }
            );
        }
        assert!(!root_plan(DIR | RECALL_DATA, 0).walk);
        // Junction o symlink: no se sigue y mide 0 (aunque lleve atributos de recuperación).
        for tag in [0xA000_0003, 0xA000_000C] {
            assert_eq!(
                root_plan(DIR | REPARSE | RECALL_DATA, tag),
                RootPlan {
                    flags: flags::DIR | flags::REPARSE,
                    walk: false,
                    open_cloud: false,
                    zero_size: true
                }
            );
        }
    }

    #[test]
    fn filetime_a_unix() {
        assert_eq!(filetime_to_unix(0), 0);
        assert_eq!(filetime_to_unix(116_444_736_000_000_000), 0); // 1970-01-01
        assert_eq!(filetime_to_unix(116_444_736_000_000_000 + 10_000_000), 1);
        assert_eq!(filetime_to_unix(133_000_000_000_000_000), 1_655_526_400);
        assert_eq!(filetime_to_unix(i64::MAX), u32::MAX);
        assert_eq!(filetime_to_unix(-5), 0);
    }

    #[test]
    fn alloc_estimado_redondea_al_cluster() {
        assert_eq!(estimate_alloc(0, 4096), 0);
        assert_eq!(estimate_alloc(1, 4096), 4096);
        assert_eq!(estimate_alloc(4096, 4096), 4096);
        assert_eq!(estimate_alloc(4097, 4096), 8192);
    }
}
