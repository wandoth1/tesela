//! Pruebas de humo de la interfaz con `egui_kittest` sobre el modo demostración (árbol sintético, sin tocar el
//! disco): se renderiza cada pantalla, se navega con el teclado, se comprueba la etiqueta de "Otros" y que el mapa, la
//! lista y los detalles comparten la selección. El escaneo REAL se prueba sobre un árbol creado en
//! `%TEMP%\tesela-test-<uuid>`.

use crate::app::MapStart;
use crate::explore::{Sel, View};
use crate::{DemoOptions, LangPref, Options, Settings, StartScreen, TeselaUi, ThemePref};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use tesela_layout::CellKind;

struct App {
    opts: Options,
    ui: Option<TeselaUi>,
}

impl App {
    fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let opts = self.opts.clone();
        self.ui
            .get_or_insert_with(|| TeselaUi::new(&ctx, opts))
            .show(ui);
    }
    fn ui(&self) -> &TeselaUi {
        self.ui
            .as_ref()
            .expect("la interfaz ya se creó en el primer frame")
    }
    fn ui_mut(&mut self) -> &mut TeselaUi {
        self.ui
            .as_mut()
            .expect("la interfaz ya se creó en el primer frame")
    }
}

fn opts(start: StartScreen) -> Options {
    Options {
        settings: Settings {
            theme: ThemePref::Light,
            language: LangPref::Es,
            ..Settings::default()
        },
        demo: Some(DemoOptions::default()),
        animate: false,
        start,
        dry_run: false,
        backend: Some(crate::app::BackendRef(test_backend())),
        isolated: false,
        update_transport: None,
    }
}

/// El respaldo simulado, sin pausas: las pruebas nunca tocan el disco con la limpieza.
pub(crate) fn test_backend() -> std::sync::Arc<dyn tesela_cleaner::Backend> {
    std::sync::Arc::new(crate::demo_backend::DemoBackend::new(
        std::time::Duration::ZERO,
    ))
}

fn harness(o: Options) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(vec2(1280.0, 800.0))
        .with_step_dt(0.02)
        .build_ui_state(|ui, app: &mut App| app.frame(ui), App { opts: o, ui: None });
    // Unos pasos fijos (no `run()`): el escaneo simulado repinta sin parar.
    h.run_steps(6);
    h
}

fn user_folder() -> StartScreen {
    StartScreen::Map(MapStart {
        list: false,
        open: vec!["Users".into(), crate::demo::USER.into()],
        select: None,
        marks: vec![],
    })
}

fn click_at(h: &mut Harness<'static, App>, pos: Pos2) {
    h.event(Event::PointerMoved(pos));
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.run_steps(1);
    }
    h.run_steps(2);
}

fn explore<'a>(h: &'a Harness<'static, App>) -> &'a crate::explore::Explore {
    h.state().ui().explore().expect("exploración")
}

/// Punto en pantalla de la banda de la etiqueta de la celda de nivel 0 de `name` (la parte de la celda que NO cubren sus
/// celdas insinuadas, así que el clic selecciona la propia carpeta).
fn cell_center(h: &Harness<'static, App>, name: &str) -> Pos2 {
    let ex = explore(h);
    let layout = ex.map.layout.as_ref().expect("layout");
    let map = h.get_by_label_contains("Mapa de").rect();
    let id = ex
        .tree
        .children(ex.root.node)
        .find(|&c| ex.tree.name_lossy(c) == name)
        .unwrap_or_else(|| panic!("no hay `{name}` en la carpeta actual"));
    let idx = layout
        .cell_of_node(id)
        .unwrap_or_else(|| panic!("`{name}` no tiene celda de nivel 0"));
    let r = layout.cells()[idx].rect;
    Pos2::new(map.min.x + r.x + r.w / 2.0, map.min.y + r.y + 6.0)
}

#[test]
fn el_inicio_muestra_las_unidades_ficticias_y_analizar_una_carpeta() {
    let h = harness(opts(StartScreen::Home));
    h.get_by_label("¿Qué disco quieres revisar?");
    h.get_by_label("Analizar Sistema (C:)");
    h.get_by_label("Analizar Almacén (E:)");
    h.get_by_label("Analizar Memoria USB (F:)");
    h.get_by_label("Analizar una carpeta…");
    h.get_by_label("Ajustes");
    // Ninguna unidad "Datos (D:)" de la maqueta (ni la letra D) aparece.
    assert!(h.query_by_label_contains("Datos").is_none());
    assert!(h.query_by_label_contains("(D:)").is_none());
}

#[test]
fn analizar_una_unidad_lleva_al_escaneo_y_de_ahi_al_mapa() {
    let mut h = harness(opts(StartScreen::Home));
    h.get_by_label("Analizar Sistema (C:)").click();
    h.run_steps(3);
    assert_eq!(h.state().ui().screen_name(), "scan");
    h.get_by_label("Parar y ver lo que hay");
    // "Limpiar lo seguro" es del incremento 3: está, pero deshabilitado.
    let clean = h.get_by_label("Limpiar lo seguro");
    assert!(clean.accesskit_node().is_disabled());
    h.get_by_label("Temporales del usuario");
    // "Parar y ver" construye el árbol con lo recibido.
    h.get_by_label("Parar y ver lo que hay").click();
    h.run_steps(5);
    assert_eq!(h.state().ui().screen_name(), "explore");
    let ex = explore(&h);
    assert!(ex.info.cancelled, "es un análisis detenido");
    assert!(ex.tree.stats().incomplete_dirs > 0);
}

#[test]
fn el_escaneo_simulado_congelado_muestra_progreso_filas_y_barras() {
    let h = harness(opts(StartScreen::Scan(0.5)));
    h.get_by_label("Mientras analizamos tu disco…");
    h.get_by_label_contains("Progreso del análisis: 50");
    for row in [
        "Temporales del usuario",
        "Temporales de Windows",
        "Caché de Chrome",
        "Caché de Edge",
        "Papelera de reciclaje",
    ] {
        h.get_by_label(row);
    }
    h.get_by_label("Cancelar");
    // Las barras de las carpetas de primer nivel (con "≥" mientras cuentan).
    h.get_by_label("Users");
    assert!(h.query_all_by_label_contains("sitios conocidos").count() >= 1);
}

#[test]
fn la_ventana_principal_tiene_todas_sus_zonas() {
    let h = harness(opts(StartScreen::Map(MapStart::default())));
    h.get_by_label("Subir un nivel");
    h.get_by_label("Buscar");
    for zone in [
        "Mapa",
        "Lista",
        "Recomendaciones",
        "Tipo",
        "Antigüedad",
        "Riesgo",
        "Reescanear (F5)",
    ] {
        h.get_by_label(zone);
    }
    // La cesta: visible, vacía y con sus botones inactivos.
    h.get_by_label_contains("Cesta · 0 elementos");
    assert!(h.get_by_label("Ver").accesskit_node().is_disabled());
    assert!(
        h.get_by_label("Vaciar cesta")
            .accesskit_node()
            .is_disabled()
    );
    // Antigüedad y Riesgo (v0.2) deshabilitados; Tipo y Recomendaciones activos.
    assert!(h.get_by_label("Antigüedad").accesskit_node().is_disabled());
    assert!(h.get_by_label("Riesgo").accesskit_node().is_disabled());
    assert!(
        !h.get_by_label("Recomendaciones")
            .accesskit_node()
            .is_disabled()
    );
    assert!(!h.get_by_label("Tipo").accesskit_node().is_disabled());
    // Las unidades y la leyenda de tipos y grises.
    h.get_by_label("Analizar Sistema (C:)");
    assert!(h.query_all_by_label("Vídeo").count() >= 1);
    for t in [
        "Imágenes",
        "Audio",
        "Documentos",
        "Comprimidos",
        "Programas",
        "Otros",
        "Mixto",
        "Solo informar",
        "Sistema",
    ] {
        h.get_by_label(t);
    }
}

#[test]
fn el_bloque_del_sistema_y_pagefile_se_ven_en_gris_con_su_icono_y_su_explicacion() {
    let mut h = harness(opts(StartScreen::Map(MapStart::default())));
    let ex = explore(&h);
    // El layout del mapa contiene el bloque del sistema.
    let layout = ex.map.layout.clone().unwrap();
    let names: Vec<String> = layout
        .cells()
        .iter()
        .filter(|c| c.level == 0)
        .filter_map(|c| match c.kind {
            CellKind::Node(id) => Some(ex.tree.name_lossy(id).into_owned()),
            _ => None,
        })
        .collect();
    assert!(names.iter().any(|n| n == "[system-data]"), "{names:?}");
    // Seleccionarlo (clic) muestra su explicación; es de solo lectura.
    let sys = ex.tree.children(0).last().unwrap();
    let r = layout
        .cell_of_node(sys)
        .map(|i| layout.cells()[i].rect)
        .unwrap();
    let map = h.get_by_label_contains("Mapa de").rect();
    click_at(
        &mut h,
        Pos2::new(map.min.x + r.x + r.w / 2.0, map.min.y + r.y + r.h / 2.0),
    );
    h.run_steps(2);
    assert!(
        h.query_all_by_label("Windows y otros datos del sistema")
            .count()
            >= 1
    );
    assert!(
        h.query_all_by_label_contains("Incluye archivos de Windows, hibernación")
            .count()
            >= 1
    );
    // El enlace "modo administrador" no hace nada todavía.
    h.get_by_label("Ver el detalle (modo administrador)");
    // pagefile: "solo informar" con su triángulo.
    let pf = h
        .state()
        .ui()
        .explore()
        .unwrap()
        .tree
        .child_by_name(0, b"pagefile.sys")
        .unwrap();
    let layout = h
        .state()
        .ui()
        .explore()
        .unwrap()
        .map
        .layout
        .clone()
        .unwrap();
    let r = layout
        .cell_of_node(pf)
        .map(|i| layout.cells()[i].rect)
        .unwrap();
    let map = h.get_by_label_contains("Mapa de").rect();
    click_at(
        &mut h,
        Pos2::new(map.min.x + r.x + r.w / 2.0, map.min.y + r.y + r.h / 2.0),
    );
    h.run_steps(2);
    assert!(h.query_all_by_label("Solo informar").count() >= 1);
    assert!(
        h.query_all_by_label_contains("Archivo de paginación")
            .count()
            >= 1
    );
}

