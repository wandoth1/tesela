//! Pruebas de punta a punta: cesta -> aprobación (identidades) -> ejecución (definitivo, Papelera, vaciar con alcance
//! enumerado, simulación) sobre árboles en `%TEMP%\tesela-test-<uuid>`.
//!
//! SEGURIDAD: las categorías se inyectan con raíces del sandbox, así que NUNCA se toca el `%TEMP%`, las cachés ni la
//! Papelera reales. Las pruebas que envían a la Papelera usan SOLO elementos propios (filtrados por el uuid de la
//! prueba), con una guarda RAII que los purga, y comprueban que el conjunto de ids de la Papelera es el de antes. NINGUNA
//! prueba vacía la Papelera real: el alcance de "vaciar" se limita con `with_bin_filter` a los elementos propios.

use super::*;
use crate::basket::Basket;
use crate::plan::{ApproveOptions, Cleaner, Fit, RejectReason, UnfitReason};
use crate::protect::{ProtectionRoots, Verdict};
use crate::recycle::bintest::{BinGuard, bin_usable, ids, is_mine, real_recycle_enabled};
use crate::roots::Roots;
use crate::rules::{CategoryId, Risk};
use crate::testutil::{Sandbox, bin_lock};
use std::fs;
use std::sync::Mutex;
use tesela_scanner::BrowserRoots;

fn cleaner_for(sb: &Sandbox) -> Cleaner {
    sb.mkdir("Temp");
    sb.mkdir("Local/Google/Chrome/User Data/Default/Cache");
    sb.mkdir("Local/Google/Chrome/User Data/Default/Code Cache");
    sb.mkdir("Roaming");
    sb.mkdir("Docs");
    let roots = Roots {
        user_temp: Some(sb.long("Temp")),
        windows_temp: None,
        browser: BrowserRoots {
            local_appdata: Some(sb.long("Local")),
            roaming_appdata: Some(sb.long("Roaming")),
        },
        downloads: None,
        startup_rejections: Vec::new(),
        protection: ProtectionRoots {
            denied: vec![sb.long("Local/Google/Chrome/User Data"), sb.long("Prot")],
            untouchable: vec![sb.long("Local"), sb.long("Roaming")],
            content_roots: vec![sb.long("Docs")],
            exempt: vec![],
        },
    };
    let uuid = sb.uuid();
    Cleaner::new(roots)
        .with_threads(4)
        .with_bin_filter(Arc::new(move |i| is_mine(i, &uuid)))
}

fn cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// Un registro de prueba para `c`: en su carpeta de datos (dentro del sandbox) o, si no tiene, en una carpeta propia de
/// la prueba que `exec` borra al terminar.
fn log_for(c: &Cleaner) -> (Arc<LogWriter>, Option<PathBuf>) {
    match c.data_dir.clone() {
        Some(d) => (Arc::new(LogWriter::create(&d, false).unwrap()), None),
        None => {
            let d = std::env::temp_dir().join(format!("tesela-test-log-{}", uuid::Uuid::new_v4()));
            (Arc::new(LogWriter::create(&d, false).unwrap()), Some(d))
        }
    }
}

/// Ejecuta el plan. Sin registro explícito, una ejecución REAL usa uno de prueba (M4: sin registro no se limpia; la
/// simulación sí puede ir sin él).
fn exec(
    c: &Cleaner,
    plan: &Plan,
    dry: bool,
    allow_unfit: bool,
    log: Option<Arc<LogWriter>>,
) -> Result<Report, ExecError> {
    let (log, tmp) = match log {
        None if !dry => {
            let (l, tmp) = log_for(c);
            (Some(l), tmp)
        }
        other => (other, None),
    };
    let r = c.execute(
        plan,
        &ExecOptions {
            dry_run: dry,
            allow_unfit_permanent: allow_unfit,
            log,
        },
        &Progress::new(),
    );
    if let Some(d) = tmp {
        let _ = fs::remove_dir_all(d);
    }
    r
}

fn approve(c: &Cleaner, b: &Basket) -> Plan {
    c.approve(b, &ApproveOptions::default(), &cancel())
}

#[test]
fn las_categorias_borran_sus_raices_con_su_regla_y_respetan_el_perfil() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    // Temporales: solo lo de más de 24 h.
    sb.write_aged("Temp/viejo.tmp", 30_000, 72);
    sb.write("Temp/nuevo.tmp", 30_000);
    // Caché de Chrome: sin límite de antigüedad. El perfil (historial, cookies...) NO es caché.
    sb.write(
        "Local/Google/Chrome/User Data/Default/Cache/f_000001",
        50_000,
    );
    sb.write(
        "Local/Google/Chrome/User Data/Default/Code Cache/js/a",
        20_000,
    );
    sb.write("Local/Google/Chrome/User Data/Default/History", 8_000);
    sb.write("Local/Google/Chrome/User Data/Default/Cookies", 8_000);
    sb.write("Local/Google/Chrome/User Data/Local State", 1_000);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    b.set_category(CategoryId::ChromeCache, true);
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 3, "temporales + dos directorios de caché");
    assert!(plan.rejected.is_empty(), "{:?}", plan.rejected);
    assert!(
        plan.items
            .iter()
            .all(|i| i.destination == Destination::Permanent && i.risk == Risk::Safe)
    );
    assert!(plan.permanent_bytes() >= 100_000);
    // La estimación solo cuenta lo que se va a tocar (los temporales recientes no).
    let temp = plan
        .items
        .iter()
        .find(|i| i.category == Some(CategoryId::UserTemp))
        .unwrap();
    assert_eq!(temp.estimate.files, 1);
    assert_eq!(temp.estimate.young, 1);

    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert!(
        rep.items.iter().all(|i| i.status == ItemStatus::Done),
        "{:?}",
        rep.items
    );
    assert!(!sb.path("Temp/viejo.tmp").exists());
    assert!(
        sb.path("Temp/nuevo.tmp").exists(),
        "min_age: lo reciente se deja"
    );
    assert!(
        !sb.path("Local/Google/Chrome/User Data/Default/Cache/f_000001")
            .exists()
    );
    assert!(
        !sb.path("Local/Google/Chrome/User Data/Default/Code Cache/js")
            .exists()
    );
    assert!(
        sb.path("Local/Google/Chrome/User Data/Default/Cache")
            .is_dir(),
        "la raíz de la categoría no se borra"
    );
    for keep in ["Default/History", "Default/Cookies", "Local State"] {
        assert!(
            sb.path(&format!("Local/Google/Chrome/User Data/{keep}"))
                .exists(),
            "{keep}"
        );
    }
    assert!(rep.permanent_bytes >= 100_000, "{rep:?}");
    assert_eq!(rep.recycled_bytes, 0);
    assert!(rep.freed_now.is_some());
    assert!(!rep.dry_run && !rep.cancelled);
    assert_eq!(
        rep.totals.in_use + rep.totals.denied + rep.totals.changed,
        0
    );
}

#[test]
fn dry_run_no_toca_nada_y_registra_las_identidades_aprobadas() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write_aged("Temp/a.tmp", 10_000, 72);
    sb.write_aged("Temp/sub/b.tmp", 10_000, 72);
    sb.write("Docs/mio.doc", 5_000);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    b.add_path(sb.long("Docs/mio.doc"), c.protection());
    let plan = approve(&c, &b);
    let logdir = sb.path("logs");
    let log = Arc::new(LogWriter::create(&logdir, true).unwrap());
    let before = (sb.count_entries("Temp"), sb.count_entries("Docs"));
    // Lo único que depende de la Papelera real es "ni la Papelera": va detrás de TESELA_REAL_RECYCLE_TESTS=1. Todo lo
    // demás de la prueba corre siempre.
    let ids_before = real_recycle_enabled().then(ids);
    let rep = exec(&c, &plan, true, false, Some(log.clone())).unwrap();
    assert!(rep.dry_run);
    assert_eq!(
        (sb.count_entries("Temp"), sb.count_entries("Docs")),
        before,
        "--dry-run no toca nada"
    );
    if let Some(ids_before) = ids_before {
        assert_eq!(ids(), ids_before, "ni la Papelera");
    }
    assert!(
        rep.freed_now.is_none(),
        "en simulación no se mide liberación"
    );
    assert!(
        rep.permanent_bytes >= 20_000 && rep.recycled_bytes >= 5_000,
        "{rep:?}"
    );
    log.flush();
    let text = fs::read_to_string(log.path()).unwrap();
    assert!(text.contains("\"dry_run\":true"));
    let entries: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["t"] == "entry")
        .collect();
    assert!(entries.len() >= 3, "{text}");
    assert!(
        entries
            .iter()
            .all(|e| e["out"] == "would-delete" || e["out"] == "would-recycle")
    );
    assert!(
        entries.iter().filter(|e| e["id"].is_string()).count() >= 3,
        "lleva las identidades"
    );
}

#[test]
fn un_elemento_del_usuario_va_a_la_papelera_por_su_ruta_original_y_se_verifica() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    sb.write("Docs/vacaciones.mov", 70_000);
    sb.write("Docs/carpeta/a.txt", 3_000);
    sb.write("Docs/carpeta/sub/b.txt", 3_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/vacaciones.mov"), c.protection());
    b.add_path(sb.long("Docs/carpeta"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2);
    assert!(
        plan.items
            .iter()
            .all(|i| i.destination == Destination::Recycle
                && i.risk == Risk::Review
                && i.category.is_none())
    );
    assert!(plan.items.iter().all(|i| i.fit == Fit::Fits));
    assert_eq!(
        plan.biggest_user_items(5)[0].path,
        sb.long("Docs/vacaciones.mov")
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert!(
        rep.items.iter().all(|i| i.status == ItemStatus::Recycled),
        "{:?}",
        rep.items
    );
    assert!(!sb.path("Docs/vacaciones.mov").exists());
    assert!(!sb.path("Docs/carpeta").exists());
    assert_eq!(sb.count_entries("Docs"), 0, "no queda nada en su sitio");
    assert!(rep.recycled_bytes >= 76_000);
    assert!(
        rep.bin_total_after.is_some_and(|t| t > 0),
        "la shell dice cuánto hay en la Papelera: {:?}",
        rep.bin_total_after
    );
    assert_eq!(rep.permanent_bytes, 0);
    assert_eq!(
        (
            rep.deleted_unexpectedly,
            rep.not_recycled,
            rep.recycled_other,
            rep.unverified
        ),
        (0, 0, 0, 0)
    );
    // Están en la Papelera con su NOMBRE ORIGINAL (no hay lápidas: nada que el usuario vea o restaure con un nombre raro).
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(mine.len(), 2, "{mine:?}");
    let names: Vec<String> = mine
        .iter()
        .map(|i| i.name.to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().any(|n| n == "vacaciones.mov"), "{names:?}");
    assert!(names.iter().any(|n| n == "carpeta"), "{names:?}");
    assert!(
        names.iter().all(|n| !n.contains("tesela-")),
        "ninguna lápida: {names:?}"
    );
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before, "la Papelera queda exactamente como estaba");
}

#[test]
fn vaciar_la_papelera_purga_solo_la_lista_capturada() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    // Dos elementos propios en la Papelera...
    for n in ["uno.txt", "dos.txt"] {
        sb.write(&format!("Docs/{n}"), 2_000);
    }
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/uno.txt"), c.protection());
    b.add_path(sb.long("Docs/dos.txt"), c.protection());
    exec(&c, &approve(&c, &b), false, false, None).unwrap();
    assert_eq!(
        recycle::list_bin()
            .unwrap()
            .iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .count(),
        2
    );
    // ...se confirma "Vaciar" (se captura la lista: solo esos dos, por el filtro de la prueba)...
    let mut v = Basket::new();
    v.set_empty_bin(true);
    let plan = c.approve(
        &v,
        &ApproveOptions {
            bin_bytes_hint: 4_000,
        },
        &cancel(),
    );
    assert_eq!(plan.bin.as_ref().unwrap().items.len(), 2);
    assert!(plan.items.is_empty());
    // El tamaño que se enseña es el de la lista enumerada (con el filtro de la prueba, solo lo propio), no el de toda la
    // Papelera ni lo que sugirió quien llama.
    let listed: u64 = plan
        .bin
        .as_ref()
        .unwrap()
        .items
        .iter()
        .map(|i| i.size)
        .sum();
    assert_eq!(plan.bin.as_ref().unwrap().bytes_hint, listed);
    assert!(listed >= 4_000, "{listed}");
    // ...y después entra otro elemento propio en la Papelera, que NO está en la lista capturada.
    sb.write("Docs/tres.txt", 2_000);
    let mut b3 = Basket::new();
    b3.add_path(sb.long("Docs/tres.txt"), c.protection());
    exec(&c, &approve(&c, &b3), false, false, None).unwrap();
    assert_eq!(
        recycle::list_bin()
            .unwrap()
            .iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .count(),
        3
    );
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    let rep = exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    let bin = rep.bin.unwrap();
    assert_eq!((bin.requested, bin.purged, bin.still_there), (2, 2, 0));
    assert!(!bin.failed);
    // M4: cada elemento purgado consta POR SEPARADO en el registro, tras la intención del trozo; el que entró después no.
    let text = fs::read_to_string(log.path()).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let begin = lines
        .iter()
        .position(|v| v["t"] == "bin-begin")
        .expect("falta la intención de la purga");
    let items: Vec<(usize, &serde_json::Value)> = lines
        .iter()
        .enumerate()
        .filter(|(_, v)| v["t"] == "bin-item")
        .collect();
    assert_eq!(items.len(), 2, "{text}");
    for (i, v) in &items {
        assert!(*i > begin);
        assert_eq!(v["out"], "purged");
        let path = v["path"].as_str().unwrap();
        assert!(
            path.ends_with("uno.txt") || path.ends_with("dos.txt"),
            "{path}"
        );
        assert_eq!(v["bytes"], 2_000);
    }
    assert!(!text.contains("tres.txt"));
    let left: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(
        left.len(),
        1,
        "lo que entró después de confirmar no se toca: {left:?}"
    );
    assert_eq!(left[0].name.to_string_lossy(), "tres.txt");
    // Y los ajenos siguen exactamente como estaban.
    let foreign: HashSet<OsString> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| !is_mine(i, &sb.uuid()))
        .map(|i| i.id)
        .collect();
    assert_eq!(foreign, before);
}

