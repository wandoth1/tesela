//! Testing multiple selection (checking multiple items and adding them together to basket) with SIMULATED support of the
//! demonstration: synthetic tree, nothing touches the disc. The names are those of the demo (Videos, Downloads, Projects,
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

/// A child of the current folder, by name.
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

/// Click a row in the list (by the beginning of its label) with the given modifiers.
fn click_row(h: &mut Harness<'static, App>, name: &str, mods: Modifiers) {
    h.get_by_label_contains(&format!("{name}, "))
        .click_modifiers(mods);
    h.run_steps(3);
}

/// Primary click on a position with modifiers held during the click.
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

/// Center of a level 0 (or "Other") cell of `layout`, on screen.
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
    // Without anything marked the bar explains how to mark.
    h.get_by_label_contains("Ctrl+clic marca varios");
    click_row(&mut h, "Vídeos", Modifiers::COMMAND);
    click_row(&mut h, "Descargas", Modifiers::COMMAND);
    assert_eq!(marked(&h), sorted(vec![v, d]));
    // Ctrl+click also moves the active element (the details show this), like in the Explorer.
    assert_eq!(explore(&h).selection, Some(Sel::Node(d)));
    h.get_by_label_contains("2 seleccionados · ");
    // The marked row says so with its accessible status (`toggled`) and does not repeat "selected" in the label; the element
    // active (the last one with Ctrl+click) is still displayed as `selected` even if the rows are boxes.
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
    // Ctrl+click again unchecks it (the double-click time passes: it is not "enter").
    h.run_steps(30);
    click_row(&mut h, "Vídeos", Modifiers::COMMAND);
    assert_eq!(marked(&h), vec![d]);
    assert_eq!(
        explore(&h).root.node,
        here,
        "los Ctrl+clic no entraron en ninguna carpeta"
    );
    // A normal click only changes the active element: the marked remains as it was.
    click_row(&mut h, "Proyectos", Modifiers::NONE);
    assert_eq!(explore(&h).selection, Some(Sel::Node(p)));
    assert_eq!(marked(&h), vec![d]);
}

