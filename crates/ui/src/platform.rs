//! Consultas a Windows que necesita la interfaz. Aquí vive TODO el `unsafe` del crate, en funciones pequeñas con
//! su `// SAFETY:` y un test que las ejecuta de verdad (Windows es el único destino del producto).
//!
//! Todo es de SOLO LECTURA o abre una ventana del propio sistema (Propiedades); nada escribe ni borra archivos.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{FALSE, TRUE};
use windows_sys::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows_sys::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode};
use windows_sys::Win32::UI::Shell::Common::ITEMIDLIST;
use windows_sys::Win32::UI::Shell::{
    FOLDERID_Fonts, ILFree, SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC,
    SHELLEXECUTEINFOW, SHGetKnownFolderPath, SHOpenFolderAndSelectItems, SHParseDisplayName,
    ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SW_SHOW, SystemParametersInfoW,
};

// Valores de `GetDriveTypeW` (están en `Win32_System_WindowsProgramming`, que no se necesita para nada más).
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;

/// Tipo de unidad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    /// Disco fijo.
    Fixed,
    /// Extraíble (USB, tarjeta): no tiene Papelera.
    Removable,
    /// Unidad de red: no tiene Papelera.
    Network,
}

/// Una unidad de disco para la pantalla de inicio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveInfo {
    /// Letra de unidad (`C`).
    pub letter: char,
    /// Etiqueta del volumen (vacía si no tiene).
    pub label: String,
    pub kind: DriveKind,
    /// Capacidad del VOLUMEN en bytes (no la cuota del usuario).
    pub total: u64,
    /// Bytes libres en el VOLUMEN entero. `total - free` es lo usado: el anillo de uso y el "espacio no explicado"
    /// usan solo estas dos (mismo ámbito).
    pub free: u64,
    /// Bytes disponibles para el usuario (ámbito usuario): menor que `free` si hay una cuota de disco. Solo se
    /// enseña como nota aparte en la tarjeta; nunca se resta de `total`.
    pub available: u64,
    /// `total` y `free` son del VOLUMEN. Es `false` con una cuota cuando Windows no da la capacidad real: entonces son
    /// las cifras del usuario (coherentes entre sí) y la tarjeta lo avisa.
    pub volume_scope: bool,
}

impl DriveInfo {
    /// Raíz de la unidad (`C:\`), construida en tiempo de ejecución.
    pub fn root(&self) -> PathBuf {
        PathBuf::from(format!("{}:\\", self.letter))
    }

    /// Porcentaje usado, 0..=100.
    pub fn used_percent(&self) -> u8 {
        if self.total == 0 {
            return 0;
        }
        let used = self.total.saturating_sub(self.free);
        ((used as u128 * 100 + self.total as u128 / 2) / self.total as u128).min(100) as u8
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_path(p: &Path) -> Vec<u16> {
    p.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Carpeta de fuentes del sistema (`SHGetKnownFolderPath(FOLDERID_Fonts)`), o `None` si Windows no la da.
pub fn fonts_dir() -> Option<PathBuf> {
    let mut p: *mut u16 = std::ptr::null_mut();
    // SAFETY: `FOLDERID_Fonts` es una constante válida; el token nulo significa "el usuario actual" y `p` es
    // escribible. Si devuelve éxito, `p` apunta a una cadena UTF-16 terminada en NUL reservada por COM.
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_Fonts, 0, std::ptr::null_mut(), &mut p) };
    if hr != 0 || p.is_null() {
        if !p.is_null() {
            // SAFETY: según la documentación hay que liberar el puntero también cuando la llamada falla.
            unsafe { CoTaskMemFree(p as *const c_void) };
        }
        return None;
    }
    let mut len = 0usize;
    // SAFETY: `p` apunta a una cadena terminada en NUL (la acaba de reservar la llamada correcta de arriba), así que
    // no se lee más allá del terminador.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: los `len` primeros elementos son válidos y están inicializados.
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) });
    // SAFETY: `p` lo reservó COM y no se ha liberado antes; no se vuelve a usar después.
    unsafe { CoTaskMemFree(p as *const c_void) };
    Some(PathBuf::from(s))
}

