//! Memoria del propio proceso: *private working set* (la columna "Memoria" del Administrador de tareas). Lo usa la
//! herramienta de desarrollo `--idle-memory`; no se toca nada del sistema, solo se lee un contador del proceso actual.
#![allow(unsafe_code)]

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
