//! Scans started from a folder's SHORT 8.3 form (some runners use it for `%TEMP%`: a long profile component
//! appears abbreviated as `NAME~1`). The scanner must reconstruct long-form paths, and the priority queue must adopt them
//! even when the priority path is given in short form. If the volume does not have active 8.3 names,
//! the tests are skipped with a message.

mod common;

use common::{TempTree, short_path};
use tesela_scanner::{ScanOptions, scan};

const LARGA_A: &str = "carpeta-con-un-nombre-bastante-largo";
const LARGA_B: &str = "subcarpeta-con-un-nombre-tambien-largo";

fn opts(root: &std::path::Path) -> ScanOptions {
    let mut o = ScanOptions::new(root);
    o.threads = 4;
    o
}

/// Creates the test tree and returns the short forms of the root and deep folder, or `None` if 8.3 names are unavailable.
fn setup(t: &TempTree) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    t.write(&format!("{LARGA_A}/{LARGA_B}/archivo-uno.bin"), 30_000);
    t.write(
        &format!("{LARGA_A}/{LARGA_B}/hondo/archivo-dos.bin"),
        11_000,
    );
    t.write(&format!("{LARGA_A}/otro-archivo-largo.bin"), 9_000);
    t.write("otra-carpeta-con-nombre-largo/suelto.bin", 7_000);
    let deep = t.long_path(&format!("{LARGA_A}/{LARGA_B}"));
    let short_deep = short_path(&deep);
    let short_root = short_path(&t.long_root());
    let has_83 = short_deep.to_string_lossy().contains('~') && short_root != t.long_root();
    if !has_83 {
        eprintln!(
            "(este volumen no tiene nombres 8.3 activos: se omite la prueba de formas cortas)"
        );
        return None;
    }
    Some((short_root, short_deep))
}

/// Settles the directories' `mtime` (NTFS refreshes it lazily) so two scans can be compared.
fn settle(root: &std::path::Path) {
    let mut o = opts(root);
    o.priority_paths = Some(vec![]);
    scan(o).expect("escaneo de asentamiento");
}

#[test]
fn desde_la_raiz_en_forma_corta_las_rutas_salen_en_forma_larga() {
    let t = TempTree::new();
    let Some((short_root, _)) = setup(&t) else {
        return;
    };
    settle(&short_root);
    let r = scan(opts(&short_root)).unwrap();
    r.tree.check_invariants().unwrap();
    assert_eq!(
        r.tree.path(0),
        t.long_root(),
        "la raíz se muestra en forma larga"
    );
    assert_eq!(r.summary.root, t.long_root());
    for id in 0..r.tree.len() as u32 {
        let p = r.tree.path(id);
        assert!(
            !p.to_string_lossy().contains('~'),
            "una ruta reconstruida sigue en forma corta"
        );
        assert!(
            std::fs::symlink_metadata(&p).is_ok(),
            "la ruta reconstruida no existe"
        );
    }
    let deep = r
        .tree
        .children(0)
        .find(|&c| r.tree.name_bytes(c) == LARGA_A.as_bytes())
        .unwrap();
    assert_eq!(r.tree.path(deep), t.long_path(LARGA_A));
    assert_eq!(r.tree.stats().files, 4);
}

#[test]
fn la_cola_prioritaria_adopta_igual_con_rutas_en_forma_corta() {
    let t = TempTree::new();
    let Some((short_root, short_deep)) = setup(&t) else {
        return;
    };
    settle(&short_root);
    let mut base = opts(&short_root);
    base.priority_paths = Some(vec![]);
    let reference = scan(base).unwrap().tree;

    // Short root + short priority path; long root + short priority path; short root + long priority path.
    let cases = [
        (short_root.clone(), short_deep.clone()),
        (t.long_root(), short_deep.clone()),
        (
            short_root.clone(),
            t.long_path(&format!("{LARGA_A}/{LARGA_B}")),
        ),
    ];
    for (i, (root, prio)) in cases.into_iter().enumerate() {
        let mut o = opts(&root);
        o.priority_paths = Some(vec![prio]);
        let r = scan(o).unwrap();
        r.tree.check_invariants().unwrap();
        if let Some(d) = r.tree.first_difference(&reference) {
            panic!("caso {i}: el árbol difiere del de referencia: {d}");
        }
        assert_eq!(
            r.summary.priority.len(),
            1,
            "caso {i}: la ruta prioritaria no se aceptó"
        );
        let p = &r.summary.priority[0];
        assert!(p.done, "caso {i}");
        assert_eq!(p.files, 2, "caso {i}: archivo-uno y archivo-dos");
        assert_eq!(
            dunce::simplified(&p.path),
            t.long_path(&format!("{LARGA_A}/{LARGA_B}")),
            "caso {i}: ruta canónica en forma larga"
        );
    }
}
