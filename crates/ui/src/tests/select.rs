//! Pruebas de la selección múltiple (marcar varios elementos y añadirlos juntos a la cesta) con el respaldo SIMULADO de la
//! demostración: árbol sintético, nada toca el disco. Los nombres son los de la demostración (Vídeos, Descargas, Proyectos,
//! AppData...).

use super::*;
use crate::app::CleanStart;
use crate::explore::AddState;
use crate::theme::Palette;
use egui::accesskit::Toggled;
use tesela_model::NodeId;

fn explore_mut<'a>(h: &'a mut Harness<'static, App>) -> &'a mut crate::explore::Explore {
    h.state_mut().ui_mut().explore_mut().expect("exploración")
}

/// Un hijo de la carpeta actual, por nombre.
fn kid(h: &Harness<'static, App>, name: &str) -> NodeId {
    let ex = explore(h);
    ex.tree
        .child_by_name(ex.root.node, name.as_bytes())
        .unwrap_or_else(|| panic!("no hay `{name}` en la carpeta actual"))
}

fn sorted(mut v: Vec<NodeId>) -> Vec<NodeId> {
    v.sort_unstable();
    v
}

fn marked(h: &Harness<'static, App>) -> Vec<NodeId> {
    explore(h).multi.sorted()
}

fn mark_api(h: &mut Harness<'static, App>, names: &[&str]) {
    let ids: Vec<NodeId> = names.iter().map(|n| kid(h, n)).collect();
    for id in ids {
        explore_mut(h).marks_toggle(Sel::Node(id));
    }
    h.run_steps(3);
}

/// Pulsa una fila de la lista (por el principio de su etiqueta) con los modificadores dados.
fn click_row(h: &mut Harness<'static, App>, name: &str, mods: Modifiers) {
    h.get_by_label_contains(&format!("{name}, "))
        .click_modifiers(mods);
    h.run_steps(3);
}

/// Clic primario en una posición con los modificadores sostenidos durante el clic.
fn click_at_mods(h: &mut Harness<'static, App>, pos: Pos2, mods: Modifiers) {
    h.event(Event::PointerMoved(pos));
    h.run_steps(1);
    h.event(Event::ModifiersChanged(mods));
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: mods,
        });
        h.run_steps(1);
    }
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(3);
}

/// Centro de una celda de nivel 0 (o de "Otros") de `layout`, en pantalla.
fn rect_of_cell(h: &Harness<'static, App>, idx: usize) -> egui::Rect {
    let layout = explore(h).map.layout.clone().expect("layout");
    let map = h.get_by_label_contains("Mapa de").rect();
    let r = layout.cells()[idx].rect;
    egui::Rect::from_min_size(Pos2::new(map.min.x + r.x, map.min.y + r.y), vec2(r.w, r.h))
}

fn es() -> crate::Tr {
    crate::Tr::new(crate::Lang::Es)
}

#[test]
fn ctrl_clic_alterna_filas_y_un_clic_normal_no_toca_lo_marcado() {
    let mut h = harness(opts(list_start()));
    let here = explore(&h).root.node;
    let (v, d, p) = (
        kid(&h, "Vídeos"),
        kid(&h, "Descargas"),
        kid(&h, "Proyectos"),
    );
    assert!(explore(&h).multi.is_empty());
    // Sin nada marcado la barra explica cómo marcar.
    h.get_by_label_contains("Ctrl+clic marca varios");
    click_row(&mut h, "Vídeos", Modifiers::COMMAND);
    click_row(&mut h, "Descargas", Modifiers::COMMAND);
    assert_eq!(marked(&h), sorted(vec![v, d]));
    // Ctrl+clic también mueve el elemento activo (los detalles lo enseñan), como en el Explorador.
    assert_eq!(explore(&h).selection, Some(Sel::Node(d)));
    h.get_by_label_contains("2 seleccionados · ");
    // La fila marcada lo dice con su estado accesible (`toggled`) y no repite "seleccionado" en la etiqueta; el elemento
    // activo (el último con Ctrl+clic) se sigue exponiendo como `selected` aunque las filas sean casillas.
    let row = h.get_by_label_contains("Vídeos, ");
    assert_eq!(row.accesskit_node().toggled(), Some(Toggled::True));
    assert!(
        !row.accesskit_node()
            .label()
            .unwrap()
            .contains("seleccionado")
    );
    assert_eq!(row.accesskit_node().is_selected(), Some(false));
    assert_eq!(
        h.get_by_label_contains("Descargas, ")
            .accesskit_node()
            .is_selected(),
        Some(true)
    );
    // Ctrl+clic otra vez la desmarca (se deja pasar el tiempo de doble clic: no es "entrar").
    h.run_steps(30);
    click_row(&mut h, "Vídeos", Modifiers::COMMAND);
    assert_eq!(marked(&h), vec![d]);
    assert_eq!(
        explore(&h).root.node,
        here,
        "los Ctrl+clic no entraron en ninguna carpeta"
    );
    // Un clic normal solo cambia el elemento activo: lo marcado se queda como estaba.
    click_row(&mut h, "Proyectos", Modifiers::NONE);
    assert_eq!(explore(&h).selection, Some(Sel::Node(p)));
    assert_eq!(marked(&h), vec![d]);
}

