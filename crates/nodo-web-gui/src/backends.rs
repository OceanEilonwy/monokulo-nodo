//! Where a session's backend comes from: the helper on the device, the mock for development and
//! tests.
//!
//! `HelperBackend` keeps state between calls (site ids, the drive check, the update status), so
//! every session gets one of its own. It is `Send` but not `Sync`; the session keeps it behind a
//! mutex and uses it only from the blocking pool.
//!
//! The helper's login gate is one for the whole process whatever the number of sessions. It is
//! spoken to with the helper's own requests ([`login`], [`logout`], [`status`]) over whichever
//! backend [`Backends::connect`] gave, so the mock is asked exactly what the device is.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use nodo_api::{ApiError, Request, Response, WebGuiStatus};
use nodo_ui_core::exec::Data;
use nodo_ui_core::helper_backend::HelperBackend;
use nodo_ui_core::mock::{MockBackend, MockWebGui};
use nodo_ui_core::secret::Secret;

/// Makes one backend per session: a connection to the socket only the web GUI's account may
/// open, on which nothing is answered before a login.
pub trait Backends: Send + Sync + 'static {
    fn connect(&self) -> Result<Box<dyn Data>, ApiError>;
}

/// Opens a backend for a new session and logs it in with the device passphrase. The helper
/// verifies it, unlocks a locked device, and may take up to a minute after wrong guesses.
pub fn login(backends: &dyn Backends, passphrase: &Secret) -> Result<Box<dyn Data>, ApiError> {
    let mut backend = backends.connect()?;
    // The passphrase becomes plain text only here, as the request is built.
    backend.call(Request::WebGuiLogin { passphrase: nodo_api::Secret::new(passphrase.expose()) })?;
    Ok(backend)
}

/// Closes the helper's gate. Sent only when the last session has ended.
pub fn logout(backend: &mut dyn Data) -> Result<(), ApiError> {
    match backend.call(Request::WebGuiLogout) {
        Ok(_) => Ok(()),
        // The gate is closed already, which is what was asked for.
        Err(error) if error.is_web_login_required() => Ok(()),
        Err(error) => Err(error),
    }
}

/// Whether the web GUI is on, where it answers and whether the gate is open. Asked before every
/// page is read, since it is the one answer that says "log in again" as a value and not as the
/// text of an error.
pub fn status(backend: &mut dyn Data) -> Result<WebGuiStatus, ApiError> {
    match backend.call(Request::WebGuiStatus)? {
        Response::WebGui(status) => Ok(status),
        other => Err(ApiError::Failed(format!("unexpected reply: {other:?}"))),
    }
}

/// The helper, on the socket only the web GUI's account may open.
pub struct HelperBackends {
    pub socket: PathBuf,
}

impl Backends for HelperBackends {
    fn connect(&self) -> Result<Box<dyn Data>, ApiError> {
        let backend = HelperBackend::connect(&self.socket)
            .map_err(|error| ApiError::Failed(format!("the device's helper did not answer ({error})")))?;
        Ok(Box::new(backend))
    }
}

/// A fresh `MockBackend` per session, for `--mock`, playing the web GUI's socket of one device
/// whose web GUI is on: the passphrase is `MOCK_PASSPHRASE`, and logging in unlocks the device
/// as the helper's login does.
#[derive(Default)]
pub struct MockBackends {
    /// The device's one gate, shared by every session's backend.
    pub gate: Arc<MockWebGui>,
}

impl Backends for MockBackends {
    fn connect(&self) -> Result<Box<dyn Data>, ApiError> {
        let mut backend = MockBackend::new();
        backend.knobs.web_socket = true;
        backend.web_gui = self.gate.clone();
        backend.web_gui.enabled.store(true, Ordering::SeqCst);
        // One update that grows locked storage, so that the page asking for the passphrase can
        // be reached under `--mock`; the mock's own updates have none.
        if let Some(item) = backend.updates.iter_mut().find(|item| item.name == "wallet-relay") {
            item.grows = Some("256 MB to 512 MB".to_owned());
        }
        // A suite of apps and an app's setup page, which the mock has none of by itself, so
        // those pages can be reached too.
        backend.bundles.insert("sample-tool-01".to_owned(), nodo_ui_core::mock::sample_bundle_review());
        backend.owner_views.insert("moneropay".to_owned(), ("Setup".to_owned(), nodo_ui_core::mock::sample_owner_page()));
        Ok(Box::new(backend))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodo_ui_core::mock::MOCK_PASSPHRASE;

    #[test]
    fn the_mock_device_is_logged_into_with_the_helpers_own_requests() {
        let backends = MockBackends::default();
        assert_eq!(login(&backends, &Secret::new("wrong".to_owned())).err(), Some(ApiError::BadPassphrase));
        let mut backend = login(&backends, &Secret::new(MOCK_PASSPHRASE.to_owned())).expect("the passphrase");
        assert!(status(&mut *backend).expect("the status").open);
        // A second session's backend finds the same gate open, and one logout closes it for both.
        let mut other = backends.connect().expect("the mock");
        assert!(status(&mut *other).expect("the status").open);
        assert_eq!(logout(&mut *backend), Ok(()));
        assert!(!status(&mut *other).expect("the status").open);
        assert_eq!(logout(&mut *other), Ok(()), "closed already, which is what was asked for");
    }
}
