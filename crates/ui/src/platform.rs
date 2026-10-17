//! Queries Windows that needs the interface. Here lives ALL the `unsafe` of the crate, in small functions with
//! its `// SAFETY:` and a test that actually runs them (Windows is the only destination for the product).
//!
//! Everything is READ ONLY or opens a window of the system itself (Properties); nothing writes or deletes files.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{FALSE, TRUE};
use windows_sys::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree, CoUninitialize,
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

// `GetDriveTypeW` values ​​(they are in `Win32_System_WindowsProgramming`, which is not needed for anything else).
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;

/// Unit type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    /// Fixed disk.
    Fixed,
    /// Removable drive (USB or card): no Recycle Bin.
    Removable,
    /// Network drive: has no Recycle Bin.
    Network,
}

/// A drive for the home screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveInfo {
    /// Drive letter (`C`).
    pub letter: char,
    /// Volume label (empty if none).
    pub label: String,
    pub kind: DriveKind,
    /// VOLUME capacity in bytes (not user quota).
    pub total: u64,
    /// Free bytes in the entire VOLUME. `total - free` is what is used: the use ring and the "unexplained space"
    /// They use only these two (same scope).
    pub free: u64,
    /// Bytes available to user (user scope): less than `free` if there is a disk quota. I just know
    /// shows as a separate note on the card; it is never subtracted from `total`.
    pub available: u64,
    /// `total` and `free` are from VOLUME. It is `false` with a quota when Windows does not give the real capacity: then they are
    /// the user's figures (consistent with each other) and the card notifies it.
    pub volume_scope: bool,
}

impl DriveInfo {
    /// Drive root (`C:\`), constructed at runtime.
    pub fn root(&self) -> PathBuf {
        PathBuf::from(format!("{}:\\", self.letter))
    }

    /// Percentage used, 0..=100.
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

/// System fonts folder (`SHGetKnownFolderPath(FOLDERID_Fonts)`), or `None` if Windows doesn't provide it.
pub fn fonts_dir() -> Option<PathBuf> {
    let mut p: *mut u16 = std::ptr::null_mut();
    // SAFETY: `FOLDERID_Fonts` is a valid constant; the null token means "the current user" and `p` is
    // writable. If it returns success, `p` points to a NUL-terminated UTF-16 string reserved by COM.
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_Fonts, 0, std::ptr::null_mut(), &mut p) };
    if hr != 0 || p.is_null() {
        if !p.is_null() {
            // SAFETY: according to the documentation, the pointer must also be released when the call fails.
            unsafe { CoTaskMemFree(p as *const c_void) };
        }
        return None;
    }
    let mut len = 0usize;
    // SAFETY: `p` points to a NUL-terminated string (it was just reserved by the correct call above), so
    // it is not read beyond the terminator.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: The first `len` elements are valid and initialized.
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) });
    // SAFETY: `p` was reserved by COM and has not been released before; It is not used again afterwards.
    unsafe { CoTaskMemFree(p as *const c_void) };
    Some(PathBuf::from(s))
}

/// Does the user have client area animations (`SPI_GETCLIENTAREAANIMATION`) enabled?
/// If Windows is not responding, it is assumed that it is.
pub fn client_area_animation_enabled() -> bool {
    let mut enabled: i32 = TRUE;
    // SAFETY: for this `SPI_*` the fourth argument is a writable `BOOL*`; `enabled` occupies 4 bytes.
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

/// Letters and types of drives present (only local queries, without touching the media: no crash).
fn enumerate_drives() -> Vec<(char, DriveKind)> {
    // SAFETY: does not receive pointers; returns a bitmask with one letter per bit.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(&format!("{letter}:\\"));
        // SAFETY: `root` is a NUL-terminated UTF-16 string.
        let kind = match unsafe { GetDriveTypeW(root.as_ptr()) } {
            DRIVE_FIXED => DriveKind::Fixed,
            DRIVE_REMOVABLE => DriveKind::Removable,
            DRIVE_REMOTE => DriveKind::Network,
            _ => continue, // CD-ROM, RAM disk, or unknown drive types
        };
        out.push((letter, kind));
    }
    out
}

