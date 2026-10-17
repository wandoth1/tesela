//! Testing the interface cleanup flow (basket, Recommendations, confirmation, execution and summary) with the
//! SIMULATED demo mode backup: None of this touches the disk.

use super::*;
use crate::app::CleanStart;
use egui::accesskit::Toggled;
use tesela_cleaner::CategoryId;

fn clean_opts(start: CleanStart) -> Options {
    opts(StartScreen::Clean(start))
}

/// Steps until a tag disappears.
fn wait_gone(h: &mut Harness<'static, App>, label: &str) -> bool {
    for _ in 0..200 {
        h.run_steps(2);
        if h.query_by_label(label).is_none() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

/// Steps until a label CONTAINING `text` appears; then a few more steps: a modal window takes a few
/// frames to settle its position and a click in the middle of that movement fails the button.
fn wait_contains(h: &mut Harness<'static, App>, text: &str) -> bool {
    for _ in 0..200 {
        h.run_steps(2);
        if h.query_all_by_label_contains(text).count() >= 1 {
            h.run_steps(6);
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

fn basket_count(h: &Harness<'static, App>) -> usize {
    explore(h).flow.basket.len()
}

/// A-1: with the actual MINIMUM window (960x600) the Recommendations screen does not step on anything: the long headline ("Up to
/// low-risk candidates...") goes to several lines next to the main button without covering it, nor the subtitle, nor the text
/// (long) from hiberfil.sys. In Spanish and in English.
#[test]
fn con_la_ventana_minima_recomendaciones_no_pisa_titular_boton_ni_textos() {
    for (lang, items) in [
        (
            LangPref::Es,
            vec![
                "candidatos de bajo riesgo",
                "más que conviene que mires tú",
                "Limpiar lo seguro",
                "powercfg /h /type reduced",
            ],
        ),
        (
            LangPref::En,
            vec![
                "low-risk candidates",
                "you should look at yourself",
                "Clean the safe items",
                "powercfg /h /type reduced",
            ],
        ),
    ] {
        let mut o = clean_opts(CleanStart::Recommendations);
        o.settings.language = lang;
        let mut h = harness_sized(o, egui::vec2(960.0, 600.0));
        h.run_steps(4);
        let rects = rects_of(&h, &items);
        for (i, (a, ra)) in rects.iter().enumerate() {
            assert!(
                ra.left() >= -0.5 && ra.right() <= 960.5,
                "{lang:?}: `{a}` fuera de la ventana por los lados: {ra:?}"
            );
            for (b, rb) in &rects[i + 1..] {
                assert!(
                    !ra.intersects(*rb),
                    "{lang:?}: `{a}` y `{b}` se solapan: {ra:?} {rb:?}"
                );
            }
        }
    }
}

#[test]
fn recomendaciones_ensenan_el_titular_las_tarjetas_y_preseleccionan_lo_seguro() {
    let mut h = harness(clean_opts(CleanStart::Recommendations));
    h.run_steps(3);
    h.get_by_label_contains("candidatos de bajo riesgo");
    h.get_by_label_contains("más que conviene que mires tú");
    // The three kinds of risk with their phrase.
    h.get_by_label("Seguro");
    h.get_by_label_contains("Ya marcado");
    h.get_by_label("Revisar");
    assert!(h.query_all_by_label("Solo informar").count() >= 1);
    // The safe thing is checked (the first time): temporary and the caches of Edge and Firefox; Chrome is open.
    for name in [
        "Incluir Temporales del usuario",
        "Incluir Temporales de Windows",
        "Incluir Caché de navegadores",
    ] {
        let n = h.get_by_label(name);
        assert_eq!(n.accesskit_node().toggled(), Some(Toggled::True), "{name}");
    }
    h.get_by_label_contains("Chrome está abierto: ciérralo para liberar hasta");
    h.get_by_label("Volver a comprobar");
    {
        let b = &explore(&h).flow.basket;
        assert!(
            b.contains_category(CategoryId::UserTemp) && b.contains_category(CategoryId::EdgeCache)
        );
        assert!(
            !b.contains_category(CategoryId::ChromeCache),
            "una caché de un navegador abierto no se preselecciona"
        );
    }
    // Downloads and the Recycle Bin: Review, with its button; those that only inform, without a box or button.
    assert!(
        h.query_all_by_label("Ver").count() >= 2,
        "el botón de Descargas y el de la cesta"
    );
    h.get_by_label("Vaciar");
    h.get_by_label_contains("pagefile.sys");
    h.get_by_label_contains("hiberfil.sys");
    // The basket bar reflects what is marked.
    h.get_by_label_contains("Cesta · 4 elementos");
    // Unchecking a card removes it from the basket.
    h.get_by_label("Incluir Temporales del usuario").click();
    h.run_steps(3);
    assert!(
        !explore(&h)
            .flow
            .basket
            .contains_category(CategoryId::UserTemp)
    );
}

#[test]
fn limpiar_lo_seguro_abre_la_confirmacion_y_cancelar_o_escape_no_tocan_nada() {
    let mut h = harness(clean_opts(CleanStart::Recommendations));
    h.run_steps(3);
    let before_version = explore(&h).version;
    h.get_by_label_contains("Limpiar lo seguro").click();
    assert!(
        wait_contains(&mut h, "Revisa antes de limpiar"),
        "no se abrió la confirmación"
    );
    h.get_by_label_contains("Se borra y se vuelve a crear sola");
    h.get_by_label_contains("comprobamos cada ruta otra vez justo antes de borrar");
    // Focus starts on Cancel (pressing Enter alone never deletes).
    assert!(h.get_by_label("Cancelar").is_focused());
    // The main button says what is going to happen.
    h.get_by_label_contains("Liberar ");
    // Cancel closes without touching anything.
    h.get_by_label("Cancelar").click();
    assert!(wait_gone(&mut h, "Revisa antes de limpiar"));
    assert_eq!(explore(&h).version, before_version);
    assert!(explore(&h).overlay.is_none(), "cancelar no cambia el árbol");
    // Escape too.
    h.get_by_label_contains("Limpiar lo seguro").click();
    assert!(wait_contains(&mut h, "Revisa antes de limpiar"));
    h.key_press(Key::Escape);
    assert!(wait_gone(&mut h, "Revisa antes de limpiar"));
    assert!(explore(&h).overlay.is_none());
    assert!(basket_count(&h) >= 3, "la cesta se conserva");
}

#[test]
fn confirmar_ejecuta_enseña_el_resumen_y_el_arbol_encoge_sin_reescanear() {
    let mut h = harness(clean_opts(CleanStart::Recommendations));
    h.run_steps(3);
    let temp_node = {
        let ex = explore(&h);
        let (_, recs) = ex.recs.as_ref().unwrap();
        recs.row(CategoryId::UserTemp).unwrap().parts[0].node
    };
    let alloc_before = explore(&h).view().alloc(temp_node);
    let root_before = explore(&h).view().alloc(0);
    h.get_by_label_contains("Limpiar lo seguro").click();
    assert!(wait_contains(&mut h, "Revisa antes de limpiar"));
    h.get_by_label_contains("Liberar ").click();
    assert!(
        wait_contains(&mut h, "Cambio medido del espacio libre"),
        "no llegó el resumen"
    );
    // The three counters and what was skipped.
    h.get_by_label("Eliminado definitivamente");
    h.get_by_label("Movido a la Papelera");
    h.get_by_label_contains("archivos en uso");
    h.get_by_label_contains("Chrome sigue abierto");
    // No "Undo" (it's from v0.2) and without opening the actual Recycle Bin in the demo.
    assert!(
        h.query_by_label("Deshacer lo enviado a la Papelera")
            .is_none()
    );
    assert!(
        h.get_by_label("Abrir la Papelera")
            .accesskit_node()
            .is_disabled()
    );
    h.get_by_label("Volver al mapa").click();
    h.run_steps(4);
    // The tree was updated WITHOUT rescanning: the temp folder and root shrank; histograms are "approximate."
    let ex = explore(&h);
    assert!(ex.overlay.is_some() && ex.cleaned);
    assert!(ex.view().alloc(temp_node) < alloc_before);
    assert!(ex.view().alloc(0) < root_before);
    assert_eq!(
        ex.tree.node(temp_node).alloc,
        alloc_before,
        "el árbol base no se toca"
    );
    assert!(ex.flow.basket.is_empty(), "lo ejecutado sale de la cesta");
    h.get_by_label("≈ Aproximado hasta reescanear");
}

#[test]
fn anadir_desde_el_menu_quitar_y_la_cesta_se_agrupa_por_destino() {
    let mut h = harness(opts(user_folder()));
    h.get_by_label("Lista").click();
    h.run_steps(3);
    h.get_by_label_contains("Proyectos, ").click_secondary();
    h.run_steps(3);
    // Menu entry (there is another copy in the details panel: the menu is the one inside the popup).
    let items: Vec<_> = h.query_all_by_label("Añadir a la cesta").collect();
    assert!(items.len() >= 2);
    items.into_iter().next().unwrap().click();
    h.run_steps(3);
    assert_eq!(basket_count(&h), 1);
    h.get_by_label_contains("Cesta · 1 elemento");
    // He marks the detail and offers to remove it.
    h.get_by_label("En la cesta");
    h.get_by_label("Quitar de la cesta").click();
    h.run_steps(3);
    assert_eq!(basket_count(&h), 0);
    // The same from the details button, and "View" opens the list grouped by destination with its phrases.
    h.get_by_label_contains("Proyectos, ").click();
    h.run_steps(2);
    h.get_by_label("Añadir a la cesta").click();
    h.run_steps(3);
    h.get_by_label("Ver").click();
    h.run_steps(3);
    h.get_by_label("Cesta");
    assert!(
        h.query_all_by_label("Va a la Papelera de reciclaje · puedes recuperarlo")
            .count()
            >= 1
    );
    h.get_by_label_contains("Quitar Proyectos de la cesta")
        .click();
    h.run_steps(3);
    assert_eq!(basket_count(&h), 0);
}

#[test]
fn lo_protegido_no_se_anade_ver_contenido_ocupa_su_lugar_y_las_carpetas_conocidas_ofrecen_su_contenido()
 {
    let mut h = harness(opts(user_folder()));
    h.get_by_label("Lista").click();
    h.run_steps(3);
    // AppData is untouchable as a destination: it is locked, "Add" explains why, and "View content" takes its place.
    let row = h.get_by_label_contains("AppData, ");
    assert!(
        row.accesskit_node()
            .label()
            .is_some_and(|l| l.contains("Protegida")),
        "la fila accesible dice que está protegida"
    );
    row.click_secondary();
    h.run_steps(3);
    h.get_by_label("Ver contenido");
    assert!(h.query_by_label("Entrar").is_none());
    for n in h.query_all_by_label("Añadir a la cesta") {
        assert!(n.accesskit_node().is_disabled());
    }
    h.key_press(Key::Escape);
    h.run_steps(2);
    // A known folder (Videos): only "Contents of...", never the folder itself.
    h.get_by_label_contains("Vídeos, ").click_secondary();
    h.run_steps(3);
    h.get_by_label("Añadir el contenido de «Vídeos» a la cesta")
        .click();
    h.run_steps(3);
    assert_eq!(basket_count(&h), 1);
    {
        let b = &explore(&h).flow.basket;
        assert!(matches!(
            b.sources().next(),
            Some(tesela_cleaner::Source::ContentsOf(_))
        ));
    }
    // System (Windows) routes are denied completely.
    h.get_by_label("Ir a Users").click();
    h.run_steps(3);
}

#[test]
fn lo_que_no_cabe_en_la_papelera_sale_en_el_grupo_ambar_con_su_casilla() {
    let mut h = harness(opts(StartScreen::Map(MapStart::default())));
    h.get_by_label("Lista").click();
    h.run_steps(3);
    // `Juegos` (58 GB) exceeds the Dummy drive's Recycle Bin limit (the conservative 5% of the drive, with margin).
    h.get_by_label_contains("Juegos, ").click_secondary();
    h.run_steps(3);
    h.query_all_by_label("Añadir a la cesta")
        .next()
        .unwrap()
        .click();
    h.run_steps(3);
    h.get_by_label_contains("Limpiar ").click();
    assert!(wait_contains(&mut h, "Revisa antes de limpiar"));
    h.get_by_label("Se eliminará definitivamente porque no se puede enviar a la Papelera");
    h.get_by_label_contains("supera el tamaño de la Papelera");
    let check = h.get_by_label("Entiendo que esto no se podrá recuperar");
    assert_eq!(check.accesskit_node().toggled(), Some(Toggled::False));
    // Without the box there is nothing to free: the main button does not promise space.
    assert!(h.query_by_label_contains("Liberar ").is_none());
    h.get_by_label("Entiendo que esto no se podrá recuperar")
        .click();
    h.run_steps(3);
    h.get_by_label_contains("Liberar ");
}

#[test]
fn vaciar_ahora_del_resumen_vuelve_a_pedir_confirmacion_con_alcance_enumerado() {
    let mut h = harness(clean_opts(CleanStart::Summary));
    h.run_steps(3);
    h.get_by_label("Cambio medido del espacio libre");
    // T1: and it says that other programs can influence that figure.
    h.get_by_label_contains("otros programas que escriban o borren a la vez");
    h.get_by_label_contains("están en la Papelera de reciclaje");
    h.get_by_label_contains("Vaciar ahora").click();
    assert!(wait_contains(&mut h, "Revisa antes de limpiar"));
    h.get_by_label("Vaciar la Papelera");
    h.get_by_label_contains("Lo que entre después no se toca");
    // It is irreversible: the focus is on Cancel.
    assert!(h.get_by_label("Cancelar").is_focused());
}

#[test]
fn la_simulacion_no_cambia_el_arbol_y_lo_dice() {
    let mut o = clean_opts(CleanStart::Recommendations);
    o.dry_run = true;
    let mut h = harness(o);
    h.run_steps(3);
    assert!(h.query_all_by_label_contains("Modo simulación").count() >= 1);
    h.get_by_label_contains("Limpiar lo seguro").click();
    assert!(wait_contains(&mut h, "Revisa antes de limpiar"));
    h.get_by_label_contains("no se borrará ni se moverá nada");
    h.get_by_label("Simular la limpieza").click();
    assert!(wait_contains(
        &mut h,
        "Simulación: esto es lo que se habría eliminado (estimación)"
    ));
    h.get_by_label("Volver al mapa").click();
    h.run_steps(3);
    assert!(
        explore(&h).overlay.is_none(),
        "una simulación no cambia el árbol"
    );
    assert!(!explore(&h).cleaned);
}

/// H4: A category dropped by a link in its path (e.g. `User Data` from Edge moved to another unit with a junction)
/// It does not disappear silently: it appears as "Not available" with its reason, in Spanish and English.
#[test]
fn una_categoria_descartada_por_un_enlace_sale_como_no_disponible_con_su_motivo() {
    for en in [false, true] {
        let be = std::sync::Arc::new(crate::demo_backend::DemoBackend::new(
            std::time::Duration::ZERO,
        ));
        be.set_unavailable(
            CategoryId::EdgeCache,
            tesela_cleaner::roots::RootRejection::ReparseInChain {
                component: "User Data".into(),
            },
        );
        let mut o = clean_opts(CleanStart::Recommendations);
        if en {
            o.settings.language = LangPref::En;
        }
        o.backend = Some(crate::app::BackendRef(be.clone()));
        let mut h = harness(o);
        h.run_steps(3);
        let (title, why) = if en {
            ("Not available", "the folder is a link (junction)")
        } else {
            ("No disponible", "la carpeta es un enlace (junction)")
        };
        h.get_by_label(title);
        h.get_by_label_contains(why);
        // Chrome is still available; Edge no longer appears as a usable tile.
        assert!(
            !explore(&h)
                .flow
                .basket
                .contains_category(CategoryId::EdgeCache)
        );
    }
}

#[test]
fn un_navegador_abierto_puede_volver_a_comprobarse() {
    let be = std::sync::Arc::new(crate::demo_backend::DemoBackend::new(
        std::time::Duration::ZERO,
    ));
    let mut o = clean_opts(CleanStart::Recommendations);
    o.backend = Some(crate::app::BackendRef(be.clone()));
    let mut h = harness(o);
    h.run_steps(3);
    h.get_by_label_contains("Chrome está abierto");
    assert!(
        !explore(&h)
            .flow
            .basket
            .contains_category(CategoryId::ChromeCache)
    );
    // The user closes Chrome and presses "Check again": its cache becomes usable (and can be marked).
    be.set_browsers(Vec::new());
    // With the keyboard (the link can be under the basket bar in a lower window).
    h.get_by_label("Volver a comprobar").focus();
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(4);
    assert!(
        explore(&h).browsers_open.is_empty(),
        "sigue marcando navegadores abiertos"
    );
    assert!(h.query_all_by_label_contains("Chrome está abierto").count() == 0);
    h.get_by_label("Incluir Caché de navegadores").click();
    h.run_steps(3);
    assert!(
        explore(&h)
            .flow
            .basket
            .contains_category(CategoryId::ChromeCache)
    );
}

#[test]
fn los_textos_de_la_limpieza_estan_en_ingles_con_el_idioma_en_ingles() {
    let mut o = clean_opts(CleanStart::Confirm);
    o.settings.language = LangPref::En;
    let h = harness(o);
    h.get_by_label("Review before cleaning");
    h.get_by_label_contains("we check every path again right before deleting");
    h.get_by_label("Deleted and recreated on its own");
    h.get_by_label("Goes to the Recycle Bin · you can recover it");
    assert!(h.query_all_by_label_contains("Your file").count() >= 1);
    // The user's files have their parent folder (distinguishes repeated names).
    assert!(h.query_all_by_label_contains("in Descargas").count() >= 1);
    h.get_by_label_contains("These are your own files; they are not recreated");
}

/// M2: While the cleanup is running, closing the window hangs (it would leave the job half done) and the cleanup window
/// progress explains to wait or cancel; "Cancel" is still available. Outside of execution, close is not touched.
#[test]
fn cerrar_la_ventana_durante_la_ejecucion_se_bloquea_y_lo_explica() {
    // Simulated backup with pauses, so that the execution lasts long enough.
    let be = std::sync::Arc::new(crate::demo_backend::DemoBackend::new(
        std::time::Duration::from_millis(1500),
    ));
    let mut o = clean_opts(CleanStart::Recommendations);
    o.backend = Some(crate::app::BackendRef(be));
    let mut h = harness(o);
    h.run_steps(3);
    // Without executing anything: the closure passes.
    assert!(!h.state_mut().ui_mut().guard_close(true));
    h.get_by_label_contains("Limpiar lo seguro").click();
    assert!(wait_contains(&mut h, "Revisa antes de limpiar"));
    // With open commit (nothing is executed yet) it also happens.
    assert!(!h.state_mut().ui_mut().guard_close(true));
    h.get_by_label_contains("Liberar ").click();
    assert!(wait_contains(&mut h, "Limpiando"));
    assert!(explore(&h).flow.is_running());
    // Without a closure request, nothing is blocked or notified.
    assert!(!h.state_mut().ui_mut().guard_close(false));
    assert!(
        h.query_all_by_label_contains("Espera a que termine")
            .count()
            == 0
    );
    // With the request: it is blocked and explained.
    assert!(h.state_mut().ui_mut().guard_close(true));
    h.run_steps(3);
    assert!(
        h.query_all_by_label_contains("Espera a que termine o cancela")
            .count()
            >= 1
    );
    // Cancel is still available.
    assert!(!h.get_by_label("Cancelar").accesskit_node().is_disabled());
}

/// M-B: "Wipe Safe" also runs from the scan screen - closing the window still crashes.
#[test]
fn cerrar_la_ventana_durante_la_ejecucion_desde_el_escaneo_tambien_se_bloquea() {
    let be = std::sync::Arc::new(crate::demo_backend::DemoBackend::new(
        std::time::Duration::from_millis(1500),
    ));
    let mut o = opts(StartScreen::Scan(1.0));
    o.backend = Some(crate::app::BackendRef(be));
    let mut h = harness(o);
    assert_eq!(h.state().ui().screen_name(), "scan");
    // Without executing anything the closure passes.
    assert!(!h.state_mut().ui_mut().guard_close(true));
    assert!(
        wait_contains(&mut h, "Limpiar lo seguro"),
        "falta el botón en el escaneo"
    );
    h.get_by_label_contains("Limpiar lo seguro").click();
    assert!(wait_contains(&mut h, "Revisa antes de limpiar"));
    assert!(
        !h.state_mut().ui_mut().guard_close(true),
        "con la confirmación abierta aún no se ejecuta nada"
    );
    h.get_by_label_contains("Liberar ").click();
    assert!(wait_contains(&mut h, "Limpiando"));
    assert_eq!(h.state().ui().screen_name(), "scan");
    assert!(h.state_mut().ui_mut().guard_close(true));
    h.run_steps(3);
    assert!(
        h.query_all_by_label_contains("Espera a que termine o cancela")
            .count()
            >= 1
    );
}

/// UX-2: The "Home" button in the top bar (to the left of "Up one level") returns to the choose disk screen.
#[test]
fn el_boton_inicio_vuelve_a_la_pantalla_de_inicio() {
    for en in [false, true] {
        let mut o = opts(user_folder());
        if en {
            o.settings.language = LangPref::En;
        }
        let mut h = harness(o);
        let (home, up) = if en {
            ("Home", "Up one level")
        } else {
            ("Inicio", "Subir un nivel")
        };
        assert_eq!(h.state().ui().screen_name(), "explore");
        // To the left of "Go up a level" and without stepping on it.
        let (rh, ru) = (h.get_by_label(home).rect(), h.get_by_label(up).rect());
        assert!(rh.right() <= ru.left(), "{rh:?} {ru:?}");
        assert!((rh.center().y - ru.center().y).abs() < 1.0);
        h.get_by_label(home).click();
        h.run_steps(4);
        assert_eq!(h.state().ui().screen_name(), "home", "en: {en}");
        h.get_by_label(if en {
            "Which drive do you want to check?"
        } else {
            "¿Qué disco quieres revisar?"
        });
    }
}

/// UX-2: Alt+Home does the same; Home is still just browsing the list (it doesn't exit the scan).
#[test]
fn alt_inicio_vuelve_a_la_pantalla_de_inicio_e_inicio_a_secas_no() {
    let mut h = harness(opts(user_folder()));
    h.key_press(Key::Home);
    h.run_steps(4);
    assert_eq!(h.state().ui().screen_name(), "explore");
    h.key_press_modifiers(Modifiers::ALT, Key::Home);
    h.run_steps(4);
    assert_eq!(h.state().ui().screen_name(), "home");
}

/// UX-2: With the minimum window (960 x 600) the new button does not overlap anything in the top bar.
#[test]
fn con_la_ventana_minima_el_boton_inicio_no_se_solapa_en_la_barra_superior() {
    for en in [false, true] {
        let mut o = opts(user_folder());
        if en {
            o.settings.language = LangPref::En;
        }
        let h = harness_sized(o, vec2(960.0, 600.0));
        let (home, up) = if en {
            ("Home", "Up one level")
        } else {
            ("Inicio", "Subir un nivel")
        };
        let mut rects = vec![
            (home.to_string(), h.get_by_label(home).rect()),
            (up.to_string(), h.get_by_label(up).rect()),
        ];
        for t in ["Buscar", "Ir a "] {
            if let Some(n) = h
                .query_all_by_label_contains(t)
                .find(|n| n.rect().height() > 1.0)
            {
                rects.push((t.to_string(), n.rect()));
            }
        }
        for (i, (a, ra)) in rects.iter().enumerate() {
            assert!(
                ra.left() >= -0.5 && ra.right() <= 960.5,
                "{a} fuera: {ra:?}"
            );
            for (b, rb) in &rects[i + 1..] {
                assert!(
                    !ra.intersects(*rb),
                    "en: {en}: `{a}` y `{b}` se solapan: {ra:?} {rb:?}"
                );
            }
        }
    }
}

/// UX-2: With a cleanup in progress, "Home" is disabled and Alt+Home does nothing (such as closing the window).
#[test]
fn con_una_limpieza_en_curso_inicio_esta_deshabilitado() {
    let be = std::sync::Arc::new(crate::demo_backend::DemoBackend::new(
        std::time::Duration::from_millis(1500),
    ));
    let mut o = clean_opts(CleanStart::Recommendations);
    o.backend = Some(crate::app::BackendRef(be));
    let mut h = harness(o);
    h.run_steps(3);
    // Uncleaned: Enabled.
    assert!(!h.get_by_label("Inicio").accesskit_node().is_disabled());
    h.get_by_label_contains("Limpiar lo seguro").click();
    assert!(wait_contains(&mut h, "Revisa antes de limpiar"));
    h.get_by_label_contains("Liberar ").click();
    assert!(wait_contains(&mut h, "Limpiando"));
    assert!(explore(&h).flow.is_running());
    assert!(h.get_by_label("Inicio").accesskit_node().is_disabled());
    h.key_press_modifiers(Modifiers::ALT, Key::Home);
    h.run_steps(3);
    assert!(
        explore(&h).flow.is_running(),
        "Alt+Inicio no debe salir durante la limpieza"
    );
    assert_eq!(h.state().ui().screen_name(), "explore");
}