#[test]
fn mayus_clic_marca_un_rango_y_mayus_flechas_lo_extienden() {
    let mut h = harness(opts(list_start()));
    let kids: Vec<NodeId> = explore(&h).shown_children().iter().collect();
    let name = |h: &Harness<'static, App>, id| explore(h).tree.name_lossy(id).into_owned();
    // A normal click sets the anchor; Shift+click marks from it to the row pressed, both included.
    let n1 = name(&h, kids[1]);
    click_row(&mut h, &n1, Modifiers::NONE);
    assert!(explore(&h).multi.is_empty(), "el clic normal no marca");
    let n4 = name(&h, kids[4]);
    click_row(&mut h, &n4, Modifiers::SHIFT);
    assert_eq!(marked(&h), sorted(kids[1..=4].to_vec()));
    // Shift+down arrow from the last row expands the range (and moves the active element and focus).
    h.get_by_label_contains(&format!("{n4}, ")).focus();
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    h.run_steps(3);
    assert_eq!(marked(&h), sorted(kids[1..=5].to_vec()));
    assert_eq!(explore(&h).selection, Some(Sel::Node(kids[5])));
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    h.run_steps(3);
    assert_eq!(marked(&h), sorted(kids[1..=6].to_vec()));
    // An arrow without Shift moves without marking anything else.
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
    // Ctrl+A marks all the children of the folder (also those that go under "Others").
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(3);
    assert_eq!(marked(&h), sorted(all.clone()));
    h.get_by_label_contains(&format!("{} seleccionados", all.len()));
    h.get_by_label_contains(&format!("Seleccionados para la cesta: {}", all.len()));
    // "Everything but one": Ctrl+click on a map cell removes it.
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
    // With the list and context menu, "Invert selection" leaves only the missing one checked.
    h.get_by_label("Lista").click();
    h.run_steps(3);
    h.get_by_label_contains("Proyectos, ").click_secondary();
    h.run_steps(3);
    // The header box and the menu option have different names.
    h.get_by_label("Seleccionar todas las filas");
    h.get_by_label("Seleccionar todo aquí");
    h.get_by_label("Invertir selección").click();
    h.run_steps(4);
    assert_eq!(marked(&h), vec![p]);
    // Esc first empties the marked and keeps the active element; the second Esc empties the active element.
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
    // Ctrl+click on the "Other" cell marks all its elements; uncheck them again.
    let c = rect_of_cell(&h, idx);
    click_at_mods(&mut h, c.center(), Modifiers::COMMAND);
    assert_eq!(marked(&h).len(), o.count as usize);
    assert!(explore(&h).others_all_marked(o));
    h.run_steps(30);
    click_at_mods(&mut h, c.center(), Modifiers::COMMAND);
    assert!(explore(&h).multi.is_empty());
    // With everything checked, "Other" is also checked; If one of its elements is removed, it ceases to exist.
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
    // Marking something inside marks the folder of this view: what is marked are always siblings, with no nesting possible.
    explore_mut(&mut h).marks_toggle(Sel::Node(web));
    assert_eq!(marked(&h), vec![p]);
    explore_mut(&mut h).marks_toggle(Sel::Node(p));
    assert!(explore(&h).multi.is_empty());
    // What is not hanging from this folder is not marked.
    let users = explore(&h).tree.child_by_name(0, b"Users").unwrap();
    explore_mut(&mut h).marks_toggle(Sel::Node(users));
    assert!(explore(&h).multi.is_empty());
    // The size of the bar is the sum of each sibling only once.
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
    // The bar says it: count, size of what would be added and why the rest does not enter.
    h.get_by_label_contains(&format!(
        "3 seleccionados · se añadirían {}",
        es().size(s.bytes)
    ));
    h.get_by_label_contains("1 no se puede añadir: 1 protegido");
    h.get_by_label_contains("1 carpeta conocida: se añade su contenido");
    // Add: two entries in the basket, in the same way as the icon (Videos as "Content of").
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
    // Remark what is already in the basket: it is excluded, it is explained and there is nothing to add.
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
    // "Select" mode: a normal click check (and the switch is an accessible box).
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
    // Enter the marked empty folder.
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(explore(&h).root.node, p);
    assert!(explore(&h).multi.is_empty(), "entrar vacía lo marcado");
    // And it warns you: you don't miss a selection in silence.
    h.get_by_label_contains("Se quitó la selección (1)");
    assert!(explore(&h).multi.mode, "el modo no se pierde");
    // Raise with Backspace too.
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(3);
    assert!(!explore(&h).multi.is_empty());
    h.key_press(Key::Backspace);
    h.run_steps(3);
    assert_eq!(explore(&h).root.node, user);
    assert!(explore(&h).multi.is_empty(), "subir vacía lo marcado");
    // And jump for the crumbs.
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
    // The box at the header checks everything you see...
    h.get_by_label("Seleccionar todas las filas").click();
    h.run_steps(3);
    assert_eq!(marked(&h).len(), n);
    // ...and in selection mode a normal click on a row removes it: "everything but one", just with the mouse.
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
    // With any unchecked, the header box checks all; with all of them, he removes them.
    h.get_by_label("Seleccionar todas las filas").click();
    h.run_steps(3);
    assert_eq!(marked(&h).len(), n);
    h.get_by_label("Seleccionar todas las filas").click();
    h.run_steps(3);
    assert!(explore(&h).multi.is_empty());
}

/// Is there a veil and a "selected" badge above cell `cell` (on screen)?
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
        // Ctrl+click on the map marks the cell and makes it active.
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
        // A hinted cell (level 1) of Downloads marks Downloads, which is the one with the label.
        let di = layout.cell_of_node(d).unwrap();
        let area = |c: &tesela_layout::Cell| c.rect.w * c.rect.h;
        let hint = (0..layout.cells().len())
            .filter(|&i| layout.cells()[i].level == 1 && layout.cells()[i].parent == di as u32)
            .max_by(|&a, &b| area(&layout.cells()[a]).total_cmp(&area(&layout.cells()[b])))
            .expect("Descargas tiene celdas insinuadas");
        let hc = rect_of_cell(&h, hint);
        click_at_mods(&mut h, hc.center(), Modifiers::COMMAND);
        assert_eq!(marked(&h), sorted(vec![v, d]), "dark={dark}");
        // Shift+click map toggles just like Ctrl+click.
        h.run_steps(30);
        click_at_mods(&mut h, hc.center(), Modifiers::SHIFT);
        assert_eq!(marked(&h), vec![v], "dark={dark}");
        // Space checks or unchecks the active element (the keyboard path); a normal click just changes it.
        let pp = cell_center(&h, "Proyectos");
        click_at(&mut h, pp);
        assert_eq!(explore(&h).selection, Some(Sel::Node(p)));
        assert_eq!(marked(&h), vec![v], "el clic normal no toca lo marcado");
        h.key_press(Key::Space);
        h.run_steps(3);
        assert_eq!(marked(&h), sorted(vec![v, p]), "dark={dark}");
        // The screen reader receives the count and whether the active item is checked.
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
    // Tab cycles through the bar in reading order: switch, Add, Uncheck all.
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
    // The switch is also reached with Tab and Space activates it.
    tab_to(&mut h, "Seleccionar");
    h.key_press(Key::Space);
    h.run_steps(3);
    assert!(explore(&h).multi.mode);
}