/// M3 + M4 con la Papelera real (solo elementos propios): si la identidad capturada al confirmar de un elemento ya no
/// cuadra (aquí, falseada en el plan), se SALTA al vaciar: la shell no lo ve, consta como cambiado en el informe y en el
/// registro, y los demás se purgan.
#[test]
fn vaciar_salta_el_elemento_cuya_identidad_cambio_y_lo_deja_en_el_registro() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    for n in ["uno.txt", "dos.txt"] {
        sb.write(&format!("Docs/{n}"), 2_000);
    }
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/uno.txt"), c.protection());
    b.add_path(sb.long("Docs/dos.txt"), c.protection());
    exec(&c, &approve(&c, &b), false, false, None).unwrap();
    let mut v = Basket::new();
    v.set_empty_bin(true);
    let mut plan = c.approve(&v, &ApproveOptions::default(), &cancel());
    let bp = plan.bin.as_mut().unwrap();
    assert_eq!(bp.items.len(), 2);
    // Falsear la identidad del `$R` del primero: ya no es lo que se aprobó.
    bp.items[0].data_id = Some(FileIdentity {
        volume_serial: 1,
        file_id: 2,
    });
    let skipped_name = bp.items[0].original_path();
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    let rep = exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    let bin = rep.bin.unwrap();
    assert_eq!(
        (bin.requested, bin.purged, bin.still_there, bin.changed),
        (2, 1, 1, 1)
    );
    let text = fs::read_to_string(log.path()).unwrap();
    let changed_lines = text
        .lines()
        .filter(|l| l.contains("\"bin-item\"") && l.contains("\"changed\""))
        .count();
    assert_eq!(changed_lines, 1, "{text}");
    assert!(text.contains("\"purged\""));
    let left: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0].original_path(), skipped_name);
    recycle::purge_items(&left, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

#[test]
fn el_orden_primero_vaciar_y_despues_mover() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    sb.write("Docs/viejo.txt", 2_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/viejo.txt"), c.protection());
    exec(&c, &approve(&c, &b), false, false, None).unwrap();
    // Vaciar (lista: el viejo) y mover uno nuevo en la MISMA cesta.
    sb.write("Docs/nuevo.txt", 2_000);
    let mut b2 = Basket::new();
    b2.set_empty_bin(true);
    b2.add_path(sb.long("Docs/nuevo.txt"), c.protection());
    let plan = approve(&c, &b2);
    assert_eq!(plan.bin.as_ref().unwrap().items.len(), 1);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(rep.bin.unwrap().purged, 1);
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    // El nuevo (movido DESPUÉS de vaciar) sigue recuperable en la Papelera.
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].name.to_string_lossy(), "nuevo.txt");
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

#[test]
fn lo_que_no_cabe_en_la_papelera_exige_la_casilla() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    // Una ruta demasiado larga para la Papelera (más de 258 caracteres).
    let seg = "x".repeat(80);
    let rel = format!("Docs/{seg}/{seg}/{seg}/{seg}");
    let verbatim = PathBuf::from(format!(
        r"\\?\{}\{}",
        sb.long_root().display(),
        rel.replace('/', "\\")
    ));
    fs::create_dir_all(&verbatim).unwrap();
    fs::write(verbatim.join("grande.bin"), vec![3u8; 10_000]).unwrap();
    let file = sb.long(&format!("{rel}/grande.bin"));
    assert!(file.as_os_str().len() > 258);
    let mut b = Basket::new();
    b.add_path(file.clone(), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].fit, Fit::Unfit(UnfitReason::PathTooLong));
    assert_eq!(plan.unfit_items().count(), 1);
    assert_eq!(plan.recycle_items().count(), 0);
    assert!(plan.unfit_bytes() >= 10_000);
    // Sin la casilla: no se toca.
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(rep.skipped_unfit, 1);
    assert_eq!(rep.items[0].status, ItemStatus::SkippedUnfit);
    assert!(
        file.exists(),
        "nunca se convierte en definitivo en silencio"
    );
    // Con la casilla: se elimina definitivamente (sin pasar por la Papelera).
    let rep = exec(&c, &plan, false, true, None).unwrap();
    assert_eq!(rep.items[0].destination, Destination::Permanent);
    assert_eq!(rep.items[0].status, ItemStatus::Done);
    assert!(!file.exists());
    assert!(rep.permanent_bytes >= 10_000);
}

#[test]
fn si_cambia_lo_que_cabe_se_pide_confirmar_otra_vez_antes_de_tocar_nada() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write("Docs/a.txt", 2_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/a.txt"), c.protection());
    let mut plan = approve(&c, &b);
    assert_eq!(plan.items[0].fit, Fit::Fits);
    // Lo aprobado decía "no cabe" pero ahora cabe (o al revés): hay que volver a confirmar.
    plan.items[0].fit = Fit::Unfit(UnfitReason::TooBig);
    let r = exec(&c, &plan, false, true, None);
    assert_eq!(
        r.unwrap_err(),
        ExecError::NeedsReconfirm(vec![plan.items[0].id])
    );
    assert!(sb.path("Docs/a.txt").exists());
}

// ---- Carreras con la operación, a nivel de plan ------------------------------------------------------------------------

#[test]
fn sustituir_un_elemento_aprobado_por_otro_igual_se_rechaza_en_la_ejecucion() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    let f = sb.write("Docs/foto.jpg", 12_288);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/foto.jpg"), c.protection());
    let plan = approve(&c, &b);
    // Después de confirmar: se borra y se crea otro archivo del mismo tamaño y nombre.
    fs::remove_file(&f).unwrap();
    sb.write("Docs/foto.jpg", 12_288);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(rep.items[0].status, ItemStatus::Root(RootStatus::Changed));
    assert!(f.exists(), "el sustituto no se toca");
    assert_eq!(rep.totals.changed, 1);
    assert_eq!(ids(), before);
}

#[test]
fn con_un_ancestro_cambiado_por_un_junction_la_papelera_no_mueve_lo_ajeno() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let mut sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    sb.write("Docs/a/b/aprobado.txt", 4_000);
    sb.write("Docs/otra/b/ajeno.txt", 4_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/a/b"), c.protection());
    let plan = approve(&c, &b);
    fs::rename(sb.path("Docs/a"), sb.path("Docs/a-original")).unwrap();
    sb.junction("Docs/a", "Docs/otra");
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert!(
        matches!(rep.items[0].status, ItemStatus::Root(_)),
        "{:?}",
        rep.items[0].status
    );
    assert_ne!(rep.items[0].status, ItemStatus::Recycled);
    assert!(sb.path("Docs/otra/b/ajeno.txt").exists());
    assert!(sb.path("Docs/a-original/b/aprobado.txt").exists());
    sb.remove_junction("Docs/a");
    assert_eq!(ids(), before);
}

#[test]
fn un_archivo_abierto_en_exclusiva_no_se_envia_y_una_carpeta_con_algo_abierto_dentro_tampoco() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    let uso = sb.write("Docs/en-uso.bin", 4_000);
    sb.write("Docs/carpeta/dentro.bin", 4_000);
    sb.write("Docs/libre.bin", 4_000);
    let mut b = Basket::new();
    for p in ["Docs/en-uso.bin", "Docs/carpeta", "Docs/libre.bin"] {
        b.add_path(sb.long(p), c.protection());
    }
    let plan = approve(&c, &b);
    // Uno abierto en exclusiva y otro archivo abierto dentro de la carpeta.
    use std::os::windows::fs::OpenOptionsExt;
    let _a = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&uso)
        .unwrap();
    let _b = fs::File::open(sb.path("Docs/carpeta/dentro.bin")).unwrap();
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let st = |p: &str| {
        rep.items
            .iter()
            .find(|i| i.path == sb.long(p))
            .unwrap()
            .status
    };
    assert_eq!(st("Docs/libre.bin"), ItemStatus::Recycled);
    assert!(
        matches!(
            st("Docs/en-uso.bin"),
            ItemStatus::NotRecycled(RecycleFail::InUse)
        ),
        "{:?}",
        st("Docs/en-uso.bin")
    );
    assert_eq!(
        st("Docs/carpeta"),
        ItemStatus::NotRecycled(RecycleFail::Denied),
        "una carpeta con algo abierto dentro falla en seguro (mover es renombrar): la shell dice acceso denegado"
    );
    // No se renombró nada: siguen con su nombre.
    assert!(sb.path("Docs/en-uso.bin").exists() && sb.path("Docs/carpeta/dentro.bin").exists());
    assert_eq!(
        sb.count_entries("Docs"),
        3,
        "solo lo que no se envió, con su nombre: ninguna lápida suelta"
    );
    assert!(
        rep.totals.in_use + rep.totals.denied >= 2,
        "{:?}",
        rep.totals
    );
    drop((_a, _b));
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

// ---- Protección, enlaces duros, rechazos ----------------------------------------------------------------------------

