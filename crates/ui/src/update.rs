//! La comprobación de actualizaciones OPCIONAL (desactivada por defecto) vista desde la interfaz: cuándo se pide, qué
//! se hace con la respuesta y qué se enseña. Todo lo que toca la red está en `tesela-update`; aquí solo se decide.
//!
//! Garantías (con pruebas):
//!
//! * Sin el ajuste activado no se hace NINGUNA petición; con él, una como mucho cada 24 horas (el intento se anota al
//!   empezar, salga bien o mal).
//! * La demostración y las herramientas de medición (`--demo`, `--idle-memory`, `--bench-hover`, `--screenshot`) no
//!   comprueban nunca: ni siquiera reciben el transporte, estén o no activadas.
//! * La petición va en un hilo propio: la interfaz nunca espera. Cualquier fallo se ignora (como mucho, una línea en
//!   Ajustes) y el aviso no sale.
//! * Solo se descarga el nombre de la última versión. Nunca se descarga ni se ejecuta nada: "Ver novedades" abre en el
//!   navegador la página de esa versión, con una URL que construye Tesela a partir de la versión validada.

use crate::app::{TransportRef, UpdatePrefs};
use crossbeam_channel::{Receiver, TryRecvError};
use std::sync::Arc;
use tesela_update::{CheckError, Transport, Version};

/// Lo que la interfaz enseña de la comprobación en este frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateView {
    /// ¿Se ofrece el ajuste? No en la demostración ni en las herramientas de medición.
    pub available: bool,
    /// La versión nueva de la que avisar, si la hay y no se descartó.
    pub notice: Option<Version>,
    /// La última comprobación falló (solo con el ajuste activado): una línea en Ajustes.
    pub failed: bool,
}

/// Estado de la comprobación: el transporte, la petición en vuelo y si la última falló.
pub(crate) struct Updater {
    /// `None` en la demostración, en las herramientas de medición y cuando el ejecutable no pasa transporte.
    transport: Option<Arc<dyn Transport>>,
    rx: Option<Receiver<Result<Version, CheckError>>>,
    failed: bool,
    /// Cómo se abre una página en el navegador (las pruebas lo sustituyen).
    opener: Box<dyn Fn(&str)>,
}

impl Updater {
    /// `allowed` es falso en la demostración y en las herramientas de medición: entonces el transporte se descarta.
    pub fn new(transport: Option<TransportRef>, allowed: bool) -> Updater {
        Updater {
            transport: transport.filter(|_| allowed).map(|t| t.0),
            rx: None,
            failed: false,
            opener: Box::new(crate::platform::open_url_async),
        }
    }

    /// Empieza la petición, en un hilo, si hay transporte, el ajuste está activado, ya toca y no hay otra en vuelo.
    /// Devuelve `true` si la empezó (quien llama anota el intento).
    pub fn start_if_due(&mut self, ctx: &egui::Context, prefs: &UpdatePrefs, now: u64) -> bool {
        let Some(transport) = &self.transport else {
            return false;
        };
        if self.rx.is_some() || !tesela_update::is_due(prefs.check, prefs.last_check, now) {
            return false;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        let transport = transport.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("tesela-update-check".into())
            .spawn(move || {
                let _ = tx.send(tesela_update::fetch_latest(&*transport));
                ctx.request_repaint();
            });
        if spawned.is_err() {
            return false;
        }
        self.rx = Some(rx);
        self.failed = false;
        true
    }

    /// Recoge la respuesta si ya llegó. Devuelve `true` si cambió algo que se guarda.
    pub fn poll(&mut self, prefs: &mut UpdatePrefs) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        let answer = match rx.try_recv() {
            Err(TryRecvError::Empty) => return false,
            Ok(answer) => Some(answer),
            // El hilo murió sin contestar: es un fallo más.
            Err(TryRecvError::Disconnected) => None,
        };
        self.rx = None;
        // Si se desactivó mientras tanto, la respuesta se olvida.
        if !prefs.check {
            return false;
        }
        match answer {
            Some(Ok(latest)) => {
                self.failed = false;
                let changed = prefs.latest_seen != Some(latest);
                prefs.latest_seen = Some(latest);
                changed
            }
            _ => {
                self.failed = true;
                false
            }
        }
    }

    /// El usuario cambió el ajuste: al desactivarlo se olvida lo que estuviera en vuelo y el fallo anterior.
    pub fn settings_changed(&mut self, on: bool) {
        if !on {
            self.rx = None;
            self.failed = false;
        }
    }

    /// Lo que se enseña ahora.
    pub fn view(&self, prefs: &UpdatePrefs) -> UpdateView {
        let available = self.transport.is_some();
        UpdateView {
            available,
            notice: available
                .then(|| {
                    tesela_update::notice_for(
                        prefs.check,
                        prefs.latest_seen,
                        Version::current(),
                        prefs.dismissed,
                    )
                })
                .flatten(),
            failed: available && prefs.check && self.failed,
        }
    }

    /// Abre en el navegador la página de la versión `v`. La URL la construye Tesela; ninguna viene de la respuesta de
    /// GitHub. Sin comprobación disponible (demostración, herramientas de medición) no hace nada.
    pub fn open_release(&self, v: Version) {
        if self.transport.is_some() {
            (self.opener)(&tesela_update::release_url(v));
        }
    }

    /// Marca la última comprobación como fallida (las pruebas de disposición).
    #[cfg(test)]
    pub fn force_failed(&mut self) {
        self.failed = true;
    }

    /// Sustituye la forma de abrir el navegador (las pruebas no abren nada).
    #[cfg(test)]
    pub fn set_opener(&mut self, opener: Box<dyn Fn(&str)>) {
        self.opener = opener;
    }
}