/// ¿Tiene el usuario activadas las animaciones del área de cliente (`SPI_GETCLIENTAREAANIMATION`)?
/// Si Windows no responde, se asume que sí.
pub fn client_area_animation_enabled() -> bool {
    let mut enabled: i32 = TRUE;
    // SAFETY: para este `SPI_*` el cuarto argumento es un `BOOL*` escribible; `enabled` ocupa 4 bytes.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            &mut enabled as *mut i32 as *mut c_void,
            0,
        )
    };
    ok == FALSE || enabled != FALSE
}

/// Letras y tipos de las unidades presentes (solo consultas locales, sin tocar el medio: no se bloquea).
fn enumerate_drives() -> Vec<(char, DriveKind)> {
    // SAFETY: no recibe punteros; devuelve una máscara de bits con una letra por bit.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(&format!("{letter}:\\"));
        // SAFETY: `root` es una cadena UTF-16 terminada en NUL.
        let kind = match unsafe { GetDriveTypeW(root.as_ptr()) } {
            DRIVE_FIXED => DriveKind::Fixed,
            DRIVE_REMOVABLE => DriveKind::Removable,
            DRIVE_REMOTE => DriveKind::Network,
            _ => continue, // CD-ROM, disco RAM, desconocidas
        };
        out.push((letter, kind));
    }
    out
}

/// Espacio y etiqueta de una unidad. PUEDE BLOQUEARSE (unidad de red caída, medio que no responde): se llama desde un
/// hilo con plazo ([`drives`]). Con `SEM_FAILCRITICALERRORS` en ESTE hilo, un medio ausente falla sin abrir el diálogo
/// "Inserte un disco".
fn query_drive(letter: char, kind: DriveKind) -> Option<DriveInfo> {
    let mut old: u32 = 0;
    // SAFETY: `old` es un `u32` escribible; el modo de error es por hilo y se restaura al terminar.
    let changed = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, &mut old) } != FALSE;
    let root = wide(&format!("{letter}:\\"));
    // Capacidad y libre del volumen, y disponible para el usuario, por separado (ver `tesela_scanner::VolumeSpace`).
    let space = tesela_scanner::volume_space(Path::new(&format!("{letter}:\\")))
        .ok()
        .filter(|v| v.total > 0);
    let info = if let Some(space) = space {
        let mut name = [0u16; 261];
        // SAFETY: `root` termina en NUL; `name` tiene la longitud indicada; el resto de salidas son opcionales (nulas).
        let got = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        let label = if got != FALSE {
            let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
            String::from_utf16_lossy(&name[..end])
        } else {
            String::new()
        };
        Some(DriveInfo {
            letter,
            label,
            kind,
            total: space.total,
            free: space.free,
            available: space.available,
            volume_scope: space.volume_scope,
        })
    } else {
        None
    };
    if changed {
        // SAFETY: restaura el valor que devolvió la llamada anterior en este mismo hilo.
        unsafe { SetThreadErrorMode(old, std::ptr::null_mut()) };
    }
    info
}

/// Ejecuta cada trabajo en su propio hilo y espera como mucho `deadline` EN TOTAL: los que no terminan a tiempo dan
/// `None` (su hilo sigue por su cuenta, sin retener a nadie). Así una unidad de red caída no congela la aplicación.
pub fn collect_with_deadline<T: Send + 'static>(
    jobs: Vec<Box<dyn FnOnce() -> Option<T> + Send>>,
    deadline: std::time::Duration,
) -> Vec<Option<T>> {
    let n = jobs.len();
    let (tx, rx) = crossbeam_channel::unbounded::<(usize, Option<T>)>();
    for (i, job) in jobs.into_iter().enumerate() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send((i, job()));
        });
    }
    drop(tx);
    let end = std::time::Instant::now() + deadline;
    let mut out: Vec<Option<T>> = (0..n).map(|_| None).collect();
    let mut got = 0;
    while got < n {
        let left = end.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok((i, v)) => {
                out[i] = v;
                got += 1;
            }
            Err(_) => break,
        }
    }
    out
}

/// Unidades de disco del equipo con su espacio. Cada unidad se consulta en su propio hilo con un plazo de 3 s: las que no
/// responden (sin medio, red caída) se omiten sin retrasar a las demás. Llamar desde un hilo, no desde el de la interfaz.
pub fn drives() -> Vec<DriveInfo> {
    drives_with_deadline(std::time::Duration::from_secs(3))
}