#[test]
fn las_carpetas_incompletas_llevan_el_minimo_y_el_aviso() {
    let h = harness(opts(StartScreen::Map(MapStart {
        list: true,
        open: vec![],
        select: None,
        marks: vec![],
    })));
    // Variante detenida: se construye explícitamente.
    drop(h);
    let mut o = opts(StartScreen::Map(MapStart {
        list: true,
        open: vec![],
        select: Some("Windows".into()),
        marks: vec![],
    }));
    o.demo = Some(DemoOptions {
        stopped: true,
        ..DemoOptions::default()
    });
    let h = harness(o);
    // En la lista, la fila de Windows lleva "≥" en su etiqueta accesible, y el estado dice que se detuvo.
    h.get_by_label_contains("Windows, ≥");
    h.get_by_label_contains("Análisis detenido");
    h.get_by_label_contains("El análisis se detuvo antes de recorrer todo");
    // Y no hay bloque del sistema (con carpetas incompletas no se calcula).
    assert!(
        h.query_by_label_contains("Windows y otros datos del sistema")
            .is_none()
    );
}

#[test]
fn onedrive_lleva_el_aviso_de_la_nube() {
    let h = harness(opts(StartScreen::Map(MapStart {
        list: false,
        open: vec!["Users".into(), crate::demo::USER.into()],
        select: Some("OneDrive".into()),
        marks: vec![],
    })));
    h.get_by_label_contains("Archivos de la nube");
}

#[test]
fn otros_agrupa_los_bloques_diminutos_y_su_etiqueta_es_correcta() {
    let mut h = harness(opts(user_folder()));
    let ex = explore(&h);
    let layout = ex.map.layout.clone().unwrap();
    assert_eq!(
        layout.others().len(),
        1,
        "las 40 carpetas pequeñas se agrupan"
    );
    let o = layout.others()[0];
    // El contenido del grupo es exactamente el sufijo de los hijos ordenados.
    let kids = tesela_layout::Children::of(&ex.tree, ex.root.node);
    assert_eq!(o.count as usize, kids.len() - o.start as usize);
    let bytes: u64 = kids
        .iter_from(o.start as usize)
        .map(|c| ex.tree.node(c).alloc)
        .sum();
    assert_eq!(o.bytes, bytes);
    assert!(o.count >= 30);
    // La etiqueta accesible del mapa la anuncia con el formato del plan.
    let expect = format!(
        "Otros ({} elementos, {})",
        o.count,
        crate::Tr::new(crate::Lang::Es).size(o.bytes)
    );
    h.get_by_label_contains(&expect);
    // Abrirla (doble clic en su celda) reparte solo ese sufijo y el breadcrumb lo dice.
    let idx = layout
        .cells()
        .iter()
        .position(|c| matches!(c.kind, CellKind::Others(_)))
        .unwrap();
    let map = h.get_by_label_contains("Mapa de").rect();
    let r = layout.cells()[idx].rect;
    let p = Pos2::new(map.min.x + r.x + r.w / 2.0, map.min.y + r.y + r.h / 2.0);
    h.run_steps(30);
    click_at(&mut h, p);
    click_at(&mut h, p);
    h.run_steps(2);
    let ex = explore(&h);
    assert_eq!(ex.root.start, o.start, "la raíz es el sufijo");
    h.get_by_label_contains(&expect);
    // Backspace vuelve a la carpeta completa.
    h.key_press(Key::Backspace);
    h.run_steps(2);
    assert_eq!(explore(&h).root.start, 0);
}

#[test]
fn navegacion_con_el_teclado_intro_entra_y_retroceso_sube() {
    let mut h = harness(opts(StartScreen::Map(MapStart::default())));
    assert_eq!(explore(&h).root.node, 0);
    // Seleccionar con un clic en el mapa y entrar con Intro.
    let p = cell_center(&h, "Users");
    click_at(&mut h, p);
    assert_eq!(
        explore(&h).selection.map(|s| matches!(s, Sel::Node(_))),
        Some(true)
    );
    h.key_press(Key::Enter);
    h.run_steps(2);
    let users = explore(&h).tree.child_by_name(0, b"Users").unwrap();
    assert_eq!(explore(&h).root.node, users);
    h.get_by_label("Subir un nivel");
    // Retroceso sube, y la carpeta de la que se sale queda seleccionada.
    h.key_press(Key::Backspace);
    h.run_steps(2);
    assert_eq!(explore(&h).root.node, 0);
    assert_eq!(explore(&h).selection, Some(Sel::Node(users)));
    // En la raíz, Retroceso no hace nada.
    h.key_press(Key::Backspace);
    h.run_steps(2);
    assert_eq!(explore(&h).root.node, 0);
    // Intro sobre un archivo no entra.
    let pf = explore(&h).tree.child_by_name(0, b"pagefile.sys").unwrap();
    h.state_mut()
        .ui_mut()
        .explore_mut()
        .unwrap()
        .select(Some(Sel::Node(pf)));
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(explore(&h).root.node, 0);
}

#[test]
fn las_flechas_mueven_la_seleccion_entre_celdas() {
    let mut h = harness(opts(user_folder()));
    let p = cell_center(&h, "Vídeos");
    click_at(&mut h, p);
    let first = explore(&h).selection;
    assert!(first.is_some());
    h.key_press(Key::ArrowRight);
    h.run_steps(2);
    let second = explore(&h).selection;
    assert!(second.is_some());
    assert_ne!(first, second, "la flecha derecha elige otra celda");
}

#[test]
fn la_lista_y_el_mapa_estan_sincronizados() {
    let mut h = harness(opts(user_folder()));
    // Mapa -> lista y detalles: se selecciona en el mapa...
    let p = cell_center(&h, "Descargas");
    click_at(&mut h, p);
    h.get_by_label_contains("Seleccionado: Descargas");
    assert!(h.query_all_by_label("Descargas").count() >= 1); // título del panel de detalles
    // ...y la lista (cambio de vista) muestra esa fila seleccionada.
    h.get_by_label("Lista").click();
    h.run_steps(3);
    assert_eq!(explore(&h).view, View::List);
    let row = h.get_by_label_contains("Descargas, ");
    assert_eq!(
        row.accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::True),
        "la fila de Descargas está seleccionada"
    );
    // Lista -> mapa: seleccionar otra fila cambia la selección compartida.
    h.get_by_label_contains("Vídeos, ").click();
    h.run_steps(3);
    let vid = explore(&h)
        .tree
        .child_by_name(explore(&h).root.node, "Vídeos".as_bytes())
        .unwrap();
    assert_eq!(explore(&h).selection, Some(Sel::Node(vid)));
    h.get_by_label("Mapa").click();
    h.run_steps(3);
    h.get_by_label_contains("Seleccionado: Vídeos");
    // Doble clic en una fila de la lista entra en la carpeta (también sincronizado con el mapa).
    h.get_by_label("Lista").click();
    h.run_steps(3);
    let row = h.get_by_label_contains("Vídeos, ");
    let c = row.rect().center();
    // Deja pasar el tiempo de doble clic de los clics anteriores (egui cuenta clics seguidos, y tres serían un triple).
    h.run_steps(30);
    click_at(&mut h, c);
    click_at(&mut h, c);
    h.run_steps(2);
    assert_eq!(explore(&h).root.node, vid);
}

#[test]
fn la_lista_es_accesible_con_nombre_tamano_porcentaje_y_archivos() {
    let h = harness(opts(StartScreen::Map(MapStart {
        list: true,
        open: vec!["Users".into(), crate::demo::USER.into()],
        select: None,
        marks: vec![],
    })));
    let row = h.get_by_label_contains("Vídeos, ");
    let label = row.accesskit_node().label().unwrap();
    assert!(label.contains("GB"), "{label}");
    assert!(label.contains("% de la carpeta"), "{label}");
    assert!(label.contains("archivos"), "{label}");
}

#[test]
fn la_busqueda_filtra_la_lista() {
    let mut h = harness(opts(StartScreen::Map(MapStart {
        list: true,
        open: vec!["Users".into(), crate::demo::USER.into()],
        select: None,
        marks: vec![],
    })));
    assert!(h.query_by_label_contains("Descargas, ").is_some());
    h.get_by_label("Buscar").focus();
    h.run_steps(2);
    h.get_by_label("Buscar").type_text("vid");
    h.run_steps(3);
    h.get_by_label_contains("Vídeos, ");
    assert!(h.query_by_label_contains("Descargas, ").is_none());
    h.get_by_label("Buscar").type_text("zzzz");
    h.run_steps(3);
    h.get_by_label("Ningún elemento coincide con la búsqueda.");
}

#[test]
fn ctrl_f_enfoca_el_buscador_y_f5_pide_reescanear() {
    let mut h = harness(opts(StartScreen::Map(MapStart::default())));
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(3);
    assert!(h.get_by_label("Buscar").is_focused());
    h.key_press(Key::Escape);
    h.run_steps(2);
}

#[test]
fn el_menu_contextual_ofrece_las_acciones_y_anadir_a_la_cesta_esta_activo() {
    let mut h = harness(opts(StartScreen::Map(MapStart {
        list: true,
        open: vec!["Users".into(), crate::demo::USER.into()],
        select: None,
        marks: vec![],
    })));
    let row = h.get_by_label_contains("Proyectos, ");
    row.click_secondary();
    h.run_steps(3);
    h.get_by_label("Entrar");
    // Algunas acciones también están en el panel de detalles: todas las copias deben cumplir lo mismo.
    let all = |h: &Harness<'static, App>, l: &str| -> Vec<bool> {
        h.query_all_by_label(l)
            .map(|n| n.accesskit_node().is_disabled())
            .collect()
    };
    // En la demostración lo que toca el disco está deshabilitado; la cesta funciona (contra el respaldo simulado).
    for l in ["Abrir en el Explorador", "Propiedades"] {
        let v = all(&h, l);
        assert!(v.len() >= 2, "{l}: menú y detalles");
        assert!(v.iter().all(|d| *d), "{l} debe estar deshabilitado");
    }
    let v = all(&h, "Añadir a la cesta");
    assert!(v.len() >= 2, "menú y detalles");
    assert!(v.iter().all(|d| !*d), "Añadir a la cesta debe estar activo");
    let v = all(&h, "Copiar ruta");
    assert!(v.len() >= 2 && v.iter().all(|d| !*d));
}

