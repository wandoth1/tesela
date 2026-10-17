//! Local log of deletions (PLAN.md §3.7.9) and summary history (§2.11).
//!
//! * `%LOCALAPPDATA%\Tesela\limpieza-<date>.jsonl`: one JSON line per processed item (path, identity, size, result). It
//!   stays ONLY on the user's machine: it contains real paths and is never uploaded or versioned. The latest [`KEEP_LOGS`]
//!   are retained, each with a line limit ([`MAX_LINES`]) and byte limit ([`MAX_BYTES`]). "Clear history" in Settings also
//!   deletes these files ([`history_clear`]).
//!
//! # Names and retention (A-M2)
//! `limpieza-<YYYYMMDD>-<HHMMSS>-<three-digit sequence>[-simulacion].jsonl`. Pruning sorts by date, time, and sequence as
//! numbers (not by filename: a `-1` suffix sorts BEFORE the base name and could cause pruning to delete the newly created log).
//! It never deletes the log it just created and verifies that the file exists after creation.
//!
//! # An open log cannot be deleted externally (A2-M2)
//! The file is opened WITHOUT `FILE_SHARE_DELETE` (`FILE_SHARE_READ | FILE_SHARE_WRITE`): while a writer has it open, no
//! process can delete or rename it (sharing violation), including another writer pruning old logs. Previously, Rust's
//! default sharing allowed deletion; a run with an earlier timestamp could prune a log belonging to an active run. Its path
//! disappeared while the writer continued accepting writes with `ok()` still true, leaving a "logged" cleanup in a missing
//! file. Pruning tolerates this failure and skips the file (an in-use log is retained even if this temporarily exceeds the
//! limit); [`LogWriter::discard`] CLOSES the handle before deleting. Another handle can still read the file (`FILE_SHARE_READ`).
//!
//! # Retention (m2)
//! Real logs ([`KEEP_LOGS`]) and simulation logs (`-simulacion`, [`KEEP_SIM_LOGS`]) are separate classes with independent
//! retention. Each run prunes ONLY its own class: real cleanup prunes real logs; simulation (`--dry-run`) prunes simulation
//! logs. A simulation never touches a real log. Previously, 31 consecutive simulations could displace real logs (used to
//! reconstruct deletions) based on timestamp order.
//!
//! A log that was never used (execution was rejected before touching anything, for example `NeedsReconfirm`) is DELETED
//! ([`LogWriter::discard`]): it contains only the startup line and would take a retention slot away from a log with content.
//! This is the simplest safe approach: there is no need to decide when logging "starts" or exclude files from pruning.
//!
//! # Log guarantees (M4)
//! * **No log, no destructive action.** The executor requires a log (except in simulation) and verifies it is usable.
//! * **Write errors are never swallowed.** The first write or flush error marks the log as failed ([`LogWriter::ok`]) and
//!   sets the executor's cancellation flag. Cleanup stops at the next safe point (between entries, operations, or Recycle
//!   Bin batches) and does not touch anything else.
//! * **Intent first, result after.** Before every operation, a log intent is written and FLUSHED ([`LogWriter::write_intent`]:
//!   the permanent traversal and its roots; each batch sent to the shell and its items). Then each entry's result is written
//!   and flushed immediately, with errors checked (A-M3): no batch of results remains buffered in memory. After a forced
//!   shutdown, scope can be reconstructed from the operation intent (roots, with identity in the plan) and recorded results.
//!   Only the result for work IN FLIGHT at that moment may be missing (at most one entry per worker thread; for shell
//!   operations, the entire batch, whose list is in its intent). There is no per-entry intent: it would duplicate writes to
//!   identify the in-flight entry, not its scope, which is already recorded.
//! * **Flush and `sync_all`.** `flush` (bytes reach the system cache, which survives process termination or forced window
//!   close, the modeled threat) follows each intent and result. `sync_all` (to disk) runs only when creating the log and at
//!   the end, avoiding an `fsync` for every item (milliseconds per call, thousands of calls) when a mid-run power failure
//!   does not justify the cost.
//! * `%LOCALAPPDATA%\Tesela\history.jsonl`: summaries ONLY (date, drive, used, freed now, sent to the Recycle Bin). Never
//!   the tree or paths. It can be cleared from Settings.

use crate::engine::{Event, EventSink, Outcome};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// `FILE_SHARE_READ | FILE_SHARE_WRITE`: the file can be read (and another writer opened), but NOT deleted or renamed while
/// the log is open (A2-M2).
const SHARE_READ_WRITE: u32 = 0x1 | 0x2;

/// Number of REAL logs to retain.
pub const KEEP_LOGS: usize = 30;
/// Number of SIMULATION (`--dry-run`) logs to retain. Pruned separately from real logs (m2).
pub const KEEP_SIM_LOGS: usize = 10;
/// Maximum log lines (a huge `%TEMP%` can contain hundreds of thousands of files).
pub const MAX_LINES: u64 = 250_000;
/// Maximum log bytes (with long paths, 250,000 lines could exceed one hundred MiB).
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// UTC date and time from Unix seconds (H. Hinnant's civil date algorithm).
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

