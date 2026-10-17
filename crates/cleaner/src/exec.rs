//! La ejecución del plan (PLAN.md §2.5 y §3.7.2 a §3.7.9).
//!
//! Orden: primero se vacía la Papelera (con la lista capturada al confirmar) y después se borra lo definitivo y se mueve
//! lo que va a la Papelera: así lo que el usuario quiere conservar recuperable no se borra en el mismo paso.
//!
//! * **Definitivo**: el recorrido por handle de `engine` (apertura exclusiva, identidad, `FileDispositionInfoEx`).
//! * **Papelera** (sin lápidas): se abre el elemento en exclusiva, se verifica su identidad y su ruta resuelta (local y
//!   permitida), se CIERRA el handle y se entrega la RUTA ORIGINAL a `IFileOperation` con un receptor de eventos propio.
//!   La shell dice, por elemento, el resultado y la ruta real del elemento ya en la Papelera; se abre y se compara su
//!   IDENTIDAD con la aprobada: igual = enviado; distinta = "se movió a la Papelera un objeto distinto del aprobado"
//!   (recuperable: está en la Papelera); sin elemento en la Papelera = eliminado definitivamente (no cabía, o un sustituto
//!   que no cabía); error = omitido con su motivo, solo si el objeto aprobado sigue intacto en su sitio (A-M1).
//!   Un directorio con algo abierto dentro falla en seguro (mover es renombrar) y se informa como "en uso". El receptor
//!   vigila la entrega (devuelve `E_ABORT`): `PreDeleteItem` compara la identidad de lo que hay en la ruta con la aprobada
//!   y veta el sustituto (M1); `PostDeleteItem` corta tras el primer borrado definitivo o la primera ruta sin verificar
//!   (M2). Lo que queda del lote no se procesa y sigue en su sitio.
//!   A3-M1: un DIRECTORIO lleva una vigilancia de cambios (`ReadDirectoryChangesW` sobre un segundo handle verificado)
//!   abierta ANTES de su recorrido de medida y viva hasta su `PreDeleteItem`: si algo cambia dentro, o no se puede vigilar,
//!   la medida no vale y no se envía ("cambió desde la confirmación"). A3-M2: la ocupación de la Papelera se lee antes y
//!   después del recorrido y manda la mayor; es una heurística, no una garantía (la shell decide por su cuenta si cabe).
//! * **Tres contadores**: "eliminado definitivamente" y "movido a la Papelera" son sumas de tamaños; "liberado ahora" es la
//!   diferencia MEDIDA del espacio libre de cada volumen antes y después.
//! * **Simulación** (`dry_run`): hace todo salvo lo destructivo y registra las identidades que habría tocado.

use crate::engine::{self, Event, Mode, Outcome, RootStatus, StatsSnapshot, Unit, UnitSpec};
use crate::log::LogWriter;
use crate::paths::PathKey;
use crate::plan::{
    Cleaner, Fit, Plan, PlanItem, RejectReason, bin_big, bin_cap, fit_for, fit_for_deep,
};
use crate::protect::Verdict;
use crate::recycle::{self, BinItem, RecycleMode};
use crate::rules::{CategoryId, Destination};
use crate::sys::{self, FileIdentity};
use crate::volume;
use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use windows::core::HRESULT;

/// Cuántos rechazados del plan se anotan en el registro (el resto, en una sola línea).
const MAX_REJECTED_NOTES: usize = 200;

/// Opciones de la ejecución.
#[derive(Clone, Default)]
pub struct ExecOptions {
    /// Simulación: no se toca nada.
    pub dry_run: bool,
    /// El usuario marcó la casilla del grupo ámbar: lo que no cabe en la Papelera se elimina definitivamente.
    pub allow_unfit_permanent: bool,
    /// Registro local de la ejecución. OBLIGATORIO salvo en simulación (M4): sin un registro utilizable no hay ninguna
    /// acción destructiva ([`ExecError::LogRequired`]).
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
    cancel: Arc<AtomicBool>,
    /// El USUARIO pidió cancelar (`cancel()`). `cancel` también se activa solo si el registro falla (M4): este distingue una
    /// cosa de la otra para que el informe diga las dos (H6.5).
    user_cancelled: AtomicBool,
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
            user_cancelled: AtomicBool::new(false),
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
    /// Indica la fase en curso (lo hace el ejecutor; el respaldo simulado de la demostración también).
    pub fn set_phase(&self, p: Phase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }
    /// Fija cuántos elementos tiene la ejecución.
    pub fn set_items_total(&self, n: usize) {
        self.items_total.store(n, Ordering::Relaxed);
    }
    /// Un elemento terminado más.
    pub fn add_item_done(&self) {
        self.items_done.fetch_add(1, Ordering::Relaxed);
    }
    /// ¿Se pidió cancelar (el usuario, o el fallo del registro, que también detiene la limpieza)?
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    /// Cancela: entre entradas, lo que ya empezó termina bien y lo demás no se toca. Es la ÚNICA forma de cancelar desde
    /// fuera y siempre anota que lo pidió el usuario (`user_cancelled`), que es lo que distingue el informe (R3).
    pub fn cancel(&self) {
        self.user_cancelled.store(true, Ordering::SeqCst);
        self.cancel.store(true, Ordering::SeqCst);
    }
    /// ¿Lo canceló el usuario (aunque además haya fallado el registro)?
    pub fn user_cancelled(&self) -> bool {
        self.user_cancelled.load(Ordering::SeqCst)
    }
}

/// Cuántos elementos de la lista cambiaron desde la confirmación: los que no cuadraban al enumerar (`not_matching`) más los
/// elementos a los que se atribuye algún veto (`Veto::id`: el `$R` dueño, no la ruta vetada, que puede ser de un hijo).
pub(crate) fn bin_changed(
    not_matching: usize,
    vetoes: &[recycle::Veto],
    wanted: &HashSet<&OsString>,
) -> u64 {
    let mut vetoed: Vec<&OsString> = vetoes
        .iter()
        .map(recycle::Veto::id)
        .filter(|id| wanted.contains(id))
        .collect();
    vetoed.sort();
    vetoed.dedup();
    (not_matching + vetoed.len()) as u64
}

/// Qué falló al enviar un elemento a la Papelera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecycleFail {
    /// Algo dentro o el propio elemento está en uso (falló en seguro: mover es renombrar).
    InUse,
    /// Sin permiso.
    Denied,
    /// La shell no lo procesó y sigue en su sitio.
    NotProcessed,
    /// La shell dijo que abortó la operación en la que iba y el elemento sigue en su sitio.
    Aborted,
    /// No hay sitio en la Papelera ahora (capacidad menos lo que ya ocupa): enviarlo haría que la shell lo elimine o
    /// expulse elementos que ya estaban. No se toca.
    NoRoom,
    /// Se detuvo el envío a la Papelera por seguridad (otro elemento acabó eliminado definitivamente o no se pudo
    /// verificar): no se tocó.
    Stopped,
    /// La shell lo rechazó con este `HRESULT` (sigue en su sitio).
    Shell(i32),
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
    /// Desapareció sin llegar a la Papelera (la shell lo eliminó definitivamente: no cabía) o, en un DIRECTORIO, su contenido
    /// ya no es el de justo antes del envío (una medida completa con otra huella: perdió una parte, o cambió, por ejemplo
    /// porque otro proceso escribía dentro). Para el directorio no se afirma el borrado: se afirma la diferencia (H2).
    DeletedUnexpectedly,
    /// Se movió a la Papelera un objeto DISTINTO del aprobado (alguien lo sustituyó entre la comprobación y la shell).
    /// Está en la Papelera: se puede restaurar.
    RecycledOther,
    /// No se pudo comprobar qué pasó (la shell no dio la ruta del elemento en la Papelera, o no se pudo abrir).
    Unverified,
    /// La raíz no se pudo procesar (no existe, cambió, protegida, en uso, cancelada...).
    Root(RootStatus),
    /// No se toca: no cabe en la Papelera y no se marcó la casilla de borrado definitivo, o es de los que NUNCA se borran
    /// definitivamente ([`PlanItem::never_permanent`]: informes de errores y volcados).
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
    /// Lo que desapareció de verdad (rutas completas): la interfaz actualiza el árbol con esto, sin reescanear. Vacío en
    /// simulación y en lo que no se tocó.
    pub removed: Vec<Removed>,
}

/// Una entrada que desapareció del disco, o una parte de su contenido (ver [`ItemReport::removed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    pub path: PathBuf,
    pub is_dir: bool,
    /// Desapareció entera (si no, se eliminó solo parte de su contenido).
    pub removed: bool,
    /// Bytes en disco eliminados (sin enlaces compartidos), tamaño lógico y archivos.
    pub bytes: u64,
    pub logical: u64,
    pub files: u64,
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
    /// De ellos, los que NO se tocaron porque su `$R` o su `$I` ya no eran los de la enumeración (cambiaron desde la
    /// confirmación) o no se pudieron verificar.
    pub changed: u64,
    /// Elementos que la shell dio por purgados pero cuyo objeto aprobado NO se eliminó: entre la última comprobación y el
    /// borrado se sustituyó su `$R` y se borró lo que había con su nombre (A-M4). No cuentan como purgados y no se pudo
    /// evitar: la ventana que queda entre el receptor y la shell.
    pub replaced: u64,
    /// Elementos que no se tocaron porque su sistema de archivos no da identidad (FAT, exFAT) y no se pueden verificar.
    /// No es que hayan cambiado (H6.9).
    pub unverifiable: u64,
    /// Elementos que NO se purgaron porque su `$R` lleva reparse points dentro (junction, symlink, enlace de WSL, archivo de
    /// la nube) o no se pudo comprobar entero (M2): se excluyen ANTES de entregarlos y no cortan el lote.
    pub with_links: u64,
    /// Elementos de la Papelera que no se listaron al confirmar porque sus metadatos (`$I`) están ilegibles: no se vacían y su
    /// tamaño no cuenta en lo que se prometió (m1).
    pub unlisted: u64,
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
    /// Elementos cuyo destino en la Papelera es un objeto distinto del aprobado (están en la Papelera: se pueden restaurar).
    pub recycled_other: u64,
    /// Elementos de los que no se pudo comprobar qué pasó.
    pub unverified: u64,
    /// El primer error (`HRESULT`) que devolvió la shell o COM al enviar a la Papelera, si hubo alguno. Lo que no se pudo
    /// enviar queda como no procesado y sigue donde estaba.
    pub shell_error: Option<i32>,
    /// El envío a la Papelera se DETUVO por seguridad (ver [`StopReason`]): los que quedaban no se tocaron.
    pub recycle_stopped: bool,
    /// Por qué se detuvo (el primero), si `recycle_stopped`.
    pub stop_reason: Option<StopReason>,
    /// Elementos que no se enviaron por falta de sitio en la Papelera.
    pub no_room: u64,
    /// Elementos que no se enviaron por el corte de seguridad.
    pub stopped_items: u64,
    /// Lo que hay en TODA la Papelera tras la ejecución (según la shell): es lo que vaciaría "Vaciar ahora". `None` si
    /// no se envió nada a la Papelera, en simulación o si la consulta falló.
    pub bin_total_after: Option<u64>,
    /// Elementos que no se pudieron enviar.
    pub not_recycled: u64,
    /// Elementos que no caben en la Papelera y no se confirmaron.
    pub skipped_unfit: u64,
    /// Ruta del registro local.
    pub log_path: Option<PathBuf>,
    /// El registro local FALLÓ al escribir o volcar (M4): la limpieza se detuvo en el siguiente punto seguro y no se tocó
    /// nada más. Lo que ya estaba hecho se queda (y puede no constar entero en el registro).
    pub log_failed: bool,
    /// El registro llegó a su tope (líneas o bytes) y dejó de anotar: la limpieza siguió, pero lo que vino después NO consta
    /// en el registro (H5).
    pub log_truncated: bool,
}