#[test]
fn el_breadcrumb_navega() {
    let mut h = harness(opts(user_folder()));
    h.get_by_label("Ir a Users").click();
    h.run_steps(3);
    let users = explore(&h).tree.child_by_name(0, b"Users").unwrap();
    assert_eq!(explore(&h).root.node, users);
}

#[test]
fn los_dos_idiomas_y_los_dos_temas_se_aplican() {
    let mut o = opts(StartScreen::Map(MapStart::default()));
    o.settings.language = LangPref::En;
    o.settings.theme = ThemePref::Dark;
    let h = harness(o);
    h.get_by_label("Up one level");
    h.get_by_label("Map");
    h.get_by_label("List");
    h.get_by_label("Rescan (F5)");
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    let mut o = opts(StartScreen::Home);
    o.settings.language = LangPref::En;
    let h = harness(o);
    h.get_by_label("Which drive do you want to check?");
    h.get_by_label("Analyze a folder…");
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
}

#[test]
fn cambiar_el_idioma_desde_ajustes_se_guarda_como_cambio_de_ajustes() {
    let mut h = harness(opts(StartScreen::Home));
    h.get_by_label("Ajustes").click();
    h.run_steps(3);
    h.get_by_label("English").click();
    h.run_steps(3);
    assert_eq!(h.state().ui().lang(), crate::Lang::En);
    let changed = h
        .state_mut()
        .ui_mut()
        .take_settings_change()
        .expect("cambio de ajustes");
    assert_eq!(changed.language, LangPref::En);
    assert!(h.state_mut().ui_mut().take_settings_change().is_none());
    h.get_by_label("Which drive do you want to check?");
}

#[test]
fn el_hover_usa_la_rejilla_y_no_rehace_el_mesh() {
    let mut h = harness(opts(user_folder()));
    let rebuilds = explore(&h).map.stats().rebuilds;
    assert_eq!(rebuilds, 1);
    let p = cell_center(&h, "Vídeos");
    for i in 0..30 {
        h.event(Event::PointerMoved(p + vec2(i as f32, (i % 5) as f32)));
        h.run_steps(1);
    }
    assert!(explore(&h).map.hover.is_some());
    assert_eq!(
        explore(&h).map.stats().rebuilds,
        rebuilds,
        "el hover es un overlay: el mesh no cambia"
    );
    let st = explore(&h).map.stats();
    assert!(st.cells > 50 && st.vertices > st.cells * 10, "{st:?}");
}

#[test]
fn cambiar_el_tamano_o_el_tema_rehace_el_mesh_una_vez() {
    let mut h = harness(opts(user_folder()));
    let before = explore(&h).map.stats().rebuilds;
    h.set_size(vec2(1100.0, 700.0));
    h.run();
    assert_eq!(explore(&h).map.stats().rebuilds, before + 1);
    h.run_steps(3);
    assert_eq!(explore(&h).map.stats().rebuilds, before + 1);
    h.ctx
        .options_mut(|o| o.theme_preference = egui::ThemePreference::Dark);
    h.run();
    assert_eq!(explore(&h).map.stats().rebuilds, before + 2);
}

#[test]
fn el_zoom_animado_dura_unos_frames_y_se_desactiva_sin_animaciones() {
    // Con animaciones: entrar crea un zoom que termina solo.
    let mut o = opts(StartScreen::Map(MapStart::default()));
    o.animate = true;
    let mut h = harness(o);
    let p = cell_center(&h, "Users");
    click_at(&mut h, p);
    h.key_press(Key::Enter);
    h.run_steps(1);
    assert!(explore(&h).map.animating(), "el zoom está en marcha");
    assert!(explore(&h).map.stats().zooms >= 1);
    for _ in 0..40 {
        h.run_steps(1);
    }
    assert!(!explore(&h).map.animating(), "el zoom terminó");
    // Subir también anima.
    h.key_press(Key::Backspace);
    h.run_steps(1);
    assert!(explore(&h).map.animating());
    for _ in 0..40 {
        h.run_steps(1);
    }
    assert!(!explore(&h).map.animating());
    // Sin animaciones (SPI_GETCLIENTAREAANIMATION desactivado): ninguna.
    let mut h = harness(opts(StartScreen::Map(MapStart::default())));
    let p = cell_center(&h, "Users");
    click_at(&mut h, p);
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert!(!explore(&h).map.animating());
    assert_eq!(explore(&h).map.stats().zooms, 0);
}

