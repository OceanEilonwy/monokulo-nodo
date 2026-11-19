//! The web GUI's access: nothing until someone has logged in with the device passphrase, then
//! what an owner on SSH may do, less the few things that stay with SSH.
//!
//! nodo-web-gui runs unprivileged as its own account, behind Caddy, and connects on a socket of
//! its own. Anyone on the home network can reach its login page, and a bug in it is reachable from
//! there, so the helper, not the web process, decides what it may do:
//!
//! - While the gate is closed only `WebGuiLogin` and `WebGuiStatus` are answered, and the status
//!   then says only whether the web GUI is on. Every other request, reads included, is refused
//!   with [`ApiError::web_login_required`]: a web process nobody has logged into can do nothing.
//! - `WebGuiLogin` checks the device passphrase with the unlock's growing delay, but under its own
//!   count and lock ([`WebGate::failed_logins`]). Guesses at the login page therefore never hold
//!   the lock the owner's own unlock over SSH waits on, and neither count raises the other. A
//!   right passphrase also unlocks a locked device, since it is the same secret.
//! - The gate closes on `WebGuiLogout`, [`Timing::web_session`] after the last login,
//!   [`Timing::web_idle`] after the last request finished with none still running, when the web
//!   GUI is turned off, and with the helper itself: it lives only in memory.
//! - While open, the socket takes what the main socket takes from an owner, except the requests
//!   that only root sends, the screen's own lock and unlock, and `SetWebGui`: turning the web GUI
//!   on or off stays an act over SSH.
//!
//! The choice itself is `web-gui.toml` beside the exposure state. Backups do not carry it, so a
//! restored device starts with the web GUI off.
//!
//! [`Timing::web_session`]: crate::helper::Timing::web_session
//! [`Timing::web_idle`]: crate::helper::Timing::web_idle

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use nodo_api::{ApiError, EncryptionState, Request, Response, Secret, WebGuiStatus};
use serde::{Deserialize, Serialize};

use crate::helper::{lock, Helper, Timing};
use crate::system::{System, WriteOptions};

/// Whether the web GUI is logged in, and the failed logins since the last good one.
#[derive(Default)]
pub struct WebGate {
    state: Mutex<GateState>,
    /// Counts consecutive failed logins, apart from `Helper::failed_unlocks`. Held across the
    /// delay, like that one, so parallel guesses wait in line and each pays for its own.
    pub(crate) failed_logins: Mutex<u32>,
}

#[derive(Default)]
struct GateState {
    /// When the last login succeeded, while the gate is open.
    logged_in: Option<Instant>,
    /// When the last admitted request finished.
    last_done: Option<Instant>,
    /// Admitted requests still running. One of them keeps the gate open past the idle time, so
    /// a long backup cannot close it under itself.
    in_flight: u32,
}

impl GateState {
    /// Whether the gate is open now. An expired one is closed for good, until the next login.
    fn check(&mut self, timing: &Timing) -> bool {
        let within_session = self.logged_in.is_some_and(|at| at.elapsed() < timing.web_session);
        let active = self.in_flight > 0 || self.last_done.is_some_and(|at| at.elapsed() < timing.web_idle);
        if !(within_session && active) {
            self.logged_in = None;
        }
        self.logged_in.is_some()
    }
}

/// One admitted request. Dropping it records that the request finished.
pub(crate) struct InFlight<'a>(&'a WebGate);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.in_flight = state.in_flight.saturating_sub(1);
        state.last_done = Some(Instant::now());
    }
}

impl WebGate {
    fn open(&self) {
        let mut state = lock(&self.state);
        let now = Instant::now();
        state.logged_in = Some(now);
        state.last_done = Some(now);
    }

    pub(crate) fn close(&self) {
        lock(&self.state).logged_in = None;
    }

    pub(crate) fn is_open(&self, timing: &Timing) -> bool {
        lock(&self.state).check(timing)
    }

    /// Lets one request through if the gate is open, counting it as running until the returned
    /// value drops.
    pub(crate) fn admit(&self, timing: &Timing) -> Option<InFlight<'_>> {
        let mut state = lock(&self.state);
        if !state.check(timing) {
            return None;
        }
        state.in_flight += 1;
        Some(InFlight(self))
    }
}

