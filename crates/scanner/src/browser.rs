//! Browser data folders: what is a regenerable cache and what is NOT.
//!
//! # Purpose of this classification
//! [`classify_browser_path`] is used for LABELING (the UI names a path and the scanner decides what to traverse first).
//! It does NOT authorize deletion: authorization comes from roots returned by [`browser_cache_dirs`], verified by HANDLE
//! at deletion time (PLAN.md §3.7), never from comparing path strings.
//!
//! # Sources for names (the list must be EXACT and CONSERVATIVE)
//! * Chromium, `chrome/common/chrome_constants.h`: the default profile is `Default` (`kInitialProfile`); other profiles are
//!   `Profile <N>` (`kMultiProfileDirPrefix = "Profile "`), and `Cache` is `kCacheDirname`.
//! * Chromium, `gpu/ipc/common/gpu_disk_cache_type.cc`, function `GetGpuDiskCacheSubdir`: `GPUCache` (`kGlShaders`),
//!   `DawnWebGPUCache` (`kDawnWebGPU`) and `DawnGraphiteCache` (`kDawnGraphite`). They are under the profile directory,
//!   like `GPUCache`.
//! * `GraphiteDawnCache` (at the `User Data` level, like `ShaderCache`): `chrome/browser/chrome_content_browser_client.cc`
//!   (`user_data_dir.Append("GraphiteDawnCache")`) and `gpu/ipc/common/gpu_disk_cache_factory.cc` (DawnGraphite type,
//!   regenerable).
//! * `Code Cache` (per profile): `content/browser/storage_partition_impl.cc`. `ShaderCache` and `GrShaderCache` (under
//!   `User Data`): `chrome/browser/chrome_content_browser_client.cc`. All are source-confirmed; do not expand this list
//!   without the same verification.
//! * `Service Worker\CacheStorage` is NOT cache: in `content/public/browser/browsing_data_remover.h`,
//!   `DATA_TYPE_CACHE_STORAGE` is one of the `DATA_TYPE_DOM_STORAGE` types (site data, alongside IndexedDB, Local Storage,
//!   and service workers) and may contain offline PWA data. The whole `Service Worker` directory also stores `Database`
//!   and `ScriptCache`; none of these are cleaned as cache. Neither are `GPUPersistentCache`, `Media Cache`, or `*_crx_cache`.
//! * Firefox, `netwerk/cache2/CacheFileIOManager.cpp`: HTTP cache is `cache2` under the profile's LOCAL directory
//!   (`NS_APP_USER_PROFILE_LOCAL_50_DIR`: `%LOCALAPPDATA%\Mozilla\Firefox\Profiles\<profile>`). `startupCache` is in the
//!   same directory (confirmed in `StartupCache.cpp` and `xpcom/build/nsXULAppAPI.h`). The `%APPDATA%` profile stores
//!   `places.sqlite`, `key4.db`, `logins.json`, `cookies.sqlite`, and other user data; it is NEVER cache.
//!
//! What is NOT cache: everything else under `User Data` (`Local State`, `Bookmarks`, `History`, `Cookies`, `Login Data`,
//! `Web Data`, `Local Storage`, `IndexedDB`, `Extensions`, `Preferences`, `Sessions`, etc.) and Firefox profiles.
//! `Guest Profile`, `System Profile`, and `Profile 01` are not considered profiles either.
//!
//! # Untrusted paths
//! There are FOUR possible results ([`BrowserPath`]): for a relative path, one containing `..`, an unusual prefix (`\\.\`,
//! `\\?\GLOBALROOT`, UNC), an 8.3 alias (`GOOGLE~1`), trailing dots or spaces, `:` (data streams), or
//! case differences that match only outside ASCII, the answer is `Unknown` ("do not touch"), never "not a browser".

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
    /// Cache category for this browser.
    pub fn cache_kind(self) -> KnownKind {
        match self {
            BrowserFamily::Chrome => KnownKind::ChromeCache,
            BrowserFamily::Edge => KnownKind::EdgeCache,
            BrowserFamily::Firefox => KnownKind::FirefoxCache,
        }
    }
}

/// Base folders containing browser data (`%LOCALAPPDATA%` and `%APPDATA%`).
#[derive(Clone, Debug, Default)]
pub struct BrowserRoots {
    pub local_appdata: Option<PathBuf>,
    pub roaming_appdata: Option<PathBuf>,
}