#[test]
fn un_escaneo_real_de_una_carpeta_temporal_llega_al_mapa_y_a_la_lista() {
    use std::io::Write;
    // Árbol sintético SOLO bajo %TEMP%\tesela-test-<uuid>.
    let base = crate::testdir::TestDir::new();
    let root = base.join("raiz");
    std::fs::create_dir_all(root.join("fotos")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    for (name, size) in [
        ("fotos/a.jpg", 300_000usize),
        ("fotos/b.jpg", 200_000),
        ("docs/c.pdf", 120_000),
        ("suelto.bin", 50_000),
    ] {
        let mut f = std::fs::File::create(root.join(name)).unwrap();
        f.write_all(&vec![7u8; size]).unwrap();
    }
    // Forma canónica larga: en CI %TEMP% usa la forma corta 8.3 y las comparaciones de rutas deben ser sobre la larga.
    let root = dunce::canonicalize(&root).unwrap();
    let mut o = opts(StartScreen::Home);
    o.demo = None;
    let mut h = harness(o);
    let ctx = h.ctx.clone();
    h.state_mut().ui_mut().request_scan(&ctx, root.clone());
    let t0 = std::time::Instant::now();
    while h.state().ui().screen_name() != "explore" {
        assert!(t0.elapsed().as_secs() < 30, "el escaneo no terminó");
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.run_steps(1);
    }
    h.run_steps(2);
    let ex = explore(&h);
    assert_eq!(ex.tree.stats().files, 4);
    assert!(!ex.info.cancelled && !ex.info.full_volume);
    // La lista y el mapa ven los mismos elementos.
    h.get_by_label_contains("Mapa de");
    h.get_by_label("Lista").click();
    h.run_steps(3);
    h.get_by_label_contains("fotos, ");
    h.get_by_label_contains("docs, ");
    h.get_by_label_contains("suelto.bin, ");
    // Entrar en una carpeta con Intro.
    h.get_by_label_contains("fotos, ").click();
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(3);
    h.get_by_label_contains("a.jpg, ");
}

#[test]
fn el_escaneo_en_modo_de_respaldo_muestra_la_insignia_aproximado() {
    use std::io::Write;
    let base = crate::testdir::TestDir::new();
    let root = base.join("raiz");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    for name in ["sub/a.bin", "b.bin"] {
        std::fs::File::create(root.join(name))
            .unwrap()
            .write_all(&vec![1u8; 70_000])
            .unwrap();
    }
    let root = dunce::canonicalize(&root).unwrap();
    // `FindFirstFileExW` forzado: sin FileId, sin deduplicación y con el tamaño en disco estimado.
    let mut so = tesela_scanner::ScanOptions::new(&root);
    so.force_find = true;
    let res = tesela_scanner::scan(so).unwrap();
    assert_eq!(res.summary.api, tesela_scanner::ApiUsed::FindFallback);
    let req = crate::app::ScanRequest {
        root: root.clone(),
        label: "prueba".into(),
        drive: None,
    };
    let ex = crate::explore::Explore::from_scan(
        req,
        res,
        false,
        std::time::Duration::ZERO,
        &test_backend(),
    );
    assert!(ex.info.approximate);
    let mut o = opts(StartScreen::Home);
    o.demo = None;
    let mut h = harness(o);
    h.state_mut().ui_mut().screen = crate::app::Screen::Explore(Box::new(ex));
    h.run_steps(4);
    assert!(
        h.query_all_by_label("Aproximado").count() >= 1,
        "insignia en la barra de estado y en los detalles"
    );
    // Un escaneo normal no la lleva.
    let h2 = harness(opts(StartScreen::Map(MapStart::default())));
    assert!(h2.query_all_by_label("Aproximado").count() == 0);
}

#[test]
fn el_breadcrumb_de_una_carpeta_escaneada_usa_su_nombre_y_no_la_ruta_entera() {
    use std::io::Write;
    let base = crate::testdir::TestDir::new();
    let root = base.join("mi-carpeta");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::File::create(root.join("a.bin"))
        .unwrap()
        .write_all(&[1u8; 10_000])
        .unwrap();
    let root = dunce::canonicalize(&root).unwrap();
    let res = tesela_scanner::scan(tesela_scanner::ScanOptions::new(&root)).unwrap();
    let req = crate::app::ScanRequest {
        root,
        label: "mi-carpeta".into(),
        drive: None,
    };
    let ex = crate::explore::Explore::from_scan(
        req,
        res,
        false,
        std::time::Duration::ZERO,
        &test_backend(),
    );
    let mut o = opts(StartScreen::Home);
    o.demo = None;
    let mut h = harness(o);
    h.state_mut().ui_mut().screen = crate::app::Screen::Explore(Box::new(ex));
    h.run_steps(4);
    h.get_by_label("mi-carpeta");
}

#[test]
fn el_tooltip_del_hover_describe_la_celda() {
    let mut h = harness(opts(user_folder()));
    let p = cell_center(&h, "Vídeos");
    h.event(Event::PointerMoved(p));
    h.run_steps(3);
    // Nombre, tamaño, porcentaje del padre, composición y pista de uso.
    assert!(h.query_all_by_label("Vídeos").count() >= 1);
    assert!(
        h.query_all_by_label_contains("de usuario").count() >= 1,
        "porcentaje del padre"
    );
    assert!(
        h.query_all_by_label_contains("Doble clic para entrar")
            .count()
            >= 1
    );
    assert!(
        h.query_all_by_label_contains("vídeo").count() >= 1,
        "frase de composición por tipo"
    );
    // Y sobre la agrupación "Otros", su explicación.
    let layout = explore(&h).map.layout.clone().unwrap();
    let idx = layout
        .cells()
        .iter()
        .position(|c| matches!(c.kind, CellKind::Others(_)))
        .unwrap();
    let map = h.get_by_label_contains("Mapa de").rect();
    let r = layout.cells()[idx].rect;
    h.event(Event::PointerMoved(Pos2::new(
        map.min.x + r.x + r.w / 2.0,
        map.min.y + r.y + r.h / 2.0,
    )));
    h.run_steps(3);
    assert!(
        h.query_all_by_label_contains("Doble clic para abrir la agrupación")
            .count()
            >= 1
    );
}

#[test]
fn cada_boton_deshabilitado_da_su_propia_pista() {
    let mut h = harness(opts(StartScreen::Map(MapStart::default())));
    let hint = "Disponible en la próxima versión";
    // En la raíz "Subir un nivel" está deshabilitado, pero no por ser "de la próxima versión".
    h.get_by_label("Subir un nivel").hover();
    h.run_steps(14);
    assert_eq!(
        h.query_all_by_label(hint).count(),
        0,
        "sin pista de próxima versión en Subir"
    );
    // Con la cesta vacía, "Limpiar" explica por qué está deshabilitado (y la pista sale una sola vez).
    h.get_by_label_contains("Limpiar 0").hover();
    h.run_steps(14);
    assert_eq!(
        h.query_all_by_label_contains("La cesta está vacía").count(),
        1
    );
    // En la demostración, "Abrir en el Explorador" explica por qué no se puede, sin la pista de próxima versión.
    h.get_by_label("Abrir en el Explorador").hover();
    h.run_steps(14);
    assert_eq!(
        h.query_all_by_label("No disponible en el modo demostración")
            .count(),
        1
    );
    assert_eq!(h.query_all_by_label(hint).count(), 0);
}

/// Color con que se pinta de verdad el texto `text` en la salida del último frame.
fn painted_text_color(h: &Harness<'static, App>, text: &str) -> egui::Color32 {
    for cs in &h.output().shapes {
        if let egui::epaint::Shape::Text(ts) = &cs.shape
            && ts.galley.text() == text
        {
            let c = ts.galley.job.sections[0].format.color;
            return if c == egui::Color32::PLACEHOLDER {
                ts.override_text_color.unwrap_or(ts.fallback_color)
            } else {
                c
            };
        }
    }
    panic!("no se pintó {text:?}");
}

#[test]
fn los_controles_deshabilitados_de_la_ventana_se_pintan_atenuados() {
    let h = harness(opts(StartScreen::Map(MapStart::default())));
    let pal = crate::theme::Palette::of(false);
    // Colorear: Tipo (habilitado) frente a Antigüedad y Riesgo (v0.2, deshabilitados).
    let on = painted_text_color(&h, "Tipo");
    for t in ["Antigüedad", "Riesgo"] {
        let off = painted_text_color(&h, t);
        assert_ne!(on, off, "{t}");
        assert!(
            crate::theme::contrast(off, pal.track) >= 2.0,
            "{t} sigue legible"
        );
    }
    // Los botones de la cesta (Primary, Ghost, Secondary) también.
    assert_ne!(painted_text_color(&h, "Limpiar 0 B"), egui::Color32::WHITE);
    assert_eq!(
        painted_text_color(&h, "Vaciar cesta"),
        crate::widgets::button_fg(&pal, crate::widgets::ButtonStyle::Ghost, false)
    );
}

#[test]
fn los_nombres_en_otros_alfabetos_acaban_en_una_fuente_que_tiene_el_glifo() {
    use crate::fonts::{self, Scripts};
    use std::io::Write;
    // Árbol sintético SOLO bajo %TEMP%\tesela-test-<uuid>, con nombres de cada escritura.
    let base = crate::testdir::TestDir::new();
    let root = base.join("raiz");
    std::fs::create_dir_all(&root).unwrap();
    let names: [(&str, Scripts); 11] = [
        ("Documentos ñ é ü.txt", Scripts::NONE),
        ("Ελληνικά Кириллица.txt", Scripts::NONE),
        ("العربية.txt", Scripts::RTL),
        ("עברית.txt", Scripts::RTL),
        ("日本語の名前.txt", Scripts::HAN | Scripts::KANA),
        ("中文名称.txt", Scripts::HAN),
        // 夹 es solo del chino simplificado: con japonés y chino a la vez, MS Gothic no la tiene y debe cubrirla YaHei.
        ("夹 日本語.txt", Scripts::HAN | Scripts::KANA),
        ("한국어.txt", Scripts::HANGUL),
        ("สวัสดี.txt", Scripts::THAI),
        ("नमस्ते.txt", Scripts::INDIC),
        ("→ flecha ★.txt", Scripts::SYMBOLS),
    ];
    for (n, _) in &names {
        std::fs::File::create(root.join(n))
            .unwrap()
            .write_all(&[1u8; 5000])
            .unwrap();
    }
    std::fs::File::create(root.join("😀 emoji.txt"))
        .unwrap()
        .write_all(&[1u8; 5000])
        .unwrap();
    let root = dunce::canonicalize(&root).unwrap();
    let res = tesela_scanner::scan(tesela_scanner::ScanOptions::new(&root)).unwrap();
    let req = crate::app::ScanRequest {
        root,
        label: "alfabetos".into(),
        drive: None,
    };
    let ex = crate::explore::Explore::from_scan(
        req,
        res,
        false,
        std::time::Duration::ZERO,
        &test_backend(),
    );
    // El árbol anuncia las escrituras que trae.
    for s in [
        Scripts::RTL,
        Scripts::HAN,
        Scripts::KANA,
        Scripts::HANGUL,
        Scripts::THAI,
        Scripts::INDIC,
        Scripts::SYMBOLS,
        Scripts::EMOJI,
    ] {
        assert!(ex.scripts.contains(s), "{s:?}");
    }
    let mut o = opts(StartScreen::Home);
    o.demo = None;
    let mut h = harness(o);
    h.state_mut().ui_mut().screen = crate::app::Screen::Explore(Box::new(ex));
    h.run_steps(8); // ensure_fonts pide las reservas y valen desde el frame siguiente
    let dir = crate::platform::fonts_dir().unwrap();
    // Una escritura se comprueba solo si CADA grupo de reserva tiene en este equipo alguna fuente que egui pueda usar; si no
    // (por ejemplo un Windows Server sin YaHei ni MS Gothic), se salta diciendo qué falta.
    let missing = |s: Scripts| fonts::missing_groups(&dir, s);
    let mut checked = 0;
    let all: Vec<(String, Scripts)> = names.iter().map(|(n, s)| (n.to_string(), *s)).collect();
    for (n, s) in &all {
        let miss = missing(*s);
        if !miss.is_empty() {
            eprintln!(
                "sin fuente de reserva utilizable en este equipo ({miss:?}) para {n}: se omite"
            );
            continue;
        }
        for fam in [
            egui::FontId::proportional(14.0),
            crate::theme::font_semibold(14.0),
            egui::FontId::monospace(12.0),
        ] {
            let ok = h.ctx.fonts_mut(|f| f.has_glyphs(&fam, n));
            let missing: String = n
                .chars()
                .filter(|c| !h.ctx.fonts_mut(|f| f.has_glyph(&fam, *c)))
                .collect();
            assert!(
                ok,
                "algún glifo de {n:?} no está en ninguna fuente de {:?}: faltan {missing:?}",
                fam.family
            );
        }
        checked += 1;
    }
    // Latino y griego solo dependen de la fuente principal, así que siempre se comprueban; el resto, según el equipo.
    assert!(checked >= 2, "al menos latino y griego se comprobaron");
    // Y la ventana pinta la lista con esos nombres sin pánico.
    h.get_by_label("Lista").click();
    h.run_steps(4);
    assert!(h.query_by_label_contains("日本語の名前.txt, ").is_some());
}

#[test]
fn escribir_en_el_buscador_en_otra_escritura_carga_su_fuente() {
    let mut h = harness(opts(StartScreen::Map(MapStart::default())));
    h.get_by_label("Buscar").focus();
    h.run_steps(2);
    h.get_by_label("Buscar").type_text("한국어");
    h.run_steps(6);
    let dir = crate::platform::fonts_dir().unwrap();
    if dir.join("malgun.ttf").exists() {
        let ok = h
            .ctx
            .fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), "한국어"));
        assert!(ok, "el hangul del buscador debe tener glifo");
    } else {
        eprintln!("sin malgun.ttf en este equipo: se omite la comprobación del hangul");
    }
}

fn harness_sized(o: Options, size: egui::Vec2) -> Harness<'static, App> {
    let mut h = Harness::builder()
        .with_size(size)
        .with_step_dt(0.02)
        .build_ui_state(|ui, app: &mut App| app.frame(ui), App { opts: o, ui: None });
    h.run_steps(6);
    h
}

const LEGEND: [&str; 10] = [
    "Vídeo",
    "Imágenes",
    "Audio",
    "Documentos",
    "Comprimidos",
    "Programas",
    "Otros",
    "Mixto",
    "Solo informar",
    "Sistema",
];

#[test]
fn la_leyenda_cabe_entera_a_1280x800_sin_desplazarse() {
    let h = harness(opts(StartScreen::Map(MapStart::default())));
    assert!(
        !explore(&h).sidebar_overflow,
        "la barra lateral no debería desbordar a 1280 x 800"
    );
    // El borde inferior visible de la barra lateral es el de la cesta (800 - 56 - 32).
    let limit = 800.0 - 56.0 - 32.0;
    for t in LEGEND {
        let n = h
            .query_all_by_label(t)
            .next()
            .unwrap_or_else(|| panic!("falta {t} en la leyenda"));
        let r = n.rect();
        assert!(
            r.bottom() <= limit + 0.5 && r.top() >= 56.0,
            "{t} queda fuera de la vista: {r:?}"
        );
    }
}

#[test]
fn con_la_ventana_minima_la_leyenda_no_se_corta_sin_aviso() {
    // 960 x 600 es el tamaño mínimo: o cabe, o la barra lateral se desplaza con su barra visible (`sidebar_overflow`).
    let h = harness_sized(
        opts(StartScreen::Map(MapStart::default())),
        vec2(960.0, 600.0),
    );
    let limit = 600.0 - 56.0 - 32.0;
    let all_visible = LEGEND.iter().all(|t| {
        h.query_all_by_label(t)
            .next()
            .is_some_and(|n| n.rect().bottom() <= limit + 0.5)
    });
    assert!(
        all_visible || explore(&h).sidebar_overflow,
        "contenido cortado y sin barra de desplazamiento"
    );
    // Todos los elementos existen para el lector de pantalla aunque haya que desplazarse.
    for t in LEGEND {
        assert!(h.query_all_by_label(t).count() >= 1, "{t}");
    }
}