#[test]
fn mayus_clic_marca_un_rango_y_mayus_flechas_lo_extienden() {
    let mut h = harness(opts(list_start()));
    let kids: Vec<NodeId> = explore(&h).shown_children().iter().collect();
    let name = |h: &Harness<'static, App>, id| explore(h).tree.name_lossy(id).into_owned();
    // Un clic normal fija el ancla; Mayús+clic marca desde ella hasta la fila pulsada, ambas incluidas.
    let n1 = name(&h, kids[1]);
    click_row(&mut h, &n1, Modifiers::NONE);
    assert!(explore(&h).multi.is_empty(), "el clic normal no marca");
    let n4 = name(&h, kids[4]);
    click_row(&mut h, &n4, Modifiers::SHIFT);
    assert_eq!(marked(&h), sorted(kids[1..=4].to_vec()));
    // Mayús+flecha abajo desde la última fila amplía el rango (y mueve el elemento activo y el foco).
    h.get_by_label_contains(&format!("{n4}, ")).focus();
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    h.run_steps(3);
    assert_eq!(marked(&h), sorted(kids[1..=5].to_vec()));
    assert_eq!(explore(&h).selection, Some(Sel::Node(kids[5])));
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    h.run_steps(3);
    assert_eq!(marked(&h), sorted(kids[1..=6].to_vec()));
    // Una flecha sin Mayús mueve sin marcar nada más.
    h.key_press(Key::ArrowDown);
    h.run_steps(3);
    assert_eq!(marked(&h), sorted(kids[1..=6].to_vec()));
    assert_eq!(explore(&h).selection, Some(Sel::Node(kids[7])));
}

#[test]
fn ctrl_a_marca_todo_ctrl_clic_quita_uno_e_invertir_deja_solo_ese_y_esc_vacia() {
    let mut h = harness(opts(user_folder()));
    let all: Vec<NodeId> = explore(&h).shown_children().iter().collect();
    assert!(all.len() > 40, "{} hijos", all.len());
    let p = kid(&h, "Proyectos");
    // Ctrl+A marca todos los hijos de la carpeta (también los que van dentro de "Otros").
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(3);
    assert_eq!(marked(&h), sorted(all.clone()));
    h.get_by_label_contains(&format!("{} seleccionados", all.len()));
    h.get_by_label_contains(&format!("Seleccionados para la cesta: {}", all.len()));
    // "Todo menos uno": Ctrl+clic sobre una celda del mapa la quita.
    let idx = explore(&h)
        .map
        .layout
        .as_ref()
        .unwrap()
        .cell_of_node(p)
        .unwrap();
    let c = rect_of_cell(&h, idx);
    click_at_mods(
        &mut h,
        Pos2::new(c.center().x, c.min.y + 6.0),
        Modifiers::COMMAND,
    );
    assert_eq!(marked(&h).len(), all.len() - 1);
    assert!(!explore(&h).multi.contains(p));
    // Con la lista y el menú contextual, "Invertir selección" deja marcada solo la que faltaba.
    h.get_by_label("Lista").click();
    h.run_steps(3);
    h.get_by_label_contains("Proyectos, ").click_secondary();
    h.run_steps(3);
    // La casilla de cabecera y la opción del menú tienen nombres distintos.
    h.get_by_label("Seleccionar todas las filas");
    h.get_by_label("Seleccionar todo aquí");
    h.get_by_label("Invertir selección").click();
    h.run_steps(4);
    assert_eq!(marked(&h), vec![p]);
    // Esc vacía primero lo marcado y se queda con el elemento activo; el segundo Esc vacía el elemento activo.
    assert!(explore(&h).selection.is_some());
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(explore(&h).multi.is_empty());
    assert!(explore(&h).selection.is_some());
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert_eq!(explore(&h).selection, None);
}

#[test]
fn otros_se_marca_entero_con_ctrl_a_y_con_ctrl_clic_en_su_celda() {
    let mut h = harness(opts(user_folder()));
    let layout = explore(&h).map.layout.clone().unwrap();
    let o = layout.others()[0];
    let idx = layout
        .cells()
        .iter()
        .position(|c| matches!(c.kind, CellKind::Others(_)))
        .unwrap();
    // Ctrl+clic sobre la celda "Otros" marca todos sus elementos; otra vez los desmarca.
    let c = rect_of_cell(&h, idx);
    click_at_mods(&mut h, c.center(), Modifiers::COMMAND);
    assert_eq!(marked(&h).len(), o.count as usize);
    assert!(explore(&h).others_all_marked(o));
    h.run_steps(30);
    click_at_mods(&mut h, c.center(), Modifiers::COMMAND);
    assert!(explore(&h).multi.is_empty());
    // Con todo marcado, "Otros" también lo está; si se quita uno de sus elementos deja de estarlo.
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(3);
    assert!(explore(&h).others_all_marked(o));
    let member = explore(&h).others_members(o)[0];
    explore_mut(&mut h).marks_toggle(Sel::Node(member));
    assert!(!explore(&h).others_all_marked(o));
}

