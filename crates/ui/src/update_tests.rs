//! Testing optional update checking with a FALSE transport: none of them touch the network (nor open the
//! browser: the way to open it is also replaced).

use crate::app::{BackendRef, TransportRef, UpdatePrefs, unix_now};
use crate::tests::test_backend;
use crate::{DemoOptions, LangPref, Options, Settings, StartScreen, TeselaUi, ThemePref};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tesela_update::{Request, Transport, TransportError, Version, release_url};

const DAY: u64 = 24 * 60 * 60;

/// False Transportation: write down each request and answer what was given. He never touches the net.
struct Mock {
    answer: Mutex<Result<Vec<u8>, TransportError>>,
    requests: Mutex<Vec<Request>>,
    /// If there is, the response waits for it to be released (to test what happens with an in-flight request).
    gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl Mock {
    fn new(answer: Result<Vec<u8>, TransportError>) -> Arc<Mock> {
        Arc::new(Mock {
            answer: Mutex::new(answer),
            requests: Mutex::new(Vec::new()),
            gate: Mutex::new(None),
        })
    }
    /// Like [`Mock::tag`], but does not respond until something is sent on the returned channel.
    fn gated(tag: &str) -> (Arc<Mock>, std::sync::mpsc::Sender<()>) {
        let mock = Mock::tag(tag);
        let (tx, rx) = std::sync::mpsc::channel();
        *mock.gate.lock().unwrap() = Some(rx);
        (mock, tx)
    }
    /// Reply with a JSON of `releases/latest` that carries that `tag_name` (and a foreign URL that should not be used).
    fn tag(tag: &str) -> Arc<Mock> {
        Mock::new(Ok(format!(
            r#"{{"tag_name":"{tag}","html_url":"https://evil.example/pwned","assets":[{{"browser_download_url":"https://evil.example/tesela.exe"}}]}}"#
        )
        .into_bytes()))
    }
    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Transport for Mock {
    fn get(&self, request: &Request) -> Result<Vec<u8>, TransportError> {
        self.requests.lock().unwrap().push(request.clone());
        let gate = self.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            let _ = gate.recv_timeout(Duration::from_secs(10));
        }
        self.answer.lock().unwrap().clone()
    }
}

fn current() -> Version {
    Version::current().expect("la versión del producto es X.Y.Z")
}

/// A newer version than this executable.
fn newer() -> Version {
    Version {
        patch: current().patch + 1,
        ..current()
    }
}

struct App {
    opts: Options,
    ui: Option<TeselaUi>,
    /// What would have been opened in the browser.
    opened: Arc<Mutex<Vec<String>>>,
}

impl App {
    fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let opts = self.opts.clone();
        let opened = self.opened.clone();
        self.ui
            .get_or_insert_with(|| {
                let mut u = TeselaUi::new(&ctx, opts);
                u.set_url_opener(Box::new(move |url| {
                    opened.lock().unwrap().push(url.to_owned());
                }));
                u
            })
            .show(ui);
    }
    fn ui_mut(&mut self) -> &mut TeselaUi {
        self.ui.as_mut().expect("la interfaz ya se creó")
    }
}

fn opts(prefs: UpdatePrefs, lang: LangPref, mock: Option<&Arc<Mock>>) -> Options {
    Options {
        settings: Settings {
            theme: ThemePref::Light,
            language: lang,
            update: prefs,
        },
        demo: None,
        animate: false,
        start: StartScreen::Home,
        dry_run: false,
        backend: Some(BackendRef(test_backend())),
        isolated: false,
        update_transport: mock.map(|m| TransportRef(m.clone())),
    }
}

fn on(last_check: Option<u64>) -> UpdatePrefs {
    UpdatePrefs {
        check: true,
        last_check,
        ..UpdatePrefs::default()
    }
}

