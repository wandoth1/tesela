//! La Papelera de reciclaje: enviar rutas, listar su contenido y purgar una lista enumerada, con COM (`windows`).
//!
//! `IFileOperation` trabaja por RUTAS y no se puede atar a un handle. El diseño (sin lápidas) es:
//! 1. quien llama abre el elemento en exclusiva, captura su identidad y detecta "en uso", y CIERRA el handle;
//! 2. este módulo envía la RUTA ORIGINAL a la Papelera con un `IFileOperationProgressSink` propio ([`Recorder`]) que
//!    registra, por elemento, `hrDelete` y la ruta real del elemento ya en la Papelera (`psiNewlyCreated`);
//! 3. quien llama abre ESA ruta (`$R…`) y compara su identidad con la aprobada.
//!
//! Entre la comprobación del paso 1 y que la shell abra el elemento hay una ventana. Se cierra con un handle de GUARDA
//! (`DELETE | READ_ATTRIBUTES`, compartiéndolo todo) que quien llama conserva abierto hasta después del paso 3: mientras
//! está abierto Windows no deja renombrar el padre ni el abuelo del elemento (error 5), así que no se puede cambiar un
//! ancestro por un junction hacia otra carpeta (medido por la auditoría; cubierto por las pruebas) y la shell recicla
//! igual. Lo que NO puede impedir el handle es la sustitución del propio nombre por otro objeto (sí se puede renombrar el
//! elemento: el handle comparte `DELETE`): ese caso cae en el paso 3.
//!
//! La garantía real es, pues: (a) la comprobación de ruta y protección es previa y se mantiene con el handle de guarda;
//! (b) lo que llega a la Papelera se comprueba por IDENTIDAD, y además `Recycled` exige que el nombre aprobado ya no
//! exista y que el objeto de la guarda esté ahora dentro de una Papelera (otro nombre del mismo objeto, p. ej. un enlace
//! duro, no cuenta); (c) cualquier otra cosa que llegue a la Papelera es recuperable y se informa (`RecycledOther`), pero
//! NO se garantiza que "nunca" pueda ser un objeto no aprobado: solo que se detecta y es restaurable.
//!
//! Banderas (decididas por las pruebas, ver `tests::wantnukewarning_*`):
//! * enviar: `FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE`;
//! * purgar elementos de la Papelera: `FOF_NO_UI`.
#![allow(unsafe_code)]

