//! Pruebas del flujo de limpieza de la interfaz (cesta, Recomendaciones, confirmación, ejecución y resumen) con el
//! respaldo SIMULADO del modo demostración: nada de esto toca el disco.

use super::*;
use crate::app::CleanStart;
use egui::accesskit::Toggled;
use tesela_cleaner::CategoryId;

fn clean_opts(start: CleanStart) -> Options {
    opts(StartScreen::Clean(start))
}

/// Pasos hasta que desaparece una etiqueta.
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

/// Pasos hasta que aparece una etiqueta que CONTIENE `text`; después unos pasos más: una ventana modal tarda unos
/// fotogramas en asentar su posición y un clic en mitad de ese movimiento falla el botón.
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

/// R-1: con la ventana MÍNIMA real (960x600) la pantalla de Recomendaciones no pisa nada: el titular largo ("Hasta X en
/// candidatos de bajo riesgo...") pasa a varias líneas junto al botón principal sin cubrirlo, ni al subtítulo, ni al texto
/// (largo) de hiberfil.sys. En español y en inglés.
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
    // Las tres clases de riesgo con su frase.
    h.get_by_label("Seguro");
    h.get_by_label_contains("Ya marcado");
    h.get_by_label("Revisar");
    assert!(h.query_all_by_label("Solo informar").count() >= 1);
    // Lo seguro viene marcado (la primera vez): temporales y las cachés de Edge y Firefox; Chrome está abierto.
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
    // Descargas y la Papelera: Revisar, con su botón; los de solo informar, sin casilla ni botón.
    assert!(
        h.query_all_by_label("Ver").count() >= 2,
        "el botón de Descargas y el de la cesta"
    );
    h.get_by_label("Vaciar");
    h.get_by_label_contains("pagefile.sys");
    h.get_by_label_contains("hiberfil.sys");
    // La barra de la cesta refleja lo marcado.
    h.get_by_label_contains("Cesta · 4 elementos");
    // Desmarcar una tarjeta la saca de la cesta.
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
    // El primer foco está en Cancelar (un Intro suelto nunca borra).
    assert!(h.get_by_label("Cancelar").is_focused());
    // El botón principal dice lo que va a pasar.
    h.get_by_label_contains("Liberar ");
    // Cancelar cierra sin tocar nada.
    h.get_by_label("Cancelar").click();
    assert!(wait_gone(&mut h, "Revisa antes de limpiar"));
    assert_eq!(explore(&h).version, before_version);
    assert!(explore(&h).overlay.is_none(), "cancelar no cambia el árbol");
    // Escape también.
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
    // Los tres contadores y lo que se saltó.
    h.get_by_label("Eliminado definitivamente");
    h.get_by_label("Movido a la Papelera");
    h.get_by_label_contains("archivos en uso");
    h.get_by_label_contains("Chrome sigue abierto");
    // Sin "Deshacer" (es de la v0.2) y sin abrir la Papelera real en la demostración.
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
    // El árbol se actualizó SIN reescanear: la carpeta temporal y la raíz encogieron; los histogramas son "aproximados".
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
    // Entrada del menú (hay otra copia en el panel de detalles: el menú es la que está dentro del popup).
    let items: Vec<_> = h.query_all_by_label("Añadir a la cesta").collect();
    assert!(items.len() >= 2);
    items.into_iter().next().unwrap().click();
    h.run_steps(3);
    assert_eq!(basket_count(&h), 1);
    h.get_by_label_contains("Cesta · 1 elemento");
    // El detalle lo marca y ofrece quitarlo.
    h.get_by_label("En la cesta");
    h.get_by_label("Quitar de la cesta").click();
    h.run_steps(3);
    assert_eq!(basket_count(&h), 0);
    // Lo mismo desde el botón de los detalles, y "Ver" abre la lista agrupada por destino con sus frases.
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
    // AppData es intocable como destino: se ve con candado, "Añadir" explica por qué y "Ver contenido" ocupa su lugar.
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
    // Una carpeta conocida (Vídeos): solo "Contenido de …", nunca la carpeta en sí.
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
    // Las rutas del sistema (Windows) están denegadas por completo.
    h.get_by_label("Ir a Users").click();
    h.run_steps(3);
}

