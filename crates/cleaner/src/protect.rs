//! Protection (PLAN.md §2.7): what can be added to the basket and what is preserved during deletion.
//!
//! Four path classes, all represented by [`PathKey`] (long canonical paths):
//! * **Denied** (`denied`): neither these paths nor anything beneath them. `C:\Windows`, `Program Files`, `ProgramData`,
//!   other users' profiles, browser data, the application itself, and a volume's `$` metadata.
//! * **Untouchable as destinations** (`untouchable`): these paths cannot be deleted as such, but their CONTENTS can be
//!   cleaned: the profile root, `AppData`, `Local`, `Roaming`, `LocalLow`, drive roots, and `C:\Users`. This also includes
//!   ANCESTORS of denied paths (deleting one would take the denied path with it).
//! * **Known folder roots** (`content_roots`: Documents, Desktop, Videos...): added as "Contents of X", without including
//!   the folder itself.
//! * **Exemptions** (`exempt`): roots of rule-based categories (`Windows\Temp`, browser caches): descendants are not
//!   denied even when they fall within ONE specific denied zone declared by that exemption ([`Exempt::zone`]). An
//!   exemption never lifts "that zone or any ancestor": only that zone.
//!
//! The actual defense is not the interface: [`Protection::child_action`] is checked for EVERY child during deletion
//! recursion, not only when the basket is approved.

use crate::paths::PathKey;
use std::collections::HashSet;
use std::path::PathBuf;

/// Input paths for protection (all in long canonical form when they exist).
#[derive(Clone, Debug, Default)]
pub struct ProtectionRoots {
    pub denied: Vec<PathBuf>,
    pub untouchable: Vec<PathBuf>,
    pub content_roots: Vec<PathBuf>,
    pub exempt: Vec<Exempt>,
}

/// An exempt category root and the ONLY denial it may lift.
///
/// `zone` is the exact denied path containing the root (Chrome or Edge's `User Data` folder, Firefox's folder, or
/// `C:\Windows` for `Windows\Temp`). Any denial other than that zone (or the root itself) remains in force: if a
/// "canonical" root ends up inside another denied zone (for example, through a link in an ancestor), that zone denies it.
/// `None` means the root is not inside any declared denial (only a denial on the root itself is lifted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exempt {
    pub root: PathBuf,
    pub zone: Option<PathBuf>,
}

impl Exempt {
    pub fn new(root: PathBuf, zone: Option<PathBuf>) -> Exempt {
        Exempt { root, zone }
    }
}

/// What can be done with a path as a TARGET (what the user adds to the basket).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Can be added.
    Allowed,
    /// Cannot be added as such ("View contents" takes its place), but its contents can.
    Untouchable,
    /// Known folder: only "Contents of …".
    ContentsOnly,
    /// Completely denied.
    Denied,
}

/// What to do with a child while traversing a container being deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildAction {
    /// Delete it entirely.
    Delete,
    /// Process its contents but do NOT delete it (it is protected or contains something protected).
    Descend,
    /// Leave it untouched: it is denied.
    Skip,
}

/// Names of volume-root entries that are never touched (in addition to everything starting with `$`).
const SYSTEM_ROOT_NAMES: [&str; 4] = [
    "system volume information",
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
];

/// The protection rules, ready to be queried.
#[derive(Clone, Debug)]
pub struct Protection {
    denied: HashSet<PathKey>,
    untouchable: HashSet<PathKey>,
    content_roots: HashSet<PathKey>,
    /// (exempt root, denied zone it may lift).
    exempt: Vec<(PathKey, Option<PathKey>)>,
    /// Ancestors of denied paths (untouchable as destinations).
    anc_denied: HashSet<PathKey>,
    /// Ancestors of untouchable and content roots (traversed when deleting a container, but not deleted).
    anc_retain: HashSet<PathKey>,
    /// Roots of volumes mounted in FOLDERS (GM-M6): like a drive-letter root, they contain system entries (`$Recycle.Bin`,
    /// `System Volume Information`...) and are themselves left untouched.
    mount_roots: HashSet<PathKey>,
}

fn keys(v: &[PathBuf]) -> HashSet<PathKey> {
    v.iter().map(|p| PathKey::new(p)).collect()
}

