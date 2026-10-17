//! Optional update checking (disabled by default) as seen by the interface: when it is requested, how
//! the response is handled, and what is shown. Network access is in `tesela-update`; this module decides when to request it.
//!
//! Guarantees (with evidence):
//!
//! * No request is made unless the setting is enabled. When enabled, at most one request is made every 24 hours; the attempt
//!   is recorded when it starts, whether it succeeds or fails.
//! * Demo and measurement tools (`--demo`, `--idle-memory`, `--bench-hover`, `--screenshot`) are never checked. They do not
//!   receive the transport, whether update checking is enabled or disabled.
//! * The request runs on its own thread, so the interface never waits. Failures are ignored; at most, one line appears in
//!   Settings, and no update notice is shown.
//! * Only the latest version name is fetched. No installer or other file is downloaded or run. "See what's new" opens the
//!   page for that version in the browser, using a URL Tesela builds from the validated version.

use crate::app::{TransportRef, UpdatePrefs};
use crossbeam_channel::{Receiver, TryRecvError};
use std::sync::Arc;
use tesela_update::{CheckError, Transport, Version};

/// What the interface shows about the check in this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateView {
    /// Is adjustment offered? Not in the demonstration or the measurement tools.
    pub available: bool,
    /// The new version to report, if there is one and it has not been discarded.
    pub notice: Option<Version>,
    /// Last check failed (only with setting enabled) - a line in Settings.
    pub failed: bool,
}

/// Check status: transportation, in-flight request and if the last one failed.
pub(crate) struct Updater {
    /// `None` in the demo, in the measurement tools and when the executable does not pass transport.
    transport: Option<Arc<dyn Transport>>,
    rx: Option<Receiver<Result<Version, CheckError>>>,
    failed: bool,
    /// How a page opens in the browser (tests replace it).
    opener: Box<dyn Fn(&str)>,
}

impl Updater {
    /// `allowed` is false in the demo and measurement tools: then the transport is discarded.
    pub fn new(transport: Option<TransportRef>, allowed: bool) -> Updater {
        Updater {
            transport: transport.filter(|_| allowed).map(|t| t.0),
            rx: None,
            failed: false,
            opener: Box::new(crate::platform::open_url_async),
        }
    }

    /// The request begins, in a thread, if there is transport, the setting is activated, it is time and there is no other in flight.
    /// Returns `true` if it was started (the caller notes the attempt).
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

    /// Pick up the response if it has already arrived. Returns `true` if you changed something that is saved.
    pub fn poll(&mut self, prefs: &mut UpdatePrefs) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        let answer = match rx.try_recv() {
            Err(TryRecvError::Empty) => return false,
            Ok(answer) => Some(answer),
            // The thread died without answering: it's one more bug.
            Err(TryRecvError::Disconnected) => None,
        };
        self.rx = None;
        // If it was disabled in the meantime, the response is forgotten.
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

    /// The user changed the setting: deactivating it forgets whatever was in flight and the previous fault.
    pub fn settings_changed(&mut self, on: bool) {
        if !on {
            self.rx = None;
            self.failed = false;
        }
    }

    /// What is currently displayed.
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

    /// Open the `v` version page in the browser. The URL is built by Tesela; none comes from the answer of
    /// GitHub. Without verification available (demo, measurement tools) it does nothing.
    pub fn open_release(&self, v: Version) {
        if self.transport.is_some() {
            (self.opener)(&tesela_update::release_url(v));
        }
    }

    /// Marks the last check as failed (the layout tests).
    #[cfg(test)]
    pub fn force_failed(&mut self) {
        self.failed = true;
    }

    /// Replaces the way you open the browser (tests don't open anything).
    #[cfg(test)]
    pub fn set_opener(&mut self, opener: Box<dyn Fn(&str)>) {
        self.opener = opener;
    }
}