fn harness_sized(o: Options, w: f32, h: f32) -> Harness<'static, App> {
    let mut harness = Harness::builder()
        .with_size(vec2(w, h))
        .with_step_dt(0.02)
        .build_ui_state(
            |ui, app: &mut App| app.frame(ui),
            App {
                opts: o,
                ui: None,
                opened: Arc::default(),
            },
        );
    harness.run_steps(6);
    harness
}

fn harness(o: Options) -> Harness<'static, App> {
    harness_sized(o, 1280.0, 800.0)
}

/// Keep painting until `cond` is fulfilled (the request is in another thread) or your patience runs out.
fn wait_until(
    h: &mut Harness<'static, App>,
    cond: impl Fn(&mut Harness<'static, App>) -> bool,
) -> bool {
    for _ in 0..300 {
        h.run_steps(1);
        if cond(h) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Allow time and frames to pass to verify that nothing is happening.
fn settle(h: &mut Harness<'static, App>) {
    std::thread::sleep(Duration::from_millis(120));
    h.run_steps(8);
}

fn prefs(h: &mut Harness<'static, App>) -> UpdatePrefs {
    h.state_mut().ui_mut().settings().update
}

fn view(h: &mut Harness<'static, App>) -> crate::update::UpdateView {
    h.state_mut().ui_mut().update_view()
}

#[test]
fn desactivado_no_se_hace_ninguna_peticion() {
    let mock = Mock::tag(&newer().tag());
    let mut h = harness(opts(UpdatePrefs::default(), LangPref::Es, Some(&mock)));
    settle(&mut h);
    assert_eq!(mock.calls(), 0);
    let p = prefs(&mut h);
    assert!(!p.check && p.last_check.is_none() && p.latest_seen.is_none());
    assert!(view(&mut h).notice.is_none());
    // The default setting is "disabled".
    assert!(!Settings::default().update.check);
    assert!(h.state_mut().ui_mut().take_settings_change().is_none());
}

#[test]
fn activado_hace_una_peticion_y_no_mas_de_una_cada_24_horas() {
    let before = unix_now();
    let mock = Mock::tag(&newer().tag());
    let mut h = harness(opts(on(None), LangPref::Es, Some(&mock)));
    assert!(wait_until(&mut h, |h| prefs(h).latest_seen.is_some()));
    assert_eq!(mock.calls(), 1);
    // The attempt was noted (and saved: settings changed).
    let last = prefs(&mut h).last_check.expect("fecha del intento");
    assert!((before..=unix_now()).contains(&last));
    let saved = h
        .state_mut()
        .ui_mut()
        .take_settings_change()
        .expect("se guarda");
    assert_eq!(saved.update.latest_seen, Some(newer()));
    // Painting more does not repeat the request.
    settle(&mut h);
    assert_eq!(mock.calls(), 1);

    // Restart Tesela with what was saved, an hour later: it does not ask again, and the warning is still there.
    let again = Mock::tag(&newer().tag());
    let mut saved_prefs = saved.update;
    saved_prefs.last_check = Some(unix_now() - 3600);
    let mut h2 = harness(opts(saved_prefs, LangPref::Es, Some(&again)));
    settle(&mut h2);
    assert_eq!(
        again.calls(),
        0,
        "menos de 24 horas desde el último intento"
    );
    assert_eq!(view(&mut h2).notice, Some(newer()));

    // After 24 hours, yes.
    let late = Mock::tag(&newer().tag());
    let mut h3 = harness(opts(
        on(Some(unix_now() - DAY - 5)),
        LangPref::Es,
        Some(&late),
    ));
    assert!(wait_until(&mut h3, |h| prefs(h).latest_seen.is_some()));
    assert_eq!(late.calls(), 1);
}

#[test]
fn un_intento_fallido_tambien_cuenta_para_las_24_horas() {
    let mock = Mock::new(Err(TransportError::Unavailable));
    let mut h = harness(opts(on(None), LangPref::Es, Some(&mock)));
    assert!(wait_until(&mut h, |h| view(h).failed));
    assert_eq!(mock.calls(), 1);
    assert!(prefs(&mut h).last_check.is_some());
    settle(&mut h);
    assert_eq!(mock.calls(), 1);
}

#[test]
fn la_peticion_es_exactamente_la_esperada() {
    let mock = Mock::tag(&current().tag());
    let mut h = harness(opts(on(None), LangPref::Es, Some(&mock)));
    assert!(wait_until(&mut h, |h| prefs(h).latest_seen.is_some()));
    let sent = mock.requests.lock().unwrap().clone();
    assert_eq!(
        sent,
        vec![Request {
            url: "https://api.github.com/repos/wandoth1/tesela/releases/latest".to_owned(),
            user_agent: "Tesela".to_owned(),
        }]
    );
}

#[test]
fn activarlo_en_ajustes_comprueba_al_momento_y_desactivarlo_olvida_la_respuesta() {
    let mock = Mock::tag(&newer().tag());
    let mut h = harness(opts(UpdatePrefs::default(), LangPref::Es, Some(&mock)));
    h.get_by_label("Ajustes").click();
    h.run_steps(3);
    h.get_by_label("Buscar actualizaciones al abrir Tesela")
        .click();
    h.run_steps(3);
    assert!(prefs(&mut h).check);
    assert!(wait_until(&mut h, |h| prefs(h).latest_seen.is_some()));
    assert_eq!(mock.calls(), 1);
    assert_eq!(view(&mut h).notice, Some(newer()));
    // Deactivate: the warning disappears and is not asked again (although it is reactivated immediately: there are 24 hours left).
    h.state_mut()
        .ui_mut()
        .apply_for_test(crate::app::Action::SetCheckUpdates(false));
    h.run_steps(2);
    assert!(view(&mut h).notice.is_none());
    h.state_mut()
        .ui_mut()
        .apply_for_test(crate::app::Action::SetCheckUpdates(true));
    settle(&mut h);
    assert_eq!(mock.calls(), 1);
}

#[test]
fn una_respuesta_que_llega_con_el_ajuste_desactivado_se_olvida() {
    let (mock, release) = Mock::gated(&newer().tag());
    let mut h = harness(opts(on(None), LangPref::Es, Some(&mock)));
    assert!(wait_until(&mut h, |_| mock.calls() == 1));
    // It is deactivated with the request in flight; then the answer comes.
    h.state_mut()
        .ui_mut()
        .apply_for_test(crate::app::Action::SetCheckUpdates(false));
    release.send(()).unwrap();
    settle(&mut h);
    assert!(prefs(&mut h).latest_seen.is_none());
    assert!(view(&mut h).notice.is_none());
}

#[test]
fn la_demostracion_y_las_herramientas_de_medicion_nunca_comprueban() {
    // Demo with the setting on and a hand carry: nothing.
    let mock = Mock::tag(&newer().tag());
    let mut o = opts(on(None), LangPref::Es, Some(&mock));
    o.demo = Some(DemoOptions::default());
    let mut h = harness(o);
    settle(&mut h);
    assert_eq!(mock.calls(), 0, "demo");
    let v = view(&mut h);
    assert!(!v.available && v.notice.is_none() && !v.failed);
    assert!(prefs(&mut h).last_check.is_none());
    // Adjustment is not offered in the demo.
    h.get_by_label("Ajustes").click();
    h.run_steps(3);
    assert!(
        h.query_by_label("Buscar actualizaciones al abrir Tesela")
            .is_none()
    );

    // Measurement tools (isolated session, with and without `--demo`).
    for demo in [None, Some(DemoOptions::default())] {
        let mock = Mock::tag(&newer().tag());
        let mut o = opts(on(None), LangPref::Es, Some(&mock));
        o.isolated = true;
        o.demo = demo;
        let mut h = harness(o);
        settle(&mut h);
        assert_eq!(mock.calls(), 0, "aislada (demo: {})", demo.is_some());
        assert!(!view(&mut h).available);
        // It doesn't even open a page: there is no way to get to the URL.
        h.state_mut()
            .ui_mut()
            .apply_for_test(crate::app::Action::OpenRelease(newer()));
        assert!(h.state().opened.lock().unwrap().is_empty());
    }

    // No transport (the executable does not pass it): there is nothing to offer either.
    let mut h = harness(opts(on(None), LangPref::Es, None));
    settle(&mut h);
    assert!(!view(&mut h).available);
}

#[test]
fn el_aviso_depende_de_la_version_que_responde_github() {
    let cases: Vec<(String, bool)> = vec![
        (newer().tag(), true),
        (
            Version {
                minor: current().minor + 1,
                patch: 0,
                ..current()
            }
            .tag(),
            true,
        ),
        (current().tag(), false),
        (
            Version {
                major: 0,
                minor: 0,
                patch: 1,
            }
            .tag(),
            false,
        ),
        ("v9.9.9-rc1".to_owned(), false),
        ("9.9.9".to_owned(), false),
        ("v9.9".to_owned(), false),
        (String::new(), false),
    ];
    for (tag, notice) in cases {
        let mock = Mock::tag(&tag);
        let mut h = harness(opts(on(None), LangPref::Es, Some(&mock)));
        assert!(wait_until(&mut h, |_| mock.calls() == 1));
        settle(&mut h);
        assert_eq!(view(&mut h).notice.is_some(), notice, "tag {tag:?}");
    }
}

#[test]
fn json_roto_y_errores_del_transporte_se_ignoran_con_una_linea_en_ajustes() {
    let answers: Vec<Result<Vec<u8>, TransportError>> = vec![
        Ok(b"{ esto no es json".to_vec()),
        Ok(b"<html>portal cautivo</html>".to_vec()),
        Ok(br#"{"tag_name":42}"#.to_vec()),
        Ok(Vec::new()),
        Err(TransportError::Unavailable),
        Err(TransportError::Status(403)),
        Err(TransportError::Status(404)),
        Err(TransportError::TooLarge),
    ];
    for answer in answers {
        let mock = Mock::new(answer.clone());
        let mut h = harness(opts(on(None), LangPref::Es, Some(&mock)));
        assert!(wait_until(&mut h, |h| view(h).failed), "{answer:?}");
        assert!(view(&mut h).notice.is_none(), "{answer:?}");
        // The startup notice does not appear and the line only appears within the Settings menu.
        assert!(h.query_by_label_contains("Hay una versión nueva").is_none());
        h.get_by_label("Ajustes").click();
        h.run_steps(3);
        h.get_by_label_contains("No se pudo comprobar");
    }
}

#[test]
fn el_aviso_sale_se_descarta_y_no_vuelve_a_salir_para_esa_version() {
    let mock = Mock::tag(&newer().tag());
    let mut h = harness(opts(on(None), LangPref::Es, Some(&mock)));
    assert!(wait_until(&mut h, |h| view(h).notice.is_some()));
    h.run_steps(2);
    h.get_by_label_contains(&format!("Hay una versión nueva de Tesela: {}", newer()));
    h.get_by_label("Ver novedades");
    h.get_by_label("Descartar").click();
    h.run_steps(3);
    assert!(h.query_by_label_contains("Hay una versión nueva").is_none());
    let saved = h
        .state_mut()
        .ui_mut()
        .take_settings_change()
        .expect("se guarda");
    assert_eq!(saved.update.dismissed, Some(newer()));

    // When restarting with the saved (after 24 hours, ask again and GitHub says the same): it still doesn't exit.
    let mut p = saved.update;
    p.last_check = Some(unix_now() - DAY - 5);
    let again = Mock::tag(&newer().tag());
    let mut h2 = harness(opts(p, LangPref::Es, Some(&again)));
    assert!(wait_until(&mut h2, |_| again.calls() == 1));
    settle(&mut h2);
    assert!(view(&mut h2).notice.is_none());
    assert!(
        h2.query_by_label_contains("Hay una versión nueva")
            .is_none()
    );

    // A later version does warn again.
    let later = Version {
        patch: newer().patch + 1,
        ..newer()
    };
    let mock3 = Mock::tag(&later.tag());
    let mut h3 = harness(opts(
        UpdatePrefs {
            last_check: Some(unix_now() - DAY - 5),
            ..p
        },
        LangPref::Es,
        Some(&mock3),
    ));
    assert!(wait_until(&mut h3, |h| view(h).notice == Some(later)));
}

#[test]
fn un_aviso_descartado_no_vuelve_con_una_version_menor_o_igual_a_la_descartada() {
    let higher = Version {
        patch: newer().patch + 1,
        ..newer()
    };
    for answer in [newer(), higher] {
        let mock = Mock::tag(&answer.tag());
        let mut p = on(None);
        p.dismissed = Some(higher);
        let mut h = harness(opts(p, LangPref::Es, Some(&mock)));
        assert!(wait_until(&mut h, |_| mock.calls() == 1));
        settle(&mut h);
        assert!(view(&mut h).notice.is_none(), "{answer}");
        assert!(h.query_by_label_contains("Hay una versión nueva").is_none());
    }
}

#[test]
fn ver_novedades_abre_la_pagina_que_construye_tesela_y_no_una_url_de_la_respuesta() {
    let mock = Mock::tag(&newer().tag());
    let mut h = harness(opts(on(None), LangPref::En, Some(&mock)));
    assert!(wait_until(&mut h, |h| view(h).notice.is_some()));
    h.run_steps(2);
    h.get_by_label("See what's new").click();
    h.run_steps(3);
    let opened = h.state().opened.lock().unwrap().clone();
    assert_eq!(
        opened,
        vec![format!(
            "https://github.com/wandoth1/tesela/releases/tag/v{}.{}.{}",
            current().major,
            current().minor,
            current().patch + 1
        )]
    );
    assert_eq!(opened[0], release_url(newer()));
    assert!(!opened[0].contains("evil"));
    // Opening the page does not dismiss the notice.
    assert_eq!(view(&mut h).notice, Some(newer()));
}

#[test]
fn los_textos_del_aviso_y_de_ajustes_salen_en_los_dos_idiomas() {
    for (lang, settings, available, view_text, dismiss, check, explain, when_first, only_number) in [
        (
            LangPref::Es,
            "Ajustes",
            "Hay una versión nueva de Tesela:",
            "Ver novedades",
            "Descartar",
            "Buscar actualizaciones al abrir Tesela",
            "nunca se descarga ni se instala ninguna actualización",
            "al marcar la casilla y, después, al abrirse",
            "Solo se descarga el número de la última versión",
        ),
        (
            LangPref::En,
            "Settings",
            "A new version of Tesela is available:",
            "See what's new",
            "Dismiss",
            "Check for updates when Tesela opens",
            "no update is ever downloaded or installed",
            "when you tick the box and then when it opens",
            "Only the latest version number is downloaded",
        ),
    ] {
        let mock = Mock::tag(&newer().tag());
        let mut h = harness(opts(on(None), lang, Some(&mock)));
        assert!(wait_until(&mut h, |h| view(h).notice.is_some()));
        h.run_steps(2);
        h.get_by_label_contains(available);
        h.get_by_label(view_text);
        h.get_by_label(dismiss);
        // The "·" separators are painted: they are not accessibility nodes that a screen reader will read.
        assert!(h.query_by_label_contains("·").is_none());
        h.get_by_label(settings).click();
        h.run_steps(3);
        h.get_by_label(check);
        h.get_by_label_contains(explain);
        // Consent says when it is first consulted and what is actually downloaded.
        h.get_by_label_contains(when_first);
        h.get_by_label_contains(only_number);
        // The explanation says what is consulted and what is sent, without promising more than it does.
        h.get_by_label_contains("GitHub");
    }
}

/// The notice and the Settings menu fit and are not stepped on at 960x600 (the minimum) and 1280x800, in both languages.
#[test]
fn la_disposicion_cabe_a_960x600_y_a_1280x800_en_es_y_en() {
    for (w, hgt) in [(960.0_f32, 600.0_f32), (1280.0, 800.0)] {
        for lang in [LangPref::Es, LangPref::En] {
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(w, hgt));
            // With a previous failure, so that the menu also has the line "could not be verified".
            let mock = Mock::tag(&newer().tag());
            let mut h = harness_sized(opts(on(None), lang, Some(&mock)), w, hgt);
            assert!(wait_until(&mut h, |h| view(h).notice.is_some()));
            h.state_mut().ui_mut().force_update_failed_for_test();
            h.run_steps(3);
            let es = lang == LangPref::Es;
            let (avail, title, settings, check, explain, failed) = if es {
                (
                    "Hay una versión nueva",
                    "¿Qué disco quieres revisar?",
                    "Ajustes",
                    "Buscar actualizaciones al abrir Tesela",
                    "GitHub",
                    "No se pudo comprobar",
                )
            } else {
                (
                    "A new version of Tesela",
                    "Which drive do you want to check?",
                    "Settings",
                    "Check for updates when Tesela opens",
                    "GitHub",
                    "Could not check",
                )
            };
            let notice = h.get_by_label_contains(avail).rect();
            let see = h
                .get_by_label(if es {
                    "Ver novedades"
                } else {
                    "See what's new"
                })
                .rect();
            let dismiss = h
                .get_by_label(if es { "Descartar" } else { "Dismiss" })
                .rect();
            let button = h.get_by_label(settings).rect();
            let title_rect = h.query_by_label_contains(title).map(|n| n.rect());
            for r in [notice, see, dismiss] {
                assert!(
                    screen.contains_rect(r),
                    "{w}x{hgt} {lang:?}: {r:?} fuera de la ventana"
                );
            }
            // In a single line, to the left of the Settings button, without stepping on the title.
            assert!(
                dismiss.max.x <= button.min.x,
                "{w}x{hgt} {lang:?}: el aviso pisa Ajustes"
            );
            assert!(
                notice.max.x <= see.min.x && see.max.x <= dismiss.min.x,
                "{w}x{hgt} {lang:?}: el aviso se solapa consigo mismo"
            );
            assert!(
                notice.height() < 24.0 && see.height() < 24.0,
                "{w}x{hgt} {lang:?}: el aviso no cabe en una línea"
            );
            if let Some(t) = title_rect {
                assert!(
                    !t.intersects(notice) && !t.intersects(dismiss),
                    "{w}x{hgt} {lang:?}: el aviso tapa el título"
                );
            }

            // The Settings menu with explanation and fault line: inside the window and without overlapping.
            h.get_by_label(settings).click();
            h.run_steps(3);
            let c = h.get_by_label(check).rect();
            let e = h.get_by_label_contains(explain).rect();
            let f = h.get_by_label_contains(failed).rect();
            for (name, r) in [("casilla", c), ("explicación", e), ("fallo", f)] {
                assert!(
                    screen.contains_rect(r),
                    "{w}x{hgt} {lang:?}: {name} {r:?} fuera de la ventana"
                );
            }
            assert!(
                c.max.y <= e.min.y && e.max.y <= f.min.y,
                "{w}x{hgt} {lang:?}: se solapan en vertical"
            );
            assert!(
                e.width() <= 345.0,
                "{w}x{hgt} {lang:?}: la explicación ensancha el menú ({})",
                e.width()
            );
            // The explanation is divided into several lines, it is not cut off or left the menu.
            assert!(
                e.height() > 40.0,
                "{w}x{hgt} {lang:?}: la explicación no se partió ({})",
                e.height()
            );
        }
    }
}
