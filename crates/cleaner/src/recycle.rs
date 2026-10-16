//! La Papelera de reciclaje: enviar rutas, listar su contenido y purgar una lista enumerada, con COM (`windows`).
//!
//! `IFileOperation` trabaja por RUTAS y no se puede atar a un handle: por eso el borrado a la Papelera renombra antes el
//! objeto por el handle verificado a una lápida (ver `exec`) y solo entonces entrega la ruta lápida a este módulo.
//! Aquí no se decide nada: se ejecuta, y quien llama VERIFICA cada elemento (sigue ahí, o está en la Papelera, o
//! desapareció sin llegar a ella).
//!
//! Banderas (decididas por las pruebas, ver `tests::wantnukewarning_*`):
//! * enviar: `FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE`;
//! * purgar elementos de la Papelera: `FOF_NO_UI`.
#![allow(unsafe_code)]

use std::ffi::{OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{FILETIME, PROPERTYKEY};
use windows::Win32::System::Com::StructuredStorage::PropVariantToBSTR;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
#[cfg(test)]
use windows::Win32::UI::Shell::FOF_WANTNUKEWARNING;
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOF_ALLOWUNDO, FOF_NO_UI, FOFX_RECYCLEONDELETE, FOLDERID_RecycleBinFolder,
    FileOperation, IEnumShellItems, IFileOperation, IShellItem, IShellItem2, KF_FLAG_DEFAULT,
    PID_DISPLACED_DATE, PID_DISPLACED_FROM, PSGUID_DISPLACED, SHCreateItemFromParsingName,
    SHGetKnownFolderItem, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_PARENTRELATIVE,
};
use windows::core::{Interface, PCWSTR, PWSTR};

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
    /// Nombre con el que se ve en la Papelera (el original, o el de la lápida).
    pub name: OsString,
    /// Carpeta de la que se borró.
    pub original_parent: PathBuf,
    /// Cuándo se borró (segundos Unix).
    pub time_deleted: i64,
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
            out.push(BinItem {
                id,
                name,
                original_parent,
                time_deleted,
            });
        }
        Ok(out)
    }
}

/// Resultado de una operación por lotes de la shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchResult {
    /// `GetAnyOperationsAborted`: la shell abortó al menos una operación (sin decir cuál).
    pub any_aborted: bool,
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

/// Envía las rutas (una operación `IFileOperation` con todas) a la Papelera. Las rutas son LÁPIDAS ya renombradas por
/// handle. No dice qué elementos llegaron: se verifican aparte.
pub fn recycle_paths(paths: &[PathBuf], mode: RecycleMode) -> Result<BatchResult> {
    if paths.is_empty() {
        return Ok(BatchResult { any_aborted: false });
    }
    let _com = Com::init()?;
    let flags = match mode {
        RecycleMode::Normal => FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE,
        #[cfg(test)]
        RecycleMode::WithNukeWarning => {
            FOF_NO_UI | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_WANTNUKEWARNING
        }
    };
    // SAFETY: COM está inicializado en este hilo; las cadenas de análisis (`names`) viven durante todas las llamadas.
    unsafe {
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
        op.SetOperationFlags(flags)?;
        let mut added = 0usize;
        for p in paths {
            let name = parsing_name(p);
            // Una ruta que la shell no entiende (demasiado larga...) no aborta el lote: ese elemento queda sin enviar y
            // quien llama lo verá (sigue existiendo).
            let Ok(item) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(name.as_ptr()), None)
            else {
                continue;
            };
            if op.DeleteItem(&item, None).is_ok() {
                added += 1;
            }
        }
        if added == 0 {
            return Ok(BatchResult { any_aborted: false });
        }
        op.PerformOperations()?;
        Ok(BatchResult {
            any_aborted: op.GetAnyOperationsAborted()?.as_bool(),
        })
    }
}

