//! The web GUI: the device's screens in a browser on the home network, logged into with the
//! device passphrase (docs/ARCHITECTURE.md, "Web GUI").
//!
//! It is off until the owner turns it on, and that is done over SSH or on the device's own
//! terminal, never from the web GUI itself or the touchscreen: the helper refuses the switch
//! there. [`WebGui`] is the page that explains it and holds the switch. Turning it on asks
//! first, since from then on the device passphrase can be tried from the home network; turning
//! it off does not ask.
//!
//! Whether it is on and where it answers is in the device's `Snapshot` (`web_gui`), read with
//! everything else, so the switch is followed by a fresh read and needs no load of its own.

use nodo_api::{ApiError, WebGuiStatus};

use crate::flows::{Confirm, Notice, Question, Step};
use crate::model::Snapshot;
use crate::words::counted;
use crate::{Effect, Outcome};

/// How the web GUI is turned on from a shell, for a message that says it is off.
pub const TURN_ON_COMMAND: &str = "nodo-tui --ctl web-gui 1";

/// What it is, a line each.
pub const ABOUT: [&str; 3] = [
    "The same pages as here, in a browser on your home network.",
    "You log in with the device passphrase. A login ends after 15 minutes without use.",
    "It is never on the internet or on Tor, and it is off until you turn it on here.",
];

/// What turning it on means for the device's safety, a line each.
pub const SECURITY: [&str; 2] = [
    "While it is on, anyone on your home network can try the device passphrase at its login page. Until now that took an SSH key.",
    "Wrong tries are slowed down, up to a minute each. The pages can add SSH keys, so the passphrase alone then leads to a shell.",
];

/// Said where it cannot be turned on yet.
pub const NEEDS_PASSPHRASE: &str = "Set up the device passphrase first: the web GUI logs in with it.";

/// How a browser comes to trust the address, a line each: the local CA's root, as for local
/// sites. `trust_url` is the page that offers the root, while it is served.
pub fn trust_lines(trust_url: Option<&str>) -> Vec<String> {
    let mut lines = vec!["The address has a certificate from this Nodo's own local CA, as local sites do.".to_owned()];
    lines.push(match trust_url {
        Some(url) => format!("A browser warns until its device trusts that CA: open {url} there and follow its steps."),
        None => "A browser warns until its device trusts that CA. The page that offers it is served once the web GUI is on.".to_owned(),
    });
    lines.push("Install the root only if that page shows this same fingerprint.".to_owned());
    lines
}

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

/// The question before the web GUI is turned on. It starts on Cancel.
pub fn ask_turn_on() -> Question {
    Question::careful("Turn the web GUI on?", SECURITY.iter().map(|line| (*line).to_owned()).collect(), "Turn it on", Confirm::WebGuiOn)
}

/// The page with the web GUI's switch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebGui {
    /// The switch was sent and has not answered yet: nothing else is taken meanwhile.
    pub busy: bool,
}

impl WebGui {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether it is on, as the device last said. `None` when it did not say.
    pub fn status(snapshot: &Snapshot) -> Option<&WebGuiStatus> {
        snapshot.web_gui.as_ref()
    }

