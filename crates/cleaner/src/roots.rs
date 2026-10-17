//! Category and protection roots. Every path the `cleaner` may touch comes from here, and is INJECTABLE: production
//! populates it with [`Roots::from_system`] using `SHGetKnownFolderPath` (never environment variables, which a process can
//! change); tests build a [`Roots`] rooted in their temporary directory and never touch the real `%TEMP%`, caches, or Recycle Bin.
//!
//! `%TEMP%` is not a known folder. Windows provides it (`GetTempPathW`), and it is offered as "User temporary files" ONLY
//! if its canonical path is `%LOCALAPPDATA%\Temp` (the default location, with `%LOCALAPPDATA%` from
//! `SHGetKnownFolderPath`) or below it. A `TEMP` redirected elsewhere (Documents, a drive root, all of `%LOCALAPPDATA%`, or
//! `Local\Programs`, for example) cannot make its contents "safe". Any other value rejects the category.

use crate::paths::{PathKey, canonical_path};
use crate::protect::{Exempt, Protection, ProtectionRoots};
use crate::rules::CategoryId;
use crate::sys::{self, FileIdentity};
use std::path::{Path, PathBuf};
use tesela_scanner::{
    BrowserRoots, KnownKind, WER_ZONE_REL, browser_cache_dirs, diagnostic_dirs, known_folder,
};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_LocalAppData,
    FOLDERID_LocalAppDataLow, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile,
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, FOLDERID_Public,
    FOLDERID_RoamingAppData, FOLDERID_UserProfiles, FOLDERID_Videos, FOLDERID_Windows,
};

/// All paths required for cleanup.
#[derive(Clone, Debug, Default)]
pub struct Roots {
    /// User `%TEMP%` (only if it is under `%LOCALAPPDATA%`).
    pub user_temp: Option<PathBuf>,
    /// `Windows\Temp`.
    pub windows_temp: Option<PathBuf>,
    /// `%LOCALAPPDATA%` and `%APPDATA%`, which contain browser data.
    pub browser: BrowserRoots,
    /// The Downloads folder.
    pub downloads: Option<PathBuf>,
    /// Roots rejected by `from_system` at startup, with their reasons. `category_root_checks` returns them as rejections,
    /// so a category rejected due to a link in its path does not disappear silently (H4).
    pub startup_rejections: Vec<(CategoryId, PathBuf, RootRejection)>,
    /// Protection rules.
    pub protection: ProtectionRoots,
}

/// Long canonical path of an existing directory, or `None`.
fn canon(p: &Path) -> Option<PathBuf> {
    canonical_path(p).ok()
}

/// Why a category root was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootRejection {
    /// A component in the path chain (from the volume root) is a reparse point: a link there could redirect the "root" to a
    /// protected folder.
    ReparseInChain { component: String },
    /// The path resolved from the handle differs from the path that was traversed (it changed during verification).
    NotCanonical,
    /// The user's temporary folder is redirected outside `%LOCALAPPDATA%` (it is neither `Local\Temp` nor below it).
    OutsideLocalAppData,
    /// The path cannot be used (it is not an absolute drive-letter path, does not exist, or cannot be opened): Windows error code.
    Os(u32),
}

impl RootRejection {
    /// Should the user be told (the category is unavailable for a security reason they may not expect)? An `Os` error
    /// (missing or inaccessible path) is not shown: there is simply nothing to clean.
    pub fn is_user_visible(&self) -> bool {
        !matches!(self, RootRejection::Os(_))
    }
}

/// A verified category root: its canonical path and the ONLY denied zone it may exempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRoot {
    /// Long canonical path resolved from the handle to the final component.
    pub path: PathBuf,
    /// Identity (volume + FileId) of the object seen while traversing the chain. Approval compares it with the handle used
    /// to open the root for listing; a mismatch means something changed between the two steps (H1).
    pub identity: FileIdentity,
    /// The denied zone containing this root (see [`Exempt`]).
    pub zone: Option<PathBuf>,
}

/// A candidate category root and the result of its verification.
#[derive(Clone, Debug)]
pub struct RootCheck {
    /// The path as received (without resolving it).
    pub raw: PathBuf,
    pub result: Result<VerifiedRoot, RootRejection>,
}