#[test]
fn un_descendiente_se_marca_por_su_carpeta_y_no_se_suma_dos_veces() {
    let mut h = harness(opts(user_folder()));
    let p = kid(&h, "Proyectos");
    let web = explore(&h).tree.child_by_name(p, b"tienda-web").unwrap();
    // Marcar algo de dentro marca la carpeta de esta vista: lo marcado son siempre hermanos, sin anidación posible.
    explore_mut(&mut h).marks_toggle(Sel::Node(web));
    assert_eq!(marked(&h), vec![p]);
    explore_mut(&mut h).marks_toggle(Sel::Node(p));
    assert!(explore(&h).multi.is_empty());
    // Lo que no cuelga de esta carpeta no se marca.
    let users = explore(&h).tree.child_by_name(0, b"Users").unwrap();
    explore_mut(&mut h).marks_toggle(Sel::Node(users));
    assert!(explore(&h).multi.is_empty());
    // El tamaño de la barra es la suma de cada hermano una sola vez.
    explore_mut(&mut h).marks_toggle(Sel::Node(web));
    explore_mut(&mut h).marks_toggle(Sel::Node(p));
    explore_mut(&mut h).marks_toggle(Sel::Node(web));
    assert_eq!(marked(&h), vec![p]);
    let s = explore(&h).compute_summary();
    assert_eq!(s.bytes, explore(&h).view().alloc(p));
}

#[test]
fn la_barra_cuenta_los_bytes_explica_lo_protegido_y_anade_solo_lo_que_se_puede() {
    let mut h = harness(opts(list_start()));
    let (p, v, a) = (kid(&h, "Proyectos"), kid(&h, "Vídeos"), kid(&h, "AppData"));
    assert_eq!(explore(&h).flow.basket.len(), 0);
    for n in ["Proyectos", "Vídeos", "AppData"] {
        click_row(&mut h, n, Modifiers::COMMAND);
    }
    let s = explore(&h).compute_summary();
    assert_eq!(s.marked, 3);
    assert_eq!(s.add, sorted(vec![p, v]));
    assert_eq!(
        s.contents(),
        1,
        "Vídeos es una carpeta conocida: su contenido"
    );
    assert_eq!(s.protected, 1, "AppData es intocable");
    let alloc = |id| explore(&h).view().alloc(id);
    assert_eq!(s.bytes, alloc(p) + alloc(v), "sin AppData y sin duplicados");
    // La barra lo dice: recuento, tamaño de lo que se añadiría y por qué no entra el resto.
    h.get_by_label_contains(&format!(
        "3 seleccionados · se añadirían {}",
        es().size(s.bytes)
    ));
    h.get_by_label_contains("1 no se puede añadir: 1 protegido");
    h.get_by_label_contains("1 carpeta conocida: se añade su contenido");
    // Añadir: dos entradas en la cesta, por el mismo camino que el icono (Vídeos como "Contenido de").
    h.get_by_label("Añadir 2 a la cesta").click();
    h.run_steps(4);
    {
        let ex = explore(&h);
        assert!(ex.multi.is_empty(), "añadir vacía la selección");
        assert_eq!(ex.flow.basket.len(), 2);
        assert!(ex.basket_has_own(Sel::Node(p)) && ex.basket_has_own(Sel::Node(v)));
        assert!(
            !ex.basket_has_own(Sel::Node(a))
                && !ex.flow.basket.covers(&ex.node_key(a))
                && ex.add_state(Sel::Node(a)) == AddState::Untouchable,
            "la selección no se salta ninguna protección"
        );
    }
    h.get_by_label_contains("2 elementos añadidos a la cesta · 1 no se podía añadir");
    h.get_by_label_contains("Cesta · 2 elementos");
    // Volver a marcar lo que ya está en la cesta: se excluye, se explica y no hay nada que añadir.
    click_row(&mut h, "Proyectos", Modifiers::COMMAND);
    h.get_by_label_contains("1 no se puede añadir: 1 ya en la cesta");
    assert!(
        h.get_by_label("Añadir selección a la cesta")
            .accesskit_node()
            .is_disabled()
    );
    h.get_by_label("Desmarcar todo").click();
    h.run_steps(3);
    assert!(explore(&h).multi.is_empty());
    assert_eq!(explore(&h).flow.basket.len(), 2);
}

