//! Clasificación de entradas y conversión de tiempos (funciones puras).

use tesela_model::flags;

const ATTR_DIRECTORY: u32 = 0x0000_0010;
const ATTR_REPARSE_POINT: u32 = 0x0000_0400;
/// `FILE_ATTRIBUTE_RECALL_ON_OPEN`.
const ATTR_RECALL_ON_OPEN: u32 = 0x0004_0000;
/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`: el contenido no está en el disco, se descarga al abrirlo.
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// ¿Es un directorio?
pub fn is_dir(attrs: u32) -> bool {
    attrs & ATTR_DIRECTORY != 0
}

/// ¿Es un reparse point (junction, symlink, punto de montaje, placeholder...)?
pub fn is_reparse(attrs: u32) -> bool {
    attrs & ATTR_REPARSE_POINT != 0
}

/// ¿Es un elemento de la nube (placeholder de OneDrive u otro proveedor de Cloud Files)?
/// Etiquetas `IO_REPARSE_TAG_CLOUD` (`0x9000001A`) y `IO_REPARSE_TAG_CLOUD_1..F` (el nibble 12..15 varía), o los
/// atributos de "recuperar al abrir / al acceder a los datos".
pub fn is_cloud(attrs: u32, reparse_tag: u32) -> bool {
    let tag_is_cloud = is_reparse(attrs) && (reparse_tag & !0xF000) == 0x9000_001A;
    tag_is_cloud || attrs & (ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS) != 0
}

/// ¿Es un reparse point de la nube (etiqueta `IO_REPARSE_TAG_CLOUD*`)? Estos directorios SÍ se recorren (su filtro
/// permite listarlos); el resto de reparse points de directorio (junctions, symlinks, montajes) no se siguen.
pub fn is_cloud_reparse(attrs: u32, reparse_tag: u32) -> bool {
    is_reparse(attrs) && (reparse_tag & !0xF000) == 0x9000_001A
}

/// Flags de una entrada del listado.
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

/// FILETIME (intervalos de 100 ns desde 1601) a segundos Unix en `u32`: lo anterior a 1970 es 0 y lo posterior a
/// 2106 se satura.
pub fn filetime_to_unix(ft: i64) -> u32 {
    const UNIX_EPOCH_SECS: i64 = 11_644_473_600;
    let secs = ft / 10_000_000 - UNIX_EPOCH_SECS;
    secs.clamp(0, u32::MAX as i64) as u32
}

/// `alloc` estimado cuando solo hay `EndOfFile` (respaldo con `FindFirstFileExW`): EOF redondeado al cluster.
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