/// Requests the web GUI may never send, even logged in: those only root sends, the touchscreen's
/// own lock and unlock, and the web GUI's own switch.
fn never_from_web(request: &Request) -> bool {
    matches!(
        request,
        Request::LocalAddressChanged
            | Request::RunAutoUpdate
            | Request::RunSystemUpdate
            | Request::SetWebGui(_)
            | Request::ScreenUnlock { .. }
            | Request::ScreenLock
    )
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WebGuiFile {
    enabled: bool,
}

const STATE_FILE: WriteOptions = WriteOptions {
    mode: 0o644,
    group: None,
};

fn turned_off() -> ApiError {
    ApiError::Failed(nodo_api::WEBGUI_TURNED_OFF.to_owned())
}

impl<S: System> Helper<S> {
    pub(crate) fn web_gui_path(&self) -> PathBuf {
        self.paths.exposure_state.with_file_name("web-gui.toml")
    }

    /// The owner's saved choice. Off on a new device.
    pub(crate) fn web_gui_wanted(&self) -> Result<bool, ApiError> {
        let Some(bytes) = self.sys.read_file(&self.web_gui_path())? else {
            return Ok(false);
        };
        let file: WebGuiFile = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|t| toml::from_str(t).ok())
            .ok_or_else(|| ApiError::Failed("the web GUI setting is corrupt".to_owned()))?;
        Ok(file.enabled)
    }

    /// What the main socket and the screen are told, and the web GUI once it is logged in.
    /// `open` is the caller's own gate, so it is `false` for every caller but the web GUI.
    pub(crate) fn web_gui_status(&self, open: bool) -> WebGuiStatus {
        // A choice that cannot be read counts as off, as it does for the proxy.
        let enabled = self.web_gui_wanted().unwrap_or(false);
        let url = if enabled {
            self.current_web_input().ok().and_then(|input| self.exposure.web_gui_url(&input))
        } else {
            None
        };
        WebGuiStatus { enabled, waiting: enabled && url.is_none(), url, open }
    }

    /// Turns the web GUI on or off: saves the choice, then has the proxy, the firewall and the
    /// web GUI's service follow it. Only the main socket reaches this.
    pub(crate) fn set_web_gui(&self, enabled: bool) -> Result<Response, ApiError> {
        if enabled {
            // Its login is the device passphrase, so there must be one.
            self.read_keystore()?;
        }
        let _guard = lock(&self.service_lock);
        let path = self.web_gui_path();
        let previous = self.sys.read_file(&path)?;
        let text = toml::to_string(&WebGuiFile { enabled }).map_err(|e| ApiError::Failed(e.to_string()))?;
        if let Some(dir) = path.parent() {
            self.sys.ensure_dir(dir, 0o755, None)?;
        }
        self.sys.atomic_write(&path, text.as_bytes(), STATE_FILE)?;
        if !enabled {
            self.web.close();
        }
        let apply = || {
            self.current_web_input()
                .map_err(|e| e.to_string())
                .and_then(|input| self.exposure.apply(&input))
        };
        match apply() {
            Ok(()) => Ok(Response::WebGui(self.web_gui_status(false))),
            Err(reason) if enabled => {
                // The saved choice must not claim a web GUI that never came up, and nothing of a
                // half-applied one may stay: no route and no listener while it is off.
                let _ = match &previous {
                    Some(bytes) => self.sys.atomic_write(&path, bytes, STATE_FILE),
                    None => self.sys.remove_file(&path),
                };
                if let Err(e) = apply() {
                    eprintln!("nodo-helper: withdrawing the web GUI after a failed start: {e}");
                }
                Err(ApiError::Failed(format!("could not turn the web GUI on: {reason}")))
            }
            // Off stays the saved choice whatever failed, so the next apply finishes the job.
            Err(reason) => Err(ApiError::Failed(format!(
                "the web GUI is off and nobody is logged in, but the web proxy could not be updated: {reason}"
            ))),
        }
    }

    /// At start, brings the web GUI's service to the saved choice, in case the two came apart
    /// while the helper was not running. The gate itself is always closed at start.
    pub fn sync_web_gui_service(&self) -> Result<(), ApiError> {
        let _guard = lock(&self.service_lock);
        let input = self.current_web_input()?;
        self.exposure.sync_web_gui_service(&input).map_err(ApiError::Failed)
    }

    /// Checks the device passphrase under the web GUI's own count, unlocks a locked device, and
    /// opens the gate.
    fn web_login(&self, passphrase: &Secret) -> Result<Response, ApiError> {
        let file = self.read_keystore()?;
        if !self.web_gui_wanted()? {
            return Err(turned_off());
        }
        let device_key = {
            let mut failures = lock(&self.web.failed_logins);
            self.verify_passphrase(&mut failures, passphrase, &file)?
        };
        // The same secret unlocks the device, so the owner is not asked for it twice. A problem
        // here does not undo the login: the passphrase was right, and the pages show what is
        // still locked and offer the unlock again, which reports it.
        if !matches!(self.encryption_state(), Ok(EncryptionState::Unlocked)) {
            if let Err(e) = self.unlock_with_key(&device_key) {
                eprintln!("nodo-helper: web login could not finish unlocking: {e}");
            }
        }
        self.web.open();
        // Turned off while the passphrase was being checked: closed again. Turning it off saves
        // the choice before it closes the gate, so one of the two always closes this login.
        if !self.web_gui_wanted().unwrap_or(false) {
            self.web.close();
            return Err(turned_off());
        }
        Ok(Response::WebGui(self.web_gui_status(true)))
    }

    /// Everything the web GUI's account sends comes through here.
    pub fn handle_web(&self, uid: u32, request: Request) -> Result<Response, ApiError> {
        match request {
            Request::WebGuiStatus => Ok(Response::WebGui(if self.web.is_open(&self.timing) {
                self.web_gui_status(true)
            } else {
                // Not logged in: on or off, and nothing about the device or its network.
                WebGuiStatus {
                    enabled: self.web_gui_wanted().unwrap_or(false),
                    url: None,
                    waiting: false,
                    open: false,
                }
            })),
            Request::WebGuiLogin { passphrase } => self.web_login(&passphrase),
            request => {
                // Secrets in a refused request are wiped when it drops here.
                let Some(_running) = self.web.admit(&self.timing) else {
                    return Err(ApiError::web_login_required());
                };
                match request {
                    Request::WebGuiLogout => {
                        self.web.close();
                        Ok(Response::Ok)
                    }
                    request if never_from_web(&request) => Err(ApiError::Denied),
                    // As on the main socket: the status, and the PIN with the device passphrase.
                    request @ (Request::ScreenStatus | Request::SetScreenPin { .. }) => {
                        self.screen_request_from_owner(request)
                    }
                    request => self.handle_trusted(uid, request),
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "webgui_tests.rs"]
mod tests;
