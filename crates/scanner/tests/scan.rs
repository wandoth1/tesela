//! Tests de integración del escáner sobre árboles creados en `%TEMP%\tesela-test-<uuid>` (ver `common`).

mod common;

use common::TempTree;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tesela_model::{Tree, flags};
use tesela_scanner::{ApiUsed, ScanOptions, ScanResult, scan};

fn opts(t: &TempTree) -> ScanOptions {
    let mut o = ScanOptions::new(t.root());
    o.threads = 4;
    o
}

fn scan_tree(t: &TempTree) -> ScanResult {
    let r = scan(opts(t)).expect("escaneo");
    r.tree.check_invariants().expect("invariantes del árbol");
    assert_eq!(r.tree.stats().orphans, 0);
    r
}

/// Oráculo independiente con `std::fs`: (archivos, directorios sin contar la raíz, bytes lógicos de archivos).
/// No entra en reparse points.
fn oracle(root: &Path) -> (u64, u64, u64) {
    let (mut files, mut dirs, mut logical) = (0, 0, 0);
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let md = std::fs::symlink_metadata(e.path()).unwrap();
            let reparse = md.file_attributes() & 0x400 != 0;
            if md.file_attributes() & 0x10 != 0 {
                if !reparse {
                    dirs += 1;
                    stack.push(e.path());
                }
            } else {
                files += 1;
                logical += md.len();
            }
        }
    }
    (files, dirs, logical)
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

/// Suma lógica de los nodos archivo de un árbol.
fn file_logical_sum(tree: &Tree) -> u64 {
    tree.nodes()
        .iter()
        .filter(|n| !n.is_dir() && n.flags & flags::SYSTEM_INFO == 0)
        .map(|n| n.logical)
        .sum()
}

/// Escaneo previo descartado. NTFS refresca el `mtime` de un directorio en el índice de su padre (el que usa el
/// listado) de forma diferida, al cerrarse los handles del directorio: justo tras crear un árbol, un primer
/// escaneo puede ver un `mtime` de directorio 1 s más antiguo que los siguientes. Los tests que comparan dos
/// escaneos del mismo árbol lo asientan primero.
fn settle(t: &TempTree) {
    let mut o = opts(t);
    o.priority_paths = Some(vec![]);
    scan(o).expect("escaneo de asentamiento");
}

/// Suma de lo que ocupan los propios directorios (su alloc menos el de sus hijos).
fn dirs_own_sum(tree: &Tree) -> u64 {
    (0..tree.len() as u32)
        .filter(|&i| tree.node(i).is_dir())
        .map(|i| tree.node(i).alloc - tree.children(i).map(|c| tree.node(c).alloc).sum::<u64>())
        .sum()
}

#[test]
fn arbol_basico_coincide_con_el_oraculo_y_las_rutas_son_exactas() {
    let t = TempTree::new();
    t.write("a.txt", 10_000);
    t.write("sub/b.bin", 20_000);
    t.write("sub/deep/c.dat", 30_000);
    t.write("sub/deep/d.dat", 123_457);
    t.mkdir("vacia");
    let r = scan_tree(&t);
    let (files, dirs, logical) = oracle(t.root());
    let st = r.tree.stats();
    assert_eq!(st.files, files);
    assert_eq!(st.dirs, dirs + 1, "directorios más la raíz");
    assert_eq!(file_logical_sum(&r.tree), logical);
    assert_eq!(r.summary.api, ApiUsed::Extended);
    assert_eq!(r.summary.errors(), 0);
    assert!(!r.summary.cancelled && !r.summary.full_volume);
    // Los archivos grandes ocupan al menos su tamaño y no mucho más (redondeo a cluster).
    for rel in ["a.txt", "sub/b.bin", "sub/deep/c.dat", "sub/deep/d.dat"] {
        let n = r.tree.node(find(&r.tree, rel));
        assert!(
            n.alloc >= n.logical && n.alloc < n.logical + 128 * 1024,
            "{rel}: alloc {} logical {}",
            n.alloc,
            n.logical
        );
    }
    // Cada nodo existe de verdad en el disco con la ruta que reconstruye el árbol.
    for id in 0..r.tree.len() as u32 {
        let p = r.tree.path(id);
        assert!(
            std::fs::symlink_metadata(&p).is_ok(),
            "no existe {}",
            p.display()
        );
    }
    assert_eq!(
        r.tree.path(0).canonicalize().unwrap(),
        t.root().canonicalize().unwrap()
    );
    // No hay SYSTEM_INFO fuera de un volumen completo ni INCOMPLETE tras un escaneo terminado.
    assert!(
        r.tree
            .nodes()
            .iter()
            .all(|n| n.flags & (flags::SYSTEM_INFO | flags::INCOMPLETE | flags::ERROR) == 0)
    );
    assert_eq!(r.tree.stats().unexplained_alloc, 0);
    let vac = r.tree.node(find(&r.tree, "vacia"));
    assert!(vac.is_dir() && vac.child_count == 0);
}

