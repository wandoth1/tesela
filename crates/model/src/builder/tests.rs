//! `TreeBuilder` tests on seeded synthetic trees.
//!
//! A logical tree (`Item`) is emitted as batches with different provisional ids in each permutation (as the walker
//! would do depending on thread scheduling) and compared with an independent recursive calculation.

use super::*;
use crate::flags;
use crate::record::{Batch, EntryData};
use crate::tree::NodeId;

const DAY: u32 = 86_400;
const NOW: u32 = 4_000_000_000;

/// SplitMix64: minimal, reproducible pseudorandom generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, self.below(i + 1));
        }
    }
}

#[derive(Clone)]
struct Item {
    name: Vec<u8>,
    dir: Option<Vec<Item>>,
    alloc: u64,
    logical: u64,
    mtime: u32,
    file_id: u128,
    flags: u16,
}

impl Item {
    fn dir(name: &str, children: Vec<Item>) -> Item {
        Item {
            name: name.into(),
            dir: Some(children),
            alloc: 0,
            logical: 0,
            mtime: 0,
            file_id: 0,
            flags: flags::DIR,
        }
    }
    fn file(name: &str, alloc: u64) -> Item {
        Item {
            name: name.into(),
            dir: None,
            alloc,
            logical: alloc,
            mtime: NOW - 400 * DAY,
            file_id: 0,
            flags: 0,
        }
    }
    fn with_id(mut self, id: u128) -> Item {
        self.file_id = id;
        self
    }
    fn with_mtime(mut self, days_old: u32) -> Item {
        self.mtime = NOW - days_old * DAY;
        self
    }
    fn own_dir_alloc(mut self, a: u64) -> Item {
        self.alloc = a;
        self.logical = a;
        self
    }
}

/// Synthetic tree with sibling size ties and hard links spread across different directories.
fn generate(seed: u64) -> Item {
    let mut rng = Rng(seed);
    let mut links: Vec<(u128, u64, u32)> = Vec::new();
    let mut next_id = 1u128;
    fn build(
        rng: &mut Rng,
        depth: u32,
        links: &mut Vec<(u128, u64, u32)>,
        next_id: &mut u128,
        name: String,
    ) -> Item {
        let mut children = Vec::new();
        let n = if depth >= 4 { 0 } else { rng.below(7) };
        for i in 0..n {
            let label = ["a", "b", "video", "Z", "doc"][rng.below(5)];
            if depth < 3 && rng.chance(35) {
                children.push(build(
                    rng,
                    depth + 1,
                    links,
                    next_id,
                    format!("{label}d{i}"),
                ));
            } else {
                let ext = ["mp4", "jpg", "txt", "zip", "exe", "xyz", ""][rng.below(7)];
                let nm = if ext.is_empty() {
                    format!("{label}f{i}")
                } else {
                    format!("{label}f{i}.{ext}")
                };
                // Few distinct sizes: creates sibling ties.
                let alloc = [0u64, 4096, 4096, 8192, 12288][rng.below(5)];
                let mut f = Item::file(&nm, alloc).with_mtime(rng.below(2000) as u32);
                if rng.chance(15) && !links.is_empty() {
                    let (id, a, m) = links[rng.below(links.len())];
                    f.file_id = id;
                    f.alloc = a;
                    f.logical = a;
                    f.mtime = NOW - m * DAY;
                } else if rng.chance(15) {
                    f.file_id = *next_id;
                    links.push((*next_id, alloc, (NOW - f.mtime) / DAY));
                    *next_id += 1;
                }
                children.push(f);
            }
        }
        let own = [0u64, 0, 4096][rng.below(3)];
        Item::dir(&name, children).own_dir_alloc(own)
    }
    build(&mut rng, 0, &mut links, &mut next_id, "ROOT".into())
}

/// Emit an `Item` as batches with provisional ids (and arrival order) shuffled by `perm_seed`.
/// Directories in `skip` (paths with `\`) do not deliver a batch (they remain `INCOMPLETE`).
fn emit(root: &Item, perm_seed: u64, skip: &[&str]) -> Vec<Batch> {
    let mut rng = Rng(perm_seed);
    let mut root_batch = Batch::detached(0);
    root_batch.push(
        0,
        &root.name,
        EntryData {
            flags: flags::DIR,
            alloc: root.alloc,
            logical: root.logical,
            ..EntryData::default()
        },
    );
    let mut batches = vec![root_batch];
    let mut counter = 1u32;
    let mut frontier: Vec<(&Item, u32, String)> = vec![(root, 0, String::new())];
    while !frontier.is_empty() {
        let k = rng.below(frontier.len());
        let (item, id, path) = frontier.swap_remove(k);
        let Some(children) = &item.dir else { continue };
        if skip.contains(&path.as_str()) {
            continue;
        }
        let base = counter + rng.below(3) as u32; // huecos entre lotes
        counter = base + children.len() as u32;
        let mut order: Vec<usize> = (0..children.len()).collect();
        rng.shuffle(&mut order);
        let mut b = Batch::listing(id, base);
        for &ci in &order {
            let c = &children[ci];
            let cid = b.push(
                id,
                &c.name,
                EntryData {
                    flags: c.flags,
                    alloc: c.alloc,
                    logical: c.logical,
                    mtime: c.mtime,
                    file_id: c.file_id,
                },
            );
            if c.dir.is_some() && c.flags & flags::REPARSE == 0 {
                let p = if path.is_empty() {
                    String::from_utf8_lossy(&c.name).into()
                } else {
                    format!("{path}\\{}", String::from_utf8_lossy(&c.name))
                };
                frontier.push((c, cid, p));
            }
        }
        batches.push(b);
    }
    rng.shuffle(&mut batches);
    batches
}