/// Como [`drives`], con el plazo indicado.
pub fn drives_with_deadline(deadline: std::time::Duration) -> Vec<DriveInfo> {
    let jobs: Vec<Box<dyn FnOnce() -> Option<DriveInfo> + Send>> = enumerate_drives()
        .into_iter()
        .map(|(letter, kind)| {
            Box::new(move || query_drive(letter, kind))
                as Box<dyn FnOnce() -> Option<DriveInfo> + Send>
        })
        .collect();
    collect_with_deadline(jobs, deadline)
        .into_iter()
        .flatten()
        .collect()
}

/// Abre la hoja de Propiedades del Explorador para `path`. Devuelve `false` si Windows no pudo (ruta inexistente...).
/// Sin interfaz de error (`SEE_MASK_FLAG_NO_UI`): si falla, no sale ningún cuadro.
pub fn show_properties(path: &Path) -> bool {
    show_properties_impl(path, false)
}

/// Abre las Propiedades en un hilo propio (la E/S del shell no debe bloquear la interfaz). El hilo inicializa COM y espera a
/// que el shell termine (`SEE_MASK_NOASYNC`): si no, al acabar el hilo se cerraría la hoja. Devuelve el hilo (su resultado
/// es si Windows pudo abrirla).
pub fn show_properties_async(path: PathBuf) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        // SAFETY: `CoInitializeEx` no recibe punteros (nulo = reservado) y se empareja con `CoUninitialize` solo si tuvo éxito.
        let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        let ok = show_properties_impl(&path, true);
        if hr >= 0 {
            // SAFETY: la llamada anterior inicializó COM en ESTE hilo.
            unsafe { CoUninitialize() };
        }
        ok
    })
}

fn show_properties_impl(path: &Path, wait: bool) -> bool {
    let file = wide_path(path);
    let verb = wide("properties");
    // SAFETY: `SHELLEXECUTEINFOW` es una estructura de enteros y punteros para la que todo cero es un valor válido
    // (punteros nulos); se rellenan a continuación los campos que usa esta llamada.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask =
        SEE_MASK_INVOKEIDLIST | SEE_MASK_FLAG_NO_UI | if wait { SEE_MASK_NOASYNC } else { 0 };
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.nShow = SW_SHOW;
    // SAFETY: `info` está inicializada con su tamaño real; `verb` y `file` terminan en NUL y viven hasta que la
    // llamada vuelve (`SEE_MASK_INVOKEIDLIST` sin `NOASYNC` no los usa después).
    unsafe { ShellExecuteExW(&mut info) != FALSE }
}

/// Por qué no se pudo abrir el Explorador. TIPADO: la interfaz lo traduce (el código `HRESULT` va aparte).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevealError {
    /// La ruta no sirve (vacía o con un NUL dentro).
    InvalidPath,
    /// `SHParseDisplayName` no pudo crear el identificador de la ruta (`HRESULT`).
    ParsePath(i32),
    /// `SHOpenFolderAndSelectItems` falló (`HRESULT`).
    Open(i32),
}

impl RevealError {
    /// El `HRESULT` de Windows, si lo hay.
    pub fn hresult(&self) -> Option<i32> {
        match self {
            RevealError::InvalidPath => None,
            RevealError::ParsePath(h) | RevealError::Open(h) => Some(*h),
        }
    }
}

impl std::fmt::Display for RevealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RevealError::InvalidPath => write!(f, "invalid path"),
            RevealError::ParsePath(h) => {
                write!(f, "SHParseDisplayName failed (HRESULT {:#010X})", *h as u32)
            }
            RevealError::Open(h) => write!(
                f,
                "SHOpenFolderAndSelectItems failed (HRESULT {:#010X})",
                *h as u32
            ),
        }
    }
}

impl std::error::Error for RevealError {}

/// Identificador de una ruta del shell (PIDL) que se libera al soltarlo.
struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: el puntero lo reservó `SHParseDisplayName` (con el asignador COM) y solo se libera aquí, una vez.
            unsafe { ILFree(self.0) };
        }
    }
}

