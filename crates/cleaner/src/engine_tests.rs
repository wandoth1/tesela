//! Pruebas del recorrido: borrado por handle, antigüedad, protección, enlaces duros, simulación y carreras con la
//! operación (sustituir un archivo aprobado, cambiar un ancestro por un junction, abrirlo en exclusiva desde otro hilo).
//! Todo ocurre en `%TEMP%\tesela-test-<uuid>`.

use super::*;
use crate::protect::ProtectionRoots;
use crate::testutil::Sandbox;
use std::collections::HashSet;
use std::fs;
use std::sync::atomic::AtomicBool;

fn identity_of(path: &Path) -> FileIdentity {
    sys::meta(&sys::open_for_query(path).unwrap())
        .unwrap()
        .identity
}

/// Protección con las rutas dadas (relativas al sandbox) como denegadas.
fn protection(sb: &Sandbox, denied: &[&str], retain: &[&str]) -> Arc<Protection> {
    Arc::new(Protection::new(&ProtectionRoots {
        denied: denied.iter().map(|r| sb.long(r)).collect(),
        content_roots: retain.iter().map(|r| sb.long(r)).collect(),
        ..ProtectionRoots::default()
    }))
}

struct Run {
    unit: Arc<Unit>,
    events: Arc<Mutex<Vec<Event>>>,
}

impl Run {
    fn stats(&self) -> StatsSnapshot {
        self.unit.stats.snapshot()
    }
    fn outcomes(&self, o: Outcome) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.outcome == o)
            .count()
    }
}

fn mk_env(
    mode: Mode,
    prot: Arc<Protection>,
    min_age_hours: u64,
    events: Arc<Mutex<Vec<Event>>>,
) -> Arc<Env> {
    let e2 = events.clone();
    Arc::new(Env {
        protection: prot,
        mode,
        min_age_ft: duration_to_ft(Duration::from_secs(min_age_hours * 3600)),
        now_ft: now_filetime(),
        cancel: Arc::new(AtomicBool::new(false)),
        sink: Some(Arc::new(move |ev| e2.lock().unwrap().push(ev))),
        progress: Arc::new(AtomicU64::new(0)),
    })
}

fn run_contents(
    sb: &Sandbox,
    rel: &str,
    mode: Mode,
    min_age_hours: u64,
    prot: Arc<Protection>,
    threads: usize,
) -> Run {
    let events = Arc::new(Mutex::new(Vec::new()));
    let env = mk_env(mode, prot, min_age_hours, events.clone());
    let path = sb.long(rel);
    let unit = Unit::new(
        0,
        UnitSpec::Contents {
            approved: identity_of(&path),
            path,
        },
        env,
    );
    run(std::slice::from_ref(&unit), threads);
    Run { unit, events }
}

fn run_entry(
    sb: &Sandbox,
    rel: &str,
    approved: FileIdentity,
    is_dir: bool,
    mode: Mode,
    prot: Arc<Protection>,
) -> Run {
    let events = Arc::new(Mutex::new(Vec::new()));
    let env = mk_env(mode, prot, 0, events.clone());
    let unit = Unit::new(
        0,
        UnitSpec::Entry {
            path: sb.long(rel),
            approved,
            approved_tag: 0,
            is_dir,
        },
        env,
    );
    run(std::slice::from_ref(&unit), 2);
    Run { unit, events }
}

fn no_protection() -> Arc<Protection> {
    Arc::new(Protection::new(&ProtectionRoots::default()))
}

fn build_tree(sb: &Sandbox) {
    sb.write("R/a.bin", 10_000);
    sb.write("R/b.bin", 20_000);
    sb.write("R/d1/c.bin", 5_000);
    sb.write("R/d1/d2/e.bin", 7_000);
    sb.mkdir("R/vacia");
}

#[test]
fn borra_los_hijos_de_la_raiz_y_no_la_raiz() {
    let sb = Sandbox::new();
    build_tree(&sb);
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 4);
    assert_eq!(r.unit.status(), Some(RootStatus::Done));
    assert!(sb.path("R").is_dir(), "la raíz de la categoría no se borra");
    assert_eq!(sb.count_entries("R"), 0);
    let s = r.stats();
    assert_eq!((s.files, s.dirs), (4, 3));
    assert!(s.bytes >= 42_000, "{s:?}");
    assert_eq!((s.in_use, s.denied, s.changed, s.protected), (0, 0, 0, 0));
}

