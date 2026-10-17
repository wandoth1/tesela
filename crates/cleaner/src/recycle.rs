//! Windows Recycle Bin: send paths to it, list its contents, and purge an enumerated list through COM (`windows`).
//!
//! `IFileOperation` works by PATH and cannot be bound to a handle. The design (without tombstones) is:
//! 1. The caller opens the item exclusively, captures its identity, detects "in use", then CLOSES the handle;
//! 2. This module sends the ORIGINAL PATH to the Recycle Bin with a custom `IFileOperationProgressSink` ([`Recorder`]) that
//!    records `hrDelete` and the actual path of the item in the Recycle Bin (`psiNewlyCreated`) for each item;
//! 3. The caller opens THAT path (`$R…`) and compares its identity with the approved identity.
//!
//! There is a race window between the check in step 1 and the shell opening the item. The GUARD handle

//! (`DELETE | READ_ATTRIBUTES`, sharing all access) that the caller keeps open through step 3 prevents
//! renaming the item's ANCESTORS (parent and grandparent: error 5), so an ancestor cannot be replaced with a
//! junction to another folder (measured in the audit and covered by tests); the shell still recycles the item. The guard does NOT
//! prevent renaming the item itself (the handle shares `DELETE`, which the shell needs to move it into `$Recycle.Bin`) or
//! replacing it with another object of the same name. The event sink handles this (see below): `PreDeleteItem` checks the
//! IDENTITY of the object at the path immediately before the shell acts.
//!
//! The actual guarantee is: (a) the path and protection check happens first and is maintained by the guard handle;
//! (b) `PreDeleteItem` vetoes anything other than the approved object, with a RESIDUAL window between that check and the
//! shell's actual move (the shell opens and moves by path, so it cannot be bound to a handle); (c) anything that reaches the
//! Recycle Bin is checked by IDENTITY; `Recycled` also requires that the approved name no longer exists and that the guarded object
//! is now inside a Recycle Bin (another name for the same object, such as a hard link, does not count); (d)
//! anything other than the approved object that reaches the Recycle Bin with a known path is reported as `RecycledOther` (it is there and
//! can be restored). This does NOT guarantee that a replacement will end up there; see the residual risk below.
//!
//! Flags (determined by tests, see `tests::wantnukewarning_*`):
//! * enviar: `FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_NO_CONNECTED_ELEMENTS`;
//! * purge Recycle Bin items: `FOF_NO_UI | FOF_NO_CONNECTED_ELEMENTS`.
//!
//! `FOF_NO_CONNECTED_ELEMENTS` (S1): without it, approving `page.html` can also include `page_files\`, which was not approved.
//!
//! # The event sink guards operations (M1 and M2)
//! * `PreDeleteItem` (M1) opens each item by path without following links and compares its IDENTITY with the approved one; if they do not
//!   match (or the item cannot be opened, or is not the approved item), it records a [`Veto`] and returns `E_ABORT`: the shell cancels
//!   pending operations and does NOT send the replacement (fail closed). Precisely stated RESIDUAL RISK: between this check and the
//!   actual move, the shell acts by path and a replacement placed exactly there DOES reach the shell. If it fits in the
//!   Recycle Bin and the shell provides its path, step 3 detects it as `RecycledOther` (recoverable). Otherwise (the allocation and
//!   capacity were calculated for the APPROVED object, not the replacement; or the shell provides no path; or the move fails), report it as
//!   `DeletedUnexpectedly` or `Unverified`; a permanent deletion of the replacement CANNOT be undone.
//! * `PreDeleteItem` (A2-B1) also remeasures, THROUGH THE GUARD HANDLE (without reopening the path), every FILE in
//!   a batch with other items: if it exceeds its accounted size in the operation budget, veto it with `E_ABORT` (`Grew`). A
//!   directory is never batched: it travels alone and is measured immediately before submission. RESIDUAL RISK: growth while the
//!   shell operation is underway (between `PreDeleteItem` and the actual move).
//! * `PostDeleteItem` (M2) records the event FIRST and, if the shell permanently deleted the item (`psiNewlyCreated` is null) or did not
//!   say where it went, returns `E_ABORT`: the first unexpected deletion does not affect the rest of the batch. (A2-M1) A
//!   FAILURE result (`hrDelete < 0`) also stops the batch: partial data loss may have occurred, and the next item may be at risk.
//!   This applies only when SENDING items; permanent deletion is expected when emptying the Recycle Bin, each item
//!   is independent, and a failure does not stop the operation (purging has no partial-loss risk to avoid).
//!
//! # Emptying the Recycle Bin (A-M4)
//! Items in the list are verified by identity (`$R` and `$I`) during enumeration, and the shell acts later. To close this
//! window, process items in batches of up to 500:
//! * Open a GUARD handle for each `$R` (`DELETE | READ_ATTRIBUTES`, sharing all access) immediately before the operation and require it to
//!   be the captured object: while it is open, no one can rename `$R`'s ancestors (the user's folder under `$Recycle.Bin`);
//!   verified with the real shell: purging works with the guard handles open;
//! * Submit the operation with a sink whose `PreDeleteItem` rechecks by path (without following links) the identity of
//!   `$R` and `$I`, and vetoes with `E_ABORT` anything that no longer matches the captured items. A Recycle Bin folder reaches `PreDeleteItem`
//!   one child at a time (verified): items under an approved `$R` are checked against that `$R`'s identity;
//! * Afterward, and before releasing the guards, verify that every item the shell reported as purged was the approved object
//!   (`delete_pending`); otherwise record it as "replaced" (`replaced_bin`).
//!
//! Verified with the real shell after a purge veto: the shell reports the veto as `E_ABORT` (`0x80004004`, "Operation
//! aborted"), purges earlier items in the batch, and does NOT process later items (they remain in the Recycle Bin). Stop remaining batches.
//!
//! REMAINING race window: between the `PreDeleteItem` check (by path) and the shell's actual deletion. The shell cannot be
//! bound to a handle. A replacement placed exactly there IS deleted (purging is permanent and cannot be
//! undone); it is detected immediately afterward (the guard is not pending deletion) and recorded as replaced, but is not prevented.
#![allow(unsafe_code)]

use crate::paths::PathKey;
use crate::sys::{self, FileIdentity};
use std::ffi::{OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{E_ABORT, FILETIME, PROPERTYKEY};
use windows::Win32::Storage::EnhancedStorage::PKEY_Size;
use windows::Win32::System::Com::StructuredStorage::PropVariantToBSTR;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
#[cfg(test)]
use windows::Win32::UI::Shell::FOF_WANTNUKEWARNING;
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NO_CONNECTED_ELEMENTS, FOF_NO_UI,
    FOFX_RECYCLEONDELETE, FOLDERID_RecycleBinFolder, FileOperation, IEnumShellItems,
    IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem,
    IShellItem2, KF_FLAG_DEFAULT, PID_DISPLACED_DATE, PID_DISPLACED_FROM, PSGUID_DISPLACED,
    SHCreateItemFromParsingName, SHGetKnownFolderItem, SHQUERYRBINFO, SHQueryRecycleBinW, SIGDN,
    SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH, SIGDN_PARENTRELATIVE,
};
use windows::core::{HRESULT, Interface, PCWSTR, PWSTR, Ref, implement};

/// Shell or COM error (`HRESULT` code and technical description; the interface translates it for display).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellError {
    pub hresult: i32,
    pub detail: String,
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "shell error 0x{:08X}: {}",
            self.hresult as u32, self.detail
        )
    }
}

impl std::error::Error for ShellError {}

impl From<windows::core::Error> for ShellError {
    fn from(e: windows::core::Error) -> ShellError {
        ShellError {
            hresult: e.code().0,
            detail: e.message(),
        }
    }
}

type Result<T> = std::result::Result<T, ShellError>;

/// COM initialized on the current thread (single-threaded apartment, as required by the shell). Uninitialize on drop,
/// unless the thread was already initialized in another mode (`RPC_E_CHANGED_MODE`), in which case keep using it as is.
pub struct Com {
    uninit: bool,
}

impl Com {
    pub fn init() -> Result<Com> {
        // SAFETY: `CoInitializeEx` has no preconditions; the reserved pointer is null, as required by the API.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_ok() {
            // S_OK y S_FALSE ("ya inicializado en este hilo") se equilibran cada uno con su `CoUninitialize`.
            Ok(Com { uninit: true })
        } else if hr.0 == 0x8001_0106u32 as i32 {
            Ok(Com { uninit: false })
        } else {
            Err(ShellError {
                hresult: hr.0,
                detail: "CoInitializeEx".into(),
            })
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: balances the successful `CoInitializeEx` call from `init` on this same thread.
            unsafe { CoUninitialize() };
        }
    }
}

const SCID_ORIGINAL_LOCATION: PROPERTYKEY = PROPERTYKEY {
    fmtid: PSGUID_DISPLACED,
    pid: PID_DISPLACED_FROM,
};
const SCID_DATE_DELETED: PROPERTYKEY = PROPERTYKEY {
    fmtid: PSGUID_DISPLACED,
    pid: PID_DISPLACED_DATE,
};

/// Ruta en la forma que acepta `SHCreateItemFromParsingName` (sin `\\?\`), UTF-16 con NUL final.
fn parsing_name(path: &Path) -> Vec<u16> {
    let p = crate::sys::strip_verbatim(path.to_path_buf());
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// Un elemento de la Papelera tal como lo ve la shell.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BinItem {
    /// Nombre de análisis absoluto en la Papelera (el identificador estable del elemento).
    pub id: OsString,
    /// Nombre con el que se ve en la Papelera (el original; la shell puede ocultar la extensión).
    pub name: OsString,
    /// Carpeta de la que se borró.
    pub original_parent: PathBuf,
    /// Cuándo se borró (segundos Unix).
    pub time_deleted: i64,
    /// Tamaño que da la shell (`System.Size`; 0 si no lo da, como en las carpetas).
    pub size: u64,
    /// Identidad (volumen + FileId) del `$R` (los datos) al enumerar. Al purgar se vuelve a leer: si no coincide, el
    /// elemento cambió y se salta. `None` = no se pudo leer al enumerar: no se puede verificar y no se purga.
    pub data_id: Option<FileIdentity>,
    /// Identidad del `$I` hermano (los metadatos) al enumerar (misma regla).
    pub meta_id: Option<FileIdentity>,
    /// El sistema de archivos del `$R` no da identidad (FAT, exFAT: `FileIdInfo` no existe): el elemento no se puede
    /// verificar y no se purga, pero NO es que haya "cambiado": se cuenta aparte (H6.9).
    pub no_identity: bool,
}

/// Identidad (volumen + FileId) de lo que hay en `p`, sin seguir enlaces. `None` si no se puede abrir o leer.
fn identity_at(p: &Path) -> Option<FileIdentity> {
    identity_result(p).ok()
}

/// Como [`identity_at`], con el código de Windows si falla.
fn identity_result(p: &Path) -> std::result::Result<FileIdentity, sys::Win32> {
    sys::open_for_query(p)
        .and_then(|h| sys::meta(&h))
        .map(|m| m.identity)
}

/// ¿Es el código de "este sistema de archivos no da `FileIdInfo`"? (`ERROR_INVALID_FUNCTION`, `ERROR_NOT_SUPPORTED`,
/// `ERROR_INVALID_PARAMETER`, como en `plan::verify`.)
fn is_no_identity_error(code: sys::Win32) -> bool {
    matches!(code, 1 | 50 | 87)
}

/// Qué hacer con un elemento de la lista aprobada al purgar.
#[derive(Debug, PartialEq, Eq)]
enum BinCheck {
    /// Sigue siendo el mismo: se purga.
    Same,
    /// Cambió desde que se enumeró: se salta ("cambió").
    Changed,
    /// Su sistema de archivos no da identidad: no se puede verificar, se salta (motivo propio).
    Unverifiable,
}

fn check_bin_item(item: &BinItem) -> BinCheck {
    if item.no_identity {
        BinCheck::Unverifiable
    } else if bin_item_unchanged(item) {
        BinCheck::Same
    } else {
        BinCheck::Changed
    }
}

/// El `$I` hermano EXACTO de un `$R`: `$R<token>[.ext]` -> `$I<token>[.ext]`. `None` si no es un `$R`.
fn meta_sibling(data: &Path) -> Option<PathBuf> {
    let name = data.file_name()?.to_str()?;
    (name.starts_with("$R") && name.len() > 2)
        .then(|| data.with_file_name(format!("$I{}", &name[2..])))
}

/// Is this still the approved item? Reread the `$R` and `$I` identities and compare them with those captured during enumeration.
/// If no identity was captured or either one differs (someone emptied and refilled the Recycle Bin, or replaced the files),
/// skip the item ("changed since confirmation").
fn bin_item_unchanged(item: &BinItem) -> bool {
    let data = PathBuf::from(&item.id);
    let (Some(want_data), Some(want_meta)) = (item.data_id, item.meta_id) else {
        return false;
    };
    identity_at(&data) == Some(want_data)
        && meta_sibling(&data).and_then(|m| identity_at(&m)) == Some(want_meta)
}

impl BinItem {
    /// Ruta de la que se borró el elemento.
    pub fn original_path(&self) -> PathBuf {
        self.original_parent.join(&self.name)
    }
}

/// Copy a shell `PWSTR` to a string and free it with `CoTaskMemFree`.
///
/// # Safety
/// `p` must be a NUL-terminated UTF-16 string allocated with `CoTaskMemAlloc` (such as those returned by
/// `IShellItem::GetDisplayName`), and must not be used again.
unsafe fn take_pwstr(p: PWSTR) -> OsString {
    // SAFETY: the caller guarantees `p` is a valid NUL-terminated string until it is freed.
    let s = unsafe { p.as_wide() };
    let out = OsString::from_wide(s);
    // SAFETY: the string was allocated by COM and is freed exactly once, after copying it.
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    out
}

fn filetime_to_unix(ft: FILETIME) -> i64 {
    const EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
    let t = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
    (t.saturating_sub(EPOCH_AS_FILETIME) / 10_000_000) as i64
}

/// List ALL items in the user's Recycle Bin (all drives). Runtime is linear in the size of the Recycle Bin
/// (tens of seconds for 100,000 items, PLAN.md §3.6): callers should run this on a worker thread.
///
/// GM-S1: skip an item with corrupt metadata (`$I`) (unreadable original path, date, or name) and record that it was not
/// listed; previously, one corrupt `$I` aborted the entire listing and all Recycle Bin cleanup. An unlisted item is never
/// emptied (it stays in place). Only an enumeration failure aborts the operation.
pub fn list_bin() -> Result<Vec<BinItem>> {
    list_bin_counted().map(|(items, _)| items)
}

/// Como [`list_bin`], y además cuántos elementos se SALTARON por tener los metadatos ilegibles (m1 de la auditoría): quien
/// llama lo anota en el plan, en el registro y en el informe, y no cuenta su tamaño en lo que promete vaciar.
pub fn list_bin_counted() -> Result<(Vec<BinItem>, u64)> {
    let _com = Com::init()?;
    // SAFETY: COM is initialized on this thread; all interfaces and strings obtained here belong to this function and
    // are released by `windows` RAII or `take_pwstr`.
    unsafe {
        let bin: IShellItem =
            SHGetKnownFolderItem(&FOLDERID_RecycleBinFolder, KF_FLAG_DEFAULT, None)?;
        let en: IEnumShellItems = bin.BindToHandler(None, &BHID_EnumItems)?;
        let mut out = Vec::new();
        let mut skipped = 0u64;
        loop {
            let mut fetched = 0u32;
            let mut arr = [None];
            en.Next(&mut arr, Some(&mut fetched))?;
            if fetched == 0 {
                break;
            }
            let Some(item) = &arr[0] else { break };
            // SAFETY (call): `item` is a live interface from this enumeration.
            if !push_readable(&mut out, read_bin_item(item)) {
                skipped += 1;
            }
        }
        Ok((out, skipped))
    }
}

/// Añade el elemento si se pudo leer; si no (su `$I` está corrupto), lo salta (GM-S1). Devuelve si se añadió.
fn push_readable(out: &mut Vec<BinItem>, item: Result<BinItem>) -> bool {
    match item {
        Ok(i) => {
            out.push(i);
            true
        }
        Err(_) => false,
    }
}

/// Lee un elemento enumerado de la Papelera (nombre, origen, fecha, tamaño e identidades del `$R` y del `$I`).
///
/// # Safety
/// `item` debe ser una interfaz viva, y COM debe estar inicializado en el hilo que llama.
unsafe fn read_bin_item(item: &IShellItem) -> Result<BinItem> {
    // SAFETY: the caller guarantees this; strings are freed with `take_pwstr` and the rest uses `windows` RAII.
    unsafe {
        let id = take_pwstr(item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING)?);
        let name = take_pwstr(item.GetDisplayName(SIGDN_PARENTRELATIVE)?);
        let item2: IShellItem2 = item.cast()?;
        let loc = item2.GetProperty(&SCID_ORIGINAL_LOCATION)?;
        let loc = PropVariantToBSTR(&loc)?;
        let original_parent = PathBuf::from(OsString::from_wide(&loc));
        let time_deleted = filetime_to_unix(item2.GetFileTime(&SCID_DATE_DELETED)?);
        let size = item2.GetUInt64(&PKEY_Size).unwrap_or(0);
        // Las identidades del `$R` y del `$I`, para volver a comprobarlas al purgar (M3).
        let data = PathBuf::from(&id);
        let data_res = identity_result(&data);
        let no_identity = data_res.as_ref().is_err_and(|c| is_no_identity_error(*c));
        let data_id = data_res.ok();
        let meta_id = meta_sibling(&data).and_then(|m| identity_at(&m));
        Ok(BinItem {
            id,
            name,
            original_parent,
            time_deleted,
            size,
            data_id,
            meta_id,
            no_identity,
        })
    }
}

/// Number of items and bytes in the user's Recycle Bin (all drives), as reported by the shell.
pub fn bin_totals() -> Result<(u64, u64)> {
    let mut info = SHQUERYRBINFO {
        cbSize: size_of::<SHQUERYRBINFO>() as u32,
        ..SHQUERYRBINFO::default()
    };
    // SAFETY: `info` is writable and has its `cbSize` set; a null path queries all volumes.
    unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info)? };
    Ok((info.i64NumItems.max(0) as u64, info.i64Size.max(0) as u64))
}

/// What happened to an item, according to the shell (`IFileOperationProgressSink::PostDeleteItem`).
///
/// Semántica (documentación de Microsoft, `PostDeleteItem`, learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/
/// nf-shobjidl_core-ifileoperationprogresssink-postdeleteitem): `hrDelete` es el resultado de la eliminación real (no el
/// de `DeleteItem`, que solo la encola) y `psiNewlyCreated` es "el elemento eliminado, ahora en la Papelera"; si el
/// elemento se eliminó por completo, es NULL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BinState {
    /// `psiNewlyCreated` es nulo: la shell lo eliminó definitivamente (por ejemplo, porque no cabía en la Papelera).
    Deleted,
    /// Está en la Papelera; esta es su ruta real (`$R…`), que se puede abrir para comprobar su identidad.
    InBin(PathBuf),
    /// Está en la Papelera pero la shell no dio su ruta real.
    InBinUnknownPath,
}

/// Lo que la shell notificó de un elemento.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteEvent {
    /// Nombre de análisis del elemento original: su ruta, o el `id` de un elemento de la Papelera.
    pub item: OsString,
    /// `hrDelete`: `>= 0` es éxito.
    pub hr: i32,
    pub state: BinState,
}

impl DeleteEvent {
    pub fn succeeded(&self) -> bool {
        self.hr >= 0
    }
}

/// Lo que ocupa la Papelera del usuario EN UN VOLUMEN (`root`, p. ej. la raíz de la unidad), según la shell.
pub fn bin_size_on(root: &Path) -> Result<u64> {
    let mut info = SHQUERYRBINFO {
        cbSize: size_of::<SHQUERYRBINFO>() as u32,
        ..SHQUERYRBINFO::default()
    };
    let w = parsing_name(root);
    // SAFETY: `w` is NUL-terminated and remains alive for the call; `info` is writable and has its `cbSize` set.
    unsafe { SHQueryRecycleBinW(PCWSTR(w.as_ptr()), &mut info)? };
    Ok(info.i64Size.max(0) as u64)
}

/// ¿Está `p` dentro de una Papelera (`<raíz del volumen>\$Recycle.Bin\...`)? La raíz del volumen es la de una unidad con letra o
/// la de un volumen montado en una carpeta (GM-M6: `C:\montajes\datos\$Recycle.Bin\...`); se consulta a Windows
/// (`GetVolumePathNameW`) por la raíz de `p`.
pub fn is_recycle_bin_path(p: &Path) -> bool {
    is_recycle_bin_path_on(p, crate::volume::volume_root(p).as_deref())
}

/// [`is_recycle_bin_path`] con la raíz del volumen de `p` ya conocida (`None` = no se sabe: solo valen las raíces de unidad
/// con letra). Mira la forma de la ruta, sin consultar al sistema.
pub(crate) fn is_recycle_bin_path_on(p: &Path, volume_root: Option<&Path>) -> bool {
    use crate::paths::PathKey;
    let key = PathKey::new(p);
    let root = volume_root.map(PathKey::new);
    let is_bin = |k: &PathKey| {
        k.file_name() == "$recycle.bin"
            && k.parent()
                .is_some_and(|q| q.is_drive_root() || root.as_ref() == Some(&q))
    };
    is_bin(&key) || key.ancestors().iter().any(is_bin)
}

/// Resultado de una operación por lotes de la shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchOutcome {
    /// Un evento por cada elemento que la shell llegó a procesar (en el orden en que los procesó).
    pub events: Vec<DeleteEvent>,
    /// `GetAnyOperationsAborted`: la shell abortó al menos una operación (sin decir cuál).
    pub any_aborted: bool,
    /// `PerformOperations` falló (la parte que ya se hizo consta en `events`).
    pub error: Option<ShellError>,
    /// Solo al purgar: `$I` de elementos purgados que la shell dejó huérfanos y que se quitaron (ver
    /// [`remove_orphan_meta`]).
    pub meta_removed: u64,
    /// Solo al enviar: el receptor CANCELÓ el lote (devolvió un error de aborto) por un veto o por un borrado definitivo
    /// inesperado. Lo que no tiene evento sigue en su sitio.
    pub sink_aborted: bool,
    /// Solo al enviar: elementos a los que `PreDeleteItem` puso veto (la identidad no era la aprobada).
    pub vetoed: Vec<Veto>,
    /// Solo al purgar: elementos de la lista que NO se purgaron porque su `$R` o su `$I` ya no tenían la identidad
    /// capturada al enumerar (o no se capturó): `id` de cada uno.
    pub changed_bin: Vec<OsString>,
    /// Solo al purgar: elementos cuya purga la shell dio por buena pero cuyo objeto aprobado NO se eliminó (se borró otro
    /// objeto con su nombre tras la última comprobación): `id` de cada uno. No cuentan como purgados.
    pub replaced_bin: Vec<OsString>,
    /// Solo al purgar: elementos que NO se purgaron porque su sistema de archivos no da identidad (FAT, exFAT) y no se
    /// pueden verificar: `id` de cada uno. No es que hayan cambiado (H6.9).
    pub unverifiable_bin: Vec<OsString>,
    /// Solo al purgar (M2): elementos cuyo `$R` lleva reparse points dentro (junction, symlink, enlace de WSL, archivo de la
    /// nube) o no se pudo comprobar entero: se EXCLUYEN antes de entregarlos a la shell (no se purgan y no cortan el lote).
    pub linked_bin: Vec<OsString>,
}

