//! Integration tests for the increment 1 audit fixes (over synthetic trees in `%TEMP%`).

mod common;

use common::TempTree;
use tesela_model::{Tree, flags};
use tesela_scanner::{ScanOptions, VolumeSpace, scan};

fn opts(t: &TempTree) -> ScanOptions {
    let mut o = ScanOptions::new(t.root());
    o.threads = 4;
    o
}

fn find(tree: &Tree, rel: &str) -> u32 {
    let mut id = 0;
    for part in rel.split('/') {
        id = tree
            .child_by_name(id, part.as_bytes())
            .unwrap_or_else(|| panic!("no existe {rel}"));
    }
    id
}

/// Discarded warm-up scan (NTFS lazily refreshes a directory's mtime in its parent's index).
fn settle(t: &TempTree) {
    let mut o = opts(t);
    o.priority_paths = Some(vec![]);
    scan(o).expect("escaneo de asentamiento");
}

#[test]
fn alloc_de_cada_archivo_coincide_con_filestandardinfo_por_handle() {
    let t = TempTree::new();
    t.write("a.bin", 10_000);
    t.write("sub/b.bin", 4_096);
    t.write("sub/deep/c.bin", 123_457);
    t.write("sub/deep/pequeno.txt", 10); // puede ser residente en la MFT (alloc 0)
    t.write("d.dat", 0);
    let r = scan(opts(&t)).unwrap();
    r.tree.check_invariants().unwrap();
    let mut checked = 0;
    for id in 0..r.tree.len() as u32 {
        let n = r.tree.node(id);
        if n.is_dir() {
            continue;
        }
        assert_eq!(
            n.alloc,
            common::allocation_size(&r.tree.path(id)),
            "{}",
            r.tree.name_lossy(id)
        );
        checked += 1;
    }
    assert_eq!(checked, 5);
}

#[test]
fn el_espacio_no_explicado_se_calcula_de_punta_a_punta_con_un_volumen_inyectado() {
    let t = TempTree::new();
    t.write("a.bin", 40_000);
    t.write("b/c.bin", 20_000);
    let mut o = opts(&t);
    o.priority_paths = Some(vec![]);
    o.volume_override = Some(VolumeSpace::new(10 << 30, 4 << 30));
    let r = scan(o.clone()).unwrap();
    r.tree.check_invariants().unwrap();
    assert!(r.summary.full_volume && !r.summary.cancelled);
    let st = r.tree.stats();
    let used = 6u64 << 30;
    assert_eq!(st.unexplained_alloc, used - st.scanned_alloc);
    let last = r.tree.children(0).last().unwrap();
    assert_eq!(
        r.tree.node(last).flags,
        flags::SYSTEM_INFO,
        "va siempre el último"
    );
    assert_eq!(r.tree.node(last).alloc, used - st.scanned_alloc);
    assert_eq!(r.tree.node(0).alloc, used);
    // With a disk quota, the user's available space is outside the calculation (it has a different scope): the result is unchanged.
    let mut quota = VolumeSpace::new(10 << 30, 4 << 30);
    quota.available = 1 << 30;
    assert!(quota.quota_limited() && quota.used() == used);
    let mut oq = o.clone();
    oq.volume_override = Some(quota);
    let rq = scan(oq).unwrap();
    assert_eq!(rq.tree.stats().unexplained_alloc, used - st.scanned_alloc);
    assert_eq!(rq.summary.volume, Some(quota));
    // With a quota and WITHOUT the actual volume capacity (class 14 unsupported), `total` and `free` are user-scoped: there
    // is no residual (subtracting them from the scan would mix scopes), and the system block is not added.
    let mut user_scope = VolumeSpace::new(10 << 30, 4 << 30);
    user_scope.volume_scope = false;
    assert!(user_scope.quota_limited());
    let mut ou = o.clone();
    ou.volume_override = Some(user_scope);
    let ru = scan(ou).unwrap();
    ru.tree.check_invariants().unwrap();
    assert_eq!(ru.tree.stats().unexplained_alloc, 0);
    assert!(
        ru.tree
            .nodes()
            .iter()
            .all(|n| n.flags & flags::SYSTEM_INFO == 0)
    );
    assert_eq!(ru.summary.volume, Some(user_scope));
    // If used space is less than the scanned amount, it is clamped to 0 and there is no node.
    o.volume_override = Some(VolumeSpace::new(10 << 30, (10 << 30) - 100));
    let r = scan(o).unwrap();
    assert_eq!(r.tree.stats().unexplained_alloc, 0);
    assert!(
        r.tree
            .nodes()
            .iter()
            .all(|n| n.flags & flags::SYSTEM_INFO == 0)
    );
}