#[test]
fn el_progreso_cuenta_archivos_y_acumula_por_carpeta_de_primer_nivel() {
    let t = TempTree::new();
    t.write("suelto.bin", 8192);
    t.write("uno/a.bin", 50_000);
    t.write("uno/x/b.bin", 50_000);
    t.write("dos/c.bin", 9000);
    let h = tesela_scanner::start(opts(&t)).unwrap();
    let progress = h.progress().clone();
    let r = h.wait().unwrap();
    assert!(progress.finished.load(Ordering::SeqCst));
    assert_eq!(progress.files.load(Ordering::SeqCst), 4);
    assert_eq!(progress.dirs.load(Ordering::SeqCst), 3);
    let top = progress.top_levels();
    assert_eq!(top.len(), 3, "raíz más dos carpetas");
    let by_name = |n: &str| top.iter().find(|t| t.name == n).unwrap();
    assert_eq!(by_name("uno").files.load(Ordering::SeqCst), 2);
    assert_eq!(by_name("dos").files.load(Ordering::SeqCst), 1);
    assert_eq!(
        top[0].files.load(Ordering::SeqCst),
        1,
        "índice 0 = archivos de la raíz"
    );
    let uno = r.tree.node(find(&r.tree, "uno"));
    assert!(
        by_name("uno").alloc.load(Ordering::SeqCst) >= uno.alloc - 8192,
        "el acumulador casi iguala al nodo"
    );
    assert_eq!(progress.errors.load(Ordering::SeqCst), 0);
}

#[test]
fn un_enlace_duro_se_cuenta_una_sola_vez_y_gana_la_ruta_menor() {
    let t = TempTree::new();
    t.write("x/orig.bin", 100_000);
    t.hard_link("x/orig.bin", "y/link.bin");
    t.write("z/solo.bin", 50_000);
    let r = scan_tree(&t);
    let tree = &r.tree;
    let (orig, link, solo) = (
        tree.node(find(tree, "x/orig.bin")),
        tree.node(find(tree, "y/link.bin")),
        tree.node(find(tree, "z/solo.bin")),
    );
    assert!(
        orig.alloc >= 100_000,
        "x\\orig.bin es la ruta menor: conserva el tamaño"
    );
    assert_eq!((link.alloc, link.logical), (0, 0));
    assert_eq!(link.flags & flags::HARDLINK, flags::HARDLINK);
    assert_eq!(orig.flags & flags::HARDLINK, 0);
    assert_eq!(
        tree.stats().files,
        3,
        "las tres entradas cuentan como archivos"
    );
    assert_eq!(tree.stats().hardlink_groups, 1);
    assert_eq!(tree.stats().hardlink_extra_entries, 1);
    assert_eq!(tree.stats().hardlink_alloc_saved, orig.alloc);
    assert_eq!(
        tree.node(0).alloc,
        orig.alloc + solo.alloc + dirs_own_sum(tree),
        "la suma cuenta el enlace una sola vez"
    );
    // Sin deduplicar se cuenta dos veces.
    let mut o = opts(&t);
    o.dedup_hard_links = false;
    let no_dedup = scan(o).unwrap().tree;
    assert_eq!(no_dedup.node(0).alloc, tree.node(0).alloc + orig.alloc);
    assert_eq!(no_dedup.stats().hardlink_extra_entries, 0);
}

