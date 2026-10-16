//! La ejecución del plan (PLAN.md §2.5 y §3.7.2 a §3.7.9).
//!
//! Orden: primero se vacía la Papelera (con la lista capturada al confirmar) y después se borra lo definitivo y se mueve
//! lo que va a la Papelera: así lo que el usuario quiere conservar recuperable no se borra en el mismo paso.
//!
//! * **Definitivo**: el recorrido por handle de `engine` (apertura exclusiva, identidad, `FileDispositionInfoEx`).
//! * **Papelera**: se abre el elemento por handle, se verifica su identidad y su ruta resuelta, se RENOMBRA por ese handle a
//!   una lápida aleatoria en el mismo directorio (falla en seguro si hay algo abierto dentro) y se entrega la ruta lápida a
//!   `IFileOperation`. Después se VERIFICA cada elemento: sigue ahí (falló, se le devuelve su nombre), está en la Papelera
//!   (bien) o desapareció sin llegar a ella (se eliminó definitivamente y así se informa; nunca en silencio).
//! * **Tres contadores**: "eliminado definitivamente" y "movido a la Papelera" son sumas de tamaños; "liberado ahora" es la
//!   diferencia MEDIDA del espacio libre de cada volumen antes y después.
//! * **Simulación** (`dry_run`): hace todo salvo lo destructivo y registra las identidades que habría tocado.

use crate::engine::{self, Event, Mode, Outcome, RootStatus, StatsSnapshot, Unit, UnitSpec};
use crate::log::LogWriter;
use crate::paths::PathKey;
use crate::plan::{Cleaner, Plan, PlanItem, TOMBSTONE_EXTRA, fit_for};
use crate::protect::Verdict;
use crate::recycle::{self, BinItem, RecycleMode};
use crate::rules::{CategoryId, Destination};
use crate::sys::{self, FileIdentity};
use crate::volume;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Opciones de la ejecución.
#[derive(Clone, Default)]
pub struct ExecOptions {
    /// Simulación: no se toca nada.
    pub dry_run: bool,
    /// El usuario marcó la casilla del grupo ámbar: lo que no cabe en la Papelera se elimina definitivamente.
    pub allow_unfit_permanent: bool,
    /// Registro local de la ejecución.
    pub log: Option<Arc<LogWriter>>,
}

/// Fase en curso (para la interfaz).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Preparing = 0,
    EmptyingBin = 1,
    Deleting = 2,
    Recycling = 3,
    Verifying = 4,
    Done = 5,
}

/// Progreso compartido con la interfaz y cancelación.
pub struct Progress {
    /// Poner a `true` cancela entre entradas: lo que ya empezó termina bien, lo demás no se toca.
    pub cancel: Arc<AtomicBool>,
    /// Entradas procesadas por el recorrido.
    pub entries: Arc<AtomicU64>,
    phase: AtomicU8,
    /// Elementos del plan terminados y totales.
    pub items_done: AtomicUsize,
    pub items_total: AtomicUsize,
}

impl Progress {
    pub fn new() -> Arc<Progress> {
        Arc::new(Progress {
            cancel: Arc::new(AtomicBool::new(false)),
            entries: Arc::new(AtomicU64::new(0)),
            phase: AtomicU8::new(Phase::Preparing as u8),
            items_done: AtomicUsize::new(0),
            items_total: AtomicUsize::new(0),
        })
    }
    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Relaxed) {
            0 => Phase::Preparing,
            1 => Phase::EmptyingBin,
            2 => Phase::Deleting,
            3 => Phase::Recycling,
            4 => Phase::Verifying,
            _ => Phase::Done,
        }
    }
    fn set_phase(&self, p: Phase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Qué falló al enviar un elemento a la Papelera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecycleFail {
    /// Algo dentro o el propio elemento está en uso (el renombrado falló en seguro).
    InUse,
    /// Sin permiso.
    Denied,
    /// Otro error del sistema al renombrar.
    Rename(u32),
    /// La Papelera no lo aceptó (sigue en su sitio).
    Shell,
}

/// Cómo terminó un elemento.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    /// Definitivo: la raíz se procesó (lo que quedó, en los contadores).
    Done,
    /// Verificado en la Papelera.
    Recycled,
    /// Simulación: se habría enviado a la Papelera.
    WouldRecycle,
    /// No se envió.
    NotRecycled(RecycleFail),
    /// Desapareció sin llegar a la Papelera (se eliminó definitivamente).
    DeletedUnexpectedly,
    /// Desapareció y no se pudo comprobar si llegó a la Papelera.
    Unverified,
    /// La raíz no se pudo procesar (no existe, cambió, protegida, en uso, cancelada...).
    Root(RootStatus),
    /// No cabe en la Papelera y no se marcó la casilla: no se toca.
    SkippedUnfit,
}

