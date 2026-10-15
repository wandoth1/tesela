//! Tests del layout sobre árboles sintéticos con semilla.

use super::*;
use std::collections::VecDeque;
use std::sync::Arc;
use tesela_model::{Batch, EntryData, FinishOptions, Tree, TreeBuilder, flags};

// --- Generación de árboles ------------------------------------------------------------------------------------------

/// SplitMix64: generador pseudoaleatorio mínimo y reproducible.
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
    /// Tamaño con cola pesada (de unos KB a unos GB).
    fn size(&mut self) -> u64 {
        let e = 12 + self.below(18) as u32; // 4 KiB .. 1 GiB
        (1u64 << e) + (self.next() % (1u64 << e))
    }
}

enum Spec {
    File(String, u64),
    Dir(String, Vec<Spec>),
}

fn name_of(s: &Spec) -> &str {
    match s {
        Spec::File(n, _) | Spec::Dir(n, _) => n,
    }
}

/// Construye un `Tree` desde una especificación (la raíz es un directorio).
fn build(root: &Spec, volume_used: Option<u64>) -> Tree {
    let mut b = TreeBuilder::new();
    let mut rb = Batch::detached(0);
    rb.push(
        0,
        name_of(root).as_bytes(),
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    b.add_batch(rb).unwrap();
    let mut counter = 1u32;
    let mut queue: VecDeque<(&Spec, u32)> = VecDeque::new();
    queue.push_back((root, 0));
    while let Some((spec, id)) = queue.pop_front() {
        let Spec::Dir(_, kids) = spec else { continue };
        let mut batch = Batch::listing(id, counter);
        counter += kids.len() as u32;
        let mut ids = Vec::new();
        for k in kids {
            let (fl, alloc) = match k {
                Spec::File(_, a) => (0, *a),
                Spec::Dir(..) => (flags::DIR, 0),
            };
            let cid = batch.push(
                id,
                name_of(k).as_bytes(),
                EntryData {
                    flags: fl,
                    alloc,
                    logical: alloc,
                    ..EntryData::default()
                },
            );
            ids.push(cid);
        }
        b.add_batch(batch).unwrap();
        for (k, cid) in kids.iter().zip(ids) {
            queue.push_back((k, cid));
        }
    }
    b.finish(&FinishOptions {
        now_unix: 4_000_000_000,
        dedup_hard_links: true,
        volume_used,
    })
}

/// Carpeta raíz con `n` hijos: carpetas con archivos (profundidad 2) y archivos sueltos, tamaños con cola pesada.
fn random_spec(seed: u64, n: usize, files_per_dir: usize) -> Spec {
    let mut r = Rng(seed);
    let mut kids = Vec::new();
    for i in 0..n {
        if i % 3 == 0 {
            let files = (0..files_per_dir)
                .map(|j| Spec::File(format!("f{i}-{j}.bin"), r.size()))
                .collect();
            kids.push(Spec::Dir(format!("d{i}"), files));
        } else {
            kids.push(Spec::File(format!("a{i}.bin"), r.size()));
        }
    }
    Spec::Dir("raiz".into(), kids)
}

fn key(size: (u32, u32), depth: Depth) -> Key {
    Key {
        root: Root::node(0),
        size,
        depth,
        version: 1,
        params: Params::default(),
    }
}

fn level(l: &Layout, lv: u8) -> Vec<&Cell> {
    l.cells().iter().filter(|c| c.level == lv).collect()
}

const EPS: f32 = 1e-2;

fn assert_no_overlap(cells: &[&Cell]) {
    for (i, a) in cells.iter().enumerate() {
        for b in &cells[i + 1..] {
            let ov = a.rect.intersection_area(&b.rect);
            assert!(
                ov < 0.5,
                "solape de {ov} px² entre {:?} y {:?}",
                a.rect,
                b.rect
            );
        }
    }
}

// --- squarified -----------------------------------------------------------------------------------------------------

#[test]
fn squarify_cubre_el_rectangulo_y_reparte_proporcional() {
    let vals = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
    let total: f64 = vals.iter().sum();
    let rect = Rect::new(10.0, 20.0, 600.0, 400.0);
    let mut out = Vec::new();
    squarify(&vals, total, rect, &mut out);
    assert_eq!(out.len(), vals.len());
    let mut sum_area = 0.0;
    for (r, v) in out.iter().zip(vals) {
        let expect = rect.area() as f64 * v / total;
        assert!(
            ((r.area() as f64) - expect).abs() < expect * 1e-4 + 0.01,
            "{v}: {} frente a {expect}",
            r.area()
        );
        assert!(rect.contains_rect(r, EPS));
        sum_area += r.area() as f64;
    }
    assert!((sum_area - rect.area() as f64).abs() < 0.5);
    let refs: Vec<Cell> = out
        .iter()
        .map(|r| Cell {
            rect: *r,
            kind: CellKind::Node(0),
            level: 0,
            parent: NO_CELL,
        })
        .collect();
    assert_no_overlap(&refs.iter().collect::<Vec<_>>());
}

#[test]
fn squarify_ejemplo_del_articulo_de_bruls() {
    // Ejemplo de Bruls et al.: 6, 6, 4, 3, 2, 2, 1 en 6 x 4. La primera fila (6 y 6) son dos celdas de 3 x 2
    // (relacion 1,5); anadir el 4 daria 4 x 1,5, 4 x 1,5 y 4 x 1 (peor relacion), asi que la fila se cierra.
    let vals = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
    let mut out = Vec::new();
    squarify(&vals, 24.0, Rect::new(0.0, 0.0, 6.0, 4.0), &mut out);
    let worst = out
        .iter()
        .map(|r| (r.w / r.h).max(r.h / r.w))
        .fold(0.0f32, f32::max);
    assert!(worst < 3.0, "peor relación de aspecto {worst}");
    assert_eq!((out[0].w, out[0].h), (3.0, 2.0));
    assert_eq!((out[1].w, out[1].h), (3.0, 2.0));
    assert!((out[0].w * out[0].h - 6.0).abs() < 1e-4);
}

#[test]
fn squarify_con_hueco_final_no_cambia_la_escala() {
    // `total` mayor que la suma: lo que sobra queda sin repartir y las áreas siguen siendo proporcionales a `total`.
    let vals = [50.0, 30.0];
    let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
    let mut out = Vec::new();
    squarify(&vals, 100.0, rect, &mut out);
    assert!((out[0].area() - 5000.0).abs() < 1.0);
    assert!((out[1].area() - 3000.0).abs() < 1.0);
}

#[test]
fn squarify_valores_nulos_y_rectangulo_vacio() {
    let mut out = Vec::new();
    squarify(
        &[3.0, 0.0, 0.0],
        3.0,
        Rect::new(0.0, 0.0, 50.0, 50.0),
        &mut out,
    );
    assert_eq!(out.len(), 3);
    assert_eq!(out[1].area(), 0.0);
    let mut out = Vec::new();
    squarify(&[1.0, 2.0], 3.0, Rect::new(0.0, 0.0, 0.0, 10.0), &mut out);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|r| r.area() == 0.0));
    let mut out = Vec::new();
    squarify(&[], 0.0, Rect::new(0.0, 0.0, 10.0, 10.0), &mut out);
    assert!(out.is_empty());
}

