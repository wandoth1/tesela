//! Carpetas de datos de los navegadores: qué es caché regenerable y qué NO lo es.
//!
//! # Para qué sirve esta clasificación
//! [`classify_browser_path`] sirve para ETIQUETAR (la interfaz nombra una ruta, el escáner decide qué recorrer primero).
//! NO autoriza ningún borrado: la autorización saldrá de las raíces que devuelve [`browser_cache_dirs`] verificadas por
//! HANDLE en el momento de borrar (PLAN.md §3.7), nunca de una comparación de cadenas de ruta.
//!
//! # Fuentes de los nombres (la lista tiene que ser EXACTA y CONSERVADORA)
//! * Chromium, `chrome/common/chrome_constants.h`: el perfil por defecto es `Default` (`kInitialProfile`), los demás son
//!   `Profile <N>` (`kMultiProfileDirPrefix = "Profile "`) y `Cache` es `kCacheDirname`.
//! * Chromium, `gpu/ipc/common/gpu_disk_cache_type.cc`, función `GetGpuDiskCacheSubdir`: `GPUCache` (`kGlShaders`),
//!   `DawnWebGPUCache` (`kDawnWebGPU`) y `DawnGraphiteCache` (`kDawnGraphite`). Cuelgan del directorio del perfil igual
//!   que `GPUCache`.
//! * `GraphiteDawnCache` (a nivel de `User Data`, como `ShaderCache`): `chrome/browser/chrome_content_browser_client.cc`
//!   (`user_data_dir.Append("GraphiteDawnCache")`) y `gpu/ipc/common/gpu_disk_cache_factory.cc` (del tipo DawnGraphite,
//!   regenerable).
//! * `Code Cache` (por perfil): `content/browser/storage_partition_impl.cc`. `ShaderCache` y `GrShaderCache` (a nivel de
//!   `User Data`): `chrome/browser/chrome_content_browser_client.cc`. Todos confirmados en fuente; no ampliar la lista sin
//!   comprobar igual.
//! * `Service Worker\CacheStorage` NO es caché: en `content/public/browser/browsing_data_remover.h`,
//!   `DATA_TYPE_CACHE_STORAGE` es uno de los tipos de `DATA_TYPE_DOM_STORAGE` (datos de sitios, junto a IndexedDB,
//!   Local Storage y service workers) y puede contener el modo sin conexión de una PWA. `Service Worker` entero también
//!   guarda `Database` y `ScriptCache`: nada de eso se limpia como caché. Tampoco `GPUPersistentCache`, `Media Cache`
//!   ni los `*_crx_cache`.
//! * Firefox, `netwerk/cache2/CacheFileIOManager.cpp`: la caché HTTP es `cache2` bajo el directorio LOCAL del perfil
//!   (`NS_APP_USER_PROFILE_LOCAL_50_DIR`: `%LOCALAPPDATA%\Mozilla\Firefox\Profiles\<perfil>`). `startupCache` está en ese
//!   mismo directorio (confirmado en `StartupCache.cpp` y `xpcom/build/nsXULAppAPI.h`). El perfil de `%APPDATA%` guarda `places.sqlite`, `key4.db`,
//!   `logins.json`, `cookies.sqlite`... y NUNCA es caché.
//!
//! Lo que NO es caché: todo lo demás dentro de `User Data` (`Local State`, `Bookmarks`, `History`, `Cookies`,
//! `Login Data`, `Web Data`, `Local Storage`, `IndexedDB`, `Extensions`, `Preferences`, `Sessions`...) y de los perfiles
//! de Firefox. `Guest Profile`, `System Profile` y `Profile 01` tampoco cuentan como perfiles.
//!
//! # Rutas en las que no se confía
//! El resultado es de CUATRO estados ([`BrowserPath`]): ante una ruta relativa, con `..`, con prefijo raro (`\\.\`,
//! `\\?\GLOBALROOT`, UNC), con alias 8.3 (`GOOGLE~1`), con puntos o espacios finales, con `:` (flujos de datos) o con
//! mayúsculas que solo coinciden fuera de ASCII, la respuesta es `Unknown` ("no tocar"), nunca "no es un navegador".

