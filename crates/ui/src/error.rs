//! Errores TIPADOS hasta la interfaz. Los crates de abajo (`tesela-scanner`, `platform`) devuelven enums, no frases: aquí
//! se traducen con Fluent al idioma de la sesión (y se retraducen si el usuario cambia de idioma). El detalle técnico del
//! sistema (el texto de un `io::Error`, un código `HRESULT`) va aparte, entre paréntesis, tal como lo da Windows.

use crate::args;
use crate::i18n::Tr;
use crate::platform::RevealError;
use tesela_scanner::ScanError;

/// Por qué no pudo empezar (o acabar) un análisis.
#[derive(Debug)]
pub enum StartError {
    /// Error del escáner (la raíz no se abre, no es una carpeta, un hilo falló).
    Scan(ScanError),
    /// El hilo de arranque terminó sin dejar resultado (no debería ocurrir).
    StartupLost,
    /// Se cerró o canceló la pantalla antes de que arrancara: no se enseña como error.
    Cancelled,
}

impl StartError {
    /// El motivo, traducido y con el detalle técnico aparte cuando lo hay.
    pub fn reason(&self, tr: &Tr) -> String {
        match self {
            StartError::Scan(ScanError::Root(e)) => match e.kind() {
                std::io::ErrorKind::NotFound => tr.t("error-scan-root-missing"),
                std::io::ErrorKind::PermissionDenied => tr.t("error-scan-root-denied"),
                _ => tr.ta("error-scan-root", args!["detail" => e.to_string()]),
            },
            StartError::Scan(ScanError::NotADirectory(_)) => tr.t("error-scan-not-dir"),
            StartError::Scan(ScanError::Panicked) => tr.t("error-scan-panicked"),
            StartError::StartupLost => tr.t("error-scan-startup-lost"),
            StartError::Cancelled => tr.t("error-scan-cancelled"),
        }
    }

    /// El aviso completo de la pantalla de inicio: "No se pudo empezar el análisis: …".
    pub fn message(&self, tr: &Tr) -> String {
        tr.ta("error-scan-start", args!["error" => self.reason(tr)])
    }
}

/// El aviso de que no se pudo abrir el Explorador: "No se pudo abrir el Explorador: …" (el código de Windows, aparte).
pub fn reveal_message(tr: &Tr, e: &RevealError) -> String {
    let reason = match e {
        RevealError::InvalidPath => tr.t("error-reveal-invalid"),
        RevealError::ParsePath(h) => {
            tr.ta("error-reveal-parse", args!["detail" => hresult_text(*h)])
        }
        RevealError::Open(h) => tr.ta("error-reveal-open", args!["detail" => hresult_text(*h)]),
    };
    tr.ta("error-open-explorer", args!["error" => reason])
}

/// `HRESULT` en hexadecimal (`0x80070002`), el detalle técnico que se enseña aparte.
fn hresult_text(h: i32) -> String {
    format!("{:#010X}", h as u32)
}

/// Un aviso que la pantalla de inicio enseña hasta que se lanza otro análisis. Se guarda TIPADO y se traduce al pintarlo.
#[derive(Debug)]
pub enum Notice {
    ScanStart(StartError),
}

impl Notice {
    pub fn text(&self, tr: &Tr) -> String {
        match self {
            Notice::ScanStart(e) => e.message(tr),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use std::path::PathBuf;

    fn io(code: i32) -> std::io::Error {
        std::io::Error::from_raw_os_error(code)
    }

    #[test]
    fn el_error_de_abrir_el_explorador_se_traduce_en_los_dos_idiomas() {
        let es = Tr::new(Lang::Es);
        let en = Tr::new(Lang::En);
        for (e, want_es, want_en) in [
            (
                RevealError::InvalidPath,
                "la ruta no es válida",
                "the path is not valid",
            ),
            (
                RevealError::ParsePath(-2147024894),
                "Windows no reconoce la ruta (0x80070002)",
                "Windows does not recognize the path (0x80070002)",
            ),
            (
                RevealError::Open(-2147024809),
                "el Explorador no pudo mostrarla (0x80070057)",
                "Explorer could not show it (0x80070057)",
            ),
        ] {
            let (m_es, m_en) = (reveal_message(&es, &e), reveal_message(&en, &e));
            assert!(
                m_es.starts_with("No se pudo abrir el Explorador: "),
                "{m_es}"
            );
            assert!(m_en.starts_with("Explorer could not be opened: "), "{m_en}");
            assert!(m_es.contains(want_es), "{m_es}");
            assert!(m_en.contains(want_en), "{m_en}");
            assert!(!m_es.contains("SHParseDisplayName") && !m_en.contains("SHOpenFolder"));
        }
    }

    #[test]
    fn cada_error_se_traduce_en_los_dos_idiomas_y_el_detalle_del_sistema_va_aparte() {
        let es = Tr::new(Lang::Es);
        let en = Tr::new(Lang::En);
        let cases: Vec<(StartError, &str, &str)> = vec![
            (
                StartError::Scan(ScanError::Root(io(2))),
                "No se pudo empezar el análisis: la ruta no existe",
                "The scan could not start: the path does not exist",
            ),
            (
                StartError::Scan(ScanError::Root(io(3))),
                "la ruta no existe",
                "the path does not exist",
            ),
            (
                StartError::Scan(ScanError::Root(io(5))),
                "no tienes permiso para abrir esa ruta",
                "you do not have permission to open that path",
            ),
            (
                // Otro código: el detalle técnico del sistema se enseña aparte, entre paréntesis.
                StartError::Scan(ScanError::Root(io(32))),
                "no se puede abrir la ruta (",
                "the path cannot be opened (",
            ),
            (
                StartError::Scan(ScanError::NotADirectory(PathBuf::from("x"))),
                "la ruta no es una carpeta ni una unidad",
                "the path is not a folder or a drive",
            ),
            (
                StartError::Scan(ScanError::Panicked),
                "falló de forma inesperada",
                "failed unexpectedly",
            ),
            (StartError::StartupLost, "se interrumpió", "was interrupted"),
        ];
        for (err, want_es, want_en) in &cases {
            let (m_es, m_en) = (err.message(&es), err.message(&en));
            assert!(m_es.contains(want_es), "{m_es}");
            assert!(m_en.contains(want_en), "{m_en}");
            assert_ne!(m_es, m_en);
            // Ningún texto de los crates de abajo se cuela sin traducir: el mensaje del escáner (inglés neutro) no sale.
            for m in [&m_es, &m_en] {
                assert!(!m.contains("scanner thread panicked"), "{m}");
                assert!(!m.contains("cannot open the scan root"), "{m}");
            }
        }
        // El detalle (el texto del sistema para ese código) es lo que va entre paréntesis.
        let detail = io(32).to_string();
        assert!(
            StartError::Scan(ScanError::Root(io(32)))
                .message(&es)
                .contains(&format!("({detail})"))
        );
        // El mensaje del escáner en sí no está en español (lo traduce la interfaz).
        assert!(!ScanError::Panicked.to_string().contains("pánico"));
        // Los avisos guardan el error tipado y se retraducen al cambiar de idioma.
        let n = Notice::ScanStart(StartError::Scan(ScanError::Root(io(2))));
        assert!(n.text(&es).starts_with("No se pudo empezar el análisis"));
        assert!(n.text(&en).starts_with("The scan could not start"));
    }
}