#[test]
fn la_simulacion_no_toca_nada_y_cuenta_lo_mismo_que_el_borrado() {
    let sb = Sandbox::new();
    build_tree(&sb);
    let before = sb.count_entries("R");
    let dry = run_contents(&sb, "R", Mode::DryRun, 0, no_protection(), 4);
    assert_eq!(sb.count_entries("R"), before, "--dry-run no borra nada");
    assert!(sb.path("R/d1/d2/e.bin").exists());
    let measure = run_contents(&sb, "R", Mode::Measure, 0, no_protection(), 4);
    assert_eq!(sb.count_entries("R"), before);
    assert_eq!(dry.stats(), measure.stats());
    // Registra la identidad de cada entrada que habría borrado.
    let would = dry
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.outcome == Outcome::WouldDelete && !e.is_dir && e.identity.is_some())
        .count();
    assert_eq!(would, 4);
    let real = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 4);
    assert_eq!(real.stats().files, dry.stats().files);
    assert_eq!(real.stats().dirs, dry.stats().dirs);
    assert_eq!(real.stats().bytes, dry.stats().bytes);
}

#[test]
fn solo_se_borra_lo_de_mas_de_la_antiguedad_minima() {
    let sb = Sandbox::new();
    sb.write_aged("R/viejo.tmp", 4_000, 72);
    sb.write("R/nuevo.tmp", 4_000);
    // Carpeta vieja con un archivo viejo: se borra. Carpeta nueva con un archivo viejo: el archivo sí, la carpeta no.
    sb.write_aged("R/dir-viejo/x.tmp", 4_000, 72);
    sb.set_age(&sb.path("R/dir-viejo"), 72);
    sb.write_aged("R/dir-nuevo/y.tmp", 4_000, 72);
    // Una carpeta vieja con un archivo nuevo dentro: el archivo se queda y, con él, la carpeta.
    sb.write("R/dir-mixto/z.tmp", 4_000);
    sb.set_age(&sb.path("R/dir-mixto"), 72);
    let r = run_contents(&sb, "R", Mode::Delete, 24, no_protection(), 3);
    assert!(!sb.path("R/viejo.tmp").exists());
    assert!(sb.path("R/nuevo.tmp").exists());
    assert!(!sb.path("R/dir-viejo").exists());
    assert!(!sb.path("R/dir-nuevo/y.tmp").exists());
    assert!(
        sb.path("R/dir-nuevo").exists(),
        "carpeta recién creada: se deja"
    );
    assert!(sb.path("R/dir-mixto/z.tmp").exists());
    assert!(sb.path("R/dir-mixto").exists());
    let s = r.stats();
    assert!(s.young >= 3, "{s:?}");
    assert_eq!(s.files, 3);
}

#[test]
fn un_archivo_abierto_en_exclusiva_o_con_share_delete_se_salta_y_se_cuenta() {
    let sb = Sandbox::new();
    sb.write("R/libre.bin", 3_000);
    let en_uso = sb.write("R/uso.bin", 3_000);
    let compartido = sb.write("R/compartido.bin", 3_000);
    // Abierto "como Chromium": con FILE_SHARE_DELETE (el que `std::fs::File::open` usa). El borrado POSIX parecería
    // funcionar pero no liberaría nada: la apertura exclusiva lo rechaza.
    let _a = fs::File::open(&compartido).unwrap();
    // Abierto desde otro hilo en exclusiva (sin compartir nada).
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let p = en_uso.clone();
    let t = std::thread::spawn(move || {
        use std::os::windows::fs::OpenOptionsExt;
        let _f = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&p)
            .unwrap();
        tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });
    rx.recv().unwrap();
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 2);
    done_tx.send(()).unwrap();
    t.join().unwrap();
    assert!(!sb.path("R/libre.bin").exists());
    assert!(
        sb.path("R/uso.bin").exists(),
        "el archivo en uso no se toca"
    );
    assert!(
        sb.path("R/compartido.bin").exists(),
        "con FILE_SHARE_DELETE tampoco: no 'libera' en falso"
    );
    let s = r.stats();
    assert_eq!(s.in_use, 2, "{s:?}");
    assert_eq!(s.files, 1);
    assert_eq!(r.outcomes(Outcome::InUse), 2);
    // La medición (aperturas compartidas) no ve conflicto, la simulación (apertura exclusiva) sí.
    let _a2 = fs::File::open(sb.path("R/compartido.bin")).unwrap();
    assert_eq!(
        run_contents(&sb, "R", Mode::Measure, 0, no_protection(), 2)
            .stats()
            .in_use,
        0
    );
    assert_eq!(
        run_contents(&sb, "R", Mode::DryRun, 0, no_protection(), 2)
            .stats()
            .in_use,
        1
    );
}