#[test]
fn una_carpeta_con_una_carpeta_conocida_dentro_se_expande_y_esa_se_respeta() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    // `Docs/Mezcla/Protegida` es una raíz conocida (solo "Contenido de"); lo demás es borrable.
    let mut c = cleaner_for(&sb);
    c.protection = Arc::new(crate::protect::Protection::new(&ProtectionRoots {
        content_roots: vec![sb.long("Docs/Mezcla/Protegida")],
        ..ProtectionRoots::default()
    }));
    sb.write("Docs/Mezcla/suelto.txt", 5_000);
    sb.write("Docs/Mezcla/otra/dentro.txt", 5_000);
    sb.write("Docs/Mezcla/Protegida/doc.txt", 2_000);
    let mut b = Basket::new();
    assert_eq!(
        b.add_path(sb.long("Docs/Mezcla"), c.protection()),
        crate::basket::AddOutcome::Added { absorbed: 0 },
        "contiene algo protegido pero se acepta"
    );
    let plan = approve(&c, &b);
    let paths: Vec<PathBuf> = plan.items.iter().map(|i| i.path.clone()).collect();
    assert!(
        paths.contains(&sb.long("Docs/Mezcla/suelto.txt")),
        "{paths:?}"
    );
    assert!(paths.contains(&sb.long("Docs/Mezcla/otra")));
    assert!(
        paths.contains(&sb.long("Docs/Mezcla/Protegida/doc.txt")),
        "su contenido sí"
    );
    assert!(
        !paths.contains(&sb.long("Docs/Mezcla")),
        "la carpeta entera no se mueve"
    );
    assert!(
        !paths.contains(&sb.long("Docs/Mezcla/Protegida")),
        "la carpeta conocida en sí no"
    );
    assert_eq!(
        plan.protected_total().0,
        1,
        "se omite 1 elemento protegido: {:?}",
        plan.omitted_protected
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert!(
        rep.items.iter().all(|i| i.status == ItemStatus::Recycled),
        "{:?}",
        rep.items
    );
    assert!(sb.path("Docs/Mezcla/Protegida").is_dir());
    assert!(!sb.path("Docs/Mezcla/Protegida/doc.txt").exists());
    assert!(!sb.path("Docs/Mezcla/suelto.txt").exists());
    assert!(
        sb.path("Docs/Mezcla").is_dir(),
        "el contenedor se queda: contiene lo protegido"
    );
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

#[test]
fn los_enlaces_duros_se_marcan_en_el_plan_y_no_suman() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write("Docs/original.bin", 40_000);
    sb.hard_link("Docs/original.bin", "Docs/enlace.bin");
    sb.write("Docs/normal.bin", 10_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/enlace.bin"), c.protection());
    b.add_path(sb.long("Docs/normal.bin"), c.protection());
    let plan = approve(&c, &b);
    let enlace = plan
        .items
        .iter()
        .find(|i| i.path.ends_with("enlace.bin"))
        .unwrap();
    assert!(enlace.hard_linked);
    assert_eq!(enlace.estimate.bytes, 0, "no libera espacio");
    assert!(enlace.estimate.shared_bytes >= 40_000);
    assert!(
        plan.recycle_bytes() < 40_000,
        "no suma en la estimación: {}",
        plan.recycle_bytes()
    );
    assert_eq!(plan.shared_total().0, 1);
}

#[test]
fn las_entradas_que_no_se_pueden_aprobar_se_rechazan_con_su_motivo() {
    let mut sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write("Docs/destino/x.bin", 1_000);
    sb.junction("Docs/enlace", "Docs/destino");
    sb.write("Docs/ok.bin", 1_000);
    let mut b = Basket::new();
    // El propio add_path ya rebota lo denegado/intocable; aquí se fuerzan entradas que solo la aprobación puede rechazar.
    b.add_path(sb.long("Docs/enlace"), c.protection());
    b.add_path(sb.long("Docs/no-existe.bin"), c.protection());
    b.add_path(sb.long("Docs/ok.bin"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1);
    assert!(
        plan.rejected
            .iter()
            .any(|r| r.reason == RejectReason::Reparse && r.path.ends_with("enlace")),
        "{:?}",
        plan.rejected
    );
    assert!(
        plan.rejected
            .iter()
            .any(|r| r.reason == RejectReason::Missing),
        "{:?}",
        plan.rejected
    );
    // Una carpeta que pasó a estar protegida entre añadirla y aprobar.
    let mut c2 = cleaner_for(&sb);
    c2.protection = Arc::new(crate::protect::Protection::new(&ProtectionRoots {
        denied: vec![sb.long("Docs/ok.bin")],
        ..ProtectionRoots::default()
    }));
    let mut b2 = Basket::new();
    b2.add_path(
        sb.long("Docs/ok.bin"),
        &crate::protect::Protection::new(&ProtectionRoots::default()),
    );
    let plan2 = approve(&c2, &b2);
    assert!(plan2.items.is_empty());
    assert_eq!(
        plan2.rejected[0].reason,
        RejectReason::Protected(Verdict::Denied)
    );
    sb.remove_junction("Docs/enlace");
}

#[test]
fn contenido_de_una_carpeta_conocida_son_sus_hijos_y_no_ella() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    sb.write("Docs/a.txt", 3_000);
    sb.write("Docs/b/c.txt", 3_000);
    let mut b = Basket::new();
    assert_eq!(
        b.add_path(sb.long("Docs"), c.protection()),
        crate::basket::AddOutcome::Rejected(Verdict::ContentsOnly)
    );
    b.add_contents_of(sb.long("Docs"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2);
    assert!(
        plan.items
            .iter()
            .all(|i| i.category.is_none() && i.destination == Destination::Recycle)
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert!(rep.items.iter().all(|i| i.status == ItemStatus::Recycled));
    assert!(
        sb.path("Docs").is_dir(),
        "la carpeta conocida en sí se queda"
    );
    assert_eq!(sb.count_entries("Docs"), 0);
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

#[test]
fn cancelar_antes_de_empezar_no_toca_nada() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write_aged("Temp/a.tmp", 10_000, 72);
    sb.write("Docs/b.txt", 1_000);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    b.add_path(sb.long("Docs/b.txt"), c.protection());
    let plan = approve(&c, &b);
    let progress = Progress::new();
    progress.cancel();
    let rep = c
        .execute(
            &plan,
            &ExecOptions {
                log: Some(log_for(&c).0),
                ..ExecOptions::default()
            },
            &progress,
        )
        .unwrap();
    assert!(rep.cancelled);
    assert!(sb.path("Temp/a.tmp").exists() && sb.path("Docs/b.txt").exists());
}

// ---- Solo unidades locales (B2) y la propia aplicación dentro del temporal (B3) ---------------------------------------

/// La misma carpeta del sandbox por una ruta de red a la compartición administrativa.
fn via_share(host: &str, p: &Path) -> PathBuf {
    let s = p.to_string_lossy().into_owned();
    let letter = s.chars().next().unwrap();
    let rest: String = s.chars().skip(3).collect();
    let sep = '\\';
    PathBuf::from(format!("{sep}{sep}{host}{sep}{letter}${sep}{rest}"))
}

/// B2: una ruta de red a la compartición administrativa de un disco local (con `localhost` o `127.0.0.1` como equipo)
/// esquivaba la protección (que se escribe sobre rutas de unidad). Se rechazan
/// al aprobar, aunque la ruta de red sea accesible; y si alguien las cuela en un plan, la ejecución también las rechaza.
#[test]
fn las_rutas_de_red_a_un_disco_local_no_esquivan_la_proteccion() {
    let _lock = bin_lock();
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    let f = sb.write("Docs/privado.txt", 2_000);
    sb.write_aged("Temp/viejo.tmp", 3_000, 72);
    for host in ["localhost", "127.0.0.1"] {
        // Ya al AÑADIR a la cesta: rechazada con su motivo (no solo al aprobar).
        let net = via_share(host, &sb.long("Docs/privado.txt"));
        let mut b = Basket::new();
        assert_eq!(
            b.add_path(net.clone(), c.protection()),
            crate::basket::AddOutcome::NotLocal
        );
        assert!(b.is_empty());
        assert!(approve(&c, &b).items.is_empty());
        // En la ejecución: se toma un plan legítimo y se le cambia la ruta por la de red (misma identidad).
        let mut b2 = Basket::new();
        b2.add_path(sb.long("Docs/privado.txt"), c.protection());
        let mut plan2 = approve(&c, &b2);
        assert_eq!(plan2.items.len(), 1);
        let it = &mut plan2.items[0];
        it.path = net.clone();
        it.fit = crate::plan::fit_for(&net, 2_000);
        if let UnitSpec::Entry { path, .. } = &mut it.spec {
            *path = net.clone();
        }
        // Una ruta de red nunca "cabe" en la Papelera: sin la casilla se salta...
        let rep = exec(&c, &plan2, false, false, None).unwrap();
        assert_eq!(rep.items[0].status, ItemStatus::SkippedUnfit);
        // ...y con la casilla (borrado definitivo por el recorrido por handle) la raíz se rechaza por no ser local.
        let rep = exec(&c, &plan2, false, true, None).unwrap();
        eprintln!("{host}: estado en la ejecución = {:?}", rep.items[0].status);
        assert_eq!(rep.items[0].status, ItemStatus::Root(RootStatus::NotLocal));
        assert!(f.exists(), "{host}: no se tocó nada");
        // La raíz de una categoría por la ruta de red (borrado definitivo del contenido).
        let mut b3 = Basket::new();
        b3.set_category(CategoryId::UserTemp, true);
        let mut plan3 = approve(&c, &b3);
        assert_eq!(plan3.items.len(), 1);
        let net_temp = via_share(host, &sb.long("Temp"));
        plan3.items[0].path = net_temp.clone();
        if let UnitSpec::Contents { path, .. } = &mut plan3.items[0].spec {
            *path = net_temp;
        }
        let rep3 = exec(&c, &plan3, false, false, None).unwrap();
        eprintln!("{host}: categoría por la red = {:?}", rep3.items[0].status);
        assert_eq!(rep3.items[0].status, ItemStatus::Root(RootStatus::NotLocal));
        assert!(
            sb.path("Temp/viejo.tmp").exists(),
            "{host}: no se borró nada por la ruta de red"
        );
    }
}

/// B3: la carpeta de la propia aplicación DENTRO del temporal (una raíz exenta) nunca se borra, ni su contenido: la
/// exención levanta lo que hay por encima de la raíz exenta, no una denegación que está dentro de ella.
#[test]
fn la_propia_aplicacion_dentro_del_temporal_nunca_se_borra() {
    let sb = Sandbox::new();
    sb.mkdir("Temp");
    let roots = Roots {
        user_temp: Some(sb.long("Temp")),
        windows_temp: None,
        browser: BrowserRoots::default(),
        downloads: None,
        startup_rejections: Vec::new(),
        protection: ProtectionRoots {
            denied: vec![sb.long("Temp/app")],
            untouchable: vec![],
            content_roots: vec![],
            exempt: vec![],
        },
    };
    let c = Cleaner::new(roots).with_threads(4);
    sb.write_aged("Temp/app/tesela.exe", 5_000, 72);
    sb.write_aged("Temp/app/datos/config.toml", 1_000, 72);
    sb.write_aged("Temp/viejo.tmp", 3_000, 72);
    sb.write_aged("Temp/otra/x.tmp", 3_000, 72);
    sb.set_age(&sb.path("Temp/otra"), 72);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(rep.items[0].status, ItemStatus::Done);
    assert!(sb.path("Temp/app/tesela.exe").exists());
    assert!(sb.path("Temp/app/datos/config.toml").exists());
    assert!(!sb.path("Temp/viejo.tmp").exists());
    assert!(!sb.path("Temp/otra").exists());
    assert!(rep.totals.protected >= 1);
    // Y como destino suelto tampoco se puede añadir ni aprobar.
    let mut b2 = Basket::new();
    b2.add_path(sb.long("Temp/app"), c.protection());
    assert!(approve(&c, &b2).items.is_empty());
}

/// Roots de prueba con una carpeta "protegida" (`Prot`), SIN canonicalizar `Local`/`Temp` (un junction en sus ancestros
/// se verá en la cadena tal como se escribió).
fn roots_with_raw(sb: &Sandbox, local: PathBuf, user_temp: Option<PathBuf>) -> Roots {
    sb.mkdir("Roaming");
    Roots {
        user_temp,
        windows_temp: None,
        browser: BrowserRoots {
            local_appdata: Some(local.clone()),
            roaming_appdata: Some(sb.long("Roaming")),
        },
        downloads: None,
        startup_rejections: Vec::new(),
        protection: ProtectionRoots {
            denied: vec![sb.long("Prot"), local.join(r"Google\Chrome\User Data")],
            untouchable: vec![sb.long("Roaming")],
            content_roots: vec![],
            exempt: vec![],
        },
    }
}

/// B1 (v0.1.1): un junction ANCESTRO de la raíz de una categoría (`User Data` -> una carpeta protegida con un
/// `ShaderCache`) no convierte lo protegido en caché borrable: la raíz se descarta (capa 1) y, aunque se aceptara, la
/// exención solo levanta su zona declarada (capa 2). Nada de lo protegido se toca ni en simulación ni de verdad.
#[test]
fn un_junction_por_encima_de_user_data_no_vuelve_borrable_lo_protegido() {
    let mut sb = Sandbox::new();
    sb.write_aged("Prot/UD/ShaderCache/s.bin", 4_000, 72);
    sb.write_aged("Prot/UD/GrShaderCache/g.bin", 3_000, 72);
    sb.write_aged("Prot/UD/Default/Cache/c.bin", 2_000, 72);
    sb.write_aged("Prot/UD/Cookies", 100, 72);
    sb.mkdir("Local/Google/Chrome");
    sb.junction("Local/Google/Chrome/User Data", "Prot/UD");
    let roots = roots_with_raw(&sb, sb.path("Local"), None);
    let c = Cleaner::new(roots);
    // El escáner ve las cachés a través del enlace, pero ninguna pasa la cadena.
    let checks = c.roots().category_root_checks(CategoryId::ChromeCache);
    assert!(
        !checks.is_empty(),
        "el escáner las lista a través del junction"
    );
    for ck in &checks {
        assert!(
            matches!(
                &ck.result,
                Err(crate::roots::RootRejection::ReparseInChain { component })
                    if component == "User Data"
            ),
            "{:?}",
            ck.result
        );
    }
    assert!(c.roots().category_roots(CategoryId::ChromeCache).is_empty());
    // Lo protegido, por su ruta real, sigue denegado.
    for rel in [
        "Prot/UD/ShaderCache",
        "Prot/UD/ShaderCache/s.bin",
        "Prot/UD/Default/Cache",
    ] {
        assert_eq!(
            c.protection()
                .target_verdict(&crate::paths::PathKey::new(&sb.long(rel))),
            Verdict::Denied,
            "{rel}"
        );
    }
    // Aprobar y ejecutar (simulación y real): no hay elementos, todo queda en su sitio.
    let mut b = Basket::new();
    b.set_category(CategoryId::ChromeCache, true);
    let plan = approve(&c, &b);
    assert!(plan.items.is_empty(), "{:?}", plan.items.len());
    assert!(!plan.rejected.is_empty());
    assert!(
        plan.rejected
            .iter()
            .all(|r| r.reason == RejectReason::ReparseAncestor),
        "{:?}",
        plan.rejected
    );
    exec(&c, &plan, true, false, None).unwrap();
    exec(&c, &plan, false, false, None).unwrap();
    for rel in [
        "Prot/UD/ShaderCache/s.bin",
        "Prot/UD/GrShaderCache/g.bin",
        "Prot/UD/Default/Cache/c.bin",
        "Prot/UD/Cookies",
    ] {
        assert!(sb.path(rel).exists(), "{rel}");
    }
}

/// H4: una raíz descartada por un enlace en su ruta no desaparece en silencio: `unavailable_roots` da el motivo (lo que la
/// interfaz enseña como "no disponible"), las raíces descartadas al arrancar también se devuelven, y el registro de la
/// ejecución anota lo rechazado con su motivo.
#[test]
fn una_raiz_descartada_deja_su_motivo_visible_y_en_el_registro() {
    let mut sb = Sandbox::new();
    sb.write_aged("Prot/UD/ShaderCache/s.bin", 4_000, 72);
    sb.mkdir("Local/Google/Chrome");
    sb.junction("Local/Google/Chrome/User Data", "Prot/UD");
    let mut roots = roots_with_raw(&sb, sb.path("Local"), None);
    // Y una raíz que `from_system` descartó al arrancar (p. ej. un %TEMP% redirigido).
    roots.startup_rejections.push((
        CategoryId::UserTemp,
        sb.path("Redirigida"),
        crate::roots::RootRejection::OutsideLocalAppData,
    ));
    let c = Cleaner::new(roots);
    let un = c.roots().unavailable_roots(CategoryId::ChromeCache);
    assert!(!un.is_empty());
    assert!(un.iter().all(|(_, why)| matches!(
        why,
        crate::roots::RootRejection::ReparseInChain { component } if component == "User Data"
    )));
    let temp = c.roots().unavailable_roots(CategoryId::UserTemp);
    assert_eq!(temp.len(), 1);
    assert_eq!(temp[0].1, crate::roots::RootRejection::OutsideLocalAppData);
    // Lo que no es de seguridad (no existe, sin permiso) no se enseña como "no disponible".
    assert!(
        !crate::roots::RootRejection::Os(5).is_user_visible()
            && crate::roots::RootRejection::NotCanonical.is_user_visible()
    );
    // La aprobación lo rechaza y el registro lo anota.
    let mut b = Basket::new();
    b.set_category(CategoryId::ChromeCache, true);
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    assert!(plan.items.is_empty());
    assert!(
        plan.rejected
            .iter()
            .all(|r| r.reason == RejectReason::ReparseAncestor)
    );
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    let text = fs::read_to_string(log.path()).unwrap();
    assert!(
        text.lines()
            .any(|l| l.contains("\"rejected\"") && l.contains("ReparseAncestor")),
        "{text}"
    );
}

/// B1 (capa 2, aislada): aunque una raíz "canónica" llegue a `Prot\UD\ShaderCache` por el camino que sea (aquí la capa 1
/// no se ejecuta: se inyecta la ruta resuelta), la exención de Chrome declara SU zona (`...\User Data`), no `Prot`.
#[test]
fn la_exencion_de_una_cache_no_levanta_una_denegacion_ajena() {
    let sb = Sandbox::new();
    sb.write_aged("Prot/UD/ShaderCache/s.bin", 4_000, 72);
    sb.mkdir("Local/Google/Chrome");
    let local = sb.long("Local");
    let user_data = local.join(r"Google\Chrome\User Data");
    let prot = crate::protect::Protection::new(&ProtectionRoots {
        denied: vec![sb.long("Prot"), user_data.clone()],
        exempt: vec![crate::protect::Exempt::new(
            sb.long("Prot/UD/ShaderCache"),
            Some(user_data),
        )],
        ..ProtectionRoots::default()
    });
    assert_eq!(
        prot.target_verdict(&crate::paths::PathKey::new(
            &sb.long("Prot/UD/ShaderCache/s.bin")
        )),
        Verdict::Denied
    );
}

/// B1 (v0.1.1), mismo agujero con `%TEMP%`: un ancestro de la raíz del temporal es un junction hacia una carpeta
/// protegida. Con la ruta tal como se escribió se descarta (capa 1); con la ruta ya resuelta (lo que daba `canon`) la
/// exención de `%TEMP%` no declara zona y la denegación de `Prot` sigue en pie (capa 2).
#[test]
fn un_junction_por_encima_de_temp_no_vuelve_borrable_lo_protegido() {
    let mut sb = Sandbox::new();
    sb.write_aged("Prot/T/Temp/x.tmp", 3_000, 72);
    sb.write_aged("Prot/T/Temp/sub/y.tmp", 3_000, 72);
    sb.junction("Enlace", "Prot/T");
    // (1) La ruta como la daría `GetTempPath`: pasa por el junction.
    let c = Cleaner::new(roots_with_raw(
        &sb,
        sb.long("Local"),
        Some(sb.path("Enlace/Temp")),
    ));
    assert!(c.roots().category_roots(CategoryId::UserTemp).is_empty());
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    assert!(plan.items.is_empty());
    assert_eq!(plan.rejected.len(), 1);
    assert_eq!(plan.rejected[0].reason, RejectReason::ReparseAncestor);
    exec(&c, &plan, false, false, None).unwrap();
    assert!(sb.path("Prot/T/Temp/x.tmp").exists());
    // (2) Ya resuelta: la cadena está limpia, pero la raíz cae dentro de `Prot` (denegada) y nada la levanta.
    let c2 = Cleaner::new(roots_with_raw(
        &sb,
        sb.long("Local"),
        Some(sb.long("Prot/T/Temp")),
    ));
    let plan2 = approve(&c2, &b);
    assert!(plan2.items.is_empty(), "{:?}", plan2.items.len());
    assert_eq!(
        plan2.rejected[0].reason,
        RejectReason::Protected(Verdict::Denied)
    );
    exec(&c2, &plan2, false, false, None).unwrap();
    assert!(sb.path("Prot/T/Temp/x.tmp").exists());
    assert!(sb.path("Prot/T/Temp/sub/y.tmp").exists());
}

/// H1: la aprobación abre la raíz para listarla DESPUÉS de comprobar la cadena. Si en esa ventana un ancestro se cambia
/// por un junction hacia otra carpeta del usuario (no denegada), lo que se abriría sería otro objeto, y la identidad
/// capturada ya sería la equivocada. Se compara con la identidad que vio el recorrido y con la ruta resuelta: se rechaza.
#[test]
fn un_ancestro_cambiado_entre_la_cadena_y_la_apertura_se_rechaza() {
    let mut sb = Sandbox::new();
    sb.write_aged("Anc/Temp/bueno.tmp", 1_000, 72);
    sb.write_aged("Otra/Temp/ajeno.tmp", 1_000, 72);
    let roots = roots_with_raw(&sb, sb.long("Local"), Some(sb.path("Anc/Temp")));
    let c = Cleaner::new(roots);
    // Justo antes de que la aprobación abra la raíz: `Anc` pasa a ser un junction hacia `Otra`.
    let (anc, anc_real, otra) = (sb.path("Anc"), sb.path("Anc-real"), sb.path("Otra"));
    let _hook = crate::engine::test_hooks::install(sb.long("Anc/Temp"), move |_| {
        if !anc_real.exists() {
            fs::rename(&anc, &anc_real).unwrap();
            let out = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&anc)
                .arg(&otra)
                .output()
                .unwrap();
            assert!(out.status.success());
        }
    });
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    sb.register_junction("Anc");
    assert!(
        plan.items.is_empty(),
        "se aprobó el objeto equivocado: {:?}",
        plan.items.iter().map(|i| &i.path).collect::<Vec<_>>()
    );
    assert_eq!(plan.rejected.len(), 1, "{:?}", plan.rejected);
    assert_eq!(plan.rejected[0].reason, RejectReason::ReparseAncestor);
    assert!(sb.path("Otra/Temp/ajeno.tmp").exists());
}

/// H1 (pura): misma identidad y misma ruta resuelta, o no es lo que vio la cadena.
#[test]
fn chain_matches_exige_la_misma_identidad_y_la_misma_ruta() {
    use crate::paths::PathKey;
    let id = |n: u128| FileIdentity {
        volume_serial: 7,
        file_id: n,
    };
    let root = Path::new(r"C:\Users\<usuario>\AppData\Local\Temp");
    let key = PathKey::new(root);
    assert!(crate::plan::chain_matches(id(1), root, id(1), &key));
    assert!(!crate::plan::chain_matches(id(1), root, id(2), &key));
    let otra = PathKey::new(Path::new(r"C:\Otra\Temp"));
    assert!(!crate::plan::chain_matches(id(1), root, id(1), &otra));
    // El volumen también cuenta.
    let otro_vol = FileIdentity {
        volume_serial: 8,
        file_id: 1,
    };
    assert!(!crate::plan::chain_matches(id(1), root, otro_vol, &key));
}

/// B1: el caso benigno: raíces reales, sin reparse points, siguen funcionando y llevan la zona declarada.
#[test]
fn las_raices_sin_reparse_points_siguen_siendo_categorias() {
    let sb = Sandbox::new();
    sb.mkdir("Temp");
    sb.mkdir("Local/Google/Chrome/User Data/Default/Cache");
    sb.mkdir("Local/Mozilla/Firefox/Profiles/p1/cache2");
    let mut roots = roots_with_raw(&sb, sb.long("Local"), Some(sb.long("Temp")));
    roots
        .protection
        .denied
        .push(sb.long("Local/Mozilla/Firefox"));
    let c = Cleaner::new(roots);
    for id in [
        CategoryId::UserTemp,
        CategoryId::ChromeCache,
        CategoryId::FirefoxCache,
    ] {
        let cks = c.roots().category_root_checks(id);
        assert_eq!(cks.len(), 1, "{id:?}");
        assert!(cks[0].result.is_ok(), "{id:?}: {:?}", cks[0].result);
    }
    let chrome = c.roots().category_root_checks(CategoryId::ChromeCache);
    let v = chrome[0].result.as_ref().unwrap();
    assert_eq!(
        v.path,
        sb.long("Local/Google/Chrome/User Data/Default/Cache")
    );
    assert_eq!(
        v.zone.as_deref(),
        Some(sb.long("Local/Google/Chrome/User Data").as_path())
    );
    assert_eq!(
        c.protection()
            .target_verdict(&crate::paths::PathKey::new(&v.path.join("f_000001"))),
        Verdict::Allowed
    );
    let mut b = Basket::new();
    b.set_category(CategoryId::ChromeCache, true);
    b.set_category(CategoryId::FirefoxCache, true);
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 3, "{:?}", plan.rejected);
    assert!(plan.rejected.is_empty());
}

// ---- El receptor de eventos: identidad (no nombre), carreras tras soltar el handle, y el juicio por elemento ----------

/// B1: `.lnk`, `.url` y extensiones conocidas (`.txt`, `.jpg`...): el elemento se reconoce en la Papelera por su
/// IDENTIDAD, no por nombre, así que ni la extensión ni "ocultar extensiones" lo despistan.
#[test]
fn lnk_url_y_extensiones_conocidas_se_verifican_por_identidad() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    let names = [
        "acceso directo.lnk",
        "enlace web.url",
        "nota.txt",
        "foto.jpg",
        "datos.csv",
        "sin-extension",
        "Mayusculas.TXT",
        "con.varios.puntos.tar.gz",
    ];
    let mut b = Basket::new();
    for n in names {
        sb.write(&format!("Docs/{n}"), 2_500);
        b.add_path(sb.long(&format!("Docs/{n}")), c.protection());
    }
    sb.write("Docs/carpeta.lnk/dentro.txt", 1_000);
    b.add_path(sb.long("Docs/carpeta.lnk"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), names.len() + 1);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    for it in &rep.items {
        assert_eq!(it.status, ItemStatus::Recycled, "{:?}", it.path);
        assert_eq!(
            it.removed.len(),
            1,
            "se actualiza el árbol solo con lo verificado"
        );
    }
    assert_eq!(sb.count_entries("Docs"), 0);
    assert_eq!(
        (
            rep.recycled_other,
            rep.unverified,
            rep.deleted_unexpectedly,
            rep.not_recycled
        ),
        (0, 0, 0, 0)
    );
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(mine.len(), names.len() + 1, "{mine:?}");
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

/// Carrera (M1): tras SOLTAR el handle y antes de que la shell abra el elemento, el mismo usuario lo sustituye por otro
/// objeto del mismo nombre y tamaño. `PreDeleteItem` compara la IDENTIDAD con la aprobada, ve otra y devuelve `E_ABORT`:
/// el sustituto NO se envía (queda en su sitio), el original aprobado tampoco se toca, el elemento consta como "cambió
/// desde la confirmación" y el lote se corta (lo que venía detrás no se procesa). Antes de M1 el sustituto llegaba a la
/// Papelera (`RecycledOther`, recuperable).
#[test]
fn sustituir_tras_soltar_el_handle_no_envia_el_sustituto_y_corta_el_lote() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    sb.write("Docs/foto.jpg", 12_288);
    sb.write("Docs/otro.jpg", 4_096);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/foto.jpg"), c.protection());
    b.add_path(sb.long("Docs/otro.jpg"), c.protection());
    let plan = approve(&c, &b);
    let (orig, moved) = (sb.path("Docs/foto.jpg"), sb.path("Docs/foto-original.jpg"));
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), {
        let (orig, moved) = (orig.clone(), moved.clone());
        move |p| {
            if p.ends_with("foto.jpg") {
                fs::rename(&orig, &moved).unwrap();
                fs::write(&orig, vec![7u8; 12_288]).unwrap();
            }
        }
    });
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let st = |n: &str| rep.items.iter().find(|i| i.path.ends_with(n)).unwrap();
    assert_eq!(
        st("foto.jpg").status,
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert!(
        st("foto.jpg").removed.is_empty(),
        "el árbol no se actualiza con lo que no es lo aprobado"
    );
    assert_eq!(
        st("otro.jpg").status,
        ItemStatus::NotRecycled(RecycleFail::Stopped)
    );
    assert!(rep.recycle_stopped);
    assert_eq!(rep.stop_reason, Some(StopReason::Changed));
    assert_eq!((rep.recycled_other, rep.unverified), (0, 0));
    assert!(rep.totals.changed >= 1);
    assert!(moved.exists(), "el original aprobado no se toca");
    assert!(orig.exists(), "el sustituto NO se envió: sigue en su sitio");
    assert!(sb.path("Docs/otro.jpg").exists());
    assert_eq!(rep.recycled_bytes, 0);
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(mine.len(), 0, "nada llegó a la Papelera: {mine:?}");
    assert_eq!(ids(), before);
}

