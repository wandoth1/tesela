//! TYPED errors up to the interface. The crates below (`tesela-scanner`, `platform`) return enums, not phrases: here
//! they are translated with Fluent into the language of the session (and retranslated if the user changes languages). The technical detail of
//! system (the text of a `io::Error`, a `HRESULT` code) goes separately, in parentheses, as given by Windows.

use crate::args;
use crate::i18n::Tr;
use crate::platform::{RevealError, RevealOutcome};
use tesela_scanner::ScanError;

/// Why you couldn't start (or finish) an analysis.
#[derive(Debug)]
pub enum StartError {
    /// Scanner error (root does not open, not a folder, a thread failed).
    Scan(ScanError),
    /// The bootstrap thread terminated without output (shouldn't happen).
    StartupLost,
    /// Screen was closed or canceled before booting: not displayed as an error.
    Cancelled,
}

impl StartError {
    /// The reason, translated and with the technical detail aside when there is one.
    pub fn reason(&self, tr: &Tr) -> String {
        match self {
            StartError::Scan(ScanError::Root(e)) => match e.kind() {
                std::io::ErrorKind::NotFound => tr.t("error-scan-root-missing"),
                std::io::ErrorKind::PermissionDenied => tr.t("error-scan-root-denied"),
                _ => tr.ta("error-scan-root", args!["detail" => e.to_string()]),
            },
            StartError::Scan(ScanError::NotADirectory(_)) => tr.t("error-scan-not-dir"),
            StartError::Scan(ScanError::RootIsLink(_)) => tr.t("error-scan-root-link"),
            StartError::Scan(ScanError::Panicked) => tr.t("error-scan-panicked"),
            StartError::StartupLost => tr.t("error-scan-startup-lost"),
            StartError::Cancelled => tr.t("error-scan-cancelled"),
        }
    }

    /// The full home screen warning: "Failed to start analysis:...".
    pub fn message(&self, tr: &Tr) -> String {
        tr.ta("error-scan-start", args!["error" => self.reason(tr)])
    }
}

/// The warning that Explorer could not be opened: "Explorer could not be opened:..." (Windows code, separate).
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

/// The toast of how a request to open the Explorer ended; `None` if it went well.
pub fn reveal_outcome_message(tr: &Tr, o: &RevealOutcome) -> Option<String> {
    match o {
        RevealOutcome::Done => None,
        RevealOutcome::Failed(e) => Some(reveal_message(tr, e)),
        RevealOutcome::NoResponse => Some(tr.t("error-reveal-timeout")),
        RevealOutcome::Busy => Some(tr.t("error-reveal-busy")),
    }
}

/// `HRESULT` in hexadecimal (`0x80070002`), the technical detail shown separately.
fn hresult_text(h: i32) -> String {
    format!("{:#010X}", h as u32)
}

/// A warning that the home screen displays until another analysis is launched. It is saved TYPE and translated when painted.
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
    fn los_desenlaces_de_abrir_el_explorador_se_traducen() {
        let es = Tr::new(Lang::Es);
        let en = Tr::new(Lang::En);
        assert_eq!(reveal_outcome_message(&es, &RevealOutcome::Done), None);
        assert!(
            reveal_outcome_message(&es, &RevealOutcome::NoResponse)
                .unwrap()
                .contains("no responde")
        );
        assert!(
            reveal_outcome_message(&en, &RevealOutcome::NoResponse)
                .unwrap()
                .contains("not responding")
        );
        assert!(
            reveal_outcome_message(&es, &RevealOutcome::Busy)
                .unwrap()
                .contains("ocupado")
        );
        assert!(
            reveal_outcome_message(&en, &RevealOutcome::Busy)
                .unwrap()
                .contains("busy")
        );
        assert!(
            reveal_outcome_message(&es, &RevealOutcome::Failed(RevealError::InvalidPath))
                .unwrap()
                .starts_with("No se pudo abrir el Explorador")
        );
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
                // Other code: the technical detail of the system is shown separately, in parentheses.
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
                StartError::Scan(ScanError::RootIsLink(PathBuf::from("x"))),
                "la carpeta elegida es un enlace (junction o symlink); Tesela no sigue enlaces",
                "the chosen folder is a link (junction or symlink); Tesela does not follow links",
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
            // No text from the crates below slips through untranslated: the scanner message (neutral English) does not appear.
            for m in [&m_es, &m_en] {
                assert!(!m.contains("scanner thread panicked"), "{m}");
                assert!(!m.contains("cannot open the scan root"), "{m}");
                assert!(!m.contains("links are not followed"), "{m}");
            }
        }
        // The detail (the system text for that code) is what goes in parentheses.
        let detail = io(32).to_string();
        assert!(
            StartError::Scan(ScanError::Root(io(32)))
                .message(&es)
                .contains(&format!("({detail})"))
        );
        // The scanner message itself is not in Spanish (it is translated by the interface).
        assert!(!ScanError::Panicked.to_string().contains("pánico"));
        // The notices save the typed error and are retranslated when changing languages.
        let n = Notice::ScanStart(StartError::Scan(ScanError::Root(io(2))));
        assert!(n.text(&es).starts_with("No se pudo empezar el análisis"));
        assert!(n.text(&en).starts_with("The scan could not start"));
    }
}