/// Un elemento que se entrega a la shell: su ruta original y la IDENTIDAD aprobada (volumen + FileId).
#[derive(Clone, Debug)]
pub struct SendItem {
    pub path: PathBuf,
    pub identity: FileIdentity,
    /// A2-B1: solo un ARCHIVO que viaja en un lote con otros. `PreDeleteItem` vuelve a medirlo por su handle de guarda (sin
    /// reabrir la ruta) y veta si ocupa más de lo que se contó para el presupuesto. Un directorio va siempre solo en su
    /// operación y se mide justo antes de entregarlo: no lleva comprobación aquí.
    pub size_guard: Option<SizeGuard>,
    /// A3-M1: solo un DIRECTORIO. La vigilancia de cambios abierta ANTES de medirlo y viva hasta ahora: si algo cambió dentro
    /// desde que se midió, la medida no es cierta. `PreDeleteItem` la consulta (veto) y, si pasa, la para.
    pub watch: Option<Arc<sys::DirWatch>>,
}

/// Cómo vuelve a medirse un archivo en `PreDeleteItem` (A2-B1): su handle de guarda y el tamaño con el que se contó en el
/// reparto.
#[derive(Clone)]
pub struct SizeGuard {
    /// El handle de guarda del ejecutor (`DELETE | READ_ATTRIBUTES`, compartiéndolo todo). Sigue abierto durante toda la
    /// operación.
    pub handle: Arc<sys::Handle>,
    /// Lo que se contó para el presupuesto de la operación: el mayor entre bytes en disco y tamaño lógico.
    pub budget: u64,
}

impl std::fmt::Debug for SizeGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SizeGuard")
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

impl SizeGuard {
    /// ¿Sigue el archivo dentro de lo que se contó? Lo lee por el handle de guarda: no hay reapertura por ruta ni ventana de
    /// sustitución. Si no se puede leer, se veta (falla cerrado).
    fn check(&self) -> std::result::Result<(), VetoKind> {
        match sys::sizes(&self.handle) {
            Ok(s) if s.charge() > self.budget => Err(VetoKind::Grew),
            Ok(_) => Ok(()),
            Err(c) => Err(VetoKind::OpenFailed(c)),
        }
    }
}

impl SendItem {
    /// Tests only: captures the current identity of `path` (in production the executor approves it; it is not read here).
    #[cfg(test)]
    pub(crate) fn capture(path: &Path) -> SendItem {
        let h = sys::open_for_query(path).expect("abre el elemento");
        SendItem {
            path: path.to_path_buf(),
            identity: sys::meta(&h).expect("lee la identidad").identity,
            size_guard: None,
            watch: None,
        }
    }
}

/// Por qué `PreDeleteItem` vetó un elemento.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VetoKind {
    /// Lo que hay ahora en esa ruta no es el objeto aprobado (sustituido tras la comprobación).
    IdentityMismatch,
    /// No se pudo abrir o leer (código de Windows): ante la duda no se envía.
    OpenFailed(u32),
    /// La shell va a borrar algo que no estaba en la lista aprobada (por ejemplo un elemento conectado).
    NotApproved,
    /// A2-B1: un archivo del lote ocupa ahora MÁS de lo que se contó para el presupuesto de la operación (creció desde que
    /// se midió). Se veta: el reparto se hizo con un tamaño que ya no es cierto.
    Grew,
    /// A3-M1: algo DENTRO de un directorio cambió desde que se midió (la vigilancia que lo cubre lo vio, se desbordó o falló):
    /// la medida ya no es cierta y el reparto se hizo con ella. Se veta.
    Modified,
}

/// Un veto del receptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Veto {
    /// Nombre de análisis de lo que se vetó (su ruta). Al purgar una CARPETA de la Papelera es la ruta de un HIJO.
    pub item: OsString,
    /// El `id` (ruta del `$R`) del elemento aprobado de la lista al que pertenece lo vetado, si lo había (un hijo de una
    /// carpeta de la Papelera): a quien llama le sirve para saber QUÉ elemento de su lista se vetó (M-2). `None` si no
    /// pertenecía a ninguno.
    pub owner: Option<OsString>,
    pub kind: VetoKind,
}

impl Veto {
    /// El elemento de la lista aprobada al que se atribuye el veto: su dueño si lo hay y, si no, la propia ruta.
    pub fn id(&self) -> &OsString {
        self.owner.as_ref().unwrap_or(&self.item)
    }
}

type Events = Arc<Mutex<Vec<DeleteEvent>>>;

/// Nombre de análisis o ruta de un elemento de la shell, si se puede.
fn display(item: &IShellItem, sigdn: SIGDN) -> Option<OsString> {
    // SAFETY: `item` is a live interface; the returned string is owned by COM and `take_pwstr` frees it exactly once.
    unsafe { item.GetDisplayName(sigdn).ok().map(|p| take_pwstr(p)) }
}

/// Estado que el receptor comparte con quien lo creó.
#[derive(Default)]
struct SinkState {
    /// El lote está cancelado: cualquier `PreDeleteItem` posterior falla (defensa por si la shell sigue tras un error).
    tripped: AtomicBool,
    vetoes: Mutex<Vec<Veto>>,
}

/// Qué se aprobó para una ruta.
enum Approved {
    /// Al ENVIAR: la identidad del objeto en su ruta original y, en un archivo de un lote, cómo volver a medirlo (A2-B1).
    Object {
        identity: FileIdentity,
        size: Option<SizeGuard>,
        /// A3-M1: la vigilancia de cambios de un directorio (ver [`SendItem::watch`]).
        watch: Option<Arc<sys::DirWatch>>,
    },
    /// Al PURGAR (A-M4): las identidades del `$R` (los datos) y del `$I` hermano (los metadatos) capturadas al enumerar. La
    /// ruta es la del `$R` (el `id` del elemento de la Papelera).
    Bin {
        data_path: PathBuf,
        data: FileIdentity,
        meta_path: PathBuf,
        meta: FileIdentity,
    },
}

/// Lo aprobado, por ruta: lo que `PreDeleteItem` comprueba antes de que la shell actúe.
struct Guard {
    approved: Vec<(PathKey, Approved)>,
    /// El nombre EXACTO (tal como se entregó) de cada elemento de `approved`, en el mismo orden (GM-M5): en un directorio
    /// sensible a mayúsculas dos nombres que solo difieren en ellas tienen la misma `PathKey` (que pliega mayúsculas).
    names: Vec<OsString>,
    /// Es la guarda de una PURGA de la Papelera (si no, de un envío).
    purge: bool,
}

impl Guard {
    /// Guarda de un ENVÍO a la Papelera (ruta original + identidad aprobada).
    fn new(items: &[SendItem]) -> Guard {
        Guard {
            names: items
                .iter()
                .map(|i| i.path.as_os_str().to_os_string())
                .collect(),
            approved: items
                .iter()
                .map(|i| {
                    (
                        PathKey::new(&i.path),
                        Approved::Object {
                            identity: i.identity,
                            size: i.size_guard.clone(),
                            watch: i.watch.clone(),
                        },
                    )
                })
                .collect(),
            purge: false,
        }
    }

    /// Guarda de una PURGA (A-M4): cada elemento de la lista con las identidades de su `$R` y de su `$I` capturadas al
    /// enumerar. Sin las dos, el elemento no se purga (nunca llega aquí: ver `check_bin_item`); si falta alguna se vetaría.
    fn purge(items: &[&BinItem]) -> Guard {
        let kept: Vec<&&BinItem> = items
            .iter()
            .filter(|i| {
                i.data_id.is_some()
                    && i.meta_id.is_some()
                    && meta_sibling(Path::new(&i.id)).is_some()
            })
            .collect();
        Guard {
            names: kept.iter().map(|i| i.id.clone()).collect(),
            approved: items
                .iter()
                .filter_map(|i| {
                    let data = PathBuf::from(&i.id);
                    let (want_data, want_meta) = (i.data_id?, i.meta_id?);
                    let meta_path = meta_sibling(&data)?;
                    Some((
                        PathKey::new(&data),
                        Approved::Bin {
                            data_path: data,
                            data: want_data,
                            meta_path,
                            meta: want_meta,
                        },
                    ))
                })
                .collect(),
            purge: true,
        }
    }

    /// ¿Es una guarda de ENVÍO? Solo al enviar un borrado definitivo inesperado corta el lote (al purgar es lo esperado).
    fn sends(&self) -> bool {
        !self.purge
    }

    /// El elemento aprobado al que corresponde `path`. Al PURGAR, un elemento de la Papelera que es una CARPETA llega a
    /// `PreDeleteItem` hijo a hijo (medido con la shell real: `...\$R<token>\hijo.txt`), no con la ruta de su `$R`: lo que
    /// cuelga de un `$R` aprobado se atribuye a ESE `$R`. Al ENVIAR vale solo la ruta exacta (un evento por carpeta).
    fn find(&self, path: &OsString) -> Option<&(PathKey, Approved)> {
        // GM-M5: primero el nombre exacto; la clave (que pliega mayúsculas) solo si no hay ambigüedad.
        if let Some(i) = crate::paths::owner_index(&self.names, path) {
            return self.approved.get(i);
        }
        let key = PathKey::new(Path::new(path));
        self.purge
            .then(|| self.approved.iter().find(|(k, _)| key.is_same_or_under(k)))
            .flatten()
    }

    /// El `id` (ruta del `$R`) del elemento de una purga al que pertenece `path` (él mismo o uno de sus hijos); `None` si no
    /// pertenece a ninguno o si es una guarda de envío.
    fn owner_of(&self, path: &OsString) -> Option<OsString> {
        match self.find(path) {
            Some((_, Approved::Bin { data_path, .. })) => {
                Some(data_path.as_os_str().to_os_string())
            }
            _ => None,
        }
    }

    /// ¿Es `path` lo aprobado? Abre por ruta sin seguir enlaces y compara la identidad. SOLO pasa lo que está aprobado
    /// por esa ruta exacta: un hijo de un elemento aprobado, un alias (otro nombre del mismo objeto) o cualquier otra cosa
    /// se vetan (`NotApproved`). Con la shell real una carpeta emite un único evento, así que la rama "los hijos pasan"
    /// era inalcanzable y fallar cerrado es mejor (H6.1). Al purgar se comprueban el `$R` y el `$I`.
    fn check(&self, path: &OsString) -> std::result::Result<(), VetoKind> {
        let Some((_, want)) = self.find(path) else {
            return Err(VetoKind::NotApproved);
        };
        let same = |p: &Path, want: &FileIdentity| match sys::open_for_query(p)
            .and_then(|h| sys::meta(&h))
        {
            Ok(m) if m.identity == *want => Ok(()),
            Ok(_) => Err(VetoKind::IdentityMismatch),
            Err(c) => Err(VetoKind::OpenFailed(c)),
        };
        match want {
            Approved::Object {
                identity,
                size,
                watch,
            } => {
                same(Path::new(path), identity)?;
                // A2-B1: lo aprobado sigue siendo lo que había, pero un archivo que creció desde que se midió ya no cabe en lo
                // que se repartió: se mide por el handle de guarda (el mismo objeto), no por la ruta.
                if let Some(g) = size {
                    g.check()?;
                }
                // A3-M1: un directorio lleva la vigilancia de cambios que se abrió ANTES de medirlo. Si algo cambió dentro (o
                // la vigilancia falló), la medida no vale. Si no, es la última comprobación y se PARA aquí: no queda un handle
                // con E/S pendiente sobre el directorio mientras la shell lo mueve (y se vuelve a mirar tras parar: lo que
                // cayera en el último instante consta).
                if let Some(w) = watch {
                    if w.changed() {
                        return Err(VetoKind::Modified);
                    }
                    w.stop();
                    if w.changed() {
                        return Err(VetoKind::Modified);
                    }
                }
                Ok(())
            }
            Approved::Bin {
                data_path,
                data,
                meta_path,
                meta,
            } => {
                // GM-m5: al purgar una CARPETA de la Papelera la shell pregunta por sus hijos (`...\$R<token>\hijo`). Un hijo
                // solo vale si, abierto por su ruta sin seguir enlaces, no es un reparse point (un junction dentro del `$R`
                // llevaría el borrado fuera), está en el MISMO volumen que el `$R` aprobado y su ruta final cuelga de ese `$R`
                // (un ancestro enlazado la sacaría de él). Antes bastaba con que el texto de la ruta empezase por el `$R`.
                if PathKey::new(Path::new(path)) != PathKey::new(data_path) {
                    let h = sys::open_for_query(Path::new(path)).map_err(VetoKind::OpenFailed)?;
                    let m = sys::meta(&h).map_err(VetoKind::OpenFailed)?;
                    if m.is_reparse() || m.identity.volume_serial != data.volume_serial {
                        return Err(VetoKind::IdentityMismatch);
                    }
                    let under = sys::final_path(&h)
                        .map_err(VetoKind::OpenFailed)
                        .map(|f| PathKey::new(&f).is_same_or_under(&PathKey::new(data_path)))?;
                    if !under {
                        return Err(VetoKind::NotApproved);
                    }
                }
                // El `$R` aprobado (no el hijo por el que se pregunta, si es el caso) y su `$I`.
                same(data_path, data)?;
                same(meta_path, meta)
            }
        }
    }
}

/// El estado de un elemento según `psiNewlyCreated` de `PostDeleteItem`: `None` = nulo (eliminado definitivamente);
/// `Some(None)` = hay elemento pero no se pudo leer su ruta; `Some(Some(ruta))` = está en la Papelera. Pura.
fn bin_state(newly_created: Option<Option<PathBuf>>) -> BinState {
    match newly_created {
        None => BinState::Deleted,
        Some(Some(p)) => BinState::InBin(p),
        Some(None) => BinState::InBinUnknownPath,
    }
}

/// ¿Debe el receptor cortar el lote tras este `PostDeleteItem`? Solo al ENVIAR (`sending`: al purgar el borrado
/// definitivo es lo esperado y cada elemento es independiente) y en dos casos:
/// * la shell dijo que fue bien pero el elemento no está verificablemente en la Papelera: eliminado definitivamente
///   (`Deleted`) o sin ruta (`InBinUnknownPath`);
/// * (A2-M1) el resultado es un FALLO (`hrdelete < 0`): un fallo no prueba que la shell no tocara nada (puede haber perdido
///   un hijo de una carpeta antes de fallar) y no hay forma de garantizar que lo siguiente del lote no sufra lo mismo; se
///   corta lo pendiente por precaución. El ejecutor juzga después el elemento fallido por su objeto y su contenido.
///
/// Pura.
fn post_delete_aborts(sending: bool, hrdelete: i32, state: &BinState) -> bool {
    sending && (hrdelete < 0 || matches!(state, BinState::Deleted | BinState::InBinUnknownPath))
}

/// La lógica del receptor SIN COM (así se prueba en cualquier equipo): `Recorder` solo traduce los argumentos de la shell
/// y llama aquí.
pub(crate) struct SinkCore {
    events: Events,
    state: Arc<SinkState>,
    /// Obligatoria (M-3): sin guarda no hay receptor, así que una operación que borra (enviar o purgar) sin comprobar
    /// identidades no compila.
    guard: Guard,
}

impl SinkCore {
    /// The receiver for a SEND operation with guards for these items (the same logic used by the real shell; test shell
    /// simulations call it instead of reimplementing it).
    #[cfg(test)]
    pub(crate) fn for_send(items: &[SendItem]) -> SinkCore {
        SinkCore {
            events: Arc::new(Mutex::new(Vec::new())),
            state: Arc::new(SinkState::default()),
            guard: Guard::new(items),
        }
    }

    /// What the receiver recorded: events, vetoes, and whether it stopped the batch.
    #[cfg(test)]
    pub(crate) fn finish(&self) -> (Vec<DeleteEvent>, Vec<Veto>, bool) {
        let events = std::mem::take(&mut *self.events.lock().unwrap_or_else(|e| e.into_inner()));
        let vetoes =
            std::mem::take(&mut *self.state.vetoes.lock().unwrap_or_else(|e| e.into_inner()));
        (events, vetoes, self.state.tripped.load(Ordering::SeqCst))
    }

    /// `PreDeleteItem`: `true` = la shell puede seguir con este elemento; `false` = el receptor debe devolver `E_ABORT`.
    pub(crate) fn pre_delete(&self, item: OsString) -> bool {
        // Tests only: stage 2 hook (just before checking the item), including with the real shell.
        #[cfg(test)]
        crate::engine::test_hooks::before_pre_delete(Path::new(&item));
        let guard = &self.guard;
        // Defensa: si el lote ya está cancelado, ningún elemento más se procesa aunque la shell siguiera.
        if self.state.tripped.load(Ordering::SeqCst) {
            return false;
        }
        match guard.check(&item) {
            Ok(()) => true,
            Err(kind) => {
                // Veto: el sustituto NO se envía y se cancela el resto del lote (falla cerrado).
                self.state.tripped.store(true, Ordering::SeqCst);
                self.state
                    .vetoes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(Veto {
                        owner: guard.owner_of(&item),
                        item,
                        kind,
                    });
                false
            }
        }
    }

    /// `PostDeleteItem`: registra el evento PRIMERO. `true` = el receptor debe devolver `E_ABORT` (y el lote queda
    /// cancelado).
    pub(crate) fn post_delete(&self, item: OsString, hrdelete: i32, state: BinState) -> bool {
        let abort = post_delete_aborts(self.guard.sends(), hrdelete, &state);
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(DeleteEvent {
                item,
                hr: hrdelete,
                state,
            });
        if abort {
            self.state.tripped.store(true, Ordering::SeqCst);
        }
        abort
    }
}

/// El receptor de eventos de `IFileOperation`. Implementa la interfaz COM con `windows::core::implement` (la vtable,
/// `QueryInterface`, `AddRef` y `Release` los genera la macro). Al PURGAR solo registra. Al ENVIAR (con `guard`) además
/// vigila: veta lo que no es lo aprobado y corta el lote tras el primer borrado definitivo (ver el módulo). Un error de un
/// método del receptor hace que la shell cancele lo que quedara pendiente: es exactamente lo que se quiere.
#[implement(IFileOperationProgressSink)]
struct Recorder {
    core: SinkCore,
}

impl IFileOperationProgressSink_Impl for Recorder_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _hrresult: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(
        &self,
        _dwflags: u32,
        _psiitem: Ref<IShellItem>,
        _psznewname: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostRenameItem(
        &self,
        _dwflags: u32,
        _psiitem: Ref<IShellItem>,
        _psznewname: &PCWSTR,
        _hrrename: HRESULT,
        _psinewlycreated: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreMoveItem(
        &self,
        _dwflags: u32,
        _psiitem: Ref<IShellItem>,
        _psidestinationfolder: Ref<IShellItem>,
        _psznewname: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostMoveItem(
        &self,
        _dwflags: u32,
        _psiitem: Ref<IShellItem>,
        _psidestinationfolder: Ref<IShellItem>,
        _psznewname: &PCWSTR,
        _hrmove: HRESULT,
        _psinewlycreated: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreCopyItem(
        &self,
        _dwflags: u32,
        _psiitem: Ref<IShellItem>,
        _psidestinationfolder: Ref<IShellItem>,
        _psznewname: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostCopyItem(
        &self,
        _dwflags: u32,
        _psiitem: Ref<IShellItem>,
        _psidestinationfolder: Ref<IShellItem>,
        _psznewname: &PCWSTR,
        _hrcopy: HRESULT,
        _psinewlycreated: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreDeleteItem(&self, _dwflags: u32, psiitem: Ref<IShellItem>) -> windows::core::Result<()> {
        let item = psiitem
            .as_ref()
            .and_then(|i| display(i, SIGDN_DESKTOPABSOLUTEPARSING))
            .unwrap_or_default();
        if self.core.pre_delete(item) {
            Ok(())
        } else {
            Err(E_ABORT.into())
        }
    }
    fn PostDeleteItem(
        &self,
        _dwflags: u32,
        psiitem: Ref<IShellItem>,
        hrdelete: HRESULT,
        psinewlycreated: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        let item = psiitem
            .as_ref()
            .and_then(|i| display(i, SIGDN_DESKTOPABSOLUTEPARSING))
            .unwrap_or_default();
        let state = bin_state(
            psinewlycreated
                .as_ref()
                .map(|n| display(n, SIGDN_FILESYSPATH).map(PathBuf::from)),
        );
        if self.core.post_delete(item, hrdelete.0, state) {
            Err(E_ABORT.into())
        } else {
            Ok(())
        }
    }
    fn PreNewItem(
        &self,
        _dwflags: u32,
        _psidestinationfolder: Ref<IShellItem>,
        _psznewname: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostNewItem(
        &self,
        _dwflags: u32,
        _psidestinationfolder: Ref<IShellItem>,
        _psznewname: &PCWSTR,
        _psztemplatename: &PCWSTR,
        _dwfileattributes: u32,
        _hrnew: HRESULT,
        _psinewitem: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn UpdateProgress(&self, _iworktotal: u32, _iworksofar: u32) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Una operación `IFileOperation` con su receptor ya conectado.
struct Operation {
    op: IFileOperation,
    cookie: u32,
    events: Events,
    state: Arc<SinkState>,
}

impl Operation {
    fn new(flags: FILEOPERATION_FLAGS, guard: Guard) -> Result<Operation> {
        let events: Events = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(SinkState::default());
        let sink: IFileOperationProgressSink = Recorder {
            core: SinkCore {
                events: events.clone(),
                state: state.clone(),
                guard,
            },
        }
        .into();
        // SAFETY: COM is initialized on this thread (the caller guarantees this); `sink` is a live interface that `Advise`
        // retiene (AddRef) hasta `Unadvise`.
        unsafe {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            op.SetOperationFlags(flags)?;
            let cookie = op.Advise(&sink)?;
            Ok(Operation {
                op,
                cookie,
                events,
                state,
            })
        }
    }

    /// Ejecuta lo encolado y devuelve lo que la shell notificó.
    fn perform(self) -> BatchOutcome {
        // SAFETY: COM is initialized on this thread; `self.op` is a live interface and `cookie` is from its `Advise` call.
        let (error, aborted) = unsafe {
            let error = self.op.PerformOperations().err().map(ShellError::from);
            let aborted = self
                .op
                .GetAnyOperationsAborted()
                .map(|b| b.as_bool())
                .unwrap_or(false);
            let _ = self.op.Unadvise(self.cookie);
            (error, aborted)
        };
        let events = std::mem::take(&mut *self.events.lock().unwrap_or_else(|e| e.into_inner()));
        let vetoed =
            std::mem::take(&mut *self.state.vetoes.lock().unwrap_or_else(|e| e.into_inner()));
        BatchOutcome {
            events,
            any_aborted: aborted,
            error,
            meta_removed: 0,
            sink_aborted: self.state.tripped.load(Ordering::SeqCst),
            vetoed,
            changed_bin: Vec::new(),
            linked_bin: Vec::new(),
            unverifiable_bin: Vec::new(),
            replaced_bin: Vec::new(),
        }
    }
}

/// Qué banderas se usan al enviar a la Papelera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecycleMode {
    /// `FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE`: lo que usa la aplicación.
    Normal,
    /// Lo mismo más `FOF_WANTNUKEWARNING`: solo existe en las pruebas (§3.7.4). Con `FOF_NO_UI` la shell se queda
    /// esperando una respuesta que nadie puede dar: la aplicación NUNCA la usa.
    #[cfg(test)]
    WithNukeWarning,
}

/// Las banderas con las que se envía a la Papelera (S1: `FOF_NO_CONNECTED_ELEMENTS` evita que aprobar `pagina.html`
/// arrastre `pagina_files\`, que nadie aprobó). `FOF_WANTNUKEWARNING` solo existe en las pruebas.
pub(crate) fn send_flags(mode: RecycleMode) -> FILEOPERATION_FLAGS {
    let base = FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_NO_CONNECTED_ELEMENTS;
    match mode {
        RecycleMode::Normal => base,
        #[cfg(test)]
        RecycleMode::WithNukeWarning => base | FOF_WANTNUKEWARNING,
    }
}

/// Las banderas con las que se purgan elementos de la Papelera (también sin elementos conectados).
pub(crate) fn purge_flags() -> FILEOPERATION_FLAGS {
    FOF_NO_UI | FOF_NO_CONNECTED_ELEMENTS
}

/// Envía las rutas (una operación `IFileOperation` con todas) a la Papelera POR SU RUTA ORIGINAL. La shell abre cada
/// elemento por su cuenta: entre que quien llama cierra su handle y ese momento hay una ventana, acotada por el destino
/// (un sustituto puesto por el mismo usuario puede llegar a la shell, que lo mueve a la Papelera si cabe, o lo elimina). Por
/// eso el resultado trae, por elemento, la ruta real del elemento en la Papelera: quien llama compara su IDENTIDAD con la
/// aprobada.
pub(crate) fn recycle_paths(items: &[SendItem], mode: RecycleMode) -> Result<BatchOutcome> {
    if items.is_empty() {
        return Ok(BatchOutcome::default());
    }
    let _com = Com::init()?;
    let op = Operation::new(send_flags(mode), Guard::new(items))?;
    let mut added = 0usize;
    for it in items {
        let name = parsing_name(&it.path);
        // Una ruta que la shell no entiende (demasiado larga...) no aborta el lote: ese elemento no genera evento y
        // quien llama lo verá (sigue existiendo).
        // SAFETY: `name` is NUL-terminated and remains alive for the call.
        let Ok(item) = (unsafe {
            SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(name.as_ptr()), None)
        }) else {
            continue;
        };
        // SAFETY: `op.op` and `item` are live interfaces.
        if unsafe { op.op.DeleteItem(&item, None) }.is_ok() {
            added += 1;
        }
    }
    if added == 0 {
        return Ok(BatchOutcome::default());
    }
    Ok(op.perform())
}

/// Un paso de la purga, para el registro local (M4): lo que se va a hacer, lo que se saltó y lo que pasó, ELEMENTO A
/// ELEMENTO.
pub enum PurgeStep<'a> {
    /// Antes de cada trozo: los elementos que se van a entregar a la shell (la INTENCIÓN). Si quien observa no puede
    /// dejarla escrita, cancela (`cancel`) y el trozo NO se entrega.
    Begin(&'a [&'a BinItem]),
    /// Elementos que se saltaron porque su `$R` o su `$I` ya no eran los de la enumeración.
    Skipped(&'a [&'a BinItem]),
    /// Elementos que se saltaron porque su sistema de archivos no da identidad y no se pueden verificar.
    Unverifiable(&'a [&'a BinItem]),
    /// Elementos cuya purga la shell notificó con éxito pero cuyo objeto aprobado NO está eliminado (A-M4): entre la comprobación
    /// del receptor y el borrado de la shell se sustituyó el `$R` y se borró lo que había con ese nombre.
    Replaced(&'a [&'a BinItem]),
    /// Elementos a los que `PreDeleteItem` puso veto (A-M4): entre la enumeración y el instante en que la shell iba a
    /// borrarlos, su `$R` o su `$I` dejaron de ser los capturados. No se purgaron.
    Vetoed(&'a [&'a BinItem]),
    /// Elementos que NO se purgaron porque su `$R` lleva reparse points dentro o no se pudo comprobar entero (M2): se saltan
    /// ANTES de entregar el trozo y el resto del lote sigue.
    Linked(&'a [&'a BinItem]),
    /// Después de cada trozo: cada elemento con el `HRESULT` que notificó la shell (`None` = sin aviso).
    Done(&'a [(&'a BinItem, Option<i32>)]),
}

