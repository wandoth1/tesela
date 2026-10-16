//! La Papelera de reciclaje: enviar rutas, listar su contenido y purgar una lista enumerada, con COM (`windows`).
//!
//! `IFileOperation` trabaja por RUTAS y no se puede atar a un handle. El diseño (sin lápidas) es:
//! 1. quien llama abre el elemento en exclusiva, captura su identidad y detecta "en uso", y CIERRA el handle;
//! 2. este módulo envía la RUTA ORIGINAL a la Papelera con un `IFileOperationProgressSink` propio ([`Recorder`]) que
//!    registra, por elemento, `hrDelete` y la ruta real del elemento ya en la Papelera (`psiNewlyCreated`);
//! 3. quien llama abre ESA ruta (`$R…`) y compara su identidad con la aprobada.
//!
//! Entre la comprobación del paso 1 y que la shell abra el elemento hay una ventana. El handle de GUARDA
//! (`DELETE | READ_ATTRIBUTES`, compartiéndolo todo) que quien llama conserva abierto hasta después del paso 3 impide
//! renombrar los ANCESTROS del elemento (el padre y el abuelo: error 5), así que no se puede cambiar un ancestro por un
//! junction hacia otra carpeta (medido por la auditoría; cubierto por las pruebas) y la shell recicla igual. La guarda NO
//! impide renombrar el propio elemento (el handle comparte `DELETE`, que la shell necesita para moverlo a `$Recycle.Bin`) ni
//! poner otro objeto con ese nombre. Eso lo vigila el receptor de eventos (ver más abajo): `PreDeleteItem` comprueba la
//! IDENTIDAD de lo que hay en la ruta justo antes de que la shell actúe.
//!
//! La garantía real es, pues: (a) la comprobación de ruta y protección es previa y se mantiene con el handle de guarda;
//! (b) `PreDeleteItem` veta lo que no es el objeto aprobado, con una ventana RESIDUAL entre esa comprobación y el
//! movimiento real de la shell (la shell abre y mueve por ruta: no hay forma de atarla a un handle); (c) lo que llega a la
//! Papelera se comprueba por IDENTIDAD, y además `Recycled` exige que el nombre aprobado ya no exista y que el objeto de
//! la guarda esté ahora dentro de una Papelera (otro nombre del mismo objeto, p. ej. un enlace duro, no cuenta); (d)
//! lo que NO es el objeto aprobado y llega a la Papelera con su ruta conocida se informa como `RecycledOther` (está en la
//! Papelera: se puede restaurar). Esto NO es una garantía de que un sustituto acabe ahí: ver el residual de más abajo.
//!
//! Banderas (decididas por las pruebas, ver `tests::wantnukewarning_*`):
//! * enviar: `FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_NO_CONNECTED_ELEMENTS`;
//! * purgar elementos de la Papelera: `FOF_NO_UI | FOF_NO_CONNECTED_ELEMENTS`.
//!
//! `FOF_NO_CONNECTED_ELEMENTS` (S1): sin ella, aprobar `pagina.html` puede arrastrar `pagina_files\`, que nadie aprobó.
//!
//! # El receptor de eventos vigila (M1 y M2)
//! * `PreDeleteItem` (M1) abre cada elemento por su ruta, sin seguir enlaces, y compara su IDENTIDAD con la aprobada; si no
//!   coincide (o no se puede abrir, o no es lo aprobado), registra un [`Veto`] y devuelve `E_ABORT`: la shell cancela lo
//!   pendiente y NO envía el sustituto (falla cerrado). RESIDUAL, dicho con precisión: entre esa comprobación y el
//!   movimiento real la shell actúa por ruta y un sustituto puesto exactamente ahí SÍ llega a la shell. Si cabe en la
//!   Papelera y la shell da su ruta, el paso 3 lo detecta como `RecycledOther` (recuperable). Si no (el reparto y la
//!   capacidad se calcularon para el objeto APROBADO, no para el sustituto; o la shell no da ruta, o el movimiento falla), se
//!   informa como `DeletedUnexpectedly` o `Unverified` y un borrado definitivo del sustituto NO se puede deshacer.
//! * `PostDeleteItem` (M2) registra el evento PRIMERO y, si la shell eliminó definitivamente (`psiNewlyCreated` nulo) o no
//!   dijo dónde quedó, devuelve `E_ABORT`: el primer borrado inesperado ya no arrastra al resto del lote.
//!   Solo al ENVIAR; al purgar la Papelera el borrado definitivo es lo esperado y no corta.
//!
//! # La purga de la Papelera (A-M4)
//! Los elementos de la lista se verifican por identidad (`$R` y `$I`) al enumerar y la shell actúa después. Para cerrar esa
//! ventana, cada trozo de hasta 500 elementos:
//! * abre un handle de GUARDA por `$R` (`DELETE | READ_ATTRIBUTES`, compartiéndolo todo) justo antes de la operación y exige que
//!   sea el objeto capturado: mientras vive, nadie renombra los ancestros del `$R` (la carpeta del usuario en `$Recycle.Bin`);
//!   medido con la shell real: la purga funciona igual con las guardas abiertas;
//! * entrega la operación con un receptor cuyo `PreDeleteItem` vuelve a comprobar por ruta (sin seguir enlaces) la identidad del
//!   `$R` y del `$I`, y veta con `E_ABORT` lo que ya no es lo capturado. Una carpeta de la Papelera llega a `PreDeleteItem`
//!   hijo a hijo (medido): lo que cuelga de un `$R` aprobado se comprueba por la identidad de ese `$R`;
//! * al terminar, y antes de soltar las guardas, comprueba que todo lo que la shell dio por purgado era el objeto aprobado
//!   (`delete_pending`); si no, consta como "sustituido" (`replaced_bin`).
//!
//! Medido con la shell real tras un veto en una purga: la shell avisa del vetado con `E_ABORT` (`0x80004004`, "Operación
//! anulada"), purga los anteriores del lote y NO procesa los siguientes (siguen en la Papelera). Se corta el resto de trozos.
//!
//! Ventana que SIGUE abierta: entre la comprobación de `PreDeleteItem` (por ruta) y el borrado real de la shell. No hay forma
//! de atar la shell a un handle. Un sustituto puesto exactamente ahí SÍ se borra (la purga es definitiva y no se puede
//! deshacer); se detecta justo después (guarda no pendiente de borrar) y consta como sustituido, pero no se evita.
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

/// Error de la shell o de COM (código `HRESULT` y descripción técnica; la interfaz lo traduce a su manera).
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

/// COM inicializado en el hilo actual (apartamento de un solo hilo, como pide la shell). Se desinicializa al soltarlo,
/// salvo que el hilo ya estuviera en otro modelo (`RPC_E_CHANGED_MODE`): entonces se usa tal cual.
pub struct Com {
    uninit: bool,
}