fn build(batches: Vec<Batch>, volume_used: Option<u64>) -> Tree {
    let mut b = TreeBuilder::new();
    for batch in batches {
        b.add_batch(batch).expect("lote válido");
    }
    b.finish(&FinishOptions {
        now_unix: NOW,
        dedup_hard_links: true,
        volume_used,
    })
}

fn build_item(root: &Item, perm_seed: u64) -> Tree {
    build(emit(root, perm_seed, &[]), None)
}

// --- Independent oracle --------------------------------------------------------------------------------------------

/// Which links retain their size under the plan's rule: smallest full path in bytewise lexicographic order.
fn losers(root: &Item) -> std::collections::HashSet<Vec<u8>> {
    fn walk(it: &Item, path: Vec<u8>, out: &mut Vec<(u128, Vec<u8>)>) {
        if let Some(ch) = &it.dir {
            for c in ch {
                let mut p = path.clone();
                if !p.is_empty() {
                    p.push(b'\\');
                }
                p.extend_from_slice(&c.name);
                if c.dir.is_some() {
                    walk(c, p, out);
                } else if c.file_id != 0 {
                    out.push((c.file_id, p));
                }
            }
        }
    }
    let mut all = Vec::new();
    walk(root, Vec::new(), &mut all);
    let mut by_id: std::collections::BTreeMap<u128, Vec<Vec<u8>>> = Default::default();
    for (id, p) in all {
        by_id.entry(id).or_default().push(p);
    }
    let mut out = std::collections::HashSet::new();
    for (_, mut paths) in by_id {
        paths.sort();
        out.extend(paths.into_iter().skip(1));
    }
    out
}

/// Compare the tree with the recursively calculated expectation. Returns (alloc, logical, files) for the subtree.
fn check_item(
    tree: &Tree,
    id: NodeId,
    it: &Item,
    path: Vec<u8>,
    losers: &std::collections::HashSet<Vec<u8>>,
) -> (u64, u64, u64) {
    let node = tree.node(id);
    assert_eq!(tree.name_bytes(id), it.name.as_slice());
    let Some(children) = &it.dir else {
        let lost = losers.contains(&path);
        let (a, l) = if lost { (0, 0) } else { (it.alloc, it.logical) };
        assert_eq!(
            (node.alloc, node.logical),
            (a, l),
            "archivo {:?}",
            String::from_utf8_lossy(&path)
        );
        assert_eq!(node.flags & flags::HARDLINK != 0, lost);
        return (a, l, 1);
    };
    assert_eq!(node.child_count as usize, children.len());
    let (mut alloc, mut logical, mut files) = (it.alloc, it.logical, 0);
    for c in children {
        let cid = tree.child_by_name(id, &c.name).expect("hijo presente");
        let mut p = path.clone();
        if !p.is_empty() {
            p.push(b'\\');
        }
        p.extend_from_slice(&c.name);
        let (a, l, f) = check_item(tree, cid, c, p, losers);
        alloc += a;
        logical += l;
        files += f;
    }
    assert_eq!(
        (node.alloc, node.logical),
        (alloc, logical),
        "directorio {:?}",
        String::from_utf8_lossy(&path)
    );
    assert_eq!(node.files_below as u64, files);
    (alloc, logical, files)
}

// --- Tests ---------------------------------------------------------------------------------------------------------

#[test]
fn los_tamanos_de_las_estructuras_son_los_del_plan() {
    assert_eq!(size_of::<Node>(), 40);
    assert_eq!(size_of::<Record>(), 32);
    assert_eq!(size_of::<Hist>(), 14);
}

#[test]
fn invariantes_y_totales_con_muchas_semillas() {
    let mut ties = 0usize;
    for seed in 0..300 {
        let root = generate(seed);
        let tree = build_item(&root, seed ^ 0xABCD);
        tree.check_invariants()
            .unwrap_or_else(|e| panic!("semilla {seed}: {e}"));
        let l = losers(&root);
        let (alloc, logical, files) = check_item(&tree, 0, &root, Vec::new(), &l);
        assert_eq!(tree.node(0).alloc, alloc);
        assert_eq!(tree.node(0).logical, logical);
        assert_eq!(tree.stats().files, files);
        assert_eq!(tree.stats().hardlink_extra_entries as usize, l.len());
        assert_eq!(tree.stats().orphans, 0);
        // Generator sanity check: some siblings have the same alloc (otherwise the determinism test proves nothing).
        for id in 0..tree.len() as u32 {
            let ch: Vec<_> = tree.children(id).collect();
            ties += ch
                .windows(2)
                .filter(|w| tree.node(w[0]).alloc == tree.node(w[1]).alloc)
                .count();
        }
    }
    assert!(
        ties > 500,
        "el generador debe producir empates entre hermanos ({ties})"
    );
}

#[test]
fn hijos_contiguos_y_ordenados_por_alloc_descendente() {
    let root = Item::dir(
        "ROOT",
        vec![
            Item::file("pequeno", 4096),
            Item::dir(
                "carpeta",
                vec![Item::file("x", 20000), Item::file("y", 100)],
            ),
            Item::file("mediano", 8192),
            Item::file("grande", 1 << 30),
        ],
    );
    let tree = build_item(&root, 1);
    tree.check_invariants().unwrap();
    let names: Vec<_> = tree
        .children(0)
        .map(|c| tree.name_lossy(c).into_owned())
        .collect();
    assert_eq!(names, ["grande", "carpeta", "mediano", "pequeno"]);
    let kids = tree.children(0);
    assert_eq!(kids.end - kids.start, 4);
    assert!(kids.clone().all(|c| tree.node(c).parent == 0));
}

