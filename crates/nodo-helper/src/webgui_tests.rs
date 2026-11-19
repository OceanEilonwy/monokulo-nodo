use super::*;
use crate::exposure::{ExposureApplier, WebInput};
use crate::fake::{FakeSystem, TestPolicy, ENCRYPTED, SHOP, TEST_PATHS};
use crate::helper::AllowedUids;
use crate::keys::KdfParams;
use nodo_api::{SiteId, WebStatus, WEBGUI_URL};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

const PASS: &str = "correct horse battery";
const OWNER: u32 = 1000;
const WEB: u32 = 4243;
const SCREEN: u32 = 4242;

/// Stands in for the web proxy: records every apply and whether the web GUI was on in it.
#[derive(Clone, Default)]
struct Recorder {
    applied: Arc<Mutex<Vec<bool>>>,
    synced: Arc<Mutex<Vec<bool>>>,
    /// Fails an apply that has the web GUI on, as a device without the service would.
    refuse_on: Arc<AtomicBool>,
    /// Fails every apply.
    refuse_all: Arc<AtomicBool>,
    /// No local address, so the web GUI waits.
    no_address: Arc<AtomicBool>,
}

impl ExposureApplier for Recorder {
    fn apply(&self, input: &WebInput) -> Result<(), String> {
        self.applied.lock().unwrap().push(input.web_gui);
        if self.refuse_all.load(Ordering::SeqCst) || (input.web_gui && self.refuse_on.load(Ordering::SeqCst)) {
            return Err("the proxy said no".to_owned());
        }
        Ok(())
    }
    fn web_gui_url(&self, input: &WebInput) -> Option<String> {
        (input.web_gui && !self.no_address.load(Ordering::SeqCst)).then(|| WEBGUI_URL.to_owned())
    }
    fn sync_web_gui_service(&self, input: &WebInput) -> Result<(), String> {
        self.synced.lock().unwrap().push(input.web_gui);
        Ok(())
    }
}

fn timing() -> Timing {
    Timing { health_attempts: 5, ..Timing::default() }
}

fn bare(applier: impl ExposureApplier + 'static, manifests: &[&str]) -> Helper<FakeSystem> {
    let sys = FakeSystem::default();
    for manifest in manifests {
        sys.add_manifest(manifest);
    }
    Helper::new(sys, AllowedUids::new([OWNER]), Box::new(TestPolicy), Box::new(applier))
        .with_paths(TEST_PATHS())
        .with_kdf(KdfParams { memory_kib: 64, iterations: 1, lanes: 1 })
        .with_timing(timing())
        .with_radios(crate::radio_tests::no_radios())
}

/// A set-up device with the web GUI turned on and nobody logged in.
fn turned_on() -> (Helper<FakeSystem>, Recorder) {
    let recorder = Recorder::default();
    let h = bare(recorder.clone(), &[SHOP]);
    h.init_device(&Secret::new(PASS)).unwrap();
    h.handle(OWNER, Request::SetWebGui(true)).unwrap();
    (h, recorder)
}

fn login(h: &Helper<FakeSystem>, passphrase: &str) -> Result<Response, ApiError> {
    h.handle_web(WEB, Request::WebGuiLogin { passphrase: Secret::new(passphrase) })
}

fn logged_in() -> (Helper<FakeSystem>, Recorder) {
    let (h, recorder) = turned_on();
    login(&h, PASS).unwrap();
    (h, recorder)
}

fn status(result: Result<Response, ApiError>) -> WebGuiStatus {
    match result {
        Ok(Response::WebGui(status)) => status,
        other => panic!("{other:?}"),
    }
}

fn site() -> SiteId {
    SiteId { package: "shop".into(), site: "web".into() }
}