#[test]
fn un_junction_no_se_sigue_y_su_destino_se_cuenta_una_vez() {
    let mut t = TempTree::new();
    t.write("real/f1.bin", 20_000);
    t.write("real/sub/f2.bin", 20_000);
    t.junction("enlace", "real");
    let r = scan_tree(&t);
    let tree = &r.tree;
    let j = tree.node(find(tree, "enlace"));
    assert_eq!(
        j.flags & (flags::REPARSE | flags::DIR),
        flags::REPARSE | flags::DIR
    );
    assert_eq!(j.child_count, 0, "no se entra en el junction");
    assert_eq!((j.alloc, j.logical), (0, 0));
    assert_eq!(
        tree.stats().files,
        2,
        "los archivos del destino no se cuentan por el junction"
    );
    assert_eq!(tree.stats().reparse_dirs, 1);
    assert_eq!(
        tree.stats().incomplete_dirs,
        0,
        "un reparse point no es INCOMPLETE"
    );
    let real = tree.node(find(tree, "real"));
    assert!(real.alloc >= 40_000);
    assert_eq!(tree.node(0).files_below, 2);
    // El oráculo (que tampoco sigue enlaces) coincide.
    let (files, _, logical) = oracle(t.root());
    assert_eq!(
        (tree.stats().files, file_logical_sum(tree)),
        (files, logical)
    );
}

#[test]
fn parar_y_ver_construye_con_lo_recibido_y_marca_incomplete() {
    let t = TempTree::new();
    t.write("raiz.bin", 20_000);
    t.write("uno/a.bin", 20_000);
    t.write("uno/x/b.bin", 20_000);
    t.mkdir("dos");
    let mut o = opts(&t);
    o.cancel = Some(Arc::new(AtomicBool::new(true))); // ya cancelado: solo se lista la raíz
    let r = scan(o).unwrap();
    r.tree.check_invariants().unwrap();
    assert!(r.summary.cancelled);
    let tree = &r.tree;
    // Lo que se llegó a listar (la raíz) está; las carpetas sin recorrer son INCOMPLETE y sin contenido.
    assert!(tree.child_by_name(0, b"raiz.bin").is_some());
    for d in ["uno", "dos"] {
        let n = tree.node(find(tree, d));
        assert_eq!(n.flags & flags::INCOMPLETE, flags::INCOMPLETE, "{d}");
        assert_eq!(n.child_count, 0);
    }
    assert_eq!(tree.stats().incomplete_dirs, 2);
    assert!(tree.child_by_name(find(tree, "uno"), b"a.bin").is_none());
    assert_eq!(tree.stats().orphans, 0);
    assert_eq!(tree.stats().unexplained_alloc, 0);
}

#[test]
fn cancelar_a_mitad_siempre_da_un_arbol_valido() {
    let t = TempTree::new();
    for i in 0..60 {
        for j in 0..4 {
            t.write(&format!("d{i}/s{j}/f.bin"), 5000);
        }
    }
    for n in 0..8 {
        let h = tesela_scanner::start(opts(&t)).unwrap();
        if n % 2 == 0 {
            std::thread::yield_now();
        }
        h.cancel();
        let r = h.wait().unwrap();
        r.tree.check_invariants().unwrap();
        assert_eq!(r.tree.stats().orphans, 0);
        if r.summary.cancelled {
            // Todo directorio no recorrido está marcado; los recorridos no.
            let incomplete = r
                .tree
                .nodes()
                .iter()
                .filter(|n| n.flags & flags::INCOMPLETE != 0)
                .count() as u64;
            assert_eq!(incomplete, r.tree.stats().incomplete_dirs);
        } else {
            assert_eq!(r.tree.stats().incomplete_dirs, 0);
            assert_eq!(r.tree.stats().files, 240);
        }
    }
}