#[test]
fn empates_de_tamano_se_deshacen_por_nombre() {
    let root = Item::dir(
        "ROOT",
        vec![
            Item::file("c", 4096),
            Item::file("a", 4096),
            Item::file("b", 4096),
        ],
    );
    for seed in 0..10 {
        let tree = build_item(&root, seed);
        let names: Vec<_> = tree
            .children(0)
            .map(|c| tree.name_lossy(c).into_owned())
            .collect();
        assert_eq!(names, ["a", "b", "c"]);
    }
}

#[test]
fn la_suma_de_los_hijos_es_la_del_padre_mas_lo_propio() {
    let root = Item::dir(
        "ROOT",
        vec![
            Item::dir("d", vec![Item::file("f1", 5000), Item::file("f2", 7000)])
                .own_dir_alloc(4096),
            Item::file("g", 100),
        ],
    )
    .own_dir_alloc(8192);
    let tree = build_item(&root, 3);
    let d = tree.child_by_name(0, b"d").unwrap();
    assert_eq!(tree.node(d).alloc, 4096 + 5000 + 7000);
    assert_eq!(tree.node(0).alloc, 8192 + 4096 + 5000 + 7000 + 100);
    assert_eq!(tree.node(0).files_below, 3);
    assert_eq!(tree.node(d).files_below, 2);
}

#[test]
fn files_below_cuenta_archivos_no_directorios() {
    let root = Item::dir(
        "ROOT",
        vec![
            Item::dir("vacia", vec![]),
            Item::dir(
                "a",
                vec![
                    Item::dir("b", vec![Item::file("x", 1), Item::file("y", 1)]),
                    Item::file("z", 1),
                ],
            ),
        ],
    );
    let tree = build_item(&root, 9);
    let a = tree.child_by_name(0, b"a").unwrap();
    let b = tree.child_by_name(a, b"b").unwrap();
    let vacia = tree.child_by_name(0, b"vacia").unwrap();
    assert_eq!(tree.node(0).files_below, 3);
    assert_eq!(tree.node(a).files_below, 3);
    assert_eq!(tree.node(b).files_below, 2);
    assert_eq!(tree.node(vacia).files_below, 0);
    assert_eq!(tree.stats().dirs, 4);
    tree.check_invariants().unwrap();
}

#[test]
fn dedup_determinista_con_ids_provisionales_y_orden_barajados() {
    for seed in 0..60 {
        let root = generate(seed + 1000);
        let a = build_item(&root, 111);
        let b = build_item(&root, 222);
        let c = build_item(&root, 333);
        assert!(
            a.structural_eq(&b),
            "semilla {seed}: dos ejecuciones difieren"
        );
        assert!(
            a.structural_eq(&c),
            "semilla {seed}: dos ejecuciones difieren"
        );
    }
}

#[test]
fn el_enlace_duro_que_conserva_es_el_de_ruta_menor() {
    let root = Item::dir(
        "ROOT",
        vec![
            Item::dir("b", vec![Item::file("archivo", 10_000).with_id(7)]),
            Item::dir("a", vec![Item::file("zz", 10_000).with_id(7)]),
            Item::dir(
                "m",
                vec![
                    Item::file("otro", 5000).with_id(8),
                    Item::file("copia", 5000).with_id(8),
                ],
            ),
        ],
    );
    for seed in 0..20 {
        let tree = build_item(&root, seed);
        let a = tree.child_by_name(0, b"a").unwrap();
        let zz = tree.child_by_name(a, b"zz").unwrap();
        let b = tree.child_by_name(0, b"b").unwrap();
        let arch = tree.child_by_name(b, b"archivo").unwrap();
        assert_eq!(tree.node(zz).alloc, 10_000);
        assert_eq!(tree.node(zz).flags & flags::HARDLINK, 0);
        assert_eq!((tree.node(arch).alloc, tree.node(arch).logical), (0, 0));
        assert_eq!(tree.node(arch).flags & flags::HARDLINK, flags::HARDLINK);
        // "m\copia" < "m\otro": copia is retained.
        let m = tree.child_by_name(0, b"m").unwrap();
        assert_eq!(
            tree.node(tree.child_by_name(m, b"copia").unwrap()).alloc,
            5000
        );
        assert_eq!(tree.node(tree.child_by_name(m, b"otro").unwrap()).alloc, 0);
        assert_eq!(tree.node(0).alloc, 10_000 + 5000);
        assert_eq!(tree.stats().hardlink_groups, 2);
        assert_eq!(tree.stats().hardlink_extra_entries, 2);
        assert_eq!(tree.stats().hardlink_alloc_saved, 15_000);
        assert_eq!(tree.stats().hardlink_logical_saved, 15_000);
        assert_eq!(tree.stats().files, 4);
    }
}

#[test]
fn la_ruta_se_compara_en_bytes_con_la_barra_inversa() {
    // Byte order: "a b\x" < "a\x" because ' ' (0x20) < '\\' (0x5C). Component-wise ordering would say otherwise.
    let root = Item::dir(
        "ROOT",
        vec![
            Item::dir("a", vec![Item::file("x", 4096).with_id(1)]),
            Item::dir("a b", vec![Item::file("x", 4096).with_id(1)]),
        ],
    );
    let tree = build_item(&root, 5);
    let ab = tree.child_by_name(0, b"a b").unwrap();
    let a = tree.child_by_name(0, b"a").unwrap();
    assert_eq!(tree.node(tree.child_by_name(ab, b"x").unwrap()).alloc, 4096);
    assert_eq!(tree.node(tree.child_by_name(a, b"x").unwrap()).alloc, 0);
}