#[cfg(test)]
type AfterPrune = std::cell::RefCell<Option<Box<dyn Fn(&Path)>>>;

#[cfg(test)]
thread_local! {
    /// TESTS ONLY (s-4): runs in `create_at` after pruning and before checking that the log exists.
    static TEST_AFTER_PRUNE: AfterPrune = const { std::cell::RefCell::new(None) };
}

/// Log file. Tests can make it fail deliberately after `left` bytes.
enum LogFile {
    Real(File),
    /// The log was discarded (A2-M2): its handle was closed so the file could be deleted. Writes return an error (the log
    /// no longer exists); flush does nothing.
    Closed,
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
            LogFile::Closed => Err(std::io::Error::other("log was discarded")),
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
            LogFile::Closed => Ok(()),
            #[cfg(test)]
            LogFile::FailAfter { file, .. } => file.flush(),
        }
    }
}

impl LogFile {
    fn sync_all(&self) -> std::io::Result<()> {
        match self {
            LogFile::Real(f) => f.sync_all(),
            LogFile::Closed => Err(std::io::Error::other("log was discarded")),
            #[cfg(test)]
            LogFile::FailAfter { file, .. } => file.sync_all(),
        }
    }
}

/// Writer for one run's log.
pub struct LogWriter {
    path: PathBuf,
    out: Mutex<BufWriter<LogFile>>,
    lines: AtomicU64,
    bytes: AtomicU64,
    truncated: AtomicBool,
    /// Byte limit (lowered by tests).
    max_bytes: AtomicU64,
    /// Line limit (lowered only by tests).
    max_lines: AtomicU64,
    /// The first write or flush error permanently marks the log as failed.
    failed: AtomicBool,
    first_error: Mutex<Option<String>>,
    /// Executor cancellation flag: a write error sets it (cleanup stops at the next safe point).
    cancel_on_error: Mutex<Option<Arc<AtomicBool>>>,
    /// Has anything for an OPERATION been written yet (an intent, entry, `bin-item`, or result)? Until then, the log only
    /// contains its startup line and `rejected` notes, so it can be discarded (R2).
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
    /// Creates a run log in `dir` (creating the directory if needed) and prunes old logs. Failure to create the file, write
    /// its startup line, or flush it with `sync_all` is an error: cleanup cannot proceed without a log.
    pub fn create(dir: &Path, dry_run: bool) -> std::io::Result<LogWriter> {
        Self::create_with(dir, dry_run, LogFile::Real)
    }

    /// TESTS ONLY: a log whose file fails writes after `bytes` bytes.
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