use std::collections::HashSet;
use std::ffi::{OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{FILETIME, PROPERTYKEY};
use windows::Win32::Storage::EnhancedStorage::PKEY_Size;
use windows::Win32::System::Com::StructuredStorage::PropVariantToBSTR;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
#[cfg(test)]
use windows::Win32::UI::Shell::FOF_WANTNUKEWARNING;
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NO_UI, FOFX_RECYCLEONDELETE,
    FOLDERID_RecycleBinFolder, FileOperation, IEnumShellItems, IFileOperation,
    IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem, IShellItem2,
    KF_FLAG_DEFAULT, PID_DISPLACED_DATE, PID_DISPLACED_FROM, PSGUID_DISPLACED,
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
            out.push(BinItem {
                id,
                name,
                original_parent,
                time_deleted,
                size,
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
}

type Events = Arc<Mutex<Vec<DeleteEvent>>>;

/// Nombre de análisis o ruta de un elemento de la shell, si se puede.
fn display(item: &IShellItem, sigdn: SIGDN) -> Option<OsString> {
    // SAFETY: `item` es una interfaz viva; la cadena que devuelve es de COM y `take_pwstr` la libera una sola vez.
    unsafe { item.GetDisplayName(sigdn).ok().map(|p| take_pwstr(p)) }
}

/// El receptor de eventos de `IFileOperation`. Implementa la interfaz COM con `windows::core::implement` (la vtable,
/// `QueryInterface`, `AddRef` y `Release` los genera la macro). Solo REGISTRA: nunca decide ni falla (si un método
/// devolviera un error, la shell cancelaría todo lo que quedara pendiente).
#[implement(IFileOperationProgressSink)]
struct Recorder {
    events: Events,
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
    fn PreDeleteItem(&self, _dwflags: u32, _psiitem: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
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
        let state = match psinewlycreated.as_ref() {
            None => BinState::Deleted,
            Some(n) => match display(n, SIGDN_FILESYSPATH) {
                Some(p) => BinState::InBin(PathBuf::from(p)),
                None => BinState::InBinUnknownPath,
            },
        };
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(DeleteEvent {
                item,
                hr: hrdelete.0,
                state,
            });
        Ok(())
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
}

impl Operation {
    fn new(flags: FILEOPERATION_FLAGS) -> Result<Operation> {
        let events: Events = Arc::new(Mutex::new(Vec::new()));
        let sink: IFileOperationProgressSink = Recorder {
            events: events.clone(),
        }
        .into();
        // SAFETY: COM está inicializado en este hilo (lo garantiza quien llama); `sink` es una interfaz viva que `Advise`
        // retiene (AddRef) hasta `Unadvise`.
        unsafe {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            op.SetOperationFlags(flags)?;
            let cookie = op.Advise(&sink)?;
            Ok(Operation { op, cookie, events })
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
        BatchOutcome {
            events,
            any_aborted: aborted,
            error,
            meta_removed: 0,
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

/// Envía las rutas (una operación `IFileOperation` con todas) a la Papelera POR SU RUTA ORIGINAL. La shell abre cada
/// elemento por su cuenta: entre que quien llama cierra su handle y ese momento hay una ventana, acotada por el destino
/// (una sustitución hecha por el mismo usuario solo puede mandar OTRO objeto a la Papelera, recuperable). Por eso el
/// resultado trae, por elemento, la ruta real del elemento en la Papelera: quien llama compara su IDENTIDAD con la
/// aprobada.
pub fn recycle_paths(paths: &[PathBuf], mode: RecycleMode) -> Result<BatchOutcome> {
    if paths.is_empty() {
        return Ok(BatchOutcome::default());
    }
    let _com = Com::init()?;
    let flags = match mode {
        RecycleMode::Normal => FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE,
        #[cfg(test)]
        RecycleMode::WithNukeWarning => {
            FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_WANTNUKEWARNING
        }
    };
    let op = Operation::new(flags)?;
    let mut added = 0usize;
    for p in paths {
        let name = parsing_name(p);
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

/// Purga (borra definitivamente) SOLO los elementos de la lista, PASANDO POR LA CARPETA SHELL de la Papelera: se
/// enumera la Papelera y se llama a `IFileOperation::DeleteItem` sobre los `IShellItem` enumerados, de modo que la shell
/// elimina a la vez el `$R` (los datos) y el `$I` (sus metadatos). Nunca por la ruta `$R`: eso dejaba el `$I` huérfano.
/// Quien llama pasa una lista enumerada al confirmar y los elementos se casan por su `id` estable: lo que haya entrado
/// después no se toca. La cancelación se mira entre trozos de 500.
pub fn purge_items(items: &[BinItem], cancel: &AtomicBool) -> Result<BatchOutcome> {
    purge_items_flags(items, cancel, FOF_NO_UI)
}

fn purge_items_flags(
    items: &[BinItem],
    cancel: &AtomicBool,
    flags: FILEOPERATION_FLAGS,
) -> Result<BatchOutcome> {
    if items.is_empty() {
        return Ok(BatchOutcome::default());
    }
    let _com = Com::init()?;
    let wanted: HashSet<&OsString> = items.iter().map(|i| &i.id).collect();
    let mut found: Vec<IShellItem> = Vec::with_capacity(items.len());
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
                && wanted.contains(&id)
            {
                found.push(item);
                if found.len() == wanted.len() {
                    break;
                }
            }
        }
    }
    let mut out = BatchOutcome::default();
    let by_id: std::collections::HashMap<&OsString, &BinItem> =
        items.iter().map(|i| (&i.id, i)).collect();
    for chunk in found.chunks(500) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let op = Operation::new(flags)?;
        let mut added = 0usize;
        for it in chunk {
            // SAFETY: `op.op` y `it` son interfaces vivas.
            if unsafe { op.op.DeleteItem(it, None) }.is_ok() {
                added += 1;
            }
        }
        if added == 0 {
            continue;
        }
        let r = op.perform();
        out.any_aborted |= r.any_aborted;
        // Medido en Windows 11 (compilación 26300): la shell borra el `$R` pero puede dejar el `$I` (los metadatos del
        // elemento) huérfano, sea cual sea la bandera. Se quita ese `$I` exacto, con las comprobaciones de abajo.
        for e in &r.events {
            if e.succeeded()
                && let Some(it) = by_id.get(&e.item)
                && remove_orphan_meta(it)
            {
                out.meta_removed += 1;
            }
        }
        out.events.extend(r.events);
        if let Some(e) = r.error {
            out.error = Some(e);
        }
    }
    Ok(out)
}

/// Lo que guarda un `$I` (metadatos de un elemento de la Papelera): versión, tamaño y ruta original.
#[derive(Debug, PartialEq, Eq)]
struct MetaInfo {
    version: u64,
    size: u64,
    original: PathBuf,
}

/// Interpreta un `$I` (versión 1: ruta fija de 260 unidades; versión 2: longitud y ruta). `None` si no lo es.
fn parse_meta(b: &[u8]) -> Option<MetaInfo> {
    let u64_at = |at: usize| Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?));
    let version = u64_at(0)?;
    let size = u64_at(8)?;
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
        original: PathBuf::from(OsString::from_wide(&units)),
    })
}

/// Quita el `$I` que la shell dejó huérfano tras purgar el elemento `item` (cuyo `$R` ya no existe). Es lo único que este
/// módulo hace por ruta dentro de `$Recycle.Bin`, y solo con TODAS estas condiciones:
/// * el `$R` (el `id` del elemento) ya no existe, se llama `$R<token>[.ext]` y está en `<unidad>:\$Recycle.Bin\<SID del
///   usuario actual>\`;
/// * existe el hermano EXACTO `$I<token>[.ext]`, es un archivo normal pequeño (sin reparse point) y su contenido es un `$I`
///   válido cuya ruta original cuelga de la misma carpeta que dice el elemento y con la misma extensión;
/// * se abre en exclusiva y se lee, se interpreta y se borra por ese mismo handle.
fn remove_orphan_meta(item: &BinItem) -> bool {
    use crate::paths::PathKey;
    let data = PathBuf::from(&item.id);
    if std::fs::symlink_metadata(&data).is_ok() {
        return false;
    }
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
    let meta_path = sid_dir.join(format!("$I{}", &name[2..]));
    // Se abre en EXCLUSIVA primero y se lee, se interpreta y se borra por ese MISMO handle: lo que se lee es lo que se
    // borra.
    let Ok(h) = crate::sys::open_path_exclusive_read(&meta_path) else {
        return false;
    };
    let Ok(m) = crate::sys::meta(&h) else {
        return false;
    };
    if m.is_dir || m.is_reparse() || m.logical > 70_000 {
        return false;
    }
    let Ok(bytes) = crate::sys::read_small(&h, 70_000) else {
        return false;
    };
    let Some(info) = parse_meta(&bytes) else {
        return false;
    };
    let same_folder = info
        .original
        .parent()
        .is_some_and(|p| PathKey::new(p) == PathKey::new(&item.original_parent));
    let ext = |p: &Path| {
        p.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    if !same_folder || ext(&info.original) != ext(&data) {
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

    /// ¿Están activadas las pruebas con la Papelera real? Solo con `TESELA_REAL_RECYCLE_TESTS=1`. El CI de GitHub
    /// (`windows-latest`, Windows Server sin sesión interactiva) no tiene una Papelera de usuario fiable: la shell la
    /// lista pero no devuelve lo que se envía a ella, y esas pruebas fallaban allí por el entorno, no por el código. En
    /// un equipo de escritorio la suite completa se lanza con la variable (ver el README). Sin ella, estas pruebas se
    /// saltan con un mensaje; las aserciones NO se han debilitado.
    pub fn real_recycle_enabled() -> bool {
        if std::env::var(REAL_RECYCLE_VAR).is_ok_and(|v| v == "1") {
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
        let r = recycle_paths(&paths, RecycleMode::Normal).unwrap();
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

    fn meta_v2(original: &str) -> Vec<u8> {
        let wide: Vec<u16> = original.encode_utf16().chain(Some(0)).collect();
        let mut b = Vec::new();
        b.extend_from_slice(&2u64.to_le_bytes());
        b.extend_from_slice(&1234u64.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(&(wide.len() as u32).to_le_bytes());
        for u in wide {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b
    }

    #[test]
    fn el_i_se_interpreta_en_sus_dos_versiones_y_rechaza_lo_que_no_lo_es() {
        let v2 = parse_meta(&meta_v2(r"C:\a\b\uno.txt")).unwrap();
        assert_eq!(
            v2,
            MetaInfo {
                version: 2,
                size: 1234,
                original: PathBuf::from(r"C:\a\b\uno.txt")
            }
        );
        let mut v1 = Vec::new();
        v1.extend_from_slice(&1u64.to_le_bytes());
        v1.extend_from_slice(&99u64.to_le_bytes());
        v1.extend_from_slice(&0u64.to_le_bytes());
        let mut path: Vec<u16> = r"C:\x\y.bin".encode_utf16().collect();
        path.resize(260, 0);
        for u in path {
            v1.extend_from_slice(&u.to_le_bytes());
        }
        let v1 = parse_meta(&v1).unwrap();
        assert_eq!((v1.version, v1.size), (1, 99));
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

    #[test]
    fn el_i_huerfano_solo_se_quita_si_todo_cuadra() {
        let sb = Sandbox::new();
        // Fuera de `$Recycle.Bin\<mi SID>\`: nunca se toca, aunque el nombre y el contenido parezcan los de un `$I`.
        let fake_dir = sb.mkdir(&format!(
            "$Recycle.Bin/{}",
            ["S-1-5", "-21-1-2-3-1001"].concat()
        ));
        std::fs::write(
            fake_dir.join("$IABC123.txt"),
            meta_v2(&format!("{}\\p\\uno.txt", sb.long_root().display())),
        )
        .unwrap();
        let item = BinItem {
            id: fake_dir.join("$RABC123.txt").into_os_string(),
            name: OsString::from("uno.txt"),
            original_parent: sb.long("p"),
            time_deleted: 0,
            size: 0,
        };
        assert!(!remove_orphan_meta(&item));
        assert!(fake_dir.join("$IABC123.txt").exists());
        // Un id que no es un `$R`, o cuyo `$R` sigue existiendo.
        let not_r = BinItem {
            id: fake_dir.join("otra.txt").into_os_string(),
            ..item.clone()
        };
        assert!(!remove_orphan_meta(&not_r));
        std::fs::write(fake_dir.join("$RABC123.txt"), b"datos").unwrap();
        assert!(!remove_orphan_meta(&item));
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
        recycle_paths(&paths, RecycleMode::Normal).unwrap();
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
        let res = recycle_paths(std::slice::from_ref(&lp), RecycleMode::Normal);
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
        let _ = recycle_paths(&[p], RecycleMode::WithNukeWarning);
    }
}
