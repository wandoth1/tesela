//! Registro local de lo borrado (PLAN.md §3.7.9) y historial de resúmenes (§2.11).
//!
//! * `%LOCALAPPDATA%\Tesela\limpieza-<fecha>.jsonl`: una línea JSON por entrada procesada (ruta, identidad, tamaño,
//!   resultado). SOLO en la máquina del usuario: contiene rutas reales y nunca se sube ni se versiona. Se conservan los
//!   últimos [`KEEP_LOGS`] y cada uno tiene un tope de líneas ([`MAX_LINES`]) y de bytes ([`MAX_BYTES`]). "Borrar el
//!   historial" de Ajustes los borra también ([`history_clear`]).
//!
//! # Nombres y retención (A-M2)
//! `limpieza-<AAAAMMDD>-<HHMMSS>-<secuencia de 3 cifras>[-simulacion].jsonl`. La poda ordena por fecha, hora y secuencia leídas
//! como números (no por el texto del nombre: un sufijo `-1` ordena ANTES que el nombre base y la poda podía borrar el registro
//! recién creado), nunca borra el registro que acaba de crear y, tras crearlo, comprueba que existe.
//!
//! # Retención (m2)
//! Hay dos clases de registro, con retención PROPIA e independiente: los reales ([`KEEP_LOGS`]) y los de simulación
//! (`-simulacion`, [`KEEP_SIM_LOGS`]). Al crear su registro, cada ejecución poda SOLO su clase: una limpieza real, los
//! reales; una simulación (`--dry-run`), las simulaciones. Una simulación nunca toca un registro real: antes, 31
//! simulaciones seguidas desplazaban los registros reales (los que sirven para reconstruir lo borrado) por orden de fecha.
//!
//! Un registro que no llegó a usarse (la ejecución se rechazó antes de tocar nada: p. ej. `NeedsReconfirm`) se BORRA
//! ([`LogWriter::discard`]): contiene solo la línea de arranque y ocuparía una plaza de la retención de los registros
//! con contenido. Es lo más simple y seguro: no hay que decidir cuándo "empezar" a registrar ni excluir nada de la poda.
//!
//! # Garantías del registro (M4)
//! * **Sin registro no hay acción destructiva.** Quien ejecuta exige un registro (salvo en simulación) y lo comprueba.
//! * **Un error de escritura no se traga.** El primer error de escritura o de volcado MARCA el registro como fallido
//!   ([`LogWriter::ok`]) y activa la bandera de cancelación del ejecutor: la limpieza se detiene en el siguiente punto
//!   seguro (entre entradas, entre operaciones, entre trozos de la Papelera) y no se toca nada más.
//! * **Intención antes, resultado después.** Antes de cada operación se escribe y se VUELCA una línea de intención
//!   ([`LogWriter::write_intent`]: en cada lote entregado a la shell lista sus elementos); después, el resultado.
//!   Los resultados por entrada del recorrido por handle se vuelcan cada [`FLUSH_EVERY`] líneas y al terminar cada operación.
//! * **Volcado y `sync_all`.** `flush` (los bytes llegan a la caché del sistema, que sobrevive a que el proceso muera o la
//!   ventana se cierre a la fuerza: la amenaza que se modela) tras cada intención y cada elemento; `sync_all` (a disco) solo
//!   al crear el registro y al terminar, para no frenar cada elemento con un `fsync` (milisegundos por llamada, miles de
//!   llamadas) cuando el riesgo de un corte de corriente a mitad no justifica ese coste.
//! * `%LOCALAPPDATA%\Tesela\history.jsonl`: SOLO resúmenes (fecha, unidad, usado, liberado ahora, enviado a la
//!   Papelera). Nunca el árbol ni rutas. Se puede borrar desde Ajustes.

use crate::engine::{Event, EventSink, Outcome};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Registros REALES que se conservan.
pub const KEEP_LOGS: usize = 30;
/// Registros de SIMULACIÓN (`--dry-run`) que se conservan. Se podan aparte de los reales (m2).
pub const KEEP_SIM_LOGS: usize = 10;
/// Tope de líneas de un registro (un `%TEMP%` enorme tiene cientos de miles de archivos).
pub const MAX_LINES: u64 = 250_000;
/// Tope de bytes de un registro (rutas largas: 250 000 líneas podrían pasar de cien MiB).
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Cada cuántas líneas se vuelca el registro a disco sin esperar a que termine un elemento.
const FLUSH_EVERY: u64 = 256;

/// Fecha y hora UTC a partir de segundos Unix (algoritmo de días civiles de H. Hinnant).
pub fn utc_parts(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (
        if m <= 2 { y + 1 } else { y },
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// El archivo del registro. En las pruebas puede fallar a propósito después de `left` bytes.
enum LogFile {
    Real(File),
    #[cfg(test)]
    FailAfter {
        file: File,
        left: usize,
    },
}

impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            LogFile::Real(f) => f.write(buf),
            #[cfg(test)]
            LogFile::FailAfter { file, left } => {
                if *left == 0 {
                    return Err(std::io::Error::other("fallo de escritura simulado"));
                }
                let n = buf.len().min(*left);
                let w = file.write(&buf[..n])?;
                *left -= w;
                Ok(w)
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            LogFile::Real(f) => f.flush(),
            #[cfg(test)]
            LogFile::FailAfter { file, .. } => file.flush(),
        }
    }
}