/// B1 layer 1: accepts `raw` only if the entire chain, from the volume root and component by component, contains no
/// reparse points (see [`sys::open_reparse_free_dir`]) and the path returned by the final handle matches the canonical path.
/// Returns the canonical path and identity of the final component.
pub(crate) fn verify_chain(raw: &Path) -> Result<(PathBuf, FileIdentity), RootRejection> {
    let h = sys::open_reparse_free_dir(raw).map_err(|e| match e {
        sys::ChainError::Reparse { component } => RootRejection::ReparseInChain { component },
        sys::ChainError::BadPath => RootRejection::Os(ERROR_BAD_PATHNAME),
        sys::ChainError::Os(c) => RootRejection::Os(c),
    })?;
    let canonical = sys::final_path(&h).map_err(RootRejection::Os)?;
    let identity = sys::meta(&h).map_err(RootRejection::Os)?.identity;
    // `canonical_path` opens by path: if it does not reach the same object as the traversed handle, something changed in between.
    if canon(raw).is_none_or(|c| PathKey::new(&c) != PathKey::new(&canonical)) {
        return Err(RootRejection::NotCanonical);
    }
    Ok((canonical, identity))
}

/// `ERROR_BAD_PATHNAME`.
const ERROR_BAD_PATHNAME: u32 = 161;

/// The DENIED folder (relative to `%LOCALAPPDATA%`) containing each browser's caches.
fn browser_zone_rel(id: CategoryId) -> Option<&'static str> {
    match id {
        CategoryId::ChromeCache => Some(r"Google\Chrome\User Data"),
        CategoryId::EdgeCache => Some(r"Microsoft\Edge\User Data"),
        CategoryId::FirefoxCache => Some(r"Mozilla\Firefox"),
        _ => None,
    }
}

fn kf(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    known_folder(id).and_then(|p| canon(&p))
}

/// Is canonical `temp` accepted as "User temporary files"? Only if it is `<local>\Temp` or below it.
pub(crate) fn accept_user_temp(temp: &Path, local: Option<&Path>) -> bool {
    local.is_some_and(|l| PathKey::new(temp).is_same_or_under(&PathKey::new(&l.join("Temp"))))
}

/// Other profiles' user folders: children of `users_dir` other than `current_profile`.
fn other_profiles(users_dir: &Path, current_profile: Option<&Path>) -> Vec<PathBuf> {
    let cur = current_profile.map(PathBuf::from).map(|p| PathKey::new(&p));
    let Ok(rd) = std::fs::read_dir(users_dir) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| cur.as_ref() != Some(&PathKey::new(p)))
        .collect()
}