// --- Layout ---------------------------------------------------------------------------------------------------------

#[test]
fn el_area_es_proporcional_al_tamano_sin_solapes_y_dentro_del_viewport() {
    for seed in 0..6 {
        let tree = build(&random_spec(seed, 40 + 7 * seed as usize, 12), None);
        let size = (1000u32, 640u32);
        let l = Layout::compute(&tree, key(size, Depth::Two));
        let vp = Rect::new(0.0, 0.0, size.0 as f32, size.1 as f32);
        let total: f64 = tree.children(0).map(|c| tree.node(c).alloc as f64).sum();
        let l0 = level(&l, 0);
        assert!(!l0.is_empty());
        // Proporcionalidad: cada celda de nivel 0 que es un nodo ocupa (alloc / total) del viewport.
        let mut covered = 0.0f64;
        for c in &l0 {
            assert!(
                vp.contains_rect(&c.rect, EPS),
                "fuera del viewport: {:?}",
                c.rect
            );
            covered += c.rect.area() as f64;
            if let CellKind::Node(n) = c.kind {
                let expect = vp.area() as f64 * tree.node(n).alloc as f64 / total;
                assert!(
                    (c.rect.area() as f64 - expect).abs() <= expect * 1e-3 + 0.5,
                    "seed {seed} nodo {n}: {} px² frente a {expect}",
                    c.rect.area()
                );
            }
        }
        // El nivel 0 reparte TODO el viewport.
        assert!((covered - vp.area() as f64).abs() < 1.0, "cubre {covered}");
        assert_no_overlap(&l0);
        // Las celdas insinuadas: dentro de su padre, sin solaparse entre sí y sin pasar del tope.
        let l1 = level(&l, 1);
        assert!(!l1.is_empty(), "seed {seed}: sin nivel insinuado");
        for c in &l1 {
            assert!(vp.contains_rect(&c.rect, EPS));
            let parent = &l.cells()[c.parent as usize];
            assert_eq!(parent.level, 0);
            assert!(parent.rect.contains_rect(&c.rect, EPS));
        }
        for p in 0..l.stats().level0 {
            let kids: Vec<&Cell> = l1
                .iter()
                .copied()
                .filter(|c| c.parent == p as u32)
                .collect();
            assert_no_overlap(&kids);
        }
        assert!(l.cells().len() <= Params::default().max_rects);
    }
}