fn ancestors_of<'a>(it: impl Iterator<Item = &'a PathKey>) -> HashSet<PathKey> {
    let mut out = HashSet::new();
    for k in it {
        out.extend(k.ancestors());
    }
    out
}

impl Protection {
    pub fn new(roots: &ProtectionRoots) -> Protection {
        let denied = keys(&roots.denied);
        let untouchable = keys(&roots.untouchable);
        let content_roots = keys(&roots.content_roots);
        let exempt: Vec<(PathKey, Option<PathKey>)> = roots
            .exempt
            .iter()
            .map(|e| (PathKey::new(&e.root), e.zone.as_deref().map(PathKey::new)))
            .collect();
        let anc_denied = ancestors_of(denied.iter());
        let mut anc_retain = ancestors_of(untouchable.iter().chain(content_roots.iter()));
        anc_retain.retain(|k| !anc_denied.contains(k));
        Protection {
            denied,
            untouchable,
            content_roots,
            exempt,
            anc_denied,
            anc_retain,
            mount_roots: HashSet::new(),
        }
    }

    /// Adds roots of volumes mounted in FOLDERS (GM-M6): like a drive-letter root, their system entries are denied and
    /// the roots themselves are untouchable.
    pub fn with_mount_roots(mut self, roots: &[PathBuf]) -> Protection {
        self.mount_roots = keys(roots);
        self
    }

    /// Does an exemption lift the denial at `zone` (an ancestor of `key`, or `key` itself)? Only when `key` is beneath
    /// an exempt root `e` and `zone` is EXACTLY the zone declared by that exemption (or `e` itself): a denial between the
    /// exempt root (exclusive) and `key`, or any other zone above `e`, remains in force. This ensures the application's
    /// own folder is never deleted even when it is inside an exempt temporary folder, and a "canonical" root redirected
    /// into another denied zone by a link does not become deletable.
    fn lifts(&self, key: &PathKey, zone: &PathKey) -> bool {
        self.exempt.iter().any(|(root, declared)| {
            key.is_same_or_under(root) && (zone == root || declared.as_ref() == Some(zone))
        })
    }

    /// Is this a system entry at a volume root (`$Recycle.Bin`, `$MFT`, `System Volume Information`, `pagefile.sys`...)?
    fn is_system_entry(&self, key: &PathKey) -> bool {
        key.parent()
            .is_some_and(|p| p.is_drive_root() || self.mount_roots.contains(&p))
            && {
                let n = key.file_name();
                n.starts_with('$') || SYSTEM_ROOT_NAMES.contains(&n)
            }
    }

    /// Is `key` or any of its ancestors denied without an exemption lifting the denial (see [`Protection::lifts`])?
    fn is_denied(&self, key: &PathKey) -> bool {
        // GM-S1: a malformed path (`C:x`, `file:stream`) is not interpreted; it is denied.
        if !key.is_valid() {
            return true;
        }
        let hit = |k: &PathKey| {
            (self.denied.contains(k) || self.is_system_entry(k)) && !self.lifts(key, k)
        };
        hit(key) || key.ancestors().iter().any(hit)
    }

    /// Verdict for `key` as a basket TARGET.
    pub fn target_verdict(&self, key: &PathKey) -> Verdict {
        if self.is_denied(key) {
            return Verdict::Denied;
        }
        if key.is_drive_root()
            || self.mount_roots.contains(key)
            || self.untouchable.contains(key)
            || self.anc_denied.contains(key)
        {
            return Verdict::Untouchable;
        }
        if self.content_roots.contains(key) {
            return Verdict::ContentsOnly;
        }
        Verdict::Allowed
    }

    /// What to do with child `key` while traversing a container being deleted (see [`ChildAction`]).
    pub fn child_action(&self, key: &PathKey) -> ChildAction {
        if self.is_denied(key) {
            return ChildAction::Skip;
        }
        if self.untouchable.contains(key)
            || self.content_roots.contains(key)
            || self.anc_denied.contains(key)
            || self.anc_retain.contains(key)
        {
            return ChildAction::Descend;
        }
        ChildAction::Delete
    }