/// Por qué se detuvo el envío a la Papelera (corte de seguridad).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// La shell eliminó definitivamente un elemento (no cabía en la Papelera) o el contenido de un directorio ya no es el de
    /// antes del envío (H2: puede haber cambiado sin que se haya perdido nada).
    DeletedUnexpectedly,
    /// No se pudo comprobar qué pasó con un elemento (la shell no dijo dónde quedó, o no se pudo abrir).
    Unverified,
    /// La shell dijo que "eliminó" el elemento sin enviarlo a la Papelera (no cabía) pero NO lo tocó: sigue en su sitio, o
    /// falló con un error, se comprobó que lo dejó intacto y el receptor canceló con ello lo que quedaba de su lote
    /// (A2-M1). Se detiene el envío igual (la shell se comporta de forma inesperada), pero no se afirma que se haya borrado
    /// nada.
    ShellDeclined,
    /// Lo que había en la ruta justo antes de que la shell actuara no era lo aprobado (`PreDeleteItem` lo vetó): se detuvo
    /// el envío y no se mandó nada más.
    Changed,
}

/// Por qué no se ejecutó.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecError {
    /// Lo que cabe en la Papelera cambió desde la confirmación: hay que volver a confirmar (PLAN.md §2.5). Lleva los ids.
    NeedsReconfirm(Vec<usize>),
    /// No hay registro local utilizable (no se pudo crear, o ya había fallado) y no es una simulación: sin registro no hay
    /// acción destructiva (M4). No se ha tocado nada.
    LogRequired,
    /// El proceso se ejecuta ELEVADO (como administrador): la limpieza real y vaciar la Papelera no se hacen (PLAN.md §3.9: la
    /// aplicación nunca limpia elevada). El análisis y la simulación sí se permiten. No se ha tocado nada.
    Elevated,
}

/// ¿Está elevado el proceso? Un rasgo para poder probar las dos ramas sin elevar nada (el CI no se puede elevar).
pub(crate) trait Elevation: Send + Sync {
    fn is_elevated(&self) -> bool;
}

/// La consulta real al token del proceso. FALLA CERRADO: si no se puede consultar, se trata como elevado (sin limpieza).
pub(crate) struct RealElevation;

impl Elevation for RealElevation {
    fn is_elevated(&self) -> bool {
        elevated_from(sys::process_elevation())
    }
}

/// SOLO PRUEBAS: "no elevado", el valor POR DEFECTO de todo `Cleaner` en las pruebas. Los ejecutores de integración continua
/// (`windows-latest`) corren ELEVADOS (UAC desactivado) y, con la consulta real, toda prueba que limpia daría `Elevated`: las
/// pruebas no dependen del proceso que las lanza. Solo las pruebas de E1 inyectan "elevado" (`Cleaner::with_elevation`). En
/// producción el valor por defecto es siempre [`RealElevation`].
#[cfg(test)]
pub(crate) struct NotElevated;

#[cfg(test)]
impl Elevation for NotElevated {
    fn is_elevated(&self) -> bool {
        false
    }
}