#[test]
fn profundidad_uno_no_crea_nivel_insinuado() {
    let tree = build(&random_spec(3, 30, 8), None);
    let l = Layout::compute(&tree, key((800, 600), Depth::One));
    assert_eq!(l.stats().level1, 0);
    assert!(l.cells().iter().all(|c| c.level == 0));
}

#[test]
fn el_layout_es_determinista() {
    let tree = build(&random_spec(11, 60, 10), None);
    let a = Layout::compute(&tree, key((900, 700), Depth::Two));
    let b = Layout::compute(&tree, key((900, 700), Depth::Two));
    assert_eq!(a.cells(), b.cells());
    assert_eq!(a.others(), b.others());
}

#[test]
fn otros_agrupa_el_sufijo_de_bloques_menores_de_24x24() {
    // 6 grandes y 200 diminutos: los diminutos no caben en 24×24 px y se funden en "Otros".
    let mut kids: Vec<Spec> = (0..6)
        .map(|i| Spec::File(format!("grande{i}"), 1_000_000_000 - i as u64 * 1_000_000))
        .collect();
    for i in 0..200u64 {
        kids.push(Spec::File(format!("chico{i:03}"), 1_000_000 + i));
    }
    let tree = build(&Spec::Dir("raiz".into(), kids), None);
    let l = Layout::compute(&tree, key((800, 600), Depth::One));
    let order = Children::of(&tree, 0);
    assert_eq!(l.others().len(), 1);
    let o = l.others()[0];
    // El grupo es EXACTAMENTE el sufijo de la lista ordenada de hijos.
    let expect_bytes: u64 = order
        .iter_from(o.start as usize)
        .map(|c| tree.node(c).alloc)
        .sum();
    assert_eq!(o.bytes, expect_bytes);
    assert_eq!(o.count as usize, order.len() - o.start as usize);
    assert_eq!(o.parent, 0);
    assert_eq!(o.start, 6, "los 6 grandes quedan fuera del grupo");
    assert_eq!(o.count, 200);
    // Su celda existe, es la última y mide su parte proporcional.
    let last = l.cells().last().unwrap();
    assert_eq!(last.kind, CellKind::Others(0));
    let total: f64 = tree.children(0).map(|c| tree.node(c).alloc as f64).sum();
    let expect = 800.0 * 600.0 * o.bytes as f64 / total;
    assert!((last.rect.area() as f64 - expect).abs() < expect * 1e-3 + 0.5);
    // Todas las demás celdas miden al menos 24×24 px de área; el total sigue cubriendo el viewport.
    for c in &l.cells()[..l.cells().len() - 1] {
        assert!(
            c.rect.area() >= 24.0 * 24.0 - 0.5,
            "celda de {} px² sin agrupar",
            c.rect.area()
        );
    }
    let covered: f64 = l.cells().iter().map(|c| c.rect.area() as f64).sum();
    assert!((covered - 480_000.0).abs() < 1.0);
    assert_eq!(l.stats().grouped, 200);
}