/// M1 con la shell SIMULADA (no toca la Papelera real): lo mismo que la prueba anterior, con la comprobación de identidad
/// del receptor emulada por la shell simulada.
#[test]
fn con_la_shell_simulada_el_sustituto_se_veta_y_el_resto_del_lote_no_se_procesa() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("a.txt", 100, 100),
            ("b.txt", 100, 100),
            ("c.txt", 100, 100),
        ],
    );
    let (orig, moved) = (sb.path("Docs/b.txt"), sb.path("Docs/b-original.txt"));
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), {
        let (orig, moved) = (orig.clone(), moved.clone());
        move |p| {
            if p.ends_with("b.txt") {
                fs::rename(&orig, &moved).unwrap();
                fs::write(&orig, vec![7u8; 100]).unwrap();
            }
        }
    });
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let st = |n: &str| {
        rep.items
            .iter()
            .find(|i| i.path.ends_with(n))
            .unwrap()
            .status
    };
    // `a` se procesó antes del veto; `b` (sustituido) cambió; `c`, detrás en el lote, no se procesó.
    assert_eq!(st("a.txt"), ItemStatus::Recycled);
    assert_eq!(
        st("b.txt"),
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert_eq!(st("c.txt"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert!(orig.exists() && moved.exists() && sb.path("Docs/c.txt").exists());
    assert!(rep.recycle_stopped);
    assert_eq!(rep.stop_reason, Some(StopReason::Changed));
    assert_eq!(rep.stopped_items, 1);
    assert_eq!(rep.shell_error, None);
}

/// M-A: con el handle de GUARDA abierto, cambiar el padre o el abuelo del elemento (para poner un junction hacia otra
/// carpeta, incluso una PROTEGIDA) falla con "acceso denegado" y la shell recicla el aprobado. Medido en esta prueba: si
/// el renombrado llegara a funcionar, el ataque sigue (se crea el junction) y las comprobaciones de abajo lo cantan.
fn ataque_de_ancestro(
    sb: &mut Sandbox,
    victima: &'static str,
    destino: &'static str,
) -> (Report, Vec<Option<i32>>) {
    let c = cleaner_for(sb);
    // `victima` es `Docs/a/b/c/aprobado.txt`; el padre es `Docs/a/b/c`, el abuelo `Docs/a/b`, el bisabuelo `Docs/a`.
    sb.write(victima, 4_000);
    let ajeno = format!("{destino}/b/c/aprobado.txt");
    sb.write(&ajeno, 4_000);
    let mut b = Basket::new();
    b.add_path(sb.long(victima), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    let codes = Arc::new(Mutex::new(Vec::new()));
    let (a, a_orig, dest) = (
        sb.path("Docs/a"),
        sb.path("Docs/a-original"),
        sb.path(destino),
    );
    let (padre, abuelo) = (sb.path("Docs/a/b/c"), sb.path("Docs/a/b"));
    let _hook = {
        let codes = codes.clone();
        crate::engine::test_hooks::install_before_shell(sb.long("Docs"), move |_| {
            // El padre y el abuelo: no se pueden renombrar mientras el elemento esté abierto.
            for (from, to) in [
                (&padre, sb_sibling(&padre, "c-movido")),
                (&abuelo, sb_sibling(&abuelo, "b-movido")),
            ] {
                let r = fs::rename(from, &to);
                codes
                    .lock()
                    .unwrap()
                    .push(r.as_ref().err().and_then(|e| e.raw_os_error()));
                if r.is_ok() {
                    fs::rename(&to, from).unwrap();
                }
            }
            // El ataque entero: cambiar el ancestro por un junction hacia `destino`.
            if fs::rename(&a, &a_orig).is_ok() {
                let out = std::process::Command::new("cmd")
                    .args(["/C", "mklink", "/J"])
                    .arg(&a)
                    .arg(&dest)
                    .output()
                    .unwrap();
                assert!(out.status.success());
                codes.lock().unwrap().push(None);
            } else {
                codes.lock().unwrap().push(Some(-1));
            }
        })
    };
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let v = codes.lock().unwrap().clone();
    (rep, v)
}

fn sb_sibling(p: &Path, name: &str) -> PathBuf {
    p.with_file_name(name)
}

#[test]
fn con_el_handle_de_guarda_no_se_puede_cambiar_un_ancestro_por_un_junction_a_otra_carpeta() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let mut sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let (rep, codes) = ataque_de_ancestro(&mut sb, "Docs/a/b/c/aprobado.txt", "Docs/otra");
    sb.register_junction("Docs/a");
    eprintln!(
        "renombrados durante la operación: {codes:?}; estado: {:?}",
        rep.items[0].status
    );
    // Padre, abuelo y bisabuelo: error 5 (acceso denegado), nada se movió.
    assert_eq!(codes, vec![Some(5), Some(5), Some(-1)], "{codes:?}");
    assert_eq!(rep.items[0].status, ItemStatus::Recycled);
    assert_eq!((rep.recycled_other, rep.unverified), (0, 0));
    assert!(
        sb.path("Docs/otra/b/c/aprobado.txt").exists(),
        "lo ajeno no se toca"
    );
    assert!(
        !sb.path("Docs/a/b/c/aprobado.txt").exists(),
        "lo aprobado está en la Papelera"
    );
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(mine.len(), 1, "{mine:?}");
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

#[test]
fn con_el_handle_de_guarda_un_junction_hacia_una_carpeta_protegida_tampoco_se_cuela() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let mut sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    // `Prot` es una carpeta PROTEGIDA (denegada) con un archivo del mismo nombre en la misma estructura.
    let (rep, codes) = ataque_de_ancestro(&mut sb, "Docs/a/b/c/aprobado.txt", "Prot");
    sb.register_junction("Docs/a");
    assert_eq!(codes, vec![Some(5), Some(5), Some(-1)], "{codes:?}");
    assert_eq!(rep.items[0].status, ItemStatus::Recycled);
    assert!(
        sb.path("Prot/b/c/aprobado.txt").exists(),
        "el archivo protegido no se recicla"
    );
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].original_parent, sb.long("Docs/a/b/c"));
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

/// M-A: un enlace duro. Tras soltar la comprobación se cambia el nombre del archivo y en su sitio se pone otro nombre
/// del MISMO objeto: la shell recicla ese nombre y la identidad coincide. Antes daba un `Recycled` falso (el archivo
/// sigue vivo con su otro nombre y no se ha liberado nada). Ahora el objeto de la guarda no está en la Papelera.
#[test]
fn un_enlace_duro_sustituto_ya_no_da_un_recycled_falso() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    sb.write("Docs/x.txt", 6_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/x.txt"), c.protection());
    let plan = approve(&c, &b);
    let (orig, moved) = (sb.path("Docs/x.txt"), sb.path("Docs/x-original.txt"));
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), {
        let (orig, moved) = (orig.clone(), moved.clone());
        move |_| {
            fs::rename(&orig, &moved).unwrap();
            fs::hard_link(&moved, &orig).unwrap();
        }
    });
    let rep = exec(&c, &plan, false, false, None).unwrap();
    eprintln!("enlace duro sustituto: {:?}", rep.items[0].status);
    assert_eq!(rep.items[0].status, ItemStatus::RecycledOther);
    assert!(rep.items[0].removed.is_empty(), "el árbol no se actualiza");
    assert_eq!(rep.recycled_bytes, 0, "no se cuenta como movido");
    assert!(
        moved.exists(),
        "el objeto aprobado sigue vivo con su otro nombre"
    );
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

