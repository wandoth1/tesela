//! Modo demostración (`tesela --demo`): un árbol SINTÉTICO y determinista (semilla fija) con carpetas de nombres
//! inventados, unidades ficticias y un escaneo simulado. Es el ÚNICO modo con el que se hacen capturas: nada de aquí
//! sale de un disco real ni de ningún equipo.
//!
//! Los tamaños se reparten con una distribución de cola pesada, y las fechas de modificación caen en todos los tramos
//! de antigüedad, de modo que los histogramas y los colores del mapa se ven tal como en un disco de verdad.

use crate::platform::{DriveInfo, DriveKind};
use std::collections::VecDeque;
use tesela_model::{Batch, EntryData, FinishOptions, Tree, TreeBuilder, flags};

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;
const DAY: u32 = 86_400;
/// SID ficticio del usuario de la demostración (nombre de su carpeta en la Papelera).
pub const DEMO_SID: &str = "S-1-5-DEMO-1001";
/// "Ahora" fijo del árbol de demostración (segundos Unix), para que las edades no dependan del reloj.
pub const DEMO_NOW: u32 = 1_760_000_000;

/// Variante del árbol de demostración.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// Análisis completo de la unidad: con el bloque "Windows y otros datos del sistema".
    Complete,
    /// Análisis detenido con "Parar y ver": carpetas `INCOMPLETE` (rayas, "≥") y sin bloque del sistema.
    Stopped,
    /// Una carpeta con ~20 000 celdas visibles (600 carpetas de 33 archivos): el peor caso del mapa, para medir el coste
    /// por frame (`--stress`). No es un disco "realista".
    Stress,
}

/// Unidades ficticias para la pantalla de inicio.
pub fn drives() -> Vec<DriveInfo> {
    vec![
        DriveInfo {
            letter: 'C',
            label: "Sistema".into(),
            kind: DriveKind::Fixed,
            total: 476 * GIB,
            free: 38 * GIB,
            available: 38 * GIB,
            volume_scope: true,
        },
        DriveInfo {
            letter: 'E',
            label: "Almacén".into(),
            kind: DriveKind::Fixed,
            total: 932 * GIB,
            free: 722 * GIB,
            available: 722 * GIB,
            volume_scope: true,
        },
        DriveInfo {
            letter: 'F',
            label: "Memoria USB".into(),
            kind: DriveKind::Removable,
            total: 64 * GIB,
            free: 52 * GIB,
            available: 52 * GIB,
            volume_scope: true,
        },
    ]
}

/// Raíz del árbol de demostración (la unidad ficticia del sistema), construida en tiempo de ejecución.
pub fn root_name() -> String {
    format!("{}:\\", drives()[0].letter)
}

/// Nombre del usuario ficticio.
pub const USER: &str = "usuario";

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
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

enum Spec {
    File {
        name: String,
        alloc: u64,
        mtime: u32,
        flags: u16,
    },
    Dir {
        name: String,
        kids: Vec<Spec>,
        flags: u16,
        /// Escaneo detenido aqui: se entrega el listado de esta carpeta y el de sus subcarpetas directas, pero no el de
        /// las siguientes (quedan `INCOMPLETE`).
        stop: bool,
    },
}

impl Spec {
    fn name(&self) -> &str {
        match self {
            Spec::File { name, .. } | Spec::Dir { name, .. } => name,
        }
    }
}

fn dir(name: &str, kids: Vec<Spec>) -> Spec {
    Spec::Dir {
        name: name.into(),
        kids,
        flags: flags::DIR,
        stop: false,
    }
}

fn file(name: &str, alloc: u64, age_days: u32) -> Spec {
    Spec::File {
        name: name.into(),
        alloc,
        mtime: DEMO_NOW - age_days * DAY,
        flags: 0,
    }
}

