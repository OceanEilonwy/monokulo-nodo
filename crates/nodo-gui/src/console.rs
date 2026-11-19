//! The console on the touchscreen (`docs/design/device-console/`): Status, Services, Network and
//! Device, over nodo-ui-core.
//!
//! The device's screen is a console, not a second admin. It shows how the device is, turns a
//! service on or off and restarts it, unlocks the device after a restart, gets the owner back in
//! (the web password, SSH access, the screen PIN), troubleshoots the network, joins Wi-Fi and sets
//! the screen. Everything else (apps, settings, hosting, updates, backups, logs) is in the web
//! admin, which every page points to with a QR code of its address.
//!
//! What an action does, what a result means and the wording of consequences are the core's flows
//! (`nodo_ui_core::flows`), the same ones the terminal and the web GUI use. Effects go to the
//! core's worker thread and each outcome comes back to its flow. What the design asks for and no
//! helper call does yet goes through [`crate::missing::Missing`].
//!
//! The screen talks to the helper on its own socket, where the helper answers anything that only
//! reads and refuses changes until the screen is unlocked with the screen PIN (nodo-helper's
//! `screen` module). When a batch of effects comes back with that refusal, the screen asks for
//! the PIN and runs the same effects again once it unlocks.

// A dropped result is how failures went unreported. Where dropping is right, an #[allow] says why.
// The crate as a whole cannot deny these, because Slint's generated code breaks them.
#![deny(clippy::let_underscore_must_use, unused_must_use)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use nodo_api::{ApiError, Request, Response, ScreenStatus, ServiceState};
use nodo_ui_core::exec::{EffectWorker, Finished};
use nodo_ui_core::flows::network::{Network, RadioRead, WifiRow};
use nodo_ui_core::flows::services::AppPage;
use nodo_ui_core::flows::unlock::{self, Unlock};
use nodo_ui_core::flows::{self, drive, Confirm, Dest, Notice, NoticeKind, Question, Step};
use nodo_ui_core::model::{AttentionLevel, Hint, PackageStatus, Snapshot};
use nodo_ui_core::secret::Input;
use nodo_ui_core::{words, Effect, Outcome};
use slint::{ModelRc, SharedString, VecModel};

use crate::missing::{Missing, NetworkChecks, ResetCode, NOT_YET};
use crate::screen_prefs::{LogoStyle, Prefs};
use crate::{CAlert, CDevice, CDialog, CKeyboard, CKv, CLogo, CNetwork, CPin, CReset, CService, CServices, CStatus, CStep, CTile, CWeb, CWifi, CWifiSheet, MainWindow};

/// The rail's pages, in its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Status,
    Services,
    Network,
    Device,
}

impl Page {
    fn number(self) -> i32 {
        self as i32
    }

    fn from_number(n: usize) -> Page {
        match n {
            1 => Page::Services,
            2 => Page::Network,
            3 => Page::Device,
            _ => Page::Status,
        }
    }
}

/// What is shown over the page, as `ui/console.slint` numbers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None = 0,
    Unlock = 1,
    Keyboard = 2,
    Pin = 3,
    Reset = 4,
    Dialog = 5,
    Wifi = 6,
    Logo = 7,
    Web = 8,
}

/// A sheet over the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sheet {
    Wifi,
    Logo,
    Web,
}

/// What the full-screen keyboard is typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Typing {
    DevicePassphrase,
    WifiPassphrase,
}

struct Keyboard {
    typing: Typing,
    text: Input,
    shift: bool,
    symbols: bool,
    reveal: bool,
    error: String,
}

/// Why the PIN pad is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PinFor {
    /// A change the screen's lock refused, run again once it unlocks.
    #[default]
    Change,
    ResetWebPassword,
}

#[derive(Default)]
struct Pin {
    shown: bool,
    digits: String,
    wrong: bool,
    busy: bool,
    purpose: PinFor,
}

/// What the dialog's confirming button does.
enum Yes {
    Nothing,
    /// A question of the core's: the flow is told, and its step followed.
    Confirm(Confirm),
    /// Removes every SSH key.
    ResetSsh,
}

struct Dialog {
    title: String,
    text: String,
    danger: bool,
    /// The confirming button's words; empty when the dialog only says something.
    yes_label: String,
    yes: Yes,
}

/// The device passphrase being asked for.
struct UnlockPage {
    flow: Unlock,
    /// When the next try may come, after a wrong one.
    retry_at: Option<Instant>,
}

/// The screen's own requests, which are no effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asked {
    Status,
    Unlock,
    Lock,
}

/// What was handed to the worker and is not over yet.
enum Sent {
    Batch(Vec<Effect>),
    Call(Asked),
}

/// A batch the screen's lock refused, with what the helper answered.
struct Refused {
    effects: Vec<Effect>,
    outcomes: Vec<Outcome>,
}

/// What the tiles screen has to do after a tap in the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handoff {
    None,
    /// The power screen: Cancel, Restart or Shut down.
    Power,
}

/// How often the device is read while the console is closed, so the lock in the top bar is right.
const READ_WHILE_CLOSED: Duration = Duration::from_secs(60);

pub struct Console {
    pub open: bool,
    page: Page,
    /// The service shown beside the list.
    selected: Option<String>,
    snapshot: Option<Snapshot>,
    screen: ScreenStatus,
    worker: Option<EffectWorker>,
    /// Oldest first; the worker answers in this order.
    sent: VecDeque<Sent>,
    /// The effects the screen's lock refused, run again once the PIN unlocks it.
    retry: Option<Refused>,
    toast: Option<(String, bool, Instant)>,
    dialog: Option<Dialog>,
    keyboard: Option<Keyboard>,
    pin: Pin,
    unlock: Option<UnlockPage>,
    /// The lock in the top bar asked to unlock before the device was read.
    unlock_when_read: bool,
    reset: Option<ResetCode>,
    sheet: Option<Sheet>,
    /// Opened for a sheet or the passphrase from the tiles: closing that goes back to them.
    back_to_tiles: bool,
    network: Network,
    network_asked: Instant,
    /// The router, names and internet, as far as they can be checked.
    checks: Option<Result<NetworkChecks, String>>,
    /// The device switched to a public name server from the Network page.
    switched_dns: bool,
    /// The device's own address, from its network interfaces.
    pub address: String,
    pub host: String,
    pub missing: Box<dyn Missing>,
    last_touch: Instant,
    last_read: Instant,
    started: Instant,
    last_second: u64,
    pub prefs: Prefs,
    /// Bumped on every change the window should redraw for.
    pub version: u64,
    /// Why the helper could not be reached, when it could not.
    offline: Option<String>,
}

