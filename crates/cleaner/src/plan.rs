//! La aprobación (PLAN.md §3.7.1): de la cesta a un [`Plan`] con IDENTIDADES.
//!
//! Al confirmar, el `cleaner` reabre cada elemento por handle (`FILE_FLAG_OPEN_REPARSE_POINT | BACKUP_SEMANTICS`) y guarda
//! `(número de serie del volumen, FileId de 128 bits)` y `NumberOfLinks`. Las rutas solo sirven para volver a encontrar
//! los objetos: al ejecutar, lo que se abra tiene que tener ESA identidad.
//!
//! La aprobación además:
//! * re-valida la protección sobre la ruta resuelta por el handle (no sobre la cadena que llegó);
//! * expande una carpeta que contiene algo protegido en sus partes permitidas ("se omitirán N elementos protegidos");
//! * mide cada elemento con el mismo recorrido que borrará (modo `Measure`): tamaño en disco SIN contar los enlaces
//!   duros compartidos, tamaño lógico, archivos, entradas protegidas / recientes / enlaces que se respetarán;
//! * decide si cada elemento con destino Papelera CABE en ella ([`Fit`]), con el límite real del volumen;
//! * captura la lista de la Papelera si se pidió vaciarla (alcance enumerado).

use crate::basket::{Basket, Source};
use crate::engine::{self, Env, Mode, StatsSnapshot, Unit, UnitSpec};
use crate::paths::PathKey;
use crate::protect::{ChildAction, Protection, Verdict};
use crate::recycle::BinItem;
use crate::roots::Roots;
use crate::rules::{Action, CategoryId, Destination, Risk, Rules, rules};
use crate::sys;
use crate::volume::{self, BinLimit};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::time::Duration;

/// Por qué un elemento NO cabe en la Papelera (se mostraría en el grupo ámbar de la confirmación).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnfitReason {
    /// La unidad no tiene Papelera (red, extraíble).
    NoBin,
    /// El usuario desactivó la Papelera de ese volumen ("no mover los archivos a la Papelera").
    NukeOnDelete,
    /// Supera (con margen) el tamaño máximo de la Papelera del volumen.
    TooBig,
    /// La ruta (con el nombre de lápida) no cabe en los 260 caracteres que admite la Papelera.
    PathTooLong,
}

/// Si un elemento con destino Papelera cabe en ella.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    Fits,
    Unfit(UnfitReason),
}

/// Por qué una entrada de la cesta no llegó al plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// Ya no existe.
    Missing,
    /// Windows no deja abrirla (permisos).
    AccessDenied,
    /// El sistema de archivos no da identidad (FAT, exFAT...): no se aprueba lo que no se puede verificar.
    NoIdentity,
    /// Es un enlace (junction, symlink, punto de montaje): no se sigue ni se borra.
    Reparse,
    /// Protegida: denegada, intocable o carpeta conocida.
    Protected(Verdict),
    /// Cualquier otro error del sistema.
    Other(u32),
}

/// Una entrada de la cesta que no pasó la aprobación.
#[derive(Clone, Debug)]
pub struct Rejected {
    pub path: PathBuf,
    pub reason: RejectReason,
}

/// Un elemento aprobado.
#[derive(Clone, Debug)]
pub struct PlanItem {
    pub id: usize,
    /// La categoría con regla de la que sale (`None` = "Tu archivo").
    pub category: Option<CategoryId>,
    /// La ruta aprobada (la raíz de la categoría, o el elemento).
    pub path: PathBuf,
    pub risk: Risk,
    /// Destino aprobado.
    pub destination: Destination,
    /// Si cabe en la Papelera (solo con destino `Recycle`; con `Permanent` es siempre `Fits`).
    pub fit: Fit,
    /// Qué y con qué identidad se aprobó.
    pub spec: UnitSpec,
    /// Antigüedad mínima de la regla.
    pub min_age: Duration,
    /// Medida al aprobar.
    pub estimate: StatsSnapshot,
    /// Es un archivo sincronizado con la nube (o lo contiene): borrarlo también lo quita de la nube.
    pub cloud_synced: bool,
    /// Es un archivo con más de un enlace: no liberará espacio.
    pub hard_linked: bool,
    /// Volumen (raíz) en el que está.
    pub volume: Option<PathBuf>,
}