/// A sample of what an owner may send, reads first. None of it may pass a closed gate.
fn owner_requests() -> Vec<Request> {
    vec![
        Request::ListServices,
        Request::DeviceOverview,
        Request::ListPackages,
        Request::ListSites,
        Request::ListSshKeys,
        Request::ListFeeds,
        Request::Health,
        Request::LogSources,
        Request::ReadLog { source: "shop".into(), max_lines: 5 },
        Request::ScreenStatus,
        Request::WebStatus,
        Request::StartService { id: "shop".into() },
        Request::StopService { id: "shop".into() },
        Request::Unlock { passphrase: Secret::new(PASS) },
        Request::ChangePassphrase { current: Secret::new(PASS), new: Secret::new("another long one") },
        Request::InitEncryption { passphrase: Secret::new(PASS) },
        Request::AddSshKey { line: "ssh-ed25519 AAAA attacker".into() },
        Request::RemoveSshKey { fingerprint: "x".into() },
        Request::SetExposure { site: site(), exposure: Default::default(), confirmation: None },
        Request::SetScreenPin { passphrase: Secret::new(PASS), pin: None },
        Request::ApplySettings { package: "shop".into(), changes: vec![] },
        Request::WebGuiLogout,
        Request::SetWebGui(false),
        Request::SetWebGui(true),
        Request::LocalAddressChanged,
        Request::RunAutoUpdate,
        Request::RunSystemUpdate,
        Request::ScreenLock,
        Request::ScreenUnlock { pin: Secret::new("000000") },
    ]
}