/// Space and label of a unit. MAY BE BLOCKED (network drive down, unresponsive media): Called from a
/// thread with deadline ([`drives`]). With `SEM_FAILCRITICALERRORS` in THIS thread, a missing media fails without opening the dialog
/// "Insert a disc."
fn query_drive(letter: char, kind: DriveKind) -> Option<DriveInfo> {
    let mut old: u32 = 0;
    // SAFETY: `old` is a writable `u32`; error mode is per thread and is restored upon completion.
    let changed = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, &mut old) } != FALSE;
    let root = wide(&format!("{letter}:\\"));
    // Capacity and free volume, and available to the user, separately (see `tesela_scanner::VolumeSpace`).
    let space = tesela_scanner::volume_space(Path::new(&format!("{letter}:\\")))
        .ok()
        .filter(|v| v.total > 0);
    let info = if let Some(space) = space {
        let mut name = [0u16; 261];
        // SAFETY: `root` ends in NUL; `name` has the indicated length; the rest of the outputs are optional (null).
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
        // SAFETY: Restore the value returned by the previous call in this same thread.
        unsafe { SetThreadErrorMode(old, std::ptr::null_mut()) };
    }
    info
}

/// Runs each job in its own thread and waits at most `deadline` TOTAL: those that don't finish on time give
/// `None` (his thread continues on its own, without holding anyone back). So a crashed network drive does not freeze the application.
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

/// Computer disk drives with their space. Each unit is consulted in its own thread with a deadline of 3 s: those that do not
/// respond (no medium, network down) are omitted without delaying the others. Call from a thread, not from the interface.
pub fn drives() -> Vec<DriveInfo> {
    drives_with_deadline(std::time::Duration::from_secs(3))
}

/// Like [`drives`], with the indicated term.
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

/// Open the Explorer Properties sheet for `path`. Returns `false` if Windows failed (nonexistent path...).
/// No error interface (`SEE_MASK_FLAG_NO_UI`): If it fails, no frame is output.
pub fn show_properties(path: &Path) -> bool {
    show_properties_impl(path, false)
}

/// Open Properties in its own thread (shell I/O should not block the interface). The thread initializes COM and waits for
/// for the shell to terminate (`SEE_MASK_NOASYNC`): otherwise, when the thread terminates, the sheet would close. Returns the thread (its result
/// is whether Windows was able to open it).
pub fn show_properties_async(path: PathBuf) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        // SAFETY: `CoInitializeEx` does not receive pointers (null = reserved) and is paired with `CoUninitialize` only if it succeeded.
        let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        let ok = show_properties_impl(&path, true);
        if hr >= 0 {
            // SAFETY: The previous call initialized COM on THIS thread.
            unsafe { CoUninitialize() };
        }
        ok
    })
}

/// `ShellExecuteExW` flags to open the Recycle Bin: no error interface (`NO_UI`) and SYNCHRONOUS (`NOASYNC`): with
/// COM initialized on the thread, the shell terminates the request before the thread terminates (without it, a terminating thread
/// You can leave the request halfway).
const RECYCLE_BIN_MASK: u32 = SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC;

/// Flags to open a web page in the browser (same reasons as [`RECYCLE_BIN_MASK`], in a constant
/// own so that each use has its test).
const URL_OPEN_MASK: u32 = SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC;

/// Verb and destination of the request to the shell (UTF-16 with trailing NUL). The call itself opens an Explorer window and
/// cannot be run in tests: this part, which decides WHAT is requested, is tested.
fn recycle_bin_request() -> (Vec<u16>, Vec<u16>) {
    (wide("open"), wide("shell:RecycleBinFolder"))
}

/// Open the Recycle Bin in Explorer (`shell:RecycleBinFolder`), in its own thread: shell I/O does not
/// you should block the interface. There is nothing to check: if Windows can't, nothing happens.
pub fn open_recycle_bin() {
    let (verb, file) = recycle_bin_request();
    shell_open_async(verb, file, RECYCLE_BIN_MASK);
}

/// Opens `url` in the default browser (shell verb `open`), in its own thread. ONLY open the page of one
/// version of this repository as built by `tesela_update::release_url` (the exact prefix plus a tag
/// valid `vX.Y.Z` and nothing else: `tesela_update::is_release_page`); anything else is discarded without doing anything.
pub fn open_url_async(url: &str) {
    if tesela_update::is_release_page(url) {
        shell_open_async(wide("open"), wide(url), URL_OPEN_MASK);
    }
}

