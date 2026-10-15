//! Flags de un nodo (`Record::flags` y `Node::flags`).

/// Es un directorio (también los reparse points de directorio, que llevan además [`REPARSE`]).
pub const DIR: u16 = 1 << 0;
/// Reparse point (junction, symlink, punto de montaje...). Los de directorio no se siguen y miden 0.
pub const REPARSE: u16 = 1 << 1;
/// Archivo o carpeta de la nube (placeholder de OneDrive, etc.): su `alloc` puede ser 0 aunque `logical` no.
pub const CLOUD: u16 = 1 << 2;
/// Enlace duro que NO conserva el tamaño: `alloc` y `logical` son 0 porque otro enlace ya lo cuenta.
pub const HARDLINK: u16 = 1 << 3;
/// Directorio cuyo contenido no se llegó a listar entero (escaneo detenido): su tamaño es un mínimo ("≥ X").
pub const INCOMPLETE: u16 = 1 << 4;
/// Nodo sintético "Windows y otros datos del sistema" (espacio no explicado). No es un archivo real.
pub const SYSTEM_INFO: u16 = 1 << 5;
/// No se pudo listar el directorio (permisos, error de E/S...).
pub const ERROR: u16 = 1 << 6;