#[test]
fn navegar_vacia_lo_marcado_pero_el_modo_seleccionar_se_conserva() {
    let mut h = harness(opts(list_start()));
    let user = explore(&h).root.node;
    let p = kid(&h, "Proyectos");
    // El modo "Seleccionar": un clic normal marca (y el interruptor es una casilla accesible).
    assert_eq!(
        h.get_by_label("Seleccionar").accesskit_node().toggled(),
        Some(Toggled::False)
    );
    h.get_by_label("Seleccionar").click();
    h.run_steps(3);
    assert!(explore(&h).multi.mode);
    assert_eq!(
        h.get_by_label("Seleccionar").accesskit_node().toggled(),
        Some(Toggled::True)
    );
    click_row(&mut h, "Proyectos", Modifiers::NONE);
    assert_eq!(marked(&h), vec![p]);
    // Entrar en la carpeta vacía lo marcado.
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(explore(&h).root.node, p);
    assert!(explore(&h).multi.is_empty(), "entrar vacía lo marcado");
    // Y lo avisa: no se pierde una selección en silencio.
    h.get_by_label_contains("Se quitó la selección (1)");
    assert!(explore(&h).multi.mode, "el modo no se pierde");
    // Subir con Retroceso también.
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(3);
    assert!(!explore(&h).multi.is_empty());
    h.key_press(Key::Backspace);
    h.run_steps(3);
    assert_eq!(explore(&h).root.node, user);
    assert!(explore(&h).multi.is_empty(), "subir vacía lo marcado");
    // Y saltar por las migas.
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(3);
    assert!(!explore(&h).multi.is_empty());
    h.get_by_label("Ir a Users").click();
    h.run_steps(3);
    assert!(explore(&h).multi.is_empty(), "las migas vacían lo marcado");
}

#[test]
fn el_modo_seleccionar_da_casillas_y_la_de_la_cabecera_marca_todo_y_quita_todo() {
    let mut h = harness(opts(list_start()));
    let n = explore(&h).shown_children().len();
    let p = kid(&h, "Proyectos");
    h.get_by_label("Seleccionar").click();
    h.run_steps(3);
    // La casilla de la cabecera marca todo lo que se ve...
    h.get_by_label("Seleccionar todas las filas").click();
    h.run_steps(3);
    assert_eq!(marked(&h).len(), n);
    // ...y en modo selección un clic normal en una fila la quita: "todo menos una", solo con el ratón.
    click_row(&mut h, "Proyectos", Modifiers::NONE);
    assert_eq!(marked(&h).len(), n - 1);
    assert!(!explore(&h).multi.contains(p));
    assert_eq!(
        h.get_by_label_contains("Vídeos, ")
            .accesskit_node()
            .toggled(),
        Some(Toggled::True)
    );
    assert_eq!(
        h.get_by_label_contains("Proyectos, ")
            .accesskit_node()
            .toggled(),
        Some(Toggled::False)
    );
    // Con alguna sin marcar, la casilla de la cabecera marca todas; con todas, las quita.
    h.get_by_label("Seleccionar todas las filas").click();
    h.run_steps(3);
    assert_eq!(marked(&h).len(), n);
    h.get_by_label("Seleccionar todas las filas").click();
    h.run_steps(3);
    assert!(explore(&h).multi.is_empty());
}

/// ¿Hay un velo y una insignia de "seleccionado" sobre la celda `cell` (en pantalla)?
fn mark_shapes(h: &Harness<'static, App>, cell: egui::Rect, pal: &Palette) -> (bool, bool) {
    let r = cell.shrink(1.0);
    let tint = crate::multisel::mark_tint(pal);
    let badge =
        r.right_bottom() - vec2(crate::map::CHECK_BADGE_INSET, crate::map::CHECK_BADGE_INSET);
    let shapes = &h.output().shapes;
    let tinted = shapes.iter().any(|cs| {
        matches!(&cs.shape, egui::epaint::Shape::Rect(rs)
            if rs.fill == tint && rs.rect.min.distance(r.min) < 0.5 && rs.rect.max.distance(r.max) < 0.5)
    });
    let badged = shapes.iter().any(|cs| {
        matches!(&cs.shape, egui::epaint::Shape::Circle(c)
            if c.fill == pal.accent && c.center.distance(badge) < 1.0)
    });
    (tinted, badged)
}