#[test]
fn abrir_otros_reparte_solo_el_sufijo() {
    let mut kids: Vec<Spec> = (0..4)
        .map(|i| Spec::File(format!("grande{i}"), 2_000_000_000 - i as u64))
        .collect();
    for i in 0..60u64 {
        kids.push(Spec::File(format!("chico{i:02}"), 3_000_000 + i * 1000));
    }
    let tree = build(&Spec::Dir("raiz".into(), kids), None);
    let vp = (900, 700);
    let l = Layout::compute(&tree, key(vp, Depth::One));
    let o = l.others()[0];
    let mut k = key(vp, Depth::One);
    k.root = o.root();
    let inner = Layout::compute(&tree, k);
    let order = Children::of(&tree, 0);
    // Los 60 diminutos se repartieron ahora todo el viewport (puede quedar un sufijo, que sería otro "Otros").
    let shown: usize = inner
        .cells()
        .iter()
        .map(|c| match c.kind {
            CellKind::Node(_) => 1,
            CellKind::Others(i) => inner.others()[i as usize].count as usize,
        })
        .sum();
    assert_eq!(shown, order.len() - o.start as usize);
    let covered: f64 = inner.cells().iter().map(|c| c.rect.area() as f64).sum();
    assert!((covered - (900.0 * 700.0)).abs() < 1.0);
    for c in inner.cells() {
        if let CellKind::Node(n) = c.kind {
            assert!(
                order.iter_from(o.start as usize).any(|x| x == n),
                "{n} no es del sufijo"
            );
        }
    }
}

#[test]
fn un_unico_elemento_pequeno_no_se_agrupa() {
    let kids = vec![
        Spec::File("grande".into(), 1_000_000_000),
        Spec::File("chico".into(), 4096),
    ];
    let tree = build(&Spec::Dir("raiz".into(), kids), None);
    let l = Layout::compute(&tree, key((300, 200), Depth::One));
    assert!(l.others().is_empty(), "un grupo de uno no tiene sentido");
    assert_eq!(l.cells().len(), 2);
}

#[test]
fn los_tamanos_cero_no_ocupan_area_ni_cuentan_como_grupo_si_no_pesan() {
    let mut kids = vec![
        Spec::File("a".into(), 5_000_000_000),
        Spec::File("b".into(), 4_000_000_000),
    ];
    for i in 0..50 {
        kids.push(Spec::File(format!("vacio{i}"), 0));
    }
    let tree = build(&Spec::Dir("raiz".into(), kids), None);
    let l = Layout::compute(&tree, key((400, 300), Depth::One));
    assert!(l.others().is_empty());
    assert_eq!(l.cells().len(), 2);
}

#[test]
fn directorio_vacio_o_viewport_vacio_no_producen_celdas() {
    let tree = build(&Spec::Dir("raiz".into(), vec![]), None);
    assert!(
        Layout::compute(&tree, key((100, 100), Depth::Two))
            .cells()
            .is_empty()
    );
    let tree = build(&random_spec(1, 10, 3), None);
    assert!(
        Layout::compute(&tree, key((0, 100), Depth::Two))
            .cells()
            .is_empty()
    );
    assert!(
        Layout::compute(&tree, key((100, 0), Depth::Two))
            .cells()
            .is_empty()
    );
}

#[test]
fn el_lod_no_dibuja_por_debajo_de_3x3_y_respeta_el_tope() {
    // Una carpeta enorme con 50 000 archivos pequeños: no se crea ninguna celda de menos de 3 px de lado.
    let mut kids = vec![];
    let files: Vec<Spec> = (0..50_000u64)
        .map(|i| Spec::File(format!("x{i}"), 4096 + (i % 997) * 4096))
        .collect();
    kids.push(Spec::Dir("enorme".into(), files));
    kids.push(Spec::File("relleno".into(), 200_000_000));
    let tree = build(&Spec::Dir("raiz".into(), kids), None);
    let mut k = key((1280, 800), Depth::Two);
    let l = Layout::compute(&tree, k);
    assert!(l.stats().level1 > 0);
    for c in l.cells().iter().filter(|c| c.level == 1) {
        assert!(c.rect.w >= 3.0 && c.rect.h >= 3.0, "celda de {:?}", c.rect);
    }
    // Con un tope bajo, se respeta y se avisa.
    k.params.max_rects = 500;
    let l = Layout::compute(&tree, k);
    assert!(l.cells().len() <= 500, "{} celdas", l.cells().len());
    assert!(l.stats().truncated);
}