/// Crea el PIDL de la ruta a partir de su UTF-16 EXACTO (`path.as_os_str().encode_wide()`, sin pasar por `str`): así
/// los nombres con unidades de código sueltas (que `display()` o `to_string_lossy()` cambiarían por `U+FFFD`) llegan
/// intactos al shell. Los prefijos `\\?\` se quitan si es seguro (`dunce`), porque el shell no los entiende bien.
fn parse_pidl(path: &Path) -> Result<Pidl, RevealError> {
    let simple = dunce::simplified(path);
    let wide: Vec<u16> = simple.as_os_str().encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(RevealError::InvalidPath);
    }
    let mut z = wide;
    z.push(0);
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: `z` termina en NUL y vive hasta que la llamada vuelve; el contexto de enlace es nulo (permitido), `pidl` es
    // escribible, y los atributos de entrada y salida son 0 y nulo (no se piden).
    let hr = unsafe {
        SHParseDisplayName(
            z.as_ptr(),
            std::ptr::null_mut(),
            &mut pidl,
            0,
            std::ptr::null_mut(),
        )
    };
    if hr < 0 || pidl.is_null() {
        if !pidl.is_null() {
            // SAFETY: aunque falle, el puntero que haya dejado es del asignador COM y se libera una vez.
            unsafe { ILFree(pidl) };
        }
        return Err(RevealError::ParsePath(if hr < 0 { hr } else { -1 }));
    }
    Ok(Pidl(pidl))
}

/// `SHOpenFolderAndSelectItems` con un PIDL completo: abre la carpeta que contiene el elemento y lo selecciona (con
/// `cidl = 0`, el propio `pidl` es el elemento). Devuelve el `HRESULT`.
///
/// # Safety
/// `pidl` debe ser nulo (la API lo rechaza con un error) o apuntar a un PIDL válido que viva durante la llamada. COM
/// tiene que estar inicializado en este hilo.
unsafe fn shell_open_and_select(pidl: *const ITEMIDLIST) -> i32 {
    // SAFETY: lo garantiza quien llama; `apidl` es nulo con `cidl = 0` y los flags son 0.
    unsafe { SHOpenFolderAndSelectItems(pidl, 0, std::ptr::null(), 0) }
}

/// Hace el trabajo en el hilo actual, que ya tiene COM inicializado.
fn reveal_on_this_thread(path: &Path) -> Result<(), RevealError> {
    let pidl = parse_pidl(path)?;
    // SAFETY: `pidl` es un PIDL válido que vive hasta el final de la función; COM está inicializado (lo hace quien llama).
    let hr = unsafe { shell_open_and_select(pidl.0) };
    if hr < 0 {
        return Err(RevealError::Open(hr));
    }
    Ok(())
}

/// Plazo tras el cual se avisa de que el shell no responde.
pub const REVEAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Tope de hilos de apertura vivos a la vez: con un shell colgado no se acumulan hilos.
pub const MAX_LIVE_REVEALS: usize = 2;

/// Hilos de apertura vivos ahora mismo.
static LIVE_REVEALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Cómo acabó una petición de abrir el Explorador.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevealOutcome {
    /// El shell aceptó la petición.
    Done,
    /// El shell la rechazó (error tipado, con su `HRESULT`).
    Failed(RevealError),
    /// Pasó el plazo sin respuesta: el hilo sigue por su cuenta y ya no se espera.
    NoResponse,
    /// Ya hay demasiadas peticiones sin terminar (el shell no responde): esta ni se intentó.
    Busy,
}

/// Una petición en curso. NUNCA bloquea: la interfaz la consulta con [`RevealHandle::poll`] en cada frame.
pub struct RevealHandle {
    rx: crossbeam_channel::Receiver<Result<(), RevealError>>,
    started: std::time::Instant,
    deadline: std::time::Duration,
}

/// Resultado de lanzar la petición.
pub enum RevealStart {
    Started(RevealHandle),
    /// El tope de hilos vivos está alcanzado.
    Busy,
}

impl RevealHandle {
    /// `Some` cuando hay un desenlace: el resultado del shell, o `NoResponse` si pasó el plazo (tras lo cual el
    /// llamador suelta el manejador). `None` si sigue esperándose.
    pub fn poll(&mut self) -> Option<RevealOutcome> {
        use crossbeam_channel::TryRecvError;
        match self.rx.try_recv() {
            Ok(Ok(())) => Some(RevealOutcome::Done),
            Ok(Err(e)) => Some(RevealOutcome::Failed(e)),
            // El hilo terminó sin dejar resultado (un pánico): E_FAIL.
            Err(TryRecvError::Disconnected) => Some(RevealOutcome::Failed(RevealError::Open(
                0x8000_4005u32 as i32,
            ))),
            Err(TryRecvError::Empty) => {
                (self.started.elapsed() >= self.deadline).then_some(RevealOutcome::NoResponse)
            }
        }
    }
}