impl LogFile {
    fn sync_all(&self) -> std::io::Result<()> {
        match self {
            LogFile::Real(f) => f.sync_all(),
            #[cfg(test)]
            LogFile::FailAfter { file, .. } => file.sync_all(),
        }
    }
}

/// El escritor del registro de una ejecución.
pub struct LogWriter {
    path: PathBuf,
    out: Mutex<BufWriter<LogFile>>,
    lines: AtomicU64,
    bytes: AtomicU64,
    truncated: AtomicBool,
    /// Tope de bytes (las pruebas lo bajan).
    max_bytes: AtomicU64,
    /// El primer error de escritura o de volcado marca el registro como fallido para siempre.
    failed: AtomicBool,
    first_error: Mutex<Option<String>>,
    /// Bandera de cancelación del ejecutor: un error de escritura la activa (la limpieza se detiene en el siguiente punto
    /// seguro).
    cancel_on_error: Mutex<Option<Arc<AtomicBool>>>,
    /// ¿Se escribió ya algo de una OPERACIÓN (una intención, una entrada, un `bin-item`, un resultado)? Mientras no, el
    /// registro solo tiene la línea de arranque y las notas `rejected`, y se puede descartar (R2).
    operation_written: AtomicBool,
}

#[derive(Serialize)]
struct Line<'a> {
    t: &'a str,
    unit: usize,
    path: String,
    dir: bool,
    vol: Option<u64>,
    id: Option<String>,
    bytes: u64,
    links: u32,
    out: String,
}

fn outcome_name(o: Outcome) -> String {
    match o {
        Outcome::Deleted => "deleted".into(),
        Outcome::WouldDelete => "would-delete".into(),
        Outcome::Recycled => "recycled".into(),
        Outcome::WouldRecycle => "would-recycle".into(),
        Outcome::NotRecycled => "not-recycled".into(),
        Outcome::DeletedUnexpectedly => "deleted-unexpectedly".into(),
        Outcome::RecycledOther => "recycled-other-object".into(),
        Outcome::Unverified => "unverified".into(),
        Outcome::InUse => "in-use".into(),
        Outcome::Denied => "denied".into(),
        Outcome::Changed => "changed".into(),
        Outcome::Protected => "protected".into(),
        Outcome::Young => "too-recent".into(),
        Outcome::Reparse => "reparse-skipped".into(),
        Outcome::Failed(c) => format!("error-{c}"),
        Outcome::NotEmpty => "not-empty".into(),
    }
}

impl LogWriter {
    /// Crea el registro de una ejecución en `dir` (se crea si no existe) y poda los antiguos. Si no se puede crear o no se
    /// puede escribir y volcar la línea de arranque (con `sync_all`), es un error: sin registro no se limpia.
    pub fn create(dir: &Path, dry_run: bool) -> std::io::Result<LogWriter> {
        Self::create_with(dir, dry_run, LogFile::Real)
    }

    /// SOLO PARA PRUEBAS: un registro cuyo archivo falla al escribir pasados `bytes` bytes.
    #[cfg(test)]
    pub(crate) fn create_failing_after(
        dir: &Path,
        dry_run: bool,
        bytes: usize,
    ) -> std::io::Result<LogWriter> {
        Self::create_with(dir, dry_run, move |file| LogFile::FailAfter {
            file,
            left: bytes,
        })
    }

    fn create_with(
        dir: &Path,
        dry_run: bool,
        wrap: impl FnOnce(File) -> LogFile,
    ) -> std::io::Result<LogWriter> {
        Self::create_at(dir, dry_run, wrap, unix_now())
    }

