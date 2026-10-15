//! Contadores de progreso en vivo (atómicos): la interfaz los lee 15-30 veces por segundo sin bloquear a los hilos.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Acumulador de una carpeta de primer nivel de la ruta escaneada.
#[derive(Debug)]
pub struct TopLevel {
    /// Nombre de la carpeta (el índice 0 es "los archivos sueltos de la raíz" y no tiene nombre).
    pub name: OsString,
    /// Archivos vistos.
    pub files: AtomicU64,
    /// Bytes en disco vistos (incluye los enlaces duros repetidos: la deduplicación se hace al construir el árbol).
    pub alloc: AtomicU64,
}

impl TopLevel {
    pub(crate) fn new(name: OsString) -> TopLevel {
        TopLevel {
            name,
            files: AtomicU64::new(0),
            alloc: AtomicU64::new(0),
        }
    }
}

/// Estado de una ruta prioritaria (temporales, cachés, Papelera propia, Descargas).
#[derive(Debug)]
pub struct PriorityTarget {
    /// Ruta canónica de la carpeta.
    pub path: PathBuf,
    /// Bytes en disco vistos hasta ahora en su subárbol.
    pub alloc: AtomicU64,
    /// Archivos vistos hasta ahora en su subárbol.
    pub files: AtomicU64,
    pub(crate) pending: AtomicU32,
    pub(crate) done: AtomicBool,
}

impl PriorityTarget {
    pub(crate) fn new(path: PathBuf) -> PriorityTarget {
        PriorityTarget {
            path,
            alloc: AtomicU64::new(0),
            files: AtomicU64::new(0),
            pending: AtomicU32::new(1),
            done: AtomicBool::new(false),
        }
    }

    /// ¿Se terminó de recorrer todo su subárbol? Entonces `alloc` ya no cambia.
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

/// Progreso de un escaneo. Todos los campos se pueden leer desde otro hilo mientras se escanea.
#[derive(Debug, Default)]
pub struct Progress {
    /// Archivos (entradas que no son directorios) vistos.
    pub files: AtomicU64,
    /// Directorios vistos (no cuenta los reparse points, que no se siguen).
    pub dirs: AtomicU64,
    /// Bytes en disco vistos (sin deduplicar enlaces duros).
    pub alloc: AtomicU64,
    /// Directorios que no se pudieron listar.
    pub errors: AtomicU64,
    /// El escaneo terminó (o se canceló y ya se ha construido el árbol).
    pub finished: AtomicBool,
    pub(crate) top: OnceLock<Vec<TopLevel>>,
    pub(crate) priority: OnceLock<Vec<PriorityTarget>>,
}

impl Progress {
    /// Acumuladores por carpeta de primer nivel (vacío hasta que se lista la raíz, que es lo primero que se hace).
    pub fn top_levels(&self) -> &[TopLevel] {
        self.top.get().map_or(&[], Vec::as_slice)
    }

    /// Rutas prioritarias que se están escaneando (vacío si ninguna cuelga de la ruta escaneada).
    pub fn priority_targets(&self) -> &[PriorityTarget] {
        self.priority.get().map_or(&[], Vec::as_slice)
    }
}