impl PlanItem {
    pub fn is_dir(&self) -> bool {
        match &self.spec {
            UnitSpec::Contents { .. } => true,
            UnitSpec::Entry { is_dir, .. } => *is_dir,
        }
    }
    /// ¿Va a la Papelera pero no cabe? (El grupo ámbar de la confirmación.)
    pub fn is_unfit(&self) -> bool {
        self.destination == Destination::Recycle && matches!(self.fit, Fit::Unfit(_))
    }
    /// Bytes que liberará (sin los enlaces duros compartidos).
    pub fn bytes(&self) -> u64 {
        self.estimate.bytes
    }
}

/// La Papelera capturada al confirmar (alcance enumerado).
#[derive(Clone, Debug)]
pub struct BinPlan {
    /// La lista de elementos, tal como estaba al confirmar. Solo se purga ESTA lista.
    pub items: Vec<BinItem>,
    /// Tamaño aproximado de la Papelera (de lo que sabía quien llamó; 0 si no se sabe).
    pub bytes_hint: u64,
}

/// Lo aprobado.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub items: Vec<PlanItem>,
    pub bin: Option<BinPlan>,
    pub rejected: Vec<Rejected>,
    /// Hijos de carpetas expandidas que se omiten por estar protegidos: (entradas, bytes del listado).
    pub omitted_protected: (u64, u64),
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.bin.as_ref().is_none_or(|b| b.items.is_empty())
    }

    /// Elementos que se borran definitivamente (regla), sin los que van a la Papelera.
    pub fn permanent_items(&self) -> impl Iterator<Item = &PlanItem> {
        self.items
            .iter()
            .filter(|i| i.destination == Destination::Permanent)
    }

    /// Elementos que van a la Papelera y caben.
    pub fn recycle_items(&self) -> impl Iterator<Item = &PlanItem> {
        self.items
            .iter()
            .filter(|i| i.destination == Destination::Recycle && !i.is_unfit())
    }

    /// Elementos que irían a la Papelera pero no caben (grupo ámbar: "se eliminará definitivamente").
    pub fn unfit_items(&self) -> impl Iterator<Item = &PlanItem> {
        self.items.iter().filter(|i| i.is_unfit())
    }

    pub fn permanent_bytes(&self) -> u64 {
        self.permanent_items().map(PlanItem::bytes).sum()
    }
    pub fn recycle_bytes(&self) -> u64 {
        self.recycle_items().map(PlanItem::bytes).sum()
    }
    pub fn unfit_bytes(&self) -> u64 {
        self.unfit_items().map(PlanItem::bytes).sum()
    }

    /// Los `n` elementos más grandes de "Tu archivo" (sin categoría), de mayor a menor.
    pub fn biggest_user_items(&self, n: usize) -> Vec<&PlanItem> {
        let mut v: Vec<&PlanItem> = self.items.iter().filter(|i| i.category.is_none()).collect();
        v.sort_by(|a, b| b.bytes().cmp(&a.bytes()).then_with(|| a.path.cmp(&b.path)));
        v.truncate(n);
        v
    }

    /// Entradas protegidas que se respetarán, dentro de los elementos medidos y al expandir carpetas: (entradas, bytes).
    pub fn protected_total(&self) -> (u64, u64) {
        let mut n = self.omitted_protected.0;
        let mut b = self.omitted_protected.1;
        for i in &self.items {
            n += i.estimate.protected;
            b += i.estimate.protected_bytes;
        }
        (n, b)
    }

    /// Bytes en archivos con más de un enlace (no liberarán espacio) y cuántos son.
    pub fn shared_total(&self) -> (u64, u64) {
        self.items.iter().fold((0, 0), |(n, b), i| {
            (n + i.estimate.shared_files, b + i.estimate.shared_bytes)
        })
    }
}

/// Opciones de la aprobación.
#[derive(Clone, Debug, Default)]
pub struct ApproveOptions {
    /// Tamaño aproximado de la Papelera, si se pidió vaciarla (por ejemplo, el de su carpeta en el árbol escaneado).
    pub bin_bytes_hint: u64,
}