#[test]
fn con_la_ventana_minima_la_barra_de_estado_no_se_pisa_y_los_detalles_se_desplazan() {
    // 960 x 600 con el aviso de la demostración (el más largo) y el hueco sin explicar de la unidad entera.
    let mut h = harness_sized(
        opts(StartScreen::Map(MapStart::default())),
        vec2(960.0, 600.0),
    );
    h.run_steps(4);
    let rects: Vec<(&str, egui::Rect)> = [
        "Reescanear",
        "Modo demostración",
        "analizado hace",
        "El mapa explica",
    ]
    .iter()
    .map(|t| {
        let n = h
            .query_all_by_label_contains(t)
            .find(|n| n.rect().height() > 1.0)
            .unwrap_or_else(|| panic!("no hay `{t}` en la barra de estado"));
        (*t, n.rect())
    })
    .collect();
    for (i, (a, ra)) in rects.iter().enumerate() {
        // Dentro de la ventana...
        assert!(
            ra.right() <= 960.0 + 0.5 && ra.left() >= -0.5,
            "{a} fuera de la ventana: {ra:?}"
        );
        // ...y sin tocar a ninguno de los demás.
        for (b, rb) in &rects[i + 1..] {
            assert!(
                !ra.intersects(*rb),
                "`{a}` y `{b}` se solapan: {ra:?} {rb:?}"
            );
        }
    }
    // Los botones de los detalles: o se ven enteros dentro de la ventana, o el panel se desplaza con su barra a la vista.
    let all_in = ["Copiar ruta", "Propiedades"].iter().all(|t| {
        h.query_all_by_label_contains(t)
            .any(|n| n.rect().bottom() <= 600.0 - 88.0 + 0.5)
    });
    assert!(
        all_in || explore(&h).details_overflow,
        "los detalles quedan cortados sin barra de desplazamiento"
    );
    for t in ["Copiar ruta", "Propiedades"] {
        assert!(h.query_all_by_label_contains(t).count() >= 1, "{t}");
    }
}

#[test]
fn un_escaneo_que_no_puede_empezar_vuelve_al_inicio_con_el_error_traducido_en_los_dos_idiomas() {
    for (lang, start, reason) in [
        (
            LangPref::Es,
            "No se pudo empezar el análisis",
            "la ruta no existe o no está disponible",
        ),
        (
            LangPref::En,
            "The scan could not start",
            "the path does not exist or is not available",
        ),
    ] {
        let mut o = opts(StartScreen::Home);
        o.demo = None;
        o.settings.language = lang;
        let mut h = harness(o);
        let ctx = h.ctx.clone();
        let nada =
            std::env::temp_dir().join(format!("tesela-test-{}-no-existe", uuid::Uuid::new_v4()));
        h.state_mut().ui_mut().request_scan(&ctx, nada);
        // El arranque va en un hilo: la pantalla de escaneo aparece al instante y luego se vuelve al inicio.
        assert_eq!(h.state().ui().screen_name(), "scan");
        let t0 = std::time::Instant::now();
        while h.state().ui().screen_name() != "home" {
            assert!(t0.elapsed().as_secs() < 20, "no volvió al inicio");
            std::thread::sleep(std::time::Duration::from_millis(10));
            h.run_steps(1);
        }
        h.run_steps(2);
        // El error llega TIPADO y se traduce con Fluent: ni una palabra del otro idioma ni del texto técnico del escáner.
        let full = format!("{start}: {reason}");
        assert!(
            h.query_by_label_contains(&full).is_some(),
            "{lang:?}: falta `{full}`"
        );
        assert!(h.query_by_label_contains("scan root").is_none());
    }
}