#[test]
fn en_el_mapa_ctrl_clic_marca_con_velo_e_insignia_en_los_dos_temas_y_espacio_alterna_el_activo() {
    for dark in [false, true] {
        let mut o = opts(user_folder());
        o.settings.theme = if dark {
            ThemePref::Dark
        } else {
            ThemePref::Light
        };
        let mut h = harness(o);
        let pal = Palette::of(dark);
        let (v, d, p) = (
            kid(&h, "Vídeos"),
            kid(&h, "Descargas"),
            kid(&h, "Proyectos"),
        );
        let layout = explore(&h).map.layout.clone().unwrap();
        // Ctrl+clic en el mapa marca la celda y la hace la activa.
        let vi = layout.cell_of_node(v).unwrap();
        let vc = rect_of_cell(&h, vi);
        assert_eq!(
            mark_shapes(&h, vc, &pal),
            (false, false),
            "antes: sin marca"
        );
        click_at_mods(
            &mut h,
            Pos2::new(vc.center().x, vc.min.y + 6.0),
            Modifiers::COMMAND,
        );
        assert_eq!(marked(&h), vec![v], "dark={dark}");
        assert_eq!(explore(&h).selection, Some(Sel::Node(v)));
        assert_eq!(
            mark_shapes(&h, vc, &pal),
            (true, true),
            "dark={dark}: velo e insignia sobre la celda marcada"
        );
        // Una celda insinuada (nivel 1) de Descargas marca a Descargas, que es la que lleva etiqueta.
        let di = layout.cell_of_node(d).unwrap();
        let area = |c: &tesela_layout::Cell| c.rect.w * c.rect.h;
        let hint = (0..layout.cells().len())
            .filter(|&i| layout.cells()[i].level == 1 && layout.cells()[i].parent == di as u32)
            .max_by(|&a, &b| area(&layout.cells()[a]).total_cmp(&area(&layout.cells()[b])))
            .expect("Descargas tiene celdas insinuadas");
        let hc = rect_of_cell(&h, hint);
        click_at_mods(&mut h, hc.center(), Modifiers::COMMAND);
        assert_eq!(marked(&h), sorted(vec![v, d]), "dark={dark}");
        // Mayús+clic en el mapa alterna igual que Ctrl+clic.
        h.run_steps(30);
        click_at_mods(&mut h, hc.center(), Modifiers::SHIFT);
        assert_eq!(marked(&h), vec![v], "dark={dark}");
        // Espacio marca o desmarca el elemento activo (la vía de teclado); un clic normal solo lo cambia.
        let pp = cell_center(&h, "Proyectos");
        click_at(&mut h, pp);
        assert_eq!(explore(&h).selection, Some(Sel::Node(p)));
        assert_eq!(marked(&h), vec![v], "el clic normal no toca lo marcado");
        h.key_press(Key::Space);
        h.run_steps(3);
        assert_eq!(marked(&h), sorted(vec![v, p]), "dark={dark}");
        // El lector de pantalla recibe el recuento y si el elemento activo está marcado.
        h.get_by_label_contains("Seleccionados para la cesta: 2");
        h.get_by_label_contains("Este elemento está seleccionado para la cesta");
        h.key_press(Key::Space);
        h.run_steps(3);
        assert_eq!(marked(&h), vec![v], "dark={dark}");
    }
}

#[test]
fn con_el_teclado_se_llega_a_los_botones_de_la_barra_y_se_usan() {
    let mut h = harness(opts(user_folder()));
    mark_api(&mut h, &["Vídeos", "Proyectos"]);
    tab_to(&mut h, "Añadir 2 a la cesta");
    h.key_press(Key::Enter);
    h.run_steps(4);
    assert_eq!(explore(&h).flow.basket.len(), 2);
    assert!(explore(&h).multi.is_empty());
    assert_eq!(
        focused_label(&h),
        "Seleccionar",
        "tras Añadir con el teclado el foco pasa al interruptor"
    );
    mark_api(&mut h, &["Descargas"]);
    // Tab recorre la barra en orden de lectura: interruptor, Añadir, Desmarcar todo.
    tab_to(&mut h, "Seleccionar");
    h.key_press(Key::Tab);
    h.run_steps(2);
    assert_eq!(focused_label(&h), "Añadir 1 a la cesta");
    h.key_press(Key::Tab);
    h.run_steps(2);
    assert_eq!(focused_label(&h), "Desmarcar todo");
    h.key_press(Key::Space);
    h.run_steps(3);
    assert!(explore(&h).multi.is_empty());
    assert_eq!(
        focused_label(&h),
        "Seleccionar",
        "tras Desmarcar con el teclado el foco pasa al interruptor"
    );
    // El interruptor también se alcanza con Tab y Espacio lo activa.
    tab_to(&mut h, "Seleccionar");
    h.key_press(Key::Space);
    h.run_steps(3);
    assert!(explore(&h).multi.mode);
}

/// Rectángulo (en pantalla) de la primera etiqueta que contiene `text` y está dentro de la banda de la barra de selección.
fn in_bar(h: &Harness<'static, App>, text: &str, band: (f32, f32)) -> (String, egui::Rect) {
    let n = h
        .query_all_by_label_contains(text)
        .find(|n| n.rect().top() >= band.0 - 1.0 && n.rect().bottom() <= band.1 + 1.0)
        .unwrap_or_else(|| panic!("no hay `{text}` dentro de la barra de selección {band:?}"));
    (text.to_string(), n.rect())
}