// ---- M4: el registro es condición de la limpieza ------------------------------------------------------------------------

/// M4: sin registro NO hay ninguna acción destructiva (ni definitiva ni a la Papelera ni vaciar): la ejecución real sin
/// registro, o con uno que ya falló, devuelve `LogRequired` y no toca nada. La simulación sí puede ir sin registro.
#[test]
fn sin_registro_no_hay_ninguna_accion_destructiva() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write_aged("Temp/viejo.tmp", 10_000, 72);
    sb.write("Docs/b.txt", 1_000);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    b.add_path(sb.long("Docs/b.txt"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
    // Sin registro: nada.
    let opts = ExecOptions::default();
    assert_eq!(
        c.execute(&plan, &opts, &Progress::new()).err(),
        Some(ExecError::LogRequired)
    );
    assert!(sb.path("Temp/viejo.tmp").exists() && sb.path("Docs/b.txt").exists());
    assert!(fake.names().is_empty());
    // Con un registro que ya falló: tampoco.
    let failed = Arc::new(
        LogWriter::create_failing_after(&sb.path("LogMalo"), false, 400).expect("arranca"),
    );
    for i in 0..60 {
        failed.write_note(
            "x",
            &format!("relleno largo para agotar el presupuesto {i}"),
        );
    }
    failed.flush();
    assert!(!failed.ok(), "el registro debería haber fallado");
    assert_eq!(
        c.execute(
            &plan,
            &ExecOptions {
                log: Some(failed),
                ..ExecOptions::default()
            },
            &Progress::new()
        )
        .err(),
        Some(ExecError::LogRequired)
    );
    assert!(sb.path("Temp/viejo.tmp").exists() && sb.path("Docs/b.txt").exists());
    // La simulación va sin registro.
    let dry = exec(&c, &plan, true, false, None).unwrap();
    assert!(dry.dry_run && !dry.log_failed);
    assert!(sb.path("Temp/viejo.tmp").exists() && sb.path("Docs/b.txt").exists());
}

/// M4: un registro que no se puede crear es un error (la interfaz no ejecuta nada): aquí, la carpeta de datos es un archivo.
#[test]
fn un_registro_que_no_se_puede_crear_es_un_error() {
    let sb = Sandbox::new();
    sb.write("Tesela", 10); // un archivo donde debería ir la carpeta
    assert!(LogWriter::create(&sb.path("Tesela"), false).is_err());
    assert!(LogWriter::create(&sb.path("Tesela/sub"), false).is_err());
    // Y uno que falla ya en la línea de arranque.
    assert!(LogWriter::create_failing_after(&sb.path("L0"), false, 0).is_err());
}

/// M4: la INTENCIÓN antes del resultado, para cada operación: el lote que se entrega a la shell lista sus elementos, y el
/// lote definitivo, sus raíces, ANTES de cualquier resultado.
#[test]
fn el_registro_lleva_la_intencion_de_cada_operacion_antes_de_sus_resultados() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write_aged("Temp/viejo.tmp", 10_000, 72);
    let mut plan = plan_de(&sb, &c, &[("a.txt", 100, 100), ("b.txt", 100, 100)]);
    // Más un elemento definitivo (la categoría de temporales).
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    let perm = approve(&c, &b);
    plan.items.extend(perm.items.into_iter().map(|mut i| {
        i.id = 2;
        i
    }));
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    let text = fs::read_to_string(log.path()).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let pos = |kind: &str| lines.iter().position(|v| v["t"] == kind);
    let (del_begin, rec_begin) = (
        pos("delete-begin").expect("falta la intención del lote definitivo"),
        pos("recycle-begin").expect("falta la intención del lote de la Papelera"),
    );
    // La intención del lote definitivo, antes de cualquier resultado de sus entradas.
    let first_entry = pos("entry").expect("hay entradas");
    assert!(del_begin < first_entry, "{del_begin} < {first_entry}");
    // La del envío, antes de los resultados `recycle`.
    let first_recycle = pos("recycle").expect("hay resultados de la Papelera");
    assert!(rec_begin < first_recycle);
    // Lista los elementos del lote.
    let paths = lines[rec_begin]["paths"].as_array().unwrap();
    let names: Vec<&str> = paths.iter().map(|p| p.as_str().unwrap()).collect();
    assert!(
        names.iter().any(|n| n.ends_with("a.txt")) && names.iter().any(|n| n.ends_with("b.txt"))
    );
    let roots = lines[del_begin]["paths"].as_array().unwrap();
    assert!(roots[0].as_str().unwrap().ends_with("Temp"));
    assert!(pos("delete-end").is_some() && pos("summary").is_some());
    assert!(log.ok());
}

/// M4: un error de escritura del registro DETIENE la limpieza en el siguiente punto seguro: si falla la intención del
/// lote definitivo, no se borra nada; si falla a mitad, el resto no se procesa. El informe lo dice (`log_failed`) y no se
/// presenta como una cancelación del usuario.
#[test]
fn un_error_de_escritura_del_registro_detiene_la_limpieza() {
    // (1) Falla ya en la intención: no se borra NADA.
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    for i in 0..30 {
        sb.write_aged(&format!("Temp/f{i:02}.tmp"), 1_000, 72);
    }
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    // El presupuesto alcanza para la línea de arranque (≈ 50 bytes) pero no para la intención.
    let log = Arc::new(LogWriter::create_failing_after(&sb.path("L1"), false, 80).unwrap());
    let rep = exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    assert!(rep.log_failed && !rep.cancelled, "{rep:?}");
    assert!(!log.ok());
    assert_eq!(sb.count_entries("Temp"), 30, "no se borró nada");
    assert!(matches!(
        rep.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Cancelled)
    ));

    // (2) Falla a mitad (la intención cabe, los resultados por entrada no): se detiene en el siguiente punto seguro, con
    // trabajo sin hacer.
    let sb2 = Sandbox::new();
    let c2 = cleaner_for(&sb2).with_threads(2);
    for i in 0..400 {
        sb2.write_aged(&format!("Temp/g{i:03}.tmp"), 100, 72);
    }
    let mut b2 = Basket::new();
    b2.set_category(CategoryId::UserTemp, true);
    let plan2 = approve(&c2, &b2);
    let log2 = Arc::new(LogWriter::create_failing_after(&sb2.path("L2"), false, 1_500).unwrap());
    let rep2 = exec(&c2, &plan2, false, false, Some(log2.clone())).unwrap();
    assert!(rep2.log_failed && !rep2.cancelled, "{rep2:?}");
    let left = sb2.count_entries("Temp");
    assert!(left > 0, "la limpieza debía detenerse antes de terminar");
    assert!(left < 400, "algo se había hecho antes del fallo: {left}");
    assert!(log2.error().is_some());

    // (3) Falla la intención del lote de la Papelera: la shell NO ve nada.
    let sb3 = Sandbox::new();
    let fake = FakeShell::new(&sb3, Some(1 << 40), 0, "nunca");
    let c3 = cleaner_for(&sb3).with_shell(fake.clone());
    let plan3 = plan_de(&sb3, &c3, &[("a.txt", 100, 100), ("b.txt", 100, 100)]);
    let log3 = Arc::new(LogWriter::create_failing_after(&sb3.path("L3"), false, 80).unwrap());
    let rep3 = exec(&c3, &plan3, false, false, Some(log3)).unwrap();
    assert!(rep3.log_failed);
    assert!(fake.names().is_empty(), "no se entregó nada a la shell");
    assert!(sb3.path("Docs/a.txt").exists() && sb3.path("Docs/b.txt").exists());
    for it in &rep3.items {
        assert_eq!(
            it.status,
            ItemStatus::Root(crate::engine::RootStatus::Cancelled)
        );
    }
}