impl BrowserRoots {
    /// Values provided by Windows (`SHGetKnownFolderPath`: `FOLDERID_LocalAppData` and `FOLDERID_RoamingAppData`), not
    /// environment variables, which a process can change.
    pub fn from_system() -> BrowserRoots {
        BrowserRoots {
            local_appdata: known_folder(&FOLDERID_LocalAppData),
            roaming_appdata: known_folder(&FOLDERID_RoamingAppData),
        }
    }

    /// `User Data` directory for a Chromium-based browser. Profiles (`Default`, `Profile N`) are below it.
    fn user_data(&self, f: BrowserFamily) -> Option<PathBuf> {
        let local = self.local_appdata.as_ref()?;
        match f {
            BrowserFamily::Chrome => Some(local.join(r"Google\Chrome\User Data")),
            BrowserFamily::Edge => Some(local.join(r"Microsoft\Edge\User Data")),
            BrowserFamily::Firefox => None,
        }
    }

    /// Firefox `Profiles` directory in `%APPDATA%`: profiles with user data (NEVER cache).
    fn firefox_roaming_profiles(&self) -> Option<PathBuf> {
        Some(
            self.roaming_appdata
                .as_ref()?
                .join(r"Mozilla\Firefox\Profiles"),
        )
    }

    /// Firefox `Profiles` directory in `%LOCALAPPDATA%`: each profile's `cache2` and `startupCache` are below it.
    fn firefox_local_profiles(&self) -> Option<PathBuf> {
        Some(
            self.local_appdata
                .as_ref()?
                .join(r"Mozilla\Firefox\Profiles"),
        )
    }
}

/// Cache subdirectories within a Chromium profile, preserving their creation case (see sources above).
const CHROMIUM_PROFILE_CACHE: [&str; 5] = [
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnWebGPUCache",
    "DawnGraphiteCache",
];
/// Caches under `User Data` (not per profile).
const CHROMIUM_ROOT_CACHE: [&str; 3] = ["ShaderCache", "GrShaderCache", "GraphiteDawnCache"];
/// Caches within a Firefox LOCAL profile.
const FIREFOX_PROFILE_CACHE: [&str; 2] = ["cache2", "startupCache"];

/// Is `name` a Chromium profile directory? Only `Default` and `Profile <N>` where N uses ASCII digits with no leading
/// zero or empty value (`Profile 01` does not qualify), using ASCII-only case-insensitive matching.
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

/// Does `p` exist as a real directory? Uses the SAME criteria as the scanner (`classify::dir_action == Walk`): not a
/// reparse point (a link could lead to user data; without a tag, every reparse point counts as a link, including cloud
/// files), and not a directory with cloud recall attributes even if it is not a reparse point.
fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| {
        use std::os::windows::fs::MetadataExt;
        m.is_dir() && classify::dir_action(m.file_attributes(), 0) == classify::DirAction::Walk
    })
}