#[test]
fn el_bloque_del_sistema_se_coloca_por_tamano_y_se_oculta_si_pesa_menos_del_1_por_ciento() {
    let kids = vec![
        Spec::File("a".into(), 60_000),
        Spec::File("b".into(), 30_000),
        Spec::File("c".into(), 5_000),
    ];
    let spec = Spec::Dir("raiz".into(), kids);
    // Grande: 40 000 (> 1 %) -> entre `a` y `b` por tamaño.
    let tree = build(&spec, Some(95_000 + 40_000));
    let order = Children::of(&tree, 0);
    assert_eq!(order.len(), 4);
    let names: Vec<String> = order
        .iter_from(0)
        .map(|c| tree.name_lossy(c).into_owned())
        .collect();
    assert_eq!(names, ["a", "[system-data]", "b", "c"]);
    // El layout lo reparte entre los demás.
    let l = Layout::compute(&tree, key((500, 400), Depth::One));
    assert_eq!(l.cells().len(), 4);
    // Mínimo: 100 de 95 100 (< 1 %) -> oculto, y su espacio no se reparte.
    let tree = build(&spec, Some(95_000 + 100));
    let order = Children::of(&tree, 0);
    assert_eq!(order.len(), 3);
    let l = Layout::compute(&tree, key((500, 400), Depth::One));
    assert_eq!(l.cells().len(), 3);
    let covered: f64 = l.cells().iter().map(|c| c.rect.area() as f64).sum();
    assert!((covered - 200_000.0).abs() < 1.0, "{covered}");
}

// --- Hit-test y caché -----------------------------------------------------------------------------------------------

#[test]
fn el_hit_test_coincide_con_la_busqueda_exhaustiva() {
    let tree = build(&random_spec(5, 90, 14), None);
    let l = Layout::compute(&tree, key((1100, 700), Depth::Two));
    assert_eq!(l.grid_entries(), 0, "la rejilla se construye al primer uso");
    let mut r = Rng(99);
    let mut hits = 0;
    for _ in 0..4000 {
        let (x, y) = (
            (r.next() % 1_200_000) as f32 / 1000.0,
            (r.next() % 760_000) as f32 / 1000.0,
        );
        let got = l.hit(x, y);
        // Exhaustivo: la celda más profunda que contiene el punto (a igual nivel, no hay dos: no se solapan).
        let mut best: Option<usize> = None;
        for (i, c) in l.cells().iter().enumerate() {
            if c.rect.contains(x, y) && best.is_none_or(|b| c.level >= l.cells()[b].level) {
                best = Some(i);
            }
        }
        assert_eq!(got, best, "en ({x}, {y})");
        hits += got.is_some() as usize;
    }
    assert!(hits > 2000, "el viewport debe estar casi todo cubierto");
    assert!(l.grid_entries() > 0);
    assert_eq!(l.hit(-1.0, 5.0), None);
    assert_eq!(l.hit(5.0, 5000.0), None);
}

#[test]
fn la_cache_devuelve_el_mismo_layout_solo_si_la_clave_coincide() {
    let tree = build(&random_spec(2, 30, 6), None);
    let mut cache = LayoutCache::new(2);
    let k = key((800, 600), Depth::Two);
    let a = cache.get(&tree, k);
    let b = cache.get(&tree, k);
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!((cache.hits(), cache.misses()), (1, 1));
    // Cambia cada componente de la clave: viewport, profundidad, versión y raíz.
    let mut changed = vec![];
    let mut k2 = k;
    k2.size = (801, 600);
    changed.push(k2);
    let mut k3 = k;
    k3.depth = Depth::One;
    changed.push(k3);
    let mut k4 = k;
    k4.version = 2;
    changed.push(k4);
    let mut k5 = k;
    k5.root = Root::node(tree.children(0).next().unwrap());
    changed.push(k5);
    for kk in changed {
        let c = cache.get(&tree, kk);
        assert!(!Arc::ptr_eq(&a, &c));
    }
    // Capacidad 2: la clave original ya se desalojó.
    let d = cache.get(&tree, k);
    assert!(!Arc::ptr_eq(&a, &d));
    cache.clear();
    let before = cache.misses();
    let _ = cache.get(&tree, k);
    assert_eq!(cache.misses(), before + 1);
}

