//! End-to-end tests: basket -> approval (identities) -> execution (permanent deletion, Recycle Bin, scoped purge,
//! dry run) over trees in `%TEMP%\tesela-test-<uuid>`.
//!
//! SAFETY: categories are injected with sandbox roots, so the real `%TEMP%`, caches, and Recycle Bin are NEVER touched.
//! Tests that send items to the Recycle Bin use ONLY owned items (filtered by the test UUID), protected by an RAII guard
//! that purges them, and verify that the Recycle Bin ID set is unchanged. NO test purges the real Recycle Bin: the scope
//! of `purge` is limited to owned items by `with_bin_filter`.

use super::*;
use crate::basket::Basket;
use crate::plan::{ApproveOptions, Cleaner, Fit, RejectReason, UnfitReason};
use crate::protect::{ProtectionRoots, Verdict};
use crate::recycle::bintest::{BinGuard, bin_usable, ids, is_mine, real_recycle_enabled};
use crate::roots::Roots;
use crate::rules::{CategoryId, Risk};
use crate::testutil::{RemoveOnDrop, Sandbox, bin_lock};
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

/// A test log for `c`: stored in its data folder (inside the sandbox), or in a test-owned folder that `exec` deletes when
/// finished if no data folder is configured.
fn log_for(c: &Cleaner) -> (Arc<LogWriter>, Option<PathBuf>) {
    match c.data_dir.clone() {
        Some(d) => (Arc::new(LogWriter::create(&d, false).unwrap()), None),
        None => {
            let d = std::env::temp_dir().join(format!("tesela-test-log-{}", uuid::Uuid::new_v4()));
            (Arc::new(LogWriter::create(&d, false).unwrap()), Some(d))
        }
    }
}

/// Executes the plan. Without an explicit log, a REAL execution uses a test log (M4: cleaning requires a log; a dry run
/// may proceed without one).
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
            allow_local_mail_permanent: false,
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
    // Temporary files: only those older than 7 days (the test dates are 10 days old).
    sb.write_aged("Temp/viejo.tmp", 30_000, 240);
    sb.write("Temp/nuevo.tmp", 30_000);
    // Chrome cache: no age limit. The profile (history, cookies, etc.) is NOT cache.
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
    // The estimate counts only what will be touched (not recent temporary files).
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

/// E2: temporary files are cleaned after 7 days (matching `cleanmgr`: "not modified in the last week"), not after 24
/// hours: a 6-day-old file is kept and counted as recent, while an 8-day-old file is deleted.
#[test]
fn los_temporales_de_menos_de_7_dias_se_dejan() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write_aged("Temp/de-6-dias.tmp", 10_000, 6 * 24);
    sb.write_aged("Temp/de-8-dias.tmp", 10_000, 8 * 24);
    sb.write_aged("Temp/de-1-dia.tmp", 10_000, 25);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    assert_eq!(plan.items[0].estimate.files, 1, "solo el de 8 días");
    assert_eq!(plan.items[0].estimate.young, 2);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert!(!sb.path("Temp/de-8-dias.tmp").exists());
    assert!(sb.path("Temp/de-6-dias.tmp").exists() && sb.path("Temp/de-1-dia.tmp").exists());
    assert_eq!(rep.totals.young, 2);
}

#[test]
fn dry_run_no_toca_nada_y_registra_las_identidades_aprobadas() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write_aged("Temp/a.tmp", 10_000, 240);
    sb.write_aged("Temp/sub/b.tmp", 10_000, 240);
    sb.write("Docs/mio.doc", 5_000);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    b.add_path(sb.long("Docs/mio.doc"), c.protection());
    let plan = approve(&c, &b);
    let logdir = sb.path("logs");
    let log = Arc::new(LogWriter::create(&logdir, true).unwrap());
    let before = (sb.count_entries("Temp"), sb.count_entries("Docs"));
    // Only the "not even the Recycle Bin" case depends on the real Recycle Bin and runs behind
    // TESELA_REAL_RECYCLE_TESTS=1. The rest of this test always runs.
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
    // They are in the Recycle Bin under their ORIGINAL NAMES (no tombstones or strange names for users to see or restore).
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

/// M3a (with the REAL shell, only under `TESELA_REAL_RECYCLE_TESTS=1`): a THREE-level folder is sent to the Recycle Bin and
/// becomes `Recycled`. This checks that change monitoring does not produce a false positive for the real shell's work
/// between operation start and `PreDeleteItem`. Owned items only (`tesela-test-<uuid>`).
#[test]
fn con_la_shell_real_una_carpeta_de_tres_niveles_se_envia_y_queda_recycled() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    sb.write("Docs/tres/a.txt", 2_000);
    sb.write("Docs/tres/n1/b.txt", 2_000);
    sb.write("Docs/tres/n1/n2/c.txt", 2_000);
    sb.write("Docs/tres/n1/n2/n3/d.txt", 2_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/tres"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Recycled,
        "{:?}",
        rep.items[0]
    );
    assert!(!sb.path("Docs/tres").exists());
    assert_eq!(
        (
            rep.deleted_unexpectedly,
            rep.not_recycled,
            rep.recycled_other,
            rep.unverified
        ),
        (0, 0, 0, 0)
    );
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(mine.len(), 1, "{mine:?}");
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before, "la Papelera queda exactamente como estaba");
}

/// M3b (with the REAL shell, gated): a folder that changes RIGHT BEFORE `PreDeleteItem` (test hook in the receiver) is
/// is vetoed as "changed since confirmation." The send stops, and the folder remains in place without reaching the Recycle Bin.
#[test]
fn con_la_shell_real_un_cambio_justo_antes_de_pre_delete_item_veta_la_carpeta() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    sb.write("Docs/d/f.bin", 5_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/d"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    let _hook = crate::engine::test_hooks::install_before_pre_delete(sb.long("Docs"), |p| {
        if p.ends_with("d") {
            fs::write(p.join("nuevo.bin"), b"x").unwrap();
        }
    });
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Changed),
        "{:?}",
        rep.items[0]
    );
    assert!(rep.recycle_stopped);
    assert_eq!(rep.stop_reason, Some(StopReason::Changed));
    assert!(sb.path("Docs/d/f.bin").exists() && sb.path("Docs/d/nuevo.bin").exists());
    let mine = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .count();
    assert_eq!(mine, 0, "no llegó a la Papelera");
    assert_eq!(ids(), before);
}

/// M3c (with the REAL shell, gated): an OWN `$R` containing a junction (to another sandbox-owned folder under `%TEMP%`,
/// never toward a root) is EXCLUDED ("with links"), and the rest of the batch is purged: the junction target is untouched.
/// Remove the junction as a link before cleanup.
#[test]
fn con_la_shell_real_un_r_propio_con_un_junction_dentro_se_excluye_y_el_resto_se_purga() {
    struct RemoveLink(PathBuf);
    impl Drop for RemoveLink {
        fn drop(&mut self) {
            // Only the link (`remove_dir` on a junction does not traverse it).
            let _ = fs::remove_dir(&self.0);
        }
    }
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    sb.write("Docs/uno.txt", 2_000);
    sb.write("Docs/dos.txt", 2_000);
    sb.write("Docs/carpeta/x.txt", 2_000);
    sb.write("fuera/secreto.txt", 10);
    let mut b = Basket::new();
    for n in ["uno.txt", "dos.txt", "carpeta"] {
        b.add_path(sb.long(&format!("Docs/{n}")), c.protection());
    }
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 3, "{:?}", plan.rejected);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert!(
        rep.items.iter().all(|i| i.status == ItemStatus::Recycled),
        "{:?}",
        rep.items
    );
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(mine.len(), 3, "{mine:?}");
    let carpeta = mine
        .iter()
        .find(|i| i.name == "carpeta")
        .expect("la carpeta propia")
        .clone();
    // A junction INSIDE the folder's `$R`, pointing to another sandbox-owned folder.
    let link = PathBuf::from(&carpeta.id).join("enlace");
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(sb.long("fuera"))
        .output()
        .expect("lanza cmd");
    assert!(out.status.success(), "mklink /J falló");
    let _link_guard = RemoveLink(link.clone());
    // Purge (owned items only): exclude the folder containing the junction and purge the rest.
    let mut empty = Basket::new();
    empty.set_empty_bin(true);
    let plan = approve(&c, &empty);
    assert_eq!(plan.bin.as_ref().map(|b| b.items.len()), Some(3));
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let bin = rep.bin.expect("informe de la Papelera");
    assert_eq!(bin.with_links, 1, "{bin:?}");
    assert_eq!(bin.purged, 2, "{bin:?}");
    assert!(!bin.failed, "{bin:?}");
    assert!(
        sb.path("fuera/secreto.txt").exists(),
        "el destino del junction no se toca"
    );
    assert!(
        PathBuf::from(&carpeta.id).exists(),
        "el excluido sigue en la Papelera"
    );
    // Cleanup: remove the link as a link, then purge what remains.
    fs::remove_dir(&link).unwrap();
    assert!(fs::symlink_metadata(&link).is_err());
    let rest: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(rest.len(), 1, "{rest:?}");
    recycle::purge_items(&rest, &AtomicBool::new(false)).unwrap();
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
    // Two owned items in the Recycle Bin...
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
    // ...confirm "Purge" (capture the list: only those two items, due to the test filter)...
    let mut v = Basket::new();
    v.set_empty_bin(true);
    let plan = c.approve(
        &v,
        &ApproveOptions {
            progress: None,
            bin_bytes_hint: 4_000,
        },
        &cancel(),
    );
    assert_eq!(plan.bin.as_ref().unwrap().items.len(), 2);
    assert!(plan.items.is_empty());
    // The displayed size is for the enumerated list (with the test filter, owned items only), not the entire Recycle Bin
    // or the value suggested by the caller.
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
    // ...then another owned item enters the Recycle Bin, which is NOT in the captured list.
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
    // M4: each purged item gets its own log entry after the chunk intent; the later item does not.
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
    // Unowned items remain exactly as they were.
    let foreign: HashSet<OsString> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| !is_mine(i, &sb.uuid()))
        .map(|i| i.id)
        .collect();
    assert_eq!(foreign, before);
}

/// M3 + M4 with the real Recycle Bin (owned items only): if an item's identity captured at confirmation no longer
/// does not match (forged in the plan here), it is SKIPPED during purge: the shell never sees it, it is reported and logged
/// as changed, and the remaining items are purged.
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
    // Forge the first `$R` identity: it is no longer the approved item.
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