/// Falla cerrado: sin poder consultar el token se trata como elevado.
fn elevated_from(state: Option<bool>) -> bool {
    state.unwrap_or(true)
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
        // E1: ELEVADO NO SE LIMPIA (ni se vacía la Papelera). La simulación sí puede seguir. Se comprueba ANTES que nada y
        // aquí, en el `cleaner` (no solo en la interfaz): el manifiesto es `asInvoker` y un proceso elevado que lance Tesela
        // le pasa sus privilegios.
        if !opts.dry_run && self.elevation.is_elevated() {
            if let Some(l) = &opts.log {
                let _ = l.discard();
            }
            return Err(ExecError::Elevated);
        }
        // M4: SIN REGISTRO UTILIZABLE NO HAY ACCIÓN DESTRUCTIVA. La simulación sí puede seguir sin él.
        if !opts.dry_run && opts.log.as_ref().is_none_or(|l| !l.ok()) {
            if let Some(l) = &opts.log {
                // Si ya contiene operaciones no se borra (`discard` lo impide).
                let _ = l.discard();
            }
            return Err(ExecError::LogRequired);
        }
        // Un error de escritura del registro activa la cancelación: la limpieza se detiene en el siguiente punto seguro.
        if let Some(l) = &opts.log {
            l.cancel_on_error(progress.cancel.clone());
            // Lo que no llegó al plan consta en el registro con su motivo (una raíz descartada por un enlace, p. ej.).
            // Con tope: un plan con miles de rechazados no puede inflar el registro (R4).
            // Los "demasiado recientes" van los últimos: pueden ser muchos y no deben tapar un rechazo de seguridad.
            let ordered = plan
                .rejected
                .iter()
                .filter(|r| r.reason != RejectReason::Young)
                .chain(
                    plan.rejected
                        .iter()
                        .filter(|r| r.reason == RejectReason::Young),
                );
            for r in ordered.take(MAX_REJECTED_NOTES) {
                l.write_rejected(&r.path, &r.reason.log_text());
            }
            if plan.rejected.len() > MAX_REJECTED_NOTES {
                l.write_rejected(
                    Path::new("..."),
                    &format!(
                        "{} más sin anotar",
                        plan.rejected.len() - MAX_REJECTED_NOTES
                    ),
                );
            }
        }
        // 0. Lo que cabe en la Papelera se recalcula AL EJECUTAR; si cambió, se pide confirmar otra vez ANTES de tocar nada.
        let changed: Vec<usize> = plan
            .items
            .iter()
            .filter(|i| i.destination == Destination::Recycle)
            .filter(|i| {
                // GM-M3: bytes + compartidos (conjuntos disjuntos) y el lógico; el mayor manda. Con la ruta más larga de lo
                // que hay dentro, como al aprobar.
                let size = i.estimate.recycle_charge();
                self.fit_deep(&i.path, size, i.estimate.max_path as usize) != i.fit
            })
            .map(|i| i.id)
            .collect();
        if !changed.is_empty() {
            // Nada se ejecutó: el registro (solo la línea de arranque) no debe ocupar una plaza de la retención.
            if let Some(l) = &opts.log {
                let _ = l.discard();
            }
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
            // Falla cerrado: lo que `never_permanent` prohíbe (informes y volcados, o una unidad suelta con antigüedad mínima,
            // cuyo recorrido definitivo no es todo-o-nada) no llega nunca a la rama definitiva: ni por destino ni con la casilla.
            match (it.destination, it.is_unfit()) {
                (Destination::Permanent, _) if !it.never_permanent() => perm.push(it),
                (Destination::Recycle, false) => recyc.push(it),
                (Destination::Recycle, true)
                    if opts.allow_unfit_permanent && !it.never_permanent() =>
                {
                    perm.push(it)
                }
                (Destination::Permanent | Destination::Recycle, _) => {
                    report.skipped_unfit += 1;
                    report.items.push(ItemReport {
                        id: it.id,
                        category: it.category,
                        path: it.path.clone(),
                        destination: it.destination,
                        status: ItemStatus::SkippedUnfit,
                        stats: StatsSnapshot::default(),
                        removed: Vec::new(),
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
            // m1: lo que no se pudo listar consta en el registro (y en el informe, `BinReport::unlisted`).
            if plan.bin_unlisted > 0
                && let Some(l) = &opts.log
            {
                l.write_note(
                    "bin-unlisted",
                    &format!(
                        "{} elementos de la Papelera con los metadatos ilegibles no se listaron y no se vacían",
                        plan.bin_unlisted
                    ),
                );
            }
            report.bin = Some(self.empty_bin(
                &bp.items,
                bp.bytes_hint,
                opts.dry_run,
                cancel,
                sink.as_deref(),
                opts.log.as_deref(),
            ));
            if let Some(b) = report.bin.as_mut() {
                b.unlisted = plan.bin_unlisted;
            }
            progress.items_done.fetch_add(1, Ordering::Relaxed);
        }

        // 2. Lo definitivo: el recorrido por handle. Antes, la INTENCIÓN (los elementos de la operación) escrita y volcada: si
        // no se puede, no se borra nada.
        let can_delete = !perm.is_empty()
            && !cancel.load(Ordering::Relaxed)
            && opts.log.as_ref().is_none_or(|l| {
                let roots: Vec<PathBuf> = perm.iter().map(|it| it.path.clone()).collect();
                l.write_intent("delete-begin", &roots).is_ok()
            });
        if can_delete {
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
                // El resultado de cada raíz, tras la intención (las entradas ya constan una a una).
                if let Some(l) = &opts.log {
                    l.write_note("delete-end", &format!("{} -> {:?}", it.path.display(), st));
                }
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
                    removed: if opts.dry_run {
                        Vec::new()
                    } else {
                        removed_of(it, u)
                    },
                });
                progress.items_done.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(l) = &opts.log {
                l.flush();
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
                    removed: Vec::new(),
                });
            }
        }

        // 3. Lo que va a la Papelera: comprobar, soltar el handle, IFileOperation con receptor y juicio por identidad.
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
                (_, ItemStatus::RecycledOther) => report.recycled_other += 1,
                (_, ItemStatus::Unverified) => report.unverified += 1,
                (_, ItemStatus::NotRecycled(RecycleFail::NoRoom)) => {
                    report.no_room += 1;
                    report.not_recycled += 1;
                }
                (_, ItemStatus::NotRecycled(RecycleFail::Stopped)) => {
                    report.stopped_items += 1;
                    report.not_recycled += 1;
                }
                (_, ItemStatus::NotRecycled(_)) => report.not_recycled += 1,
                _ => {}
            }
        }
        if !opts.dry_run && (report.recycled_bytes > 0 || report.recycled_other > 0) {
            report.bin_total_after = recycle::bin_totals().ok().map(|t| t.1);
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
            // Al terminar, `sync_all` a disco (una vez; ver el módulo `log`).
            let _ = l.sync();
        }
        report.log_failed = opts.log.as_ref().is_some_and(|l| !l.ok());
        report.log_truncated = opts.log.as_ref().is_some_and(|l| l.truncated());
        // `cancelled` es SOLO la cancelación del usuario; un fallo del registro se informa aparte (y pueden darse las dos).
        report.cancelled = progress.user_cancelled();
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
        log: Option<&LogWriter>,
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
        if cancel.load(Ordering::Relaxed) || self.elevation.is_elevated() {
            // Cancelado, o elevado (E1: segunda línea de defensa; `execute` ya lo rechaza antes): no se purga nada.
            rep.still_there = rep.requested;
            rep.failed = !cancel.load(Ordering::Relaxed);
            return rep;
        }
        // Se purga a través de la carpeta shell de la Papelera (ver `recycle::purge_items`): la shell borra `$R` y `$I` a la
        // vez. El resultado sale de lo que la propia shell notificó por elemento (no se vuelve a enumerar la Papelera).
        // Cada elemento consta POR SEPARADO en el registro: la intención de cada trozo antes de entregarlo a la shell y, tras
        // él, el resultado de cada uno (purgado, error o sin aviso); los saltados por haber cambiado, también.
        let mut observe = |step: recycle::PurgeStep<'_>| {
            let Some(l) = log else { return };
            match step {
                recycle::PurgeStep::Begin(items) => {
                    let paths: Vec<PathBuf> = items.iter().map(|i| i.original_path()).collect();
                    // Un error aquí marca el registro y activa la cancelación: el trozo no se entrega.
                    let _ = l.write_intent("bin-begin", &paths);
                }
                recycle::PurgeStep::Skipped(items) => {
                    for i in items {
                        l.write_bin_item(&i.original_path(), &i.id, i.size, "changed");
                    }
                    l.flush();
                }
                recycle::PurgeStep::Unverifiable(items) => {
                    for i in items {
                        l.write_bin_item(&i.original_path(), &i.id, i.size, "no-identity");
                    }
                    l.flush();
                }
                recycle::PurgeStep::Replaced(items) => {
                    // A-M4: la shell dio la purga por buena pero lo borrado no fue el objeto aprobado.
                    for i in items {
                        l.write_bin_item(&i.original_path(), &i.id, i.size, "replaced");
                    }
                    l.flush();
                }
                recycle::PurgeStep::Linked(items) => {
                    // M2: el `$R` lleva enlaces dentro (o no se pudo comprobar): no se entrega a la shell.
                    for i in items {
                        l.write_bin_item(&i.original_path(), &i.id, i.size, "linked");
                    }
                    l.flush();
                }
                recycle::PurgeStep::Vetoed(items) => {
                    // A-M4: la shell iba a borrar algo que ya no era lo capturado y el receptor lo vetó.
                    for i in items {
                        l.write_bin_item(&i.original_path(), &i.id, i.size, "vetoed");
                    }
                    l.flush();
                }
                recycle::PurgeStep::Done(done) => {
                    for (i, hr) in done {
                        let out = match hr {
                            Some(h) if *h >= 0 => "purged".to_string(),
                            Some(h) => format!("error-0x{:08X}", *h as u32),
                            None => "not-processed".to_string(),
                        };
                        l.write_bin_item(&i.original_path(), &i.id, i.size, &out);
                    }
                    l.flush();
                }
            }
        };
        match self.shell.purge(items, cancel, &mut observe) {
            Ok(out) => {
                let wanted: HashSet<&OsString> = items.iter().map(|i| &i.id).collect();
                let purged: HashSet<&OsString> = out
                    .events
                    .iter()
                    .filter(|e| e.succeeded() && wanted.contains(&e.item))
                    .filter(|e| !out.replaced_bin.contains(&e.item))
                    .map(|e| &e.item)
                    .collect();
                rep.replaced = out
                    .replaced_bin
                    .iter()
                    .filter(|i| wanted.contains(i))
                    .count() as u64;
                rep.purged = purged.len() as u64;
                rep.still_there = rep.requested - rep.purged;
                // Cambiaron desde la confirmación: los que no cuadraban al enumerar y los que cambiaron DESPUÉS, justo antes de
                // que la shell los borrara (veto de `PreDeleteItem`, A-M4).
                rep.changed = bin_changed(out.changed_bin.len(), &out.vetoed, &wanted);
                rep.unverifiable = out.unverifiable_bin.len() as u64;
                rep.with_links =
                    out.linked_bin.iter().filter(|i| wanted.contains(i)).count() as u64;
                // El `E_ABORT` que provoca el propio receptor al vetar no es un fallo de la shell.
                rep.failed = out.error.is_some() && !out.sink_aborted;
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

    /// Envía los elementos a la Papelera (ver el módulo), por trozos:
    /// 1. se PREPARA cada elemento (apertura exclusiva para capturar la identidad y detectar "en uso", ruta resuelta y
    ///    protección) y se deja abierto un handle de GUARDA hasta después de juzgarlo (ver `prepare_recycle`);
    /// 2. los elementos de un volumen se reparten en OPERACIONES de la shell según el espacio que queda en su Papelera
    ///    ([`take_op`]): la shell decide POR OPERACIÓN si lo que va cabe, y un elemento que no cabe arrastra a todos los
    ///    de su operación. Se lee la ocupación antes de cada operación y, JUSTO ANTES de entregarla, se vuelve a medir cada
    ///    candidato (A-B1: un archivo por su handle de guarda, un directorio con un recorrido COMPLETO, A-B2) y se vuelve a
    ///    comprobar el presupuesto con esos tamaños y la ocupación de ese instante;
    /// 3. se entregan las rutas ORIGINALES a la shell y se juzga cada elemento con lo que la propia shell notificó;
    /// 4. CORTE DE SEGURIDAD: si un elemento acaba eliminado definitivamente (`DeletedUnexpectedly`), se DETIENE el resto
    ///    del envío: los siguientes quedan `NotRecycled(Stopped)` y el resumen lo dice. Nada se pierde en cadena.
    ///
    /// Cada elemento se anota y se vuelca al registro en cuanto termina, y antes de cada operación se anotan sus rutas.
    fn recycle_items(
        &self,
        items: &[&PlanItem],
        opts: &ExecOptions,
        progress: &Arc<Progress>,
        sink: Option<Arc<engine::EventSink>>,
        report: &mut Report,
    ) {
        let shell: &dyn RecycleShell = self.shell.as_ref();
        let mut results: HashMap<usize, (ItemStatus, StatsSnapshot)> = HashMap::new();
        let cancel = &progress.cancel;
        // Para las comprobaciones POSTERIORES a la shell (¿intacto el contenido?): no se cancelan.
        let verify_cancel = Arc::new(AtomicBool::new(false));
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
            // Tras CADA elemento se vuelca el registro: si la aplicación se cierra a mitad, consta hasta dónde llegó.
            if let Some(l) = &opts.log {
                l.flush();
            }
        };
        // A3-M1: ¿cambió algo dentro de alguno de los directorios de la operación desde que se midió (lo vio su vigilancia)?
        let watch_changed = |entries: &[(&PlanItem, Pending)]| {
            entries
                .iter()
                .any(|(_, p)| p.watch.as_ref().is_some_and(|w| w.changed()))
        };
        // La operación no se entrega: sus elementos constan como "cambió desde la confirmación".
        let reject_changed =
            |entries: &[(&PlanItem, Pending)],
             results: &mut HashMap<usize, (ItemStatus, StatsSnapshot)>| {
                for (it, _) in entries {
                    emit(it, None, 0, Outcome::Changed);
                    results.insert(
                        it.id,
                        (
                            ItemStatus::Root(RootStatus::Changed),
                            StatsSnapshot {
                                changed: 1,
                                ..StatsSnapshot::default()
                            },
                        ),
                    );
                }
            };
        let mut stopped = false;
        for chunk in items.chunks(RECYCLE_CHUNK) {
            if cancel.load(Ordering::Relaxed) {
                for it in chunk {
                    results.insert(
                        it.id,
                        (
                            ItemStatus::Root(RootStatus::Cancelled),
                            StatsSnapshot::default(),
                        ),
                    );
                }
                continue;
            }
            if stopped {
                for it in chunk {
                    emit(it, None, 0, Outcome::NotRecycled);
                    results.insert(
                        it.id,
                        (
                            ItemStatus::NotRecycled(RecycleFail::Stopped),
                            StatsSnapshot::default(),
                        ),
                    );
                }
                continue;
            }
            // 1. Preparar: comprobar y dejar el handle de guarda.
            let mut slots: Vec<Option<(&PlanItem, Pending)>> = Vec::new();
            for &it in chunk {
                let mut st = StatsSnapshot::default();
                progress.entries.fetch_add(1, Ordering::Relaxed);
                match self.prepare_recycle(it, &mut st) {
                    Prepared::Ready(mut p) if opts.dry_run => {
                        // La simulación también mide lo que mediría el envío real.
                        if let Err((status, o)) = self.fresh_size(it, &mut p, &mut st, cancel) {
                            emit(it, None, 0, o);
                            results.insert(it.id, (status, st));
                            continue;
                        }
                        emit(it, Some(p.identity), p.links, Outcome::WouldRecycle);
                        st.bytes = p.disk;
                        st.logical = p.logical;
                        results.insert(it.id, (ItemStatus::WouldRecycle, st));
                    }
                    Prepared::Ready(p) => slots.push(Some((it, *p))),
                    Prepared::Rejected(status, o) => {
                        emit(it, None, 0, o);
                        results.insert(it.id, (status, st));
                    }
                }
            }
            // 2. Repartir por volumen (en el orden de llegada) y enviar operación a operación.
            let mut buckets: Vec<(Option<PathKey>, Option<PathBuf>, VecDeque<usize>)> = Vec::new();
            for (i, s) in slots.iter().enumerate() {
                let (it, _) = s.as_ref().expect("recién preparado");
                let key = it.volume.as_deref().map(PathKey::new);
                match buckets.iter_mut().find(|(k, ..)| *k == key) {
                    Some((.., q)) => q.push_back(i),
                    None => buckets.push((key, it.volume.clone(), VecDeque::from([i]))),
                }
            }
            for (_, vol, mut queue) in buckets {
                while let Some(&first) = queue.front() {
                    if cancel.load(Ordering::Relaxed) || stopped {
                        // Sin enviar: lo preparado y no entregado a la shell no se toca.
                        for i in queue.drain(..) {
                            if let Some((it, _)) = slots[i].take() {
                                let fail = if stopped {
                                    emit(it, None, 0, Outcome::NotRecycled);
                                    ItemStatus::NotRecycled(RecycleFail::Stopped)
                                } else {
                                    ItemStatus::Root(RootStatus::Cancelled)
                                };
                                results.insert(it.id, (fail, StatsSnapshot::default()));
                            }
                        }
                        break;
                    }
                    let first_is_dir = slots[first].as_ref().is_some_and(|(it, _)| it.is_dir());
                    // A2-B1: un DIRECTORIO va SIEMPRE solo en su operación. Un recorrido tarda; con varios candidatos en la
                    // misma operación, lo que ya estaba medido podía crecer mientras se medían los demás y llegar a la shell
                    // con un tamaño que ya no era cierto (`PreDeleteItem` comprueba la identidad, no el tamaño). Solo, el
                    // directorio se mide y se entrega sin nada de por medio salvo la línea de intención del registro
                    // (§3.7.13). A3-M2: la ocupación de la Papelera se lee ANTES del recorrido y OTRA VEZ después, y manda la
                    // MAYOR de las dos (`BinRoom::worst`): con una sola lectura previa, la Papelera podía llenarse mientras
                    // se medía (Astra: capacidad 100 000, vacía al empezar, 99 000 al acabar el recorrido, y una carpeta de
                    // 4 000 bytes se enviaba con la lectura vieja). La segunda lectura son unos milisegundos y cae dentro de la
                    // ventana, que es donde debe estar. La ocupación es una HEURÍSTICA, no una garantía: un tercero puede
                    // cambiarla hasta que la shell actúe y la shell decide por su cuenta si el elemento cabe.
                    let entries: Vec<(&PlanItem, Pending)> = if first_is_dir {
                        queue.pop_front();
                        let Some((it, mut p)) = slots[first].take() else {
                            continue;
                        };
                        let room = vol
                            .as_deref()
                            .map_or(BinRoom::UNKNOWN, |r| shell.bin_room(r));
                        let mut st = StatsSnapshot::default();
                        if let Err((status, o)) = self.fresh_size(it, &mut p, &mut st, cancel) {
                            emit(it, None, 0, o);
                            results.insert(it.id, (status, st));
                            continue;
                        }
                        // Con la medida recién hecha y la MAYOR de las dos ocupaciones: la de antes y la de después del recorrido.
                        let room_after = vol
                            .as_deref()
                            .map_or(BinRoom::UNKNOWN, |r| shell.bin_room(r));
                        let room = room.worst(room_after);
                        if let Take::NoRoom = take_op(&[p.size], room) {
                            // No hay sitio ahora: enviarlo haría que la shell lo elimine o expulse lo que ya hay en la
                            // Papelera. No se toca (el handle de guarda se suelta al salir de este bloque).
                            emit(it, None, 0, Outcome::NotRecycled);
                            results.insert(
                                it.id,
                                (
                                    ItemStatus::NotRecycled(RecycleFail::NoRoom),
                                    StatsSnapshot::default(),
                                ),
                            );
                            continue;
                        }
                        vec![(it, p)]
                    } else {
                        // ARCHIVOS: pueden viajar juntos (nunca con un directorio). El primero de la cola se mide ahora
                        // (justo antes de decidir su operación): si creció, no se envía.
                        if let Some((it, p)) = slots[first].as_mut() {
                            let mut st = StatsSnapshot::default();
                            if let Err((status, o)) = self.fresh_size(it, p, &mut st, cancel) {
                                emit(it, None, 0, o);
                                results.insert(it.id, (status, st));
                                slots[first] = None;
                                queue.pop_front();
                                continue;
                            }
                        }
                        // El grupo se corta en el primer directorio de la cola: la operación solo lleva archivos.
                        let sizes: Vec<u64> = queue
                            .iter()
                            .take_while(|&&i| slots[i].as_ref().is_some_and(|s| !s.0.is_dir()))
                            .map(|&i| slots[i].as_ref().map_or(0, |s| s.1.size))
                            .collect();
                        let room = vol
                            .as_deref()
                            .map_or(BinRoom::UNKNOWN, |r| shell.bin_room(r));
                        let n = match take_op(&sizes, room) {
                            Take::NoRoom => {
                                // No hay sitio ahora: enviarlo haría que la shell lo elimine o expulse lo que ya hay en la
                                // Papelera. No se toca.
                                queue.pop_front();
                                if let Some((it, _)) = slots[first].take() {
                                    emit(it, None, 0, Outcome::NotRecycled);
                                    results.insert(
                                        it.id,
                                        (
                                            ItemStatus::NotRecycled(RecycleFail::NoRoom),
                                            StatsSnapshot::default(),
                                        ),
                                    );
                                }
                                continue;
                            }
                            Take::Send(n) => n,
                        };
                        // Los candidatos se miden DE NUEVO ahora, por su handle de guarda, y la medida de cada archivo
                        // viaja con él: `PreDeleteItem` lo vuelve a leer justo antes de que la shell lo toque y veta lo que
                        // creció (A2-B1; sin eso, lo medido al principio del lote podía crecer mientras se medían los demás).
                        let mut staged: Vec<(usize, &PlanItem, Pending)> = Vec::new();
                        let candidates: Vec<usize> = queue.drain(..n).collect();
                        for i in candidates {
                            let Some((it, mut p)) = slots[i].take() else {
                                continue;
                            };
                            let mut st = StatsSnapshot::default();
                            match self.fresh_size(it, &mut p, &mut st, cancel) {
                                Ok(()) => staged.push((i, it, p)),
                                Err((status, o)) => {
                                    emit(it, None, 0, o);
                                    results.insert(it.id, (status, st));
                                }
                            }
                        }
                        if staged.is_empty() {
                            continue;
                        }
                        // ...y se vuelve a comprobar el presupuesto con los tamaños y la ocupación de AHORA: lo que ya no
                        // cabe en la operación vuelve al principio de la cola (se medirá otra vez cuando le toque).
                        let room_now = room.worst(
                            vol.as_deref()
                                .map_or(BinRoom::UNKNOWN, |r| shell.bin_room(r)),
                        );
                        let fresh_sizes: Vec<u64> = staged.iter().map(|s| s.2.size).collect();
                        let keep = match take_op(&fresh_sizes, room_now) {
                            Take::NoRoom => {
                                let (_, it, _guard) = staged.remove(0);
                                emit(it, None, 0, Outcome::NotRecycled);
                                results.insert(
                                    it.id,
                                    (
                                        ItemStatus::NotRecycled(RecycleFail::NoRoom),
                                        StatsSnapshot::default(),
                                    ),
                                );
                                0
                            }
                            Take::Send(k) => k.min(staged.len()),
                        };
                        for (i, it, p) in staged.drain(keep..).rev() {
                            slots[i] = Some((it, p));
                            queue.push_front(i);
                        }
                        staged.into_iter().map(|(_, it, p)| (it, p)).collect()
                    };
                    if entries.is_empty() {
                        continue;
                    }
                    let paths: Vec<PathBuf> = entries.iter().map(|(_, p)| p.path.clone()).collect();
                    // La identidad aprobada viaja con la ruta: el receptor la comprueba en `PreDeleteItem`.
                    // Un ARCHIVO lleva además su handle de guarda y el tamaño contado: `PreDeleteItem` lo vuelve a medir (A2-B1).
                    let send: Vec<recycle::SendItem> = entries
                        .iter()
                        .map(|(it, p)| recycle::SendItem {
                            path: p.path.clone(),
                            identity: p.identity,
                            // A3-M1: un DIRECTORIO lleva la vigilancia de cambios abierta antes de medirlo.
                            watch: p.watch.clone(),
                            size_guard: (!it.is_dir()).then(|| recycle::SizeGuard {
                                handle: p.held.clone(),
                                budget: p.size,
                            }),
                        })
                        .collect();
                    #[cfg(test)]
                    for p in &paths {
                        crate::engine::test_hooks::before_shell(p);
                    }
                    // A3-M1 (S1 de la auditoría): también ANTES de escribir la línea de intención. Una carpeta que ya cambió no
                    // deja un `recycle-begin` sin entrega; se vuelve a mirar justo después de escribirla (lo último antes de la
                    // shell), porque la escritura tarda y cae dentro de la ventana.
                    if watch_changed(&entries) {
                        reject_changed(&entries, &mut results);
                        drop(entries);
                        continue;
                    }
                    // Antes de entregarlas a la shell se anota la INTENCIÓN (la lista de rutas del lote) y se vuelca: tras un
                    // cierre forzoso se sabe qué pudo moverse. Si no se puede dejar escrita, NO se entrega nada a la shell.
                    if let Some(l) = &opts.log
                        && l.write_intent("recycle-begin", &paths).is_err()
                    {
                        for (it, _) in &entries {
                            emit(it, None, 0, Outcome::NotRecycled);
                            results.insert(
                                it.id,
                                (
                                    ItemStatus::Root(RootStatus::Cancelled),
                                    StatsSnapshot::default(),
                                ),
                            );
                        }
                        drop(entries);
                        continue;
                    }
                    // A3-M1: lo último antes de la shell. Si algo cambió dentro del directorio desde que se midió (la vigilancia
                    // lo vio), la medida no es cierta: no se entrega ("cambió desde la confirmación"). Un directorio va solo en
                    // su operación; en la shell, `PreDeleteItem` vuelve a consultar la misma vigilancia (y, si pasa, la para).
                    if watch_changed(&entries) {
                        reject_changed(&entries, &mut results);
                        drop(entries);
                        continue;
                    }
                    let outcome = shell.recycle(&send);
                    // Un error de la shell o de COM no se pierde: consta en el registro local y en el resumen. El `E_ABORT`
                    // que provoca el propio receptor al cortar el lote NO es un error de la shell: el motivo del corte va en
                    // `stop_reason`.
                    if let Some(e) = &outcome.error {
                        let own_abort = outcome.sink_aborted
                            && e.hresult == windows::Win32::Foundation::E_ABORT.0;
                        if !own_abort {
                            report.shell_error.get_or_insert(e.hresult);
                        } else if let Some(f) = outcome.events.iter().find(|ev| {
                            !ev.succeeded() && ev.hr != windows::Win32::Foundation::E_ABORT.0
                        }) {
                            // A2-M1: el receptor corta tras un resultado de FALLO y el `E_ABORT` es suyo; el error de la
                            // shell es el de ese resultado, y no se pierde. Se ignora el `E_ABORT` del propio receptor: tras
                            // un veto la shell avisa del vetado con `PostDeleteItem(E_ABORT)` (medido) y eso no es un error
                            // de Windows.
                            report.shell_error.get_or_insert(f.hr);
                        }
                        if let Some(l) = &opts.log {
                            l.write_note(
                                "recycle-error",
                                &format!("hresult 0x{:08X}: {}", e.hresult as u32, e.detail),
                            );
                            l.flush();
                        }
                    }
                    // GM-M5: cada evento y cada veto se asigna al elemento de la operación por su nombre EXACTO; la clave (que
                    // pliega mayúsculas) solo si no hay ambigüedad. Con dos nombres que solo difieren en mayúsculas en un
                    // directorio sensible a ellas, una tabla por clave los confundía y el segundo heredaba el resultado (o el
                    // veto) del primero.
                    let names: Vec<&std::ffi::OsStr> =
                        entries.iter().map(|(_, p)| p.path.as_os_str()).collect();
                    let mut event_of: Vec<Option<&recycle::DeleteEvent>> = vec![None; names.len()];
                    for e in &outcome.events {
                        if let Some(i) = crate::paths::owner_index(&names, &e.item) {
                            event_of[i] = Some(e);
                        }
                    }
                    let mut veto_of: Vec<bool> = vec![false; names.len()];
                    for v in &outcome.vetoed {
                        if let Some(i) = crate::paths::owner_index(&names, v.id()) {
                            veto_of[i] = true;
                        }
                    }
                    // 3. Juzgar cada elemento por lo que notificó la shell (con el handle de guarda aún abierto).
                    for (idx, (it, p)) in entries.iter().enumerate() {
                        let ev = event_of[idx];
                        let (mut status, mut o) = if veto_of[idx] {
                            // `PreDeleteItem` vio otro objeto en la ruta aprobada y lo vetó: no se envió (sigue ahí, el
                            // sustituto) y consta como "cambió desde la confirmación".
                            (ItemStatus::Root(RootStatus::Changed), Outcome::Changed)
                        } else {
                            judge(p, ev, shell, outcome.any_aborted, outcome.sink_aborted)
                        };
                        // R6 + A-M1: "no se envió y sigue donde estaba" de un DIRECTORIO solo vale si tampoco se tocó lo
                        // que contiene: el objeto carpeta intacto no dice nada de sus hijos. Vale para CUALQUIER resultado
                        // de "no enviado" tras pasar por la shell (la shell dijo "eliminado" sin tocar, falló, no lo
                        // procesó...): un fallo puede llegar después de perder un hijo. Se vuelve a medir (mismo recorrido
                        // relativo, COMPLETO) y la huella del contenido se compara con la de justo antes del envío, con TRES
                        // resultados (A2-m1): igual, `Intact`; distinta en una medida completa, `Changed` = borrado
                        // definitivo, aunque sea parcial; y si la medida no se pudo hacer entera (una subcarpeta sin leer, un
                        // fallo, una cancelación) es `Unknown`: no se afirma ninguna pérdida, es `Unverified` y el envío se
                        // detiene. La comprobación NO se interrumpe por la cancelación: debe terminar para poder decir la
                        // verdad del informe.
                        if matches!(status, ItemStatus::NotRecycled(_)) && it.is_dir() {
                            match self.dir_check(it, p, &verify_cancel) {
                                DirCheck::Intact => {}
                                DirCheck::Changed => {
                                    status = ItemStatus::DeletedUnexpectedly;
                                    o = Outcome::DeletedUnexpectedly;
                                }
                                DirCheck::Unknown => {
                                    status = ItemStatus::Unverified;
                                    o = Outcome::Unverified;
                                }
                            }
                        }
                        let mut st = StatsSnapshot::default();
                        match status {
                            ItemStatus::Recycled => {
                                st.bytes = p.disk;
                                st.logical = p.logical;
                                st.files = it.estimate.files;
                                st.dirs = it.estimate.dirs;
                            }
                            ItemStatus::NotRecycled(RecycleFail::InUse) => st.in_use += 1,
                            ItemStatus::NotRecycled(RecycleFail::Denied) => st.denied += 1,
                            ItemStatus::NotRecycled(_) => st.other_errors += 1,
                            ItemStatus::Root(RootStatus::Changed) => st.changed += 1,
                            _ => {}
                        }
                        // CORTE DE SEGURIDAD: tras un borrado inesperado o algo que no se pudo verificar no se envía nada más.
                        if matches!(
                            status,
                            ItemStatus::DeletedUnexpectedly | ItemStatus::Unverified
                        ) {
                            stopped = true;
                            report.recycle_stopped = true;
                            report.stop_reason.get_or_insert(
                                if status == ItemStatus::DeletedUnexpectedly {
                                    StopReason::DeletedUnexpectedly
                                } else {
                                    StopReason::Unverified
                                },
                            );
                        }
                        // La shell dijo "eliminado" pero el elemento sigue ahí (ver `judge`): el envío se detiene igual.
                        if matches!(status, ItemStatus::NotRecycled(RecycleFail::Shell(_)))
                            && ev.is_some_and(|e| {
                                e.succeeded() && e.state == recycle::BinState::Deleted
                            })
                        {
                            stopped = true;
                            report.recycle_stopped = true;
                            report.stop_reason.get_or_insert(StopReason::ShellDeclined);
                        }
                        emit(it, Some(p.identity), p.links, o);
                        if let Some(l) = &opts.log {
                            l.write_note(
                                "recycle",
                                &format!(
                                    "{} -> {:?} (hresult 0x{:08X})",
                                    p.path.display(),
                                    status,
                                    ev.map_or(0, |e| e.hr as u32)
                                ),
                            );
                            l.flush();
                        }
                        results.insert(it.id, (status, st));
                    }
                    // Un veto (o cualquier otro corte del receptor) detiene también lo que queda por enviar.
                    if outcome.sink_aborted && !report.recycle_stopped {
                        // H5 (decisión conservadora, documentada en PLAN): un solo fallo de un ARCHIVO con pendientes en su lote
                        // detiene TODO el envío, aunque en un archivo no pueda haber pérdida parcial. La alternativa (reencolar
                        // los pendientes en otra operación) se descartó por simplicidad: el caso es raro, porque tras preparar
                        // el lote la guarda impide bloquear un archivo desde fuera.
                        // A2-M1: si el corte lo causó solo el FALLO de un resultado (ni veto ni pérdida: esos ya pararon el
                        // envío arriba), cancela lo pendiente del MISMO lote (queda como `Stopped`, intacto y comprobado) y
                        // solo entonces se detiene todo lo demás. Si el fallo fue lo último del lote (siempre, para un
                        // directorio, que va solo) no había nada pendiente y las operaciones siguientes siguen.
                        let failure_only = outcome.vetoed.is_empty()
                            && outcome.events.iter().any(|e| !e.succeeded());
                        let cancelled_pending = event_of.iter().any(Option::is_none);
                        if !failure_only || cancelled_pending {
                            stopped = true;
                            report.recycle_stopped = true;
                            report.stop_reason.get_or_insert(if failure_only {
                                StopReason::ShellDeclined
                            } else {
                                StopReason::Changed
                            });
                        }
                    }
                    // Aquí se sueltan los handles de guarda de la operación.
                    drop(entries);
                }
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
            let removed = if status == ItemStatus::Recycled {
                vec![Removed {
                    path: it.path.clone(),
                    is_dir: it.is_dir(),
                    removed: true,
                    bytes: stats.bytes,
                    logical: stats.logical,
                    files: it.estimate.files,
                }]
            } else {
                Vec::new()
            };
            report.items.push(ItemReport {
                id: it.id,
                category: it.category,
                path: it.path.clone(),
                destination: Destination::Recycle,
                status,
                stats,
                removed,
            });
            progress.items_done.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Comprobación previa de un elemento que va a la Papelera:
    /// * lo abre en exclusiva (un archivo en uso falla con `SHARING_VIOLATION`; un directorio se abre compartiendo, y si
    ///   tiene algo abierto dentro la propia shell falla al moverlo), verifica su identidad y su ruta resuelta (local, la
    ///   aprobada, permitida);
    /// * y deja abierto un handle de GUARDA (`DELETE | READ_ATTRIBUTES`, compartiéndolo todo) que se conserva durante toda
    ///   la operación de la shell y hasta después de juzgar el elemento. Mientras está abierto, Windows NO deja renombrar
    ///   el padre ni el abuelo del elemento (error 5): no se puede cambiar un ancestro por un junction a otra carpeta en
    ///   la ventana entre esta comprobación y que la shell abra el elemento. La shell recicla igual (el handle comparte
    ///   `DELETE`: la guarda NO impide renombrar el propio elemento ni poner otro con su nombre; eso lo vigila
    ///   `PreDeleteItem`, que compara la identidad justo antes de que la shell actúe). Además permite comprobar al final
    ///   que el objeto aprobado es el que acabó en la Papelera.
    fn prepare_recycle(&self, it: &PlanItem, st: &mut StatsSnapshot) -> Prepared {
        let UnitSpec::Entry {
            path,
            approved,
            approved_tag,
            approved_links,
            is_dir,
        } = &it.spec
        else {
            return Prepared::Rejected(ItemStatus::Root(RootStatus::Changed), Outcome::Changed);
        };
        #[cfg(test)]
        crate::engine::test_hooks::before_open(path);
        let opened = if *is_dir {
            sys::open_path_for_rename(path)
        } else {
            sys::open_path_exclusive(path)
        };
        let h = match opened {
            Ok(h) => h,
            Err(c) => return map_open_error(c, st),
        };
        let m = match sys::meta(&h) {
            Ok(m) => m,
            Err(c) => return map_open_error(c, st),
        };
        let reparse_ok = !m.is_reparse()
            || (!m.is_dir && m.reparse_tag == *approved_tag && sys::is_cloud_tag(m.reparse_tag));
        // Misma identidad pero otro número de enlaces: alguien añadió o quitó nombres desde la confirmación.
        if m.identity != *approved
            || m.is_dir != *is_dir
            || !reparse_ok
            || m.links != *approved_links
        {
            st.changed += 1;
            return Prepared::Rejected(ItemStatus::Root(RootStatus::Changed), Outcome::Changed);
        }
        // M-1: una unidad con antigüedad mínima (informes, volcados) solo se envía si el objeto SIGUE siendo así de antiguo. La
        // identidad y el tamaño no bastan: un volcado "tocado" tras la confirmación conserva ambos.
        if too_young(it, m.newest_filetime) {
            st.changed += 1;
            return Prepared::Rejected(ItemStatus::Root(RootStatus::Changed), Outcome::Changed);
        }
        let final_path = match sys::final_path(&h) {
            Ok(p) => p,
            Err(c) => return map_open_error(c, st),
        };
        if !volume::is_local_path(&final_path) {
            st.protected += 1;
            return Prepared::Rejected(ItemStatus::Root(RootStatus::NotLocal), Outcome::Protected);
        }
        let key = PathKey::new(&final_path);
        if PathKey::new(path) != key || self.protection.target_verdict(&key) != Verdict::Allowed {
            st.protected += 1;
            return Prepared::Rejected(ItemStatus::Root(RootStatus::Protected), Outcome::Protected);
        }
        // El handle de guarda. Un directorio ya se abrió compartiéndolo todo con `DELETE`: ese mismo es la guarda. Un
        // archivo se abrió en exclusiva (para detectar "en uso"): se suelta y se abre la guarda compartiendo, y se
        // vuelve a comprobar que es el mismo objeto en la misma ruta.
        let held = if *is_dir {
            h
        } else {
            drop(h);
            let g = match sys::open_path_for_rename(&final_path) {
                Ok(g) => g,
                Err(c) => return map_open_error(c, st),
            };
            let same = sys::meta(&g).is_ok_and(|m2| {
                m2.identity == *approved
                    && !m2.is_dir
                    && m2.reparse_tag == m.reparse_tag
                    && m2.links == *approved_links
            }) && sys::final_path(&g).is_ok_and(|p| PathKey::new(&p) == key);
            if !same {
                st.changed += 1;
                return Prepared::Rejected(ItemStatus::Root(RootStatus::Changed), Outcome::Changed);
            }
            g
        };
        // Lo que ocupa de verdad ahora frente a lo medido al aprobar. Un directorio NO se vuelve a medir aquí (un trozo son
        // hasta 32 elementos y entre este punto y su turno pasa tiempo): se mide JUSTO ANTES de entregarlo, solo en su
        // operación, en `fresh_size` (H6.2, A2-B1). Mientras tanto vale lo aprobado.
        let estimate = it.estimate.recycle_charge();
        // Un archivo: lo que ocupa de verdad ahora frente a lo medido al aprobar, el mayor manda (provisional: se vuelve a
        // medir por la guarda JUSTO ANTES de la operación, en `fresh_size`).
        let (size, last) = if m.is_dir {
            (estimate, None)
        } else {
            (
                estimate.max(m.alloc).max(m.logical),
                Some(m.alloc.max(m.logical)),
            )
        };
        Prepared::Ready(Box::new(Pending {
            path: final_path,
            identity: m.identity,
            links: m.links,
            held: Arc::new(held),
            size,
            last,
            // Lo que se informará si llega a la Papelera: lo aprobado hasta que `fresh_size` lo mida de nuevo (GM-m6).
            disk: if m.is_dir || m.links > 1 {
                it.estimate.bytes
            } else {
                m.alloc
            },
            logical: if m.is_dir {
                it.estimate.logical
            } else {
                m.logical
            },
            snapshot: None,
            watch: None,
        }))
    }

    /// R6 + A2-m1: ¿sigue el directorio igual que justo antes del envío? Tres estados:
    /// * `Intact`: una medida COMPLETA con la misma huella (tamaños, número de entradas e identidades);
    /// * `Changed`: una medida COMPLETA que difiere en cualquier sentido (se perdió o se cambió algo);
    /// * `Unknown`: la medida no se pudo hacer entera (A-B2: una subcarpeta sin listar, una entrada que cambió o desapareció
    ///   durante el recorrido, un fallo al abrir, una cancelación) o no hay medida anterior con la que comparar. No demuestra
    ///   nada, ni que esté intacto ni que se perdiera algo.
    fn dir_check(&self, it: &PlanItem, p: &Pending, cancel: &Arc<AtomicBool>) -> DirCheck {
        match (self.remeasure(it, cancel), p.snapshot) {
            (Remeasure::Now(after), Some(before)) => {
                if snap_of(&after) == before {
                    DirCheck::Intact
                } else {
                    DirCheck::Changed
                }
            }
            _ => DirCheck::Unknown,
        }
    }

    /// B2 + H6.2 + A-B1 + A2-B1: lo que ocupa un elemento pudo CRECER desde la aprobación, y también mientras esperaba su
    /// turno (una operación de la shell puede tardar). Se vuelve a medir JUSTO ANTES de entregarlo (un directorio, solo en su
    /// operación; un archivo, en su lote, donde además `PreDeleteItem` lo mide otra vez), no al preparar el trozo entero y no
    /// una sola vez:
    /// * un ARCHIVO, por su handle de guarda (`FileStandardInfo`: tamaño en disco y lógico), sin volver a abrir la ruta, y
    ///   comprobando de paso que sigue siendo el mismo objeto (identidad, enlaces, no pendiente de borrar);
    /// * un DIRECTORIO, con el mismo recorrido (relativo, sin entrar en reparse points) y solo si la medida es COMPLETA
    ///   (A-B2).
    ///
    /// Si creció (frente a lo aprobado o frente a la última medida), no se envía: el reparto se basa en un tamaño que ya no
    /// es cierto y `Err` lleva el estado "cambió desde la confirmación"; si encogió, manda el tamaño nuevo. Quien llama
    /// vuelve a comprobar el presupuesto de la Papelera (`take_op`) con estos tamaños nuevos.
    fn fresh_size(
        &self,
        it: &PlanItem,
        p: &mut Pending,
        st: &mut StatsSnapshot,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(), (ItemStatus, Outcome)> {
        let changed = |st: &mut StatsSnapshot| {
            st.changed += 1;
            Err((ItemStatus::Root(RootStatus::Changed), Outcome::Changed))
        };
        if !it.is_dir() {
            // Por el handle de guarda: el objeto que se aprobó, no lo que haya ahora en la ruta.
            let Ok(m) = sys::meta(&p.held) else {
                return changed(st);
            };
            if m.identity != p.identity || m.is_dir || m.delete_pending || m.links != p.links {
                return changed(st);
            }
            // M-1: otra vez la antigüedad mínima, ahora justo antes de la operación.
            if too_young(it, m.newest_filetime) {
                return changed(st);
            }
            let fresh = m.alloc.max(m.logical);
            // Si creció frente a lo APROBADO o frente a la última medida real (la de `prepare_recycle` y las posteriores),
            // no se envía: el reparto de la operación se decidió con un tamaño que ya no es cierto.
            let approved = it.estimate.recycle_charge();
            if fresh > approved || p.last.is_some_and(|l| fresh > l) {
                return changed(st);
            }
            p.size = fresh;
            p.last = Some(fresh);
            // GM-m6: lo que se informa si llega a la Papelera es lo de AHORA (un archivo que encogió).
            p.disk = if m.links > 1 { 0 } else { m.alloc };
            p.logical = m.logical;
            return Ok(());
        }
        // M-1: la fecha de la propia carpeta (lo de dentro lo comprueba la huella: un archivo reciente cambia la clase de su
        // entrada). Sin leerla, o con otro objeto, no se envía.
        match sys::meta(&p.held) {
            Ok(m) if !too_young(it, m.newest_filetime) => {}
            _ => return changed(st),
        }
        // A3-M1: la vigilancia de cambios se abre ANTES del recorrido (lo que cambie antes no se vería) y vive hasta el
        // `PreDeleteItem` de su operación: una carpeta puede crecer MIENTRAS se mide (el recorrido ya contó el primer archivo
        // cuando este se agranda) y el recorrido no se entera. Segundo handle del directorio con identidad verificada; si
        // no se puede abrir o arrancar, la carpeta NO se envía (falla cerrado).
        let watch = match sys::DirWatch::start(&p.path, p.identity) {
            Ok(w) => w,
            Err(sys::WatchError::Changed) => return changed(st),
            Err(sys::WatchError::Os(c)) => {
                st.other_errors += 1;
                return Err((ItemStatus::Root(RootStatus::Failed(c)), Outcome::Failed(c)));
            }
        };
        match self.remeasure(it, cancel) {
            Remeasure::Now(_) if watch.changed() => changed(st),
            Remeasure::Now(fresh) => {
                // GM-B3: un directorio solo se envía con el MISMO contenido que se aprobó: huella (identidad, tamaños,
                // enlaces y clase de cada entrada, independiente del orden) y recuentos idénticos, y sin MÁS protegidos,
                // reparse points ni archivos de la nube. Antes solo se miraba que no hubiese crecido: si un archivo
                // desaparecía y aparecía otro más pequeño, la carpeta se enviaba con contenido que nadie aprobó. Una carpeta
                // cuyo contenido cambió en cualquier sentido (también si encogió) vuelve a la confirmación.
                let est = &it.estimate;
                let same = snap_of(&fresh) == snap_of(est)
                    && fresh.reparse <= est.reparse
                    && fresh.cloud <= est.cloud
                    && fresh.protected <= est.protected;
                let size = fresh.recycle_charge();
                if !same || p.last.is_some_and(|l| size > l) {
                    return changed(st);
                }
                p.size = size;
                p.last = Some(size);
                p.disk = fresh.bytes;
                p.logical = fresh.logical;
                p.snapshot = Some(snap_of(&fresh));
                p.watch = Some(Arc::new(watch));
                Ok(())
            }
            Remeasure::Cancelled => Err((
                ItemStatus::Root(RootStatus::Cancelled),
                Outcome::NotRecycled,
            )),
            Remeasure::Incomplete | Remeasure::Failed => changed(st),
        }
    }
}

/// Qué se sabe del contenido de un directorio que la shell no envió (ver [`Cleaner::dir_check`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirCheck {
    /// Medida completa e igual a la de antes del envío.
    Intact,
    /// Medida completa y distinta: algo se perdió o cambió.
    Changed,
    /// La medida no se pudo hacer entera: no se puede afirmar nada.
    Unknown,
}

/// Resultado de volver a medir un directorio justo antes de enviarlo.
enum Remeasure {
    /// La medida de un recorrido COMPLETO (ver [`StatsSnapshot::is_complete`]).
    Now(StatsSnapshot),
    Cancelled,
    /// El recorrido terminó pero NO vio todo (una subcarpeta sin listar, una entrada que cambió o desapareció...): su resultado
    /// no vale. Se trata como "cambió desde la confirmación" al enviar y como "NO intacto" al juzgar (R6).
    Incomplete,
    /// No se pudo medir (cambió, ya no existe, protegido...): se trata como "cambió desde la confirmación".
    Failed,
}

impl Cleaner {
    /// Vuelve a medir el directorio aprobado de `it` con el mismo recorrido de la aprobación (modo `Measure`: apertura
    /// relativa, identidad verificada y sin entrar en reparse points).
    fn remeasure(&self, it: &PlanItem, cancel: &Arc<AtomicBool>) -> Remeasure {
        let progress = Arc::new(AtomicU64::new(0));
        let unit = Unit::new(
            it.id,
            it.spec.clone(),
            self.env(Mode::Measure, it.min_age, cancel, &progress, None),
        );
        engine::run(std::slice::from_ref(&unit), self.threads);
        // Un recorrido interrumpido por la cancelación (o por un pánico de un hilo, que también la activa) no vio todo.
        if cancel.load(Ordering::Relaxed) {
            return Remeasure::Cancelled;
        }
        match unit.status() {
            // A-B2: `Done` solo dice que la RAÍZ se abrió y se listó. La medida vale únicamente si el recorrido entero fue
            // completo: una subcarpeta que no se pudo listar, una entrada que cambió o desapareció, un error de apertura...
            // dejan contenido sin contar.
            Some(RootStatus::Done) => {
                let s = unit.stats.snapshot();
                if s.is_complete() {
                    Remeasure::Now(s)
                } else {
                    Remeasure::Incomplete
                }
            }
            Some(RootStatus::Cancelled) => Remeasure::Cancelled,
            _ => Remeasure::Failed,
        }
    }
}

/// Elementos por trozo (y por vuelta de la cancelación).
const RECYCLE_CHUNK: usize = 32;

/// Un elemento comprobado y listo para entregar a la shell por su ruta original.
struct Pending {
    path: PathBuf,
    identity: FileIdentity,
    links: u32,
    /// Handle de guarda (ver `prepare_recycle`): abierto hasta después de juzgar el elemento. Compartido (`Arc`) porque el
    /// receptor de la shell lo usa para volver a medir un archivo en `PreDeleteItem` (A2-B1).
    held: Arc<sys::Handle>,
    /// Lo que se cuenta para repartir en operaciones: provisional al preparar (el mayor entre lo aprobado y lo que ocupa
    /// ahora) y, tras cada [`Cleaner::fresh_size`], lo que ocupa AHORA.
    size: u64,
    /// La última medida real (un archivo, por su handle de guarda; un directorio, por el recorrido) o `None` si aún no
    /// hay ninguna. Si lo que se mide ahora es MÁS que esto, el elemento creció mientras esperaba su turno: no se envía.
    last: Option<u64>,
    /// La medida del directorio hecha justo antes de entregarlo a la shell (tamaño y número de entradas): lo que
    /// `dir_check` compara después si la shell dice "eliminado" sin dar elemento en la Papelera (R6).
    snapshot: Option<DirSnap>,
    /// Lo que se informa si llega a la Papelera: bytes en disco (sin enlaces compartidos) y tamaño lógico de AHORA (GM-m6: un
    /// archivo que encogió desde la aprobación no se cuenta con el tamaño aprobado).
    disk: u64,
    /// A3-M1: solo un DIRECTORIO. La vigilancia de cambios abierta ANTES del recorrido de medida y viva hasta el
    /// `PreDeleteItem` de su operación (o hasta que se suelte el elemento): cualquier cambio dentro invalida la medida.
    watch: Option<Arc<sys::DirWatch>>,
    logical: u64,
}

/// Lo que se compara de un directorio antes y después: bytes en disco, tamaño lógico, archivos, directorios, lo compartido y
/// la HUELLA del contenido (independiente del orden: identidad, tamaños y enlaces de cada entrada). Los totales solos no
/// bastan: quitar un hijo y crear otro del mismo tamaño los deja iguales (A-M1).
type DirSnap = (u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64);

fn snap_of(s: &StatsSnapshot) -> DirSnap {
    (
        s.bytes,
        s.logical,
        s.files,
        s.dirs,
        s.shared_files,
        s.shared_bytes,
        s.fp_sum,
        s.fp_xor,
        // La ruta más larga de dentro (GM-M3): renombrar algo a un nombre más largo cambia lo que cabe en la Papelera.
        s.max_path,
        // Lo que ocupan los reparse points de dentro (GM-B1).
        s.reparse_bytes,
        s.reparse_logical,
    )
}

/// Espacio de la Papelera de un volumen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BinRoom {
    /// Tamaño máximo configurado (`MaxCapacity`); `None` si el registro no lo da.
    pub max: Option<u64>,
    /// Lo que ocupa ahora la Papelera en ese volumen; `None` si no se pudo leer.
    pub used: Option<u64>,
}

impl BinRoom {
    pub const UNKNOWN: BinRoom = BinRoom {
        max: None,
        used: None,
    };

    /// La peor de dos lecturas del mismo volumen (A3-M2): la MAYOR ocupación y la MENOR capacidad. Si falta cualquiera de los
    /// datos en cualquiera de las dos lecturas, falta en el resultado y `take_op` falla cerrado (`NoRoom`).
    pub fn worst(self, other: BinRoom) -> BinRoom {
        let pick = |a: Option<u64>, b: Option<u64>, f: fn(u64, u64) -> u64| match (a, b) {
            (Some(x), Some(y)) => Some(f(x, y)),
            _ => None,
        };
        BinRoom {
            max: pick(self.max, other.max, u64::min),
            used: pick(self.used, other.used, u64::max),
        }
    }
}

/// Lo que el ejecutor necesita de la shell para enviar a la Papelera. La implementación real usa `IFileOperation`; las
/// pruebas ponen una simulada para probar el reparto en operaciones y el corte de seguridad sin que una operación real
/// pueda eliminar o expulsar nada ajeno.
pub(crate) trait RecycleShell: Send + Sync {
    /// Envía los elementos (ruta original + identidad aprobada) en UNA operación y dice qué notificó la shell de cada
    /// uno. El receptor de eventos veta lo que ya no es el objeto aprobado.
    fn recycle(&self, items: &[recycle::SendItem]) -> recycle::BatchOutcome;
    /// Purga (borra definitivamente) los elementos de la Papelera enumerados al confirmar. Por defecto, la shell real; las
    /// pruebas ponen una simulada para probar el reparto y lo que se excluye sin tocar la Papelera de nadie (M2).
    fn purge(
        &self,
        items: &[recycle::BinItem],
        cancel: &AtomicBool,
        obs: &mut dyn FnMut(recycle::PurgeStep<'_>),
    ) -> Result<recycle::BatchOutcome, recycle::ShellError> {
        recycle::purge_items_observed(items, cancel, obs)
    }
    /// Capacidad y ocupación de la Papelera del volumen cuya raíz es `root`.
    fn bin_room(&self, root: &Path) -> BinRoom;
    /// ¿Cabe un elemento de `bytes` con ruta `path` en la Papelera de su volumen? Se decide al aprobar y se vuelve a
    /// decidir al ejecutar (la real consulta el registro y las directivas; las pruebas con shell simulada no dependen del
    /// equipo).
    fn fit(&self, path: &Path, bytes: u64) -> Fit;
    /// Como [`RecycleShell::fit`], sabiendo cuánto mide la ruta más larga de lo que hay dentro del elemento (unidades UTF-16;
    /// de la medida). Por defecto lo ignora (las shells simuladas no dependen de la longitud de las rutas).
    fn fit_deep(&self, path: &Path, bytes: u64, _deepest: usize) -> Fit {
        self.fit(path, bytes)
    }
    /// ¿Está `p` dentro de una Papelera (`<unidad>:\$Recycle.Bin\...`)?
    fn is_bin_path(&self, p: &Path) -> bool;
}

/// La shell de verdad.
pub(crate) struct RealShell;

impl RecycleShell for RealShell {
    fn recycle(&self, items: &[recycle::SendItem]) -> recycle::BatchOutcome {
        match recycle::recycle_paths(items, RecycleMode::Normal) {
            Ok(o) => o,
            // Un fallo al arrancar COM o la operación no se traga: sin eventos, cada elemento queda como no procesado
            // (o no verificado), y el error consta en el resultado.
            Err(e) => recycle::BatchOutcome {
                error: Some(e),
                ..recycle::BatchOutcome::default()
            },
        }
    }
    fn bin_room(&self, root: &Path) -> BinRoom {
        room_from(volume::bin_limit(root), recycle::bin_size_on(root).ok())
    }
    fn fit(&self, path: &Path, bytes: u64) -> Fit {
        fit_for(path, bytes)
    }
    fn fit_deep(&self, path: &Path, bytes: u64, deepest: usize) -> Fit {
        fit_for_deep(path, bytes, deepest)
    }
    fn is_bin_path(&self, p: &Path) -> bool {
        recycle::is_recycle_bin_path(p)
    }
}

/// La capacidad de la Papelera de un volumen a efectos del reparto: SOLO la configurada (`MaxCapacity`). Sin Papelera,
/// desactivada, indeterminada o sin valor en el registro (la shell usa un porcentaje por defecto que no está
/// documentado) no hay capacidad que garantizar: `max = None` y [`take_op`] falla cerrado (`NoRoom`). Ya no se asume
/// ningún porcentaje del volumen (B2).
pub(crate) fn room_from(limit: volume::BinLimit, used: Option<u64>) -> BinRoom {
    let max = match limit {
        volume::BinLimit::Max(m) => Some(m),
        _ => None,
    };
    BinRoom { max, used }
}

/// Qué hacer con el principio de la cola de un volumen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Take {
    /// Enviar los `n` primeros en UNA operación.
    Send(usize),
    /// No hay sitio para el primero: no se envía.
    NoRoom,
}

/// Reparte en operaciones: cada operación, como mucho, al 90 % de la capacidad de la Papelera MENOS lo que ya ocupa.
/// * FALLA CERRADO (B2): sin capacidad conocida (`max`) o sin ocupación conocida (`used`) no se puede garantizar que
///   quepa y la shell, si no cabe, elimina de verdad: `NoRoom`, nunca `Send`.
/// * Un elemento que por sí solo pasa de un 10 % de la capacidad va SOLO.
/// * Si el primero no cabe ni solo en el espacio que queda, `NoRoom`: enviarlo haría que la shell lo elimine o expulse
///   elementos que ya estaban en la Papelera.
pub(crate) fn take_op(sizes: &[u64], room: BinRoom) -> Take {
    let Some(&first) = sizes.first() else {
        return Take::Send(0);
    };
    let (Some(max), Some(used)) = (room.max, room.used) else {
        return Take::NoRoom;
    };
    let cap = bin_cap(max);
    let budget = cap.saturating_sub(used);
    if first > budget {
        return Take::NoRoom;
    }
    let big = bin_big(max);
    if first > big {
        return Take::Send(1);
    }
    let mut n = 1;
    let mut sum = first;
    while n < sizes.len() && sizes[n] <= big && sum.saturating_add(sizes[n]) <= budget {
        sum += sizes[n];
        n += 1;
    }
    Take::Send(n)
}

/// Qué pasó con un elemento según la shell. La comparación es por IDENTIDAD (volumen + FileId), nunca por nombre:
/// da igual `.lnk`, `.url`, extensiones conocidas o `HideFileExt`, y no se enumera la Papelera. Pero la identidad sola no
/// basta (otro nombre del mismo objeto, p. ej. un enlace duro, la comparte): `Recycled` exige además que
/// * el NOMBRE aprobado ya no exista en su ruta original,
/// * el handle de guarda, que sigue al objeto aprobado, esté ahora DENTRO de una Papelera, y
/// * el objeto conserve su número de enlaces (un renombrado a la Papelera no lo cambia).
///
/// `aborted`: la shell dijo que abortó alguna operación (sin decir cuál): un elemento sin aviso se cuenta entonces como
/// abortado. `sink_aborted`: el receptor cortó el lote (M2): un elemento sin aviso que sigue en su sitio quedó sin
/// procesar por el corte (`Stopped`).
fn judge(
    p: &Pending,
    ev: Option<&recycle::DeleteEvent>,
    shell: &dyn RecycleShell,
    aborted: bool,
    sink_aborted: bool,
) -> (ItemStatus, Outcome) {
    use recycle::BinState;
    let Some(ev) = ev else {
        // La shell no llegó a procesarlo. "No se envió" solo se afirma si el OBJETO aprobado sigue intacto en su sitio (A-M1);
        // si su guarda está pendiente de borrar, la shell lo eliminó sin avisar; si no se puede saber, no se afirma nada.
        return match in_place(p) {
            InPlace::Intact => {
                let why = if sink_aborted {
                    RecycleFail::Stopped
                } else if aborted {
                    RecycleFail::Aborted
                } else {
                    RecycleFail::NotProcessed
                };
                (ItemStatus::NotRecycled(why), Outcome::NotRecycled)
            }
            InPlace::Deleted => (
                ItemStatus::DeletedUnexpectedly,
                Outcome::DeletedUnexpectedly,
            ),
            InPlace::Unknown => (ItemStatus::Unverified, Outcome::Unverified),
        };
    };
    if !ev.succeeded() {
        let fail = classify_hr(ev.hr);
        let o = match fail {
            RecycleFail::InUse => Outcome::InUse,
            RecycleFail::Denied => Outcome::Denied,
            _ => Outcome::NotRecycled,
        };
        // A-M1: un resultado de FALLO no prueba que la shell no tocara nada (puede haber perdido un hijo antes de fallar).
        // Se aplica el mismo juicio que a un borrado: el objeto aprobado, intacto en su sitio. (Si es un directorio, quien
        // llama comprueba además su contenido.)
        return match in_place(p) {
            InPlace::Intact => (ItemStatus::NotRecycled(fail), o),
            InPlace::Deleted => (
                ItemStatus::DeletedUnexpectedly,
                Outcome::DeletedUnexpectedly,
            ),
            InPlace::Unknown => (ItemStatus::Unverified, Outcome::Unverified),
        };
    }
    match &ev.state {
        BinState::Deleted => {
            // `psiNewlyCreated` nulo con éxito suele ser un borrado definitivo, pero la shell también lo notifica sin
            // haber tocado nada (medido: una carpeta con un archivo disperso enorme da `hr = 0x00270003`, nulo, y la
            // carpeta sigue en el disco). Se decide POR OBJETO, no por nombre: con el handle de guarda aún abierto, un
            // archivo que la shell SÍ eliminó mantiene su nombre visible como "pendiente de borrar" hasta soltar la guarda
            // (medido: `hr = 0x00270008`, el nombre existe, abrirlo da acceso denegado y `DeletePending` es cierto).
            // Solo si el objeto aprobado NO está pendiente de borrar Y reabrir la ruta da la MISMA identidad, la shell no
            // tocó nada. Todo lo demás (pendiente de borrar, identidad distinta, ruta ilegible, handle ilegible) es un
            // borrado definitivo.
            if untouched_by_shell(p) {
                (
                    ItemStatus::NotRecycled(RecycleFail::Shell(ev.hr)),
                    Outcome::NotRecycled,
                )
            } else {
                (
                    ItemStatus::DeletedUnexpectedly,
                    Outcome::DeletedUnexpectedly,
                )
            }
        }
        BinState::InBinUnknownPath => (ItemStatus::Unverified, Outcome::Unverified),
        BinState::InBin(bin_path) => {
            let found = sys::open_for_query(bin_path)
                .and_then(|h| sys::meta(&h))
                .map(|m| m.identity);
            match found {
                Ok(id) if id == p.identity => {
                    let name_gone = std::fs::symlink_metadata(&p.path).is_err();
                    let held_in_bin = sys::final_path(&p.held)
                        .ok()
                        .is_some_and(|f| shell.is_bin_path(&f));
                    // El número de enlaces del objeto no puede haber cambiado desde la comprobación: un recycle legítimo
                    // es un renombrado y lo conserva; si cambió, alguien le añadió o quitó nombres en la ventana.
                    let same_links = sys::meta(&p.held).is_ok_and(|m| m.links == p.links);
                    if name_gone && held_in_bin && same_links {
                        (ItemStatus::Recycled, Outcome::Recycled)
                    } else {
                        // Mismo objeto por otro nombre, o el aprobado no es el que está en la Papelera.
                        (ItemStatus::RecycledOther, Outcome::RecycledOther)
                    }
                }
                Ok(_) => (ItemStatus::RecycledOther, Outcome::RecycledOther),
                Err(_) => (ItemStatus::Unverified, Outcome::Unverified),
            }
        }
    }
}

/// ¿Sigue el objeto aprobado intacto en su sitio? `true` solo si el handle de guarda NO está pendiente de borrar y la
/// ruta, reabierta, da la misma identidad. Es la comprobación por OBJETO de `judge` (R1).
fn untouched_by_shell(p: &Pending) -> bool {
    let not_pending = sys::meta(&p.held).is_ok_and(|m| !m.delete_pending);
    let same_object = sys::open_for_query(&p.path)
        .and_then(|h| sys::meta(&h))
        .is_ok_and(|m| m.identity == p.identity);
    not_pending && same_object
}

/// Dónde está el objeto aprobado tras una operación de la shell que no lo envió (A-M1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InPlace {
    /// El handle de guarda no está pendiente de borrar Y la ruta, reabierta, da la misma identidad: sigue donde estaba.
    Intact,
    /// El handle de guarda está pendiente de borrar: la shell lo eliminó (el nombre sigue visible hasta soltar la guarda).
    Deleted,
    /// Ni intacto ni pendiente de borrar (la ruta ya no es el objeto, o no se pudo leer): no se puede afirmar nada.
    Unknown,
}

/// Juicio por OBJETO de un elemento que no llegó a la Papelera (ver [`InPlace`]).
fn in_place(p: &Pending) -> InPlace {
    match sys::meta(&p.held) {
        Ok(m) if m.delete_pending => InPlace::Deleted,
        Ok(_) if untouched_by_shell(p) => InPlace::Intact,
        _ => InPlace::Unknown,
    }
}

/// Traduce el `HRESULT` de una eliminación fallida de la shell.
fn classify_hr(hr: i32) -> RecycleFail {
    use windows::Win32::UI::Shell::{
        COPYENGINE_E_ACCESS_DENIED_DEST, COPYENGINE_E_ACCESS_DENIED_SRC,
        COPYENGINE_E_SHARING_VIOLATION_DEST, COPYENGINE_E_SHARING_VIOLATION_SRC,
    };
    let code = hr as u32;
    // HRESULT_FROM_WIN32(x) = 0x8007_0000 | x.
    let win32 = (code & 0xFFFF_0000 == 0x8007_0000).then_some(code & 0xFFFF);
    match win32 {
        Some(32 | 33) => RecycleFail::InUse,
        Some(5) => RecycleFail::Denied,
        _ => {
            // Códigos del motor de copia de la shell (`COPYENGINE_E_*`). Se MIDIÓ `ACCESS_DENIED_SRC` al mover a la
            // Papelera una carpeta con un archivo abierto dentro (mover es renombrar).
            let h = HRESULT(hr);
            if h == COPYENGINE_E_SHARING_VIOLATION_SRC || h == COPYENGINE_E_SHARING_VIOLATION_DEST {
                RecycleFail::InUse
            } else if h == COPYENGINE_E_ACCESS_DENIED_SRC || h == COPYENGINE_E_ACCESS_DENIED_DEST {
                RecycleFail::Denied
            } else {
                RecycleFail::Shell(hr)
            }
        }
    }
}

/// ¿Es el objeto (con esa fecha más reciente de creación, escritura y cambio) más nuevo que la antigüedad mínima de `it`? Sin
/// antigüedad mínima nunca. Un reloj adelantado o fechas futuras cuentan como "reciente" (falla cerrado).
fn too_young(it: &PlanItem, newest_filetime: i64) -> bool {
    let min = engine::duration_to_ft(it.min_age);
    min > 0 && engine::now_filetime() - newest_filetime < min
}

enum Prepared {
    Ready(Box<Pending>),
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

/// Lo que desapareció de un elemento definitivo, con rutas completas.
fn removed_of(it: &PlanItem, u: &Unit) -> Vec<Removed> {
    match &it.spec {
        UnitSpec::Contents { path, .. } => u
            .tops()
            .into_iter()
            .map(|t| Removed {
                path: path.join(&t.name),
                is_dir: t.is_dir,
                removed: t.removed,
                bytes: t.bytes,
                logical: t.logical,
                files: t.files,
            })
            .collect(),
        UnitSpec::Entry { path, .. } => u
            .root_result()
            .into_iter()
            .map(|t| Removed {
                path: path.clone(),
                is_dir: t.is_dir,
                removed: t.removed,
                bytes: t.bytes,
                logical: t.logical,
                files: t.files,
            })
            .collect(),
    }
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