// --- Rendimiento ----------------------------------------------------------------------------------------------------

/// Relayout de ~20 000 rectángulos en menos de 5 ms (PLAN.md §4, objetivo de la v0.2 que ya se mide aquí).
/// Ignorado por defecto porque solo vale en `--release`:
/// `cargo test -p tesela-layout --release -- --ignored relayout_de_20k`.
#[test]
#[ignore = "benchmark: ejecutar con --release"]
fn relayout_de_20k_rectangulos_en_menos_de_5_ms() {
    // 600 carpetas de 33 archivos de tamaños parecidos = ~20 400 celdas (cada una mide más de 3 x 3 px).
    let kids: Vec<Spec> = {
        let mut r = Rng(1234);
        (0..600)
            .map(|i| {
                let files = (0..33)
                    .map(|j| Spec::File(format!("f{j}"), 1_000_000 + r.next() % 3_000_000))
                    .collect();
                Spec::Dir(format!("d{i}"), files)
            })
            .collect()
    };
    let tree = build(&Spec::Dir("raiz".into(), kids), None);
    let mut k = key((2400, 1500), Depth::Two);
    k.params.max_rects = 25_000;
    let probe = Layout::compute(&tree, k);
    let n = probe.cells().len();
    assert!(
        n >= 15_000,
        "el árbol de prueba da {n} celdas; se quería ~20 000"
    );
    let mut best = f64::MAX;
    for _ in 0..30 {
        let t = std::time::Instant::now();
        let l = Layout::compute(&tree, k);
        // El primer hit-test construye la rejilla: también cuenta como parte del relayout.
        let _ = l.hit(10.0, 10.0);
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        std::hint::black_box(&l);
    }
    eprintln!("relayout de {n} celdas (con rejilla): mejor de 30 = {best:.2} ms");
    assert!(best < 5.0, "{best:.2} ms para {n} celdas");
}

// --- "Otros" siempre progresa -------------------------------------------------------------------------------------------

/// Recorre TODAS las agrupaciones "Otros" a partir de `root`: cada una, al abrirse, tiene que reducir el conjunto
/// (menos hijos que la raíz que la contiene) y acabar mostrando elementos. Devuelve cuántos nodos se vieron como celdas
/// (cada hijo con tamaño aparece exactamente una vez) y la profundidad máxima de anidamiento.
fn explore_groups(tree: &Tree, root: Root, size: (u32, u32), depth: usize) -> (usize, usize) {
    assert!(
        depth < 8,
        "anidamiento demasiado profundo: 'Otros' no converge"
    );
    let kids = Children::of(tree, root.node);
    let end = (root.end as usize).min(kids.len());
    let start = (root.start as usize).min(end);
    let slice_len = end - start;
    let mut k = key(size, Depth::One);
    k.root = root;
    let l = Layout::compute(tree, k);
    assert!(
        !l.cells().is_empty(),
        "una raíz con elementos con tamaño no puede quedar vacía"
    );
    let (mut nodes, mut deepest) = (0usize, depth);
    for c in l.cells() {
        match c.kind {
            CellKind::Node(_) => nodes += 1,
            CellKind::Others(i) => {
                let o = l.others()[i as usize];
                assert!(
                    (o.count as usize) < slice_len,
                    "abrir 'Otros' no reduce el conjunto: {} de {slice_len}",
                    o.count
                );
                assert_ne!(o.root(), root, "abrir 'Otros' devuelve la misma raíz");
                let (n, d) = explore_groups(tree, o.root(), size, depth + 1);
                nodes += n;
                deepest = deepest.max(d);
            }
        }
    }
    (nodes, deepest)
}

fn flat(n: usize, mut size_of: impl FnMut(usize) -> u64) -> Tree {
    let kids = (0..n)
        .map(|i| Spec::File(format!("f{i:06}"), size_of(i)))
        .collect();
    build(&Spec::Dir("raiz".into(), kids), None)
}