/// Ask the shell for `verb` about `file` (UTF-16 with trailing NUL) from a thread of its own, with COM initialized and those `mask`.
fn shell_open_async(verb: Vec<u16>, file: Vec<u16>, mask: u32) {
    std::thread::spawn(move || {
        // `COINIT_DISABLE_OLE1DDE`: The combination recommended by the `ShellExecuteExW` documentation (without OLE1 DDE support).
        // SAFETY: `CoInitializeEx` does not receive pointers (null = reserved) and is paired with `CoUninitialize` only if it succeeded.
        let hr = unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        // SAFETY: `SHELLEXECUTEINFOW` is an integer and pointer structure for which all zero is a valid value
        // (null pointers); The fields used by this call are filled in below.
        let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = mask;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.nShow = SW_SHOW;
        // SAFETY: `info` is complete and its strings (`verb`, `file`, NUL terminated) live during the call.
        unsafe { ShellExecuteExW(&mut info) };
        if hr >= 0 {
            // SAFETY: The previous call initialized COM on THIS thread.
            unsafe { CoUninitialize() };
        }
    });
}

fn show_properties_impl(path: &Path, wait: bool) -> bool {
    let file = wide_path(path);
    let verb = wide("properties");
    // SAFETY: `SHELLEXECUTEINFOW` is an integer and pointer structure for which all zero is a valid value
    // (null pointers); The fields used by this call are filled in below.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask =
        SEE_MASK_INVOKEIDLIST | SEE_MASK_FLAG_NO_UI | if wait { SEE_MASK_NOASYNC } else { 0 };
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.nShow = SW_SHOW;
    // SAFETY: `info` is initialized to its actual size; `verb` and `file` end in NUL and live until the
    // call returns (`SEE_MASK_INVOKEIDLIST` without `NOASYNC` does not use them afterwards).
    unsafe { ShellExecuteExW(&mut info) != FALSE }
}

/// Why couldn't Explorer open? TYPING: the interface translates it (the `HRESULT` code is separate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevealError {
    /// The path is useless (empty or with a NUL inside).
    InvalidPath,
    /// `SHParseDisplayName` could not create the route identifier (`HRESULT`).
    ParsePath(i32),
    /// `SHOpenFolderAndSelectItems` failed (`HRESULT`).
    Open(i32),
}

impl RevealError {
    /// The Windows `HRESULT`, if there is one.
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

/// Shell path identifier (PIDL), freed when dropped.
struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: The pointer was reserved by `SHParseDisplayName` (with the COM allocator) and is only freed here, once.
            unsafe { ILFree(self.0) };
        }
    }
}

/// Creates the PIDL of the route from its EXACT UTF-16 (`path.as_os_str().encode_wide()`, bypassing `str`): like this
/// names with single code units (which `display()` or `to_string_lossy()` would change to `U+FFFD`) arrive
/// intact to the shell. `\\?\` prefixes are removed if safe (`dunce`), because they are not well understood by the shell.
fn parse_pidl(path: &Path) -> Result<Pidl, RevealError> {
    let simple = dunce::simplified(path);
    let wide: Vec<u16> = simple.as_os_str().encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(RevealError::InvalidPath);
    }
    let mut z = wide;
    z.push(0);
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: `z` terminates in NUL and lives until the call returns; binding context is null (allowed), `pidl` is
    // writable, and the input and output attributes are 0 and null (not requested).
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
            // SAFETY: Even if it fails, the pointer it left behind is the COM allocator and is freed once.
            unsafe { ILFree(pidl) };
        }
        return Err(RevealError::ParsePath(if hr < 0 { hr } else { -1 }));
    }
    Ok(Pidl(pidl))
}

/// `SHOpenFolderAndSelectItems` with a full PIDL: Open the folder containing the item and select it (with
/// `cidl = 0`, `pidl` itself is the element). Returns the `HRESULT`.
///
/// # Safety
/// `pidl` must be null (the API rejects it with an error) or point to a valid PIDL that lives during the call.
/// COM must be initialized on this thread.
unsafe fn shell_open_and_select(pidl: *const ITEMIDLIST) -> i32 {
    // SAFETY: guaranteed by the caller; `apidl` is null with `cidl = 0` and the flags are 0.
    unsafe { SHOpenFolderAndSelectItems(pidl, 0, std::ptr::null(), 0) }
}

