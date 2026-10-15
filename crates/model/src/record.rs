//! Registros planos y lotes (`Batch`) que los escáneres entregan al `TreeBuilder` (PLAN.md §3.2).
//!
//! # Identificadores provisionales
//! Cada escáner numera sus entradas como quiera (el walker, con un `AtomicU32::fetch_add` por directorio, de modo que
//! los hijos de un directorio ocupan ids contiguos `base_id..base_id + n`). El `TreeBuilder` no exige ningún orden: ni
//! de llegada de los lotes ni de los ids. Solo se pide:
//! * el id `0` es la raíz (su `parent` se escribe como `0`);
//! * `parent` apunta al id provisional de un directorio;
//! * un id no se usa dos veces.
//!
//! Un hueco (`Batch::push_hole`) reserva un id sin entrada (p. ej. un directorio ya adoptado por otro camino).

use crate::wtf8;

/// Marca de "sin entrada" en `Record::parent`. También es el `parent` de la raíz en el `Tree` final.
pub const NO_PARENT: u32 = u32::MAX;

/// Registro plano de una entrada (32 B). Los nombres viven aparte, en el arena del lote o del builder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Record {
    /// Bytes ocupados en disco (`AllocationSize`).
    pub alloc: u64,
    /// Tamaño lógico (`EndOfFile`).
    pub logical: u64,
    /// Id provisional del directorio padre (`NO_PARENT` = hueco; `0` en la raíz).
    pub parent: u32,
    /// Posición del nombre en el arena de nombres.
    pub name_off: u32,
    /// Última modificación, en segundos Unix (0 si se desconoce).
    pub mtime: u32,
    /// Longitud del nombre en bytes WTF-8.
    pub name_len: u16,
    /// Flags de [`crate::flags`].
    pub flags: u16,
}

const _: () = assert!(
    size_of::<Record>() == 32,
    "Record debe medir 32 B (PLAN.md §3.2)"
);

impl Record {
    /// Registro vacío que marca un hueco.
    pub const HOLE: Record = Record {
        alloc: 0,
        logical: 0,
        parent: NO_PARENT,
        name_off: 0,
        mtime: 0,
        name_len: 0,
        flags: 0,
    };

    /// ¿Es una entrada real (no un hueco ni un id sin recibir)?
    pub fn is_present(&self) -> bool {
        self.parent != NO_PARENT
    }
}

/// Datos de una entrada al añadirla a un lote.
#[derive(Clone, Copy, Debug, Default)]
pub struct EntryData {
    /// Flags de [`crate::flags`].
    pub flags: u16,
    /// Bytes ocupados en disco.
    pub alloc: u64,
    /// Tamaño lógico.
    pub logical: u64,
    /// Última modificación en segundos Unix.
    pub mtime: u32,
    /// `FileId` de 128 bits del archivo (0 = desconocido: ese archivo no participa en la deduplicación de enlaces).
    pub file_id: u128,
}

/// Resultado de listar un directorio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirOutcome {
    /// Listado completo.
    Listed,
    /// No se pudo listar (código de error del sistema); el directorio queda con el flag `ERROR`.
    Error(u32),
}

/// Entradas de un directorio (o un grupo de entradas sueltas) con sus nombres.
#[derive(Debug)]
pub struct Batch {
    pub(crate) dir: Option<u32>,
    pub(crate) outcome: DirOutcome,
    pub(crate) base_id: u32,
    pub(crate) records: Vec<Record>,
    pub(crate) file_ids: Vec<u128>,
    pub(crate) names: Vec<u8>,
}

impl Batch {
    /// Lote con el contenido del directorio `dir`; sus entradas ocupan los ids `base_id..`.
    ///
    /// Entregar un lote por CADA directorio, aunque esté vacío o falle, es lo que permite al builder distinguir un
    /// directorio vacío de uno que nunca se llegó a listar (`INCOMPLETE`).
    pub fn listing(dir: u32, base_id: u32) -> Batch {
        Batch::new(Some(dir), base_id, 0, 0)
    }

    /// Como [`Batch::listing`], reservando espacio para `entries` entradas y `name_bytes` bytes de nombres.
    pub fn listing_with_capacity(
        dir: u32,
        base_id: u32,
        entries: usize,
        name_bytes: usize,
    ) -> Batch {
        Batch::new(Some(dir), base_id, entries, name_bytes)
    }

    /// Lote sin directorio asociado (la raíz, o los ancestros precreados por la cola prioritaria): el `parent` de
    /// cada entrada se indica en [`Batch::push`].
    pub fn detached(base_id: u32) -> Batch {
        Batch::new(None, base_id, 0, 0)
    }

    /// Lote vacío que indica que el directorio `dir` no se pudo listar (`code` = error del sistema).
    pub fn failed(dir: u32, code: u32) -> Batch {
        let mut b = Batch::new(Some(dir), 0, 0, 0);
        b.outcome = DirOutcome::Error(code);
        b
    }

    fn new(dir: Option<u32>, base_id: u32, entries: usize, name_bytes: usize) -> Batch {
        Batch {
            dir,
            outcome: DirOutcome::Listed,
            base_id,
            records: Vec::with_capacity(entries),
            file_ids: Vec::with_capacity(entries),
            names: Vec::with_capacity(name_bytes),
        }
    }

    /// Primer id de este lote.
    pub fn base_id(&self) -> u32 {
        self.base_id
    }

    /// Número de ids que ocupa el lote (entradas más huecos).
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// ¿No ocupa ningún id?
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Añade una entrada con el nombre en WTF-8 y devuelve su id provisional (`base_id + índice`).
    /// Los nombres de más de 65 535 bytes se recortan (Windows no pasa de 255 unidades UTF-16).
    pub fn push(&mut self, parent: u32, name_wtf8: &[u8], e: EntryData) -> u32 {
        let name = &name_wtf8[..name_wtf8.len().min(u16::MAX as usize)];
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        self.push_record(parent, off, name.len() as u16, e)
    }

    /// Como [`Batch::push`] con el nombre en UTF-16 (lo que devuelve Windows), convirtiéndolo a WTF-8.
    pub fn push_utf16(&mut self, parent: u32, name: &[u16], e: EntryData) -> u32 {
        let off = self.names.len();
        wtf8::encode_utf16(name, &mut self.names);
        let len = (self.names.len() - off).min(u16::MAX as usize);
        self.names.truncate(off + len);
        self.push_record(parent, off as u32, len as u16, e)
    }

    /// Reserva un id sin entrada.
    pub fn push_hole(&mut self) -> u32 {
        self.records.push(Record::HOLE);
        self.file_ids.push(0);
        self.base_id + (self.records.len() - 1) as u32
    }

    fn push_record(&mut self, parent: u32, name_off: u32, name_len: u16, e: EntryData) -> u32 {
        debug_assert_ne!(parent, NO_PARENT);
        self.records.push(Record {
            alloc: e.alloc,
            logical: e.logical,
            parent,
            name_off,
            mtime: e.mtime,
            name_len,
            flags: e.flags,
        });
        self.file_ids.push(e.file_id);
        self.base_id + (self.records.len() - 1) as u32
    }
}