/// A-M4 with the real Recycle Bin (owned items only): if the first item's `$R` is replaced just before the shell
/// deletes it (the window between verification and deletion), the receiver vetoes it. The report marks it "changed since
/// confirmation" (nothing else is purged: the shell stops processing the batch after a veto), the log contains one
/// `vetoed` line and one `not-processed` line for each remaining item, and the replacement remains in place.
#[test]
fn vaciar_con_un_r_sustituido_al_final_consta_como_vetado_y_no_se_purga_nada_mas() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    for n in ["uno.txt", "dos.txt", "tres.txt"] {
        sb.write(&format!("Docs/{n}"), 2_000);
    }
    let mut b = Basket::new();
    for n in ["uno.txt", "dos.txt", "tres.txt"] {
        b.add_path(sb.long(&format!("Docs/{n}")), c.protection());
    }
    exec(&c, &approve(&c, &b), false, false, None).unwrap();
    let mut v = Basket::new();
    v.set_empty_bin(true);
    let plan = c.approve(&v, &ApproveOptions::default(), &cancel());
    let items = plan.bin.as_ref().unwrap().items.clone();
    assert_eq!(items.len(), 3);
    // The first item in the shell's processing order.
    let victim = PathBuf::from(&items[0].id);
    let _apartado = RemoveOnDrop(victim.with_extension("apartado"));
    let hook = crate::engine::test_hooks::install_before_shell(victim.clone(), {
        let victim = victim.clone();
        move |_| {
            fs::rename(&victim, victim.with_extension("apartado")).unwrap();
            fs::write(&victim, b"sustituto").unwrap();
        }
    });
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    let rep = exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    let bin = rep.bin.unwrap();
    eprintln!("vaciar con veto: {bin:?}");
    assert_eq!((bin.requested, bin.purged, bin.still_there), (3, 0, 3));
    assert_eq!(bin.changed, 1, "el vetado cuenta como cambiado");
    assert_eq!((bin.replaced, bin.unverifiable), (0, 0));
    assert!(!bin.failed, "el E_ABORT propio no es un fallo de la shell");
    assert_eq!(fs::read(&victim).unwrap(), b"sustituto");
    let text = fs::read_to_string(log.path()).unwrap();
    let outs: Vec<String> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["t"] == "bin-item")
        .map(|v| v["out"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(outs.iter().filter(|o| *o == "vetoed").count(), 1, "{text}");
    assert_eq!(
        outs.iter().filter(|o| *o == "not-processed").count(),
        2,
        "{text}"
    );
    assert!(!outs.iter().any(|o| o == "purged"), "{text}");
    drop(hook);
    let left: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    recycle::purge_items(&left, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

/// M-2 with the real Recycle Bin (owned items only): veto a replaced Recycle Bin FOLDER (the shell queries
/// the folder before its children) is recorded as "changed since confirmation," and the log contains a `vetoed` entry for THAT
/// the item), not `error-0x...`/`not-processed`. (The COM-free test `el_veto_de_un_hijo_se_atribuye_al_r_de_su_carpeta`
/// verifies that a child veto is attributed to its folder; with the real shell, the change cannot be forced between the
/// folder query and its children.)
#[test]
fn vaciar_con_un_hijo_vetado_de_una_carpeta_consta_como_cambiado_y_vetado() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    sb.write("Docs/carpeta/sub/hijo.txt", 2_000);
    sb.write("Docs/otro.txt", 2_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/carpeta"), c.protection());
    b.add_path(sb.long("Docs/otro.txt"), c.protection());
    exec(&c, &approve(&c, &b), false, false, None).unwrap();
    let mut v = Basket::new();
    v.set_empty_bin(true);
    let plan = c.approve(&v, &ApproveOptions::default(), &cancel());
    let items = plan.bin.as_ref().unwrap().items.clone();
    assert_eq!(items.len(), 2);
    let folder = items.iter().find(|i| i.name == "carpeta").unwrap().clone();
    let victim = PathBuf::from(&folder.id);
    let _apartado = RemoveOnDrop(victim.with_extension("apartado"));
    let hook = crate::engine::test_hooks::install_before_shell(victim.clone(), {
        let victim = victim.clone();
        move |_| {
            // With the guard open (sharing `DELETE`), move the folder aside and put another folder with its name and a child in its place.
            fs::rename(&victim, victim.with_extension("apartado")).unwrap();
            fs::create_dir_all(victim.join("sub")).unwrap();
            fs::write(victim.join("sub/hijo.txt"), b"sustituto").unwrap();
        }
    });
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    let rep = exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    let bin = rep.bin.unwrap();
    eprintln!("vaciar con un hijo vetado: {bin:?}");
    assert_eq!(bin.changed, 1, "el elemento carpeta cuenta como cambiado");
    assert!(!bin.failed);
    let text = fs::read_to_string(log.path()).unwrap();
    let vetoed: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["t"] == "bin-item" && v["out"] == "vetoed")
        .collect();
    assert_eq!(vetoed.len(), 1, "{text}");
    assert_eq!(
        vetoed[0]["id"].as_str().unwrap(),
        folder.id.to_string_lossy()
    );
    assert!(
        !text.contains("error-0x"),
        "el vetado no consta como error de la shell: {text}"
    );
    drop(hook);
    let left: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    recycle::purge_items(&left, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

/// M4 with the real Recycle Bin (one owned item only): if the chunk's `bin-begin` intent cannot be written, the shell is NOT
/// sees that chunk: nothing is purged (`purged = 0`), the item remains in the Recycle Bin, and the report says `log_failed`.
#[test]
fn un_error_de_escritura_en_bin_begin_impide_que_la_shell_vea_el_trozo() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    sb.write("Docs/uno.txt", 2_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/uno.txt"), c.protection());
    exec(&c, &approve(&c, &b), false, false, None).unwrap();
    let mut v = Basket::new();
    v.set_empty_bin(true);
    let plan = c.approve(&v, &ApproveOptions::default(), &cancel());
    assert_eq!(plan.bin.as_ref().unwrap().items.len(), 1);
    // The budget covers the startup line but not the `bin-begin` intent.
    let log = Arc::new(LogWriter::create_failing_after(&sb.path("L"), false, 80).unwrap());
    let rep = exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    let bin = rep.bin.unwrap();
    assert_eq!((bin.requested, bin.purged, bin.still_there), (1, 0, 1));
    assert!(rep.log_failed, "{rep:?}");
    assert!(!log.ok());
    let left: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(left.len(), 1, "el elemento sigue en la Papelera: {left:?}");
    assert!(PathBuf::from(&left[0].id).exists());
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
    // Capture the purge list (the old item), then move a new item into the SAME bin.
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
    // The new item (moved AFTER the purge) remains recoverable in the Recycle Bin.
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].name.to_string_lossy(), "nuevo.txt");
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

#[test]
fn lo_que_no_cabe_en_la_papelera_exige_la_casilla() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    // A path too long for the Recycle Bin (more than 258 characters).
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
    // Without the checkbox, it remains untouched.
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(rep.skipped_unfit, 1);
    assert_eq!(rep.items[0].status, ItemStatus::SkippedUnfit);
    assert!(
        file.exists(),
        "nunca se convierte en definitivo en silencio"
    );
    // With the checkbox enabled, permanently delete it (without using the Recycle Bin).
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
    // Approval said "does not fit," but it fits now (or vice versa): confirmation is required again.
    plan.items[0].fit = Fit::Unfit(UnfitReason::TooBig);
    let r = exec(&c, &plan, false, true, None);
    assert_eq!(
        r.unwrap_err(),
        ExecError::NeedsReconfirm(vec![plan.items[0].id])
    );
    assert!(sb.path("Docs/a.txt").exists());
}

// ---- Operation races at the plan level ------------------------------------------------------------------------

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
    // After confirmation: delete the file and create another with the same name and size.
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
    // One file opened exclusively, and another open inside the folder.
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
    // Nothing was renamed: the original names remain.
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

// ---- Protection, hard links, and rejections ----------------------------------------------------------------------------

#[test]
fn una_carpeta_con_una_carpeta_conocida_dentro_se_expande_y_esa_se_respeta() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    // `Docs/Mezcla/Protegida` is a known root ("Contents of" only); everything else can be deleted.
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
    // add_path already rejects denied/untouchable paths; force entries here that only approval can reject.
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
    // A folder that became protected between being added and approval.
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
    sb.write_aged("Temp/a.tmp", 10_000, 240);
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

// ---- Local volumes only (B2) and the application itself under the temp directory (B3) ---------------------------------------

/// The same sandbox folder accessed through a network path to the administrative share.
fn via_share(host: &str, p: &Path) -> PathBuf {
    let s = p.to_string_lossy().into_owned();
    let letter = s.chars().next().unwrap();
    let rest: String = s.chars().skip(3).collect();
    let sep = '\\';
    PathBuf::from(format!("{sep}{sep}{host}{sep}{letter}${sep}{rest}"))
}

/// B2: a network path to a local disk's administrative share (using `localhost` or `127.0.0.1` as the host) bypassed
/// protection (which is defined over drive paths). Reject these paths during approval, even if the network path is
/// accessible; execution also rejects them if someone inserts them into a plan.
#[test]
fn las_rutas_de_red_a_un_disco_local_no_esquivan_la_proteccion() {
    let _lock = bin_lock();
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    let f = sb.write("Docs/privado.txt", 2_000);
    sb.write_aged("Temp/viejo.tmp", 3_000, 240);
    for host in ["localhost", "127.0.0.1"] {
        // Rejected with its reason when ADDED to the basket (not only during approval).
        let net = via_share(host, &sb.long("Docs/privado.txt"));
        let mut b = Basket::new();
        assert_eq!(
            b.add_path(net.clone(), c.protection()),
            crate::basket::AddOutcome::NotLocal
        );
        assert!(b.is_empty());
        assert!(approve(&c, &b).items.is_empty());
        // During execution: take a valid plan and replace its path with the network path (same identity).
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
        // A network path never "fits" in the Recycle Bin: without the checkbox, it is skipped...
        let rep = exec(&c, &plan2, false, false, None).unwrap();
        assert_eq!(rep.items[0].status, ItemStatus::SkippedUnfit);
        // ...and with the checkbox enabled (permanent deletion via handle traversal), reject the root because it is not local.
        let rep = exec(&c, &plan2, false, true, None).unwrap();
        eprintln!("{host}: estado en la ejecución = {:?}", rep.items[0].status);
        assert_eq!(rep.items[0].status, ItemStatus::Root(RootStatus::NotLocal));
        assert!(f.exists(), "{host}: no se tocó nada");
        // A category root through the network path (permanent deletion of its contents).
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

/// B3: the application's own folder UNDER the temp directory (an exempt root) is never deleted, nor are its contents.
/// The exemption lifts protection above the exempt root, not a denial inside it.
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
    sb.write_aged("Temp/app/tesela.exe", 5_000, 240);
    sb.write_aged("Temp/app/datos/config.toml", 1_000, 240);
    sb.write_aged("Temp/viejo.tmp", 3_000, 240);
    sb.write_aged("Temp/otra/x.tmp", 3_000, 240);
    sb.set_age(&sb.path("Temp/otra"), 240);
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
    // It also cannot be added or approved as a standalone destination.
    let mut b2 = Basket::new();
    b2.add_path(sb.long("Temp/app"), c.protection());
    assert!(approve(&c, &b2).items.is_empty());
}

/// Test roots with a "protected" folder (`Prot`), WITHOUT canonicalizing `Local`/`Temp` (a junction in their ancestors
/// (the chain is checked exactly as written).
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

/// B1 (v0.1.1): a junction ANCESTOR of a category root (`User Data` -> a protected folder containing a `ShaderCache`)
/// does not turn protected content into deletable cache: the root is discarded (layer 1), and even if it were accepted,
/// the exemption would lift only its declared zone (layer 2). Protected content remains untouched in dry runs and execution.
#[test]
fn un_junction_por_encima_de_user_data_no_vuelve_borrable_lo_protegido() {
    let mut sb = Sandbox::new();
    sb.write_aged("Prot/UD/ShaderCache/s.bin", 4_000, 240);
    sb.write_aged("Prot/UD/GrShaderCache/g.bin", 3_000, 240);
    sb.write_aged("Prot/UD/Default/Cache/c.bin", 2_000, 240);
    sb.write_aged("Prot/UD/Cookies", 100, 240);
    sb.mkdir("Local/Google/Chrome");
    sb.junction("Local/Google/Chrome/User Data", "Prot/UD");
    let roots = roots_with_raw(&sb, sb.path("Local"), None);
    let c = Cleaner::new(roots);
    // The scanner sees caches through the link, but none pass the path check.
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
    // Protected content remains denied through its real path.
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
    // Approve and execute (dry run and real): there are no items, and everything remains in place.
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

/// H4: a root discarded because its path contains a link does not disappear silently: `unavailable_roots` gives the reason
/// (shown as "unavailable" in the UI), roots discarded at startup are also returned, and the execution log records each
/// rejection with its reason.
#[test]
fn una_raiz_descartada_deja_su_motivo_visible_y_en_el_registro() {
    let mut sb = Sandbox::new();
    sb.write_aged("Prot/UD/ShaderCache/s.bin", 4_000, 240);
    sb.mkdir("Local/Google/Chrome");
    sb.junction("Local/Google/Chrome/User Data", "Prot/UD");
    let mut roots = roots_with_raw(&sb, sb.path("Local"), None);
    // Also a root discarded by `from_system` at startup (e.g. redirected `%TEMP%`).
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
    // Non-security issues (nonexistent, access denied) are not shown as "unavailable."
    assert!(
        !crate::roots::RootRejection::Os(5).is_user_visible()
            && crate::roots::RootRejection::NotCanonical.is_user_visible()
    );
    // Approval rejects it, and the log records it.
    let mut b = Basket::new();
    b.set_category(CategoryId::ChromeCache, true);
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    assert!(plan.items.is_empty());
    // Chrome: a link in its path; redirected `%TEMP%`: its own reason (R5), not "a link."
    assert!(
        plan.rejected
            .iter()
            .filter(|r| r.reason != RejectReason::OutsideLocalAppData)
            .all(|r| r.reason == RejectReason::ReparseAncestor)
    );
    assert_eq!(
        plan.rejected
            .iter()
            .filter(|r| r.reason == RejectReason::OutsideLocalAppData)
            .count(),
        1,
        "{:?}",
        plan.rejected
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

/// B1 (layer 2, isolated): even if a "canonical" root reaches `Prot\UD\ShaderCache` by any path (layer 1 is skipped here;
/// the resolved path is injected), Chrome's exemption declares ITS zone (`...\User Data`), not `Prot`.
#[test]
fn la_exencion_de_una_cache_no_levanta_una_denegacion_ajena() {
    let sb = Sandbox::new();
    sb.write_aged("Prot/UD/ShaderCache/s.bin", 4_000, 240);
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

/// B1 (v0.1.1), same gap for `%TEMP%`: an ancestor of the temp root is a junction to a protected folder. The written path
/// is discarded (layer 1); with the resolved path (as returned by `canon`), `%TEMP%`'s exemption declares no zone and the
/// `Prot` denial still applies (layer 2).
#[test]
fn un_junction_por_encima_de_temp_no_vuelve_borrable_lo_protegido() {
    let mut sb = Sandbox::new();
    sb.write_aged("Prot/T/Temp/x.tmp", 3_000, 240);
    sb.write_aged("Prot/T/Temp/sub/y.tmp", 3_000, 240);
    sb.junction("Enlace", "Prot/T");
    // (1) The path returned by `GetTempPath`: it passes through the junction.
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
    // (2) Already resolved: the path is clean, but the root falls under denied `Prot`, with no exemption.
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

/// H1: approval opens the root for listing AFTER checking the path. If an ancestor becomes a junction to another
/// (non-denied) user folder during that window, a different object would be opened and the captured identity would be
/// wrong. Compare it with the identity observed during traversal and the resolved path; reject it.
#[test]
fn un_ancestro_cambiado_entre_la_cadena_y_la_apertura_se_rechaza() {
    let mut sb = Sandbox::new();
    sb.write_aged("Anc/Temp/bueno.tmp", 1_000, 240);
    sb.write_aged("Otra/Temp/ajeno.tmp", 1_000, 240);
    let roots = roots_with_raw(&sb, sb.long("Local"), Some(sb.path("Anc/Temp")));
    let c = Cleaner::new(roots);
    // Right before approval opens the root: `Anc` becomes a junction to `Other`.
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

/// H1 (direct): same identity and same resolved path, or it is not what the path check observed.
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
    // The volume counts too.
    let otro_vol = FileIdentity {
        volume_serial: 8,
        file_id: 1,
    };
    assert!(!crate::plan::chain_matches(id(1), root, otro_vol, &key));
}

/// B1: benign case: real roots without reparse points still work and carry their declared zone.
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

/// WER error reports and crash dumps (v0.1.5): "Review," send to the Recycle Bin by item, and only for items older than 7
/// days. The entire `WER` tree is denied; only `ReportArchive` and `ReportQueue` exempt their zones. Nothing else (`ERC`,
/// `Temp`, configuration) is approved. A report folder containing a file from today is kept whole, a link inside a root
/// is rejected, and a link in the path discards the root. Uses a fake shell and synthetic sandbox folders throughout.
#[test]
fn informes_de_wer_y_volcados_van_a_la_papelera_por_elementos_y_solo_los_de_mas_de_7_dias() {
    use crate::paths::PathKey;
    let mut sb = Sandbox::new();
    let wer = "Local/Microsoft/Windows/WER";
    // Reports: one old and intact, one containing a file from today, one recent, and one old in the queue.
    sb.write_aged(
        &format!("{wer}/ReportArchive/Informe_viejo/Report.wer"),
        3_000,
        240,
    );
    sb.write_aged(
        &format!("{wer}/ReportArchive/Informe_viejo/mem.hdmp"),
        8_000,
        240,
    );
    sb.write_aged(
        &format!("{wer}/ReportArchive/Informe_mixto/Report.wer"),
        3_000,
        240,
    );
    sb.write(
        &format!("{wer}/ReportArchive/Informe_mixto/mem.hdmp"),
        2_000,
    );
    sb.write(
        &format!("{wer}/ReportArchive/Informe_reciente/Report.wer"),
        3_000,
    );
    sb.write_aged(
        &format!("{wer}/ReportQueue/Cola_vieja/Report.wer"),
        2_000,
        240,
    );
    // Anything that is NOT a report is never touched.
    sb.write_aged(&format!("{wer}/ERC/ajustes.dat"), 500, 240);
    sb.write_aged(&format!("{wer}/Temp/t.tmp"), 500, 240);
    sb.write_aged(&format!("{wer}/config.ini"), 100, 240);
    // Dumps: one old and one from today.
    sb.write_aged("Local/CrashDumps/app.1111.dmp", 6_000, 240);
    sb.write("Local/CrashDumps/app.2222.dmp", 6_000);
    // A link inside the root pointing to an unrelated folder.
    sb.write_aged("Prot/ajeno/secreto.dat", 4_000, 240);
    sb.junction(&format!("{wer}/ReportArchive/enlace"), "Prot/ajeno");
    // Set folder timestamps last (writing inside changes them).
    for d in [
        "ReportArchive/Informe_viejo",
        "ReportArchive/Informe_mixto",
        "ReportQueue/Cola_vieja",
    ] {
        sb.set_age(&sb.path(&format!("{wer}/{d}")), 240);
    }
    let mut roots = roots_with_raw(&sb, sb.long("Local"), None);
    roots.protection.denied.push(sb.long(wer));
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = Cleaner::new(roots).with_threads(4).with_shell(fake.clone());

    // Roots: exactly the two report folders (with the `WER` zone) and the dump folder (without a zone).
    let wer_checks = c.roots().category_root_checks(CategoryId::WerReports);
    let paths: Vec<PathBuf> = wer_checks
        .iter()
        .map(|k| k.result.as_ref().unwrap().path.clone())
        .collect();
    assert_eq!(
        paths,
        vec![
            sb.long(&format!("{wer}/ReportArchive")),
            sb.long(&format!("{wer}/ReportQueue"))
        ]
    );
    for k in &wer_checks {
        assert_eq!(
            k.result.as_ref().unwrap().zone.as_deref(),
            Some(sb.long(wer).as_path())
        );
    }
    let dumps = c.roots().category_root_checks(CategoryId::CrashDumps);
    assert_eq!(dumps.len(), 1);
    let v = dumps[0].result.as_ref().unwrap();
    assert_eq!(v.path, sb.long("Local/CrashDumps"));
    assert_eq!(v.zone, None);

    // Protection: deny `WER` and everything outside the two folders; allow those folders and their contents.
    let prot = c.protection();
    let verdict = |rel: &str| prot.target_verdict(&PathKey::new(&sb.long(rel)));
    for denied in [
        wer.to_string(),
        format!("{wer}/ERC"),
        format!("{wer}/ERC/ajustes.dat"),
        format!("{wer}/Temp"),
        format!("{wer}/Temp/t.tmp"),
        format!("{wer}/config.ini"),
    ] {
        assert_eq!(verdict(&denied), Verdict::Denied, "{denied}");
    }
    for allowed in [
        format!("{wer}/ReportArchive"),
        format!("{wer}/ReportArchive/Informe_viejo"),
        format!("{wer}/ReportArchive/Informe_viejo/Report.wer"),
        format!("{wer}/ReportQueue/Cola_vieja"),
        "Local/CrashDumps".to_string(),
        "Local/CrashDumps/app.1111.dmp".to_string(),
    ] {
        assert_eq!(verdict(&allowed), Verdict::Allowed, "{allowed}");
    }
    // Denied ancestors cannot themselves be deleted.
    assert_eq!(verdict("Local/Microsoft/Windows"), Verdict::Untouchable);

    // Approve: three standalone items (old report, queued report, and old dump), all "Review" to the Recycle Bin,
    // with its category. Recent items, the folder containing a file from today, and the link are excluded.
    let mut b = Basket::new();
    b.set_category(CategoryId::WerReports, true);
    b.set_category(CategoryId::CrashDumps, true);
    let plan = approve(&c, &b);
    let mut got: Vec<(PathBuf, Option<CategoryId>)> = plan
        .items
        .iter()
        .map(|i| (i.path.clone(), i.category))
        .collect();
    got.sort();
    let mut want = vec![
        (
            sb.long("Local/CrashDumps/app.1111.dmp"),
            Some(CategoryId::CrashDumps),
        ),
        (
            sb.long(&format!("{wer}/ReportArchive/Informe_viejo")),
            Some(CategoryId::WerReports),
        ),
        (
            sb.long(&format!("{wer}/ReportQueue/Cola_vieja")),
            Some(CategoryId::WerReports),
        ),
    ];
    want.sort();
    assert_eq!(got, want, "{:?}", plan.rejected);
    for it in &plan.items {
        assert_eq!(it.risk, Risk::Review);
        assert_eq!(it.destination, Destination::Recycle);
        assert_eq!(it.fit, Fit::Fits);
        assert!(!it.min_age.is_zero());
    }
    // Rejections: the link inside the root and items kept because they are recent (recorded, not silently omitted).
    let young: Vec<String> = plan
        .rejected
        .iter()
        .filter(|r| r.reason == RejectReason::Young)
        .map(|r| r.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(plan.rejected.len(), 4, "{:?}", plan.rejected);
    assert_eq!(
        plan.rejected
            .iter()
            .filter(|r| r.reason == RejectReason::Reparse)
            .count(),
        1,
        "{:?}",
        plan.rejected
    );
    assert_eq!(plan.young_count(), 3, "{young:?}");
    for n in ["Informe_mixto", "Informe_reciente", "app.2222.dmp"] {
        assert!(young.iter().any(|y| y == n), "{n}: {young:?}");
    }

    // Dry run and real execution (using a fake shell): approved items go to the fake Recycle Bin; EVERYTHING else stays put.
    exec(&c, &plan, true, false, None).unwrap();
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(rep.items.len(), 3);
    assert!(
        rep.items.iter().all(|i| i.status == ItemStatus::Recycled),
        "{:?}",
        rep.items.iter().map(|i| &i.status).collect::<Vec<_>>()
    );
    for gone in [
        format!("{wer}/ReportArchive/Informe_viejo"),
        format!("{wer}/ReportQueue/Cola_vieja"),
        "Local/CrashDumps/app.1111.dmp".to_string(),
    ] {
        assert!(!sb.path(&gone).exists(), "{gone}");
    }
    for kept in [
        format!("{wer}/ReportArchive"),
        format!("{wer}/ReportQueue"),
        format!("{wer}/ReportArchive/Informe_mixto/Report.wer"),
        format!("{wer}/ReportArchive/Informe_mixto/mem.hdmp"),
        format!("{wer}/ReportArchive/Informe_reciente/Report.wer"),
        format!("{wer}/ERC/ajustes.dat"),
        format!("{wer}/Temp/t.tmp"),
        format!("{wer}/config.ini"),
        "Local/CrashDumps".to_string(),
        "Local/CrashDumps/app.2222.dmp".to_string(),
        "Prot/ajeno/secreto.dat".to_string(),
    ] {
        assert!(sb.path(&kept).exists(), "{kept}");
    }

    // A link IN a root's path discards it (layer 1): `ReportQueue` -> unrelated folder.
    let mut sb2 = Sandbox::new();
    sb2.mkdir(&format!("{wer}/ReportArchive"));
    sb2.write_aged("Prot/q/ajeno.dat", 1_000, 240);
    sb2.junction(&format!("{wer}/ReportQueue"), "Prot/q");
    let mut roots2 = roots_with_raw(&sb2, sb2.long("Local"), None);
    roots2.protection.denied.push(sb2.long(wer));
    let c2 = Cleaner::new(roots2).with_shell(FakeShell::new(&sb2, Some(1 << 40), 0, "nunca"));
    let checks = c2.roots().category_root_checks(CategoryId::WerReports);
    assert_eq!(checks.len(), 2);
    assert!(checks[0].result.is_ok(), "{:?}", checks[0].result);
    assert!(
        matches!(
            &checks[1].result,
            Err(crate::roots::RootRejection::ReparseInChain { component }) if component == "ReportQueue"
        ),
        "{:?}",
        checks[1].result
    );
    let mut b2 = Basket::new();
    b2.set_category(CategoryId::WerReports, true);
    let plan2 = approve(&c2, &b2);
    assert!(plan2.items.is_empty(), "{:?}", plan2.rejected);
    assert!(sb2.path("Prot/q/ajeno.dat").exists());
}

const WER_REL: &str = "Local/Microsoft/Windows/WER";

/// A `Cleaner` over the sandbox with `WER` denied (as in `from_system`) and the given shell.
fn diag_cleaner(sb: &Sandbox, shell: Arc<dyn RecycleShell>) -> Cleaner {
    let mut roots = roots_with_raw(sb, sb.long("Local"), None);
    roots.protection.denied.push(sb.long(WER_REL));
    Cleaner::new(roots).with_threads(4).with_shell(shell)
}

/// A shell without `MaxCapacity` (like a Recycle Bin without a configured maximum size): nothing "fits" (`CapacityUnknown`).
struct NoCapacityShell(Arc<FakeShell>);

impl RecycleShell for NoCapacityShell {
    fn recycle(&self, items: &[recycle::SendItem]) -> recycle::BatchOutcome {
        self.0.recycle(items)
    }
    fn bin_room(&self, _: &Path) -> BinRoom {
        BinRoom {
            max: None,
            used: Some(0),
        }
    }
    fn fit(&self, _: &Path, _: u64) -> Fit {
        Fit::Unfit(UnfitReason::CapacityUnknown)
    }
    fn is_bin_path(&self, p: &Path) -> bool {
        self.0.is_bin_path(p)
    }
}

fn status_of(rep: &Report, name: &str) -> ItemStatus {
    rep.items
        .iter()
        .find(|i| i.path.ends_with(name))
        .unwrap_or_else(|| panic!("no hay `{name}` en el informe"))
        .status
}

/// M-1: a dump, report folder, or file inside it that is modified AFTER approval (same identity and size, current timestamps)
/// is not sent: the minimum age is checked again through the handle during execution. The untouched control does reach the
/// Recycle Bin.
#[test]
fn lo_que_se_toca_tras_aprobar_deja_de_cumplir_los_7_dias_y_no_se_envia() {
    let sb = Sandbox::new();
    sb.write_aged("Local/CrashDumps/app.1111.dmp", 6_000, 240);
    for d in ["Informe_a", "Informe_b", "Informe_c"] {
        sb.write_aged(
            &format!("{WER_REL}/ReportArchive/{d}/Report.wer"),
            3_000,
            240,
        );
    }
    for d in ["Informe_a", "Informe_b", "Informe_c"] {
        sb.set_age(&sb.path(&format!("{WER_REL}/ReportArchive/{d}")), 240);
    }
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = diag_cleaner(&sb, fake.clone());
    let mut b = Basket::new();
    b.set_category(CategoryId::WerReports, true);
    b.set_category(CategoryId::CrashDumps, true);
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 4, "{:?}", plan.rejected);
    assert!(plan.rejected.is_empty(), "{:?}", plan.rejected);
    // After approval, update the dump and folder A timestamps, and the file inside folder B.
    sb.set_age(&sb.path("Local/CrashDumps/app.1111.dmp"), 0);
    sb.set_age(&sb.path(&format!("{WER_REL}/ReportArchive/Informe_a")), 0);
    sb.set_age(
        &sb.path(&format!("{WER_REL}/ReportArchive/Informe_b/Report.wer")),
        0,
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    for n in ["app.1111.dmp", "Informe_a", "Informe_b"] {
        assert_eq!(
            status_of(&rep, n),
            ItemStatus::Root(crate::engine::RootStatus::Changed),
            "{n}"
        );
    }
    assert_eq!(status_of(&rep, "Informe_c"), ItemStatus::Recycled);
    // S-7: items rejected as recent during execution are logged as "too recent" (the dump and folder A timestamp), not as
    // arbitrary changes; the item inside folder B (the fingerprint) remains a change.
    let log = std::fs::read_to_string(rep.log_path.as_ref().expect("registro")).unwrap();
    let young = log.matches("too-recent").count();
    assert!((2..=3).contains(&young), "{young}: {log}");
    for kept in [
        "Local/CrashDumps/app.1111.dmp".to_string(),
        format!("{WER_REL}/ReportArchive/Informe_a/Report.wer"),
        format!("{WER_REL}/ReportArchive/Informe_b/Report.wer"),
    ] {
        assert!(sb.path(&kept).exists(), "{kept}");
    }
    assert!(
        !sb.path(&format!("{WER_REL}/ReportArchive/Informe_c"))
            .exists()
    );
}

/// Residual case 1: items manually added as "Your file" under a diagnostic root are not permanently deleted either. If
/// they do not fit in the Recycle Bin, they are excluded from the amber group and remain untouched even with the checkbox
/// enabled. An ordinary file still goes to the amber group.
#[test]
fn una_carpeta_de_informe_anadida_a_mano_que_no_cabe_no_va_al_grupo_ambar() {
    let sb = Sandbox::new();
    sb.write_aged(
        &format!("{WER_REL}/ReportArchive/Informe_a/Report.wer"),
        3_000,
        240,
    );
    sb.write_aged("Local/CrashDumps/app.1111.dmp", 3_000, 240);
    sb.write_aged("Docs/mio.txt", 6_000, 240);
    let fake = FakeShell::new(&sb, Some(1_000), 0, "nunca");
    let c = diag_cleaner(&sb, fake.clone());
    let mut b = Basket::new();
    b.add_path(
        sb.long(&format!("{WER_REL}/ReportArchive/Informe_a")),
        c.protection(),
    );
    b.add_path(sb.long("Local/CrashDumps/app.1111.dmp"), c.protection());
    b.add_path(sb.long("Docs/mio.txt"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 3, "{:?}", plan.rejected);
    for it in &plan.items {
        // Manually added: no category and no minimum age; only the path distinguishes them.
        assert_eq!(it.category, None);
        assert!(it.min_age.is_zero());
        assert_eq!(it.fit, Fit::Unfit(UnfitReason::TooBig), "{:?}", it.path);
    }
    assert_eq!(plan.kept_unfit_items().count(), 2);
    let amber: Vec<_> = plan.unfit_items().collect();
    assert_eq!(amber.len(), 1);
    assert!(amber[0].path.ends_with("mio.txt"));
    let rep = exec(&c, &plan, false, true, None).unwrap();
    assert_eq!(status_of(&rep, "Informe_a"), ItemStatus::SkippedUnfit);
    assert_eq!(status_of(&rep, "app.1111.dmp"), ItemStatus::SkippedUnfit);
    assert_eq!(status_of(&rep, "mio.txt"), ItemStatus::Done);
    assert!(
        sb.path(&format!("{WER_REL}/ReportArchive/Informe_a/Report.wer"))
            .exists()
    );
    assert!(sb.path("Local/CrashDumps/app.1111.dmp").exists());
    assert!(!sb.path("Docs/mio.txt").exists());
}

/// M-2: reports and dumps are NEVER permanently deleted. If they do not fit in the Recycle Bin (unknown `MaxCapacity`
/// or size), they are excluded from the amber group (permanent deletion with the checkbox); they remain separate with
/// their reason and stay untouched even when the checkbox is enabled. A user file with the same issue still goes amber.
#[test]
fn lo_que_no_cabe_en_la_papelera_no_se_borra_definitivamente_ni_con_la_casilla() {
    for (shell_kind, reason) in [
        ("tamano", UnfitReason::TooBig),
        ("sin-capacidad", UnfitReason::CapacityUnknown),
    ] {
        let sb = Sandbox::new();
        sb.write_aged("Local/CrashDumps/app.1111.dmp", 6_000, 240);
        sb.write_aged(
            &format!("{WER_REL}/ReportArchive/Informe_a/Report.wer"),
            3_000,
            240,
        );
        sb.set_age(&sb.path(&format!("{WER_REL}/ReportArchive/Informe_a")), 240);
        sb.write_aged("Docs/mio.txt", 6_000, 240);
        let fake = FakeShell::new(&sb, Some(1_000), 0, "nunca");
        let shell: Arc<dyn RecycleShell> = if shell_kind == "tamano" {
            fake.clone()
        } else {
            Arc::new(NoCapacityShell(fake.clone()))
        };
        let c = diag_cleaner(&sb, shell);
        let mut b = Basket::new();
        b.set_category(CategoryId::WerReports, true);
        b.set_category(CategoryId::CrashDumps, true);
        b.add_path(sb.long("Docs/mio.txt"), c.protection());
        let plan = approve(&c, &b);
        assert_eq!(plan.items.len(), 3, "{shell_kind}: {:?}", plan.rejected);
        for it in &plan.items {
            assert_eq!(it.fit, Fit::Unfit(reason), "{shell_kind}: {:?}", it.path);
        }
        // Report and dump: kept separate (untouched); user file: amber group.
        assert_eq!(plan.kept_unfit_items().count(), 2, "{shell_kind}");
        assert!(
            plan.kept_unfit_items()
                .all(|i| i.category.is_some_and(CategoryId::is_diagnostic)),
            "{shell_kind}"
        );
        let amber: Vec<_> = plan.unfit_items().collect();
        assert_eq!(amber.len(), 1, "{shell_kind}");
        assert!(amber[0].path.ends_with("mio.txt"), "{shell_kind}");
        // With the checkbox enabled, the user file is permanently deleted; diagnostic items are kept.
        exec(&c, &plan, true, true, None).unwrap();
        let rep = exec(&c, &plan, false, true, None).unwrap();
        assert_eq!(status_of(&rep, "app.1111.dmp"), ItemStatus::SkippedUnfit);
        assert_eq!(status_of(&rep, "Informe_a"), ItemStatus::SkippedUnfit);
        assert_eq!(status_of(&rep, "mio.txt"), ItemStatus::Done);
        assert_eq!(rep.skipped_unfit, 2, "{shell_kind}");
        assert!(sb.path("Local/CrashDumps/app.1111.dmp").exists());
        assert!(
            sb.path(&format!("{WER_REL}/ReportArchive/Informe_a/Report.wer"))
                .exists()
        );
        assert!(!sb.path("Docs/mio.txt").exists());
    }
}

/// General safeguard: a standalone `Entry` with a minimum age never reaches permanent deletion, even if its destination
/// or category is changed and the checkbox is enabled (fail closed). The permanent-deletion traversal is not all-or-nothing.
#[test]
fn una_unidad_suelta_con_antiguedad_minima_nunca_llega_a_la_rama_definitiva() {
    // 1. Permanent destination (a tampered plan).
    let sb = Sandbox::new();
    sb.write_aged("Local/CrashDumps/app.1111.dmp", 6_000, 240);
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = diag_cleaner(&sb, fake.clone());
    let mut b = Basket::new();
    b.set_category(CategoryId::CrashDumps, true);
    let mut plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    plan.items[0].destination = Destination::Permanent;
    plan.items[0].category = None;
    assert!(plan.items[0].never_permanent());
    let rep = exec(&c, &plan, false, true, None).unwrap();
    assert_eq!(status_of(&rep, "app.1111.dmp"), ItemStatus::SkippedUnfit);
    assert!(sb.path("Local/CrashDumps/app.1111.dmp").exists());
    // 2. It also stays skipped when the Recycle Bin rejects it, its diagnostic category is removed, and the checkbox is enabled.
    let sb = Sandbox::new();
    sb.write_aged("Local/CrashDumps/app.1111.dmp", 6_000, 240);
    let fake = FakeShell::new(&sb, Some(1_000), 0, "nunca");
    let c = diag_cleaner(&sb, fake.clone());
    let mut b = Basket::new();
    b.set_category(CategoryId::CrashDumps, true);
    let mut plan = approve(&c, &b);
    plan.items[0].category = None;
    assert_eq!(plan.items[0].fit, Fit::Unfit(UnfitReason::TooBig));
    assert_eq!(plan.unfit_items().count(), 0);
    assert_eq!(plan.kept_unfit_items().count(), 1);
    let rep = exec(&c, &plan, false, true, None).unwrap();
    assert_eq!(status_of(&rep, "app.1111.dmp"), ItemStatus::SkippedUnfit);
    assert!(sb.path("Local/CrashDumps/app.1111.dmp").exists());
    // 3. Control: with zero minimum age (a "Your file" item), the same entry can reach permanent deletion.
    plan.items[0].min_age = Duration::ZERO;
    // The dump is under `CrashDumps`; clear the path marker too, isolating the minimum-age rule.
    plan.items[0].diagnostic_path = false;
    assert!(!plan.items[0].never_permanent());
    assert_eq!(plan.unfit_items().count(), 1);
    // Execution also enforces the fixed diagnostic path, even if an old plan missed it.
    let rep = exec(&c, &plan, true, true, None).unwrap();
    assert_eq!(status_of(&rep, "app.1111.dmp"), ItemStatus::SkippedUnfit);
}

/// m-2: items kept because they are recent are recorded as rejected (`Young`) in the plan and log. m-3: a nonexistent root
/// is not a rejection and produces no warning. m-1: the plan records more than 200 diagnostic items (and confirmation warns).
#[test]
fn lo_reciente_consta_una_raiz_inexistente_no_avisa_y_muchos_elementos_se_cuentan() {
    // No `ReportQueue` or `CrashDumps`: one old report, one recent report, and one old report containing a file from today.
    let sb = Sandbox::new();
    sb.write_aged(
        &format!("{WER_REL}/ReportArchive/Informe_viejo/Report.wer"),
        3_000,
        240,
    );
    sb.write_aged(
        &format!("{WER_REL}/ReportArchive/Informe_mixto/Report.wer"),
        3_000,
        240,
    );
    sb.write(
        &format!("{WER_REL}/ReportArchive/Informe_mixto/mem.hdmp"),
        2_000,
    );
    sb.write(
        &format!("{WER_REL}/ReportArchive/Informe_reciente/Report.wer"),
        3_000,
    );
    for d in ["Informe_viejo", "Informe_mixto"] {
        sb.set_age(&sb.path(&format!("{WER_REL}/ReportArchive/{d}")), 240);
    }
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = diag_cleaner(&sb, fake.clone());
    let mut b = Basket::new();
    b.set_category(CategoryId::WerReports, true);
    b.set_category(CategoryId::CrashDumps, true);
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    assert!(plan.items[0].path.ends_with("Informe_viejo"));
    // Only the two recent items are recorded; nonexistent roots add nothing.
    assert_eq!(plan.rejected.len(), 2, "{:?}", plan.rejected);
    assert!(
        plan.rejected
            .iter()
            .all(|r| r.reason == RejectReason::Young),
        "{:?}",
        plan.rejected
    );
    assert_eq!(plan.young_count(), 2);
    let mut names: Vec<String> = plan
        .rejected
        .iter()
        .map(|r| r.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["Informe_mixto", "Informe_reciente"]);
    assert_eq!(plan.many_diagnostic_items(), None);
    // In the execution log.
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let log = std::fs::read_to_string(rep.log_path.as_ref().expect("registro")).unwrap();
    assert_eq!(log.matches("\"t\":\"rejected\"").count(), 2, "{log}");
    assert!(
        log.contains("Young (modified less than the minimum age ago)"),
        "{log}"
    );

    // Muchos elementos: 201 volcados viejos.
    let sb = Sandbox::new();
    for i in 0..=200 {
        sb.write_aged(&format!("Local/CrashDumps/app.{i}.dmp"), 100, 240);
    }
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = diag_cleaner(&sb, fake.clone());
    let mut b = Basket::new();
    b.set_category(CategoryId::CrashDumps, true);
    let mut plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 201, "{:?}", plan.rejected);
    assert_eq!(plan.many_diagnostic_items(), Some(201));
    // The threshold is "more than 200."
    plan.items.pop();
    assert_eq!(plan.many_diagnostic_items(), None);
}

// ---- Event receiver: identity (not name), races after releasing the handle, and per-item judgment ----------

/// B1: `.lnk`, `.url`, and known extensions (`.txt`, `.jpg`, etc.): identify the Recycle Bin item by its
/// IDENTITY, not name, so neither the extension nor "hide extensions" can fool it.
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

/// Race (M1): after the handle is RELEASED and before the shell opens the item, the same user replaces it with another
/// object with the same name and size. `PreDeleteItem` compares its IDENTITY with the approved one, detects the mismatch,
/// and returns `E_ABORT`: the replacement is NOT sent (it remains in place), the approved original is untouched, the item
/// is recorded as "changed since confirmation," and the batch stops (later items are not processed). Before M1, the replacement reached the
/// Recycle Bin (`RecycledOther`, recoverable).
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

/// M1 with the FAKE shell (does not touch the real Recycle Bin): same as the previous test, with the identity check
/// receiver check emulated by the fake shell.
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
    // `a` was processed before the veto; `b` (replaced) changed; `c`, later in the batch, was not processed.
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

/// M-A: with the GUARD handle open, renaming the item's parent or grandparent (to place a junction to another folder,
/// even a PROTECTED one) fails with "access denied," and the shell recycles the approved item. If renaming did succeed,
/// the attack would continue (the junction would be created), and the checks below would detect it.
fn ataque_de_ancestro(
    sb: &mut Sandbox,
    victima: &'static str,
    destino: &'static str,
) -> (Report, Vec<Option<i32>>) {
    let c = cleaner_for(sb);
    // `victima` is the fixture path `Docs/a/b/c/aprobado.txt`; its parent is `Docs/a/b/c`, grandparent `Docs/a/b`, and great-grandparent `Docs/a`.
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
            // The parent and grandparent cannot be renamed while the item is open.
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
            // Full attack: replace the ancestor with a junction to `destino` (the destination path).
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
    // Parent, grandparent, and great-grandparent: error 5 (access denied); nothing was moved.
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
    // `Prot` is a PROTECTED (denied) folder containing a file with the same name in the same tree.
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

/// M-A: a hard link. After releasing the check, rename the file and put another name in its place
/// for the SAME object: the shell recycles that name, and identity matches. Previously, this falsely reported `Recycled` (the file
/// (the original remains alive under its other name, so nothing was freed). The guarded object is not in the Recycle Bin.
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

// ---- M4: the log is required for cleaning ------------------------------------------------------------------------

/// M4: without a log, NO destructive action is allowed (permanent deletion, sending to the Recycle Bin, or purging): a real
/// execution without a log, or with a failed log, returns `LogRequired` and touches nothing. A dry run may omit the log.
#[test]
fn sin_registro_no_hay_ninguna_accion_destructiva() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write_aged("Temp/viejo.tmp", 10_000, 240);
    sb.write("Docs/b.txt", 1_000);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    b.add_path(sb.long("Docs/b.txt"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
    // Without a log: nothing.
    let opts = ExecOptions::default();
    assert_eq!(
        c.execute(&plan, &opts, &Progress::new()).err(),
        Some(ExecError::LogRequired)
    );
    assert!(sb.path("Temp/viejo.tmp").exists() && sb.path("Docs/b.txt").exists());
    assert!(fake.names().is_empty());
    // A log that has already failed is also rejected.
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
    // The dry run proceeds without a log.
    let dry = exec(&c, &plan, true, false, None).unwrap();
    assert!(dry.dry_run && !dry.log_failed);
    assert!(sb.path("Temp/viejo.tmp").exists() && sb.path("Docs/b.txt").exists());
}

/// H6.8: execution rejected before touching anything (`NeedsReconfirm`, `LogRequired`) must not leave a log containing only
/// its startup line and occupying a retention slot.
#[test]
fn una_ejecucion_rechazada_no_deja_registro() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let mut plan = plan_de(&sb, &c, &[("a.txt", 100, 100)]);
    // Fit changed since confirmation: approval said "does not fit," but it fits now.
    plan.items[0].fit = Fit::Unfit(UnfitReason::TooBig);
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    let path = log.path().to_path_buf();
    assert!(path.exists());
    let r = exec(&c, &plan, false, false, Some(log));
    assert!(matches!(r, Err(ExecError::NeedsReconfirm(_))), "{r:?}");
    assert!(
        !path.exists(),
        "el registro de una ejecución que no se hizo se borra"
    );
    assert!(sb.path("Docs/a.txt").exists());
    // A previously failed log is also discarded.
    let plan2 = plan_de(&sb, &c, &[("b.txt", 100, 100)]);
    let bad = Arc::new(LogWriter::create_failing_after(&sb.path("Tesela2"), false, 400).unwrap());
    for i in 0..60 {
        bad.write_rejected(
            Path::new("x"),
            &format!("relleno largo para agotar el presupuesto {i}"),
        );
    }
    bad.flush();
    assert!(!bad.ok());
    let bad_path = bad.path().to_path_buf();
    let r2 = exec(&c, &plan2, false, false, Some(bad));
    assert_eq!(r2.err(), Some(ExecError::LogRequired));
    assert!(!bad_path.exists());
}

/// H5: if the log reaches its cap and stops recording, cleaning continues but the report says so (`log_truncated`).
#[test]
fn un_registro_truncado_se_propaga_al_informe() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    for i in 0..60 {
        sb.write_aged(&format!("Temp/t{i:02}.tmp"), 100, 240);
    }
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    log.set_max_bytes_for_test(2_000);
    let rep = exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    assert!(log.truncated());
    assert!(
        rep.log_truncated && !rep.log_failed && !rep.cancelled,
        "{rep:?}"
    );
    assert_eq!(
        sb.count_entries("Temp"),
        0,
        "la limpieza siguió hasta el final"
    );
    // A normal log is not marked.
    let sb2 = Sandbox::new();
    let c2 = cleaner_for(&sb2);
    sb2.write_aged("Temp/a.tmp", 100, 240);
    let plan2 = approve(&c2, &b);
    let rep2 = exec(&c2, &plan2, false, false, None).unwrap();
    assert!(!rep2.log_truncated);
}

/// H6.5: user cancellation and log failure are reported separately; a real cancellation is no longer hidden when the log
/// also fails.
#[test]
fn la_cancelacion_del_usuario_no_se_oculta_si_ademas_falla_el_registro() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write_aged("Temp/a.tmp", 10_000, 240);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    // The user cancels AND the log fails (the intent does not fit).
    let log = Arc::new(LogWriter::create_failing_after(&sb.path("L1"), false, 80).unwrap());
    let progress = Progress::new();
    progress.cancel();
    let rep = c
        .execute(
            &plan,
            &ExecOptions {
                log: Some(log),
                ..ExecOptions::default()
            },
            &progress,
        )
        .unwrap();
    assert!(rep.cancelled, "la cancelación real consta");
    assert!(sb.path("Temp/a.tmp").exists());
    // Only the log fails; this is not a user cancellation.
    let sb2 = Sandbox::new();
    let c2 = cleaner_for(&sb2);
    sb2.write_aged("Temp/a.tmp", 10_000, 240);
    let plan2 = approve(&c2, &b);
    let log2 = Arc::new(LogWriter::create_failing_after(&sb2.path("L2"), false, 80).unwrap());
    let rep2 = exec(&c2, &plan2, false, false, Some(log2)).unwrap();
    assert!(rep2.log_failed && !rep2.cancelled);
}

/// R3: the cancellation flag is no longer public: `cancel()` is the only way to cancel externally and always records that
/// the user requested it.
#[test]
fn cancelar_siempre_anota_que_lo_pidio_el_usuario() {
    let p = Progress::new();
    assert!(!p.is_cancelled() && !p.user_cancelled());
    p.cancel();
    assert!(p.is_cancelled() && p.user_cancelled());
}

/// R4: a plan with many rejected items does not inflate the log (notes are capped, with a final line for the remainder).
#[test]
fn los_rechazados_del_plan_se_anotan_con_tope() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write_aged("Temp/a.tmp", 100, 240);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    let mut plan = approve(&c, &b);
    for i in 0..500 {
        plan.rejected.push(crate::plan::Rejected {
            path: PathBuf::from(format!("C:/falso/{i}")),
            reason: RejectReason::Missing,
        });
    }
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    let text = fs::read_to_string(log.path()).unwrap();
    let n = text.lines().filter(|l| l.contains("\"rejected\"")).count();
    assert_eq!(n, 201, "200 notas y una línea con el resto");
    assert!(text.contains("300 más sin anotar"));
}

/// M4: failure to create a log is an error (the UI executes nothing); here, the data directory is a file.
#[test]
fn un_registro_que_no_se_puede_crear_es_un_error() {
    let sb = Sandbox::new();
    sb.write("Tesela", 10); // un archivo donde debería ir la carpeta
    assert!(LogWriter::create(&sb.path("Tesela"), false).is_err());
    assert!(LogWriter::create(&sb.path("Tesela/sub"), false).is_err());
    // Also test one that fails on the startup line.
    assert!(LogWriter::create_failing_after(&sb.path("L0"), false, 0).is_err());
}

/// M4: record INTENT before the result for each operation: list the items in a batch sent to the shell, and the roots in a
/// permanent-deletion batch, BEFORE any result is recorded.
#[test]
fn el_registro_lleva_la_intencion_de_cada_operacion_antes_de_sus_resultados() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write_aged("Temp/viejo.tmp", 10_000, 240);
    let mut plan = plan_de(&sb, &c, &[("a.txt", 100, 100), ("b.txt", 100, 100)]);
    // Plus one permanent-deletion item (the temporary-files category).
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
    // The permanent-deletion batch intent precedes any result for its entries.
    let first_entry = pos("entry").expect("hay entradas");
    assert!(del_begin < first_entry, "{del_begin} < {first_entry}");
    // The send intent precedes the `recycle` results.
    let first_recycle = pos("recycle").expect("hay resultados de la Papelera");
    assert!(rec_begin < first_recycle);
    // List the batch items.
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

/// M4: a log write error STOPS cleaning at the next safe point: if writing the intent fails,
/// permanent-deletion batch, nothing is deleted; if it fails midway, the rest is not processed. The report indicates this (`log_failed`) and it is not
/// it is not reported as a user cancellation.
#[test]
fn un_error_de_escritura_del_registro_detiene_la_limpieza() {
    // (1) It fails while writing the intent: NOTHING is deleted.
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    for i in 0..30 {
        sb.write_aged(&format!("Temp/f{i:02}.tmp"), 1_000, 240);
    }
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    let plan = approve(&c, &b);
    // The budget fits the startup line (about 50 bytes) but not the intent.
    let log = Arc::new(LogWriter::create_failing_after(&sb.path("L1"), false, 80).unwrap());
    let rep = exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    assert!(rep.log_failed && !rep.cancelled, "{rep:?}");
    assert!(!log.ok());
    assert_eq!(sb.count_entries("Temp"), 30, "no se borró nada");
    assert!(matches!(
        rep.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Cancelled)
    ));

    // (2) It fails midway (the intent fits, but per-item results do not): execution stops at the next safe point, with
    // work not completed.
    let sb2 = Sandbox::new();
    let c2 = cleaner_for(&sb2).with_threads(2);
    for i in 0..400 {
        sb2.write_aged(&format!("Temp/g{i:03}.tmp"), 100, 240);
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

    // (3) Writing the Recycle Bin batch intent fails: the shell sees NOTHING.
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

// ---- m1: the approved link count is EXACT ------------------------------------------------------------------------

/// m1: approve a file with one link, then add a second name (same object, different path) before execution. The identity
/// is unchanged, so this used to pass; now the exact approved link count is enforced and it is rejected as "changed since
/// confirmation," for both Recycle Bin and permanent destinations. Neither name is touched.
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
        // After approval: add a second name for the same object.
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

/// m1: the reverse case (approved with two links, then one removed) is also rejected; with no changes, the two-link item
/// is sent normally.
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
    // With no changes, it is sent.
    let rep_ok = exec(&c, &plan, true, false, None).unwrap();
    assert_eq!(rep_ok.items[0].status, ItemStatus::WouldRecycle);
    // Remove the other name: the link count no longer matches approval.
    fs::remove_file(sb.path("Docs/y.bin")).unwrap();
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert!(sb.path("Docs/x.bin").exists());
    assert!(fake.names().is_empty());
}

// ---- Operation batching and safety cutoff (M-C), using a FAKE shell ----------------------------------------

/// A fake shell: it "recycles" by moving items into a sandbox folder. If an operation contains an item whose name
/// includes `nuke`, the shell decides the operation does not fit: it DELETES the first item (`PostDeleteItem` with null
/// `psiNewlyCreated`), and the event receiver STOPS the batch (M2); remaining items are neither processed nor reported. An
/// item whose name includes `unknown` is recycled, but the shell "does not say where"; this also stops the batch. It records
/// operaciones.
struct FakeShell {
    bin: PathBuf,
    max: Option<u64>,
    used: Mutex<u64>,
    nuke: &'static str,
    ops: Mutex<Vec<Vec<PathBuf>>>,
    /// If `Some`, the shell "fails": no events are reported, and this error is returned (as with a COM or `PerformOperations` failure).
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

/// A3-m1: the fake shell's size for an item is its logical size, or, for a directory, the SUM of all descendants (without
/// following links). It previously used `metadata(directory).len()` (0 on NTFS), so simulated occupancy did not grow with
/// contents and assertions such as "does not exceed 90%" measured nothing.
fn tree_len(p: &Path) -> u64 {
    let Ok(md) = fs::symlink_metadata(p) else {
        return 0;
    };
    if !md.is_dir() {
        return md.len();
    }
    fs::read_dir(p)
        .map(|rd| rd.flatten().map(|e| tree_len(&e.path())).sum())
        .unwrap_or(0)
}

impl RecycleShell for FakeShell {
    fn recycle(&self, items: &[recycle::SendItem]) -> recycle::BatchOutcome {
        use crate::recycle::BinState;
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
        // The REAL event receiver (`SinkCore`): the fake shell calls `PreDeleteItem` and `PostDeleteItem` just like the real
        // shell, so tests exercise the same veto and stop logic (identity, size, failures) rather than a separate copy that
        // could diverge.
        let core = recycle::SinkCore::for_send(items);
        for (i, p) in paths.iter().enumerate() {
            // `PreDeleteItem`: the current path must refer to the approved object (and a file must not have grown); otherwise,
            // veto it and stop the batch.
            if !core.pre_delete(p.clone().into_os_string()) {
                // As measured with the REAL shell: report the vetoed item with `PostDeleteItem(E_ABORT)` and no new item.
                // The executor must not mistake the receiver's own `E_ABORT` for a Windows error.
                let _ = core.post_delete(
                    p.clone().into_os_string(),
                    windows::Win32::Foundation::E_ABORT.0,
                    BinState::Deleted,
                );
                break;
            }
            // A3-m1: a directory occupies the size of ALL its contents in the Recycle Bin, not the size of its directory entry.
            let len = tree_len(p);
            let unknown = p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains("unknown"));
            let intact = p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains("intacto"));
            let mut hr = 0;
            let state = if nuke {
                // The shell uses classic deletion semantics: with the guard handle open, the name remains "pending deletion"
                // until the handle is released (R1). `fs::remove_file` uses POSIX semantics and would not reproduce this.
                sys::delete_legacy_for_test(p).unwrap();
                BinState::Deleted
            } else if p.is_dir()
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains("parcial"))
            {
                // R6: the shell permanently deletes ONE child and reports the folder as "deleted," though it remains in place.
                let child = fs::read_dir(p).unwrap().flatten().next().unwrap().path();
                sys::delete_legacy_for_test(&child).unwrap();
                BinState::Deleted
            } else if p.is_dir()
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains("falla"))
            {
                // A-M1 (case 1): the shell permanently deletes ONE child and THEN fails with access denied: the result is
                // an error, but the folder has already lost an item. On failure, the receiver does not stop the batch (only on success).
                let child = fs::read_dir(p).unwrap().flatten().next().unwrap().path();
                sys::delete_legacy_for_test(&child).unwrap();
                hr = 0x8007_0005u32 as i32;
                BinState::Deleted
            } else if p.is_dir()
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains("sustituye"))
            {
                // A-M1 (case 2): the shell deletes one child and leaves a NEW object with the same name and size: totals are
                // identical, contents differ. It reports successful deletion without sending anything.
                let child = fs::read_dir(p).unwrap().flatten().next().unwrap().path();
                let len = fs::metadata(&child).unwrap().len() as usize;
                sys::delete_legacy_for_test(&child).unwrap();
                // Same name and size, but a DIFFERENT object (different `FileId`).
                fs::write(&child, vec![9u8; len]).unwrap();
                BinState::Deleted
            } else if p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains("deniega"))
            {
                // The shell fails with access denied WITHOUT touching anything (file or folder): report a FAILURE and no
                // new item.
                hr = 0x8007_0005u32 as i32;
                BinState::Deleted
            } else if intact {
                // The shell reports "deleted" (null, success) WITHOUT touching anything (verified with a sparse file).
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
            // The receiver records the event and THEN decides whether to return E_ABORT (`post_delete_aborts`): the shell does not process
            // the rest.
            if core.post_delete(p.clone().into_os_string(), hr, state) {
                break;
            }
        }
        let (events, vetoed, sink_aborted) = core.finish();
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
    /// M2: before delivery, the fake purge checks whether `$R` contains links and excludes it (`linked`) WITHOUT stopping
    /// the batch; the rest is "purged" (its `$R` and `$I` are deleted). This matches the real shell's `purge_items_flags`.
    fn purge(
        &self,
        items: &[recycle::BinItem],
        cancel: &AtomicBool,
        obs: &mut dyn FnMut(recycle::PurgeStep<'_>),
    ) -> Result<recycle::BatchOutcome, recycle::ShellError> {
        let refs: Vec<&recycle::BinItem> = items.iter().collect();
        let (clean, linked) = recycle::partition_linked(&refs);
        let mut out = recycle::BatchOutcome::default();
        if !linked.is_empty() {
            out.linked_bin = linked.iter().map(|i| i.id.clone()).collect();
            obs(recycle::PurgeStep::Linked(&linked));
        }
        if clean.is_empty() || cancel.load(Ordering::Relaxed) {
            return Ok(out);
        }
        obs(recycle::PurgeStep::Begin(&clean));
        let mut done: Vec<(&recycle::BinItem, Option<i32>)> = Vec::new();
        for i in &clean {
            let r = PathBuf::from(&i.id);
            if fs::symlink_metadata(&r).is_ok_and(|m| m.is_dir()) {
                fs::remove_dir_all(&r).unwrap();
            } else {
                fs::remove_file(&r).unwrap();
            }
            let name = r
                .file_name()
                .unwrap()
                .to_string_lossy()
                .replacen("$R", "$I", 1);
            let _ = fs::remove_file(r.with_file_name(name));
            out.events.push(recycle::DeleteEvent {
                item: i.id.clone(),
                hr: 0,
                state: recycle::BinState::Deleted,
            });
            done.push((*i, Some(0)));
        }
        obs(recycle::PurgeStep::Done(&done));
        Ok(out)
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
    /// Fit does not depend on the machine (registry or policies): items fit unless they exceed 90% of simulated capacity.
    fn fit(&self, _path: &Path, bytes: u64) -> Fit {
        match self.max {
            Some(max) if (bytes as u128) * 100 > (max as u128) * 90 => {
                Fit::Unfit(UnfitReason::TooBig)
            }
            _ => Fit::Fits,
        }
    }
}

/// A3-m1: fake Recycle Bin occupancy grows with the CONTENTS it receives (the sum of all descendants), not the directory
/// entry size. Negative control: with the old calculation (`metadata(dir).len()`), a folder containing 95,000 bytes added
/// nothing, and an assertion that it "does not exceed 90,000" passed without measuring the sent contents.
#[test]
fn la_shell_simulada_cuenta_el_contenido_de_los_directorios_en_su_ocupacion() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    sb.write("Docs/pequena/a.bin", 1_000);
    sb.write("Docs/pequena/sub/b.bin", 2_000);
    sb.write("Docs/pequena/sub/hondo/c.bin", 3_000);
    sb.write("Docs/grande/x.bin", 60_000);
    sb.write("Docs/grande/y.bin", 35_000);
    assert_eq!(tree_len(&sb.path("Docs/pequena")), 6_000);
    assert_eq!(tree_len(&sb.path("Docs/grande")), 95_000);
    // The old calculation returned the directory entry size, not its contents.
    assert!(fs::metadata(sb.path("Docs/grande")).unwrap().len() < 95_000);
    let send = |rel: &str| recycle::SendItem::capture(&sb.long(rel));
    fake.recycle(&[send("Docs/pequena")]);
    assert_eq!(*fake.used.lock().unwrap(), 6_000);
    // Occupancy DOES reflect a folder whose contents exceed 90% of capacity (negative control): the assertion
    // the tests' "does not exceed 90%" assertion can now fail.
    fake.recycle(&[send("Docs/grande")]);
    assert_eq!(*fake.used.lock().unwrap(), 101_000);
    assert!(*fake.used.lock().unwrap() > 90_000);
}

/// A sandbox-owned `BinItem`: `$R<token>` (an existing file or folder) and its `$I`, with captured identities.
fn bin_item_at(sb: &Sandbox, token: &str, is_dir: bool) -> recycle::BinItem {
    let dir = sb.long("FakeBin/S");
    let r = dir.join(format!("$R{token}"));
    let i = dir.join(format!("$I{token}"));
    if is_dir {
        fs::create_dir_all(&r).unwrap();
    } else {
        fs::write(&r, b"datos").unwrap();
    }
    fs::write(&i, b"meta").unwrap();
    let id_of = |p: &Path| {
        sys::meta(&sys::open_for_query(p).unwrap())
            .unwrap()
            .identity
    };
    recycle::BinItem {
        id: r.clone().into_os_string(),
        name: OsString::from(format!("{token}.txt")),
        original_parent: sb.long("origen"),
        time_deleted: 0,
        size: 5,
        data_id: Some(id_of(&r)),
        meta_id: Some(id_of(&i)),
        no_identity: false,
    }
}

/// M2 (audit): a Recycle Bin `$R` containing links (junctions or files with reparse points) is EXCLUDED before delivery,
/// with the reason recorded in the report and log. It does NOT stop the batch; remaining items are purged. Previously the
/// receiver vetoed the linked child, and that veto stopped the entire purge (every `purge` stopped there).
#[test]
fn un_r_con_enlaces_dentro_se_excluye_y_el_resto_del_lote_se_purga() {
    let mut sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    sb.mkdir("FakeBin/S");
    sb.write("fuera/secreto.txt", 10);
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let a = bin_item_at(&sb, "AAAA01", false);
    let con_junction = bin_item_at(&sb, "CCCC01", true);
    sb.write("FakeBin/S/$RCCCC01/normal.txt", 5);
    sb.junction("FakeBin/S/$RCCCC01/enlace", "fuera");
    let b = bin_item_at(&sb, "BBBB01", true);
    sb.write("FakeBin/S/$RBBBB01/sub/hondo/x.txt", 5);
    let con_reparse = bin_item_at(&sb, "DDDD01", true);
    let tiene_reparse =
        crate::testutil::make_reparse_file(&sb.path("FakeBin/S/$RDDDD01/nube.bin"), 1_000);
    let f = bin_item_at(&sb, "EEEE01", false);
    let items = vec![
        a.clone(),
        con_junction.clone(),
        b.clone(),
        con_reparse.clone(),
        f.clone(),
    ];
    let plan = Plan {
        bin: Some(crate::plan::BinPlan {
            items,
            bytes_hint: 0,
        }),
        ..Plan::default()
    };
    let (log, tmp) = log_for(&c);
    let rep = c
        .execute(
            &plan,
            &ExecOptions {
                dry_run: false,
                allow_unfit_permanent: false,
                allow_local_mail_permanent: false,
                log: Some(log.clone()),
            },
            &Progress::new(),
        )
        .unwrap();
    let bin = rep.bin.expect("informe de la Papelera");
    let esperados_con_enlaces = if tiene_reparse { 2 } else { 1 };
    assert_eq!(bin.requested, 5);
    assert_eq!(bin.with_links, esperados_con_enlaces, "{bin:?}");
    // Clean items are purged: the file, nested folder, and last item; the batch was NOT stopped.
    let limpios = 5 - esperados_con_enlaces;
    assert_eq!(bin.purged, limpios, "{bin:?}");
    assert_eq!(bin.still_there, esperados_con_enlaces);
    assert!(!bin.failed, "{bin:?}");
    assert!(!PathBuf::from(&a.id).exists());
    assert!(!PathBuf::from(&b.id).exists());
    assert!(
        !PathBuf::from(&f.id).exists(),
        "lo que va DESPUÉS del excluido también se purga"
    );
    // The excluded item remains unchanged, including its junction and target.
    assert!(PathBuf::from(&con_junction.id).join("normal.txt").exists());
    assert!(sb.path("fuera/secreto.txt").exists());
    // Recorded in the log with its reason.
    log.flush();
    let text = fs::read_to_string(log.path()).unwrap();
    assert!(text.contains("\"out\":\"linked\""), "{text}");
    drop(log);
    if let Some(d) = tmp {
        let _ = fs::remove_dir_all(d);
    }
    sb.remove_junction("FakeBin/S/$RCCCC01/enlace");
}

/// m1 (audit): Recycle Bin items that could not be listed (unreadable metadata) are recorded in the log and in the
/// report (`BinReport::unlisted`); listed items are still purged.
#[test]
fn los_elementos_sin_listar_constan_en_el_registro_y_en_el_informe() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    sb.mkdir("FakeBin/S");
    let c = cleaner_for(&sb).with_shell(fake);
    let a = bin_item_at(&sb, "AAAA02", false);
    let plan = Plan {
        bin: Some(crate::plan::BinPlan {
            items: vec![a.clone()],
            bytes_hint: 5,
        }),
        bin_unlisted: 3,
        ..Plan::default()
    };
    let (log, tmp) = log_for(&c);
    let rep = c
        .execute(
            &plan,
            &ExecOptions {
                dry_run: false,
                allow_unfit_permanent: false,
                allow_local_mail_permanent: false,
                log: Some(log.clone()),
            },
            &Progress::new(),
        )
        .unwrap();
    let bin = rep.bin.unwrap();
    assert_eq!(
        (bin.requested, bin.purged, bin.unlisted),
        (1, 1, 3),
        "{bin:?}"
    );
    log.flush();
    let text = fs::read_to_string(log.path()).unwrap();
    assert!(
        text.contains("bin-unlisted") && text.contains("3 elementos"),
        "{text}"
    );
    drop(log);
    if let Some(d) = tmp {
        let _ = fs::remove_dir_all(d);
    }
}

/// A plan containing items under `Docs/<name>` with the given size: the actual file is `len` bytes, and the approved ESTIMATE is
/// `est` (never below its actual size). Since A-B1, batching uses the size measured by the guard handle immediately
/// used before the operation, not the estimate: `est` must match the file's size (no huge files: the
/// thresholds scale with simulated capacity).
fn plan_de(sb: &Sandbox, c: &Cleaner, items: &[(&str, usize, u64)]) -> Plan {
    let mut b = Basket::new();
    for (name, len, _) in items {
        sb.write(&format!("Docs/{name}"), *len);
        b.add_path(sb.long(&format!("Docs/{name}")), c.protection());
    }
    let mut plan = approve(c, &b);
    assert_eq!(plan.items.len(), items.len(), "{:?}", plan.rejected);
    for (it, (_, _, est)) in plan.items.iter_mut().zip(items) {
        // Never below the file's actual size (A-B1: an item that grows beyond approval is not sent).
        it.estimate.bytes = (*est).max(it.estimate.bytes);
        it.estimate.logical = (*est).max(it.estimate.logical);
        let size = it.estimate.bytes.max(it.estimate.logical);
        it.fit = c.fit(&it.path, size);
        assert_eq!(it.fit, Fit::Fits);
    }
    plan
}

#[test]
fn un_elemento_que_no_cabe_entre_otros_pequenos_nunca_arrastra_a_los_demas_y_corta_el_envio() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(10_000), 0, "enorme");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    // `huge` is 5,000 bytes (over 10% of the 10,000 capacity): it goes ALONE. The small items are separate.
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("a.txt", 100, 100),
            ("enorme.bin", 5_000, 5_000),
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
    // STOP: later items were NOT sent (or deleted); they remain in place.
    assert_eq!(st("c.txt"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert_eq!(st("d.txt"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert!(sb.path("Docs/c.txt").exists() && sb.path("Docs/d.txt").exists());
    assert!(rep.recycle_stopped);
    assert_eq!((rep.deleted_unexpectedly, rep.stopped_items), (1, 2));
    // The first item had its own operation (not grouped with the huge one), which also had its own; there were no others.
    assert_eq!(fake.names(), vec![vec!["a.txt"], vec!["enorme.bin"]]);
}

/// M2: the first unexpected permanent deletion in an operation STOPS the current batch (the receiver returns `E_ABORT`
/// after logging the event): later items in the SAME operation are not processed and remain in place, and nothing else is
/// sent. The receiver's own `E_ABORT` is not counted as a shell error.
#[test]
fn el_primer_borrado_inesperado_corta_el_lote_en_curso() {
    let sb = Sandbox::new();
    // All items are small (grouped in ONE operation), with one named `huge`: the shell decides the operation does not fit.
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
    // The first item was deleted (event logged); the rest of the operation was NOT processed and remains in place.
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

/// R1 (direct, without environment variables): the shell deletes the file with CLASSIC semantics while the guard handle remains open;
/// the name remains visible as "pending deletion" (opening it is denied). `judge` decides by OBJECT: it is a deletion
/// this is permanent deletion, not "the shell touched nothing." An implementation that checks by name (`symlink_metadata`) fails here.
#[test]
fn un_borrado_con_la_guarda_abierta_sigue_siendo_un_borrado_definitivo() {
    let sb = Sandbox::new();
    let shell = FakeShell::new(&sb, None, 0, "nunca");
    let shell: &dyn RecycleShell = shell.as_ref();
    let f = sb.write("r/borrado.bin", 1_000);
    let id = {
        let h = sys::open_for_query(&f).unwrap();
        sys::meta(&h).unwrap().identity
    };
    let p = pending(id, &f, &f);
    // Before deletion: intact.
    assert!(untouched_by_shell(&p));
    // The shell deletes it (classic semantics) while the guard is still open.
    sys::delete_legacy_for_test(&f).unwrap();
    // The NAME remains visible (which fooled `symlink_metadata`); opening it is denied, and the guard
    // reports it as pending deletion.
    assert!(fs::symlink_metadata(&f).is_ok(), "el nombre sigue visible");
    assert_eq!(sys::open_for_query(&f).err(), Some(sys::ACCESS_DENIED));
    assert!(sys::meta(&p.held).unwrap().delete_pending);
    assert!(!untouched_by_shell(&p));
    let ev = crate::recycle::DeleteEvent {
        item: OsString::from(f.as_os_str()),
        hr: 0x0027_0008,
        state: crate::recycle::BinState::Deleted,
    };
    assert_eq!(
        judge(&p, Some(&ev), shell, false, false).0,
        ItemStatus::DeletedUnexpectedly
    );
    // Once the guard is released, the file really disappears.
    drop(p);
    assert!(!f.exists());
    // Same for an empty folder deleted with classic semantics.
    let d = sb.mkdir("r/carpeta");
    let did = sys::meta(&sys::open_for_query(&d).unwrap())
        .unwrap()
        .identity;
    let pd = pending(did, &d, &d);
    assert!(untouched_by_shell(&pd));
    sys::delete_legacy_for_test(&d).unwrap();
    assert!(!untouched_by_shell(&pd));
    let evd = crate::recycle::DeleteEvent {
        item: OsString::from(d.as_os_str()),
        hr: 0x0027_0008,
        state: crate::recycle::BinState::Deleted,
    };
    assert_eq!(
        judge(&pd, Some(&evd), shell, false, false).0,
        ItemStatus::DeletedUnexpectedly
    );
    // An item replaced by a different object under the same name (different identity) is not "intact" either.
    let g = sb.write("r/sustituido.bin", 10);
    let gid = sys::meta(&sys::open_for_query(&g).unwrap())
        .unwrap()
        .identity;
    let pg = pending(gid, &g, &g);
    fs::rename(&g, sb.path("r/sustituido-ori.bin")).unwrap();
    sb.write("r/sustituido.bin", 10);
    assert!(!untouched_by_shell(&pg));
}

/// R6 (without the real Recycle Bin): the shell permanently deletes ONE child of a folder and reports the folder as "deleted," though it
/// remains in place. The folder object is intact, but its contents are not: this is NOT `ShellDeclined`, but a permanent deletion
/// remains in place. Report partial deletion and keep the stop. If the folder is truly intact, it remains `ShellDeclined`.
#[test]
fn una_carpeta_que_pierde_hijos_no_es_shell_declined() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    // One folder whose name makes the fake shell delete a child, and another left intact.
    sb.write("Docs/parcial/a.bin", 3_000);
    sb.write("Docs/parcial/b.bin", 3_000);
    sb.write("Docs/intacto/a.bin", 3_000);
    sb.write("Docs/intacto/b.bin", 3_000);
    for (name, esperado) in [
        ("parcial", ItemStatus::DeletedUnexpectedly),
        ("intacto", ItemStatus::NotRecycled(RecycleFail::Shell(0))),
    ] {
        let mut b = Basket::new();
        b.add_path(sb.long(&format!("Docs/{name}")), c.protection());
        let plan = approve(&c, &b);
        assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
        let rep = exec(&c, &plan, false, false, None).unwrap();
        assert_eq!(rep.items[0].status, esperado, "{name}");
        assert!(rep.recycle_stopped, "{name}: el corte se mantiene");
        assert!(sb.path(&format!("Docs/{name}")).exists());
        if name == "parcial" {
            assert_eq!(rep.deleted_unexpectedly, 1);
            assert_eq!(rep.stop_reason, Some(StopReason::DeletedUnexpectedly));
            assert_eq!(sb.count_entries("Docs/parcial"), 1, "perdió un hijo");
        } else {
            assert_eq!(rep.deleted_unexpectedly, 0);
            assert_eq!(rep.stop_reason, Some(StopReason::ShellDeclined));
            assert_eq!(sb.count_entries("Docs/intacto"), 2);
        }
    }
}

/// A-M1 (case 1): the shell deletes a folder child and THEN fails with `E_ACCESSDENIED`. Previously, the failure result
/// was returned as-is (`NotRecycled`) without checking effects: `deleted_unexpectedly` stayed at 0 and the next item
/// was sent anyway. Now a failure is judged by whether the object (and, for a folder, its contents) remained intact; it is recorded
/// Partial deletion is now detected, and sending stops.
#[test]
fn un_fallo_de_la_shell_tras_perder_un_hijo_es_un_borrado_parcial_y_corta_el_envio() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(10_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/pierde-y-falla/a.bin", 3_000);
    sb.write("Docs/pierde-y-falla/b.bin", 3_000);
    sb.write("Docs/otro.txt", 100);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/pierde-y-falla"), c.protection());
    b.add_path(sb.long("Docs/otro.txt"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let st = |n: &str| {
        rep.items
            .iter()
            .find(|i| i.path.ends_with(n))
            .unwrap()
            .status
    };
    assert_eq!(st("pierde-y-falla"), ItemStatus::DeletedUnexpectedly);
    assert_eq!(
        st("otro.txt"),
        ItemStatus::NotRecycled(RecycleFail::Stopped)
    );
    assert_eq!(rep.deleted_unexpectedly, 1);
    assert!(rep.recycle_stopped);
    assert_eq!(rep.stop_reason, Some(StopReason::DeletedUnexpectedly));
    assert_eq!(
        fake.names(),
        vec![vec!["pierde-y-falla"]],
        "no se envió nada más"
    );
    assert!(sb.path("Docs/otro.txt").exists());
    assert_eq!(sb.count_entries("Docs/pierde-y-falla"), 1, "perdió un hijo");
}

/// A-M1 (case 2): the shell deletes one child and leaves another with the same name and size. Totals (size and entry count)
/// are identical, so previously this looked like "the shell touched nothing" (`ShellDeclined`). The content FINGERPRINT
/// (identity, size, and links for each entry) now distinguishes it as a partial deletion.
#[test]
fn quitar_un_hijo_y_poner_otro_del_mismo_tamano_no_pasa_por_intacto() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/sustituye/a.bin", 3_000);
    sb.write("Docs/sustituye/b.bin", 3_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/sustituye"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    let before = plan.items[0].estimate;
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(rep.items[0].status, ItemStatus::DeletedUnexpectedly);
    assert_eq!(rep.stop_reason, Some(StopReason::DeletedUnexpectedly));
    assert_eq!(rep.deleted_unexpectedly, 1);
    // The totals really would not reveal anything: same count, same bytes.
    let after = {
        let it = &plan.items[0];
        let Remeasure::Now(s) = c.remeasure(it, &cancel()) else {
            panic!("la medida debía ser completa");
        };
        s
    };
    assert_eq!(
        (before.bytes, before.logical, before.files, before.dirs),
        (after.bytes, after.logical, after.files, after.dirs)
    );
    assert_ne!(
        (before.fp_sum, before.fp_xor),
        (after.fp_sum, after.fp_xor),
        "la huella sí cambia"
    );
}

/// A2-M1 (Astra repeat at the shell-operation level): TWO small folders in one operation; the shell deletes a child from each
/// and returns an error. Previously the receiver reported success after a failure, the next `PreDeleteItem` passed, and both
/// suffered unexpected loss. Now the first failure stops the batch; the second folder is not processed and retains all its
/// children. (The fake shell uses the real receiver, `SinkCore`.)
#[test]
fn con_dos_carpetas_en_la_misma_operacion_un_fallo_con_perdida_parcial_corta_antes_de_la_segunda() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    for d in ["falla1", "falla2"] {
        sb.write(&format!("Docs/{d}/a.bin"), 100);
        sb.write(&format!("Docs/{d}/b.bin"), 100);
    }
    let items = [
        recycle::SendItem::capture(&sb.long("Docs/falla1")),
        recycle::SendItem::capture(&sb.long("Docs/falla2")),
    ];
    let out = fake.recycle(&items);
    assert!(out.sink_aborted, "el fallo cortó el lote");
    assert_eq!(
        out.events.len(),
        1,
        "la segunda no se procesó: {:?}",
        out.events
    );
    assert_eq!(out.events[0].hr, 0x8007_0005u32 as i32);
    assert!(out.vetoed.is_empty(), "no es un veto");
    // The first folder lost a child; the second remains intact.
    assert_eq!(sb.count_entries("Docs/falla1"), 1);
    assert_eq!(sb.count_entries("Docs/falla2"), 2);
}

/// A2-M1 in the executor: three small files in ONE operation; the shell fails on the second (access denied, without touching
/// nothing). The receiver stops the batch: the second item is judged correctly (`Denied`, intact); the third remains unprocessed
/// it); later items are `Stopped` and remain in place, and sending stops with "Windows could not send an item and left it intact." The
/// shell error is recorded even though the `E_ABORT` came from the receiver.
#[test]
fn un_fallo_a_mitad_de_un_lote_de_archivos_cancela_lo_pendiente_y_detiene_el_envio() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("a.bin", 100, 4_096),
            ("deniega.bin", 100, 4_096),
            ("c.bin", 100, 4_096),
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
    assert_eq!(st("a.bin"), ItemStatus::Recycled);
    assert_eq!(
        st("deniega.bin"),
        ItemStatus::NotRecycled(RecycleFail::Denied)
    );
    assert_eq!(st("c.bin"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert_eq!(fake.names().len(), 1, "una sola operación");
    assert!(sb.path("Docs/deniega.bin").exists() && sb.path("Docs/c.bin").exists());
    assert!(rep.recycle_stopped);
    assert_eq!(rep.stop_reason, Some(StopReason::ShellDeclined));
    assert_eq!((rep.deleted_unexpectedly, rep.stopped_items), (0, 1));
    assert_eq!(
        rep.shell_error,
        Some(0x8007_0005u32 as i32),
        "el error de la shell no se pierde tras el E_ABORT del receptor"
    );
}

/// A2-M1: the reverse. If the failure is the LAST item in its operation (always true for a folder, which goes alone), stopping does not cancel
/// nothing pending, and subsequent operations continue: a folder with access denied (verified intact) does not prevent sending
/// the next operation.
#[test]
fn un_fallo_de_una_carpeta_que_va_sola_no_detiene_las_operaciones_siguientes() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/deniega-d1/x.bin", 100);
    sb.write("Docs/d2/y.bin", 100);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/deniega-d1"), c.protection());
    b.add_path(sb.long("Docs/d2"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let st = |n: &str| {
        rep.items
            .iter()
            .find(|i| i.path.ends_with(n))
            .unwrap()
            .status
    };
    assert_eq!(
        st("deniega-d1"),
        ItemStatus::NotRecycled(RecycleFail::Denied)
    );
    assert_eq!(st("d2"), ItemStatus::Recycled);
    assert!(!rep.recycle_stopped);
    assert_eq!(rep.shell_error, Some(0x8007_0005u32 as i32));
    assert_eq!(fake.names(), vec![vec!["deniega-d1"], vec!["d2"]]);
    assert_eq!(sb.count_entries("Docs/deniega-d1"), 1, "intacta");
}

/// R6 with the REAL shell (owned items only, after `TESELA_REAL_RECYCLE_TESTS=1`): a folder containing a huge sparse file (which does not
/// and small files. Record what the shell actually does and enforce the invariant: if the result is "the shell touched
/// nothing" (`NotRecycled`), all children remain; if any are missing, the result is `DeletedUnexpectedly`.
#[test]
fn con_la_shell_real_una_carpeta_con_un_disperso_enorme_no_pierde_hijos_sin_decirlo() {
    use crate::exec::{BinRoom, RealShell};
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    struct PassShell;
    impl RecycleShell for PassShell {
        fn recycle(&self, items: &[recycle::SendItem]) -> recycle::BatchOutcome {
            RealShell.recycle(items)
        }
        fn bin_room(&self, _: &Path) -> BinRoom {
            BinRoom {
                max: Some(u64::MAX / 4),
                used: Some(0),
            }
        }
        fn fit(&self, _: &Path, _: u64) -> Fit {
            Fit::Fits
        }
        fn is_bin_path(&self, p: &Path) -> bool {
            RealShell.is_bin_path(p)
        }
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb).with_shell(Arc::new(PassShell));
    let total = tesela_scanner::volume_space(&sb.long_root())
        .map(|v| v.total)
        .unwrap_or(1 << 40);
    let len = total.saturating_mul(2).max(1 << 40);
    sb.write("Docs/carpeta/a-pequeno.bin", 2_000);
    crate::testutil::sparse_file(&sb.path("Docs/carpeta/b-enorme.bin"), len);
    sb.write("Docs/carpeta/c-pequeno.bin", 2_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/carpeta"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let left = if sb.path("Docs/carpeta").exists() {
        sb.count_entries("Docs/carpeta")
    } else {
        0
    };
    eprintln!(
        "carpeta con disperso enorme (shell real): estado = {:?}, hijos que quedan = {left} de 3, recycle_stopped = {}",
        rep.items[0].status, rep.recycle_stopped
    );
    match rep.items[0].status {
        // The shell says it touched nothing: all three children must remain.
        ItemStatus::NotRecycled(_) => assert_eq!(left, 3, "faltan hijos sin decirlo"),
        // Deleted (fully or partially) or moved to the Recycle Bin: each result is consistent with what remains.
        ItemStatus::DeletedUnexpectedly | ItemStatus::Recycled | ItemStatus::RecycledOther => {}
        other => panic!("estado inesperado: {other:?}"),
    }
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

/// R1 with the REAL shell (one owned file only, after `TESELA_REAL_RECYCLE_TESTS=1`): a sparse file twice the volume size,
/// which the shell really deletes, reaches `judge` with the guard open and is recorded as `DeletedUnexpectedly` (not
/// as "untouched"). The real shell receives the item (a wrapper shell skips the batching logic that would have stopped it).
#[test]
fn con_la_shell_real_un_disperso_enorme_eliminado_consta_como_borrado_definitivo() {
    use crate::exec::{BinRoom, RealShell};
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    /// The real shell, but with batching that always accepts and a fit check that always succeeds.
    struct PassShell;
    impl RecycleShell for PassShell {
        fn recycle(&self, items: &[recycle::SendItem]) -> recycle::BatchOutcome {
            RealShell.recycle(items)
        }
        fn bin_room(&self, _: &Path) -> BinRoom {
            BinRoom {
                max: Some(u64::MAX / 4),
                used: Some(0),
            }
        }
        fn fit(&self, _: &Path, _: u64) -> Fit {
            Fit::Fits
        }
        fn is_bin_path(&self, p: &Path) -> bool {
            RealShell.is_bin_path(p)
        }
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb).with_shell(Arc::new(PassShell));
    let total = tesela_scanner::volume_space(&sb.long_root())
        .map(|v| v.total)
        .unwrap_or(1 << 40);
    let len = total.saturating_mul(2).max(1 << 40);
    sb.mkdir("Docs");
    crate::testutil::sparse_file(&sb.path("Docs/enorme.bin"), len);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/enorme.bin"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    eprintln!("disperso enorme (real): {:?}", rep.items[0].status);
    assert_eq!(rep.items[0].status, ItemStatus::DeletedUnexpectedly);
    assert_eq!(rep.deleted_unexpectedly, 1);
    assert!(rep.recycle_stopped);
    assert_eq!(rep.stop_reason, Some(StopReason::DeletedUnexpectedly));
    assert!(
        !sb.path("Docs/enorme.bin").exists(),
        "la shell lo eliminó de verdad"
    );
    assert_eq!(ids(), before);
}

/// H2: the shell reports success with null `psiNewlyCreated` but did NOT touch the item (tested with a folder containing a
/// huge sparse file: `hr = 0x00270003`, and the folder remains on disk). Do not claim anything was deleted
/// (`DeletedUnexpectedly` applies only if the name is gone), but keep the safety stop.
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

/// M2: an unverified path (`InBinUnknownPath` with success) also stops the batch, and `Unverified` causes
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
    // Capacity 10,000: a "large" item (> 1,000) goes alone; the budget is 9,000 minus occupancy.
    let fake = FakeShell::new(&sb, Some(10_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("a", 400, 400),
            ("b", 400, 400),
            ("c", 400, 400),
            ("grande", 2_000, 2_000),
            ("d", 400, 400),
        ],
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert!(
        rep.items.iter().all(|i| i.status == ItemStatus::Recycled),
        "{:?}",
        rep.items
    );
    // a, b, and c together; the large item alone; d alone (the rest of the queue).
    assert_eq!(
        fake.names(),
        vec![vec!["a", "b", "c"], vec!["grande"], vec!["d"]]
    );
    assert!(!rep.recycle_stopped);

    // With the Recycle Bin nearly full, the budget runs out: items that do not fit are NOT sent.
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
    // Budget = 9,000 - 8,500 = 500: one item fits (400); 100 remain afterward, so no more fit.
    assert_eq!(st("f1"), ItemStatus::Recycled);
    for n in ["f2", "f3", "f4"] {
        assert_eq!(st(n), ItemStatus::NotRecycled(RecycleFail::NoRoom), "{n}");
        assert!(sb2.path(&format!("Docs/{n}")).exists());
    }
    assert_eq!(fake2.names(), vec![vec!["f1"]]);
    assert_eq!(rep2.no_room, 3);
    assert!(!rep2.recycle_stopped);
}

/// B2: without known Recycle Bin capacity, there is no guarantee that an item fits (and the shell really deletes items that do not):
/// NO items are sent (`NoRoom`), including the small ones. Previously each went alone.
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

/// B2: batching capacity is ONLY the configured value (`MaxCapacity`); without it (`Unknown`) or in any other case
/// (no Recycle Bin, disabled, indeterminate), capacity is unknown: `max = None` and batching fails closed. Do not assume an
/// percentage of the volume.
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
    // Capacity is configured but occupancy is unknown: nothing is sent.
    assert_eq!(
        take_op(&[1], room_from(BinLimit::Max(10_000), None)),
        Take::NoRoom
    );
    // With configured capacity, the fake shell batches and rejects as usual.
    let sb = Sandbox::new();
    let room = room_from(BinLimit::Max(10_000), Some(0));
    let fake = FakeShell::new(&sb, room.max, 0, "enorme");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    // Capacity 10,000: items over 9,000 do not fit even alone; a "large" item (> 1,000) goes alone; small ones are grouped.
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("a", 100, 100),
            ("b", 100, 100),
            ("grande", 2_000, 2_000),
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

/// A directory under `Docs/<name>` containing a `len`-byte file, approved with its actual size.
fn plan_dir(sb: &Sandbox, c: &Cleaner, name: &str, len: usize) -> Plan {
    sb.write(&format!("Docs/{name}/f0.bin"), len);
    let mut b = Basket::new();
    b.add_path(sb.long(&format!("Docs/{name}")), c.protection());
    let plan = approve(c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    assert!(plan.items[0].is_dir());
    plan
}

/// B2: a directory that GREW by 1 MiB after approval is not sent: batching used a size that is no longer accurate. It
/// remains in place with status "changed since confirmation" (the summary's `changed` count).
#[test]
fn un_directorio_que_crecio_tras_la_aprobacion_no_se_envia() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_dir(&sb, &c, "crece", 5_000);
    // After approval: add 1 MiB inside.
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
    // With no changes, the same directory is sent (control).
    let sb2 = Sandbox::new();
    let fake2 = FakeShell::new(&sb2, Some(1 << 40), 0, "nunca");
    let c2 = cleaner_for(&sb2).with_shell(fake2.clone());
    let plan2 = plan_dir(&sb2, &c2, "igual", 5_000);
    let rep2 = exec(&c2, &plan2, false, false, None).unwrap();
    assert_eq!(rep2.items[0].status, ItemStatus::Recycled);
}

/// H6.2: measure a directory RIGHT BEFORE its operation, not when preparing the whole chunk. Here it grows during the
/// previous operation (a large file sent alone), so it is not sent when its turn arrives.
#[test]
fn el_directorio_se_mide_justo_antes_de_su_operacion_no_al_preparar_el_trozo() {
    let sb = Sandbox::new();
    // Capacity 10,000: anything over 1,000 goes ALONE in its operation (two items means two operations).
    let fake = FakeShell::new(&sb, Some(10_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/a.bin", 6_000);
    sb.write("Docs/crece/f0.bin", 6_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/a.bin"), c.protection());
    b.add_path(sb.long("Docs/crece"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2);
    // Estimates match actual sizes (~8 KiB each: over 10% of 10,000, and they fit).
    for it in &plan.items {
        assert!(it.estimate.bytes > 1_000 && it.fit == Fit::Fits);
    }
    // The directory grows as `a.bin` is about to be sent (after chunk preparation, before its own operation).
    // If measured during preparation, the directory would have been batched using its old size.
    let grow = sb.path("Docs/crece/nuevo.bin");
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), move |p| {
        if p.ends_with("a.bin") {
            fs::write(&grow, vec![1u8; 1 << 20]).unwrap();
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
    assert_eq!(st("a.bin"), ItemStatus::Recycled);
    assert_eq!(
        st("crece"),
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert!(sb.path("Docs/crece/nuevo.bin").exists());
    assert_eq!(
        fake.names(),
        vec![vec!["a.bin"]],
        "el directorio no llegó a la shell"
    );
}

/// A-B1 / M-1: a SECOND directory that grows while the first is measured is not sent. (Before this test, skipping
/// remeasurement of non-first candidates—`let skip = true`—survived all other tests. Since A2-B1, each directory has its
/// own operation, so the second is measured later when its turn arrives.)
#[test]
fn un_segundo_directorio_de_la_misma_operacion_que_crece_no_se_envia() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/d1/x1.bin", 1_000);
    sb.write("Docs/d2/y1.bin", 1_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/d1"), c.protection());
    b.add_path(sb.long("Docs/d2"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
    let grow = sb.path("Docs/d2/crece.bin");
    // While d1 (the first directory) is being measured, d2 grows.
    let _hook = crate::engine::test_hooks::install(sb.long("Docs"), move |q| {
        if q.ends_with("x1.bin") {
            fs::write(&grow, vec![1u8; 1 << 20]).unwrap();
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
    assert_eq!(st("d1"), ItemStatus::Recycled);
    assert_eq!(
        st("d2"),
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert_eq!(fake.names(), vec![vec!["d1"]]);
}

/// A2-B1 (external audit blocker), Astra repeat: a simulated Recycle Bin with capacity 100,000 and two small folders.
/// Previously they shared the SAME operation and were measured one after another; while the first received its final
/// measurement, the already-measured second folder grew by 1 MiB. `PreDeleteItem` checked only identities, so both reached
/// the shell and became `Recycled`. Now each directory has its own operation and is measured just before delivery, so a
/// folder that grows while the previous one is in the shell is measured afterward and not sent.
#[test]
fn dos_directorios_pequenos_de_astra_no_comparten_operacion_y_el_que_crece_no_se_envia() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/d1/x1.bin", 1_000);
    sb.write("Docs/d2/y1.bin", 1_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/d1"), c.protection());
    b.add_path(sb.long("Docs/d2"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
    for it in &plan.items {
        assert_eq!(it.fit, Fit::Fits);
    }
    // Growth happens RIGHT as d1 is sent to the shell: in a shared operation, d2 would already have been measured.
    let grow = sb.path("Docs/d2/crece.bin");
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), move |q| {
        if q.ends_with("d1") {
            fs::write(&grow, vec![1u8; 1 << 20]).unwrap();
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
    assert_eq!(st("d1"), ItemStatus::Recycled);
    assert_eq!(
        st("d2"),
        ItemStatus::Root(crate::engine::RootStatus::Changed),
        "no se entrega con un tamaño que ya no es cierto"
    );
    assert_eq!(
        fake.names(),
        vec![vec!["d1"]],
        "cada directorio, en su operación; el que creció no llegó a la shell"
    );
    assert!(sb.path("Docs/d2/crece.bin").exists() && sb.path("Docs/d2/y1.bin").exists());
    assert!(rep.totals.changed >= 1);
    // The sent items also stay within 90% of simulated capacity. A3-m1: simulated occupancy sums the CONTENTS of the
    // sent folder (d1: 1,000 bytes; d2 was not sent), so this assertion measures something (previously `metadata(dir).len()` was 0).
    assert_eq!(*fake.used.lock().unwrap(), 1_000);
    assert!(*fake.used.lock().unwrap() <= 90_000);
}

/// H1 (audit): recheck the budget immediately before sending a DIRECTORY. Simulated Recycle Bin capacity 10,000, occupancy 8,950
/// already occupied (budget 9,000 - 8,950 = 50): a 200-byte folder (about 4 KiB on disk) no longer fits when it is about to be
/// sent. It never reaches the shell: `NoRoom` (the real shell would have deleted it or evicted existing Recycle Bin contents).
#[test]
fn un_directorio_que_ya_no_cabe_al_entregarlo_no_llega_a_la_shell() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(10_000), 8_950, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/d1/x.bin", 200);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/d1"), c.protection());
    let plan = approve(&c, &b);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::NotRecycled(RecycleFail::NoRoom)
    );
    assert!(fake.names().is_empty());
    assert!(sb.path("Docs/d1/x.bin").exists());
    assert_eq!(rep.no_room, 1);
}

/// A2-B1: a directory NEVER shares an operation with anything (other directories or files); consecutive small files are
/// grouped. Queue: file, directory, directory, file, file => [a], [d1], [d2], [b, c].
#[test]
fn un_directorio_va_siempre_solo_y_los_archivos_contiguos_se_agrupan() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/a.txt", 100);
    sb.write("Docs/d1/x.bin", 100);
    sb.write("Docs/d2/y.bin", 100);
    sb.write("Docs/b.txt", 100);
    sb.write("Docs/c.txt", 100);
    let mut b = Basket::new();
    for n in ["a.txt", "d1", "d2", "b.txt", "c.txt"] {
        b.add_path(sb.long(&format!("Docs/{n}")), c.protection());
    }
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 5, "{:?}", plan.rejected);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert!(rep.items.iter().all(|i| i.status == ItemStatus::Recycled));
    assert_eq!(
        fake.names(),
        vec![
            vec!["a.txt"],
            vec!["d1"],
            vec!["d2"],
            vec!["b.txt", "c.txt"]
        ]
    );
}

/// GM-B3 (replaces B2, "a directory that shrank is batched using its new size"): a folder is sent only with the SAME
/// contents that were approved. Previously, `fresh_size` only checked that it had not grown, so a folder that LOST a file
/// or had a file replaced (with a smaller or same-size file with a different `FileId`) was sent with unapproved contents.
/// Now any content change, including shrinking, requires reconfirmation ("changed since confirmation") and is not sent.
#[test]
fn un_directorio_cuyo_contenido_cambio_en_cualquier_sentido_no_se_envia() {
    // Control: with no changes, the same directory is sent.
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/igual/a.bin", 5_000);
    sb.write("Docs/igual/b.bin", 5_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/igual"), c.protection());
    let plan = approve(&c, &b);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(rep.items[0].status, ItemStatus::Recycled);

    // Three post-approval changes, each with its own tree.
    for (name, change) in [
        ("pierde", "quita"),
        ("cambia-por-menor", "menor"),
        ("cambia-por-igual", "igual-tamano"),
    ] {
        let sb = Sandbox::new();
        let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
        let c = cleaner_for(&sb).with_shell(fake.clone());
        sb.write(&format!("Docs/{name}/a.bin"), 5_000);
        sb.write(&format!("Docs/{name}/b.bin"), 5_000);
        let mut b = Basket::new();
        b.add_path(sb.long(&format!("Docs/{name}")), c.protection());
        let plan = approve(&c, &b);
        assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
        let a = sb.path(&format!("Docs/{name}/a.bin"));
        fs::remove_file(&a).unwrap();
        match change {
            "quita" => {}
            "menor" => {
                sb.write(&format!("Docs/{name}/c.bin"), 100);
            }
            _ => {
                // A different object (different `FileId`) with the same name and size: totals remain unchanged.
                sb.write(&format!("Docs/{name}/a.bin"), 5_000);
            }
        }
        let rep = exec(&c, &plan, false, false, None).unwrap();
        assert_eq!(
            rep.items[0].status,
            ItemStatus::Root(crate::engine::RootStatus::Changed),
            "{name}: {:?}",
            rep.items[0]
        );
        assert!(
            fake.names().is_empty(),
            "{name}: no debe llegar a la shell con contenido no aprobado"
        );
        assert!(rep.totals.changed >= 1, "{name}");
    }
}

/// A-B1 (blocker): a FILE is also remeasured immediately before its operation. Two ~20 KB files in a simulated Recycle Bin
/// with capacity 100,000 (each exceeds 10% and gets its own operation): while the shell handles the first, the second grows
/// to 1 MiB. Previously it was sent using its size at chunk preparation (`measured: true`, and `fresh_size` was not checked).
/// Now it is not sent: "changed since confirmation."
#[test]
fn un_archivo_que_crece_mientras_espera_su_turno_no_se_envia() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(
        &sb,
        &c,
        &[("a.bin", 20_000, 20_480), ("b.bin", 20_000, 20_480)],
    );
    let grow = sb.path("Docs/b.bin");
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), move |p| {
        if p.ends_with("a.bin") {
            fs::write(&grow, vec![1u8; 1 << 20]).unwrap();
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
    assert_eq!(st("a.bin"), ItemStatus::Recycled);
    assert_eq!(
        st("b.bin"),
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert_eq!(
        fake.names(),
        vec![vec!["a.bin"]],
        "el que creció no llegó a la shell"
    );
    assert!(sb.path("Docs/b.bin").exists(), "sigue en su sitio");
    assert!(rep.totals.changed >= 1);
    assert!(!rep.recycle_stopped, "es un cambio, no un corte");
}

/// A2-B1: two small files share the SAME operation (each is under 10% of capacity). Between preparing the batch and
/// batch and the second item's `PreDeleteItem`, it grows by 1 MiB. The receiver remeasures it through its guard handle (without reopening
/// sending, one grows (same identity and path) and is vetoed: it never reaches the shell, is recorded as "changed since confirmation," and sending stops.
#[test]
fn un_archivo_que_crece_entre_preparar_el_lote_y_su_pre_delete_item_se_veta() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(
        &sb,
        &c,
        &[
            ("a.bin", 5_000, 8_192),
            ("b.bin", 5_000, 8_192),
            ("c.bin", 5_000, 8_192),
        ],
    );
    // The `before_shell` hook runs AFTER measurement and batch preparation, immediately before delivery.
    let grow = sb.path("Docs/b.bin");
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), move |p| {
        if p.ends_with("a.bin") {
            fs::write(&grow, vec![1u8; 1 << 20]).unwrap();
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
    assert_eq!(
        st("a.bin"),
        ItemStatus::Recycled,
        "se procesó antes del veto"
    );
    assert_eq!(
        st("b.bin"),
        ItemStatus::Root(crate::engine::RootStatus::Changed),
        "el que creció se veta: cambió desde la confirmación"
    );
    assert_eq!(
        st("c.bin"),
        ItemStatus::NotRecycled(RecycleFail::Stopped),
        "el veto corta el resto del lote"
    );
    assert_eq!(
        fake.names(),
        vec![vec!["a.bin", "b.bin", "c.bin"]],
        "una sola operación (la comprobación es del receptor)"
    );
    assert!(sb.path("Docs/b.bin").exists() && sb.path("Docs/c.bin").exists());
    assert!(rep.recycle_stopped);
    assert_eq!(rep.stop_reason, Some(StopReason::Changed));
    assert!(rep.totals.changed >= 1);
    assert_eq!(rep.deleted_unexpectedly, 0);
    assert_eq!(
        rep.shell_error, None,
        "el E_ABORT del vetado es del receptor: no es un error de Windows"
    );
}

/// A2-B1: the same case with the REAL shell (owned items only, after `TESELA_REAL_RECYCLE_TESTS=1`): the real receiver
/// remeasures through the guard in `PreDeleteItem` and vetoes the grown file with `E_ABORT`; the earlier file reaches the Recycle Bin
/// and the next item is not processed.
#[test]
fn con_la_shell_real_un_archivo_que_crece_en_el_lote_se_veta() {
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let sb = Sandbox::new();
    let _guard = BinGuard { uuid: sb.uuid() };
    let c = cleaner_for(&sb);
    let room =
        crate::exec::RealShell.bin_room(&crate::volume::volume_root(&sb.long_root()).unwrap());
    let Some(max) = room.max else {
        eprintln!("la Papelera de este volumen no tiene capacidad configurada: la prueba se salta");
        return;
    };
    // With real capacity, three 1,000-byte files fit together (actual occupancy may leave too little room, so this is skipped).
    if room
        .used
        .is_none_or(|u| max / 10 * 9 < u.saturating_add(1 << 20))
    {
        eprintln!("la Papelera de este volumen no tiene sitio de sobra: la prueba se salta");
        return;
    }
    for n in ["a.txt", "b.txt", "c.txt"] {
        sb.write(&format!("Docs/{n}"), 1_000);
    }
    let mut b = Basket::new();
    for n in ["a.txt", "b.txt", "c.txt"] {
        b.add_path(sb.long(&format!("Docs/{n}")), c.protection());
    }
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 3, "{:?}", plan.rejected);
    let grow = sb.path("Docs/b.txt");
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), move |p| {
        if p.ends_with("a.txt") {
            fs::write(&grow, vec![1u8; 1 << 20]).unwrap();
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
    assert_eq!(st("a.txt"), ItemStatus::Recycled, "{rep:?}");
    assert_eq!(
        st("b.txt"),
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert_eq!(st("c.txt"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert!(sb.path("Docs/b.txt").exists() && sb.path("Docs/c.txt").exists());
    assert_eq!(rep.stop_reason, Some(StopReason::Changed));
    // A receiver veto is not a Windows error: the shell reports it with `E_ABORT` (from the receiver itself), and this must not
    // must appear as "the Recycle Bin returned an error."
    assert_eq!(rep.shell_error, None, "{rep:?}");
    let mine: Vec<BinItem> = recycle::list_bin()
        .unwrap()
        .into_iter()
        .filter(|i| is_mine(i, &sb.uuid()))
        .collect();
    assert_eq!(mine.len(), 1, "solo a.txt llegó a la Papelera: {mine:?}");
    recycle::purge_items(&mine, &AtomicBool::new(false)).unwrap();
    assert_eq!(ids(), before);
}

/// A-B1: the reverse. If the current size is LESS than the previous measurement, use the new size and update the latest
/// measurement. (NTFS does not release allocation for a truncated file while handles remain open—the guard does—so with
/// the guard open, only the logical size shrinks; check `fresh_size` directly.)
#[test]
fn un_archivo_que_encoge_se_mide_con_su_tamano_nuevo() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake);
    let plan = plan_de(&sb, &c, &[("a.bin", 20_000, 20_480)]);
    let it = &plan.items[0];
    let mut st = StatsSnapshot::default();
    let Prepared::Ready(mut p) = c.prepare_recycle(it, &mut st) else {
        panic!("debía prepararse");
    };
    let real = p.last.expect("un archivo se mide al preparar");
    assert_eq!(real, 20_480);
    // Simulate a larger previous measurement (the file was 50,000 bytes and was truncated).
    p.size = 50_000;
    p.last = Some(50_000);
    c.fresh_size(it, &mut p, &mut st, &cancel()).unwrap();
    assert_eq!((p.size, p.last), (real, Some(real)));
    // If it grows beyond the APPROVED size (even if it stays under the latest measurement, simulated as huge here), it is not sent.
    fs::write(sb.path("Docs/a.bin"), vec![1u8; 30_000]).unwrap();
    p.last = Some(u64::MAX);
    let err = c.fresh_size(it, &mut p, &mut st, &cancel()).unwrap_err();
    assert_eq!(err.0, ItemStatus::Root(crate::engine::RootStatus::Changed));
    assert_eq!(st.changed, 1);
    // It is also not sent if it exceeds the latest measurement but not the approved size (simulated with huge approval).
    let mut big = plan.items[0].clone();
    big.estimate.bytes = 1 << 30;
    p.last = Some(20_480);
    let err = c.fresh_size(&big, &mut p, &mut st, &cancel()).unwrap_err();
    assert_eq!(err.0, ItemStatus::Root(crate::engine::RootStatus::Changed));
    assert_eq!(st.changed, 2);
}

/// GM-m6: report the current size of a file that shrank between approval and sending (measured by the guard), not its
/// approved estimate: `stats.bytes`, `removed`, and the `recycled_bytes` counter.
#[test]
fn un_archivo_que_encogio_se_informa_con_su_tamano_de_ahora() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake);
    sb.write("Docs/e.bin", 20_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/e.bin"), c.protection());
    let plan = approve(&c, &b);
    let approved = plan.items[0].estimate;
    assert!(approved.bytes >= 20_000 && approved.logical == 20_000);
    // Shrinks after approval: from 20,000 to 5,000 bytes.
    let f = fs::OpenOptions::new()
        .write(true)
        .open(sb.path("Docs/e.bin"))
        .unwrap();
    f.set_len(5_000).unwrap();
    drop(f);
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Recycled,
        "{:?}",
        rep.items[0]
    );
    let now = rep.items[0].stats;
    assert!(now.bytes < approved.bytes, "{now:?} frente a {approved:?}");
    assert_eq!(rep.recycled_bytes, now.bytes);
    let r = &rep.items[0].removed[0];
    assert_eq!((r.bytes, r.logical), (now.bytes, 5_000));
}

/// GM-B1: space used by reparse points INSIDE a folder counts against Recycle Bin capacity. Previously, a hydrated cloud file
/// (or any reparse point) added 0 to `bytes` and `logical`: the shell moves the WHOLE folder and counts those
/// files against `MaxCapacity`, so a folder that appeared small did not fit and the shell deleted it. Here, the reparse
/// point is a third-party file with a huge logical size (sparse, so it occupies no disk space).
#[test]
fn una_carpeta_con_un_reparse_point_grande_dentro_no_cabe_en_la_papelera() {
    let sb = Sandbox::new();
    // Capacity 100,000: 90% is 90,000 bytes.
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/nube/pequeno.bin", 1_000);
    sb.mkdir("Docs/nube");
    if !crate::testutil::make_reparse_file(&sb.path("Docs/nube/hidratado.bin"), 5 << 20) {
        eprintln!("el volumen no admite reparse points de prueba: se omite");
        return;
    }
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/nube"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    let it = &plan.items[0];
    let est = it.estimate;
    // The reparse point is counted separately (not in `bytes`/`logical`, which represent space freed by deletion).
    assert_eq!(est.reparse, 1, "{est:?}");
    assert_eq!(est.reparse_logical, 5 << 20, "{est:?}");
    assert!(est.logical < 5_000, "{est:?}");
    // The previous calculation (excluding reparse points) would have said "fits."
    let old = est.bytes.max(est.logical).max(est.shared_bytes);
    assert!(old <= 90_000, "{est:?}");
    assert!(est.recycle_charge() >= 5 << 20);
    assert_eq!(it.fit, Fit::Unfit(UnfitReason::TooBig), "{it:?}");
    // Without the checkbox, it remains untouched and never reaches the shell.
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(rep.items[0].status, ItemStatus::SkippedUnfit);
    assert!(fake.names().is_empty());
    assert!(sb.path("Docs/nube/pequeno.bin").exists());

    // Control: the same tree with a small reparse point does fit and is sent.
    let sb2 = Sandbox::new();
    let fake2 = FakeShell::new(&sb2, Some(100_000), 0, "nunca");
    let c2 = cleaner_for(&sb2).with_shell(fake2.clone());
    sb2.write("Docs/nube/pequeno.bin", 1_000);
    assert!(crate::testutil::make_reparse_file(
        &sb2.path("Docs/nube/chico.bin"),
        2_000
    ));
    let mut b2 = Basket::new();
    b2.add_path(sb2.long("Docs/nube"), c2.protection());
    let plan2 = approve(&c2, &b2);
    assert_eq!(plan2.items[0].fit, Fit::Fits, "{:?}", plan2.items[0]);
    let rep2 = exec(&c2, &plan2, false, false, None).unwrap();
    assert_eq!(rep2.items[0].status, ItemStatus::Recycled);
}

/// GM-B1: a folder's measurement changes when the size of a reparse point inside it changes (included in the fingerprint
/// and snapshot); it is not sent with contents different from what was approved.
#[test]
fn un_reparse_point_que_cambia_de_tamano_tras_aprobar_no_se_envia() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/nube/pequeno.bin", 1_000);
    if !crate::testutil::make_reparse_file(&sb.path("Docs/nube/r.bin"), 2_000) {
        return;
    }
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/nube"), c.protection());
    let plan = approve(&c, &b);
    // Replace the reparse point with a different-sized one (a different object).
    fs::remove_file(sb.path("Docs/nube/r.bin")).unwrap();
    assert!(crate::testutil::make_reparse_file(
        &sb.path("Docs/nube/r.bin"),
        3_000
    ));
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert!(fake.names().is_empty());
}

/// A fake shell whose Recycle Bin occupancy changes between reads (another application fills it): the first read decides
/// the group, and the check AFTER remeasurement (A-B1) sees the change.
struct FlakyRoom {
    inner: Arc<FakeShell>,
    calls: std::sync::atomic::AtomicUsize,
    used: Vec<u64>,
}

impl RecycleShell for FlakyRoom {
    fn recycle(&self, items: &[recycle::SendItem]) -> recycle::BatchOutcome {
        self.inner.recycle(items)
    }
    fn bin_room(&self, _: &Path) -> BinRoom {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        BinRoom {
            max: self.inner.max,
            used: Some(self.used[n.min(self.used.len() - 1)]),
        }
    }
    fn fit(&self, p: &Path, b: u64) -> Fit {
        self.inner.fit(p, b)
    }
    fn is_bin_path(&self, p: &Path) -> bool {
        self.inner.is_bin_path(p)
    }
}

/// A3-M1 (Astra repeat): a folder with two 1,000-byte files and a simulated Recycle Bin of capacity 100,000. After traversal
/// counts the first file, it grows to 1 MiB. Traversal does not notice (it already counted it), and the fingerprint compared
/// with approval reflects what it SAW, so previously measurement passed and the folder became `Recycled`. With change
/// monitoring started before traversal, the measurement is invalid and the folder is not sent.
#[test]
fn una_carpeta_que_crece_durante_su_propio_recorrido_no_se_envia() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone()).with_threads(1);
    sb.write("Docs/d/f1.bin", 1_000);
    sb.write("Docs/d/f2.bin", 1_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/d"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    assert_eq!(plan.items[0].fit, Fit::Fits);
    // When opening the SECOND file during execution traversal (the first is already counted), grow the first to 1 MiB. The
    // approval hook has already run, so install this hook now.
    let seen: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let _hook = {
        let seen = seen.clone();
        crate::engine::test_hooks::install(sb.long("Docs/d"), move |p| {
            if p.extension().is_none_or(|e| e != "bin") {
                return;
            }
            let mut first = seen.lock().unwrap();
            match first.as_ref() {
                None => *first = Some(p.to_path_buf()),
                Some(counted) if counted != p => {
                    fs::write(counted, vec![1u8; 1 << 20]).unwrap();
                }
                _ => {}
            }
        })
    };
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert!(seen.lock().unwrap().is_some(), "el gancho corrió");
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Changed),
        "{:?}",
        rep.items[0]
    );
    assert!(fake.names().is_empty(), "no llegó a la shell");
    assert!(rep.totals.changed >= 1);
    assert!(
        *fake.used.lock().unwrap() < 90_000,
        "lo enviado nunca pasa del 90 %"
    );
}

/// A3-M1: a change AFTER traversal and BEFORE delivering the folder to the shell (the `before_shell` window) keeps it from
/// reaching the shell: "changed since confirmation."
#[test]
fn un_cambio_entre_el_recorrido_y_la_entrega_no_llega_a_la_shell() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/d/f1.bin", 1_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/d"), c.protection());
    let plan = approve(&c, &b);
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), |p| {
        if p.ends_with("d") {
            fs::write(p.join("f1.bin"), vec![1u8; 1 << 20]).unwrap();
        }
    });
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Changed),
        "{:?}",
        rep.items[0]
    );
    assert!(fake.names().is_empty(), "no se entrega");
    assert!(sb.path("Docs/d/f1.bin").exists());
}

/// A3-M1: a change AFTER submitting the operation and BEFORE `PreDeleteItem` (while the operation is in progress) is vetoed
/// by the receiver with `E_ABORT`: the status remains "changed since confirmation" and sending stops. Two cases: a folder
/// gains a new file (creation, immediate notification), or a file in it is renamed.
#[test]
fn un_cambio_antes_del_pre_delete_item_veta_la_carpeta() {
    for change in ["alta", "renombrado"] {
        let sb = Sandbox::new();
        let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
        let c = cleaner_for(&sb).with_shell(fake.clone());
        sb.write("Docs/d/f1.bin", 1_000);
        sb.write("Docs/otra/g.bin", 1_000);
        let mut b = Basket::new();
        b.add_path(sb.long("Docs/d"), c.protection());
        b.add_path(sb.long("Docs/otra"), c.protection());
        let plan = approve(&c, &b);
        assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
        let _hook =
            crate::engine::test_hooks::install_before_pre_delete(sb.long("Docs"), move |p| {
                if p.ends_with("d") {
                    match change {
                        "alta" => {
                            fs::write(p.join("nuevo.bin"), b"x").unwrap();
                        }
                        _ => fs::rename(p.join("f1.bin"), p.join("f2.bin")).unwrap(),
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
        assert_eq!(
            st("d"),
            ItemStatus::Root(crate::engine::RootStatus::Changed),
            "{change}"
        );
        // It reached the operation (the receiver vetoed it), and the veto stops the remaining items.
        assert_eq!(fake.names(), vec![vec!["d"]], "{change}");
        assert!(rep.recycle_stopped, "{change}");
        assert_eq!(rep.stop_reason, Some(StopReason::Changed), "{change}");
        assert_eq!(
            st("otra"),
            ItemStatus::NotRecycled(RecycleFail::Stopped),
            "{change}"
        );
        assert!(sb.path("Docs/d").exists() && sb.path("Docs/otra/g.bin").exists());
    }
}

/// A3-M1: do not send without monitoring (fail closed). If the second directory handle cannot be opened, the folder does
/// NOT reach the shell and the status gives the reason (system error); other items continue normally.
#[test]
fn si_no_se_puede_abrir_la_vigilancia_la_carpeta_no_se_envia() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/d/f1.bin", 1_000);
    sb.write("Docs/a.txt", 100);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/d"), c.protection());
    b.add_path(sb.long("Docs/a.txt"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
    let _fail = crate::sys::fail_watch_under(&sb.long("Docs/d"));
    let rep = exec(&c, &plan, false, false, None).unwrap();
    let it = |n: &str| rep.items.iter().find(|i| i.path.ends_with(n)).unwrap();
    assert_eq!(
        it("d").status,
        ItemStatus::Root(crate::engine::RootStatus::Failed(crate::sys::ACCESS_DENIED))
    );
    assert!(it("d").stats.other_errors >= 1);
    assert_eq!(
        it("a.txt").status,
        ItemStatus::Recycled,
        "un archivo no lleva vigilancia"
    );
    assert_eq!(
        fake.names(),
        vec![vec!["a.txt"]],
        "la carpeta no llegó a la shell"
    );
    assert!(sb.path("Docs/d/f1.bin").exists());
    // Control: without the forced failure, the same folder is sent.
    drop(_fail);
    let sb2 = Sandbox::new();
    let fake2 = FakeShell::new(&sb2, Some(100_000), 0, "nunca");
    let c2 = cleaner_for(&sb2).with_shell(fake2.clone());
    sb2.write("Docs/d/f1.bin", 1_000);
    let mut b2 = Basket::new();
    b2.add_path(sb2.long("Docs/d"), c2.protection());
    let plan2 = approve(&c2, &b2);
    let rep2 = exec(&c2, &plan2, false, false, None).unwrap();
    assert_eq!(rep2.items[0].status, ItemStatus::Recycled);
}

/// A3-M1: an untouched folder is sent (monitoring has no false positives from traversal or the guard, and the
/// the receiver), even with subfolders, and monitoring remains stopped at the end (the shell moves the folder). In
/// dry run is also measured and monitored, without touching anything.
#[test]
fn una_carpeta_que_nadie_toca_se_envia_con_su_vigilancia() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/d/a.bin", 5_000);
    sb.write("Docs/d/sub/b.bin", 5_000);
    sb.write("Docs/d/sub/hondo/c.bin", 5_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/d"), c.protection());
    let plan = approve(&c, &b);
    let dry = exec(&c, &plan, true, false, None).unwrap();
    assert_eq!(dry.items[0].status, ItemStatus::WouldRecycle);
    assert!(sb.path("Docs/d/sub/hondo/c.bin").exists());
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Recycled,
        "{:?}",
        rep.items[0]
    );
    assert_eq!(fake.names(), vec![vec!["d"]]);
    assert!(!sb.path("Docs/d").exists(), "la carpeta se movió");
}

/// A3-M1 (receiver, no COM): `PreDeleteItem` checks a directory's monitor: with no changes, it passes (and stops monitoring);
/// a change between monitoring startup and `PreDeleteItem` is vetoed with `VetoKind::Modified`.
#[test]
fn el_receptor_veta_el_directorio_que_cambio_y_deja_pasar_el_que_no() {
    for cambia in [false, true] {
        let sb = Sandbox::new();
        sb.write("Docs/d/f.bin", 1_000);
        let dir = sb.long("Docs/d");
        let mut item = recycle::SendItem::capture(&dir);
        // What approval did before monitoring started.
        drop(sys::open_dir_for_listing(&dir).unwrap());
        drop(sys::open_for_query(&dir.join("f.bin")).unwrap());
        let w = Arc::new(sys::DirWatch::start(&dir, item.identity).unwrap());
        item.watch = Some(w.clone());
        let core = recycle::SinkCore::for_send(std::slice::from_ref(&item));
        if cambia {
            fs::write(dir.join("f.bin"), vec![1u8; 1 << 20]).unwrap();
            // The notification arrives asynchronously.
            for _ in 0..40 {
                if w.changed() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let pasa = core.pre_delete(dir.clone().into_os_string());
        let (_, vetoes, tripped) = core.finish();
        if cambia {
            assert!(!pasa && tripped);
            assert_eq!(vetoes[0].kind, recycle::VetoKind::Modified);
        } else {
            assert!(pasa && vetoes.is_empty());
        }
    }
}

/// A3-M1 (measured cost): start monitoring, perform one measurement traversal, check, and stop for 50 folders containing four files. Print the results with
/// Print with `--nocapture`; do not assert timings (they depend on the machine), only that none produce false positives.
#[test]
fn medicion_a3_m1_coste_de_la_vigilancia_por_directorio() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    let mut dirs = Vec::new();
    for i in 0..50 {
        for j in 0..4 {
            sb.write(&format!("Docs/m{i}/s/f{j}.bin"), 100);
        }
        dirs.push(sb.long(&format!("Docs/m{i}")));
    }
    // What approval did before monitoring.
    let mut b = Basket::new();
    for d in &dirs {
        b.add_path(d.clone(), c.protection());
    }
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 50);
    let (mut watch, mut measure, mut stop) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    for it in &plan.items {
        let UnitSpec::Entry { approved, .. } = &it.spec else {
            panic!("entrada")
        };
        let t = std::time::Instant::now();
        let w = sys::DirWatch::start(&it.path, *approved).unwrap();
        watch += t.elapsed();
        let t = std::time::Instant::now();
        let r = c.remeasure(it, &cancel());
        measure += t.elapsed();
        assert!(matches!(r, Remeasure::Now(_)));
        assert!(!w.changed(), "sin falsos positivos: {:?}", it.path);
        let t = std::time::Instant::now();
        w.stop();
        stop += t.elapsed();
    }
    eprintln!(
        "A3-M1 coste por directorio (50 carpetas, 4 archivos): abrir+arrancar {:?}, recorrido de medida {:?}, parar {:?}",
        watch / 50,
        measure / 50,
        stop / 50
    );
}

/// M1 (audit): only cloud sync folders are tolerated as reparse ancestors. Junctions, symlinks, mount points,
/// mount point, WSL link, other tags, and a cloud-tagged file are NOT tolerated. A pure metadata test.
#[test]
fn solo_la_nube_se_tolera_como_ancestro_reparse() {
    use crate::plan::tolerated_ancestor;
    let meta = |tag: u32, is_dir: bool| sys::Meta {
        identity: FileIdentity {
            volume_serial: 1,
            file_id: 1,
        },
        links: 1,
        attrs: 0x400 | if is_dir { 0x10 } else { 0 },
        reparse_tag: tag,
        is_dir,
        delete_pending: false,
        alloc: 0,
        logical: 0,
        newest_filetime: 0,
    };
    // The two OneDrive tags measured at a sync root and its child folders.
    assert!(tolerated_ancestor(&meta(0x9000_701A, true)));
    assert!(tolerated_ancestor(&meta(0x9000_601A, true)));
    // Mount point / junction (0xA0000003), symlink (0xA000000C), WSL link (0xA000001D), third-party tag: not tolerated.
    for tag in [0xA000_0003u32, 0xA000_000C, 0xA000_001D, 0x0000_0123, 0] {
        assert!(!tolerated_ancestor(&meta(tag, true)), "{tag:#x}");
    }
    // A cloud-tagged FILE cannot be an ancestor.
    assert!(!tolerated_ancestor(&meta(0x9000_601A, false)));
}

/// M1: the ancestor check still rejects every reparse point by default and accepts one only if the caller allows it.
/// Under the approval policy (`tolerated_ancestor`), a reparse point with another tag is rejected; so is a junction.
#[test]
fn la_cadena_de_ancestros_solo_acepta_lo_que_la_politica_permite() {
    let mut sb = Sandbox::new();
    let tag = sb.path("Docs/etiquetada");
    sb.register_junction("Docs/etiquetada");
    if !crate::testutil::make_reparse_dir(&tag) {
        eprintln!("el volumen no admite reparse points de prueba en directorios: se omite");
        return;
    }
    let long = sb.long("Docs/etiquetada");
    // Rejected by default (it is a reparse point).
    assert!(matches!(
        sys::open_reparse_free_dir(&long),
        Err(sys::ChainError::Reparse { .. })
    ));
    // Rejected by the approval policy too (its tag is not a cloud tag).
    assert!(matches!(
        sys::open_chain_dir(&long, crate::plan::tolerated_ancestor),
        Err(sys::ChainError::Reparse { .. })
    ));
    // If the caller allows it, it is accepted (the tolerance mechanism is explicit).
    assert!(sys::open_chain_dir(&long, |_| true).is_ok());
    // A junction is rejected by the approval policy.
    sb.mkdir("Docs/real");
    sb.junction("Docs/enlace", "Docs/real");
    assert!(matches!(
        sys::open_chain_dir(&sb.long("Docs/enlace"), crate::plan::tolerated_ancestor),
        Err(sys::ChainError::Reparse { .. })
    ));
}

/// GM-B2: reject a junction ANCESTOR during approval (`ReparseAncestor`). Previously, `GetFinalPathNameByHandleW` silently
/// resolved the junction and the plan replaced the requested path with the resolved one: a DIFFERENT folder from the one
/// selected by the user was approved and sent to the Recycle Bin (`Real\...` instead of `Link\...`).
#[test]
fn un_ancestro_que_es_un_junction_se_rechaza_al_aprobar() {
    let mut sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write("Docs/real/dir/f.bin", 1_000);
    sb.write("Docs/real/suelto.bin", 1_000);
    sb.junction("Docs/enlace", "Docs/real");
    for rel in [
        "Docs/enlace/dir",
        "Docs/enlace/suelto.bin",
        "Docs/enlace/dir/f.bin",
    ] {
        let mut b = Basket::new();
        b.add_path(sb.long(rel), c.protection());
        let plan = approve(&c, &b);
        assert!(plan.items.is_empty(), "{rel}: {:?}", plan.items);
        assert_eq!(plan.rejected.len(), 1, "{rel}: {:?}", plan.rejected);
        assert_eq!(
            plan.rejected[0].reason,
            RejectReason::ReparseAncestor,
            "{rel}"
        );
    }
    // Control: approving the same folder through its real path succeeds and retains the requested path.
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/real/dir"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    assert_eq!(plan.items[0].path, sb.long("Docs/real/dir"));
}

/// GM-B2: paths supplied in 8.3 form (GitHub CI has `%TEMP%` this way) are NOT links and are approved. The check
/// traverses the actual path chain; it does not compare strings.
#[test]
fn una_ruta_en_forma_8_3_no_se_rechaza_como_ancestro_enlazado() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write(
        "Docs/Carpeta con nombre largo/archivo con nombre largo.bin",
        1_000,
    );
    let long = sb.long("Docs/Carpeta con nombre largo/archivo con nombre largo.bin");
    let short = crate::testutil::short_path(&long);
    let mut b = Basket::new();
    b.add_path(short.clone(), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{short:?}: {:?}", plan.rejected);
    // The plan stores the canonical long form.
    assert_eq!(plan.items[0].path, long);
}

/// GM-B2: reject a child replaced BETWEEN listing and opening (`Contents of X`): open it RELATIVE to the root handle and compare with the
/// folder and compares it with the listing's `FileId`. Previously, it reopened by path and captured the identity of the
/// is present now.
#[test]
fn un_hijo_cambiado_entre_el_listado_y_la_apertura_no_se_aprueba() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    sb.write("Docs/a.txt", 1_000);
    sb.write("Docs/b.txt", 1_000);
    // Control: without a race, both children are approved.
    let mut b = Basket::new();
    b.add_contents_of(sb.long("Docs"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
    // Race case: just before opening `b.txt`, replace it with another file (a different `FileId`).
    let swapped = Arc::new(AtomicBool::new(false));
    let target = sb.path("Docs/b.txt");
    let _hook = {
        let swapped = swapped.clone();
        crate::engine::test_hooks::install(sb.long("Docs"), move |p| {
            if p.ends_with("b.txt") && !swapped.swap(true, Ordering::SeqCst) {
                fs::remove_file(&target).unwrap();
                fs::write(&target, vec![9u8; 1_000]).unwrap();
            }
        })
    };
    let plan = approve(&c, &b);
    assert!(swapped.load(Ordering::SeqCst), "el gancho corrió");
    let names: Vec<String> = plan
        .items
        .iter()
        .map(|i| i.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["a.txt"], "{:?}", plan.rejected);
    assert!(
        plan.rejected.iter().any(|r| r.path.ends_with("b.txt")),
        "{:?}",
        plan.rejected
    );
}

/// S2 (audit): isolate and test the post-traversal `fresh_size` check. Call `fresh_size` directly (without the pre-send
/// check or receiver): if the first counted file grows during traversal, `fresh_size` itself returns "changed since
/// confirmation." Without the check it would return `Ok` (the observed fingerprint matches approval because it reflects
/// the pre-growth size). Control: no change returns `Ok`, and monitoring remains on the item.
#[test]
fn fresh_size_por_si_sola_ve_lo_que_crecio_durante_el_recorrido() {
    for crece in [false, true] {
        let sb = Sandbox::new();
        let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
        let c = cleaner_for(&sb).with_shell(fake).with_threads(1);
        sb.write("Docs/d/f1.bin", 1_000);
        sb.write("Docs/d/f2.bin", 1_000);
        let mut b = Basket::new();
        b.add_path(sb.long("Docs/d"), c.protection());
        let plan = approve(&c, &b);
        assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
        let it = &plan.items[0];
        let mut st = StatsSnapshot::default();
        let Prepared::Ready(mut p) = c.prepare_recycle(it, &mut st) else {
            panic!("debía prepararse");
        };
        let seen: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let _hook = crate::engine::test_hooks::install(sb.long("Docs/d"), {
            let seen = seen.clone();
            move |q| {
                if !crece || q.extension().is_none_or(|e| e != "bin") {
                    return;
                }
                let mut first = seen.lock().unwrap();
                match first.as_ref() {
                    None => *first = Some(q.to_path_buf()),
                    Some(counted) if counted != q => {
                        fs::write(counted, vec![1u8; 1 << 20]).unwrap();
                    }
                    _ => {}
                }
            }
        });
        let r = c.fresh_size(it, &mut p, &mut st, &cancel());
        if crece {
            let err = r.unwrap_err();
            assert_eq!(err.0, ItemStatus::Root(crate::engine::RootStatus::Changed));
            assert_eq!(err.1, Outcome::Changed);
            assert_eq!(st.changed, 1);
            assert!(
                p.watch.is_none(),
                "una medida que no vale no deja vigilancia"
            );
        } else {
            r.unwrap();
            assert!(p.watch.is_some());
        }
    }
}

/// S1 (audit): a folder that changed BEFORE the intent line is written must not leave a `recycle-begin` without delivery.
#[test]
fn una_carpeta_cambiada_antes_de_la_intencion_no_deja_recycle_begin() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/d/f1.bin", 1_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/d"), c.protection());
    let plan = approve(&c, &b);
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), |p| {
        if p.ends_with("d") {
            fs::write(p.join("f1.bin"), vec![1u8; 1 << 20]).unwrap();
        }
    });
    let (log, tmp) = log_for(&c);
    let rep = exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert!(fake.names().is_empty());
    log.flush();
    let text = fs::read_to_string(log.path()).unwrap();
    assert!(
        !text.contains("recycle-begin"),
        "no se anota intención de lo que no se va a entregar: {text}"
    );
    drop(log);
    if let Some(d) = tmp {
        let _ = fs::remove_dir_all(d);
    }
}

/// A3-M2 (Astra repeat): capacity 100,000; the Recycle Bin is empty when measurement starts and has 99,000 bytes used when
/// traversal ends. A 4,000-byte folder was sent using the stale (empty) reading. Occupancy is now also read AFTER
/// after traversal and use the HIGHER occupancy: `NoRoom`, without reaching the shell.
#[test]
fn una_papelera_que_se_llena_durante_la_medida_deja_el_directorio_sin_enviar() {
    for (lecturas, enviado) in [
        // Before: 0; after: 99,000. It does not fit (the later reading applies).
        (vec![0u64, 99_000], false),
        // Before: 88,000; after: 0. The larger earlier value applies (2,000 budget < 4,096).
        (vec![88_000, 0], false),
        // Control: both reads are empty; the item is sent.
        (vec![0, 0], true),
        // Control: medium occupancy in both reads; it fits in the remaining capacity (90,000 - 50,000).
        (vec![50_000, 50_000], true),
    ] {
        let sb = Sandbox::new();
        let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
        let flaky = Arc::new(FlakyRoom {
            inner: fake.clone(),
            calls: std::sync::atomic::AtomicUsize::new(0),
            used: lecturas.clone(),
        });
        let c = cleaner_for(&sb).with_shell(flaky.clone());
        let plan = plan_dir(&sb, &c, "d", 4_000);
        let rep = exec(&c, &plan, false, false, None).unwrap();
        if enviado {
            assert_eq!(rep.items[0].status, ItemStatus::Recycled, "{lecturas:?}");
            assert_eq!(fake.names(), vec![vec!["d"]]);
        } else {
            assert_eq!(
                rep.items[0].status,
                ItemStatus::NotRecycled(RecycleFail::NoRoom),
                "{lecturas:?}"
            );
            assert!(fake.names().is_empty(), "{lecturas:?}: no llegó a la shell");
            assert!(sb.path("Docs/d/f0.bin").exists());
            assert_eq!(rep.no_room, 1);
        }
        // Two reads per directory: before and after traversal.
        assert!(flaky.calls.load(Ordering::SeqCst) >= 2, "{lecturas:?}");
    }
}

/// A3-M2: `BinRoom::worst` takes the HIGHEST occupancy and LOWEST capacity, failing closed if either value is missing.
#[test]
fn la_peor_de_dos_lecturas_de_la_papelera() {
    let a = BinRoom {
        max: Some(100),
        used: Some(10),
    };
    let b = BinRoom {
        max: Some(80),
        used: Some(40),
    };
    assert_eq!(
        a.worst(b),
        BinRoom {
            max: Some(80),
            used: Some(40)
        }
    );
    assert_eq!(a.worst(b), b.worst(a));
    // If either occupancy is missing, the result is `None` and `take_op` returns `NoRoom`.
    let sin_uso = BinRoom {
        max: Some(100),
        used: None,
    };
    assert_eq!(a.worst(sin_uso).used, None);
    assert_eq!(take_op(&[1], a.worst(sin_uso)), Take::NoRoom);
    assert_eq!(a.worst(BinRoom::UNKNOWN), BinRoom::UNKNOWN);
}

/// A-B1: after remeasurement, recheck the budget against CURRENT occupancy. If the whole group no longer fits, return the
/// remaining items to the front of the queue (and send them in the next operation); if even the first item does not fit, send nothing.
#[test]
fn tras_volver_a_medir_se_vuelve_a_comprobar_el_presupuesto() {
    // Capacity 10,000 (budget 9,000). Occupancy reads: first = 0 (forms a group of 3), second = 8,790 (210 budget remains,
    // enough for two items of about 104); subsequent reads are 0.
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(10_000), 0, "nunca");
    let flaky = Arc::new(FlakyRoom {
        inner: fake.clone(),
        calls: std::sync::atomic::AtomicUsize::new(0),
        used: vec![0, 8_790, 0],
    });
    let c = cleaner_for(&sb).with_shell(flaky.clone());
    let plan = plan_de(
        &sb,
        &c,
        &[("a", 100, 104), ("b", 100, 104), ("c", 100, 104)],
    );
    let rep = exec(&c, &plan, false, false, None).unwrap();
    for it in &rep.items {
        assert_eq!(it.status, ItemStatus::Recycled, "{:?}", it.path);
    }
    assert_eq!(fake.names(), vec![vec!["a", "b"], vec!["c"]]);

    // If the second read leaves no room even for the first item (budget 50 < 104), it is not sent (`NoRoom`); the others
    // continue in the next operation.
    let sb2 = Sandbox::new();
    let fake2 = FakeShell::new(&sb2, Some(10_000), 0, "nunca");
    let flaky2 = Arc::new(FlakyRoom {
        inner: fake2.clone(),
        calls: std::sync::atomic::AtomicUsize::new(0),
        used: vec![0, 8_950, 0],
    });
    let c2 = cleaner_for(&sb2).with_shell(flaky2);
    let plan2 = plan_de(
        &sb2,
        &c2,
        &[("a", 100, 104), ("b", 100, 104), ("c", 100, 104)],
    );
    let rep2 = exec(&c2, &plan2, false, false, None).unwrap();
    let st = |n: &str| {
        rep2.items
            .iter()
            .find(|i| i.path.ends_with(n))
            .unwrap()
            .status
    };
    assert_eq!(st("a"), ItemStatus::NotRecycled(RecycleFail::NoRoom));
    assert_eq!(st("b"), ItemStatus::Recycled);
    assert_eq!(st("c"), ItemStatus::Recycled);
    assert_eq!(fake2.names(), vec![vec!["b", "c"]]);
    assert!(sb2.path("Docs/a").exists());
    assert_eq!(rep2.no_room, 1);
}

/// Open `dir` with `dwShareMode = 0`: while the handle is open, opening it for listing fails (`ERROR_SHARING_VIOLATION`). This lets us
/// contain a subfolder that cannot be listed without changing permissions (nothing to clean afterward).
fn lock_dir(dir: &Path) -> fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)
        .expect("abre la carpeta en exclusiva")
}

/// A-B2 (blocker): an INCOMPLETE measurement is invalid. A subfolder that cannot be listed during remeasurement leaves its
/// contents without counting it; previously, `remeasure` accepted `Done` and the folder was sent to the shell with an
/// incorrect size. Now it is not sent: "changed since confirmation." Without the lock (control), it is sent.
#[test]
fn una_subcarpeta_que_no_se_puede_listar_al_volver_a_medir_impide_el_envio() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/carpeta/f0.bin", 5_000);
    sb.write("Docs/carpeta/sub/dentro.bin", 5_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/carpeta"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    let lock = lock_dir(&sb.path("Docs/carpeta/sub"));
    let rep = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(
        rep.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    assert!(rep.items[0].removed.is_empty());
    assert!(fake.names().is_empty(), "no llegó a la shell");
    assert!(sb.path("Docs/carpeta/sub/dentro.bin").exists());
    assert!(rep.totals.changed >= 1);
    // Also in a dry run (which measures the same way as a real send).
    let dry = exec(&c, &plan, true, false, None).unwrap();
    assert_eq!(
        dry.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::Changed)
    );
    // Control: without the lock, the same plan is sent.
    drop(lock);
    let rep2 = exec(&c, &plan, false, false, None).unwrap();
    assert_eq!(rep2.items[0].status, ItemStatus::Recycled);
    assert_eq!(fake.names(), vec![vec!["carpeta"]]);
}

/// m-2: cancellation MIDWAY through a new measurement (not only cancellation already active at the start) invalidates the
/// measurement: `remeasure` returns `Cancelled` even if traversal finished with the root open and no counted errors.
#[test]
fn una_cancelacion_a_mitad_de_la_nueva_medida_no_es_una_medida() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake);
    for i in 0..6 {
        sb.write(&format!("Docs/carpeta/f{i}.bin"), 1_000);
    }
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/carpeta"), c.protection());
    let plan = approve(&c, &b);
    let it = &plan.items[0];
    let flag = cancel();
    let flag2 = flag.clone();
    let _hook = crate::engine::test_hooks::install(sb.long("Docs"), move |q| {
        if q.ends_with("f2.bin") {
            flag2.store(true, Ordering::SeqCst);
        }
    });
    assert!(matches!(c.remeasure(it, &flag), Remeasure::Cancelled));
    assert!(flag.load(Ordering::SeqCst));
}

/// m-4: reject a folder destined for the Recycle Bin if it already contains unreadable content DURING APPROVAL (a locked subfolder)
/// with its own reason ("could not be fully read"), rather than being approved with an underestimated size and ALWAYS
/// failing during execution as "changed since confirmation." Items permanently deleted by rules are unaffected.
#[test]
fn una_carpeta_que_no_se_puede_leer_entera_no_se_aprueba_para_la_papelera() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/carpeta/f0.bin", 5_000);
    sb.write("Docs/carpeta/sub/dentro.bin", 5_000);
    let basket = || {
        let mut b = Basket::new();
        b.add_path(sb.long("Docs/carpeta"), c.protection());
        b
    };
    let lock = lock_dir(&sb.path("Docs/carpeta/sub"));
    let plan = approve(&c, &basket());
    assert!(plan.items.is_empty(), "no se aprueba");
    assert_eq!(plan.rejected.len(), 1, "{:?}", plan.rejected);
    assert!(matches!(
        plan.rejected[0].reason,
        RejectReason::Incomplete(n) if n >= 1
    ));
    assert!(plan.rejected[0].path.ends_with("carpeta"));
    // Without the lock, it is approved and sent normally.
    drop(lock);
    let plan = approve(&c, &basket());
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    assert_eq!(
        exec(&c, &plan, false, false, None).unwrap().items[0].status,
        ItemStatus::Recycled
    );
}

/// R-4: `BinReport.changed` counts items whose veto is attributed to their OWNER (the folder's `$R`), not by
/// the vetoed path, which may be a child path with the real shell. Count an item with multiple vetoes once; do not count items outside
/// the list.
#[test]
fn los_cambiados_de_la_papelera_cuentan_el_dueno_del_veto_y_no_la_ruta() {
    let id = |n: &str| OsString::from(format!("C:\\$Recycle.Bin\\S-x\\{n}"));
    let (a, b, ajeno) = (id("$RAAAA"), id("$RBBBB"), id("$RZZZZ"));
    let wanted: HashSet<&OsString> = [&a, &b].into_iter().collect();
    let veto = |item: OsString, owner: Option<OsString>| recycle::Veto {
        item,
        owner,
        kind: recycle::VetoKind::IdentityMismatch,
    };
    let child = |parent: &OsString| {
        let mut p = parent.clone();
        p.push("\\sub\\hijo.txt");
        p
    };
    // A veto for a CHILD of `a` counts `a` (the child's path would not appear in the list).
    assert_eq!(
        bin_changed(0, &[veto(child(&a), Some(a.clone()))], &wanted),
        1
    );
    // Two vetoes for one item and one for another: count 2, plus enumeration mismatches.
    let vetoes = [
        veto(child(&a), Some(a.clone())),
        veto(a.clone(), Some(a.clone())),
        veto(b.clone(), None),
    ];
    assert_eq!(bin_changed(0, &vetoes, &wanted), 2);
    assert_eq!(bin_changed(3, &vetoes, &wanted), 5);
    // A veto for an item outside the list does not count.
    assert_eq!(bin_changed(0, &[veto(ajeno.clone(), None)], &wanted), 0);
    assert_eq!(bin_changed(1, &[], &wanted), 1);
}

/// Tests do not depend on whether their process is elevated (some CI runners are): a test `Cleaner` is not elevated by
/// default, and elevation is injected only in tests for E1.
#[test]
fn un_cleaner_de_pruebas_no_esta_elevado_por_defecto() {
    let sb = Sandbox::new();
    assert!(!cleaner_for(&sb).is_elevated());
    assert!(!Cleaner::new(Roots::default()).is_elevated());
    assert!(
        Cleaner::new(Roots::default())
            .with_elevation(Arc::new(FakeElevation(true)))
            .is_elevated()
    );
}

/// E1: inject elevation to test both branches (CI cannot elevate itself).
struct FakeElevation(bool);

impl Elevation for FakeElevation {
    fn is_elevated(&self) -> bool {
        self.0
    }
}

/// E1: do not clean while ELEVATED. Real cleaning (permanent deletion or Recycle Bin) returns `ExecError::Elevated`, touching nothing and
/// logging; analysis (approval) and dry runs remain allowed. Without elevation, the same plan executes.
#[test]
fn elevado_no_se_limpia_pero_si_se_analiza_y_se_simula() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb)
        .with_shell(fake.clone())
        .with_elevation(Arc::new(FakeElevation(true)));
    assert!(c.is_elevated());
    sb.write_aged("Temp/viejo.tmp", 10_000, 240);
    sb.write("Docs/b.txt", 1_000);
    let mut b = Basket::new();
    b.set_category(CategoryId::UserTemp, true);
    b.add_path(sb.long("Docs/b.txt"), c.protection());
    // Analysis (approval) is allowed while elevated.
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
    // Dry run is allowed too.
    let dry = exec(&c, &plan, true, false, None).unwrap();
    assert!(dry.dry_run);
    // Real cleaning is not: typed error, nothing touched, and the execution log discarded.
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    let log_path = log.path().to_path_buf();
    let r = c.execute(
        &plan,
        &ExecOptions {
            log: Some(log),
            ..ExecOptions::default()
        },
        &Progress::new(),
    );
    assert_eq!(r.err(), Some(ExecError::Elevated));
    assert!(sb.path("Temp/viejo.tmp").exists() && sb.path("Docs/b.txt").exists());
    assert!(fake.names().is_empty(), "la shell no vio nada");
    assert!(
        !log_path.exists(),
        "una ejecución rechazada no deja registro"
    );
    // Also with the test helper (which adds its own log).
    assert_eq!(
        exec(&c, &plan, false, false, None).err(),
        Some(ExecError::Elevated)
    );
    assert!(sb.path("Docs/b.txt").exists());
    // Without elevation, the same plan executes.
    let c2 = cleaner_for(&sb)
        .with_shell(fake.clone())
        .with_elevation(Arc::new(FakeElevation(false)));
    assert!(!c2.is_elevated());
    let rep = exec(&c2, &plan, false, false, None).unwrap();
    assert!(
        rep.items
            .iter()
            .all(|i| matches!(i.status, ItemStatus::Done | ItemStatus::Recycled)),
        "{:?}",
        rep.items
    );
    assert!(!sb.path("Temp/viejo.tmp").exists() && !sb.path("Docs/b.txt").exists());
}

/// E1: do not purge the Recycle Bin while elevated, either through `execute` (full rejection) or direct calls to `empty_bin`
/// (second line of defense): nothing is purged, and the failure is recorded.
#[test]
fn elevado_no_se_vacia_la_papelera() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb).with_elevation(Arc::new(FakeElevation(true)));
    let item = BinItem {
        id: OsString::from("no-existe-en-ninguna-parte"),
        name: OsString::from("x.txt"),
        original_parent: sb.long("origen"),
        time_deleted: 0,
        size: 1,
        data_id: None,
        meta_id: None,
        no_identity: false,
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let rep = c.empty_bin(std::slice::from_ref(&item), 1, false, &cancel, None, None);
    assert_eq!((rep.requested, rep.purged, rep.still_there), (1, 0, 1));
    assert!(rep.failed, "elevado: no se pudo vaciar");
    // Dry run still counts what would be purged.
    let dry = c.empty_bin(std::slice::from_ref(&item), 1, true, &cancel, None, None);
    assert_eq!((dry.purged, dry.failed), (1, false));
    // `execute` with a plan that ONLY purges the Recycle Bin is also rejected entirely.
    let mut plan = Plan::default();
    plan.bin = Some(crate::plan::BinPlan {
        items: vec![item],
        bytes_hint: 1,
    });
    assert_eq!(
        exec(&c, &plan, false, false, None).err(),
        Some(ExecError::Elevated)
    );
}

