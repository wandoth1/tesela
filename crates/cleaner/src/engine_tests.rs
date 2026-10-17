//! Traversal tests: deletion by handle, age, protection, hard links, dry runs, and races with the operation (replacing
//! an approved file, changing an ancestor to a junction, and opening it exclusively from another thread).
//! Everything runs under `%TEMP%\tesela-test-<uuid>`.

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

/// Protection with the given paths (relative to the sandbox) denied.
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
        allow_local_mail_permanent: false,
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
            approved_links: 1,
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
    // Records the identity of each entry that would have been deleted.
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
    // An old folder with an old file is deleted. In a new folder with an old file, only the file is deleted.
    sb.write_aged("R/dir-viejo/x.tmp", 4_000, 72);
    sb.set_age(&sb.path("R/dir-viejo"), 72);
    sb.write_aged("R/dir-nuevo/y.tmp", 4_000, 72);
    // An old folder containing a new file is kept, along with the file.
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
    // Opened "like Chromium," with FILE_SHARE_DELETE (used by `std::fs::File::open`). POSIX deletion would appear to
    // work but would free nothing, so the exclusive open rejects it.
    let _a = fs::File::open(&compartido).unwrap();
    // Opened exclusively from another thread (without sharing).
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
    // Measurement (shared opens) sees no conflict, but the dry run (exclusive open) does.
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
    // The approved identity belongs to the link; traversal still does not follow it.
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
    // Ancestors of protected entries are kept (they cannot be left empty).
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

// ---- Races with the operation -----------------------------------------------------------------------------------