#[test]
fn con_muchos_hijos_iguales_ninguno_cabe_y_otros_se_pagina() {
    for n in [1500usize, 5000, 20_000] {
        let t = flat(n, |_| 1_000_000);
        // 1200 × 800: ninguno de los n hijos llega a 24 × 24 px cuando n > 1666.
        let l = Layout::compute(&t, key((1200, 800), Depth::Two));
        let fits = (1200.0 * 800.0 / 576.0) as usize;
        if n > fits {
            assert!(
                l.cells()
                    .iter()
                    .all(|c| matches!(c.kind, CellKind::Others(_))),
                "n = {n}: solo páginas"
            );
            assert!(
                l.others().len() > 1,
                "n = {n}: más de una página (no un único 'Otros' con todo)"
            );
            assert!(l.others().iter().all(|o| (o.count as usize) < n));
            let total: u32 = l.others().iter().map(|o| o.count).sum();
            assert_eq!(total as usize, n, "las páginas cubren todos los hijos");
        }
        let (nodes, deep) = explore_groups(&t, Root::node(0), (1200, 800), 0);
        assert_eq!(nodes, n, "n = {n}: cada hijo se ve exactamente una vez");
        assert!(deep <= 3, "n = {n}: profundidad {deep}");
    }
}

#[test]
fn zipf_de_200k_hijos_se_puede_recorrer_entero_por_otros() {
    let t = flat(200_000, |i| 4_000_000_000 / (i as u64 + 1) + 4096);
    let (nodes, deep) = explore_groups(&t, Root::node(0), (1200, 800), 0);
    assert_eq!(nodes, 200_000);
    assert!(deep <= 7, "profundidad {deep}");
}

#[test]
fn abrir_otros_siempre_cambia_la_raiz_y_muestra_elementos_con_cualquier_viewport() {
    // Viewports pequeños y grandes, y tamaños mixtos: la propiedad no depende de la forma de la distribución.
    for (n, size) in [
        (3000usize, (320u32, 240u32)),
        (3000, (2560, 1440)),
        (700, (200, 200)),
        (50_000, (800, 600)),
    ] {
        let mut r = Rng(n as u64);
        let t = flat(n, |_| 100_000 + r.next() % 50_000);
        let (nodes, _) = explore_groups(&t, Root::node(0), size, 0);
        assert_eq!(nodes, n, "n = {n}, viewport {size:?}");
    }
}

#[test]
fn con_viewports_diminutos_abrir_otros_sigue_reduciendo_el_conjunto() {
    // Por debajo de ~34 × 34 px la página mínima (2 hijos) alcanza a todo el conjunto: antes salía UN "Otros" con todo,
    // idéntico a la raíz. Ahora, si una página no reduce el conjunto, no se agrupa y se reparten los hijos tal cual.
    for size in [
        (1u32, 1u32),
        (10, 10),
        (20, 20),
        (33, 33),
        (30, 25),
        (34, 34),
        (40, 20),
    ] {
        for n in [1usize, 2, 3, 4, 5, 8, 40, 200] {
            for equal in [true, false] {
                let mut r = Rng(n as u64 * 31 + size.0 as u64);
                let t = flat(n, |i| {
                    if equal {
                        1_000_000
                    } else {
                        1_000_000 / (i as u64 + 1) + 1 + r.next() % 7
                    }
                });
                let (nodes, deep) = explore_groups(&t, Root::node(0), size, 0);
                assert_eq!(nodes, n, "n = {n}, viewport {size:?}, iguales {equal}");
                assert!(deep < 8, "n = {n}, viewport {size:?}: profundidad {deep}");
            }
        }
    }
    // El caso exacto: dos hijos iguales en un viewport de 20 × 20 se muestran los dos, sin "Otros".
    let t = flat(2, |_| 5_000_000);
    let l = Layout::compute(&t, key((20, 20), Depth::One));
    assert_eq!(l.others().len(), 0);
    assert_eq!(l.cells().len(), 2);
}

#[test]
fn el_sufijo_normal_sigue_funcionando_y_el_cero_no_se_pagina() {
    // Hijos de tamaños muy distintos: grandes + cola diminuta = un solo 'Otros' sufijo (comportamiento original).
    let mut kids: Vec<Spec> = (0..5)
        .map(|i| Spec::File(format!("g{i}"), 1_000_000_000 - i))
        .collect();
    kids.extend((0..400).map(|i| Spec::File(format!("c{i:03}"), 100_000 + i)));
    kids.extend((0..50).map(|i| Spec::File(format!("z{i}"), 0)));
    let t = build(&Spec::Dir("raiz".into(), kids), None);
    let l = Layout::compute(&t, key((800, 600), Depth::One));
    assert_eq!(l.others().len(), 1);
    assert_eq!(l.others()[0].start, 5);
    assert_eq!(
        l.others()[0].count,
        450,
        "el sufijo incluye los de tamaño 0, como antes"
    );
    assert_eq!(l.others()[0].root().end as usize, 455);
}