/// E1: a REAL token query returns `false` in a normal process (CI and this machine are not elevated); if it were elevated,
/// the test is skipped with a message (nothing can be guaranteed). It is never `None` in a normal process.
#[test]
fn el_proceso_de_pruebas_no_esta_elevado_segun_el_token() {
    let state = crate::sys::process_elevation();
    assert!(state.is_some(), "el token del proceso se puede consultar");
    if state == Some(true) {
        eprintln!("el proceso de pruebas está ELEVADO: la prueba se salta");
        return;
    }
    assert_eq!(state, Some(false));
    // Fail closed: inability to query the token is treated as elevated.
    assert!(elevated_from(None));
    assert!(elevated_from(Some(true)) && !elevated_from(Some(false)));
    assert!(!RealElevation.is_elevated());
    // `Cleaner::new` uses "not elevated" in tests (see `NotElevated`); injecting the real query also returns `false`.
    assert!(!Cleaner::new(Roots::default()).is_elevated());
    assert!(
        !Cleaner::new(Roots::default())
            .with_elevation(Arc::new(RealElevation))
            .is_elevated()
    );
}

/// A-B2 + A2-m1: an incomplete measurement proves NOTHING for R6 judgment (`dir_check`: neither intact nor changed, `Unknown`), and
/// `remeasure` distinguishes it from a
/// is incomplete for any of these reasons: an unlistable subfolder, a child disappearing between listing and opening,
/// opening, or cancellation midway through traversal.
#[test]
fn una_medida_incompleta_nunca_demuestra_que_nada_cambio() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake);
    sb.write("Docs/carpeta/f0.bin", 5_000);
    sb.write("Docs/carpeta/f1.bin", 5_000);
    sb.write("Docs/carpeta/sub/dentro.bin", 5_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/carpeta"), c.protection());
    let plan = approve(&c, &b);
    let it = &plan.items[0];
    let mut st = StatsSnapshot::default();
    let Prepared::Ready(mut p) = c.prepare_recycle(it, &mut st) else {
        panic!("debía prepararse");
    };
    c.fresh_size(it, &mut p, &mut st, &cancel()).unwrap();
    assert!(p.snapshot.is_some());
    assert_eq!(
        c.dir_check(it, &p, &cancel()),
        DirCheck::Intact,
        "sin cambios: intacto"
    );
    // (1) A subfolder cannot be listed.
    {
        let _lock = lock_dir(&sb.path("Docs/carpeta/sub"));
        assert!(matches!(c.remeasure(it, &cancel()), Remeasure::Incomplete));
        assert_eq!(
            c.dir_check(it, &p, &cancel()),
            DirCheck::Unknown,
            "no se pudo medir entero: no se afirma ni intacto ni perdido"
        );
    }
    assert_eq!(
        c.dir_check(it, &p, &cancel()),
        DirCheck::Intact,
        "soltada, vuelve a valer"
    );
    // (2) A child disappears just before opening (after listing).
    {
        let victim = sb.path("Docs/carpeta/f1.bin");
        let _hook = crate::engine::test_hooks::install(sb.long("Docs"), move |q| {
            if q.ends_with("f1.bin") {
                let _ = fs::remove_file(&victim);
            }
        });
        assert!(matches!(c.remeasure(it, &cancel()), Remeasure::Incomplete));
    }
    // (3) Cancelled midway: not accepted as a measurement.
    let cancelled = Arc::new(AtomicBool::new(true));
    assert!(matches!(c.remeasure(it, &cancelled), Remeasure::Cancelled));
    assert_eq!(c.dir_check(it, &p, &cancelled), DirCheck::Unknown);
    // (4) A COMPLETE measurement that differs: that is a change (a child was lost).
    fs::remove_file(sb.path("Docs/carpeta/f1.bin")).ok();
    assert_eq!(c.dir_check(it, &p, &cancel()), DirCheck::Changed);
    // (5) Without a previous measurement for comparison, make no claim either.
    p.snapshot = None;
    assert_eq!(c.dir_check(it, &p, &cancel()), DirCheck::Unknown);
}