/// Tests only: permanently purges ONLY items in the list THROUGH the Recycle Bin shell folder. It enumerates the bin and
/// calls `IFileOperation::DeleteItem` on the enumerated `IShellItem`s, so the shell removes both `$R` (data) and `$I`
/// (metadata) together. It never deletes via the `$R` path, which would leave `$I` orphaned. The caller provides the list
/// enumerated at confirmation, and items are matched by their stable `id`; later arrivals are untouched. Before acting,
/// it verifies that each `$R` and `$I` still have the identities captured during enumeration (M3). Cancellation is checked
/// between chunks of 500.
#[cfg(test)]
pub(crate) fn purge_items(items: &[BinItem], cancel: &AtomicBool) -> Result<BatchOutcome> {
    purge_items_flags(items, cancel, purge_flags(), &mut |_| {})
}

/// Como [`purge_items`], pero avisa de cada paso (ver [`PurgeStep`]) para dejarlo en el registro.
pub(crate) fn purge_items_observed(
    items: &[BinItem],
    cancel: &AtomicBool,
    obs: &mut dyn FnMut(PurgeStep<'_>),
) -> Result<BatchOutcome> {
    purge_items_flags(items, cancel, purge_flags(), obs)
}

fn purge_items_flags(
    items: &[BinItem],
    cancel: &AtomicBool,
    flags: FILEOPERATION_FLAGS,
    obs: &mut dyn FnMut(PurgeStep<'_>),
) -> Result<BatchOutcome> {
    if items.is_empty() {
        return Ok(BatchOutcome::default());
    }
    let _com = Com::init()?;
    let by_id: std::collections::HashMap<&OsString, &BinItem> =
        items.iter().map(|i| (&i.id, i)).collect();
    let mut changed: Vec<&BinItem> = Vec::new();
    let mut unverifiable: Vec<&BinItem> = Vec::new();
    let mut found: Vec<(IShellItem, &BinItem)> = Vec::with_capacity(items.len());
    // GM-m5: los `id` ya casados con la lista. Si la enumeración de la shell repitiera un elemento, se entregaría dos veces a la
    // operación y el recuento de los que faltan por casar (`found + changed + unverifiable == by_id.len()`) se adelantaría.
    let mut matched: std::collections::HashSet<OsString> = std::collections::HashSet::new();
    // SAFETY: COM is initialized on this thread; all interfaces and strings belong to this function and are
    // released by `windows` RAII or `take_pwstr`.
    unsafe {
        let bin: IShellItem =
            SHGetKnownFolderItem(&FOLDERID_RecycleBinFolder, KF_FLAG_DEFAULT, None)?;
        let en: IEnumShellItems = bin.BindToHandler(None, &BHID_EnumItems)?;
        loop {
            let mut fetched = 0u32;
            let mut arr = [None];
            en.Next(&mut arr, Some(&mut fetched))?;
            if fetched == 0 {
                break;
            }
            let Some(item) = arr[0].take() else { break };
            if let Some(id) = display(&item, SIGDN_DESKTOPABSOLUTEPARSING)
                && let Some(approved) = by_id.get(&id).copied()
                && first_match(&mut matched, &id)
            {
                // M3: el elemento se identifica por `id` (un nombre), pero antes de tocarlo se comprueba que el `$R` y el
                // `$I` son los mismos objetos que al enumerar.
                match check_bin_item(approved) {
                    BinCheck::Same => found.push((item, approved)),
                    BinCheck::Changed => changed.push(approved),
                    BinCheck::Unverifiable => unverifiable.push(approved),
                }
                if found.len() + changed.len() + unverifiable.len() == by_id.len() {
                    break;
                }
            }
        }
    }
    let mut out = BatchOutcome {
        changed_bin: changed.iter().map(|i| i.id.clone()).collect(),
        unverifiable_bin: unverifiable.iter().map(|i| i.id.clone()).collect(),
        ..BatchOutcome::default()
    };
    if !changed.is_empty() {
        obs(PurgeStep::Skipped(&changed));
    }
    if !unverifiable.is_empty() {
        obs(PurgeStep::Unverifiable(&unverifiable));
    }
    let chunk_len = purge_chunk();
    for (idx, chunk) in found.chunks(chunk_len).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // A-M4: un handle de GUARDA por `$R` del trozo (`DELETE | READ_ATTRIBUTES`, compartiéndolo todo, como al enviar),
        // abierto JUSTO ANTES de la operación y verificado por su identidad. Medido con la shell real: la purga funciona igual
        // con ellos abiertos (el `$R` queda "pendiente de borrar" hasta soltarlos). Mientras viven, nadie puede renombrar los
        // ancestros del `$R` (la carpeta del usuario dentro de `$Recycle.Bin`), y al terminar demuestran si lo que la shell
        // borró fue de verdad el objeto aprobado (`delete_pending`).
        let mut staged: Vec<(&IShellItem, &BinItem, sys::Handle)> = Vec::with_capacity(chunk.len());
        let mut lost: Vec<&BinItem> = Vec::new();
        for (it, bi) in chunk {
            match open_bin_guard(bi) {
                Some(h) => staged.push((it, *bi, h)),
                None => lost.push(*bi),
            }
        }
        if !lost.is_empty() {
            // Cambiaron entre la enumeración y ahora: no se tocan (constan como "cambiados").
            out.changed_bin.extend(lost.iter().map(|i| i.id.clone()));
            obs(PurgeStep::Skipped(&lost));
        }
        // M2: un `$R` con reparse points dentro (junction, symlink, enlace de WSL, archivo de la nube) se EXCLUYE aquí, antes
        // de entregar nada: el receptor vetaría el hijo-enlace en `PreDeleteItem` (GM-m5) y un veto corta el lote entero, de
        // modo que "vaciar" se detendría en ese elemento cada vez. Excluido así, solo él se queda y el resto se purga. El
        // veto del receptor sigue como última defensa por si el enlace aparece después de esta comprobación.
        let mut linked: Vec<&BinItem> = Vec::new();
        staged.retain(|(_, bi, _)| {
            if bin_item_has_links(bi) {
                linked.push(*bi);
                false
            } else {
                true
            }
        });
        if !linked.is_empty() {
            out.linked_bin.extend(linked.iter().map(|i| i.id.clone()));
            obs(PurgeStep::Linked(&linked));
        }
        if staged.is_empty() {
            continue;
        }
        // Record INTENT before passing anything to the shell: if the observer could not write it, cancel and do not send
        // this chunk.
        let chunk_items: Vec<&BinItem> = staged.iter().map(|(_, i, _)| *i).collect();
        obs(PurgeStep::Begin(&chunk_items));
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // Tests only: the window between the guards and the shell (a hook can replace a `$R` right here).
        #[cfg(test)]
        for (_, it, _) in &staged {
            crate::engine::test_hooks::before_shell(Path::new(&it.id));
        }
        // A-M4: el receptor lleva una guarda con las identidades del `$R` y del `$I` de cada elemento: `PreDeleteItem` las
        // vuelve a comprobar por ruta (sin seguir enlaces) justo antes de que la shell borre cada uno, y veta lo que ya no
        // es lo capturado (`E_ABORT`: lo que quede del lote no se procesa).
        let op = Operation::new(flags, Guard::purge(&chunk_items))?;
        let mut added = 0usize;
        for (it, _, _) in &staged {
            // SAFETY: `op.op` and `it` are live interfaces.
            if unsafe { op.op.DeleteItem(*it, None) }.is_ok() {
                added += 1;
            }
        }
        if added == 0 {
            continue;
        }
        let r = op.perform();
        out.any_aborted |= r.any_aborted;
        // La shell dijo "borrado" con éxito: ¿era el objeto aprobado? Con las guardas aún abiertas, su `delete_pending` lo
        // demuestra. Si no, la shell borró OTRA cosa con ese nombre (sustituido tras la comprobación del receptor): ya no se
        // puede deshacer, pero se sabe y se cuenta aparte.
        let guards: Vec<(&OsString, &sys::Handle)> =
            staged.iter().map(|(_, i, h)| (&i.id, h)).collect();
        let replaced_ids = replaced_after(&r.events, &guards);
        out.replaced_bin.extend(replaced_ids.iter().cloned());
        // Las guardas se sueltan YA: hasta entonces el `$R` borrado sigue visible como "pendiente de borrar" y no se podría
        // comprobar que ha desaparecido (ver `remove_orphan_meta`).
        drop(staged);
        // Medido en Windows 11 (compilación 26300): la shell borra el `$R` pero puede dejar el `$I` (los metadatos del
        // elemento) huérfano, sea cual sea la bandera. Se quita ese `$I` exacto, con las comprobaciones de abajo.
        for e in &r.events {
            if e.succeeded()
                && !replaced_ids.contains(&e.item)
                && let Some(it) = by_id.get(&e.item)
                && remove_orphan_meta(it)
            {
                out.meta_removed += 1;
            }
        }
        let vetoed_items = vetoed_items(&chunk_items, &r.vetoed);
        if !vetoed_items.is_empty() {
            obs(PurgeStep::Vetoed(&vetoed_items));
        }
        let done: Vec<(&BinItem, Option<i32>)> = chunk_items
            .iter()
            .filter(|i| !vetoed_items.iter().any(|v| v.id == i.id) && !replaced_ids.contains(&i.id))
            .map(|i| (*i, r.events.iter().find(|e| e.item == i.id).map(|e| e.hr)))
            .collect();
        obs(PurgeStep::Done(&done));
        let replaced_items: Vec<&BinItem> = chunk_items
            .iter()
            .copied()
            .filter(|i| replaced_ids.contains(&i.id))
            .collect();
        if !replaced_items.is_empty() {
            obs(PurgeStep::Replaced(&replaced_items));
        }
        out.events.extend(r.events);
        out.vetoed.extend(r.vetoed);
        if let Some(e) = r.error {
            out.error = Some(e);
        }
        // Un veto significa que algo tocó la Papelera mientras se purgaba: se corta aquí. Los trozos que faltan no se
        // entregan a la shell (constan como "no procesados" y siguen en la Papelera).
        if r.sink_aborted {
            out.sink_aborted = true;
            let rest: Vec<(&BinItem, Option<i32>)> = found
                [((idx + 1) * chunk_len).min(found.len())..]
                .iter()
                .map(|(_, i)| (*i, None))
                .collect();
            if !rest.is_empty() {
                obs(PurgeStep::Done(&rest));
            }
            break;
        }
    }
    Ok(out)
}

/// `true` la PRIMERA vez que se ve un `id` (GM-m5): los repetidos de la enumeración de la shell no se vuelven a casar.
fn first_match(seen: &mut std::collections::HashSet<OsString>, id: &OsString) -> bool {
    seen.insert(id.clone())
}

/// Profundidad máxima del recorrido de [`bin_item_has_links`]: más allá no se comprueba y se trata como "con enlaces".
const MAX_LINK_SCAN_DEPTH: usize = 512;

/// M2: ¿lleva el `$R` de un elemento de la Papelera algún reparse point dentro (junction, symlink, enlace de WSL, archivo de la
/// nube) o no se pudo comprobar entero? Recorre el `$R` por handles relativos (sin entrar en ningún reparse point). FALLA
/// CERRADO: cualquier error al abrir o listar, una identidad que no es la capturada o una profundidad excesiva cuentan como
/// "con enlaces" y el elemento no se purga. Un `$R` archivo está limpio salvo que él mismo sea un reparse point.
fn bin_item_has_links(item: &BinItem) -> bool {
    fn walk(dir: &sys::Handle, volume: u64, depth: usize) -> bool {
        if depth > MAX_LINK_SCAN_DEPTH {
            return true;
        }
        let Ok(entries) = sys::list_dir(dir) else {
            return true;
        };
        for e in entries {
            if e.is_reparse() {
                return true;
            }
            if e.is_dir() {
                let Ok(child) = sys::open_child_dir_for_listing(dir, &e.name) else {
                    return true;
                };
                let same = sys::meta(&child).is_ok_and(|m| {
                    m.identity.file_id == e.file_id
                        && m.identity.volume_serial == volume
                        && !m.is_reparse()
                });
                if !same || walk(&child, volume, depth + 1) {
                    return true;
                }
            }
        }
        false
    }
    let data = Path::new(&item.id);
    let Ok(h) = sys::open_dir_for_listing(data) else {
        return true;
    };
    let Ok(m) = sys::meta(&h) else {
        return true;
    };
    if m.is_reparse() || item.data_id.is_none_or(|want| want != m.identity) {
        return true;
    }
    m.is_dir && walk(&h, m.identity.volume_serial, 0)
}

/// Tests only: separates items with no links inside (purged) from items that have links or could not be checked (M2).
#[cfg(test)]
pub(crate) fn partition_linked<'a>(items: &[&'a BinItem]) -> (Vec<&'a BinItem>, Vec<&'a BinItem>) {
    items.iter().copied().partition(|i| !bin_item_has_links(i))
}

/// Elementos por trozo al purgar (y por vuelta de la cancelación).
const PURGE_CHUNK: usize = 500;

/// Tests only: alternate chunk size (0 = normal), to exercise multi-chunk behavior with few items.
#[cfg(test)]
static TEST_PURGE_CHUNK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn purge_chunk() -> usize {
    #[cfg(test)]
    {
        let n = TEST_PURGE_CHUNK.load(Ordering::Relaxed);
        if n > 0 {
            return n;
        }
    }
    PURGE_CHUNK
}

/// Abre el handle de guarda del `$R` de un elemento de la lista (ver `purge_items_flags`) y verifica que es el objeto
/// capturado al enumerar y que no está ya pendiente de borrar. `None` si no se puede (cambió, no se abre...): no se toca.
fn open_bin_guard(item: &BinItem) -> Option<sys::Handle> {
    let want = item.data_id?;
    let h = sys::open_path_for_rename(Path::new(&item.id)).ok()?;
    let m = sys::meta(&h).ok()?;
    (m.identity == want && !m.delete_pending).then_some(h)
}

/// Los elementos del trozo a los que se atribuye algún veto (R-4): por el `id` del elemento al que pertenece lo vetado
/// (`Veto::id`), no por la ruta vetada, que al purgar una carpeta de la Papelera es la de un HIJO.
pub(crate) fn vetoed_items<'a>(chunk: &[&'a BinItem], vetoes: &[Veto]) -> Vec<&'a BinItem> {
    chunk
        .iter()
        .copied()
        .filter(|i| vetoes.iter().any(|v| *v.id() == i.id))
        .collect()
}

/// Elementos con un evento de éxito cuyo objeto aprobado (el de su guarda) NO está pendiente de borrar: la shell borró otra
/// cosa con ese nombre. `guards` son los `id` de los elementos del trozo con su handle de guarda, aún abierto.
fn replaced_after(events: &[DeleteEvent], guards: &[(&OsString, &sys::Handle)]) -> Vec<OsString> {
    events
        .iter()
        .filter(|e| e.succeeded())
        .filter(|e| {
            guards
                .iter()
                .find(|(id, _)| **id == e.item)
                .is_some_and(|(_, h)| !guard_proves_deleted(h))
        })
        .map(|e| e.item.clone())
        .collect()
}

/// ¿Demuestra la guarda que el objeto aprobado se eliminó? Solo si está pendiente de borrar (el nombre sigue visible
/// mientras haya handles abiertos). Un error al leer no demuestra nada.
fn guard_proves_deleted(h: &sys::Handle) -> bool {
    sys::meta(h).is_ok_and(|m| m.delete_pending)
}

/// Lo que guarda un `$I` (metadatos de un elemento de la Papelera): versión, tamaño, cuándo se borró y ruta original.
#[derive(Debug, PartialEq, Eq)]
struct MetaInfo {
    version: u64,
    size: u64,
    /// Momento del borrado (FILETIME), tal como lo guarda la shell en el desplazamiento 16.
    deleted_ft: i64,
    original: PathBuf,
}

/// Interpreta un `$I` (versión 1: ruta fija de 260 unidades; versión 2: longitud y ruta). `None` si no lo es.
fn parse_meta(b: &[u8]) -> Option<MetaInfo> {
    let u64_at = |at: usize| Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?));
    let version = u64_at(0)?;
    let size = u64_at(8)?;
    let deleted_ft = u64_at(16)? as i64;
    let (start, max_units) = match version {
        1 => (24usize, 260usize),
        2 => {
            let n = u32::from_le_bytes(b.get(24..28)?.try_into().ok()?) as usize;
            (28usize, n.min(32_768))
        }
        _ => return None,
    };
    let raw = b.get(start..)?;
    // Un `$I` cortado (menos unidades que las que dice su cabecera) no es de fiar.
    if raw.len() < max_units.min(if version == 2 { max_units } else { 1 }) * 2 {
        return None;
    }
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .take(max_units)
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    if units.is_empty() {
        return None;
    }
    Some(MetaInfo {
        version,
        size,
        deleted_ft,
        original: PathBuf::from(OsString::from_wide(&units)),
    })
}

