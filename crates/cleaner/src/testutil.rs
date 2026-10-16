//! Utilidades de las pruebas: un árbol temporal `%TEMP%\tesela-test-<uuid>` con guarda RAII.
//!
//! SEGURIDAD: las pruebas solo escriben y borran dentro de esa carpeta, creada por la propia prueba. La guarda comprueba,
//! antes de borrar nada, que la ruta es hija directa de `%TEMP%` y que su nombre empieza por `tesela-test-`. Los
//! junctions se quitan antes como enlaces (nunca se entra en ellos) y sus destinos están dentro del mismo árbol.
//! Ninguna prueba toca el `%TEMP%` real, las cachés de los navegadores ni la Papelera real salvo con elementos PROPIOS
//! (ver `recycle`).
#![allow(dead_code, unsafe_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Sandbox {
    root: PathBuf,
    junctions: Vec<PathBuf>,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let root = std::env::temp_dir().join(format!("tesela-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).expect("crea la carpeta temporal");
        Sandbox {
            root,
            junctions: Vec::new(),
        }
    }

    /// Raíz tal como se creó (`temp_dir()`; en algunos runners con componentes 8.3).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// El uuid de esta carpeta (el sufijo de `tesela-test-<uuid>`).
    pub fn uuid(&self) -> String {
        self.root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches("tesela-test-")
            .to_string()
    }

    /// Raíz en forma LARGA canónica (sin 8.3 y sin `\\?\`).
    pub fn long_root(&self) -> PathBuf {
        dunce::canonicalize(&self.root).expect("canonicaliza la raíz")
    }

    pub fn long(&self, rel: &str) -> PathBuf {
        self.long_root().join(rel.replace('/', "\\"))
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel.replace('/', "\\"))
    }

    pub fn mkdir(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(&p).expect("mkdir");
        p
    }

    /// Crea un archivo de `size` bytes con contenido no nulo.
    pub fn write(&self, rel: &str, size: usize) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).expect("mkdir padre");
        let mut f = fs::File::create(&p).expect("crea archivo");
        let chunk: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8 + 1).collect();
        let mut left = size;
        while left > 0 {
            let n = left.min(chunk.len());
            f.write_all(&chunk[..n]).unwrap();
            left -= n;
        }
        f.sync_all().ok();
        p
    }

    /// Crea un archivo y fija su fecha de modificación y de creación a `age_hours` horas atrás.
    pub fn write_aged(&self, rel: &str, size: usize, age_hours: u64) -> PathBuf {
        let p = self.write(rel, size);
        self.set_age(&p, age_hours);
        p
    }

    /// Fija las cuatro fechas del objeto (creación, acceso, escritura y cambio) a `age_hours` horas atrás.
    pub fn set_age(&self, p: &Path, age_hours: u64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // FILETIME: intervalos de 100 ns desde 1601.
        let ft = ((now - age_hours * 3600) as i64 + 11_644_473_600) * 10_000_000;
        crate::sys::set_all_times_for_test(p, ft).expect("fija las fechas");
    }

    pub fn hard_link(&self, from_rel: &str, to_rel: &str) {
        let to = self.path(to_rel);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::hard_link(self.path(from_rel), to).expect("hard_link");
    }

    /// `mklink /J` (no necesita administrador). El destino debe estar dentro de este mismo árbol.
    pub fn junction(&mut self, link_rel: &str, target_rel: &str) {
        let (link, target) = (self.path(link_rel), self.path(target_rel));
        assert!(
            target.starts_with(&self.root),
            "el destino del junction debe estar dentro del árbol de prueba"
        );
        let out = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .expect("lanza cmd");
        assert!(
            out.status.success(),
            "mklink /J falló: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        self.junctions.push(link);
    }

    /// Registra un junction creado por fuera (p. ej. dentro de un gancho de carrera) para quitarlo como enlace al soltar.
    pub fn register_junction(&mut self, link_rel: &str) {
        self.junctions.push(self.path(link_rel));
    }

    /// Quita un junction creado con [`Sandbox::junction`] (solo el enlace; el destino queda intacto).
    pub fn remove_junction(&mut self, link_rel: &str) {
        let link = self.path(link_rel);
        fs::remove_dir(&link).expect("quita el junction");
        self.junctions.retain(|j| j != &link);
    }

    /// Cuenta las entradas (archivos y carpetas) bajo `rel` sin seguir enlaces.
    pub fn count_entries(&self, rel: &str) -> usize {
        fn go(p: &Path) -> usize {
            let Ok(rd) = fs::read_dir(p) else { return 0 };
            rd.flatten()
                .map(|e| {
                    let is_real_dir = e.file_type().is_ok_and(|t| t.is_dir())
                        && !e.file_type().is_ok_and(|t| t.is_symlink());
                    1 + if is_real_dir { go(&e.path()) } else { 0 }
                })
                .sum()
        }
        go(&self.path(rel))
    }
}