/// Purga (borra definitivamente) SOLO los elementos de la lista, de la Papelera. Quien llama pasa una lista enumerada
/// al confirmar: lo que haya entrado después no se toca.
pub fn purge_items(items: &[BinItem]) -> Result<BatchResult> {
    if items.is_empty() {
        return Ok(BatchResult { any_aborted: false });
    }
    let _com = Com::init()?;
    // SAFETY: COM está inicializado en este hilo; las cadenas de análisis viven durante todas las llamadas.
    unsafe {
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
        op.SetOperationFlags(FOF_NO_UI)?;
        let mut added = 0usize;
        for it in items {
            let id: Vec<u16> = it.id.encode_wide().chain(Some(0)).collect();
            let Ok(item) =
                SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(id.as_ptr()), None)
            else {
                continue;
            };
            if op.DeleteItem(&item, None).is_ok() {
                added += 1;
            }
        }
        if added == 0 {
            return Ok(BatchResult { any_aborted: false });
        }
        op.PerformOperations()?;
        Ok(BatchResult {
            any_aborted: op.GetAnyOperationsAborted()?.as_bool(),
        })
    }
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
                let _ = purge_items(&mine);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::bintest::*;
    use super::*;
    use crate::testutil::{Sandbox, bin_lock, sparse_file};
    use std::collections::HashSet;

    fn tomb(name: &str) -> String {
        format!("{name}.tesela-{}", uuid::Uuid::new_v4().simple())
    }

    #[test]
    fn enviar_listar_y_purgar_solo_elementos_propios() {
        let _lock = bin_lock();
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        sb.write("p/uno.txt", 1000);
        sb.write("p/carpeta/x.bin", 2000);
        let (t1, t2) = (tomb("uno"), tomb("carpeta"));
        std::fs::rename(sb.path("p/uno.txt"), sb.path(&format!("p/{t1}"))).unwrap();
        std::fs::rename(sb.path("p/carpeta"), sb.path(&format!("p/{t2}"))).unwrap();
        let paths = vec![sb.long(&format!("p/{t1}")), sb.long(&format!("p/{t2}"))];
        let r = recycle_paths(&paths, RecycleMode::Normal).unwrap();
        assert!(!r.any_aborted);
        assert!(
            paths.iter().all(|p| !p.exists()),
            "las lápidas ya no están en su sitio"
        );
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
        assert!(names.contains(&t1) && names.contains(&t2));
        assert!(mine.iter().all(|i| i.original_parent == sb.long("p")));
        // Los ajenos no se tocan: los ids de fuera de los míos son los de antes.
        let foreign: HashSet<OsString> = items
            .iter()
            .filter(|i| !is_mine(i, &sb.uuid()))
            .map(|i| i.id.clone())
            .collect();
        assert_eq!(foreign, before);
        // Purga SOLO la lista enumerada.
        let r = purge_items(&mine).unwrap();
        assert!(!r.any_aborted);
        assert_eq!(ids(), before, "la Papelera queda exactamente como estaba");
    }

    #[test]
    fn purgar_una_lista_vacia_no_hace_nada() {
        let _lock = bin_lock();
        let before = ids();
        assert_eq!(
            purge_items(&[]).unwrap(),
            BatchResult { any_aborted: false }
        );
        assert_eq!(
            recycle_paths(&[], RecycleMode::Normal).unwrap(),
            BatchResult { any_aborted: false }
        );
        assert_eq!(ids(), before);
    }

    /// Qué pasó con un elemento tras `recycle_paths`.
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
    ///    elemento (sigue ahí / en la Papelera / desapareció sin llegar a ella).
    /// 2. Con `FOF_WANTNUKEWARNING` añadida, la shell no avisa por el código de retorno: se queda esperando una
    ///    respuesta que `FOF_NO_UI` impide dar. Por eso NO se usa nunca (la variante solo existe en pruebas) y esta
    ///    parte se ejecuta en un PROCESO HIJO con plazo: si se cuelga se mata, sin dejar hilos ni diálogos huérfanos.
    #[test]
    fn wantnukewarning_con_no_ui_y_un_elemento_que_no_cabe() {
        let _lock = bin_lock();
        let before = ids();
        let sb = Sandbox::new();
        let _guard = BinGuard { uuid: sb.uuid() };
        let total = tesela_scanner::volume_space(&sb.long_root())
            .map(|v| v.total)
            .unwrap_or(1 << 40);
        let len = total.saturating_mul(2).max(1 << 40);

        // (1) Banderas de la aplicación: la llamada vuelve y el desenlace se distingue verificando.
        let t = tomb("enorme-normal");
        let p = sb.path(&format!("n/{t}"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        sparse_file(&p, len);
        let lp = sb.long(&format!("n/{t}"));
        let res = recycle_paths(std::slice::from_ref(&lp), RecycleMode::Normal);
        let f = fate(&lp, &list_bin().unwrap(), &sb.uuid());
        eprintln!(
            "banderas de la aplicación sobre {} GiB lógicos: retorno = {:?}, desenlace = {:?}",
            len >> 30,
            res.as_ref()
                .map(|r| r.any_aborted)
                .map_err(|e| e.to_string()),
            f
        );
        assert!(res.is_ok());
        assert!(matches!(
            f,
            Fate::StillThere | Fate::InBin | Fate::GoneNotInBin
        ));

        // (2) Con FOF_WANTNUKEWARNING, en un proceso hijo con plazo.
        let t2 = tomb("enorme-nukewarning");
        let p2 = sb.path(&format!("n/{t2}"));
        sparse_file(&p2, len);
        let lp2 = sb.long(&format!("n/{t2}"));
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
        purge_items(&mine).unwrap();
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
        // Solo actúa sobre una lápida de la propia prueba, dentro de su carpeta temporal.
        let s = p.to_string_lossy().to_lowercase();
        assert!(s.contains("tesela-test-") && s.contains("enorme-nukewarning.tesela-"));
        let _ = recycle_paths(&[p], RecycleMode::WithNukeWarning);
    }
}