#[test]
fn sin_dedup_no_se_tocan_los_enlaces() {
    let root = Item::dir(
        "ROOT",
        vec![
            Item::file("a", 4096).with_id(1),
            Item::file("b", 4096).with_id(1),
        ],
    );
    let mut b = TreeBuilder::new();
    for batch in emit(&root, 1, &[]) {
        b.add_batch(batch).unwrap();
    }
    let t = b.finish(&FinishOptions {
        now_unix: NOW,
        dedup_hard_links: false,
        volume_used: None,
    });
    assert_eq!(t.node(0).alloc, 8192);
    assert_eq!(t.stats().hardlink_extra_entries, 0);
}

#[test]
fn los_file_id_de_128_bits_solo_colisionan_si_son_iguales_enteros() {
    let lo = 0x1234u128;
    let root = Item::dir(
        "ROOT",
        vec![
            Item::file("a", 4096).with_id(lo),
            Item::file("b", 4096).with_id(lo | (1u128 << 64)), // misma mitad baja, distinta alta: otro archivo
            Item::file("c", 4096).with_id(lo | (1u128 << 64)), // igual que b: enlace duro
            Item::file("d", 4096),                             // sin FileId: nunca se deduplica
            Item::file("e", 4096),
        ],
    );
    let tree = build_item(&root, 2);
    assert_eq!(tree.node(0).alloc, 4096 * 4);
    assert_eq!(tree.stats().hardlink_extra_entries, 1);
    assert_eq!(tree.node(tree.child_by_name(0, b"c").unwrap()).alloc, 0);
    assert_eq!(tree.node(tree.child_by_name(0, b"b").unwrap()).alloc, 4096);
}

#[test]
fn los_directorios_nunca_participan_en_la_dedup() {
    let mut d1 = Item::dir("d1", vec![]);
    let mut d2 = Item::dir("d2", vec![]);
    d1.file_id = 5;
    d2.file_id = 5;
    let tree = build_item(&Item::dir("ROOT", vec![d1, d2]), 1);
    assert_eq!(tree.stats().hardlink_extra_entries, 0);
}

// --- Histograms -----------------------------------------------------------------------------------------------------

fn hist_of(files: Vec<Item>) -> Hist {
    let tree = build_item(&Item::dir("ROOT", files), 1);
    *tree.hist(0)
}

#[test]
fn umbral_del_50_por_ciento_de_tipo_es_estricto() {
    // Exactly 50%: Mixed.
    let h = hist_of(vec![Item::file("a.mp4", 500), Item::file("b.jpg", 500)]);
    assert_eq!(h.dominant_type(), None);
    assert_eq!(h.type_pct[FileType::Video as usize], 50);
    assert_eq!(h.type_pct.iter().map(|&x| x as u32).sum::<u32>(), 100);
    // 50.05%: video.
    let h = hist_of(vec![Item::file("a.mp4", 501), Item::file("b.jpg", 499)]);
    assert_eq!(h.dominant_type(), Some(FileType::Video));
    // 51% in "other" also counts as the dominant type.
    let h = hist_of(vec![Item::file("a.xyz", 51), Item::file("b.jpg", 49)]);
    assert_eq!(h.dominant_type(), Some(FileType::Other));
}

#[test]
fn el_umbral_se_calcula_sobre_u64_y_no_sobre_los_porcentajes_redondeados() {
    // 50.0000001%, but the rounded percentage would be 50: dominance uses the u64 values.
    let big = 1u64 << 40;
    let h = hist_of(vec![Item::file("a.mp4", big + 1), Item::file("b.jpg", big)]);
    assert_eq!(h.dominant_type(), Some(FileType::Video));
    assert_eq!(h.type_pct[0], 50);
    // Values near the u64 limit do not overflow.
    let h = hist_of(vec![
        Item::file("a.mp4", 1 << 62),
        Item::file("b.jpg", (1 << 62) - 1),
    ]);
    assert_eq!(h.dominant_type(), Some(FileType::Video));
}

#[test]
fn umbral_del_80_por_ciento_de_antiguedad() {
    // Exactly 80% in the oldest band (> 3 years): it qualifies.
    let h = hist_of(vec![
        Item::file("a.txt", 800).with_mtime(2000),
        Item::file("b.txt", 200).with_mtime(1),
    ]);
    assert_eq!(h.dominant_age(), Some(4));
    // 79% in the oldest band: not enough; the recent 21% pulls the result to the recent band.
    let h = hist_of(vec![
        Item::file("a.txt", 790).with_mtime(2000),
        Item::file("b.txt", 210).with_mtime(1),
    ]);
    assert_eq!(h.dominant_age(), Some(0));
    // 70% > 3 years + 20% in 1-3 years: 90% is at least 1 year old -> band 3.
    let h = hist_of(vec![
        Item::file("a.txt", 700).with_mtime(2000),
        Item::file("b.txt", 200).with_mtime(500),
        Item::file("c.txt", 100).with_mtime(10),
    ]);
    assert_eq!(h.dominant_age(), Some(3));
    assert_eq!(h.age_pct.iter().map(|&x| x as u32).sum::<u32>(), 100);
    // All recent.
    let h = hist_of(vec![Item::file("a.txt", 100).with_mtime(2)]);
    assert_eq!(h.dominant_age(), Some(0));
}