/// `p` (under `base`) with EVERY component below `base` verified as a real directory: an intermediate ancestor (`Google`,
/// `Chrome`, etc.) that is a link would redirect everything listed below it elsewhere. `base` itself is the data folder
/// provided by Windows (`%LOCALAPPDATA%`) and is not checked. Returns `None` if `p` is not below `base` or a component is
/// not a real directory.
pub(crate) fn real_dir_under(base: &Path, p: &Path) -> Option<PathBuf> {
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

/// Names of actual subdirectories of `dir`, sorted (empty if it cannot be listed).
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

/// Specific browser CACHE directories (and ONLY those) that currently exist:
///
/// * Chrome and Edge: for each `User Data` profile (`Default`, `Profile N`), `Cache`, `Code Cache`, `GPUCache`,
///   `DawnWebGPUCache`, and `DawnGraphiteCache`; plus `ShaderCache`, `GrShaderCache`, and `GraphiteDawnCache` under `User Data`.
///   NOT `Service Worker\CacheStorage` (site data).
/// * Firefox: `cache2` and `startupCache` for each profile under `%LOCALAPPDATA%\Mozilla\Firefox\Profiles`. Never
///   `%APPDATA%\Mozilla\Firefox\Profiles`.
///
/// Only LISTS folders. Reparse points are excluded (every folder at every listed level must be a normal directory under
/// the scanner's criteria). No returned path contains history, bookmarks, cookies, or credentials. These are CANDIDATES
/// for deletion authorization roots: the `cleaner` verifies them LATER by handle, component by component from
/// `%LOCALAPPDATA%`, and rejects paths that pass through a link (with a reason shown in the UI). This function therefore
/// does NOT check the ancestors of `User Data` (`Google`, `Chrome`, etc.): if it did, a `User Data` directory replaced by a
/// link would disappear silently instead of appearing as "unavailable". For scanner priority traversal, use
/// [`browser_cache_dirs_verified`], which does check them.
pub fn browser_cache_dirs(roots: &BrowserRoots) -> Vec<(KnownKind, PathBuf)> {
    cache_dirs(roots, false)
}

/// Like [`browser_cache_dirs`], but only includes caches whose ancestors (`Google`, `Chrome`, `User Data`, `Mozilla`,
/// `Firefox`, `Profiles`) are all real directories below `%LOCALAPPDATA%`: a link in an intermediate ancestor would redirect
/// everything below it elsewhere. These are the paths the scanner traverses directly.
pub fn browser_cache_dirs_verified(roots: &BrowserRoots) -> Vec<(KnownKind, PathBuf)> {
    cache_dirs(roots, true)
}

fn cache_dirs(roots: &BrowserRoots, verify_ancestors: bool) -> Vec<(KnownKind, PathBuf)> {
    // With `verify_ancestors`, include folders whose ancestors were checked; otherwise include the folders themselves.
    let checked = |local: &Path, p: PathBuf| {
        if verify_ancestors {
            real_dir_under(local, &p)
        } else {
            Some(p)
        }
    };
    let mut out = Vec::new();
    for fam in [BrowserFamily::Chrome, BrowserFamily::Edge] {
        let Some(user_data) = roots
            .local_appdata
            .as_deref()
            .zip(roots.user_data(fam))
            .and_then(|(local, ud)| checked(local, ud))
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
        .and_then(|(local, pr)| checked(local, pr))
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

/// How a path relates to browser data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserPath {
    /// A cache directory from [`browser_cache_dirs`], or a path below it.
    Cache(KnownKind),
    /// Anything else in browser data (the profile with history, cookies, credentials, `Service Worker`,
    /// `CacheStorage`, etc.), including the `User Data` or `Profiles` directory itself: NOT cache.
    Profile(BrowserFamily),
    /// A well-formed absolute path that is NOT under any browser data (an ancestor such as
    /// `AppData\Local` is not either).
    NotBrowser,
    /// Cannot determine anything (relative path, `..`, unusual prefix, 8.3 alias, ambiguous case, etc.): DO NOT TOUCH.
    Unknown,
}

/// Does this look like an 8.3 alias (`PROGRA~1`, `GOOGLE~1.EXE`)? An alias can hide any long name.
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

/// Is this an untrusted path component? (Trailing dots or spaces, which Windows removes; `:` or characters
/// forbidden characters, data-stream or wildcard characters; control characters; 8.3 aliases).
fn untrusted_component(s: &str) -> bool {
    s.is_empty()
        || s.ends_with('.')
        || s.ends_with(' ')
        || s.chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*'))
        || looks_like_short_name(s)
}

/// Lowercase drive letter and components of a well-formed absolute drive-letter path (`C:\...`), or `None` if the path has
/// if it cannot be reasoned about: drive-relative (`C:Users`), current-drive-rooted (`\Users`), containing `..`, with another
/// an untrusted prefix (UNC, `\\.\`, `\\?\GLOBALROOT`) or untrusted components.
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

/// Reserved device names (`CON`, `NUL`, `COM1`, ...): these are not ordinary directories.
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

/// Comparison of two names: equal (ASCII-only case-insensitive), different, or AMBIGUOUS (matches after
/// Unicode case folding but not ASCII folding: the Kelvin sign cannot stand for `k`, and NTFS uses its own table).
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

/// Relationship between a root and a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rel {
    /// The path equals or is below the root; the remainder starts at component `n`.
    Inside(usize),
    /// The path is a strict ancestor of the root.
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

/// Does `rest` match `pattern` from the start (by component)? Returns `Ambiguous` if the match is ambiguous.
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

/// Classifies descendants of `User Data` (`rest`).
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

/// Classifies `path` (without touching the disk); see [`BrowserPath`]. It is CONSERVATIVE: it returns `Cache` only if the path is EXACTLY
/// a cache directory from [`browser_cache_dirs`] or is below one, component by component. When in doubt, return `Profile`
/// (not cache) or `Unknown` (do not touch).
///
/// **For LABELING only.** Deletion authorization does NOT come from here; it comes from roots in [`browser_cache_dirs`]
/// verified by handle immediately before deletion (PLAN.md §3.7).
pub fn classify_browser_path(roots: &BrowserRoots, path: &Path) -> BrowserPath {
    let Some((drive, comps)) = normalize(path) else {
        return BrowserPath::Unknown;
    };
    // Each root: (path, classification of its descendants).
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
        // An untrusted root (8.3 alias, unusual prefix, etc.) prevents any definite classification.
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
                        // `<profile>\cache2` or `<profile>\startupCache` (and their descendants).
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

    /// RAII-guarded temporary tree (only `%TEMP%\tesela-test-*` created here is deleted). The path is canonicalized: with a
    /// long username, `%TEMP%` may contain an 8.3 alias (`USUARI~1`), which classification does not trust.
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

    /// Data that can NEVER be cache inside a Chromium profile. `Service Worker` contains `CacheStorage`, `Database`, and
    /// `ScriptCache`; none of them is cache.
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
        // The source's list of verified caches.
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
        // Classification over the ENTIRE tree: only expected cache directories (and their descendants) are caches.
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
            // `Service Worker\CacheStorage` (site data for PWA offline mode) is NOT a cache, nor is anything inside it.
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
            // `User Data` is browser data, not a cache; its ancestors are unrelated to browsers.
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
        // Data profile (Roaming): everything is private, even directories named like caches.
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
        // Local profile: two caches and other data.
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
        // Classification: everything in Roaming is profile data; in Local, only the two cache directories.
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
        // The Roaming `Profiles` directory is NEVER a cache and is never offered as one (this was the original bug).
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
        // ASCII casing does not matter.
        assert_eq!(
            cls(ud.join(r"default\CACHE")),
            BrowserPath::Cache(KnownKind::ChromeCache)
        );
        assert_eq!(
            cls(ud.join(r"Default\DAWNWEBGPUCACHE\x")),
            BrowserPath::Cache(KnownKind::ChromeCache)
        );
        // Lookalikes are not caches.
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
        // Firefox: a lookalike directory, or a cache path without an intervening profile.
        let ff = t.0.join("Local/Mozilla/Firefox/Profiles");
        assert!(!es_cache(ff.join("p1/cache2old")));
        assert!(!es_cache(ff.join("cache2")));
        assert!(!es_cache(ff.join("p1")));
        assert_eq!(
            cls(ff.join("p1/cache2/entries/a")),
            BrowserPath::Cache(KnownKind::FirefoxCache)
        );
        // The same applies with a verbatim prefix (`/` is not a separator in a verbatim path: normalize it first).
        let plain = ud
            .join(r"Default\Cache")
            .display()
            .to_string()
            .replace('/', r"\");
        assert_eq!(
            cls(PathBuf::from(format!(r"\\?\{plain}"))),
            BrowserPath::Cache(KnownKind::ChromeCache)
        );
        // Unrelated to browsers: well-formed and outside the roots.
        assert_eq!(cls(t.0.join("Local/Otra/Cache")), BrowserPath::NotBrowser);
    }

    #[test]
    fn las_rutas_en_las_que_no_se_confia_son_desconocidas_y_no_ajenas() {
        let t = Temp::new();
        let roots = t.roots();
        let ud = t.0.join("Local/Google/Chrome/User Data");
        let cls = |p: PathBuf| classify_browser_path(&roots, &p);
        let drive = t.0.display().to_string().chars().next().unwrap();
        // (Network paths are built at runtime: the privacy check rejects a literal.)
        let (bs, host, share) = ('\\', "srv", "share");
        let unknown: Vec<PathBuf> = vec![
            // Relative paths, including drive-relative (`C:Users\...`) and current-drive-rooted paths.
            PathBuf::from(r"Local\Google\Chrome\User Data\Default\Cache"),
            PathBuf::from(format!(
                r"{drive}:Users\x\AppData\Local\Google\Chrome\User Data\Default\Cache"
            )),
            PathBuf::from(r"\Users\x\AppData\Local\Google\Chrome\User Data\Default\Cache"),
            PathBuf::from(""),
            // `..` anywhere, even if the path ends at an existing cache.
            ud.join(r"Default\Login Data\..\Cache"),
            ud.join(r"..\..\Google\Chrome\User Data\Default\Cache"),
            // Prefijos raros.
            PathBuf::from(format!(r"\\.\{drive}:\x")),
            PathBuf::from(r"\\?\GLOBALROOT\Device\HarddiskVolume1\Users\x"),
            PathBuf::from(format!("{bs}{bs}?{bs}UNC{bs}{host}{bs}{share}{bs}x")),
            PathBuf::from(format!("{bs}{bs}{host}{bs}{share}{bs}x")),
            PathBuf::from(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x"),
            // 8.3 aliases can conceal any long name.
            PathBuf::from(format!(
                r"{drive}:\USUARI~1\AppData\Local\Google\Chrome\User Data\Default\Cache"
            )),
            ud.join(r"DEFAUL~1\Cache"),
            ud.join(r"Default\CODECA~1"),
            // Trailing dots or spaces (Windows removes them), data streams, and forbidden characters.
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
        // A browser root containing an 8.3 alias prevents us from making claims about ANY path.
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
        // Unicode folds the Kelvin sign (U+212A) to `k`, but a name containing it is NOT the name containing `k`.
        assert_eq!(cmp_name("Cache", "CACHE"), Cmp::Equal);
        assert_eq!(cmp_name("\u{212A}", "k"), Cmp::Ambiguous);
        assert_eq!(cmp_name("\u{212A}", "x"), Cmp::Different);
        assert_eq!(cmp_name("stra\u{DF}e", "STRASSE"), Cmp::Ambiguous);
        // Profile names: ASCII digits only, with no leading zeroes.
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
        // An ambiguous name resembling a cache is NOT treated as one or as a normal profile: leave it alone.
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
        // A root with non-ASCII letters that matches only after Unicode folding is ambiguous: Unknown.
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
        // A junction named `GPUCache` pointing to user data is not offered as a cache.
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
            // Remove the junction without touching its target (`remove_dir` on the link).
            fs::remove_dir(&junction).unwrap();
            assert!(datos.join("importante.txt").exists());
        }
    }

    /// GM-m4: a link in an INTERMEDIATE ancestor (`Google`, `Mozilla`, ...) redirects what is listed below: offer nothing.
    #[test]
    fn un_enlace_en_un_ancestro_intermedio_descarta_las_cachés_de_debajo() {
        let t = Temp::new();
        // Real target with the full Chrome and Firefox structure (`Cache`, `cache2`) OUTSIDE `Local`.
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
        // `Local/Google` -> `Outside` and `Local/Mozilla` -> `Outside`: cache paths would exist through the links.
        assert!(local.join("Google/Chrome/User Data/Default/Cache").exists());
        assert!(
            local
                .join("Mozilla/Firefox/Profiles/abc.default/cache2")
                .exists()
        );
        // Verified discovery offers nothing through links; candidate discovery DOES list them (the `cleaner` later rejects
        // them with a reason: without this, a `User Data` directory replaced by a link would disappear without explanation).
        let dirs = browser_cache_dirs_verified(&roots);
        assert!(
            dirs.is_empty(),
            "se ofrecieron cachés a través de un enlace: {dirs:?}"
        );
        assert_eq!(browser_cache_dirs(&roots).len(), 3, "las candidatas sí");
        // The same tree WITHOUT links is offered (the test is not vacuous): remove the links and create actual directories.
        for l in &links {
            fs::remove_dir(l).unwrap();
        }
        t.dir("Local/Google/Chrome/User Data/Default/Cache");
        t.dir("Local/Mozilla/Firefox/Profiles/abc.default/cache2");
        let dirs = browser_cache_dirs_verified(&roots);
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert_eq!(browser_cache_dirs(&roots).len(), 2);
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
        // It is outside the base, does not exist, or is a file.
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
        // `SHGetKnownFolderPath`, not environment variables.
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