// ---- m1: el número de enlaces aprobado es EXACTO ------------------------------------------------------------------------

/// m1: se aprueba un archivo con 1 enlace y, antes de ejecutar, alguien le añade un segundo nombre (mismo objeto, otra
/// ruta). Con la misma identidad, antes pasaba; ahora el número de enlaces aprobado es exacto y se rechaza ("cambió desde la
/// confirmación"), tanto con destino Papelera como definitivo. Ningún nombre se toca.
#[test]
fn un_enlace_duro_anadido_tras_la_aprobacion_se_rechaza() {
    for destination in [Destination::Recycle, Destination::Permanent] {
        let sb = Sandbox::new();
        let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
        let c = cleaner_for(&sb).with_shell(fake.clone());
        sb.write("Docs/x.bin", 6_000);
        let mut b = Basket::new();
        b.add_path(sb.long("Docs/x.bin"), c.protection());
        let mut plan = approve(&c, &b);
        assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
        assert!(!plan.items[0].hard_linked);
        plan.items[0].destination = destination;
        // Después de aprobar: un segundo nombre para el mismo objeto.
        sb.hard_link("Docs/x.bin", "Docs/x-otro-nombre.bin");
        let rep = exec(&c, &plan, false, false, None).unwrap();
        assert_eq!(
            rep.items[0].status,
            ItemStatus::Root(crate::engine::RootStatus::Changed),
            "{destination:?}"
        );
        assert!(rep.totals.changed >= 1, "{destination:?}");
        assert!(rep.items[0].removed.is_empty());
        assert!(sb.path("Docs/x.bin").exists() && sb.path("Docs/x-otro-nombre.bin").exists());
        assert!(fake.names().is_empty(), "no llegó a la shell");
    }
}

/// m1: lo mismo al revés (se aprueba con 2 enlaces y se quita uno) se rechaza; sin cambios, el elemento con 2 enlaces se
/// envía con normalidad.
#[test]
fn quitar_un_enlace_duro_tras_la_aprobacion_tambien_se_rechaza() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/x.bin", 6_000);
    sb.hard_link("Docs/x.bin", "Docs/y.bin");
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/x.bin"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1);
    assert!(plan.items[0].hard_linked);
    // Sin cambios: se envía.
    let rep_ok = exec(&c, &plan, true, false, None).unwrap();
    assert_eq!(rep_ok.items[0].status, ItemStatus::WouldRecycle);
    // Se quita el otro nombre: el número de enlaces ya no es el aprobado.
    fs::remove_file(sb.path("Docs/y.bin")).unwrap();
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert!(sb.path("Docs/x.bin").exists());
    assert!(fake.names().is_empty());
}

// ---- Reparto en operaciones y corte de seguridad (M-C), con una shell SIMULADA ----------------------------------------

/// Una shell simulada: "recicla" moviendo a una carpeta del sandbox y, si en la operación hay un elemento cuyo nombre
/// contiene `nuke`, la shell decide por operación que no cabe: ELIMINA el primer elemento (`PostDeleteItem` con
/// `psiNewlyCreated` nulo) y el receptor de eventos CORTA el lote (M2): los demás no se procesan ni tienen evento. Un
/// elemento cuyo nombre contiene `unknown` se recicla pero la shell "no dice dónde": también corta el lote. Anota las
/// operaciones.
struct FakeShell {
    bin: PathBuf,
    max: Option<u64>,
    used: Mutex<u64>,
    nuke: &'static str,
    ops: Mutex<Vec<Vec<PathBuf>>>,
    /// Si es `Some`, la shell "falla": sin eventos y con este error (como un fallo de COM o de `PerformOperations`).
    fail: Option<i32>,
}

impl FakeShell {
    fn new(sb: &Sandbox, max: Option<u64>, used: u64, nuke: &'static str) -> Arc<FakeShell> {
        sb.mkdir("FakeBin");
        Arc::new(FakeShell {
            bin: sb.long("FakeBin"),
            max,
            used: Mutex::new(used),
            nuke,
            ops: Mutex::new(Vec::new()),
            fail: None,
        })
    }
    fn failing(sb: &Sandbox, hresult: i32) -> Arc<FakeShell> {
        sb.mkdir("FakeBin");
        Arc::new(FakeShell {
            bin: sb.long("FakeBin"),
            max: Some(10_000),
            used: Mutex::new(0),
            nuke: "nunca",
            ops: Mutex::new(Vec::new()),
            fail: Some(hresult),
        })
    }
    fn names(&self) -> Vec<Vec<String>> {
        self.ops
            .lock()
            .unwrap()
            .iter()
            .map(|op| {
                op.iter()
                    .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                    .collect()
            })
            .collect()
    }
}

impl RecycleShell for FakeShell {
    fn recycle(&self, items: &[recycle::SendItem]) -> recycle::BatchOutcome {
        use crate::recycle::{BinState, DeleteEvent};
        let paths: Vec<PathBuf> = items.iter().map(|i| i.path.clone()).collect();
        let paths = paths.as_slice();
        self.ops.lock().unwrap().push(paths.to_vec());
        if let Some(hresult) = self.fail {
            return recycle::BatchOutcome {
                error: Some(crate::recycle::ShellError {
                    hresult,
                    detail: "fallo simulado".into(),
                }),
                ..recycle::BatchOutcome::default()
            };
        }
        let nuke = paths.iter().any(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().contains(self.nuke))
        });
        let mut events = Vec::new();
        let mut vetoed = Vec::new();
        let mut sink_aborted = false;
        for (i, p) in paths.iter().enumerate() {
            // `PreDeleteItem` (M1): lo que hay ahora en la ruta debe ser el objeto aprobado; si no, veto y se corta el lote.
            let now = sys::open_for_query(p)
                .and_then(|h| sys::meta(&h))
                .map(|m| m.identity);
            if now.ok() != Some(items[i].identity) {
                vetoed.push(recycle::Veto {
                    item: p.clone().into_os_string(),
                    kind: recycle::VetoKind::IdentityMismatch,
                });
                sink_aborted = true;
                break;
            }
            let len = fs::metadata(p).map_or(0, |m| m.len());
            let unknown = p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains("unknown"));
            let intact = p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains("intacto"));
            let state = if nuke {
                fs::remove_file(p).unwrap();
                BinState::Deleted
            } else if intact {
                // La shell notifica "eliminado" (nulo, con éxito) SIN haber tocado nada (medido con un archivo disperso).
                BinState::Deleted
            } else {
                let dest = self.bin.join(format!(
                    "R{}-{i}-{}",
                    self.ops.lock().unwrap().len(),
                    p.file_name().unwrap().to_string_lossy()
                ));
                fs::rename(p, &dest).unwrap();
                *self.used.lock().unwrap() += len;
                if unknown {
                    BinState::InBinUnknownPath
                } else {
                    BinState::InBin(dest)
                }
            };
            let unsafe_outcome = matches!(state, BinState::Deleted | BinState::InBinUnknownPath);
            events.push(DeleteEvent {
                item: p.clone().into_os_string(),
                hr: 0,
                state,
            });
            // El receptor registra el evento y DESPUÉS devuelve E_ABORT: la shell no procesa el resto.
            if unsafe_outcome {
                sink_aborted = true;
                break;
            }
        }
        recycle::BatchOutcome {
            events,
            sink_aborted,
            vetoed,
            error: sink_aborted.then(|| crate::recycle::ShellError {
                hresult: windows::Win32::Foundation::E_ABORT.0,
                detail: "cancelado por el receptor".into(),
            }),
            ..recycle::BatchOutcome::default()
        }
    }
    fn bin_room(&self, _root: &Path) -> BinRoom {
        BinRoom {
            max: self.max,
            used: Some(*self.used.lock().unwrap()),
        }
    }
    fn is_bin_path(&self, p: &Path) -> bool {
        crate::paths::PathKey::new(p).is_same_or_under(&crate::paths::PathKey::new(&self.bin))
    }
    /// El encaje no depende del equipo (registro, directivas): cabe salvo que pase del 90 % de la capacidad simulada.
    fn fit(&self, _path: &Path, bytes: u64) -> Fit {
        match self.max {
            Some(max) if (bytes as u128) * 100 > (max as u128) * 90 => {
                Fit::Unfit(UnfitReason::TooBig)
            }
            _ => Fit::Fits,
        }
    }
}

/// Un plan de elementos de `Docs/<nombre>` con el tamaño dado: el archivo real mide `len` bytes y la ESTIMACIÓN que se
/// reparte es `est` (así se prueba el reparto sin archivos enormes).
fn plan_de(sb: &Sandbox, c: &Cleaner, items: &[(&str, usize, u64)]) -> Plan {
    let mut b = Basket::new();
    for (name, len, _) in items {
        sb.write(&format!("Docs/{name}"), *len);
        b.add_path(sb.long(&format!("Docs/{name}")), c.protection());
    }
    let mut plan = approve(c, &b);
    assert_eq!(plan.items.len(), items.len(), "{:?}", plan.rejected);
    for (it, (_, _, est)) in plan.items.iter_mut().zip(items) {
        it.estimate.bytes = *est;
        it.estimate.logical = *est;
        it.fit = c.fit(&it.path, *est);
        assert_eq!(it.fit, Fit::Fits);
    }
    plan
}