/// `n` archivos que suman `total` bytes (repartidos con cola pesada), con extensiones ponderadas y edades entre
/// `age.0` y `age.1` días.
fn files(
    rng: &mut Rng,
    prefix: &str,
    n: usize,
    total: u64,
    exts: &[(&str, u32)],
    age: (u32, u32),
) -> Vec<Spec> {
    let weights: Vec<f64> = (0..n)
        .map(|_| (-rng.unit().max(1e-9).ln()).powf(1.8) + 0.02)
        .collect();
    let sum: f64 = weights.iter().sum();
    let wsum: u32 = exts.iter().map(|e| e.1).sum();
    (0..n)
        .map(|i| {
            let mut pick = rng.next() as u32 % wsum;
            let ext = exts
                .iter()
                .find(|e| {
                    if pick < e.1 {
                        true
                    } else {
                        pick -= e.1;
                        false
                    }
                })
                .map_or("bin", |e| e.0);
            let a = ((total as f64) * weights[i] / sum) as u64;
            // Redondea al clúster de 4 KiB, como `AllocationSize`.
            let alloc = a.div_ceil(4096) * 4096;
            let span = age.1.saturating_sub(age.0).max(1);
            let days = age.0 + (rng.next() % span as u64) as u32;
            file(&format!("{prefix}{:04}.{ext}", i + 1), alloc, days)
        })
        .collect()
}

/// Datos de los navegadores del usuario ficticio, con SUS cachés reales (`Cache`, `Code Cache`, `cache2`) y su perfil
/// (historial, cookies...), que NO es caché. Se generan con un generador aparte para no alterar el resto del árbol.
fn browsers(extra: &mut Rng) -> Vec<Spec> {
    let cache = |extra: &mut Rng, prefix: &str, n: usize, total: u64| {
        files(extra, prefix, n, total, &[("dat", 6), ("bin", 2)], (0, 20))
    };
    let chrome_profile = dir(
        "Default",
        vec![
            dir("Cache", cache(extra, "f_", 300, 1900 * MIB)),
            dir("Code Cache", cache(extra, "js-", 80, 300 * MIB)),
            file("History", 40 * MIB, 2),
            file("Cookies", 12 * MIB, 1),
            file("Login Data", 2 * MIB, 40),
        ],
    );
    let edge_profile = dir(
        "Default",
        vec![
            dir("Cache", cache(extra, "f_", 400, 3100 * MIB)),
            file("History", 30 * MIB, 3),
            file("Cookies", 9 * MIB, 1),
        ],
    );
    vec![
        dir(
            "Google",
            vec![dir(
                "Chrome",
                vec![dir(
                    "User Data",
                    vec![chrome_profile, file("Local State", MIB, 9)],
                )],
            )],
        ),
        dir(
            "Microsoft",
            vec![dir(
                "Edge",
                vec![dir(
                    "User Data",
                    vec![edge_profile, file("Local State", MIB, 12)],
                )],
            )],
        ),
        dir(
            "Mozilla",
            vec![dir(
                "Firefox",
                vec![dir(
                    "Profiles",
                    vec![dir(
                        "demo.default-release",
                        vec![
                            dir("cache2", cache(extra, "e-", 120, 800 * MIB)),
                            file("places.sqlite", 60 * MIB, 1),
                            file("key4.db", 2 * MIB, 50),
                        ],
                    )],
                )],
            )],
        ),
    ]
}

