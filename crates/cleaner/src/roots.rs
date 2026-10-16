//! Raíces de las categorías y de la protección. Todo lo que el `cleaner` va a tocar sale de aquí, y es INYECTABLE: en
//! producción lo rellena [`Roots::from_system`] con `SHGetKnownFolderPath` (nunca con variables de entorno, que un
//! proceso puede alterar); las pruebas construyen un [`Roots`] sobre su carpeta temporal y así nunca tocan el `%TEMP%`,
//! las cachés ni la Papelera reales.
//!
//! `%TEMP%` no es una carpeta conocida. Se pregunta a Windows (`GetTempPathW`) y SOLO se ofrece como "Temporales del
//! usuario" si, ya canonicalizada, es `%LOCALAPPDATA%\Temp` (el sitio por defecto, con `%LOCALAPPDATA%` de
//! `SHGetKnownFolderPath`) o cuelga de ella: un `TEMP` redirigido a otra parte (Documentos, la raíz de una unidad,
//! `%LOCALAPPDATA%` entero o `Local\Programs`...) no puede volver "seguro" lo que hay allí. Cualquier otro valor descarta
//! la categoría.

use crate::paths::{PathKey, canonical_path};
use crate::protect::{Protection, ProtectionRoots};
use crate::rules::CategoryId;
use std::path::{Path, PathBuf};
use tesela_scanner::{BrowserRoots, KnownKind, browser_cache_dirs, known_folder};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_LocalAppData,
    FOLDERID_LocalAppDataLow, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile,
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, FOLDERID_Public,
    FOLDERID_RoamingAppData, FOLDERID_UserProfiles, FOLDERID_Videos, FOLDERID_Windows,
};

/// Todas las rutas de las que depende la limpieza.
#[derive(Clone, Debug, Default)]
pub struct Roots {
    /// `%TEMP%` del usuario (solo si cuelga de `%LOCALAPPDATA%`).
    pub user_temp: Option<PathBuf>,
    /// `Windows\Temp`.
    pub windows_temp: Option<PathBuf>,
    /// `%LOCALAPPDATA%` y `%APPDATA%`, de las que cuelgan los datos de los navegadores.
    pub browser: BrowserRoots,
    /// La carpeta Descargas.
    pub downloads: Option<PathBuf>,
    /// Qué proteger.
    pub protection: ProtectionRoots,
}

/// Ruta canónica larga de un directorio que existe, o `None`.
fn canon(p: &Path) -> Option<PathBuf> {
    canonical_path(p).ok()
}

fn kf(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    known_folder(id).and_then(|p| canon(&p))
}

/// ¿Se acepta `temp` (canónica) como "Temporales del usuario"? Solo si es `<local>\Temp` o cuelga de ella.
pub(crate) fn accept_user_temp(temp: &Path, local: Option<&Path>) -> bool {
    local.is_some_and(|l| PathKey::new(temp).is_same_or_under(&PathKey::new(&l.join("Temp"))))
}