    /// The words on the switch.
    pub fn switch_label(snapshot: &Snapshot) -> &'static str {
        if Self::status(snapshot).is_some_and(|status| status.enabled) { "Turn it off" } else { "Turn it on…" }
    }

    /// The switch. Off needs no question and ends every browser's login. On is refused before
    /// the device passphrase is set up, and asks first.
    pub fn toggle(&mut self, snapshot: &Snapshot) -> Step {
        if self.busy {
            return Step::Stay;
        }
        if Self::status(snapshot).is_some_and(|status| status.enabled) {
            self.busy = true;
            return Step::Run(vec![Effect::SetWebGui(false)]);
        }
        if snapshot.overview.setup_needed {
            return Step::Failed(Notice::error(NEEDS_PASSPHRASE));
        }
        Step::Ask(ask_turn_on())
    }

    /// The owner confirmed turning it on: it shows as under way.
    pub fn confirmed(&mut self, confirm: &Confirm) {
        if matches!(confirm, Confirm::WebGuiOn) {
            self.busy = true;
        }
    }

    /// What the switch came to. The device is read again by the effect itself.
    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        let Outcome::WebGuiSet { enabled, result } = outcome else {
            return Step::Stay;
        };
        self.busy = false;
        match (result, enabled) {
            (Ok(status), true) => Step::Done(Notice::ok(match &status.url {
                Some(url) => format!("The web GUI is on at {url}."),
                None => "The web GUI is on. It waits for the Nodo's address on the home network.".to_owned(),
            })),
            (Ok(_), false) => Step::Done(Notice::ok("The web GUI is off. Every browser was logged out.")),
            (Err(ApiError::NotSetUp), _) => Step::Failed(Notice::error(NEEDS_PASSPHRASE)),
            (Err(error), true) => Step::Failed(Notice::error(format!("The web GUI was not turned on: {error}"))),
            (Err(error), false) => Step::Failed(Notice::error(format!("The web GUI was not turned off: {error}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::{MockBackend, MOCK_PASSPHRASE};
    use nodo_api::{Backend, Request, Response, Secret};
    use std::sync::atomic::Ordering;

    fn login(mock: &mut MockBackend, passphrase: &str) -> Result<Response, ApiError> {
        mock.call(Request::WebGuiLogin { passphrase: Secret::new(passphrase) })
    }

    /// Runs a step's effects and gives what the flow makes of the results, with the device as
    /// read afterwards.
    fn run(mock: &mut MockBackend, flow: &mut WebGui, step: Step) -> (Vec<Step>, Snapshot) {
        let mut said = Vec::new();
        let mut snapshot = None;
        for effect in step.effects() {
            for outcome in execute_effect(mock, effect) {
                if let Outcome::Refreshed(fresh) = &outcome {
                    snapshot = Some((**fresh).clone());
                }
                said.push(flow.apply(&outcome));
            }
        }
        said.retain(|step| *step != Step::Stay);
        (said, snapshot.expect("the device is read again"))
    }

    #[test]
    fn turning_it_on_asks_first_and_says_where_it_answers() {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        assert_eq!(WebGui::status(&snapshot).map(|status| status.enabled), Some(false), "off on a new device");
        assert_eq!(WebGui::switch_label(&snapshot), "Turn it on…");
        let mut flow = WebGui::new();
        let Step::Ask(question) = flow.toggle(&snapshot) else { panic!("it asks first") };
        assert!(question.cancel_is_default && question.confirm == Confirm::WebGuiOn);
        assert_eq!((question.title.as_str(), question.confirm_label.as_str()), ("Turn the web GUI on?", "Turn it on"));
        assert!(question.body[0].contains("anyone on your home network can try the device passphrase"));
        assert!(!mock.calls.iter().any(|call| matches!(call, Request::SetWebGui(_))), "nothing was done yet");

        flow.confirmed(&question.confirm);
        assert!(flow.busy);
        assert_eq!(flow.toggle(&snapshot), Step::Stay, "nothing else while it is under way");
        let (said, snapshot) = run(&mut mock, &mut flow, question.confirm.step(&snapshot));
        assert_eq!(said, [Step::Done(Notice::ok("The web GUI is on at https://nodo.local."))]);
        assert!(!flow.busy && mock.calls.contains(&Request::SetWebGui(true)));
        assert_eq!(WebGui::status(&snapshot).and_then(|status| status.url.as_deref()), Some("https://nodo.local"));
        assert_eq!(WebGui::switch_label(&snapshot), "Turn it off");
    }

    #[test]
    fn turning_it_off_does_not_ask_and_a_device_without_an_address_waits() {
        let mut mock = MockBackend::new();
        mock.knobs.web_gui_waiting = true;
        mock.web_gui.enabled.store(true, Ordering::SeqCst);
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        assert!(WebGui::status(&snapshot).is_some_and(|status| status.waiting && status.url.is_none()));
        let mut flow = WebGui::new();
        let step = flow.toggle(&snapshot);
        assert_eq!(step, Step::Run(vec![Effect::SetWebGui(false)]));
        let (said, snapshot) = run(&mut mock, &mut flow, step);
        assert_eq!(said, [Step::Done(Notice::ok("The web GUI is off. Every browser was logged out."))]);
        assert_eq!(WebGui::status(&snapshot).map(|status| status.enabled), Some(false));

        // On again, still without an address: it says that it waits.
        let (said, _) = run(&mut mock, &mut flow, Confirm::WebGuiOn.step(&snapshot));
        assert_eq!(said, [Step::Done(Notice::ok("The web GUI is on. It waits for the Nodo's address on the home network."))]);
    }

    #[test]
    fn it_is_refused_before_the_device_passphrase_is_set_up() {
        let mut mock = MockBackend::new();
        mock.setup_needed = true;
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let mut flow = WebGui::new();
        assert_eq!(flow.toggle(&snapshot), Step::Failed(Notice::error(NEEDS_PASSPHRASE)));
        assert!(!mock.calls.iter().any(|call| matches!(call, Request::SetWebGui(_))));
        // The helper refuses it too, should it be asked all the same.
        let (said, _) = run(&mut mock, &mut flow, Confirm::WebGuiOn.step(&snapshot));
        assert_eq!(said, [Step::Failed(Notice::error(NEEDS_PASSPHRASE))]);
        // Any other failure is said with the helper's words.
        let refused = Outcome::WebGuiSet { enabled: true, result: Err(ApiError::Failed("could not turn the web GUI on: no proxy".into())) };
        assert_eq!(
            flow.apply(&refused),
            Step::Failed(Notice::error("The web GUI was not turned on: could not turn the web GUI on: no proxy"))
        );
        assert_eq!(flow.apply(&Outcome::Info("hello".into())), Step::Stay);
    }

    #[test]
    fn the_trust_instructions_name_the_page_while_it_is_served() {
        assert!(trust_lines(Some("http://nodo.local"))[1].contains("open http://nodo.local there"));
        assert!(trust_lines(None)[1].contains("served once the web GUI is on"));
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