    /// Like `create_with`, with a given Unix timestamp: tests fix the second to exercise collisions.
    fn create_at(
        dir: &Path,
        dry_run: bool,
        wrap: impl FnOnce(File) -> LogFile,
        now: u64,
    ) -> std::io::Result<LogWriter> {
        fs::create_dir_all(dir)?;
        let (y, mo, d, h, mi, sec) = utc_parts(now);
        let stamp = format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{sec:02}");
        // The name sorts correctly as both text and numbers (A-M2): `limpieza-<date>-<time>-<three-digit sequence>[-simulacion]`.
        // The sequence is one greater than the largest already used in that second (not the first gap: if 001 was pruned,
        // the new name must not reuse 001 and sort before an older one). A previous collision suffix `-1` sorted BEFORE the
        // base name (`-` < `.`), so pruning could delete the newly created log.
        let mut n = next_sequence(dir, &stamp);
        let sim = if dry_run { "-simulacion" } else { "" };
        let (path, f) = loop {
            let path = dir.join(format!("limpieza-{stamp}-{n:03}{sim}.jsonl"));
            // Without `FILE_SHARE_DELETE` (A2-M2), no one can delete or rename the log while it is open.
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .share_mode(SHARE_READ_WRITE)
                .open(&path)
            {
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
            max_lines: AtomicU64::new(MAX_LINES),
            failed: AtomicBool::new(false),
            first_error: Mutex::new(None),
            cancel_on_error: Mutex::new(None),
            operation_written: AtomicBool::new(false),
        };
        w.raw(&format!(
            "{{\"t\":\"start\",\"dry_run\":{dry_run},\"unix\":{}}}",
            unix_now()
        ))?;
        // The startup line is on disk (`sync_all`): even if the application exits immediately, the run is recorded.
        w.sync()?;
        // Each run prunes ONLY its own class: a simulation never touches a real log (m2). The newly created log is EXCLUDED
        // from pruning (A-M2), so it can never delete itself because of a future timestamp (clock skew) or a misordered name.
        prune(
            dir,
            dry_run,
            if dry_run { KEEP_SIM_LOGS } else { KEEP_LOGS },
            Some(&w.path),
        );
        // TESTS ONLY: the interval between pruning and verification (a hook can delete the log here).
        #[cfg(test)]
        TEST_AFTER_PRUNE.with(|h| {
            if let Some(f) = &*h.borrow() {
                f(&w.path);
            }
        });
        // Verify it: a missing log is not a log, and cleanup cannot proceed without one.
        ensure_exists(&w.path)?;
        Ok(w)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Marks the log as failed (saving the first error) and triggers executor cancellation.
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

    /// Writes a RESULT line and flushes it to the system within the same critical section, checking both operations for errors
    /// (A-M3): when this returns, another handle can read the line; if either operation failed, the log is marked failed and
    /// cleanup is cancelled at the next safe point.
    fn raw_flushed(&self, line: &str) -> std::io::Result<()> {
        let r = {
            let mut g = self.out.lock().unwrap_or_else(|e| e.into_inner());
            writeln!(g, "{line}").and_then(|()| g.flush())
        };
        self.bytes
            .fetch_add(line.len() as u64 + 1, Ordering::Relaxed);
        self.checked(r)
    }

    /// Deletes the log file for a run rejected before touching anything (see the module documentation). This is allowed ONLY
    /// if no operation data (intent, entry, `bin-item`, or result) was written; otherwise it returns an error and preserves
    /// the file (R2). First CLOSES the handle (A2-M2: the file was opened without `FILE_SHARE_DELETE`, so it cannot be
    /// deleted while open), then deletes it. Writes to a discarded log return an error.
    pub(crate) fn discard(&self) -> std::io::Result<()> {
        if self.operation_written.load(Ordering::SeqCst) {
            return Err(std::io::Error::other(
                "log already contains operations and cannot be discarded",
            ));
        }
        {
            let mut g = self.out.lock().unwrap_or_else(|e| e.into_inner());
            // Drop the previous writer (flush pending data and close its handle), then replace it with a closed writer.
            drop(std::mem::replace(&mut *g, BufWriter::new(LogFile::Closed)));
        }
        // H6a: a discarded writer is no longer a valid log: `ok()` reports this immediately (without waiting for a failing
        // write). Executor cancellation is not triggered; discarding is the intended result of rejecting a run.
        self.failed.store(true, Ordering::SeqCst);
        match fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Has the log reached its line or byte limit and stopped recording? Cleanup continues, but later events are not logged.
    pub fn truncated(&self) -> bool {
        self.truncated.load(Ordering::Relaxed)
    }

    /// TESTS ONLY: lowers the byte limit.
    #[cfg(test)]
    pub(crate) fn set_max_bytes_for_test(&self, n: u64) {
        self.max_bytes.store(n, Ordering::Relaxed);
    }

    /// Is the log healthy? Returns `false` as soon as a write or flush fails (later actions can no longer be trusted to be
    /// recorded).
    pub fn ok(&self) -> bool {
        !self.failed.load(Ordering::SeqCst)
    }

    /// The first write or flush error, if any.
    pub fn error(&self) -> Option<String> {
        self.first_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Makes a write error set `flag` (executor cancellation). If the log had already failed, sets the flag immediately.
    pub fn cancel_on_error(&self, flag: Arc<AtomicBool>) {
        if !self.ok() {
            flag.store(true, Ordering::SeqCst);
        }
        *self
            .cancel_on_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(flag);
    }

    /// Flushes written data to the system (and `sync_all` to disk). An error marks the log as failed.
    pub fn sync(&self) -> std::io::Result<()> {
        let r = {
            let mut g = self.out.lock().unwrap_or_else(|e| e.into_inner());
            g.flush().and_then(|()| g.get_ref().sync_all())
        };
        self.checked(r)
    }

    /// Writes and flushes an INTENT line before the operation: `kind` and the list of paths to be touched. If writing or
    /// flushing fails, returns an error and the operation MUST NOT proceed.
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

    /// One Recycle Bin item purged or skipped: source path, `id`, size, and result.
    pub fn write_bin_item(&self, original: &Path, id: &OsStr, size: u64, out: &str) {
        self.operation_written.store(true, Ordering::SeqCst);
        let v = serde_json::json!({
            "t": "bin-item",
            "path": original.to_string_lossy(),
            "id": id.to_string_lossy(),
            "bytes": size,
            "out": out,
        });
        let _ = self.raw_flushed(&v.to_string());
    }

    /// Like [`LogWriter::flush`], but returns any error.
    pub fn flush_checked(&self) -> std::io::Result<()> {
        let r = self.out.lock().unwrap_or_else(|e| e.into_inner()).flush();
        self.checked(r)
    }

    /// Counts a line and checks whether the log has exceeded its line or byte limit. If so, writes ONE truncation marker
    /// and records nothing else. Entries and `rejected` notes share this check so neither can exceed the limit (R4).
    ///
    /// GM-S1: count and compare INSIDE the writer lock (previously, both ran outside it, so several threads could read the
    /// same value and exceed the limit), using `>=`: with `n > MAX`, one extra line (`MAX + 1`) was written. The lock is
    /// released before the caller writes its line; its slot has already been reserved.
    fn over_cap(&self) -> Option<u64> {
        let mut g = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let n = self.lines.fetch_add(1, Ordering::Relaxed);
        if n >= self.max_lines.load(Ordering::Relaxed)
            || self.bytes.load(Ordering::Relaxed) > self.max_bytes.load(Ordering::Relaxed)
        {
            if !self.truncated.swap(true, Ordering::Relaxed) {
                // Write the marker with the lock already held (`raw` would try to acquire it again).
                let line = "{\"t\":\"truncated\"}";
                let r = writeln!(g, "{line}").and_then(|()| g.flush());
                self.bytes
                    .fetch_add(line.len() as u64 + 1, Ordering::Relaxed);
                let _ = self.checked(r);
            }
            return None;
        }
        Some(n)
    }

    /// TESTS ONLY: lowers the line limit.
    #[cfg(test)]
    pub(crate) fn set_max_lines_for_test(&self, n: u64) {
        self.max_lines.store(n, Ordering::Relaxed);
    }

    /// A note about something that did NOT make it into the plan (`rejected`), subject to the same limits and truncation
    /// behavior as entries. It does not count as a written operation (R2).
    pub fn write_rejected(&self, path: &Path, reason: &str) {
        if self.over_cap().is_none() {
            return;
        }
        let v =
            serde_json::json!({ "t": "rejected", "path": path.to_string_lossy(), "text": reason });
        let _ = self.raw(&v.to_string());
    }

    /// Writes a processed item.
    pub fn write_event(&self, ev: &Event) {
        self.operation_written.store(true, Ordering::SeqCst);
        let Some(_) = self.over_cap() else {
            return;
        };
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
        // A-M3: each entry result is flushed to the system as it is written (previously, up to 256 were buffered).
        if let Ok(s) = serde_json::to_string(&line) {
            let _ = self.raw_flushed(&s);
        }
    }

    /// A free-text line written by the log itself (summary or warnings).
    pub fn write_note(&self, kind: &str, text: &str) {
        self.operation_written.store(true, Ordering::SeqCst);
        let v = serde_json::json!({ "t": kind, "text": text });
        let _ = self.raw_flushed(&v.to_string());
    }

    /// Event sink for the traversal.
    pub fn sink(self: &Arc<Self>) -> Arc<EventSink> {
        let me = self.clone();
        Arc::new(move |ev| me.write_event(&ev))
    }

    /// Flushes written data to the system. An error marks the log as failed (see [`LogWriter::ok`]).
    pub fn flush(&self) {
        let _ = self.flush_checked();
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Is this a simulation log? (`limpieza-<date>-simulacion[-n].jsonl`).
fn is_sim_log(name: &str) -> bool {
    name.contains("-simulacion")
}

/// Sort key for a log by NAME: (date, time, sequence), parsed as NUMBERS. Understands
/// `limpieza-<YYYYMMDD>-<HHMMSS>[-<sequence>][-simulacion].jsonl` (older logs used `-1`, `-2`, etc.; new ones use `-001`).
/// Unrecognized names sort as oldest (and among themselves by name).
fn log_key(name: &str) -> (u64, u64, u64, String) {
    let parsed = (|| {
        let rest = name.strip_prefix("limpieza-")?;
        let (date, rest) = rest.split_once('-')?;
        let time = rest.get(..6)?;
        let rest = rest[6..].strip_suffix(".jsonl").unwrap_or(&rest[6..]);
        if date.len() != 8
            || !date.bytes().all(|b| b.is_ascii_digit())
            || !time.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        // What follows the time is split into `-`-separated parts: a numeric sequence and `simulacion`, in either order
        // (v0.1.1 names use `...-simulacion-1`; new names use `...-001-simulacion`).
        let seq = rest
            .split('-')
            .find(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|t| t.parse::<u64>().ok())
            .unwrap_or(0);
        Some((date.parse::<u64>().ok()?, time.parse::<u64>().ok()?, seq))
    })();
    let (d, t, q) = parsed.unwrap_or((0, 0, 0));
    (d, t, q, name.to_lowercase())
}

/// Next sequence for the `stamp` second (`YYYYMMDD-HHMMSS`): one greater than the largest in `dir` (1 if none exist).
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

/// Retains the `keep` newest logs of ONE class in `dir` (`sim`: simulation logs; otherwise real logs), sorted by the date,
/// time, and sequence in the name ([`log_key`]). The other class is untouched, and `active` (the newly created log, counted
/// as one of the `keep`) is NEVER deleted.
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
    // The active log occupies one slot: retain `keep - 1` others (or `keep` if there is no active log).
    let allowed = if active.is_some() {
        keep.saturating_sub(1)
    } else {
        keep
    };
    if logs.len() > allowed {
        for old in &logs[..logs.len() - allowed] {
            // A2-M2: a log held open by ANOTHER writer cannot be deleted (it is opened without `FILE_SHARE_DELETE`): sharing
            // or access errors are tolerated and the file is skipped until it is no longer in use. Other errors do not stop
            // pruning either.
            let _ = fs::remove_file(old);
        }
    }
}

/// A newly created log must still exist: a missing log is not a log. Since A2-M2, an open file cannot be deleted externally,
/// but this check remains as defense in depth.
fn ensure_exists(path: &Path) -> std::io::Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(std::io::Error::other("newly created log no longer exists"))
    }
}

/// A history summary (without paths).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// When (Unix seconds).
    pub unix: u64,
    /// Drive letter (0 if this was a folder).
    pub drive: u8,
    /// Bytes used on the volume during cleanup.
    pub used: u64,
    /// "Freed now" (measured).
    pub freed_now: u64,
    /// Sent to the Recycle Bin.
    pub to_bin: u64,
}

/// Appends a summary to `history.jsonl`.
pub fn history_append(dir: &Path, e: &HistoryEntry) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("history.jsonl"))?;
    let line = serde_json::to_string(e).map_err(std::io::Error::other)?;
    writeln!(f, "{line}")
}