/// El `cleaner`: reglas, raíces y protección, listos para aprobar y ejecutar.
pub struct Cleaner {
    pub(crate) rules: &'static Rules,
    pub(crate) roots: Roots,
    pub(crate) protection: Arc<Protection>,
    pub(crate) threads: usize,
    /// Alcance de "vaciar la Papelera": por defecto TODA la del usuario; se puede restringir (las pruebas lo limitan a
    /// elementos propios para no tocar nunca los ajenos).
    pub(crate) bin_filter: Option<BinFilter>,
    /// `%LOCALAPPDATA%\Tesela`: registro local e historial.
    pub(crate) data_dir: Option<PathBuf>,
}

/// Filtro del alcance de "vaciar la Papelera".
pub type BinFilter = Arc<dyn Fn(&BinItem) -> bool + Send + Sync>;

/// Hilos del pool de borrado por defecto: `min(8, 2 × núcleos)` (el borrado es sobre todo E/S del sistema de archivos).
pub fn default_threads() -> usize {
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
    (2 * cores).clamp(2, 8)
}

/// Margen de seguridad frente al tamaño máximo de la Papelera: por encima del 90 % no se considera que quepa (la shell
/// compara con su propio criterio y, si no cabe, elimina SIN avisar: ver las pruebas de `recycle`).
const BIN_MARGIN_PERCENT: u64 = 90;

/// Caracteres que añade el nombre de lápida (`.tesela-` + 12 hex) al nombre original.
pub const TOMBSTONE_EXTRA: usize = 20;
/// La Papelera no admite rutas de 260 caracteres o más.
const MAX_BIN_PATH: usize = 258;

impl Cleaner {
    pub fn new(roots: Roots) -> Cleaner {
        let protection = Arc::new(roots.protection());
        let data_dir = roots
            .browser
            .local_appdata
            .as_ref()
            .map(|l| l.join("Tesela"));
        Cleaner {
            rules: rules(),
            roots,
            protection,
            threads: default_threads(),
            bin_filter: None,
            data_dir,
        }
    }

    /// Cambia la carpeta del registro y del historial (las pruebas usan la suya).
    pub fn with_data_dir(mut self, dir: PathBuf) -> Cleaner {
        self.data_dir = Some(dir);
        self
    }

    /// Restringe el alcance de "vaciar la Papelera" a los elementos que cumplan el filtro.
    pub fn with_bin_filter(mut self, f: BinFilter) -> Cleaner {
        self.bin_filter = Some(f);
        self
    }

    pub fn with_threads(mut self, n: usize) -> Cleaner {
        self.threads = n.clamp(1, 32);
        self
    }

    pub fn roots(&self) -> &Roots {
        &self.roots
    }

