//! Process memory: *private working set* (the Task Manager "Memory" column).
//! Windows does not track the private working set peak (`PeakWorkingSetSize` is for the total working set, which
//! includes shared DLLs), so the peak is estimated by sampling.
#![allow(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use windows_sys::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Current process private working set, in bytes.
pub fn private_working_set() -> Option<u64> {
    // SAFETY: PROCESS_MEMORY_COUNTERS_EX2 is a POD structure (integers): all-zero is a valid value.
    let mut c: PROCESS_MEMORY_COUNTERS_EX2 = unsafe { std::mem::zeroed() };
    c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
    // SAFETY: the current process pseudo-handle is always valid; `c` is writable and `cb` is its size. The API accepts
    // the pointer as PROCESS_MEMORY_COUNTERS because EX2 starts with that same structure.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut _, c.cb) };
    (ok != 0).then_some(c.PrivateWorkingSetSize as u64)
}

/// Sample the private working set every few milliseconds and retain the maximum.
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

    /// Stop sampling and return the observed maximum.
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