fn build_spec(rng: &mut Rng, variant: Variant) -> Spec {
    let mut extra = Rng(0xD0CA_11ED_5EED_0002);
    let user = USER;
    // --- Carpeta del usuario ---
    let videos = dir(
        "Vídeos",
        vec![
            dir(
                "Viaje a Japón",
                files(
                    rng,
                    "VID_",
                    40,
                    14 * GIB,
                    &[("mp4", 9), ("mov", 1)],
                    (700, 1100),
                ),
            ),
            dir(
                "Fiesta familiar",
                files(rng, "FIESTA_", 22, 9 * GIB, &[("mp4", 1)], (1300, 1800)),
            ),
            dir(
                "Tutoriales",
                files(
                    rng,
                    "tutorial-",
                    30,
                    6 * GIB,
                    &[("mkv", 3), ("mp4", 2)],
                    (200, 900),
                ),
            ),
            dir(
                "Capturas de pantalla",
                files(rng, "grabacion-", 60, 4 * GIB, &[("mp4", 1)], (5, 400)),
            ),
            file("presentacion-final.mov", 2 * GIB + 300 * MIB, 600),
        ],
    );
    let downloads = dir(
        "Descargas",
        vec![
            file("distro-linux-24.04.iso", 6 * GIB + 200 * MIB, 330),
            file("instalador-sistema.iso", 5 * GIB + 800 * MIB, 400),
            file("fotos-2019.zip", 3 * GIB + 100 * MIB, 1200),
            file("juego-instalador.exe", 2 * GIB + 400 * MIB, 500),
            file("controlador-grafico.exe", 900 * MIB, 200),
            dir(
                "Comprimidos",
                files(
                    rng,
                    "descarga-",
                    120,
                    4 * GIB,
                    &[("zip", 4), ("7z", 1), ("rar", 1)],
                    (30, 900),
                ),
            ),
            dir(
                "Instaladores",
                files(
                    rng,
                    "setup-",
                    80,
                    3 * GIB,
                    &[("exe", 3), ("msi", 2)],
                    (60, 1000),
                ),
            ),
            dir(
                "Documentos sueltos",
                files(
                    rng,
                    "doc-",
                    300,
                    600 * MIB,
                    &[("pdf", 5), ("docx", 2), ("txt", 1)],
                    (10, 1500),
                ),
            ),
        ],
    );
    let project_tree = |rng: &mut Rng, name: &str, modules_gib_tenths: u64| {
        dir(
            name,
            vec![
                dir(
                    "node_modules",
                    files(
                        rng,
                        "pkg-",
                        400,
                        modules_gib_tenths * GIB / 10,
                        &[("js", 6), ("map", 2), ("json", 2)],
                        (30, 300),
                    ),
                ),
                dir(
                    "src",
                    files(
                        rng,
                        "mod-",
                        60,
                        40 * MIB,
                        &[("ts", 4), ("css", 1), ("json", 1)],
                        (1, 120),
                    ),
                ),
                dir(
                    "dist",
                    files(
                        rng,
                        "bundle-",
                        20,
                        200 * MIB,
                        &[("js", 3), ("map", 1)],
                        (1, 60),
                    ),
                ),
            ],
        )
    };
    let projects = dir(
        "Proyectos",
        vec![
            project_tree(rng, "tienda-web", 34),
            project_tree(rng, "app-notas", 24),
            project_tree(rng, "juego-puzzle", 15),
            dir(
                "scripts",
                files(
                    rng,
                    "tarea-",
                    200,
                    300 * MIB,
                    &[("py", 5), ("ps1", 2), ("txt", 1)],
                    (5, 900),
                ),
            ),
            dir(
                "datos-experimentos",
                files(
                    rng,
                    "muestra-",
                    90,
                    9 * GIB,
                    &[("csv", 3), ("bin", 1)],
                    (100, 1300),
                ),
            ),
        ],
    );
    let images = dir(
        "Imágenes",
        vec![
            dir(
                "Vacaciones 2022",
                files(
                    rng,
                    "IMG_",
                    400,
                    7 * GIB,
                    &[("jpg", 8), ("heic", 2), ("png", 1)],
                    (1000, 1200),
                ),
            ),
            dir(
                "Fotos de familia",
                files(
                    rng,
                    "FAM_",
                    500,
                    8 * GIB,
                    &[("jpg", 9), ("png", 1)],
                    (1500, 3000),
                ),
            ),
            dir(
                "Capturas",
                files(rng, "captura-", 300, 2 * GIB, &[("png", 1)], (1, 500)),
            ),
            dir(
                "Fondos",
                files(
                    rng,
                    "fondo-",
                    60,
                    4 * GIB,
                    &[("jpg", 1), ("png", 1)],
                    (200, 1500),
                ),
            ),
        ],
    );
    let documents = dir(
        "Documentos",
        vec![
            dir(
                "Trabajo",
                files(
                    rng,
                    "informe-",
                    200,
                    6 * GIB,
                    &[("docx", 3), ("pdf", 4), ("xlsx", 2)],
                    (20, 1200),
                ),
            ),
            dir(
                "Estudios",
                files(
                    rng,
                    "apuntes-",
                    120,
                    5 * GIB,
                    &[("pdf", 5), ("pptx", 2)],
                    (400, 2000),
                ),
            ),
            dir(
                "Libros",
                files(
                    rng,
                    "libro-",
                    50,
                    3 * GIB,
                    &[("epub", 2), ("pdf", 3)],
                    (100, 2500),
                ),
            ),
        ],
    );
    let music = dir(
        "Música",
        vec![
            dir(
                "Álbumes",
                files(
                    rng,
                    "pista-",
                    700,
                    8 * GIB,
                    &[("mp3", 6), ("flac", 2), ("m4a", 1)],
                    (800, 3500),
                ),
            ),
            dir(
                "Podcasts",
                files(
                    rng,
                    "episodio-",
                    100,
                    GIB + 100 * MIB,
                    &[("mp3", 1)],
                    (10, 600),
                ),
            ),
        ],
    );
    let onedrive = Spec::Dir {
        name: "OneDrive".into(),
        flags: flags::DIR | flags::CLOUD,
        stop: false,
        kids: {
            let mut v = files(
                rng,
                "nube-",
                80,
                6 * GIB + 200 * MIB,
                &[("docx", 2), ("jpg", 2), ("pdf", 2)],
                (20, 800),
            );
            // Archivos que solo están en la nube: no ocupan disco aunque pesen.
            for i in 0..30 {
                v.push(Spec::File {
                    name: format!("solo-nube-{i:02}.pdf"),
                    alloc: 0,
                    mtime: DEMO_NOW - (40 + i * 20) * DAY,
                    flags: flags::CLOUD,
                });
            }
            v
        },
    };
    // 40 carpetas pequeñas (de 80 a 275 MiB): acaban agrupadas en "Otros".
    let mut small: Vec<Spec> = (0..40)
        .map(|i| {
            dir(
                &format!("Varios {:02}", i + 1),
                files(
                    rng,
                    "v-",
                    8,
                    (80 + 5 * (40 - i as u64)) * MIB,
                    &[("txt", 2), ("jpg", 1), ("dat", 1)],
                    (30, 2000),
                ),
            )
        })
        .collect();
    let appdata = dir(
        "AppData",
        vec![
            dir("Local", {
                let mut v = vec![
                    dir(
                        "Paquetes",
                        files(
                            rng,
                            "pk-",
                            300,
                            20 * GIB,
                            &[("dll", 3), ("dat", 3), ("exe", 1)],
                            (10, 800),
                        ),
                    ),
                    dir(
                        "Temp",
                        files(
                            rng,
                            "tmp",
                            400,
                            2 * GIB + 100 * MIB,
                            &[("tmp", 5), ("log", 2)],
                            (0, 12),
                        ),
                    ),
                    dir(
                        "Cache de aplicaciones",
                        files(
                            rng,
                            "cache-",
                            500,
                            4 * GIB,
                            &[("dat", 5), ("png", 1)],
                            (0, 40),
                        ),
                    ),
                    dir(
                        "Programas",
                        files(
                            rng,
                            "prog-",
                            200,
                            12 * GIB,
                            &[("dll", 4), ("exe", 1), ("dat", 3)],
                            (30, 600),
                        ),
                    ),
                ];
                v.extend(browsers(&mut extra));
                v
            }),
            dir(
                "Roaming",
                files(
                    rng,
                    "ajuste-",
                    150,
                    9 * GIB,
                    &[("json", 3), ("db", 1), ("dat", 3)],
                    (5, 900),
                ),
            ),
        ],
    );
    let vms = dir(
        "Máquinas virtuales",
        files(rng, "vm-", 6, 40 * GIB, &[("vhdx", 1)], (20, 400)),
    );
    let mut user_kids = vec![
        vms, appdata, videos, downloads, projects, images, documents, music, onedrive,
    ];
    user_kids.append(&mut small);
    // Una carpeta de usuario más y otra pública, pequeñas.
    let users = dir(
        "Users",
        vec![
            dir(user, user_kids),
            dir(
                "Público",
                files(
                    rng,
                    "pub-",
                    30,
                    800 * MIB,
                    &[("jpg", 1), ("txt", 1)],
                    (100, 2000),
                ),
            ),
        ],
    );

    // --- Raíz de la unidad ---
    let stopped = variant == Variant::Stopped;
    let nested = |rng: &mut Rng,
                  name: &str,
                  own_gib: u64,
                  deep_gib: u64,
                  prefix: &str,
                  exts: &[(&str, u32)]| {
        let mut kids = files(rng, prefix, 300, own_gib * GIB, exts, (20, 900));
        kids.push(dir(
            "Profundo",
            files(rng, "p-", 200, deep_gib * GIB, exts, (20, 900)),
        ));
        dir(name, kids)
    };
    let windows = Spec::Dir {
        name: "Windows".into(),
        flags: flags::DIR,
        stop: stopped,
        kids: vec![
            nested(
                rng,
                "Sistema",
                5,
                7,
                "sys-",
                &[("dll", 6), ("exe", 1), ("sys", 2)],
            ),
            nested(
                rng,
                "Componentes",
                3,
                4,
                "comp-",
                &[("dll", 4), ("dat", 2), ("cab", 1)],
            ),
            dir(
                "Temp",
                files(rng, "t-", 60, 300 * MIB, &[("tmp", 2)], (0, 30)),
            ),
        ],
    };
    let program_files = dir(
        "Archivos de programa",
        vec![
            dir(
                "Editor de fotos",
                files(
                    rng,
                    "ef-",
                    120,
                    18 * GIB,
                    &[("dll", 4), ("exe", 1), ("dat", 4)],
                    (30, 500),
                ),
            ),
            dir(
                "Navegador",
                files(
                    rng,
                    "nav-",
                    150,
                    10 * GIB,
                    &[("dll", 5), ("pak", 3), ("exe", 1)],
                    (3, 120),
                ),
            ),
            dir(
                "Suite de oficina",
                files(
                    rng,
                    "of-",
                    200,
                    20 * GIB,
                    &[("dll", 5), ("exe", 2), ("dat", 3)],
                    (60, 800),
                ),
            ),
            dir(
                "Herramientas de desarrollo",
                files(
                    rng,
                    "dev-",
                    180,
                    8 * GIB,
                    &[("dll", 5), ("exe", 2), ("lib", 2)],
                    (20, 600),
                ),
            ),
        ],
    );
    let games = dir(
        "Juegos",
        vec![
            dir(
                "Aventura espacial",
                files(
                    rng,
                    "asset-",
                    80,
                    34 * GIB,
                    &[("dll", 4), ("exe", 2), ("pak", 1)],
                    (100, 700),
                ),
            ),
            dir(
                "Carreras",
                files(
                    rng,
                    "coche-",
                    60,
                    24 * GIB,
                    &[("dll", 4), ("exe", 2), ("pak", 1)],
                    (200, 900),
                ),
            ),
        ],
    );
    let program_data = Spec::Dir {
        name: "ProgramData".into(),
        flags: flags::DIR,
        stop: stopped,
        kids: {
            let mut k = files(
                rng,
                "pd-",
                150,
                5 * GIB,
                &[("dat", 4), ("db", 1), ("log", 1)],
                (5, 700),
            );
            k.push(dir(
                "Datos de aplicaciones",
                files(rng, "da-", 100, 4 * GIB, &[("dat", 3), ("db", 1)], (5, 700)),
            ));
            k
        },
    };
    // La Papelera del usuario: su carpeta lleva el SID (ficticio).
    let recycle_bin = dir(
        "$Recycle.Bin",
        vec![dir(
            DEMO_SID,
            files(
                &mut extra,
                "$R",
                14,
                520 * MIB,
                &[("mp4", 1), ("zip", 1), ("pdf", 1)],
                (2, 60),
            ),
        )],
    );
    let mut root_kids = vec![
        users,
        program_files,
        games,
        windows,
        program_data,
        recycle_bin,
        file("pagefile.sys", 16 * GIB, 1),
        file("hiberfil.sys", 13 * GIB, 3),
        file("swapfile.sys", 256 * MIB, 3),
    ];
    // Algunos archivos sueltos en la raíz.
    root_kids.extend(files(
        rng,
        "raiz-",
        3,
        40 * MIB,
        &[("log", 1), ("txt", 1)],
        (50, 900),
    ));
    let _ = rng.below(2);
    Spec::Dir {
        name: root_name(),
        kids: root_kids,
        flags: flags::DIR,
        stop: false,
    }
}

