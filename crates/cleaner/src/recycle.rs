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
//! * `PreDeleteItem` (A2-B1) vuelve además a medir, POR EL HANDLE DE GUARDA (sin reabrir la ruta), cada ARCHIVO que viaja en
//!   un lote con otros: si ocupa más de lo que se contó para el presupuesto de la operación, veta con `E_ABORT` (`Grew`). Un
//!   directorio nunca viaja con otros: va solo y se mide justo antes de entregarlo. RESIDUAL: lo que crece mientras la propia
//!   operación de la shell está en marcha (entre ese `PreDeleteItem` y el movimiento real).
//! * `PostDeleteItem` (M2) registra el evento PRIMERO y, si la shell eliminó definitivamente (`psiNewlyCreated` nulo) o no
//!   dijo dónde quedó, devuelve `E_ABORT`: el primer borrado inesperado ya no arrastra al resto del lote. (A2-M1) Un
//!   resultado de FALLO (`hrDelete < 0`) corta igual: puede haber una pérdida parcial detrás y no se puede garantizar que lo
//!   siguiente no sufra lo mismo. Solo al ENVIAR; al purgar la Papelera el borrado definitivo es lo esperado, cada elemento
//!   es independiente y un fallo no corta (no se cambió: la purga no tiene "pérdida parcial" que evitar).
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
    /// SOLO PARA PRUEBAS: captura la identidad actual de `path` (en producción la aprueba el ejecutor, no se lee aquí).
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
    /// El receptor de un ENVÍO con la guarda de estos elementos (la misma lógica que usa la shell real; las shells simuladas
    /// de las pruebas la llaman en vez de reimplementarla).
    #[cfg(test)]
    pub(crate) fn for_send(items: &[SendItem]) -> SinkCore {
        SinkCore {
            events: Arc::new(Mutex::new(Vec::new())),
            state: Arc::new(SinkState::default()),
            guard: Guard::new(items),
        }
    }

    /// Lo que el receptor registró: los eventos, los vetos y si cortó el lote.
    #[cfg(test)]
    pub(crate) fn finish(&self) -> (Vec<DeleteEvent>, Vec<Veto>, bool) {
        let events = std::mem::take(&mut *self.events.lock().unwrap_or_else(|e| e.into_inner()));
        let vetoes =
            std::mem::take(&mut *self.state.vetoes.lock().unwrap_or_else(|e| e.into_inner()));
        (events, vetoes, self.state.tripped.load(Ordering::SeqCst))
    }

    /// `PreDeleteItem`: `true` = la shell puede seguir con este elemento; `false` = el receptor debe devolver `E_ABORT`.
    pub(crate) fn pre_delete(&self, item: OsString) -> bool {
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
        let op = Operation::new(flags, Guard::purge(&chunk_items))?;
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

/// Elementos por trozo al purgar (y por vuelta de la cancelación).
const PURGE_CHUNK: usize = 500;

/// SOLO PRUEBAS: tamaño de trozo alternativo (0 = el normal), para probar con pocos elementos lo que ocurre con varios trozos.
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

    /// Fija el tamaño de trozo de la purga mientras viva (solo se usa con `bin_lock()`: las pruebas de la Papelera real van
    /// serializadas).
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

    /// ¿Tiene `name` la forma `tesela-test-<uuid>` (el nombre de un `Sandbox`: UUID v4 de 36 caracteres, 8-4-4-4-12 en
    /// hexadecimal)? Con `exact`, sin nada detrás; sin él, puede seguir otra cosa (un archivo suelto con ese prefijo).
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

    /// ¿Es un elemento de ALGUNA prueba (de cualquier ejecución)? Su carpeta de origen es hija directa de `temp` y se llama
    /// exactamente `tesela-test-<uuid>`, o cuelga de ella; o es `temp` mismo y el nombre del elemento empieza por
    /// `tesela-test-<uuid>`. Con la FORMA del uuid (R-6): un `tesela-test-algo` del usuario no cuenta.
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

    /// [`is_test_item_in`] con el `%TEMP%` canónico de este proceso.
    pub fn is_any_test_item(item: &BinItem) -> bool {
        let temp = dunce::canonicalize(std::env::temp_dir()).unwrap();
        is_test_item_in(item, &temp)
    }

    /// ¿Lo barre el barrido de restos? Solo un elemento de alguna prueba ([`is_test_item_in`]) borrado hace MÁS de 2 minutos
    /// (nunca lo de una prueba que corra a la vez en otro proceso).
    pub fn sweep_candidate(item: &BinItem, now: i64, temp: &Path) -> bool {
        is_test_item_in(item, temp) && now - item.time_deleted > 120
    }

    /// ¿Es `p` un objeto normal, abierto sin seguir enlaces y que NO es un reparse point? Falla CERRADO: si no se puede abrir
    /// o leer, no es "normal".
    pub fn is_plain(p: &Path) -> bool {
        sys::open_for_query(p)
            .and_then(|h| sys::meta(&h))
            .is_ok_and(|m| !m.is_reparse())
    }

    /// ¿Cuelga `p` de `<unidad>:\$Recycle.Bin\<sid>\`? (Los componentes se comprueban uno a uno.)
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

    /// Borra POR RUTA, sin el código bajo prueba, el `$R` y el `$I` de un elemento propio que quedó en la Papelera. Solo si el
    /// elemento es de una prueba ([`is_any_test_item`], defensa en profundidad: lo exige también el llamante), ambos cuelgan
    /// de `<unidad>:\$Recycle.Bin\<SID del usuario actual>\` y ninguno es un reparse point ([`is_plain`], que falla cerrado).
    /// `true` si quedó sin `$R`.
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

    /// Purga lo que quede de las pruebas que cumplen `which`: hasta 3 intentos con `list_bin` + `purge_items` y, si aún queda
    /// algo, borrado por ruta (ver [`remove_by_path`]). Devuelve cuántos elementos quedaron, o el error si la Papelera no se
    /// puede listar (S-3: no se puede decir "0 fugas" sin haberla visto).
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

    /// ¿Hay que hacer `panic!` por lo que dejó la limpieza? `Some(mensaje)` si quedan elementos o si no se pudo listar la
    /// Papelera, salvo que ya se esté en un pánico (un `panic!` en un `drop` durante un pánico aborta el proceso).
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

    /// Barre los restos de ejecuciones ANTERIORES (origen bajo `tesela-test-<uuid>`, borrados hace más de 2 minutos: nunca lo
    /// de una prueba que corra a la vez en otro proceso). Solo se llama con las pruebas reales activadas y con `bin_lock()`.
    pub fn sweep_leftovers() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let temp = dunce::canonicalize(std::env::temp_dir()).unwrap();
        if let Some(msg) = leak_verdict(clean_own(&|i| sweep_candidate(i, now, &temp)), false) {
            panic!("BARRIDO: {msg}");
        }
    }

    /// Al soltarla (también en un pánico) purga SOLO los elementos propios de la prueba: hasta 3 intentos con el código y,
    /// si algo queda, borrado por ruta SIN el código bajo prueba (ver [`remove_by_path`]). Dice cuántos quedaron y, si no
    /// es 0 (o no se pudo listar la Papelera), hace `panic!` (salvo que ya se esté en un pánico): una fuga no puede pasar
    /// desapercibida.
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

    /// Un elemento sintético de la Papelera (`$R` y `$I` en una carpeta del sandbox) con las identidades capturadas ahora.
    fn synthetic_bin_item(sb: &Sandbox, token: &str, data: &[u8]) -> BinItem {
        sb.mkdir("bin/S-synthetic");
        // Forma larga (ver `send_item`): el `id` de un elemento real es la ruta larga que da la shell.
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

    /// El receptor de una PURGA con la guarda de estos elementos (A-M4), sin COM.
    fn purge_core(items: &[&BinItem]) -> SinkCore {
        SinkCore {
            events: Arc::new(Mutex::new(Vec::new())),
            state: Arc::new(SinkState::default()),
            guard: Guard::purge(items),
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
        assert!(!core.guard.sends());
        assert!(core.pre_delete(a.id.clone()));
        assert!(!core.post_delete(a.id.clone(), 0, BinState::Deleted));
        assert!(!core.state.tripped.load(Ordering::SeqCst));
        assert_eq!(core.events.lock().unwrap().len(), 1);
        // Y una guarda de envío sí corta (la de siempre).
        let it = send_item(&sb, "s/uno.txt", 10);
        let envio = core_for(std::slice::from_ref(&it));
        assert!(envio.guard.sends());
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

    /// R-4: el elemento al que se atribuye un veto es el DUEÑO de la ruta vetada, no la ruta: con un veto cuya ruta es la de un
    /// HIJO de una carpeta de la Papelera (`owner` = su `$R`) el elemento sigue constando como vetado en el trozo.
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
        // Un veto sin dueño (un elemento al que se vetó él mismo) se atribuye por su propia ruta.
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
        // Sin vetos, ninguno.
        assert!(vetoed_items(&chunk, &[]).is_empty());
    }

    /// R-5: el barrido de restos SOLO coge elementos de alguna prueba (carpeta de origen `tesela-test-<uuid>` hija de `%TEMP%`,
    /// o un elemento suelto de `%TEMP%` con ese nombre) borrados hace más de 2 minutos: nunca uno del usuario, ni uno reciente.
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
        // De una prueba y antiguo: sí.
        assert!(sweep_candidate(&item(&test_dir, "a.txt", old), now, &temp));
        // ...pero reciente (hace 120 s o menos): no.
        assert!(!sweep_candidate(
            &item(&test_dir, "a.txt", now - 120),
            now,
            &temp
        ));
        assert!(!sweep_candidate(&item(&test_dir, "a.txt", now), now, &temp));
        // Del usuario (cualquier otra carpeta), antiguo: no.
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
        // R-6b: una carpeta de origen que SOLO empieza como `tesela-test-<uuid>` (con algo detrás) no es de una prueba.
        let extra_dir = format!(r"{}\tesela-test-{UUID}-extra\x", temp.display());
        assert!(!sweep_candidate(
            &item(&extra_dir, "a.txt", old),
            now,
            &temp
        ));
        // Un elemento SUELTO en `%TEMP%`: solo con la forma `tesela-test-<uuid>`.
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
        // R-6: la forma del uuid (36 caracteres, 8-4-4-4-12 en hexadecimal) y, para carpetas, sin nada detrás.
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

    /// R-5: el borrado por ruta de la limpieza rechaza un reparse point (junction) y falla CERRADO si no puede abrir lo que va
    /// a borrar; y exige la forma de la Papelera propia.
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
        // La forma de la Papelera propia.
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

    /// S-3: la detección de fugas del `BinGuard` no dice "0 fugas" si no pudo mirar: sin poder listar la Papelera hay error, y
    /// con fugas hay `panic!` salvo que ya se esté en un pánico.
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

    /// Incidente de la Papelera (limpieza de las pruebas): el borrado POR RUTA que usa `BinGuard` cuando el código bajo prueba no
    /// consigue purgar solo toca lo que cuelga de `<unidad>:\$Recycle.Bin\<SID propio>\`: un elemento sintético (en el sandbox)
    /// no se toca aunque se lo pidan. Sin la variable.
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
        // Y tampoco una carpeta del sandbox con la forma `$Recycle.Bin\<otro SID>`.
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

    /// Con la shell REAL (propios): el borrado por ruta de la limpieza quita el `$R` (también una carpeta) y su `$I` de un
    /// elemento propio y deja la Papelera como estaba.
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

    /// m-6: `remove_orphan_meta_at` no toca el `$I` de un elemento cuyo `$R` sigue existiendo (un SUSTITUIDO: la shell dio la
    /// purga por buena pero el objeto aprobado no se eliminó) ni mientras el `$R` esté solo "pendiente de borrar" (la guarda
    /// abierta): solo con el `$R` realmente inexistente. (El bucle de la purga tampoco la llama para los sustituidos.)
    #[test]
    fn el_i_de_un_sustituido_no_se_toca_mientras_su_r_exista() {
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"datos a");
        let data = PathBuf::from(&a.id);
        let meta = meta_sibling(&data).unwrap();
        assert!(!remove_orphan_meta_at(&a, &data, &meta), "el `$R` existe");
        assert!(meta.exists());
        // Con la guarda abierta y el `$R` borrado por la shell, el nombre sigue visible como pendiente de borrar: tampoco.
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

    /// Un elemento a enviar con la forma LARGA de la ruta: `PreDeleteItem` recibe de la shell
    /// `SIGDN_DESKTOPABSOLUTEPARSING`, que es siempre larga, y `PathKey` no resuelve nombres 8.3 (en el CI `%TEMP%` es 8.3).
    fn send_item(sb: &Sandbox, rel: &str, size: usize) -> SendItem {
        sb.write(rel, size);
        SendItem::capture(&sb.long(rel))
    }

    /// GM-M5 (sin sistema de archivos): dos nombres que solo difieren en mayúsculas (posible en un directorio sensible a ellas)
    /// tienen la MISMA `PathKey`. La guarda busca primero el nombre EXACTO, así que cada uno recibe su propia identidad; con
    /// la búsqueda por clave, el segundo habría heredado la del primero. Un nombre que no coincide con ninguno exacto solo se
    /// resuelve por clave si es inequívoco.
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
        // Ni uno ni otro exacto (`A.BIN`): con dos candidatos por clave no se adivina (se vetaría como no aprobado).
        assert_eq!(want(r"C:\x\A.BIN"), None);
        // Con un solo candidato la clave sí resuelve (la shell puede dar otras mayúsculas o la forma `c:`).
        let solo = Guard::new(&[item(r"C:\x\a.bin", 1)]);
        assert!(matches!(
            solo.find(&OsString::from(r"c:\X\A.BIN")),
            Some((_, Approved::Object { identity, .. })) if *identity == id(1)
        ));
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
        // A2-M1: un FALLO también corta al enviar, sea cual sea el estado: no prueba que la shell no tocara nada.
        assert!(post_delete_aborts(true, fail, &BinState::Deleted));
        assert!(post_delete_aborts(true, fail, &BinState::InBinUnknownPath));
        let in_bin = |n: &str| BinState::InBin(PathBuf::from(format!(r"C:\$Recycle.Bin\S\{n}")));
        assert!(post_delete_aborts(true, fail, &in_bin("$R2")));
        assert!(post_delete_aborts(true, i32::MIN, &BinState::Deleted));
        // Un código de ÉXITO con facilidad (positivo, como `0x00270003`) no es un fallo.
        assert!(!post_delete_aborts(true, 0x0027_0008, &in_bin("$R3")));
        // Al PURGAR el borrado definitivo y los fallos son lo esperado de cada elemento: nunca corta.
        assert!(!post_delete_aborts(false, ok, &BinState::Deleted));
        assert!(!post_delete_aborts(false, ok, &BinState::InBinUnknownPath));
        assert!(!post_delete_aborts(false, fail, &BinState::Deleted));
        assert!(!post_delete_aborts(
            false,
            fail,
            &BinState::InBinUnknownPath
        ));
    }

    /// El receptor de un ENVÍO con la guarda de estos elementos.
    fn core_for(items: &[SendItem]) -> SinkCore {
        SinkCore {
            events: Arc::new(Mutex::new(Vec::new())),
            state: Arc::new(SinkState::default()),
            guard: Guard::new(items),
        }
    }

    /// H3 (sin COM): el receptor registra el evento ANTES de decidir cortar, y una vez cortado niega todo lo posterior.
    #[test]
    fn el_receptor_registra_primero_corta_despues_y_niega_lo_posterior() {
        let sb = Sandbox::new();
        let a = send_item(&sb, "r/a.txt", 10);
        let b = send_item(&sb, "r/b.txt", 10);
        let core = core_for(&[a.clone(), b.clone()]);
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
        let ok = core_for(std::slice::from_ref(&a));
        let bin = PathBuf::from(r"C:\$Recycle.Bin\S\$R1");
        assert!(!ok.post_delete(os(&a.path), 0, BinState::InBin(bin)));
        assert!(!ok.state.tripped.load(Ordering::SeqCst));
        // Al purgar (con la guarda de la purga) el borrado definitivo no corta, y lo aprobado no se veta.
        let pi = synthetic_bin_item(&sb, "PPPP05", b"x");
        let purge = purge_core(&[&pi]);
        assert!(purge.pre_delete(pi.id.clone()));
        assert!(!purge.post_delete(pi.id.clone(), 0, BinState::Deleted));
        assert!(!purge.state.tripped.load(Ordering::SeqCst));
        assert_eq!(purge.events.lock().unwrap().len(), 1);
    }

    /// H3 (sin COM): un veto registra su motivo, corta y niega lo siguiente.
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

    /// Un archivo a enviar con la comprobación de tamaño (A2-B1): su handle de guarda (el mismo que abre el ejecutor) y lo que se
    /// contó para el presupuesto. El handle viaja DENTRO del `SendItem` (`size_guard`) y se cierra al soltar el elemento.
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

    /// A2-B1 (sin COM): la comprobación de `PreDeleteItem` vuelve a medir el archivo POR SU HANDLE DE GUARDA. Dentro de lo que se
    /// contó, pasa (también si encogió); más grande (en disco o lógico), se veta como `Grew`; la identidad se comprueba antes
    /// (un sustituto sigue siendo `IdentityMismatch`) y un archivo sin comprobación de tamaño (un directorio) no se mide.
    #[test]
    fn el_guard_veta_un_archivo_que_crecio_por_su_handle_de_guarda() {
        let sb = Sandbox::new();
        // 1 000 bytes ocupan 4 096 en disco (un cluster): se cuenta eso.
        let a = sized_item(&sb, "z/a.txt", 1_000, 4_096);
        let guard = Guard::new(std::slice::from_ref(&a));
        let os = |p: &Path| p.as_os_str().to_os_string();
        assert_eq!(guard.check(&os(&a.path)), Ok(()), "tal cual se midió");
        // Crece un poco dentro del mismo cluster: el tamaño lógico sube pero no pasa de lo contado.
        std::fs::write(sb.path("z/a.txt"), vec![1u8; 4_000]).unwrap();
        assert_eq!(guard.check(&os(&a.path)), Ok(()), "dentro de lo contado");
        // Pasa del cluster: más en disco y más lógico que lo contado.
        std::fs::write(sb.path("z/a.txt"), vec![1u8; 1 << 20]).unwrap();
        assert_eq!(guard.check(&os(&a.path)), Err(VetoKind::Grew));
        // Encoge: dentro de lo contado (el presupuesto solo se rompe al crecer).
        std::fs::write(sb.path("z/a.txt"), vec![1u8; 10]).unwrap();
        assert_eq!(guard.check(&os(&a.path)), Ok(()));
        // H3: sin tolerancia alguna. Con un presupuesto de 4 096, 4 096 bytes caben (en disco, un cluster) y 4 097 no (el
        // tamaño lógico pasa de lo contado aunque la asignación siga siendo de un cluster u ocho KiB).
        let e = sized_item(&sb, "z/exacto.txt", 4_096, 4_096);
        let ge = Guard::new(std::slice::from_ref(&e));
        assert_eq!(ge.check(&os(&e.path)), Ok(()), "justo lo contado");
        std::fs::write(sb.path("z/exacto.txt"), vec![1u8; 4_097]).unwrap();
        assert_eq!(ge.check(&os(&e.path)), Err(VetoKind::Grew), "un byte más");
        // H3: la asignación en disco cuenta aunque el tamaño lógico no cambie (`FileAllocationInfo` sin tocar `EndOfFile`).
        let pre = sized_item(&sb, "z/prealoca.txt", 1_000, 4_096);
        let gp = Guard::new(std::slice::from_ref(&pre));
        assert_eq!(gp.check(&os(&pre.path)), Ok(()));
        sys::preallocate_for_test(&sb.path("z/prealoca.txt"), 1 << 20).unwrap();
        assert_eq!(
            gp.check(&os(&pre.path)),
            Err(VetoKind::Grew),
            "más en disco"
        );
        // Solo el tamaño LÓGICO pasa de lo contado (un disperso: poco en disco, mucho lógico): también se veta.
        let big = sb.path("z/dispersa.bin");
        crate::testutil::sparse_file(&big, 1 << 30);
        let mut d = SendItem::capture(&sb.long("z/dispersa.bin"));
        d.size_guard = Some(SizeGuard {
            handle: Arc::new(sys::open_path_for_rename(&sb.long("z/dispersa.bin")).unwrap()),
            budget: 4_096,
        });
        let g2 = Guard::new(std::slice::from_ref(&d));
        assert_eq!(g2.check(&os(&d.path)), Err(VetoKind::Grew));
        // Un sustituto en la ruta: la identidad manda y se comprueba ANTES que el tamaño.
        std::fs::rename(sb.path("z/a.txt"), sb.path("z/a-ori.txt")).unwrap();
        sb.write("z/a.txt", 10);
        assert_eq!(guard.check(&os(&a.path)), Err(VetoKind::IdentityMismatch));
        // Sin comprobación de tamaño (un directorio, o un archivo suelto) no se mide.
        let dir = SendItem::capture(&sb.mkdir("z/carpeta"));
        sb.write("z/carpeta/hijo.txt", 1 << 20);
        let g3 = Guard::new(std::slice::from_ref(&dir));
        assert_eq!(g3.check(&os(&dir.path)), Ok(()));
    }

    /// A2-M1 (sin COM): un lote con DOS carpetas pequeñas. La shell pierde un hijo de la primera y notifica un fallo
    /// (`E_ACCESSDENIED`, sin elemento nuevo): el receptor registra el evento, corta, y la segunda carpeta ya no recibe
    /// `PreDeleteItem` (la shell no la procesa). Antes un fallo devolvía éxito: el siguiente pasaba y podía perder también un
    /// hijo.
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

    /// A2-B1 (sin COM): el receptor veta al archivo que creció, corta el lote y lo anota con su motivo; los anteriores ya
    /// procesados siguen en sus eventos.
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
        // `b` crece después de preparar el lote y antes de su `PreDeleteItem`.
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

    /// A-m1: `PreDeleteItem` por la interfaz COM. Lo aprobado pasa; un sustituto en la ruta se veta con `E_ABORT` (y consta el
    /// veto); una vez cortado el lote, lo siguiente se niega aunque sea lo aprobado. (Un mutante `PreDeleteItem -> Ok(())` falla.)
    #[test]
    fn el_callback_pre_delete_item_veta_con_e_abort_lo_que_no_es_lo_aprobado() {
        let _com = Com::init().unwrap();
        let sb = Sandbox::new();
        let a = send_item(&sb, "c/a.txt", 100);
        let b = send_item(&sb, "c/b.txt", 100);
        let (sink, _events, state) = com_sink(Guard::new(&[a.clone(), b.clone()]));
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

    /// A2-B1: `PreDeleteItem` por la interfaz COM con la comprobación de tamaño: el archivo que creció tras preparar el lote se
    /// veta con `E_ABORT` y consta `Grew`; antes de crecer, el mismo callback lo deja pasar. (Un mutante que quite la
    /// comprobación de tamaño de `Guard::check` falla aquí.)
    #[test]
    fn el_callback_pre_delete_item_veta_con_e_abort_el_archivo_que_crecio() {
        let _com = Com::init().unwrap();
        let sb = Sandbox::new();
        let a = sized_item(&sb, "k/a.txt", 100, 4_096);
        let (sink, _events, state) = com_sink(Guard::new(std::slice::from_ref(&a)));
        // SAFETY: interfaz COM viva y en su hilo; `IShellItem` vivos.
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

    /// A-m1: `PostDeleteItem` por la interfaz COM: registra el evento (con su `HRESULT` y el estado según `psiNewlyCreated`) y
    /// devuelve `E_ABORT` solo cuando la shell eliminó definitivamente con éxito (nulo) o no dio la ruta. (Un mutante
    /// `PostDeleteItem -> Ok(())` falla: no registraría nada ni cortaría.)
    #[test]
    fn el_callback_post_delete_item_registra_el_evento_y_corta_tras_un_borrado_definitivo() {
        let _com = Com::init().unwrap();
        let sb = Sandbox::new();
        let a = send_item(&sb, "d/a.txt", 100);
        let b = send_item(&sb, "d/b.txt", 100);
        sb.write("d/bin/R1.txt", 10);
        let dest = sb.long("d/bin/R1.txt");
        let (sink, events, state) = com_sink(Guard::new(&[a.clone(), b.clone()]));
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
        // A2-M1: con un FALLO de la shell (y nulo) SÍ se corta (antes devolvía éxito y dejaba pasar el siguiente
        // `PreDeleteItem`), y consta. Es la repetición de Astra: `PostDeleteItem(E_ACCESSDENIED, NULL)`.
        let (sink, events, state) = com_sink(Guard::new(&[a.clone(), b.clone()]));
        // SAFETY: igual que arriba.
        unsafe {
            let fail = HRESULT(0x8007_0005u32 as i32);
            let r = sink.PostDeleteItem(0, &shell_item(&a.path), fail, None::<&IShellItem>);
            assert!(is_abort(r), "un fallo corta el lote con E_ABORT");
            assert!(state.tripped.load(Ordering::SeqCst));
            // Y lo siguiente del lote, que es lo aprobado, se niega.
            assert!(is_abort(sink.PreDeleteItem(0, &shell_item(&b.path))));
        }
        assert_eq!(events.lock().unwrap()[0].hr, 0x8007_0005u32 as i32);
        // Un fallo con elemento en la Papelera (hr < 0 pero `psiNewlyCreated` dado) corta igual.
        let (sink, _events, state) = com_sink(Guard::new(std::slice::from_ref(&a)));
        // SAFETY: igual que arriba.
        unsafe {
            let fail = HRESULT(0x8007_0020u32 as i32);
            let r = sink.PostDeleteItem(0, &shell_item(&a.path), fail, &shell_item(&dest));
            assert!(is_abort(r));
            assert!(state.tripped.load(Ordering::SeqCst));
        }
        // Al PURGAR, el borrado definitivo es lo esperado: se registra y no se corta.
        let pi = synthetic_bin_item(&sb, "PPPP05", b"x");
        let (sink, events, state) = com_sink(Guard::purge(&[&pi]));
        // SAFETY: igual que arriba.
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

    /// M-2 (sin COM): el veto de un HIJO de una carpeta de la Papelera se atribuye al elemento de la lista que lo autorizó
    /// (su `$R`), no a la ruta del hijo; y el de un elemento sin dueño, a sí mismo.
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
        // El `$R` se sustituye por otra carpeta: el hijo se veta y el veto es de ESE elemento.
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
        // Algo que no cuelga de ningún elemento aprobado: sin dueño, el veto es de su propia ruta.
        let core = purge_core(&[&dir_item]);
        let ajeno = sb.path("bin/S-synthetic/otro.txt").into_os_string();
        assert!(!core.pre_delete(ajeno.clone()));
        let v = core.state.vetoes.lock().unwrap()[0].clone();
        assert_eq!((v.owner.as_ref(), v.id()), (None, &ajeno));
    }

    /// M-3 con la shell REAL (propios): TRES elementos y el veto en el MEDIO (el segundo en el orden de la shell). Medido: el
    /// anterior se purga, el vetado recibe `E_ABORT` y su `$R` sigue (el sustituto) y el siguiente no recibe aviso y sigue en
    /// la Papelera.
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
        // El orden en que la shell procesa los elementos es el de la enumeración: el del medio es el vetado.
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

    /// M-3 con la shell REAL (propios): con trozos de 2 y 5 elementos, un veto en el PRIMER trozo corta también los trozos
    /// siguientes: ninguno de los otros 3 se purga (sin el `break` de `purge_items_flags` se purgarían).
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
                PurgeStep::Replaced(_) => "replaced".to_string(),
                PurgeStep::Done(d) => format!("done {}", d.len()),
            };
            log_steps.lock().unwrap().push(name);
        })
        .unwrap();
        eprintln!("trozos: {:?}", log_steps.lock().unwrap());
        assert!(r.sink_aborted && r.vetoed.len() == 1);
        // Solo hubo un trozo entregado (con 2 elementos); el resto consta como "no procesado" (done con 3) sin intención.
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

    /// A-m1 + A-M4: la guarda de una PURGA por la interfaz COM, con elementos sintéticos de la Papelera: el `PreDeleteItem` que
    /// recibe el nombre de análisis del elemento comprueba el `$R` y el `$I` y veta con `E_ABORT` el `$R` sustituido.
    #[test]
    fn el_callback_pre_delete_item_de_una_purga_veta_el_r_sustituido() {
        let _com = Com::init().unwrap();
        let sb = Sandbox::new();
        let a = synthetic_bin_item(&sb, "AAAA01", b"datos a");
        let b = synthetic_bin_item(&sb, "BBBB02", b"datos b");
        let (sink, _events, state) = com_sink(Guard::purge(&[&a, &b]));
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

    /// A2-B1 con la shell REAL (solo elementos propios): tres archivos en una operación, el del medio crece tras "preparar el
    /// lote" (su tamaño contado se queda corto). El receptor lo vuelve a medir por su handle de guarda en `PreDeleteItem`, lo
    /// veta con `E_ABORT` y la shell no procesa lo siguiente; el anterior llega a la Papelera. (Es la medida real de lo que
    /// hace la shell con el veto por tamaño, igual que el veto por identidad de la prueba anterior.)
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
        // Tras "preparar": b crece más allá de lo contado.
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

    /// MEDICIÓN A2-B1 (no es una prueba; `cargo test -- --ignored --nocapture medicion_a2_b1`, con
    /// `TESELA_REAL_RECYCLE_TESTS=1`, sola y con elementos propios): coste con la shell REAL de enviar 500 archivos pequeños
    /// (a) en UNA operación con la comprobación de tamaño en `PreDeleteItem`, (b) en una operación sin esa comprobación
    /// (control) y (c) uno por operación (la alternativa descartada). Imprime milisegundos por ronda.
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
        // `guarded`: cada archivo lleva su handle de guarda y su tamaño contado (como en producción).
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

    /// MEDICIÓN A2-B1 (sin shell, sin Papelera): lo que añade la comprobación de tamaño a `Guard::check` por archivo (la
    /// reapertura por ruta de la identidad, que ya existía, frente a esa más la lectura de `FileStandardInfo` por el handle
    /// de guarda). `cargo test -- --ignored --nocapture medicion_a2_b1_coste_del_check`.
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