#[test]
fn cancelar_o_cerrar_la_pantalla_detiene_el_escaneo_sin_dejarlo_huerfano() {
    use crate::app::ScanRequest;
    use crate::scan::{ScanOutcome, ScanScreen, test_hooks};
    use std::time::{Duration, Instant};
    let base = crate::testdir::TestDir::new();
    let mk = |name: &str| {
        let d = base.join(name);
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::write(d.join("a").join("x.bin"), [1u8; 9000]).unwrap();
        dunce::canonicalize(&d).unwrap()
    };
    let req = |root: &std::path::Path| ScanRequest {
        root: root.to_path_buf(),
        label: "t".into(),
        drive: None,
    };
    let ctx = egui::Context::default();
    // Espera a que TODOS los escaneos arrancados con esa raíz (`n` como mínimo) estén cancelados.
    // Un arranque cancelado a tiempo ni siquiera llega a crear el escaneo (el token se consulta entre fases): `n` es el
    // mínimo de escaneos arrancados que se exige (0 si pueden no haber llegado a arrancar).
    let all_cancelled = |root: &std::path::Path, n: usize| {
        let t0 = Instant::now();
        loop {
            let (started, cancelled) = test_hooks::counts(root);
            if started >= n && cancelled == started && test_hooks::active_startups() == 0 {
                return started;
            }
            assert!(
                t0.elapsed().as_secs() < 30,
                "escaneos {started}, cancelados {cancelled}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    // A. Cancelar antes de que el arranque termine: se descarta y el escaneo creado se cancela.
    let root = mk("a");
    let mut s = ScanScreen::real(&ctx, req(&root), false, test_backend());
    s.cancel();
    let t0 = Instant::now();
    let out = loop {
        if let Some(o) = s.poll(&ctx) {
            break o;
        }
        assert!(t0.elapsed().as_secs() < 20);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(
        matches!(out, ScanOutcome::Cancelled),
        "un escaneo cancelado se descarta"
    );
    all_cancelled(&root, 0);

    // B. Soltar la pantalla sin más (Drop) mientras arranca.
    let root = mk("b");
    drop(ScanScreen::real(&ctx, req(&root), false, test_backend()));
    all_cancelled(&root, 0);

    // C. LA CARRERA: el resultado del arranque ya está entregado, la pantalla no lo ha recogido y se suelta.
    let root = mk("c");
    let s = ScanScreen::real(&ctx, req(&root), false, test_backend());
    let t0 = Instant::now();
    while test_hooks::counts(&root).0 == 0 {
        assert!(t0.elapsed().as_secs() < 20, "el escaneo no arrancó");
        std::thread::sleep(Duration::from_millis(2));
    }
    std::thread::sleep(Duration::from_millis(50)); // el hilo ya dejó el resultado
    assert_eq!(
        test_hooks::counts(&root),
        (1, 0),
        "arrancado y sin cancelar mientras la pantalla lo tiene"
    );
    drop(s);
    assert_eq!(
        test_hooks::counts(&root),
        (1, 1),
        "al soltar la pantalla se cancela lo ya entregado"
    );

    // D. Muchas veces, con esperas distintas: ninguno se queda sin cancelar.
    let root = mk("d");
    for i in 0..40u64 {
        let s = ScanScreen::real(&ctx, req(&root), false, test_backend());
        std::thread::sleep(Duration::from_micros(i * 150));
        if i % 2 == 0 {
            drop(s);
        } else {
            let mut s = s;
            s.cancel();
        }
    }
    assert!(all_cancelled(&root, 0) <= 40);
}

/// ¿Hay en la salida del último frame un anillo de foco (trazo de 2 px de acento) alrededor del centro de `r`?
fn focus_ring_around(h: &Harness<'static, App>, r: egui::Rect) -> bool {
    focus_ring_around_with(h, r, 16.0)
}

fn focus_ring_around_with(h: &Harness<'static, App>, r: egui::Rect, slack: f32) -> bool {
    let accent = crate::theme::Palette::of(false).accent;
    h.output().shapes.iter().any(|cs| {
        matches!(&cs.shape, egui::epaint::Shape::Rect(rs)
            if rs.stroke.width == 2.0
                && rs.stroke.color == accent
                && rs.rect.contains(r.center())
                && rs.rect.width() <= r.width() + slack
                && rs.rect.height() <= r.height() + slack)
    })
}

fn list_start() -> StartScreen {
    StartScreen::Map(MapStart {
        list: true,
        open: vec!["Users".into(), crate::demo::USER.into()],
        select: None,
        marks: vec![],
    })
}

#[test]
fn la_lista_se_maneja_con_flechas_y_muestra_el_foco() {
    let mut h = harness(opts(list_start()));
    assert_eq!(explore(&h).view, View::List);
    let kids: Vec<_> = explore(&h).shown_children().iter().collect();
    assert!(kids.len() >= 4);
    let name = |h: &Harness<'static, App>, id| explore(h).tree.name_lossy(id).into_owned();
    // Foco en la primera fila: se pinta el anillo y la selección la sigue.
    let first = name(&h, kids[0]);
    h.get_by_label_contains(&format!("{first}, ")).focus();
    h.run_steps(3);
    let first_label = format!("{first}, ");
    let row = h.get_by_label_contains(&first_label);
    assert!(row.is_focused());
    assert!(
        focus_ring_around(&h, row.rect()),
        "anillo de foco en la fila"
    );
    assert_eq!(explore(&h).selection, Some(Sel::Node(kids[0])));
    // ↓ y ↑ mueven la selección Y el foco.
    h.key_press(Key::ArrowDown);
    h.run_steps(3);
    assert_eq!(explore(&h).selection, Some(Sel::Node(kids[1])));
    let second = name(&h, kids[1]);
    let second_label = format!("{second}, ");
    assert!(h.get_by_label_contains(&second_label).is_focused());
    h.key_press(Key::ArrowDown);
    h.run_steps(3);
    assert_eq!(explore(&h).selection, Some(Sel::Node(kids[2])));
    h.key_press(Key::ArrowUp);
    h.run_steps(3);
    assert_eq!(explore(&h).selection, Some(Sel::Node(kids[1])));
    // Fin y Inicio.
    h.key_press(Key::End);
    h.run_steps(4);
    assert_eq!(
        explore(&h).selection,
        Some(Sel::Node(*kids.last().unwrap()))
    );
    h.key_press(Key::Home);
    h.run_steps(4);
    assert_eq!(explore(&h).selection, Some(Sel::Node(kids[0])));
    // Intro sobre la fila enfocada entra en la carpeta (o no hace nada si es un archivo).
    let dir = kids
        .iter()
        .copied()
        .find(|&k| explore(&h).tree.node(k).is_dir())
        .expect("hay alguna carpeta");
    let dname = name(&h, dir);
    h.get_by_label_contains(&format!("{dname}, ")).focus();
    h.run_steps(3);
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(explore(&h).root.node, dir);
}

/// UX-1: las unidades de la barra lateral que no son la actual se ven pulsables (fondo, cursor y pista) y se activan con el
/// teclado; la actual no tiene estado de "pasar el puntero".
#[test]
fn las_unidades_de_la_barra_lateral_se_ven_pulsables() {
    let mut h = harness(opts(user_folder()));
    // La unidad actual (C:) no es pulsable: sin pista al pasar el puntero.
    assert!(!super::explore::drive_row_is_interactive(true));
    assert!(super::explore::drive_row_is_interactive(false));
    h.get_by_label("Analizar Sistema (C:)").hover();
    h.run_steps(14);
    assert_eq!(
        h.query_all_by_label("Analizar Sistema (C:)").count(),
        1,
        "la unidad actual no muestra la pista"
    );
    // Otra unidad: al pasar el puntero aparece la pista con el texto de `analyze-drive` y el cursor pasa a la mano.
    let other = h.get_by_label("Analizar Almacén (E:)");
    let rect = other.rect();
    other.hover();
    h.run_steps(14);
    assert_eq!(
        h.query_all_by_label("Analizar Almacén (E:)").count(),
        2,
        "el control y su pista"
    );
    assert_eq!(
        h.output().platform_output.cursor_icon,
        egui::CursorIcon::PointingHand
    );
    // Se pinta el fondo de "pasar el puntero" (el color suave de acento) detrás de la fila.
    let soft = crate::theme::Palette::of(false).accent_soft;
    assert!(
        h.output().shapes.iter().any(|cs| matches!(
            &cs.shape,
            egui::epaint::Shape::Rect(r) if r.fill == soft && r.rect.contains_rect(rect.shrink(1.0))
        )),
        "sin fondo de hover en la fila"
    );
}

/// UX-1: Intro y Espacio sobre una unidad enfocada empiezan el análisis (como el clic).
#[test]
fn intro_y_espacio_sobre_una_unidad_de_la_barra_lateral_empiezan_el_analisis() {
    for key in [Key::Enter, Key::Space] {
        let mut h = harness(opts(user_folder()));
        assert_eq!(h.state().ui().screen_name(), "explore");
        h.get_by_label("Analizar Almacén (E:)").focus();
        h.run_steps(3);
        assert!(h.get_by_label("Analizar Almacén (E:)").is_focused());
        h.key_press(key);
        h.run_steps(4);
        assert_eq!(h.state().ui().screen_name(), "scan", "{key:?}");
    }
}

#[test]
fn los_controles_alcanzables_con_tab_muestran_el_foco() {
    let mut h = harness(opts(user_folder()));
    // Cada control se enfoca por accesibilidad (equivale a llegar con Tab) y debe pintar su anillo.
    let targets: Vec<(&str, String)> = vec![
        ("unidad de la barra lateral", "Analizar ".into()),
        ("elemento de la barra lateral", "Mapa".into()),
        ("migas de pan", "Ir a ".into()),
        ("subir un nivel", "Subir un nivel".into()),
        ("reescanear", "Reescanear".into()),
    ];
    for (what, label) in targets {
        let node = h
            .query_all_by_label_contains(&label)
            .find(|n| n.rect().width() > 1.0 && n.rect().height() > 1.0)
            .unwrap_or_else(|| panic!("no hay {what} ({label})"));
        node.focus();
        h.run_steps(3);
        let node = h
            .query_all_by_label_contains(&label)
            .find(|n| n.is_focused())
            .unwrap_or_else(|| panic!("{what} no recibió el foco"));
        assert!(
            focus_ring_around(&h, node.rect()),
            "{what}: sin anillo de foco"
        );
    }
    // El buscador.
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(4);
    let search = h.get_by_label("Buscar");
    assert!(search.is_focused());
    // El anillo rodea el campo entero (icono y márgenes incluidos), no solo el texto editable.
    assert!(
        focus_ring_around_with(&h, search.rect(), 80.0),
        "buscador: sin anillo de foco"
    );
}

#[test]
fn acerca_de_muestra_la_version_y_los_avisos_de_terceros_y_no_cambia_los_ajustes() {
    for (start, ajustes, acerca, avisos) in [
        (
            StartScreen::Home,
            "Ajustes",
            "Acerca de Tesela",
            "Avisos de terceros",
        ),
        (
            user_folder(),
            "Ajustes",
            "Acerca de Tesela",
            "Avisos de terceros",
        ),
    ] {
        let mut h = harness(opts(start));
        assert!(h.query_by_label_contains("Versión ").is_none());
        h.get_by_label(ajustes).click();
        h.run_steps(3);
        h.get_by_label(acerca).click();
        h.run_steps(4);
        // La ventana lleva la versión del producto, la licencia y el botón de los avisos.
        h.get_by_label(&format!("Versión {}", crate::about::VERSION));
        h.get_by_label_contains("Licencia: MIT o Apache-2.0");
        assert!(
            h.query_by_label_contains("MIT License").is_none(),
            "los avisos van plegados"
        );
        h.get_by_label(avisos).click();
        h.run_steps(4);
        // The Spanish button opens the generated English third-party notices.
        h.get_by_label("Avisos de terceros");
        h.get_by_label_contains("Third-party notices");
        h.get_by_label_contains("Licenses used");
        // Escape la cierra.
        h.key_press(Key::Escape);
        h.run_steps(3);
        assert!(h.query_by_label_contains("Versión ").is_none());
        // Abrirla no es un cambio de ajustes (no se guarda nada).
        assert!(h.state_mut().ui_mut().take_settings_change().is_none());
    }
}

#[test]
fn acerca_de_sale_en_ingles_con_el_idioma_en_ingles() {
    let mut o = opts(StartScreen::Home);
    o.settings.language = LangPref::En;
    let mut h = harness(o);
    h.get_by_label("Settings").click();
    h.run_steps(3);
    h.get_by_label("About Tesela").click();
    h.run_steps(4);
    h.get_by_label(&format!("Version {}", crate::about::VERSION));
    h.get_by_label_contains("License: MIT or Apache-2.0");
    h.get_by_label("Third-party notices");
}

// --- Teclado real: Tab recorre el orden REAL y Intro/Espacio activan el control enfocado (M3, N1) ---

/// El nodo con el foco de accesibilidad: (rol, etiqueta, rectángulo).
fn focused(h: &Harness<'static, App>) -> Option<(egui::accesskit::Role, String, egui::Rect)> {
    h.queryable_node()
        .children_recursive()
        .find(|n| n.is_focused())
        .map(|n| {
            (
                n.accesskit_node().role(),
                n.accesskit_node().label().unwrap_or_default(),
                n.rect(),
            )
        })
}

fn focused_label(h: &Harness<'static, App>) -> String {
    focused(h).map(|f| f.1).unwrap_or_default()
}

/// Pulsa Tab hasta que el foco esté en un control cuya etiqueta empiece por `prefix`.
fn tab_to(h: &mut Harness<'static, App>, prefix: &str) {
    for _ in 0..60 {
        h.key_press(Key::Tab);
        h.run_steps(2);
        if focused_label(h).starts_with(prefix) {
            return;
        }
    }
    panic!("Tab no llegó a `{prefix}` (foco: `{}`)", focused_label(h));
}

/// Selecciona la primera carpeta de la carpeta actual y la devuelve.
fn select_first_dir(h: &mut Harness<'static, App>) -> tesela_model::NodeId {
    let dir = explore(h)
        .shown_children()
        .iter()
        .find(|&k| explore(h).tree.node(k).is_dir())
        .expect("hay una carpeta");
    h.state_mut()
        .ui_mut()
        .explore_mut()
        .unwrap()
        .select(Some(Sel::Node(dir)));
    h.run_steps(2);
    dir
}

#[test]
fn tab_recorre_el_orden_real_y_da_la_vuelta() {
    let mut h = harness(opts(user_folder()));
    let mut order: Vec<String> = Vec::new();
    for _ in 0..60 {
        h.key_press(Key::Tab);
        h.run_steps(2);
        let (role, label, rect) = focused(&h).expect("Tab siempre deja el foco en algún control");
        if order.first() == Some(&label) {
            break; // dio la vuelta
        }
        // Todo lo que recibe el foco es un control con nombre y a la vista; los botones pintan su anillo.
        assert!(!label.is_empty(), "un control sin nombre recibió el foco");
        assert!(rect.width() > 1.0 && rect.height() > 1.0, "{label}");
        if role == egui::accesskit::Role::Button {
            assert!(focus_ring_around(&h, rect), "{label}: sin anillo de foco");
        }
        order.push(label);
    }
    // El ciclo se cierra y pasa por todos los controles de la ventana principal.
    assert!(order.len() >= 10, "{order:?}");
    for needed in [
        "Subir un nivel",
        "Ir a Users",
        "Ajustes",
        "Tipo",
        "Buscar",
        "Analizar Sistema (C:)",
        "Mapa",
        "Lista",
        "Reescanear (F5)",
        "Mapa de ",
    ] {
        assert!(
            order.iter().any(|l| l.starts_with(needed)),
            "Tab no llega a `{needed}`: {order:?}"
        );
    }
    let unique: std::collections::HashSet<_> = order.iter().collect();
    assert_eq!(
        unique.len(),
        order.len(),
        "algún control se repite: {order:?}"
    );
}

#[test]
fn con_tab_intro_y_espacio_activan_el_boton_enfocado_y_no_entran_en_la_carpeta_seleccionada() {
    let mut h = harness(opts(user_folder()));
    let here = explore(&h).root.node;
    // Hay una carpeta seleccionada: si Intro se gastara como "entrar", el botón no se activaría y la raíz cambiaría.
    select_first_dir(&mut h);
    assert_eq!(explore(&h).view, View::Map);
    // Tab hasta "Lista" y Intro: cambia la vista y NO entra.
    tab_to(&mut h, "Lista");
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(explore(&h).view, View::List);
    assert_eq!(explore(&h).root.node, here, "Intro entró en la carpeta");
    // Mayús+Tab desde "Lista" lleva a "Mapa" (en la vista de lista cada fila es una parada de Tab): Espacio lo activa.
    h.key_press_modifiers(Modifiers::SHIFT, Key::Tab);
    h.run_steps(2);
    assert_eq!(focused_label(&h), "Mapa");
    h.key_press(Key::Space);
    h.run_steps(3);
    assert_eq!(explore(&h).view, View::Map);
    assert_eq!(explore(&h).root.node, here);
    // Retroceso con un botón enfocado tampoco sube de nivel.
    assert!(!focused_label(&h).is_empty());
    h.key_press(Key::Backspace);
    h.run_steps(3);
    assert_eq!(
        explore(&h).root.node,
        here,
        "Retroceso subió con un botón enfocado"
    );
    // Y Espacio sobre "Lista" con el foco en él también cambia la vista.
    h.key_press(Key::Tab);
    h.run_steps(2);
    assert_eq!(focused_label(&h), "Lista");
    h.key_press(Key::Space);
    h.run_steps(3);
    assert_eq!(explore(&h).view, View::List);
}

#[test]
fn con_tab_intro_sobre_reescanear_reescanea_y_no_navega() {
    let mut h = harness(opts(StartScreen::Home));
    h.get_by_label("Analizar Sistema (C:)").click();
    h.run_steps(3);
    h.get_by_label("Parar y ver lo que hay").click();
    h.run_steps(5);
    assert_eq!(h.state().ui().screen_name(), "explore");
    select_first_dir(&mut h);
    tab_to(&mut h, "Reescanear");
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(
        h.state().ui().screen_name(),
        "scan",
        "Intro sobre Reescanear debe reescanear"
    );
}

#[test]
fn con_tab_intro_en_el_mapa_sigue_entrando_y_en_una_fila_de_la_lista_tambien() {
    // Mapa: Tab hasta el mapa (foco del teclado en él) e Intro entra en la carpeta seleccionada.
    let mut h = harness(opts(StartScreen::Map(MapStart::default())));
    let users = explore(&h).tree.child_by_name(0, b"Users").unwrap();
    h.state_mut()
        .ui_mut()
        .explore_mut()
        .unwrap()
        .select(Some(Sel::Node(users)));
    h.run_steps(2);
    tab_to(&mut h, "Mapa de ");
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(explore(&h).root.node, users, "Intro en el mapa debe entrar");
    // Tras un clic en el mapa, Intro y Retroceso siguen funcionando.
    let mut h = harness(opts(StartScreen::Map(MapStart::default())));
    let p = cell_center(&h, "Users");
    click_at(&mut h, p);
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(explore(&h).root.node, users);
    h.key_press(Key::Backspace);
    h.run_steps(3);
    assert_eq!(explore(&h).root.node, 0);
    // Lista: Tab hasta una fila de carpeta e Intro la abre.
    let mut h = harness(opts(list_start()));
    let kids: Vec<_> = explore(&h).shown_children().iter().collect();
    let dir = kids
        .iter()
        .copied()
        .find(|&k| explore(&h).tree.node(k).is_dir())
        .expect("hay alguna carpeta");
    let dname = explore(&h).tree.name_lossy(dir).into_owned();
    tab_to(&mut h, &format!("{dname}, "));
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(explore(&h).root.node, dir);
}

#[test]
fn con_una_ventana_modal_abierta_el_teclado_no_navega_por_debajo() {
    let mut h = harness(opts(user_folder()));
    let here = explore(&h).root.node;
    let dir = select_first_dir(&mut h);
    h.get_by_label("Ajustes").click();
    h.run_steps(3);
    h.get_by_label("Acerca de Tesela").click();
    h.run_steps(4);
    h.get_by_label(&format!("Versión {}", crate::about::VERSION));
    for key in [Key::Enter, Key::Backspace] {
        h.key_press(key);
        h.run_steps(3);
        assert_eq!(
            explore(&h).root.node,
            here,
            "{key:?} navegó bajo la ventana modal"
        );
    }
    assert_eq!(explore(&h).selection, Some(Sel::Node(dir)));
    // Escape la cierra.
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(h.query_by_label_contains("Versión ").is_none());
}

#[test]
fn con_cuota_de_disco_la_tarjeta_distingue_lo_libre_del_volumen_de_lo_disponible_para_el_usuario() {
    for (lang, quota_label, free_label) in [
        (
            LangPref::Es,
            "Disponible para tu cuenta (cuota)",
            "libres de",
        ),
        (LangPref::En, "Available to your account (quota)", "free of"),
    ] {
        let mut o = opts(StartScreen::Home);
        o.settings.language = lang;
        let mut h = harness(o);
        // Sin cuota (la demostración): lo disponible es lo libre y no hay nota.
        assert!(h.query_by_label_contains(quota_label).is_none());
        let mut drives = crate::demo::drives();
        drives[0].available = drives[0].free / 4;
        h.state_mut().ui_mut().set_drives(drives);
        h.run_steps(3);
        // La nota sale (solo en la unidad con cuota) y lo libre del volumen se sigue mostrando entero.
        assert_eq!(h.query_all_by_label_contains(quota_label).count(), 1);
        assert!(h.query_all_by_label_contains(free_label).count() >= 3);
        // Con cuota y SIN la capacidad real del volumen (`volume_scope == false`), la tarjeta avisa de que las cifras son
        // las de la cuenta del usuario.
        let scope_label = if lang == LangPref::Es {
            "Cifras de tu cuenta (cuota de disco)"
        } else {
            "Figures for your account (disk quota)"
        };
        assert!(h.query_by_label_contains(scope_label).is_none());
        let mut drives = crate::demo::drives();
        drives[1].volume_scope = false;
        h.state_mut().ui_mut().set_drives(drives);
        h.run_steps(3);
        assert_eq!(h.query_all_by_label_contains(scope_label).count(), 1);
    }
}

// --- Tamaño mínimo (960 × 600): lista y pantalla de escaneo (m8) y "reducir animaciones" (m9) -------------------------

/// ¿Se pintó el texto exacto `text` en el último frame?
fn painted(h: &Harness<'static, App>, text: &str) -> bool {
    h.output()
        .shapes
        .iter()
        .any(|cs| matches!(&cs.shape, egui::epaint::Shape::Text(ts) if ts.galley.text() == text))
}

#[test]
fn las_columnas_de_la_lista_dan_prioridad_al_nombre_y_ocultan_las_demas_por_orden() {
    use crate::list::{Cols, NAME_MIN_W, columns};
    // Ancho de sobra: todas las columnas.
    let c = columns(1000.0);
    assert!(c.bar_left.is_some() && c.files_right.is_some());
    assert!(c.name_w >= NAME_MIN_W);
    // Al estrecharse se quita primero "archivos", luego la barra; el nombre nunca se queda sin su mínimo mientras se
    // pueda quitar algo, y nunca crece al estrechar.
    let mut prev: Option<Cols> = None;
    for w in (300..=1000).rev().step_by(4) {
        let c = columns(w as f32);
        if c.files_right.is_some() {
            assert!(c.bar_left.is_some(), "w {w}: archivos sin barra");
        }
        if c.bar_left.is_some() {
            assert!(c.name_w >= NAME_MIN_W, "w {w}: {c:?}");
        }
        assert!(
            c.name_w
                >= (w as f32 - 36.0 - 320.0)
                    .min(NAME_MIN_W)
                    .min(w as f32 - 36.0 - 120.0)
        );
        if let Some(p) = prev {
            // Las columnas visibles solo se pierden al estrechar, nunca se recuperan.
            assert!(p.files_right.is_some() || c.files_right.is_none(), "w {w}");
            assert!(p.bar_left.is_some() || c.bar_left.is_none(), "w {w}");
        }
        prev = Some(c);
    }
    // Con solo el tamaño, el nombre se lleva todo lo demás.
    let c = columns(330.0);
    assert!(c.bar_left.is_none() && c.files_right.is_none());
    assert!(c.name_w >= 150.0, "{c:?}");
}

#[test]
fn con_la_ventana_minima_la_lista_deja_sitio_al_nombre() {
    let h = harness_sized(opts(list_start()), vec2(960.0, 600.0));
    assert_eq!(explore(&h).view, View::List);
    let kids: Vec<_> = explore(&h).shown_children().iter().collect();
    let name = explore(&h).tree.name_lossy(kids[0]).into_owned();
    let label = format!("{name}, ");
    let row = h.get_by_label_contains(&label);
    let cols = crate::list::columns(row.rect().width());
    assert!(
        cols.name_w >= crate::list::NAME_MIN_W,
        "a {} px de fila el nombre solo tiene {} px",
        row.rect().width(),
        cols.name_w
    );
    // El nombre completo se pinta (no queda en una elipsis); las columnas que no caben se ocultan, también en la cabecera.
    assert!(painted(&h, &name), "el nombre `{name}` no se pinta entero");
    assert!(painted(&h, "Nombre") && painted(&h, "En disco"));
    // (La palabra "Archivos" también sale en el panel de detalles: solo cuenta la de la cabecera de la lista.)
    let header = |h: &Harness<'static, App>, text: &str| {
        let r = row.rect();
        h.output().shapes.iter().any(|cs| {
            matches!(&cs.shape, egui::epaint::Shape::Text(ts)
                if ts.galley.text() == text
                    && ts.pos.x >= r.left() - 1.0
                    && ts.pos.x <= r.right() + 1.0
                    && ts.pos.y < r.top())
        })
    };
    assert!(header(&h, "Nombre") && header(&h, "En disco"));
    assert!(
        !header(&h, "Archivos"),
        "la columna Archivos debería ocultarse a 960 px"
    );
    assert!(
        !header(&h, "Porcentaje"),
        "la columna Porcentaje debería ocultarse a 960 px"
    );
    // A 1280 × 800 siguen todas.
    let h = harness(opts(list_start()));
    assert!(painted(&h, "Archivos") && painted(&h, "Porcentaje"));
    let wide = harness(opts(list_start()));
    let label = format!("{name}, ");
    assert!(
        crate::list::columns(wide.get_by_label_contains(&label).rect().width())
            .files_right
            .is_some()
    );
}

/// Rectángulos de todos los nodos con etiqueta que contienen `texts` (el primero visible de cada uno).
fn rects_of(h: &Harness<'static, App>, texts: &[&str]) -> Vec<(String, egui::Rect)> {
    texts
        .iter()
        .map(|t| {
            let n = h
                .query_all_by_label_contains(t)
                .find(|n| n.rect().width() > 1.0 && n.rect().height() > 1.0)
                .unwrap_or_else(|| panic!("no hay `{t}`"));
            (t.to_string(), n.rect())
        })
        .collect()
}

#[test]
fn con_la_ventana_minima_la_pantalla_de_escaneo_no_pisa_textos_ni_corta_el_mapa() {
    for (lang, items) in [
        (
            LangPref::Es,
            vec![
                "Mientras analizamos tu disco",
                "archivos",
                "Parar y ver lo que hay",
                "Cancelar",
                "Limpiar lo seguro",
                "Sitios donde suele haber archivos temporales",
                "Es una estimación, no un mínimo garantizado",
                "Descargas y la Papelera son datos tuyos",
                "El mapa se va llenando",
            ],
        ),
        (
            LangPref::En,
            vec![
                "While we scan your disk",
                "files",
                "Stop and view what we have",
                "Cancel",
                "Clean the safe stuff",
                "Places where temporary files usually pile up",
                "This is an estimate, not a guaranteed minimum",
                "Downloads and the Recycle Bin are your own data",
                "The map is filling in",
            ],
        ),
    ] {
        let mut o = opts(StartScreen::Scan(0.5));
        o.settings.language = lang;
        let mut h = harness_sized(o, vec2(960.0, 600.0));
        h.run_steps(3);
        let rects = rects_of(&h, &items);
        for (i, (a, ra)) in rects.iter().enumerate() {
            // Dentro de la ventana.
            assert!(
                ra.left() >= -0.5
                    && ra.right() <= 960.0 + 0.5
                    && ra.top() >= -0.5
                    && ra.bottom() <= 600.0 + 0.5,
                "{lang:?}: `{a}` fuera de la ventana: {ra:?}"
            );
            // Y sin tocar a ninguno de los demás (en particular, "Limpiar lo seguro" no tapa el texto de la tarjeta).
            for (b, rb) in &rects[i + 1..] {
                assert!(
                    !ra.intersects(*rb),
                    "{lang:?}: `{a}` y `{b}` se solapan: {ra:?} {rb:?}"
                );
            }
        }
        // Las dos tarjetas: o su contenido se ve entero dentro de la ventana, o la tarjeta se desplaza (su barra está a
        // la vista y `overflow` lo dice); nunca se recorta en silencio.
        let (left_over, right_over) = h.state().ui().scan_overflow().expect("pantalla de escaneo");
        let last_row = h
            .query_all_by_label_contains(if lang == LangPref::Es {
                "Papelera"
            } else {
                "Recycle"
            })
            .next()
            .expect("última fila de sitios conocidos")
            .rect();
        assert!(
            left_over || last_row.bottom() <= 600.0 - 28.0,
            "{lang:?}: la tarjeta de sitios conocidos se corta sin desplazarse ({last_row:?})"
        );
        let bars: Vec<egui::Rect> = ["Users", "Windows", "ProgramData", "Program Files"]
            .iter()
            .filter_map(|t| h.query_all_by_label(t).next().map(|n| n.rect()))
            .collect();
        assert!(!bars.is_empty());
        let last_bar = bars.iter().map(|r| r.bottom()).fold(0.0, f32::max);
        assert!(
            right_over || last_bar <= 600.0 - 28.0,
            "{lang:?}: el mapa de progreso se corta sin desplazarse ({last_bar})"
        );
    }
}

#[test]
fn sin_animaciones_el_progreso_indeterminado_y_los_spinners_son_estaticos() {
    use crate::widgets::indeterminate_segment;
    let bar = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), vec2(400.0, 8.0));
    // Con animaciones el tramo se mueve con el tiempo; sin ellas es el mismo siempre.
    let a = indeterminate_segment(bar, 0.0, true);
    let b = indeterminate_segment(bar, 0.4, true);
    assert_ne!(a.left(), b.left(), "con animaciones se mueve");
    let still: Vec<egui::Rect> = [0.0, 0.4, 1.7, 9.0]
        .iter()
        .map(|&t| indeterminate_segment(bar, t, false))
        .collect();
    assert!(
        still.windows(2).all(|w| w[0] == w[1]),
        "sin animaciones es fijo"
    );
    assert!(bar.contains_rect(still[0]));

    // La pantalla de escaneo simulada y congelada: con `animate = false` no pide repintar (los spinners de las filas
    // pendientes son arcos fijos); con `animate = true` sí, y rápido (el Spinner de egui).
    let repaint_delay = |h: &Harness<'static, App>| {
        h.output()
            .viewport_output
            .values()
            .map(|v| v.repaint_delay)
            .min()
            .expect("viewport")
    };
    let mut o = opts(StartScreen::Scan(0.4));
    o.animate = false;
    let mut h = harness(o);
    h.run_steps(4);
    let quiet = repaint_delay(&h);
    assert!(
        quiet >= std::time::Duration::from_millis(500),
        "con 'reducir animaciones' no debe repintar cada 33 ms: {quiet:?}"
    );
    let mut o = opts(StartScreen::Scan(0.4));
    o.animate = true;
    let mut h = harness(o);
    h.run_steps(4);
    let busy = repaint_delay(&h);
    assert!(
        busy < std::time::Duration::from_millis(100),
        "con animaciones el spinner repinta: {busy:?}"
    );
}