#[test]
fn cancelar_con_cualquier_retardo_nunca_deja_espacio_no_explicado_sobre_un_arbol_incompleto() {
    let t = TempTree::new();
    // A deep chain (one file at each level) plus a fan-out of folders with subfolders.
    let mut chain = String::new();
    for i in 0..40 {
        chain.push_str(&format!("c{i}/"));
        t.write(&format!("{chain}f.bin"), 5000);
    }
    for i in 0..25 {
        for j in 0..3 {
            t.write(&format!("abanico{i}/s{j}/f.bin"), 5000);
        }
    }
    let expected_files = 40 + 25 * 3;
    let (mut cancelled_seen, mut complete_seen) = (0, 0);
    for delay_us in [
        0u64, 0, 30, 60, 100, 200, 400, 800, 1500, 3000, 6000, 20_000,
    ] {
        for _ in 0..4 {
            let mut o = opts(&t);
            o.priority_paths = Some(vec![]);
            o.volume_override = Some(VolumeSpace::new(10 << 30, 4 << 30));
            let h = tesela_scanner::start(o).unwrap();
            std::thread::sleep(std::time::Duration::from_micros(delay_us));
            h.cancel();
            let r = h.wait().unwrap();
            r.tree.check_invariants().unwrap();
            let st = r.tree.stats();
            let has_system = r
                .tree
                .nodes()
                .iter()
                .any(|n| n.flags & flags::SYSTEM_INFO != 0);
            if st.incomplete_dirs > 0 {
                cancelled_seen += 1;
                assert!(
                    r.summary.cancelled,
                    "incompleto pero cancelled = false (retardo {delay_us} us)"
                );
                assert!(
                    !has_system && st.unexplained_alloc == 0,
                    "espacio no explicado sobre un árbol a medias"
                );
                assert_eq!(
                    r.tree.node(0).flags & flags::INCOMPLETE_BELOW,
                    flags::INCOMPLETE_BELOW
                );
            } else {
                complete_seen += 1;
                assert_eq!(
                    st.files, expected_files,
                    "sin directorios incompletos el árbol está entero"
                );
                assert!(!r.summary.cancelled);
                assert!(
                    has_system,
                    "escaneo terminado de un volumen completo: hay espacio no explicado"
                );
            }
        }
    }
    eprintln!("cancelados con árbol incompleto: {cancelled_seen}, completos: {complete_seen}");
    assert!(
        cancelled_seen > 0,
        "ningún retardo llegó a cancelar a tiempo: el test no probaría nada"
    );
}

#[test]
fn con_distincion_de_mayusculas_la_cola_prioritaria_no_confunde_hermanos() {
    // 1) Case-sensitive top-level folder `cs`, with siblings Temp / temp / TEMP.
    let t = TempTree::new();
    t.mkdir("cs");
    if !t.case_sensitive("cs") {
        eprintln!("(fsutil no admite carpetas sensibles a mayúsculas aquí: se omite)");
        return;
    }
    t.write("cs/Temp/f1.bin", 10_000);
    t.write("cs/temp/f2a.bin", 11_000);
    t.write("cs/temp/f2b.bin", 12_000);
    t.write("cs/temp/sub/g.bin", 13_000);
    t.write("cs/TEMP/f3a.bin", 14_000);
    t.write("cs/TEMP/f3b.bin", 15_000);
    t.write("cs/TEMP/f3c.bin", 16_000);
    settle(&t);
    let mut base = opts(&t);
    base.priority_paths = Some(vec![]);
    let reference = scan(base).unwrap().tree;
    for (target, files) in [
        ("cs/temp", 3u64),
        ("cs/TEMP", 3),
        ("cs/Temp", 1),
        ("cs/temp/sub", 1),
    ] {
        let mut o = opts(&t);
        o.priority_paths = Some(vec![t.path(target)]);
        let r = scan(o).unwrap();
        r.tree.check_invariants().unwrap();
        if let Some(d) = r.tree.first_difference(&reference) {
            panic!("{target}: el árbol difiere del de referencia: {d}");
        }
        assert_eq!(r.summary.priority.len(), 1, "{target}");
        assert_eq!(
            r.summary.priority[0].files, files,
            "{target}: contenido atribuido al hermano equivocado"
        );
        assert!(r.summary.priority[0].done);
        assert!(
            r.summary.priority[0]
                .path
                .ends_with(target.rsplit('/').next().unwrap())
        );
    }
    let cs = find(&reference, "cs");
    let counts: Vec<_> = ["Temp", "temp", "TEMP"]
        .iter()
        .map(|n| {
            reference
                .node(reference.child_by_name(cs, n.as_bytes()).unwrap())
                .files_below
        })
        .collect();
    assert_eq!(counts, [1, 3, 3]);

    // 2) The root itself is case-sensitive: Temp / temp / TEMP are top-level folders.
    let t2 = TempTree::new();
    if !t2.case_sensitive("") {
        return;
    }
    t2.write("Temp/f1.bin", 10_000);
    t2.write("temp/f2a.bin", 11_000);
    t2.write("temp/f2b.bin", 12_000);
    t2.write("TEMP/f3a.bin", 14_000);
    t2.write("TEMP/f3b.bin", 15_000);
    t2.write("TEMP/f3c.bin", 16_000);
    settle(&t2);
    let mut base = opts(&t2);
    base.priority_paths = Some(vec![]);
    let reference = scan(base).unwrap().tree;
    for (target, files) in [("temp", 2u64), ("TEMP", 3), ("Temp", 1)] {
        let mut o = opts(&t2);
        o.priority_paths = Some(vec![t2.path(target)]);
        let r = scan(o).unwrap();
        r.tree.check_invariants().unwrap();
        assert!(r.tree.structural_eq(&reference), "{target}");
        assert_eq!(r.summary.priority.len(), 1);
        assert_eq!(
            r.summary.priority[0].files, files,
            "{target}: hermano equivocado en la raíz"
        );
    }
}