impl Com {
    pub fn init() -> Result<Com> {
        // SAFETY: `CoInitializeEx` no tiene precondiciones; el puntero reservado es nulo, como exige la API.
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
            // SAFETY: equilibra el `CoInitializeEx` correcto de `init` en este mismo hilo.
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

/// ¿Sigue siendo el elemento aprobado? Re-lee la identidad del `$R` y del `$I` y la compara con la capturada al enumerar.
/// Sin identidad capturada o con otra distinta (alguien vació y volvió a llenar la Papelera, o sustituyó los archivos),
/// no: el elemento se salta ("cambió desde la confirmación").
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

/// Texto de un `PWSTR` de la shell, liberándolo con `CoTaskMemFree`.
///
/// # Safety
/// `p` debe ser una cadena UTF-16 terminada en NUL reservada con `CoTaskMemAlloc` (como las que devuelve
/// `IShellItem::GetDisplayName`), y no se vuelve a usar.
unsafe fn take_pwstr(p: PWSTR) -> OsString {
    // SAFETY: lo garantiza quien llama: `p` es una cadena terminada en NUL, válida hasta que se libere.
    let s = unsafe { p.as_wide() };
    let out = OsString::from_wide(s);
    // SAFETY: la cadena se reservó con el asignador de COM y se libera una sola vez, tras copiarla.
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    out
}

fn filetime_to_unix(ft: FILETIME) -> i64 {
    const EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
    let t = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
    (t.saturating_sub(EPOCH_AS_FILETIME) / 10_000_000) as i64
}

/// Lista TODOS los elementos de la Papelera del usuario (todas las unidades). Es lineal en el tamaño de la Papelera
/// (decenas de segundos con 100 000 elementos, PLAN.md §3.6): quien llama la ejecuta en un hilo de trabajo.
pub fn list_bin() -> Result<Vec<BinItem>> {
    let _com = Com::init()?;
    // SAFETY: COM está inicializado en este hilo; todas las interfaces y cadenas que se obtienen aquí son propias de
    // esta función y se liberan con el RAII de `windows` o con `take_pwstr`.
    unsafe {
        let bin: IShellItem =
            SHGetKnownFolderItem(&FOLDERID_RecycleBinFolder, KF_FLAG_DEFAULT, None)?;
        let en: IEnumShellItems = bin.BindToHandler(None, &BHID_EnumItems)?;
        let mut out = Vec::new();
        loop {
            let mut fetched = 0u32;
            let mut arr = [None];
            en.Next(&mut arr, Some(&mut fetched))?;
            if fetched == 0 {
                break;
            }
            let Some(item) = &arr[0] else { break };
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
            out.push(BinItem {
                id,
                name,
                original_parent,
                time_deleted,
                size,
                data_id,
                meta_id,
                no_identity,
            });
        }
        Ok(out)
    }
}

/// Cuántos elementos y cuántos bytes hay en la Papelera del usuario (todas las unidades), según la shell.
pub fn bin_totals() -> Result<(u64, u64)> {
    let mut info = SHQUERYRBINFO {
        cbSize: size_of::<SHQUERYRBINFO>() as u32,
        ..SHQUERYRBINFO::default()
    };
    // SAFETY: `info` es escribible y trae su `cbSize`; una ruta nula consulta todas las unidades.
    unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info)? };
    Ok((info.i64NumItems.max(0) as u64, info.i64Size.max(0) as u64))
}

/// Qué le pasó a un elemento, según la propia shell (`IFileOperationProgressSink::PostDeleteItem`).
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
    // SAFETY: `w` termina en NUL y vive durante la llamada; `info` es escribible y trae su `cbSize`.
    unsafe { SHQueryRecycleBinW(PCWSTR(w.as_ptr()), &mut info)? };
    Ok(info.i64Size.max(0) as u64)
}

/// ¿Está `p` dentro de una Papelera (`<unidad>:\$Recycle.Bin\...`)? Mira la forma de la ruta (carpeta de primer nivel
/// de la raíz de una unidad llamada `$Recycle.Bin`), sin consultar al sistema.
pub fn is_recycle_bin_path(p: &Path) -> bool {
    use crate::paths::PathKey;
    let key = PathKey::new(p);
    let is_bin = |k: &PathKey| {
        k.file_name() == "$recycle.bin" && k.parent().is_some_and(|q| q.is_drive_root())
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
}

/// Un elemento que se entrega a la shell: su ruta original y la IDENTIDAD aprobada (volumen + FileId).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendItem {
    pub path: PathBuf,
    pub identity: FileIdentity,
}

impl SendItem {
    /// SOLO PARA PRUEBAS: captura la identidad actual de `path` (en producción la aprueba el ejecutor, no se lee aquí).
    #[cfg(test)]
    pub(crate) fn capture(path: &Path) -> SendItem {
        let h = sys::open_for_query(path).expect("abre el elemento");
        SendItem {
            path: path.to_path_buf(),
            identity: sys::meta(&h).expect("lee la identidad").identity,
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
}

/// Un veto del receptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Veto {
    /// Nombre de análisis del elemento (su ruta).
    pub item: OsString,
    pub kind: VetoKind,
}

type Events = Arc<Mutex<Vec<DeleteEvent>>>;

/// Nombre de análisis o ruta de un elemento de la shell, si se puede.
fn display(item: &IShellItem, sigdn: SIGDN) -> Option<OsString> {
    // SAFETY: `item` es una interfaz viva; la cadena que devuelve es de COM y `take_pwstr` la libera una sola vez.
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
    /// Al ENVIAR: la identidad del objeto en su ruta original.
    Object(FileIdentity),
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
    /// Es la guarda de una PURGA de la Papelera (si no, de un envío).
    purge: bool,
}

impl Guard {
    /// Guarda de un ENVÍO a la Papelera (ruta original + identidad aprobada).
    fn new(items: &[SendItem]) -> Guard {
        Guard {
            approved: items
                .iter()
                .map(|i| (PathKey::new(&i.path), Approved::Object(i.identity)))
                .collect(),
            purge: false,
        }
    }