#[test]
fn un_archivo_de_solo_lectura_se_borra() {
    let sb = Sandbox::new();
    let p = sb.write("R/ro.bin", 1_000);
    let mut perm = fs::metadata(&p).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(&p, perm).unwrap();
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 1);
    assert!(!p.exists());
    assert_eq!(r.stats().files, 1);
}

#[test]
fn los_enlaces_duros_se_marcan_y_no_suman() {
    let sb = Sandbox::new();
    sb.write("fuera/original.bin", 40_000);
    sb.write("R/propio.bin", 8_000);
    sb.hard_link("fuera/original.bin", "R/enlace.bin");
    let measure = run_contents(&sb, "R", Mode::Measure, 0, no_protection(), 1).stats();
    assert_eq!(measure.shared_files, 1);
    assert!(measure.shared_bytes >= 40_000);
    assert!(
        measure.bytes >= 8_000 && measure.bytes < 40_000,
        "no suma el enlace compartido: {measure:?}"
    );
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 1);
    assert!(!sb.path("R/enlace.bin").exists(), "se borra el nombre");
    assert!(
        sb.path("fuera/original.bin").exists(),
        "el otro nombre sigue ahí"
    );
    assert_eq!(r.stats().shared_files, 1);
    assert_eq!(r.stats().bytes, measure.bytes);
}

#[test]
fn nunca_se_entra_en_un_junction_ni_se_borra_su_destino() {
    let mut sb = Sandbox::new();
    sb.write("destino/importante.bin", 9_000);
    sb.write("R/normal.bin", 1_000);
    sb.junction("R/enlace", "destino");
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 2);
    assert!(
        sb.path("destino/importante.bin").exists(),
        "el destino del junction está intacto"
    );
    assert!(!sb.path("R/normal.bin").exists());
    assert!(
        sb.path("R/enlace").exists(),
        "el propio enlace tampoco se borra"
    );
    assert_eq!(r.stats().reparse, 1);
    assert_eq!(r.outcomes(Outcome::Reparse), 1);
    sb.remove_junction("R/enlace");
}

#[test]
fn una_raiz_que_es_un_junction_se_rechaza() {
    let mut sb = Sandbox::new();
    sb.write("destino/importante.bin", 9_000);
    sb.junction("R", "destino");
    // La identidad aprobada es la del enlace; aun así no se entra en él.
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 1);
    assert_eq!(r.unit.status(), Some(RootStatus::Changed));
    assert!(sb.path("destino/importante.bin").exists());
    sb.remove_junction("R");
}

#[test]
fn lo_protegido_se_respeta_y_lo_demas_se_borra() {
    let sb = Sandbox::new();
    sb.write("R/borrar.bin", 1_000);
    sb.write("R/ok/borrar2.bin", 1_000);
    sb.write("R/denegada/secreto.bin", 1_000);
    sb.write("R/ok/mixta/denegada2/secreto2.bin", 1_000);
    sb.write("R/ok/mixta/tambien.bin", 1_000);
    let p = protection(&sb, &["R/denegada", "R/ok/mixta/denegada2"], &[]);
    let r = run_contents(&sb, "R", Mode::Delete, 0, p, 2);
    assert!(sb.path("R/denegada/secreto.bin").exists());
    assert!(sb.path("R/ok/mixta/denegada2/secreto2.bin").exists());
    assert!(!sb.path("R/borrar.bin").exists());
    assert!(!sb.path("R/ok/borrar2.bin").exists());
    assert!(!sb.path("R/ok/mixta/tambien.bin").exists());
    // Los ancestros de lo protegido se quedan (no pueden quedar vacíos).
    assert!(sb.path("R/ok/mixta").exists() && sb.path("R/ok").exists());
    assert_eq!(r.stats().protected, 2);
    assert_eq!(r.outcomes(Outcome::Protected), 2);
}

#[test]
fn una_carpeta_conocida_dentro_se_recorre_sin_borrarla() {
    let sb = Sandbox::new();
    sb.write("R/conocida/doc.txt", 1_000);
    sb.write("R/otra/x.txt", 1_000);
    let p = protection(&sb, &[], &["R/conocida"]);
    let r = run_contents(&sb, "R", Mode::Delete, 0, p, 2);
    assert!(
        sb.path("R/conocida").is_dir(),
        "la carpeta conocida en sí se respeta"
    );
    assert!(
        !sb.path("R/conocida/doc.txt").exists(),
        "su contenido sí se limpia"
    );
    assert!(!sb.path("R/otra").exists());
    assert_eq!(r.stats().files, 2);
}