/// Descuenta un hilo de apertura vivo al terminar (también si entra en pánico).
struct LiveGuard(&'static std::sync::atomic::AtomicUsize);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Lanza `work` en un hilo suelto con un tope de `max` hilos vivos en `live`.
fn spawn_reveal<F>(
    work: F,
    live: &'static std::sync::atomic::AtomicUsize,
    max: usize,
    deadline: std::time::Duration,
) -> RevealStart
where
    F: FnOnce() -> Result<(), RevealError> + Send + 'static,
{
    use std::sync::atomic::Ordering;
    if live.fetch_add(1, Ordering::SeqCst) >= max {
        live.fetch_sub(1, Ordering::SeqCst);
        return RevealStart::Busy;
    }
    let guard = LiveGuard(live);
    let (tx, rx) = crossbeam_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name("tesela-reveal".into())
        .spawn(move || {
            let _guard = guard;
            let _ = tx.send(work());
        });
    if spawned.is_err() {
        // `guard` se soltó con el cierre descartado: el contador ya está al día.
        return RevealStart::Busy;
    }
    RevealStart::Started(RevealHandle {
        rx,
        started: std::time::Instant::now(),
        deadline,
    })
}

/// El trabajo de abrir el Explorador: este hilo inicializa COM (el shell lo necesita) y hace la llamada.
fn reveal_work(path: &Path) -> Result<(), RevealError> {
    // SAFETY: `CoInitializeEx` no recibe punteros (nulo = reservado) y se empareja con `CoUninitialize` solo si tuvo éxito
    // (un `HRESULT` negativo significa que no se inicializó en este hilo).
    let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    let out = reveal_on_this_thread(path);
    if hr >= 0 {
        // SAFETY: la llamada anterior inicializó COM en ESTE hilo.
        unsafe { CoUninitialize() };
    }
    out
}

/// Abre el Explorador con `path` seleccionado (o su carpeta, si es un directorio) con `SHOpenFolderAndSelectItems` y un
/// PIDL creado desde la ruta UTF-16 EXACTA: funciona con nombres que no se pueden escribir como texto (unidades de código
/// sueltas), que con `explorer.exe /select,"…"` se perdían. NO bloquea el hilo de la interfaz: va en un hilo suelto (con un
/// tope de [`MAX_LIVE_REVEALS`] vivos a la vez) y el resultado se recoge con [`RevealHandle::poll`]; si el shell no
/// responde en [`REVEAL_DEADLINE`], `poll` da `NoResponse` y la interfaz avisa.
pub fn reveal_in_explorer_async(path: &Path) -> RevealStart {
    let path = path.to_path_buf();
    spawn_reveal(
        move || reveal_work(&path),
        &LIVE_REVEALS,
        MAX_LIVE_REVEALS,
        REVEAL_DEADLINE,
    )
}

/// La ventana principal como padre de los diálogos del sistema (selector de carpeta): así el diálogo es modal de ELLA
/// (se queda encima y no se pierde detrás) en lugar de flotar sin dueño. Guarda solo el `HWND`; el selector lo usa dentro
/// de la llamada.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParentWindow(std::num::NonZeroIsize);

impl ParentWindow {
    /// El padre que indica un manejador de ventana de `raw-window-handle` (`None` si no es de Win32).
    pub fn from_handle(w: &impl raw_window_handle::HasWindowHandle) -> Option<ParentWindow> {
        match w.window_handle().ok()?.as_raw() {
            raw_window_handle::RawWindowHandle::Win32(h) => Some(ParentWindow(h.hwnd)),
            _ => None,
        }
    }
}

impl raw_window_handle::HasWindowHandle for ParentWindow {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        let raw = raw_window_handle::RawWindowHandle::Win32(
            raw_window_handle::Win32WindowHandle::new(self.0),
        );
        // SAFETY: el `HWND` es el de la ventana principal, que vive todo el proceso; el selector de carpeta lo consulta
        // durante la propia llamada y se bloquea (diálogo modal) hasta cerrarse, antes de que la ventana pueda destruirse.
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
    }
}