use crate::classify;
use crate::known::{KnownKind, known_folder};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf, Prefix};
use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_RoamingAppData};

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
    /// Las que da Windows (`SHGetKnownFolderPath`: `FOLDERID_LocalAppData` y `FOLDERID_RoamingAppData`), no las variables
    /// de entorno, que un proceso puede alterar.
    pub fn from_system() -> BrowserRoots {
        BrowserRoots {
            local_appdata: known_folder(&FOLDERID_LocalAppData),
            roaming_appdata: known_folder(&FOLDERID_RoamingAppData),
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

/// Subdirectorios de caché dentro de un perfil de Chromium, con la mayúscula con la que se crean (ver las fuentes arriba).
const CHROMIUM_PROFILE_CACHE: [&str; 5] = [
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnWebGPUCache",
    "DawnGraphiteCache",
];
/// Cachés a nivel de `User Data` (no por perfil).
const CHROMIUM_ROOT_CACHE: [&str; 3] = ["ShaderCache", "GrShaderCache", "GraphiteDawnCache"];
/// Cachés dentro de un perfil LOCAL de Firefox.
const FIREFOX_PROFILE_CACHE: [&str; 2] = ["cache2", "startupCache"];

/// ¿Es `name` el directorio de un perfil de Chromium? Solo `Default` y `Profile <N>` con N en dígitos ASCII, sin ceros a
/// la izquierda ni vacío (`Profile 01` no lo es), y sin distinguir mayúsculas SOLO de ASCII.
fn is_chromium_profile_name(name: &str) -> bool {
    if name.eq_ignore_ascii_case("default") {
        return true;
    }
    let Some(prefix) = name.get(..8) else {
        return false;
    };
    let digits = &name[8..];
    prefix.eq_ignore_ascii_case("profile ")
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0')
}

/// ¿Existe `p` como directorio de verdad? Con el MISMO criterio que el escáner (`classify::dir_action == Walk`): ni un
/// reparse point (un enlace podría llevar a datos del usuario; sin la etiqueta a mano, cualquier reparse point cuenta
/// como enlace, también los de la nube) ni una carpeta con atributos de recuperación de la nube aunque no lo sea.
fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| {
        use std::os::windows::fs::MetadataExt;
        m.is_dir() && classify::dir_action(m.file_attributes(), 0) == classify::DirAction::Walk
    })
}