#[test]
fn la_barra_de_seleccion_cabe_sin_solaparse_a_960x600_y_1280x800_en_los_dos_idiomas() {
    for (lang, size) in [
        (LangPref::Es, vec2(1600.0, 900.0)),
        (LangPref::Es, vec2(1280.0, 800.0)),
        (LangPref::Es, vec2(960.0, 600.0)),
        (LangPref::En, vec2(1280.0, 800.0)),
        (LangPref::En, vec2(960.0, 600.0)),
    ] {
        let es = lang == LangPref::Es;
        let mut o = opts(user_folder());
        o.settings.language = lang;
        let mut h = harness_sized(o, size);
        // La banda de la barra: justo encima de la barra de la cesta (56 px, con su etiqueta centrada).
        let basket = h
            .get_by_label_contains(if es { "Cesta ·" } else { "Basket ·" })
            .rect();
        let bottom = basket.center().y - 28.0;
        let band = (bottom - 44.0, bottom);
        // La barra vive en la columna central: entre la barra lateral (232) y los detalles (300).
        let inside = |label: &str, r: egui::Rect| {
            assert!(
                r.left() >= 232.0 - 0.5 && r.right() <= size.x - 300.0 + 0.5,
                "{lang:?} {size:?}: `{label}` fuera de la columna central: {r:?}"
            );
        };
        let disjoint = |items: &[(String, egui::Rect)]| {
            for (i, (a, ra)) in items.iter().enumerate() {
                inside(a, *ra);
                for (b, rb) in &items[i + 1..] {
                    assert!(
                        !ra.intersects(*rb),
                        "{lang:?} {size:?}: `{a}` y `{b}` se solapan: {ra:?} {rb:?}"
                    );
                }
            }
        };
        // Sin nada marcado: el interruptor y la pista.
        let toggle = if es { "Seleccionar" } else { "Select" };
        let hint = if es {
            "Ctrl+clic marca varios"
        } else {
            "Ctrl+click selects several"
        };
        let t = h.get_by_label(toggle).rect();
        assert!(
            t.top() >= band.0 - 1.0 && t.bottom() <= band.1 + 1.0,
            "{t:?}"
        );
        disjoint(&[(toggle.to_string(), t), in_bar(&h, hint, band)]);
        // Con tres marcados (uno protegido y una carpeta conocida): recuento, aviso y los dos botones.
        mark_api(&mut h, &["Vídeos", "Proyectos", "AppData"]);
        h.run_steps(3);
        let labels: [&str; 3] = if es {
            [
                "3 seleccionados",
                "1 no se puede añadir",
                "Añadir 2 a la cesta",
            ]
        } else {
            ["3 selected", "1 cannot be added", "Add 2 to the basket"]
        };
        let mut items = vec![(toggle.to_string(), h.get_by_label(toggle).rect())];
        for l in labels {
            items.push(in_bar(&h, l, band));
        }
        let clear = if es {
            "Desmarcar todo"
        } else {
            "Clear selection"
        };
        items.push((clear.to_string(), h.get_by_label(clear).rect()));
        disjoint(&items);
        // La lista con las casillas visibles deja sitio al nombre aunque la ventana sea la mínima.
        explore_mut(&mut h).view = View::List;
        h.run_steps(4);
        let kids: Vec<NodeId> = explore(&h).shown_children().iter().collect();
        let name = explore(&h).tree.name_lossy(kids[0]).into_owned();
        assert!(
            painted(&h, &name),
            "{lang:?} {size:?}: `{name}` no se pinta entero"
        );
    }
}

#[test]
fn una_limpieza_retira_lo_marcado_que_ya_no_existe_y_conserva_el_resto() {
    let mut h = harness(opts(user_folder()));
    let (v, p) = (kid(&h, "Vídeos"), kid(&h, "Proyectos"));
    mark_api(&mut h, &["Vídeos", "Proyectos"]);
    let path = explore(&h).tree.path(p);
    explore_mut(&mut h).apply_removed(&[tesela_cleaner::Removed {
        path,
        is_dir: true,
        bytes: 0,
        logical: 0,
        files: 0,
        removed: true,
    }]);
    h.run_steps(3);
    assert_eq!(
        marked(&h),
        vec![v],
        "Proyectos ya no existe; Vídeos sigue marcado"
    );
}

fn descargas_start() -> StartScreen {
    StartScreen::Map(MapStart {
        list: true,
        open: vec!["Users".into(), crate::demo::USER.into(), "Descargas".into()],
        select: None,
    })
}

#[test]
fn la_busqueda_deja_solo_las_marcas_que_siguen_visibles_y_anadir_no_anade_lo_oculto() {
    let mut h = harness(opts(list_start()));
    let d = kid(&h, "Descargas");
    for n in ["Vídeos", "Descargas", "Proyectos"] {
        click_row(&mut h, n, Modifiers::COMMAND);
    }
    assert_eq!(marked(&h).len(), 3);
    h.get_by_label("Buscar").focus();
    h.run_steps(2);
    h.get_by_label("Buscar").type_text("desc");
    h.run_steps(4);
    // La lista solo enseña Descargas: lo demás deja de estar marcado, y la barra lo cuenta así.
    assert_eq!(marked(&h), vec![d]);
    h.get_by_label_contains("1 seleccionado · ");
    h.get_by_label("Añadir 1 a la cesta").click();
    h.run_steps(4);
    let ex = explore(&h);
    assert_eq!(ex.flow.basket.len(), 1, "no se añadió nada oculto");
    assert!(ex.basket_has_own(Sel::Node(d)));
}