/// ¿Es `info` (el `$I` leído) el de `item`, la lista aprobada? Compara TODO lo que el `$I` y la shell comparten: la carpeta
/// de la que se borró, el nombre (la shell puede ocultar la extensión), el tamaño (cuando la shell lo da: 0 en las carpetas)
/// y el instante del borrado. `data` es la ruta del `$R` (de él sale la extensión).
fn meta_matches(info: &MetaInfo, item: &BinItem, data: &Path) -> bool {
    use crate::paths::PathKey;
    let same_folder = info
        .original
        .parent()
        .is_some_and(|p| PathKey::new(p) == PathKey::new(&item.original_parent));
    let ext = |p: &Path| {
        p.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    let lower = |s: &std::ffi::OsStr| s.to_string_lossy().to_lowercase();
    let want = lower(&item.name);
    let same_name = info.original.file_name().is_some_and(|n| {
        lower(n) == want
            || (info
                .original
                .file_stem()
                .is_some_and(|stem| lower(stem) == want)
                && ext(&info.original) == ext(data))
    });
    let same_size = item.size == 0 || info.size == item.size;
    let same_time = filetime_to_unix(FILETIME {
        dwLowDateTime: info.deleted_ft as u64 as u32,
        dwHighDateTime: ((info.deleted_ft as u64) >> 32) as u32,
    }) == item.time_deleted;
    same_folder && same_name && same_size && same_time && ext(&info.original) == ext(data)
}

/// ¿Qué hay en la ruta del `$R`? Distingue "no existe" de "no se pudo saber" (un error de acceso NO es "no existe").
#[derive(Debug, PartialEq, Eq)]
enum DataState {
    Gone,
    Exists,
    Unknown,
}

fn classify_data_error(code: sys::Win32) -> DataState {
    // ERROR_FILE_NOT_FOUND y ERROR_PATH_NOT_FOUND: no existe. Cualquier otro (acceso denegado, uso compartido...): no se
    // sabe.
    if matches!(code, 2 | 3) {
        DataState::Gone
    } else {
        DataState::Unknown
    }
}

fn data_state(data: &Path) -> DataState {
    match sys::open_for_query(data) {
        Ok(_) => DataState::Exists,
        Err(c) => classify_data_error(c),
    }
}

/// Quita el `$I` que la shell dejó huérfano tras purgar el elemento `item` (cuyo `$R` ya no existe). Es lo único que este
/// módulo hace por ruta dentro de `$Recycle.Bin`, y solo con TODAS estas condiciones:
/// * el `$R` (el `id` del elemento) se llama `$R<token>[.ext]` y está en `<unidad>:\$Recycle.Bin\<SID del usuario
///   actual>\`, y de verdad NO existe (un error de acceso no cuenta como "no existe": ver [`remove_orphan_meta_at`]);
/// * existe el hermano EXACTO `$I<token>[.ext]`, con la misma identidad que al enumerar.
///
/// El resto de comprobaciones (contenido y borrado) están en [`remove_orphan_meta_at`], que no depende de dónde esté la
/// carpeta y por eso se prueba con archivos sintéticos.
fn remove_orphan_meta(item: &BinItem) -> bool {
    use crate::paths::PathKey;
    let data = PathBuf::from(&item.id);
    let (Some(name), Some(sid_dir)) = (data.file_name().and_then(|n| n.to_str()), data.parent())
    else {
        return false;
    };
    if !name.starts_with("$R") || name.len() < 3 {
        return false;
    }
    let bin_dir = sid_dir.parent();
    let drive_root = bin_dir.and_then(Path::parent);
    let sid_name = sid_dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let bin_name = bin_dir
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let mine = tesela_scanner::current_user_sid().is_some_and(|s| s.eq_ignore_ascii_case(sid_name));
    let root_ok = drive_root.is_some_and(|r| PathKey::new(r).is_drive_root());
    if !(mine && root_ok && bin_name.eq_ignore_ascii_case("$Recycle.Bin")) {
        return false;
    }
    let Some(meta_path) = meta_sibling(&data) else {
        return false;
    };
    remove_orphan_meta_at(item, &data, &meta_path)
}

/// Las condiciones sobre el contenido (M3), con las rutas del `$R` y del `$I` dadas:
/// * el `$R` no existe de verdad: solo "no encontrado" cuenta; cualquier otro error (acceso, uso compartido...) es
///   "se desconoce" y el `$I` NO se borra;
/// * el `$I` es un archivo normal pequeño (sin reparse point) con la MISMA identidad que se capturó al enumerar
///   (`item.meta_id`), se abre en exclusiva y se lee, se interpreta y se borra por ese mismo handle;
/// * su contenido coincide con lo aprobado: carpeta de origen, nombre, tamaño (si la shell lo da) y momento del borrado.
fn remove_orphan_meta_at(item: &BinItem, data: &Path, meta_path: &Path) -> bool {
    if data_state(data) != DataState::Gone {
        return false;
    }
    let Some(want_id) = item.meta_id else {
        return false;
    };
    // Se abre en EXCLUSIVA primero y se lee, se interpreta y se borra por ese MISMO handle: lo que se lee es lo que se
    // borra.
    let Ok(h) = crate::sys::open_path_exclusive_read(meta_path) else {
        return false;
    };
    let Ok(m) = crate::sys::meta(&h) else {
        return false;
    };
    if m.is_dir || m.is_reparse() || m.logical > 70_000 || m.identity != want_id {
        return false;
    }
    let Ok(bytes) = crate::sys::read_small(&h, 70_000) else {
        return false;
    };
    let Some(info) = parse_meta(&bytes) else {
        return false;
    };
    if !meta_matches(&info, item, data) {
        return false;
    }
    crate::sys::delete_on_handle(&h).is_ok()
}

#[cfg(test)]
pub(crate) mod bintest {
    //! Helpers for tests that touch the user's REAL Recycle Bin: only with owned items (filtered by the test UUID) and an
    //! RAII guard that purges them no matter what. The Recycle Bin is NEVER emptied and unrelated items are never touched;
    //! tests compare the set of IDs before and after.
    use super::*;
    use std::collections::HashSet;

    /// Does this item belong to this test? `original_parent` is under `%TEMP%` (canonical long form), and its next
    /// component is `tesela-test-<uuid>` (with the full UUID); or it is `%TEMP%` itself and the name starts that way.
    pub fn is_mine(item: &BinItem, uuid: &str) -> bool {
        let prefix = format!("tesela-test-{uuid}").to_lowercase();
        let temp = dunce::canonicalize(std::env::temp_dir()).unwrap();
        let key = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
        let parent = key(&crate::sys::strip_verbatim(item.original_parent.clone()));
        let t = key(&temp);
        let t = t.trim_end_matches('\\');
        if parent == t {
            return item
                .name
                .to_string_lossy()
                .to_lowercase()
                .starts_with(&prefix);
        }
        match parent.strip_prefix(&format!("{t}\\")) {
            Some(rest) => rest.split('\\').next().unwrap_or("") == prefix,
            None => false,
        }
    }

    /// Environment variable that enables tests using the user's REAL Recycle Bin (`TESELA_REAL_RECYCLE_TESTS=1`).
    pub const REAL_RECYCLE_VAR: &str = "TESELA_REAL_RECYCLE_TESTS";

    /// Is `TESELA_REAL_RECYCLE_TESTS=1` set? (Does not print anything.)
    pub fn real_recycle_requested() -> bool {
        std::env::var(REAL_RECYCLE_VAR).is_ok_and(|v| v == "1")
    }

    /// Are tests using the real Recycle Bin enabled? Only when `TESELA_REAL_RECYCLE_TESTS=1`. GitHub CI
    /// (`windows-latest`, Windows Server without an interactive session) does not provide a reliable user Recycle Bin: the
    /// shell lists it but does not return items sent to it, so these tests failed there because of the environment, not the
    /// code. On a desktop machine, run the full suite with this variable (see the README). Without it, these tests are
    /// skipped with a message; their assertions have NOT been weakened.
    pub fn real_recycle_enabled() -> bool {
        if real_recycle_requested() {
            return true;
        }
        eprintln!(
            "Papelera real desactivada: defina {REAL_RECYCLE_VAR}=1 para ejecutar esta prueba"
        );
        false
    }

    /// Can the Recycle Bin be tested on this machine? Tests using the real bin must be enabled
    /// ([`real_recycle_enabled`]), the shell must be able to list it, and the `%TEMP%` volume must have it enabled. On a
    /// machine without a Recycle Bin, or with "do not move files to the Recycle Bin" enabled, tests that send items there
    /// are skipped instead of failing because of the environment.
    pub fn bin_usable() -> bool {
        if !real_recycle_enabled() {
            return false;
        }
        if list_bin().is_err() {
            eprintln!("Papelera no disponible en este equipo: la prueba se salta");
            return false;
        }
        let tmp = dunce::canonicalize(std::env::temp_dir()).unwrap();
        let limit = crate::volume::volume_root(&tmp).map(|r| crate::volume::bin_limit(&r));
        if matches!(
            limit,
            Some(crate::volume::BinLimit::NoBin | crate::volume::BinLimit::NukeOnDelete)
        ) {
            eprintln!("el volumen del temporal no tiene Papelera activa: la prueba se salta");
            return false;
        }
        true
    }

    /// Sets the purge chunk size while this guard lives (used only with `bin_lock()`: tests using the real Recycle Bin are
    /// serialized).
    pub struct PurgeChunkGuard;

    pub fn set_purge_chunk(n: usize) -> PurgeChunkGuard {
        TEST_PURGE_CHUNK.store(n, Ordering::Relaxed);
        PurgeChunkGuard
    }

    impl Drop for PurgeChunkGuard {
        fn drop(&mut self) {
            TEST_PURGE_CHUNK.store(0, Ordering::Relaxed);
        }
    }

    pub fn ids() -> HashSet<OsString> {
        list_bin()
            .expect("lista la Papelera")
            .into_iter()
            .map(|i| i.id)
            .collect()
    }

    /// Does `name` have the form `tesela-test-<uuid>` (a `Sandbox` name: a 36-character UUID v4, in hexadecimal 8-4-4-4-12)?
    /// With `exact`, nothing may follow; otherwise, a suffix is allowed (for a loose file with this prefix).
    pub fn has_test_shape(name: &str, exact: bool) -> bool {
        const PREFIX: &str = "tesela-test-";
        let lower = name.to_lowercase();
        let Some(rest) = lower.strip_prefix(PREFIX) else {
            return false;
        };
        let Some(uuid) = rest.get(..36) else {
            return false;
        };
        let shape_ok = uuid.char_indices().all(|(i, ch)| match i {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        });
        shape_ok && (!exact || rest.len() == 36)
    }

    /// Is this an item from ANY test run? Its source folder is a direct child of `temp` named exactly
    /// `tesela-test-<uuid>`, or is under such a folder; or the item is directly in `temp` and its name starts with
    /// `tesela-test-<uuid>`. The UUID must have the correct FORM (R-6): a user file named `tesela-test-something` does not count.
    pub fn is_test_item_in(item: &BinItem, temp: &Path) -> bool {
        let key = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
        let parent = key(&crate::sys::strip_verbatim(item.original_parent.clone()));
        let t = key(temp);
        let t = t.trim_end_matches('\\');
        match parent.strip_prefix(&format!("{t}\\")) {
            Some(rest) => has_test_shape(rest.split('\\').next().unwrap_or(""), true),
            None => parent == t && has_test_shape(&item.name.to_string_lossy(), false),
        }
    }

    /// [`is_test_item_in`] using this process's canonical `%TEMP%`.
    pub fn is_any_test_item(item: &BinItem) -> bool {
        let temp = dunce::canonicalize(std::env::temp_dir()).unwrap();
        is_test_item_in(item, &temp)
    }

    /// Should the residue sweep remove this item? Only an item from some test ([`is_test_item_in`]) deleted MORE than two
    /// minutes ago qualifies; items belonging to a test running concurrently in another process are excluded.
    pub fn sweep_candidate(item: &BinItem, now: i64, temp: &Path) -> bool {
        is_test_item_in(item, temp) && now - item.time_deleted > 120
    }

    /// Is `p` a normal object, opened without following links, and NOT a reparse point? Fail CLOSED: if it cannot be opened
    /// or read, it is not "normal."
    pub fn is_plain(p: &Path) -> bool {
        sys::open_for_query(p)
            .and_then(|h| sys::meta(&h))
            .is_ok_and(|m| !m.is_reparse())
    }

    /// Is `p` under `<drive>:\$Recycle.Bin\<sid>\`? (Each component is checked individually.)
    pub fn under_bin_of(p: &Path, sid: &str) -> bool {
        let sid_dir = p.parent();
        let bin_dir = sid_dir.and_then(Path::parent);
        let drive = bin_dir.and_then(Path::parent);
        !sid.is_empty()
            && sid_dir
                .and_then(Path::file_name)
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(sid))
            && bin_dir
                .and_then(Path::file_name)
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("$Recycle.Bin"))
            && drive.is_some_and(|d| PathKey::new(d).is_drive_root())
    }

    /// Deletes an owned item's `$R` and `$I` by PATH, without using the code under test, if it remains in the Recycle Bin.
    /// Only proceeds when the item belongs to a test ([`is_any_test_item`]; defense in depth, since the caller also checks),
    /// both paths are under `<drive>:\$Recycle.Bin\<current user SID>\`, and neither is a reparse point ([`is_plain`],
    /// which fails closed). Returns `true` if `$R` is gone.
    pub fn remove_by_path(item: &BinItem) -> bool {
        let data = PathBuf::from(&item.id);
        if !is_any_test_item(item) {
            return false;
        }
        let sid = tesela_scanner::current_user_sid().unwrap_or_default();
        let safe = |p: &Path| under_bin_of(p, &sid) && is_plain(p);
        if let Some(meta) = meta_sibling(&data)
            && safe(&meta)
        {
            let _ = std::fs::remove_file(&meta);
        }
        if safe(&data) {
            match std::fs::symlink_metadata(&data) {
                Ok(m) if m.is_dir() => {
                    let _ = std::fs::remove_dir_all(&data);
                }
                Ok(_) => {
                    let _ = std::fs::remove_file(&data);
                }
                Err(_) => {}
            }
        }
        !data.exists()
    }

    /// Purges remaining items from tests matching `which`: up to three attempts with `list_bin` + `purge_items`, then deletes
    /// any leftovers by path (see [`remove_by_path`]). Returns the number of remaining items, or an error if the Recycle Bin
    /// cannot be listed (S-3: you cannot claim "zero leaks" without inspecting it).
    fn clean_own(which: &dyn Fn(&BinItem) -> bool) -> Result<usize> {
        for _ in 0..3 {
            let mine: Vec<BinItem> = list_bin()?.into_iter().filter(|i| which(i)).collect();
            if mine.is_empty() {
                return Ok(0);
            }
            let _ = purge_items(&mine, &AtomicBool::new(false));
        }
        let left: Vec<BinItem> = list_bin()?.into_iter().filter(|i| which(i)).collect();
        for it in &left {
            remove_by_path(it);
        }
        Ok(list_bin()?.into_iter().filter(|i| which(i)).count())
    }

    /// Should cleanup leftovers trigger `panic!`? Returns `Some(message)` if items remain or the Recycle Bin could not be
    /// listed, unless the thread is already panicking (`panic!` in `drop` during a panic aborts the process).
    pub fn leak_verdict(left: Result<usize>, panicking: bool) -> Option<String> {
        let msg = match left {
            Ok(0) => return None,
            Ok(n) => format!("quedan {n} elementos propios en la Papelera"),
            Err(e) => {
                format!("no se pudo listar la Papelera para comprobar que no quedan restos: {e}")
            }
        };
        eprintln!("BinGuard: {msg}");
        (!panicking).then_some(msg)
    }

    /// Sweeps residue from PREVIOUS runs (source under `tesela-test-<uuid>`, deleted more than two minutes ago; never items
    /// from a test running concurrently in another process). Called only when real tests are enabled and with `bin_lock()`.
    pub fn sweep_leftovers() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let temp = dunce::canonicalize(std::env::temp_dir()).unwrap();
        if let Some(msg) = leak_verdict(clean_own(&|i| sweep_candidate(i, now, &temp)), false) {
            panic!("BARRIDO: {msg}");
        }
    }

    /// On drop (including during a panic), purges ONLY this test's owned items: up to three attempts using the code under
    /// test, then deletes leftovers by path WITHOUT that code (see [`remove_by_path`). Reports how many remain and calls
    /// `panic!` if the count is nonzero or the Recycle Bin could not be listed (unless already panicking), so leaks cannot
    /// go unnoticed.
    pub struct BinGuard {
        pub uuid: String,
    }

    impl Drop for BinGuard {
        fn drop(&mut self) {
            let left = clean_own(&|i| is_mine(i, &self.uuid));
            if let Some(msg) = leak_verdict(left, std::thread::panicking()) {
                panic!("BinGuard: {msg}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::bintest::*;
    use super::*;
    use crate::paths::PathKey;
    use crate::testutil::{RemoveOnDrop, Sandbox, bin_lock, sparse_file};
    use std::collections::HashSet;

    /// Items to send, captured with their current identities (the "approved" state).
    fn send_items(paths: &[PathBuf]) -> Vec<SendItem> {
        paths.iter().map(|p| SendItem::capture(p)).collect()
    }

    /// Identity (volume + FileId) of the object at `p`.
    fn identity_of(p: &Path) -> crate::sys::FileIdentity {
        let h = crate::sys::open_for_query(p).unwrap();
        crate::sys::meta(&h).unwrap().identity
    }

    /// The `$I` (metadata) paired with a Recycle Bin item's `$R` (data): `$R9XYZ.txt` -> `$I9XYZ.txt`.
    fn meta_file_of(item: &BinItem) -> PathBuf {
        let p = PathBuf::from(&item.id);
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("$R"), "{name}");
        p.with_file_name(format!("$I{}", &name[2..]))
    }

    #[test]
    fn enviar_listar_y_purgar_solo_elementos_propios() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        sb.write("p/uno.txt", 1000);
        sb.write("p/carpeta/x.bin", 2000);
        let paths = vec![sb.long("p/uno.txt"), sb.long("p/carpeta")];
        let ids_before_send: Vec<_> = paths.iter().map(|p| identity_of(p)).collect();
        let r = recycle_paths(&send_items(&paths), RecycleMode::Normal).unwrap();
        assert!(!r.any_aborted && r.error.is_none());
        assert!(paths.iter().all(|p| !p.exists()), "ya no están en su sitio");
        // The shell reports each result and the item's actual Recycle Bin path; that path has the SAME identity as the
        // sent item (renaming within a volume preserves the FileId).
        assert_eq!(r.events.len(), 2, "{:?}", r.events);
        for (p, want) in paths.iter().zip(&ids_before_send) {
            let ev = r
                .events
                .iter()
                .find(|e| PathKey::new(Path::new(&e.item)) == PathKey::new(p))
                .unwrap_or_else(|| panic!("{p:?}: {:?}", r.events));
            assert!(ev.succeeded(), "{ev:?}");
            let BinState::InBin(bin_path) = &ev.state else {
                panic!("debería estar en la Papelera: {ev:?}");
            };
            assert_eq!(&identity_of(bin_path), want);
        }
        let items = list_bin().unwrap();
        let mine: Vec<BinItem> = items
            .iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .cloned()
            .collect();
        assert_eq!(mine.len(), 2, "{mine:?}");
        let names: HashSet<String> = mine
            .iter()
            .map(|i| i.name.to_string_lossy().into_owned())
            .collect();
        assert!(
            names.contains("uno.txt") && names.contains("carpeta"),
            "{names:?}"
        );
        assert!(mine.iter().all(|i| i.original_parent == sb.long("p")));
        assert!(mine.iter().any(|i| i.size == 1000), "{mine:?}");
        // Unrelated items are untouched: IDs outside this test's set remain unchanged.
        let foreign: HashSet<OsString> = items
            .iter()
            .filter(|i| !is_mine(i, &sb.uuid()))
            .map(|i| i.id.clone())
            .collect();
        assert_eq!(foreign, before);
        // Each item has an `$I` (metadata) file in addition to its `$R` (data) file.
        let metas: Vec<PathBuf> = mine.iter().map(meta_file_of).collect();
        let datas: Vec<PathBuf> = mine.iter().map(|i| PathBuf::from(&i.id)).collect();
        assert!(metas.iter().all(|m| m.exists()), "{metas:?}");
        assert!(datas.iter().all(|d| d.exists()), "{datas:?}");
        // Purge ONLY the enumerated list through the Recycle Bin shell folder: both files for each item are removed
        // (no `$I` is left orphaned), and items are matched by `id`, so unrelated items are untouched.
        let r = purge_items(&mine, &AtomicBool::new(false)).unwrap();
        assert!(!r.any_aborted && r.error.is_none());
        // The shell also reports items INSIDE a directory: count the listed items, matched by `id`.
        for it in &mine {
            let ev = r.events.iter().find(|e| e.item == it.id);
            assert!(
                ev.is_some_and(DeleteEvent::succeeded),
                "{it:?}: {:?}",
                r.events
            );
        }
        assert!(
            metas.iter().all(|m| !m.exists()),
            "no quedan `$I` huérfanos: {metas:?}"
        );
        assert!(datas.iter().all(|d| !d.exists()));
        assert_eq!(ids(), before, "la Papelera queda exactamente como estaba");
    }

    /// A version 2 `$I` file with the given size, deletion time (FILETIME), and original path.
    fn meta_v2_with(original: &str, size: u64, deleted_ft: i64) -> Vec<u8> {
        let wide: Vec<u16> = original.encode_utf16().chain(Some(0)).collect();
        let mut b = Vec::new();
        b.extend_from_slice(&2u64.to_le_bytes());
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&(deleted_ft as u64).to_le_bytes());
        b.extend_from_slice(&(wide.len() as u32).to_le_bytes());
        for u in wide {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b
    }

    fn meta_v2(original: &str) -> Vec<u8> {
        meta_v2_with(original, 1234, 0)
    }

    /// FILETIME for a Unix timestamp.
    fn ft_of(unix: i64) -> i64 {
        (unix + 11_644_473_600) * 10_000_000
    }

    #[test]
    fn el_i_se_interpreta_en_sus_dos_versiones_y_rechaza_lo_que_no_lo_es() {
        let v2 = parse_meta(&meta_v2(r"C:\a\b\uno.txt")).unwrap();
        assert_eq!(
            v2,
            MetaInfo {
                version: 2,
                size: 1234,
                deleted_ft: 0,
                original: PathBuf::from(r"C:\a\b\uno.txt")
            }
        );
        // The deletion timestamp is at offset 16.
        let con_fecha =
            parse_meta(&meta_v2_with(r"C:\a\b\uno.txt", 7, ft_of(1_700_000_000))).unwrap();
        assert_eq!(con_fecha.deleted_ft, ft_of(1_700_000_000));
        let mut v1 = Vec::new();
        v1.extend_from_slice(&1u64.to_le_bytes());
        v1.extend_from_slice(&99u64.to_le_bytes());
        v1.extend_from_slice(&5u64.to_le_bytes());
        let mut path: Vec<u16> = r"C:\x\y.bin".encode_utf16().collect();
        path.resize(260, 0);
        for u in path {
            v1.extend_from_slice(&u.to_le_bytes());
        }
        let v1 = parse_meta(&v1).unwrap();
        assert_eq!((v1.version, v1.size, v1.deleted_ft), (1, 99, 5));
        assert_eq!(v1.original, PathBuf::from(r"C:\x\y.bin"));
        // Junk, unknown versions, truncated data, and empty paths.
        assert!(parse_meta(&[]).is_none());
        assert!(parse_meta(&[0u8; 10]).is_none());
        assert!(parse_meta(&[0u8; 400]).is_none());
        let mut bad = meta_v2(r"C:\a\b");
        bad[0] = 9;
        assert!(parse_meta(&bad).is_none());
        assert!(parse_meta(&meta_v2("")).is_none());
        assert!(parse_meta(&meta_v2(r"C:\a\b")[..30]).is_none());
    }

    const UNIX_T: i64 = 1_700_000_000;

    /// A SYNTHETIC Recycle Bin item in a test folder (not the real bin): `$R<token>.txt` no longer exists, and the sibling
    /// `$I<token>.txt` contains the given data. Returns the item as `list_bin` would enumerate it (the `$R` and `$I`
    /// identities), along with their paths.
    struct Orphan {
        item: BinItem,
        data: PathBuf,
        meta: PathBuf,
    }

    fn orphan(sb: &Sandbox, token: &str, content: &[u8]) -> Orphan {
        let dir = sb.mkdir("bin/S-synthetic");
        let data = dir.join(format!("$R{token}.txt"));
        let meta = dir.join(format!("$I{token}.txt"));
        std::fs::write(&data, b"datos del elemento").unwrap();
        std::fs::write(&meta, content).unwrap();
        let data_id = identity_at(&data);
        let meta_id = identity_at(&meta);
        // The shell has already purged `$R`.
        std::fs::remove_file(&data).unwrap();
        Orphan {
            item: BinItem {
                id: data.clone().into_os_string(),
                name: OsString::from("uno.txt"),
                original_parent: sb.long("p"),
                time_deleted: UNIX_T,
                size: 1234,
                data_id,
                meta_id,
                no_identity: false,
            },
            data,
            meta,
        }
    }

    fn good_meta(sb: &Sandbox) -> Vec<u8> {
        meta_v2_with(
            &format!("{}\\p\\uno.txt", sb.long_root().display()),
            1234,
            ft_of(UNIX_T),
        )
    }

    /// M3: an orphaned `$I` is removed only if EVERYTHING matches the approved state. With synthetic files (the function
    /// accepts their paths), every mismatch leaves `$I` intact.
    #[test]
    fn el_i_huerfano_solo_se_quita_si_todo_cuadra_con_lo_aprobado() {
        let sb = Sandbox::new();
        // Valid case: it is removed (only `$I`).
        let o = orphan(&sb, "GOOD01", &good_meta(&sb));
        assert!(remove_orphan_meta_at(&o.item, &o.data, &o.meta));
        assert!(!o.meta.exists());
        // The shell hides extensions (`HideFileExt`): the approved name is `uno`, which still matches.
        let mut o = orphan(&sb, "HIDEXT", &good_meta(&sb));
        o.item.name = OsString::from("uno");
        assert!(remove_orphan_meta_at(&o.item, &o.data, &o.meta));
        // The shell reports no directory size (0): do not compare it.
        let mut o = orphan(&sb, "DIR001", &good_meta(&sb));
        o.item.size = 0;
        assert!(remove_orphan_meta_at(&o.item, &o.data, &o.meta));

        // Each mismatch leaves `$I` in place.
        type Tamper = Box<dyn Fn(&mut BinItem, &Sandbox)>;
        let cases: Vec<(&str, Tamper)> = vec![
            (
                "nombre",
                Box::new(|i, _| i.name = OsString::from("otro.txt")),
            ),
            ("tamano", Box::new(|i, _| i.size = 999)),
            ("fecha", Box::new(|i, _| i.time_deleted += 1)),
            (
                "carpeta",
                Box::new(|i, sb| i.original_parent = sb.long("otra")),
            ),
            ("sin identidad del $I", Box::new(|i, _| i.meta_id = None)),
            (
                "identidad del $I distinta",
                Box::new(|i, _| {
                    i.meta_id = Some(FileIdentity {
                        volume_serial: 1,
                        file_id: 2,
                    })
                }),
            ),
        ];
        for (n, (why, f)) in cases.iter().enumerate() {
            let mut o = orphan(&sb, &format!("BAD{n:03}"), &good_meta(&sb));
            f(&mut o.item, &sb);
            assert!(!remove_orphan_meta_at(&o.item, &o.data, &o.meta), "{why}");
            assert!(o.meta.exists(), "{why}: el $I debe seguir");
        }
        // An extension different from the `$R` extension.
        let o = orphan(
            &sb,
            "EXT001",
            &meta_v2_with(
                &format!("{}\\p\\uno.doc", sb.long_root().display()),
                1234,
                ft_of(UNIX_T),
            ),
        );
        assert!(!remove_orphan_meta_at(&o.item, &o.data, &o.meta));
        assert!(o.meta.exists());
        // An `$I` file whose contents are not a valid `$I` record.
        let o = orphan(&sb, "JUNK01", b"esto no es un $I");
        assert!(!remove_orphan_meta_at(&o.item, &o.data, &o.meta));
        assert!(o.meta.exists());
    }

    /// Outside `<drive>:\$Recycle.Bin\<my SID>\`, `$I` is never touched, even when its name and contents (and even its
    /// identities and metadata) look like those of a Recycle Bin item.
    #[test]
    fn el_i_huerfano_fuera_de_la_papelera_del_usuario_nunca_se_toca() {
        let sb = Sandbox::new();
        let o = orphan(&sb, "LOC001", &good_meta(&sb));
        // The contents and identities match (so `remove_orphan_meta_at` would remove it), but the path is not in this
        // user's Recycle Bin.
        assert!(!remove_orphan_meta(&o.item));
        assert!(o.meta.exists());
        // An ID that is not a `$R`.
        let not_r = BinItem {
            id: o.data.with_file_name("otra.txt").into_os_string(),
            ..o.item.clone()
        };
        assert!(!remove_orphan_meta(&not_r));
        assert!(o.meta.exists());
    }

    /// M3: if `$R` still exists, or querying it returns an error other than "not found" (access denied, invalid name, etc.),
    /// `$I` is NOT deleted: an error means "unknown," not "does not exist."
    #[test]
    fn un_error_al_consultar_el_r_no_cuenta_como_que_no_existe() {
        for c in [2, 3] {
            assert_eq!(classify_data_error(c), DataState::Gone, "{c}");
        }
        for c in [1, 5, 32, 87, 123, 1920] {
            assert_eq!(classify_data_error(c), DataState::Unknown, "{c}");
        }
        let sb = Sandbox::new();
        // Exists -> Exists.
        let o = orphan(&sb, "STATE1", &good_meta(&sb));
        std::fs::write(&o.data, b"x").unwrap();
        assert_eq!(data_state(&o.data), DataState::Exists);
        assert!(!remove_orphan_meta_at(&o.item, &o.data, &o.meta));
        assert!(o.meta.exists());
        std::fs::remove_file(&o.data).unwrap();
        assert_eq!(data_state(&o.data), DataState::Gone);
        // A path that returns an error other than "not found" (invalid name): unknown, so `$I` remains.
        let invalid = o.data.with_file_name("$RSTATE1*.txt");
        assert_eq!(data_state(&invalid), DataState::Unknown);
        assert!(!remove_orphan_meta_at(&o.item, &invalid, &o.meta));
        assert!(o.meta.exists());
        // When `$R` is truly absent, it is removed.
        assert!(remove_orphan_meta_at(&o.item, &o.data, &o.meta));
    }

    /// M3: when purging, `$R` and `$I` must still be the same objects that were enumerated.
    #[test]
    fn un_elemento_de_la_papelera_cuyo_r_o_i_cambiaron_no_es_el_aprobado() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("bin/S-synthetic");
        let (data, meta) = (dir.join("$RTOKEN1.txt"), dir.join("$ITOKEN1.txt"));
        std::fs::write(&data, b"datos").unwrap();
        std::fs::write(&meta, b"meta").unwrap();
        let item = BinItem {
            id: data.clone().into_os_string(),
            name: OsString::from("uno.txt"),
            original_parent: sb.long("p"),
            time_deleted: UNIX_T,
            size: 5,
            data_id: identity_at(&data),
            meta_id: identity_at(&meta),
            no_identity: false,
        };
        assert!(bin_item_unchanged(&item));
        assert_eq!(check_bin_item(&item), BinCheck::Same);
        // H6.9: without a FileId (FAT, exFAT), this is not "changed": it has its own reason.
        assert_eq!(
            check_bin_item(&BinItem {
                no_identity: true,
                data_id: None,
                ..item.clone()
            }),
            BinCheck::Unverifiable
        );
        assert_eq!(
            check_bin_item(&BinItem {
                data_id: None,
                ..item.clone()
            }),
            BinCheck::Changed
        );
        assert!(is_no_identity_error(1) && is_no_identity_error(50) && is_no_identity_error(87));
        assert!(!is_no_identity_error(5) && !is_no_identity_error(2));
        // Without captured identities, verification is impossible.
        assert!(!bin_item_unchanged(&BinItem {
            data_id: None,
            ..item.clone()
        }));
        assert!(!bin_item_unchanged(&BinItem {
            meta_id: None,
            ..item.clone()
        }));
        // `$I` replaced by another file with the same name (different identity).
        std::fs::remove_file(&meta).unwrap();
        std::fs::write(&meta, b"meta").unwrap();
        assert!(!bin_item_unchanged(&item), "el $I ya no es el mismo objeto");
        // `$R` replaced.
        let item2 = BinItem {
            meta_id: identity_at(&meta),
            ..item.clone()
        };
        assert!(bin_item_unchanged(&item2));
        std::fs::remove_file(&data).unwrap();
        std::fs::write(&data, b"datos").unwrap();
        assert!(
            !bin_item_unchanged(&item2),
            "el $R ya no es el mismo objeto"
        );
        // It also fails if it disappears.
        std::fs::remove_file(&data).unwrap();
        assert!(!bin_item_unchanged(&item2));
    }

    /// M3 with the REAL shell: an OWNED item whose captured identity no longer matches is SKIPPED during purge (left
    /// untouched and recorded as changed); a matching identity is purged. The Recycle Bin is unchanged afterward.
    #[test]
    fn purgar_salta_un_elemento_propio_cuya_identidad_no_cuadra() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        sb.write("q/uno.txt", 600);
        recycle_paths(&send_items(&[sb.long("q/uno.txt")]), RecycleMode::Normal).unwrap();
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        assert_eq!(mine.len(), 1, "{mine:?}");
        let good = mine[0].clone();
        assert!(good.data_id.is_some() && good.meta_id.is_some(), "{good:?}");
        // `$R` identity spoofed: it is skipped.
        let tampered = BinItem {
            data_id: Some(FileIdentity {
                volume_serial: 1,
                file_id: 2,
            }),
            ..good.clone()
        };
        let r = purge_items(std::slice::from_ref(&tampered), &AtomicBool::new(false)).unwrap();
        assert_eq!(r.changed_bin, vec![tampered.id.clone()]);
        assert!(r.events.is_empty(), "la shell no vio nada: {:?}", r.events);
        assert!(PathBuf::from(&good.id).exists(), "el $R sigue ahí");
        // The same applies to `$I`.
        let tampered_meta = BinItem {
            meta_id: None,
            ..good.clone()
        };
        let r = purge_items(
            std::slice::from_ref(&tampered_meta),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(r.changed_bin.len(), 1);
        assert!(PathBuf::from(&good.id).exists());
        // With matching identities, the item is purged and no orphaned `$I` remains.
        let meta_path = meta_sibling(Path::new(&good.id)).unwrap();
        let r = purge_items(std::slice::from_ref(&good), &AtomicBool::new(false)).unwrap();
        assert!(r.changed_bin.is_empty());
        assert!(r.events.iter().any(|e| e.item == good.id && e.succeeded()));
        assert!(!PathBuf::from(&good.id).exists() && !meta_path.exists());
        assert_eq!(ids(), before);
    }

    #[test]
    fn purgar_respeta_la_cancelacion_y_no_toca_lo_que_no_esta_en_la_lista() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        for n in ["a.txt", "b.txt", "c.txt"] {
            sb.write(&format!("p/{n}"), 500);
        }
        let paths: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt"]
            .iter()
            .map(|n| sb.long(&format!("p/{n}")))
            .collect();
        recycle_paths(&send_items(&paths), RecycleMode::Normal).unwrap();
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        assert_eq!(mine.len(), 3);
        // Cancelled before starting: nothing is purged.
        let r = purge_items(&mine, &AtomicBool::new(true)).unwrap();
        assert!(r.events.is_empty());
        assert_eq!(
            list_bin()
                .unwrap()
                .iter()
                .filter(|i| is_mine(i, &sb.uuid()))
                .count(),
            3
        );
        // Only two of the three: the third remains.
        let r = purge_items(&mine[..2], &AtomicBool::new(false)).unwrap();
        assert_eq!(r.events.iter().filter(|e| e.succeeded()).count(), 2);
        let left: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        assert_eq!(left, vec![mine[2].clone()]);
        purge_items(&left, &AtomicBool::new(false)).unwrap();
        assert_eq!(ids(), before);
    }

    /// A synthetic Recycle Bin item (`$R` and `$I` in a sandbox folder) with identities captured now.
    fn synthetic_bin_item(sb: &Sandbox, token: &str, data: &[u8]) -> BinItem {
        sb.mkdir("bin/S-synthetic");
        // Long form (see `send_item`): a real item's `id` is the long path returned by the shell.
        let dir = sb.long("bin/S-synthetic");
        let (d, m) = (
            dir.join(format!("$R{token}.txt")),
            dir.join(format!("$I{token}.txt")),
        );
        std::fs::write(&d, data).unwrap();
        std::fs::write(&m, b"meta").unwrap();
        BinItem {
            id: d.clone().into_os_string(),
            name: OsString::from(format!("{token}.txt")),
            original_parent: sb.long("origen"),
            time_deleted: UNIX_T,
            size: data.len() as u64,
            data_id: identity_at(&d),
            meta_id: identity_at(&m),
            no_identity: false,
        }
    }

    /// A purge receiver guarded by these items (A-M4), without COM.
    fn purge_core(items: &[&BinItem]) -> SinkCore {
        SinkCore {
            events: Arc::new(Mutex::new(Vec::new())),
            state: Arc::new(SinkState::default()),
            guard: Guard::purge(items),
        }
    }

    /// A-M4 (without COM): the purge race window. Items are checked during enumeration, and the shell acts later. Previously,
    /// a purge receiver had no guard, so `PreDeleteItem` allowed anything, including an `$R` replaced after the check. Now
    /// `PreDeleteItem` rechecks the captured `$R` and `$I` by path: if they differ, cannot be opened, or are not approved,
    /// it vetoes with `E_ABORT`, records the veto, and rejects everything afterward. (A mutant that always returns `true`
    /// from `PreDeleteItem` fails here.)
    #[test]
    fn la_purga_veta_un_r_o_un_i_sustituido_despues_de_enumerar() {
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"datos a");
        let b = synthetic_bin_item(&sb, "BBBB02", b"datos b");
        let c = synthetic_bin_item(&sb, "CCCC03", b"datos c");
        let os = |i: &BinItem| i.id.clone();
        // Unchanged since enumeration: allowed.
        let core = purge_core(&[&a, &b, &c]);
        assert!(core.pre_delete(os(&a)));
        // Replace `b`'s `$R` with another file of the same name (a different FileId) AFTER the check.
        std::fs::remove_file(PathBuf::from(&b.id)).unwrap();
        std::fs::write(PathBuf::from(&b.id), b"datos b").unwrap();
        assert!(!core.pre_delete(os(&b)), "el sustituto se veta");
        let vetoes = core.state.vetoes.lock().unwrap().clone();
        assert_eq!(vetoes.len(), 1);
        assert_eq!(vetoes[0].kind, VetoKind::IdentityMismatch);
        assert_eq!(vetoes[0].item, b.id);
        assert!(core.state.tripped.load(Ordering::SeqCst));
        // The batch is cut off: the next item is rejected even though it is unchanged.
        assert!(!core.pre_delete(os(&c)));
        // The same with a replaced `$I`, in a new batch (a separate `tripped` state).
        let core = purge_core(&[&a, &c]);
        let meta_c = meta_sibling(Path::new(&c.id)).unwrap();
        std::fs::remove_file(&meta_c).unwrap();
        std::fs::write(&meta_c, b"meta").unwrap();
        assert!(core.pre_delete(os(&a)));
        assert!(!core.pre_delete(os(&c)), "un `$I` distinto también se veta");
        assert_eq!(
            core.state.vetoes.lock().unwrap()[0].kind,
            VetoKind::IdentityMismatch
        );
        // The `$R` disappears (cannot be opened), and an item absent from the list: both are vetoed with their reasons.
        let core = purge_core(&[&a]);
        std::fs::remove_file(PathBuf::from(&a.id)).unwrap();
        assert!(!core.pre_delete(os(&a)));
        assert!(matches!(
            core.state.vetoes.lock().unwrap()[0].kind,
            VetoKind::OpenFailed(2 | 3)
        ));
        let core = purge_core(&[&c]);
        let ajeno = sb.write("bin/S-synthetic/$RZZZZ99.txt", 10);
        assert!(!core.pre_delete(ajeno.into_os_string()));
        assert_eq!(
            core.state.vetoes.lock().unwrap()[0].kind,
            VetoKind::NotApproved
        );
    }

    /// A-M4: during PURGE, permanent deletion is expected, so a purge guard does not cut off the batch (unlike a send guard)
    /// and records the event.
    #[test]
    fn una_guarda_de_purga_no_corta_por_el_borrado_definitivo_esperado() {
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"x");
        let core = purge_core(&[&a]);
        assert!(!core.guard.sends());
        assert!(core.pre_delete(a.id.clone()));
        assert!(!core.post_delete(a.id.clone(), 0, BinState::Deleted));
        assert!(!core.state.tripped.load(Ordering::SeqCst));
        assert_eq!(core.events.lock().unwrap().len(), 1);
        // A send guard still cuts off the batch, as before.
        let it = send_item(&sb, "s/uno.txt", 10);
        let envio = core_for(std::slice::from_ref(&it));
        assert!(envio.guard.sends());
        assert!(envio.post_delete(it.path.as_os_str().to_os_string(), 0, BinState::Deleted));
    }

    /// A-M4 with the REAL shell (owned items only, after `TESELA_REAL_RECYCLE_TESTS=1`): after enumeration and verification,
    /// replace `b.txt`'s `$R` during the race window using a test hook. The receiver vetoes it: the replacement is NOT purged,
    /// and the veto is recorded. Measure (and print) what the shell does with the rest of the batch.
    #[test]
    fn con_la_shell_real_un_r_sustituido_antes_de_purgar_se_veta_y_se_mide_el_resto_del_lote() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        for n in ["a.txt", "b.txt", "c.txt"] {
            sb.write(&format!("v/{n}"), 700);
        }
        let paths: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt"]
            .iter()
            .map(|n| sb.long(&format!("v/{n}")))
            .collect();
        recycle_paths(&send_items(&paths), RecycleMode::Normal).unwrap();
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        assert_eq!(mine.len(), 3, "{mine:?}");
        // The FIRST in enumeration order (the order the shell processes them), so we can see what happens to items after the veto.
        let victim = mine[0].clone();
        let victim_r = PathBuf::from(&victim.id);
        let hook = crate::engine::test_hooks::install_before_shell(victim_r.clone(), move |p| {
            // With the guard open (sharing `DELETE`), the `$R` can be moved aside and replaced by another item of the same name.
            std::fs::rename(p, p.with_extension("apartado")).unwrap();
            std::fs::write(p, b"sustituto").unwrap();
        });
        // Remove the moved-aside `$R` (test-owned data) regardless of the outcome.
        let _apartado = RemoveOnDrop(victim_r.with_extension("apartado"));
        let r = purge_items(&mine, &AtomicBool::new(false)).unwrap();
        eprintln!(
            "purga con veto: eventos = {:?}, vetos = {:?}, sink_aborted = {}, any_aborted = {}, error = {:?}",
            r.events, r.vetoed, r.sink_aborted, r.any_aborted, r.error
        );
        assert!(r.sink_aborted, "el receptor cortó el lote");
        assert_eq!(r.vetoed.len(), 1, "{:?}", r.vetoed);
        assert_eq!(r.vetoed[0].item, victim.id);
        assert_eq!(r.vetoed[0].kind, VetoKind::IdentityMismatch);
        // The shell reports the veto as a failure (`E_ABORT`), never success: the item was not purged.
        assert!(
            r.events
                .iter()
                .filter(|e| e.item == victim.id)
                .all(|e| !e.succeeded()),
            "{:?}",
            r.events
        );
        let others: Vec<&BinItem> = mine.iter().skip(1).collect();
        for o in &others {
            let ev = r.events.iter().find(|e| e.item == o.id);
            eprintln!(
                "tras el vetado ({}): {} -> {:?}, ¿sigue en la Papelera? {}",
                victim.name.to_string_lossy(),
                o.name.to_string_lossy(),
                ev.map(|e| e.hr),
                PathBuf::from(&o.id).exists()
            );
        }
        assert_eq!(
            std::fs::read(&victim_r).unwrap(),
            b"sustituto",
            "el sustituto sigue ahí"
        );
        let left: Vec<String> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .map(|i| i.name.to_string_lossy().into_owned())
            .collect();
        eprintln!("tras el veto quedan en la Papelera (propios): {left:?}");
        assert!(left.iter().any(|n| *n == *victim.name.to_string_lossy()));
        // Cleanup: disable the hook (it would replace the first `$R` again) and purge any remaining owned items using their
        // current identities.
        drop(hook);
        let rest: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        purge_items(&rest, &AtomicBool::new(false)).unwrap();
        assert_eq!(ids(), before);
    }

    /// R-4: a veto is attributed to the OWNER of the vetoed path, not the path itself. When the vetoed path is a CHILD of a
    /// Recycle Bin folder (`owner` = its `$R`), the owning item is still recorded as vetoed in the chunk.
    #[test]
    fn el_veto_de_un_hijo_marca_como_vetado_al_elemento_de_su_carpeta() {
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"x");
        let b = synthetic_bin_item(&sb, "BBBB02", b"y");
        let c = synthetic_bin_item(&sb, "CCCC03", b"z");
        let chunk = [&a, &b, &c];
        let child_of_b = PathBuf::from(&b.id).join("sub/hijo.txt").into_os_string();
        let veto = Veto {
            item: child_of_b,
            owner: Some(b.id.clone()),
            kind: VetoKind::IdentityMismatch,
        };
        let v = vetoed_items(&chunk, std::slice::from_ref(&veto));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, b.id);
        // A veto with no owner (an item vetoed itself) is attributed to its own path.
        let own = Veto {
            item: a.id.clone(),
            owner: None,
            kind: VetoKind::NotApproved,
        };
        let v = vetoed_items(&chunk, &[own, veto]);
        assert_eq!(
            v.iter().map(|i| &i.id).collect::<Vec<_>>(),
            vec![&a.id, &b.id]
        );
        // No vetoes: no items.
        assert!(vetoed_items(&chunk, &[]).is_empty());
    }

    /// R-5: the residue sweep selects ONLY items from a test (a source folder named `tesela-test-<uuid>` under `%TEMP%`, or a
    /// loose item in `%TEMP%` with that name) that were deleted more than two minutes ago: never a user item or a recent item.
    #[test]
    fn el_barrido_solo_coge_restos_de_pruebas_antiguos() {
        const UUID: &str = "0b9e4a6c-1d2f-4e3a-8b7c-5d6e7f8a9b0c";
        let temp = PathBuf::from(r"C:\Users\<usuario>\AppData\Local\Temp");
        let item = |parent: &str, name: &str, deleted: i64| BinItem {
            id: OsString::from(r"C:\$Recycle.Bin\S-synthetic\$RXXXXXX.txt"),
            name: OsString::from(name),
            original_parent: PathBuf::from(parent),
            time_deleted: deleted,
            size: 1,
            data_id: None,
            meta_id: None,
            no_identity: false,
        };
        let now = 10_000;
        let old = now - 121;
        let test_dir = format!(r"{}\tesela-test-{UUID}\Docs", temp.display());
        // From a test and old: yes.
        assert!(sweep_candidate(&item(&test_dir, "a.txt", old), now, &temp));
        // ...but recent (120 seconds old or less): no.
        assert!(!sweep_candidate(
            &item(&test_dir, "a.txt", now - 120),
            now,
            &temp
        ));
        assert!(!sweep_candidate(&item(&test_dir, "a.txt", now), now, &temp));
        // From the user (any other folder), even if old: no.
        for user in [
            r"C:\Users\<usuario>\Documents",
            r"C:\Users\<usuario>\AppData\Local\Temp\otra",
            r"C:\Proyectos\tesela",
            r"C:\Users\<usuario>\AppData\Local\Temp\tesela-test-sin-uuid\x",
        ] {
            assert!(
                !sweep_candidate(&item(user, "a.txt", old), now, &temp),
                "{user}"
            );
        }
        // R-6b: a source folder that merely starts with `tesela-test-<uuid>` (with a suffix) is not a test folder.
        let extra_dir = format!(r"{}\tesela-test-{UUID}-extra\x", temp.display());
        assert!(!sweep_candidate(
            &item(&extra_dir, "a.txt", old),
            now,
            &temp
        ));
        // A LOOSE item in `%TEMP%`: only the exact `tesela-test-<uuid>` form qualifies.
        let temp_s = temp.display().to_string();
        assert!(sweep_candidate(
            &item(&temp_s, &format!("tesela-test-{UUID}.txt"), old),
            now,
            &temp
        ));
        assert!(!sweep_candidate(
            &item(&temp_s, "tesela-test-mis-notas.txt", old),
            now,
            &temp
        ));
        assert!(!sweep_candidate(
            &item(&temp_s, "informe.txt", old),
            now,
            &temp
        ));
        // R-6: UUID shape (36 characters, hexadecimal 8-4-4-4-12), with no suffix for folders.
        assert!(has_test_shape(&format!("tesela-test-{UUID}"), true));
        assert!(has_test_shape(
            &format!("TESELA-TEST-{}", UUID.to_uppercase()),
            true
        ));
        assert!(!has_test_shape(&format!("tesela-test-{UUID}-extra"), true));
        assert!(has_test_shape(&format!("tesela-test-{UUID}-extra"), false));
        for bad in [
            "tesela-test-",
            "tesela-test-1234",
            "tesela-test-0b9e4a6c1d2f4e3a8b7c5d6e7f8a9b0c0000",
            "tesela-test-0b9e4a6c-1d2f-4e3a-8b7c-5d6e7f8a9b0z",
            "tesela-tests-0b9e4a6c-1d2f-4e3a-8b7c-5d6e7f8a9b0c",
        ] {
            assert!(!has_test_shape(bad, false), "{bad}");
        }
    }

    /// R-5: cleanup deletion by path rejects a reparse point (junction) and fails CLOSED if it cannot open the item to delete;
    /// it also requires the path to match the current user's Recycle Bin form.
    #[test]
    fn el_borrado_por_ruta_rechaza_reparse_points_y_falla_cerrado() {
        let mut sb = Sandbox::new();
        sb.write("p/normal.txt", 10);
        sb.mkdir("p/carpeta");
        sb.junction("p/enlace", "p/carpeta");
        assert!(is_plain(&sb.long("p/normal.txt")));
        assert!(is_plain(&sb.long("p/carpeta")));
        assert!(!is_plain(&sb.long("p/enlace")), "un junction no es normal");
        assert!(
            !is_plain(&sb.long("p/no-existe")),
            "lo que no se abre, no es normal"
        );
        // The current user's Recycle Bin path form.
        let ok = Path::new(r"C:\$Recycle.Bin\S-synthetic-x\$RAAAA.txt");
        assert!(under_bin_of(ok, "S-synthetic-x"));
        assert!(under_bin_of(
            Path::new(r"c:\$recycle.bin\s-synthetic-x\$RAAAA.txt"),
            "S-SYNTHETIC-X"
        ));
        assert!(!under_bin_of(ok, "S-synthetic-otro"));
        assert!(!under_bin_of(ok, ""));
        assert!(!under_bin_of(
            Path::new(r"C:\carpeta\S-synthetic-x\$RAAAA.txt"),
            "S-synthetic-x"
        ));
        assert!(!under_bin_of(
            Path::new(r"C:\x\$Recycle.Bin\S-synthetic-x\$RAAAA.txt"),
            "S-synthetic-x"
        ));
        sb.remove_junction("p/enlace");
    }

    /// S-3: `BinGuard` leak detection must not report "0 leaks" when it could not inspect the bin: failure to list the bin is
    /// an error, and leaks cause `panic!` unless a panic is already in progress.
    #[test]
    fn la_comprobacion_de_fugas_falla_si_no_puede_listar_la_papelera() {
        assert_eq!(leak_verdict(Ok(0), false), None);
        assert_eq!(leak_verdict(Ok(0), true), None);
        assert!(leak_verdict(Ok(3), false).is_some_and(|m| m.contains("3 elementos")));
        assert_eq!(
            leak_verdict(Ok(3), true),
            None,
            "ya hay un pánico: no se aborta"
        );
        let err = || ShellError {
            hresult: 0x8000_4005u32 as i32,
            detail: "no se puede listar".into(),
        };
        assert!(leak_verdict(Err(err()), false).is_some_and(|m| m.contains("no se pudo listar")));
        assert_eq!(leak_verdict(Err(err()), true), None);
    }

    /// Recycle Bin incident (test cleanup): when the code under test fails to purge, `BinGuard`'s deletion BY PATH only touches
    /// items under `<drive>:\$Recycle.Bin\<current SID>\`. A synthetic sandbox item is untouched even if requested. Without
    /// the environment variable.
    #[test]
    fn el_borrado_por_ruta_de_la_limpieza_no_toca_lo_que_no_esta_en_la_papelera_propia() {
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"datos a");
        assert!(
            !remove_by_path(&a),
            "no es de la Papelera propia: no se borra"
        );
        assert!(PathBuf::from(&a.id).exists());
        assert!(meta_sibling(Path::new(&a.id)).unwrap().exists());
        // Nor a sandbox folder shaped like `$Recycle.Bin\<another SID>`.
        let dir = sb.long("fake/$Recycle.Bin/S-synthetic-otro");
        std::fs::create_dir_all(&dir).unwrap();
        let (d, m) = (dir.join("$RBBBB02.txt"), dir.join("$IBBBB02.txt"));
        std::fs::write(&d, b"x").unwrap();
        std::fs::write(&m, b"y").unwrap();
        let b = BinItem {
            id: d.clone().into_os_string(),
            ..a.clone()
        };
        assert!(!remove_by_path(&b));
        assert!(d.exists() && m.exists());
    }

    /// With the REAL shell (owned items): cleanup deletion by path removes an owned item's `$R` (including a folder) and `$I`,
    /// leaving the Recycle Bin as it was.
    #[test]
    fn con_la_shell_real_el_borrado_por_ruta_quita_un_elemento_propio_entero() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        sb.write("r/a.txt", 700);
        sb.write("r/carpeta/x.txt", 300);
        let paths = vec![sb.long("r/a.txt"), sb.long("r/carpeta")];
        recycle_paths(&send_items(&paths), RecycleMode::Normal).unwrap();
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        assert_eq!(mine.len(), 2, "{mine:?}");
        for it in &mine {
            assert!(remove_by_path(it), "{it:?}");
            assert!(!PathBuf::from(&it.id).exists());
            assert!(!meta_sibling(Path::new(&it.id)).unwrap().exists());
        }
        assert_eq!(ids(), before);
    }

    /// m-6: `remove_orphan_meta_at` leaves an item's `$I` alone while its `$R` still exists (a REPLACEMENT: the shell reported
    /// a successful purge but the approved object was not deleted), and while the `$R` is merely "pending deletion" (its guard
    /// remains open). It removes the metadata only when the `$R` is truly absent. (The purge loop also skips replacements.)
    #[test]
    fn el_i_de_un_sustituido_no_se_toca_mientras_su_r_exista() {
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"datos a");
        let data = PathBuf::from(&a.id);
        let meta = meta_sibling(&data).unwrap();
        assert!(!remove_orphan_meta_at(&a, &data, &meta), "el `$R` existe");
        assert!(meta.exists());
        // With the guard open and the shell having deleted the `$R`, its name remains visible as pending deletion: still no.
        let guard = open_bin_guard(&a).unwrap();
        crate::sys::delete_legacy_for_test(&data).unwrap();
        assert!(
            data.exists(),
            "pendiente de borrar: el nombre sigue visible"
        );
        assert!(!remove_orphan_meta_at(&a, &data, &meta));
        assert!(meta.exists(), "el `$I` sigue ahí");
        drop(guard);
        assert!(!data.exists(), "soltada la guarda desaparece de verdad");
    }

    /// A-M4 (without COM): a folder item reaches `PreDeleteItem` once per child (measured with the real shell). Children of an
    /// approved `$R` pass based on the identities of its `$R` and `$I`; if the `$R` is replaced, they are vetoed too. This does
    /// not apply to SEND: only the exact path is allowed.
    #[test]
    fn los_hijos_de_un_r_aprobado_se_comprueban_por_la_identidad_de_su_r() {
        let sb = Sandbox::new();
        let dir_item = {
            let it = synthetic_bin_item(&sb, "DDDD04", b"x");
            // This item's `$R` is a folder with children.
            std::fs::remove_file(PathBuf::from(&it.id)).unwrap();
            std::fs::create_dir_all(PathBuf::from(&it.id).join("sub")).unwrap();
            std::fs::write(PathBuf::from(&it.id).join("sub/hijo.txt"), b"h").unwrap();
            BinItem {
                data_id: identity_at(Path::new(&it.id)),
                ..it
            }
        };
        let child = PathBuf::from(&dir_item.id).join("sub/hijo.txt");
        let core = purge_core(&[&dir_item]);
        assert!(core.pre_delete(child.clone().into_os_string()));
        assert!(core.pre_delete(PathBuf::from(&dir_item.id).join("sub").into_os_string()));
        // An item NOT beneath the approved `$R`: rejected.
        assert!(!core.pre_delete(sb.path("bin/S-synthetic/otro/hijo.txt").into_os_string()));
        // Replace the `$R` with another folder: its children are vetoed.
        let core = purge_core(&[&dir_item]);
        std::fs::rename(
            PathBuf::from(&dir_item.id),
            sb.path("bin/S-synthetic/apartada"),
        )
        .unwrap();
        std::fs::create_dir_all(PathBuf::from(&dir_item.id).join("sub")).unwrap();
        std::fs::write(&child, b"h").unwrap();
        assert!(!core.pre_delete(child.clone().into_os_string()));
        assert_eq!(
            core.state.vetoes.lock().unwrap()[0].kind,
            VetoKind::IdentityMismatch
        );
        // With a SEND guard, a child is rejected even if its path is under an approved item.
        let envio = Guard::new(&[SendItem::capture(Path::new(&dir_item.id))]);
        assert_eq!(
            envio.check(&child.into_os_string()),
            Err(VetoKind::NotApproved)
        );
    }

    /// GM-m5: a child of a folder `$R` is allowed only if opening it by path shows that it is not a reparse point, is on the
    /// same volume, and its final path is beneath the approved `$R`. A junction INSIDE the `$R` (or a linked ancestor that
    /// redirects the path outside it) could make the shell delete outside the approved item, so it is vetoed. Since no other
    /// volume is available for this test, the final-path check also covers the volume criterion.
    #[test]
    fn un_hijo_de_un_r_carpeta_con_un_enlace_se_veta() {
        let mut sb = Sandbox::new();
        sb.write("fuera/secreto.txt", 10);
        let dir_item = {
            let it = synthetic_bin_item(&sb, "DDDD07", b"x");
            std::fs::remove_file(PathBuf::from(&it.id)).unwrap();
            std::fs::create_dir_all(PathBuf::from(&it.id).join("sub")).unwrap();
            std::fs::write(PathBuf::from(&it.id).join("sub/hijo.txt"), b"h").unwrap();
            BinItem {
                data_id: identity_at(Path::new(&it.id)),
                ..it
            }
        };
        let r = PathBuf::from(&dir_item.id);
        // Control: a normal child and the `sub` directory itself are allowed.
        let core = purge_core(&[&dir_item]);
        assert!(core.pre_delete(r.join("sub/hijo.txt").into_os_string()));
        assert!(core.pre_delete(r.join("sub").into_os_string()));
        // A junction INSIDE the `$R` that points outside: the link itself is rejected.
        let rel = "bin/S-synthetic/$RDDDD07.txt/enlace";
        sb.junction(rel, "fuera");
        let core = purge_core(&[&dir_item]);
        assert!(!core.pre_delete(r.join("enlace").into_os_string()));
        assert_eq!(
            core.state.vetoes.lock().unwrap()[0].kind,
            VetoKind::IdentityMismatch
        );
        // A child reached THROUGH the junction: its final path is outside the `$R`.
        let core = purge_core(&[&dir_item]);
        assert!(!core.pre_delete(r.join("enlace/secreto.txt").into_os_string()));
        assert_eq!(
            core.state.vetoes.lock().unwrap()[0].kind,
            VetoKind::NotApproved
        );
        // A child that does not exist: cannot be opened, so it is vetoed.
        let core = purge_core(&[&dir_item]);
        assert!(!core.pre_delete(r.join("sub/no-esta.txt").into_os_string()));
        assert!(matches!(
            core.state.vetoes.lock().unwrap()[0].kind,
            VetoKind::OpenFailed(_)
        ));
        sb.remove_junction(rel);
    }

    /// GM-m5: the shell's enumeration can repeat an item; it is matched only the first time.
    #[test]
    fn un_elemento_repetido_en_la_enumeracion_solo_se_casa_una_vez() {
        let mut seen = std::collections::HashSet::new();
        let a = OsString::from("a");
        let b = OsString::from("b");
        assert!(first_match(&mut seen, &a));
        assert!(!first_match(&mut seen, &a));
        assert!(first_match(&mut seen, &b));
        assert!(!first_match(&mut seen, &b));
        assert!(!first_match(&mut seen, &a));
    }

    /// GM-S1: an item with corrupt Recycle Bin metadata is skipped and does NOT abort the listing; items before and after it
    /// are preserved.
    #[test]
    fn un_elemento_con_el_i_corrupto_se_salta_sin_abortar_la_lista() {
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"a");
        let c = synthetic_bin_item(&sb, "CCCC01", b"c");
        let corrupto: Result<BinItem> = Err(ShellError {
            hresult: 0x8000_4005u32 as i32,
            detail: "metadatos corruptos".into(),
        });
        let mut out = Vec::new();
        assert!(push_readable(&mut out, Ok(a.clone())));
        assert!(!push_readable(&mut out, corrupto));
        assert!(push_readable(&mut out, Ok(c.clone())));
        assert_eq!(out, vec![a, c]);
    }

    /// A-M4 (without COM): an `$R` guard opens only if it is the captured object and is not already pending deletion; it proves
    /// deletion only when the item is pending. `replaced_after` distinguishes what the shell reported purged from what really was.
    #[test]
    fn la_guarda_de_la_purga_demuestra_si_se_borro_el_objeto_aprobado() {
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"datos a");
        let b = synthetic_bin_item(&sb, "BBBB02", b"datos b");
        // Unchanged: the guard opens and proves nothing.
        let ga = open_bin_guard(&a).expect("el `$R` capturado se abre");
        let gb = open_bin_guard(&b).expect("el `$R` capturado se abre");
        assert!(!guard_proves_deleted(&ga));
        // No captured identity, or a different identity: the guard does not open.
        assert!(
            open_bin_guard(&BinItem {
                data_id: None,
                ..a.clone()
            })
            .is_none()
        );
        assert!(
            open_bin_guard(&BinItem {
                data_id: Some(FileIdentity {
                    volume_serial: 1,
                    file_id: 2
                }),
                ..a.clone()
            })
            .is_none()
        );
        // Pending deletion: a second guard cannot open (the name returns access denied), and the first guard proves it.
        crate::sys::delete_legacy_for_test(Path::new(&a.id)).unwrap();
        assert!(guard_proves_deleted(&ga));
        assert!(open_bin_guard(&a).is_none());
        // The shell reported both purged successfully, but only `a` was actually deleted. `b` still exists under its name:
        // the object deleted under that name (in reality, a replacement) was not the approved object.
        let ev = |i: &BinItem, hr: i32| DeleteEvent {
            item: i.id.clone(),
            hr,
            state: BinState::Deleted,
        };
        let events = [ev(&a, 0), ev(&b, 0)];
        let guards = [(&a.id, &ga), (&b.id, &gb)];
        assert_eq!(replaced_after(&events, &guards), vec![b.id.clone()]);
        // A failure is not "reported as purged": do not inspect it.
        let fail = [ev(&b, 0x8000_4004u32 as i32)];
        assert!(replaced_after(&fail, &guards).is_empty());
    }

    /// A-M4 with the REAL shell (owned items only, after `TESELA_REAL_RECYCLE_TESTS=1`): with the receiver guard and item
    /// guards open, purge still works for a file and a FOLDER with children (which the shell reports one by one): no vetoes,
    /// no cutoffs, no replacements, and neither `$R` nor `$I` remains. Measure and print the result.
    #[test]
    fn con_la_shell_real_se_purgan_archivos_y_carpetas_con_las_guardas_abiertas() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        sb.write("g/a.txt", 700);
        sb.write("g/carpeta/x.txt", 300);
        sb.write("g/carpeta/sub/y.txt", 300);
        let paths = vec![sb.long("g/a.txt"), sb.long("g/carpeta")];
        recycle_paths(&send_items(&paths), RecycleMode::Normal).unwrap();
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        assert_eq!(mine.len(), 2, "{mine:?}");
        let r = purge_items(&mine, &AtomicBool::new(false)).unwrap();
        eprintln!(
            "purga con guardas: {} eventos ({} con éxito), vetos = {:?}, sustituidos = {:?}, sink_aborted = {}, error = {:?}, $I huérfanos quitados = {}",
            r.events.len(),
            r.events.iter().filter(|e| e.succeeded()).count(),
            r.vetoed,
            r.replaced_bin,
            r.sink_aborted,
            r.error,
            r.meta_removed
        );
        assert!(r.vetoed.is_empty() && !r.sink_aborted && r.replaced_bin.is_empty());
        assert!(r.error.is_none(), "{:?}", r.error);
        for it in &mine {
            assert!(
                r.events.iter().any(|e| e.item == it.id && e.succeeded()),
                "{it:?}"
            );
            assert!(!PathBuf::from(&it.id).exists(), "el `$R` ya no está");
            assert!(
                !meta_sibling(Path::new(&it.id)).unwrap().exists(),
                "ni su `$I`"
            );
        }
        assert_eq!(ids(), before);
    }

    /// An item to send using the LONG path form: the shell passes `SIGDN_DESKTOPABSOLUTEPARSING` to `PreDeleteItem`, which is
    /// always long, and `PathKey` does not resolve 8.3 names (in CI, `%TEMP%` uses 8.3 names).
    fn send_item(sb: &Sandbox, rel: &str, size: usize) -> SendItem {
        sb.write(rel, size);
        SendItem::capture(&sb.long(rel))
    }

    /// GM-M5 (without a case-sensitive filesystem): two names that differ only in case (possible in a case-sensitive directory)
    /// have the SAME `PathKey`. The guard first looks for an EXACT name, so each gets its own identity; key-only lookup would
    /// have assigned the first item's identity to the second. A name with no exact match is resolved by key only when unambiguous.
    #[test]
    fn la_guarda_distingue_dos_nombres_que_solo_difieren_en_mayusculas() {
        let id = |n: u128| FileIdentity {
            volume_serial: 9,
            file_id: n,
        };
        let item = |p: &str, n: u128| SendItem {
            path: PathBuf::from(p),
            identity: id(n),
            size_guard: None,
            watch: None,
        };
        let g = Guard::new(&[item(r"C:\x\a.bin", 1), item(r"C:\x\A.bin", 2)]);
        assert_eq!(
            PathKey::new(Path::new(r"C:\x\a.bin")),
            PathKey::new(Path::new(r"C:\x\A.bin"))
        );
        let want = |p: &str| -> Option<FileIdentity> {
            match g.find(&OsString::from(p)) {
                Some((_, Approved::Object { identity, .. })) => Some(*identity),
                _ => None,
            }
        };
        assert_eq!(want(r"C:\x\a.bin"), Some(id(1)));
        assert_eq!(want(r"C:\x\A.bin"), Some(id(2)));
        // Neither is an exact match for `A.BIN`: with two key candidates, do not guess (it would be vetoed as unapproved).
        assert_eq!(want(r"C:\x\A.BIN"), None);
        // With a single candidate, the key resolves it (the shell may use different casing or the `c:` form).
        let solo = Guard::new(&[item(r"C:\x\a.bin", 1)]);
        assert!(matches!(
            solo.find(&OsString::from(r"c:\X\A.BIN")),
            Some((_, Approved::Object { identity, .. })) if *identity == id(1)
        ));
    }

    /// GM-M6: an item recycled on a volume mounted in a FOLDER is recognized as "inside a Recycle Bin" (its `$Recycle.Bin`
    /// is under that volume's root, not a drive-letter root). Pure path test with the root injected.
    #[test]
    fn la_papelera_de_un_volumen_montado_en_una_carpeta_se_reconoce() {
        let mount = Path::new(r"C:\montajes\datos\");
        let bin = r"C:\montajes\datos\$Recycle.Bin\<sid>\$R123.txt";
        assert!(is_recycle_bin_path_on(Path::new(bin), Some(mount)));
        assert!(is_recycle_bin_path_on(
            Path::new(r"C:\montajes\datos\$Recycle.Bin"),
            Some(mount)
        ));
        // Without the volume root, only drive-letter paths qualify (the previous behavior).
        assert!(!is_recycle_bin_path_on(Path::new(bin), None));
        // Only at the volume root: a subfolder with that name is not a Recycle Bin.
        assert!(!is_recycle_bin_path_on(
            Path::new(r"C:\montajes\datos\sub\$Recycle.Bin\x"),
            Some(mount)
        ));
        // A different mount root is not affected.
        assert!(!is_recycle_bin_path_on(
            Path::new(r"C:\montajes\otro\$Recycle.Bin\x"),
            Some(mount)
        ));
        // Drive-letter paths behave the same with or without a known root.
        for root in [None, Some(Path::new(r"C:\"))] {
            assert!(is_recycle_bin_path_on(
                Path::new(r"C:\$Recycle.Bin\S-1-5\$Rabc"),
                root
            ));
            assert!(!is_recycle_bin_path_on(
                Path::new(r"C:\Users\<usuario>\a.txt"),
                root
            ));
        }
    }

    /// H3 (without COM): the path check in `PreDeleteItem`. The same identity passes; a different identity, a missing file, a
    /// child, an alias (another name for the same object), and any unapproved path are vetoed.
    #[test]
    fn el_guard_solo_deja_pasar_la_ruta_aprobada_con_su_identidad() {
        let sb = Sandbox::new();
        let a = send_item(&sb, "g/a.txt", 100);
        let dir = SendItem::capture(&sb.mkdir("g/carpeta"));
        sb.write("g/carpeta/hijo.txt", 50);
        let guard = Guard::new(&[a.clone(), dir.clone()]);
        let os = |p: &Path| p.as_os_str().to_os_string();
        // Same path and identity: allowed (including different capitalization).
        assert_eq!(guard.check(&os(&a.path)), Ok(()));
        assert_eq!(guard.check(&os(&dir.path)), Ok(()));
        let upper = PathBuf::from(a.path.to_string_lossy().to_uppercase());
        assert_eq!(guard.check(&os(&upper)), Ok(()));
        // A child of an approved item: vetoed (the real shell emits one event per folder).
        assert_eq!(
            guard.check(&os(&dir.path.join("hijo.txt"))),
            Err(VetoKind::NotApproved)
        );
        // An item that was not in the list.
        let otro = sb.write("g/otro.txt", 10);
        assert_eq!(guard.check(&os(&otro)), Err(VetoKind::NotApproved));
        // An alias of the approved object (hard link under ANOTHER name): same identity, but not the approved path.
        sb.hard_link("g/a.txt", "g/alias.txt");
        assert_eq!(
            guard.check(&os(&sb.path("g/alias.txt"))),
            Err(VetoKind::NotApproved)
        );
        // Move the approved item aside and put another object in its place: different identity.
        std::fs::remove_file(sb.path("g/alias.txt")).unwrap();
        std::fs::rename(sb.path("g/a.txt"), sb.path("g/a-original.txt")).unwrap();
        sb.write("g/a.txt", 100);
        assert_eq!(guard.check(&os(&a.path)), Err(VetoKind::IdentityMismatch));
        // A hard link to ANOTHER object at the approved name: different identity.
        std::fs::remove_file(sb.path("g/a.txt")).unwrap();
        sb.write("g/ajeno.txt", 100);
        sb.hard_link("g/ajeno.txt", "g/a.txt");
        assert_eq!(guard.check(&os(&a.path)), Err(VetoKind::IdentityMismatch));
        // A hard link to the SAME approved object under its name: identity matches (m1 detects this by comparing
        // `NumberOfLinks`; this check does not).
        std::fs::remove_file(sb.path("g/a.txt")).unwrap();
        sb.hard_link("g/a-original.txt", "g/a.txt");
        assert_eq!(guard.check(&os(&a.path)), Ok(()));
        // The approved name no longer exists: it cannot be checked, so it is vetoed.
        std::fs::remove_file(sb.path("g/a.txt")).unwrap();
        assert!(matches!(
            guard.check(&os(&a.path)),
            Err(VetoKind::OpenFailed(2 | 3))
        ));
    }

    /// H3 (without COM): the state reported by `psiNewlyCreated` and whether to stop the batch after `PostDeleteItem`.
    #[test]
    fn la_decision_de_cortar_el_lote_es_pura_y_cubre_todos_los_casos() {
        let p = PathBuf::from(r"C:\$Recycle.Bin\S\$R1");
        assert_eq!(bin_state(None), BinState::Deleted);
        assert_eq!(bin_state(Some(None)), BinState::InBinUnknownPath);
        assert_eq!(bin_state(Some(Some(p.clone()))), BinState::InBin(p.clone()));
        let ok = 0;
        let fail = 0x8007_0005u32 as i32;
        // On a successful SEND: deleted or missing path stops the batch; an item in the Recycle Bin does not.
        assert!(post_delete_aborts(true, ok, &BinState::Deleted));
        assert!(post_delete_aborts(true, ok, &BinState::InBinUnknownPath));
        assert!(post_delete_aborts(true, 0x0027_0003, &BinState::Deleted));
        assert!(!post_delete_aborts(true, ok, &BinState::InBin(p)));
        // A2-M1: a SEND failure also stops the batch regardless of state; it does not prove the shell left the item untouched.
        assert!(post_delete_aborts(true, fail, &BinState::Deleted));
        assert!(post_delete_aborts(true, fail, &BinState::InBinUnknownPath));
        let in_bin = |n: &str| BinState::InBin(PathBuf::from(format!(r"C:\$Recycle.Bin\S\{n}")));
        assert!(post_delete_aborts(true, fail, &in_bin("$R2")));
        assert!(post_delete_aborts(true, i32::MIN, &BinState::Deleted));
        // A success code (positive, such as `0x00270003`) is not a failure.
        assert!(!post_delete_aborts(true, 0x0027_0008, &in_bin("$R3")));
        // During PURGE, permanent deletion and per-item failures are expected; neither stops the batch.
        assert!(!post_delete_aborts(false, ok, &BinState::Deleted));
        assert!(!post_delete_aborts(false, ok, &BinState::InBinUnknownPath));
        assert!(!post_delete_aborts(false, fail, &BinState::Deleted));
        assert!(!post_delete_aborts(
            false,
            fail,
            &BinState::InBinUnknownPath
        ));
    }

    /// A SEND receiver guarded by these items.
    fn core_for(items: &[SendItem]) -> SinkCore {
        SinkCore {
            events: Arc::new(Mutex::new(Vec::new())),
            state: Arc::new(SinkState::default()),
            guard: Guard::new(items),
        }
    }

    /// H3 (without COM): the receiver records the event BEFORE deciding to stop; after stopping, it rejects everything else.
    #[test]
    fn el_receptor_registra_primero_corta_despues_y_niega_lo_posterior() {
        let sb = Sandbox::new();
        let a = send_item(&sb, "r/a.txt", 10);
        let b = send_item(&sb, "r/b.txt", 10);
        let core = core_for(&[a.clone(), b.clone()]);
        let os = |p: &Path| p.as_os_str().to_os_string();
        assert!(core.pre_delete(os(&a.path)));
        // Permanently deleted successfully: record the event and request a cutoff.
        assert!(core.post_delete(os(&a.path), 0, BinState::Deleted));
        {
            let ev = core.events.lock().unwrap();
            assert_eq!(ev.len(), 1);
            assert_eq!((ev[0].hr, &ev[0].state), (0, &BinState::Deleted));
        }
        assert!(core.state.tripped.load(Ordering::SeqCst));
        // Cut off: reject the next `PreDeleteItem` even if it is approved.
        assert!(!core.pre_delete(os(&b.path)));
        // A batch where everything goes to the Recycle Bin is not cut off and records every event.
        let ok = core_for(std::slice::from_ref(&a));
        let bin = PathBuf::from(r"C:\$Recycle.Bin\S\$R1");
        assert!(!ok.post_delete(os(&a.path), 0, BinState::InBin(bin)));
        assert!(!ok.state.tripped.load(Ordering::SeqCst));
        // During purge (with a purge guard), permanent deletion does not cut off the batch, and approved items are not vetoed.
        let pi = synthetic_bin_item(&sb, "PPPP05", b"x");
        let purge = purge_core(&[&pi]);
        assert!(purge.pre_delete(pi.id.clone()));
        assert!(!purge.post_delete(pi.id.clone(), 0, BinState::Deleted));
        assert!(!purge.state.tripped.load(Ordering::SeqCst));
        assert_eq!(purge.events.lock().unwrap().len(), 1);
    }

    /// H3 (without COM): a veto records its reason, cuts off the batch, and rejects subsequent items.
    #[test]
    fn un_veto_del_receptor_registra_el_motivo_y_corta_el_lote() {
        let sb = Sandbox::new();
        let a = send_item(&sb, "v/a.txt", 10);
        let b = send_item(&sb, "v/b.txt", 10);
        let core = core_for(&[a.clone(), b.clone()]);
        let os = |p: &Path| p.as_os_str().to_os_string();
        std::fs::rename(sb.path("v/a.txt"), sb.path("v/a-ori.txt")).unwrap();
        sb.write("v/a.txt", 10);
        assert!(!core.pre_delete(os(&a.path)), "el sustituto se veta");
        let vetoes = core.state.vetoes.lock().unwrap().clone();
        assert_eq!(vetoes.len(), 1);
        assert_eq!(vetoes[0].kind, VetoKind::IdentityMismatch);
        assert!(core.state.tripped.load(Ordering::SeqCst));
        assert!(!core.pre_delete(os(&b.path)), "lo siguiente, también");
    }

    /// A file to send with the size check (A2-B1): its guard handle (the same one opened by the executor) and the size counted
    /// toward the budget. The handle travels INSIDE `SendItem` (`size_guard`) and closes when the item is dropped.
    fn sized_item(sb: &Sandbox, rel: &str, size: usize, budget: u64) -> SendItem {
        sb.write(rel, size);
        let path = sb.long(rel);
        let mut item = SendItem::capture(&path);
        item.size_guard = Some(SizeGuard {
            handle: Arc::new(sys::open_path_for_rename(&path).expect("abre la guarda")),
            budget,
        });
        item
    }

    /// A2-B1 (without COM): `PreDeleteItem` measures the file again THROUGH ITS GUARD HANDLE. Within the counted size, it passes
    /// (including if it shrank); if larger (on disk or logically), it is vetoed as `Grew`. Identity is checked first (a
    /// replacement remains `IdentityMismatch`), and an item without a size check (a directory) is not measured.
    #[test]
    fn el_guard_veta_un_archivo_que_crecio_por_su_handle_de_guarda() {
        let sb = Sandbox::new();
        // 1,000 bytes occupy 4,096 on disk (one cluster): that is the counted size.
        let a = sized_item(&sb, "z/a.txt", 1_000, 4_096);
        let guard = Guard::new(std::slice::from_ref(&a));
        let os = |p: &Path| p.as_os_str().to_os_string();
        assert_eq!(guard.check(&os(&a.path)), Ok(()), "tal cual se midió");
        // It grows slightly within the same cluster: logical size increases but stays within the counted size.
        std::fs::write(sb.path("z/a.txt"), vec![1u8; 4_000]).unwrap();
        assert_eq!(guard.check(&os(&a.path)), Ok(()), "dentro de lo contado");
        // It exceeds the cluster: both on-disk and logical sizes exceed the counted size.
        std::fs::write(sb.path("z/a.txt"), vec![1u8; 1 << 20]).unwrap();
        assert_eq!(guard.check(&os(&a.path)), Err(VetoKind::Grew));
        // It shrinks: still within the counted size (only growth exceeds the budget).
        std::fs::write(sb.path("z/a.txt"), vec![1u8; 10]).unwrap();
        assert_eq!(guard.check(&os(&a.path)), Ok(()));
        // H3: no tolerance. With a 4,096-byte budget, 4,096 bytes fit (one cluster on disk) and 4,097 do not (logical size
        // exceeds the count even if allocation remains one cluster or eight KiB).
        let e = sized_item(&sb, "z/exacto.txt", 4_096, 4_096);
        let ge = Guard::new(std::slice::from_ref(&e));
        assert_eq!(ge.check(&os(&e.path)), Ok(()), "justo lo contado");
        std::fs::write(sb.path("z/exacto.txt"), vec![1u8; 4_097]).unwrap();
        assert_eq!(ge.check(&os(&e.path)), Err(VetoKind::Grew), "un byte más");
        // H3: on-disk allocation counts even when logical size is unchanged (`FileAllocationInfo` without changing `EndOfFile`).
        let pre = sized_item(&sb, "z/prealoca.txt", 1_000, 4_096);
        let gp = Guard::new(std::slice::from_ref(&pre));
        assert_eq!(gp.check(&os(&pre.path)), Ok(()));
        sys::preallocate_for_test(&sb.path("z/prealoca.txt"), 1 << 20).unwrap();
        assert_eq!(
            gp.check(&os(&pre.path)),
            Err(VetoKind::Grew),
            "más en disco"
        );
        // Only LOGICAL size exceeds the count (a sparse file: little on disk, much larger logically): veto it too.
        let big = sb.path("z/dispersa.bin");
        crate::testutil::sparse_file(&big, 1 << 30);
        let mut d = SendItem::capture(&sb.long("z/dispersa.bin"));
        d.size_guard = Some(SizeGuard {
            handle: Arc::new(sys::open_path_for_rename(&sb.long("z/dispersa.bin")).unwrap()),
            budget: 4_096,
        });
        let g2 = Guard::new(std::slice::from_ref(&d));
        assert_eq!(g2.check(&os(&d.path)), Err(VetoKind::Grew));
        // A replacement at the path: identity takes precedence and is checked BEFORE size.
        std::fs::rename(sb.path("z/a.txt"), sb.path("z/a-ori.txt")).unwrap();
        sb.write("z/a.txt", 10);
        assert_eq!(guard.check(&os(&a.path)), Err(VetoKind::IdentityMismatch));
        // Without a size check (a directory or an unguarded file), no measurement is made.
        let dir = SendItem::capture(&sb.mkdir("z/carpeta"));
        sb.write("z/carpeta/hijo.txt", 1 << 20);
        let g3 = Guard::new(std::slice::from_ref(&dir));
        assert_eq!(g3.check(&os(&dir.path)), Ok(()));
    }

    /// A2-M1 (without COM): a batch with TWO small folders. The shell fails on a child of the first and reports an error
    /// (`E_ACCESSDENIED`, no new item): the receiver records the event and cuts off the batch, so the second folder never gets
    /// `PreDeleteItem` (the shell does not process it). Previously, an error returned success, allowing the next item through
    /// and potentially losing another child.
    #[test]
    fn un_fallo_en_la_primera_carpeta_del_lote_corta_antes_de_la_segunda() {
        let sb = Sandbox::new();
        sb.write("q/c1/a.bin", 100);
        sb.write("q/c2/a.bin", 100);
        let c1 = SendItem::capture(&sb.long("q/c1"));
        let c2 = SendItem::capture(&sb.long("q/c2"));
        let core = core_for(&[c1.clone(), c2.clone()]);
        let os = |p: &Path| p.as_os_str().to_os_string();
        assert!(core.pre_delete(os(&c1.path)));
        let denied = 0x8007_0005u32 as i32;
        assert!(
            core.post_delete(os(&c1.path), denied, BinState::Deleted),
            "el fallo corta el lote"
        );
        assert!(core.state.tripped.load(Ordering::SeqCst));
        {
            let ev = core.events.lock().unwrap();
            assert_eq!(ev.len(), 1, "el evento consta antes de cortar");
            assert_eq!(ev[0].hr, denied);
        }
        assert!(
            !core.pre_delete(os(&c2.path)),
            "la segunda carpeta no se procesa"
        );
        assert_eq!(core.events.lock().unwrap().len(), 1);
        assert!(
            core.state.vetoes.lock().unwrap().is_empty(),
            "no es un veto"
        );
    }

    /// A2-B1 (without COM): the receiver vetoes the file that grew, cuts off the batch, and records the reason; previously
    /// processed items remain in the event log.
    #[test]
    fn el_receptor_veta_el_archivo_que_crecio_y_corta_el_lote() {
        let sb = Sandbox::new();
        let a = sized_item(&sb, "w/a.txt", 100, 4_096);
        let b = sized_item(&sb, "w/b.txt", 100, 4_096);
        let c = sized_item(&sb, "w/c.txt", 100, 4_096);
        let core = core_for(&[a.clone(), b.clone(), c.clone()]);
        let os = |p: &Path| p.as_os_str().to_os_string();
        let bin = PathBuf::from(r"C:\$Recycle.Bin\S\$R1");
        assert!(core.pre_delete(os(&a.path)));
        assert!(!core.post_delete(os(&a.path), 0, BinState::InBin(bin)));
        // `b` grows after the batch is prepared and before its `PreDeleteItem`.
        std::fs::write(sb.path("w/b.txt"), vec![1u8; 1 << 20]).unwrap();
        assert!(!core.pre_delete(os(&b.path)), "el que creció se veta");
        let vetoes = core.state.vetoes.lock().unwrap().clone();
        assert_eq!(vetoes.len(), 1);
        assert_eq!(vetoes[0].kind, VetoKind::Grew);
        assert_eq!(
            PathKey::new(Path::new(vetoes[0].id())),
            PathKey::new(&b.path)
        );
        assert!(core.state.tripped.load(Ordering::SeqCst));
        assert!(!core.pre_delete(os(&c.path)), "y el resto no se procesa");
        assert_eq!(core.events.lock().unwrap().len(), 1);
    }

    // ---- A-m1: receiver COM CALLBACKS called directly (without a real Recycle Bin operation) ---------------------
    //
    // So far, only the pure `SinkCore` logic was tested: mutants returning `Ok(())` from `PreDeleteItem` or `PostDeleteItem`
    // survived because nothing checked that `IFileOperationProgressSink` methods called that logic and translated its result
    // to `E_ABORT`. Here, the receiver is created as a COM interface and its methods are called with `IShellItem`s created by
    // `SHCreateItemFromParsingName` for sandbox-owned files. COM must be initialized on the test thread, but no shell operation
    // or Recycle Bin is used, so this runs in CI.

    /// An `IShellItem` for a sandbox path.
    fn shell_item(path: &Path) -> IShellItem {
        let name = parsing_name(path);
        // SAFETY: `name` is NUL-terminated and lives for the duration of the call; the caller initialized COM.
        unsafe { SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(name.as_ptr()), None) }
            .expect("crea el IShellItem")
    }

    /// The COM receiver and its shared state, for inspection afterward.
    fn com_sink(guard: Guard) -> (IFileOperationProgressSink, Events, Arc<SinkState>) {
        let events: Events = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(SinkState::default());
        let sink: IFileOperationProgressSink = Recorder {
            core: SinkCore {
                events: events.clone(),
                state: state.clone(),
                guard,
            },
        }
        .into();
        (sink, events, state)
    }

    fn is_abort(r: windows::core::Result<()>) -> bool {
        r.is_err_and(|e| e.code() == E_ABORT)
    }

    /// A-m1: `PreDeleteItem` through the COM interface. Approved items pass; a replacement at the path is vetoed with
    /// `E_ABORT` (and the veto is recorded); after the batch is cut off, the next item is rejected even if approved. (A mutant
    /// `PreDeleteItem -> Ok(())` fails.)
    #[test]
    fn el_callback_pre_delete_item_veta_con_e_abort_lo_que_no_es_lo_aprobado() {
        let _com = Com::init().unwrap();
        let sb = Sandbox::new();
        let a = send_item(&sb, "c/a.txt", 100);
        let b = send_item(&sb, "c/b.txt", 100);
        let (sink, _events, state) = com_sink(Guard::new(&[a.clone(), b.clone()]));
        // SAFETY: the COM interface is alive on its owning thread; the `IShellItem`s are alive.
        unsafe {
            assert!(sink.PreDeleteItem(0, &shell_item(&a.path)).is_ok());
            // After checking, replace `a.txt` with another file of the same name.
            std::fs::rename(sb.path("c/a.txt"), sb.path("c/a-original.txt")).unwrap();
            std::fs::write(sb.path("c/a.txt"), vec![7u8; 100]).unwrap();
            let veto = sink.PreDeleteItem(0, &shell_item(&a.path));
            assert!(is_abort(veto), "el sustituto se veta con E_ABORT");
            assert!(state.tripped.load(Ordering::SeqCst));
            assert_eq!(state.vetoes.lock().unwrap().len(), 1);
            assert_eq!(
                state.vetoes.lock().unwrap()[0].kind,
                VetoKind::IdentityMismatch
            );
            // The batch is cut off: `b.txt` is approved but still rejected.
            assert!(is_abort(sink.PreDeleteItem(0, &shell_item(&b.path))));
        }
    }

    /// A2-B1: `PreDeleteItem` through the COM interface with the size check: a file that grew after batch preparation is vetoed
    /// with `E_ABORT` and records `Grew`; before it grows, the same callback allows it. (A mutant removing the size check from
    /// `Guard::check` fails here.)
    #[test]
    fn el_callback_pre_delete_item_veta_con_e_abort_el_archivo_que_crecio() {
        let _com = Com::init().unwrap();
        let sb = Sandbox::new();
        let a = sized_item(&sb, "k/a.txt", 100, 4_096);
        let (sink, _events, state) = com_sink(Guard::new(std::slice::from_ref(&a)));
        // SAFETY: the COM interface is alive on its owning thread; the `IShellItem`s are alive.
        unsafe {
            assert!(sink.PreDeleteItem(0, &shell_item(&a.path)).is_ok());
            std::fs::write(sb.path("k/a.txt"), vec![7u8; 1 << 20]).unwrap();
            let veto = sink.PreDeleteItem(0, &shell_item(&a.path));
            assert!(is_abort(veto), "el archivo que creció se veta con E_ABORT");
        }
        assert!(state.tripped.load(Ordering::SeqCst));
        let vetoes = state.vetoes.lock().unwrap().clone();
        assert_eq!(vetoes.len(), 1);
        assert_eq!(vetoes[0].kind, VetoKind::Grew);
    }

    /// A-m1: `PostDeleteItem` through the COM interface records the event (with its `HRESULT` and `psiNewlyCreated` state) and
    /// returns `E_ABORT` only when the shell permanently deleted the item successfully (null) or supplied no path. (A mutant
    /// `PostDeleteItem -> Ok(())` fails because it would neither record nor cut off the batch.)
    #[test]
    fn el_callback_post_delete_item_registra_el_evento_y_corta_tras_un_borrado_definitivo() {
        let _com = Com::init().unwrap();
        let sb = Sandbox::new();
        let a = send_item(&sb, "d/a.txt", 100);
        let b = send_item(&sb, "d/b.txt", 100);
        sb.write("d/bin/R1.txt", 10);
        let dest = sb.long("d/bin/R1.txt");
        let (sink, events, state) = com_sink(Guard::new(&[a.clone(), b.clone()]));
        // SAFETY: the COM interface is alive on its owning thread; the `IShellItem`s are alive.
        unsafe {
            // Success with an item in the Recycle Bin: record it and do NOT cut off the batch.
            let r = sink.PostDeleteItem(0, &shell_item(&a.path), HRESULT(0), &shell_item(&dest));
            assert!(r.is_ok());
            assert!(!state.tripped.load(Ordering::SeqCst));
            // Success with null `psiNewlyCreated`: the shell permanently deleted it -> cut off with `E_ABORT`.
            let r = sink.PostDeleteItem(0, &shell_item(&b.path), HRESULT(0), None::<&IShellItem>);
            assert!(is_abort(r), "borrado definitivo con éxito: E_ABORT");
            assert!(state.tripped.load(Ordering::SeqCst));
        }
        let ev = events.lock().unwrap().clone();
        assert_eq!(ev.len(), 2, "el evento se registra ANTES de decidir cortar");
        assert_eq!(PathKey::new(Path::new(&ev[0].item)), PathKey::new(&a.path));
        assert_eq!(ev[0].hr, 0);
        assert!(
            matches!(&ev[0].state, BinState::InBin(p) if PathKey::new(p) == PathKey::new(&dest)),
            "{:?}",
            ev[0].state
        );
        assert_eq!(ev[1].state, BinState::Deleted);
        // A2-M1: a shell FAILURE (and null) DOES cut off the batch (previously it returned success and let the next
        // `PreDeleteItem` through), and is recorded. This reproduces Astra: `PostDeleteItem(E_ACCESSDENIED, NULL)`.
        let (sink, events, state) = com_sink(Guard::new(&[a.clone(), b.clone()]));
        // SAFETY: same as above.
        unsafe {
            let fail = HRESULT(0x8007_0005u32 as i32);
            let r = sink.PostDeleteItem(0, &shell_item(&a.path), fail, None::<&IShellItem>);
            assert!(is_abort(r), "un fallo corta el lote con E_ABORT");
            assert!(state.tripped.load(Ordering::SeqCst));
            // The next item in the batch is approved but still rejected.
            assert!(is_abort(sink.PreDeleteItem(0, &shell_item(&b.path))));
        }
        assert_eq!(events.lock().unwrap()[0].hr, 0x8007_0005u32 as i32);
        // A failure with an item in the Recycle Bin (`hr < 0` but `psiNewlyCreated` supplied) also cuts off the batch.
        let (sink, _events, state) = com_sink(Guard::new(std::slice::from_ref(&a)));
        // SAFETY: same as above.
        unsafe {
            let fail = HRESULT(0x8007_0020u32 as i32);
            let r = sink.PostDeleteItem(0, &shell_item(&a.path), fail, &shell_item(&dest));
            assert!(is_abort(r));
            assert!(state.tripped.load(Ordering::SeqCst));
        }
        // During PURGE, permanent deletion is expected: record it without cutting off the batch.
        let pi = synthetic_bin_item(&sb, "PPPP05", b"x");
        let (sink, events, state) = com_sink(Guard::purge(&[&pi]));
        // SAFETY: same as above.
        unsafe {
            assert!(
                sink.PreDeleteItem(0, &shell_item(Path::new(&pi.id)))
                    .is_ok()
            );
            let r = sink.PostDeleteItem(
                0,
                &shell_item(Path::new(&pi.id)),
                HRESULT(0),
                None::<&IShellItem>,
            );
            assert!(r.is_ok());
        }
        assert_eq!(events.lock().unwrap().len(), 1);
        assert!(!state.tripped.load(Ordering::SeqCst));
    }

    /// M-2 (without COM): a veto for a CHILD of a Recycle Bin folder is attributed to the list item that authorized it
    /// (its `$R`), not the child's path; a veto with no owner is attributed to the item itself.
    #[test]
    fn el_veto_de_un_hijo_se_atribuye_al_r_de_su_carpeta() {
        let sb = Sandbox::new();
        let it = synthetic_bin_item(&sb, "DDDD04", b"x");
        std::fs::remove_file(PathBuf::from(&it.id)).unwrap();
        std::fs::create_dir_all(PathBuf::from(&it.id).join("sub")).unwrap();
        std::fs::write(PathBuf::from(&it.id).join("sub/hijo.txt"), b"h").unwrap();
        let dir_item = BinItem {
            data_id: identity_at(Path::new(&it.id)),
            ..it
        };
        let child = PathBuf::from(&dir_item.id).join("sub/hijo.txt");
        let core = purge_core(&[&dir_item]);
        // Replace the `$R` with another folder: the child is vetoed and the veto belongs to THAT item.
        std::fs::rename(
            PathBuf::from(&dir_item.id),
            sb.path("bin/S-synthetic/apartada"),
        )
        .unwrap();
        std::fs::create_dir_all(PathBuf::from(&dir_item.id).join("sub")).unwrap();
        std::fs::write(&child, b"h").unwrap();
        assert!(!core.pre_delete(child.clone().into_os_string()));
        let v = core.state.vetoes.lock().unwrap()[0].clone();
        assert_eq!(v.item, child.clone().into_os_string());
        assert_eq!(v.owner.as_ref(), Some(&dir_item.id));
        assert_eq!(v.id(), &dir_item.id, "se atribuye al `$R`, no al hijo");
        // An item outside every approved item: no owner, so the veto belongs to its own path.
        let core = purge_core(&[&dir_item]);
        let ajeno = sb.path("bin/S-synthetic/otro.txt").into_os_string();
        assert!(!core.pre_delete(ajeno.clone()));
        let v = core.state.vetoes.lock().unwrap()[0].clone();
        assert_eq!((v.owner.as_ref(), v.id()), (None, &ajeno));
    }

    /// M-3 with the REAL shell (owned items): THREE items, with a veto in the MIDDLE (second in shell order). Measured result:
    /// the first is purged, the vetoed replacement remains at its `$R` path, and the last receives no callback and stays in
    /// the Recycle Bin.
    #[test]
    fn con_la_shell_real_un_veto_en_el_medio_del_lote_purga_el_anterior_y_no_el_siguiente() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        for n in ["a.txt", "b.txt", "c.txt"] {
            sb.write(&format!("w/{n}"), 700);
        }
        let paths: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt"]
            .iter()
            .map(|n| sb.long(&format!("w/{n}")))
            .collect();
        recycle_paths(&send_items(&paths), RecycleMode::Normal).unwrap();
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        assert_eq!(mine.len(), 3, "{mine:?}");
        // The shell processes items in enumeration order; the middle item is the one vetoed.
        let (first, victim, last) = (mine[0].clone(), mine[1].clone(), mine[2].clone());
        let victim_r = PathBuf::from(&victim.id);
        let _apartado = RemoveOnDrop(victim_r.with_extension("apartado"));
        let hook = crate::engine::test_hooks::install_before_shell(victim_r.clone(), move |p| {
            std::fs::rename(p, p.with_extension("apartado")).unwrap();
            std::fs::write(p, b"sustituto").unwrap();
        });
        let r = purge_items(&mine, &AtomicBool::new(false)).unwrap();
        eprintln!(
            "veto en el medio: eventos = {:?}, sink_aborted = {}",
            r.events
                .iter()
                .map(|e| (e.hr, e.succeeded()))
                .collect::<Vec<_>>(),
            r.sink_aborted
        );
        assert!(r.sink_aborted);
        assert_eq!(r.vetoed.len(), 1);
        assert_eq!(r.vetoed[0].id(), &victim.id);
        let ev = |i: &BinItem| r.events.iter().find(|e| e.item == i.id);
        assert!(
            ev(&first).is_some_and(DeleteEvent::succeeded),
            "el anterior se purga"
        );
        assert!(!PathBuf::from(&first.id).exists());
        assert!(
            ev(&victim).is_some_and(|e| !e.succeeded()),
            "el vetado, con fallo"
        );
        assert_eq!(std::fs::read(&victim_r).unwrap(), b"sustituto");
        assert!(ev(&last).is_none(), "el siguiente no recibe aviso");
        assert!(PathBuf::from(&last.id).exists(), "y sigue en la Papelera");
        drop(hook);
        let rest: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        purge_items(&rest, &AtomicBool::new(false)).unwrap();
        assert_eq!(ids(), before);
    }

    /// M-3 with the REAL shell (owned items): with chunks of 2 and 5 items, a veto in the FIRST chunk also cuts off later
    /// chunks, so none of the other 3 items is purged (without the `break` in `purge_items_flags`, they would be).
    #[test]
    fn con_la_shell_real_un_veto_corta_tambien_los_trozos_restantes() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        let _chunk = set_purge_chunk(2);
        let names = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"];
        for n in names {
            sb.write(&format!("t/{n}"), 700);
        }
        let paths: Vec<PathBuf> = names.iter().map(|n| sb.long(&format!("t/{n}"))).collect();
        recycle_paths(&send_items(&paths), RecycleMode::Normal).unwrap();
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        assert_eq!(mine.len(), 5, "{mine:?}");
        let victim = mine[0].clone();
        let victim_r = PathBuf::from(&victim.id);
        let _apartado = RemoveOnDrop(victim_r.with_extension("apartado"));
        let hook = crate::engine::test_hooks::install_before_shell(victim_r.clone(), move |p| {
            std::fs::rename(p, p.with_extension("apartado")).unwrap();
            std::fs::write(p, b"sustituto").unwrap();
        });
        let log_steps = Mutex::new(Vec::<String>::new());
        let r = purge_items_observed(&mine, &AtomicBool::new(false), &mut |step| {
            let name = match step {
                PurgeStep::Begin(i) => format!("begin {}", i.len()),
                PurgeStep::Skipped(_) => "skipped".to_string(),
                PurgeStep::Unverifiable(_) => "unverifiable".to_string(),
                PurgeStep::Vetoed(i) => format!("vetoed {}", i.len()),
                PurgeStep::Linked(i) => format!("linked {}", i.len()),
                PurgeStep::Replaced(_) => "replaced".to_string(),
                PurgeStep::Done(d) => format!("done {}", d.len()),
            };
            log_steps.lock().unwrap().push(name);
        })
        .unwrap();
        eprintln!("trozos: {:?}", log_steps.lock().unwrap());
        assert!(r.sink_aborted && r.vetoed.len() == 1);
        // Only one chunk (with 2 items) was submitted; the rest are recorded as "not processed" (done with 3) with no intent.
        let steps = log_steps.lock().unwrap().clone();
        assert_eq!(
            steps.iter().filter(|s| s.starts_with("begin")).count(),
            1,
            "{steps:?}"
        );
        assert!(steps.contains(&"done 3".to_string()), "{steps:?}");
        for it in &mine[1..] {
            assert!(PathBuf::from(&it.id).exists(), "{it:?} no se debía purgar");
        }
        drop(hook);
        let rest: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        purge_items(&rest, &AtomicBool::new(false)).unwrap();
        assert_eq!(ids(), before);
    }

    /// A-m1 + A-M4: the PURGE guard through the COM interface, using synthetic Recycle Bin items. `PreDeleteItem`, given the
    /// item's parsing name, checks the `$R` and `$I` and vetoes a replaced `$R` with `E_ABORT`.
    #[test]
    fn el_callback_pre_delete_item_de_una_purga_veta_el_r_sustituido() {
        let _com = Com::init().unwrap();
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"datos a");
        let b = synthetic_bin_item(&sb, "BBBB02", b"datos b");
        let (sink, _events, state) = com_sink(Guard::purge(&[&a, &b]));
        // SAFETY: the COM interface is alive on its owning thread; the `IShellItem`s are alive.
        unsafe {
            assert!(sink.PreDeleteItem(0, &shell_item(Path::new(&a.id))).is_ok());
            std::fs::remove_file(PathBuf::from(&b.id)).unwrap();
            std::fs::write(PathBuf::from(&b.id), b"datos b").unwrap();
            assert!(is_abort(
                sink.PreDeleteItem(0, &shell_item(Path::new(&b.id)))
            ));
        }
        assert_eq!(state.vetoes.lock().unwrap().len(), 1);
        assert_eq!(state.vetoes.lock().unwrap()[0].item, b.id);
    }

    /// S1: send and purge flags include `FOF_NO_CONNECTED_ELEMENTS` (approving `pagina.html` must not include `pagina_files\`),
    /// and send retains the flags that make it recoverable and silent.
    #[test]
    fn las_banderas_de_envio_y_de_purga_excluyen_los_elementos_conectados() {
        let has = |f: FILEOPERATION_FLAGS, x: FILEOPERATION_FLAGS| f.0 & x.0 == x.0;
        let send = send_flags(RecycleMode::Normal);
        for flag in [
            FOF_NO_CONNECTED_ELEMENTS,
            FOF_NO_UI,
            FOF_ALLOWUNDO,
            FOFX_RECYCLEONDELETE,
        ] {
            assert!(has(send, flag), "{flag:?}");
        }
        assert!(has(
            send_flags(RecycleMode::WithNukeWarning),
            FOF_NO_CONNECTED_ELEMENTS
        ));
        let purge = purge_flags();
        assert!(has(purge, FOF_NO_CONNECTED_ELEMENTS) && has(purge, FOF_NO_UI));
        // And never the flag that hangs when combined with FOF_NO_UI.
        assert!(!has(send, FOF_WANTNUKEWARNING) && !has(purge, FOF_WANTNUKEWARNING));
    }

    /// S1 with the REAL shell: sending `pagina.html` does not include its `pagina_files\` folder. Owned items only.
    #[test]
    fn enviar_pagina_html_no_arrastra_su_carpeta_pagina_files() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        sb.write("w/pagina.html", 1_000);
        sb.write("w/pagina_files/imagen.png", 2_000);
        let r = recycle_paths(
            &send_items(&[sb.long("w/pagina.html")]),
            RecycleMode::Normal,
        )
        .unwrap();
        assert!(r.error.is_none() && !r.any_aborted, "{r:?}");
        assert!(!sb.path("w/pagina.html").exists(), "el aprobado se fue");
        assert!(
            sb.path("w/pagina_files/imagen.png").exists(),
            "el elemento conectado NO se aprobó y no debe irse"
        );
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        assert_eq!(mine.len(), 1, "{mine:?}");
        purge_items(&mine, &AtomicBool::new(false)).unwrap();
        assert_eq!(ids(), before);
    }

    /// M2 with the REAL shell: a huge sparse file (too large for any Recycle Bin) FIRST in a batch with two small files. The
    /// shell permanently deletes it; the receiver records the event and returns `E_ABORT`, and the two small files are NOT
    /// processed: they remain in place and are not in the Recycle Bin. Owned items only.
    #[test]
    fn con_la_shell_real_el_primer_borrado_inesperado_no_arrastra_al_resto_del_lote() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        let total = tesela_scanner::volume_space(&sb.long_root())
            .map(|v| v.total)
            .unwrap_or(1 << 40);
        let len = total.saturating_mul(2).max(1 << 40);
        let big = sb.path("m/a-enorme.bin");
        std::fs::create_dir_all(big.parent().unwrap()).unwrap();
        sparse_file(&big, len);
        sb.write("m/b.txt", 500);
        sb.write("m/c.txt", 500);
        let paths = vec![
            sb.long("m/a-enorme.bin"),
            sb.long("m/b.txt"),
            sb.long("m/c.txt"),
        ];
        let r = recycle_paths(&send_items(&paths), RecycleMode::Normal).unwrap();
        eprintln!(
            "lote con un disperso enorme primero: eventos = {:?}, abortado = {}, sink_aborted = {}, error = {:?}",
            r.events, r.any_aborted, r.sink_aborted, r.error
        );
        let ev = r.events.first().expect("la shell avisa de lo que elimina");
        assert!(ev.succeeded() && ev.state == BinState::Deleted, "{ev:?}");
        assert!(r.sink_aborted, "el receptor cortó el lote");
        assert_eq!(r.events.len(), 1, "nada más se procesó: {:?}", r.events);
        assert!(
            !paths[0].exists(),
            "el enorme se eliminó (evento registrado)"
        );
        assert!(
            paths[1].exists() && paths[2].exists(),
            "los demás siguen en su sitio"
        );
        let in_bin = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .count();
        assert_eq!(in_bin, 0, "ninguno llegó a la Papelera");
        assert_eq!(ids(), before);
    }

    /// M1 with the REAL shell: approve `b.txt`, then rename it and put another file under the same name BEFORE passing it to
    /// the shell. `PreDeleteItem` compares identities, finds a mismatch, and returns `E_ABORT`: the replacement does NOT reach
    /// the Recycle Bin, the batch is cut off (the next item is not processed), and the veto is recorded.
    #[test]
    fn con_la_shell_real_un_sustituto_con_el_mismo_nombre_no_se_envia() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        sb.write("s/a.txt", 700);
        sb.write("s/b.txt", 800);
        sb.write("s/c.txt", 900);
        let paths = vec![sb.long("s/a.txt"), sb.long("s/b.txt"), sb.long("s/c.txt")];
        let items = send_items(&paths);
        // After approval: move the approved item aside and put another file under its name.
        std::fs::rename(sb.path("s/b.txt"), sb.path("s/b-original.txt")).unwrap();
        sb.write("s/b.txt", 800);
        let r = recycle_paths(&items, RecycleMode::Normal).unwrap();
        eprintln!(
            "sustituto: eventos = {:?}, vetos = {:?}, sink_aborted = {}",
            r.events, r.vetoed, r.sink_aborted
        );
        assert!(r.sink_aborted, "el receptor cortó el lote");
        assert_eq!(r.vetoed.len(), 1, "{:?}", r.vetoed);
        assert_eq!(r.vetoed[0].kind, VetoKind::IdentityMismatch);
        assert_eq!(
            PathKey::new(Path::new(&r.vetoed[0].item)),
            PathKey::new(&paths[1])
        );
        // The replacement remains in place, the original is still moved aside, and `c.txt` was not processed.
        assert!(sb.path("s/b.txt").exists() && sb.path("s/b-original.txt").exists());
        assert!(
            sb.path("s/c.txt").exists(),
            "el resto del lote no se procesó"
        );
        // Only `a.txt` could have reached the Recycle Bin (it was processed before the veto), and that is verified.
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        assert!(
            mine.iter().all(|i| i.name != "b.txt" && i.name != "c.txt"),
            "{mine:?}"
        );
        purge_items(&mine, &AtomicBool::new(false)).unwrap();
        assert_eq!(ids(), before);
    }

    /// A2-B1 with the REAL shell (owned items only): three files in one operation; the middle file grows after batch preparation
    /// (its counted size is now too small). The receiver measures it again through its guard handle in `PreDeleteItem`, vetoes
    /// it with `E_ABORT`, and the shell does not process the next item; the first reaches the Recycle Bin. This measures how
    /// the shell handles a size veto, like the identity veto in the previous test.
    #[test]
    fn con_la_shell_real_un_archivo_que_crecio_se_veta_y_el_resto_del_lote_no_se_procesa() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        let a = sized_item(&sb, "g/a.txt", 700, 4_096);
        let b = sized_item(&sb, "g/b.txt", 800, 4_096);
        let c = sized_item(&sb, "g/c.txt", 900, 4_096);
        let paths = [a.path.clone(), b.path.clone(), c.path.clone()];
        // After preparation, `b` grows beyond the counted size.
        std::fs::write(sb.path("g/b.txt"), vec![1u8; 1 << 20]).unwrap();
        let r = recycle_paths(&[a, b, c], RecycleMode::Normal).unwrap();
        eprintln!(
            "crecido: eventos = {:?}, vetos = {:?}, sink_aborted = {}",
            r.events, r.vetoed, r.sink_aborted
        );
        assert!(r.sink_aborted, "el receptor cortó el lote");
        assert_eq!(r.vetoed.len(), 1, "{:?}", r.vetoed);
        assert_eq!(r.vetoed[0].kind, VetoKind::Grew);
        assert_eq!(
            PathKey::new(Path::new(&r.vetoed[0].item)),
            PathKey::new(&paths[1])
        );
        assert!(sb.path("g/b.txt").exists(), "el que creció no se envió");
        assert!(sb.path("g/c.txt").exists(), "el resto no se procesó");
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        assert!(
            mine.iter().all(|i| i.name != "b.txt" && i.name != "c.txt"),
            "{mine:?}"
        );
        purge_items(&mine, &AtomicBool::new(false)).unwrap();
        assert_eq!(ids(), before);
    }

    /// A2-B1 MEASUREMENT (not a test; run alone with `cargo test -- --ignored --nocapture medicion_a2_b1` and
    /// `TESELA_REAL_RECYCLE_TESTS=1`, using owned items only): cost of sending 500 small files with the REAL shell (a) in ONE
    /// operation with the `PreDeleteItem` size check, (b) in one operation without that check (control), and (c) one operation
    /// per file (the rejected alternative). Prints milliseconds per round.
    #[test]
    #[ignore = "medición A2-B1"]
    fn medicion_a2_b1_coste_de_la_comprobacion_de_tamano_con_500_archivos() {
        const N: usize = 500;
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        // `guarded`: every file carries its guard handle and counted size, as in production.
        let make = |tag: &str, round: usize, guarded: bool| -> Vec<SendItem> {
            (0..N)
                .map(|i| {
                    let rel = format!("m/{tag}{round}/f{i:04}.txt");
                    sb.write(&rel, 1_000);
                    let path = sb.long(&rel);
                    let mut it = SendItem::capture(&path);
                    if guarded {
                        it.size_guard = Some(SizeGuard {
                            handle: Arc::new(sys::open_path_for_rename(&path).unwrap()),
                            budget: 4_096,
                        });
                    }
                    it
                })
                .collect()
        };
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
        let mut rows: Vec<(String, f64)> = Vec::new();
        for round in 0..3 {
            let items = make("a", round, true);
            let t = std::time::Instant::now();
            let r = recycle_paths(&items, RecycleMode::Normal).unwrap();
            let d = ms(t);
            assert!(
                r.vetoed.is_empty() && r.events.len() == N,
                "{}",
                r.events.len()
            );
            rows.push((format!("a{round} (1 operación, con comprobación)"), d));
            drop(items);
            let items = make("b", round, false);
            let t = std::time::Instant::now();
            let r = recycle_paths(&items, RecycleMode::Normal).unwrap();
            let d = ms(t);
            assert!(r.vetoed.is_empty() && r.events.len() == N);
            rows.push((format!("b{round} (1 operación, sin comprobación)"), d));
        }
        {
            let items = make("c", 0, false);
            let t = std::time::Instant::now();
            for it in &items {
                let r = recycle_paths(std::slice::from_ref(it), RecycleMode::Normal).unwrap();
                assert!(r.vetoed.is_empty() && r.events.len() == 1);
            }
            rows.push(("c0 (500 operaciones de 1 archivo)".to_string(), ms(t)));
        }
        for (name, d) in &rows {
            eprintln!(
                "MEDICION {name}: {d:.1} ms ({:.3} ms por archivo)",
                d / N as f64
            );
        }
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        purge_items(&mine, &AtomicBool::new(false)).unwrap();
        assert_eq!(ids(), before);
    }

    /// A2-B1 MEASUREMENT (no shell, no Recycle Bin): per-file cost added by the size check in `Guard::check` (existing identity
    /// reopen by path versus that plus reading `FileStandardInfo` through the guard handle). Run with
    /// `cargo test -- --ignored --nocapture medicion_a2_b1_coste_del_check`.
    #[test]
    #[ignore = "medición A2-B1"]
    fn medicion_a2_b1_coste_del_check_por_archivo() {
        const N: usize = 500;
        let sb = Sandbox::new();
        let plain: Vec<SendItem> = (0..N)
            .map(|i| {
                sb.write(&format!("p/f{i:04}.txt"), 1_000);
                SendItem::capture(&sb.long(&format!("p/f{i:04}.txt")))
            })
            .collect();
        let sized: Vec<SendItem> = plain
            .iter()
            .map(|it| {
                let mut it = it.clone();
                it.size_guard = Some(SizeGuard {
                    handle: Arc::new(sys::open_path_for_rename(&it.path).unwrap()),
                    budget: 4_096,
                });
                it
            })
            .collect();
        for (name, items) in [("solo identidad", &plain), ("identidad + tamaño", &sized)] {
            let g = Guard::new(items);
            let mut best = f64::MAX;
            for _ in 0..20 {
                let t = std::time::Instant::now();
                for it in items {
                    assert_eq!(g.check(&it.path.as_os_str().to_os_string()), Ok(()));
                }
                best = best.min(t.elapsed().as_secs_f64() * 1e6 / N as f64);
            }
            eprintln!("MEDICION check por archivo, {name}: {best:.1} us");
        }
    }

    /// Sentinel: without `TESELA_REAL_RECYCLE_TESTS=1`, tests using the real Recycle Bin are silently skipped (they pass without
    /// running). This test reports how many were skipped (the `bin_usable()` calls in this file and `exec_tests.rs`). Cargo
    /// hides output from passing tests; show it with `cargo test -- --nocapture`.
    #[test]
    fn centinela_pruebas_de_la_papelera_real_saltadas() {
        if bintest::real_recycle_requested() {
            return;
        }
        let call = concat!("if !bin_", "usable()");
        let n = include_str!("recycle.rs").matches(call).count()
            + include_str!("exec_tests.rs").matches(call).count();
        assert!(
            n > 0,
            "el centinela no encuentra ninguna prueba de la Papelera real"
        );
        eprintln!(
            "{n} tests de la Papelera real NO se han ejecutado (defina {}=1 para ejecutarlos)",
            bintest::REAL_RECYCLE_VAR
        );
    }

    #[test]
    fn los_totales_de_la_papelera_son_coherentes_con_la_lista() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let (n, _bytes) = bin_totals().expect("SHQueryRecycleBin");
        let listed = list_bin().unwrap().len() as u64;
        // The shell counts top-level items the same way as enumeration (the count may differ if the user empties or fills the
        // Recycle Bin at this moment).
        assert!(n.abs_diff(listed) <= 2, "{n} frente a {listed}");
    }

    #[test]
    fn purgar_una_lista_vacia_no_hace_nada() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        assert_eq!(
            purge_items(&[], &AtomicBool::new(false)).unwrap(),
            BatchOutcome::default()
        );
        assert_eq!(
            recycle_paths(&[], RecycleMode::Normal).unwrap(),
            BatchOutcome::default()
        );
        assert_eq!(ids(), before);
    }

    /// What happened to an item after `recycle_paths` (checking disk and the Recycle Bin without the receiver).
    #[derive(Debug, PartialEq, Eq)]
    enum Fate {
        StillThere,
        InBin,
        GoneNotInBin,
    }

    fn fate(path: &Path, bin: &[BinItem], uuid: &str) -> Fate {
        if path.exists() {
            return Fate::StillThere;
        }
        let name = path.file_name().unwrap();
        if bin.iter().any(|i| is_mine(i, uuid) && i.name == name) {
            Fate::InBin
        } else {
            Fate::GoneNotInBin
        }
    }

    /// REQUIRED TEST (PLAN.md §3.7.4): `FOF_WANTNUKEWARNING` combined with `FOF_NO_UI` and an item that does NOT fit in the
    /// Recycle Bin (a sparse file with a logical size larger than the entire volume; it occupies no space).
    ///
    /// Findings that define the design (measured on Windows 11; the test requires invariants, not an exact outcome):
    /// 1. With the application's flags (`FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE`), the shell returns SUCCESS
    ///    (`any_aborted == false`) and the item DISAPPEARS without reaching the Recycle Bin: it is silently deleted permanently.
    ///    Therefore, the application checks BEFOREHAND whether it fits (the volume's actual limit) and then VERIFIES each item
    ///    through the event receiver: null `psiNewlyCreated` means permanent deletion. This test compares the receiver's report
    ///    with the disk and Recycle Bin state (the documented semantics must hold).
    /// 2. With `FOF_WANTNUKEWARNING` added, the shell does not report a return-code warning; it waits for a response that
    ///    `FOF_NO_UI` prevents. Therefore, this flag is never used in production (this variant exists only in tests), and this
    ///    part runs in a timed CHILD PROCESS that is killed if it hangs, avoiding orphaned threads or dialogs.
    #[test]
    fn wantnukewarning_con_no_ui_y_un_elemento_que_no_cabe() {
        let _lock = bin_lock();
        if !bin_usable() {
            return;
        }
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        let total = tesela_scanner::volume_space(&sb.long_root())
            .map(|v| v.total)
            .unwrap_or(1 << 40);
        let len = total.saturating_mul(2).max(1 << 40);

        // (1) Application flags: the call returns, and verification identifies the outcome.
        let p = sb.path("n/enorme-normal.bin");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        sparse_file(&p, len);
        let lp = sb.long("n/enorme-normal.bin");
        let res = recycle_paths(&send_items(std::slice::from_ref(&lp)), RecycleMode::Normal);
        let f = fate(&lp, &list_bin().unwrap(), &sb.uuid());
        let out = res.as_ref().expect("la llamada vuelve");
        eprintln!(
            "banderas de la aplicación sobre {} GiB lógicos: abortado = {}, eventos = {:?}, desenlace = {f:?}",
            len >> 30,
            out.any_aborted,
            out.events
        );
        // The receiver reports the outcome: `Deleted` (null `psiNewlyCreated`) = gone without reaching the Recycle Bin;
        // `InBin` = in the Recycle Bin; no event (or an error) = still at its original location.
        let ev = out.events.first();
        match f {
            Fate::GoneNotInBin => {
                let ev = ev.expect("la shell avisa de lo que elimina");
                assert!(ev.succeeded() && ev.state == BinState::Deleted, "{ev:?}");
                // M2: the receiver recorded the event and CUT OFF the batch (returned E_ABORT after `PostDeleteItem`).
                assert!(out.sink_aborted, "el receptor debe cortar el lote: {out:?}");
            }
            Fate::InBin => {
                let ev = ev.expect("la shell avisa de lo que envía");
                assert!(
                    ev.succeeded() && matches!(ev.state, BinState::InBin(_)),
                    "{ev:?}"
                );
            }
            Fate::StillThere => assert!(ev.is_none_or(|e| !e.succeeded()), "{ev:?}"),
        }

        // (2) With FOF_WANTNUKEWARNING, in a timed child process.
        let p2 = sb.path("n/enorme-nukewarning.bin");
        sparse_file(&p2, len);
        let lp2 = sb.long("n/enorme-nukewarning.bin");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "recycle::tests::hijo_nukewarning",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TESELA_NUKE_CHILD_PATH", &lp2)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("lanza el proceso hijo de la prueba");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let completed = loop {
            if let Some(st) = child.try_wait().unwrap() {
                break Some(st.success());
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        let f2 = fate(&lp2, &list_bin().unwrap(), &sb.uuid());
        eprintln!(
            "FOF_WANTNUKEWARNING con FOF_NO_UI: {} ; desenlace = {:?}",
            match completed {
                None =>
                    "SE CUELGA (hubo que matar el proceso: espera una respuesta que no se puede dar)",
                Some(true) => "vuelve",
                Some(false) => "el hijo falló",
            },
            f2
        );
        // Explicit cleanup (the guard repeats it): no owned items remain in the Recycle Bin, and the bin is restored to its
        // previous state.
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        purge_items(&mine, &AtomicBool::new(false)).unwrap();
        assert_eq!(ids(), before);
    }

    /// Child-process body for the previous test (not run on its own: `#[ignore]` and requires the environment variable).
    #[test]
    #[ignore = "solo lo lanza wantnukewarning_con_no_ui_y_un_elemento_que_no_cabe"]
    fn hijo_nukewarning() {
        let Some(p) = std::env::var_os("TESELA_NUKE_CHILD_PATH") else {
            return;
        };
        let p = PathBuf::from(p);
        // Operates only on a file owned by this test, inside its temporary directory.
        let s = p.to_string_lossy().to_lowercase();
        assert!(s.contains("tesela-test-") && s.ends_with("enorme-nukewarning.bin"));
        let _ = recycle_paths(&send_items(&[p]), RecycleMode::WithNukeWarning);
    }
}