/// Resultado de un elemento del plan.
#[derive(Clone, Debug)]
pub struct ItemReport {
    pub id: usize,
    pub category: Option<CategoryId>,
    pub path: PathBuf,
    /// Destino con el que se ejecutó (un elemento que no cabía y se confirmó pasa a `Permanent`).
    pub destination: Destination,
    pub status: ItemStatus,
    pub stats: StatsSnapshot,
}

/// Resultado de vaciar la Papelera.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BinReport {
    /// Elementos de la lista capturada.
    pub requested: u64,
    /// Elementos de la lista que ya no están en la Papelera.
    pub purged: u64,
    /// Elementos de la lista que siguen.
    pub still_there: u64,
    /// Tamaño aproximado de la Papelera al confirmar.
    pub bytes_hint: u64,
    /// La shell no pudo hacer la operación.
    pub failed: bool,
}

/// Espacio libre de un volumen antes y después.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeDelta {
    pub root: PathBuf,
    pub free_before: u64,
    pub free_after: u64,
}

/// El resumen de una ejecución.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub dry_run: bool,
    pub cancelled: bool,
    pub items: Vec<ItemReport>,
    /// Eliminado definitivamente (suma de tamaños, sin enlaces compartidos). En simulación, lo que se habría eliminado.
    pub permanent_bytes: u64,
    /// Movido a la Papelera (verificado). En simulación, lo que se habría movido.
    pub recycled_bytes: u64,
    pub bin: Option<BinReport>,
    /// Contadores sumados de todo (en uso, sin permiso, cambiados, protegidos, enlaces compartidos...).
    pub totals: StatsSnapshot,
    /// "Liberado ahora": diferencia MEDIDA del espacio libre de los volumenes (con signo; `None` en simulación).
    pub freed_now: Option<i64>,
    pub volumes: Vec<VolumeDelta>,
    /// Elementos que desaparecieron sin llegar a la Papelera.
    pub deleted_unexpectedly: u64,
    /// Elementos que no se pudieron enviar.
    pub not_recycled: u64,
    /// Elementos que no caben en la Papelera y no se confirmaron.
    pub skipped_unfit: u64,
    /// Ruta del registro local.
    pub log_path: Option<PathBuf>,
}

/// Por qué no se ejecutó.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecError {
    /// Lo que cabe en la Papelera cambió desde la confirmación: hay que volver a confirmar (PLAN.md §2.5). Lleva los ids.
    NeedsReconfirm(Vec<usize>),
}

/// Un elemento que ya está renombrado a su lápida y espera a la Papelera.
struct Moved {
    item: usize,
    landed: PathBuf,
    original_path: PathBuf,
    original_name: OsString,
    identity: FileIdentity,
    links: u32,
}