#[test]
fn los_histogramas_son_agregados_de_todo_el_subarbol_y_los_archivos_no_tienen() {
    let root = Item::dir(
        "ROOT",
        vec![
            Item::dir(
                "sub",
                vec![Item::file("v.mkv", 900), Item::file("w.mkv", 100)],
            ),
            Item::file("x.pdf", 1000),
        ],
    );
    let tree = build_item(&root, 4);
    let sub = tree.child_by_name(0, b"sub").unwrap();
    assert_eq!(tree.hist(sub).dominant_type(), Some(FileType::Video));
    assert_eq!(tree.hist(0).type_pct[FileType::Video as usize], 50);
    assert_eq!(tree.hist(0).type_pct[FileType::Document as usize], 50);
    assert_eq!(tree.hist(0).dominant_type(), None);
    let v = tree.child_by_name(sub, b"v.mkv").unwrap();
    assert_eq!(*tree.hist(v), Hist::EMPTY);
}

#[test]
fn un_directorio_sin_bytes_no_tiene_dominantes_y_los_dedup_no_pesan() {
    let h = hist_of(vec![Item::file("a.mp4", 0), Item::file("b.txt", 0)]);
    assert_eq!(h, Hist::EMPTY);
    // The duplicate link (alloc 0 after deduplication) does not contribute to histograms.
    let root = Item::dir(
        "ROOT",
        vec![
            Item::file("a.mp4", 1000).with_id(9),
            Item::file("b.txt", 1000).with_id(9),
            Item::file("c.txt", 100),
        ],
    );
    let tree = build_item(&root, 8);
    assert_eq!(tree.hist(0).dominant_type(), Some(FileType::Video));
}

#[test]
fn los_porcentajes_suman_100() {
    for seed in 0..200 {
        let tree = build_item(&generate(seed), seed);
        for id in 0..tree.len() as u32 {
            let h = tree.hist(id);
            if h.dominant_age().is_some() {
                assert_eq!(h.type_pct.iter().map(|&x| x as u32).sum::<u32>(), 100);
                assert_eq!(h.age_pct.iter().map(|&x| x as u32).sum::<u32>(), 100);
            }
        }
    }
}

// --- INCOMPLETE, ERROR, orphans, synthetic node, edge cases ---------------------------------------------------------

#[test]
fn directorios_sin_lote_son_incompletos_pero_los_vacios_y_los_reparse_no() {
    let mut junction = Item::dir("junction", vec![Item::file("no-se-sigue", 99)]);
    junction.flags |= flags::REPARSE;
    junction.alloc = 0;
    let root = Item::dir(
        "ROOT",
        vec![
            Item::dir("vacia", vec![]),
            Item::dir("pendiente", vec![Item::file("f", 5000)]),
            Item::dir("hecha", vec![Item::file("g", 100)]),
            junction,
        ],
    );
    // `pendiente` delivers no batch (scan stopped); its contents never arrive.
    let tree = build(emit(&root, 1, &["pendiente"]), None);
    tree.check_invariants().unwrap();
    let flag = |n: &[u8]| tree.node(tree.child_by_name(0, n).unwrap()).flags;
    assert_eq!(flag(b"pendiente") & flags::INCOMPLETE, flags::INCOMPLETE);
    assert_eq!(flag(b"vacia") & flags::INCOMPLETE, 0);
    assert_eq!(flag(b"hecha") & flags::INCOMPLETE, 0);
    let j = flag(b"junction");
    assert_eq!(
        j & (flags::REPARSE | flags::DIR),
        flags::REPARSE | flags::DIR
    );
    assert_eq!(j & flags::INCOMPLETE, 0);
    assert_eq!(
        tree.node(tree.child_by_name(0, b"junction").unwrap())
            .child_count,
        0
    );
    assert_eq!(tree.stats().incomplete_dirs, 1);
    assert_eq!(tree.stats().reparse_dirs, 1);
    assert_eq!(tree.node(0).files_below, 1);
}