    pub fn protection(&self) -> &Arc<Protection> {
        &self.protection
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    pub(crate) fn env(
        &self,
        mode: Mode,
        min_age: Duration,
        cancel: &Arc<AtomicBool>,
        progress: &Arc<AtomicU64>,
        sink: Option<Arc<engine::EventSink>>,
    ) -> Arc<Env> {
        Arc::new(Env {
            protection: self.protection.clone(),
            mode,
            min_age_ft: engine::duration_to_ft(min_age),
            now_ft: engine::now_filetime(),
            cancel: cancel.clone(),
            sink,
            progress: progress.clone(),
        })
    }

    /// ¿Cabe `bytes` (el mayor entre el tamaño en disco y el lógico) de un elemento cuya ruta es `path` en la Papelera
    /// de su volumen? Se calcula AL APROBAR y se vuelve a calcular AL EJECUTAR.
    pub fn fit(&self, path: &Path, bytes: u64) -> Fit {
        fit_for(path, bytes)
    }

    /// Aprueba la cesta: captura identidades, re-valida la protección, mide y decide el encaje en la Papelera.
    pub fn approve(
        &self,
        basket: &Basket,
        opts: &ApproveOptions,
        cancel: &Arc<AtomicBool>,
    ) -> Plan {
        let mut cands: Vec<Candidate> = Vec::new();
        let mut plan = Plan::default();
        for src in basket.sources() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            match src {
                Source::Category(id) => self.candidates_of_category(*id, &mut cands, &mut plan),
                Source::Path(p) => self.candidate_entry(p, &mut cands, &mut plan),
                Source::ContentsOf(p) => self.candidates_of_contents(p, &mut cands, &mut plan),
                Source::EmptyBin => {
                    if let Ok(mut items) = crate::recycle::list_bin() {
                        if let Some(f) = &self.bin_filter {
                            items.retain(|i| f(i));
                        }
                        let hint = if opts.bin_bytes_hint > 0 {
                            opts.bin_bytes_hint
                        } else {
                            crate::recycle::bin_totals().map_or(0, |t| t.1)
                        };
                        plan.bin = Some(BinPlan {
                            items,
                            bytes_hint: hint,
                        });
                    }
                }
            }
        }
        // Medir con el mismo recorrido que borrará (aperturas compartidas, nada se toca).
        let progress = Arc::new(AtomicU64::new(0));
        let units: Vec<Arc<Unit>> = cands
            .iter()
            .enumerate()
            .map(|(i, c)| {
                Unit::new(
                    i,
                    c.spec.clone(),
                    self.env(Mode::Measure, c.min_age, cancel, &progress, None),
                )
            })
            .collect();
        engine::run(&units, self.threads);
        for (c, u) in cands.into_iter().zip(units) {
            let est = u.stats.snapshot();
            let id = plan.items.len();
            let dest = c.destination;
            let size = est.bytes.max(est.logical).max(est.shared_bytes);
            let fit = if dest == Destination::Recycle {
                fit_for(&c.canon, size)
            } else {
                Fit::Fits
            };
            plan.items.push(PlanItem {
                id,
                category: c.category,
                path: c.canon,
                risk: c.risk,
                destination: dest,
                fit,
                spec: c.spec,
                min_age: c.min_age,
                estimate: est,
                cloud_synced: c.cloud || est.cloud > 0,
                hard_linked: c.links > 1,
                volume: c.volume,
            });
        }
        plan
    }

    fn candidates_of_category(&self, id: CategoryId, out: &mut Vec<Candidate>, plan: &mut Plan) {
        let rule = self.rules.get(id);
        if rule.action != Action::Contents {
            return;
        }
        let Some(dest) = rule.destination else { return };
        for root in self.roots.category_roots(id) {
            let h = match sys::open_dir_for_listing(&root) {
                Ok(h) => h,
                Err(c) => {
                    plan.rejected.push(Rejected {
                        path: root,
                        reason: reason_of(c),
                    });
                    continue;
                }
            };
            let Some((m, canon, key)) = self.verify(&h, &root, plan) else {
                continue;
            };
            // La raíz de una categoría solo vale si es un directorio de verdad y NO está denegada (los ancestros de lo
            // protegido y las carpetas conocidas valen: se limpia su contenido, no ella).
            if !m.is_dir
                || m.is_reparse()
                || self.protection.target_verdict(&key) == Verdict::Denied
            {
                plan.rejected.push(Rejected {
                    path: canon,
                    reason: if m.is_reparse() {
                        RejectReason::Reparse
                    } else {
                        RejectReason::Protected(Verdict::Denied)
                    },
                });
                continue;
            }
            out.push(Candidate {
                category: Some(id),
                risk: rule.risk,
                destination: dest,
                spec: UnitSpec::Contents {
                    path: canon.clone(),
                    approved: m.identity,
                },
                min_age: rule.min_age,
                volume: volume::volume_root(&canon),
                canon,
                links: 1,
                cloud: false,
            });
        }
    }