/// It does the work on the current thread, which already has COM initialized.
fn reveal_on_this_thread(path: &Path) -> Result<(), RevealError> {
    let pidl = parse_pidl(path)?;
    // SAFETY: `pidl` is a valid PIDL that lives until the end of the function; COM is initialized (it is done by the caller).
    let hr = unsafe { shell_open_and_select(pidl.0) };
    if hr < 0 {
        return Err(RevealError::Open(hr));
    }
    Ok(())
}

/// Period after which you are warned that the shell is not responding.
pub const REVEAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Stop live opening threads at the same time: with a hung shell no threads accumulate.
pub const MAX_LIVE_REVEALS: usize = 2;

/// Opening threads live right now.
static LIVE_REVEALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How a request to open Explorer ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevealOutcome {
    /// The shell accepted the request.
    Done,
    /// The shell rejected it (type error, with its `HRESULT`).
    Failed(RevealError),
    /// The deadline passed without a response: the thread continues on its own and is no longer expected.
    NoResponse,
    /// There are already too many unfinished requests (shell not responding): this one was not even attempted.
    Busy,
}

/// An in-progress request. It never blocks: the interface queries it with [`RevealHandle::poll`] each frame.
pub struct RevealHandle {
    rx: crossbeam_channel::Receiver<Result<(), RevealError>>,
    started: std::time::Instant,
    deadline: std::time::Duration,
}

/// Result of starting the request.
pub enum RevealStart {
    Started(RevealHandle),
    /// The limit of live wires has been reached.
    Busy,
}

impl RevealHandle {
    /// `Some` when there is an outcome: the shell result, or `NoResponse` if the deadline has passed (after which the
    /// caller releases the handle). `None` if still waiting.
    pub fn poll(&mut self) -> Option<RevealOutcome> {
        use crossbeam_channel::TryRecvError;
        match self.rx.try_recv() {
            Ok(Ok(())) => Some(RevealOutcome::Done),
            Ok(Err(e)) => Some(RevealOutcome::Failed(e)),
            // The thread ended without results (a panic): E_FAIL.
            Err(TryRecvError::Disconnected) => Some(RevealOutcome::Failed(RevealError::Open(
                0x8000_4005u32 as i32,
            ))),
            Err(TryRecvError::Empty) => {
                (self.started.elapsed() >= self.deadline).then_some(RevealOutcome::NoResponse)
            }
        }
    }
}

/// Discount a live opening thread upon completion (also if you panic).
struct LiveGuard(&'static std::sync::atomic::AtomicUsize);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Runs `work` on a background thread, limiting the number of active threads tracked by `live` to `max`.
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
        // The closure was discarded, so `guard` was dropped and the counter is up to date.
        return RevealStart::Busy;
    }
    RevealStart::Started(RevealHandle {
        rx,
        started: std::time::Instant::now(),
        deadline,
    })
}

/// The job of opening Explorer: This thread initializes COM (the shell needs it) and makes the call.
fn reveal_work(path: &Path) -> Result<(), RevealError> {
    // SAFETY: `CoInitializeEx` does not receive pointers (null = reserved) and matches `CoUninitialize` only if it succeeded
    // (a negative `HRESULT` means it was not initialized in this thread.)
    let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    let out = reveal_on_this_thread(path);
    if hr >= 0 {
        // SAFETY: The previous call initialized COM on THIS thread.
        unsafe { CoUninitialize() };
    }
    out
}

/// Opens Explorer with `path` selected (or its containing folder, if it is a directory) using
/// `SHOpenFolderAndSelectItems` and a PIDL created from the exact UTF-16 path. This supports names containing UTF-16 code
/// units that cannot be represented as valid text, which are lost by `explorer.exe /select,"…"`. It does not block the
/// interface thread: the request runs on a background thread, with at most [`MAX_LIVE_REVEALS`] active at a time.
/// [`RevealHandle::poll`] collects the result; if the shell does not respond before [`REVEAL_DEADLINE`], it returns
/// `NoResponse` and the interface displays a warning.
pub fn reveal_in_explorer_async(path: &Path) -> RevealStart {
    let path = path.to_path_buf();
    spawn_reveal(
        move || reveal_work(&path),
        &LIVE_REVEALS,
        MAX_LIVE_REVEALS,
        REVEAL_DEADLINE,
    )
}