impl raw_window_handle::HasDisplayHandle for ParentWindow {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(raw_window_handle::DisplayHandle::windows())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetDesktopWindow;

    #[test]
    fn la_ventana_padre_entrega_su_hwnd_al_selector_de_carpeta() {
        // SAFETY: no recibe argumentos; devuelve el HWND del escritorio (válido durante toda la sesión).
        let hwnd = unsafe { GetDesktopWindow() } as isize;
        let nz = std::num::NonZeroIsize::new(hwnd).expect("el escritorio tiene HWND");
        let p = ParentWindow(nz);
        match p.window_handle().unwrap().as_raw() {
            RawWindowHandle::Win32(h) => assert_eq!(h.hwnd, nz),
            other => panic!("manejador inesperado: {other:?}"),
        }
        assert!(matches!(
            p.display_handle().unwrap().as_raw(),
            RawDisplayHandle::Windows(_)
        ));
        // Ida y vuelta: lo que sale de `from_handle` es el mismo padre.
        assert_eq!(ParentWindow::from_handle(&p), Some(p));
    }

    #[test]
    fn la_carpeta_de_fuentes_existe() {
        let d = fonts_dir().expect("carpeta de fuentes");
        assert!(d.is_dir(), "{d:?}");
    }

    #[test]
    fn las_animaciones_se_consultan_sin_fallar() {
        // Cualquiera de los dos valores es válido; lo que se prueba es que la llamada se ejecuta.
        let _ = client_area_animation_enabled();
    }

    #[test]
    fn las_unidades_incluyen_la_del_sistema_con_espacio_coherente() {
        let drives = drives();
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let letter = sys.chars().next().unwrap().to_ascii_uppercase();
        let d = drives
            .iter()
            .find(|d| d.letter == letter)
            .expect("unidad del sistema");
        assert_eq!(d.kind, DriveKind::Fixed);
        assert!(d.total > 0 && d.free <= d.total);
        assert!(d.used_percent() <= 100);
        assert_eq!(d.root(), PathBuf::from(format!("{letter}:\\")));
    }

    #[test]
    fn las_propiedades_en_un_hilo_propio_tambien_fallan_sin_abrir_nada() {
        let p =
            std::env::temp_dir().join(format!("tesela-test-{}-no-existe", uuid::Uuid::new_v4()));
        assert!(
            !show_properties_async(p)
                .join()
                .expect("el hilo no entra en pánico")
        );
    }

    #[test]
    fn propiedades_de_una_ruta_inexistente_falla_sin_abrir_nada() {
        // Una ruta que no existe bajo %TEMP%: la llamada se ejecuta y Windows la rechaza sin mostrar ningún cuadro.
        let p = std::env::temp_dir().join(format!("tesela-test-{}-no-existe", std::process::id()));
        assert!(!p.exists());
        assert!(!show_properties(&p));
    }

    /// Inicializa COM en este hilo (como hace `reveal_work` en el suyo) y ejecuta `f`.
    fn with_com<T>(f: impl FnOnce() -> T) -> T {
        // SAFETY: `CoInitializeEx` no recibe punteros (nulo = reservado); se empareja con `CoUninitialize` si tuvo éxito.
        let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        let out = f();
        if hr >= 0 {
            // SAFETY: la llamada anterior inicializó COM en ESTE hilo.
            unsafe { CoUninitialize() };
        }
        out
    }