impl Default for Sandbox {
    fn default() -> Sandbox {
        Sandbox::new()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let temp = std::env::temp_dir();
        let name_ok = self
            .root
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("tesela-test-"));
        let parent_ok = self.root.parent().is_some_and(|p| p == temp);
        if !(name_ok && parent_ok) {
            eprintln!(
                "Sandbox: ruta inesperada, no se borra nada: {}",
                self.root.display()
            );
            return;
        }
        for j in &self.junctions {
            // `remove_dir` sobre un junction borra el enlace y deja el destino intacto.
            let _ = fs::remove_dir(j);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Serializa las pruebas que tocan la Papelera real (comparan el conjunto de ids antes y después).
pub static BIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Bloquea [`BIN_LOCK`] aunque otra prueba haya entrado en pánico con él cogido. Con las pruebas de la Papelera real
/// activadas, antes barre los restos de ejecuciones anteriores (SOLO elementos cuyo origen está bajo `tesela-test-*`).
pub fn bin_lock() -> std::sync::MutexGuard<'static, ()> {
    let g = BIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if crate::recycle::bintest::real_recycle_requested() {
        crate::recycle::bintest::sweep_leftovers();
    }
    g
}

/// Borra al soltarse un archivo o una carpeta propios de una prueba (p. ej. el `$R` apartado dentro de la Papelera). Una
/// carpeta se borra entera, pero nunca se sigue un enlace.
pub struct RemoveOnDrop(pub PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        match fs::symlink_metadata(&self.0) {
            Ok(m) if m.is_dir() => {
                let _ = fs::remove_dir_all(&self.0);
            }
            Ok(_) => {
                let _ = fs::remove_file(&self.0);
            }
            Err(_) => {}
        }
    }
}

/// Crea un archivo DISPERSO de `len` bytes lógicos (no ocupa espacio en disco), para probar elementos "demasiado
/// grandes" para la Papelera sin llenar el disco.
pub fn sparse_file(path: &Path, len: u64) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    let f = fs::File::create(path).expect("crea el archivo disperso");
    let mut returned = 0u32;
    // SAFETY: el handle pertenece a `f`, que sigue vivo; FSCTL_SET_SPARSE sin búfer de entrada marca el archivo como
    // disperso; `returned` es escribible y no hay estructura OVERLAPPED (llamada síncrona).
    let ok = unsafe {
        DeviceIoControl(
            f.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        ok, 0,
        "FSCTL_SET_SPARSE falló (¿volumen sin soporte de archivos dispersos?)"
    );
    f.set_len(len).expect("fija la longitud lógica");
}

/// Forma corta 8.3 de una ruta EXISTENTE (`GetShortPathNameW`); sin nombres 8.3 activos devuelve la ruta sin cambios.
pub fn short_path(path: &Path) -> PathBuf {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let w: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buf = vec![0u16; 2048];
    // SAFETY: `w` termina en NUL; `buf` es escribible con `buf.len()` unidades UTF-16.
    let n = unsafe { GetShortPathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return path.to_path_buf();
    }
    PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]))
}
