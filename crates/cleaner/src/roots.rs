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
    /// Raíces que `from_system` descartó al arrancar (con su motivo): `category_root_checks` las devuelve como rechazos,
    /// de modo que una categoría que desaparece por un enlace en su ruta no lo hace en silencio (H4).
    pub startup_rejections: Vec<(CategoryId, PathBuf, RootRejection)>,
    /// Qué proteger.
    pub protection: ProtectionRoots,
}

/// Ruta canónica larga de un directorio que existe, o `None`.
fn canon(p: &Path) -> Option<PathBuf> {
    canonical_path(p).ok()
}

/// Por qué se descartó la raíz de una categoría.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootRejection {
    /// Un componente de la cadena (desde la raíz del volumen) es un reparse point: un enlace ahí puede llevar la "raíz" a
    /// una carpeta protegida.
    ReparseInChain { component: String },
    /// La ruta resuelta por el handle no es la que se recorrió (cambió mientras se comprobaba).
    NotCanonical,
    /// La carpeta temporal del usuario está redirigida fuera de `%LOCALAPPDATA%` (no es `Local\Temp` ni cuelga de ella).
    OutsideLocalAppData,
    /// La ruta no es utilizable (no es absoluta con letra de unidad, no existe o no se abre): código de Windows.
    Os(u32),
}

impl RootRejection {
    /// ¿Es algo que el usuario debe saber (la categoría "desaparece" por un motivo de seguridad que puede no esperar)?
    /// Un `Os` (no existe, sin permiso) no lo es: simplemente no hay nada que limpiar.
    pub fn is_user_visible(&self) -> bool {
        !matches!(self, RootRejection::Os(_))
    }
}

/// La raíz de una categoría ya comprobada: su ruta canónica y la ÚNICA zona denegada que puede levantar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRoot {
    /// Ruta canónica larga, resuelta por el handle del último componente.
    pub path: PathBuf,
    /// Identidad (volumen + FileId) del objeto que vio el recorrido de la cadena. La aprobación la compara con la del
    /// handle con el que abre la raíz para listarla: si difieren, algo cambió entre los dos pasos (H1).
    pub identity: FileIdentity,
    /// La zona denegada en cuyo interior vive la raíz (ver [`Exempt`]).
    pub zone: Option<PathBuf>,
}

/// Una raíz candidata de una categoría y el resultado de su comprobación.
#[derive(Clone, Debug)]
pub struct RootCheck {
    /// La ruta tal como se tenía (sin resolver nada).
    pub raw: PathBuf,
    pub result: Result<VerifiedRoot, RootRejection>,
}

/// Capa 1 de B1: acepta `raw` solo si TODA la cadena, desde la raíz del volumen y componente a componente, está libre de
/// reparse points (ver [`sys::open_reparse_free_dir`]) y la ruta que da el handle del último coincide con la canónica.
/// Devuelve la ruta canónica y la identidad del último componente.
pub(crate) fn verify_chain(raw: &Path) -> Result<(PathBuf, FileIdentity), RootRejection> {
    let h = sys::open_reparse_free_dir(raw).map_err(|e| match e {
        sys::ChainError::Reparse { component } => RootRejection::ReparseInChain { component },
        sys::ChainError::BadPath => RootRejection::Os(ERROR_BAD_PATHNAME),
        sys::ChainError::Os(c) => RootRejection::Os(c),
    })?;
    let canonical = sys::final_path(&h).map_err(RootRejection::Os)?;
    let identity = sys::meta(&h).map_err(RootRejection::Os)?.identity;
    // `canonical_path` abre por ruta: si no da el mismo sitio que el handle recorrido, algo cambió entre medias.
    if canon(raw).is_none_or(|c| PathKey::new(&c) != PathKey::new(&canonical)) {
        return Err(RootRejection::NotCanonical);
    }
    Ok((canonical, identity))
}

/// `ERROR_BAD_PATHNAME`.
const ERROR_BAD_PATHNAME: u32 = 161;

