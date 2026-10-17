//! Utilidades compartidas de las pruebas de integración de este crate.

/// Variable de entorno que activa las pruebas que LANZAN PROCESOS HIJO (copias del ejecutable, ventanas, archivos con
/// nombre de DLL junto a un `.exe`): `TESELA_PROCESS_TESTS=1`. Un antivirus de escritorio puede bloquear ese patrón en un
/// equipo de desarrollo; el CI (un runner efímero) las ejecuta siempre. En local se saltan con un mensaje.
pub const PROCESS_TESTS_VAR: &str = "TESELA_PROCESS_TESTS";

/// ¿Está definida `TESELA_PROCESS_TESTS=1`? Si no, lo dice por la salida de errores y la prueba debe volver sin hacer nada
/// (las aserciones no se han debilitado: con la variable corren completas).
///
/// En CI (variable `CI` definida, como hace GitHub Actions) el salto NO es posible: `cargo test` no enseña lo que una prueba
/// que pasa escribe por stderr, así que una variable perdida del workflow dejaría estas pruebas en verde sin ejecutarse.
/// Allí falta la variable = `panic!`.
pub fn process_tests_enabled() -> bool {
    if std::env::var(PROCESS_TESTS_VAR).is_ok_and(|v| v == "1") {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "CI sin {PROCESS_TESTS_VAR}=1: las pruebas con procesos hijo no pueden saltarse en CI (revisa el workflow)"
    );
    eprintln!(
        "pruebas con procesos hijo desactivadas: defina {PROCESS_TESTS_VAR}=1 para ejecutar esta prueba"
    );
    false
}