impl Roots {
    /// Actual system roots. `exe_dir` is the application's own directory (protected).
    pub fn from_system(exe_dir: Option<&Path>) -> Roots {
        let local = kf(&FOLDERID_LocalAppData);
        let roaming = kf(&FOLDERID_RoamingAppData);
        let local_low = kf(&FOLDERID_LocalAppDataLow);
        let profile = kf(&FOLDERID_Profile);
        let users = kf(&FOLDERID_UserProfiles);
        let windows = kf(&FOLDERID_Windows);
        let program_files = kf(&FOLDERID_ProgramFiles);
        let program_files_x86 = kf(&FOLDERID_ProgramFilesX86);
        let program_data = kf(&FOLDERID_ProgramData);
        let public = kf(&FOLDERID_Public);

        // %TEMP%: only if its chain as returned by Windows (WITHOUT canonicalizing) has no reparse points and, once
        // canonicalized, is %LOCALAPPDATA%\Temp or a descendant of it.
        let mut startup_rejections: Vec<(CategoryId, PathBuf, RootRejection)> = Vec::new();
        let user_temp = crate::volume::temp_path().and_then(|t| match verify_chain(&t) {
            Ok((p, _)) if accept_user_temp(&p, local.as_deref()) => Some(p),
            Ok(_) => {
                startup_rejections.push((
                    CategoryId::UserTemp,
                    t,
                    RootRejection::OutsideLocalAppData,
                ));
                None
            }
            Err(e) => {
                startup_rejections.push((CategoryId::UserTemp, t, e));
                None
            }
        });
        let windows_temp = windows.as_ref().and_then(|w| {
            let raw = w.join("Temp");
            match verify_chain(&raw) {
                Ok((p, _)) => Some(p),
                Err(e) => {
                    startup_rejections.push((CategoryId::WindowsTemp, raw, e));
                    None
                }
            }
        });

        let mut denied: Vec<PathBuf> = Vec::new();
        denied.extend(windows.iter().cloned());
        denied.extend(program_files.iter().cloned());
        denied.extend(program_files_x86.iter().cloned());
        denied.extend(program_data.iter().cloned());
        denied.extend(public.iter().cloned());
        if let Some(u) = &users {
            // Other users' profiles (including `Default`, `Public`, ...), excluding the current one.
            denied.extend(other_profiles(u, profile.as_deref()));
        }
        // Browser data: the profile is NEVER cache (specific cache directories are exceptions below).
        if let Some(l) = &local {
            for id in [
                CategoryId::ChromeCache,
                CategoryId::EdgeCache,
                CategoryId::FirefoxCache,
            ] {
                denied.extend(browser_zone_rel(id).map(|z| l.join(z)));
            }
        }
        if let Some(r) = &roaming {
            denied.push(r.join(r"Mozilla\Firefox"));
            denied.push(r.join("Tesela"));
        }
        if let Some(l) = &local {
            denied.push(l.join("Tesela"));
            // WER: the entire folder is denied; only its two report folders are exceptions (see `zone_of`).
            denied.push(l.join(WER_ZONE_REL));
        }
        denied.extend(exe_dir.map(Path::to_path_buf));

        // Untouchable as destinations: the profile root, AppData, Local, Roaming, LocalLow, and C:\Users.
        let mut untouchable: Vec<PathBuf> = Vec::new();
        untouchable.extend(profile.iter().cloned());
        untouchable.extend(users.iter().cloned());
        untouchable.extend(roaming.iter().cloned());
        untouchable.extend(local.iter().cloned());
        untouchable.extend(local_low.iter().cloned());
        // AppData: the parent of Roaming (and Local).
        for base in [&roaming, &local] {
            if let Some(a) = base.as_ref().and_then(|b| b.parent()) {
                untouchable.push(a.to_path_buf());
            }
        }

        let downloads = kf(&FOLDERID_Downloads);
        let content_roots: Vec<PathBuf> = [
            kf(&FOLDERID_Documents),
            kf(&FOLDERID_Desktop),
            kf(&FOLDERID_Videos),
            kf(&FOLDERID_Pictures),
            kf(&FOLDERID_Music),
            downloads.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();

        Roots {
            user_temp,
            windows_temp,
            browser: BrowserRoots {
                local_appdata: local,
                roaming_appdata: roaming,
            },
            downloads,
            startup_rejections,
            protection: ProtectionRoots {
                denied,
                untouchable,
                content_roots,
                exempt: Vec::new(),
            },
        }
    }

    /// Browser caches that currently exist and whose path chains contain no reparse points (see [`verify_chain`]; only
    /// specific cache directories, see `tesela_scanner::browser_cache_dirs`), with their category and canonical path
    /// resolved from a handle.
    pub fn browser_cache_dirs(&self) -> Vec<(CategoryId, PathBuf)> {
        [
            CategoryId::ChromeCache,
            CategoryId::EdgeCache,
            CategoryId::FirefoxCache,
        ]
        .into_iter()
        .flat_map(|id| {
            self.category_root_checks(id)
                .into_iter()
                .filter_map(move |c| c.result.ok().map(|v| (id, v.path)))
        })
        .collect()
    }

    /// Candidate roots for a category with the `Contents` action, AS RECEIVED (unresolved), and their verification:
    /// a chain with no reparse points from the volume root (B1 layer 1) and the denied zone each root may exempt (layer 2).
    /// A rejected root is not a category: it is not offered or approved and exempts nothing.
    pub fn category_root_checks(&self, id: CategoryId) -> Vec<RootCheck> {
        let raws: Vec<PathBuf> = match id {
            CategoryId::UserTemp => self.user_temp.iter().cloned().collect(),
            CategoryId::WindowsTemp => self.windows_temp.iter().cloned().collect(),
            CategoryId::ChromeCache | CategoryId::EdgeCache | CategoryId::FirefoxCache => {
                let kind = match id {
                    CategoryId::ChromeCache => KnownKind::ChromeCache,
                    CategoryId::EdgeCache => KnownKind::EdgeCache,
                    _ => KnownKind::FirefoxCache,
                };
                browser_cache_dirs(&self.browser)
                    .into_iter()
                    .filter(|(k, _)| *k == kind)
                    .map(|(_, p)| p)
                    .collect()
            }
            // Error report and dump folders come from the scanner (a single definition of these paths).
            CategoryId::WerReports | CategoryId::CrashDumps => {
                let kind = if id == CategoryId::WerReports {
                    KnownKind::WerReports
                } else {
                    KnownKind::CrashDumps
                };
                diagnostic_dirs(&self.browser)
                    .into_iter()
                    .filter(|(k, _)| *k == kind)
                    .map(|(_, p)| p)
                    .collect()
            }
            _ => Vec::new(),
        };
        let startup = self
            .startup_rejections
            .iter()
            .filter(|(c, ..)| *c == id)
            .map(|(_, raw, why)| RootCheck {
                raw: raw.clone(),
                result: Err(why.clone()),
            });
        let checked = raws
            .into_iter()
            .map(|raw| {
                let result = verify_chain(&raw).map(|(path, identity)| VerifiedRoot {
                    zone: self.zone_of(id, &path),
                    path,
                    identity,
                });
                RootCheck { raw, result }
            })
            .collect::<Vec<_>>();
        startup.chain(checked).collect()
    }

    /// Rejected category roots and their reasons (only those the user should know about; see
    /// [`RootRejection::is_user_visible`]).
    pub fn unavailable_roots(&self, id: CategoryId) -> Vec<(PathBuf, RootRejection)> {
        self.category_root_checks(id)
            .into_iter()
            .filter_map(|c| c.result.err().map(|e| (c.raw, e)))
            .filter(|(_, e)| e.is_user_visible())
            .collect()
    }

    /// The ONLY denial that canonical category root `root` for `id` may exempt (see [`Exempt`]): the denied folder that
    /// contains it. `Windows\Temp` → the Windows folder (its parent); browser caches → their `User Data` folder (Firefox:
    /// `Mozilla\Firefox`), computed from the same path as the denial; `%TEMP%` → none (`Local` is untouchable, not denied).
    fn zone_of(&self, id: CategoryId, root: &Path) -> Option<PathBuf> {
        match id {
            CategoryId::WindowsTemp => root.parent().map(Path::to_path_buf),
            CategoryId::ChromeCache | CategoryId::EdgeCache | CategoryId::FirefoxCache => {
                let local = self.browser.local_appdata.as_ref()?;
                browser_zone_rel(id).map(|z| local.join(z))
            }
            // Error report folders are inside the denied `WER` zone; only that zone is exempted.
            CategoryId::WerReports => self
                .browser
                .local_appdata
                .as_ref()
                .map(|l| l.join(WER_ZONE_REL)),
            // `%LOCALAPPDATA%\CrashDumps` is not inside a denied zone (like `%TEMP%`: `Local` is untouchable, not denied).
            _ => None,
        }
    }

    /// Canonical directory roots for a category with the `Contents` action that passed verification.
    pub fn category_roots(&self, id: CategoryId) -> Vec<PathBuf> {
        self.category_root_checks(id)
            .into_iter()
            .filter_map(|c| c.result.ok().map(|v| v.path))
            .collect()
    }

    /// Diagnostic folders are never eligible for permanent deletion. Use the fixed known-folder
    /// paths rather than currently existing category roots: a folder can disappear while a plan
    /// is measured and then return with the same identity before execution.
    pub(crate) fn is_diagnostic_path(&self, path: &Path) -> bool {
        let candidate = PathKey::new(path);
        diagnostic_dirs(&self.browser)
            .into_iter()
            .any(|(_, root)| candidate.is_same_or_under(&PathKey::new(&root)))
    }

    /// Protection rules with verified category roots as exceptions: descendants of those roots are not denied, but they
    /// exempt ONLY the denied zone each one declares (never "that zone or any ancestor").
    pub fn protection(&self) -> Protection {
        let mut pr = self.protection.clone();
        for id in [
            CategoryId::UserTemp,
            CategoryId::WindowsTemp,
            CategoryId::ChromeCache,
            CategoryId::EdgeCache,
            CategoryId::FirefoxCache,
            CategoryId::WerReports,
            CategoryId::CrashDumps,
        ] {
            pr.exempt.extend(
                self.category_root_checks(id)
                    .into_iter()
                    .filter_map(|c| c.result.ok())
                    .map(|v| Exempt::new(v.path, v.zone)),
            );
        }
        Protection::new(&pr).with_mount_roots(&crate::volume::mounted_roots())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protect::Verdict;

    #[test]
    fn las_raices_del_sistema_son_coherentes() {
        let r = Roots::from_system(None);
        let p = r.protection();
        // The profile cannot be a destination; its known folders are "Contents" roots only.
        let profile = kf(&FOLDERID_Profile).expect("perfil");
        assert_eq!(
            p.target_verdict(&PathKey::new(&profile)),
            Verdict::Untouchable
        );
        let docs = kf(&FOLDERID_Documents).expect("Documentos");
        assert_eq!(
            p.target_verdict(&PathKey::new(&docs)),
            Verdict::ContentsOnly
        );
        let win = kf(&FOLDERID_Windows).expect("Windows");
        assert_eq!(p.target_verdict(&PathKey::new(&win)), Verdict::Denied);
        assert_eq!(
            p.target_verdict(&PathKey::new(&win.join("System32"))),
            Verdict::Denied
        );
        // Category roots (if present) are allowed destinations and are ALWAYS long canonical paths.
        for id in [CategoryId::UserTemp, CategoryId::WindowsTemp] {
            for root in r.category_roots(id) {
                assert_eq!(
                    p.target_verdict(&PathKey::new(&root)),
                    Verdict::Allowed,
                    "{root:?}"
                );
                assert_eq!(canonical_path(&root).unwrap(), root);
            }
        }
        // If offered, the user's temporary directory is under LocalAppData.
        if let (Some(t), Some(l)) = (&r.user_temp, &r.browser.local_appdata) {
            assert!(PathKey::new(t).is_same_or_under(&PathKey::new(l)));
        }
        // Browser caches are specific cache directories, never the profile.
        for (_, c) in r.browser_cache_dirs() {
            assert_eq!(p.target_verdict(&PathKey::new(&c)), Verdict::Allowed);
            let s = c.to_string_lossy().to_lowercase();
            assert!(
                !s.ends_with("user data") && !s.ends_with("profiles"),
                "{c:?}"
            );
        }
        // Deny browser profiles.
        if let Some(l) = &r.browser.local_appdata {
            let ud = l.join(r"Google\Chrome\User Data\Default\History");
            assert_eq!(p.target_verdict(&PathKey::new(&ud)), Verdict::Denied);
        }
    }

    /// Chain verification requests only ATTRIBUTES for each component: `Windows\Temp` is accepted even if a non-elevated user
    /// cannot list it (previously, requesting `FILE_LIST_DIRECTORY` silently rejected it). Existing system roots also pass
    /// chain verification on this machine.
    #[test]
    fn la_cadena_no_exige_poder_listar_la_carpeta() {
        let win = kf(&FOLDERID_Windows).expect("Windows");
        let temp = win.join("Temp");
        if temp.is_dir() {
            let ok = verify_chain(&temp);
            assert_eq!(
                ok.as_ref().ok().map(|(p, _)| p.as_path()),
                Some(canon(&temp).unwrap().as_path()),
                "{ok:?}"
            );
            assert!(Roots::from_system(None).windows_temp.is_some());
        }
        let local = kf(&FOLDERID_LocalAppData).expect("LocalAppData");
        assert!(verify_chain(&local).is_ok());
    }

    #[test]
    fn solo_se_acepta_un_temp_que_es_localappdata_temp_o_cuelga_de_el() {
        let local = Path::new(r"C:\Users\<usuario>\AppData\Local");
        let ok = |s: &str| accept_user_temp(Path::new(s), Some(local));
        // The default location, with different casing, with the verbatim prefix, and with a subdirectory.
        assert!(ok(r"C:\Users\<usuario>\AppData\Local\Temp"));
        assert!(ok(r"c:\users\<USUARIO>\appdata\local\TEMP"));
        assert!(ok(r"\\?\C:\Users\<usuario>\AppData\Local\Temp"));
        assert!(ok(r"C:\Users\<usuario>\AppData\Local\Temp\sub"));
        // TEMP = all of %LOCALAPPDATA%, other Local folders, Documents, or the drive root: reject the category.
        assert!(!ok(r"C:\Users\<usuario>\AppData\Local"));
        assert!(!ok(r"C:\Users\<usuario>\AppData\Local\Programs"));
        assert!(!ok(r"C:\Users\<usuario>\AppData\Local\Temporal"));
        assert!(!ok(r"C:\Users\<usuario>\AppData\Local\Microsoft\Temp"));
        assert!(!ok(r"C:\Users\<usuario>\Documents"));
        assert!(!ok(r"C:\"));
        assert!(!ok(r"C:\Windows\Temp"));
        // Sin LocalAppData conocida no se ofrece nada.
        assert!(!accept_user_temp(
            Path::new(r"C:\Users\<usuario>\AppData\Local\Temp"),
            None
        ));
    }
}