/// 600 carpetas de 33 archivos de tamaños parecidos (cada archivo da una celda insinuada de más de 3 × 3 px).
fn stress_spec(rng: &mut Rng) -> Spec {
    let kids = (0..600)
        .map(|i| {
            let files = (0..33)
                .map(|j| {
                    file(
                        &format!("archivo-{j:02}.dat"),
                        1_000_000 + rng.next() % 3_000_000,
                        30 + (i % 900) as u32,
                    )
                })
                .collect();
            dir(&format!("carpeta-{i:03}"), files)
        })
        .collect();
    Spec::Dir {
        name: root_name(),
        kids,
        flags: flags::DIR,
        stop: false,
    }
}

fn emit(root: &Spec) -> Vec<Batch> {
    let mut rb = Batch::detached(0);
    rb.push(
        0,
        root.name().as_bytes(),
        EntryData {
            flags: flags::DIR,
            ..EntryData::default()
        },
    );
    let mut batches = vec![rb];
    let mut counter = 1u32;
    // (carpeta, id, ¿el escaneo se detuvo en su padre? Entonces se lista pero no se baja más).
    let mut queue: VecDeque<(&Spec, u32, bool)> = VecDeque::new();
    queue.push_back((root, 0, false));
    while let Some((spec, id, below_stop)) = queue.pop_front() {
        let Spec::Dir { kids, stop, .. } = spec else {
            continue;
        };
        let mut batch = Batch::listing(id, counter);
        counter += kids.len() as u32;
        let mut ids = Vec::with_capacity(kids.len());
        for k in kids {
            let data = match k {
                Spec::File {
                    alloc,
                    mtime,
                    flags,
                    ..
                } => EntryData {
                    flags: *flags,
                    alloc: *alloc,
                    // El tamaño lógico es algo menor que el de disco (redondeo al clúster).
                    logical: if *alloc == 0 {
                        // Solo en la nube: no ocupa disco pero pesa (el escáner informa el tamaño lógico).
                        MIB + 4096 * (*mtime as u64 % 700)
                    } else {
                        alloc.saturating_sub(alloc / 40)
                    },
                    mtime: *mtime,
                    file_id: 0,
                },
                Spec::Dir { flags, .. } => EntryData {
                    flags: *flags,
                    mtime: DEMO_NOW - 30 * DAY,
                    ..EntryData::default()
                },
            };
            ids.push(batch.push(id, k.name().as_bytes(), data));
        }
        batches.push(batch);
        // Una carpeta con `stop` baja un nivel más (sus subcarpetas se listan); las de debajo de esas, no: sin lote,
        // el builder las marca `INCOMPLETE`.
        if !below_stop {
            for (k, cid) in kids.iter().zip(ids) {
                queue.push_back((k, cid, *stop));
            }
        }
    }
    batches
}