#[test]
fn a_closed_gate_answers_only_login_and_status() {
    let (h, recorder) = turned_on();
    h.sys.clear_calls();
    let applied = recorder.applied.lock().unwrap().len();
    for request in owner_requests() {
        let refused = h.handle_web(WEB, request.clone()).unwrap_err();
        assert!(refused.is_web_login_required(), "{request:?}: {refused:?}");
        assert_eq!(refused, ApiError::web_login_required());
    }
    assert!(h.sys.calls().is_empty(), "nothing was touched: {:?}", h.sys.calls());
    assert_eq!(recorder.applied.lock().unwrap().len(), applied, "and nothing applied");
    assert!(h.sys.sleeps().is_empty(), "no passphrase was even looked at");

    // The status says on or off and nothing else, whatever the device knows.
    let closed = status(h.handle_web(WEB, Request::WebGuiStatus));
    assert_eq!(closed, WebGuiStatus { enabled: true, url: None, waiting: false, open: false });
    // A wrong login is answered as wrong, and opens nothing.
    assert_eq!(login(&h, "nope nope nope"), Err(ApiError::BadPassphrase));
    assert!(h.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
}

#[test]
fn a_login_opens_the_gate_for_what_an_owner_may_do() {
    let (h, _) = turned_on();
    let opened = status(login(&h, PASS));
    assert_eq!(opened, WebGuiStatus { enabled: true, url: Some(WEBGUI_URL.into()), waiting: false, open: true });
    assert_eq!(status(h.handle_web(WEB, Request::WebGuiStatus)), opened);

    let Ok(Response::Services(list)) = h.handle_web(WEB, Request::ListServices) else { panic!() };
    assert_eq!(list[0].id, "shop");
    assert_eq!(h.handle_web(WEB, Request::StartService { id: "shop".into() }), Ok(Response::Ok));
    assert!(matches!(h.handle_web(WEB, Request::DeviceOverview), Ok(Response::Overview(_))));
    assert!(matches!(h.handle_web(WEB, Request::CpuStatus), Ok(Response::Cpu(_))));
    assert!(matches!(h.handle_web(WEB, Request::ListSshKeys), Ok(Response::SshKeys(_))));
    // The screen's status and PIN, as on the main socket: the PIN still needs the passphrase.
    assert!(matches!(h.handle_web(WEB, Request::ScreenStatus), Ok(Response::Screen(_))));
    let pin = |passphrase: &str| Request::SetScreenPin { passphrase: Secret::new(passphrase), pin: Some(Secret::new("482913")) };
    assert_eq!(h.handle_web(WEB, pin("nope nope nope")), Err(ApiError::BadPassphrase));
    assert_eq!(h.handle_web(WEB, pin(PASS)), Ok(Response::Ok));
    assert!(h.screen_status().pin_set);
    // Unlocking again is allowed, as it is over SSH.
    assert_eq!(h.handle_web(WEB, Request::Unlock { passphrase: Secret::new(PASS) }), Ok(Response::Ok));
}

#[test]
fn some_requests_never_come_from_the_web_gui_even_logged_in() {
    let (h, recorder) = logged_in();
    let applied = recorder.applied.lock().unwrap().len();
    for request in [
        Request::SetWebGui(false),
        Request::SetWebGui(true),
        Request::LocalAddressChanged,
        Request::RunAutoUpdate,
        Request::RunSystemUpdate,
        Request::ScreenLock,
        Request::ScreenUnlock { pin: Secret::new("000000") },
    ] {
        assert_eq!(h.handle_web(WEB, request.clone()), Err(ApiError::Denied), "{request:?}");
    }
    assert_eq!(recorder.applied.lock().unwrap().len(), applied);
    assert!(h.web_gui_wanted().unwrap(), "still on");
    assert!(h.web.is_open(&h.timing), "and still logged in");
    // Even an account that is also root on this socket: the socket decides, not the uid.
    assert_eq!(h.handle_web(0, Request::RunAutoUpdate), Err(ApiError::Denied));
    assert_eq!(h.handle_web(0, Request::LocalAddressChanged), Err(ApiError::Denied));
}

#[test]
fn logging_out_closes_the_gate_at_once() {
    let (h, _) = logged_in();
    assert_eq!(h.handle_web(WEB, Request::WebGuiLogout), Ok(Response::Ok));
    assert!(h.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
    assert!(!status(h.handle_web(WEB, Request::WebGuiStatus)).open);
    // Logging out twice is refused like everything else, and logging in again works.
    assert!(h.handle_web(WEB, Request::WebGuiLogout).unwrap_err().is_web_login_required());
    login(&h, PASS).unwrap();
    assert!(h.handle_web(WEB, Request::ListServices).is_ok());
}

#[test]
fn the_gate_closes_after_the_idle_time_and_every_request_restarts_it() {
    let (mut h, _) = turned_on();
    h.timing = Timing { web_idle: Duration::from_millis(400), ..timing() };
    login(&h, PASS).unwrap();
    // Requests closer together than the idle time keep it open well past one idle time.
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(50));
        assert!(h.handle_web(WEB, Request::ListServices).is_ok());
    }
    std::thread::sleep(Duration::from_millis(600));
    assert!(h.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
    assert!(!status(h.handle_web(WEB, Request::WebGuiStatus)).open);
    // Expired stays closed: a later request does not find it open again.
    assert!(h.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
    // Asking for the status is not activity, so polling it cannot hold the gate open.
    login(&h, PASS).unwrap();
    for _ in 0..6 {
        std::thread::sleep(Duration::from_millis(100));
        let _ = h.handle_web(WEB, Request::WebGuiStatus);
    }
    assert!(h.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
}

/// Holds a `WebStatus` request inside the proxy until the test lets it go.
struct Slow {
    entered: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl ExposureApplier for Slow {
    fn apply(&self, _: &WebInput) -> Result<(), String> {
        Ok(())
    }
    fn status(&self, _: &WebInput) -> Result<WebStatus, ApiError> {
        self.entered.lock().unwrap().send(()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        Err(ApiError::Failed("done waiting".to_owned()))
    }
}

#[test]
fn a_request_in_flight_keeps_the_gate_open_past_the_idle_time() {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let mut h = bare(Slow { entered: Mutex::new(entered_tx), release: Mutex::new(release_rx) }, &[SHOP]);
    h.timing = Timing { web_idle: Duration::from_millis(300), ..timing() };
    h.init_device(&Secret::new(PASS)).unwrap();
    h.handle(OWNER, Request::SetWebGui(true)).unwrap();
    login(&h, PASS).unwrap();

    let h = Arc::new(h);
    let long = {
        let h = h.clone();
        std::thread::spawn(move || h.handle_web(WEB, Request::WebStatus))
    };
    entered.recv().unwrap();
    // Three idle times pass with the long request still running, and nothing else sent.
    std::thread::sleep(Duration::from_millis(900));
    assert!(h.web.is_open(&h.timing), "a long backup must not close the gate under itself");
    release.send(()).unwrap();
    assert_eq!(long.join().unwrap(), Err(ApiError::Failed("done waiting".to_owned())));
    // The idle time counts from when it finished.
    assert!(h.handle_web(WEB, Request::ListServices).is_ok());
    std::thread::sleep(Duration::from_millis(500));
    assert!(h.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
}

#[test]
fn the_gate_closes_twelve_hours_after_the_last_login_however_busy() {
    assert_eq!(Timing::default().web_session, Duration::from_secs(12 * 60 * 60));
    assert_eq!(Timing::default().web_idle, Duration::from_secs(15 * 60));
    let (mut h, _) = turned_on();
    h.timing = Timing { web_session: Duration::from_millis(200), ..timing() };
    login(&h, PASS).unwrap();
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_millis(350) {
        // Activity all the way through does not extend it.
        let _ = h.handle_web(WEB, Request::ListServices);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(h.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
    // A new login starts a new twelve hours.
    login(&h, PASS).unwrap();
    assert!(h.handle_web(WEB, Request::ListServices).is_ok());
}

#[test]
fn web_logins_and_unlocks_count_their_failures_apart() {
    let (h, _) = turned_on();
    let unlock = |passphrase: &str| h.handle(OWNER, Request::Unlock { passphrase: Secret::new(passphrase) });
    let secs = |list: &[u64]| list.iter().map(|s| Duration::from_secs(*s)).collect::<Vec<_>>();

    // Three wrong guesses at the login page: the same growing delay as an unlock.
    for _ in 0..3 {
        assert_eq!(login(&h, "nope nope nope"), Err(ApiError::BadPassphrase));
    }
    assert_eq!(h.sys.sleeps(), secs(&[2, 4, 8]));
    assert_eq!(*lock(&h.web.failed_logins), 3);
    assert_eq!(*lock(&h.failed_unlocks), 0, "the owner's own unlock is not held up by them");

    // The owner's first wrong unlock pays the first delay, not the fourth.
    assert_eq!(unlock("nope nope nope"), Err(ApiError::BadPassphrase));
    assert_eq!(h.sys.sleeps().last(), Some(&Duration::from_secs(2)));
    assert_eq!(unlock("nope nope nope"), Err(ApiError::BadPassphrase));
    assert_eq!((*lock(&h.failed_unlocks), *lock(&h.web.failed_logins)), (2, 3));
    // And the next wrong login goes on from its own count.
    assert_eq!(login(&h, "nope nope nope"), Err(ApiError::BadPassphrase));
    assert_eq!(h.sys.sleeps().last(), Some(&Duration::from_secs(16)));

    // A good unlock clears only the unlock's count; a good login only the login's.
    unlock(PASS).unwrap();
    assert_eq!((*lock(&h.failed_unlocks), *lock(&h.web.failed_logins)), (0, 4));
    assert_eq!(unlock("nope nope nope"), Err(ApiError::BadPassphrase));
    login(&h, PASS).unwrap();
    assert_eq!((*lock(&h.failed_unlocks), *lock(&h.web.failed_logins)), (1, 0));
    assert_eq!(h.sys.sleeps().len(), 7, "right passphrases are never delayed");

    // Logged in, the web GUI's other passphrase checks are the owner's own and share the unlock's.
    let pin = Request::SetScreenPin { passphrase: Secret::new("nope nope nope"), pin: None };
    assert_eq!(h.handle_web(WEB, pin), Err(ApiError::BadPassphrase));
    assert_eq!((*lock(&h.failed_unlocks), *lock(&h.web.failed_logins)), (2, 0));
}

#[test]
fn the_login_delay_is_capped_like_the_unlocks() {
    let (h, _) = turned_on();
    for _ in 0..40 {
        let _ = login(&h, "nope nope nope");
    }
    assert_eq!(h.sys.sleeps().last(), Some(&Duration::from_secs(60)));
    assert!(!h.web.is_open(&h.timing));
}

const NOTES: &str = "api_version = 1\nid = \"notes\"\nversion = \"1\"\nexec = [\"a\"]\n[storage]\nencrypted = true\n";

/// A device with two encrypted volumes, locked as a reboot leaves it, with the web GUI on.
fn locked_device() -> Helper<FakeSystem> {
    let h = bare(Recorder::default(), &[ENCRYPTED, NOTES, SHOP]);
    h.init_device(&Secret::new(PASS)).unwrap();
    h.create_volume("vault", &Secret::new(PASS), 1 << 20).unwrap();
    h.create_volume("notes", &Secret::new(PASS), 1 << 20).unwrap();
    h.handle(OWNER, Request::SetWebGui(true)).unwrap();
    h.sys.reboot();
    *lock(&h.secrets) = None;
    assert_eq!(h.encryption_state(), Ok(EncryptionState::Locked));
    h
}

#[test]
fn a_login_unlocks_a_locked_device_without_touching_the_unlock_count() {
    let h = locked_device();
    assert_eq!(login(&h, "nope nope nope"), Err(ApiError::BadPassphrase));
    assert_eq!(h.encryption_state(), Ok(EncryptionState::Locked), "a wrong login unlocks nothing");
    assert!(!h.sys.mounted("/run/nodo/volumes/vault"));

    h.sys.clear_calls();
    let opened = status(login(&h, PASS));
    assert!(opened.open);
    assert!(h.sys.mounted("/run/nodo/volumes/vault") && h.sys.mounted("/run/nodo/volumes/notes"));
    assert_eq!(h.encryption_state(), Ok(EncryptionState::Unlocked));
    assert!(h.secrets_key().is_some(), "sealed secrets open again");
    assert_eq!(*lock(&h.failed_unlocks), 0);
    assert_eq!(h.sys.sleeps(), [Duration::from_secs(2)], "only the one wrong login was delayed");
    let calls = h.sys.calls().join("\n");
    assert!(!calls.contains(PASS), "{calls}");

    // Already unlocked: the next login opens nothing again.
    h.handle_web(WEB, Request::WebGuiLogout).unwrap();
    h.sys.clear_calls();
    login(&h, PASS).unwrap();
    assert!(!h.sys.calls().iter().any(|c| c.starts_with("luks")), "{:?}", h.sys.calls());
}

#[test]
fn a_login_stands_even_when_a_volume_will_not_open() {
    let h = locked_device();
    h.sys.fail("luks open /v/notes.luks");
    assert!(status(login(&h, PASS)).open, "the passphrase was right");
    assert!(h.sys.mounted("/run/nodo/volumes/vault"));
    assert!(!h.sys.mounted("/run/nodo/volumes/notes"));
    // The pages can then offer the unlock again, which says what is wrong.
    let Err(ApiError::Failed(message)) = h.handle_web(WEB, Request::Unlock { passphrase: Secret::new(PASS) }) else {
        panic!()
    };
    assert!(message.contains("opened 1 of 2"), "{message}");
}

#[test]
fn the_web_gui_cannot_be_turned_on_before_setup_or_logged_into_while_off() {
    let recorder = Recorder::default();
    let h = bare(recorder.clone(), &[SHOP]);
    // No passphrase yet: no web GUI, and no login.
    assert_eq!(h.handle(OWNER, Request::SetWebGui(true)), Err(ApiError::NotSetUp));
    assert!(recorder.applied.lock().unwrap().is_empty());
    assert_eq!(h.sys.get("/s/web-gui.toml"), None);
    assert_eq!(login(&h, PASS), Err(ApiError::NotSetUp));
    assert_eq!(status(h.handle(OWNER, Request::WebGuiStatus)), WebGuiStatus { enabled: false, url: None, waiting: false, open: false });
    // Turning it off needs nothing.
    assert!(h.handle(OWNER, Request::SetWebGui(false)).is_ok());

    // Set up but off: the right passphrase is refused without being checked.
    h.init_device(&Secret::new(PASS)).unwrap();
    assert_eq!(h.handle(OWNER, Request::SetWebGui(false)).map(status_of), Ok(false));
    let refused = login(&h, PASS).unwrap_err();
    assert!(matches!(&refused, ApiError::Failed(m) if m.contains("turned off")), "{refused:?}");
    let refused = login(&h, "nope nope nope").unwrap_err();
    assert!(matches!(&refused, ApiError::Failed(m) if m.contains("turned off")), "{refused:?}");
    assert!(h.sys.sleeps().is_empty() && *lock(&h.web.failed_logins) == 0);
    assert!(h.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
    assert!(!status(h.handle_web(WEB, Request::WebGuiStatus)).enabled);
}

fn status_of(response: Response) -> bool {
    match response {
        Response::WebGui(status) => status.enabled,
        other => panic!("{other:?}"),
    }
}

#[test]
fn only_the_main_socket_turns_the_web_gui_on_or_off() {
    let (h, recorder) = logged_in();
    let applied = recorder.applied.lock().unwrap().len();
    // Not the web GUI itself, logged in or not.
    assert_eq!(h.handle_web(WEB, Request::SetWebGui(false)), Err(ApiError::Denied));
    h.handle_web(WEB, Request::WebGuiLogout).unwrap();
    assert!(h.handle_web(WEB, Request::SetWebGui(false)).unwrap_err().is_web_login_required());
    // Not the touchscreen, even with its PIN entered.
    h.handle(OWNER, Request::SetScreenPin { passphrase: Secret::new(PASS), pin: Some(Secret::new("482913")) }).unwrap();
    assert!(matches!(h.handle_screen(SCREEN, Request::SetWebGui(false)), Err(ApiError::Locked(_))));
    h.handle_screen(SCREEN, Request::ScreenUnlock { pin: Secret::new("482913") }).unwrap();
    assert_eq!(h.handle_screen(SCREEN, Request::SetWebGui(false)), Err(ApiError::Denied));
    assert_eq!(h.handle_screen(SCREEN, Request::SetWebGui(true)), Err(ApiError::Denied));
    // Nor may the main socket or the screen log the web GUI in or out.
    assert_eq!(h.handle(OWNER, Request::WebGuiLogin { passphrase: Secret::new(PASS) }), Err(ApiError::Denied));
    assert_eq!(h.handle(OWNER, Request::WebGuiLogout), Err(ApiError::Denied));
    assert_eq!(h.handle_screen(SCREEN, Request::WebGuiLogin { passphrase: Secret::new(PASS) }), Err(ApiError::Denied));
    assert!(!h.web.is_open(&h.timing));
    assert_eq!(recorder.applied.lock().unwrap().len(), applied, "nothing was applied");
    assert!(h.web_gui_wanted().unwrap(), "still on");
    // And an account the helper does not know gets nowhere.
    assert_eq!(h.handle(WEB, Request::SetWebGui(false)), Err(ApiError::Denied));
    assert_eq!(h.handle(WEB, Request::WebGuiStatus), Err(ApiError::Denied));
}

#[test]
fn every_socket_reads_the_status() {
    let (h, recorder) = turned_on();
    let served = WebGuiStatus { enabled: true, url: Some(WEBGUI_URL.into()), waiting: false, open: false };
    assert_eq!(status(h.handle(OWNER, Request::WebGuiStatus)), served);
    // The screen may always look, PIN or not.
    assert_eq!(status(h.handle_screen(SCREEN, Request::WebGuiStatus)), served);
    assert!(crate::screen::is_read_only(&Request::WebGuiStatus));
    assert!(crate::screen::is_read_only(&Request::CpuStatus));
    assert!(!crate::screen::is_read_only(&Request::SetWebGui(true)));
    assert!(!crate::screen::is_read_only(&Request::WebGuiLogin { passphrase: Secret::new(PASS) }));
    // `open` is the caller's own gate, so a login shows only on the web socket.
    login(&h, PASS).unwrap();
    assert_eq!(status(h.handle(OWNER, Request::WebGuiStatus)), served);
    assert_eq!(status(h.handle_web(WEB, Request::WebGuiStatus)), WebGuiStatus { open: true, ..served.clone() });

    // Without a local address it waits, and says so.
    recorder.no_address.store(true, Ordering::SeqCst);
    let waiting = WebGuiStatus { enabled: true, url: None, waiting: true, open: false };
    assert_eq!(status(h.handle(OWNER, Request::WebGuiStatus)), waiting);
    assert_eq!(status(h.handle_web(WEB, Request::WebGuiStatus)), WebGuiStatus { open: true, ..waiting });
}

#[test]
fn turning_it_on_saves_the_choice_and_applies_it() {
    let (h, recorder) = turned_on();
    assert_eq!(*recorder.applied.lock().unwrap(), [true]);
    assert_eq!(h.sys.get("/s/web-gui.toml").as_deref(), Some(&b"enabled = true\n"[..]));
    assert!(h.current_web_input().unwrap().web_gui);
    // The answer carries the address, so the caller can show it at once.
    let again = status(h.handle(OWNER, Request::SetWebGui(true)));
    assert_eq!(again, WebGuiStatus { enabled: true, url: Some(WEBGUI_URL.into()), waiting: false, open: false });
    // At start the service follows the saved choice.
    h.sync_web_gui_service().unwrap();
    assert_eq!(*recorder.synced.lock().unwrap(), [true]);
}

#[test]
fn turning_it_off_closes_the_gate_and_stops_the_service() {
    let (h, recorder) = logged_in();
    assert!(h.handle_web(WEB, Request::ListServices).is_ok());
    let off = status(h.handle(OWNER, Request::SetWebGui(false)));
    assert_eq!(off, WebGuiStatus { enabled: false, url: None, waiting: false, open: false });
    assert_eq!(recorder.applied.lock().unwrap().last(), Some(&false), "the proxy and the service were told");
    assert_eq!(h.sys.get("/s/web-gui.toml").as_deref(), Some(&b"enabled = false\n"[..]));
    assert!(!h.current_web_input().unwrap().web_gui);

    // Every session is over, and nobody can start another.
    assert!(h.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
    assert!(matches!(login(&h, PASS), Err(ApiError::Failed(_))));
    assert_eq!(status(h.handle_web(WEB, Request::WebGuiStatus)), WebGuiStatus { enabled: false, url: None, waiting: false, open: false });
    h.sync_web_gui_service().unwrap();
    assert_eq!(*recorder.synced.lock().unwrap(), [false]);

    // Turning it on again does not bring the old login back.
    h.handle(OWNER, Request::SetWebGui(true)).unwrap();
    assert!(h.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
}

#[test]
fn a_failed_start_leaves_it_off_and_withdraws_what_was_applied() {
    let recorder = Recorder::default();
    let h = bare(recorder.clone(), &[SHOP]);
    h.init_device(&Secret::new(PASS)).unwrap();
    recorder.refuse_on.store(true, Ordering::SeqCst);
    let err = h.handle(OWNER, Request::SetWebGui(true)).unwrap_err();
    assert!(err.to_string().contains("could not turn the web GUI on: the proxy said no"), "{err}");
    assert_eq!(h.sys.get("/s/web-gui.toml"), None, "the choice is as it was");
    assert!(!h.web_gui_wanted().unwrap());
    // Applied once with it on, then again with it off, so no route or listener is left behind.
    assert_eq!(*recorder.applied.lock().unwrap(), [true, false]);
    assert!(matches!(login(&h, PASS), Err(ApiError::Failed(_))));
}

#[test]
fn turning_it_off_holds_even_when_the_proxy_cannot_follow() {
    let (h, recorder) = logged_in();
    recorder.refuse_all.store(true, Ordering::SeqCst);
    let err = h.handle(OWNER, Request::SetWebGui(false)).unwrap_err();
    assert!(err.to_string().contains("the web GUI is off"), "{err}");
    assert!(!h.web_gui_wanted().unwrap(), "off is saved, so the next apply finishes it");
    assert!(h.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
    assert!(matches!(login(&h, PASS), Err(ApiError::Failed(_))));
}

#[test]
fn a_damaged_choice_counts_as_off() {
    let (h, _) = logged_in();
    h.sys.put("/s/web-gui.toml", b"enabled = maybe");
    assert!(!h.current_web_input().unwrap().web_gui);
    assert!(!status(h.handle(OWNER, Request::WebGuiStatus)).enabled);
    h.handle_web(WEB, Request::WebGuiLogout).unwrap();
    assert!(matches!(login(&h, PASS), Err(ApiError::Failed(_))));
    // The owner's switch repairs it.
    h.handle(OWNER, Request::SetWebGui(true)).unwrap();
    assert!(login(&h, PASS).is_ok());
}

#[test]
fn the_gate_is_closed_in_a_new_helper() {
    let (h, _) = logged_in();
    // A restart is a new helper over the same files: the choice survives, the login does not.
    let Helper { sys, .. } = h;
    let restarted = Helper::new(sys, AllowedUids::new([OWNER]), Box::new(TestPolicy), Box::new(Recorder::default()))
        .with_paths(TEST_PATHS())
        .with_kdf(KdfParams { memory_kib: 64, iterations: 1, lanes: 1 });
    assert!(restarted.web_gui_wanted().unwrap());
    assert!(restarted.handle_web(WEB, Request::ListServices).unwrap_err().is_web_login_required());
}