#[test]
fn adopcion_por_file_id_de_las_rutas_prioritarias() {
    let t = TempTree::new();
    t.write("a/b/c/objetivo/f1.bin", 30_000);
    t.write("a/b/c/objetivo/q/f1b.bin", 11_000);
    t.write("a/b/otra/f2.bin", 20_000);
    t.write("a/z.txt", 5_000);
    t.write("m/n/f3.bin", 40_000);
    t.write("m/g.bin", 9_000);
    t.write("fuera/f4.bin", 7_000);
    settle(&t);
    // Referencia: sin cola prioritaria.
    let mut base = opts(&t);
    base.priority_paths = Some(vec![]);
    let reference = scan(base).unwrap();
    reference.tree.check_invariants().unwrap();
    assert!(reference.summary.priority.is_empty());
    for threads in [1usize, 2, 4, 8, 16] {
        for _ in 0..5 {
            let mut o = opts(&t);
            o.threads = threads;
            o.priority_paths = Some(vec![
                t.path("a/b/c/objetivo"),
                t.path("m"), // carpeta de primer nivel: sustituye a su tarea general
                t.path("a/b/c/objetivo/q"), // dentro de otra: se ignora
                t.path("no/existe"), // no existe: se ignora
                t.root().join("A").join("B").join("C"), // mayúsculas distintas, ancestro de otra: es una ruta válida
                std::env::temp_dir(),                   // ancestro de la raíz: no cuelga de ella
            ]);
            let r = scan(o).unwrap();
            r.tree.check_invariants().unwrap();
            assert_eq!(r.tree.stats().orphans, 0);
            // Mismo árbol, id a id, que sin cola prioritaria (nodos precreados con los mismos valores).
            assert!(
                r.tree.structural_eq(&reference.tree),
                "con {threads} hilos el árbol difiere del de referencia"
            );
            // `a/b/c` queda dentro de nada aceptado: son 3 rutas (m, a/b/c, a/b/c/objetivo no: va dentro de a/b/c).
            let paths: Vec<_> = r
                .summary
                .priority
                .iter()
                .map(|p| p.path.file_name().unwrap().to_string_lossy().to_lowercase())
                .collect();
            assert_eq!(paths.len(), 2, "{paths:?}");
            assert!(paths.contains(&"m".to_string()) && paths.contains(&"c".to_string()));
            assert!(
                r.summary.priority.iter().all(|p| p.done),
                "todas se recorren enteras"
            );
            let c = r
                .summary
                .priority
                .iter()
                .find(|p| p.path.ends_with("c"))
                .unwrap();
            assert_eq!(c.files, 2, "f1 y f1b");
            let m = r
                .summary
                .priority
                .iter()
                .find(|p| p.path.ends_with("m"))
                .unwrap();
            assert_eq!(m.files, 2);
            // Sin nodos duplicados entre hermanos.
            for id in 0..r.tree.len() as u32 {
                let names: Vec<_> = r
                    .tree
                    .children(id)
                    .map(|c| r.tree.name_bytes(c).to_vec())
                    .collect();
                assert_eq!(
                    names.len(),
                    names.iter().collect::<BTreeSet<_>>().len(),
                    "nombres repetidos bajo {id}"
                );
            }
        }
    }
}

#[test]
fn la_ruta_prioritaria_de_primer_nivel_y_la_profunda_dan_el_mismo_arbol() {
    let t = TempTree::new();
    t.write("p/q/r/f.bin", 10_000);
    t.write("p/q/s.bin", 10_000);
    t.write("otro/g.bin", 10_000);
    settle(&t);
    let mut a = opts(&t);
    a.priority_paths = Some(vec![]);
    let reference = scan(a).unwrap().tree;
    let mut b = opts(&t);
    b.priority_paths = Some(vec![t.path("p/q/r"), t.path("otro")]);
    let r = scan(b).unwrap();
    assert!(r.tree.structural_eq(&reference));
    assert_eq!(r.summary.priority.len(), 2);
}

#[test]
fn el_respaldo_find_da_los_mismos_totales_sin_dedup_ni_cola_prioritaria() {
    let t = TempTree::new();
    t.write("a/x.bin", 100_000);
    t.hard_link("a/x.bin", "b/y.bin");
    t.write("a/deep/z.bin", 33_333);
    t.write("w.bin", 4_097);
    let ext = scan_tree(&t);
    let mut o = opts(&t);
    o.force_find = true;
    o.priority_paths = Some(vec![t.path("a")]);
    let fres = scan(o).unwrap();
    fres.tree.check_invariants().unwrap();
    assert_eq!(fres.summary.api, ApiUsed::FindFallback);
    assert!(
        fres.summary.priority.is_empty(),
        "sin FileId no hay adopción: la cola prioritaria se omite"
    );
    assert_eq!(fres.tree.stats().files, ext.tree.stats().files);
    assert_eq!(fres.tree.stats().dirs, ext.tree.stats().dirs);
    assert_eq!(
        file_logical_sum(&fres.tree),
        file_logical_sum(&ext.tree) + 100_000,
        "el enlace duro se cuenta dos veces"
    );
    assert_eq!(fres.tree.stats().hardlink_extra_entries, 0);
    assert!(
        fres.tree
            .nodes()
            .iter()
            .all(|n| n.flags & flags::HARDLINK == 0)
    );
    // alloc estimado = EOF redondeado a cluster: coincide con el real en archivos normales.
    let (e, f) = (
        fres.tree
            .node(fres.tree.child_by_name(0, b"w.bin").unwrap()),
        ext.tree.node(ext.tree.child_by_name(0, b"w.bin").unwrap()),
    );
    assert_eq!(e.alloc, f.alloc);
    assert_eq!(
        fres.tree.node(0).alloc,
        ext.tree.node(0).alloc + ext.tree.node(find(&ext.tree, "a/x.bin")).alloc
    );
}