/// El árbol de demostración. Determinista: la misma variante da siempre el mismo árbol.
pub fn tree(variant: Variant) -> Tree {
    let mut rng = Rng(0x7E5E_1A00_D3A0_0001);
    let spec = if variant == Variant::Stress {
        stress_spec(&mut rng)
    } else {
        build_spec(&mut rng, variant)
    };
    let mut b = TreeBuilder::new();
    for batch in emit(&spec) {
        b.add_batch(batch).expect("lote válido");
    }
    let used = match variant {
        Variant::Complete => {
            let d = &drives()[0];
            Some(d.total - d.free)
        }
        Variant::Stopped | Variant::Stress => None,
    };
    b.finish(&FinishOptions {
        now_unix: DEMO_NOW,
        dedup_hard_links: true,
        volume_used: used,
    })
}

/// Fila de "sitios conocidos" del escaneo simulado: (clave i18n, bytes finales, fracción del escaneo en que acaba). Los
/// tamaños son los de las carpetas correspondientes del árbol de demostración.
pub const DEMO_KNOWN_ROWS: [(&str, u64, f32); 6] = [
    ("known-user-temp", 2_254_000_000, 0.10),
    ("known-windows-temp", 314_572_800, 0.22),
    ("known-chrome-cache", 2_361_000_000, 0.36),
    ("known-edge-cache", 3_250_000_000, 0.52),
    ("known-firefox-cache", 838_860_800, 0.60),
    ("known-recycle-bin", 545_259_520, 0.66),
];

