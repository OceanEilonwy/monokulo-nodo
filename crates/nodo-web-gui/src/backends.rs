//! Where a session's backend comes from: the helper on the device, the mock for development and
//! tests.
//!
//! `HelperBackend` keeps state between calls (site ids, the drive check, the update status), so
//! every session gets one of its own. It is `Send` but not `Sync`; the session keeps it behind a
//! mutex and uses it only from the blocking pool.

use std::path::PathBuf;

use nodo_api::{ApiError, Backend, Request, Response, WebGuiStatus};
use nodo_ui_core::exec::Data;
use nodo_ui_core::helper_backend::HelperBackend;
use nodo_ui_core::mock::{MockBackend, MOCK_PASSPHRASE};
use nodo_ui_core::secret::Secret;

/// Makes one backend per session and speaks for the helper's login gate, which is one for the
/// whole process whatever the number of sessions.
pub trait Backends: Send + Sync + 'static {
    /// Opens a backend for a new session and logs it in with the device passphrase. The helper
    /// verifies it, unlocks a locked device, and may take up to a minute after wrong guesses.
    fn login(&self, passphrase: &Secret) -> Result<Box<dyn Data>, ApiError>;

    /// Closes the helper's gate. Sent only when the last session has ended.
    fn logout(&self, backend: &mut dyn Data) -> Result<(), ApiError>;

    /// Whether the web GUI is on, where it answers and whether the gate is open. Asked before
    /// every page is read, since it is the one answer that says "log in again" as a value and
    /// not as the text of an error.
    fn status(&self, backend: &mut dyn Data) -> Result<WebGuiStatus, ApiError>;
}

/// The helper, on the socket only the web GUI's account may open.
pub struct HelperBackends {
    pub socket: PathBuf,
}

impl Backends for HelperBackends {
    fn login(&self, passphrase: &Secret) -> Result<Box<dyn Data>, ApiError> {
        let mut backend = HelperBackend::connect(&self.socket)
            .map_err(|error| ApiError::Failed(format!("the device's helper did not answer ({error})")))?;
        // The passphrase becomes plain text only here, as the request is built.
        backend.call(Request::WebGuiLogin { passphrase: nodo_api::Secret::new(passphrase.expose()) })?;
        Ok(Box::new(backend))
    }

    fn logout(&self, backend: &mut dyn Data) -> Result<(), ApiError> {
        match backend.call(Request::WebGuiLogout) {
            Ok(_) => Ok(()),
            // The gate is closed already, which is what was asked for.
            Err(error) if error.is_web_login_required() => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn status(&self, backend: &mut dyn Data) -> Result<WebGuiStatus, ApiError> {
        match backend.call(Request::WebGuiStatus)? {
            Response::WebGui(status) => Ok(status),
            other => Err(ApiError::Failed(format!("unexpected reply: {other:?}"))),
        }
    }
}

/// A fresh `MockBackend` per session, for `--mock` and tests. `MockBackend` does not answer the
/// web GUI's own requests, so the gate is played here: the passphrase is `MOCK_PASSPHRASE`, and
/// logging in unlocks the device as the helper's login does.
pub struct MockBackends {
    /// False leaves the device locked after login, to see the unlock card.
    pub unlock_on_login: bool,
}

impl Default for MockBackends {
    fn default() -> Self {
        Self { unlock_on_login: true }
    }
}

impl Backends for MockBackends {
    fn login(&self, passphrase: &Secret) -> Result<Box<dyn Data>, ApiError> {
        if passphrase.expose() != MOCK_PASSPHRASE {
            return Err(ApiError::BadPassphrase);
        }
        let mut backend = MockBackend::new();
        if self.unlock_on_login {
            backend.call(Request::Unlock { passphrase: nodo_api::Secret::new(passphrase.expose()) })?;
        }
        Ok(Box::new(backend))
    }

    fn logout(&self, _backend: &mut dyn Data) -> Result<(), ApiError> {
        Ok(())
    }

    fn status(&self, _backend: &mut dyn Data) -> Result<WebGuiStatus, ApiError> {
        Ok(WebGuiStatus { enabled: true, url: Some(nodo_api::WEBGUI_URL.to_owned()), waiting: false, open: true })
    }
}