    /// Abre `h`, lee su identidad y ruta resuelta. Registra el rechazo y devuelve `None` si no se puede.
    fn verify(
        &self,
        h: &sys::Handle,
        path: &Path,
        plan: &mut Plan,
    ) -> Option<(sys::Meta, PathBuf, PathKey)> {
        let m = match sys::meta(h) {
            Ok(m) => m,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: path.to_path_buf(),
                    reason: match c {
                        1 | 50 | 87 => RejectReason::NoIdentity,
                        other => reason_of(other),
                    },
                });
                return None;
            }
        };
        let canon = match sys::final_path(h) {
            Ok(p) => p,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: path.to_path_buf(),
                    reason: reason_of(c),
                });
                return None;
            }
        };
        let key = PathKey::new(&canon);
        Some((m, canon, key))
    }

    fn candidates_of_contents(&self, folder: &Path, out: &mut Vec<Candidate>, plan: &mut Plan) {
        let h = match sys::open_dir_for_listing(folder) {
            Ok(h) => h,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: folder.to_path_buf(),
                    reason: reason_of(c),
                });
                return;
            }
        };
        let Some((m, canon, key)) = self.verify(&h, folder, plan) else {
            return;
        };
        let verdict = self.protection.target_verdict(&key);
        if !m.is_dir || m.is_reparse() || verdict != Verdict::ContentsOnly {
            plan.rejected.push(Rejected {
                path: canon,
                reason: if m.is_reparse() {
                    RejectReason::Reparse
                } else {
                    RejectReason::Protected(verdict)
                },
            });
            return;
        }
        let entries = match sys::list_dir(&h) {
            Ok(e) => e,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: canon,
                    reason: reason_of(c),
                });
                return;
            }
        };
        for e in entries {
            let child = canon.join(OsString::from_wide(&e.name));
            if e.is_reparse() && !(sys::is_cloud_tag(e.reparse_tag) && !e.is_dir()) {
                plan.rejected.push(Rejected {
                    path: child,
                    reason: RejectReason::Reparse,
                });
                continue;
            }
            self.candidate_entry(&child, out, plan);
        }
    }

    /// Un archivo o una carpeta del usuario ("Tu archivo"): captura su identidad y, si la carpeta contiene algo
    /// protegido, la expande en sus partes permitidas.
    fn candidate_entry(&self, path: &Path, out: &mut Vec<Candidate>, plan: &mut Plan) {
        let h = match sys::open_for_query(path) {
            Ok(h) => h,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: path.to_path_buf(),
                    reason: reason_of(c),
                });
                return;
            }
        };
        let Some((m, canon, key)) = self.verify(&h, path, plan) else {
            return;
        };
        let cloud_file = !m.is_dir && m.is_reparse() && sys::is_cloud_tag(m.reparse_tag);
        if m.is_reparse() && !cloud_file {
            plan.rejected.push(Rejected {
                path: canon,
                reason: RejectReason::Reparse,
            });
            return;
        }
        let verdict = self.protection.target_verdict(&key);
        if verdict != Verdict::Allowed {
            plan.rejected.push(Rejected {
                path: canon,
                reason: RejectReason::Protected(verdict),
            });
            return;
        }
        if m.is_dir && self.protection.contains_protected(&key) {
            // Para listar hace falta otro handle (el de consulta no puede listar); se comprueba que es el mismo objeto.
            match sys::open_dir_for_listing(&canon) {
                Ok(lh) if sys::meta(&lh).is_ok_and(|x| x.identity == m.identity) => {
                    self.expand_dir(&lh, &canon, &key, m.identity.volume_serial, out, plan);
                }
                Ok(_) => plan.rejected.push(Rejected {
                    path: canon,
                    reason: RejectReason::Other(0),
                }),
                Err(c) => plan.rejected.push(Rejected {
                    path: canon,
                    reason: reason_of(c),
                }),
            }
            return;
        }
        out.push(Candidate {
            category: None,
            risk: Risk::Review,
            destination: Destination::Recycle,
            spec: UnitSpec::Entry {
                path: canon.clone(),
                approved: m.identity,
                approved_tag: if cloud_file { m.reparse_tag } else { 0 },
                is_dir: m.is_dir,
            },
            min_age: Duration::ZERO,
            volume: volume::volume_root(&canon),
            canon,
            links: m.links,
            cloud: cloud_file,
        });
    }

    /// Una carpeta que contiene algo protegido: sus hijos permitidos pasan a ser elementos y lo protegido se omite.
    fn expand_dir(
        &self,
        dir: &sys::Handle,
        dir_path: &Path,
        dir_key: &PathKey,
        volume_serial: u64,
        out: &mut Vec<Candidate>,
        plan: &mut Plan,
    ) {
        let entries = match sys::list_dir(dir) {
            Ok(e) => e,
            Err(c) => {
                plan.rejected.push(Rejected {
                    path: dir_path.to_path_buf(),
                    reason: reason_of(c),
                });
                return;
            }
        };
        for e in entries {
            let ckey = dir_key.join(&String::from_utf16_lossy(&e.name));
            let child = dir_path.join(OsString::from_wide(&e.name));
            match self.protection.child_action(&ckey) {
                ChildAction::Skip => {
                    plan.omitted_protected.0 += 1;
                    plan.omitted_protected.1 += e.alloc;
                }
                ChildAction::Descend if e.is_dir() && !e.is_reparse() => {
                    // Una carpeta conocida o intocable DENTRO se respeta (ella, no su contenido): cuenta como protegida.
                    if self.protection.is_retained_root(&ckey) {
                        plan.omitted_protected.0 += 1;
                    }
                    // Se recorre por handle relativo y se verifica que es el mismo objeto que el listado.
                    match sys::open_child_dir_for_listing(dir, &e.name) {
                        Ok(ch) => {
                            let same = sys::meta(&ch).is_ok_and(|m| {
                                m.identity.file_id == e.file_id
                                    && m.identity.volume_serial == volume_serial
                                    && !m.is_reparse()
                            });
                            if same {
                                self.expand_dir(&ch, &child, &ckey, volume_serial, out, plan);
                            } else {
                                plan.rejected.push(Rejected {
                                    path: child,
                                    reason: RejectReason::Reparse,
                                });
                            }
                        }
                        Err(c) => plan.rejected.push(Rejected {
                            path: child,
                            reason: reason_of(c),
                        }),
                    }
                }
                ChildAction::Descend => {
                    plan.omitted_protected.0 += 1;
                    plan.omitted_protected.1 += e.alloc;
                }
                ChildAction::Delete => {
                    if e.is_reparse() && !(sys::is_cloud_tag(e.reparse_tag) && !e.is_dir()) {
                        plan.rejected.push(Rejected {
                            path: child,
                            reason: RejectReason::Reparse,
                        });
                    } else {
                        self.candidate_entry(&child, out, plan);
                    }
                }
            }
        }
    }
}