    /// Is anything protected (denied, untouchable, or a known folder) beneath `key`? Such a container is deleted "except
    /// for protected items".
    pub fn contains_protected(&self, key: &PathKey) -> bool {
        self.anc_denied.contains(key) || self.anc_retain.contains(key)
    }

    /// Is `key` a root that must be preserved as such (untouchable as a destination or a known folder)?
    pub fn is_retained_root(&self, key: &PathKey) -> bool {
        self.untouchable.contains(key) || self.content_roots.contains(key)
    }

    /// Is `key` a known-folder root ("Contents of …")?
    pub fn is_content_root(&self, key: &PathKey) -> bool {
        self.content_roots.contains(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn k(s: &str) -> PathKey {
        PathKey::new(Path::new(s))
    }
    fn pb(v: &[&str]) -> Vec<PathBuf> {
        v.iter().map(PathBuf::from).collect()
    }

    /// A complete synthetic profile.
    fn sample() -> Protection {
        Protection::new(&ProtectionRoots {
            denied: pb(&[
                r"C:\Windows",
                r"C:\Program Files",
                r"C:\Program Files (x86)",
                r"C:\ProgramData",
                r"C:\Users\Public",
                r"C:\Users\Default",
                r"C:\Users\<usuario>2",
                r"C:\Users\<usuario>\AppData\Local\Google\Chrome\User Data",
                r"C:\Users\<usuario>\AppData\Local\Tesela",
            ]),
            untouchable: pb(&[
                r"C:\Users",
                r"C:\Users\<usuario>",
                r"C:\Users\<usuario>\AppData",
                r"C:\Users\<usuario>\AppData\Local",
                r"C:\Users\<usuario>\AppData\Roaming",
                r"C:\Users\<usuario>\AppData\LocalLow",
            ]),
            content_roots: pb(&[
                r"C:\Users\<usuario>\Documents",
                r"C:\Users\<usuario>\Videos",
                r"C:\Users\<usuario>\Downloads",
            ]),
            exempt: vec![
                Exempt::new(
                    PathBuf::from(r"C:\Windows\Temp"),
                    Some(PathBuf::from(r"C:\Windows")),
                ),
                Exempt::new(
                    PathBuf::from(
                        r"C:\Users\<usuario>\AppData\Local\Google\Chrome\User Data\Default\Cache",
                    ),
                    Some(PathBuf::from(
                        r"C:\Users\<usuario>\AppData\Local\Google\Chrome\User Data",
                    )),
                ),
            ],
        })
    }

    #[test]
    fn denegadas_por_completo_con_todo_lo_que_contienen() {
        let p = sample();
        for s in [
            r"C:\Windows",
            r"C:\Windows\System32\kernel32.dll",
            r"C:\Program Files\App\x.exe",
            r"C:\ProgramData\Foo",
            r"C:\Users\<usuario>2\Documents\secreto.txt",
            r"C:\Users\Public\x",
            r"C:\Users\<usuario>\AppData\Local\Google\Chrome\User Data\Default\History",
            r"C:\Users\<usuario>\AppData\Local\Tesela\history.jsonl",
            r"C:\$Recycle.Bin",
            r"C:\$Recycle.Bin\S-1-5-21\$R123",
            r"C:\$MFT",
            r"C:\System Volume Information\x",
            r"C:\pagefile.sys",
            r"C:\hiberfil.sys",
        ] {
            assert_eq!(p.target_verdict(&k(s)), Verdict::Denied, "{s}");
            assert_eq!(p.child_action(&k(s)), ChildAction::Skip, "{s}");
        }
        // Another volume: its root system entries are protected too.
        let d = ["D", r":\$RECYCLE.BIN"].concat();
        assert_eq!(p.target_verdict(&k(&d)), Verdict::Denied);
        // Case, already-canonical 8.3 paths, and the verbatim prefix do not change the result.
        assert_eq!(
            p.target_verdict(&k(r"\\?\c:\WINDOWS\system32")),
            Verdict::Denied
        );
        assert_eq!(p.target_verdict(&k("c:/windows")), Verdict::Denied);
    }

    #[test]
    fn intocables_como_destino_pero_su_contenido_si() {
        let p = sample();
        for s in [
            r"C:\",
            r"C:\Users",
            r"C:\Users\<usuario>",
            r"C:\Users\<usuario>\AppData",
            r"C:\Users\<usuario>\AppData\Local",
            r"C:\Users\<usuario>\AppData\Roaming",
            r"C:\Users\<usuario>\AppData\LocalLow",
            // Ancestors of a denied path.
            r"C:\Users\<usuario>\AppData\Local\Google",
            r"C:\Users\<usuario>\AppData\Local\Google\Chrome",
        ] {
            assert_eq!(p.target_verdict(&k(s)), Verdict::Untouchable, "{s}");
        }
        // Its children (if not denied) can be added and deleted when traversing a container.
        for s in [
            r"C:\Users\<usuario>\AppData\Local\Microsoft",
            r"C:\Users\<usuario>\Pictures",
            r"C:\Users\<usuario>\Desktop",
            r"C:\Temp",
        ] {
            assert_eq!(p.target_verdict(&k(s)), Verdict::Allowed, "{s}");
            assert_eq!(p.child_action(&k(s)), ChildAction::Delete, "{s}");
        }
        // Another volume root is untouchable, but its ordinary contents are not.
        let dd = ["D", r":\"].concat();
        assert_eq!(p.target_verdict(&k(&dd)), Verdict::Untouchable);
        let dx = ["D", r":\datos\algo.bin"].concat();
        assert_eq!(p.target_verdict(&k(&dx)), Verdict::Allowed);
    }

    #[test]
    fn carpetas_conocidas_solo_contenido_de() {
        let p = sample();
        for s in [
            r"C:\Users\<usuario>\Documents",
            r"C:\Users\<usuario>\Videos",
            r"C:\Users\<usuario>\Downloads",
        ] {
            assert_eq!(p.target_verdict(&k(s)), Verdict::ContentsOnly, "{s}");
            // Inside a container being deleted, preserve the folder and process its contents.
            assert_eq!(p.child_action(&k(s)), ChildAction::Descend, "{s}");
        }
        assert_eq!(
            p.target_verdict(&k(r"C:\Users\<usuario>\Documents\carta.docx")),
            Verdict::Allowed
        );
        assert!(p.is_content_root(&k(r"C:\Users\<usuario>\Videos")));
        assert!(!p.is_content_root(&k(r"C:\Users\<usuario>\Videos\viaje")));
    }

    #[test]
    fn las_excepciones_de_categoria_levantan_la_denegacion_de_lo_que_cuelga_de_ellas() {
        let p = sample();
        for s in [
            r"C:\Windows\Temp",
            r"C:\Windows\Temp\x.tmp",
            r"C:\Users\<usuario>\AppData\Local\Google\Chrome\User Data\Default\Cache",
            r"C:\Users\<usuario>\AppData\Local\Google\Chrome\User Data\Default\Cache\f_000001",
        ] {
            assert_eq!(p.target_verdict(&k(s)), Verdict::Allowed, "{s}");
            assert_eq!(p.child_action(&k(s)), ChildAction::Delete, "{s}");
        }
        // The rest of the zone remains denied: siblings and ancestors of the exception.
        for s in [
            r"C:\Windows\System32",
            r"C:\Windows\Temp2",
            r"C:\Users\<usuario>\AppData\Local\Google\Chrome\User Data\Default\Cookies",
            r"C:\Users\<usuario>\AppData\Local\Google\Chrome\User Data\Default",
        ] {
            assert_eq!(p.target_verdict(&k(s)), Verdict::Denied, "{s}");
        }
    }

    #[test]
    fn una_excepcion_no_levanta_una_denegacion_que_esta_dentro_de_ella() {
        // The user temp directory is an exempt root, and the application folder is INSIDE it: the exemption
        // lifts protection above the root, not the denial inside it.
        let p = Protection::new(&ProtectionRoots {
            denied: pb(&[
                r"C:\Users\<usuario>\AppData\Local\Temp\app",
                r"C:\Users\<usuario>\AppData\Local\Tesela",
            ]),
            untouchable: pb(&[r"C:\Users\<usuario>\AppData\Local"]),
            content_roots: Vec::new(),
            exempt: vec![Exempt::new(
                PathBuf::from(r"C:\Users\<usuario>\AppData\Local\Temp"),
                None,
            )],
        });
        let t = r"C:\Users\<usuario>\AppData\Local\Temp";
        for s in [
            format!(r"{t}\app"),
            format!(r"{t}\app\tesela.exe"),
            format!(r"{t}\app\datos\x.bin"),
        ] {
            assert_eq!(p.target_verdict(&k(&s)), Verdict::Denied, "{s}");
            assert_eq!(p.child_action(&k(&s)), ChildAction::Skip, "{s}");
        }
        // Other temp contents remain cleanable, but the temp root itself is not a deletion target (it contains the app).
        for s in [
            format!(r"{t}\x.tmp"),
            format!(r"{t}\apps"),
            format!(r"{t}\otra\x"),
        ] {
            assert_eq!(p.target_verdict(&k(&s)), Verdict::Allowed, "{s}");
            assert_eq!(p.child_action(&k(&s)), ChildAction::Delete, "{s}");
        }
        assert_eq!(p.target_verdict(&k(t)), Verdict::Untouchable);
        assert_eq!(p.child_action(&k(t)), ChildAction::Descend);
        // A denial EXACTLY on the exempt root is lifted, as is its declared zone (`Windows`).
        let w = Protection::new(&ProtectionRoots {
            denied: pb(&[r"C:\Windows", r"C:\Windows\Temp"]),
            exempt: vec![Exempt::new(
                PathBuf::from(r"C:\Windows\Temp"),
                Some(PathBuf::from(r"C:\Windows")),
            )],
            ..ProtectionRoots::default()
        });
        assert_eq!(w.target_verdict(&k(r"C:\Windows\Temp\x")), Verdict::Allowed);
        assert_eq!(
            w.target_verdict(&k(r"C:\Windows\System32")),
            Verdict::Denied
        );
    }

    #[test]
    fn una_excepcion_solo_levanta_su_zona_declarada_no_cualquiera_por_encima() {
        // B1: the exempt root declares its zone (`C:\Windows`). A "canonical" root redirected by a link into ANOTHER denied zone
        // (`C:\Datos\Privado`) is not exempt: that denial was not declared, even if it is above the root.
        let p = Protection::new(&ProtectionRoots {
            denied: pb(&[r"C:\Windows", r"C:\Datos\Privado"]),
            exempt: vec![
                Exempt::new(
                    PathBuf::from(r"C:\Windows\Temp"),
                    Some(PathBuf::from(r"C:\Windows")),
                ),
                Exempt::new(
                    PathBuf::from(r"C:\Datos\Privado\ShaderCache"),
                    Some(PathBuf::from(
                        r"C:\Users\<usuario>\AppData\Local\Google\Chrome\User Data",
                    )),
                ),
            ],
            ..ProtectionRoots::default()
        });
        // The declared zone is exempt.
        assert_eq!(p.target_verdict(&k(r"C:\Windows\Temp\x")), Verdict::Allowed);
        // The root that falls under another denied zone is not exempt; its contents remain denied.
        for s in [
            r"C:\Datos\Privado\ShaderCache",
            r"C:\Datos\Privado\ShaderCache\x.bin",
        ] {
            assert_eq!(p.target_verdict(&k(s)), Verdict::Denied, "{s}");
            assert_eq!(p.child_action(&k(s)), ChildAction::Skip, "{s}");
        }
        // Declaring another zone does not lift the `Windows` denial.
        let q = Protection::new(&ProtectionRoots {
            denied: pb(&[r"C:\Windows"]),
            exempt: vec![Exempt::new(
                PathBuf::from(r"C:\Windows\Temp"),
                Some(PathBuf::from(r"C:\Otra")),
            )],
            ..ProtectionRoots::default()
        });
        assert_eq!(q.target_verdict(&k(r"C:\Windows\Temp\x")), Verdict::Denied);
    }

    #[test]
    fn un_contenedor_que_contiene_algo_protegido_se_recorre_sin_borrarlo() {
        let p = sample();
        // The user folder contains Documents (a known root), AppData (untouchable), and much more.
        assert!(p.contains_protected(&k(r"C:\Users\<usuario>")));
        assert!(p.contains_protected(&k(r"C:\Users\<usuario>\AppData\Local\Google")));
        assert!(!p.contains_protected(&k(r"C:\Users\<usuario>\Pictures")));
        assert!(!p.contains_protected(&k(r"C:\Users\<usuario>\Documents")));
        // An ordinary container with a known root inside (Documents redirected under OneDrive).
        let p2 = Protection::new(&ProtectionRoots {
            content_roots: pb(&[r"C:\Users\<usuario>\OneDrive\Documentos"]),
            ..ProtectionRoots::default()
        });
        assert!(p2.contains_protected(&k(r"C:\Users\<usuario>\OneDrive")));
        assert_eq!(
            p2.child_action(&k(r"C:\Users\<usuario>\OneDrive")),
            ChildAction::Descend
        );
        assert_eq!(
            p2.child_action(&k(r"C:\Users\<usuario>\OneDrive\Documentos")),
            ChildAction::Descend
        );
        assert_eq!(
            p2.child_action(&k(r"C:\Users\<usuario>\OneDrive\Fotos")),
            ChildAction::Delete
        );
        assert_eq!(
            p2.target_verdict(&k(r"C:\Users\<usuario>\OneDrive")),
            Verdict::Allowed
        );
    }

    #[test]
    fn los_prefijos_de_nombre_no_confunden_las_zonas() {
        let p = sample();
        assert_eq!(
            p.target_verdict(&k(r"C:\WindowsApps")),
            Verdict::Allowed,
            "no es C:\\Windows"
        );
        assert_eq!(
            p.target_verdict(&k(r"C:\Users\<usuario>b")),
            Verdict::Allowed,
            "no es el perfil de ana"
        );
        assert_eq!(
            p.target_verdict(&k(r"C:\Program Files Extra")),
            Verdict::Allowed
        );
        // An ordinary entry at the root of a non-system volume.
        assert_eq!(
            p.target_verdict(&k(r"C:\Temp\pagefile.sys")),
            Verdict::Allowed,
            "solo en la raíz del volumen"
        );
        assert_eq!(p.target_verdict(&k(r"C:\datos")), Verdict::Allowed);
    }

    /// GM-M6: a volume mounted in a FOLDER has its own volume root: `$Recycle.Bin`, `System Volume Information`,
    /// `$MFT`, and `pagefile.sys` under IT are system entries (like those under `C:\`), and the mount root itself is
    /// untouchable. Without mount information, these were not recognized (only drive-letter roots counted). Pure path-based test.
    #[test]
    fn un_volumen_montado_en_una_carpeta_tiene_sus_propias_entradas_de_sistema() {
        let mount = PathBuf::from(r"C:\montajes\datos");
        let con = Protection::new(&ProtectionRoots::default()).with_mount_roots(&[mount]);
        let sin = Protection::new(&ProtectionRoots::default());
        for rel in [
            r"$Recycle.Bin",
            r"$Recycle.Bin\<sid>\$R123.txt",
            r"System Volume Information",
            r"System Volume Information\algo",
            r"$MFT",
            r"pagefile.sys",
        ] {
            let p = k(&format!(r"C:\montajes\datos\{rel}"));
            assert_eq!(con.target_verdict(&p), Verdict::Denied, "{rel}");
            assert_eq!(con.child_action(&p), ChildAction::Skip, "{rel}");
            // Before mount information was available, this could be added to the basket.
            assert_eq!(sin.target_verdict(&p), Verdict::Allowed, "{rel}");
        }
        // The mount root is untouchable like a drive root; ordinary contents are not.
        assert_eq!(
            con.target_verdict(&k(r"C:\montajes\datos")),
            Verdict::Untouchable
        );
        assert_eq!(
            con.target_verdict(&k(r"C:\montajes\datos\mis cosas\a.txt")),
            Verdict::Allowed
        );
        // Only at the volume root: `pagefile.sys` inside a mount subfolder is an ordinary file.
        assert_eq!(
            con.target_verdict(&k(r"C:\montajes\datos\sub\pagefile.sys")),
            Verdict::Allowed
        );
        // Paths outside the mount are unaffected.
        assert_eq!(
            con.target_verdict(&k(r"C:\montajes\otro\$Recycle.Bin")),
            Verdict::Allowed
        );
        // Drive-letter roots behave as before.
        assert_eq!(con.target_verdict(&k(r"C:\$Recycle.Bin")), Verdict::Denied);
    }
}