    /// Como `create_with`, con el reloj (segundos Unix) dado: las pruebas fijan el segundo para probar las colisiones.
    fn create_at(
        dir: &Path,
        dry_run: bool,
        wrap: impl FnOnce(File) -> LogFile,
        now: u64,
    ) -> std::io::Result<LogWriter> {
        fs::create_dir_all(dir)?;
        let (y, mo, d, h, mi, sec) = utc_parts(now);
        let stamp = format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{sec:02}");
        // El nombre ORDENA bien como texto y como número (A-M2): `limpieza-<fecha>-<hora>-<secuencia de 3 cifras>[-simulacion]`.
        // La secuencia es la mayor que ya haya en ese segundo más uno (no el primer hueco: si se podó el 001, el nuevo no puede
        // volver a ser 001 y quedar por debajo de uno anterior). Antes la colisión añadía `-1` al nombre, que ordena ANTES
        // que el nombre base (`-` < `.`): la poda podía borrar el registro recién creado.
        let mut n = next_sequence(dir, &stamp);
        let sim = if dry_run { "-simulacion" } else { "" };
        let (path, f) = loop {
            let path = dir.join(format!("limpieza-{stamp}-{n:03}{sim}.jsonl"));
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(f) => break (path, f),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        };
        let w = LogWriter {
            path,
            out: Mutex::new(BufWriter::new(wrap(f))),
            lines: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            truncated: AtomicBool::new(false),
            max_bytes: AtomicU64::new(MAX_BYTES),
            failed: AtomicBool::new(false),
            first_error: Mutex::new(None),
            cancel_on_error: Mutex::new(None),
            operation_written: AtomicBool::new(false),
        };
        w.raw(&format!(
            "{{\"t\":\"start\",\"dry_run\":{dry_run},\"unix\":{}}}",
            unix_now()
        ))?;
        // La línea de arranque ya está en disco (con `sync_all`): aunque la aplicación muera enseguida, consta que hubo
        // una limpieza.
        w.sync()?;
        // Cada ejecución poda SOLO su clase: una simulación nunca toca un registro real (m2). El registro que acaba de crearse
        // se EXCLUYE de la poda (A-M2): nunca puede borrarse a sí mismo, ni por una fecha posterior (reloj adelantado) ni por
        // un nombre que ordene mal.
        prune(
            dir,
            dry_run,
            if dry_run { KEEP_SIM_LOGS } else { KEEP_LOGS },
            Some(&w.path),
        );
        // Y se comprueba: un registro que no existe no es un registro, y sin registro no se limpia.
        if !w.path.is_file() {
            return Err(std::io::Error::other(
                "el registro recién creado ya no existe",
            ));
        }
        Ok(w)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Marca el registro como fallido (la primera vez guarda el error) y activa la cancelación del ejecutor.
    fn poison(&self, e: &std::io::Error) {
        self.failed.store(true, Ordering::SeqCst);
        self.first_error
            .lock()
            .unwrap_or_else(|x| x.into_inner())
            .get_or_insert_with(|| e.to_string());
        if let Some(c) = self
            .cancel_on_error
            .lock()
            .unwrap_or_else(|x| x.into_inner())
            .as_ref()
        {
            c.store(true, Ordering::SeqCst);
        }
    }

    fn checked<T>(&self, r: std::io::Result<T>) -> std::io::Result<T> {
        if let Err(e) = &r {
            self.poison(e);
        }
        r
    }

    fn raw(&self, line: &str) -> std::io::Result<()> {
        let r = {
            let mut g = self.out.lock().unwrap_or_else(|e| e.into_inner());
            writeln!(g, "{line}")
        };
        self.bytes
            .fetch_add(line.len() as u64 + 1, Ordering::Relaxed);
        self.checked(r)
    }

    /// Borra el archivo del registro: para una ejecución que se rechazó antes de tocar nada (ver el módulo). SOLO si no se
    /// escribió nada de una operación (intención, entrada, `bin-item`, resultado): con algo escrito NO se borra y devuelve
    /// un error (R2). No hace falta cerrar nada antes: Windows admite borrar un archivo abierto con compartición de borrado.
    pub(crate) fn discard(&self) -> std::io::Result<()> {
        if self.operation_written.load(Ordering::SeqCst) {
            return Err(std::io::Error::other(
                "el registro ya contiene operaciones: no se descarta",
            ));
        }
        match fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// ¿Llegó el registro a su tope (líneas o bytes) y dejó de anotar? La limpieza sigue, pero lo posterior no consta.
    pub fn truncated(&self) -> bool {
        self.truncated.load(Ordering::Relaxed)
    }

    /// SOLO PARA PRUEBAS: baja el tope de bytes.
    #[cfg(test)]
    pub(crate) fn set_max_bytes_for_test(&self, n: u64) {
        self.max_bytes.store(n, Ordering::Relaxed);
    }

    /// ¿Va bien el registro? `false` en cuanto falló una escritura o un volcado (ya no se puede confiar en que conste lo
    /// que se haga).
    pub fn ok(&self) -> bool {
        !self.failed.load(Ordering::SeqCst)
    }

    /// El primer error de escritura o de volcado, si lo hubo.
    pub fn error(&self) -> Option<String> {
        self.first_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Hace que un error de escritura active `flag` (la cancelación del ejecutor). Si ya había fallado, la activa ya.
    pub fn cancel_on_error(&self, flag: Arc<AtomicBool>) {
        if !self.ok() {
            flag.store(true, Ordering::SeqCst);
        }
        *self
            .cancel_on_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(flag);
    }

    /// Vuelca lo escrito al sistema (y `sync_all` a disco). Un error marca el registro como fallido.
    pub fn sync(&self) -> std::io::Result<()> {
        let r = {
            let mut g = self.out.lock().unwrap_or_else(|e| e.into_inner());
            g.flush().and_then(|()| g.get_ref().sync_all())
        };
        self.checked(r)
    }

    /// Escribe una LÍNEA DE INTENCIÓN y la vuelca antes de que se haga la operación: `kind` con la lista de rutas que se van
    /// a tocar. Si no se puede escribir o volcar, es un error y la operación NO debe hacerse.
    pub fn write_intent(&self, kind: &str, paths: &[PathBuf]) -> std::io::Result<()> {
        let list: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let v = serde_json::json!({ "t": kind, "paths": list });
        self.operation_written.store(true, Ordering::SeqCst);
        self.raw(&v.to_string())?;
        self.flush_checked()
    }

    /// Un elemento de la Papelera purgado (o saltado), individualmente: ruta de origen, `id`, tamaño y resultado.
    pub fn write_bin_item(&self, original: &Path, id: &OsStr, size: u64, out: &str) {
        self.operation_written.store(true, Ordering::SeqCst);
        let v = serde_json::json!({
            "t": "bin-item",
            "path": original.to_string_lossy(),
            "id": id.to_string_lossy(),
            "bytes": size,
            "out": out,
        });
        let _ = self.raw(&v.to_string());
    }

    /// Como [`LogWriter::flush`], pero devuelve el error.
    pub fn flush_checked(&self) -> std::io::Result<()> {
        let r = self.out.lock().unwrap_or_else(|e| e.into_inner()).flush();
        self.checked(r)
    }

    /// Cuenta una línea y dice si el registro ya pasó su tope (líneas o bytes): en ese caso deja UNA marca de truncado y ya
    /// no se escribe más. Lo comparten las entradas y las notas `rejected`, para que ninguna se salte el tope (R4).
    fn over_cap(&self) -> Option<u64> {
        let n = self.lines.fetch_add(1, Ordering::Relaxed);
        if n > MAX_LINES
            || self.bytes.load(Ordering::Relaxed) > self.max_bytes.load(Ordering::Relaxed)
        {
            if !self.truncated.swap(true, Ordering::Relaxed) {
                let _ = self.raw("{\"t\":\"truncated\"}");
                self.flush();
            }
            return None;
        }
        Some(n)
    }

    /// Una nota de algo que NO llegó al plan (`rejected`), con el mismo tope y el mismo truncado que las entradas. No cuenta
    /// como operación escrita (R2).
    pub fn write_rejected(&self, path: &Path, reason: &str) {
        if self.over_cap().is_none() {
            return;
        }
        let v =
            serde_json::json!({ "t": "rejected", "path": path.to_string_lossy(), "text": reason });
        let _ = self.raw(&v.to_string());
    }

    /// Escribe una entrada procesada.
    pub fn write_event(&self, ev: &Event) {
        self.operation_written.store(true, Ordering::SeqCst);
        let Some(n) = self.over_cap() else {
            return;
        };
        if n % FLUSH_EVERY == FLUSH_EVERY - 1 {
            self.flush();
        }
        let line = Line {
            t: "entry",
            unit: ev.unit,
            path: ev.path.to_string_lossy().into_owned(),
            dir: ev.is_dir,
            vol: ev.identity.map(|i| i.volume_serial),
            id: ev.identity.map(|i| format!("{:032x}", i.file_id)),
            bytes: ev.bytes,
            links: ev.links,
            out: outcome_name(ev.outcome),
        };
        if let Ok(s) = serde_json::to_string(&line) {
            let _ = self.raw(&s);
        }
    }

    /// Una línea de texto libre del propio registro (resumen, avisos).
    pub fn write_note(&self, kind: &str, text: &str) {
        self.operation_written.store(true, Ordering::SeqCst);
        let v = serde_json::json!({ "t": kind, "text": text });
        let _ = self.raw(&v.to_string());
    }

    /// El receptor de eventos para el recorrido.
    pub fn sink(self: &Arc<Self>) -> Arc<EventSink> {
        let me = self.clone();
        Arc::new(move |ev| me.write_event(&ev))
    }

    /// Vuelca lo escrito al sistema. Un error marca el registro como fallido (ver [`LogWriter::ok`]).
    pub fn flush(&self) {
        let _ = self.flush_checked();
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        self.flush();
    }
}

/// ¿Es un registro de simulación? (`limpieza-<fecha>-simulacion[-n].jsonl`).
fn is_sim_log(name: &str) -> bool {
    name.contains("-simulacion")
}

/// Clave de orden de un registro por su NOMBRE: (fecha, hora, secuencia) leídas como NÚMEROS. Entiende
/// `limpieza-<AAAAMMDD>-<HHMMSS>[-<secuencia>][-simulacion].jsonl` (la secuencia de los registros antiguos era `-1`, `-2`...
/// y la de los nuevos, `-001`). Lo que no se entiende ordena como el más antiguo (y entre sí, por nombre).
fn log_key(name: &str) -> (u64, u64, u64, String) {
    let parsed = (|| {
        let rest = name.strip_prefix("limpieza-")?;
        let (date, rest) = rest.split_once('-')?;
        let time = rest.get(..6)?;
        let rest = &rest[6..];
        if date.len() != 8
            || !date.bytes().all(|b| b.is_ascii_digit())
            || !time.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let seq = rest
            .strip_prefix('-')
            .map(|r| {
                r.bytes()
                    .take_while(u8::is_ascii_digit)
                    .collect::<Vec<u8>>()
            })
            .filter(|d| !d.is_empty())
            .and_then(|d| String::from_utf8(d).ok()?.parse::<u64>().ok())
            .unwrap_or(0);
        Some((date.parse::<u64>().ok()?, time.parse::<u64>().ok()?, seq))
    })();
    let (d, t, q) = parsed.unwrap_or((0, 0, 0));
    (d, t, q, name.to_lowercase())
}

/// La secuencia siguiente para el segundo `stamp` (`AAAAMMDD-HHMMSS`): la mayor que haya en `dir` más uno (1 si no hay).
fn next_sequence(dir: &Path, stamp: &str) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 1 };
    let prefix = format!("limpieza-{stamp}");
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(&prefix).then(|| log_key(&name).2)
        })
        .max()
        .map_or(1, |m| m + 1)
}