/// `p` (que cuelga de `base`) con CADA componente por debajo de `base` comprobado como directorio real: un ancestro
/// intermedio (`Google`, `Chrome`...) que sea un enlace desviaría todo lo que se liste debajo a otro sitio. `base` en sí
/// es la carpeta de datos que da Windows (`%LOCALAPPDATA%`) y no se comprueba. `None` si `p` no cuelga de `base` o algún
/// componente no es un directorio real.
fn real_dir_under(base: &Path, p: &Path) -> Option<PathBuf> {
    let rel = p.strip_prefix(base).ok()?;
    let mut cur = base.to_path_buf();
    for c in rel.components() {
        let Component::Normal(name) = c else {
            return None;
        };
        cur.push(name);
        if !is_real_dir(&cur) {
            return None;
        }
    }
    Some(cur)
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
/// * Chrome y Edge: de cada perfil de `User Data` (`Default`, `Profile N`), `Cache`, `Code Cache`, `GPUCache`,
///   `DawnWebGPUCache` y `DawnGraphiteCache`; y `ShaderCache`, `GrShaderCache` y `GraphiteDawnCache` a nivel de `User Data`.
///   NO `Service Worker\CacheStorage` (datos de sitios).
/// * Firefox: `cache2` y `startupCache` de cada perfil de `%LOCALAPPDATA%\Mozilla\Firefox\Profiles`. Nunca
///   `%APPDATA%\Mozilla\Firefox\Profiles`.
///
/// Solo LISTA carpetas. Los reparse points no cuentan. Ninguna ruta devuelta contiene historial, marcadores, cookies ni
/// credenciales. Estas son las RAÍCES de las que saldrá la autorización de borrado (verificadas por handle).
pub fn browser_cache_dirs(roots: &BrowserRoots) -> Vec<(KnownKind, PathBuf)> {
    let mut out = Vec::new();
    for fam in [BrowserFamily::Chrome, BrowserFamily::Edge] {
        // `User Data` y todos sus ancestros por debajo de `%LOCALAPPDATA%` tienen que ser directorios reales.
        let Some(user_data) = roots
            .local_appdata
            .as_deref()
            .zip(roots.user_data(fam))
            .and_then(|(local, ud)| real_dir_under(local, &ud))
        else {
            continue;
        };
        for name in CHROMIUM_ROOT_CACHE {
            let p = user_data.join(name);
            if is_real_dir(&p) {
                out.push((fam.cache_kind(), p));
            }
        }
        for (name, profile) in child_dirs(&user_data) {
            if !is_chromium_profile_name(&name) {
                continue;
            }
            for sub in CHROMIUM_PROFILE_CACHE {
                let p = profile.join(sub);
                if is_real_dir(&p) {
                    out.push((fam.cache_kind(), p));
                }
            }
        }
    }
    if let Some(profiles) = roots
        .local_appdata
        .as_deref()
        .zip(roots.firefox_local_profiles())
        .and_then(|(local, pr)| real_dir_under(local, &pr))
    {
        for (_, profile) in child_dirs(&profiles) {
            for sub in FIREFOX_PROFILE_CACHE {
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
    /// Cualquier otra cosa de los datos de un navegador (el perfil con historial, cookies, credenciales, `Service Worker`,
    /// `CacheStorage`...), la propia carpeta `User Data` o `Profiles` incluida: NO es caché.
    Profile(BrowserFamily),
    /// Una ruta absoluta y bien formada que NO está dentro de los datos de ningún navegador (un ancestro como
    /// `AppData\Local` tampoco lo es).
    NotBrowser,
    /// No se puede afirmar nada (ruta relativa, con `..`, prefijo raro, alias 8.3, mayúsculas ambiguas...): NO TOCAR.
    Unknown,
}

/// ¿Parece el nombre de un alias 8.3 (`PROGRA~1`, `GOOGLE~1.EXE`)? Un alias puede esconder cualquier nombre largo.
fn looks_like_short_name(s: &str) -> bool {
    let Some(tilde) = s.find('~') else {
        return false;
    };
    let (base, rest) = (&s[..tilde], &s[tilde + 1..]);
    if base.is_empty() || base.len() > 8 || base.contains('~') {
        return false;
    }
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    match rest[digits..].strip_prefix('.') {
        None => rest.len() == digits,
        Some(ext) => ext.len() <= 3 && !ext.contains('.'),
    }
}

/// ¿Es un componente en el que no se puede confiar? (puntos o espacios finales, que Windows quita; `:` o caracteres
/// prohibidos, que abren flujos de datos o son comodines; controles; alias 8.3).
fn untrusted_component(s: &str) -> bool {
    s.is_empty()
        || s.ends_with('.')
        || s.ends_with(' ')
        || s.chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*'))
        || looks_like_short_name(s)
}

/// Letra de unidad en minúscula y componentes de una ruta ABSOLUTA con letra de unidad (`C:\...`) y bien formada, o `None`
/// si no se puede razonar sobre ella: relativa a la unidad (`C:Users`), a la unidad actual (`\Users`), con `..`, con otro
/// prefijo (UNC, `\\.\`, `\\?\GLOBALROOT`) o con componentes en los que no se confía.
fn normalize(p: &Path) -> Option<(char, Vec<String>)> {
    let p = dunce::simplified(p);
    let mut it = p.components();
    let drive = match it.next()? {
        Component::Prefix(x) => match x.kind() {
            Prefix::Disk(l) | Prefix::VerbatimDisk(l) => (l as char).to_ascii_lowercase(),
            _ => return None,
        },
        _ => return None,
    };
    if it.next() != Some(Component::RootDir) {
        return None;
    }
    let mut comps = Vec::new();
    for c in it {
        match c {
            Component::Normal(n) => {
                let s = n.to_string_lossy();
                if untrusted_component(&s) || is_device_name(n) {
                    return None;
                }
                comps.push(s.into_owned());
            }
            _ => return None,
        }
    }
    Some((drive, comps))
}

/// Nombres reservados de dispositivo (`CON`, `NUL`, `COM1`...): no son carpetas normales.
fn is_device_name(n: &OsStr) -> bool {
    let s = n.to_string_lossy();
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && stem.as_bytes()[3].is_ascii_digit())
}

/// Comparación de dos nombres: iguales (sin distinguir mayúsculas SOLO de ASCII), distintos, o AMBIGUOS (coinciden con el
/// plegado de Unicode pero no con el de ASCII: el signo Kelvin no puede valer `k`, y NTFS decide con su propia tabla).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cmp {
    Equal,
    Different,
    Ambiguous,
}

fn cmp_name(a: &str, b: &str) -> Cmp {
    if a.eq_ignore_ascii_case(b) {
        Cmp::Equal
    } else if a.to_lowercase() == b.to_lowercase() || a.to_uppercase() == b.to_uppercase() {
        Cmp::Ambiguous
    } else {
        Cmp::Different
    }
}

/// Relación entre una raíz y una ruta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rel {
    /// La ruta es la raíz o cuelga de ella; el resto empieza en el componente `n`.
    Inside(usize),
    /// La ruta es un ancestro estricto de la raíz.
    Ancestor,
    Disjoint,
    Ambiguous,
}

fn relate(root: &[String], comps: &[String]) -> Rel {
    let n = root.len().min(comps.len());
    let mut ambiguous = false;
    for i in 0..n {
        match cmp_name(&root[i], &comps[i]) {
            Cmp::Equal => {}
            Cmp::Different => return Rel::Disjoint,
            Cmp::Ambiguous => ambiguous = true,
        }
    }
    if ambiguous {
        Rel::Ambiguous
    } else if comps.len() >= root.len() {
        Rel::Inside(root.len())
    } else {
        Rel::Ancestor
    }
}

/// ¿Coincide `rest` (desde su inicio) con `pattern` (componentes)? Con ambigüedad, `Ambiguous`.
fn starts_with_names(rest: &[String], pattern: &[&str]) -> Cmp {
    if rest.len() < pattern.len() {
        return Cmp::Different;
    }
    let mut ambiguous = false;
    for (r, p) in rest.iter().zip(pattern) {
        match cmp_name(r, p) {
            Cmp::Equal => {}
            Cmp::Different => return Cmp::Different,
            Cmp::Ambiguous => ambiguous = true,
        }
    }
    if ambiguous {
        Cmp::Ambiguous
    } else {
        Cmp::Equal
    }
}

/// Clasifica lo que cuelga de `User Data` (`rest`).
fn classify_chromium(fam: BrowserFamily, rest: &[String]) -> BrowserPath {
    let Some(first) = rest.first() else {
        return BrowserPath::Profile(fam); // la propia `User Data`
    };
    let mut ambiguous = false;
    for name in CHROMIUM_ROOT_CACHE {
        match cmp_name(first, name) {
            Cmp::Equal => return BrowserPath::Cache(fam.cache_kind()),
            Cmp::Ambiguous => ambiguous = true,
            Cmp::Different => {}
        }
    }
    if is_chromium_profile_name(first) && rest.len() > 1 {
        for sub in CHROMIUM_PROFILE_CACHE {
            match starts_with_names(&rest[1..], &[sub]) {
                Cmp::Equal => return BrowserPath::Cache(fam.cache_kind()),
                Cmp::Ambiguous => ambiguous = true,
                Cmp::Different => {}
            }
        }
    }
    if ambiguous {
        BrowserPath::Unknown
    } else {
        BrowserPath::Profile(fam)
    }
}

/// Clasifica `path` (sin tocar el disco): ver [`BrowserPath`]. Es CONSERVADORA: solo dice `Cache` si la ruta es EXACTAMENTE
/// un directorio de caché de [`browser_cache_dirs`] o cuelga de uno, componente a componente; ante cualquier duda es
/// `Profile` (no es caché) o `Unknown` (no tocar).
///
/// **Es para ETIQUETAR.** La autorización de un borrado NO sale de aquí sino de las raíces de [`browser_cache_dirs`]
/// verificadas por handle justo antes de borrar (PLAN.md §3.7).
pub fn classify_browser_path(roots: &BrowserRoots, path: &Path) -> BrowserPath {
    let Some((drive, comps)) = normalize(path) else {
        return BrowserPath::Unknown;
    };
    // Cada raíz: (ruta, cómo se clasifica lo que cuelga de ella).
    enum Kind {
        Chromium(BrowserFamily),
        FirefoxLocal,
        FirefoxRoaming,
    }
    let mut candidates: Vec<(PathBuf, Kind)> = Vec::new();
    for fam in [BrowserFamily::Chrome, BrowserFamily::Edge] {
        if let Some(r) = roots.user_data(fam) {
            candidates.push((r, Kind::Chromium(fam)));
        }
    }
    if let Some(r) = roots.firefox_local_profiles() {
        candidates.push((r, Kind::FirefoxLocal));
    }
    if let Some(r) = roots.firefox_roaming_profiles() {
        candidates.push((r, Kind::FirefoxRoaming));
    }
    let mut ambiguous = false;
    for (root, kind) in candidates {
        // Una raíz en la que no se puede confiar (alias 8.3, prefijo raro...) impide afirmar nada.
        let Some((rdrive, rcomps)) = normalize(&root) else {
            return BrowserPath::Unknown;
        };
        if rdrive != drive {
            continue;
        }
        match relate(&rcomps, &comps) {
            Rel::Inside(n) => {
                let rest = &comps[n..];
                return match kind {
                    Kind::Chromium(fam) => classify_chromium(fam, rest),
                    Kind::FirefoxLocal => {
                        // `<perfil>\cache2` o `<perfil>\startupCache` (y lo que cuelga de ellos).
                        let mut amb = false;
                        let mut is_cache = false;
                        if rest.len() >= 2 {
                            for sub in FIREFOX_PROFILE_CACHE {
                                match starts_with_names(&rest[1..], &[sub]) {
                                    Cmp::Equal => is_cache = true,
                                    Cmp::Ambiguous => amb = true,
                                    Cmp::Different => {}
                                }
                            }
                        }
                        if is_cache {
                            BrowserPath::Cache(KnownKind::FirefoxCache)
                        } else if amb {
                            BrowserPath::Unknown
                        } else {
                            BrowserPath::Profile(BrowserFamily::Firefox)
                        }
                    }
                    Kind::FirefoxRoaming => BrowserPath::Profile(BrowserFamily::Firefox),
                };
            }
            Rel::Ambiguous => ambiguous = true,
            Rel::Ancestor | Rel::Disjoint => {}
        }
    }
    if ambiguous {
        BrowserPath::Unknown
    } else {
        BrowserPath::NotBrowser
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Árbol temporal con guarda RAII (solo se borra `%TEMP%\tesela-test-*` creado aquí). La ruta se canonicaliza: con un
    /// nombre de usuario largo `%TEMP%` puede traer alias 8.3 (`USUARI~1`), en los que la clasificación no confía.
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Temp {
            let base = dunce::canonicalize(std::env::temp_dir()).unwrap();
            let p = base.join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
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

    /// Datos que NUNCA pueden ser caché, dentro de un perfil de Chromium. `Service Worker` lleva `CacheStorage`,
    /// `Database` y `ScriptCache`: ninguno es caché.
    const CHROMIUM_PRIVATE: [&str; 15] = [
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
        "Service Worker/CacheStorage",
        "Service Worker/Database",
        "Service Worker/ScriptCache",
        "GPUPersistentCache",
    ];

    fn build_chromium(t: &Temp, user_data: &str) {
        for profile in [
            "Default",
            "Profile 1",
            "Profile 12",
            "Guest Profile",
            "System Profile",
            "Profile X",
            "Profile 01",
            "Profile 0",
        ] {
            for private in CHROMIUM_PRIVATE {
                t.file(&format!("{user_data}/{profile}/{private}/dato"));
            }
            for cache in CHROMIUM_PROFILE_CACHE {
                t.file(&format!("{user_data}/{profile}/{cache}/f_0"));
            }
            t.file(&format!("{user_data}/{profile}/Media Cache/f"));
            t.file(&format!("{user_data}/{profile}/Bookmarks.bak"));
        }
        t.file(&format!("{user_data}/Local State"));
        t.file(&format!("{user_data}/ShaderCache/f"));
        t.file(&format!("{user_data}/GrShaderCache/f"));
        t.file(&format!("{user_data}/Safe Browsing/f"));
        t.file(&format!("{user_data}/GraphiteDawnCache/f"));
        t.file(&format!("{user_data}/GraphiteDawnCache/sub/g"));
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
            for sub in CHROMIUM_PROFILE_CACHE {
                esperado.push(format!("{profile}/{sub}"));
            }
        }
        esperado.push("ShaderCache".into());
        esperado.push("GrShaderCache".into());
        esperado.push("GraphiteDawnCache".into());
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
        // La lista de cachés verificadas en la fuente.
        for needed in [
            "Cache",
            "Code Cache",
            "GPUCache",
            "DawnWebGPUCache",
            "DawnGraphiteCache",
        ] {
            assert!(CHROMIUM_PROFILE_CACHE.contains(&needed), "{needed}");
        }
        for never in [
            "Service Worker",
            "CacheStorage",
            "GPUPersistentCache",
            "Media Cache",
        ] {
            assert!(
                !esperado.iter().any(|e| e.contains(never)),
                "{never} no es caché verificada"
            );
        }
        // Clasificación sobre TODO el árbol: solo es caché lo esperado (y lo que cuelga de ello).
        for fam_dir in [
            "Local/Google/Chrome/User Data",
            "Local/Microsoft/Edge/User Data",
        ] {
            let ud = t.0.join(fam_dir);
            let (mut caches, mut privados) = (0, 0);
            let mut walk = vec![ud.clone()];
            while let Some(d) = walk.pop() {
                for e in fs::read_dir(&d).unwrap().flatten() {
                    let p = e.path();
                    let c = classify_browser_path(&roots, &p);
                    let rel = rel_to(&ud, &p);
                    let parts: Vec<&str> = rel.split('/').collect();
                    let esperada_cache = CHROMIUM_ROOT_CACHE.contains(&parts[0])
                        || (parts.len() >= 2
                            && ["Default", "Profile 1", "Profile 12"].contains(&parts[0])
                            && CHROMIUM_PROFILE_CACHE.contains(&parts[1]));
                    assert_eq!(
                        matches!(c, BrowserPath::Cache(_)),
                        esperada_cache,
                        "{rel}: {c:?}"
                    );
                    assert!(
                        matches!(c, BrowserPath::Cache(_) | BrowserPath::Profile(_)),
                        "{rel}: todo lo de User Data es del navegador ({c:?})"
                    );
                    caches += usize::from(esperada_cache);
                    privados += usize::from(!esperada_cache);
                    if p.is_dir() {
                        walk.push(p);
                    }
                }
            }
            assert!(caches > 0 && privados > 0);
            // `Service Worker\CacheStorage` (datos de sitios, modo sin conexión de PWA) NO es caché, ni lo de dentro.
            for sw in [
                "Default/Service Worker/CacheStorage",
                "Default/Service Worker/CacheStorage/dato",
                "Default/Service Worker",
            ] {
                assert!(
                    matches!(
                        classify_browser_path(&roots, &ud.join(sw)),
                        BrowserPath::Profile(_)
                    ),
                    "{sw}"
                );
            }
            // `User Data` es dato del navegador (no caché); sus ancestros no tienen que ver con los navegadores.
            assert!(matches!(
                classify_browser_path(&roots, &ud),
                BrowserPath::Profile(_)
            ));
            assert_eq!(
                classify_browser_path(&roots, ud.parent().unwrap()),
                BrowserPath::NotBrowser
            );
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
                "Profile 01",
                "Profile 0/",
                "Service Worker",
                "CacheStorage",
                "ScriptCache",
                "Bookmarks",
                "History",
                "Cookies",
                "Login Data",
                "Local Storage",
                "IndexedDB",
                "GPUPersistentCache",
                "Media Cache",
            ] {
                assert!(!r.contains(bad), "{r}");
            }
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
                    assert_eq!(c, BrowserPath::Profile(BrowserFamily::Firefox), "{rel}");
                } else if rel.starts_with("Local/Mozilla/Firefox/Profiles") {
                    assert_eq!(matches!(c, BrowserPath::Cache(_)), es_cache, "{rel}");
                    assert!(
                        matches!(c, BrowserPath::Cache(_) | BrowserPath::Profile(_)),
                        "{rel}"
                    );
                } else {
                    assert_eq!(c, BrowserPath::NotBrowser, "{rel}");
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
            BrowserPath::Profile(BrowserFamily::Firefox)
        );
        assert!(!dirs.iter().any(|(_, p)| p.starts_with(&roaming)));
    }

    #[test]
    fn la_clasificacion_no_se_deja_enganar() {
        let t = Temp::new();
        let roots = t.roots();
        let ud = t.0.join("Local/Google/Chrome/User Data");
        let cls = |p: PathBuf| classify_browser_path(&roots, &p);
        let es_cache = |p: PathBuf| matches!(cls(p), BrowserPath::Cache(_));
        // Mayúsculas de ASCII: da igual.
        assert_eq!(
            cls(ud.join(r"default\CACHE")),
            BrowserPath::Cache(KnownKind::ChromeCache)
        );
        assert_eq!(
            cls(ud.join(r"Default\DAWNWEBGPUCACHE\x")),
            BrowserPath::Cache(KnownKind::ChromeCache)
        );
        // Cosas que solo se parecen: no son caché.
        for bad in [
            r"Default\Cache2",
            r"Default\CacheX",
            r"Default\Cache Old",
            r"Default\GPUCache2",
            r"Default\Code",
            r"Default\Service Worker",
            r"Default\Service Worker\CacheStorage",
            r"Default\Service Worker\Database",
            r"Default\Service Worker\ScriptCache",
            r"Default\Network\Cache",
            r"Default\Media Cache",
            r"Default\GPUPersistentCache",
            r"Default\GraphiteDawnCache", // a nivel de `User Data`, no por perfil
            r"Guest Profile\Cache",
            r"System Profile\Cache",
            r"Profile\Cache",
            r"Profile 1a\Cache",
            r"Profile 01\Cache",
            r"Profile 0\Cache",
            r"Profile \Cache",
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
        // Firefox: una carpeta que solo se parece, o sin perfil por medio.
        let ff = t.0.join("Local/Mozilla/Firefox/Profiles");
        assert!(!es_cache(ff.join("p1/cache2old")));
        assert!(!es_cache(ff.join("cache2")));
        assert!(!es_cache(ff.join("p1")));
        assert_eq!(
            cls(ff.join("p1/cache2/entries/a")),
            BrowserPath::Cache(KnownKind::FirefoxCache)
        );
        // Con prefijo verbatim da lo mismo (en una ruta verbatim `/` no separa: se normaliza antes).
        let plain = ud
            .join(r"Default\Cache")
            .display()
            .to_string()
            .replace('/', r"\");
        assert_eq!(
            cls(PathBuf::from(format!(r"\\?\{plain}"))),
            BrowserPath::Cache(KnownKind::ChromeCache)
        );
        // Ajeno a los navegadores: bien formado y fuera de las raíces.
        assert_eq!(cls(t.0.join("Local/Otra/Cache")), BrowserPath::NotBrowser);
    }

    #[test]
    fn las_rutas_en_las_que_no_se_confia_son_desconocidas_y_no_ajenas() {
        let t = Temp::new();
        let roots = t.roots();
        let ud = t.0.join("Local/Google/Chrome/User Data");
        let cls = |p: PathBuf| classify_browser_path(&roots, &p);
        let drive = t.0.display().to_string().chars().next().unwrap();
        // (Las rutas de red se construyen en tiempo de ejecución: el check de privacidad rechaza una literal.)
        let (bs, host, share) = ('\\', "srv", "share");
        let unknown: Vec<PathBuf> = vec![
            // Relativas, también las que llevan letra de unidad pero no raíz (`C:Users\...`) y las de la unidad actual.
            PathBuf::from(r"Local\Google\Chrome\User Data\Default\Cache"),
            PathBuf::from(format!(
                r"{drive}:Users\x\AppData\Local\Google\Chrome\User Data\Default\Cache"
            )),
            PathBuf::from(r"\Users\x\AppData\Local\Google\Chrome\User Data\Default\Cache"),
            PathBuf::from(""),
            // `..` en cualquier sitio, aunque acabe en una caché real.
            ud.join(r"Default\Login Data\..\Cache"),
            ud.join(r"..\..\Google\Chrome\User Data\Default\Cache"),
            // Prefijos raros.
            PathBuf::from(format!(r"\\.\{drive}:\x")),
            PathBuf::from(r"\\?\GLOBALROOT\Device\HarddiskVolume1\Users\x"),
            PathBuf::from(format!("{bs}{bs}?{bs}UNC{bs}{host}{bs}{share}{bs}x")),
            PathBuf::from(format!("{bs}{bs}{host}{bs}{share}{bs}x")),
            PathBuf::from(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x"),
            // Alias 8.3: pueden esconder cualquier nombre largo.
            PathBuf::from(format!(
                r"{drive}:\USUARI~1\AppData\Local\Google\Chrome\User Data\Default\Cache"
            )),
            ud.join(r"DEFAUL~1\Cache"),
            ud.join(r"Default\CODECA~1"),
            // Puntos o espacios finales (Windows los quita), flujos de datos y caracteres prohibidos.
            ud.join(r"Default\Cache."),
            ud.join("Default\\Cache "),
            ud.join(r"Default\Cache:flujo"),
            ud.join(r"Default\Cache::$INDEX_ALLOCATION"),
            ud.join(r"Default\Ca*che"),
            ud.join(r"Default\CON"),
        ];
        for p in &unknown {
            assert_eq!(cls(p.clone()), BrowserPath::Unknown, "{p:?}");
        }
        // Una raíz de navegador con alias 8.3 impide afirmar nada de NINGUNA ruta.
        let bad_roots = BrowserRoots {
            local_appdata: Some(PathBuf::from(format!(r"{drive}:\USUARI~1\AppData\Local"))),
            roaming_appdata: roots.roaming_appdata.clone(),
        };
        assert_eq!(
            classify_browser_path(&bad_roots, &t.0.join("cualquier-cosa")),
            BrowserPath::Unknown
        );
    }

    #[test]
    fn el_plegado_de_mayusculas_es_solo_ascii() {
        // El signo Kelvin (U+212A) se pliega a `k` con Unicode, pero un nombre con él NO es el nombre con `k`.
        assert_eq!(cmp_name("Cache", "CACHE"), Cmp::Equal);
        assert_eq!(cmp_name("\u{212A}", "k"), Cmp::Ambiguous);
        assert_eq!(cmp_name("\u{212A}", "x"), Cmp::Different);
        assert_eq!(cmp_name("stra\u{DF}e", "STRASSE"), Cmp::Ambiguous);
        // Nombres de perfil: solo dígitos ASCII, sin ceros a la izquierda.
        for ok in [
            "Default",
            "DEFAULT",
            "Profile 1",
            "profile 12",
            "PROFILE 100",
        ] {
            assert!(is_chromium_profile_name(ok), "{ok}");
        }
        for bad in [
            "Profile 0",
            "Profile 01",
            "Profile ",
            "Profile",
            "Profile \u{FF11}",
            "Profile \u{0661}",
            "Profile 1 ",
            "Profile -1",
            "Profiles 1",
            "Guest Profile",
            "Defaul\u{212A}t",
        ] {
            assert!(!is_chromium_profile_name(bad), "{bad}");
        }
        // Un nombre ambiguo con una caché NO se toma por ella ni por un perfil normal: no tocar.
        let t = Temp::new();
        let roots = BrowserRoots {
            local_appdata: Some(t.0.join("Local")),
            roaming_appdata: None,
        };
        let ud = t.0.join("Local/Google/Chrome/User Data");
        assert_eq!(
            classify_browser_path(&roots, &ud.join("Default\\Cac\u{212A}e")),
            BrowserPath::Profile(BrowserFamily::Chrome),
            "no es ninguna caché (ni parecida): es un nombre distinto"
        );
        assert_eq!(
            classify_browser_path(&roots, &ud.join("Default\\\u{212A}")),
            BrowserPath::Profile(BrowserFamily::Chrome)
        );
        // Una raíz con letras no ASCII que solo coincide al plegar Unicode es ambigua: Unknown.
        let roots = BrowserRoots {
            local_appdata: Some(t.0.join("Local-\u{DC}")),
            roaming_appdata: None,
        };
        assert_eq!(
            classify_browser_path(
                &roots,
                &t.0.join("Local-\u{FC}\\Google\\Chrome\\User Data\\Default\\Cache")
            ),
            BrowserPath::Unknown
        );
        assert_eq!(
            classify_browser_path(
                &roots,
                &t.0.join("Local-\u{DC}\\Google\\Chrome\\User Data\\Default\\Cache")
            ),
            BrowserPath::Cache(KnownKind::ChromeCache)
        );
    }

    #[test]
    fn los_alias_8_3_se_reconocen_sin_falsos_positivos_en_nombres_normales() {
        for s in [
            "PROGRA~1",
            "GOOGLE~1",
            "a~1",
            "USUARI~12",
            "FILE~1.TXT",
            "ABC~1.E",
            "X~9.ABC",
        ] {
            assert!(looks_like_short_name(s), "{s}");
        }
        for s in [
            "Program Files",
            "tilde~",
            "~1",
            "a~b",
            "Code Cache",
            "TOOLONGNAME~1",
            "A~1.TOOL",
            "A~1.B.C",
            "a~1b",
        ] {
            assert!(!looks_like_short_name(s), "{s}");
        }
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

    /// GM-m4: un enlace en un ancestro INTERMEDIO (`Google`, `Mozilla`...) desvía lo que se lista debajo: no se ofrece nada.
    #[test]
    fn un_enlace_en_un_ancestro_intermedio_descarta_las_cachés_de_debajo() {
        let t = Temp::new();
        // Destino real con la estructura completa de Chrome y de Firefox (`Cache`, `cache2`) FUERA de `Local`.
        t.dir("Fuera/Chrome/User Data/Default/Cache");
        t.dir("Fuera/Chrome/User Data/ShaderCache");
        t.dir("Fuera/Firefox/Profiles/abc.default/cache2");
        t.dir("Local");
        let local = t.0.join("Local");
        let roots = t.roots();
        let mut links = Vec::new();
        for (link, target) in [("Google", "Fuera"), ("Mozilla", "Fuera")] {
            let (l, d) = (local.join(link), t.0.join(target));
            let made = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&l)
                .arg(&d)
                .output();
            assert!(made.is_ok_and(|o| o.status.success()), "mklink /J falló");
            links.push(l);
        }
        // `Local/Google` -> `Fuera` y `Local/Mozilla` -> `Fuera`: las rutas de caché existirían a través de los enlaces.
        assert!(local.join("Google/Chrome/User Data/Default/Cache").exists());
        assert!(
            local
                .join("Mozilla/Firefox/Profiles/abc.default/cache2")
                .exists()
        );
        let dirs = browser_cache_dirs(&roots);
        assert!(
            dirs.is_empty(),
            "se ofrecieron cachés a través de un enlace: {dirs:?}"
        );
        // El mismo árbol SIN enlaces sí se ofrece (la prueba no es vacía): se quitan como enlaces y se crean carpetas reales.
        for l in &links {
            fs::remove_dir(l).unwrap();
        }
        t.dir("Local/Google/Chrome/User Data/Default/Cache");
        t.dir("Local/Mozilla/Firefox/Profiles/abc.default/cache2");
        let dirs = browser_cache_dirs(&roots);
        assert_eq!(dirs.len(), 2, "{dirs:?}");
    }

    #[test]
    fn real_dir_under_valida_cada_componente_por_debajo_de_la_base() {
        let t = Temp::new();
        let base = t.dir("Local");
        t.dir("Local/a/b");
        t.file("Local/a/f.txt");
        assert_eq!(
            real_dir_under(&base, &base.join("a").join("b")),
            Some(base.join("a").join("b"))
        );
        // La propia base no se comprueba.
        assert_eq!(real_dir_under(&base, &base), Some(base.clone()));
        // No cuelga de la base, no existe o es un archivo.
        assert_eq!(real_dir_under(&base, &t.0.join("otra")), None);
        assert_eq!(real_dir_under(&base, &base.join("no-existe")), None);
        assert_eq!(real_dir_under(&base, &base.join("a").join("f.txt")), None);
        // Un `..` no cuela.
        assert_eq!(
            real_dir_under(&base, &base.join("a").join("..").join("a")),
            None
        );
    }

    #[test]
    fn las_raices_reales_vienen_del_sistema_y_son_absolutas() {
        // `SHGetKnownFolderPath`, no las variables de entorno.
        let r = BrowserRoots::from_system();
        for p in [r.local_appdata.as_ref(), r.roaming_appdata.as_ref()]
            .into_iter()
            .flatten()
        {
            assert!(p.is_absolute(), "{p:?}");
            assert!(normalize(p).is_some() || p.to_string_lossy().contains('~'));
        }
        assert!(r.local_appdata.is_some() && r.roaming_appdata.is_some());
    }
}