#[test]
fn una_entrada_se_borra_con_su_identidad_aprobada() {
    let sb = Sandbox::new();
    let f = sb.write("u/archivo.bin", 6_000);
    sb.write("u/carpeta/a.bin", 3_000);
    sb.write("u/carpeta/sub/b.bin", 3_000);
    let id_f = identity_of(&f);
    let r = run_entry(
        &sb,
        "u/archivo.bin",
        id_f,
        false,
        Mode::Delete,
        no_protection(),
    );
    assert_eq!(r.unit.status(), Some(RootStatus::Done));
    assert!(!f.exists());
    let id_d = identity_of(&sb.path("u/carpeta"));
    let r = run_entry(&sb, "u/carpeta", id_d, true, Mode::Delete, no_protection());
    assert_eq!(r.unit.status(), Some(RootStatus::Done));
    assert!(
        !sb.path("u/carpeta").exists(),
        "la carpeta aprobada se borra entera, ella incluida"
    );
    assert_eq!(r.stats().files, 2);
}

// ---- Carreras con la operación -----------------------------------------------------------------------------------

#[test]
fn sustituir_un_archivo_aprobado_por_otro_del_mismo_tamano_se_rechaza() {
    let sb = Sandbox::new();
    let f = sb.write("u/aprobado.bin", 8_192);
    let approved = identity_of(&f);
    // Entre la aprobación y la ejecución se borra y se vuelve a crear otro archivo del mismo tamaño y nombre.
    fs::remove_file(&f).unwrap();
    sb.write("u/aprobado.bin", 8_192);
    let r = run_entry(
        &sb,
        "u/aprobado.bin",
        approved,
        false,
        Mode::Delete,
        no_protection(),
    );
    assert_eq!(r.unit.status(), Some(RootStatus::Changed));
    assert!(
        f.exists(),
        "el sustituto NO se borra: cambió desde la confirmación"
    );
    assert_eq!(r.stats().changed, 1);
    assert_eq!(r.stats().files, 0);
    assert_eq!(r.outcomes(Outcome::Changed), 1);
}

#[test]
fn sustituir_un_hijo_entre_el_listado_y_la_apertura_se_rechaza() {
    let sb = Sandbox::new();
    sb.write("R/victima.bin", 4_096);
    sb.write("R/otro.bin", 1_000);
    let target = sb.path("R/victima.bin");
    let done = Arc::new(AtomicBool::new(false));
    let d2 = done.clone();
    let _g = test_hooks::install(move |p| {
        if p.file_name().is_some_and(|n| n == "victima.bin") && !d2.swap(true, Ordering::SeqCst) {
            // Justo antes de abrirlo: borrar y crear otro archivo del mismo tamaño y nombre (otro FileId).
            fs::remove_file(p).unwrap();
            fs::write(p, vec![9u8; 4_096]).unwrap();
        }
    });
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 1);
    assert!(done.load(Ordering::SeqCst));
    assert!(target.exists(), "el sustituto no se borra");
    assert!(!sb.path("R/otro.bin").exists());
    assert_eq!(r.stats().changed, 1);
}

#[test]
fn cambiar_un_hijo_directorio_por_un_junction_antes_de_abrirlo_no_borra_nada_fuera() {
    let mut sb = Sandbox::new();
    sb.write("fuera/intocable.bin", 5_000);
    sb.write("R/sub/dentro.bin", 1_000);
    sb.write("R/otro.bin", 1_000);
    let (sbp_sub, sbp_out) = (sb.path("R/sub"), sb.path("fuera"));
    let done = Arc::new(AtomicBool::new(false));
    let d2 = done.clone();
    let _g = test_hooks::install(move |p| {
        if p.file_name().is_some_and(|n| n == "sub") && !d2.swap(true, Ordering::SeqCst) {
            // El directorio listado se sustituye por un junction a otra carpeta del sandbox.
            fs::remove_dir_all(&sbp_sub).unwrap();
            let out = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&sbp_sub)
                .arg(&sbp_out)
                .output()
                .unwrap();
            assert!(out.status.success());
        }
    });
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 1);
    assert!(done.load(Ordering::SeqCst));
    assert!(
        sb.path("fuera/intocable.bin").exists(),
        "nada fuera de la raíz se borra"
    );
    assert_eq!(r.stats().changed, 1, "{:?}", r.stats());
    sb.register_junction("R/sub");
    assert!(!sb.path("R/otro.bin").exists());
}