/// Nombre de lápida: `<raíz>.tesela-<12 hex><extensión>` (conserva el nombre y la extensión para que, al restaurarlo, se
/// reconozca). El sufijo es aleatorio: el uuid hace imposible adivinarlo.
pub fn tombstone_name(original: &OsString) -> OsString {
    let token = uuid::Uuid::new_v4().simple().to_string();
    let token = &token[..12];
    let wide: Vec<u16> = original.encode_wide().collect();
    // La extensión solo si es corta y "normal"; la raíz se recorta para no pasar de 200 unidades.
    let dot = wide
        .iter()
        .rposition(|&c| c == b'.' as u16)
        .filter(|&i| i > 0);
    let (stem, ext) = match dot {
        Some(i)
            if wide.len() - i <= 12
                && wide[i + 1..]
                    .iter()
                    .all(|&c| c < 128 && (c as u8).is_ascii_alphanumeric()) =>
        {
            (&wide[..i], &wide[i..])
        }
        _ => (&wide[..], &wide[wide.len()..]),
    };
    let stem = &stem[..stem.len().min(200)];
    let mut out: Vec<u16> = stem.to_vec();
    out.extend(format!(".tesela-{token}").encode_utf16());
    out.extend_from_slice(ext);
    debug_assert!(out.len() <= wide.len().min(212) + TOMBSTONE_EXTRA + 12);
    OsString::from_wide(&out)
}