struct Candidate {
    category: Option<CategoryId>,
    risk: Risk,
    destination: Destination,
    spec: UnitSpec,
    min_age: Duration,
    canon: PathBuf,
    volume: Option<PathBuf>,
    links: u32,
    cloud: bool,
}

fn reason_of(code: u32) -> RejectReason {
    match code {
        2 | 3 => RejectReason::Missing,
        5 => RejectReason::AccessDenied,
        other => RejectReason::Other(other),
    }
}

/// ¿Cabe un elemento de `bytes` con ruta `path` en la Papelera de su volumen?
pub(crate) fn fit_for(path: &Path, bytes: u64) -> Fit {
    // La Papelera no admite rutas de 260 caracteres o más (y la lápida alarga el nombre).
    if path.as_os_str().len() + TOMBSTONE_EXTRA > MAX_BIN_PATH {
        return Fit::Unfit(UnfitReason::PathTooLong);
    }
    let Some(root) = volume::volume_root(path) else {
        return Fit::Fits;
    };
    match volume::bin_limit(&root) {
        BinLimit::NoBin => Fit::Unfit(UnfitReason::NoBin),
        BinLimit::NukeOnDelete => Fit::Unfit(UnfitReason::NukeOnDelete),
        BinLimit::Max(max) => {
            if (bytes as u128) * 100 > (max as u128) * BIN_MARGIN_PERCENT as u128 {
                Fit::Unfit(UnfitReason::TooBig)
            } else {
                Fit::Fits
            }
        }
        BinLimit::Unknown => {
            // Sin valor en el registro la shell usa un porcentaje por defecto que Microsoft no documenta: SE ASUME el 10 % del
            // volumen (los `MaxCapacity` que se vieron en los volúmenes de medición eran ≈ el 10 %). Esta rama NO se ha
            // probado contra la shell real (todos esos volúmenes tenían el valor en el registro).
            match tesela_scanner::volume_space(&root) {
                Ok(v) if v.volume_scope && v.total > 0 => {
                    let max = v.total / 10;
                    if (bytes as u128) * 100 > (max as u128) * BIN_MARGIN_PERCENT as u128 {
                        Fit::Unfit(UnfitReason::TooBig)
                    } else {
                        Fit::Fits
                    }
                }
                _ => Fit::Fits,
            }
        }
    }
}