/// Reads history (ignoring lines it cannot parse).
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

/// Deletes history and cleanup logs (`limpieza-*.jsonl`, which contain paths) from the data directory (Settings: "Clear
/// history"). Only these names are touched, and logs are not followed if they are links.
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
            // The entry's `file_type` does not follow links: a link with that name is left untouched.
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
        // Simulation does NOT prune (m2): all 35 old logs remain.
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
        // A REAL cleanup prunes its real logs: `KEEP_LOGS` remain (including the new one), and simulation logs are preserved.
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

    /// m2 and H6.6: with 31 real logs and one simulation, all 31 real logs remain (simulation never touches a real log).
    /// Simulations are pruned ONLY among themselves (to `KEEP_SIM_LOGS`); real cleanup prunes only real logs.
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
        drop(sim);
        // 40 consecutive simulations (names get a suffix within the same second): real logs stay intact and simulation logs
        // are pruned among themselves. Each run closes before the next: since A2-M2, another writer cannot prune an OPEN log.
        for _ in 0..40 {
            drop(LogWriter::create(&dir, true).unwrap());
        }
        assert_eq!(count(&dir, false), 31);
        assert_eq!(count(&dir, true), KEEP_SIM_LOGS);
        // Real cleanup prunes ONLY real logs (to `KEEP_LOGS`) and leaves simulation logs unchanged.
        let _real = LogWriter::create(&dir, false).unwrap();
        assert_eq!(count(&dir, false), KEEP_LOGS);
        assert_eq!(count(&dir, true), KEEP_SIM_LOGS);
    }

    /// A-M3: MEASUREMENT (not a test: `cargo test -p tesela-cleaner medicion_a_m3 -- --ignored --nocapture`). Cost of logging
    /// 100,000 item results: only the writer, with typical-length paths.
    #[test]
    #[ignore = "medición A-M3"]
    fn medicion_a_m3_100000_resultados_en_el_escritor() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        let log = LogWriter::create(&dir, false).unwrap();
        let t = std::time::Instant::now();
        for i in 0..100_000usize {
            log.write_event(&event(i, 90));
        }
        log.flush();
        let dt = t.elapsed();
        eprintln!(
            "MEDICION escritor: 100000 resultados en {:.3} s ({:.2} us por resultado), {} bytes",
            dt.as_secs_f64(),
            dt.as_secs_f64() * 1e6 / 100_000.0,
            fs::metadata(log.path()).unwrap().len()
        );
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

    /// A-M2: with logs DATED IN THE FUTURE (clock ahead, copied from another machine), the new log sorted first and pruning
    /// immediately deleted it: cleanup continued with an unlinked file and `ok()` returning `true`. Pruning now excludes it,
    /// and its existence is checked after creation.
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
        // Retention is met (30 real logs including the new one), and the new one is among them.
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
        // Prune the OLDEST of the others (by numeric date): the earliest-dated ones.
        assert!(!dir.join("limpieza-29990101-000000-001.jsonl").exists());
        assert!(dir.join("limpieza-29990101-000034-001.jsonl").exists());
    }

    /// A-M2: two logs in the SAME second. Previously, the second was named `...-1.jsonl`, which sorts BEFORE the base name
    /// (`-` < `.`), so pruning deleted it as soon as needed. Names now use a correctly sorting three-digit sequence, set to
    /// the largest sequence for that second plus one (not the first gap). The clock is fixed for determinism.
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
        // The KEEP_LOGS newest logs survive, not the oldest.
        for p in &made[..6] {
            assert!(!p.exists(), "{p:?}");
        }
        for p in &made[6..] {
            assert!(p.exists(), "{p:?}");
        }
        // The sequence continues after pruning: the next is the largest plus one, not a gap.
        let w = LogWriter::create_at(&dir, false, LogFile::Real, NOW).unwrap();
        let seq = log_key(&w.path().file_name().unwrap().to_string_lossy()).2;
        assert_eq!(seq as usize, KEEP_LOGS + 6 + 1);
    }

    /// A-M2: filename ordering parses date, time, and sequence numerically: the base name precedes `-1`, `-2`, and `-10`
    /// (older-version names), as well as `-001`; unrecognized names sort oldest.
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

    /// A-M2: older logs (`...-1.jsonl`, `...-2.jsonl`, `...-10.jsonl`) coexist with new ones: pruning uses NUMERIC order
    /// (base, `-1`, `-2`... `-34`), not lexicographic order (`-1`, `-10`, `-11`... `-2`, which deleted recent logs), and
    /// the new log continues the sequence (35).
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

    /// s-2: colliding SIMULATION log names from the older version (`...-simulacion-1.jsonl`) are parsed with their sequence,
    /// sorted after the base name in numeric order, and pruned accordingly.
    #[test]
    fn los_nombres_de_simulacion_de_la_version_anterior_ordenan_por_su_secuencia() {
        let k = |n: &str| log_key(n);
        let ordered = [
            "limpieza-20260101-120000-simulacion.jsonl",
            "limpieza-20260101-120000-simulacion-1.jsonl",
            "limpieza-20260101-120000-simulacion-2.jsonl",
            "limpieza-20260101-120000-simulacion-10.jsonl",
            "limpieza-20260101-120000-011-simulacion.jsonl",
            "limpieza-20260101-120001-001-simulacion.jsonl",
        ];
        for w in ordered.windows(2) {
            assert!(k(w[0]) < k(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(k("limpieza-20260101-120000-simulacion-10.jsonl").2, 10);
        assert_eq!(k("limpieza-20260101-120000-011-simulacion.jsonl").2, 11);
        assert_eq!(k("limpieza-20260101-120000-simulacion.jsonl").2, 0);
        // Pruned: 15 older simulations from the same second (base, -1... -14) and one new log; the newest survive.
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        const NOW: u64 = 1_767_268_800;
        fs::write(dir.join("limpieza-20260101-120000-simulacion.jsonl"), b"{}").unwrap();
        for n in 1..=14 {
            fs::write(
                dir.join(format!("limpieza-20260101-120000-simulacion-{n}.jsonl")),
                b"{}",
            )
            .unwrap();
        }
        let w = LogWriter::create_at(&dir, true, LogFile::Real, NOW).unwrap();
        assert!(w.path().is_file());
        let sims: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(sims.len(), KEEP_SIM_LOGS, "{sims:?}");
        // Keep -6 ... -14 (9) and the new log; the base and -1 ... -5 are the oldest.
        for gone in std::iter::once("limpieza-20260101-120000-simulacion.jsonl".to_string())
            .chain((1..=5).map(|n| format!("limpieza-20260101-120000-simulacion-{n}.jsonl")))
        {
            assert!(!dir.join(&gone).exists(), "{gone}");
        }
        assert!(
            dir.join("limpieza-20260101-120000-simulacion-14.jsonl")
                .exists()
        );
    }

    /// s-4 + A2-M2: nobody can delete the newly created log between pruning and the existence check: it is open without
    /// `FILE_SHARE_DELETE`, so the attempt (the test hook) fails with a sharing error and `create` returns a writer whose file
    /// remains. (Previously, the hook deleted it and `create` failed at the check; that check remains as defense in depth:
    /// see `ensure_exists`.)
    #[test]
    fn un_registro_recien_creado_no_se_puede_borrar_antes_de_la_comprobacion() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        let tried = std::rc::Rc::new(std::cell::Cell::new(None::<i32>));
        let t2 = tried.clone();
        TEST_AFTER_PRUNE.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |p: &Path| {
                t2.set(Some(
                    fs::remove_file(p)
                        .err()
                        .and_then(|e| e.raw_os_error())
                        .unwrap_or(0),
                ));
            }));
        });
        let r = LogWriter::create(&dir, false);
        TEST_AFTER_PRUNE.with(|h| *h.borrow_mut() = None);
        let w = r.expect("el registro sigue existiendo");
        assert!(w.path().is_file() && w.ok());
        assert_eq!(
            tried.get(),
            Some(32),
            "borrarlo da ERROR_SHARING_VIOLATION (el gancho llegó a correr)"
        );
    }

    /// A2-M2: the existence check remains (defense in depth): a missing log is an error.
    #[test]
    fn la_comprobacion_de_existencia_falla_con_un_registro_que_no_existe() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        let w = LogWriter::create(&dir, false).unwrap();
        assert!(ensure_exists(w.path()).is_ok());
        assert!(ensure_exists(&dir.join("limpieza-20000101-000000-001.jsonl")).is_err());
    }

    /// A log filename with the given Unix timestamp and sequence 001.
    fn log_name_at(unix: u64) -> String {
        let (y, mo, d, h, mi, s) = utc_parts(unix);
        format!("limpieza-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}-001.jsonl")
    }

    /// A2-M2 (external audit blocker), Astra reproduction: 29 future-dated logs; writer A opens its log (the oldest future
    /// one) and writes an intent; writer B opens with an EARLIER date and sees A as the oldest excess log. Previously B deleted
    /// it: A's path disappeared while it still accepted writes with `ok()` returning true. A is now opened without
    /// `FILE_SHARE_DELETE`: B's prune fails to delete it and skips it, leaving A valid. Once A closes, the next prune can remove it.
    #[test]
    fn un_registro_abierto_no_lo_borra_la_poda_de_otro_escritor() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        const FUT: u64 = 1_900_000_000;
        const NOW: u64 = 1_760_000_000;
        for k in 1..=29u64 {
            fs::write(dir.join(log_name_at(FUT + k * 86_400)), b"{}").unwrap();
        }
        let a = LogWriter::create_at(&dir, false, LogFile::Real, FUT).unwrap();
        a.write_intent("delete-begin", &[PathBuf::from("primera")])
            .unwrap();
        let a_path = a.path().to_path_buf();
        // B opens with an earlier date: pruning sees 30 other logs (the 29 plus A), one too many: A, the oldest.
        let b = LogWriter::create_at(&dir, false, LogFile::Real, NOW).unwrap();
        assert!(a_path.is_file(), "la poda de B no borró el registro de A");
        assert!(a.ok(), "A sigue siendo un registro válido");
        a.write_intent("recycle-begin", &[PathBuf::from("segunda")])
            .unwrap();
        a.flush();
        let text = fs::read_to_string(&a_path).unwrap();
        assert!(
            text.contains("primera") && text.contains("segunda"),
            "A sigue escribiendo en SU archivo: {text}"
        );
        assert!(b.path().is_file() && b.ok());
        assert_eq!(real_logs(&dir).len(), 29 + 2, "los 29, A y B");
        // After A finishes, a third writer prunes it (B remains open and is skipped; A is closed and deleted).
        drop(a);
        let c = LogWriter::create_at(&dir, false, LogFile::Real, NOW + 1).unwrap();
        assert!(!a_path.exists(), "ya cerrado, la retención lo retira");
        assert!(b.path().is_file() && c.path().is_file());
    }

    /// A2-M2: an open log can be READ through another handle (`FILE_SHARE_READ`, as the UI and tests do), but cannot be
    /// deleted or renamed while open.
    #[test]
    fn un_registro_abierto_se_lee_pero_no_se_borra_ni_se_renombra() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        let w = LogWriter::create(&dir, false).unwrap();
        w.write_intent("delete-begin", &[PathBuf::from("x")])
            .unwrap();
        assert!(
            fs::read_to_string(w.path())
                .unwrap()
                .contains("delete-begin")
        );
        let code = |e: std::io::Error| e.raw_os_error();
        assert_eq!(code(fs::remove_file(w.path()).unwrap_err()), Some(32));
        assert_eq!(
            code(fs::rename(w.path(), dir.join("otro.jsonl")).unwrap_err()),
            Some(32)
        );
        assert!(w.path().is_file() && w.ok());
        // Once closed, it can be deleted again.
        let path = w.path().to_path_buf();
        drop(w);
        fs::remove_file(&path).unwrap();
    }

    /// A2-M2: after `discard`, the log no longer exists and the writer is closed: writing to it returns an error (the file was
    /// deleted; there is no unlinked writer with `ok()` returning true that accepts writes). Discard also works with another
    /// READ handle open (sharing deletion, as `File::open` does).
    #[test]
    fn descartar_cierra_el_handle_antes_de_borrar_y_el_escritor_queda_cerrado() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        let w = LogWriter::create(&dir, false).unwrap();
        let path = w.path().to_path_buf();
        let reader = File::open(&path).unwrap();
        assert!(w.discard().is_ok());
        assert!(!w.ok(), "descartado: ya no es un registro válido");
        drop(reader);
        assert!(!path.exists(), "descartado");
        // A write after discard must not succeed.
        w.write_rejected(Path::new("x"), "ReparseAncestor");
        assert!(w.flush_checked().is_err());
        assert!(
            !w.ok(),
            "un escritor descartado ya no es un registro válido"
        );
        assert!(!path.exists(), "y no reaparece");
    }

    /// A2-M2: "Clear history" cannot delete an open log (the ongoing cleanup needs it): it skips the log (returns a sharing
    /// error, which the UI ignores) and deletes everything else.
    #[test]
    fn borrar_el_historial_no_borra_el_registro_abierto() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        fs::write(dir.join("limpieza-20000101-000000-001.jsonl"), b"{}").unwrap();
        fs::write(dir.join("history.jsonl"), b"{}").unwrap();
        let w = LogWriter::create(&dir, false).unwrap();
        let r = history_clear(&dir);
        assert!(r.is_err(), "el registro abierto no se pudo borrar");
        assert!(
            w.path().is_file() && w.ok(),
            "sigue siendo un registro válido"
        );
        assert!(!dir.join("limpieza-20000101-000000-001.jsonl").exists());
        assert!(!dir.join("history.jsonl").exists());
        drop(w);
        assert!(history_clear(&dir).is_ok(), "cerrado, ya se puede borrar");
    }

    /// A-M2: `prune` never deletes the active log, even if it sorts first, and reserves a slot for it.
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
        // Without an active log, KEEP_LOGS of the others are retained.
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

    /// H6.8: a discarded log disappears (no file containing only the startup line remains to occupy a slot).
    #[test]
    fn un_registro_descartado_no_deja_archivo() {
        let sb = Sandbox::new();
        let dir = sb.mkdir("Tesela");
        let log = LogWriter::create(&dir, false).unwrap();
        let path = log.path().to_path_buf();
        assert!(path.exists());
        // `rejected` notes are not operations: the log can be discarded.
        log.write_rejected(Path::new("x"), "ReparseAncestor");
        assert!(log.discard().is_ok());
        assert!(!path.exists());
        // R2: once an operation is written (intent, item, `bin-item`, or result), do NOT delete the log; return an error.
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

    /// GM-S1: the line limit is EXACT. With a limit of `N`, there are `N` entries and one truncation marker: previously one
    /// extra entry was written (`n > MAX`), and with multiple threads the count and comparison outside the lock allowed more.
    #[test]
    fn el_tope_de_lineas_es_exacto_tambien_con_varios_hilos() {
        for threads in [1usize, 8] {
            let sb = Sandbox::new();
            let dir = sb.mkdir("Tesela");
            let log = Arc::new(LogWriter::create(&dir, false).unwrap());
            log.set_max_lines_for_test(25);
            std::thread::scope(|s| {
                for t in 0..threads {
                    let log = log.clone();
                    s.spawn(move || {
                        for i in 0..200 / threads {
                            log.write_event(&event(t * 1_000 + i, 5));
                        }
                    });
                }
            });
            log.flush();
            let text = fs::read_to_string(log.path()).unwrap();
            assert!(log.truncated(), "{threads}");
            assert_eq!(text.matches("\"t\":\"entry\"").count(), 25, "{threads}");
            assert_eq!(text.matches("\"truncated\"").count(), 1, "{threads}");
        }
    }

    /// R4: `rejected` notes obey the same limit and truncation behavior as entries.
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
        // No explicit caller `flush()`: the startup line is already in the file.
        assert!(
            fs::read_to_string(log.path())
                .unwrap()
                .contains("\"start\""),
            "falta la línea de arranque"
        );
        let paths = vec![PathBuf::from("a.txt"), PathBuf::from("b c.txt")];
        log.write_intent("recycle-begin", &paths).unwrap();
        // A-m1: WITHOUT a compensating test `flush()` (there used to be one, allowing a mutant that removed the flush from
        // `write_intent` to survive), the intent is already in the file on return.
        let text = fs::read_to_string(log.path()).unwrap();
        let line = text.lines().find(|l| l.contains("recycle-begin")).unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["paths"], serde_json::json!(["a.txt", "b c.txt"]));
    }

    /// A-M3: the result of EACH item is readable through another handle as soon as it is written, with NO caller `flush()`.
    /// Previously, up to 256 lines accumulated in the buffer: a forced close could lose up to 255 results for deleted items.
    /// A mutant removing the flush from `write_event` (or `write_note`, `write_bin_item`) fails here.
    #[test]
    fn cada_resultado_es_legible_desde_otro_handle_sin_volcar() {
        let sb = Sandbox::new();
        let dir = sb.path("Tesela");
        let log = Arc::new(LogWriter::create(&dir, false).unwrap());
        // Open another handle once; read whatever is in the file each time.
        let mut reader = File::open(log.path()).unwrap();
        let lines_now = |r: &mut File| {
            use std::io::{Read, Seek};
            r.seek(std::io::SeekFrom::Start(0)).unwrap();
            let mut text = String::new();
            r.read_to_string(&mut text).unwrap();
            text.lines().map(str::to_owned).collect::<Vec<_>>()
        };
        let before = lines_now(&mut reader).len();
        for i in 0..40usize {
            log.write_event(&event(i, 20));
            let lines = lines_now(&mut reader);
            assert_eq!(lines.len(), before + i + 1, "tras el resultado {i}");
            let v: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
            assert_eq!(
                (v["t"].as_str(), v["unit"].as_u64()),
                (Some("entry"), Some(i as u64))
            );
        }
        // Result notes and Recycle Bin items too.
        let n = lines_now(&mut reader).len();
        log.write_note("recycle", "x -> Recycled");
        assert_eq!(lines_now(&mut reader).len(), n + 1);
        log.write_bin_item(Path::new("a"), OsStr::new("id"), 1, "purged");
        assert_eq!(lines_now(&mut reader).len(), n + 2);
        assert!(log.ok());
    }

    /// A-M3: an error writing or flushing a result MARKS the log as failed and triggers cancellation (it is not swallowed):
    /// cleanup stops at the next safe point.
    #[test]
    fn un_error_al_volcar_un_resultado_marca_el_registro_y_cancela() {
        let sb = Sandbox::new();
        let dir = sb.path("Tesela");
        // The budget covers the startup line (about 50 bytes) and little else: the first long result fails.
        let log = LogWriter::create_failing_after(&dir, false, 120).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        log.cancel_on_error(cancel.clone());
        assert!(log.ok() && !cancel.load(Ordering::SeqCst));
        for i in 0..5usize {
            log.write_event(&event(i, 200));
        }
        assert!(!log.ok(), "el fallo consta");
        assert!(cancel.load(Ordering::SeqCst), "y la limpieza se cancela");
        assert!(log.error().is_some());
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
        // Non-Tesela files or files with a different name remain: another file, another extension, and a same-named directory.
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
        // A corrupt line does not break reading.
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
