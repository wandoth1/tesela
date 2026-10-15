//! Memoria del propio proceso: *private working set* (la columna "Memoria" del Administrador de tareas).
//! Windows no lleva cuenta del pico del private working set (`PeakWorkingSetSize` es del working set total, que
//! incluye DLL compartidas), así que el pico se estima muestreando.
#![allow(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use windows_sys::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Private working set actual del proceso, en bytes.
pub fn private_working_set() -> Option<u64> {
    // SAFETY: PROCESS_MEMORY_COUNTERS_EX2 es una estructura POD (enteros): todo-ceros es un valor válido.
    let mut c: PROCESS_MEMORY_COUNTERS_EX2 = unsafe { std::mem::zeroed() };
    c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
    // SAFETY: el pseudo-handle del proceso actual es siempre válido; `c` es escribible y `cb` es su tamaño. La API
    // acepta el puntero como PROCESS_MEMORY_COUNTERS porque EX2 empieza con esa misma estructura.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut _, c.cb) };
    (ok != 0).then_some(c.PrivateWorkingSetSize as u64)
}

/// Muestrea el private working set cada pocos milisegundos y recuerda el máximo.
pub struct PeakSampler {
    peak: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PeakSampler {
    pub fn start() -> PeakSampler {
        let peak = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (peak.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                if let Some(v) = private_working_set() {
                    p.fetch_max(v, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        PeakSampler {
            peak,
            stop,
            thread: Some(thread),
        }
    }

    /// Detiene el muestreo y devuelve el máximo observado.
    pub fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Some(v) = private_working_set() {
            self.peak.fetch_max(v, Ordering::Relaxed);
        }
        self.peak.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_private_working_set_es_plausible_y_el_pico_no_baja() {
        let before = private_working_set().expect("lectura");
        assert!(before > 0, "private working set = {before}");
        let sampler = PeakSampler::start();
        let block = vec![1u8; 32 << 20]; // 32 MiB tocados
        std::thread::sleep(Duration::from_millis(60));
        let during = private_working_set().unwrap();
        drop(block);
        let peak = sampler.finish();
        assert!(
            peak >= during.min(before + (16 << 20)),
            "el pico ({peak}) cubre la subida observada"
        );
    }
}