#[test]
fn cambiar_un_ancestro_por_un_junction_a_otra_carpeta_no_borra_lo_que_no_se_aprobo() {
    let mut sb = Sandbox::new();
    // Aprobado: `a/b` (con su identidad). Después `a` se cambia por un junction a `otra`, que tiene su propio `b`.
    sb.write("a/b/aprobado.bin", 7_000);
    sb.write("otra/b/ajeno.bin", 7_000);
    let approved = identity_of(&sb.path("a/b"));
    fs::rename(sb.path("a"), sb.path("a-original")).unwrap();
    sb.junction("a", "otra");
    let r = run_entry(&sb, "a/b", approved, true, Mode::Delete, no_protection());
    assert_ne!(r.unit.status(), Some(RootStatus::Done));
    assert!(
        sb.path("otra/b/ajeno.bin").exists(),
        "lo que cuelga del destino del junction no se toca"
    );
    assert!(sb.path("a-original/b/aprobado.bin").exists());
    sb.remove_junction("a");
}

#[test]
fn con_un_ancestro_cambiado_a_otra_ruta_con_el_mismo_objeto_la_ruta_resuelta_no_coincide() {
    // El mismo objeto, alcanzado por otra ruta (un junction al ancestro): la identidad coincide pero la ruta resuelta
    // por el handle no es la aprobada, así que se rechaza.
    let mut sb = Sandbox::new();
    sb.write("real/b/x.bin", 3_000);
    let approved = identity_of(&sb.path("real/b"));
    sb.junction("alias", "real");
    let events = Arc::new(Mutex::new(Vec::new()));
    let env = mk_env(Mode::Delete, no_protection(), 0, events);
    let unit = Unit::new(
        0,
        UnitSpec::Entry {
            path: sb.long("alias/b"),
            approved,
            approved_tag: 0,
            is_dir: true,
        },
        env,
    );
    run(std::slice::from_ref(&unit), 1);
    assert_eq!(unit.status(), Some(RootStatus::Protected));
    assert!(sb.path("real/b/x.bin").exists());
    sb.remove_junction("alias");
}

#[test]
fn la_raiz_cambiada_por_otro_directorio_se_rechaza() {
    let sb = Sandbox::new();
    sb.write("R/a.bin", 1_000);
    let events = Arc::new(Mutex::new(Vec::new()));
    let env = mk_env(Mode::Delete, no_protection(), 0, events);
    let approved = identity_of(&sb.path("R"));
    // La raíz se borra y se recrea (otro FileId) con contenido.
    fs::remove_dir_all(sb.path("R")).unwrap();
    sb.write("R/b.bin", 1_000);
    let unit = Unit::new(
        0,
        UnitSpec::Contents {
            path: sb.long("R"),
            approved,
        },
        env,
    );
    run(std::slice::from_ref(&unit), 1);
    assert_eq!(unit.status(), Some(RootStatus::Changed));
    assert!(sb.path("R/b.bin").exists());
}

#[test]
fn una_raiz_que_ahora_esta_protegida_se_rechaza_al_ejecutar() {
    let sb = Sandbox::new();
    sb.write("R/a.bin", 1_000);
    let p = protection(&sb, &["R"], &[]);
    let r = run_contents(&sb, "R", Mode::Delete, 0, p, 1);
    assert_eq!(r.unit.status(), Some(RootStatus::Protected));
    assert!(sb.path("R/a.bin").exists());
}

#[test]
fn una_raiz_inexistente_se_cuenta_como_desaparecida() {
    let sb = Sandbox::new();
    sb.mkdir("R");
    let approved = identity_of(&sb.path("R"));
    let p = sb.long("R");
    fs::remove_dir(&p).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let env = mk_env(Mode::Delete, no_protection(), 0, events);
    let unit = Unit::new(0, UnitSpec::Contents { path: p, approved }, env);
    run(std::slice::from_ref(&unit), 1);
    assert_eq!(unit.status(), Some(RootStatus::Missing));
}

// ---- Cancelación, tamaño y nombres raros ---------------------------------------------------------------------------