/// The main window as parent of system dialogs (folder selector): so the dialog is ELLA modal
/// (it stays on top and does not get lost behind) instead of floating without an owner. Save only the `HWND`; the selector uses it inside
/// of the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParentWindow(std::num::NonZeroIsize);

impl ParentWindow {
    /// The parent indicating a window handle of `raw-window-handle` (`None` if non-Win32).
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
        // SAFETY: the `HWND` is the one in the main window, which lives the entire process; the folder selector queries it
        // during the call itself and is blocked (modal dialog) until closed, before the window can be destroyed.
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
    fn abrir_la_papelera_pide_sincrono_y_sin_interfaz_de_error() {
        // GM-m7: The request carries SEE_MASK_NOASYNC (with CoInitializeEx/CoUninitialize in the thread) and NO_UI.
        assert_eq!(RECYCLE_BIN_MASK & SEE_MASK_NOASYNC, SEE_MASK_NOASYNC);
        assert_eq!(RECYCLE_BIN_MASK & SEE_MASK_FLAG_NO_UI, SEE_MASK_FLAG_NO_UI);
        let src = include_str!("platform.rs");
        // The Recycle Bin is opened by the shared shell thread (`shell_open_async`), which initializes COM.
        let open = &src[src.find("pub fn open_recycle_bin").unwrap()..];
        let open = &open[..open.find("\n}\n").unwrap()];
        assert!(open.contains("shell_open_async") && open.contains("RECYCLE_BIN_MASK"));
        // Opening a page uses ITS mask (synchronous and no error interface) and only after the version page predicate.
        assert_eq!(URL_OPEN_MASK & SEE_MASK_NOASYNC, SEE_MASK_NOASYNC);
        assert_eq!(URL_OPEN_MASK & SEE_MASK_FLAG_NO_UI, SEE_MASK_FLAG_NO_UI);
        let url = &src[src.find("pub fn open_url_async").unwrap()..];
        let url = &url[..url.find("}\n}\n").unwrap()];
        assert!(url.contains("is_release_page") && url.contains("URL_OPEN_MASK"));
        assert!(url.find("is_release_page").unwrap() < url.find("shell_open_async").unwrap());
        let body = &src[src.find("fn shell_open_async").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(body.contains("CoInitializeEx") && body.contains("CoUninitialize"));
        assert!(body.contains("fMask = mask"));
    }

    #[test]
    fn la_ventana_padre_entrega_su_hwnd_al_selector_de_carpeta() {
        // SAFETY: takes no arguments; returns the HWND of the desktop (valid throughout the session).
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
        // Round trip: what comes out of `from_handle` is the same parent.
        assert_eq!(ParentWindow::from_handle(&p), Some(p));
    }

    #[test]
    fn la_carpeta_de_fuentes_existe() {
        let d = fonts_dir().expect("carpeta de fuentes");
        assert!(d.is_dir(), "{d:?}");
    }

    #[test]
    fn las_animaciones_se_consultan_sin_fallar() {
        // Either value is valid; what is tested is that the call is executed.
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
        // A path that does not exist under %TEMP%: The call is executed and Windows rejects it without displaying a box.
        let p = std::env::temp_dir().join(format!("tesela-test-{}-no-existe", std::process::id()));
        assert!(!p.exists());
        assert!(!show_properties(&p));
    }

    /// Initialize COM in this thread (as `reveal_work` does in yours) and run `f`.
    fn with_com<T>(f: impl FnOnce() -> T) -> T {
        // SAFETY: `CoInitializeEx` does not receive pointers (null = reserved); matches `CoUninitialize` if successful.
        let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        let out = f();
        if hr >= 0 {
            // SAFETY: The previous call initialized COM on THIS thread.
            unsafe { CoUninitialize() };
        }
        out
    }

    /// Own temporary folder (only what was created here, under the temporary folder, is deleted).
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
        // Useless routes: the shell is not even called.
        assert_eq!(reveal_work(Path::new("")), Err(RevealError::InvalidPath));
        let with_nul = PathBuf::from(std::ffi::OsString::from_wide(&[
            b'a' as u16,
            0,
            b'b' as u16,
        ]));
        assert_eq!(reveal_work(&with_nul), Err(RevealError::InvalidPath));
        // The error text is technical and neutral (the interface translates it).
        let text = RevealError::ParsePath(-2147024894).to_string();
        assert!(text.contains("0x80070002"), "{text}");
        assert_eq!(RevealError::InvalidPath.hresult(), None);
    }

