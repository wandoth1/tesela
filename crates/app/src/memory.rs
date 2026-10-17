//! Memory usage for this process: the *private working set* (the "Memory" column in Task Manager). Used by the `--idle-memory`
//! developer tool; it does not modify the system and only reads a counter for the current process.
#![allow(unsafe_code)]

use windows_sys::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Current process private working set, in bytes.
pub fn private_working_set() -> Option<u64> {
    // SAFETY: PROCESS_MEMORY_COUNTERS_EX2 is a POD structure (integers); all-zero is a valid value.
    let mut c: PROCESS_MEMORY_COUNTERS_EX2 = unsafe { std::mem::zeroed() };
    c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
    // SAFETY: the current process pseudo-handle is always valid; `c` is writable and `cb` is its size. The API accepts this
    // pointer as PROCESS_MEMORY_COUNTERS because EX2 starts with that structure.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut _, c.cb) };
    (ok != 0).then_some(c.PrivateWorkingSetSize as u64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn el_private_working_set_del_proceso_es_plausible() {
        let a = super::private_working_set().expect("contador del proceso");
        assert!(a > 64 << 10 && a < 64 << 30, "{a}");
        // Reservar y tocar 32 MiB lo hace subir.
        let v = std::hint::black_box(vec![1u8; 32 << 20]);
        let b = super::private_working_set().unwrap();
        assert_eq!(std::hint::black_box(&v)[1 << 20], 1);
        assert!(b >= a + (16 << 20), "{a} -> {b}");
        drop(v);
    }
}