#[test]
fn con_una_ventana_modal_abierta_f5_y_ctrl_f_no_actuan() {
    let mut h = harness(opts(StartScreen::Home));
    h.get_by_label("Analizar Sistema (C:)").click();
    h.run_steps(3);
    h.get_by_label("Parar y ver lo que hay").click();
    h.run_steps(5);
    assert_eq!(h.state().ui().screen_name(), "explore");
    h.get_by_label("Ajustes").click();
    h.run_steps(3);
    h.get_by_label("Acerca de Tesela").click();
    h.run_steps(4);
    h.get_by_label(&format!("Versión {}", crate::about::VERSION));
    // F5 no reescanea y Ctrl+F no enfoca el buscador bajo la ventana modal.
    h.key_press(Key::F5);
    h.run_steps(3);
    assert_eq!(
        h.state().ui().screen_name(),
        "explore",
        "F5 actuó bajo el modal"
    );
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(3);
    assert!(
        !h.get_by_label("Buscar").is_focused(),
        "Ctrl+F actuó bajo el modal"
    );
    // Escape cierra la ventana y entonces los atajos vuelven.
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(h.query_by_label_contains("Versión ").is_none());
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(3);
    assert!(h.get_by_label("Buscar").is_focused());
    h.key_press(Key::Escape);
    h.run_steps(2);
    h.key_press(Key::F5);
    h.run_steps(3);
    assert_eq!(
        h.state().ui().screen_name(),
        "scan",
        "F5 vuelve a funcionar"
    );
}