#[test]
fn un_elemento_que_no_cabe_entre_otros_pequenos_nunca_arrastra_a_los_demas_y_corta_el_envio() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(10_000), 0, "enorme");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    // `enorme` pesa 5 000 (más del 10 % de la capacidad de 10 000): va SOLO. Los pequeños, por separado.
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("a.txt", 100, 100),
            ("enorme.bin", 100, 5_000),
            ("c.txt", 100, 100),
            ("d.txt", 100, 100),
        ],
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let st = |n: &str| {
        rep.items
            .iter()
            .find(|i| i.path.ends_with(n))
            .unwrap()
            .status
    };
    assert_eq!(st("a.txt"), ItemStatus::Recycled);
    assert_eq!(st("enorme.bin"), ItemStatus::DeletedUnexpectedly);
    // CORTE: lo que venía detrás NO se envió (ni se elimina): sigue en su sitio.
    assert_eq!(st("c.txt"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert_eq!(st("d.txt"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert!(sb.path("Docs/c.txt").exists() && sb.path("Docs/d.txt").exists());
    assert!(rep.recycle_stopped);
    assert_eq!((rep.deleted_unexpectedly, rep.stopped_items), (1, 2));
    // El primero fue en su propia operación (no con el enorme) y el enorme en la suya, y no hubo más.
    assert_eq!(fake.names(), vec![vec!["a.txt"], vec!["enorme.bin"]]);
}

/// M2: el primer borrado definitivo inesperado de una operación CORTA el lote en curso (el receptor devuelve `E_ABORT`
/// tras registrar el evento): lo que venía detrás en la MISMA operación no se procesa y sigue en su sitio, y no se envía
/// nada más. El `E_ABORT` propio no se cuenta como error de la shell.
#[test]
fn el_primer_borrado_inesperado_corta_el_lote_en_curso() {
    let sb = Sandbox::new();
    // Todos pequeños (van juntos en UNA operación) y uno con nombre `enorme`: la shell decide que la operación no cabe.
    let fake = FakeShell::new(&sb, Some(10_000), 0, "enorme");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("a.txt", 100, 100),
            ("enorme.bin", 100, 100),
            ("c.txt", 100, 100),
        ],
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let st = |n: &str| {
        rep.items
            .iter()
            .find(|i| i.path.ends_with(n))
            .unwrap()
            .status
    };
    assert_eq!(fake.names().len(), 1, "una sola operación");
    // El primero se eliminó (evento registrado); el resto de la operación NO se procesó y sigue ahí.
    assert_eq!(st("a.txt"), ItemStatus::DeletedUnexpectedly);
    assert_eq!(
        st("enorme.bin"),
        ItemStatus::NotRecycled(RecycleFail::Stopped)
    );
    assert_eq!(st("c.txt"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert!(sb.path("Docs/enorme.bin").exists() && sb.path("Docs/c.txt").exists());
    assert!(rep.recycle_stopped);
    assert_eq!(rep.stop_reason, Some(StopReason::DeletedUnexpectedly));
    assert_eq!((rep.deleted_unexpectedly, rep.stopped_items), (1, 2));
    assert_eq!(
        rep.shell_error, None,
        "el E_ABORT propio no es un error de la shell"
    );
}

/// H2: la shell notifica `psiNewlyCreated` nulo con éxito pero NO tocó el elemento (medido con una carpeta que contiene un
/// archivo disperso enorme: `hr = 0x00270003` y la carpeta sigue en el disco). No se afirma que se eliminó nada
/// (`DeletedUnexpectedly` solo si el nombre ya no está), pero el corte de seguridad se mantiene.
#[test]
fn una_eliminacion_notificada_sin_tocar_nada_no_se_cuenta_como_borrado_y_sigue_cortando() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("intacto.txt", 100, 100),
            ("b.txt", 100, 100),
            ("c.txt", 100, 100),
        ],
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let st = |n: &str| {
        rep.items
            .iter()
            .find(|i| i.path.ends_with(n))
            .unwrap()
            .status
    };
    assert!(matches!(
        st("intacto.txt"),
        ItemStatus::NotRecycled(RecycleFail::Shell(0))
    ));
    assert_eq!(st("b.txt"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert_eq!(st("c.txt"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert!(sb.path("Docs/intacto.txt").exists());
    assert_eq!(rep.deleted_unexpectedly, 0, "no se borró nada");
    assert!(rep.recycle_stopped, "el corte se mantiene");
    assert_eq!(rep.stop_reason, Some(StopReason::ShellDeclined));
    assert_eq!(rep.stopped_items, 2);
}

/// M2: una ruta sin verificar (`InBinUnknownPath` con éxito) también corta el lote, y `Unverified` provoca
/// `recycle_stopped`.
#[test]
fn una_ruta_sin_verificar_tambien_corta_el_lote_y_marca_recycle_stopped() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(10_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("unknown.txt", 100, 100),
            ("b.txt", 100, 100),
            ("c.txt", 100, 100),
        ],
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let st = |n: &str| {
        rep.items
            .iter()
            .find(|i| i.path.ends_with(n))
            .unwrap()
            .status
    };
    assert_eq!(st("unknown.txt"), ItemStatus::Unverified);
    assert_eq!(st("b.txt"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert_eq!(st("c.txt"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert!(sb.path("Docs/b.txt").exists() && sb.path("Docs/c.txt").exists());
    assert!(rep.recycle_stopped);
    assert_eq!(rep.stop_reason, Some(StopReason::Unverified));
    assert_eq!((rep.unverified, rep.stopped_items), (1, 2));
}

#[test]
fn los_pequenos_se_agrupan_hasta_el_presupuesto_y_nunca_pasan_del_90_por_ciento_menos_lo_ocupado() {
    let sb = Sandbox::new();
    // Capacidad 10 000: un elemento "grande" (> 1 000) va solo; el presupuesto es 9 000 menos lo ocupado.
    let fake = FakeShell::new(&sb, Some(10_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("a", 400, 400),
            ("b", 400, 400),
            ("c", 400, 400),
            ("grande", 400, 2_000),
            ("d", 400, 400),
        ],
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert!(
        rep.items.iter().all(|i| i.status == ItemStatus::Recycled),
        "{:?}",
        rep.items
    );
    // a, b y c juntos; el grande solo; d solo (lo que queda de la cola).
    assert_eq!(
        fake.names(),
        vec![vec!["a", "b", "c"], vec!["grande"], vec!["d"]]
    );
    assert!(!rep.recycle_stopped);

    // Con la Papelera casi llena el presupuesto se agota: lo que no cabe NO se envía.
    let sb2 = Sandbox::new();
    let fake2 = FakeShell::new(&sb2, Some(10_000), 8_500, "nunca");
    let c2 = cleaner_for(&sb2).with_shell(fake2.clone());
    let plan2 = plan_de(
        &sb2,
        &c2,
        &[
            ("f1", 400, 400),
            ("f2", 400, 400),
            ("f3", 400, 400),
            ("f4", 400, 400),
        ],
    );
    let rep2 = exec(&c2, &plan2, false, false, None).unwrap();
    let st = |n: &str| {
        rep2.items
            .iter()
            .find(|i| i.path.ends_with(n))
            .unwrap()
            .status
    };
    // Presupuesto = 9 000 - 8 500 = 500: cabe uno (400); después, 100: ninguno más.
    assert_eq!(st("f1"), ItemStatus::Recycled);
    for n in ["f2", "f3", "f4"] {
        assert_eq!(st(n), ItemStatus::NotRecycled(RecycleFail::NoRoom), "{n}");
        assert!(sb2.path(&format!("Docs/{n}")).exists());
    }
    assert_eq!(fake2.names(), vec![vec!["f1"]]);
    assert_eq!(rep2.no_room, 3);
    assert!(!rep2.recycle_stopped);
}

/// B2: sin capacidad conocida de la Papelera no se puede garantizar que quepa (y la shell, si no cabe, elimina de verdad):
/// NINGÚN elemento se envía (`NoRoom`), ni siquiera los pequeños. Antes iba cada uno solo.
#[test]
fn sin_capacidad_conocida_no_se_envia_nada_a_la_papelera() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, None, 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(
        &sb,
        &c,
        &[("a", 100, 100), ("b", 100, 100), ("c", 100, 100)],
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    for it in &rep.items {
        assert_eq!(
            it.status,
            ItemStatus::NotRecycled(RecycleFail::NoRoom),
            "{:?}",
            it.path
        );
    }
    assert!(fake.names().is_empty(), "la shell no vio ninguna operación");
    for n in ["a", "b", "c"] {
        assert!(sb.path(&format!("Docs/{n}")).exists());
    }
    assert_eq!(rep.no_room, 3);
    assert!(!rep.recycle_stopped);
}

/// B2: la capacidad del reparto es SOLO la configurada (`MaxCapacity`); sin ella (`Unknown`) y en cualquier otro caso
/// (sin Papelera, desactivada, indeterminada) no hay capacidad: `max = None` y el reparto falla cerrado. Ya no se asume un
/// porcentaje del volumen.
#[test]
fn la_capacidad_del_reparto_es_solo_la_configurada() {
    use crate::volume::BinLimit;
    assert_eq!(room_from(BinLimit::Max(777), None).max, Some(777));
    assert_eq!(room_from(BinLimit::Max(777), Some(5)).used, Some(5));
    for l in [
        BinLimit::Unknown,
        BinLimit::NoBin,
        BinLimit::NukeOnDelete,
        BinLimit::Indeterminate,
    ] {
        assert_eq!(room_from(l, Some(0)).max, None, "{l:?}");
        assert_eq!(take_op(&[1], room_from(l, Some(0))), Take::NoRoom, "{l:?}");
    }
    // Capacidad configurada pero ocupación desconocida: tampoco se envía.
    assert_eq!(
        take_op(&[1], room_from(BinLimit::Max(10_000), None)),
        Take::NoRoom
    );
    // Con una capacidad configurada, la shell simulada reparte y rechaza como siempre.
    let sb = Sandbox::new();
    let room = room_from(BinLimit::Max(10_000), Some(0));
    let fake = FakeShell::new(&sb, room.max, 0, "enorme");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    // Capacidad 10 000: lo que pase de 9 000 no cabe ni solo; un "grande" (> 1 000) va solo; los pequeños, juntos.
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("a", 100, 100),
            ("b", 100, 100),
            ("grande", 100, 2_000),
            ("c", 100, 100),
        ],
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    for it in &rep.items {
        assert_eq!(it.status, ItemStatus::Recycled, "{:?}", it.path);
    }
    assert_eq!(
        fake.names(),
        vec![vec!["a", "b"], vec!["grande"], vec!["c"]]
    );
}

/// Un directorio de `Docs/<nombre>` con un archivo de `len` bytes, aprobado con su medida real.
fn plan_dir(sb: &Sandbox, c: &Cleaner, name: &str, len: usize) -> Plan {
    sb.write(&format!("Docs/{name}/f0.bin"), len);
    let mut b = Basket::new();
    b.add_path(sb.long(&format!("Docs/{name}")), c.protection());
    let plan = approve(c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    assert!(plan.items[0].is_dir());
    plan
}

/// B2: un directorio que CRECIÓ 1 MiB después de aprobarlo no se envía: el reparto se basaba en un tamaño que ya no es
/// cierto. Queda en su sitio con el estado "cambió desde la confirmación" (contador `changed` del resumen).
#[test]
fn un_directorio_que_crecio_tras_la_aprobacion_no_se_envia() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_dir(&sb, &c, "crece", 5_000);
    // Después de aprobar: 1 MiB más dentro.
    sb.write("Docs/crece/nuevo.bin", 1 << 20);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert!(rep.items[0].removed.is_empty());
    assert!(sb.path("Docs/crece/f0.bin").exists() && sb.path("Docs/crece/nuevo.bin").exists());
    assert!(fake.names().is_empty(), "no llegó a la shell");
    assert!(
        rep.totals.changed >= 1,
        "consta como cambiado en el resumen"
    );
    // Sin cambios, el mismo directorio sí se envía (control).
    let sb2 = Sandbox::new();
    let fake2 = FakeShell::new(&sb2, Some(1 << 40), 0, "nunca");
    let c2 = cleaner_for(&sb2).with_shell(fake2.clone());
    let plan2 = plan_dir(&sb2, &c2, "igual", 5_000);
    let rep2 = exec(&c2, &plan2, false, false, None).unwrap();
    assert_eq!(rep2.items[0].status, ItemStatus::Recycled);
}

/// B2: un directorio que ENCOGIÓ se reparte con su tamaño nuevo (no con el aprobado). El aprobado (estimación falseada
/// a 8 500 con 2 000 ya ocupados de una capacidad de 10 000: presupuesto 7 000) no cabría; el real, mucho menor, sí.
#[test]
fn un_directorio_que_encogio_se_reparte_con_su_tamano_nuevo() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(10_000), 2_000, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let mut plan = plan_dir(&sb, &c, "encoge", 1_000);
    // La estimación al aprobar era enorme (el directorio contenía mucho más).
    plan.items[0].estimate.bytes = 8_500;
    plan.items[0].estimate.logical = 8_500;
    plan.items[0].fit = c.fit(&plan.items[0].path, 8_500);
    assert_eq!(plan.items[0].fit, Fit::Fits);
    // Con la estimación vieja, `take_op` diría NoRoom (8 500 > 7 000): la medida nueva manda.
    let room = BinRoom {
        max: Some(10_000),
        used: Some(2_000),
    };
    assert_eq!(take_op(&[8_500], room), Take::NoRoom);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Recycled,
        "{:?}",
        rep.items[0]
    );
    assert_eq!(fake.names(), vec![vec!["encoge"]]);
}

/// Un error de la shell o de COM al enviar consta en el registro local (con su HRESULT) y en el informe (para el resumen);
/// los elementos quedan sin tocar.
#[test]
fn un_error_de_la_shell_consta_en_el_registro_y_en_el_informe() {
    let sb = Sandbox::new();
    let fake = FakeShell::failing(&sb, 0x8000_4005u32 as i32);
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(&sb, &c, &[("a.txt", 100, 100), ("b.txt", 100, 100)]);
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    let rep = exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    assert_eq!(rep.shell_error, Some(0x8000_4005u32 as i32));
    // Sin eventos: nada se movió y los elementos siguen en su sitio, como no procesados.
    for n in ["a.txt", "b.txt"] {
        let it = rep.items.iter().find(|i| i.path.ends_with(n)).unwrap();
        assert_eq!(
            it.status,
            ItemStatus::NotRecycled(RecycleFail::NotProcessed)
        );
        assert!(sb.path(&format!("Docs/{n}")).exists());
    }
    let text = fs::read_to_string(log.path()).unwrap();
    let line = text
        .lines()
        .find(|l| l.contains("recycle-error"))
        .expect("falta la nota del error");
    assert!(
        line.contains("80004005") && line.contains("fallo simulado"),
        "{line}"
    );
    // Un envío sin errores no deja `shell_error`.
    let sb2 = Sandbox::new();
    let ok = FakeShell::new(&sb2, Some(10_000), 0, "nunca");
    let c2 = cleaner_for(&sb2).with_shell(ok);
    let plan2 = plan_de(&sb2, &c2, &[("a.txt", 100, 100)]);
    assert_eq!(
        exec(&c2, &plan2, false, false, None).unwrap().shell_error,
        None
    );
}

#[test]
fn take_op_es_una_funcion_pura_con_las_reglas_del_reparto() {
    let room = |max, used| BinRoom {
        max: Some(max),
        used: Some(used),
    };
    // Sin capacidad ni ocupación conocidas: nunca se envía (B2).
    assert_eq!(take_op(&[10, 10], BinRoom::UNKNOWN), Take::NoRoom);
    let sin_max = BinRoom {
        max: None,
        used: Some(0),
    };
    assert_eq!(take_op(&[1], sin_max), Take::NoRoom);
    // Todo lo pequeño junto mientras quepa en el presupuesto (90 % de 10 000 menos lo ocupado).
    assert_eq!(take_op(&[300, 300, 300], room(10_000, 0)), Take::Send(3));
    assert_eq!(
        take_op(&[300, 300, 300], room(10_000, 8_400)),
        Take::Send(2)
    );
    // Un elemento que pasa del 10 % va solo, sea el primero o el que cortaría el grupo.
    assert_eq!(take_op(&[1_500, 10], room(10_000, 0)), Take::Send(1));
    assert_eq!(take_op(&[100, 1_500, 100], room(10_000, 0)), Take::Send(1));
    // El primero no cabe en lo que queda: no se envía; con la capacidad justa (un 90 %), sí.
    assert_eq!(take_op(&[600, 10], room(10_000, 8_700)), Take::NoRoom);
    assert_eq!(take_op(&[9_000], room(10_000, 0)), Take::Send(1));
    assert_eq!(take_op(&[9_001], room(10_000, 0)), Take::NoRoom);
    // Sin ocupación conocida: no se envía (antes iba solo si cabía en una Papelera vacía).
    let sin_uso = BinRoom {
        max: Some(10_000),
        used: None,
    };
    assert_eq!(take_op(&[800, 800], sin_uso), Take::NoRoom);
    assert_eq!(take_op(&[1], sin_uso), Take::NoRoom);
    assert_eq!(take_op(&[9_500], sin_uso), Take::NoRoom);
    // Una lista vacía no envía nada.
    assert_eq!(take_op(&[], BinRoom::UNKNOWN), Take::Send(0));
}

/// M-C con la shell REAL, de forma segura: un archivo disperso de miles de GiB lógicos (no ocupa nada) entre dos pequeños.
/// Lo que ocupa de verdad (su tamaño lógico) manda sobre la estimación: no se envía (`NoRoom`) y los pequeños llegan
/// bien a la Papelera, cada uno en su operación. La shell real nunca ve el enorme, así que no puede eliminarlo ni
/// expulsar nada ajeno.
#[test]
fn con_la_shell_real_un_disperso_enorme_no_se_envia_y_los_pequenos_llegan_bien() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    // Hace falta una capacidad conocida: sin ella el reparto no se puede probar (cada uno iría solo).
    let room =
        crate::exec::RealShell.bin_room(&crate::volume::volume_root(&sb.long_root()).unwrap());
    if room.max.is_none() {
        eprintln!("la Papelera de este volumen no tiene capacidad configurada: la prueba se salta");
        return;
    }
    let total = tesela_scanner::volume_space(&sb.long_root())
        .map(|v| v.total)
        .unwrap_or(1 << 40);
    let len = total.saturating_mul(2).max(1 << 40);
    sb.write("Docs/uno.txt", 1_000);
    sb.write("Docs/tres.txt", 1_000);
    crate::testutil::sparse_file(&sb.path("Docs/enorme.bin"), len);
    let mut b = Basket::new();
    for n in ["uno.txt", "enorme.bin", "tres.txt"] {
        b.add_path(sb.long(&format!("Docs/{n}")), c.protection());
    }
    let mut plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 3);
    // En el plan honrado el enorme ya sale como "no cabe" (grupo ámbar). Se FALSEA su estimación para que el plan lo dé
    // por bueno: es justo el caso de una estimación que se queda corta, que el tamaño real corrige.
    for it in &mut plan.items {
        if it.path.ends_with("enorme.bin") {
            assert!(it.is_unfit());
            it.estimate.bytes = 1_000;
            it.estimate.logical = 1_000;
            it.estimate.shared_bytes = 0;
            it.fit = crate::plan::fit_for(&it.path, 1_000);
            assert_eq!(it.fit, Fit::Fits);
        }
    }
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let st = |n: &str| {
        rep.items
            .iter()
            .find(|i| i.path.ends_with(n))
            .unwrap()
            .status
    };
    assert_eq!(st("uno.txt"), ItemStatus::Recycled);
    assert_eq!(st("tres.txt"), ItemStatus::Recycled);
    assert_eq!(
        st("enorme.bin"),
        ItemStatus::NotRecycled(RecycleFail::NoRoom)
    );
    assert!(sb.path("Docs/enorme.bin").exists(), "el enorme no se tocó");
    assert!(!rep.recycle_stopped && rep.deleted_unexpectedly == 0);
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(mine.len(), 2, "{mine:?}");
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

/// Un `Pending` de prueba: la guarda está abierta sobre `held_path` (su ruta final es la que cuenta para "está en la
/// Papelera"); `path` es el nombre aprobado.
fn pending(identity: FileIdentity, path: &Path, held_path: &Path) -> Pending {
    Pending {
        path: path.to_path_buf(),
        identity,
        links: 1,
        held: sys::open_path_for_rename(held_path).unwrap(),
        size: 0,
    }
}

fn ident(n: u128) -> FileIdentity {
    FileIdentity {
        volume_serial: 7,
        file_id: n,
    }
}

/// El juicio por elemento sale de lo que dijo la shell: identidad igual / distinta, eliminado, error, sin aviso. Y
/// `Recycled` exige además que el nombre aprobado ya no exista y que el objeto de la guarda esté en una Papelera.
#[test]
fn el_juicio_distingue_enviado_otro_objeto_eliminado_error_y_sin_aviso() {
    use crate::recycle::{BinState, DeleteEvent};
    let sb = Sandbox::new();
    let bin_file = sb.write("bin/R1.txt", 10);
    let shell = FakeShell::new(&sb, None, 0, "nunca");
    let shell: &dyn RecycleShell = shell.as_ref();
    // `bin/` hace de Papelera; el nombre aprobado (relativo, inexistente) "ya no está".
    let fake_bin = sb.long("bin");
    let shell_bin = {
        struct B(PathBuf);
        impl RecycleShell for B {
            fn recycle(&self, _: &[recycle::SendItem]) -> recycle::BatchOutcome {
                recycle::BatchOutcome::default()
            }
            fn bin_room(&self, _: &Path) -> BinRoom {
                BinRoom::UNKNOWN
            }
            fn fit(&self, _: &Path, _: u64) -> Fit {
                Fit::Fits
            }
            fn is_bin_path(&self, p: &Path) -> bool {
                crate::paths::PathKey::new(p).is_same_or_under(&crate::paths::PathKey::new(&self.0))
            }
        }
        B(fake_bin)
    };
    let _ = shell;
    let shell: &dyn RecycleShell = &shell_bin;
    let id = {
        let h = sys::open_for_query(&bin_file).unwrap();
        sys::meta(&h).unwrap().identity
    };
    let gone = Path::new("no-existe-x");
    let ev = |hr: i32, state: BinState| DeleteEvent {
        item: OsString::from("no-existe-x"),
        hr,
        state,
    };
    // Misma identidad, el nombre aprobado ya no existe y el objeto de la guarda está en la Papelera -> enviado.
    let p = pending(id, gone, &bin_file);
    let e = ev(0, BinState::InBin(bin_file.clone()));
    assert_eq!(
        judge(&p, Some(&e), shell, false, false).0,
        ItemStatus::Recycled
    );
    // ...pero si el nombre aprobado SIGUE existiendo (otro nombre del mismo objeto llegó a la Papelera), no.
    let p_name = pending(id, &bin_file, &bin_file);
    assert_eq!(
        judge(&p_name, Some(&e), shell, false, false).0,
        ItemStatus::RecycledOther
    );
    // ...ni si el objeto de la guarda NO está en la Papelera (sigue en su sitio con otro nombre).
    let fuera = sb.write("Docs/vivo.txt", 10);
    let p_fuera = pending(id, gone, &fuera);
    assert_eq!(
        judge(&p_fuera, Some(&e), shell, false, false).0,
        ItemStatus::RecycledOther
    );
    // ...ni si el número de enlaces del objeto cambió respecto a lo comprobado (se le añadió otro nombre).
    let mut p_links = pending(id, gone, &bin_file);
    p_links.links = 3;
    assert_eq!(
        judge(&p_links, Some(&e), shell, false, false).0,
        ItemStatus::RecycledOther
    );
    // Otra identidad -> se movió a la Papelera un objeto distinto del aprobado.
    let p2 = pending(ident(id.file_id ^ 1), gone, &bin_file);
    assert_eq!(
        judge(&p2, Some(&e), shell, false, false).0,
        ItemStatus::RecycledOther
    );
    let p3 = pending(
        FileIdentity {
            volume_serial: id.volume_serial ^ 1,
            file_id: id.file_id,
        },
        gone,
        &bin_file,
    );
    assert_eq!(
        judge(&p3, Some(&e), shell, false, false).0,
        ItemStatus::RecycledOther
    );
    // La shell lo eliminó definitivamente (`psiNewlyCreated` nulo) y el nombre ya no está.
    assert_eq!(
        judge(&p, Some(&ev(0, BinState::Deleted)), shell, false, false).0,
        ItemStatus::DeletedUnexpectedly
    );
    // H2: nulo con éxito pero el elemento SIGUE en su sitio: la shell no tocó nada (no se dice que se eliminó).
    let here_ev = DeleteEvent {
        item: OsString::from(bin_file.as_os_str()),
        hr: 0x0027_0003,
        state: BinState::Deleted,
    };
    let p_here = pending(id, &bin_file, &bin_file);
    assert_eq!(
        judge(&p_here, Some(&here_ev), shell, false, false).0,
        ItemStatus::NotRecycled(RecycleFail::Shell(0x0027_0003))
    );
    // No dio ruta, o la ruta no se puede abrir -> no verificado.
    assert_eq!(
        judge(
            &p,
            Some(&ev(0, BinState::InBinUnknownPath)),
            shell,
            false,
            false
        )
        .0,
        ItemStatus::Unverified
    );
    let nada = ev(0, BinState::InBin(sb.path("bin/no-esta.txt")));
    assert_eq!(
        judge(&p, Some(&nada), shell, false, false).0,
        ItemStatus::Unverified
    );
    // Errores: en uso, sin permiso y otros, con su HRESULT.
    let hr_win32 = |c: u32| (0x8007_0000u32 | c) as i32;
    assert_eq!(
        judge(
            &p,
            Some(&ev(hr_win32(32), BinState::Deleted)),
            shell,
            false,
            false
        )
        .0,
        ItemStatus::NotRecycled(RecycleFail::InUse)
    );
    assert_eq!(
        judge(
            &p,
            Some(&ev(hr_win32(5), BinState::Deleted)),
            shell,
            false,
            false
        )
        .0,
        ItemStatus::NotRecycled(RecycleFail::Denied)
    );
    assert_eq!(
        judge(
            &p,
            Some(&ev(0x8000_4005u32 as i32, BinState::Deleted)),
            shell,
            false,
            false
        )
        .0,
        ItemStatus::NotRecycled(RecycleFail::Shell(0x8000_4005u32 as i32))
    );
    // Códigos del motor de copia de la shell: acceso denegado (medido con una carpeta con algo abierto dentro) y violación
    // de uso compartido.
    assert_eq!(
        judge(
            &p,
            Some(&ev(0x8027_0021u32 as i32, BinState::Deleted)),
            shell,
            false,
            false
        )
        .0,
        ItemStatus::NotRecycled(RecycleFail::Denied)
    );
    assert_eq!(
        judge(
            &p,
            Some(&ev(0x8027_0027u32 as i32, BinState::Deleted)),
            shell,
            false,
            false
        )
        .0,
        ItemStatus::NotRecycled(RecycleFail::InUse)
    );
    // Sin aviso: sigue en su sitio -> no procesado (o abortado, si la shell dijo que abortó); ya no está -> no verificado.
    let here = pending(id, &bin_file, &bin_file);
    assert_eq!(
        judge(&here, None, shell, false, false).0,
        ItemStatus::NotRecycled(RecycleFail::NotProcessed)
    );
    assert_eq!(
        judge(&here, None, shell, true, false).0,
        ItemStatus::NotRecycled(RecycleFail::Aborted)
    );
    // El receptor cortó el lote (M2): sin aviso y en su sitio = detenido por el corte de seguridad.
    assert_eq!(
        judge(&here, None, shell, false, true).0,
        ItemStatus::NotRecycled(RecycleFail::Stopped)
    );
    assert_eq!(
        judge(&p, None, shell, false, false).0,
        ItemStatus::Unverified
    );
}

#[test]
fn el_informe_dice_que_desaparecio_de_verdad_para_actualizar_el_arbol() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write_aged("Temp/viejo.tmp", 20_000, 72);
    sb.write_aged("Temp/dir-entero/a.tmp", 10_000, 72);
    sb.set_age(&sb.path("Temp/dir-entero"), 72);
    // Una carpeta vieja con un archivo viejo y otro nuevo: se queda y solo encoge.
    sb.write_aged("Temp/dir-parcial/viejo.tmp", 30_000, 72);
    sb.write("Temp/dir-parcial/nuevo.tmp", 5_000);
    sb.set_age(&sb.path("Temp/dir-parcial"), 72);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let removed = &rep.items[0].removed;
    let get = |n: &str| {
        removed
            .iter()
            .find(|r| r.path.ends_with(n))
            .unwrap_or_else(|| panic!("{n}: {removed:?}"))
    };
    assert!(
        get("viejo.tmp").removed && !get("viejo.tmp").is_dir && get("viejo.tmp").bytes >= 20_000
    );
    assert!(get("dir-entero").removed && get("dir-entero").is_dir && get("dir-entero").files == 1);
    let p = get("dir-parcial");
    assert!(!p.removed && p.is_dir, "{p:?}");
    assert!(p.bytes >= 30_000 && p.files == 1);
    // Y en simulación no se anota nada (no desapareció nada).
    let sb2 = Sandbox::new();
    let c2 = cleaner_for(&sb2);
    sb2.write_aged("Temp/x.tmp", 1_000, 72);
    let mut b2 = Basket::new();
    b2.set_category(CategoryId::UserTemp, true);
    let rep2 = exec(&c2, &approve(&c2, &b2), true, false, None).unwrap();
    assert!(rep2.items[0].removed.is_empty());
    assert!(sb2.path("Temp/x.tmp").exists());
}