#[test]
fn los_errores_de_directorio_se_cuentan_y_no_abortan_el_escaneo() {
    let mut t = TempTree::new();
    t.write("ok/a.bin", 20_000);
    t.write("denegada/secreto.bin", 20_000);
    t.write("otro.bin", 10_000);
    if !t.deny_list("denegada") {
        eprintln!("(el entorno ignora la denegación de ACL: se omite la comprobación de errores)");
        return;
    }
    let r = scan(opts(&t)).unwrap();
    r.tree.check_invariants().unwrap();
    assert_eq!(
        r.summary.errors_by_code.get(&5),
        Some(&1),
        "ERROR_ACCESS_DENIED una vez: {:?}",
        r.summary.errors_by_code
    );
    let d = r.tree.node(find(&r.tree, "denegada"));
    assert_eq!(d.flags & (flags::ERROR | flags::INCOMPLETE), flags::ERROR);
    assert_eq!(r.tree.stats().error_dirs, 1);
    assert_eq!(
        r.tree.stats().files,
        2,
        "el resto se escanea con normalidad"
    );
    assert_eq!(r.summary.error_samples.len(), 1);
}

#[test]
fn rutas_largas_unicode_y_nombres_con_sustitutos_desparejados() {
    let t = TempTree::new();
    let long = "x".repeat(100);
    let deep = format!("{long}/{long}/{long}/{long}");
    t.write(&format!("{deep}/archivo.bin"), 12_345);
    assert!(t.path(&deep).as_os_str().len() > 400);
    t.write("ñandú 日本語 😀.txt", 9_999);
    // Nombre con un sustituto UTF-16 desparejado (NTFS lo admite; no es UTF-8 válido).
    let weird: Vec<u16> = "raro-"
        .encode_utf16()
        .chain([0xD800u16])
        .chain("-fin.bin".encode_utf16())
        .collect();
    let weird_path = t.long_root().join(OsString::from_wide(&weird));
    std::fs::write(&weird_path, vec![7u8; 6000]).expect("crea el archivo de nombre raro");
    let r = scan_tree(&t);
    assert_eq!(r.summary.errors(), 0);
    let tree = &r.tree;
    assert_eq!(tree.stats().files, 3);
    let weird_id = tree
        .children(0)
        .find(|&c| tree.name_utf16(c) == weird)
        .expect("el nombre raro se conserva en UTF-16");
    assert_eq!(
        tree.path(weird_id),
        weird_path,
        "la ruta se reconstruye exacta (WTF-8 -> UTF-16)"
    );
    assert!(std::fs::metadata(tree.path(weird_id)).is_ok());
    let f = find(tree, &format!("{deep}/archivo.bin"));
    assert_eq!(tree.node(f).logical, 12_345);
    assert!(
        std::fs::metadata(tree.path(f)).is_ok(),
        "ruta de más de 400 caracteres"
    );
    assert!(std::fs::metadata(tree.path(find(tree, "ñandú 日本語 😀.txt"))).is_ok());
}

#[test]
fn el_resultado_no_depende_del_numero_de_hilos() {
    let t = TempTree::new();
    for i in 0..12 {
        t.write(&format!("d{i}/a{}.bin", i % 3), 4096 * (i % 4 + 1));
        t.write(&format!("d{i}/s/b.bin"), 4096 * (i % 4 + 1));
    }
    t.hard_link("d1/a1.bin", "d5/enlace.bin");
    settle(&t);
    let one = {
        let mut o = opts(&t);
        o.threads = 1;
        scan(o).unwrap().tree
    };
    for threads in [2, 7, 16, 32] {
        let mut o = opts(&t);
        o.threads = threads;
        let r = scan(o).unwrap();
        if let Some(d) = r.tree.first_difference(&one) {
            panic!("{threads} hilos: {d}");
        }
    }
}

#[test]
fn raiz_inexistente_o_que_no_es_un_directorio_son_errores() {
    let t = TempTree::new();
    let f = t.write("archivo.txt", 100);
    assert!(matches!(
        scan(ScanOptions::new(t.path("no-existe"))),
        Err(tesela_scanner::ScanError::Root(_))
    ));
    assert!(matches!(
        scan(ScanOptions::new(f)),
        Err(tesela_scanner::ScanError::NotADirectory(_))
    ));
}