/// Deja los `keep` registros más recientes de UNA clase (`sim`: simulaciones; si no, reales) de `dir`, ordenados por la fecha,
/// la hora y la secuencia de su nombre ([`log_key`]). La otra clase no se toca y `active` (el registro que se acaba de crear,
/// que cuenta como uno de los `keep`) NUNCA se borra.
fn prune(dir: &Path, sim: bool, keep: usize, active: Option<&Path>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut logs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "jsonl")
                && p.file_name().is_some_and(|n| {
                    let n = n.to_string_lossy();
                    n.starts_with("limpieza-") && is_sim_log(&n) == sim
                })
                && active != Some(p.as_path())
        })
        .collect();
    logs.sort_by_key(|p| log_key(&p.file_name().unwrap_or_default().to_string_lossy()));
    // El activo ocupa una plaza: de los demás se conservan `keep - 1` (o `keep`, si no hay activo).
    let allowed = if active.is_some() {
        keep.saturating_sub(1)
    } else {
        keep
    };
    if logs.len() > allowed {
        for old in &logs[..logs.len() - allowed] {
            let _ = fs::remove_file(old);
        }
    }
}

/// Un resumen del historial (sin rutas).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Cuándo (segundos Unix).
    pub unix: u64,
    /// Letra de la unidad (0 si era una carpeta).
    pub drive: u8,
    /// Bytes usados en el volumen al limpiar.
    pub used: u64,
    /// "Liberado ahora" (medido).
    pub freed_now: u64,
    /// Enviado a la Papelera.
    pub to_bin: u64,
}