/// A2-m1 (Astra repeat): the fake shell returns access denied WITHOUT touching anything, and an exclusive lock prevents
/// rereading a subfolder during the content check. Previously, "could not verify" was counted as confirmed loss
/// (`DeletedUnexpectedly`, "permanently deleted"); now it is `Unverified` ("could not determine what happened"): no loss
/// is claimed, and sending still stops (later items remain untouched).
#[test]
fn una_comprobacion_que_no_se_pudo_hacer_no_se_cuenta_como_perdida_y_detiene_el_envio() {
    let sb = Sandbox::new();
    let fake = FakeShell::new(&sb, Some(100_000), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    sb.write("Docs/deniega-d1/f0.bin", 1_000);
    sb.write("Docs/deniega-d1/sub/dentro.bin", 1_000);
    sb.write("Docs/d2/y.bin", 1_000);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/deniega-d1"), c.protection());
    b.add_path(sb.long("Docs/d2"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
    // The lock arrives RIGHT before sending the folder to the shell: after measuring it completely (otherwise `fresh_size`
    // would already reject it) and before the post-send check.
    let sub = sb.path("Docs/deniega-d1/sub");
    let lock: Arc<Mutex<Option<fs::File>>> = Arc::new(Mutex::new(None));
    let l2 = lock.clone();
    let _hook = crate::engine::test_hooks::install_before_shell(sb.long("Docs"), move |p| {
        if p.ends_with("deniega-d1") {
            *l2.lock().unwrap() = Some(lock_dir(&sub));
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
    assert_eq!(
        st("deniega-d1"),
        ItemStatus::Unverified,
        "no se afirma la pérdida"
    );
    assert_eq!(rep.deleted_unexpectedly, 0);
    assert_eq!(rep.unverified, 1);
    assert!(rep.recycle_stopped, "pero el envío se detiene");
    assert_eq!(rep.stop_reason, Some(StopReason::Unverified));
    assert_eq!(st("d2"), ItemStatus::NotRecycled(RecycleFail::Stopped));
    assert_eq!(
        fake.names(),
        vec![vec!["deniega-d1"]],
        "d2 no llegó a la shell"
    );
    assert!(sb.path("Docs/d2/y.bin").exists());
    assert_eq!(sb.count_entries("Docs/deniega-d1"), 2, "sigue entera");
    drop(lock.lock().unwrap().take());
}

/// A shell or COM send error is recorded in the local log (with its HRESULT) and in the report (for the summary);
/// items remain untouched.
#[test]
fn un_error_de_la_shell_consta_en_el_registro_y_en_el_informe() {
    let sb = Sandbox::new();
    let fake = FakeShell::failing(&sb, 0x8000_4005u32 as i32);
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let plan = plan_de(&sb, &c, &[("a.txt", 100, 100), ("b.txt", 100, 100)]);
    let log = Arc::new(LogWriter::create(&sb.path("Tesela"), false).unwrap());
    let rep = exec(&c, &plan, false, false, Some(log.clone())).unwrap();
    assert_eq!(rep.shell_error, Some(0x8000_4005u32 as i32));
    // No events: nothing moved, and items remain in place as unprocessed.
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
    // A send without errors leaves `shell_error` unset.
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
    // Without known capacity or occupancy, nothing is sent (B2).
    assert_eq!(take_op(&[10, 10], BinRoom::UNKNOWN), Take::NoRoom);
    let sin_max = BinRoom {
        max: None,
        used: Some(0),
    };
    assert_eq!(take_op(&[1], sin_max), Take::NoRoom);
    // Group all small items while they fit in the budget (90% of 10,000 minus current occupancy).
    assert_eq!(take_op(&[300, 300, 300], room(10_000, 0)), Take::Send(3));
    assert_eq!(
        take_op(&[300, 300, 300], room(10_000, 8_400)),
        Take::Send(2)
    );
    // An item over 10% goes alone, whether it is first or would otherwise split the group.
    assert_eq!(take_op(&[1_500, 10], room(10_000, 0)), Take::Send(1));
    assert_eq!(take_op(&[100, 1_500, 100], room(10_000, 0)), Take::Send(1));
    // If the first item does not fit in the remaining space, it is not sent; with exactly enough capacity (90%), it is.
    assert_eq!(take_op(&[600, 10], room(10_000, 8_700)), Take::NoRoom);
    assert_eq!(take_op(&[9_000], room(10_000, 0)), Take::Send(1));
    assert_eq!(take_op(&[9_001], room(10_000, 0)), Take::NoRoom);
    // Unknown occupancy means nothing is sent (previously, an item was sent alone if it fit in an empty Recycle Bin).
    let sin_uso = BinRoom {
        max: Some(10_000),
        used: None,
    };
    assert_eq!(take_op(&[800, 800], sin_uso), Take::NoRoom);
    assert_eq!(take_op(&[1], sin_uso), Take::NoRoom);
    assert_eq!(take_op(&[9_500], sin_uso), Take::NoRoom);
    // An empty list sends nothing.
    assert_eq!(take_op(&[], BinRoom::UNKNOWN), Take::Send(0));
}

/// GM-m6: approval fit (`fit_decide`) and execution batching (`take_op`) use the SAME 90% and 10% calculations
/// (`bin_cap`, `bin_big`). Previously one used `bytes * 100 > max * 90` and the other `max / 10 * 9`: for capacities not
/// divisible by 10, they disagreed at the boundary, so an item could "fit" during approval and get `NoRoom` during
/// execution (or vice versa). Test the boundary across many capacity values, including a huge one without overflow.
#[test]
fn el_encaje_y_el_reparto_usan_la_misma_cuenta_del_90_por_ciento() {
    use crate::plan::{BinFacts, bin_big, bin_cap, fit_decide};
    use crate::volume::BinLimit;
    for max in [
        10u64,
        11,
        19,
        99,
        101,
        999,
        10_001,
        1_000_003,
        (1 << 40) + 7,
        u64::MAX,
    ] {
        let cap = bin_cap(max);
        assert_eq!(cap as u128, (max as u128) * 90 / 100, "{max}");
        for bytes in [cap.saturating_sub(1), cap, cap.saturating_add(1)] {
            let facts = BinFacts {
                path_len: 40,
                volume_known: true,
                policy_no_recycle: false,
                limit: BinLimit::Max(max),
            };
            let fits = fit_decide(bytes, &facts) == Fit::Fits;
            let room = BinRoom {
                max: Some(max),
                used: Some(0),
            };
            let sent = take_op(&[bytes], room) != Take::NoRoom;
            assert_eq!(fits, sent, "max {max} bytes {bytes}");
            assert_eq!(fits, bytes <= cap, "max {max} bytes {bytes}");
        }
        // "Large" items (over 10%) go alone; smaller ones are grouped if they fit.
        let big = bin_big(max);
        assert_eq!(big as u128, (max as u128) * 10 / 100, "{max}");
        if big > 0 && big < u64::MAX / 4 {
            let room = BinRoom {
                max: Some(max),
                used: Some(0),
            };
            assert_eq!(take_op(&[big + 1, 1], room), Take::Send(1), "{max}");
            assert_eq!(take_op(&[big, 1], room), Take::Send(2), "{max}");
        }
    }
}

/// GM-M3: a folder containing single-link files AND multiply linked files counts both sets (they are disjoint)
/// against the Recycle Bin. Using the larger of the two (previous behavior), a simulated Recycle Bin whose 90% threshold
/// fell between the two calculations accepted the folder, and the shell would have counted and deleted it.
#[test]
fn una_carpeta_con_enlaces_duros_cuenta_bytes_y_compartidos_para_encajar() {
    let sb = Sandbox::new();
    let big = FakeShell::new(&sb, Some(1 << 40), 0, "nunca");
    let c = cleaner_for(&sb).with_shell(big);
    sb.write("Docs/hl/a.bin", 10_000);
    sb.write("Docs/hl/b.bin", 10_000);
    // `b.bin` has another name outside the folder, so its space is in the "shared" class.
    sb.hard_link("Docs/hl/b.bin", "Docs/otro-nombre.bin");
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/hl"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    let est = plan.items[0].estimate;
    assert!(est.bytes > 0 && est.shared_bytes > 0, "{est:?}");
    let old = est.bytes.max(est.logical).max(est.shared_bytes);
    let new = est.recycle_charge();
    assert_eq!(new, (est.bytes + est.shared_bytes).max(est.logical));
    assert!(new > old, "la suma supera al máximo: {est:?}");
    // Capacity whose 90% threshold falls between the two calculations: it fit before, but correctly does not now.
    let max = (old + new) / 2 * 100 / 90 + 1;
    assert!(old <= crate::plan::bin_cap(max) && crate::plan::bin_cap(max) < new);
    let tight = FakeShell::new(&sb, Some(max), 0, "nunca");
    let c2 = cleaner_for(&sb).with_shell(tight.clone());
    let plan2 = approve(&c2, &b);
    assert_eq!(
        plan2.items[0].fit,
        Fit::Unfit(UnfitReason::TooBig),
        "{est:?} max {max}"
    );
    assert!(tight.names().is_empty());
}

/// GM-M3 (end to end, with the REAL fit check): a folder with a descendant path longer than 258 characters is not
/// presented to the Recycle Bin even when the folder's own path is short. Names with supplementary-plane characters take
/// more bytes than UTF-16 code units: count code units (previously `as_os_str().len()` counted bytes).
#[test]
fn un_descendiente_de_ruta_demasiado_larga_deja_la_carpeta_sin_papelera() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb); // la shell REAL: el motivo de la ruta larga se decide antes de mirar el registro.
    let seg = "x".repeat(60);
    let rel = format!("Docs/larga/{seg}/{seg}/{seg}/{seg}/f.bin");
    sb.write(&rel, 10);
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/larga"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    let it = &plan.items[0];
    // The folder itself is shorter than its deepest descendant, which has four 60-character segments and separators, regardless of the
    // `%TEMP%` length: the DESCENDANT, not the approved folder, exceeds 258 characters.
    assert!(
        it.estimate.max_path as usize >= crate::plan::path_units(&it.path) + 4 * 61,
        "{:?}",
        it.estimate
    );
    assert!(it.estimate.max_path > 258, "{:?}", it.estimate);
    assert_eq!(it.fit, Fit::Unfit(UnfitReason::PathTooLong), "{it:?}");
    // `path_units` counts UTF-16 code units, not bytes.
    let p = Path::new("C:\\😀é");
    assert_eq!(crate::plan::path_units(p), 6);
    assert_eq!(p.as_os_str().len(), 9);
}

/// M-C with the REAL shell, safely: a sparse file with thousands of GiB of logical size (occupying no disk space) between
/// two small files. Actual size (logical size) overrides the estimate; because it exceeds approval, it is not sent
/// ("changed since confirmation," A-B1; previously `NoRoom`, both leaving it untouched). The small files reach the Recycle
/// Bin in separate operations. The real shell never sees the huge file and cannot delete it or evict unrelated items.
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
    // Capacity must be known; otherwise batching cannot be tested (each item would go alone).
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
    // With an honest plan, the huge item is already "too large" (amber group). FORGE its estimate so the plan accepts it:
    // this simulates an underestimated size, which the actual size must correct.
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
        ItemStatus::Root(crate::engine::RootStatus::Changed)
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

/// A test `Pending`: the guard is open on `held_path` (its final path determines whether it is "in the
/// Recycle Bin"); `path` is the approved name.
fn pending(identity: FileIdentity, path: &Path, held_path: &Path) -> Pending {
    Pending {
        path: path.to_path_buf(),
        identity,
        links: 1,
        held: Arc::new(sys::open_path_for_rename(held_path).unwrap()),
        size: 0,
        last: None,
        snapshot: None,
        disk: 0,
        logical: 0,
        watch: None,
    }
}

fn ident(n: u128) -> FileIdentity {
    FileIdentity {
        volume_serial: 7,
        file_id: n,
    }
}

/// Per-item judgment comes from the shell result: identity same/different, deleted, error, or no notification. And
/// `Recycled` also requires that the approved name no longer exists and that the guarded object is in a Recycle Bin.
#[test]
fn el_juicio_distingue_enviado_otro_objeto_eliminado_error_y_sin_aviso() {
    use crate::recycle::{BinState, DeleteEvent};
    let sb = Sandbox::new();
    let bin_file = sb.write("bin/R1.txt", 10);
    let shell = FakeShell::new(&sb, None, 0, "nunca");
    let shell: &dyn RecycleShell = shell.as_ref();
    // `bin/` serves as the Recycle Bin; the approved name (relative and nonexistent) is "gone."
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
    // Same identity, approved name is gone, and guarded object is in the Recycle Bin -> sent.
    let p = pending(id, gone, &bin_file);
    let e = ev(0, BinState::InBin(bin_file.clone()));
    assert_eq!(
        judge(&p, Some(&e), shell, false, false).0,
        ItemStatus::Recycled
    );
    // ...but not if the approved name STILL exists (another name for the same object was sent to the Recycle Bin).
    let p_name = pending(id, &bin_file, &bin_file);
    assert_eq!(
        judge(&p_name, Some(&e), shell, false, false).0,
        ItemStatus::RecycledOther
    );
    // ...nor if the guarded object is NOT in the Recycle Bin (it remains in place under another name).
    let fuera = sb.write("Docs/vivo.txt", 10);
    let p_fuera = pending(id, gone, &fuera);
    assert_eq!(
        judge(&p_fuera, Some(&e), shell, false, false).0,
        ItemStatus::RecycledOther
    );
    // ...nor if the object's link count changed since verification (another name was added).
    let mut p_links = pending(id, gone, &bin_file);
    p_links.links = 3;
    assert_eq!(
        judge(&p_links, Some(&e), shell, false, false).0,
        ItemStatus::RecycledOther
    );
    // Different identity -> a different object from the approved one was moved to the Recycle Bin.
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
    // The shell permanently deleted it (`psiNewlyCreated` is null), and the name is gone.
    assert_eq!(
        judge(&p, Some(&ev(0, BinState::Deleted)), shell, false, false).0,
        ItemStatus::DeletedUnexpectedly
    );
    // H2: null with success, but the item STILL remains in place: the shell touched nothing (do not report deletion).
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
    // No path was given, or it cannot be opened -> unverified.
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
    // Errors: in use, access denied, and other HRESULTs. A-M1: a failure means "not sent" only if the approved object is
    // still intact in place (it is here: the guard and name refer to the same file).
    let fallo = pending(id, &bin_file, &bin_file);
    let hr_win32 = |c: u32| (0x8007_0000u32 | c) as i32;
    assert_eq!(
        judge(
            &fallo,
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
            &fallo,
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
            &fallo,
            Some(&ev(0x8000_4005u32 as i32, BinState::Deleted)),
            shell,
            false,
            false
        )
        .0,
        ItemStatus::NotRecycled(RecycleFail::Shell(0x8000_4005u32 as i32))
    );
    // Shell copy-engine codes: access denied (measured with a folder containing an open item) and sharing violation
    // sharing violation.
    assert_eq!(
        judge(
            &fallo,
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
            &fallo,
            Some(&ev(0x8027_0027u32 as i32, BinState::Deleted)),
            shell,
            false,
            false
        )
        .0,
        ItemStatus::NotRecycled(RecycleFail::InUse)
    );
    // A-M1: a failure when the object is ALREADY deleted (guard pending deletion) does not mean "not sent"; it is a deletion.
    // If the object's location cannot be determined, make no claim.
    let borrado_con_fallo = {
        let f = sb.write("Docs/borrado-con-fallo.bin", 100);
        let fid = sys::meta(&sys::open_for_query(&f).unwrap())
            .unwrap()
            .identity;
        let pf = pending(fid, &f, &f);
        sys::delete_legacy_for_test(&f).unwrap();
        pf
    };
    assert_eq!(
        judge(
            &borrado_con_fallo,
            Some(&ev(hr_win32(5), BinState::Deleted)),
            shell,
            false,
            false
        )
        .0,
        ItemStatus::DeletedUnexpectedly
    );
    assert_eq!(
        judge(
            &borrado_con_fallo,
            Some(&ev(0, BinState::Deleted)),
            shell,
            false,
            false
        )
        .0,
        ItemStatus::DeletedUnexpectedly
    );
    assert_eq!(
        judge(&borrado_con_fallo, None, shell, false, true).0,
        ItemStatus::DeletedUnexpectedly,
        "sin aviso pero la guarda dice que se eliminó"
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
        ItemStatus::Unverified,
        "el nombre aprobado no existe y la guarda no está pendiente: no se puede afirmar nada"
    );
    // No event: still in place -> unprocessed (or aborted if the shell reported abort); gone -> unverified.
    let here = pending(id, &bin_file, &bin_file);
    assert_eq!(
        judge(&here, None, shell, false, false).0,
        ItemStatus::NotRecycled(RecycleFail::NotProcessed)
    );
    assert_eq!(
        judge(&here, None, shell, true, false).0,
        ItemStatus::NotRecycled(RecycleFail::Aborted)
    );
    // The receiver stopped the batch (M2): no event and still in place means stopped by the safety cutoff.
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
    sb.write_aged("Temp/viejo.tmp", 20_000, 240);
    sb.write_aged("Temp/dir-entero/a.tmp", 10_000, 240);
    sb.set_age(&sb.path("Temp/dir-entero"), 240);
    // An old folder with one old and one new file remains, and only shrinks.
    sb.write_aged("Temp/dir-parcial/viejo.tmp", 30_000, 240);
    sb.write("Temp/dir-parcial/nuevo.tmp", 5_000);
    sb.set_age(&sb.path("Temp/dir-parcial"), 240);
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
    // A dry run also records nothing (nothing disappeared).
    let sb2 = Sandbox::new();
    let c2 = cleaner_for(&sb2);
    sb2.write_aged("Temp/x.tmp", 1_000, 240);
    let mut b2 = Basket::new();
    b2.set_category(CategoryId::UserTemp, true);
    let rep2 = exec(&c2, &approve(&c2, &b2), true, false, None).unwrap();
    assert!(rep2.items[0].removed.is_empty());
    assert!(sb2.path("Temp/x.tmp").exists());
}

// ---- Real Recycle Bin flow with executable DLL hardening --------------------------------------------

/// CHILD PROCESS for `con_la_shell_real_el_flujo_funciona_con_el_endurecimiento_de_dlls_activo` (runs only when its environment variable is set;
/// otherwise, this ignored test does nothing). With hardening ACTIVE before touching COM, it:
/// send a file and a folder with children (owned, UUID in the name) through the production path
/// (`Cleaner::execute`: guard, receiver, identity verification), check `Recycled`, then purge them through the real purge
/// path (scoped to owned items) and verify they are gone. Any failure panics: the exit code
/// is nonzero.
#[test]
#[ignore = "proceso hijo de la prueba del endurecimiento de DLL"]
fn hijo_endurecimiento_dll() {
    let Ok(uuid) = std::env::var("TESELA_HARDEN_CHILD_UUID") else {
        return;
    };
    // 1. Enable hardening BEFORE touching COM.
    assert!(
        crate::testutil::restrict_dll_search_to_system32(),
        "SetDefaultDllDirectories falló"
    );
    eprintln!("ENDURECIDO: SetDefaultDllDirectories(SYSTEM32) activo");

    let sb = Sandbox::with_uuid(&uuid);
    let c = cleaner_for(&sb);
    sb.write("Docs/archivo.txt", 5_000);
    sb.write("Docs/carpeta/a.txt", 3_000);
    sb.write("Docs/carpeta/sub/b.txt", 3_000);
    // 2. Send to the Recycle Bin through the production path.
    let mut b = Basket::new();
    b.add_path(sb.long("Docs/archivo.txt"), c.protection());
    b.add_path(sb.long("Docs/carpeta"), c.protection());
    let plan = approve(&c, &b);
    assert_eq!(plan.items.len(), 2, "{:?}", plan.rejected);
    let rep = exec(&c, &plan, false, false, None).expect("la ejecución real");
    for it in &rep.items {
        eprintln!(
            "ENDURECIDO: {:?} -> {:?}",
            it.path.file_name().unwrap(),
            it.status
        );
        assert_eq!(it.status, ItemStatus::Recycled, "{:?}", it.path);
    }
    assert_eq!(
        (
            rep.deleted_unexpectedly,
            rep.recycled_other,
            rep.unverified,
            rep.not_recycled
        ),
        (0, 0, 0, 0)
    );
    assert!(rep.shell_error.is_none(), "{:?}", rep.shell_error);
    assert!(!sb.path("Docs/archivo.txt").exists() && !sb.path("Docs/carpeta").exists());
    let mine = || -> Vec<BinItem> {
        recycle::list_bin()
            .expect("lista la Papelera")
            .into_iter()
            .filter(|i| is_mine(i, &sb.uuid()))
            .collect()
    };
    assert_eq!(mine().len(), 2, "llegaron a la Papelera: {:?}", mine());
    // 3. Purge through the real path (owned items only).
    let mut v = Basket::new();
    v.set_empty_bin(true);
    let plan = c.approve(&v, &ApproveOptions::default(), &cancel());
    assert_eq!(plan.bin.as_ref().expect("plan de vaciar").items.len(), 2);
    let rep = exec(&c, &plan, false, false, None).expect("vaciar de verdad");
    let bin = rep.bin.expect("informe de la Papelera");
    eprintln!("ENDURECIDO: vaciar -> {bin:?}");
    assert_eq!((bin.requested, bin.purged, bin.still_there), (2, 2, 0));
    assert_eq!((bin.changed, bin.replaced, bin.unverifiable), (0, 0, 0));
    assert!(!bin.failed);
    assert!(mine().is_empty(), "ya no queda nada propio en la Papelera");
    eprintln!("ENDURECIDO: todo coincide");
}

/// The REAL Recycle Bin flow (COM, `IFileOperation`, receiver, guard, and purge receiver) works with
/// `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` active, as `tesela.exe` does at startup: launch a
/// child process (following the `FOF_WANTNUKEWARNING` test pattern) activates it before touching COM and runs the flow with
/// owned items. The parent checks the exit code (with a timeout) and verifies that the user's Recycle Bin is unchanged; its
/// `BinGuard` cleans up the child process's items if anything fails.
#[test]
fn con_la_shell_real_el_flujo_funciona_con_el_endurecimiento_de_dlls_activo() {
    use std::io::Read;
    let _lock = bin_lock();
    if !bin_usable() {
        return;
    }
    let before = ids();
    let uuid = uuid::Uuid::new_v4().to_string();
    let _guard = BinGuard { uuid: uuid.clone() };
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "exec::tests::hijo_endurecimiento_dll",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("TESELA_HARDEN_CHILD_UUID", &uuid)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("lanza el proceso hijo de la prueba");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break Some(st);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let (mut out, mut err) = (String::new(), String::new());
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    for l in err
        .lines()
        .chain(out.lines())
        .filter(|l| l.starts_with("ENDURECIDO"))
    {
        eprintln!("hijo: {l}");
    }
    let st = status.expect("el hijo no terminó en 90 s (se colgó)");
    assert!(st.success(), "el hijo falló ({st:?}):\n{err}\n{out}");
    assert_eq!(ids(), before, "la Papelera del usuario es la de antes");
}

/// Synthetic mail data is measured through the real traversal, with a fake shell and a dry run only.
#[test]
fn mail_measurement_counts_files_without_changing_recycle_fingerprint() {
    let sb = Sandbox::new();
    sb.write("Mail/a.PST", 1234);
    sb.write("Mail/b.ost", 2345);
    sb.write("Mail/sub/c.NST", 3456);
    sb.write("Mail/sub/not.pst.bak", 10);
    let fake = FakeShell::new(&sb, Some(1_000_000_000), 0, "never");
    let c = cleaner_for(&sb).with_shell(fake.clone());
    let mut basket = Basket::new();
    basket.add_path(sb.long("Mail"), c.protection());
    let progress = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let plan = c.approve(
        &basket,
        &ApproveOptions {
            progress: Some(progress.clone()),
            ..ApproveOptions::default()
        },
        &cancel(),
    );
    assert_eq!(plan.items.len(), 1);
    let e = plan.items[0].estimate;
    assert_eq!((e.mail_local_files, e.mail_cache_files), (1, 2));
    let alloc = |rel: &str| {
        sys::meta(&sys::open_for_query(&sb.long(rel)).unwrap())
            .unwrap()
            .alloc
    };
    assert_eq!(
        plan.mail_totals(),
        (
            (1, alloc("Mail/a.PST")),
            (2, alloc("Mail/b.ost") + alloc("Mail/sub/c.NST"))
        )
    );
    assert!(progress.load(std::sync::atomic::Ordering::Relaxed) >= 4);
    let without_mail = StatsSnapshot {
        mail_local_files: 0,
        mail_local_bytes: 0,
        mail_cache_files: 0,
        mail_cache_bytes: 0,
        ..e
    };
    assert_eq!(snap_of(&e), snap_of(&without_mail));
    exec(&c, &plan, true, false, None).unwrap();
    assert!(sb.path("Mail/a.PST").exists());
    assert!(fake.names().is_empty());
    let mut single = Basket::new();
    single.add_path(sb.long("Mail/a.PST"), c.protection());
    let single = approve(&c, &single);
    assert_eq!(single.mail_totals(), ((1, alloc("Mail/a.PST")), (0, 0)));
}

#[test]
fn newly_added_pst_requires_consent_in_cache_contents() {
    let sb = Sandbox::new();
    let cache = "Local/Google/Chrome/User Data/Default/Cache";
    sb.write(&format!("{cache}/plain.bin"), 32);
    let fake = FakeShell::new(&sb, Some(1 << 40), 0, "never");
    let c = cleaner_for(&sb).with_shell(fake);
    let mut basket = Basket::new();
    basket.set_category(CategoryId::ChromeCache, true);
    let plan = approve(&c, &basket);
    assert_eq!(plan.mail_totals().0.0, 0);
    sb.write(&format!("{cache}/new.pst"), 64);

    let run = |allow_local_mail_permanent| {
        c.execute(
            &plan,
            &ExecOptions {
                dry_run: true,
                allow_unfit_permanent: false,
                allow_local_mail_permanent,
                log: None,
            },
            &Progress::new(),
        )
        .unwrap()
    };
    let without = run(false);
    assert_eq!(
        without.items.iter().map(|i| i.stats.protected).sum::<u64>(),
        1
    );
    assert_eq!(without.items.iter().map(|i| i.stats.files).sum::<u64>(), 1);
    let with = run(true);
    assert_eq!(with.items.iter().map(|i| i.stats.protected).sum::<u64>(), 0);
    assert_eq!(with.items.iter().map(|i| i.stats.files).sum::<u64>(), 2);
    assert!(sb.path(&format!("{cache}/new.pst")).exists());
}

#[test]
fn newly_added_pst_requires_consent_in_unfit_entry() {
    let sb = Sandbox::new();
    sb.write("Docs/Own/plain.bin", 32);
    let fake = FakeShell::new(&sb, Some(1), 0, "never");
    let c = cleaner_for(&sb).with_shell(fake);
    let mut basket = Basket::new();
    basket.add_path(sb.long("Docs/Own"), c.protection());
    let plan = approve(&c, &basket);
    assert_eq!(plan.items.len(), 1);
    assert!(plan.items[0].is_unfit());
    assert_eq!(plan.mail_totals().0.0, 0);
    sb.write("Docs/Own/new.pst", 64);

    let run = |allow_local_mail_permanent| {
        c.execute(
            &plan,
            &ExecOptions {
                dry_run: true,
                allow_unfit_permanent: true,
                allow_local_mail_permanent,
                log: None,
            },
            &Progress::new(),
        )
        .unwrap()
    };
    let without = run(false);
    assert_eq!(without.items[0].destination, Destination::Permanent);
    assert_eq!(without.items[0].stats.protected, 1);
    assert_eq!(without.items[0].stats.files, 1);
    let with = run(true);
    assert_eq!(with.items[0].stats.protected, 0);
    assert_eq!(with.items[0].stats.files, 2);
    assert!(sb.path("Docs/Own/new.pst").exists());
}

#[test]
fn fixed_diagnostic_paths_do_not_depend_on_current_existence() {
    let sb = Sandbox::new();
    let c = cleaner_for(&sb);
    let root = sb.long("Local/CrashDumps");
    assert!(!root.exists());
    assert!(c.roots.is_diagnostic_path(&root));
    assert!(c.roots.is_diagnostic_path(&root.join("later.dmp")));
    assert!(!c.roots.is_diagnostic_path(&sb.long("Docs/Own")));
}

#[test]
fn approval_keeps_diagnostic_veto_when_root_disappears_after_measurement() {
    let sb = Sandbox::new();
    sb.write("Local/CrashDumps/incident.dmp", 128);
    let fake = FakeShell::new(&sb, Some(1), 0, "never");
    let mut c = diag_cleaner(&sb, fake);
    let original = sb.path("Local/CrashDumps");
    let displaced = sb.path("Local/CrashDumps-away");
    c.after_measure_for_test = Some(Arc::new({
        let original = original.clone();
        let displaced = displaced.clone();
        move || fs::rename(&original, &displaced).unwrap()
    }));
    let mut basket = Basket::new();
    basket.add_path(sb.long("Local/CrashDumps/incident.dmp"), c.protection());
    let plan = approve(&c, &basket);
    fs::rename(&displaced, &original).unwrap();
    assert_eq!(plan.items.len(), 1, "{:?}", plan.rejected);
    assert_eq!(plan.items[0].category, None);
    assert!(plan.items[0].diagnostic_path);
    assert!(plan.items[0].never_permanent());
    assert!(plan.items[0].is_unfit());
    let rep = exec(&c, &plan, true, true, None).unwrap();
    assert_eq!(status_of(&rep, "incident.dmp"), ItemStatus::SkippedUnfit);
    assert!(sb.path("Local/CrashDumps/incident.dmp").exists());
}

#[test]
fn individual_pst_entry_requires_explicit_permanent_consent() {
    let sb = Sandbox::new();
    sb.write("Docs/mail.pst", 64);
    let fake = FakeShell::new(&sb, Some(1), 0, "never");
    let c = cleaner_for(&sb).with_shell(fake);
    let mut basket = Basket::new();
    basket.add_path(sb.long("Docs/mail.pst"), c.protection());
    let plan = approve(&c, &basket);
    assert_eq!(plan.items.len(), 1);
    assert!(plan.items[0].is_unfit());
    let run = |allow_local_mail_permanent| {
        c.execute(
            &plan,
            &ExecOptions {
                dry_run: true,
                allow_unfit_permanent: true,
                allow_local_mail_permanent,
                log: None,
            },
            &Progress::new(),
        )
        .unwrap()
    };
    let without = run(false);
    assert_eq!(
        without.items[0].status,
        ItemStatus::Root(crate::engine::RootStatus::MailConsentRequired)
    );
    assert_eq!(without.items[0].stats.protected, 1);
    let with = run(true);
    assert_eq!(with.items[0].status, ItemStatus::Done);
    assert_eq!(with.items[0].stats.files, 1);
    assert!(sb.path("Docs/mail.pst").exists());
}