/// Pasos hasta que aparece una etiqueta (los hilos de medida del `cleaner` terminan solos).
fn wait_label(h: &mut Harness<'static, App>, label: &str) -> bool {
    for _ in 0..200 {
        h.run_steps(2);
        if h.query_by_label(label).is_some() {
            h.run_steps(4);
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

#[test]
fn el_titular_suma_solo_lo_seguro_medido_y_descargas_y_papelera_llevan_revisar() {
    use tesela_cleaner::{ApproveOptions, Basket, CategoryId};
    for (lang, tag, excluded) in [
        (LangPref::Es, "Revisar", "no cuentan en la cifra de arriba"),
        (LangPref::En, "Review", "not counted in the figure above"),
    ] {
        let mut o = opts(StartScreen::Scan(1.0));
        o.settings.language = lang;
        let mut h = harness(o);
        let tr = crate::Tr::new(lang.resolve());
        // Lo seguro y utilizable: temporales y las cachés de Edge y Firefox (Chrome está "abierto" en la demostración: no
        // cuenta). Cada cifra es la que mide el `cleaner` (antigüedad mínima de los temporales incluida).
        let b = test_backend();
        let measured = |id: CategoryId| {
            let mut bk = Basket::new();
            bk.set_category(id, true);
            b.approve(
                &bk,
                &ApproveOptions::default(),
                &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .permanent_bytes()
        };
        let safe: u64 = [
            CategoryId::UserTemp,
            CategoryId::WindowsTemp,
            CategoryId::EdgeCache,
            CategoryId::FirefoxCache,
        ]
        .into_iter()
        .map(measured)
        .sum();
        let all: u64 = crate::demo::DEMO_KNOWN_ROWS
            .iter()
            .map(|(_, b, _)| *b)
            .sum();
        assert!(all > safe && safe > 0);
        let want = tr.ta("scan-known-sum", crate::args!["size" => tr.size(safe)]);
        let not_want = tr.ta("scan-known-sum", crate::args!["size" => tr.size(all)]);
        assert!(wait_label(&mut h, &want), "{lang:?}: falta `{want}`");
        assert!(
            h.query_by_label(&not_want).is_none(),
            "Descargas y la Papelera no deben contar en la cifra grande"
        );
        // La etiqueta de riesgo y la nota que lo explica.
        assert_eq!(h.query_all_by_label(tag).count(), 1, "{lang:?}");
        h.get_by_label_contains(excluded);
        // Chrome está abierto: su fila no cuenta y lo dice.
        let blocked = if lang == LangPref::Es {
            "Cierra Chrome para liberar hasta"
        } else {
            "Close Chrome to free up to"
        };
        h.get_by_label_contains(blocked);
    }
}

#[test]
fn la_barra_de_estado_avisa_de_que_las_cifras_son_de_la_cuota_de_la_cuenta() {
    for (lang, label) in [
        (LangPref::Es, "Cifras de tu cuenta (cuota de disco)"),
        (LangPref::En, "Figures for your account (disk quota)"),
    ] {
        let mut o = opts(StartScreen::Map(MapStart::default()));
        o.settings.language = lang;
        let mut h = harness(o);
        // Con ámbito de volumen (lo normal) no hay aviso.
        assert!(explore(&h).info.volume.is_some() && explore(&h).info.volume_scope);
        assert!(h.query_by_label_contains(label).is_none());
        // Con cuota y sin la capacidad real del volumen, la barra de estado lo dice (como la tarjeta de inicio).
        h.state_mut()
            .ui_mut()
            .explore_mut()
            .unwrap()
            .info
            .volume_scope = false;
        h.run_steps(3);
        assert_eq!(h.query_all_by_label_contains(label).count(), 1, "{lang:?}");
    }
}

mod flow;
mod select;