    /// Guarda de una PURGA (A-M4): cada elemento de la lista con las identidades de su `$R` y de su `$I` capturadas al
    /// enumerar. Sin las dos, el elemento no se purga (nunca llega aquí: ver `check_bin_item`); si falta alguna se vetaría.
    fn purge(items: &[&BinItem]) -> Guard {
        Guard {
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

    /// ¿Es `path` lo aprobado? Abre por ruta sin seguir enlaces y compara la identidad. SOLO pasa lo que está aprobado
    /// por esa ruta exacta: un hijo de un elemento aprobado, un alias (otro nombre del mismo objeto) o cualquier otra cosa
    /// se vetan (`NotApproved`). Con la shell real una carpeta emite un único evento, así que la rama "los hijos pasan"
    /// era inalcanzable y fallar cerrado es mejor (H6.1). Al purgar se comprueban el `$R` y el `$I`.
    fn check(&self, path: &OsString) -> std::result::Result<(), VetoKind> {
        let key = PathKey::new(Path::new(path));
        // Al PURGAR, un elemento de la Papelera que es una CARPETA llega a `PreDeleteItem` hijo a hijo (medido con la shell
        // real: `...\$R<token>\hijo.txt`), no con la ruta de su `$R`: lo que cuelga de un `$R` aprobado se comprueba por
        // la identidad de ESE `$R` (y de su `$I`). Al ENVIAR sigue valiendo solo la ruta exacta (un evento por carpeta).
        let found = self.approved.iter().find(|(k, _)| *k == key).or_else(|| {
            self.purge
                .then(|| self.approved.iter().find(|(k, _)| key.is_same_or_under(k)))
                .flatten()
        });
        let Some((_, want)) = found else {
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
            Approved::Object(id) => same(Path::new(path), id),
            Approved::Bin {
                data_path,
                data,
                meta_path,
                meta,
            } => {
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
/// definitivo es lo esperado) y solo si la shell dijo que fue bien pero el elemento no está verificablemente en la
/// Papelera: eliminado definitivamente (`Deleted`) o sin ruta (`InBinUnknownPath`). Pura.
fn post_delete_aborts(sending: bool, hrdelete: i32, state: &BinState) -> bool {
    sending && hrdelete >= 0 && matches!(state, BinState::Deleted | BinState::InBinUnknownPath)
}

/// La lógica del receptor SIN COM (así se prueba en cualquier equipo): `Recorder` solo traduce los argumentos de la shell
/// y llama aquí.
struct SinkCore {
    events: Events,
    state: Arc<SinkState>,
    guard: Option<Guard>,
}

impl SinkCore {
    /// `PreDeleteItem`: `true` = la shell puede seguir con este elemento; `false` = el receptor debe devolver `E_ABORT`.
    fn pre_delete(&self, item: OsString) -> bool {
        let Some(guard) = &self.guard else {
            return true;
        };
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
                    .push(Veto { item, kind });
                false
            }
        }
    }

    /// `PostDeleteItem`: registra el evento PRIMERO. `true` = el receptor debe devolver `E_ABORT` (y el lote queda
    /// cancelado).
    fn post_delete(&self, item: OsString, hrdelete: i32, state: BinState) -> bool {
        let abort = post_delete_aborts(
            self.guard.as_ref().is_some_and(Guard::sends),
            hrdelete,
            &state,
        );
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
    fn new(flags: FILEOPERATION_FLAGS, guard: Option<Guard>) -> Result<Operation> {
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
        // SAFETY: COM está inicializado en este hilo (lo garantiza quien llama); `sink` es una interfaz viva que `Advise`
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
        // SAFETY: COM está inicializado en este hilo; `self.op` es una interfaz viva y `cookie` el de su `Advise`.
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
pub fn recycle_paths(items: &[SendItem], mode: RecycleMode) -> Result<BatchOutcome> {
    if items.is_empty() {
        return Ok(BatchOutcome::default());
    }
    let _com = Com::init()?;
    let op = Operation::new(send_flags(mode), Some(Guard::new(items)))?;
    let mut added = 0usize;
    for it in items {
        let name = parsing_name(&it.path);
        // Una ruta que la shell no entiende (demasiado larga...) no aborta el lote: ese elemento no genera evento y
        // quien llama lo verá (sigue existiendo).
        // SAFETY: `name` termina en NUL y vive durante la llamada.
        let Ok(item) = (unsafe {
            SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(name.as_ptr()), None)
        }) else {
            continue;
        };
        // SAFETY: `op.op` es una interfaz viva e `item` también.
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
    /// Después de cada trozo: cada elemento con el `HRESULT` que notificó la shell (`None` = sin aviso).
    Done(&'a [(&'a BinItem, Option<i32>)]),
}

/// Purga (borra definitivamente) SOLO los elementos de la lista, PASANDO POR LA CARPETA SHELL de la Papelera: se
/// enumera la Papelera y se llama a `IFileOperation::DeleteItem` sobre los `IShellItem` enumerados, de modo que la shell
/// elimina a la vez el `$R` (los datos) y el `$I` (sus metadatos). Nunca por la ruta `$R`: eso dejaba el `$I` huérfano.
/// Quien llama pasa una lista enumerada al confirmar y los elementos se casan por su `id` estable: lo que haya entrado
/// después no se toca. Antes de tocar un elemento se comprueba que su `$R` y su `$I` siguen teniendo la identidad que
/// tenían al enumerar (M3). La cancelación se mira entre trozos de 500.
pub fn purge_items(items: &[BinItem], cancel: &AtomicBool) -> Result<BatchOutcome> {
    purge_items_flags(items, cancel, purge_flags(), &mut |_| {})
}

/// Como [`purge_items`], pero avisa de cada paso (ver [`PurgeStep`]) para dejarlo en el registro.
pub fn purge_items_observed(
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
    // SAFETY: COM está inicializado en este hilo; todas las interfaces y cadenas son propias de esta función y se
    // liberan con el RAII de `windows` o con `take_pwstr`.
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
    for (idx, chunk) in found.chunks(500).enumerate() {
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
        if staged.is_empty() {
            continue;
        }
        // La INTENCIÓN antes de entregar nada a la shell: si quien observa no pudo dejarla escrita, cancela y este trozo no
        // se entrega.
        let chunk_items: Vec<&BinItem> = staged.iter().map(|(_, i, _)| *i).collect();
        obs(PurgeStep::Begin(&chunk_items));
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // SOLO PRUEBAS: la ventana entre las guardas y la shell (un gancho puede cambiar un `$R` justo aquí).
        #[cfg(test)]
        for (_, it, _) in &staged {
            crate::engine::test_hooks::before_shell(Path::new(&it.id));
        }
        // A-M4: el receptor lleva una guarda con las identidades del `$R` y del `$I` de cada elemento: `PreDeleteItem` las
        // vuelve a comprobar por ruta (sin seguir enlaces) justo antes de que la shell borre cada uno, y veta lo que ya no
        // es lo capturado (`E_ABORT`: lo que quede del lote no se procesa).
        let op = Operation::new(flags, Some(Guard::purge(&chunk_items)))?;
        let mut added = 0usize;
        for (it, _, _) in &staged {
            // SAFETY: `op.op` y `it` son interfaces vivas.
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
        let vetoed_ids: Vec<&OsString> = r.vetoed.iter().map(|v| &v.item).collect();
        let vetoed_items: Vec<&BinItem> = chunk_items
            .iter()
            .copied()
            .filter(|i| vetoed_ids.contains(&&i.id))
            .collect();
        if !vetoed_items.is_empty() {
            obs(PurgeStep::Vetoed(&vetoed_items));
        }
        let done: Vec<(&BinItem, Option<i32>)> = chunk_items
            .iter()
            .filter(|i| !vetoed_ids.contains(&&i.id) && !replaced_ids.contains(&i.id))
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
            let rest: Vec<(&BinItem, Option<i32>)> = found[((idx + 1) * 500).min(found.len())..]
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

/// Abre el handle de guarda del `$R` de un elemento de la lista (ver `purge_items_flags`) y verifica que es el objeto
/// capturado al enumerar y que no está ya pendiente de borrar. `None` si no se puede (cambió, no se abre...): no se toca.
fn open_bin_guard(item: &BinItem) -> Option<sys::Handle> {
    let want = item.data_id?;
    let h = sys::open_path_for_rename(Path::new(&item.id)).ok()?;
    let m = sys::meta(&h).ok()?;
    (m.identity == want && !m.delete_pending).then_some(h)
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
    //! Ayudas de las pruebas que tocan la Papelera REAL del usuario: SOLO con elementos propios (filtrados por el uuid
    //! de la prueba) y con una guarda RAII que los purga pase lo que pase. NUNCA se vacía la Papelera ni se tocan
    //! elementos ajenos; las pruebas comparan el conjunto de ids antes y después.
    use super::*;
    use std::collections::HashSet;

    /// ¿Es de esta prueba? El `original_parent` cuelga de `%TEMP%` (forma larga canónica) y su primer componente
    /// siguiente es `tesela-test-<uuid>` (con el uuid completo); o es `%TEMP%` mismo y el nombre empieza así.
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

    /// Variable de entorno que activa las pruebas con la Papelera REAL del usuario (`TESELA_REAL_RECYCLE_TESTS=1`).
    pub const REAL_RECYCLE_VAR: &str = "TESELA_REAL_RECYCLE_TESTS";

    /// ¿Está definida `TESELA_REAL_RECYCLE_TESTS=1`? (Sin imprimir nada.)
    pub fn real_recycle_requested() -> bool {
        std::env::var(REAL_RECYCLE_VAR).is_ok_and(|v| v == "1")
    }

    /// ¿Están activadas las pruebas con la Papelera real? Solo con `TESELA_REAL_RECYCLE_TESTS=1`. El CI de GitHub
    /// (`windows-latest`, Windows Server sin sesión interactiva) no tiene una Papelera de usuario fiable: la shell la
    /// lista pero no devuelve lo que se envía a ella, y esas pruebas fallaban allí por el entorno, no por el código. En
    /// un equipo de escritorio la suite completa se lanza con la variable (ver el README). Sin ella, estas pruebas se
    /// saltan con un mensaje; las aserciones NO se han debilitado.
    pub fn real_recycle_enabled() -> bool {
        if real_recycle_requested() {
            return true;
        }
        eprintln!(
            "Papelera real desactivada: defina {REAL_RECYCLE_VAR}=1 para ejecutar esta prueba"
        );
        false
    }

    /// ¿Se puede probar la Papelera en este equipo? Hace falta que estén activadas las pruebas con la Papelera real
    /// ([`real_recycle_enabled`]), que la shell la liste y que el volumen del `%TEMP%` la tenga activada (en un equipo
    /// sin Papelera, o con "no mover los archivos a la Papelera", las pruebas que envían a ella se saltan en vez de
    /// fallar por algo del entorno).
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

    pub fn ids() -> HashSet<OsString> {
        list_bin()
            .expect("lista la Papelera")
            .into_iter()
            .map(|i| i.id)
            .collect()
    }

    /// Al soltarla (también en un pánico) purga SOLO los elementos propios de la prueba.
    pub struct BinGuard {
        pub uuid: String,
    }

    impl Drop for BinGuard {
        fn drop(&mut self) {
            if let Ok(items) = list_bin() {
                let mine: Vec<BinItem> = items
                    .into_iter()
                    .filter(|i| is_mine(i, &self.uuid))
                    .collect();
                let _ = purge_items(&mine, &AtomicBool::new(false));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::bintest::*;
    use super::*;
    use crate::paths::PathKey;
    use crate::testutil::{Sandbox, bin_lock, sparse_file};
    use std::collections::HashSet;

    /// Los elementos a enviar con la identidad que tienen ahora (lo "aprobado").
    fn send_items(paths: &[PathBuf]) -> Vec<SendItem> {
        paths.iter().map(|p| SendItem::capture(p)).collect()
    }

    /// Identidad (volumen + FileId) de lo que hay en `p`.
    fn identity_of(p: &Path) -> crate::sys::FileIdentity {
        let h = crate::sys::open_for_query(p).unwrap();
        crate::sys::meta(&h).unwrap().identity
    }

    /// El `$I` (metadatos) que acompaña al `$R` (datos) de un elemento de la Papelera: `$R9XYZ.txt` -> `$I9XYZ.txt`.
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
        // La shell avisa de cada uno con el resultado y la ruta real del elemento en la Papelera, y esa ruta tiene la
        // MISMA identidad que lo que se envió (el renombrado dentro del volumen conserva el FileId).
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
        // Los ajenos no se tocan: los ids de fuera de los míos son los de antes.
        let foreign: HashSet<OsString> = items
            .iter()
            .filter(|i| !is_mine(i, &sb.uuid()))
            .map(|i| i.id.clone())
            .collect();
        assert_eq!(foreign, before);
        // Cada elemento tiene su `$I` (metadatos) además de su `$R` (datos).
        let metas: Vec<PathBuf> = mine.iter().map(meta_file_of).collect();
        let datas: Vec<PathBuf> = mine.iter().map(|i| PathBuf::from(&i.id)).collect();
        assert!(metas.iter().all(|m| m.exists()), "{metas:?}");
        assert!(datas.iter().all(|d| d.exists()), "{datas:?}");
        // Purga SOLO la lista enumerada, por la carpeta shell de la Papelera: se van los DOS archivos de cada elemento
        // (nunca queda un `$I` huérfano) y se casa por `id`, así que lo ajeno no se toca.
        let r = purge_items(&mine, &AtomicBool::new(false)).unwrap();
        assert!(!r.any_aborted && r.error.is_none());
        // La shell avisa también de lo que hay DENTRO de una carpeta: se cuenta lo de la lista, casado por `id`.
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

    /// Un `$I` de versión 2 con el tamaño, el instante de borrado (FILETIME) y la ruta original dados.
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

    /// FILETIME de un instante Unix.
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
        // El instante del borrado sale del desplazamiento 16.
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
        // Basura, versiones desconocidas, truncados y rutas vacías.
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

    /// Un "elemento de la Papelera" SINTÉTICO en una carpeta de la prueba (no la real): `$R<token>.txt` ya no existe y el
    /// `$I<token>.txt` hermano tiene el contenido dado. Devuelve el elemento tal como lo habría enumerado `list_bin`
    /// (identidades del `$R` y del `$I`), y las rutas.
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
        // El `$R` ya fue purgado por la shell.
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

    /// M3: el `$I` huérfano solo se quita si TODO cuadra con lo aprobado: con archivos sintéticos (la función toma las
    /// rutas), cada discrepancia deja el `$I` intacto.
    #[test]
    fn el_i_huerfano_solo_se_quita_si_todo_cuadra_con_lo_aprobado() {
        let sb = Sandbox::new();
        // El caso bueno: se quita (y solo el `$I`).
        let o = orphan(&sb, "GOOD01", &good_meta(&sb));
        assert!(remove_orphan_meta_at(&o.item, &o.data, &o.meta));
        assert!(!o.meta.exists());
        // La shell oculta la extensión (`HideFileExt`): el nombre aprobado es `uno` y sigue valiendo.
        let mut o = orphan(&sb, "HIDEXT", &good_meta(&sb));
        o.item.name = OsString::from("uno");
        assert!(remove_orphan_meta_at(&o.item, &o.data, &o.meta));
        // En las carpetas la shell no da tamaño (0): no se compara.
        let mut o = orphan(&sb, "DIR001", &good_meta(&sb));
        o.item.size = 0;
        assert!(remove_orphan_meta_at(&o.item, &o.data, &o.meta));

        // Cada discrepancia: el `$I` se queda.
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
        // Una extensión distinta de la del `$R`.
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
        // Un `$I` cuyo contenido no es un `$I`.
        let o = orphan(&sb, "JUNK01", b"esto no es un $I");
        assert!(!remove_orphan_meta_at(&o.item, &o.data, &o.meta));
        assert!(o.meta.exists());
    }

    /// Fuera de `<unidad>:\$Recycle.Bin\<mi SID>\` el `$I` nunca se toca, aunque el nombre y el contenido (e incluso las
    /// identidades y los metadatos) parezcan los de un elemento de la Papelera.
    #[test]
    fn el_i_huerfano_fuera_de_la_papelera_del_usuario_nunca_se_toca() {
        let sb = Sandbox::new();
        let o = orphan(&sb, "LOC001", &good_meta(&sb));
        // El contenido y las identidades cuadran (se quitaría con `remove_orphan_meta_at`), pero la ruta no es la de la
        // Papelera del usuario.
        assert!(!remove_orphan_meta(&o.item));
        assert!(o.meta.exists());
        // Un id que no es un `$R`.
        let not_r = BinItem {
            id: o.data.with_file_name("otra.txt").into_os_string(),
            ..o.item.clone()
        };
        assert!(!remove_orphan_meta(&not_r));
        assert!(o.meta.exists());
    }

    /// M3: si el `$R` existe todavía, o si preguntar por él da un error que NO es "no encontrado" (acceso, nombre no
    /// válido...), el `$I` NO se borra: un error es "se desconoce", no "no existe".
    #[test]
    fn un_error_al_consultar_el_r_no_cuenta_como_que_no_existe() {
        for c in [2, 3] {
            assert_eq!(classify_data_error(c), DataState::Gone, "{c}");
        }
        for c in [1, 5, 32, 87, 123, 1920] {
            assert_eq!(classify_data_error(c), DataState::Unknown, "{c}");
        }
        let sb = Sandbox::new();
        // Existe -> Exists.
        let o = orphan(&sb, "STATE1", &good_meta(&sb));
        std::fs::write(&o.data, b"x").unwrap();
        assert_eq!(data_state(&o.data), DataState::Exists);
        assert!(!remove_orphan_meta_at(&o.item, &o.data, &o.meta));
        assert!(o.meta.exists());
        std::fs::remove_file(&o.data).unwrap();
        assert_eq!(data_state(&o.data), DataState::Gone);
        // Una ruta que da un error distinto de "no encontrado" (nombre no válido): desconocido, el `$I` se queda.
        let invalid = o.data.with_file_name("$RSTATE1*.txt");
        assert_eq!(data_state(&invalid), DataState::Unknown);
        assert!(!remove_orphan_meta_at(&o.item, &invalid, &o.meta));
        assert!(o.meta.exists());
        // Y con el `$R` realmente ausente, sí.
        assert!(remove_orphan_meta_at(&o.item, &o.data, &o.meta));
    }

    /// M3: al purgar, el `$R` y el `$I` tienen que seguir siendo los mismos objetos que al enumerar.
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
        // H6.9: sin FileId (FAT, exFAT) no es "cambió": motivo propio.
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
        // Sin identidades capturadas: no se puede verificar.
        assert!(!bin_item_unchanged(&BinItem {
            data_id: None,
            ..item.clone()
        }));
        assert!(!bin_item_unchanged(&BinItem {
            meta_id: None,
            ..item.clone()
        }));
        // El `$I` sustituido por otro archivo con el mismo nombre (otra identidad).
        std::fs::remove_file(&meta).unwrap();
        std::fs::write(&meta, b"meta").unwrap();
        assert!(!bin_item_unchanged(&item), "el $I ya no es el mismo objeto");
        // El `$R` sustituido.
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
        // Y si desaparece, tampoco.
        std::fs::remove_file(&data).unwrap();
        assert!(!bin_item_unchanged(&item2));
    }

    /// M3 con la shell REAL: un elemento PROPIO cuya identidad capturada ya no cuadra se SALTA al purgar (no se toca y
    /// consta como cambiado); con la identidad buena se purga. La Papelera de después es la de antes.
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
        // Identidad del `$R` falseada: se salta.
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
        // Lo mismo con el `$I`.
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
        // Con las identidades buenas, se purga y no queda ni el `$I` huérfano.
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
        // Cancelado antes de empezar: no se purga nada.
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
        // Solo dos de los tres: el tercero sigue.
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

    /// Borra un archivo al soltarse (el `$R` apartado de una prueba, que no es de ningún elemento de la lista).
    struct RemoveOnDrop(PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Un elemento sintético de la Papelera (`$R` y `$I` en una carpeta del sandbox) con las identidades capturadas ahora.
    fn synthetic_bin_item(sb: &Sandbox, token: &str, data: &[u8]) -> BinItem {
        let dir = sb.mkdir("bin/S-synthetic");
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

    /// El receptor de una PURGA con la guarda de estos elementos (A-M4), sin COM.
    fn purge_core(items: &[&BinItem]) -> SinkCore {
        SinkCore {
            events: Arc::new(Mutex::new(Vec::new())),
            state: Arc::new(SinkState::default()),
            guard: Some(Guard::purge(items)),
        }
    }

    /// A-M4 (sin COM): la ventana de la purga. Los elementos se verifican al enumerar y la shell actúa más tarde; antes, el
    /// receptor de una purga no llevaba guarda y `PreDeleteItem` dejaba pasar cualquier cosa, incluido un `$R` sustituido
    /// después de la comprobación. Ahora `PreDeleteItem` vuelve a comprobar por ruta el `$R` y el `$I` contra lo capturado:
    /// si no coinciden (o no se pueden abrir, o no es lo aprobado) veta con `E_ABORT`, lo registra y niega todo lo posterior.
    /// (Un mutante que haga que `PreDeleteItem` devuelva siempre `true` falla aquí.)
    #[test]
    fn la_purga_veta_un_r_o_un_i_sustituido_despues_de_enumerar() {
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"datos a");
        let b = synthetic_bin_item(&sb, "BBBB02", b"datos b");
        let c = synthetic_bin_item(&sb, "CCCC03", b"datos c");
        let os = |i: &BinItem| i.id.clone();
        // Todo igual que al enumerar: pasa.
        let core = purge_core(&[&a, &b, &c]);
        assert!(core.pre_delete(os(&a)));
        // El `$R` de `b` se sustituye por otro archivo con el mismo nombre (otro FileId) DESPUÉS de la comprobación.
        std::fs::remove_file(PathBuf::from(&b.id)).unwrap();
        std::fs::write(PathBuf::from(&b.id), b"datos b").unwrap();
        assert!(!core.pre_delete(os(&b)), "el sustituto se veta");
        let vetoes = core.state.vetoes.lock().unwrap().clone();
        assert_eq!(vetoes.len(), 1);
        assert_eq!(vetoes[0].kind, VetoKind::IdentityMismatch);
        assert_eq!(vetoes[0].item, b.id);
        assert!(core.state.tripped.load(Ordering::SeqCst));
        // Cortado el lote: lo siguiente se niega aunque no haya cambiado.
        assert!(!core.pre_delete(os(&c)));
        // Lo mismo con el `$I` sustituido, en un lote nuevo (otro `tripped`).
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
        // El `$R` desaparece (se abre mal) y un elemento que no estaba en la lista: se vetan con su motivo.
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

    /// A-M4: al PURGAR, el borrado definitivo es lo esperado: una guarda de purga no corta el lote por eso (a diferencia de la
    /// de envío), y registra el evento.
    #[test]
    fn una_guarda_de_purga_no_corta_por_el_borrado_definitivo_esperado() {
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"x");
        let core = purge_core(&[&a]);
        assert!(!core.guard.as_ref().unwrap().sends());
        assert!(core.pre_delete(a.id.clone()));
        assert!(!core.post_delete(a.id.clone(), 0, BinState::Deleted));
        assert!(!core.state.tripped.load(Ordering::SeqCst));
        assert_eq!(core.events.lock().unwrap().len(), 1);
        // Y una guarda de envío sí corta (la de siempre).
        let it = send_item(&sb, "s/uno.txt", 10);
        let envio = core_for(std::slice::from_ref(&it), true);
        assert!(envio.guard.as_ref().unwrap().sends());
        assert!(envio.post_delete(it.path.as_os_str().to_os_string(), 0, BinState::Deleted));
    }

    /// A-M4 con la shell REAL (solo elementos propios, tras `TESELA_REAL_RECYCLE_TESTS=1`): tras enumerar y verificar, se
    /// sustituye el `$R` de `b.txt` (en la ventana, con un gancho de pruebas). El receptor lo veta: el sustituto NO se purga y
    /// consta el veto. Se MIDE (y se imprime) qué hace la shell con el resto del lote.
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
        // El PRIMERO en el orden de enumeración (el orden en que la shell los va a procesar): así se ve qué hace con los
        // que vienen detrás del vetado.
        let victim = mine[0].clone();
        let victim_r = PathBuf::from(&victim.id);
        let hook = crate::engine::test_hooks::install_before_shell(victim_r.clone(), move |p| {
            // Con la guarda abierta (comparte `DELETE`) el `$R` se puede apartar y poner otro con su nombre.
            std::fs::rename(p, p.with_extension("apartado")).unwrap();
            std::fs::write(p, b"sustituto").unwrap();
        });
        // Quita el `$R` apartado (dato propio de la prueba) pase lo que pase.
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
        // La shell avisa del vetado con un fallo (`E_ABORT`), nunca con un éxito: no se purgó.
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
        // Limpieza: sin el gancho (volvería a sustituir el `$R` del primero), se purga lo propio que quede con las
        // identidades de ahora.
        drop(hook);
        let rest: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        purge_items(&rest, &AtomicBool::new(false)).unwrap();
        assert_eq!(ids(), before);
    }

    /// A-M4 (sin COM): un elemento que es una CARPETA llega a `PreDeleteItem` hijo a hijo (medido con la shell real). Los hijos
    /// de un `$R` aprobado pasan por la identidad de su `$R` y de su `$I`; si el `$R` se sustituye, también se vetan. Al
    /// ENVIAR no vale: solo la ruta exacta.
    #[test]
    fn los_hijos_de_un_r_aprobado_se_comprueban_por_la_identidad_de_su_r() {
        let sb = Sandbox::new();
        let dir_item = {
            let it = synthetic_bin_item(&sb, "DDDD04", b"x");
            // El `$R` de este elemento es una CARPETA con hijos.
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
        // Algo que NO cuelga del `$R` aprobado: no.
        assert!(!core.pre_delete(sb.path("bin/S-synthetic/otro/hijo.txt").into_os_string()));
        // El `$R` se sustituye por otra carpeta: los hijos de la nueva se vetan.
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
        // Con una guarda de ENVÍO, un hijo no pasa aunque la ruta cuelgue de lo aprobado.
        let envio = Guard::new(&[SendItem::capture(Path::new(&dir_item.id))]);
        assert_eq!(
            envio.check(&child.into_os_string()),
            Err(VetoKind::NotApproved)
        );
    }

    /// A-M4 (sin COM): la guarda de un `$R` solo se abre si es el objeto capturado y no está ya pendiente de borrar; y demuestra
    /// el borrado solo cuando lo está. `replaced_after` separa lo que la shell dio por purgado de lo que de verdad lo fue.
    #[test]
    fn la_guarda_de_la_purga_demuestra_si_se_borro_el_objeto_aprobado() {
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"datos a");
        let b = synthetic_bin_item(&sb, "BBBB02", b"datos b");
        // Intacto: se abre y no demuestra nada.
        let ga = open_bin_guard(&a).expect("el `$R` capturado se abre");
        let gb = open_bin_guard(&b).expect("el `$R` capturado se abre");
        assert!(!guard_proves_deleted(&ga));
        // Sin identidad capturada, o con otra: no se abre.
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
        // Pendiente de borrar: ya no se abre otra guarda (el nombre da acceso denegado) y la primera lo demuestra.
        crate::sys::delete_legacy_for_test(Path::new(&a.id)).unwrap();
        assert!(guard_proves_deleted(&ga));
        assert!(open_bin_guard(&a).is_none());
        // La shell dio por purgados a los dos con éxito; solo `a` se eliminó de verdad. `b` sigue vivo con su nombre: lo que
        // se borró con ese nombre (en la realidad, un sustituto) no es el objeto aprobado.
        let ev = |i: &BinItem, hr: i32| DeleteEvent {
            item: i.id.clone(),
            hr,
            state: BinState::Deleted,
        };
        let events = [ev(&a, 0), ev(&b, 0)];
        let guards = [(&a.id, &ga), (&b.id, &gb)];
        assert_eq!(replaced_after(&events, &guards), vec![b.id.clone()]);
        // Un fallo no es "dado por purgado": no se mira.
        let fail = [ev(&b, 0x8000_4004u32 as i32)];
        assert!(replaced_after(&fail, &guards).is_empty());
    }

    /// A-M4 con la shell REAL (solo elementos propios, tras `TESELA_REAL_RECYCLE_TESTS=1`): con la guarda del receptor y los
    /// handles de guarda abiertos, la purga sigue funcionando para un archivo y para una CARPETA con hijos (que la shell
    /// notifica hijo a hijo): sin vetos, sin cortes, sin sustituidos, y ni el `$R` ni el `$I` quedan. Se mide y se imprime.
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

    fn send_item(sb: &Sandbox, rel: &str, size: usize) -> SendItem {
        SendItem::capture(&sb.write(rel, size))
    }

    /// H3 (sin COM): la comprobación de `PreDeleteItem` por ruta. Misma identidad pasa; otra identidad, un archivo que ya no
    /// está, un hijo, un alias (otro nombre del mismo objeto) y cualquier ruta no aprobada se vetan.
    #[test]
    fn el_guard_solo_deja_pasar_la_ruta_aprobada_con_su_identidad() {
        let sb = Sandbox::new();
        let a = send_item(&sb, "g/a.txt", 100);
        let dir = SendItem::capture(&sb.mkdir("g/carpeta"));
        sb.write("g/carpeta/hijo.txt", 50);
        let guard = Guard::new(&[a.clone(), dir.clone()]);
        let os = |p: &Path| p.as_os_str().to_os_string();
        // La misma ruta con la misma identidad: pasa (también con otra capitalización).
        assert_eq!(guard.check(&os(&a.path)), Ok(()));
        assert_eq!(guard.check(&os(&dir.path)), Ok(()));
        let upper = PathBuf::from(a.path.to_string_lossy().to_uppercase());
        assert_eq!(guard.check(&os(&upper)), Ok(()));
        // Un hijo de un elemento aprobado: se veta (la shell real emite un solo evento por carpeta).
        assert_eq!(
            guard.check(&os(&dir.path.join("hijo.txt"))),
            Err(VetoKind::NotApproved)
        );
        // Algo que no estaba en la lista.
        let otro = sb.write("g/otro.txt", 10);
        assert_eq!(guard.check(&os(&otro)), Err(VetoKind::NotApproved));
        // Un alias del objeto aprobado (enlace duro con OTRO nombre): misma identidad pero no es la ruta aprobada.
        sb.hard_link("g/a.txt", "g/alias.txt");
        assert_eq!(
            guard.check(&os(&sb.path("g/alias.txt"))),
            Err(VetoKind::NotApproved)
        );
        // El aprobado se aparta y otro objeto ocupa su nombre: identidad distinta.
        std::fs::remove_file(sb.path("g/alias.txt")).unwrap();
        std::fs::rename(sb.path("g/a.txt"), sb.path("g/a-original.txt")).unwrap();
        sb.write("g/a.txt", 100);
        assert_eq!(guard.check(&os(&a.path)), Err(VetoKind::IdentityMismatch));
        // Un enlace duro a OTRO objeto en el nombre aprobado: identidad distinta.
        std::fs::remove_file(sb.path("g/a.txt")).unwrap();
        sb.write("g/ajeno.txt", 100);
        sb.hard_link("g/ajeno.txt", "g/a.txt");
        assert_eq!(guard.check(&os(&a.path)), Err(VetoKind::IdentityMismatch));
        // Un enlace duro al MISMO objeto aprobado en su nombre: la identidad coincide (lo que lo detecta es m1, que
        // compara `NumberOfLinks`, no esta comprobación).
        std::fs::remove_file(sb.path("g/a.txt")).unwrap();
        sb.hard_link("g/a-original.txt", "g/a.txt");
        assert_eq!(guard.check(&os(&a.path)), Ok(()));
        // El nombre aprobado ya no existe: no se puede comprobar, se veta.
        std::fs::remove_file(sb.path("g/a.txt")).unwrap();
        assert!(matches!(
            guard.check(&os(&a.path)),
            Err(VetoKind::OpenFailed(2 | 3))
        ));
    }

    /// H3 (sin COM): el estado según `psiNewlyCreated` y la decisión de cortar el lote tras `PostDeleteItem`.
    #[test]
    fn la_decision_de_cortar_el_lote_es_pura_y_cubre_todos_los_casos() {
        let p = PathBuf::from(r"C:\$Recycle.Bin\S\$R1");
        assert_eq!(bin_state(None), BinState::Deleted);
        assert_eq!(bin_state(Some(None)), BinState::InBinUnknownPath);
        assert_eq!(bin_state(Some(Some(p.clone()))), BinState::InBin(p.clone()));
        let ok = 0;
        let fail = 0x8007_0005u32 as i32;
        // Al ENVIAR con éxito: eliminado o sin ruta cortan; en la Papelera, no.
        assert!(post_delete_aborts(true, ok, &BinState::Deleted));
        assert!(post_delete_aborts(true, ok, &BinState::InBinUnknownPath));
        assert!(post_delete_aborts(true, 0x0027_0003, &BinState::Deleted));
        assert!(!post_delete_aborts(true, ok, &BinState::InBin(p)));
        // Si la shell falló, no se corta por esto (el fallo ya consta como error del elemento).
        assert!(!post_delete_aborts(true, fail, &BinState::Deleted));
        assert!(!post_delete_aborts(true, fail, &BinState::InBinUnknownPath));
        // Al PURGAR el borrado definitivo es lo esperado: nunca corta.
        assert!(!post_delete_aborts(false, ok, &BinState::Deleted));
        assert!(!post_delete_aborts(false, ok, &BinState::InBinUnknownPath));
    }

    fn core_for(items: &[SendItem], sending: bool) -> SinkCore {
        SinkCore {
            events: Arc::new(Mutex::new(Vec::new())),
            state: Arc::new(SinkState::default()),
            guard: sending.then(|| Guard::new(items)),
        }
    }

    /// H3 (sin COM): el receptor registra el evento ANTES de decidir cortar, y una vez cortado niega todo lo posterior.
    #[test]
    fn el_receptor_registra_primero_corta_despues_y_niega_lo_posterior() {
        let sb = Sandbox::new();
        let a = send_item(&sb, "r/a.txt", 10);
        let b = send_item(&sb, "r/b.txt", 10);
        let core = core_for(&[a.clone(), b.clone()], true);
        let os = |p: &Path| p.as_os_str().to_os_string();
        assert!(core.pre_delete(os(&a.path)));
        // Eliminado definitivamente con éxito: el evento consta y se pide cortar.
        assert!(core.post_delete(os(&a.path), 0, BinState::Deleted));
        {
            let ev = core.events.lock().unwrap();
            assert_eq!(ev.len(), 1);
            assert_eq!((ev[0].hr, &ev[0].state), (0, &BinState::Deleted));
        }
        assert!(core.state.tripped.load(Ordering::SeqCst));
        // Cortado: el siguiente `PreDeleteItem` se niega aunque sea lo aprobado.
        assert!(!core.pre_delete(os(&b.path)));
        // Un lote en el que todo va a la Papelera no corta y registra cada evento.
        let ok = core_for(std::slice::from_ref(&a), true);
        let bin = PathBuf::from(r"C:\$Recycle.Bin\S\$R1");
        assert!(!ok.post_delete(os(&a.path), 0, BinState::InBin(bin)));
        assert!(!ok.state.tripped.load(Ordering::SeqCst));
        // Al purgar (sin guarda) nada corta ni veta.
        let purge = core_for(&[], false);
        assert!(purge.pre_delete(os(&a.path)));
        assert!(!purge.post_delete(os(&a.path), 0, BinState::Deleted));
        assert!(!purge.state.tripped.load(Ordering::SeqCst));
        assert_eq!(purge.events.lock().unwrap().len(), 1);
    }

    /// H3 (sin COM): un veto registra su motivo, corta y niega lo siguiente.
    #[test]
    fn un_veto_del_receptor_registra_el_motivo_y_corta_el_lote() {
        let sb = Sandbox::new();
        let a = send_item(&sb, "v/a.txt", 10);
        let b = send_item(&sb, "v/b.txt", 10);
        let core = core_for(&[a.clone(), b.clone()], true);
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

    // ---- A-m1: los CALLBACKS COM del receptor, llamados directamente (sin una operación de la Papelera real) ---------------------
    //
    // Hasta ahora solo se probaba la lógica pura (`SinkCore`): el mutante `PreDeleteItem -> Ok(())` y el `PostDeleteItem -> Ok(())`
    // sobrevivían, porque nada comprobaba que los métodos de `IFileOperationProgressSink` llamaran a esa lógica y tradujeran
    // su resultado a `E_ABORT`. Aquí se crea el receptor como interfaz COM y se llama a sus métodos con `IShellItem`s
    // creados con `SHCreateItemFromParsingName` sobre archivos propios en el sandbox. Hace falta COM inicializado en el hilo
    // de la prueba, pero NO una operación de la shell ni la Papelera: corre en el CI.

    /// Un `IShellItem` de una ruta del sandbox.
    fn shell_item(path: &Path) -> IShellItem {
        let name = parsing_name(path);
        // SAFETY: `name` termina en NUL y vive durante la llamada; COM está inicializado por quien llama.
        unsafe { SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(name.as_ptr()), None) }
            .expect("crea el IShellItem")
    }

    /// El receptor COM con su estado compartido, para inspeccionarlo después.
    fn com_sink(guard: Option<Guard>) -> (IFileOperationProgressSink, Events, Arc<SinkState>) {
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

    /// A-m1: `PreDeleteItem` por la interfaz COM. Lo aprobado pasa; un sustituto en la ruta se veta con `E_ABORT` (y consta el
    /// veto); una vez cortado el lote, lo siguiente se niega aunque sea lo aprobado. (Un mutante `PreDeleteItem -> Ok(())` falla.)
    #[test]
    fn el_callback_pre_delete_item_veta_con_e_abort_lo_que_no_es_lo_aprobado() {
        let _com = Com::init().unwrap();
        let sb = Sandbox::new();
        let a = send_item(&sb, "c/a.txt", 100);
        let b = send_item(&sb, "c/b.txt", 100);
        let (sink, _events, state) = com_sink(Some(Guard::new(&[a.clone(), b.clone()])));
        // SAFETY: interfaz COM viva y en su hilo; `IShellItem` vivos.
        unsafe {
            assert!(sink.PreDeleteItem(0, &shell_item(&a.path)).is_ok());
            // Después de comprobar, `a.txt` se sustituye por otro archivo con el mismo nombre.
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
            // El lote está cortado: `b.txt` es lo aprobado y aun así se niega.
            assert!(is_abort(sink.PreDeleteItem(0, &shell_item(&b.path))));
        }
    }

    /// A-m1: `PostDeleteItem` por la interfaz COM: registra el evento (con su `HRESULT` y el estado según `psiNewlyCreated`) y
    /// devuelve `E_ABORT` solo cuando la shell eliminó definitivamente con éxito (nulo) o no dio la ruta. (Un mutante
    /// `PostDeleteItem -> Ok(())` falla: no registraría nada ni cortaría.)
    #[test]
    fn el_callback_post_delete_item_registra_el_evento_y_corta_tras_un_borrado_definitivo() {
        let _com = Com::init().unwrap();
        let sb = Sandbox::new();
        let a = send_item(&sb, "d/a.txt", 100);
        let b = send_item(&sb, "d/b.txt", 100);
        let dest = sb.write("d/bin/R1.txt", 10);
        let (sink, events, state) = com_sink(Some(Guard::new(&[a.clone(), b.clone()])));
        // SAFETY: interfaz COM viva y en su hilo; `IShellItem` vivos.
        unsafe {
            // Éxito con elemento en la Papelera: se registra y NO se corta.
            let r = sink.PostDeleteItem(0, &shell_item(&a.path), HRESULT(0), &shell_item(&dest));
            assert!(r.is_ok());
            assert!(!state.tripped.load(Ordering::SeqCst));
            // Éxito con `psiNewlyCreated` nulo: la shell lo eliminó definitivamente -> se corta con `E_ABORT`.
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
        // Con un FALLO de la shell (y nulo) no se corta, pero consta.
        let (sink, events, state) = com_sink(Some(Guard::new(std::slice::from_ref(&a))));
        // SAFETY: igual que arriba.
        unsafe {
            let fail = HRESULT(0x8007_0005u32 as i32);
            let r = sink.PostDeleteItem(0, &shell_item(&a.path), fail, None::<&IShellItem>);
            assert!(r.is_ok(), "un fallo no corta por esto");
            assert!(!state.tripped.load(Ordering::SeqCst));
        }
        assert_eq!(events.lock().unwrap()[0].hr, 0x8007_0005u32 as i32);
        // Al PURGAR, el borrado definitivo es lo esperado: se registra y no se corta.
        let (sink, events, state) = com_sink(None);
        // SAFETY: igual que arriba.
        unsafe {
            let r = sink.PostDeleteItem(0, &shell_item(&a.path), HRESULT(0), None::<&IShellItem>);
            assert!(r.is_ok());
            assert!(sink.PreDeleteItem(0, &shell_item(&a.path)).is_ok());
        }
        assert_eq!(events.lock().unwrap().len(), 1);
        assert!(!state.tripped.load(Ordering::SeqCst));
    }

    /// A-m1 + A-M4: la guarda de una PURGA por la interfaz COM, con elementos sintéticos de la Papelera: el `PreDeleteItem` que
    /// recibe el nombre de análisis del elemento comprueba el `$R` y el `$I` y veta con `E_ABORT` el `$R` sustituido.
    #[test]
    fn el_callback_pre_delete_item_de_una_purga_veta_el_r_sustituido() {
        let _com = Com::init().unwrap();
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"datos a");
        let b = synthetic_bin_item(&sb, "BBBB02", b"datos b");
        let (sink, _events, state) = com_sink(Some(Guard::purge(&[&a, &b])));
        // SAFETY: interfaz COM viva y en su hilo; `IShellItem` vivos.
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

    /// S1: las banderas de envío y de purga llevan `FOF_NO_CONNECTED_ELEMENTS` (aprobar `pagina.html` no puede arrastrar
    /// `pagina_files\`), y el envío conserva las que lo hacen recuperable y sin diálogos.
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
        // Y nunca la bandera que se cuelga con FOF_NO_UI.
        assert!(!has(send, FOF_WANTNUKEWARNING) && !has(purge, FOF_WANTNUKEWARNING));
    }

    /// S1 con la shell REAL: enviar `pagina.html` no arrastra su carpeta `pagina_files\`. Solo elementos propios.
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

    /// M2 con la shell REAL: un archivo disperso enorme (no cabe en ninguna Papelera) EN PRIMER LUGAR de un lote con dos
    /// archivos pequeños. La shell lo elimina definitivamente; el receptor registra el evento y devuelve `E_ABORT`, y los
    /// dos pequeños NO se procesan: siguen en su sitio y no están en la Papelera. Solo elementos propios.
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

    /// M1 con la shell REAL: se aprueba `b.txt`, y ANTES de entregarlo a la shell se renombra y se pone otro archivo con el
    /// mismo nombre (sustituto). `PreDeleteItem` compara la identidad, la encuentra distinta y devuelve `E_ABORT`: el
    /// sustituto NO llega a la Papelera, el lote se corta (el siguiente elemento tampoco se procesa) y consta el veto.
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
        // Tras "aprobar": el aprobado se aparta y otro archivo ocupa su nombre.
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
        // El sustituto sigue en su sitio, el original apartado también, y `c.txt` no se procesó.
        assert!(sb.path("s/b.txt").exists() && sb.path("s/b-original.txt").exists());
        assert!(
            sb.path("s/c.txt").exists(),
            "el resto del lote no se procesó"
        );
        // Lo único que pudo llegar a la Papelera es `a.txt` (se procesó antes del veto), y está verificado.
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

    /// Centinela: sin `TESELA_REAL_RECYCLE_TESTS=1` las pruebas que usan la Papelera real se saltan EN SILENCIO (pasan sin
    /// ejecutar nada). Esta prueba lo dice con una cifra: cuántas se han saltado (las llamadas a `bin_usable()` de este
    /// archivo y de `exec_tests.rs`). Cargo oculta la salida de las pruebas que pasan: se ve con `cargo test -- --nocapture`.
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
        // La shell cuenta los elementos de primer nivel igual que la enumeración (puede haber una diferencia si el
        // usuario vacía o llena la Papelera justo ahora).
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

    /// Qué pasó con un elemento tras `recycle_paths` (mirando el disco y la Papelera, sin el receptor).
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

    /// PRUEBA OBLIGATORIA (PLAN.md §3.7.4): `FOF_WANTNUKEWARNING` junto con `FOF_NO_UI` y un elemento que NO cabe en la
    /// Papelera (archivo disperso con un tamaño lógico mayor que el volumen entero: no ocupa nada).
    ///
    /// Hallazgos que fijan el diseño (medidos en Windows 11; la prueba exige los invariantes, no el desenlace exacto):
    /// 1. Con las banderas de la aplicación (`FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE`) la shell devuelve
    ///    ÉXITO (`any_aborted == false`) y el elemento DESAPARECE sin llegar a la Papelera: se elimina definitivamente en
    ///    silencio. Por eso la aplicación comprueba ANTES si cabe (límite real del volumen) y VERIFICA después cada
    ///    elemento con el receptor de eventos: `psiNewlyCreated` nulo = eliminado definitivamente. Esta prueba cruza lo que
    ///    dice el receptor con lo que se ve en el disco y en la Papelera (la semántica documentada tiene que cumplirse).
    /// 2. Con `FOF_WANTNUKEWARNING` añadida, la shell no avisa por el código de retorno: se queda esperando una
    ///    respuesta que `FOF_NO_UI` impide dar. Por eso NO se usa nunca (la variante solo existe en pruebas) y esta
    ///    parte se ejecuta en un PROCESO HIJO con plazo: si se cuelga se mata, sin dejar hilos ni diálogos huérfanos.
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

        // (1) Banderas de la aplicación: la llamada vuelve y el desenlace se distingue verificando.
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
        // El receptor dice la verdad: `Deleted` (psiNewlyCreated nulo) = desapareció sin llegar a la Papelera;
        // `InBin` = está en la Papelera; sin evento (o con error) = sigue en su sitio.
        let ev = out.events.first();
        match f {
            Fate::GoneNotInBin => {
                let ev = ev.expect("la shell avisa de lo que elimina");
                assert!(ev.succeeded() && ev.state == BinState::Deleted, "{ev:?}");
                // M2: el receptor registró el evento y CORTÓ el lote (devolvió E_ABORT tras `PostDeleteItem`).
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

        // (2) Con FOF_WANTNUKEWARNING, en un proceso hijo con plazo.
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
        // Limpieza explícita (la guarda lo repite): nada propio queda en la Papelera y la Papelera es la de antes.
        let mine: Vec<BinItem> = list_bin()
            .unwrap()
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect();
        purge_items(&mine, &AtomicBool::new(false)).unwrap();
        assert_eq!(ids(), before);
    }

    /// Cuerpo del proceso hijo de la prueba anterior (no se ejecuta solo: `#[ignore]` y exige la variable de entorno).
    #[test]
    #[ignore = "solo lo lanza wantnukewarning_con_no_ui_y_un_elemento_que_no_cabe"]
    fn hijo_nukewarning() {
        let Some(p) = std::env::var_os("TESELA_NUKE_CHILD_PATH") else {
            return;
        };
        let p = PathBuf::from(p);
        // Solo actúa sobre un archivo de la propia prueba, dentro de su carpeta temporal.
        let s = p.to_string_lossy().to_lowercase();
        assert!(s.contains("tesela-test-") && s.ends_with("enorme-nukewarning.bin"));
        let _ = recycle_paths(&send_items(&[p]), RecycleMode::WithNukeWarning);
    }
}
