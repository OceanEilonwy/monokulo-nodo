//! The web GUI: the device's screens in a browser on the home network, logged into with the
//! device passphrase (docs/ARCHITECTURE.md, "Web GUI").
//!
//! What is here is what more than one interface says about it: why a login was refused.

use nodo_api::ApiError;

use crate::words::counted;

/// How the web GUI is turned on from a shell, for a message that says it is off.
pub const TURN_ON_COMMAND: &str = "nodo-tui --ctl web-gui 1";

/// Why the helper refused a login with the device passphrase, in words for whoever typed it.
pub fn login_refusal(error: &ApiError) -> String {
    match error {
        ApiError::BadPassphrase => "That passphrase did not unlock this device. Try again.".to_owned(),
        ApiError::NotSetUp => "This Nodo has no device passphrase yet. Set it up on the device or over SSH first.".to_owned(),
        ApiError::Failed(text) if text == nodo_api::WEBGUI_TURNED_OFF => {
            format!("The web GUI is turned off. Turn it on over SSH with: {TURN_ON_COMMAND}")
        }
        ApiError::RateLimited { retry_after_secs } => format!("Too many wrong tries. Try again in {}.", counted(*retry_after_secs, "second")),
        other => format!("Could not log in: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockBackend, MOCK_PASSPHRASE};
    use nodo_api::{Backend, Request, Response, Secret};
    use std::sync::atomic::Ordering;

    fn login(mock: &mut MockBackend, passphrase: &str) -> Result<Response, ApiError> {
        mock.call(Request::WebGuiLogin { passphrase: Secret::new(passphrase) })
    }

    #[test]
    fn each_refusal_of_a_login_has_its_own_words() {
        // The web GUI's own socket, as the mock plays it.
        let mut mock = MockBackend::new();
        mock.knobs.web_socket = true;
        let off = login(&mut mock, MOCK_PASSPHRASE).expect_err("it is off");
        assert_eq!(login_refusal(&off), "The web GUI is turned off. Turn it on over SSH with: nodo-tui --ctl web-gui 1");

        mock.web_gui.enabled.store(true, Ordering::SeqCst);
        let wrong = login(&mut mock, "not it").expect_err("a wrong passphrase");
        assert_eq!(login_refusal(&wrong), "That passphrase did not unlock this device. Try again.");

        mock.setup_needed = true;
        let new = login(&mut mock, MOCK_PASSPHRASE).expect_err("no passphrase yet");
        assert!(login_refusal(&new).contains("no device passphrase yet"), "{new:?}");

        assert_eq!(login_refusal(&ApiError::RateLimited { retry_after_secs: 30 }), "Too many wrong tries. Try again in 30 seconds.");
        assert_eq!(login_refusal(&ApiError::Denied), "Could not log in: this user may not perform that action");
    }

    #[test]
    fn the_mock_plays_the_gate_of_the_web_guis_socket() {
        let mut mock = MockBackend::new();
        mock.knobs.web_socket = true;
        mock.web_gui.enabled.store(true, Ordering::SeqCst);
        let status = |mock: &mut MockBackend| match mock.call(Request::WebGuiStatus) {
            Ok(Response::WebGui(status)) => status,
            other => panic!("{other:?}"),
        };
        // Closed: on or off, and nothing else. Every other request is refused.
        let closed = status(&mut mock);
        assert!(closed.enabled && !closed.open && closed.url.is_none());
        assert_eq!(mock.call(Request::ListServices), Err(ApiError::web_login_required()));
        assert_eq!(mock.call(Request::WebGuiLogout), Err(ApiError::web_login_required()));

        // Logging in unlocks the device and opens the gate.
        assert!(mock.services.iter().any(|svc| svc.state == nodo_api::ServiceState::Locked));
        assert!(matches!(login(&mut mock, MOCK_PASSPHRASE), Ok(Response::WebGui(status)) if status.open));
        assert!(mock.services.iter().all(|svc| svc.state != nodo_api::ServiceState::Locked));
        let open = status(&mut mock);
        assert_eq!((open.open, open.url.as_deref()), (true, Some(nodo_api::WEBGUI_URL)));
        assert!(mock.call(Request::ListServices).is_ok());
        // Its own switch is never taken from this socket.
        assert_eq!(mock.call(Request::SetWebGui(false)), Err(ApiError::Denied));

        assert_eq!(mock.call(Request::WebGuiLogout), Ok(Response::Ok));
        assert_eq!(mock.call(Request::ListServices), Err(ApiError::web_login_required()));
        assert_eq!(*mock.web_gui.log.lock().expect("the record"), ["login", "logout"]);

        // Another connection to the same device shares the gate.
        let mut other = MockBackend::new();
        other.knobs.web_socket = true;
        other.web_gui = mock.web_gui.clone();
        assert!(login(&mut mock, MOCK_PASSPHRASE).is_ok());
        assert!(other.call(Request::ListServices).is_ok());
    }

    #[test]
    fn the_main_socket_switches_it_and_never_logs_in() {
        let mut mock = MockBackend::new();
        assert_eq!(login(&mut mock, MOCK_PASSPHRASE), Err(ApiError::Denied));
        mock.setup_needed = true;
        assert_eq!(mock.call(Request::SetWebGui(true)), Err(ApiError::NotSetUp));
        mock.setup_needed = false;
        assert!(matches!(mock.call(Request::SetWebGui(true)), Ok(Response::WebGui(status)) if status.enabled && status.url.is_some() && !status.open));
        mock.knobs.web_gui_waiting = true;
        assert!(matches!(mock.call(Request::WebGuiStatus), Ok(Response::WebGui(status)) if status.enabled && status.waiting && status.url.is_none()));
        assert!(matches!(mock.call(Request::SetWebGui(false)), Ok(Response::WebGui(status)) if !status.enabled && !status.waiting));
    }
}