#[test]
fn un_listado_fallido_marca_error_y_no_incompleto() {
    let mut b = TreeBuilder::new();
    let mut root = Batch::detached(0);
    root.push(
        0,
        b"ROOT",
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    b.add_batch(root).unwrap();
    let mut l = Batch::listing(0, 1);
    l.push(
        0,
        b"denegada",
        EntryData {
            flags: flags::DIR,
            alloc: 4096,
            ..EntryData::default()
        },
    );
    b.add_batch(l).unwrap();
    b.add_batch(Batch::failed(1, 5)).unwrap();
    let t = b.finish(&FinishOptions::new(NOW));
    let d = t.child_by_name(0, b"denegada").unwrap();
    assert_eq!(
        t.node(d).flags & (flags::ERROR | flags::INCOMPLETE),
        flags::ERROR
    );
    assert_eq!(t.stats().error_dirs, 1);
    assert_eq!(t.stats().incomplete_dirs, 0);
}

#[test]
fn las_entradas_sin_padre_llegado_son_huerfanas_y_no_entran_en_el_arbol() {
    let mut b = TreeBuilder::new();
    let mut root = Batch::detached(0);
    root.push(
        0,
        b"ROOT",
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    root.push(
        0,
        b"normal",
        EntryData {
            alloc: 100,
            ..EntryData::default()
        },
    );
    b.add_batch(root).unwrap();
    let mut o = Batch::listing(50, 100); // directory 50 never arrived
    o.push(
        50,
        b"huerfano",
        EntryData {
            alloc: 7,
            ..EntryData::default()
        },
    );
    b.add_batch(o).unwrap();
    let t = b.finish(&FinishOptions::new(NOW));
    assert_eq!(t.len(), 2);
    assert_eq!(t.stats().orphans, 1);
    assert_eq!(t.node(0).alloc, 100);
    t.check_invariants().unwrap();
}

#[test]
fn espacio_no_explicado_es_un_hijo_sintetico_siempre_el_ultimo() {
    let root = Item::dir("ROOT", vec![Item::file("a", 4096), Item::file("b", 100)]);
    let tree = build(emit(&root, 1, &[]), Some(1 << 30));
    tree.check_invariants().unwrap();
    let kids: Vec<_> = tree.children(0).collect();
    let last = *kids.last().unwrap();
    assert_eq!(tree.node(last).flags, flags::SYSTEM_INFO);
    assert_eq!(tree.name_bytes(last), SYSTEM_INFO_NAME);
    assert_eq!(tree.node(last).alloc, (1 << 30) - 4196);
    assert!(
        tree.node(last).alloc > tree.node(kids[0]).alloc,
        "aun siendo el mayor va el último"
    );
    assert_eq!(tree.node(0).alloc, 1 << 30);
    assert_eq!(tree.node(0).files_below, 2, "el sintético no es un archivo");
    assert_eq!(tree.stats().unexplained_alloc, (1 << 30) - 4196);
    assert_eq!(tree.stats().scanned_alloc, 4196);
}

#[test]
fn el_espacio_no_explicado_se_recorta_a_cero() {
    let root = Item::dir("ROOT", vec![Item::file("a", 4096)]);
    // Used space is less than scanned (e.g. due to deduplication or deleted files): no node.
    let tree = build(emit(&root, 1, &[]), Some(100));
    assert_eq!(tree.len(), 2);
    assert_eq!(tree.stats().unexplained_alloc, 0);
    let tree = build(emit(&root, 1, &[]), Some(4096));
    assert_eq!(tree.len(), 2);
    let tree = build(emit(&root, 1, &[]), None);
    assert_eq!(tree.len(), 2);
}

#[test]
fn nombres_con_sustitutos_desparejados_dan_la_ruta_exacta() {
    let weird: Vec<u16> = vec![b'x' as u16, 0xD800, b'y' as u16, 0xDC00, 0xD83D, 0xDE00]; // 2 desparejados y 1 par
    let mut b = TreeBuilder::new();
    let mut root = Batch::detached(0);
    let root_name: Vec<u16> = r"C:\raiz".encode_utf16().collect();
    root.push_utf16(
        0,
        &root_name,
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    b.add_batch(root).unwrap();
    let mut l = Batch::listing(0, 1);
    l.push_utf16(
        0,
        &weird,
        EntryData {
            alloc: 1,
            ..EntryData::default()
        },
    );
    b.add_batch(l).unwrap();
    let t = b.finish(&FinishOptions::new(NOW));
    let mut expected = root_name.clone();
    expected.push(b'\\' as u16);
    expected.extend_from_slice(&weird);
    assert_eq!(t.path_utf16(1), expected);
    assert_eq!(t.name_utf16(1), weird);
    assert!(t.path_lossy(1).contains('\u{FFFD}'));
}

#[test]
fn la_ruta_de_la_raiz_de_unidad_no_duplica_la_barra() {
    let mut b = TreeBuilder::new();
    let mut root = Batch::detached(0);
    root.push(
        0,
        br"C:\",
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    root.push(
        0,
        b"Users",
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    b.add_batch(root).unwrap();
    let t = b.finish(&FinishOptions::new(NOW));
    assert_eq!(t.path_lossy(1), r"C:\Users");
    assert_eq!(t.path_lossy(0), r"C:\");
}

#[test]
fn un_id_repetido_es_un_error_y_no_altera_el_builder() {
    let mut b = TreeBuilder::new();
    let mut root = Batch::detached(0);
    root.push(
        0,
        b"ROOT",
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    b.add_batch(root).unwrap();
    let mut x = Batch::listing(0, 1);
    x.push(0, b"a", EntryData::default());
    b.add_batch(x).unwrap();
    let mut y = Batch::listing(0, 1);
    y.push(0, b"b", EntryData::default());
    assert_eq!(b.add_batch(y), Err(BuildError::DuplicateId(1)));
    let t = b.finish(&FinishOptions::new(NOW));
    assert_eq!(t.name_bytes(1), b"a");
}

#[test]
fn un_hueco_no_choca_con_una_entrada_posterior() {
    let mut b = TreeBuilder::new();
    let mut root = Batch::detached(0);
    root.push(
        0,
        b"ROOT",
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    b.add_batch(root).unwrap();
    let mut x = Batch::listing(0, 1);
    x.push_hole();
    x.push(0, b"b", EntryData::default());
    b.add_batch(x).unwrap();
    let mut adopt = Batch::detached(1);
    adopt.push(0, b"a", EntryData::default());
    b.add_batch(adopt).unwrap();
    let t = b.finish(&FinishOptions::new(NOW));
    assert_eq!(t.len(), 3);
    assert!(t.child_by_name(0, b"a").is_some());
}

#[test]
fn sin_raiz_se_devuelve_un_arbol_con_solo_la_raiz_incompleta() {
    let t = TreeBuilder::new().finish(&FinishOptions::new(NOW));
    assert_eq!(t.len(), 1);
    assert_eq!(t.node(0).flags & flags::INCOMPLETE, flags::INCOMPLETE);
    t.check_invariants().unwrap();
}

#[test]
fn solo_la_raiz_vacia() {
    let tree = build_item(&Item::dir("ROOT", vec![]), 1);
    assert_eq!(tree.len(), 1);
    tree.check_invariants().unwrap();
    assert_eq!(tree.stats().dirs, 1);
}

#[test]
fn un_arbol_muy_profundo_no_desborda_la_pila() {
    const DEPTH: u32 = 200_000;
    let mut b = TreeBuilder::new();
    let mut root = Batch::detached(0);
    root.push(
        0,
        b"ROOT",
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    b.add_batch(root).unwrap();
    for d in 0..DEPTH {
        let mut l = Batch::listing(d, d + 1);
        l.push(
            d,
            b"d",
            EntryData {
                flags: flags::DIR,
                alloc: 1,
                ..EntryData::default()
            },
        );
        b.add_batch(l).unwrap();
    }
    let mut leaf = Batch::listing(DEPTH, DEPTH + 1);
    leaf.push(
        DEPTH,
        b"f.txt",
        EntryData {
            alloc: 1000,
            logical: 900,
            ..EntryData::default()
        },
    );
    b.add_batch(leaf).unwrap();
    let t = b.finish(&FinishOptions::new(NOW));
    assert_eq!(t.len() as u32, DEPTH + 2);
    assert_eq!(t.node(0).alloc, 1000 + DEPTH as u64);
    assert_eq!(t.node(0).files_below, 1);
    t.check_invariants().unwrap();
}

#[test]
fn los_flags_cloud_y_reparse_se_cuentan() {
    let mut f = Item::file("placeholder.docx", 0);
    f.logical = 1 << 20;
    f.flags = flags::CLOUD | flags::REPARSE;
    let tree = build_item(&Item::dir("ROOT", vec![f, Item::file("x", 5)]), 1);
    assert_eq!(tree.stats().cloud_entries, 1);
    assert_eq!(tree.stats().reparse_files, 1);
    assert_eq!(tree.node(0).alloc, 5);
    assert_eq!(tree.node(0).logical, (1 << 20) + 5);
}

// --- Audit corrections ----------------------------------------------------------------------------------------------

#[test]
fn un_huerfano_con_ruta_menor_no_le_quita_el_tamano_a_un_enlace_del_arbol() {
    // `a.bin` belongs to a directory (id 50) that never arrived, so it is not in the tree. Its partial path ("a.bin") is
    // less than "z.bin", but it cannot win deduplication.
    let mut b = TreeBuilder::new();
    let mut root = Batch::detached(0);
    root.push(
        0,
        b"ROOT",
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    root.push(
        0,
        b"z.bin",
        EntryData {
            alloc: 4096,
            logical: 4000,
            file_id: 5,
            ..EntryData::default()
        },
    );
    b.add_batch(root).unwrap();
    let mut orphan = Batch::listing(50, 100);
    orphan.push(
        50,
        b"a.bin",
        EntryData {
            alloc: 4096,
            logical: 4000,
            file_id: 5,
            ..EntryData::default()
        },
    );
    b.add_batch(orphan).unwrap();
    let t = b.finish(&FinishOptions::new(NOW));
    assert_eq!(t.stats().orphans, 1);
    let z = t.child_by_name(0, b"z.bin").unwrap();
    assert_eq!((t.node(z).alloc, t.node(z).logical), (4096, 4000));
    assert_eq!(t.node(z).flags & flags::HARDLINK, 0);
    assert_eq!(t.stats().hardlink_extra_entries, 0);
    assert_eq!(t.node(0).alloc, 4096);
}

#[test]
fn los_ancestros_de_un_directorio_incompleto_lo_saben() {
    let root = Item::dir(
        "ROOT",
        vec![
            Item::dir(
                "a",
                vec![Item::dir(
                    "b",
                    vec![Item::dir("pendiente", vec![Item::file("f", 9)])],
                )],
            ),
            Item::dir("otra", vec![Item::file("g", 5)]),
        ],
    );
    let tree = build(emit(&root, 3, &[r"a\b\pendiente"]), None);
    tree.check_invariants().unwrap();
    let a = tree.child_by_name(0, b"a").unwrap();
    let b = tree.child_by_name(a, b"b").unwrap();
    let p = tree.child_by_name(b, b"pendiente").unwrap();
    let otra = tree.child_by_name(0, b"otra").unwrap();
    let fl = |id| tree.node(id).flags;
    assert_eq!(fl(p) & flags::INCOMPLETE, flags::INCOMPLETE);
    assert_eq!(
        fl(p) & flags::INCOMPLETE_BELOW,
        0,
        "el propio directorio no se marca como 'debajo'"
    );
    for anc in [a, b, 0] {
        assert_eq!(fl(anc) & flags::INCOMPLETE_BELOW, flags::INCOMPLETE_BELOW);
        assert_eq!(
            fl(anc) & flags::INCOMPLETE,
            0,
            "los ancestros se listaron enteros"
        );
    }
    assert_eq!(fl(otra) & (flags::INCOMPLETE | flags::INCOMPLETE_BELOW), 0);
}

#[test]
fn con_directorios_incompletos_no_se_calcula_el_espacio_no_explicado() {
    let root = Item::dir(
        "ROOT",
        vec![
            Item::dir("pendiente", vec![Item::file("f", 9)]),
            Item::file("a", 4096),
        ],
    );
    let tree = build(emit(&root, 1, &["pendiente"]), Some(1 << 30));
    assert_eq!(tree.stats().incomplete_dirs, 1);
    assert_eq!(
        tree.stats().unexplained_alloc,
        0,
        "defensa en profundidad del builder"
    );
    assert!(
        tree.nodes()
            .iter()
            .all(|n| n.flags & flags::SYSTEM_INFO == 0)
    );
}

#[test]
fn un_mtime_desconocido_no_cuenta_como_el_mas_antiguo() {
    let unknown = |name: &str, alloc: u64| {
        let mut f = Item::file(name, alloc);
        f.mtime = 0;
        f
    };
    // Only unknown dates: there is no age band.
    let h = hist_of(vec![unknown("a.txt", 100), unknown("b.txt", 100)]);
    assert_eq!(h.dominant_age(), None);
    assert_eq!(h.age_pct, [0; 5]);
    assert_eq!(
        h.type_pct[FileType::Document as usize],
        100,
        "el tipo sí cuenta"
    );
    // With known dates, unknown dates are excluded from the 80%: 100 B recent and 5000 B undated -> recent.
    let h = hist_of(vec![
        unknown("a.txt", 5000),
        Item::file("b.txt", 100).with_mtime(1),
    ]);
    assert_eq!(h.dominant_age(), Some(0));
    assert_eq!(h.age_pct.iter().map(|&x| x as u32).sum::<u32>(), 100);
    let h = hist_of(vec![
        unknown("a.txt", 5000),
        Item::file("b.txt", 100).with_mtime(2000),
    ]);
    assert_eq!(h.dominant_age(), Some(4));
}

#[test]
fn ts_y_mts_no_son_video() {
    assert_eq!(FileType::classify(b"app.ts"), FileType::Other);
    assert_eq!(FileType::classify(b"index.mts"), FileType::Other);
    assert_eq!(FileType::classify(b"clip.m2ts"), FileType::Video);
}

#[test]
fn los_acumuladores_saturan_en_vez_de_desbordar() {
    let big = u64::MAX / 2 + 10;
    let root = Item::dir(
        "ROOT",
        vec![
            Item::file("a.mp4", big),
            Item::file("b.mp4", big),
            Item::dir("d", vec![Item::file("c.mp4", big)]),
        ],
    );
    let tree = build_item(&root, 1); // in debug mode, `+` would overflow and panic
    assert_eq!(tree.node(0).alloc, u64::MAX);
    assert_eq!(tree.hist(0).dominant_type(), Some(FileType::Video));
}

#[test]
fn las_carpetas_de_la_nube_con_reparse_si_se_recorren() {
    // Directory with REPARSE|CLOUD (OneDrive): traversed and counted as a directory; INCOMPLETE if its batch never arrives.
    let cloud_dir = |name: &str, children: Vec<Item>| {
        let mut d = Item::dir(name, children);
        d.flags |= flags::REPARSE | flags::CLOUD;
        d
    };
    let root = Item::dir(
        "ROOT",
        vec![
            cloud_dir("nube", vec![Item::file("a.txt", 4096)]),
            cloud_dir("nube-pendiente", vec![Item::file("b.txt", 4096)]),
        ],
    );
    // `emit` only enters directories without REPARSE: cloud batches must be supplied manually here.
    let mut b = TreeBuilder::new();
    let mut r = Batch::detached(0);
    r.push(
        0,
        b"ROOT",
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    b.add_batch(r).unwrap();
    let mut l = Batch::listing(0, 1);
    for c in root.dir.as_ref().unwrap() {
        l.push(
            0,
            &c.name,
            EntryData {
                flags: c.flags,
                ..EntryData::default()
            },
        );
    }
    b.add_batch(l).unwrap();
    let mut inner = Batch::listing(1, 10);
    inner.push(
        1,
        b"a.txt",
        EntryData {
            alloc: 4096,
            logical: 4096,
            ..EntryData::default()
        },
    );
    b.add_batch(inner).unwrap();
    let t = b.finish(&FinishOptions::new(NOW));
    let nube = t.child_by_name(0, b"nube").unwrap();
    let pend = t.child_by_name(0, b"nube-pendiente").unwrap();
    assert_eq!(t.node(nube).alloc, 4096, "su contenido cuenta");
    assert_eq!(t.node(nube).flags & flags::INCOMPLETE, 0);
    assert_eq!(t.node(pend).flags & flags::INCOMPLETE, flags::INCOMPLETE);
    assert_eq!(
        t.stats().reparse_dirs,
        0,
        "no son reparse points 'sin seguir'"
    );
    assert_eq!(t.stats().dirs, 3);
    assert_eq!(t.stats().incomplete_dirs, 1);
}

#[test]
fn una_carpeta_de_la_nube_sin_poblar_se_cuenta_y_queda_incompleta() {
    // The scanner does not traverse it (no batch arrives): its AllocationSize is counted and it remains CLOUD | INCOMPLETE.
    let mut b = TreeBuilder::new();
    let mut r = Batch::detached(0);
    r.push(
        0,
        b"ROOT",
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    b.add_batch(r).unwrap();
    let mut l = Batch::listing(0, 1);
    let dir_cloud = flags::DIR | flags::CLOUD;
    l.push(
        0,
        b"nube-sin-poblar",
        EntryData {
            flags: dir_cloud,
            alloc: 4096,
            logical: 4096,
            ..EntryData::default()
        },
    );
    l.push(
        0,
        b"nube-sin-poblar-reparse",
        EntryData {
            flags: dir_cloud | flags::REPARSE,
            alloc: 4096,
            ..EntryData::default()
        },
    );
    // Junction (REPARSE without CLOUD): not followed and NOT incomplete.
    l.push(
        0,
        b"junction",
        EntryData {
            flags: flags::DIR | flags::REPARSE,
            ..EntryData::default()
        },
    );
    b.add_batch(l).unwrap();
    let t = b.finish(&FinishOptions::new(NOW));
    let fl = |n: &[u8]| t.node(t.child_by_name(0, n).unwrap()).flags;
    assert_eq!(
        fl(b"nube-sin-poblar") & (flags::CLOUD | flags::INCOMPLETE),
        flags::CLOUD | flags::INCOMPLETE
    );
    assert_eq!(
        fl(b"nube-sin-poblar-reparse") & (flags::CLOUD | flags::INCOMPLETE),
        flags::CLOUD | flags::INCOMPLETE
    );
    assert_eq!(fl(b"junction") & flags::INCOMPLETE, 0);
    assert_eq!(t.node(0).alloc, 8192, "su AllocationSize cuenta");
    assert_eq!(t.stats().incomplete_dirs, 2);
    assert_eq!(
        t.stats().reparse_dirs,
        1,
        "solo el junction es un reparse sin seguir"
    );
}