#[test]
fn el_presupuesto_limita_el_nivel_principal_y_el_exceso_va_a_otros() {
    // 20 000 elementos iguales en 4000 × 4000: cada uno mide 800 px² (más que el área mínima de agrupación), así que
    // antes se creaban 20 000 celdas con un presupuesto de 15 000.
    let n = 20_000usize;
    let t = flat(n, |_| 1_000_000);
    let size = (4000, 4000);
    let k = key(size, Depth::Two);
    assert_eq!(k.params.max_rects, 15_000);
    let l = Layout::compute(&t, k);
    assert!(
        l.cells().len() <= k.params.max_rects,
        "{} celdas con un presupuesto de {}",
        l.cells().len(),
        k.params.max_rects
    );
    assert!(l.stats().truncated);
    // Una sola agrupación con el exceso: las demás celdas son hijos reales.
    assert_eq!(l.others().len(), 1);
    let o = l.others()[0];
    assert_eq!(o.count as usize, n - (k.params.max_rects - 1));
    assert_eq!(l.stats().grouped, o.count as usize);
    assert_eq!(level(&l, 0).len(), k.params.max_rects);
    // La agrupación es un sufijo (los hijos van ordenados) y cubre exactamente lo que no se dibuja.
    assert_eq!(o.start as usize, k.params.max_rects - 1);
    let nodes = l
        .cells()
        .iter()
        .filter(|c| matches!(c.kind, CellKind::Node(_)))
        .count();
    assert_eq!(nodes + o.count as usize, n);
    // El layout sigue cubriendo el viewport sin solapes.
    assert_no_overlap(&level(&l, 0));
    // Abrir "Otros" reduce el conjunto y entre todo se ve cada hijo exactamente una vez.
    let (seen, deep) = explore_groups(&t, Root::node(0), size, 0);
    assert_eq!(seen, n);
    assert!(deep <= 3, "profundidad {deep}");
    // Con un presupuesto que no se alcanza, no hay agrupación ni aviso.
    let mut k2 = k;
    k2.params.max_rects = 30_000;
    let l2 = Layout::compute(&t, k2);
    assert!(l2.others().is_empty() && !l2.stats().truncated);
    assert_eq!(l2.stats().level0, n);
}

#[test]
fn el_presupuesto_es_unico_para_los_dos_niveles() {
    // Carpetas con hijos: nivel principal + insinuado nunca pasan de `max_rects`, también con presupuestos pequeños.
    let dirs: Vec<Spec> = (0..60)
        .map(|i| {
            Spec::Dir(
                format!("d{i:03}"),
                (0..80u64)
                    .map(|j| Spec::File(format!("a{j}"), 1_000_000 + j))
                    .collect(),
            )
        })
        .collect();
    let tree = build(&Spec::Dir("raiz".into(), dirs), None);
    for max in [2usize, 5, 40, 60, 100, 1000, 4000] {
        let mut k = key((1600, 1000), Depth::Two);
        k.params.max_rects = max;
        let l = Layout::compute(&tree, k);
        assert!(
            l.cells().len() <= max,
            "max {max}: {} celdas",
            l.cells().len()
        );
        assert_eq!(l.stats().level0 + l.stats().level1, l.cells().len());
        assert!(level(&l, 0).len() <= max);
        // El nivel principal cubre siempre todos los hijos (directamente o dentro de "Otros").
        let covered: usize = l
            .cells()
            .iter()
            .filter(|c| c.level == 0)
            .map(|c| match c.kind {
                CellKind::Node(_) => 1,
                CellKind::Others(i) => l.others()[i as usize].count as usize,
            })
            .sum();
        assert_eq!(covered, 60, "max {max}");
        if max < 60 + 60 * 80 {
            assert!(l.stats().truncated, "max {max}");
        }
    }
}