/// Añade un resumen a `history.jsonl`.
pub fn history_append(dir: &Path, e: &HistoryEntry) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("history.jsonl"))?;
    let line = serde_json::to_string(e).map_err(std::io::Error::other)?;
    writeln!(f, "{line}")
}

/// Lee el historial (ignora las líneas que no se entienden).
pub fn history_load(dir: &Path) -> Vec<HistoryEntry> {
    let Ok(f) = File::open(dir.join("history.jsonl")) else {
        return Vec::new();
    };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

/// Borra el historial y TAMBIÉN los registros de limpieza (`limpieza-*.jsonl`, que llevan rutas) de la carpeta de datos
/// (Ajustes: "Borrar el historial"). Solo toca esos nombres, y los registros no se siguen si son enlaces.
pub fn history_clear(dir: &Path) -> std::io::Result<()> {
    let mut first_err: Option<std::io::Error> = None;
    let mut remove = |p: &Path| match fs::remove_file(p) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            first_err.get_or_insert(e);
        }
    };
    remove(&dir.join("history.jsonl"));
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let is_log = p.extension().is_some_and(|x| x == "jsonl")
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("limpieza-"));
            // `file_type` de la entrada no sigue enlaces: un enlace con ese nombre no se toca.
            if is_log && e.file_type().is_ok_and(|t| t.is_file()) {
                remove(&p);
            }
        }
    }
    first_err.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::FileIdentity;
    use crate::testutil::Sandbox;

    #[test]
    fn fechas_utc() {
        assert_eq!(utc_parts(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(utc_parts(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(utc_parts(1_760_000_000), (2025, 10, 9, 8, 53, 20));
    }

    #[test]
    fn el_registro_lleva_una_linea_por_entrada_y_poda_los_antiguos() {
        let sb = Sandbox::new();
        let dir = sb.path("Tesela");
        for i in 0..(KEEP_LOGS + 5) {
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("limpieza-2000-{i:03}.jsonl")), b"x").unwrap();
        }
        let log = Arc::new(LogWriter::create(&dir, true).unwrap());
        assert!(
            log.path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-simulacion.jsonl")
        );
        // La simulación NO poda (m2): los 35 antiguos siguen.
        let olds = |dir: &Path| {
            fs::read_dir(dir)
                .unwrap()
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("limpieza-2000-")
                })
                .count()
        };
        assert_eq!(olds(&dir), KEEP_LOGS + 5);
        let sink = log.sink();
        sink(Event {
            unit: 3,
            path: PathBuf::from("C:\\x\\a \"b\".tmp"),
            is_dir: false,
            identity: Some(FileIdentity {
                volume_serial: 7,
                file_id: 255,
            }),
            bytes: 4096,
            links: 1,
            outcome: Outcome::WouldDelete,
        });
        log.write_note("summary", "hola");
        log.flush();
        let text = fs::read_to_string(log.path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"dry_run\":true"));
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["out"], "would-delete");
        assert_eq!(v["path"], "C:\\x\\a \"b\".tmp");
        assert_eq!(v["id"], "000000000000000000000000000000ff");
        assert_eq!(v["bytes"], 4096);
        let n = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("limpieza-"))
            .count();
        assert_eq!(n, KEEP_LOGS + 5 + 1, "{n}");
        // Una limpieza REAL sí poda sus reales: quedan en `KEEP_LOGS` (el nuevo cuenta) y la simulación se conserva.
        let real = LogWriter::create(&dir, false).unwrap();
        let (mut reales, mut sims) = (0, 0);
        for e in fs::read_dir(&dir).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("limpieza-") && name.ends_with(".jsonl") {
                if name.contains("-simulacion") {
                    sims += 1;
                } else {
                    reales += 1;
                }
            }
        }
        assert_eq!(reales, KEEP_LOGS);
        assert_eq!(sims, 1);
        assert!(real.path().exists());
    }

    /// m2 y H6.6: 31 registros reales y una simulación: siguen los 31 reales (la simulación nunca toca un registro real).
    /// Las simulaciones se podan SOLO entre sí (a `KEEP_SIM_LOGS`) y una limpieza real poda solo los reales.
    #[test]
    fn cada_clase_de_registro_se_poda_solo_a_si_misma() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        for i in 0..31 {
            fs::write(dir.join(format!("limpieza-2001-{i:03}.jsonl")), b"{}").unwrap();
        }
        let count = |dir: &Path, sim: bool| {
            fs::read_dir(dir)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("limpieza-") && n.contains("-simulacion") == sim)
                .count()
        };
        let sim = LogWriter::create(&dir, true).unwrap();
        assert!(sim.path().exists());
        assert_eq!(count(&dir, false), 31, "la simulación no toca los reales");
        // 40 simulaciones seguidas (en el mismo segundo los nombres llevan sufijo): los reales intactos y las
        // simulaciones, podadas entre sí.
        let sims: Vec<LogWriter> = (0..40)
            .map(|_| LogWriter::create(&dir, true).unwrap())
            .collect();
        assert_eq!(count(&dir, false), 31);
        assert_eq!(count(&dir, true), KEEP_SIM_LOGS);
        drop(sims);
        // Una limpieza real poda SOLO los reales (a `KEEP_LOGS`) y deja las simulaciones como están.
        let _real = LogWriter::create(&dir, false).unwrap();
        assert_eq!(count(&dir, false), KEEP_LOGS);
        assert_eq!(count(&dir, true), KEEP_SIM_LOGS);
    }

    fn real_logs(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("limpieza-") && !n.contains("-simulacion"))
            .collect();
        v.sort();
        v
    }

    /// A-M2: con registros FECHADOS DESPUÉS (reloj adelantado, copia de otra máquina) el registro recién creado ordenaba el
    /// primero y la poda, que corría tras crearlo, lo borraba: la limpieza seguía con un archivo desvinculado y `ok()` a
    /// `true`. Ahora la poda lo excluye, y tras crearlo se comprueba que existe.
    #[test]
    fn el_registro_nuevo_no_se_borra_a_si_mismo_aunque_haya_otros_fechados_despues() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        for i in 0..KEEP_LOGS + 5 {
            fs::write(
                dir.join(format!("limpieza-29990101-0000{i:02}-001.jsonl")),
                b"{}",
            )
            .unwrap();
        }
        let log = LogWriter::create(&dir, false).unwrap();
        assert!(log.ok());
        assert!(log.path().is_file(), "el registro nuevo existe");
        log.write_note("summary", "sigue escribiéndose en el archivo del disco");
        log.flush();
        assert!(
            fs::read_to_string(log.path()).unwrap().contains("summary"),
            "lo escrito llega al archivo que hay en el disco"
        );
        // La retención se cumple (30 reales contando el nuevo) y el nuevo es uno de ellos.
        let names = real_logs(&dir);
        assert_eq!(names.len(), KEEP_LOGS, "{names:?}");
        assert!(
            names.contains(
                &log.path()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            )
        );
        // Lo que se poda son los MÁS ANTIGUOS de los demás (por fecha leída como número): los primeros fechados.
        assert!(!dir.join("limpieza-29990101-000000-001.jsonl").exists());
        assert!(dir.join("limpieza-29990101-000034-001.jsonl").exists());
    }

    /// A-M2: dos registros en el MISMO segundo. Antes el segundo se llamaba `...-1.jsonl`, que ordena ANTES que el nombre base
    /// (`-` < `.`), y la poda lo borraba en cuanto había que podar. Ahora llevan una secuencia de 3 cifras que ordena bien, y
    /// la secuencia es la mayor del segundo más uno (no el primer hueco). Con el reloj fijo, para que sea determinista.
    #[test]
    fn varios_registros_en_el_mismo_segundo_ordenan_bien_y_se_podan_los_mas_antiguos() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        const NOW: u64 = 1_760_000_000;
        let mut made: Vec<PathBuf> = Vec::new();
        for i in 0..KEEP_LOGS + 6 {
            let w = LogWriter::create_at(&dir, false, LogFile::Real, NOW).unwrap();
            assert!(w.path().is_file(), "el registro {i} existe tras crearlo");
            assert!(w.ok());
            made.push(w.path().to_path_buf());
        }
        let mut sorted = made.clone();
        sorted.sort_by_key(|p| log_key(&p.file_name().unwrap().to_string_lossy()));
        assert_eq!(sorted, made, "se crean en el orden en que ordenan");
        let names = real_logs(&dir);
        assert_eq!(names.len(), KEEP_LOGS, "{names:?}");
        // Sobreviven los KEEP_LOGS más recientes, no los más antiguos.
        for p in &made[..6] {
            assert!(!p.exists(), "{p:?}");
        }
        for p in &made[6..] {
            assert!(p.exists(), "{p:?}");
        }
        // Y la secuencia continúa tras una poda: el siguiente es el mayor más uno, no un hueco.
        let w = LogWriter::create_at(&dir, false, LogFile::Real, NOW).unwrap();
        let seq = log_key(&w.path().file_name().unwrap().to_string_lossy()).2;
        assert_eq!(seq as usize, KEEP_LOGS + 6 + 1);
    }

    /// A-M2: el orden por nombre lee fecha, hora y secuencia como números: el nombre base va antes que `-1`, `-2` y `-10`
    /// (los nombres de las versiones anteriores) y que `-001`; lo que no se entiende, el más antiguo.
    #[test]
    fn el_orden_de_los_registros_lee_fecha_hora_y_secuencia_como_numeros() {
        let k = |n: &str| log_key(n);
        let ordered = [
            "limpieza-2000-001.jsonl", // no se entiende: el más antiguo
            "limpieza-20260101-115959-001.jsonl",
            "limpieza-20260101-120000.jsonl",
            "limpieza-20260101-120000-1.jsonl",
            "limpieza-20260101-120000-2.jsonl",
            "limpieza-20260101-120000-10.jsonl",
            "limpieza-20260101-120000-011.jsonl",
            "limpieza-20260101-120001.jsonl",
            "limpieza-20260102-000000-001.jsonl",
        ];
        for w in ordered.windows(2) {
            assert!(k(w[0]) < k(w[1]), "{} < {}", w[0], w[1]);
        }
        assert!(k("limpieza-20260101-120000.jsonl") < k("limpieza-20260101-120000-1.jsonl"));
        assert!(k("limpieza-20260101-120000-2.jsonl") < k("limpieza-20260101-120000-10.jsonl"));
        assert_eq!(k("limpieza-20260101-120000-007.jsonl").2, 7);
        assert_eq!(k("limpieza-20260101-120000-001-simulacion.jsonl").2, 1);
        assert_eq!(k("limpieza-20260101-120000.jsonl").2, 0);
    }

    /// A-M2: los registros de la versión anterior (`...-1.jsonl`, `...-2.jsonl`, `...-10.jsonl`) conviven con los nuevos: se podan
    /// por su orden NUMÉRICO (el base, `-1`, `-2`... `-34`), no por el del texto (`-1`, `-10`, `-11`... `-2`, que borraba los
    /// recientes), y el nuevo sigue la secuencia (35).
    #[test]
    fn los_registros_de_la_version_anterior_se_podan_por_su_orden_numerico() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        const NOW: u64 = 1_767_268_800; // 2026-01-01 12:00:00 UTC
        let stamp = "20260101-120000";
        assert_eq!(utc_parts(NOW), (2026, 1, 1, 12, 0, 0));
        fs::write(dir.join(format!("limpieza-{stamp}.jsonl")), b"{}").unwrap();
        for n in 1..=34 {
            fs::write(dir.join(format!("limpieza-{stamp}-{n}.jsonl")), b"{}").unwrap();
        }
        let w = LogWriter::create_at(&dir, false, LogFile::Real, NOW).unwrap();
        assert!(w.path().is_file());
        assert!(w.path().to_string_lossy().ends_with("-035.jsonl"));
        assert_eq!(real_logs(&dir).len(), KEEP_LOGS);
        for gone in std::iter::once(format!("limpieza-{stamp}.jsonl"))
            .chain((1..=5).map(|n| format!("limpieza-{stamp}-{n}.jsonl")))
        {
            assert!(!dir.join(&gone).exists(), "{gone} era de los más antiguos");
        }
        for kept in (6..=34).map(|n| format!("limpieza-{stamp}-{n}.jsonl")) {
            assert!(dir.join(&kept).exists(), "{kept} es de los más recientes");
        }
    }

    /// A-M2: `prune` nunca borra el registro activo, aunque ordene el primero, y le reserva una plaza.
    #[test]
    fn la_poda_excluye_siempre_el_registro_activo() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        for i in 0..KEEP_LOGS + 10 {
            fs::write(
                dir.join(format!("limpieza-29990101-0000{i:02}-001.jsonl")),
                b"{}",
            )
            .unwrap();
        }
        let active = dir.join("limpieza-20000101-000000-001.jsonl");
        fs::write(&active, b"{}").unwrap();
        prune(&dir, false, KEEP_LOGS, Some(&active));
        assert!(active.exists(), "el activo ordena el primero y no se borra");
        assert_eq!(
            real_logs(&dir).len(),
            KEEP_LOGS,
            "el activo cuenta como uno"
        );
        // Sin activo, se conservan KEEP_LOGS de los demás.
        prune(&dir, false, KEEP_LOGS - 1, None);
        assert_eq!(real_logs(&dir).len(), KEEP_LOGS - 1);
        assert!(
            !active.exists(),
            "sin la exclusión, el más antiguo sí se poda"
        );
    }

    fn event(i: usize, path_len: usize) -> Event {
        Event {
            unit: i,
            path: PathBuf::from("a".repeat(path_len)),
            is_dir: false,
            identity: None,
            bytes: 1,
            links: 1,
            outcome: Outcome::Deleted,
        }
    }

    /// H6.8: un registro descartado desaparece (no queda un archivo con solo la línea de arranque ocupando plaza).
    #[test]
    fn un_registro_descartado_no_deja_archivo() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        let log = LogWriter::create(&dir, false).unwrap();
        let path = log.path().to_path_buf();
        assert!(path.exists());
        // Las notas `rejected` no son una operación: se puede descartar.
        log.write_rejected(Path::new("x"), "ReparseAncestor");
        assert!(log.discard().is_ok());
        assert!(!path.exists());
        // R2: con una operación escrita (intención, entrada, `bin-item` o resultado) NO se borra y devuelve un error.
        for i in 0..4 {
            let log = LogWriter::create(&dir, false).unwrap();
            let path = log.path().to_path_buf();
            match i {
                0 => log
                    .write_intent("delete-begin", &[PathBuf::from("a")])
                    .unwrap(),
                1 => log.write_event(&event(0, 5)),
                2 => log.write_bin_item(Path::new("a"), OsStr::new("id"), 1, "purged"),
                _ => log.write_note("recycle", "resultado"),
            }
            assert!(log.discard().is_err(), "{i}");
            assert!(
                path.exists(),
                "{i}: no se borra un registro con operaciones"
            );
        }
    }

    /// R4: las notas `rejected` respetan el mismo tope y el mismo truncado que las entradas.
    #[test]
    fn las_notas_rejected_respetan_el_tope_del_registro() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        let log = LogWriter::create(&dir, false).unwrap();
        log.set_max_bytes_for_test(2_000);
        for i in 0..500 {
            log.write_rejected(Path::new(&format!("C:\\carpeta\\{i}")), "ReparseAncestor");
        }
        log.flush();
        let text = fs::read_to_string(log.path()).unwrap();
        assert!(log.truncated());
        assert!(text.len() < 2_000 + 500, "{}", text.len());
        assert_eq!(text.matches("\"truncated\"").count(), 1);
        assert!(text.matches("\"rejected\"").count() < 50);
    }

    #[test]
    fn la_linea_de_arranque_y_las_rutas_de_cada_operacion_estan_en_disco_sin_esperar() {
        let sb = Sandbox::new();
        let dir = sb.path("Tesela");
        let log = LogWriter::create(&dir, false).unwrap();
        // Nada de `flush()` explícito de quien llama: la línea de arranque ya está en el archivo.
        assert!(
            fs::read_to_string(log.path())
                .unwrap()
                .contains("\"start\""),
            "falta la línea de arranque"
        );
        let paths = vec![PathBuf::from("a.txt"), PathBuf::from("b c.txt")];
        log.write_intent("recycle-begin", &paths).unwrap();
        log.flush();
        let text = fs::read_to_string(log.path()).unwrap();
        let line = text.lines().find(|l| l.contains("recycle-begin")).unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["paths"], serde_json::json!(["a.txt", "b c.txt"]));
    }

    #[test]
    fn el_registro_se_vuelca_cada_pocas_lineas_sin_esperar_al_final() {
        let sb = Sandbox::new();
        let dir = sb.path("Tesela");
        let log = Arc::new(LogWriter::create(&dir, false).unwrap());
        for i in 0..FLUSH_EVERY as usize + 10 {
            log.write_event(&event(i, 20));
        }
        // Sin `flush()` explícito, lo escrito hasta el último volcado periódico ya está en disco.
        let on_disk = fs::read_to_string(log.path()).unwrap().lines().count();
        assert!(on_disk as u64 >= FLUSH_EVERY, "{on_disk}");
    }

    #[test]
    fn el_registro_tiene_tope_de_bytes_ademas_del_de_lineas() {
        let sb = Sandbox::new();
        let dir = sb.path("Tesela");
        let log = Arc::new(LogWriter::create(&dir, false).unwrap());
        log.max_bytes.store(10_000, Ordering::Relaxed);
        for i in 0..200 {
            log.write_event(&event(i, 300));
        }
        log.flush();
        let text = fs::read_to_string(log.path()).unwrap();
        assert!(text.len() < 10_000 + 1_000, "{}", text.len());
        assert_eq!(text.matches("\"truncated\"").count(), 1);
        assert!(text.lines().count() < 200);
    }

    #[test]
    fn borrar_el_historial_borra_tambien_los_registros_y_nada_mas() {
        let sb = Sandbox::new();
        let dir = sb.path("Tesela");
        history_append(
            &dir,
            &HistoryEntry {
                unix: 1,
                drive: b'C',
                used: 1,
                freed_now: 1,
                to_bin: 0,
            },
        )
        .unwrap();
        let log = LogWriter::create(&dir, false).unwrap();
        let log_path = log.path().to_path_buf();
        drop(log);
        fs::write(dir.join("limpieza-2000-001.jsonl"), b"x").unwrap();
        // Lo que no es de Tesela o no tiene ese nombre se queda: otro archivo, otra extensión y una carpeta homónima.
        fs::write(dir.join("config.toml"), b"x").unwrap();
        fs::write(dir.join("limpieza-notas.txt"), b"x").unwrap();
        fs::create_dir(dir.join("limpieza-carpeta.jsonl")).unwrap();
        history_clear(&dir).unwrap();
        assert!(!dir.join("history.jsonl").exists());
        assert!(!log_path.exists() && !dir.join("limpieza-2000-001.jsonl").exists());
        assert!(dir.join("config.toml").exists());
        assert!(dir.join("limpieza-notas.txt").exists());
        assert!(dir.join("limpieza-carpeta.jsonl").is_dir());
        history_clear(&dir).unwrap();
    }

    #[test]
    fn el_historial_es_solo_resumenes_y_se_puede_borrar() {
        let sb = Sandbox::new();
        let dir = sb.path("Tesela");
        assert!(history_load(&dir).is_empty());
        let e = HistoryEntry {
            unix: 1_700_000_000,
            drive: b'C',
            used: 400,
            freed_now: 11,
            to_bin: 3,
        };
        history_append(&dir, &e).unwrap();
        history_append(
            &dir,
            &HistoryEntry {
                unix: 1_700_000_100,
                ..e
            },
        )
        .unwrap();
        // Una línea corrupta no rompe la lectura.
        let mut f = OpenOptions::new()
            .append(true)
            .open(dir.join("history.jsonl"))
            .unwrap();
        writeln!(f, "esto no es json").unwrap();
        let h = history_load(&dir);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0], e);
        let text = fs::read_to_string(dir.join("history.jsonl")).unwrap();
        assert!(
            !text.contains('\\') && !text.contains("path"),
            "el historial no lleva rutas"
        );
        history_clear(&dir).unwrap();
        assert!(history_load(&dir).is_empty());
        history_clear(&dir).unwrap(); // borrar lo que no existe no falla
    }
}