impl Cleaner {
    /// Ejecuta el plan. Bloquea (la interfaz lo lanza en un hilo propio); el progreso y la cancelación van por `progress`.
    pub fn execute(
        &self,
        plan: &Plan,
        opts: &ExecOptions,
        progress: &Arc<Progress>,
    ) -> Result<Report, ExecError> {
        progress.set_phase(Phase::Preparing);
        // 0. Lo que cabe en la Papelera se recalcula AL EJECUTAR; si cambió, se pide confirmar otra vez ANTES de tocar nada.
        let changed: Vec<usize> = plan
            .items
            .iter()
            .filter(|i| i.destination == Destination::Recycle)
            .filter(|i| {
                let size = i
                    .estimate
                    .bytes
                    .max(i.estimate.logical)
                    .max(i.estimate.shared_bytes);
                fit_for(&i.path, size) != i.fit
            })
            .map(|i| i.id)
            .collect();
        if !changed.is_empty() {
            return Err(ExecError::NeedsReconfirm(changed));
        }

        let mut report = Report {
            dry_run: opts.dry_run,
            log_path: opts.log.as_ref().map(|l| l.path().to_path_buf()),
            ..Report::default()
        };
        let cancel = &progress.cancel;
        let mode = if opts.dry_run {
            Mode::DryRun
        } else {
            Mode::Delete
        };

        // Qué se hace con cada elemento.
        let mut perm: Vec<&PlanItem> = Vec::new();
        let mut recyc: Vec<&PlanItem> = Vec::new();
        for it in &plan.items {
            match (it.destination, it.is_unfit()) {
                (Destination::Permanent, _) => perm.push(it),
                (Destination::Recycle, false) => recyc.push(it),
                (Destination::Recycle, true) if opts.allow_unfit_permanent => perm.push(it),
                (Destination::Recycle, true) => {
                    report.skipped_unfit += 1;
                    report.items.push(ItemReport {
                        id: it.id,
                        category: it.category,
                        path: it.path.clone(),
                        destination: Destination::Recycle,
                        status: ItemStatus::SkippedUnfit,
                        stats: StatsSnapshot::default(),
                    });
                }
            }
        }
        let bin_items = plan
            .bin
            .as_ref()
            .map_or(0, |b| usize::from(!b.items.is_empty()));
        progress
            .items_total
            .store(perm.len() + recyc.len() + bin_items, Ordering::Relaxed);

        // Volúmenes: espacio libre antes.
        let mut roots: Vec<PathBuf> = Vec::new();
        let mut seen: HashSet<PathKey> = HashSet::new();
        let mut add_vol = |r: Option<PathBuf>| {
            if let Some(r) = r
                && seen.insert(PathKey::new(&r))
            {
                roots.push(r);
            }
        };
        for it in perm.iter().chain(&recyc) {
            add_vol(it.volume.clone());
        }
        if let Some(b) = &plan.bin {
            for bi in &b.items {
                add_vol(volume::volume_root(&bi.original_parent));
            }
        }
        let free = |r: &Path| tesela_scanner::volume_space(r).map(|v| v.free).ok();
        let before: Vec<Option<u64>> = roots.iter().map(|r| free(r)).collect();

        let sink = opts.log.as_ref().map(|l| l.sink());

        // 1. Vaciar la Papelera con la lista enumerada al confirmar.
        if let Some(bp) = plan.bin.as_ref().filter(|b| !b.items.is_empty()) {
            progress.set_phase(Phase::EmptyingBin);
            report.bin = Some(self.empty_bin(
                &bp.items,
                bp.bytes_hint,
                opts.dry_run,
                cancel,
                sink.as_deref(),
            ));
            progress.items_done.fetch_add(1, Ordering::Relaxed);
        }

        // 2. Lo definitivo: el recorrido por handle.
        if !perm.is_empty() && !cancel.load(Ordering::Relaxed) {
            progress.set_phase(Phase::Deleting);
            let units: Vec<Arc<Unit>> = perm
                .iter()
                .map(|it| {
                    Unit::new(
                        it.id,
                        it.spec.clone(),
                        self.env(mode, it.min_age, cancel, &progress.entries, sink.clone()),
                    )
                })
                .collect();
            engine::run(&units, self.threads);
            for (it, u) in perm.iter().zip(&units) {
                let st = u.status().unwrap_or(RootStatus::Cancelled);
                report.items.push(ItemReport {
                    id: it.id,
                    category: it.category,
                    path: it.path.clone(),
                    destination: Destination::Permanent,
                    status: match st {
                        RootStatus::Done => ItemStatus::Done,
                        other => ItemStatus::Root(other),
                    },
                    stats: u.stats.snapshot(),
                });
                progress.items_done.fetch_add(1, Ordering::Relaxed);
            }
        } else {
            for it in &perm {
                report.items.push(ItemReport {
                    id: it.id,
                    category: it.category,
                    path: it.path.clone(),
                    destination: Destination::Permanent,
                    status: ItemStatus::Root(RootStatus::Cancelled),
                    stats: StatsSnapshot::default(),
                });
            }
        }

        // 3. Lo que va a la Papelera: lápida por handle, IFileOperation y verificación por elemento.
        if !recyc.is_empty() {
            progress.set_phase(Phase::Recycling);
            self.recycle_items(&recyc, opts, progress, sink.clone(), &mut report);
        }

        progress.set_phase(Phase::Verifying);
        // 4. Contadores y espacio libre medido.
        report.cancelled = cancel.load(Ordering::Relaxed);
        for ir in &report.items {
            report.totals.merge(&ir.stats);
            match (ir.destination, ir.status) {
                (Destination::Permanent, ItemStatus::Done) => {
                    report.permanent_bytes += ir.stats.bytes
                }
                (Destination::Recycle, ItemStatus::Recycled | ItemStatus::WouldRecycle) => {
                    report.recycled_bytes += ir.stats.bytes;
                }
                (_, ItemStatus::DeletedUnexpectedly) => report.deleted_unexpectedly += 1,
                (_, ItemStatus::NotRecycled(_)) => report.not_recycled += 1,
                _ => {}
            }
        }
        if !opts.dry_run {
            // La liberación de espacio de NTFS puede tardar un instante en reflejarse.
            std::thread::sleep(Duration::from_millis(250));
            let mut delta: i64 = 0;
            let mut all = true;
            for (r, b) in roots.iter().zip(&before) {
                match (b, free(r)) {
                    (Some(b), Some(a)) => {
                        delta += a as i64 - *b as i64;
                        report.volumes.push(VolumeDelta {
                            root: r.clone(),
                            free_before: *b,
                            free_after: a,
                        });
                    }
                    _ => all = false,
                }
            }
            report.freed_now = (all && !roots.is_empty()).then_some(delta);
        }
        if let Some(l) = &opts.log {
            l.write_note(
                "summary",
                &format!(
                    "definitivo {} B; papelera {} B; liberado ahora {:?}; en uso {}; sin permiso {}",
                    report.permanent_bytes, report.recycled_bytes, report.freed_now, report.totals.in_use, report.totals.denied
                ),
            );
            l.flush();
        }
        progress.set_phase(Phase::Done);
        Ok(report)
    }