#[test]
fn sustituir_un_archivo_aprobado_por_otro_del_mismo_tamano_se_rechaza() {
    let sb = Sandbox::new();
    let f = sb.write("u/aprobado.bin", 8_192);
    let approved = identity_of(&f);
    // Between approval and execution, the file is deleted and another file with the same name and size is created.
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
    let _g = test_hooks::install(sb.long_root(), move |p| {
        if p.file_name().is_some_and(|n| n == "victima.bin") && !d2.swap(true, Ordering::SeqCst) {
            // Right before opening it: delete and create another file with the same name and size (a different FileId).
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
    let _g = test_hooks::install(sb.long_root(), move |p| {
        if p.file_name().is_some_and(|n| n == "sub") && !d2.swap(true, Ordering::SeqCst) {
            // Replace the listed directory with a junction to another sandbox folder.
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
    // Approved: `a/b` (with its identity). Then `a` is replaced with a junction to `other`, which has its own `b`.
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
    // The same object reached through another path (a junction at an ancestor): identity matches, but the resolved path
    // The handle resolves to a different identity than the approved one, so it is rejected.
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
            approved_links: 1,
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
    // The root is deleted and recreated (with a different FileId) and populated.
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

// ---- Cancellation, size, and unusual names ---------------------------------------------------------------------------

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
    let _g = test_hooks::install(sb.long_root(), move |_| {
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
    // The remaining state is consistent: everything still present can be cleaned up again.
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
    // Total path exceeds MAX_PATH: only handle-relative traversal can delete it without rewriting paths.
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
    // Names ending with a dot or space can only be created with the `\\?\` prefix.
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

/// Two names that differ only by a lone UTF-16 code unit (valid on NTFS) are not confused: replacing it with `U+FFFD`
/// would cause protection for one to cover the other (or vice versa).
#[test]
fn los_nombres_con_unidades_sueltas_se_protegen_y_se_borran_por_separado() {
    use std::os::windows::ffi::OsStringExt;
    let sb = Sandbox::new();
    sb.mkdir("R");
    let name = |last: u16| OsString::from_wide(&[b'n' as u16, last]);
    let (a, b) = (name(0xD800), name(0xD801));
    fs::write(sb.path("R").join(&a), vec![1u8; 2_000]).unwrap();
    fs::write(sb.path("R").join(&b), vec![2u8; 2_000]).unwrap();
    // Protect only the first one (its canonical long path, including the lone code unit).
    let prot = Arc::new(Protection::new(&ProtectionRoots {
        denied: vec![sb.long("R").join(&a)],
        ..ProtectionRoots::default()
    }));
    let r = run_contents(&sb, "R", Mode::Delete, 0, prot, 2);
    assert!(sb.path("R").join(&a).exists(), "el protegido se queda");
    assert!(!sb.path("R").join(&b).exists(), "el otro se borra");
    assert_eq!(r.stats().protected, 1);
}

// ---- A-M1: content fingerprint ----------------------------------------------------------------------------------

/// The fingerprint does not depend on ORDER or thread count (sum and XOR are commutative), and is identical across two
/// traversals of the same tree.
#[test]
fn la_huella_no_depende_del_orden_ni_de_los_hilos() {
    let sb = Sandbox::new();
    for i in 0..60 {
        sb.write(&format!("R/d{}/f{i:02}.bin", i % 7), 100 + i * 37);
    }
    sb.write("R/raiz.bin", 4_000);
    let fp = |threads: usize| {
        let s = run_contents(&sb, "R", Mode::Measure, 0, no_protection(), threads).stats();
        (s.fp_sum, s.fp_xor)
    };
    let base = fp(1);
    assert_ne!(base, (0, 0));
    for t in [2, 4, 8, 8] {
        assert_eq!(fp(t), base, "{t} hilos");
    }
}

/// Removing a child and adding another with the SAME name and size leaves totals unchanged but changes the fingerprint
/// (a different `FileId`). The fingerprint also changes when a file's link count changes or an entry becomes protected.
#[test]
fn la_huella_cambia_si_se_sustituye_un_hijo_por_otro_del_mismo_tamano() {
    let sb = Sandbox::new();
    sb.write("R/a.bin", 3_000);
    sb.write("R/b.bin", 3_000);
    let measure = |prot: Arc<Protection>| run_contents(&sb, "R", Mode::Measure, 0, prot, 2).stats();
    let before = measure(no_protection());
    fs::remove_file(sb.path("R/a.bin")).unwrap();
    sb.write("R/a.bin", 3_000);
    let after = measure(no_protection());
    assert_eq!(
        (before.bytes, before.logical, before.files, before.dirs),
        (after.bytes, after.logical, after.files, after.dirs),
        "los totales no lo delatan"
    );
    assert_ne!((before.fp_sum, before.fp_xor), (after.fp_sum, after.fp_xor));
    // One more hard link to the same file: a different `links` count.
    let b0 = measure(no_protection());
    sb.hard_link("R/b.bin", "R/b2.bin");
    let b1 = measure(no_protection());
    assert_ne!((b0.fp_sum, b0.fp_xor), (b1.fp_sum, b1.fp_xor));
    fs::remove_file(sb.path("R/b2.bin")).unwrap();
    assert_eq!(
        {
            let s = measure(no_protection());
            (s.fp_sum, s.fp_xor)
        },
        (b0.fp_sum, b0.fp_xor),
        "vuelta al principio: la misma huella"
    );
    // Treating the same entry differently (protected here) also changes the fingerprint.
    let prot = measure(protection(&sb, &["R/a.bin"], &[]));
    assert_ne!((prot.fp_sum, prot.fp_xor), (b0.fp_sum, b0.fp_xor));
}

#[test]
fn el_hash_de_una_entrada_depende_de_todos_sus_campos() {
    let base = entry_fingerprint(1, 7, 10, 20, 1);
    assert_eq!(base, entry_fingerprint(1, 7, 10, 20, 1));
    for other in [
        entry_fingerprint(2, 7, 10, 20, 1),
        entry_fingerprint(1, 8, 10, 20, 1),
        entry_fingerprint(1, 7 | (1u128 << 100), 10, 20, 1),
        entry_fingerprint(1, 7, 11, 20, 1),
        entry_fingerprint(1, 7, 10, 21, 1),
        entry_fingerprint(1, 7, 10, 20, 2),
    ] {
        assert_ne!(other, base);
    }
}

/// A-M3: BENCHMARK (not a test: `cargo test -p tesela-cleaner medicion_a_m3 -- --ignored --nocapture`). Delete 100,000
/// owned files by handle (empty files in a temporary `tesela-test-<uuid>` folder), using the local log as the receiver.
#[test]
#[ignore = "medición A-M3"]
fn medicion_a_m3_borrado_de_100000_archivos_con_registro() {
    let sb = Sandbox::new();
    for d in 0..100 {
        sb.mkdir(&format!("R/d{d:03}"));
        for f in 0..1_000 {
            fs::File::create(sb.path(&format!("R/d{d:03}/f{f:04}.tmp"))).unwrap();
        }
    }
    for threads in [1usize, 4] {
        if threads == 4 {
            // A second tree (the first one has already been deleted).
            for d in 0..100 {
                sb.mkdir(&format!("R/d{d:03}"));
                for f in 0..1_000 {
                    fs::File::create(sb.path(&format!("R/d{d:03}/f{f:04}.tmp"))).unwrap();
                }
            }
        }
        let log = Arc::new(crate::log::LogWriter::create(&sb.path("Tesela"), false).unwrap());
        let env = Arc::new(Env {
            protection: no_protection(),
            mode: Mode::Delete,
            allow_local_mail_permanent: false,
            min_age_ft: 0,
            now_ft: now_filetime(),
            cancel: Arc::new(AtomicBool::new(false)),
            sink: Some(log.sink()),
            progress: Arc::new(AtomicU64::new(0)),
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
        let t = std::time::Instant::now();
        run(std::slice::from_ref(&unit), threads);
        log.flush();
        let dt = t.elapsed();
        let s = unit.stats.snapshot();
        eprintln!(
            "MEDICION e2e ({threads} hilos): {} archivos y {} carpetas borrados en {:.3} s, registro de {} bytes",
            s.files,
            s.dirs,
            dt.as_secs_f64(),
            fs::metadata(log.path()).unwrap().len()
        );
        assert_eq!(s.files, 100_000);
    }
}

/// m-1: a measurement is COMPLETE only with zero errors: setting any error field to 1 (and all others to 0) invalidates
/// it, while non-errors (recent, protected, reparse, cloud, shared, sizes, fingerprint) do not. No filesystem access.
#[test]
fn una_medida_solo_es_completa_con_cero_errores() {
    assert!(StatsSnapshot::default().is_complete());
    type Set = fn(&mut StatsSnapshot);
    let errors: [(&str, Set); 6] = [
        ("in_use", |s| s.in_use = 1),
        ("denied", |s| s.denied = 1),
        ("changed", |s| s.changed = 1),
        ("other_errors", |s| s.other_errors = 1),
        ("not_empty", |s| s.not_empty = 1),
        ("gone", |s| s.gone = 1),
    ];
    for (name, set) in errors {
        let mut s = StatsSnapshot::default();
        set(&mut s);
        assert!(!s.is_complete(), "{name} debe invalidar la medida");
    }
    let fine: [(&str, Set); 9] = [
        ("young", |s| s.young = 1),
        ("protected", |s| s.protected = 1),
        ("reparse", |s| s.reparse = 1),
        ("cloud", |s| s.cloud = 1),
        ("shared_files", |s| s.shared_files = 1),
        ("bytes", |s| s.bytes = 1),
        ("files", |s| s.files = 1),
        ("fp_sum", |s| s.fp_sum = 1),
        ("fp_xor", |s| s.fp_xor = 1),
    ];
    for (name, set) in fine {
        let mut s = StatsSnapshot::default();
        set(&mut s);
        assert!(s.is_complete(), "{name} no es un error");
    }
}

/// s-3: replacing an entry with another of the same size changes both `fp_sum` AND `fp_xor`, each SEPARATELY (if only
/// one half of the fingerprint changed, a mutant that canceled the other half would go undetected).
#[test]
fn la_suma_y_el_xor_de_la_huella_cambian_cada_uno_por_separado() {
    let sb = Sandbox::new();
    sb.write("R/a.bin", 3_000);
    sb.write("R/b.bin", 3_000);
    let measure = || run_contents(&sb, "R", Mode::Measure, 0, no_protection(), 2).stats();
    let before = measure();
    fs::remove_file(sb.path("R/a.bin")).unwrap();
    sb.write("R/a.bin", 3_000);
    let after = measure();
    assert_ne!(before.fp_sum, after.fp_sum, "la suma cambia");
    assert_ne!(before.fp_xor, after.fp_xor, "el xor cambia");
    assert_ne!(before.fp_sum, 0);
    assert_ne!(before.fp_xor, 0);
}

/// GM-M3: measurement records the LONGEST path in the tree (UTF-16 code units, full descendant path), and measurement,
/// dry run, and deletion count the same value. Accented and supplementary-plane names count code units, not bytes.
#[test]
fn la_medida_anota_la_ruta_mas_larga_en_unidades_utf16() {
    let sb = Sandbox::new();
    build_tree(&sb);
    // A name with one supplementary-plane character (2 UTF-16 code units, 4 bytes) and one accent (1 code unit, 2 bytes).
    sb.write("R/d1/d2/😀é-larguísimo.bin", 10);
    let longest = sb.long("R/d1/d2/😀é-larguísimo.bin");
    let units = crate::plan::path_units(&longest) as u64;
    assert!(
        (longest.as_os_str().len() as u64) > units,
        "en bytes pesa más que en unidades UTF-16"
    );
    let measure = run_contents(&sb, "R", Mode::Measure, 0, no_protection(), 4);
    assert_eq!(measure.stats().max_path, units, "{:?}", measure.stats());
    let dry = run_contents(&sb, "R", Mode::DryRun, 0, no_protection(), 4);
    assert_eq!(dry.stats(), measure.stats());
    // `merge` keeps the maximum (it does not add values).
    let mut a = StatsSnapshot {
        max_path: 10,
        ..StatsSnapshot::default()
    };
    a.merge(&StatsSnapshot {
        max_path: 7,
        ..StatsSnapshot::default()
    });
    assert_eq!(a.max_path, 10);
    a.merge(&StatsSnapshot {
        max_path: 12,
        ..StatsSnapshot::default()
    });
    assert_eq!(a.max_path, 12);
}

/// GM-M3: `recycle_charge` ADDS `bytes` (single-link files) and `shared_bytes` (multiply linked files): these are disjoint sets that the
/// shell must count together. Taking their maximum (the previous behavior) underestimated a folder containing both types.
#[test]
fn el_cargo_a_la_papelera_suma_bytes_y_compartidos() {
    let s = StatsSnapshot {
        bytes: 12_288,
        shared_bytes: 12_288,
        logical: 20_000,
        ..StatsSnapshot::default()
    };
    assert_eq!(s.recycle_charge(), 24_576);
    assert_eq!(
        s.bytes.max(s.logical).max(s.shared_bytes),
        20_000,
        "la cuenta anterior"
    );
    // The logical size wins when it is larger (sparse or compressed file).
    let sparse = StatsSnapshot {
        bytes: 4_096,
        logical: 1 << 40,
        ..StatsSnapshot::default()
    };
    assert_eq!(sparse.recycle_charge(), 1 << 40);
    // Without overflow.
    let huge = StatsSnapshot {
        bytes: u64::MAX,
        shared_bytes: u64::MAX,
        ..StatsSnapshot::default()
    };
    assert_eq!(huge.recycle_charge(), u64::MAX);
}