/// Rectangle (on screen) of the first label that contains `text` and is inside the selection bar band.
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
        // The bar band: just above the basket bar (56 px, with its label centered).
        let basket = h
            .get_by_label_contains(if es { "Cesta ·" } else { "Basket ·" })
            .rect();
        let bottom = basket.center().y - 28.0;
        let band = (bottom - 44.0, bottom);
        // The bar lives in the center column: between the sidebar (232) and the details (300).
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
        // Without anything marked: the switch and the track.
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
        // With three markings (one protected and one known folder): count, notice and the two buttons.
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
        // The list with the visible boxes leaves room for the name even if the window is minimal.
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
        marks: vec![],
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
    // The list only shows Downloads: the rest is no longer checked, and the bar counts it that way.
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
    // With some marking the rows are boxes; the default click of a screen reader toggles the row.
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
    // Enter with the focused row does not alternate (and does not enter a file either).
    h.get_by_label_contains("distro-linux-24.04.iso, ").focus();
    h.run_steps(3);
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(marked(&h), vec![b], "Intro no alterna la marca");
    assert_eq!(explore(&h).root.node, here);
    // Space toggles ONE time even if held down (auto-repeat does not count).
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
    // Without going through Tab: Space marks, Shift+down arrow expands and Shift+up arrow shrinks.
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
    // An arrow without Shift moves the active element and leaves the marked as is.
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
    // The basket icon in the details panel is also disabled, and the single path to basket does not add.
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
    // By API, with Ctrl+click on the map (the cell is still there, hidden) and with Space on the active element.
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
    // The double-click time is allowed to pass: two clicks in a row on the same cell would be "enter".
    h.run_steps(30);
    let p = cell_center(&h, "Vídeos");
    click_at(&mut h, p);
    h.key_press(Key::Space);
    h.run_steps(3);
    assert!(explore(&h).multi.is_empty(), "Espacio sobre un no acierto");
    // The menu does not offer it for a wrong choice, but it does offer it for a right choice.
    assert!(!explore_mut(&mut h).can_toggle_mark(v));
    assert!(explore_mut(&mut h).can_toggle_mark(d));
    // A success is marked and unmarked.
    explore_mut(&mut h).marks_toggle(Sel::Node(d));
    assert_eq!(marked(&h), vec![d]);
    explore_mut(&mut h).marks_toggle(Sel::Node(d));
    assert!(explore(&h).multi.is_empty());
}

#[test]
fn la_pista_cortada_lleva_tooltip_y_el_interruptor_enseña_su_texto_en_cuanto_cabe() {
    // At 1280 x 800 it fits: the switch carries your text. At 960 x 600 it's just the box (accessible name follows).
    let h = harness(opts(user_folder()));
    assert!(painted(&h, "Seleccionar"));
    let mut h = harness_sized(opts(user_folder()), vec2(960.0, 600.0));
    assert!(!painted(&h, "Seleccionar"));
    h.get_by_label("Seleccionar");
    // The track (cut to that width) carries its full text and new keys on the floating track.
    h.get_by_label_contains("Ctrl+clic marca varios").hover();
    h.run_steps(14);
    assert!(
        h.query_all_by_label_contains("Espacio marca la fila")
            .count()
            >= 1
    );
}

/// Scale: 5,000 children. Mark all and add goes in batches, full basket does not recalculate its estimate on each frame
/// and the "View" window only builds the rows that are seen. (With the linear path from before it was seconds and tens
/// milliseconds per frame with 40,000.)
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
    // With the basket full and frames in a row, the estimate is calculated once (not once per frame).
    h.run_steps(2);
    let before = explore(&h).estimate_computes;
    h.run_steps(10);
    assert_eq!(
        explore(&h).estimate_computes,
        before,
        "estimación recalculada"
    );
    // Mark everything again: everything is already in the basket, and is explained without re-sorting in each frame.
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(3);
    let s = explore_mut(&mut h).marks_summary();
    assert_eq!((s.in_basket, s.add.len()), (5000, 0));
    // The "View" window does not build 5,000 rows.
    explore_mut(&mut h).basket_open = true;
    h.run_steps(3);
    let rows = h.query_all_by_label_contains("Quitar carpeta-").count();
    assert!((1..80).contains(&rows), "{rows} filas construidas");
    // The locator index is retained while the window is open and released when the window is closed.
    assert!(explore(&h).locator_cache.is_some());
    explore_mut(&mut h).basket_open = false;
    h.run_steps(2);
    assert!(explore(&h).locator_cache.is_none());
}