#[test]
fn cancelar_antes_de_empezar_no_borra_nada() {
    let sb = Sandbox::new();
    build_tree(&sb);
    let events = Arc::new(Mutex::new(Vec::new()));
    let env = mk_env(Mode::Delete, no_protection(), 0, events);
    env.cancel.store(true, Ordering::SeqCst);
    let path = sb.long("R");
    let unit = Unit::new(
        0,
        UnitSpec::Contents {
            approved: identity_of(&path),
            path,
        },
        env,
    );
    run(std::slice::from_ref(&unit), 2);
    assert_eq!(unit.status(), Some(RootStatus::Cancelled));
    assert_eq!(sb.count_entries("R"), 7);
}

#[test]
fn cancelar_a_mitad_deja_de_empezar_entradas_y_no_deja_nada_a_medias_roto() {
    let sb = Sandbox::new();
    for i in 0..200 {
        sb.write(&format!("R/d{}/f{}.bin", i % 20, i), 512);
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let env = mk_env(Mode::Delete, no_protection(), 0, events);
    let c = env.cancel.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let _g = test_hooks::install(move |_| {
        if count.fetch_add(1, Ordering::SeqCst) == 30 {
            c.store(true, Ordering::SeqCst);
        }
    });
    let path = sb.long("R");
    let unit = Unit::new(
        0,
        UnitSpec::Contents {
            approved: identity_of(&path),
            path,
        },
        env,
    );
    run(std::slice::from_ref(&unit), 1);
    let left = sb.count_entries("R");
    assert!(left > 0, "se canceló antes de terminar");
    assert!(left < 220, "algo se borró antes de cancelar");
    // El estado que queda es coherente: lo que sigue ahí se puede volver a limpiar entero.
    drop(_g);
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 2);
    assert_eq!(sb.count_entries("R"), 0);
    assert_eq!(r.unit.status(), Some(RootStatus::Done));
}

#[test]
fn un_arbol_grande_y_profundo_se_borra_con_varios_hilos() {
    let sb = Sandbox::new();
    let mut deep = String::from("R");
    for i in 0..40 {
        deep.push_str(&format!("/nivel{i:02}"));
        sb.write(&format!("{deep}/hoja{i}.bin"), 100);
    }
    for i in 0..600 {
        sb.write(&format!("R/ancho/g{}/h{}.bin", i % 30, i), 100);
    }
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 8);
    assert_eq!(sb.count_entries("R"), 0);
    let s = r.stats();
    assert_eq!(s.files, 640);
    assert_eq!(s.dirs, 40 + 1 + 30);
}

#[test]
fn nombres_largos_con_puntos_y_espacios_finales_y_unicode_fuera_del_bmp() {
    let sb = Sandbox::new();
    // Ruta total mayor que MAX_PATH: solo la recorrido relativo a handles la borra sin traducir rutas.
    let long_name = "x".repeat(120);
    let mut rel = String::from("R");
    for _ in 0..4 {
        rel.push('/');
        rel.push_str(&long_name);
    }
    let p = sb.long(&rel);
    let verbatim = std::path::PathBuf::from(format!(r"\\?\{}", p.display()));
    fs::create_dir_all(&verbatim).unwrap();
    fs::write(verbatim.join("hoja.bin"), vec![1u8; 100]).unwrap();
    assert!(p.to_string_lossy().len() > 480);
    // Nombres con punto o espacio final solo se pueden crear con `\\?\`.
    let weird = std::path::PathBuf::from(format!(r"\\?\{}\R\fin.", sb.long_root().display()));
    fs::write(&weird, b"x").unwrap();
    let weird2 = std::path::PathBuf::from(format!(r"\\?\{}\R\fin ", sb.long_root().display()));
    fs::write(&weird2, b"x").unwrap();
    sb.write("R/𝒜𝒷-日本語.bin", 100);
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 2);
    assert_eq!(sb.count_entries("R"), 0, "{:?}", r.stats());
    assert_eq!(r.stats().files, 4);
}

#[test]
fn los_eventos_llevan_ruta_identidad_y_tamano() {
    let sb = Sandbox::new();
    sb.write("R/uno.bin", 5_000);
    let r = run_contents(&sb, "R", Mode::Delete, 0, no_protection(), 1);
    let ev = r.events.lock().unwrap();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].path, sb.long("R/uno.bin"));
    assert!(ev[0].identity.is_some() && ev[0].bytes >= 5_000);
    assert_eq!(ev[0].outcome, Outcome::Deleted);
    let ids: HashSet<u128> = ev
        .iter()
        .filter_map(|e| e.identity.map(|i| i.file_id))
        .collect();
    assert_eq!(ids.len(), 1);
}