#[cfg(test)]
mod tests {
    use super::*;
    use tesela_model::flags;

    #[test]
    fn el_arbol_es_determinista_y_valido() {
        let a = tree(Variant::Complete);
        let b = tree(Variant::Complete);
        assert!(a.structural_eq(&b));
        a.check_invariants().expect("invariantes");
        assert!(a.len() > 8_000, "{} nodos", a.len());
        assert_eq!(a.name_lossy(0), root_name());
    }

    #[test]
    fn la_variante_completa_tiene_bloque_del_sistema_y_la_detenida_carpetas_incompletas() {
        let t = tree(Variant::Complete);
        let last = t.children(0).last().unwrap();
        assert!(t.node(last).flags & flags::SYSTEM_INFO != 0);
        assert_eq!(t.stats().incomplete_dirs, 0);
        // Con la unidad ficticia de 476 GiB y 38 libres, lo usado es 438 GiB.
        assert_eq!(t.node(0).alloc, 438 * GIB);

        let s = tree(Variant::Stopped);
        s.check_invariants().expect("invariantes");
        assert!(s.stats().incomplete_dirs >= 2);
        assert!(
            s.children(0)
                .all(|c| s.node(c).flags & flags::SYSTEM_INFO == 0)
        );
        assert!(s.node(0).flags & flags::INCOMPLETE_BELOW != 0);
    }

