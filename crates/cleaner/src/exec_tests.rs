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
use crate::recycle::bintest::{BinGuard, bin_usable, ids, is_mine};
use crate::roots::Roots;
use crate::rules::{CategoryId, Risk};
use crate::testutil::{Sandbox, bin_lock};
use std::fs;
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
        protection: ProtectionRoots {
            denied: vec![sb.long("Local/Google/Chrome/User Data")],
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

fn exec(
    c: &Cleaner,
    plan: &Plan,
    dry: bool,
    allow_unfit: bool,
    log: Option<Arc<LogWriter>>,
) -> Result<Report, ExecError> {
    c.execute(
        plan,
        &ExecOptions {
            dry_run: dry,
            allow_unfit_permanent: allow_unfit,
            log,
        },
        &Progress::new(),
    )
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
    let ids_before = ids();
    let rep = exec(&c, &plan, true, false, Some(log.clone())).unwrap();
    assert!(rep.dry_run);
    assert_eq!(
        (sb.count_entries("Temp"), sb.count_entries("Docs")),
        before,
        "--dry-run no toca nada"
    );
    assert_eq!(ids(), ids_before, "ni la Papelera");
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
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let bin = rep.bin.unwrap();
    assert_eq!((bin.requested, bin.purged, bin.still_there), (2, 2, 0));
    assert!(!bin.failed);
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
        ItemStatus::NotRecycled(RecycleFail::InUse),
        "una carpeta con algo abierto dentro falla en seguro (mover es renombrar) y se informa como en uso"
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
        .execute(&plan, &ExecOptions::default(), &progress)
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
        // Al aprobar: nunca llega al plan.
        let net = via_share(host, &sb.long("Docs/privado.txt"));
        let mut b = Basket::new();
        b.add_path(net.clone(), c.protection());
        let plan = approve(&c, &b);
        eprintln!("{host}: rechazos al aprobar = {:?}", plan.rejected);
        assert!(plan.items.is_empty(), "{host}: {:?}", plan.items);
        assert!(plan.rejected.iter().all(|r| matches!(
            r.reason,
            RejectReason::NotLocal
                | RejectReason::AccessDenied
                | RejectReason::Missing
                | RejectReason::Other(_)
        )));
        assert!(!plan.rejected.is_empty());
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

/// Carrera: tras SOLTAR el handle y antes de que la shell abra el elemento, el mismo usuario lo sustituye por otro
/// objeto del mismo nombre y tamaño. La shell manda el sustituto a la Papelera, el receptor lo ve (identidad distinta) y se
/// informa; el original no se toca.
#[test]
fn sustituir_tras_soltar_el_handle_manda_otro_objeto_a_la_papelera_y_se_informa() {
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
    assert_eq!(st("foto.jpg").status, ItemStatus::RecycledOther);
    assert!(
        st("foto.jpg").removed.is_empty(),
        "el árbol no se actualiza con lo que no es lo aprobado"
    );
    assert_eq!(st("otro.jpg").status, ItemStatus::Recycled);
    assert_eq!((rep.recycled_other, rep.unverified), (1, 0));
    assert!(moved.exists(), "el original aprobado no se toca");
    assert!(
        !orig.exists(),
        "el sustituto está en la Papelera (recuperable)"
    );
    assert_eq!(
        rep.recycled_bytes,
        st("otro.jpg").stats.bytes,
        "solo lo verificado cuenta como movido"
    );
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(mine.len(), 2, "{mine:?}");
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

/// Carrera: tras soltar el handle, un ancestro pasa a ser un junction a otra carpeta del mismo árbol. Lo peor que puede
/// pasar es que la shell mande a la Papelera lo que hay en el destino del junction: el receptor lo ve por la identidad.
#[test]
fn con_un_ancestro_cambiado_por_un_junction_tras_soltar_el_handle_nunca_se_da_por_bueno() {
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
    let (a, a_orig, otra) = (
        sb.path("Docs/a"),
        sb.path("Docs/a-original"),
        sb.path("Docs/otra"),
    );
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), move |_| {
        fs::rename(&a, &a_orig).unwrap();
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&a)
            .arg(&otra)
            .output()
            .unwrap();
        assert!(out.status.success());
    });
    let rep = exec(&c, &plan, false, false, None).unwrap();
    sb.register_junction("Docs/a");
    let status = rep.items[0].status;
    eprintln!("ancestro cambiado tras soltar el handle: {status:?}");
    assert_ne!(status, ItemStatus::Recycled, "nunca se da por bueno");
    assert!(
        rep.items[0].removed.is_empty(),
        "el árbol no se actualiza con lo que no es lo aprobado"
    );
    assert!(
        sb.path("Docs/a-original/b/aprobado.txt").exists(),
        "el original aprobado no se toca"
    );
    // Si la shell se llevó el destino del junction, está en la Papelera: es recuperable y el informe lo dice.
    if !sb.path("Docs/otra/b").exists() {
        assert_eq!(status, ItemStatus::RecycledOther);
        assert_eq!(rep.recycled_other, 1);
    }
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

fn pending(identity: FileIdentity) -> Pending {
    Pending {
        path: PathBuf::from("no-existe-x"),
        identity,
        links: 1,
    }
}

fn ident(n: u128) -> FileIdentity {
    FileIdentity {
        volume_serial: 7,
        file_id: n,
    }
}

/// El juicio por elemento sale de lo que dijo la shell: identidad igual / distinta, eliminado, error, sin aviso.
#[test]
fn el_juicio_distingue_enviado_otro_objeto_eliminado_error_y_sin_aviso() {
    use crate::recycle::{BinState, DeleteEvent};
    let sb = Sandbox::new();
    let bin_file = sb.write("bin/R1.txt", 10);
    let id = {
        let h = sys::open_for_query(&bin_file).unwrap();
        sys::meta(&h).unwrap().identity
    };
    let ev = |hr: i32, state: BinState| DeleteEvent {
        item: OsString::from("no-existe-x"),
        hr,
        state,
    };
    // Misma identidad -> enviado.
    let p = pending(id);
    let e = ev(0, BinState::InBin(bin_file.clone()));
    assert_eq!(judge(&p, Some(&e)).0, ItemStatus::Recycled);
    // Otra identidad -> se movió a la Papelera un objeto distinto del aprobado.
    let p2 = pending(ident(id.file_id ^ 1));
    assert_eq!(judge(&p2, Some(&e)).0, ItemStatus::RecycledOther);
    let p3 = pending(FileIdentity {
        volume_serial: id.volume_serial ^ 1,
        file_id: id.file_id,
    });
    assert_eq!(judge(&p3, Some(&e)).0, ItemStatus::RecycledOther);
    // La shell lo eliminó definitivamente (`psiNewlyCreated` nulo).
    assert_eq!(
        judge(&p, Some(&ev(0, BinState::Deleted))).0,
        ItemStatus::DeletedUnexpectedly
    );
    // No dio ruta, o la ruta no se puede abrir -> no verificado.
    assert_eq!(
        judge(&p, Some(&ev(0, BinState::InBinUnknownPath))).0,
        ItemStatus::Unverified
    );
    let gone = ev(0, BinState::InBin(sb.path("bin/no-esta.txt")));
    assert_eq!(judge(&p, Some(&gone)).0, ItemStatus::Unverified);
    // Errores: en uso, sin permiso y otros, con su HRESULT.
    let hr_win32 = |c: u32| (0x8007_0000u32 | c) as i32;
    assert_eq!(
        judge(&p, Some(&ev(hr_win32(32), BinState::Deleted))).0,
        ItemStatus::NotRecycled(RecycleFail::InUse)
    );
    assert_eq!(
        judge(&p, Some(&ev(hr_win32(5), BinState::Deleted))).0,
        ItemStatus::NotRecycled(RecycleFail::Denied)
    );
    assert_eq!(
        judge(&p, Some(&ev(0x8000_4005u32 as i32, BinState::Deleted))).0,
        ItemStatus::NotRecycled(RecycleFail::Shell(0x8000_4005u32 as i32))
    );
    // Sin aviso: sigue en su sitio -> no procesado; ya no está -> no verificado.
    let here = Pending {
        path: bin_file.clone(),
        identity: id,
        links: 1,
    };
    assert_eq!(
        judge(&here, None).0,
        ItemStatus::NotRecycled(RecycleFail::NotProcessed)
    );
    assert_eq!(judge(&p, None).0, ItemStatus::Unverified);
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
