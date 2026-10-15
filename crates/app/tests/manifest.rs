//! Comprueba el manifiesto REAL incrustado en `tesela.exe` (PLAN.md §3.9): `asInvoker` (nunca pide UAC), rutas largas,
//! PerMonitorV2, solo Windows 10/11 y SIN `activeCodePage` (la app usa las APIs W; el valor por defecto de
//! `embed-manifest` lo activaría como UTF-8).

fn manifest() -> String {
    let exe = std::fs::read(env!("CARGO_BIN_EXE_tesela")).expect("tesela.exe compilado");
    let text = String::from_utf8_lossy(&exe);
    let start = text.find("<assembly").expect("el exe lleva un manifiesto");
    let end = text[start..].find("</assembly>").expect("manifiesto completo") + start + "</assembly>".len();
    text[start..end].to_string()
}

#[test]
fn el_manifiesto_pide_lo_que_dice_el_plan() {
    let m = manifest();
    assert!(m.contains("level=\"asInvoker\""), "{m}");
    assert!(!m.contains("requireAdministrator") && !m.contains("highestAvailable"), "{m}");
    assert!(m.contains("longPathAware") && m.contains(">true</"), "rutas largas: {m}");
    assert!(m.to_lowercase().contains("permonitorv2"), "{m}");
    assert!(!m.contains("activeCodePage"), "sin UTF-8 como página de códigos: {m}");
    // Solo Windows 10 y 11 (un único GUID de supportedOS: el de Windows 10/11).
    assert_eq!(m.matches(":supportedOS ").count(), 1, "{m}");
    assert!(m.contains("8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a"), "{m}");
}