/// Las carpetas de usuario de otros perfiles: los hijos de `users_dir` que no son `current_profile`.
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
    /// Las raíces reales del equipo. `exe_dir` es la carpeta de la propia aplicación (se protege).
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

        // %TEMP%: solo si, canonicalizado, es %LOCALAPPDATA%\Temp o cuelga de ella.
        let user_temp = crate::volume::temp_path()
            .and_then(|t| canon(&t))
            .filter(|t| accept_user_temp(t, local.as_deref()));
        let windows_temp = windows.as_ref().and_then(|w| canon(&w.join("Temp")));

        let mut denied: Vec<PathBuf> = Vec::new();
        denied.extend(windows.iter().cloned());
        denied.extend(program_files.iter().cloned());
        denied.extend(program_files_x86.iter().cloned());
        denied.extend(program_data.iter().cloned());
        denied.extend(public.iter().cloned());
        if let Some(u) = &users {
            // Los perfiles de otros usuarios (incluidos `Default`, `Public`...), no el actual.
            denied.extend(other_profiles(u, profile.as_deref()));
        }
        // Los datos de los navegadores: el perfil NUNCA es caché (las cachés concretas son excepciones, abajo).
        if let Some(l) = &local {
            denied.push(l.join(r"Google\Chrome\User Data"));
            denied.push(l.join(r"Microsoft\Edge\User Data"));
            denied.push(l.join(r"Mozilla\Firefox"));
        }
        if let Some(r) = &roaming {
            denied.push(r.join(r"Mozilla\Firefox"));
            denied.push(r.join("Tesela"));
        }
        if let Some(l) = &local {
            denied.push(l.join("Tesela"));
        }
        denied.extend(exe_dir.map(Path::to_path_buf));

        // Intocables como destino: la raíz del perfil, AppData, Local, Roaming, LocalLow y C:\Users.
        let mut untouchable: Vec<PathBuf> = Vec::new();
        untouchable.extend(profile.iter().cloned());
        untouchable.extend(users.iter().cloned());
        untouchable.extend(roaming.iter().cloned());
        untouchable.extend(local.iter().cloned());
        untouchable.extend(local_low.iter().cloned());
        // AppData: el padre de Roaming (y de Local).
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
            protection: ProtectionRoots {
                denied,
                untouchable,
                content_roots,
                exempt: Vec::new(),
            },
        }
    }

    /// Las cachés de navegador que existen ahora mismo (solo directorios de caché concretos, ver
    /// `tesela_scanner::browser_cache_dirs`), con su categoría.
    pub fn browser_cache_dirs(&self) -> Vec<(CategoryId, PathBuf)> {
        browser_cache_dirs(&self.browser)
            .into_iter()
            .filter_map(|(k, p)| {
                let id = match k {
                    KnownKind::ChromeCache => CategoryId::ChromeCache,
                    KnownKind::EdgeCache => CategoryId::EdgeCache,
                    KnownKind::FirefoxCache => CategoryId::FirefoxCache,
                    _ => return None,
                };
                // La autorización sale de la raíz canónica: se resuelve por handle (nunca de la cadena).
                canon(&p).map(|c| (id, c))
            })
            .collect()
    }

    /// Las raíces (directorios) de una categoría con acción `Contents`.
    pub fn category_roots(&self, id: CategoryId) -> Vec<PathBuf> {
        match id {
            CategoryId::UserTemp => self.user_temp.iter().cloned().collect(),
            CategoryId::WindowsTemp => self.windows_temp.iter().cloned().collect(),
            CategoryId::ChromeCache | CategoryId::EdgeCache | CategoryId::FirefoxCache => self
                .browser_cache_dirs()
                .into_iter()
                .filter(|(c, _)| *c == id)
                .map(|(_, p)| p)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// La protección con las raíces de las categorías como excepciones (lo que cuelga de ellas no se deniega).
    pub fn protection(&self) -> Protection {
        let mut pr = self.protection.clone();
        for id in [
            CategoryId::UserTemp,
            CategoryId::WindowsTemp,
            CategoryId::ChromeCache,
            CategoryId::EdgeCache,
            CategoryId::FirefoxCache,
        ] {
            pr.exempt.extend(self.category_roots(id));
        }
        Protection::new(&pr)
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
        // El perfil es intocable como destino y sus carpetas conocidas solo "Contenido de".
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
        // Las raíces de las categorías (si existen) son destino permitido y SIEMPRE canónicas largas.
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
        // El temporal del usuario, si se ofrece, cuelga de LocalAppData.
        if let (Some(t), Some(l)) = (&r.user_temp, &r.browser.local_appdata) {
            assert!(PathKey::new(t).is_same_or_under(&PathKey::new(l)));
        }
        // Las cachés de navegador son directorios de caché concretos: nunca el perfil.
        for (_, c) in r.browser_cache_dirs() {
            assert_eq!(p.target_verdict(&PathKey::new(&c)), Verdict::Allowed);
            let s = c.to_string_lossy().to_lowercase();
            assert!(
                !s.ends_with("user data") && !s.ends_with("profiles"),
                "{c:?}"
            );
        }
        // Perfiles de navegador denegados.
        if let Some(l) = &r.browser.local_appdata {
            let ud = l.join(r"Google\Chrome\User Data\Default\History");
            assert_eq!(p.target_verdict(&PathKey::new(&ud)), Verdict::Denied);
        }
    }

    #[test]
    fn solo_se_acepta_un_temp_que_es_localappdata_temp_o_cuelga_de_el() {
        let local = Path::new(r"C:\Users\<usuario>\AppData\Local");
        let ok = |s: &str| accept_user_temp(Path::new(s), Some(local));
        // El sitio por defecto, con otra capitalización, con el prefijo verbatim y un subdirectorio.
        assert!(ok(r"C:\Users\<usuario>\AppData\Local\Temp"));
        assert!(ok(r"c:\users\<USUARIO>\appdata\local\TEMP"));
        assert!(ok(r"\\?\C:\Users\<usuario>\AppData\Local\Temp"));
        assert!(ok(r"C:\Users\<usuario>\AppData\Local\Temp\sub"));
        // TEMP = %LOCALAPPDATA% entero, otras carpetas de Local, Documentos o la raíz de la unidad: la categoría se descarta.
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
