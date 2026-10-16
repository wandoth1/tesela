//! Registro local de lo borrado (PLAN.md §3.7.9) y historial de resúmenes (§2.11).
//!
//! * `%LOCALAPPDATA%\Tesela\limpieza-<fecha>.jsonl`: una línea JSON por entrada procesada (ruta, identidad, tamaño,
//!   resultado). SOLO en la máquina del usuario: contiene rutas reales y nunca se sube ni se versiona. Se conservan los
//!   últimos [`KEEP_LOGS`] y cada uno tiene un tope de líneas ([`MAX_LINES`]) y de bytes ([`MAX_BYTES`]). Se vuelca a disco
//!   cada pocas líneas y tras cada elemento (si la aplicación se cierra a mitad, consta hasta dónde llegó). "Borrar el
//!   historial" de Ajustes los borra también ([`history_clear`]).
//! * `%LOCALAPPDATA%\Tesela\history.jsonl`: SOLO resúmenes (fecha, unidad, usado, liberado ahora, enviado a la
//!   Papelera). Nunca el árbol ni rutas. Se puede borrar desde Ajustes.

use crate::engine::{Event, EventSink, Outcome};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Registros que se conservan.
pub const KEEP_LOGS: usize = 30;
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

/// El escritor del registro de una ejecución.
pub struct LogWriter {
    path: PathBuf,
    out: Mutex<BufWriter<File>>,
    lines: AtomicU64,
    bytes: AtomicU64,
    truncated: AtomicBool,
    /// Tope de bytes (las pruebas lo bajan).
    max_bytes: AtomicU64,
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
    /// Crea el registro de una ejecución en `dir` (se crea si no existe) y poda los antiguos.
    pub fn create(dir: &Path, dry_run: bool) -> std::io::Result<LogWriter> {
        fs::create_dir_all(dir)?;
        let (y, mo, d, h, mi, s) = utc_parts(unix_now());
        let mut name = format!("limpieza-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}");
        if dry_run {
            name.push_str("-simulacion");
        }
        let mut path = dir.join(format!("{name}.jsonl"));
        let mut n = 1;
        while path.exists() {
            path = dir.join(format!("{name}-{n}.jsonl"));
            n += 1;
        }
        let f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        let w = LogWriter {
            path,
            out: Mutex::new(BufWriter::new(f)),
            lines: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            truncated: AtomicBool::new(false),
            max_bytes: AtomicU64::new(MAX_BYTES),
        };
        w.raw(&format!(
            "{{\"t\":\"start\",\"dry_run\":{dry_run},\"unix\":{}}}",
            unix_now()
        ));
        prune(dir, KEEP_LOGS);
        Ok(w)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn raw(&self, line: &str) {
        let mut g = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(g, "{line}");
        self.bytes
            .fetch_add(line.len() as u64 + 1, Ordering::Relaxed);
    }

    /// Escribe una entrada procesada.
    pub fn write_event(&self, ev: &Event) {
        let n = self.lines.fetch_add(1, Ordering::Relaxed);
        if n > MAX_LINES
            || self.bytes.load(Ordering::Relaxed) > self.max_bytes.load(Ordering::Relaxed)
        {
            // Una sola marca de truncado; después no se escribe más (el tope en líneas y en bytes).
            if !self.truncated.swap(true, Ordering::Relaxed) {
                self.raw("{\"t\":\"truncated\"}");
                self.flush();
            }
            return;
        }
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
            self.raw(&s);
        }
    }

    /// Una línea de texto libre del propio registro (resumen, avisos).
    pub fn write_note(&self, kind: &str, text: &str) {
        let v = serde_json::json!({ "t": kind, "text": text });
        self.raw(&v.to_string());
    }

    /// El receptor de eventos para el recorrido.
    pub fn sink(self: &Arc<Self>) -> Arc<EventSink> {
        let me = self.clone();
        Arc::new(move |ev| me.write_event(&ev))
    }

    pub fn flush(&self) {
        let _ = self.out.lock().unwrap_or_else(|e| e.into_inner()).flush();
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Deja los `keep` registros más recientes de `dir` (por nombre, que lleva la fecha).
fn prune(dir: &Path, keep: usize) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut logs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "jsonl")
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("limpieza-"))
        })
        .collect();
    logs.sort();
    if logs.len() > keep {
        for old in &logs[..logs.len() - keep] {
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
        assert!(n <= KEEP_LOGS + 1, "{n}");
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