#[test]
fn el_clic_de_accesibilidad_alterna_intro_no_y_espacio_alterna_una_sola_vez() {
    let mut h = harness(opts(descargas_start()));
    let here = explore(&h).root.node;
    let (a, b) = (
        kid(&h, "distro-linux-24.04.iso"),
        kid(&h, "instalador-sistema.iso"),
    );
    // Con alguna marca las filas son casillas; el clic por defecto de un lector de pantalla alterna la fila.
    click_row(&mut h, "instalador-sistema.iso", Modifiers::COMMAND);
    assert_eq!(marked(&h), vec![b]);
    h.get_by_label_contains("distro-linux-24.04.iso, ")
        .click_accesskit();
    h.run_steps(3);
    assert_eq!(marked(&h), sorted(vec![a, b]));
    h.get_by_label_contains("distro-linux-24.04.iso, ")
        .click_accesskit();
    h.run_steps(3);
    assert_eq!(marked(&h), vec![b]);
    // Intro con la fila enfocada no alterna (y en un archivo tampoco entra).
    h.get_by_label_contains("distro-linux-24.04.iso, ").focus();
    h.run_steps(3);
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(marked(&h), vec![b], "Intro no alterna la marca");
    assert_eq!(explore(&h).root.node, here);
    // Espacio alterna UNA vez aunque se mantenga pulsado (la autorrepetición no cuenta).
    for _ in 0..3 {
        h.key_down(Key::Space);
        h.run_steps(1);
    }
    h.key_up(Key::Space);
    h.run_steps(2);
    assert_eq!(marked(&h), sorted(vec![a, b]), "una pulsación, una marca");
}

#[test]
fn un_clic_enfoca_la_fila_y_con_el_teclado_se_marca_y_mayus_flechas_amplian_y_encogen() {
    let mut h = harness(opts(list_start()));
    let kids: Vec<NodeId> = explore(&h).shown_children().iter().collect();
    let name = |h: &Harness<'static, App>, id| explore(h).tree.name_lossy(id).into_owned();
    let n1 = name(&h, kids[1]);
    click_row(&mut h, &n1, Modifiers::NONE);
    assert!(
        h.get_by_label_contains(&format!("{n1}, ")).is_focused(),
        "un clic de ratón enfoca la fila"
    );
    // Sin pasar por Tab: Espacio marca, Mayús+flecha abajo amplía y Mayús+flecha arriba encoge.
    h.key_press(Key::Space);
    h.run_steps(3);
    assert_eq!(marked(&h), vec![kids[1]]);
    for _ in 0..2 {
        h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
        h.run_steps(3);
    }
    assert_eq!(marked(&h), sorted(kids[1..=3].to_vec()));
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowUp);
    h.run_steps(3);
    assert_eq!(marked(&h), sorted(kids[1..=2].to_vec()), "el rango encoge");
    // Una flecha sin Mayús mueve el elemento activo y deja lo marcado como está.
    h.key_press(Key::ArrowDown);
    h.run_steps(3);
    assert_eq!(explore(&h).selection, Some(Sel::Node(kids[3])));
    assert_eq!(marked(&h), sorted(kids[1..=2].to_vec()));
}

#[test]
fn con_una_limpieza_en_curso_anadir_esta_desactivado_y_no_hace_nada() {
    let mut h = harness(opts(StartScreen::Clean(CleanStart::Confirm)));
    assert!(explore(&h).flow.busy());
    let id = explore(&h)
        .tree
        .child_by_name(0, "Juegos".as_bytes())
        .unwrap();
    explore_mut(&mut h).marks_toggle(Sel::Node(id));
    h.run_steps(3);
    assert_eq!(explore(&h).compute_summary().add, vec![id]);
    assert!(
        h.get_by_label("Añadir 1 a la cesta")
            .accesskit_node()
            .is_disabled()
    );
    let before = explore(&h).flow.basket.len();
    assert_eq!(explore_mut(&mut h).marks_add_to_basket(&es(), 0.0), 0);
    assert_eq!(
        marked(&h),
        vec![id],
        "con la limpieza en curso no se toca nada"
    );
    assert_eq!(explore(&h).flow.basket.len(), before);
    // El icono de la cesta del panel de detalles también queda desactivado, y el camino único a la cesta no añade.
    explore_mut(&mut h).select(Some(Sel::Node(id)));
    h.run_steps(3);
    assert!(
        h.get_by_label("Añadir a la cesta")
            .accesskit_node()
            .is_disabled()
    );
    assert!(!explore_mut(&mut h).basket_add_one(Sel::Node(id)));
    assert_eq!(explore(&h).flow.basket.len(), before);
}

