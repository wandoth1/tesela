//! Utilidades compartidas por los binarios del spike (solo medición, sin lógica de producto).

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Convierte una ruta/cadena a UTF-16 terminado en NUL.
pub fn to_wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Memoria del proceso actual, en bytes.
#[derive(Debug, Clone, Copy)]
pub struct MemInfo {
    /// Working set (lo que muestra `Get-Process`.WorkingSet64).
    pub working_set: u64,
    /// Pico del working set.
    pub peak_working_set: u64,
    /// Private bytes (`PrivateUsage`, memoria comprometida privada).
    pub private_bytes: u64,
    /// Private working set (`PrivateWorkingSetSize`): páginas residentes NO compartidas. Es la columna "Memoria" del
    /// Administrador de tareas (lo que el usuario compara) y excluye las DLL compartidas (drivers de GPU, runtime).
    pub private_working_set: u64,
}

/// Lee `GetProcessMemoryInfo` del proceso actual.
pub fn mem_info() -> MemInfo {
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS_EX2 = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
        let ok = GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut _, c.cb);
        if ok == 0 {
            panic!("GetProcessMemoryInfo falló: {}", GetLastError());
        }
        MemInfo {
            working_set: c.WorkingSetSize as u64,
            peak_working_set: c.PeakWorkingSetSize as u64,
            private_bytes: c.PrivateUsage as u64,
            private_working_set: c.PrivateWorkingSetSize as u64,
        }
    }
}

/// `(total, libre_para_el_usuario, libre_total)` de un volumen vía `GetDiskFreeSpaceExW`.
pub fn disk_free_space(root: &str) -> std::io::Result<(u64, u64, u64)> {
    let w = to_wide(root);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((total, avail, free))
}

/// Percentil (0..=100) sobre una muestra ya ordenada, interpolación por rango más cercano.
pub fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Estadísticas básicas de una muestra de tiempos (en ms).
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub n: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

pub fn stats(samples: &[f64]) -> Stats {
    let mut v: Vec<f64> = samples.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let mean = if n == 0 { f64::NAN } else { v.iter().sum::<f64>() / n as f64 };
    Stats {
        n,
        mean,
        p50: percentile_sorted(&v, 50.0),
        p95: percentile_sorted(&v, 95.0),
        p99: percentile_sorted(&v, 99.0),
        max: v.last().copied().unwrap_or(f64::NAN),
    }
}

/// Formatea bytes en MiB con 1 decimal (para logs).
pub fn mib(b: u64) -> String {
    format!("{:.1} MiB", b as f64 / 1_048_576.0)
}