    /// Carpeta temporal propia (solo se borra lo creado aquí, bajo la carpeta temporal).
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Temp {
            let p = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&p).unwrap();
            Temp(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn abrir_el_explorador_con_una_ruta_inexistente_da_el_error_tipado_sin_abrir_nada() {
        let t = Temp::new();
        let missing = t.0.join("no-existe");
        match reveal_work(&missing) {
            Err(RevealError::ParsePath(hr)) => {
                assert!(hr < 0);
                assert_eq!(RevealError::ParsePath(hr).hresult(), Some(hr));
            }
            other => panic!("se esperaba ParsePath, salió {other:?}"),
        }
        // Rutas inservibles: ni se llama al shell.
        assert_eq!(reveal_work(Path::new("")), Err(RevealError::InvalidPath));
        let with_nul = PathBuf::from(std::ffi::OsString::from_wide(&[
            b'a' as u16,
            0,
            b'b' as u16,
        ]));
        assert_eq!(reveal_work(&with_nul), Err(RevealError::InvalidPath));
        // El texto del error es técnico y neutro (la interfaz lo traduce).
        let text = RevealError::ParsePath(-2147024894).to_string();
        assert!(text.contains("0x80070002"), "{text}");
        assert_eq!(RevealError::InvalidPath.hresult(), None);
    }

    #[test]
    fn el_pidl_se_crea_desde_el_utf16_exacto_aunque_el_nombre_no_sea_texto() {
        use std::ffi::OsString;
        let t = Temp::new();
        // Un nombre con una unidad de código suelta (válido en NTFS, no representable como texto UTF-8).
        let name = OsString::from_wide(&[b'x' as u16, 0xD800, b'y' as u16]);
        let weird = t.0.join(&name);
        std::fs::write(&weird, b"x").unwrap();
        assert!(
            weird.to_string_lossy().contains('\u{FFFD}'),
            "el nombre no es texto"
        );
        with_com(|| {
            // La ruta EXACTA se resuelve...
            assert!(
                parse_pidl(&weird).is_ok(),
                "la ruta exacta debería resolverse"
            );
            // ...y la versión "con pérdidas" (lo que hacía `path.display()`) apunta a otro archivo que no existe.
            let lossy = PathBuf::from(weird.to_string_lossy().into_owned());
            assert!(matches!(parse_pidl(&lossy), Err(RevealError::ParsePath(_))));
            // Una carpeta normal también.
            assert!(parse_pidl(&t.0).is_ok());
            // Con prefijo `\\?\` (rutas canónicas del escáner) también.
            let verbatim = PathBuf::from(format!(r"\\?\{}", t.0.display()));
            assert!(parse_pidl(&verbatim).is_ok());
        });
    }

    #[test]
    fn la_llamada_al_shell_rechaza_un_pidl_nulo_sin_abrir_ninguna_ventana() {
        let hr = with_com(|| {
            // SAFETY: un PIDL nulo está permitido por el contrato de la función (la API lo rechaza con un error); COM está
            // inicializado en este hilo.
            unsafe { shell_open_and_select(std::ptr::null()) }
        });
        assert!(hr < 0, "HRESULT {hr:#X}");
    }

    #[test]
    fn una_unidad_que_no_responde_no_retrasa_a_las_demas() {
        let t0 = std::time::Instant::now();
        let jobs: Vec<Box<dyn FnOnce() -> Option<u32> + Send>> = vec![
            Box::new(|| Some(1)),
            Box::new(|| {
                std::thread::sleep(std::time::Duration::from_secs(3));
                Some(2)
            }),
            Box::new(|| None),
            Box::new(|| Some(4)),
        ];
        let out = collect_with_deadline(jobs, std::time::Duration::from_millis(300));
        assert_eq!(out, vec![Some(1), None, None, Some(4)]);
        assert!(
            t0.elapsed().as_millis() < 1500,
            "esperó más del plazo: {:?}",
            t0.elapsed()
        );
        // Con un plazo holgado se recoge todo.
        let jobs: Vec<Box<dyn FnOnce() -> Option<u32> + Send>> =
            vec![Box::new(|| Some(7)), Box::new(|| Some(8))];
        assert_eq!(
            collect_with_deadline(jobs, std::time::Duration::from_secs(5)),
            vec![Some(7), Some(8)]
        );
    }

    #[test]
    fn porcentaje_usado() {
        let d = DriveInfo {
            letter: 'C',
            label: String::new(),
            kind: DriveKind::Fixed,
            total: 1000,
            free: 250,
            available: 250,
            volume_scope: true,
        };
        assert_eq!(d.used_percent(), 75);
        // La cuota no cambia el porcentaje usado del volumen.
        let q = DriveInfo {
            available: 10,
            ..d.clone()
        };
        assert_eq!(q.used_percent(), 75);
        let z = DriveInfo { total: 0, ..d };
        assert_eq!(z.used_percent(), 0);
    }

    static TEST_LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
        let t0 = std::time::Instant::now();
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(t0.elapsed().as_secs() < 20, "tiempo agotado");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn abrir_el_explorador_no_bloquea_y_devuelve_el_error_tipado_por_el_manejador() {
        let t = Temp::new();
        let t0 = std::time::Instant::now();
        let RevealStart::Started(mut h) = reveal_in_explorer_async(&t.0.join("no-existe")) else {
            panic!("no debería estar ocupado");
        };
        // Lanzar es inmediato (la E/S del shell va en otro hilo).
        assert!(t0.elapsed() < std::time::Duration::from_millis(500));
        let out = wait_for(|| h.poll());
        assert!(
            matches!(out, RevealOutcome::Failed(RevealError::ParsePath(hr)) if hr < 0),
            "{out:?}"
        );
    }

    #[test]
    fn con_el_shell_colgado_no_se_acumulan_hilos_y_se_avisa_de_que_no_responde() {
        use std::sync::atomic::Ordering;
        let (release_tx, release_rx) = crossbeam_channel::unbounded::<()>();
        let mk = |rx: crossbeam_channel::Receiver<()>| {
            move || {
                let _ = rx.recv(); // "el shell no responde" hasta que se suelte
                Ok(())
            }
        };
        let short = std::time::Duration::from_millis(60);
        // Dos hilos vivos (el tope de la prueba) y un tercero rechazado sin lanzar nada.
        let RevealStart::Started(mut a) =
            spawn_reveal(mk(release_rx.clone()), &TEST_LIVE, 2, short)
        else {
            panic!()
        };
        let RevealStart::Started(mut b) =
            spawn_reveal(mk(release_rx.clone()), &TEST_LIVE, 2, short)
        else {
            panic!()
        };
        assert_eq!(TEST_LIVE.load(Ordering::SeqCst), 2);
        assert!(matches!(
            spawn_reveal(mk(release_rx.clone()), &TEST_LIVE, 2, short),
            RevealStart::Busy
        ));
        assert_eq!(
            TEST_LIVE.load(Ordering::SeqCst),
            2,
            "el rechazado no deja rastro"
        );
        // Pasado el plazo sin respuesta, `poll` avisa (no antes).
        assert_eq!(a.poll(), None);
        assert_eq!(wait_for(|| a.poll()), RevealOutcome::NoResponse);
        assert_eq!(wait_for(|| b.poll()), RevealOutcome::NoResponse);
        // Los hilos siguen vivos (colgados): el tope sigue aplicándose.
        assert_eq!(TEST_LIVE.load(Ordering::SeqCst), 2);
        assert!(matches!(
            spawn_reveal(mk(release_rx.clone()), &TEST_LIVE, 2, short),
            RevealStart::Busy
        ));
        // Al responder el shell, los hilos terminan y el contador vuelve a 0.
        drop(release_tx);
        wait_for(|| (TEST_LIVE.load(Ordering::SeqCst) == 0).then_some(()));
        // Con sitio libre se vuelve a poder lanzar; y un trabajo que acaba bien da `Done`.
        let RevealStart::Started(mut ok) =
            spawn_reveal(|| Ok(()), &TEST_LIVE, 2, std::time::Duration::from_secs(5))
        else {
            panic!()
        };
        assert_eq!(wait_for(|| ok.poll()), RevealOutcome::Done);
        // Un trabajo que falla da el error; un pánico en el hilo no deja el contador subido.
        let RevealStart::Started(mut bad) = spawn_reveal(
            || Err(RevealError::Open(-5)),
            &TEST_LIVE,
            2,
            std::time::Duration::from_secs(5),
        ) else {
            panic!()
        };
        assert_eq!(
            wait_for(|| bad.poll()),
            RevealOutcome::Failed(RevealError::Open(-5))
        );
        let RevealStart::Started(mut boom) = spawn_reveal(
            || -> Result<(), RevealError> { panic!("fallo de prueba") },
            &TEST_LIVE,
            2,
            std::time::Duration::from_secs(5),
        ) else {
            panic!()
        };
        assert!(matches!(
            wait_for(|| boom.poll()),
            RevealOutcome::Failed(RevealError::Open(_))
        ));
        wait_for(|| (TEST_LIVE.load(Ordering::SeqCst) == 0).then_some(()));
    }
}