#[test]
fn con_una_busqueda_activa_no_se_marca_lo_que_no_es_acierto_pero_se_puede_desmarcar() {
    let mut h = harness(opts(user_folder()));
    let (v, d) = (kid(&h, "Vídeos"), kid(&h, "Descargas"));
    explore_mut(&mut h).search = "desc".into();
    h.run_steps(3);
    // Por API, con Ctrl+clic en el mapa (la celda sigue ahí, velada) y con Espacio sobre el elemento activo.
    explore_mut(&mut h).marks_toggle(Sel::Node(v));
    assert!(explore(&h).multi.is_empty());
    let vi = explore(&h)
        .map
        .layout
        .as_ref()
        .unwrap()
        .cell_of_node(v)
        .unwrap();
    let c = rect_of_cell(&h, vi);
    click_at_mods(
        &mut h,
        Pos2::new(c.center().x, c.min.y + 6.0),
        Modifiers::COMMAND,
    );
    assert!(explore(&h).multi.is_empty(), "Ctrl+clic en un no acierto");
    // Se deja pasar el tiempo de doble clic: dos clics seguidos en la misma celda serían "entrar".
    h.run_steps(30);
    let p = cell_center(&h, "Vídeos");
    click_at(&mut h, p);
    h.key_press(Key::Space);
    h.run_steps(3);
    assert!(explore(&h).multi.is_empty(), "Espacio sobre un no acierto");
    // El menú tampoco lo ofrece para un no acierto, y sí para un acierto.
    assert!(!explore_mut(&mut h).can_toggle_mark(v));
    assert!(explore_mut(&mut h).can_toggle_mark(d));
    // Un acierto se marca y se desmarca.
    explore_mut(&mut h).marks_toggle(Sel::Node(d));
    assert_eq!(marked(&h), vec![d]);
    explore_mut(&mut h).marks_toggle(Sel::Node(d));
    assert!(explore(&h).multi.is_empty());
}

#[test]
fn la_pista_cortada_lleva_tooltip_y_el_interruptor_enseña_su_texto_en_cuanto_cabe() {
    // A 1280 x 800 cabe: el interruptor lleva su texto. A 960 x 600 es solo la casilla (el nombre accesible sigue).
    let h = harness(opts(user_folder()));
    assert!(painted(&h, "Seleccionar"));
    let mut h = harness_sized(opts(user_folder()), vec2(960.0, 600.0));
    assert!(!painted(&h, "Seleccionar"));
    h.get_by_label("Seleccionar");
    // La pista (cortada a esa anchura) lleva su texto completo y las teclas nuevas en la pista flotante.
    h.get_by_label_contains("Ctrl+clic marca varios").hover();
    h.run_steps(14);
    assert!(
        h.query_all_by_label_contains("Espacio marca la fila")
            .count()
            >= 1
    );
}

/// Escala: 5 000 hijos. Marcar todo y añadir va por lotes, la cesta llena no recalcula su estimación en cada fotograma
/// y la ventana "Ver" solo construye las filas que se ven. (Con el recorrido lineal de antes eran segundos y decenas
/// de milisegundos por fotograma con 40 000.)
#[test]
fn con_miles_de_hijos_anadir_va_por_lotes_y_la_cesta_llena_no_recalcula_cada_fotograma() {
    let mut h = harness(opts(StartScreen::Map(MapStart::default())));
    let req = explore(&h).info.req.clone();
    *explore_mut(&mut h) = crate::explore::Explore::demo_from_tree(
        req,
        crate::demo::flat_tree(5000),
        false,
        false,
        test_backend(),
    );
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(3);
    assert_eq!(marked(&h).len(), 5000);
    h.get_by_label_contains("5.000 seleccionados");
    let t = std::time::Instant::now();
    let (added, total) = explore_mut(&mut h).marks_add_core();
    assert_eq!((added, total), (5000, 5000));
    assert!(
        t.elapsed() < std::time::Duration::from_secs(10),
        "añadir 5 000 tardó {:?}",
        t.elapsed()
    );
    assert_eq!(explore(&h).flow.basket.len(), 5000);
    assert!(explore(&h).multi.is_empty());
    // Con la cesta llena y fotogramas seguidos, la estimación se calcula una vez (no una por fotograma).
    h.run_steps(2);
    let before = explore(&h).estimate_computes;
    h.run_steps(10);
    assert_eq!(
        explore(&h).estimate_computes,
        before,
        "estimación recalculada"
    );
    // Marcar todo otra vez: todo está ya en la cesta, y se explica sin volver a clasificar en cada fotograma.
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(3);
    let s = explore_mut(&mut h).marks_summary();
    assert_eq!((s.in_basket, s.add.len()), (5000, 0));
    // La ventana "Ver" no construye 5 000 filas.
    explore_mut(&mut h).basket_open = true;
    h.run_steps(3);
    let rows = h.query_all_by_label_contains("Quitar carpeta-").count();
    assert!((1..80).contains(&rows), "{rows} filas construidas");
    // El índice del localizador se conserva mientras la ventana está abierta y se suelta al cerrarla.
    assert!(explore(&h).locator_cache.is_some());
    explore_mut(&mut h).basket_open = false;
    h.run_steps(2);
    assert!(explore(&h).locator_cache.is_none());
}