    #[test]
    fn contiene_lo_que_hay_que_ensenar() {
        let t = tree(Variant::Complete);
        let names: Vec<String> = t
            .children(0)
            .map(|c| t.name_lossy(c).into_owned())
            .collect();
        for n in ["Users", "Windows", "pagefile.sys", "hiberfil.sys", "Juegos"] {
            assert!(names.iter().any(|x| x == n), "falta {n}");
        }
        let users = t.child_by_name(0, b"Users").unwrap();
        let me = t.child_by_name(users, USER.as_bytes()).unwrap();
        let od = t.child_by_name(me, b"OneDrive").unwrap();
        assert!(t.node(od).flags & flags::CLOUD != 0);
        // Hay archivos solo en la nube (CLOUD con alloc 0).
        assert!(t.children(od).any(|c| {
            let n = t.node(c);
            n.flags & flags::CLOUD != 0 && n.alloc == 0 && n.logical > 0
        }));
        // 40 carpetas pequeñas para que "Otros" aparezca de verdad.
        assert!(t.children(me).count() >= 45);
        // Los tipos dominantes varían (vídeo en Vídeos, imágenes en Imágenes...).
        let v = t.child_by_name(me, "Vídeos".as_bytes()).unwrap();
        assert_eq!(
            t.hist(v).dominant_type(),
            Some(tesela_model::FileType::Video)
        );
    }

    #[test]
    fn las_unidades_ficticias_no_usan_la_d() {
        let d = drives();
        assert!(d.iter().all(|x| x.letter != 'D'));
        assert!(d.iter().all(|x| x.free < x.total));
        assert_eq!(d[0].kind, DriveKind::Fixed);
        assert_eq!(d[2].kind, DriveKind::Removable);
    }
}