#[test]
fn lo_que_no_cabe_en_la_papelera_sale_en_el_grupo_ambar_con_su_casilla() {
    let mut h = harness(opts(StartScreen::Map(MapStart::default())));
    h.get_by_label("Lista").click();
    h.run_steps(3);
    // `Juegos` (58 GB) supera el límite de la Papelera de la unidad ficticia (el 5 % conservador de la unidad, con margen).
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
    // Sin la casilla no hay nada que liberar: el botón principal no promete espacio.
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
    // T1: y dice que otros programas pueden influir en esa cifra.
    h.get_by_label_contains("otros programas que escriban o borren a la vez");
    h.get_by_label_contains("están en la Papelera de reciclaje");
    h.get_by_label_contains("Vaciar ahora").click();
    assert!(wait_contains(&mut h, "Revisa antes de limpiar"));
    h.get_by_label("Vaciar la Papelera");
    h.get_by_label_contains("Lo que entre después no se toca");
    // Es irreversible: el foco está en Cancelar.
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

/// H4: una categoría descartada por un enlace en su ruta (p. ej. `User Data` de Edge movida a otra unidad con un junction)
/// no desaparece en silencio: sale como "No disponible" con su motivo, en español y en inglés.
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
        // Chrome sigue disponible; Edge ya no aparece como casilla utilizable.
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
    // El usuario cierra Chrome y pulsa "Volver a comprobar": su caché pasa a ser utilizable (y se puede marcar).
    be.set_browsers(Vec::new());
    // Con el teclado (el enlace puede quedar bajo la barra de la cesta en una ventana baja).
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
    // Los archivos del usuario llevan su carpeta padre (distingue nombres repetidos).
    assert!(h.query_all_by_label_contains("in Descargas").count() >= 1);
    h.get_by_label_contains("These are your own files; they are not recreated");
}

/// M2: mientras se ejecuta la limpieza, cerrar la ventana se bloquea (dejaría el trabajo a medias) y la ventana de
/// progreso explica que espere o cancele; "Cancelar" sigue disponible. Fuera de la ejecución, cerrar no se toca.
#[test]
fn cerrar_la_ventana_durante_la_ejecucion_se_bloquea_y_lo_explica() {
    // El respaldo simulado con pausas, para que la ejecución dure lo bastante.
    let be = std::sync::Arc::new(crate::demo_backend::DemoBackend::new(
        std::time::Duration::from_millis(1500),
    ));
    let mut o = clean_opts(CleanStart::Recommendations);
    o.backend = Some(crate::app::BackendRef(be));
    let mut h = harness(o);
    h.run_steps(3);
    // Sin ejecutar nada: el cierre pasa.
    assert!(!h.state_mut().ui_mut().guard_close(true));
    h.get_by_label_contains("Limpiar lo seguro").click();
    assert!(wait_contains(&mut h, "Revisa antes de limpiar"));
    // Con la confirmación abierta (nada se ejecuta aún) también pasa.
    assert!(!h.state_mut().ui_mut().guard_close(true));
    h.get_by_label_contains("Liberar ").click();
    assert!(wait_contains(&mut h, "Limpiando"));
    assert!(explore(&h).flow.is_running());
    // Sin petición de cierre no se bloquea nada ni se avisa.
    assert!(!h.state_mut().ui_mut().guard_close(false));
    assert!(
        h.query_all_by_label_contains("Espera a que termine")
            .count()
            == 0
    );
    // Con la petición: se bloquea y se explica.
    assert!(h.state_mut().ui_mut().guard_close(true));
    h.run_steps(3);
    assert!(
        h.query_all_by_label_contains("Espera a que termine o cancela")
            .count()
            >= 1
    );
    // Cancelar sigue disponible.
    assert!(!h.get_by_label("Cancelar").accesskit_node().is_disabled());
}

/// M-B: "Limpiar lo seguro" también se ejecuta desde la pantalla de escaneo: cerrar la ventana se bloquea igual.
#[test]
fn cerrar_la_ventana_durante_la_ejecucion_desde_el_escaneo_tambien_se_bloquea() {
    let be = std::sync::Arc::new(crate::demo_backend::DemoBackend::new(
        std::time::Duration::from_millis(1500),
    ));
    let mut o = opts(StartScreen::Scan(1.0));
    o.backend = Some(crate::app::BackendRef(be));
    let mut h = harness(o);
    assert_eq!(h.state().ui().screen_name(), "scan");
    // Sin ejecutar nada el cierre pasa.
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