fn shared(s: impl AsRef<str>) -> SharedString {
    SharedString::from(s.as_ref())
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

/// Whether a refusal for want of an unlock is the screen's own lock, which the PIN lifts, and not
/// the device's, which the passphrase does. The helper's screen socket says so in words
/// (nodo-helper's `screen::handle_screen`: "enter the screen PIN to make changes", and "… until a
/// screen PIN is set …" before there is one), and both name the screen PIN.
fn is_screen_lock(refusal: &ApiError) -> bool {
    matches!(refusal, ApiError::Locked(reason) if reason.contains("screen PIN"))
}

/// A service's state in a few words, and its tone: 0 off, 1 running, 2 waits, 3 failing.
fn state_words(state: ServiceState) -> (&'static str, &'static str, i32) {
    match state {
        ServiceState::Running => ("Running", "Running", 1),
        ServiceState::Stopped => ("Off", "Off", 0),
        ServiceState::Failed => ("Keeps stopping", "Failing", 3),
        ServiceState::Locked => ("Locked until you unlock", "Locked", 2),
        ServiceState::Waiting => ("Waiting for another service", "Waiting", 2),
    }
}

/// A device row's value split for a status tile: "541 GB free of 2.0 TB" is ("541 GB free", "/ 2.0 TB").
fn split_of(value: &str) -> (String, String) {
    for joint in [" available of ", " free of ", " of "] {
        if let Some((a, b)) = value.split_once(joint) {
            let a = if joint == " of " { a.to_owned() } else { format!("{a} free") };
            return (a, format!("/ {b}"));
        }
    }
    (value.to_owned(), String::new())
}

/// The 40 keys above the keyboards' bottom row, as the design lays them out, with Shift and the
/// symbols applied.
fn keys(typing: Typing, shift: bool, symbols: bool) -> Vec<SharedString> {
    let rows = if symbols {
        "!@#$%^&*()~`'\"+=_\\|;<>[]{}?,:-/.€£¥§°±×÷"
    } else if typing == Typing::WifiPassphrase {
        "1234567890qwertyuiopasdfghjkl:zxcvbnm./-"
    } else {
        "1234567890qwertyuiopasdfghjkl-zxcvbnm.,!"
    };
    rows.chars().map(|c| if shift { c.to_uppercase().collect::<String>() } else { c.to_string() }).map(SharedString::from).collect()
}

impl Console {
    pub fn new(worker: Option<EffectWorker>, missing: Box<dyn Missing>) -> Self {
        let now = Instant::now();
        Self {
            open: false,
            page: Page::Status,
            selected: None,
            snapshot: None,
            screen: ScreenStatus { pin_set: false, unlocked: false },
            worker,
            sent: VecDeque::new(),
            retry: None,
            toast: None,
            dialog: None,
            keyboard: None,
            pin: Pin::default(),
            unlock: None,
            unlock_when_read: false,
            reset: None,
            sheet: None,
            back_to_tiles: false,
            network: Network::new(),
            network_asked: now,
            checks: None,
            switched_dns: false,
            address: String::new(),
            host: "nodo".to_owned(),
            missing,
            last_touch: now,
            last_read: now,
            started: now,
            last_second: 0,
            prefs: Prefs::default(),
            version: 0,
            offline: None,
        }
    }

    // ---------------------------------------------------------------------------------------
    // The worker: effects out, outcomes back.

    /// Hands effects to the worker. They are kept until their outcomes are back, so a batch the
    /// screen's lock refused can run again.
    fn submit(&mut self, effects: Vec<Effect>) {
        if effects.is_empty() {
            return;
        }
        if let Some(worker) = &mut self.worker {
            worker.submit(effects.clone());
            self.sent.push_back(Sent::Batch(effects));
        }
    }

    /// One of the screen's own requests.
    fn call(&mut self, request: Request, asked: Asked) {
        if let Some(worker) = &mut self.worker {
            worker.call(request);
            self.sent.push_back(Sent::Call(asked));
        }
    }

    fn idle(&self) -> bool {
        self.worker.as_ref().is_none_or(|worker| !worker.busy())
    }

    /// Takes finished helper calls. Call often from the UI thread.
    pub fn poll(&mut self) {
        let done = self.worker.as_mut().map(EffectWorker::finished).unwrap_or_default();
        for one in done {
            self.took(one);
            self.version += 1;
        }
        if let Some((_, _, at)) = &self.toast {
            if at.elapsed() > Duration::from_secs(5) {
                self.toast = None;
                self.version += 1;
            }
        }
        let typing = self.keyboard.is_some() || self.pin.shown;
        if self.open && self.last_touch.elapsed() > self.prefs.idle_return() && !typing {
            self.close();
        }
        if !self.open && self.last_read.elapsed() > READ_WHILE_CLOSED && self.idle() {
            self.last_read = Instant::now();
            self.submit(vec![Effect::Refresh]);
        }
        let second = self.started.elapsed().as_secs();
        if self.open && second != self.last_second {
            self.last_second = second;
            self.every_second(second);
        }
    }

    /// Waits for everything handed to the worker, and what that leads to: for screenshots and
    /// tests, where there is no event loop.
    pub fn drain_blocking(&mut self) {
        loop {
            let done = match &mut self.worker {
                Some(worker) if worker.busy() => worker.wait_finished(Duration::from_secs(5)),
                _ => return,
            };
            if done.is_empty() {
                return;
            }
            for one in done {
                self.took(one);
            }
        }
    }

    /// What is read again by itself: the network while a join waits for an address.
    fn every_second(&mut self, clock: u64) {
        let before = (self.network.wifi_read.clone(), self.network.bluetooth_read.clone());
        self.network.no_answer_after(self.network_asked.elapsed().as_secs());
        if before != (self.network.wifi_read.clone(), self.network.bluetooth_read.clone()) {
            self.version += 1;
        }
        // The helper does one thing at a time, so reading again never queues behind other work.
        if self.idle() {
            let effects = self.network.poll(clock);
            self.submit(effects);
        }
    }

    fn took(&mut self, done: Finished) {
        match (self.sent.pop_front(), done) {
            (Some(Sent::Batch(effects)), Finished::Batch { outcomes, refusals }) => {
                // The screen's lock refused it: the PIN, then the same effects again. Anything
                // else, the device's own lock among it, the flows say in their words.
                if self.open && refusals.iter().any(is_screen_lock) {
                    match &mut self.retry {
                        Some(waiting) => {
                            waiting.effects.extend(effects);
                            waiting.outcomes.extend(outcomes);
                        }
                        None => self.retry = Some(Refused { effects, outcomes }),
                    }
                    self.ask_pin(PinFor::Change);
                } else {
                    self.apply_all(outcomes);
                }
            }
            (Some(Sent::Call(asked)), Finished::Reply(reply)) => self.replied(asked, reply),
            // The worker answers in the order it was asked, so the two always pair up.
            _ => {}
        }
    }

    fn replied(&mut self, asked: Asked, reply: Result<Response, ApiError>) {
        match asked {
            Asked::Status => {
                if let Ok(Response::Screen(status)) = reply {
                    self.screen = status;
                }
            }
            Asked::Unlock => {
                self.pin.busy = false;
                match reply {
                    Ok(Response::Screen(status)) => {
                        self.screen = status;
                        let purpose = self.pin.purpose;
                        self.pin = Pin::default();
                        if let Some(refused) = self.retry.take() {
                            self.submit(refused.effects);
                        }
                        if purpose == PinFor::ResetWebPassword {
                            self.reset_web_password();
                        }
                    }
                    Err(ApiError::NotSetUp) => {
                        self.pin = Pin::default();
                        self.screen.pin_set = false;
                        self.ask_pin(PinFor::Change);
                    }
                    _ => self.pin.wrong = true,
                }
            }
            Asked::Lock => match reply {
                Ok(_) => self.screen.unlocked = false,
                // Nobody may be looking at the console any more, so the log is the one to hear of it.
                Err(e) => crate::log(&format!("the screen did not lock: {e}")),
            },
        }
    }

    fn apply_all(&mut self, outcomes: Vec<Outcome>) {
        for outcome in outcomes {
            let step = self.apply(outcome);
            self.follow(step);
        }
    }

    /// What the PIN would have let through is given up: the flows hear the refusal, so none of
    /// them waits for an answer, and nothing more is said, since the owner chose to stop.
    fn give_up_retry(&mut self) {
        if let Some(refused) = self.retry.take() {
            let said = self.toast.take();
            self.apply_all(refused.outcomes);
            self.toast = said;
        }
    }

    /// Hands a result to the flow it belongs to, which says what it means.
    fn apply(&mut self, outcome: Outcome) -> Step {
        match &outcome {
            Outcome::Refreshed(snapshot) => {
                self.snapshot = Some(snapshot.as_ref().clone());
                if std::mem::take(&mut self.unlock_when_read) {
                    return self.snapshot.as_ref().map_or(Step::Stay, unlock::open);
                }
                Step::Stay
            }
            Outcome::Failed(_) | Outcome::Info(_) => flows::apply(&outcome),
            Outcome::UnlockResult(_) => {
                let (Some(page), Some(snapshot)) = (&mut self.unlock, &mut self.snapshot) else { return Step::Stay };
                let failures = page.flow.failures;
                let step = page.flow.apply(&outcome, snapshot);
                if page.flow.failures > failures {
                    page.retry_at = Some(Instant::now() + Duration::from_secs(page.flow.retry_delay_seconds()));
                    if let Some(keyboard) = &mut self.keyboard {
                        keyboard.error = page.flow.error.clone().unwrap_or_default();
                    }
                }
                step
            }
            Outcome::NetworkLoaded { .. } | Outcome::WifiChanged { .. } | Outcome::WifiScanned(_) | Outcome::BluetoothChanged { .. } => {
                let step = self.network.apply(&outcome);
                // The Wi-Fi sheet has no button to look for networks: it looks once Wi-Fi is on and
                // nothing is listed yet.
                let changed = matches!(outcome, Outcome::NetworkLoaded { .. } | Outcome::WifiChanged { quiet: false, .. });
                let on = self.network.wifi.as_ref().is_some_and(|wifi| wifi.enabled && wifi.unavailable.is_none());
                if changed && on && self.network.networks.is_empty() && self.sheet == Some(Sheet::Wifi) {
                    let look = self.network.scan_wifi();
                    return Step::All(vec![step, Step::run(look)]);
                }
                step
            }
            Outcome::DriveResult(_) => drive::apply(&outcome),
            // Nothing else is asked for from the console.
            _ => Step::Stay,
        }
    }

    /// Does what a step from the core says: shows its notice or its question, goes where it
    /// sends, and runs its effects, whose outcomes come back through `apply`.
    fn follow(&mut self, step: Step) {
        match step {
            Step::Stay => {}
            Step::Done(notice) | Step::Failed(notice) => self.tell(notice),
            Step::Ask(question) => self.ask(question),
            Step::Go(dest) => self.go(dest),
            Step::Run(effects) => self.submit(effects),
            Step::All(steps) => {
                for step in steps {
                    self.follow(step);
                }
            }
        }
    }

    /// The core's destinations the console has: the passphrase, and leaving it. Anything else
    /// is done in the web admin.
    fn go(&mut self, dest: Dest) {
        match dest {
            Dest::Unlock { .. } => self.ask_passphrase(),
            Dest::Back | Dest::Resume => {
                if self.unlock.take().is_some() {
                    self.keyboard = None;
                    self.page = Page::Status;
                    self.back_to_tiles = false;
                }
            }
            Dest::Document(document) => {
                let lines: Vec<String> = document.lines.iter().take(8).cloned().collect();
                self.say_in_dialog(&document.title, &lines.join("\n"));
            }
            _ => self.say("That is done in the web admin.", false),
        }
    }

    // ---------------------------------------------------------------------------------------
    // Saying and asking.

    fn say(&mut self, message: &str, error: bool) {
        self.toast = Some((message.to_owned(), error, Instant::now()));
    }

    /// A notice of the core's. What it points at is said the screen's way.
    fn tell(&mut self, notice: Notice) {
        let how = match notice.hint {
            Some(Hint::Unlock) => " Unlock the device first.",
            _ => "",
        };
        self.say(&format!("{}{how}", notice.text), notice.kind == NoticeKind::Error);
    }

    /// A call the helper does not have yet, said for what it was to do.
    fn not_yet(&mut self, what: &str) {
        self.say(&format!("{what} from the screen is {NOT_YET}."), false);
    }

    fn say_in_dialog(&mut self, title: &str, text: &str) {
        self.dialog = Some(Dialog { title: title.to_owned(), text: text.to_owned(), danger: false, yes_label: String::new(), yes: Yes::Nothing });
    }

    /// A question of the core's, in the dialog.
    fn ask(&mut self, question: Question) {
        self.dialog = Some(Dialog {
            title: question.title,
            text: question.body.join("\n"),
            danger: question.cancel_is_default,
            yes_label: question.confirm_label,
            yes: Yes::Confirm(question.confirm),
        });
    }

    fn dialog_confirmed(&mut self) {
        let Some(dialog) = self.dialog.take() else { return };
        match dialog.yes {
            Yes::Nothing => {}
            Yes::Confirm(confirm) => {
                self.network.confirmed(&confirm);
                let step = self.snapshot.as_ref().map_or(Step::Stay, |snapshot| confirm.step(snapshot));
                self.follow(step);
            }
            Yes::ResetSsh => match self.missing.reset_ssh_access() {
                Ok(()) => self.say("Every SSH key was removed. Add a new one in the web admin.", false),
                Err(_) => self.not_yet("Removing every SSH key"),
            },
        }
    }

    /// The PIN pad, for a change the screen's lock refused or before a recovery; without a PIN
    /// set, how to set one.
    fn ask_pin(&mut self, purpose: PinFor) {
        if !self.screen.pin_set {
            self.give_up_retry();
            self.say_in_dialog(
                "Set a screen PIN first",
                "The screen can only look until a screen PIN is set. Set one over SSH with \
                 `nodo-tui --ctl screen-pin` (it asks for the device passphrase, then the PIN).",
            );
            return;
        }
        // A second refusal while the pad is up keeps the digits typed so far.
        if !self.pin.shown {
            self.pin = Pin { shown: true, purpose, ..Pin::default() };
        }
    }

    /// The device passphrase, on the full-screen keyboard.
    fn ask_passphrase(&mut self) {
        if self.unlock.is_none() {
            self.unlock = Some(UnlockPage { flow: Unlock::new(None), retry_at: None });
        }
        let error = self.unlock.as_ref().and_then(|page| page.flow.error.clone()).unwrap_or_default();
        self.keyboard = Some(Keyboard { typing: Typing::DevicePassphrase, text: Input::secret(), shift: false, symbols: false, reveal: false, error });
    }

    // ---------------------------------------------------------------------------------------
    // Opening and closing.

    pub fn touched(&mut self) {
        self.last_touch = Instant::now();
        self.version += 1;
    }

    /// The helper is out of reach: the console says so instead of showing anything.
    pub fn set_offline(&mut self, why: String) {
        self.offline = Some(why);
    }

    /// Opens on a page, reading the device, the screen's lock and the network.
    pub fn open_at(&mut self, page: Page) {
        self.open = true;
        self.touched();
        self.page = page;
        self.back_to_tiles = false;
        self.reload();
    }

    /// The lock in the tiles' top bar: the device passphrase while the device is locked.
    pub fn open_unlock(&mut self) {
        self.open_at(Page::Status);
        self.back_to_tiles = true;
        match &self.snapshot {
            Some(snapshot) => {
                let step = unlock::open(snapshot);
                self.follow(step);
            }
            None => self.unlock_when_read = true,
        }
    }

    /// The logo tile's options, from arranging.
    pub fn open_logo_options(&mut self) {
        self.open_at(Page::Status);
        self.sheet = Some(Sheet::Logo);
        self.back_to_tiles = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.keyboard = None;
        self.dialog = None;
        self.unlock = None;
        self.unlock_when_read = false;
        self.reset = None;
        self.sheet = None;
        self.give_up_retry();
        self.pin = Pin::default();
        self.call(Request::ScreenLock, Asked::Lock);
    }

    /// Reads the device, the screen's lock and the network.
    pub fn reload(&mut self) {
        self.last_read = Instant::now();
        self.submit(vec![Effect::Refresh]);
        self.call(Request::ScreenStatus, Asked::Status);
        self.network_asked = Instant::now();
        let effects = self.network.ask_again();
        self.submit(effects);
        self.check_network();
    }

    /// Asks for the router, names and internet steps.
    fn check_network(&mut self) {
        self.checks = Some(self.missing.network_checks());
    }

    /// The device waits for its passphrase, as last read.
    pub fn locked(&self) -> bool {
        self.snapshot.as_ref().is_some_and(Snapshot::is_locked)
    }

    // ---------------------------------------------------------------------------------------
    // Input from the window.

    /// A tap. Returns what the tiles screen has to do, for the power screen.
    pub fn act(&mut self, id: &str) -> Handoff {
        self.touched();
        let (verb, arg) = id.split_once(':').unwrap_or((id, ""));
        let arg = arg.to_owned();
        match verb {
            "home" => self.close(),
            "page" => {
                self.page = Page::from_number(arg.parse().unwrap_or(0));
                if self.page == Page::Network {
                    self.check_network();
                }
            }
            "unlock-device" => match &self.snapshot {
                Some(snapshot) => {
                    let step = unlock::open(snapshot);
                    self.follow(step);
                }
                None => self.unlock_when_read = true,
            },
            "drive-retry" => self.submit(drive::retry()),
            "select" => self.selected = Some(arg),
            "switch" => {
                let Some(snapshot) = &self.snapshot else { return Handoff::None };
                self.selected = Some(arg.clone());
                let step = AppPage::new(&arg).start_or_stop(snapshot);
                self.follow(step);
            }
            "restart" => self.submit(AppPage::new(&arg).restart()),
            "net-check" => {
                self.reload();
            }
            "net-public-dns" => match self.missing.use_public_dns(true) {
                Ok(()) => {
                    self.switched_dns = true;
                    self.check_network();
                }
                Err(_) => self.not_yet("Switching the name server"),
            },
            "net-undo-dns" => match self.missing.use_public_dns(false) {
                Ok(()) => {
                    self.switched_dns = false;
                    self.check_network();
                }
                Err(_) => self.not_yet("Switching the name server"),
            },
            "net-renew" => match self.missing.renew_address() {
                Ok(()) => self.reload(),
                Err(_) => self.not_yet("Renewing the address"),
            },
            "net-done" => self.page = Page::Status,
            "reset-web" => {
                if self.screen.unlocked {
                    self.reset_web_password();
                } else {
                    self.ask_pin(PinFor::ResetWebPassword);
                }
            }
            "reset-new" => self.reset_web_password(),
            "reset-done" => self.reset = None,
            "reset-ssh" => {
                self.dialog = Some(Dialog {
                    title: "Remove every SSH key?".to_owned(),
                    text: "Nobody can log in over SSH until a new key is added in the web admin.".to_owned(),
                    danger: true,
                    yes_label: "Remove all keys".to_owned(),
                    yes: Yes::ResetSsh,
                });
            }
            "change-pin" => match self.missing.change_screen_pin() {
                Ok(()) => self.say("The screen PIN was changed.", false),
                Err(_) => self.say(&format!("Changing the screen PIN here is {NOT_YET}. Change it over SSH: nodo-tui --ctl screen-pin."), false),
            },
            "web" => self.sheet = Some(Sheet::Web),
            "wifi" => {
                self.sheet = Some(Sheet::Wifi);
                let on = self.network.wifi.as_ref().is_some_and(|wifi| wifi.enabled && wifi.unavailable.is_none());
                if on && self.network.networks.is_empty() {
                    let look = self.network.scan_wifi();
                    self.submit(look);
                }
            }
            "wifi-toggle" => {
                let step = self.network.toggle_wifi();
                self.follow(step);
            }
            "wifi-join" => {
                let step = self.network.join(&arg);
                self.follow(step);
                if self.network.prompt.is_some() {
                    // Shown as typed, as the design draws it: a Wi-Fi passphrase is read off a router's label.
                    self.keyboard = Some(Keyboard { typing: Typing::WifiPassphrase, text: Input::secret(), shift: false, symbols: false, reveal: true, error: String::new() });
                }
            }
            "display-off" => {
                self.prefs.display_off_index = arg.parse().unwrap_or(self.prefs.display_off_index).min(4);
                self.prefs.save();
            }
            "brightness" => {
                self.prefs.step_brightness();
                self.prefs.save();
            }
            "logo-style" => {
                self.prefs.logo_style = if arg == "1" { LogoStyle::Text } else { LogoStyle::Drawn };
                self.prefs.save();
            }
            "logo-shimmer" => {
                self.prefs.logo_shimmer = !self.prefs.logo_shimmer;
                self.prefs.save();
            }
            "logo-name" => {
                self.prefs.logo_name = !self.prefs.logo_name;
                self.prefs.save();
            }
            "sheet-close" => {
                self.sheet = None;
                if std::mem::take(&mut self.back_to_tiles) {
                    self.close();
                }
            }
            "dialog-yes" => self.dialog_confirmed(),
            "dialog-close" => self.dialog = None,
            "power" => {
                self.close();
                return Handoff::Power;
            }
            _ => {}
        }
        Handoff::None
    }

    fn reset_web_password(&mut self) {
        match self.missing.reset_web_password() {
            Ok(code) => self.reset = Some(code),
            Err(_) => self.not_yet("Resetting the web password"),
        }
    }

    /// A key of the PIN pad (`pin:…`) or of the full-screen keyboard (`kb:…`).
    pub fn key(&mut self, k: &str) {
        self.touched();
        if let Some(digit) = k.strip_prefix("pin:") {
            self.pin_key(digit);
            return;
        }
        let Some(kb) = &mut self.keyboard else { return };
        match k {
            "shift" => kb.shift = !kb.shift,
            "symbols" => kb.symbols = !kb.symbols,
            "eye" => kb.reveal = !kb.reveal,
            "back" => kb.text.pop(),
            "space" => kb.text.push(' '),
            "cancel" => self.keyboard_cancelled(),
            "done" => self.keyboard_done(),
            typed => {
                let typed = typed.strip_prefix("kb:").unwrap_or(typed);
                let typed = if kb.shift { typed.to_uppercase() } else { typed.to_owned() };
                typed.chars().for_each(|c| kb.text.push(c));
                kb.shift = false;
            }
        }
    }

    fn pin_key(&mut self, k: &str) {
        match k {
            "cancel" => {
                self.pin = Pin::default();
                self.give_up_retry();
            }
            "back" => {
                self.pin.digits.pop();
                self.pin.wrong = false;
            }
            d if d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()) && self.pin.digits.len() < 6 => {
                self.pin.digits.push_str(d);
                self.pin.wrong = false;
                if self.pin.digits.len() == 6 {
                    let pin = nodo_api::Secret::new(std::mem::take(&mut self.pin.digits));
                    self.pin.busy = true;
                    self.call(Request::ScreenUnlock { pin }, Asked::Unlock);
                }
            }
            _ => {}
        }
    }

    fn keyboard_cancelled(&mut self) {
        let Some(kb) = self.keyboard.take() else { return };
        match kb.typing {
            Typing::DevicePassphrase => {
                self.unlock = None;
                if std::mem::take(&mut self.back_to_tiles) {
                    self.close();
                }
            }
            Typing::WifiPassphrase => drop(self.network.cancel_prompt()),
        }
    }

    /// Done on the keyboard: what was typed goes to the flow it was typed for. Where the flow
    /// refuses it, the keyboard stays with the reason.
    fn keyboard_done(&mut self) {
        let Some(mut kb) = self.keyboard.take() else { return };
        let refused = match kb.typing {
            Typing::DevicePassphrase => {
                let Some(page) = &mut self.unlock else { return };
                let wait = page.retry_at.map_or(0, |at| at.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u64);
                let effects = page.flow.submit(&mut kb.text, wait);
                if effects.is_empty() {
                    page.flow.error.clone()
                } else {
                    self.submit(effects);
                    // The keyboard stays until the helper answers: a wrong passphrase is typed again there.
                    kb.error.clear();
                    self.keyboard = Some(kb);
                    return;
                }
            }
            Typing::WifiPassphrase => {
                let effects = self.network.submit_passphrase(&mut kb.text);
                if effects.is_empty() {
                    self.network.error.clone()
                } else {
                    self.submit(effects);
                    None
                }
            }
        };
        if let Some(why) = refused {
            kb.error = why;
            self.keyboard = Some(kb);
        }
    }

    // ---------------------------------------------------------------------------------------
    // What the window shows.

    #[cfg(test)]
    pub fn page(&self) -> Page {
        self.page
    }

    pub fn overlay(&self) -> Overlay {
        if self.pin.shown {
            return Overlay::Pin;
        }
        if let Some(kb) = &self.keyboard {
            return match kb.typing {
                Typing::DevicePassphrase => Overlay::Unlock,
                Typing::WifiPassphrase => Overlay::Keyboard,
            };
        }
        if self.dialog.is_some() {
            return Overlay::Dialog;
        }
        if self.reset.is_some() {
            return Overlay::Reset;
        }
        match self.sheet {
            Some(Sheet::Wifi) => Overlay::Wifi,
            Some(Sheet::Logo) => Overlay::Logo,
            Some(Sheet::Web) => Overlay::Web,
            None => Overlay::None,
        }
    }

    pub fn toast(&self) -> (String, bool) {
        let working = self.sent.iter().find_map(|sent| match sent {
            Sent::Batch(effects) => effects.iter().find_map(|effect| match effect {
                Effect::Start(_) => Some("Starting"),
                Effect::Stop(_) => Some("Stopping"),
                Effect::Restart(_) => Some("Restarting"),
                Effect::Unlock(_) => Some("Unlocking"),
                Effect::JoinWifi { .. } => Some("Joining"),
                _ => None,
            }),
            Sent::Call(_) => None,
        });
        match working {
            Some(what) => (format!("{what}…"), false),
            None => self.toast.as_ref().map(|(m, e, _)| (m.clone(), *e)).unwrap_or_default(),
        }
    }

    /// The web admin's address: where the web GUI says it answers, else its usual name.
    pub fn web_url(&self) -> String {
        self.snapshot.as_ref().and_then(|s| s.web_gui.as_ref()).and_then(|w| w.url.clone()).unwrap_or_else(|| nodo_api::WEBGUI_URL.to_owned())
    }

    pub fn web_view(&self) -> CWeb {
        let url = self.web_url();
        let off = self.snapshot.as_ref().and_then(|s| s.web_gui.as_ref()).is_some_and(|w| !w.enabled);
        let updates = self.snapshot.as_ref().map_or(0, Snapshot::pending_updates);
        let note = if off {
            "The web admin is off. It is turned on over SSH.".to_owned()
        } else if updates > 0 {
            format!("Apps, settings, hosting and {} are in the web admin.", words::counted(updates as u64, "ready update"))
        } else {
            "Apps, settings, hosting, updates and backups are in the web admin.".to_owned()
        };
        let host = if self.address.is_empty() { format!("{}.local", self.host) } else { format!("{}\n{}.local", self.address, self.host) };
        CWeb { url: shared(&url), host: shared(host), note: shared(note), qr: crate::qr::image(&url) }
    }

    /// The alerts on Status. They never name an app: what the device says about one is counted
    /// and left to the web admin.
    fn alerts(&self, snap: &Snapshot) -> Vec<CAlert> {
        let mut alerts = Vec::new();
        let alert = |title: String, text: String, tone: i32, button: &str, id: &str| CAlert { title: shared(title), text: shared(text), tone, button: shared(button), id: shared(id) };
        if let Some(drive) = snap.overview.drive.as_ref().filter(|_| snap.landing_once_set_up() == nodo_ui_core::queries::Landing::NoDrive) {
            alerts.push(alert("The data drive is missing".to_owned(), drive.waiting.clone(), 3, "Check again", "drive-retry"));
        }
        if snap.is_locked() {
            let waiting = snap.overview.unlock_targets.iter().filter(|t| t.encrypted).count().max(snap.services.iter().filter(|s| s.state == ServiceState::Locked).count());
            let text = match waiting {
                0 => "Saved keys wait for the device passphrase.".to_owned(),
                1 => "1 service waits for the device passphrase.".to_owned(),
                n => format!("{n} services wait for the device passphrase."),
            };
            alerts.push(alert("Unlock after a restart".to_owned(), text, 1, "Unlock", "unlock-device"));
        }
        let failing = snap.services.iter().filter(|s| s.state == ServiceState::Failed).count();
        if failing > 0 {
            let title = if failing == 1 { "1 service keeps stopping".to_owned() } else { format!("{failing} services keep stopping") };
            alerts.push(alert(title, "Restart it from Services, or read why in the web admin.".to_owned(), 3, "Services", "page:1"));
        }
        let names: Vec<&str> = snap.services.iter().map(|s| s.id.as_str()).chain(snap.catalog.iter().map(|p| p.id.name.as_str())).collect();
        let mut about_apps = 0;
        for a in snap.overview.attention.iter().filter(|a| a.level != AttentionLevel::Info) {
            if names.iter().any(|name| !name.is_empty() && a.text.contains(name)) {
                about_apps += 1;
            } else {
                alerts.push(alert(a.text.clone(), String::new(), if a.level == AttentionLevel::Critical { 3 } else { 1 }, "", ""));
            }
        }
        if about_apps > 0 {
            let title = if about_apps == 1 { "1 notice about an app".to_owned() } else { format!("{about_apps} notices about apps") };
            alerts.push(alert(title, "Read it in the web admin.".to_owned(), 1, "", ""));
        }
        alerts
    }

    /// "Connected" over a cable or Wi-Fi, from the radios' answer.
    fn link(&self) -> (String, String, bool) {
        match &self.network.wifi {
            Some(w) if w.wired_up => ("Connected".to_owned(), "Cable".to_owned(), true),
            Some(w) if w.link == nodo_api::WifiLink::Connected => ("Connected".to_owned(), "Wi-Fi".to_owned(), true),
            Some(_) => ("No link".to_owned(), String::new(), false),
            None if self.network.wifi_read == RadioRead::Asking => ("Checking…".to_owned(), String::new(), false),
            None => ("Unknown".to_owned(), String::new(), false),
        }
    }

    fn status_view(&self) -> CStatus {
        let Some(snap) = &self.snapshot else {
            let heading = if self.offline.is_some() { "The screen can't reach the Nodo" } else { "Reading the Nodo…" };
            let alerts = self
                .offline
                .iter()
                .map(|why| CAlert { title: "The helper isn't answering".into(), text: shared(why), tone: 3, button: "".into(), id: "".into() })
                .collect();
            return CStatus { heading: heading.into(), alerts: model(alerts), tiles: model(Vec::new()) };
        };
        let alerts = self.alerts(snap);
        let heading = match alerts.len() {
            0 => "Everything is running".to_owned(),
            1 => "1 thing needs you".to_owned(),
            n => format!("{n} things need you"),
        };
        let device = |labels: &[&str]| snap.overview.device.iter().find(|(l, _)| labels.contains(&l.as_str())).map(|(_, v)| v.clone());
        let (link, how, up) = self.link();
        let tile = |label: &str, (value, unit): (String, String), chip: &str, tone: i32, id: &str| CTile {
            label: shared(label),
            value: shared(value),
            unit: shared(unit),
            chip: shared(chip),
            chip_tone: tone,
            id: shared(id),
        };
        let missing = || ("—".to_owned(), String::new());
        let tiles = vec![
            tile("Internet", (link, String::new()), &how, if up { 1 } else { 3 }, "page:2"),
            tile("Data drive", device(&["Data drive", "SSD"]).map_or_else(missing, |v| split_of(&v)), "", 0, ""),
            tile("Temperature", device(&["Temp", "Temperature"]).map_or_else(missing, |v| (v, String::new())), "", 0, ""),
            tile("Memory", device(&["Memory"]).map_or_else(missing, |v| split_of(&v)), "", 0, ""),
        ];
        // At most two cards fit above the tiles; the heading counts them all.
        CStatus { heading: shared(heading), alerts: model(alerts.into_iter().take(2).collect()), tiles: model(tiles) }
    }

    fn services_view(&self) -> CServices {
        let empty = CServices {
            count: "".into(),
            rows: model(Vec::new()),
            has_detail: false,
            initial: "".into(),
            name: "".into(),
            sub: "".into(),
            chip: "".into(),
            chip_tone: 0,
            kvs: model(Vec::new()),
            can_restart: false,
        };
        let Some(snap) = &self.snapshot else { return empty };
        let selected = self.selected.clone().filter(|id| snap.services.iter().any(|s| s.id == *id)).or_else(|| snap.services.first().map(|s| s.id.clone()));
        let initial = |id: &str| id.chars().next().map(|c| c.to_string()).unwrap_or_default();
        let rows: Vec<CService> = snap
            .services
            .iter()
            .map(|s| {
                let (state, _, tone) = state_words(s.state);
                CService {
                    id: shared(&s.id),
                    initial: shared(initial(&s.id)),
                    name: shared(&s.id),
                    state: shared(state),
                    tone,
                    on: s.state != ServiceState::Stopped,
                    selected: selected.as_deref() == Some(s.id.as_str()),
                }
            })
            .collect();
        let on = rows.iter().filter(|r| r.on).count();
        let count = format!("{on} of {} on", rows.len());
        let Some(s) = selected.and_then(|id| snap.services.iter().find(|s| s.id == id)) else { return CServices { count: shared(count), rows: model(rows), ..empty } };
        let (_, chip, tone) = state_words(s.state);
        let version = snap.catalog.iter().find(|p| p.id.name == s.id && p.status != PackageStatus::Available).map(|p| p.version.clone());
        let sub = [s.uptime_secs.filter(|_| s.state == ServiceState::Running).map(|u| format!("up {}", words::uptime(u))), version.map(|v| format!("version {v}"))]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let kv = |label: &str, value: String, mono: bool| CKv { label: shared(label), value: shared(value), mono };
        let mut kvs = Vec::new();
        if let Some(site) = snap.web.sites.iter().find(|site| site.package == s.id) {
            if site.exposure.local && !site.local_url.is_empty() {
                kvs.push(kv("Home network", site.local_url.clone(), true));
            }
            if let Some(host) = site.public_host.as_ref().filter(|_| site.exposure.public) {
                kvs.push(kv("Internet", format!("https://{host}"), true));
            }
            if site.exposure.onion && !site.onion_url.is_empty() {
                kvs.push(kv("Tor", site.onion_url.clone(), true));
            }
        }
        if kvs.is_empty() {
            kvs.push(kv("Addresses", "None: it has no web page".to_owned(), false));
        }
        kvs.push(match self.missing.service_ports(&s.id) {
            Some(ports) => kv("Ports", ports, true),
            None => kv("Ports", format!("Shown here is {NOT_YET}"), false),
        });
        if !s.detail.is_empty() {
            kvs.push(kv("Now", s.detail.clone(), false));
        }
        CServices {
            count: shared(count),
            rows: model(rows),
            has_detail: true,
            initial: shared(initial(&s.id)),
            name: shared(&s.id),
            sub: shared(sub),
            chip: shared(chip),
            chip_tone: tone,
            kvs: model(kvs),
            can_restart: matches!(s.state, ServiceState::Running | ServiceState::Failed),
        }
    }

    fn network_view(&self) -> CNetwork {
        let step = |name: &str, detail: &str, value: &str, kind: i32| CStep { name: shared(name), detail: shared(detail), value: shared(value), kind };
        let (_, how, up) = self.link();
        let wifi = self.network.wifi.as_ref();
        let joined = wifi.and_then(|w| w.ssid.clone()).filter(|_| wifi.is_some_and(|w| w.link == nodo_api::WifiLink::Connected));
        let mut steps = vec![match (wifi, how.as_str()) {
            (None, _) => step("Cable", "Checking…", "", 0),
            (_, "Cable") => step("Cable", "Plugged in", "", 1),
            (_, "Wi-Fi") => step("Wi-Fi", &format!("Joined {}", joined.clone().unwrap_or_default()), "", 1),
            _ => step("Cable", "Not plugged in, and no Wi-Fi", "", 2),
        }];
        let has_address = !self.address.is_empty();
        steps.push(if has_address { step("Address", "Given by your router", &self.address, 1) } else { step("Address", "None yet", "", if up { 2 } else { 0 }) });
        let checks = self.checks.as_ref().and_then(|c| c.as_ref().ok()).filter(|_| up && has_address);
        let waiting = |name: &str| step(name, if up && has_address { "Checking it here is not available yet" } else { "Waiting on the step above" }, "", 0);
        match checks {
            Some(c) => {
                let mut blocked = false;
                for (name, checked) in [("Router", &c.router), ("Names (DNS)", &c.names), ("Internet", &c.internet)] {
                    if blocked {
                        steps.push(step(name, "Waiting on the step above", "", 0));
                    } else if checked.passed {
                        steps.push(step(name, &checked.detail, &checked.value, 1));
                    } else {
                        blocked = true;
                        steps.push(step(name, &checked.detail, &checked.value, 2));
                    }
                }
            }
            None => steps.extend([waiting("Router"), waiting("Names (DNS)"), waiting("Internet")]),
        }
        let card = |title: &str, text: &str, tone: i32, fixed: bool, ok: bool| CNetwork { steps: model(steps.clone()), title: shared(title), text: shared(text), tone, fixed, ok };
        if wifi.is_none() {
            return card("Checking…", "Asking the Nodo's network interfaces.", 0, false, false);
        }
        if !up {
            return card("No cable and no Wi-Fi", "Plug a network cable into the Nodo and your router, or join Wi-Fi from Device.", 3, false, false);
        }
        if !has_address {
            return card("No address from your router", "The Nodo is connected, but your router gave it no address yet.", 3, false, false);
        }
        let Some(c) = checks else {
            return card(
                "The link and the address are fine",
                &format!("Checking the router, names and the internet from the screen is {NOT_YET}."),
                0,
                false,
                false,
            );
        };
        if !c.router.passed {
            card("Your router isn't answering", "The Nodo has an address, but the router doesn't answer it. Restart the router.", 3, false, false)
        } else if !c.names.passed {
            card("Your router's name server isn't answering", "The Nodo reaches your router, but can't look up names like monokulo.org.", 3, false, false)
        } else if !c.internet.passed {
            card("The internet can't be reached", "Names work, but nothing on the internet answers. Your internet provider may be down.", 3, false, false)
        } else if self.switched_dns && c.public_dns {
            card("The internet is back", "The Nodo now asks 1.1.1.1 for names. You can switch back in the web admin.", 1, true, true)
        } else {
            card("Online", "Every step passed.", 1, false, true)
        }
    }

    fn device_view(&self) -> CDevice {
        let (link, how, _) = self.link();
        let wifi = match &self.network.wifi {
            Some(w) if w.unavailable.is_some() => "Not available".to_owned(),
            Some(w) if !w.enabled => "Off".to_owned(),
            Some(w) => w.ssid.clone().filter(|_| w.link == nodo_api::WifiLink::Connected).unwrap_or_else(|| "On".to_owned()),
            None => String::new(),
        };
        let network = if how.is_empty() { link } else { format!("{link} · {how}") };
        CDevice {
            subtitle: shared(&self.host),
            network_note: shared(network),
            wifi_note: shared(wifi),
            off_index: self.prefs.display_off_index as i32,
            off_label: shared(self.prefs.display_off_label()),
            brightness: shared(format!("{}%", self.prefs.brightness())),
        }
    }

    fn keyboard_view(&self) -> CKeyboard {
        let Some(kb) = &self.keyboard else {
            return CKeyboard {
                title: "".into(),
                sub: "".into(),
                label: "".into(),
                text: "".into(),
                error: "".into(),
                secret: false,
                reveal: false,
                shift: false,
                symbols: false,
                done: "".into(),
                keys: model(Vec::new()),
            };
        };
        let text = if kb.reveal { kb.text.secret_value().expose().to_owned() } else { kb.text.display() };
        let (title, sub, label, done) = match kb.typing {
            Typing::DevicePassphrase => {
                let waiting = self.snapshot.as_ref().map_or(0, |s| s.overview.unlock_targets.iter().filter(|t| t.encrypted).count());
                let sub = match waiting {
                    0 => "Saved keys stay encrypted until you enter the device passphrase.".to_owned(),
                    1 => "1 service keeps its storage encrypted until you enter the device passphrase.".to_owned(),
                    n => format!("{n} services keep their storage encrypted until you enter the device passphrase."),
                };
                ("Unlock your Nodo".to_owned(), sub, "Device passphrase".to_owned(), "Unlock")
            }
            Typing::WifiPassphrase => {
                let ssid = self.network.prompt.as_ref().map(|p| p.ssid.clone()).unwrap_or_default();
                (String::new(), String::new(), format!("Wi-Fi passphrase · {ssid}"), "Join")
            }
        };
        CKeyboard {
            title: shared(title),
            sub: shared(sub),
            label: shared(label),
            text: shared(text),
            error: shared(&kb.error),
            secret: kb.text.is_secret(),
            reveal: kb.reveal,
            shift: kb.shift,
            symbols: kb.symbols,
            done: shared(done),
            keys: model(keys(kb.typing, kb.shift, kb.symbols)),
        }
    }

    fn wifi_view(&self) -> CWifiSheet {
        let flow = &self.network;
        let mut note = flow.error.clone().or_else(|| flow.busy.clone()).unwrap_or_default();
        let Some(w) = &flow.wifi else {
            if note.is_empty() {
                note = match &flow.wifi_read {
                    RadioRead::Failed(why) => why.clone(),
                    RadioRead::NoAnswer => "No answer yet. The Nodo may be busy with something else.".to_owned(),
                    _ => "Reading Wi-Fi…".to_owned(),
                };
            }
            return CWifiSheet { available: false, on: false, note: shared(note), rows: model(Vec::new()) };
        };
        if let Some(why) = &w.unavailable {
            return CWifiSheet { available: false, on: false, note: shared(why), rows: model(Vec::new()) };
        }
        let rows = if w.enabled {
            flow.wifi_rows()
                .into_iter()
                .filter_map(|listed| {
                    let (ssid, security, saved) = flow.wifi_row(listed)?;
                    let mut facts = Vec::new();
                    match listed {
                        WifiRow::InRange(index) => facts.push(format!("{} dBm", flow.networks[index].signal_dbm)),
                        WifiRow::Saved(_) => facts.push("not in range".to_owned()),
                    }
                    if security == nodo_api::WifiSecurity::Open {
                        facts.push("open".to_owned());
                    }
                    if saved {
                        facts.push("saved".to_owned());
                    }
                    let joined = flow.joined() == Some(ssid.as_str());
                    Some(CWifi { ssid: shared(&ssid), note: shared(facts.join(" · ")), joined })
                })
                .take(4)
                .collect()
        } else {
            Vec::new()
        };
        CWifiSheet { available: true, on: w.enabled, note: shared(note), rows: model(rows) }
    }

    /// Pushes everything to the window. The caller only calls it when `version` changed.
    pub fn show(&self, ui: &MainWindow) {
        ui.set_c_page(self.page.number());
        ui.set_c_overlay(self.overlay() as i32);
        match self.page {
            Page::Status => ui.set_c_status(self.status_view()),
            Page::Services => ui.set_c_services(self.services_view()),
            Page::Network => ui.set_c_network(self.network_view()),
            Page::Device => ui.set_c_device(self.device_view()),
        }
        ui.set_c_web(self.web_view());
        ui.set_c_keyboard(self.keyboard_view());
        ui.set_c_pin(CPin {
            sub: shared(match self.pin.purpose {
                PinFor::Change => "to make changes on this screen",
                PinFor::ResetWebPassword => "to reset the web password",
            }),
            digits: self.pin.digits.len() as i32,
            wrong: self.pin.wrong,
            busy: self.pin.busy,
        });
        let reset = self.reset.clone().unwrap_or(ResetCode { code: String::new(), link: String::new(), page: String::new() });
        ui.set_c_reset(CReset { code: shared(&reset.code), page: shared(&reset.page), qr: crate::qr::image(&reset.link) });
        ui.set_c_dialog(match &self.dialog {
            Some(d) => CDialog { title: shared(&d.title), text: shared(&d.text), danger: d.danger, yes: shared(&d.yes_label) },
            None => CDialog { title: "".into(), text: "".into(), danger: false, yes: "".into() },
        });
        if self.sheet == Some(Sheet::Wifi) {
            ui.set_c_wifi(self.wifi_view());
        }
        ui.set_c_logo(CLogo { text: self.prefs.logo_style == LogoStyle::Text, shimmer: self.prefs.logo_shimmer, name: self.prefs.logo_name });
        let (toast, error) = self.toast();
        ui.set_c_toast(toast.into());
        ui.set_c_toast_error(error);
        ui.set_c_open(self.open);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::missing::{NotYet, Sample};
    use nodo_ui_core::mock::MockBackend;
    use slint::Model;

    const PIN: &str = "482913";

    fn console_over(mock: MockBackend, missing: Box<dyn Missing>) -> Console {
        let mut c = Console::new(Some(EffectWorker::spawn(Box::new(mock))), missing);
        c.address = "192.168.1.155".to_owned();
        c.open_at(Page::Status);
        c.drain_blocking();
        c
    }

    fn console() -> Console {
        console_over(MockBackend::new(), Box::new(NotYet))
    }

    /// The same device behind the touchscreen's socket, where changes need the screen PIN.
    fn locked_screen(missing: Box<dyn Missing>) -> Console {
        let mut mock = MockBackend::new();
        mock.knobs.screen_pin = Some(PIN.to_owned());
        console_over(mock, missing)
    }

    fn enter_pin(c: &mut Console, pin: &str) {
        for digit in pin.chars() {
            c.key(&format!("pin:{digit}"));
        }
        c.drain_blocking();
    }

    fn type_and_done(c: &mut Console, typed: &str) {
        while !c.keyboard_view().text.is_empty() {
            c.key("back");
        }
        c.key(&format!("kb:{typed}"));
        c.key("done");
        c.drain_blocking();
    }

    fn state_of(c: &Console, app: &str) -> Option<ServiceState> {
        AppPage::new(app).state(c.snapshot.as_ref().expect("the device was read"))
    }

    #[test]
    fn status_counts_what_needs_the_owner_without_naming_an_app() {
        let mut mock = MockBackend::new();
        mock.key_locked = true;
        let c = console_over(mock, Box::new(NotYet));
        let status = c.status_view();
        let alerts: Vec<CAlert> = status.alerts.iter().collect();
        let unlock = alerts.iter().find(|a| a.id.as_str() == "unlock-device").expect("the unlock card");
        assert_eq!(unlock.title.as_str(), "Unlock after a restart");
        assert!(unlock.text.ends_with("wait for the device passphrase."), "{}", unlock.text);
        let services: Vec<String> = c.snapshot.as_ref().unwrap().services.iter().map(|s| s.id.clone()).collect();
        for a in &alerts {
            for name in &services {
                assert!(!a.title.contains(name.as_str()) && !a.text.contains(name.as_str()), "{} names {name}", a.title);
            }
        }
        assert!(status.heading.contains("need"));
        assert_eq!(status.tiles.row_count(), 4);
    }

    #[test]
    fn every_page_has_a_view() {
        let mut c = console();
        for (n, page) in [Page::Status, Page::Services, Page::Network, Page::Device].into_iter().enumerate() {
            c.act(&format!("page:{n}"));
            c.drain_blocking();
            assert_eq!(c.page(), page);
        }
        assert!(c.services_view().has_detail);
        assert_eq!(c.network_view().steps.row_count(), 5);
        assert_eq!(c.device_view().off_label.as_str(), "After 15 min idle");
        assert_eq!(c.overlay(), Overlay::None);
    }

    #[test]
    fn a_service_turns_off_and_on_after_the_pin_and_restarts() {
        let mut c = locked_screen(Box::new(NotYet));
        assert_eq!(state_of(&c, "moneropay"), Some(ServiceState::Running));
        c.act("page:1");
        c.act("switch:moneropay");
        c.drain_blocking();
        assert_eq!(c.overlay(), Overlay::Pin, "the refusal asks for the PIN");
        assert_eq!(c.toast().0, "", "and is not said as a failure");
        enter_pin(&mut c, "111111");
        assert!(c.pin.shown && c.pin.wrong);
        enter_pin(&mut c, PIN);
        assert_eq!(c.overlay(), Overlay::None);
        assert_eq!(state_of(&c, "moneropay"), Some(ServiceState::Stopped), "the same change ran again");
        let row = c.services_view().rows.iter().find(|r| r.id.as_str() == "moneropay").unwrap();
        assert!(!row.on && row.state.as_str() == "Off");
        c.act("switch:moneropay");
        c.drain_blocking();
        assert_eq!(state_of(&c, "moneropay"), Some(ServiceState::Running));
        c.act("restart:moneropay");
        c.drain_blocking();
        assert!(!c.pin.shown);
    }

    #[test]
    fn a_locked_service_says_so() {
        let mut c = console();
        c.act("switch:wallet-relay");
        assert_eq!(c.toast(), ("wallet-relay is locked. Unlock the device first.".to_owned(), true));
    }

    #[test]
    fn a_services_details_are_read_only_facts() {
        let c = console_over(MockBackend::new(), Box::new(Sample::default()));
        let mut c = c;
        c.act("select:moneropay");
        let view = c.services_view();
        assert_eq!(view.name.as_str(), "moneropay");
        let kvs: Vec<(String, String)> = view.kvs.iter().map(|k| (k.label.to_string(), k.value.to_string())).collect();
        assert!(kvs.iter().any(|(l, _)| l == "Home network"), "{kvs:?}");
        assert!(kvs.contains(&("Ports".to_owned(), "443 https · 5000 API".to_owned())), "{kvs:?}");
        // Without the helper's ports, it says so.
        let plain = console();
        let ports = plain.services_view().kvs.iter().find(|k| k.label.as_str() == "Ports").unwrap();
        assert!(ports.value.contains(NOT_YET));
    }

    #[test]
    fn the_device_passphrase_is_asked_again_with_the_reason_after_a_wrong_one() {
        let mut mock = MockBackend::new();
        mock.key_locked = true;
        let mut c = console_over(mock, Box::new(NotYet));
        c.act("unlock-device");
        assert_eq!(c.overlay(), Overlay::Unlock);
        c.key("done");
        assert_eq!(c.keyboard_view().error.as_str(), "Enter the device passphrase.");
        type_and_done(&mut c, "not the passphrase");
        assert_eq!(c.overlay(), Overlay::Unlock);
        assert_eq!(c.keyboard_view().error.as_str(), "That passphrase did not unlock this device. Try again.");
        // The eye shows what was typed.
        c.key("kb:abc");
        assert_eq!(c.keyboard_view().text.as_str(), "•••");
        c.key("eye");
        assert_eq!(c.keyboard_view().text.as_str(), "abc");
        c.unlock.as_mut().unwrap().retry_at = None;
        type_and_done(&mut c, nodo_ui_core::mock::MOCK_PASSPHRASE);
        assert_eq!(c.overlay(), Overlay::None);
        assert!(!c.snapshot.as_ref().unwrap().is_locked());
        assert_eq!(c.page(), Page::Status);
    }

    #[test]
    fn the_lock_in_the_top_bar_opens_the_passphrase_and_leaving_goes_back_to_the_tiles() {
        let mut mock = MockBackend::new();
        mock.key_locked = true;
        let mut c = console_over(mock, Box::new(NotYet));
        c.close();
        c.open_unlock();
        assert_eq!(c.overlay(), Overlay::Unlock);
        c.key("cancel");
        assert!(!c.open, "Leave it locked goes back to the tiles");
    }

    #[test]
    fn resetting_the_web_password_asks_for_the_pin_then_shows_a_one_time_code() {
        let mut c = locked_screen(Box::new(Sample::default()));
        c.act("page:3");
        c.act("reset-web");
        assert_eq!(c.overlay(), Overlay::Pin);
        assert_eq!(c.pin.purpose, PinFor::ResetWebPassword);
        enter_pin(&mut c, PIN);
        assert_eq!(c.overlay(), Overlay::Reset);
        assert_eq!(c.reset.as_ref().unwrap().code, "K7QF-29XM");
        c.act("reset-done");
        assert_eq!(c.overlay(), Overlay::None);
        // On the device it is not there yet, and says so.
        let mut d = locked_screen(Box::new(NotYet));
        d.act("reset-web");
        enter_pin(&mut d, PIN);
        assert_eq!(d.overlay(), Overlay::None);
        assert!(d.toast().0.contains(NOT_YET), "{}", d.toast().0);
    }

    #[test]
    fn resetting_ssh_access_asks_first_and_says_it_is_not_there_yet() {
        let mut c = console();
        c.act("reset-ssh");
        assert_eq!(c.overlay(), Overlay::Dialog);
        assert!(c.dialog.as_ref().unwrap().danger);
        c.act("dialog-close");
        assert_eq!(c.overlay(), Overlay::None, "cancelled, nothing happens");
        c.act("reset-ssh");
        c.act("dialog-yes");
        assert!(c.toast().0.contains(NOT_YET), "{}", c.toast().0);
        let mut s = console_over(MockBackend::new(), Box::new(Sample::default()));
        s.act("reset-ssh");
        s.act("dialog-yes");
        assert_eq!(s.toast().0, "Every SSH key was removed. Add a new one in the web admin.");
    }

    #[test]
    fn the_network_check_goes_step_by_step_and_says_what_it_cannot_check() {
        let mut c = console();
        c.act("page:2");
        let view = c.network_view();
        let steps: Vec<(String, i32)> = view.steps.iter().map(|s| (s.name.to_string(), s.kind)).collect();
        assert_eq!(steps[1], ("Address".to_owned(), 1));
        assert_eq!(steps[2].1, 0, "the router is not checked yet: {steps:?}");
        assert!(view.text.contains(NOT_YET));
        c.act("net-public-dns");
        assert!(c.toast().0.contains(NOT_YET));
        // With the checks there, a name server that does not answer is fixed from here.
        let mut s = console_over(MockBackend::new(), Box::new(Sample::default()));
        s.act("page:2");
        assert_eq!(s.network_view().title.as_str(), "Your router's name server isn't answering");
        s.act("net-public-dns");
        let fixed = s.network_view();
        assert!(fixed.fixed && fixed.ok);
        assert_eq!(fixed.title.as_str(), "The internet is back");
        s.act("net-undo-dns");
        assert!(!s.network_view().fixed);
    }

    #[test]
    fn a_closed_wifi_network_is_joined_with_its_passphrase() {
        let mut c = console();
        c.act("page:3");
        c.act("wifi");
        c.drain_blocking();
        assert_eq!(c.overlay(), Overlay::Wifi);
        assert!(c.wifi_view().rows.row_count() > 0, "the sheet looked for networks by itself");
        c.act("wifi-join:neighbour");
        assert_eq!(c.overlay(), Overlay::Keyboard);
        assert_eq!(c.keyboard_view().label.as_str(), "Wi-Fi passphrase · neighbour");
        c.key("done");
        assert_eq!(c.keyboard_view().error.as_str(), "Type the passphrase of neighbour.");
        type_and_done(&mut c, "not ours at all");
        assert_eq!(c.overlay(), Overlay::Wifi);
        assert_eq!(c.network.joined(), Some("neighbour"));
    }

    #[test]
    fn screen_settings_are_kept() {
        let mut c = console();
        c.act("display-off:0");
        assert_eq!(c.prefs.display_off(), Some(Duration::from_secs(60)));
        c.act("brightness");
        assert_eq!(c.device_view().brightness.as_str(), "40%");
        c.act("logo-style:1");
        assert_eq!(c.prefs.logo_style, LogoStyle::Text);
    }

    #[test]
    fn power_is_handed_to_the_tiles_screen_and_the_pin_is_changed_over_ssh() {
        let mut c = console();
        assert_eq!(c.act("power"), Handoff::Power);
        assert!(!c.open);
        let mut d = console();
        d.act("change-pin");
        assert!(d.toast().0.contains("nodo-tui --ctl screen-pin"));
    }

    #[test]
    fn idle_returns_to_the_tiles() {
        let mut c = console();
        c.last_touch = Instant::now() - Duration::from_secs(600);
        c.poll();
        assert!(!c.open);
    }

    #[test]
    fn device_rows_split_into_a_value_and_its_whole() {
        assert_eq!(split_of("412 GB of 1.9 TB"), ("412 GB".to_owned(), "/ 1.9 TB".to_owned()));
        assert_eq!(split_of("1.2 GiB free of 2.0 TiB"), ("1.2 GiB free".to_owned(), "/ 2.0 TiB".to_owned()));
        assert_eq!(split_of("52 °C"), ("52 °C".to_owned(), String::new()));
    }
}