    #[test]
    fn el_pidl_se_crea_desde_el_utf16_exacto_aunque_el_nombre_no_sea_texto() {
        use std::ffi::OsString;
        let t = Temp::new();
        // A name with an unpaired UTF-16 code unit (valid in NTFS, not representable as UTF-8 text).
        let name = OsString::from_wide(&[b'x' as u16, 0xD800, b'y' as u16]);
        let weird = t.0.join(&name);
        std::fs::write(&weird, b"x").unwrap();
        assert!(
            weird.to_string_lossy().contains('\u{FFFD}'),
            "el nombre no es texto"
        );
        with_com(|| {
            // The EXACT path is resolved...
            assert!(
                parse_pidl(&weird).is_ok(),
                "la ruta exacta debería resolverse"
            );
            // ...and the "lossy" version (what `path.display()` did) points to another file that doesn't exist.
            let lossy = PathBuf::from(weird.to_string_lossy().into_owned());
            assert!(matches!(parse_pidl(&lossy), Err(RevealError::ParsePath(_))));
            // A normal folder too.
            assert!(parse_pidl(&t.0).is_ok());
            // Prefixed with `\\?\` (canonical scanner routes) as well.
            let verbatim = PathBuf::from(format!(r"\\?\{}", t.0.display()));
            assert!(parse_pidl(&verbatim).is_ok());
        });
    }

    #[test]
    fn la_llamada_al_shell_rechaza_un_pidl_nulo_sin_abrir_ninguna_ventana() {
        let hr = with_com(|| {
            // SAFETY: a null PIDL is allowed by the function contract (the API rejects it with an error); COM is
            // initialized in this thread.
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
        // With a short deadline, every result is collected.
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
        // The quota does not change the percentage of volume used.
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
        // Launching is immediate (shell I/O goes in another thread).
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
                let _ = rx.recv(); // Simulate an unresponsive shell until the sender is dropped.
                Ok(())
            }
        };
        let short = std::time::Duration::from_millis(60);
        // Two live threads (the top of the test) and a third rejected without launching anything.
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
        // After the deadline without a response, `poll` notifies (not before).
        assert_eq!(a.poll(), None);
        assert_eq!(wait_for(|| a.poll()), RevealOutcome::NoResponse);
        assert_eq!(wait_for(|| b.poll()), RevealOutcome::NoResponse);
        // Threads are still alive (hanging): the cap still applies.
        assert_eq!(TEST_LIVE.load(Ordering::SeqCst), 2);
        assert!(matches!(
            spawn_reveal(mk(release_rx.clone()), &TEST_LIVE, 2, short),
            RevealStart::Busy
        ));
        // When the shell responds, the threads terminate and the counter returns to 0.
        drop(release_tx);
        wait_for(|| (TEST_LIVE.load(Ordering::SeqCst) == 0).then_some(()));
        // With free space you can launch again; and a job that ends well gives `Done`.
        let RevealStart::Started(mut ok) =
            spawn_reveal(|| Ok(()), &TEST_LIVE, 2, std::time::Duration::from_secs(5))
        else {
            panic!()
        };
        assert_eq!(wait_for(|| ok.poll()), RevealOutcome::Done);
        // A job that fails gives the error; a panic in the thread does not leave the counter up.
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

    #[test]
    fn la_peticion_de_abrir_la_papelera_es_la_carpeta_de_la_shell() {
        let (verb, file) = recycle_bin_request();
        assert_eq!(String::from_utf16_lossy(&verb), "open\0");
        assert_eq!(String::from_utf16_lossy(&file), "shell:RecycleBinFolder\0");
    }
}
