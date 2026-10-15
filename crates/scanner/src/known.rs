//! Rutas conocidas que se escanean primero (cola prioritaria, PLAN.md §3.3) y las dos consultas de Windows que
//! necesitan: el SID del usuario actual y la carpeta Descargas. SOLO LECTURA.
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath};

/// Longitud de una cadena UTF-16 terminada en NUL.
///
/// # Safety
/// `p` debe apuntar a una cadena UTF-16 válida terminada en NUL.
unsafe fn wcslen(p: *const u16) -> usize {
    let mut n = 0;
    // SAFETY: lo garantiza quien llama: la cadena termina en NUL, así que no se lee más allá del terminador.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    n
}

/// SID del usuario del proceso en texto (`S-1-5-21-...`), o `None` si Windows no lo da.
pub fn current_user_sid() -> Option<String> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: el pseudo-handle del proceso actual siempre es válido; `token` es escribible.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let sid = (|| {
        // Primera llamada: averigua el tamaño necesario (falla a propósito con ERROR_INSUFFICIENT_BUFFER).
        let mut needed = 0u32;
        // SAFETY: `token` es válido; búfer nulo de longitud 0 es la forma documentada de preguntar el tamaño.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return None;
        }
        // `u64` para garantizar la alineación que necesita TOKEN_USER.
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: `buf` es escribible, alineado y de al menos `needed` bytes; `token` es válido.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr() as *mut _,
                (buf.len() * 8) as u32,
                &mut needed,
            )
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: la llamada anterior rellenó `buf` con un TOKEN_USER (la memoria está alineada a 8 y es válida).
        let sid_ptr = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
        let mut wide: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid_ptr` apunta a un SID válido dentro de `buf`; `wide` es escribible.
        if unsafe { ConvertSidToStringSidW(sid_ptr, &mut wide) } == 0 || wide.is_null() {
            return None;
        }
        // SAFETY: `wide` es una cadena UTF-16 terminada en NUL que reservó Windows; se lee antes de liberarla.
        let text = unsafe { OsString::from_wide(std::slice::from_raw_parts(wide, wcslen(wide))) };
        // SAFETY: `wide` se reservó con LocalAlloc dentro de ConvertSidToStringSidW y se libera una sola vez.
        unsafe { LocalFree(wide as *mut _) };
        text.into_string().ok()
    })();
    // SAFETY: `token` es el handle válido de arriba y se cierra una sola vez.
    unsafe { CloseHandle(token) };
    sid
}

/// Carpeta Descargas del usuario según Windows (puede estar redirigida), o `None` si falla.
pub fn downloads_dir() -> Option<PathBuf> {
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: `FOLDERID_Downloads` es una constante válida; flags 0 y token nulo = usuario actual; `out` es escribible.
    let hr =
        unsafe { SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut out) };
    if hr < 0 || out.is_null() {
        if !out.is_null() {
            // SAFETY: incluso si falla, la documentación pide liberar el puntero que se haya devuelto.
            unsafe { CoTaskMemFree(out as *const _) };
        }
        return None;
    }
    // SAFETY: tras un HRESULT correcto, `out` es una cadena UTF-16 terminada en NUL reservada con CoTaskMemAlloc.
    let path = unsafe { OsString::from_wide(std::slice::from_raw_parts(out, wcslen(out))) };
    // SAFETY: `out` se liberó con el asignador correcto y se libera una sola vez.
    unsafe { CoTaskMemFree(out as *const _) };
    Some(path.into())
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Qué es una ruta conocida (para que la interfaz la nombre).
///
/// `ChromeCache`, `EdgeCache` y `FirefoxCache` son SOLO directorios de caché concretos (ver [`browser_cache_dirs`]),
/// nunca el perfil del navegador (historial, marcadores, cookies, credenciales...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnownKind {
    /// `%TEMP%` del usuario.
    UserTemp,
    /// `Windows\Temp`.
    WindowsTemp,
    ChromeCache,
    EdgeCache,
    FirefoxCache,
    /// La carpeta Descargas.
    Downloads,
    /// La carpeta de la Papelera del usuario en el volumen.
    RecycleBin,
}

/// Navegador cuyas carpetas se conocen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BrowserFamily {
    Chrome,
    Edge,
    Firefox,
}

impl BrowserFamily {
    /// La clase de caché de este navegador.
    pub fn cache_kind(self) -> KnownKind {
        match self {
            BrowserFamily::Chrome => KnownKind::ChromeCache,
            BrowserFamily::Edge => KnownKind::EdgeCache,
            BrowserFamily::Firefox => KnownKind::FirefoxCache,
        }
    }
}

/// Carpetas base de las que cuelgan los datos de los navegadores (`%LOCALAPPDATA%` y `%APPDATA%`).
#[derive(Clone, Debug, Default)]
pub struct BrowserRoots {
    pub local_appdata: Option<PathBuf>,
    pub roaming_appdata: Option<PathBuf>,
}

impl BrowserRoots {
    /// Las del entorno del proceso.
    pub fn from_env() -> BrowserRoots {
        BrowserRoots {
            local_appdata: env_path("LOCALAPPDATA"),
            roaming_appdata: env_path("APPDATA"),
        }
    }

    /// `User Data` de un navegador basado en Chromium. Los perfiles (`Default`, `Profile N`) cuelgan de aquí.
    fn user_data(&self, f: BrowserFamily) -> Option<PathBuf> {
        let local = self.local_appdata.as_ref()?;
        match f {
            BrowserFamily::Chrome => Some(local.join(r"Google\Chrome\User Data")),
            BrowserFamily::Edge => Some(local.join(r"Microsoft\Edge\User Data")),
            BrowserFamily::Firefox => None,
        }
    }

    /// `Profiles` de Firefox en `%APPDATA%`: el perfil con los datos del usuario (NUNCA es caché).
    fn firefox_roaming_profiles(&self) -> Option<PathBuf> {
        Some(
            self.roaming_appdata
                .as_ref()?
                .join(r"Mozilla\Firefox\Profiles"),
        )
    }

    /// `Profiles` de Firefox en `%LOCALAPPDATA%`: de aquí cuelgan `cache2` y `startupCache` de cada perfil.
    fn firefox_local_profiles(&self) -> Option<PathBuf> {
        Some(
            self.local_appdata
                .as_ref()?
                .join(r"Mozilla\Firefox\Profiles"),
        )
    }
}

// Qué es caché y qué no (la clasificación que reutilizará el borrado: tiene que ser EXACTA y CONSERVADORA).
//
// Fuentes:
// * Chromium, `chrome/common/chrome_constants.h`: el perfil por defecto es `Default`, los demás son `Profile <N>`
//   (`kMultiProfileDirPrefix = "Profile "`) y `Cache` es `kCacheDirname`. Los otros nombres de caché de abajo
//   (`Code Cache`, `GPUCache`, `Service Worker\CacheStorage`, y `ShaderCache` / `GrShaderCache` a nivel de
//   `User Data`) son los directorios de caché regenerables habituales de Chromium, pero NO están en ese archivo y no
//   se verificaron contra una fuente oficial: no hay que ampliar la lista sin comprobarlo. Según
//   <https://chromium.googlesource.com/chromium/src/+/main/docs/user_data_dir.md>, `User Data` está en
//   `%LOCALAPPDATA%\<Vendor>\<Producto>\User Data` y en Windows la caché del usuario es el propio directorio del
//   perfil.
// * Firefox, `netwerk/cache2/CacheFileIOManager.cpp`: la caché HTTP es `cache2` bajo el directorio LOCAL del perfil
//   (`NS_APP_USER_PROFILE_LOCAL_50_DIR`, es decir `%LOCALAPPDATA%\Mozilla\Firefox\Profiles\<perfil>`). `startupCache`
//   está en ese mismo directorio local (no verificado en una fuente oficial). El perfil de `%APPDATA%` guarda
//   `places.sqlite`, `key4.db`, `logins.json`, `cookies.sqlite`... y NUNCA es caché.
//
// Lo que NO es caché y nunca se clasifica como tal: todo lo demás dentro de `User Data` (incluidos `Local State`,
// `Bookmarks`, `History`, `Cookies`, `Login Data`, `Web Data`, `Local Storage`, `IndexedDB`, `Extensions`,
// `Preferences`, `Sessions`...) y de los perfiles de Firefox (en `%APPDATA%` y en `%LOCALAPPDATA%`: `thumbnails`,
// `OfflineCache`...). `Guest Profile` y `System Profile` tampoco cuentan: solo `Default` y `Profile <N>`.

/// Subdirectorios de caché dentro de un perfil de Chromium (rutas relativas al perfil, en minúsculas).
const CHROMIUM_PROFILE_CACHE: [&[&str]; 4] = [
    &["cache"],
    &["code cache"],
    &["gpucache"],
    &["service worker", "cachestorage"],
];
/// Cachés a nivel de `User Data` (no por perfil), en minúsculas.
const CHROMIUM_ROOT_CACHE: [&str; 2] = ["shadercache", "grshadercache"];
/// Cachés dentro de un perfil LOCAL de Firefox, en minúsculas.
const FIREFOX_PROFILE_CACHE: [&str; 2] = ["cache2", "startupcache"];

/// Los mismos nombres con la mayúscula con la que se crean (para construir rutas).
const CHROMIUM_PROFILE_CACHE_NAMES: [&str; 4] = [
    "Cache",
    "Code Cache",
    "GPUCache",
    r"Service Worker\CacheStorage",
];
const CHROMIUM_ROOT_CACHE_NAMES: [&str; 2] = ["ShaderCache", "GrShaderCache"];
const FIREFOX_PROFILE_CACHE_NAMES: [&str; 2] = ["cache2", "startupCache"];

/// ¿Es `name` el directorio de un perfil de Chromium? Solo `Default` y `Profile <N>` (N, dígitos), sin distinguir
/// mayúsculas (Windows).
fn is_chromium_profile_name(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "default"
        || n.strip_prefix("profile ")
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// ¿Existe `p` como directorio de verdad (no un reparse point: un enlace podría llevar a datos del usuario)?
fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| {
        use std::os::windows::fs::MetadataExt;
        m.is_dir() && m.file_attributes() & 0x400 == 0
    })
}

/// Nombres de los subdirectorios reales de `dir` (vacío si no se puede listar), ordenados.
fn child_dirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let path = e.path();
            is_real_dir(&path).then_some((name, path))
        })
        .collect();
    v.sort();
    v
}

/// Directorios de CACHÉ concretos (y SOLO esos) de los navegadores, que existen ahora mismo:
///
/// * Chrome y Edge: de cada perfil de `User Data` (`Default`, `Profile N`), `Cache`, `Code Cache`, `GPUCache` y
///   `Service Worker\CacheStorage`; y `ShaderCache` y `GrShaderCache` a nivel de `User Data`.
/// * Firefox: `cache2` y `startupCache` de cada perfil de `%LOCALAPPDATA%\Mozilla\Firefox\Profiles`. Nunca
///   `%APPDATA%\Mozilla\Firefox\Profiles`.
///
/// Solo LISTA carpetas. Los reparse points no cuentan. Ninguna ruta devuelta contiene historial, marcadores,
/// cookies ni credenciales.
pub fn browser_cache_dirs(roots: &BrowserRoots) -> Vec<(KnownKind, PathBuf)> {
    let mut out = Vec::new();
    for fam in [BrowserFamily::Chrome, BrowserFamily::Edge] {
        let Some(user_data) = roots.user_data(fam) else {
            continue;
        };
        for name in CHROMIUM_ROOT_CACHE_NAMES {
            let p = user_data.join(name);
            if is_real_dir(&p) {
                out.push((fam.cache_kind(), p));
            }
        }
        for (name, profile) in child_dirs(&user_data) {
            if !is_chromium_profile_name(&name) {
                continue;
            }
            for sub in CHROMIUM_PROFILE_CACHE_NAMES {
                let p = profile.join(sub);
                if is_real_dir(&p) {
                    out.push((fam.cache_kind(), p));
                }
            }
        }
    }
    if let Some(profiles) = roots.firefox_local_profiles() {
        for (_, profile) in child_dirs(&profiles) {
            for sub in FIREFOX_PROFILE_CACHE_NAMES {
                let p = profile.join(sub);
                if is_real_dir(&p) {
                    out.push((KnownKind::FirefoxCache, p));
                }
            }
        }
    }
    out
}

/// Qué es una ruta respecto a los datos de los navegadores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserPath {
    /// Un directorio de caché de la lista de [`browser_cache_dirs`], o algo dentro de él.
    Cache(KnownKind),
    /// Cualquier otra cosa de los datos de un navegador (el perfil con historial, cookies, credenciales...), la
    /// propia carpeta `User Data` o `Profiles` incluida: NO es caché y no se limpia como tal.
    Profile(BrowserFamily),
}

/// Prefijo y componentes en minúsculas de una ruta (sin la raíz), o `None` si lleva `.` o `..` (no se razona sobre
/// rutas sin normalizar).
fn lower_components(p: &Path) -> Option<(String, Vec<String>)> {
    use std::path::Component;
    let p = dunce::simplified(p);
    let mut prefix = String::new();
    let mut comps = Vec::new();
    for c in p.components() {
        match c {
            Component::Prefix(x) => prefix = x.as_os_str().to_string_lossy().to_lowercase(),
            Component::RootDir => {}
            Component::Normal(n) => comps.push(n.to_string_lossy().to_lowercase()),
            Component::CurDir | Component::ParentDir => return None,
        }
    }
    Some((prefix, comps))
}

/// Clasifica `path` (sin tocar el disco, comparación sin distinguir mayúsculas, solo componentes completos):
/// `Some(Cache(..))` si es EXACTAMENTE un directorio de caché de [`browser_cache_dirs`] o cuelga de uno,
/// `Some(Profile(..))` si es otra cosa dentro de los datos de un navegador (incluida la propia carpeta `User Data` o
/// `Profiles`), `None` si no tiene que ver con los navegadores (un ancestro como `AppData\Local` tampoco lo es).
///
/// Es conservadora: ante cualquier duda (rutas con `.` o `..`, un perfil que no se llama `Default` ni `Profile N`,
/// `Guest Profile`, una carpeta que solo se parece) no es caché.
pub fn classify_browser_path(roots: &BrowserRoots, path: &Path) -> Option<BrowserPath> {
    let (prefix, comps) = lower_components(path)?;
    // (resto de `path` bajo `base`, ¿`path` es `base` o cuelga de `base`?). `None` si no tienen relación; si `path` es
    // un ancestro de `base`, el resto es vacío y la bandera es `false`.
    let under = |base: &Path| -> Option<(Vec<String>, bool)> {
        let (bp, bc) = lower_components(base)?;
        if bp != prefix {
            return None;
        }
        let n = bc.len().min(comps.len());
        if bc[..n] != comps[..n] {
            return None;
        }
        Some((comps[n..].to_vec(), comps.len() >= bc.len()))
    };
    for fam in [BrowserFamily::Chrome, BrowserFamily::Edge] {
        let Some(ud) = roots.user_data(fam) else {
            continue;
        };
        let Some((rest, inside)) = under(&ud) else {
            continue;
        };
        if !inside {
            continue; // un ancestro de `User Data`: no es dato del navegador
        }
        let is_cache = match rest.as_slice() {
            [] => false,
            [first, ..] if CHROMIUM_ROOT_CACHE.contains(&first.as_str()) => true,
            [profile, sub @ ..] if is_chromium_profile_name(profile) => CHROMIUM_PROFILE_CACHE
                .iter()
                .any(|c| sub.len() >= c.len() && sub[..c.len()].iter().eq(c.iter())),
            _ => false,
        };
        return Some(if is_cache {
            BrowserPath::Cache(fam.cache_kind())
        } else {
            BrowserPath::Profile(fam)
        });
    }
    if let Some(lp) = roots.firefox_local_profiles()
        && let Some((rest, true)) = under(&lp)
    {
        let is_cache = matches!(rest.as_slice(), [_profile, sub, ..] if FIREFOX_PROFILE_CACHE.contains(&sub.as_str()));
        return Some(if is_cache {
            BrowserPath::Cache(KnownKind::FirefoxCache)
        } else {
            BrowserPath::Profile(BrowserFamily::Firefox)
        });
    }
    if let Some(rp) = roots.firefox_roaming_profiles()
        && under(&rp).is_some_and(|(_, inside)| inside)
    {
        return Some(BrowserPath::Profile(BrowserFamily::Firefox));
    }
    None
}

/// Rutas conocidas candidatas, con su clase (sin comprobar que existan ni que cuelguen de la ruta escaneada):
/// `%TEMP%`, `Windows\Temp`, los directorios de CACHÉ de Chrome, Edge y Firefox ([`browser_cache_dirs`]; nunca sus
/// perfiles), Descargas y la carpeta de la Papelera del usuario actual en el volumen `drive_root` (`C:\`). Esa carpeta
/// solo se LISTA, como todo el escaneo.
pub fn known_paths_labeled(drive_root: Option<&Path>) -> Vec<(KnownKind, PathBuf)> {
    known_paths_labeled_with(&BrowserRoots::from_env(), drive_root)
}

fn known_paths_labeled_with(
    roots: &BrowserRoots,
    drive_root: Option<&Path>,
) -> Vec<(KnownKind, PathBuf)> {
    let mut v = Vec::new();
    for var in ["TEMP", "TMP"] {
        v.extend(env_path(var).map(|p| (KnownKind::UserTemp, p)));
    }
    v.extend(env_path("SystemRoot").map(|p| (KnownKind::WindowsTemp, p.join("Temp"))));
    v.extend(browser_cache_dirs(roots));
    v.extend(
        downloads_dir()
            .or_else(|| env_path("USERPROFILE").map(|p| p.join("Downloads")))
            .map(|p| (KnownKind::Downloads, p)),
    );
    if let (Some(root), Some(sid)) = (drive_root, current_user_sid()) {
        v.push((KnownKind::RecycleBin, root.join(r"$Recycle.Bin").join(sid)));
    }
    v
}

/// Las mismas rutas sin su clase (lo que usa el escáner para la cola prioritaria).
pub fn known_paths(drive_root: Option<&Path>) -> Vec<PathBuf> {
    known_paths_labeled(drive_root)
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_sid_del_usuario_tiene_la_forma_esperada() {
        let sid = current_user_sid().expect("SID");
        assert!(
            sid.starts_with("S-1-5-21-") || sid.starts_with("S-1-5-"),
            "forma inesperada"
        );
        assert!(
            sid.chars()
                .all(|c| c.is_ascii_digit() || c == 'S' || c == '-')
        );
    }

    #[test]
    fn descargas_es_una_ruta_absoluta() {
        let d = downloads_dir().expect("Descargas");
        assert!(d.is_absolute());
    }

    #[test]
    fn las_rutas_conocidas_incluyen_la_papelera_solo_con_unidad() {
        let with = known_paths(Some(Path::new(r"C:\")));
        assert!(
            with.iter()
                .any(|p| p.to_string_lossy().contains("$Recycle.Bin"))
        );
        let without = known_paths(None);
        assert!(
            !without
                .iter()
                .any(|p| p.to_string_lossy().contains("$Recycle.Bin"))
        );
    }

    #[test]
    fn las_rutas_conocidas_reales_nunca_son_un_perfil_de_navegador() {
        // Con el entorno real: toda ruta de navegador es una caché concreta, y ninguna otra ruta conocida cae en una.
        let roots = BrowserRoots::from_env();
        for (kind, p) in known_paths_labeled(Some(Path::new(r"C:\"))) {
            match kind {
                KnownKind::ChromeCache | KnownKind::EdgeCache | KnownKind::FirefoxCache => {
                    assert_eq!(
                        classify_browser_path(&roots, &p),
                        Some(BrowserPath::Cache(kind)),
                        "{p:?}"
                    );
                }
                _ => assert!(
                    !matches!(
                        classify_browser_path(&roots, &p),
                        Some(BrowserPath::Cache(_))
                    ),
                    "{p:?}"
                ),
            }
        }
    }

    // ---- Perfiles de navegador sintéticos ----

    use std::fs;

    /// Árbol temporal con guarda RAII (solo se borra `%TEMP%\tesela-test-*` creado aquí).
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Temp {
            let p = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&p).unwrap();
            Temp(p)
        }
        fn dir(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(&p).unwrap();
            p
        }
        fn file(&self, rel: &str) {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, b"x").unwrap();
        }
        fn roots(&self) -> BrowserRoots {
            BrowserRoots {
                local_appdata: Some(self.0.join("Local")),
                roaming_appdata: Some(self.0.join("Roaming")),
            }
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Datos que NUNCA pueden ser caché, dentro de un perfil de Chromium (`Service Worker` solo contiene la caché en
    /// su subcarpeta `CacheStorage`).
    const CHROMIUM_PRIVATE: [&str; 12] = [
        "Bookmarks",
        "History",
        "Cookies",
        "Login Data",
        "Web Data",
        "Preferences",
        "Local Storage",
        "IndexedDB",
        "Extensions",
        "Sessions",
        "Network",
        "Service Worker",
    ];

    fn build_chromium(t: &Temp, user_data: &str) {
        for profile in [
            "Default",
            "Profile 1",
            "Profile 12",
            "Guest Profile",
            "System Profile",
            "Profile X",
        ] {
            for private in CHROMIUM_PRIVATE {
                t.file(&format!("{user_data}/{profile}/{private}/dato"));
            }
            for cache in [
                "Cache",
                "Code Cache",
                "GPUCache",
                "Service Worker/CacheStorage",
                "Service Worker/ScriptCache",
            ] {
                t.file(&format!("{user_data}/{profile}/{cache}/f_0"));
            }
            t.file(&format!("{user_data}/{profile}/Bookmarks.bak"));
        }
        t.file(&format!("{user_data}/Local State"));
        t.file(&format!("{user_data}/ShaderCache/f"));
        t.file(&format!("{user_data}/GrShaderCache/f"));
        t.file(&format!("{user_data}/Safe Browsing/f"));
    }

    fn rel_to(base: &Path, p: &Path) -> String {
        p.strip_prefix(base)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    }

    #[test]
    fn chromium_solo_las_caches_concretas_de_default_y_profile_n() {
        let t = Temp::new();
        build_chromium(&t, "Local/Google/Chrome/User Data");
        build_chromium(&t, "Local/Microsoft/Edge/User Data");
        let roots = t.roots();
        let dirs = browser_cache_dirs(&roots);
        let mut esperado: Vec<String> = Vec::new();
        for profile in ["Default", "Profile 1", "Profile 12"] {
            for sub in [
                "Cache",
                "Code Cache",
                "GPUCache",
                "Service Worker/CacheStorage",
            ] {
                esperado.push(format!("{profile}/{sub}"));
            }
        }
        esperado.push("ShaderCache".into());
        esperado.push("GrShaderCache".into());
        esperado.sort();
        for (kind, fam_dir) in [
            (KnownKind::ChromeCache, "Local/Google/Chrome/User Data"),
            (KnownKind::EdgeCache, "Local/Microsoft/Edge/User Data"),
        ] {
            let mut got: Vec<String> = dirs
                .iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, p)| rel_to(&t.0.join(fam_dir), p))
                .collect();
            got.sort();
            assert_eq!(got, esperado, "{fam_dir}");
        }
        assert_eq!(dirs.len(), 2 * esperado.len());
        // Clasificación sobre TODO el árbol: solo es caché lo esperado (y lo que cuelga de ello).
        for fam_dir in [
            "Local/Google/Chrome/User Data",
            "Local/Microsoft/Edge/User Data",
        ] {
            let ud = t.0.join(fam_dir);
            let mut caches = 0;
            let mut privados = 0;
            let mut walk = vec![ud.clone()];
            while let Some(d) = walk.pop() {
                for e in fs::read_dir(&d).unwrap().flatten() {
                    let p = e.path();
                    let c = classify_browser_path(&roots, &p);
                    let rel = rel_to(&ud, &p);
                    let parts: Vec<&str> = rel.split('/').collect();
                    let esperada_cache = ["ShaderCache", "GrShaderCache"].contains(&parts[0])
                        || (parts.len() >= 2
                            && ["Default", "Profile 1", "Profile 12"].contains(&parts[0])
                            && (["Cache", "Code Cache", "GPUCache"].contains(&parts[1])
                                || (parts.len() >= 3
                                    && parts[1] == "Service Worker"
                                    && parts[2] == "CacheStorage")));
                    assert_eq!(
                        matches!(c, Some(BrowserPath::Cache(_))),
                        esperada_cache,
                        "{rel}: {c:?}"
                    );
                    assert!(c.is_some(), "{rel}: todo lo de User Data es del navegador");
                    caches += usize::from(esperada_cache);
                    privados += usize::from(!esperada_cache);
                    if p.is_dir() {
                        walk.push(p);
                    }
                }
            }
            assert!(caches > 0 && privados > 0);
            // `User Data` es dato del navegador (no caché); sus ancestros no tienen que ver con los navegadores.
            assert!(matches!(
                classify_browser_path(&roots, &ud),
                Some(BrowserPath::Profile(_))
            ));
            assert_eq!(classify_browser_path(&roots, ud.parent().unwrap()), None);
        }
    }

    #[test]
    fn ninguna_ruta_de_cache_contiene_datos_privados_de_un_perfil_chromium() {
        let t = Temp::new();
        build_chromium(&t, "Local/Google/Chrome/User Data");
        for (_, p) in browser_cache_dirs(&t.roots()) {
            let r = rel_to(&t.0, &p);
            for bad in [
                "Guest Profile",
                "System Profile",
                "Profile X",
                "ScriptCache",
                "Bookmarks",
                "History",
                "Cookies",
                "Login Data",
                "Local Storage",
                "IndexedDB",
            ] {
                assert!(!r.contains(bad), "{r}");
            }
            assert!(!r.ends_with("Service Worker"), "{r}");
        }
    }

    #[test]
    fn firefox_solo_cache2_y_startupcache_del_perfil_local_y_nada_del_roaming() {
        let t = Temp::new();
        // Perfil de datos (Roaming): todo privado, incluso lo que se llame como una caché.
        for f in [
            "places.sqlite",
            "key4.db",
            "logins.json",
            "cookies.sqlite",
            "formhistory.sqlite",
            "prefs.js",
            "cache2/entries/x",
            "startupCache/x",
        ] {
            t.file(&format!(
                "Roaming/Mozilla/Firefox/Profiles/abcd.default-release/{f}"
            ));
        }
        // Perfil local: dos cachés y otras cosas.
        for f in [
            "cache2/entries/x",
            "startupCache/startupCache.8.little",
            "thumbnails/x",
            "OfflineCache/x",
            "places.sqlite",
        ] {
            t.file(&format!(
                "Local/Mozilla/Firefox/Profiles/abcd.default-release/{f}"
            ));
        }
        let roots = t.roots();
        let dirs = browser_cache_dirs(&roots);
        let mut rels: Vec<String> = dirs
            .iter()
            .map(|(k, p)| {
                assert_eq!(*k, KnownKind::FirefoxCache);
                rel_to(&t.0, p)
            })
            .collect();
        rels.sort();
        assert_eq!(
            rels,
            vec![
                "Local/Mozilla/Firefox/Profiles/abcd.default-release/cache2",
                "Local/Mozilla/Firefox/Profiles/abcd.default-release/startupCache",
            ]
        );
        // Clasificación: todo lo de Roaming es perfil; en Local solo las dos cachés.
        let mut n_cache = 0;
        let mut walk = vec![t.0.join("Roaming"), t.0.join("Local")];
        while let Some(d) = walk.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                let c = classify_browser_path(&roots, &p);
                let rel = rel_to(&t.0, &p);
                let es_cache = rel.starts_with("Local/")
                    && (rel.contains("/cache2") || rel.contains("/startupCache"));
                if rel.starts_with("Roaming/Mozilla/Firefox/Profiles") {
                    assert!(
                        matches!(c, Some(BrowserPath::Profile(BrowserFamily::Firefox))),
                        "{rel}: {c:?}"
                    );
                } else if rel.starts_with("Local/Mozilla/Firefox/Profiles") {
                    assert_eq!(matches!(c, Some(BrowserPath::Cache(_))), es_cache, "{rel}");
                    assert!(c.is_some(), "{rel}");
                } else {
                    assert_eq!(c, None, "{rel}");
                }
                n_cache += usize::from(es_cache);
                if p.is_dir() {
                    walk.push(p);
                }
            }
        }
        assert!(n_cache > 0);
        // La carpeta `Profiles` de Roaming NUNCA es caché ni se ofrece como tal (era el fallo original).
        let roaming = t.0.join("Roaming/Mozilla/Firefox/Profiles");
        assert_eq!(
            classify_browser_path(&roots, &roaming),
            Some(BrowserPath::Profile(BrowserFamily::Firefox))
        );
        assert!(!dirs.iter().any(|(_, p)| p.starts_with(&roaming)));
    }

    #[test]
    fn la_clasificacion_no_se_deja_enganar() {
        let t = Temp::new();
        let roots = t.roots();
        let ud = t.0.join("Local/Google/Chrome/User Data");
        let cls = |p: PathBuf| classify_browser_path(&roots, &p);
        let es_cache = |p: PathBuf| matches!(cls(p), Some(BrowserPath::Cache(_)));
        // Mayúsculas de Windows: da igual.
        assert_eq!(
            cls(ud.join(r"default\CACHE")),
            Some(BrowserPath::Cache(KnownKind::ChromeCache))
        );
        // Cosas que solo se parecen.
        for bad in [
            r"Default\Cache2",
            r"Default\CacheX",
            r"Default\Cache Old",
            r"Default\GPUCache2",
            r"Default\Code",
            r"Default\Service Worker",
            r"Default\Service Worker\Database",
            r"Default\Service Worker\ScriptCache",
            r"Default\Network\Cache",
            r"Guest Profile\Cache",
            r"System Profile\Cache",
            r"Profile\Cache",
            r"Profile 1a\Cache",
            r"Cache",
            r"Code Cache",
            r"ShaderCache2",
            r"Default\Extensions\Cache",
            r"Default\Local Storage\leveldb",
            r"Default\Login Data",
            r"Default",
        ] {
            assert!(!es_cache(ud.join(bad)), "{bad}");
        }
        // Rutas sin normalizar: nunca caché, aunque acaben en una caché real.
        assert!(!es_cache(ud.join(r"Default\Login Data\..\Cache")));
        // (`Default\.\Cache` es lexicamente `Default\Cache`: `Path::components` descarta los `.` intermedios.)
        // Ajeno a los navegadores.
        assert_eq!(cls(t.0.join("Local/Otra/Cache")), None);
        // Firefox: una carpeta que solo se parece, o sin perfil por medio.
        let ff = t.0.join("Local/Mozilla/Firefox/Profiles");
        assert!(!es_cache(ff.join("p1/cache2old")));
        assert!(!es_cache(ff.join("cache2")));
        assert!(!es_cache(ff.join("p1")));
        assert_eq!(
            cls(ff.join("p1/cache2/entries/a")),
            Some(BrowserPath::Cache(KnownKind::FirefoxCache))
        );
        // Con prefijo verbatim da lo mismo.
        // (En una ruta verbatim `/` no separa: se normaliza antes.)
        let plain = ud
            .join(r"Default\Cache")
            .display()
            .to_string()
            .replace('/', r"\");
        let verbatim = PathBuf::from(format!(r"\\?\{plain}"));
        assert!(es_cache(verbatim));
    }

    #[test]
    fn un_reparse_point_no_cuenta_como_directorio_de_cache() {
        // Una junction llamada `GPUCache` hacia datos del usuario no se ofrece como caché.
        let t = Temp::new();
        let ud = "Local/Google/Chrome/User Data/Default";
        t.dir(&format!("{ud}/Cache")); // real
        let datos = t.dir("Datos");
        t.file("Datos/importante.txt");
        let junction = t.0.join(ud).join("GPUCache");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&datos)
            .output();
        if made.is_ok_and(|o| o.status.success()) {
            let dirs = browser_cache_dirs(&t.roots());
            assert_eq!(dirs.len(), 1, "{dirs:?}");
            assert!(dirs[0].1.ends_with("Cache"));
            // La junction se quita sin tocar el destino (remove_dir sobre el enlace).
            fs::remove_dir(&junction).unwrap();
            assert!(datos.join("importante.txt").exists());
        }
    }

    #[test]
    fn cada_ruta_conocida_lleva_su_clase() {
        let labeled = known_paths_labeled(Some(Path::new(r"C:\")));
        // Mismo contenido y orden que `known_paths`.
        let plain: Vec<PathBuf> = labeled.iter().map(|(_, p)| p.clone()).collect();
        assert_eq!(plain, known_paths(Some(Path::new(r"C:\"))));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::Downloads));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::RecycleBin));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::UserTemp));
        assert!(labeled.iter().any(|(k, _)| *k == KnownKind::WindowsTemp));
        // Con un entorno sintético, la lista de rutas conocidas lleva SOLO cachés concretas de navegador.
        let t = Temp::new();
        build_chromium(&t, "Local/Google/Chrome/User Data");
        t.file("Roaming/Mozilla/Firefox/Profiles/p/places.sqlite");
        t.file("Local/Mozilla/Firefox/Profiles/p/cache2/entries/x");
        let roots = t.roots();
        let v = known_paths_labeled_with(&roots, None);
        let browsers: Vec<&(KnownKind, PathBuf)> = v
            .iter()
            .filter(|(k, _)| {
                matches!(
                    k,
                    KnownKind::ChromeCache | KnownKind::EdgeCache | KnownKind::FirefoxCache
                )
            })
            .collect();
        assert!(browsers.len() > 5);
        for (k, p) in browsers {
            assert_eq!(
                classify_browser_path(&roots, p),
                Some(BrowserPath::Cache(*k))
            );
            assert!(
                !p.ends_with("User Data") && !p.ends_with("Profiles"),
                "{p:?}"
            );
        }
    }
}