    /// Vacía la Papelera con la lista capturada. Solo se purga ESA lista; lo que entre después no se toca.
    fn empty_bin(
        &self,
        items: &[BinItem],
        bytes_hint: u64,
        dry: bool,
        cancel: &Arc<AtomicBool>,
        sink: Option<&engine::EventSink>,
    ) -> BinReport {
        let mut rep = BinReport {
            requested: items.len() as u64,
            bytes_hint,
            ..BinReport::default()
        };
        if dry {
            if let Some(s) = sink {
                for it in items {
                    s(Event {
                        unit: usize::MAX,
                        path: it.original_path(),
                        is_dir: false,
                        identity: None,
                        bytes: 0,
                        links: 0,
                        outcome: Outcome::WouldDelete,
                    });
                }
            }
            rep.purged = rep.requested;
            return rep;
        }
        if cancel.load(Ordering::Relaxed) {
            rep.still_there = rep.requested;
            return rep;
        }
        // En trozos: cada `IFileOperation` es una operación de la shell y la cancelación se mira entre trozos.
        for chunk in items.chunks(500) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if recycle::purge_items(chunk).is_err() {
                rep.failed = true;
            }
        }
        match recycle::list_bin() {
            Ok(now) => {
                let ids: HashSet<&OsString> = now.iter().map(|i| &i.id).collect();
                rep.still_there = items.iter().filter(|i| ids.contains(&i.id)).count() as u64;
                rep.purged = rep.requested - rep.still_there;
            }
            Err(_) => {
                rep.failed = true;
                rep.purged = 0;
                rep.still_there = rep.requested;
            }
        }
        if let Some(s) = sink {
            s(Event {
                unit: usize::MAX,
                path: PathBuf::from("<papelera>"),
                is_dir: false,
                identity: None,
                bytes: bytes_hint,
                links: 0,
                outcome: if rep.still_there == 0 {
                    Outcome::Deleted
                } else {
                    Outcome::NotEmpty
                },
            });
        }
        rep
    }

    /// Envía los elementos a la Papelera (ver el módulo).
    fn recycle_items(
        &self,
        items: &[&PlanItem],
        opts: &ExecOptions,
        progress: &Arc<Progress>,
        sink: Option<Arc<engine::EventSink>>,
        report: &mut Report,
    ) {
        let results: Mutex<HashMap<usize, (ItemStatus, StatsSnapshot)>> =
            Mutex::new(HashMap::new());
        let moved: Mutex<Vec<Moved>> = Mutex::new(Vec::new());
        let next = AtomicUsize::new(0);
        let cancel = &progress.cancel;
        let emit = |it: &PlanItem, id: Option<FileIdentity>, links: u32, o: Outcome| {
            if let Some(s) = &sink {
                s(Event {
                    unit: it.id,
                    path: it.path.clone(),
                    is_dir: it.is_dir(),
                    identity: id,
                    bytes: it.estimate.bytes,
                    links,
                    outcome: o,
                });
            }
        };
        let threads = self.threads.min(items.len()).max(1);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        let Some(&it) = items.get(i) else { return };
                        let mut st = StatsSnapshot::default();
                        if cancel.load(Ordering::Relaxed) {
                            results
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(it.id, (ItemStatus::Root(RootStatus::Cancelled), st));
                            continue;
                        }
                        progress.entries.fetch_add(1, Ordering::Relaxed);
                        let outcome = self.prepare_move(it, opts.dry_run, &mut st);
                        match outcome {
                            Prepared::Moved(m) => {
                                emit(it, Some(m.identity), m.links, Outcome::WouldRecycle);
                                if opts.dry_run {
                                    st.bytes = it.estimate.bytes;
                                    results
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .insert(it.id, (ItemStatus::WouldRecycle, st));
                                } else {
                                    moved.lock().unwrap_or_else(|e| e.into_inner()).push(m);
                                }
                            }
                            Prepared::Rejected(status, o) => {
                                emit(it, None, 0, o);
                                results
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .insert(it.id, (status, st));
                            }
                        }
                    }
                });
            }
        });
        let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
        let moved = moved.into_inner().unwrap_or_else(|e| e.into_inner());

        // Lo renombrado SIEMPRE se termina (aunque se cancele): una lápida a medias dejaría un nombre raro en su sitio.
        if !moved.is_empty() {
            for chunk in moved.chunks(64) {
                let paths: Vec<PathBuf> = chunk.iter().map(|m| m.landed.clone()).collect();
                let _ = recycle::recycle_paths(&paths, RecycleMode::Normal);
            }
            progress.set_phase(Phase::Verifying);
            // Verificación POR ELEMENTO.
            let bin = recycle::list_bin().ok();
            let in_bin: Option<HashSet<(PathKey, String)>> = bin.as_ref().map(|b| {
                b.iter()
                    .map(|i| {
                        (
                            PathKey::new(&i.original_parent),
                            PathKey::new(Path::new(&i.name)).as_str().to_string(),
                        )
                    })
                    .collect()
            });
            let by_id: HashMap<usize, &PlanItem> = items.iter().map(|i| (i.id, *i)).collect();
            for m in &moved {
                let it = by_id[&m.item];
                let mut st = StatsSnapshot::default();
                let still_there = std::fs::symlink_metadata(&m.landed).is_ok();
                let status = if still_there {
                    // No lo aceptó la Papelera: se le devuelve su nombre.
                    restore_name(m);
                    emit(it, Some(m.identity), m.links, Outcome::NotRecycled);
                    ItemStatus::NotRecycled(RecycleFail::Shell)
                } else {
                    let key = m.landed.parent().map(PathKey::new);
                    let name = m
                        .landed
                        .file_name()
                        .map(|n| PathKey::new(Path::new(n)).as_str().to_string());
                    let found = match (&in_bin, key, name) {
                        (Some(set), Some(k), Some(n)) => Some(set.contains(&(k, n))),
                        _ => None,
                    };
                    match found {
                        Some(true) => {
                            st.bytes = it.estimate.bytes;
                            st.files = it.estimate.files;
                            st.dirs = it.estimate.dirs;
                            emit(it, Some(m.identity), m.links, Outcome::Recycled);
                            ItemStatus::Recycled
                        }
                        Some(false) => {
                            emit(it, Some(m.identity), m.links, Outcome::DeletedUnexpectedly);
                            ItemStatus::DeletedUnexpectedly
                        }
                        None => ItemStatus::Unverified,
                    }
                };
                if let Some(l) = &opts.log {
                    l.write_note(
                        "tombstone",
                        &format!(
                            "{} -> {} ({:?})",
                            m.original_path.display(),
                            m.landed.display(),
                            status
                        ),
                    );
                }
                results.insert(m.item, (status, st));
            }
        }
        for it in items {
            let (status, mut stats) = results.remove(&it.id).unwrap_or((
                ItemStatus::Root(RootStatus::Cancelled),
                StatsSnapshot::default(),
            ));
            // Los contadores de lo que se respetó (protegido, reciente...) vienen de la medida al aprobar.
            if matches!(status, ItemStatus::Recycled | ItemStatus::WouldRecycle) {
                stats.shared_files = it.estimate.shared_files;
                stats.shared_bytes = it.estimate.shared_bytes;
                stats.protected = it.estimate.protected;
                stats.reparse = it.estimate.reparse;
            }
            report.items.push(ItemReport {
                id: it.id,
                category: it.category,
                path: it.path.clone(),
                destination: Destination::Recycle,
                status,
                stats,
            });
            progress.items_done.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Abre el elemento por handle, verifica su identidad y su ruta resuelta y lo renombra a su lápida (o, en simulación,
    /// solo verifica).
    fn prepare_move(&self, it: &PlanItem, dry: bool, st: &mut StatsSnapshot) -> Prepared {
        let UnitSpec::Entry {
            path,
            approved,
            approved_tag,
            is_dir,
        } = &it.spec
        else {
            return Prepared::Rejected(ItemStatus::Root(RootStatus::Changed), Outcome::Changed);
        };
        #[cfg(test)]
        crate::engine::test_hooks::before_open(path);
        let h = match sys::open_path_for_rename(path) {
            Ok(h) => h,
            Err(c) => return map_open_error(c, st),
        };
        let m = match sys::meta(&h) {
            Ok(m) => m,
            Err(c) => return map_open_error(c, st),
        };
        let reparse_ok = !m.is_reparse()
            || (!m.is_dir && m.reparse_tag == *approved_tag && sys::is_cloud_tag(m.reparse_tag));
        if m.identity != *approved || m.is_dir != *is_dir || !reparse_ok {
            st.changed += 1;
            return Prepared::Rejected(ItemStatus::Root(RootStatus::Changed), Outcome::Changed);
        }
        let landed_from = match sys::final_path(&h) {
            Ok(p) => p,
            Err(c) => return map_open_error(c, st),
        };
        let key = PathKey::new(&landed_from);
        if PathKey::new(path) != key || self.protection.target_verdict(&key) != Verdict::Allowed {
            st.protected += 1;
            return Prepared::Rejected(ItemStatus::Root(RootStatus::Protected), Outcome::Protected);
        }
        let original_name = landed_from
            .file_name()
            .map(OsString::from)
            .unwrap_or_default();
        let mv = |landed: PathBuf| Moved {
            item: it.id,
            landed,
            original_path: landed_from.clone(),
            original_name: original_name.clone(),
            identity: m.identity,
            links: m.links,
        };
        if dry {
            return Prepared::Moved(mv(landed_from.clone()));
        }
        let tomb = tombstone_name(&original_name);
        let tomb_wide: Vec<u16> = tomb.encode_wide().collect();
        match sys::rename_on_handle(&h, &tomb_wide) {
            Ok(landed) => Prepared::Moved(mv(landed)),
            Err(c) => {
                let (status, o) = match c {
                    sys::SHARING_VIOLATION => {
                        st.in_use += 1;
                        (ItemStatus::NotRecycled(RecycleFail::InUse), Outcome::InUse)
                    }
                    sys::ACCESS_DENIED => {
                        // Una carpeta con algo abierto dentro falla el renombrado con "acceso denegado".
                        st.denied += 1;
                        (
                            ItemStatus::NotRecycled(RecycleFail::Denied),
                            Outcome::Denied,
                        )
                    }
                    other => {
                        st.other_errors += 1;
                        (
                            ItemStatus::NotRecycled(RecycleFail::Rename(other)),
                            Outcome::Failed(other),
                        )
                    }
                };
                Prepared::Rejected(status, o)
            }
        }
    }
}

enum Prepared {
    Moved(Moved),
    Rejected(ItemStatus, Outcome),
}

fn map_open_error(c: sys::Win32, st: &mut StatsSnapshot) -> Prepared {
    match c {
        2 | 3 => Prepared::Rejected(ItemStatus::Root(RootStatus::Missing), Outcome::Failed(c)),
        sys::SHARING_VIOLATION => {
            st.in_use += 1;
            Prepared::Rejected(ItemStatus::NotRecycled(RecycleFail::InUse), Outcome::InUse)
        }
        sys::ACCESS_DENIED => {
            st.denied += 1;
            Prepared::Rejected(
                ItemStatus::NotRecycled(RecycleFail::Denied),
                Outcome::Denied,
            )
        }
        other => {
            st.other_errors += 1;
            Prepared::Rejected(
                ItemStatus::Root(RootStatus::Failed(other)),
                Outcome::Failed(other),
            )
        }
    }
}

/// Un elemento que la Papelera no aceptó sigue en su sitio con el nombre de lápida: se le devuelve su nombre, por handle
/// y comprobando que es el mismo objeto.
fn restore_name(m: &Moved) {
    let Ok(h) = sys::open_path_for_rename(&m.landed) else {
        return;
    };
    if sys::meta(&h).is_ok_and(|x| x.identity == m.identity) {
        let name: Vec<u16> = m.original_name.encode_wide().collect();
        let _ = sys::rename_on_handle(&h, &name);
    }
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