/// La carpeta DENEGADA (relativa a `%LOCALAPPDATA%`) en cuyo interior viven las cachés de cada navegador.
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

        // %TEMP%: solo si su cadena (tal como la da Windows, SIN canonicalizar) no tiene reparse points y, ya
        // canonicalizada, es %LOCALAPPDATA%\Temp o cuelga de ella.
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
            // Los perfiles de otros usuarios (incluidos `Default`, `Public`...), no el actual.
            denied.extend(other_profiles(u, profile.as_deref()));
        }
        // Los datos de los navegadores: el perfil NUNCA es caché (las cachés concretas son excepciones, abajo).
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
            // WER: la carpeta entera se deniega; solo sus dos carpetas de informes son excepciones (ver `zone_of`).
            denied.push(l.join(WER_ZONE_REL));
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
            startup_rejections,
            protection: ProtectionRoots {
                denied,
                untouchable,
                content_roots,
                exempt: Vec::new(),
            },
        }
    }

    /// Las cachés de navegador que existen ahora mismo y cuya cadena está libre de reparse points (ver
    /// [`verify_chain`]; solo directorios de caché concretos, ver `tesela_scanner::browser_cache_dirs`), con su categoría
    /// y la ruta CANÓNICA resuelta por handle.
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

    /// Las raíces candidatas de una categoría con acción `Contents`, TAL COMO SE TIENEN (sin resolver), y su
    /// comprobación: cadena sin reparse points desde la raíz del volumen (capa 1 de B1) y la zona denegada que cada una
    /// puede levantar (capa 2). Una raíz rechazada no es una categoría: no se ofrece, no se aprueba y no exime nada.
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
            // Las carpetas de informes de errores y de volcados salen del escáner (una sola definición de las rutas).
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

    /// Las raíces de la categoría que se descartaron y el motivo (solo las que el usuario debe conocer: ver
    /// [`RootRejection::is_user_visible`]).
    pub fn unavailable_roots(&self, id: CategoryId) -> Vec<(PathBuf, RootRejection)> {
        self.category_root_checks(id)
            .into_iter()
            .filter_map(|c| c.result.err().map(|e| (c.raw, e)))
            .filter(|(_, e)| e.is_user_visible())
            .collect()
    }

    /// La ÚNICA denegación que puede levantar la raíz canónica `root` de la categoría `id` (ver [`Exempt`]): la carpeta
    /// denegada en cuyo interior vive. `Windows\Temp` → la carpeta de Windows (su padre); las cachés de navegador → su
    /// `User Data` (Firefox: `Mozilla\Firefox`), calculada con la misma ruta que la denegación; `%TEMP%` → ninguna
    /// (`Local` es intocable, no denegada).
    fn zone_of(&self, id: CategoryId, root: &Path) -> Option<PathBuf> {
        match id {
            CategoryId::WindowsTemp => root.parent().map(Path::to_path_buf),
            CategoryId::ChromeCache | CategoryId::EdgeCache | CategoryId::FirefoxCache => {
                let local = self.browser.local_appdata.as_ref()?;
                browser_zone_rel(id).map(|z| local.join(z))
            }
            // Las carpetas de informes viven dentro de la zona denegada `WER`; solo esa zona se levanta.
            CategoryId::WerReports => self
                .browser
                .local_appdata
                .as_ref()
                .map(|l| l.join(WER_ZONE_REL)),
            // `%LOCALAPPDATA%\CrashDumps` no está dentro de ninguna denegación (como `%TEMP%`: `Local` es intocable, no denegada).
            _ => None,
        }
    }

    /// Las raíces (directorios, canónicas) de una categoría con acción `Contents` que pasaron la comprobación.
    pub fn category_roots(&self, id: CategoryId) -> Vec<PathBuf> {
        self.category_root_checks(id)
            .into_iter()
            .filter_map(|c| c.result.ok().map(|v| v.path))
            .collect()
    }

    /// La protección con las raíces de las categorías comprobadas como excepciones: lo que cuelga de ellas no se deniega,
    /// pero SOLO levantan la zona denegada que cada una declara (nunca "esa zona o cualquiera por encima").
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

    /// La comprobación de la cadena solo pide ATRIBUTOS de cada componente: `Windows\Temp` se acepta aunque un usuario sin
    /// elevar no pueda listarla (antes de esto, pedir `FILE_LIST_DIRECTORY` la descartaba en silencio). Y las raíces del
    /// sistema que existen pasan la cadena en este equipo.
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
