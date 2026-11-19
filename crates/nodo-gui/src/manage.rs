//! Management on the touchscreen: Home, Apps, Web and System, as a thin frontend over nodo-ui-core.
//!
//! What an action does, what is asked first, what a result means, the order of steps and the
//! wording of consequences are the core's flows (`nodo_ui_core::flows`), the same ones the
//! terminal uses. This file keeps what is the screen's own: which tap means which action, the
//! pages it has and how they stack, the on-screen keyboard, the pickers and the dialog, and how
//! a page is drawn. An action calls a flow, the effects it returns go to the core's worker
//! thread (a restart or an update can take minutes), each outcome goes back to the flow, and the
//! step the flow answers is followed by `follow` and `go`.
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

use nodo_api::{ApiError, Request, Response, ScreenStatus};
use nodo_ui_core::exec::{EffectWorker, Finished};
use nodo_ui_core::flows::backup::Backups;
use nodo_ui_core::flows::install::{Review, ReviewPurpose, Search};
use nodo_ui_core::flows::logs::Logs;
use nodo_ui_core::flows::network::{Network, RadioRead};
use nodo_ui_core::flows::services::AppPage;
use nodo_ui_core::flows::settings::{PickMode, Picked, Settings};
use nodo_ui_core::flows::setup::Setup;
use nodo_ui_core::flows::unlock::Unlock;
use nodo_ui_core::flows::updates::{AutoUpdate, Rollbacks};
use nodo_ui_core::flows::web::hosting::{self, Hosting};
use nodo_ui_core::flows::web::providers::{self, KeyField, ProviderStage, Providers};
use nodo_ui_core::flows::web::public::{CgnatWay, Found, PublicConfirm};
use nodo_ui_core::flows::{self, backup, documents, drive, install, logs, settings, sources, unlock, updates, web};
use nodo_ui_core::flows::{Confirm, Dest, Notice, NoticeKind, Question, Step};
use nodo_ui_core::model::*;
use nodo_ui_core::secret::Input;
use nodo_ui_core::{words, Effect, Outcome};
use slint::{ModelRc, SharedString, VecModel};

use crate::{MButton, MCard, MChip, MDialog, MKeyboard, MPicker, MPin, MRow};

const TABS: [&str; 4] = ["Home", "Apps", "Web", "System"];
const TERMINAL: &str = "the terminal (nodo-tui over SSH)";

#[derive(Debug, Clone)]
pub enum Screen {
    Home,
    Apps,
    App(String),
    Settings(String),
    GetApps,
    About(PackageId),
    Gate(InstallGate),
    Installing,
    Sources,
    AddSource,
    KeyChanged(String),
    Web,
    Hosting(String),
    Providers,
    System,
    Updates,
    AutoUpdate,
    Undo,
    Logs(Option<String>),
    Backups,
    Network,
    SshKeys,
    Display,
}

/// A box of the new backup's form, asked for one after the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackupBox {
    Device,
    First,
    Second,
}

/// What the keyboard is editing, and what Done does with it.
#[derive(Debug, Clone)]
enum Editing {
    Field(String),
    Search,
    PickerSearch,
    SourceAddress,
    TrustName,
    DevicePassphrase,
    InstallPassphrase,
    PublicHost,
    /// The address typed back before a site goes on the internet.
    PublicConfirm,
    SshKey,
    Backup(BackupBox),
    WifiPassphrase,
    ProviderField(KeyField),
}

struct Keyboard {
    editing: Editing,
    label: String,
    text: Input,
    mono: bool,
    shift: bool,
    error: String,
}

/// The reference picker over a settings field. The list, what fits and what must be approved
/// are the core's; the words searched for are the screen's.
struct RefPicker {
    flow: settings::Picker,
    /// What was typed in the field before `$(` opened the picker: the reference goes after it.
    typed: Option<String>,
}

#[derive(Default)]
struct Pin {
    shown: bool,
    digits: String,
    wrong: bool,
    busy: bool,
}

/// What the confirming button of the dialog does.
enum Yes {
    Nothing,
    /// A question of the core's: the flow is told, and its step followed.
    Confirm(Confirm),
    SaveSettings,
    /// Lets the app read the locked setting chosen in the picker.
    Grant,
    /// Goes on to typing the address that puts the site on the internet.
    TypeHost(Box<PublicConfirm>),
}

struct Dialog {
    title: String,
    text: String,
    danger: bool,
    buttons: Vec<MButton>,
    yes: Yes,
}

/// The device passphrase being asked for, over whatever page is shown.
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

pub struct Manage {
    pub open: bool,
    tab: usize,
    stack: Vec<Screen>,
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
    picker: Option<RefPicker>,
    picker_query: String,
    pin: Pin,
    /// The action that sent the owner to unlock the device, done again once it is unlocked.
    resume: Option<String>,
    last_act: String,
    unlock: Option<UnlockPage>,
    settings: Option<Settings>,
    /// The form was asked for and answered, with or without settings.
    settings_loaded: bool,
    search: Search,
    query: String,
    detail: Option<PackageDetail>,
    install: Option<InstallSnapshot>,
    probe: Option<FeedProbe>,
    source_address: String,
    key_change: Option<KeyChange>,
    logs: Logs,
    rollbacks: Rollbacks,
    backups: Backups,
    backup_boxes: (Input, Input, Input),
    ssh: Setup,
    network: Network,
    network_asked: Instant,
    /// The updates ticked.
    checked: Vec<String>,
    auto: Option<AutoUpdate>,
    hosting: Option<Hosting>,
    /// What the last reachability check found, while its dialog is shown.
    found: Option<Found>,
    /// The confirmation whose address is being typed.
    public: Option<PublicConfirm>,
    providers: Providers,
    provider_kind: Option<Provider>,
    provider_boxes: (String, Input, Input),
    last_touch: Instant,
    started: Instant,
    last_second: u64,
    pub idle_return: Duration,
    pub prefs: crate::screen_prefs::Prefs,
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

pub fn button(id: impl AsRef<str>, label: impl AsRef<str>, kind: i32) -> MButton {
    MButton { id: shared(id), label: shared(label), kind, enabled: true, lock: false, busy: false }
}

fn row(kind: &str) -> MRow {
    MRow {
        kind: shared(kind),
        id: SharedString::new(),
        title: SharedString::new(),
        sub: SharedString::new(),
        value: SharedString::new(),
        tone: 0,
        toggle: 0,
        mono: false,
        changed: false,
        placeholder: SharedString::new(),
        chips: model(Vec::new()),
        buttons: model(Vec::new()),
        options: model(Vec::new()),
        selected: 0,
        progress: 0.0,
        enabled: true,
    }
}

fn line(title: impl AsRef<str>, sub: impl AsRef<str>) -> MRow {
    MRow { title: shared(title), sub: shared(sub), ..row("row") }
}

fn link(id: impl AsRef<str>, title: impl AsRef<str>, sub: impl AsRef<str>, value: impl AsRef<str>, tone: i32) -> MRow {
    MRow { id: shared(id), title: shared(title), sub: shared(sub), value: shared(value), tone, ..row("row") }
}

fn text(t: impl AsRef<str>, tone: i32) -> MRow {
    MRow { title: shared(t), tone, ..row("text") }
}

fn long(label: impl AsRef<str>, value: impl AsRef<str>) -> MRow {
    MRow { title: shared(label), value: shared(value), ..row("long") }
}

fn toggle(id: impl AsRef<str>, title: impl AsRef<str>, sub: impl AsRef<str>, on: bool) -> MRow {
    MRow { id: shared(id), title: shared(title), sub: shared(sub), toggle: if on { 2 } else { 1 }, ..row("row") }
}

fn buttons(list: Vec<MButton>) -> MRow {
    MRow { buttons: model(list), ..row("buttons") }
}

fn segments(id: &str, title: &str, options: &[String], selected: usize) -> MRow {
    MRow {
        id: shared(id),
        title: shared(title),
        options: model(options.iter().map(shared).collect()),
        selected: selected as i32,
        ..row("segments")
    }
}

fn card(title: impl AsRef<str>, sub: impl AsRef<str>, rows: Vec<MRow>) -> MCard {
    MCard { title: shared(title), sub: shared(sub), tone: 0, rows: model(rows) }
}

fn toned(mut c: MCard, tone: i32) -> MCard {
    c.tone = tone;
    c
}

/// An app's state in one word, for the narrow column at the right of a row. The core's
/// `words::app_state` is a sentence (`Running · up 3h 12m · …`), which that column cannot hold.
fn state_label(state: nodo_api::ServiceState) -> (&'static str, i32) {
    use nodo_api::ServiceState::*;
    match state {
        Running => ("Running", 2),
        Stopped => ("Stopped", 0),
        Failed => ("Failed", 3),
        Locked => ("Locked", 1),
        Waiting => ("Waiting", 1),
    }
}

/// The mark and tone of a check that passed, failed or could not be made.
fn check_mark(state: CheckState) -> (&'static str, i32) {
    match state {
        CheckState::Passed => ("✓", 2),
        CheckState::Failed => ("✕", 3),
        CheckState::Unknown => ("·", 0),
    }
}

/// What the toast says while the helper works on something that changes the device or takes a
/// while. Nothing for what only reads.
fn working_words(effect: &Effect) -> Option<&'static str> {
    Some(match effect {
        Effect::Start(_) => "Starting",
        Effect::Stop(_) => "Stopping",
        Effect::Restart(_) => "Restarting",
        Effect::RemovePackage(_) => "Removing",
        Effect::Unlock(_) => "Unlocking",
        Effect::ProbeFeed(_) => "Looking at the source",
        Effect::TrustFeed(_) => "Adding the source",
        Effect::RefreshFeeds => "Checking sources",
        Effect::ToggleFeed(_) => "Switching the source",
        Effect::RemoveFeed(_) => "Removing the source",
        Effect::TrustNewKey { .. } => "Trusting the new key",
        Effect::Grant { .. } => "Allowing",
        Effect::ApplySettings { .. } | Effect::SaveAutoUpdate(_) => "Saving",
        Effect::CheckUpdates => "Checking for updates",
        Effect::ApplyUpdates { .. } | Effect::ApplyUpdatesWithPassphrase { .. } => "Updating",
        Effect::Drive(_) => "Checking the drive",
        Effect::SetExposure { .. } | Effect::SetPublicHost { .. } | Effect::SetPublicAccess { .. } => "Saving hosting",
        Effect::CheckReachability { .. } => "Checking that the internet can reach it",
        Effect::AddProvider { .. } => "Connecting",
        Effect::RemoveProvider(_) => "Removing",
        Effect::AddSshKey(_) => "Adding the key",
        Effect::RemoveSshKey(_) => "Removing the key",
        Effect::CreateBackup { .. } => "Backing up",
        Effect::RollBack { .. } => "Undoing",
        _ => return None,
    })
}

/// Whether a refusal for want of an unlock is the screen's own lock, which the PIN lifts, and not
/// the device's, which the passphrase does. The helper's screen socket says so in words
/// (nodo-helper's `screen::handle_screen`: "enter the screen PIN to make changes", and "… until a
/// screen PIN is set …" before there is one), and both name the screen PIN.
fn is_screen_lock(refusal: &ApiError) -> bool {
    matches!(refusal, ApiError::Locked(reason) if reason.contains("screen PIN"))
}

/// A step without its "back to the page before": for a result that arrives after the owner has
/// left the page it was about.
fn without_back(step: Step) -> Step {
    match step {
        Step::Go(Dest::Back) => Step::Stay,
        Step::All(steps) => Step::All(steps.into_iter().map(without_back).collect()),
        other => other,
    }
}

/// The page the screen shows: breadcrumb, title, header actions and cards.
pub struct Page {
    pub crumbs: Vec<String>,
    pub title: String,
    pub actions: Vec<MButton>,
    pub cards: Vec<MCard>,
}

impl Manage {
    pub fn new(worker: Option<EffectWorker>) -> Self {
        let now = Instant::now();
        Self {
            version: 0,
            offline: None,
            open: false,
            tab: 0,
            stack: vec![Screen::Home],
            snapshot: None,
            screen: ScreenStatus { pin_set: false, unlocked: false },
            worker,
            sent: VecDeque::new(),
            retry: None,
            toast: None,
            dialog: None,
            keyboard: None,
            picker: None,
            picker_query: String::new(),
            pin: Pin::default(),
            resume: None,
            last_act: String::new(),
            unlock: None,
            settings: None,
            settings_loaded: false,
            search: Search::default(),
            query: String::new(),
            detail: None,
            install: None,
            probe: None,
            source_address: String::new(),
            key_change: None,
            logs: Logs::device(),
            rollbacks: Rollbacks::new(),
            backups: Backups::new(true),
            backup_boxes: (Input::secret(), Input::secret(), Input::secret()),
            ssh: Setup::keys(),
            network: Network::new(),
            network_asked: now,
            checked: Vec::new(),
            auto: None,
            hosting: None,
            found: None,
            public: None,
            providers: Providers::list(),
            provider_kind: None,
            provider_boxes: (String::new(), Input::secret(), Input::secret()),
            last_touch: now,
            started: now,
            last_second: 0,
            idle_return: Duration::from_secs(120),
            prefs: crate::screen_prefs::Prefs::default(),
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
        if self.open && self.last_touch.elapsed() > self.idle_return && self.keyboard.is_none() && !self.pin.shown {
            self.close();
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

    /// What the page on top reads again by itself: the core says what and how often, and the
    /// screen keeps the clock.
    fn every_second(&mut self, clock: u64) {
        let effects = match self.top().clone() {
            Screen::Network => {
                let before = (self.network.wifi_read.clone(), self.network.bluetooth_read.clone());
                self.network.no_answer_after(self.network_asked.elapsed().as_secs());
                if before != (self.network.wifi_read.clone(), self.network.bluetooth_read.clone()) {
                    self.version += 1;
                }
                self.network.poll(clock)
            }
            Screen::Installing => match (&self.install, &self.snapshot) {
                (Some(report), Some(snapshot)) => install::poll(report, &snapshot.capabilities),
                _ => Vec::new(),
            },
            Screen::Logs(Some(source)) if clock % logs::FOLLOW_SECONDS == 0 => self.logs.follow_up(Some(&source)),
            _ => Vec::new(),
        };
        // The helper does one thing at a time, so reading again never queues behind other work.
        if self.idle() {
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
                    self.ask_pin();
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
                        self.pin = Pin::default();
                        if let Some(refused) = self.retry.take() {
                            self.submit(refused.effects);
                        }
                    }
                    Err(ApiError::NotSetUp) => {
                        self.pin = Pin::default();
                        self.screen.pin_set = false;
                        self.ask_pin();
                    }
                    _ => self.pin.wrong = true,
                }
            }
            Asked::Lock => match reply {
                Ok(_) => self.screen.unlocked = false,
                // Nobody may be looking at management any more, so the log is the one to hear of it.
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
        let top = self.top().clone();
        match &outcome {
            Outcome::Refreshed(snapshot) => {
                self.refreshed(snapshot.as_ref().clone());
                Step::Stay
            }
            Outcome::Failed(_) | Outcome::Info(_) => flows::apply(&outcome),
            Outcome::UnlockResult(_) => {
                let (Some(page), Some(snapshot)) = (&mut self.unlock, &mut self.snapshot) else { return Step::Stay };
                let failures = page.flow.failures;
                let step = page.flow.apply(&outcome, snapshot);
                if page.flow.failures > failures {
                    page.retry_at = Some(Instant::now() + Duration::from_secs(page.flow.retry_delay_seconds()));
                    self.ask_passphrase();
                }
                step
            }
            Outcome::SshKeysLoaded(_) | Outcome::SshKeyChanged(_) => self.ssh.apply(&outcome),
            Outcome::BackupsLoaded(_)
            | Outcome::BackupCreated(_)
            | Outcome::BackupInspected(_)
            | Outcome::BackupRestored(_)
            | Outcome::UsbLoaded(_) => self.backups.apply(&outcome),
            Outcome::RollbacksLoaded(_) | Outcome::RolledBack(_) => self.rollbacks.apply(&outcome),
            Outcome::NetworkLoaded { .. } | Outcome::WifiChanged { .. } | Outcome::WifiScanned(_) | Outcome::BluetoothChanged { .. } => {
                let step = self.network.apply(&outcome);
                // The page has no button to look for networks: it looks once Wi-Fi is on and
                // nothing is listed yet.
                let changed = matches!(outcome, Outcome::NetworkLoaded { .. } | Outcome::WifiChanged { quiet: false, .. });
                let on = self.network.wifi.as_ref().is_some_and(|wifi| wifi.enabled && wifi.unavailable.is_none());
                if changed && on && self.network.networks.is_empty() && matches!(top, Screen::Network) {
                    let look = self.network.scan_wifi();
                    return Step::All(vec![step, Step::run(look)]);
                }
                step
            }
            Outcome::DetailLoaded(detail) => {
                // The page that waited for the details has nothing to show.
                if detail.is_none() && matches!(top, Screen::About(_)) {
                    self.pop();
                }
                install::apply(&outcome)
            }
            Outcome::GateLoaded(_) | Outcome::UpdateReviewLoaded { .. } => install::apply(&outcome),
            Outcome::InstallProgress(Some(report)) => {
                let Some(snapshot) = &self.snapshot else { return Step::Stay };
                let on_install_page = match &top {
                    Screen::Installing => true,
                    Screen::Gate(InstallGate::Review(_)) => report.outcome != InstallOutcome::Cancelled,
                    _ => false,
                };
                install::progress(report, &snapshot.capabilities, on_install_page)
            }
            Outcome::InstallProgress(None) => Step::Stay,
            Outcome::UpdatesChecked(_) | Outcome::UpdatesApplied(_) => match &self.snapshot {
                Some(snapshot) => updates::apply(&outcome, snapshot),
                None => Step::Stay,
            },
            Outcome::AutoUpdateSaved(_) => match (&mut self.auto, &self.snapshot) {
                (Some(auto), _) if matches!(top, Screen::AutoUpdate) => auto.apply(&outcome),
                (_, Some(snapshot)) => updates::apply(&outcome, snapshot),
                _ => Step::Stay,
            },
            Outcome::FeedProbed(_) | Outcome::KeyChangeLoaded(_) => sources::apply(&outcome),
            Outcome::FeedTrusted(_) if matches!(top, Screen::AddSource) => sources::apply(&outcome),
            Outcome::NewKeyTrusted(_) if matches!(top, Screen::KeyChanged(_)) => sources::apply(&outcome),
            Outcome::FeedTrusted(_) | Outcome::NewKeyTrusted(_) => without_back(sources::apply(&outcome)),
            Outcome::SettingsLoaded { package, form } => {
                if form.is_none() && matches!(&top, Screen::Settings(shown) if shown == package) {
                    self.settings_loaded = true;
                }
                settings::apply(&outcome)
            }
            Outcome::ReferencesLoaded { .. } | Outcome::SettingPreviewed { .. } => {
                if let Some(form) = &mut self.settings {
                    // A list or a preview only changes what the form shows.
                    drop(form.apply(&outcome));
                }
                if let Some(picker) = &mut self.picker {
                    let picked = picker.flow.apply(&outcome);
                    self.picked(picked);
                }
                Step::Stay
            }
            Outcome::Granted { .. } => match &mut self.picker {
                Some(picker) => {
                    let picked = picker.flow.apply(&outcome);
                    self.picked(picked);
                    Step::Stay
                }
                None => settings::apply(&outcome),
            },
            Outcome::SettingsApplied(_) | Outcome::SettingsRolledBack { .. } => match &mut self.settings {
                Some(form) => form.apply(&outcome),
                None => Step::Stay,
            },
            Outcome::LogsLoaded { .. } => match &top {
                Screen::Logs(Some(source)) => self.logs.apply(&outcome, Some(source)),
                _ => Step::Stay,
            },
            Outcome::DriveResult(_) => drive::apply(&outcome),
            Outcome::ExposureSet { .. } | Outcome::PublicHostSet { .. } | Outcome::Reachability { .. } => web::apply(&outcome),
            Outcome::ProviderAdded(_) => {
                // Nothing typed is kept, whatever the helper said about the key.
                self.clear_provider_boxes();
                self.providers.apply(&outcome)
            }
            Outcome::ProvidersLoaded(result) => {
                if let Some(snapshot) = &mut self.snapshot {
                    let flow = matches!(top, Screen::Providers).then_some(&self.providers);
                    providers::catalog_loaded(snapshot, flow, result);
                }
                Step::Stay
            }
            Outcome::ProviderRemoved(_) | Outcome::ProviderRefreshed(_) => providers::apply(&outcome),
            Outcome::AcmeSaved(_) | Outcome::LocalNamesChecked(_) => web::domain::apply(&outcome),
            Outcome::DocumentLoaded(_) => documents::apply(&outcome),
            // The screen has no page that asks for these: first setup, onion addresses, the
            // first-boot restore, bundles and setup pages are done in the terminal.
            Outcome::SetupDone(_)
            | Outcome::OnionLoaded(_)
            | Outcome::OnionChanged(_)
            | Outcome::RestoreDrives(_)
            | Outcome::RestoreBackups(_)
            | Outcome::RestoreInspected(_)
            | Outcome::RestoreFinished(_)
            | Outcome::BundleInstalled(_)
            | Outcome::OwnerPageLoaded { .. }
            // The web GUI's switch is not offered on the screen: the helper refuses it here.
            | Outcome::WebGuiSet { .. } => Step::Stay,
        }
    }

    /// The device was read again. The ticks on the Updates page follow what is offered now.
    fn refreshed(&mut self, snapshot: Snapshot) {
        let shown: Vec<String> =
            self.snapshot.as_ref().map(|old| old.update_rows().iter().map(|item| item.name.clone()).collect()).unwrap_or_default();
        if self.snapshot.is_some() && self.stack.iter().any(|screen| matches!(screen, Screen::Updates)) {
            updates::carry_ticks(&mut self.checked, &shown, &snapshot);
        }
        self.snapshot = Some(snapshot);
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

    /// Maps a destination of the core to this screen's pages. What the touchscreen has no page
    /// for is said to be done in the terminal.
    fn go(&mut self, dest: Dest) {
        match dest {
            Dest::Back => self.pop(),
            Dest::Apps => self.go_tab(1),
            Dest::Updates { only } => {
                self.checked = self.snapshot.as_ref().map(|s| updates::ticked_at_open(s, only.as_deref())).unwrap_or_default();
                self.stack.push(Screen::Updates);
            }
            Dest::Unlock { resume } => {
                self.resume = resume.is_some().then(|| self.last_act.clone());
                self.unlock = Some(UnlockPage { flow: Unlock::new(resume), retry_at: None });
                self.ask_passphrase();
            }
            Dest::Resume => {
                self.pop();
                if let Some(id) = self.resume.take() {
                    self.act(&id);
                }
            }
            Dest::GetApps { source } => {
                self.push(Screen::GetApps);
                self.search.feed_filter = source;
            }
            Dest::ConfirmSource(probe) => self.probe = Some(probe),
            Dest::KeyChange(change) => {
                self.stack.push(Screen::KeyChanged(change.feed.clone()));
                self.key_change = Some(change);
            }
            Dest::Sources { select: _ } => {
                if !matches!(self.top(), Screen::Sources) {
                    self.push(Screen::Sources);
                }
            }
            Dest::AppPage(name) => {
                self.tab = 1;
                self.stack = vec![Screen::Apps, Screen::App(name)];
            }
            Dest::Detail(detail) => {
                if !matches!(self.top(), Screen::About(id) if *id == detail.id) {
                    self.stack.push(Screen::About(detail.id.clone()));
                }
                self.detail = Some(detail);
            }
            Dest::Review(review) => match &review.purpose {
                ReviewPurpose::Install => self.stack.push(Screen::Gate(InstallGate::Review(review.data))),
                // Never applied from here: an update that asks for more is approved where what
                // it asks for can be read in full.
                ReviewPurpose::Update { .. } => self.say(
                    &format!(
                        "{} asks for a new permission. Review and approve its update in {TERMINAL}. Nothing was updated.",
                        review.data.id.name
                    ),
                    false,
                ),
            },
            // A bundle asks questions, some of them secrets to paste: its page says where.
            Dest::Bundle(review) => self.stack.push(Screen::Gate(InstallGate::Bundle(*review))),
            Dest::OwnerPage(_) => self.in_terminal("An app's setup page"),
            Dest::Rejected(data) => self.stack.push(Screen::Gate(InstallGate::Rejected(data))),
            Dest::MissingCapability(data) => self.stack.push(Screen::Gate(InstallGate::MissingCapability(data))),
            // The install's page takes the place of the review that started it.
            Dest::Installing(report) => {
                self.install = Some(report);
                match self.top() {
                    Screen::Installing => {}
                    Screen::Gate(_) => {
                        self.stack.pop();
                        self.stack.push(Screen::Installing);
                    }
                    _ => self.stack.push(Screen::Installing),
                }
            }
            // Never applied without it: the batch waits for the terminal, where it is asked for.
            Dest::UpdatePassphrase(batch) => self.say(
                &format!(
                    "This needs the device passphrase, to grow encrypted storage ({}). Update it in {TERMINAL}. Nothing was updated.",
                    batch.growing.join("; ")
                ),
                false,
            ),
            Dest::UpdateResults(report) => {
                let updated: Vec<&str> = report.results.iter().filter(|r| r.applied).map(|r| r.name.as_str()).collect();
                match &report.failure {
                    Some(failure) => self.say(&format!("{}: {}", failure.name, failure.headline), true),
                    None if updated.is_empty() => self.say("Nothing was updated.", false),
                    None => self.say(&format!("Updated {}.", updated.join(", ")), false),
                }
            }
            Dest::Settings(form) => {
                // A finished install is replaced, so going back does not return to its steps.
                if matches!(self.top(), Screen::Installing) {
                    self.stack.pop();
                }
                if !matches!(self.top(), Screen::Settings(package) if *package == form.package) {
                    self.stack.push(Screen::Settings(form.package.clone()));
                }
                self.settings = Some(Settings::new(form));
                self.settings_loaded = true;
            }
            Dest::ApplyFailed(failed) => {
                let text = format!("{}\n\n{}", failed.explanation.headline, failed.explanation.hint);
                self.dialog("The new settings were undone", &text, true, vec![button("dialog-close", "Edit again", 0)]);
            }
            Dest::Document(document) => {
                const SHOWN: usize = 12;
                let mut lines: Vec<String> = document.lines.iter().take(SHOWN).cloned().collect();
                if document.lines.len() > SHOWN {
                    lines.push(format!("… The rest can be read in {TERMINAL}."));
                }
                self.dialog(&document.title, &lines.join("\n"), false, vec![button("dialog-close", "OK", 0)]);
            }
            Dest::LeaveHosting { site: _ } => {
                if matches!(self.top(), Screen::Hosting(_)) {
                    self.pop();
                }
            }
            Dest::Reachability(found) => self.show_found(found),
            Dest::PublicConfirm(confirm) => self.ask_public(confirm),
            Dest::HostingWithProvider { domains } => {
                if matches!(self.top(), Screen::Providers) {
                    self.stack.pop();
                }
                if let Some(hosting) = &mut self.hosting {
                    hosting.provider_connected(&domains);
                }
            }
            Dest::SetUpFromBackup => self.in_terminal("Setting the device up from a backup"),
            Dest::WentPublic { site: _ } => {
                if matches!(self.top(), Screen::Hosting(_)) {
                    self.pop();
                }
                self.say("It is on the internet now.", false);
            }
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
            Some(Hint::Unlock) => " Unlock the device from Home first.",
            // The switch and the list are on the page the message is shown on.
            Some(Hint::TurnOn | Hint::ScanWifi | Hint::ScanBluetooth | Hint::LookForDrives | Hint::Install) | None => "",
        };
        self.say(&format!("{}{how}", notice.text), notice.kind == NoticeKind::Error);
    }

    fn in_terminal(&mut self, what: &str) {
        self.say(&format!("{what} is done in {TERMINAL}."), false);
    }

    fn dialog(&mut self, title: &str, text: &str, danger: bool, list: Vec<MButton>) {
        self.dialog = Some(Dialog { title: title.to_owned(), text: text.to_owned(), danger, buttons: list, yes: Yes::Nothing });
    }

    /// A question of the core's, in the dialog. Where the answer starts on Cancel, Cancel is the
    /// button that stands out.
    fn ask(&mut self, question: Question) {
        let (cancel, confirm) = if question.cancel_is_default { (0, 2) } else { (1, 0) };
        self.dialog = Some(Dialog {
            title: question.title,
            text: question.body.join("\n"),
            danger: question.cancel_is_default,
            buttons: vec![button("dialog-close", "Cancel", cancel), button("dialog-yes", question.confirm_label, confirm)],
            yes: Yes::Confirm(question.confirm),
        });
    }

    fn dialog_confirmed(&mut self) {
        let Some(dialog) = self.dialog.take() else { return };
        match dialog.yes {
            Yes::Nothing => {}
            Yes::Confirm(confirm) => {
                // The flow of the page that asked is told first, so it shows the work as running.
                match self.top() {
                    Screen::Undo => self.rollbacks.confirmed(&confirm),
                    Screen::Network => self.network.confirmed(&confirm),
                    Screen::SshKeys => self.ssh.confirmed(&confirm),
                    _ => {}
                }
                let step = self.snapshot.as_ref().map_or(Step::Stay, |snapshot| confirm.step(snapshot));
                self.follow(step);
            }
            Yes::SaveSettings => {
                let effects = self.settings.as_mut().map(Settings::confirm_save).unwrap_or_default();
                self.submit(effects);
            }
            Yes::Grant => {
                let effects = self.picker.as_ref().map(|picker| picker.flow.approve()).unwrap_or_default();
                self.submit(effects);
            }
            Yes::TypeHost(confirm) => {
                let label = format!("Type {} to put it on the internet", confirm.plan.host);
                self.public = Some(*confirm);
                self.keyboard(Editing::PublicConfirm, &label, Input::plain(""), true);
            }
        }
    }

    fn dialog_closed(&mut self) {
        let Some(dialog) = self.dialog.take() else { return };
        match dialog.yes {
            Yes::SaveSettings => {
                if let Some(form) = &mut self.settings {
                    form.cancel_save();
                }
            }
            Yes::Grant => {
                if let Some(picker) = &mut self.picker {
                    picker.flow.decline();
                }
            }
            Yes::Nothing | Yes::Confirm(_) | Yes::TypeHost(_) => {}
        }
        self.found = None;
    }

    /// The PIN pad, for a change the screen's lock refused or for the Unlock button; without a
    /// PIN set, how to set one.
    fn ask_pin(&mut self) {
        if !self.screen.pin_set {
            self.give_up_retry();
            self.dialog(
                "Set a screen PIN first",
                "The screen can only look until a screen PIN is set. Set one over SSH with \
                 `nodo-tui --ctl screen-pin` (it asks for the device passphrase, then the PIN).",
                false,
                vec![button("dialog-close", "OK", 0)],
            );
            return;
        }
        // A second refusal while the pad is up keeps the digits typed so far.
        if !self.pin.shown {
            self.pin = Pin { shown: true, ..Pin::default() };
        }
    }

    /// Asks for the device passphrase, with why the last try was refused.
    fn ask_passphrase(&mut self) {
        let Some(page) = &self.unlock else { return };
        let label = match &page.flow.resume {
            Some(why) => format!("Device passphrase. {why}"),
            None => "Device passphrase".to_owned(),
        };
        let error = page.flow.error.clone().unwrap_or_default();
        self.keyboard(Editing::DevicePassphrase, &label, Input::secret(), false);
        if let Some(keyboard) = &mut self.keyboard {
            keyboard.error = error;
        }
    }

    fn keyboard(&mut self, editing: Editing, label: &str, text: Input, mono: bool) {
        self.keyboard = Some(Keyboard { editing, label: label.to_owned(), text, mono, shift: false, error: String::new() });
    }

    // ---------------------------------------------------------------------------------------
    // Pages and how they stack.

    pub fn touched(&mut self) {
        self.last_touch = Instant::now();
        self.version += 1;
    }

    /// The helper is out of reach: management says so instead of showing anything.
    pub fn set_offline(&mut self, why: String) {
        self.offline = Some(why);
    }

    pub fn open(&mut self) {
        self.open = true;
        self.touched();
        self.tab = 0;
        self.stack = vec![Screen::Home];
        self.reload();
    }

    /// Opens straight onto System › Screen (the logo tile's options, from arranging).
    pub fn open_at_screen_settings(&mut self) {
        self.open();
        self.tab = 3;
        self.stack = vec![Screen::System, Screen::Display];
    }

    pub fn close(&mut self) {
        self.open = false;
        self.keyboard = None;
        self.picker = None;
        self.dialog = None;
        self.unlock = None;
        self.give_up_retry();
        self.pin = Pin::default();
        self.call(Request::ScreenLock, Asked::Lock);
    }

    /// Reads the device, the screen's lock and whatever the current page needs.
    pub fn reload(&mut self) {
        self.submit(vec![Effect::Refresh]);
        self.call(Request::ScreenStatus, Asked::Status);
        self.load_screen();
    }

    fn top(&self) -> &Screen {
        self.stack.last().unwrap_or(&Screen::Home)
    }

    fn push(&mut self, screen: Screen) {
        self.stack.push(screen);
        self.load_screen();
    }

    /// Back to the page that opened the one shown. The device passphrase is asked over a page,
    /// so with it open that is what is left.
    fn pop(&mut self) {
        if self.unlock.take().is_some() {
            if matches!(self.keyboard.as_ref().map(|k| &k.editing), Some(Editing::DevicePassphrase)) {
                self.keyboard = None;
            }
            return;
        }
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    fn go_tab(&mut self, tab: usize) {
        self.tab = tab.min(3);
        self.stack = vec![match self.tab {
            0 => Screen::Home,
            1 => Screen::Apps,
            2 => Screen::Web,
            _ => Screen::System,
        }];
        self.load_screen();
    }

    /// What the flow of the page on top says about leaving it: a question first when there are
    /// unsaved changes, and what to read again.
    fn leaving(&self) -> Step {
        match self.top() {
            Screen::Settings(_) => self.settings.as_ref().map_or(Step::Go(Dest::Back), Settings::leave),
            Screen::AutoUpdate => self.auto.as_ref().map_or(Step::Go(Dest::Back), AutoUpdate::leave),
            Screen::Undo => self.rollbacks.leave(),
            Screen::Backups => self.backups.leave(),
            Screen::Network => self.network.leave(),
            Screen::SshKeys => self.ssh.finish(),
            Screen::Installing => self.install.as_ref().map_or(Step::Go(Dest::Back), install::leave),
            _ => Step::Go(Dest::Back),
        }
    }

    /// Leaves the page on top for one further away (a tab or a breadcrumb). False when the owner
    /// is asked first; nothing has changed then, and confirming goes back one page.
    fn leave_for_elsewhere(&mut self) -> bool {
        match self.leaving() {
            Step::Ask(question) => {
                self.ask(question);
                false
            }
            step => {
                self.submit(step.effects());
                true
            }
        }
    }

    /// Starts the page on top: its flow as it opens, and what that reads first.
    fn load_screen(&mut self) {
        let effects = match self.top().clone() {
            Screen::Settings(package) => {
                self.settings = None;
                self.settings_loaded = false;
                settings::open(&package)
            }
            Screen::GetApps => {
                self.search.clear();
                Vec::new()
            }
            Screen::About(id) => {
                self.detail = None;
                install::detail(&id)
            }
            Screen::AddSource => {
                self.probe = None;
                self.source_address.clear();
                Vec::new()
            }
            Screen::Logs(Some(source)) => {
                self.logs.view = LogView::default();
                Logs::load(&source)
            }
            Screen::Logs(None) => {
                self.logs = Logs::device();
                Vec::new()
            }
            Screen::Undo => {
                self.rollbacks = Rollbacks::new();
                Rollbacks::load()
            }
            Screen::Backups => {
                self.backups = self.snapshot.as_ref().map_or_else(|| Backups::new(true), Backups::of);
                Backups::load()
            }
            Screen::SshKeys => {
                self.ssh = Setup::keys();
                Setup::load_keys()
            }
            Screen::Network => {
                self.network = Network::new();
                self.network_asked = Instant::now();
                self.network.ask_again()
            }
            Screen::AutoUpdate => {
                self.auto = self.snapshot.as_ref().map(|s| AutoUpdate::new(s.auto_update.clone()));
                Vec::new()
            }
            Screen::Updates => {
                self.checked = self.snapshot.as_ref().map(|s| updates::ticked_at_open(s, None)).unwrap_or_default();
                Vec::new()
            }
            Screen::Providers => {
                self.providers = Providers::list();
                self.provider_kind = None;
                self.clear_provider_boxes();
                Vec::new()
            }
            _ => Vec::new(),
        };
        self.submit(effects);
    }

    // ---------------------------------------------------------------------------------------
    // Input from the window.

    pub fn tab_clicked(&mut self, tab: usize) {
        self.touched();
        if self.leave_for_elsewhere() {
            self.go_tab(tab);
        }
    }

    pub fn crumb(&mut self, index: usize) {
        self.touched();
        if self.leave_for_elsewhere() {
            self.stack.truncate(index + 1);
            self.load_screen();
        }
    }

    pub fn segment(&mut self, id: &str, n: usize) {
        self.touched();
        if id == "auto-frequency" {
            if let Some(auto) = &mut self.auto {
                if let Some(frequency) = auto.draft.frequencies.get(n).cloned() {
                    auto.set_frequency(&frequency);
                }
            }
        } else if id == "screen-timeout" {
            self.prefs.timeout_index = n;
            self.idle_return = self.prefs.timeout();
            self.prefs.save();
        } else if id == "screen-brightness" {
            self.prefs.brightness_index = n;
            self.prefs.save();
        } else if id == "screen-dim" {
            self.prefs.dim_index = n;
            self.prefs.save();
        } else if id == "logo-style" {
            self.prefs.logo_style = if n == 1 { crate::screen_prefs::LogoStyle::Text } else { crate::screen_prefs::LogoStyle::Drawn };
            self.prefs.save();
        } else if let Some(key) = id.strip_prefix("choice:") {
            let Some(form) = &mut self.settings else { return };
            let Some(index) = form.index_of(key) else { return };
            let FieldKind::Choice(options) = &form.form.fields[index].kind else { return };
            if let Some(option) = options.get(n).cloned() {
                let effects = form.set_value(index, FieldValue::Text(option));
                self.submit(effects);
            }
        }
    }

    pub fn toggled(&mut self, id: &str, on: bool) {
        self.touched();
        let (what, rest) = id.split_once(':').unwrap_or((id, ""));
        match what {
            "bool" => {
                let Some(form) = &mut self.settings else { return };
                let Some(index) = form.index_of(rest) else { return };
                let effects = form.set_value(index, FieldValue::Bool(on));
                self.submit(effects);
            }
            "feed" => self.submit(sources::toggle(rest)),
            "update" => {
                let Some(snapshot) = &self.snapshot else { return };
                if self.checked.iter().any(|name| name == rest) != on {
                    let effects = updates::tick(&mut self.checked, snapshot, rest);
                    self.submit(effects);
                }
            }
            "auto" => {
                if let Some(auto) = &mut self.auto {
                    auto.set_enabled(rest, on);
                }
            }
            "expose" => {
                if let Some(hosting) = &mut self.hosting {
                    let mut draft = hosting.draft;
                    match rest {
                        "local" => draft.local = on,
                        "onion" => draft.onion = on,
                        _ => draft.public = on,
                    }
                    hosting.set_exposure(draft.local, draft.onion, draft.public);
                }
            }
            "problems" => {
                if self.logs.errors_only != on {
                    self.logs.toggle_errors_only();
                }
            }
            "logo-shimmer" => {
                self.prefs.logo_shimmer = on;
                self.prefs.save();
            }
            "logo-name" => {
                self.prefs.logo_name = on;
                self.prefs.save();
            }
            "wifi" => {
                let step = self.network.toggle_wifi();
                self.follow(step);
            }
            _ => {}
        }
    }

    pub fn key(&mut self, k: &str) {
        self.touched();
        if let Some(digit) = k.strip_prefix("pin:") {
            self.pin_key(digit);
            return;
        }
        let Some(kb) = &mut self.keyboard else { return };
        match k {
            "shift" => kb.shift = !kb.shift,
            "back" => kb.text.pop(),
            "cancel" => self.keyboard_cancelled(),
            "done" => self.keyboard_done(),
            "paste" => {}
            t => {
                let t = if kb.shift { t.to_uppercase() } else { t.to_owned() };
                t.chars().for_each(|c| kb.text.push(c));
                kb.shift = false;
                match &kb.editing {
                    // `$(` starts a reference: the picker opens on what was typed so far.
                    Editing::Field(key) if kb.text.text().ends_with("$(") => {
                        let key = key.clone();
                        kb.text.pop();
                        kb.text.pop();
                        let typed = kb.text.text().to_owned();
                        self.open_references(&key, PickMode::Insert, Some(typed));
                    }
                    Editing::PickerSearch => self.picker_query = kb.text.text().to_owned(),
                    _ => {}
                }
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
            d if d.len() == 1 && self.pin.digits.len() < 6 => {
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
        match kb.editing {
            Editing::DevicePassphrase => {
                self.unlock = None;
                self.resume = None;
            }
            Editing::WifiPassphrase => drop(self.network.cancel_prompt()),
            Editing::Backup(_) => {
                self.backups.cancel();
                self.clear_backup_boxes();
            }
            Editing::PublicConfirm => self.public = None,
            _ => {}
        }
    }

    /// Done on the keyboard: what was typed goes to the flow it was typed for. Where the flow
    /// refuses it, the keyboard stays with the reason under the text.
    fn keyboard_done(&mut self) {
        let Some(mut kb) = self.keyboard.take() else { return };
        let refused = match kb.editing.clone() {
            Editing::Field(key) => {
                let Some(form) = &mut self.settings else { return };
                let Some(index) = form.index_of(&key) else { return };
                if form.form.fields[index].kind == FieldKind::Secret {
                    form.set_secret(index, &mut kb.text);
                } else {
                    let effects = form.set_value(index, FieldValue::Text(kb.text.text().to_owned()));
                    self.submit(effects);
                }
                None
            }
            Editing::Search => {
                self.query = kb.text.text().to_owned();
                None
            }
            Editing::PickerSearch => {
                self.picker_query = kb.text.text().to_owned();
                None
            }
            Editing::SourceAddress => match sources::probe(kb.text.text()) {
                Ok(effects) => {
                    self.source_address = kb.text.text().to_owned();
                    self.probe = None;
                    self.submit(effects);
                    None
                }
                Err(why) => Some(why),
            },
            Editing::TrustName => {
                let Some(change) = &self.key_change else { return };
                match sources::trust_new_key(change, kb.text.text()) {
                    Ok(effects) => {
                        self.submit(effects);
                        None
                    }
                    Err(why) => Some(why),
                }
            }
            Editing::DevicePassphrase => {
                let Some(page) = &mut self.unlock else { return };
                let wait = page.retry_at.map_or(0, |at| at.saturating_duration_since(Instant::now()).as_secs_f32().ceil() as u64);
                let effects = page.flow.submit(&mut kb.text, wait);
                if effects.is_empty() {
                    page.flow.error.clone()
                } else {
                    self.submit(effects);
                    None
                }
            }
            Editing::InstallPassphrase => {
                let (Screen::Gate(InstallGate::Review(data)), Some(snapshot)) = (self.top().clone(), &self.snapshot) else { return };
                match Review::new(data, ReviewPurpose::Install).approve(Some(&mut kb.text), snapshot) {
                    Step::Failed(notice) => Some(notice.text),
                    step => {
                        self.follow(step);
                        None
                    }
                }
            }
            Editing::PublicHost => {
                let (Some(hosting), Some(snapshot)) = (&mut self.hosting, &self.snapshot) else { return };
                hosting.set_host_text(kb.text.text(), snapshot).err()
            }
            Editing::PublicConfirm => {
                let (Some(confirm), Some(snapshot)) = (&self.public, &self.snapshot) else { return };
                match confirm.confirm(kb.text.text(), snapshot) {
                    Ok(effects) => {
                        self.public = None;
                        self.submit(effects);
                        None
                    }
                    Err(why) => Some(why),
                }
            }
            Editing::SshKey => {
                let effects = self.ssh.add_key(kb.text.text());
                self.submit(effects);
                None
            }
            Editing::Backup(filled) => {
                let typed = std::mem::replace(&mut kb.text, Input::secret());
                match filled {
                    BackupBox::Device => self.backup_boxes.0 = typed,
                    BackupBox::First => self.backup_boxes.1 = typed,
                    BackupBox::Second => self.backup_boxes.2 = typed,
                }
                match filled {
                    BackupBox::Device => self.ask_backup_box(BackupBox::First, ""),
                    BackupBox::First => self.ask_backup_box(BackupBox::Second, ""),
                    BackupBox::Second => self.create_backup(),
                }
                return;
            }
            Editing::WifiPassphrase => {
                let effects = self.network.submit_passphrase(&mut kb.text);
                if effects.is_empty() {
                    self.network.error.clone()
                } else {
                    self.submit(effects);
                    None
                }
            }
            Editing::ProviderField(field) => {
                match field {
                    KeyField::Domain => self.provider_boxes.0 = kb.text.text().to_owned(),
                    KeyField::ApiKey => self.provider_boxes.1 = std::mem::replace(&mut kb.text, Input::secret()),
                    KeyField::SecretKey => self.provider_boxes.2 = std::mem::replace(&mut kb.text, Input::secret()),
                }
                None
            }
        };
        if let Some(why) = refused {
            kb.error = why;
            self.keyboard = Some(kb);
        }
    }

    // A new backup: the boxes of its form, one after the other on the keyboard.

    fn clear_backup_boxes(&mut self) {
        self.backup_boxes = (Input::secret(), Input::secret(), Input::secret());
    }

    fn ask_backup_box(&mut self, which: BackupBox, error: &str) {
        let label = match which {
            BackupBox::Device => "Device passphrase, so the backup includes the device key".to_owned(),
            BackupBox::First => format!("A passphrase for this backup, at least {} characters", backup::MIN_BACKUP_PASSPHRASE_CHARS),
            BackupBox::Second => "The backup's passphrase again".to_owned(),
        };
        self.keyboard(Editing::Backup(which), &label, Input::secret(), false);
        if let Some(keyboard) = &mut self.keyboard {
            keyboard.error = error.to_owned();
        }
    }

    /// Every box is filled: the core checks them and makes the backup, or says which to type again.
    fn create_backup(&mut self) {
        let (device, first, second) = &mut self.backup_boxes;
        match self.backups.create(device, first, second) {
            Ok(effects) => {
                self.clear_backup_boxes();
                self.submit(effects);
            }
            Err(again) => {
                let why = self.backups.error.clone().unwrap_or_default();
                match again {
                    Some(backup::Field::DevicePassphrase) => self.ask_backup_box(BackupBox::Device, &why),
                    _ => {
                        self.backup_boxes.1.clear();
                        self.backup_boxes.2.clear();
                        self.ask_backup_box(BackupBox::First, &why);
                    }
                }
            }
        }
    }

    fn clear_provider_boxes(&mut self) {
        self.provider_boxes = (String::new(), Input::secret(), Input::secret());
    }

    // References in a settings field.

    /// Opens the reference picker for a field, as the core allows it.
    fn open_references(&mut self, key: &str, mode: PickMode, typed: Option<String>) {
        let Some(form) = &self.settings else { return };
        let Some(index) = form.index_of(key) else { return };
        match form.pick(index, mode, typed.as_ref().is_some_and(|text| !text.is_empty())) {
            Ok((flow, load)) => {
                self.keyboard = None;
                self.picker_query.clear();
                self.picker = Some(RefPicker { flow, typed });
                self.submit(load);
            }
            Err(why) => match &mut self.keyboard {
                Some(keyboard) => keyboard.error = why,
                None => self.say(&why, true),
            },
        }
    }

    /// What a choice in the picker, or a result there, came to.
    fn picked(&mut self, picked: Picked) {
        match picked {
            Picked::Nothing(step) => self.follow(step),
            Picked::Reference { app, setting, then } => {
                let Some(picker) = self.picker.take() else { return };
                if let Some(form) = &mut self.settings {
                    let index = picker.flow.field_index;
                    let effects = match picker.typed.filter(|typed| !typed.is_empty()) {
                        // After what was typed before `$(`.
                        Some(typed) => {
                            let mut effects = form.set_value(index, FieldValue::Text(typed));
                            effects.extend(form.insert_reference(index, usize::MAX, &app, &setting));
                            effects
                        }
                        None => form.place(index, picker.flow.mode, &app, &setting),
                    };
                    self.submit(effects);
                }
                self.follow(then);
            }
        }
    }

    /// The reference of a field its menu acts on: the first broken one, else the last.
    fn menu_reference(form: &Settings, index: usize) -> Option<usize> {
        let count = form.references(index).len();
        form.first_broken(index).or(count.checked_sub(1))
    }

    fn field_menu(&mut self, key: &str) {
        let Some(form) = &mut self.settings else { return };
        let Some(index) = form.index_of(key) else { return };
        let field = form.form.fields[index].clone();
        match &field.kind {
            FieldKind::Toggle => return,
            // A choice too long for a row of segments steps to its next option.
            FieldKind::Choice(_) => {
                form.step_choice(index, true);
                return;
            }
            _ => {}
        }
        let mut list = vec![button("dialog-close", "Close", 3)];
        // What the step takes back is said under the field; the button stays short.
        if form.undo_for(index).is_some() {
            list.push(button(format!("field-undo:{key}"), "↺ Undo", 3));
        }
        let references = form.references(index);
        let (title, text) = match Self::menu_reference(form, index).and_then(|chip| references.get(chip)) {
            Some((app, setting)) => {
                list.push(button(format!("field-remove:{key}"), "Remove", 2));
                list.push(button(format!("field-type:{key}"), "✎ Type a value instead", 1));
                list.push(button(format!("field-ref:{key}"), "Replace…", 0));
                let preview = form.preview(index);
                let text = match (preview.and_then(|p| p.problem.clone()), preview.and_then(|p| p.resolved.clone())) {
                    (Some(problem), _) => problem,
                    (None, Some(resolved)) => format!("Now: {resolved}"),
                    _ => "Working out its value…".to_owned(),
                };
                (format!("{app}.{setting}"), text)
            }
            None => {
                if field.kind.takes_reference() {
                    list.push(button(format!("field-ref:{key}"), "{ } Insert a reference", 1));
                }
                list.push(button(format!("field-type:{key}"), "✎ Type a value", 0));
                (field.label.clone(), if field.help.is_empty() { field.requirement.clone() } else { field.help.clone() })
            }
        };
        self.dialog(&title, &text, false, list);
    }

    // Going on the internet: what the check found, and the one confirmation.

    fn show_found(&mut self, found: Found) {
        match &found {
            Found::Clear(confirm) => {
                self.ask_public(confirm.clone());
                return;
            }
            Found::Problems(check) => {
                let failed = check.checks.iter().any(|c| c.state == CheckState::Failed);
                let title = if failed {
                    format!("{} can't be reached from the internet yet", check.site)
                } else {
                    format!("Some checks for {} could not be made", check.site)
                };
                let mut lines: Vec<String> = check
                    .checks
                    .iter()
                    .map(|c| format!("{}  {}{}", check_mark(c.state).0, c.text, if c.note.is_empty() { String::new() } else { format!(" ({})", c.note) }))
                    .collect();
                lines.extend(check.manual.iter().cloned());
                if check.router.is_some() {
                    lines.push(format!("How to forward the ports on your router is explained in {TERMINAL}."));
                }
                let mut list = vec![button("dialog-close", "Close", 1), button("reach-again", "Check again", 0)];
                if check.can_continue() {
                    list.push(button("reach-continue", "Continue anyway", 2));
                }
                self.dialog(&title, &lines.join("\n"), failed, list);
            }
            Found::Cgnat(cgnat) => {
                let text = format!(
                    "Your internet provider gave this home the address {}, in a range ({}) that several homes share, while the \
                     internet sees {}. Visitors cannot be sent on to the Nodo that way.\nTor needs no address of your own. The \
                     other ways on are explained in {TERMINAL}.",
                    cgnat.wan_address, cgnat.wan_range, cgnat.internet_address
                );
                let list = vec![
                    button("dialog-close", "Close", 1),
                    button("reach-again", "Check again", 1),
                    button("reach-tor", CgnatWay::Tor.label(), 0),
                ];
                self.dialog("Your home shares its internet address", &text, false, list);
            }
        }
        self.found = Some(found);
    }

    /// The confirmation before a site goes on the internet. The dialog starts on Cancel, and
    /// going on asks for the address to be typed, which the core checks.
    fn ask_public(&mut self, confirm: PublicConfirm) {
        let plan = &confirm.plan;
        let mut lines = vec![
            format!("Anyone on the internet will be able to open {}.", plan.url),
            "Visitors can see your home's internet address.".to_owned(),
        ];
        lines.extend([plan.dns_record.clone(), plan.firewall.clone()].into_iter().filter(|line| !line.is_empty()));
        lines.push("To go on, you type the address.".to_owned());
        self.dialog = Some(Dialog {
            title: format!("Put {} on the internet?", confirm.site),
            text: lines.join("\n"),
            danger: confirm.cancel_is_default(),
            buttons: vec![button("dialog-close", "Cancel", 0), button("dialog-yes", "Type the address", 2)],
            yes: Yes::TypeHost(Box::new(confirm)),
        });
    }

    pub fn act(&mut self, id: &str) {
        self.touched();
        let (verb, arg) = id.split_once(':').unwrap_or((id, ""));
        let arg = arg.to_owned();
        if !verb.starts_with("dialog-") {
            self.last_act = id.to_owned();
        }
        match verb {
            "screen" => self.close(),
            "lock" => self.call(Request::ScreenLock, Asked::Lock),
            "unlock" => self.ask_pin(),
            "power" => {}
            "dialog-close" => self.dialog_closed(),
            "dialog-yes" => self.dialog_confirmed(),
            "picker-close" => {
                if matches!(self.keyboard.as_ref().map(|k| &k.editing), Some(Editing::PickerSearch)) {
                    self.keyboard = None;
                }
                if self.picker.take().is_none() && self.providers.stage == ProviderStage::Pick {
                    let step = self.providers.back();
                    self.follow(step);
                }
            }
            "picker-search" => {
                let query = self.picker_query.clone();
                self.keyboard(Editing::PickerSearch, "Search", Input::plain(query), false);
            }
            "back" => {
                let step = match self.top() {
                    Screen::Providers => {
                        let step = self.providers.back();
                        if self.providers.stage != ProviderStage::Key {
                            self.provider_kind = None;
                            self.clear_provider_boxes();
                        }
                        step
                    }
                    _ => self.leaving(),
                };
                self.follow(step);
            }
            "open" => self.open_target(&arg),
            "ref" => {
                let Some(picker) = &mut self.picker else { return };
                let (app, setting) = arg.split_once('.').unwrap_or((&arg, ""));
                let picked = picker.flow.choose(app, setting);
                // A setting that is locked is asked about before the app may read it.
                let asks = picker.flow.granting.as_ref().map(|option| (picker.flow.package.clone(), option.path(), option.locked));
                self.picked(picked);
                if let Some((package, path, locked)) = asks {
                    let what = if locked.is_some_and(|l| l.secret) { "a secret" } else { "a setting that is not public" };
                    self.dialog = Some(Dialog {
                        title: format!("Let {package} read {path}?"),
                        text: format!("{path} is {what}. {package} will be able to read it; the value is never shown here."),
                        danger: false,
                        buttons: vec![button("dialog-close", "Cancel", 1), button("dialog-yes", "Allow", 0)],
                        yes: Yes::Grant,
                    });
                }
            }
            "field" => self.field_menu(&arg),
            "field-type" => {
                self.dialog = None;
                let Some(form) = &mut self.settings else { return };
                let Some(index) = form.index_of(&arg) else { return };
                // A reference becomes the text it stands for now, ready to be edited.
                let effects = match Self::menu_reference(form, index) {
                    Some(chip) => form.type_instead(index, chip),
                    None => Vec::new(),
                };
                let field = &form.form.fields[index];
                let (label, mono) = (field.label.clone(), matches!(field.kind, FieldKind::MoneroAddress | FieldKind::Url));
                let text = match (&field.kind, &form.draft[index]) {
                    (FieldKind::Secret, _) => Some(Input::secret()),
                    (_, FieldValue::Text(current)) => Some(Input::plain(current.clone())),
                    // Still text with references between: its pieces stay as they are.
                    _ => None,
                };
                self.submit(effects);
                if let Some(text) = text {
                    self.keyboard(Editing::Field(arg), &label, text, mono);
                }
            }
            "field-ref" => {
                self.dialog = None;
                let mode = self
                    .settings
                    .as_ref()
                    .and_then(|form| form.index_of(&arg).and_then(|index| Self::menu_reference(form, index)))
                    .map_or(PickMode::Insert, PickMode::Replace);
                self.open_references(&arg, mode, None);
            }
            "field-remove" => {
                self.dialog = None;
                let Some(form) = &mut self.settings else { return };
                let Some(index) = form.index_of(&arg) else { return };
                if let Some(chip) = Self::menu_reference(form, index) {
                    let effects = form.remove_reference(index, chip);
                    self.submit(effects);
                }
            }
            "field-undo" => {
                self.dialog = None;
                let Some(form) = &mut self.settings else { return };
                let Some(index) = form.index_of(&arg) else { return };
                let effects = form.undo(index);
                self.submit(effects);
            }
            "settings-cancel" => {
                let Some(form) = &mut self.settings else { return };
                form.discard();
                let effects = form.load();
                self.submit(effects);
            }
            "settings-save" => {
                let Some(form) = &mut self.settings else { return };
                let step = form.save();
                // Save stopped to say what the changed references allow.
                let consequences = form.confirm.clone();
                self.follow(step);
                if let Some(lines) = consequences {
                    self.dialog = Some(Dialog {
                        title: "Save these settings?".to_owned(),
                        text: format!("Saving also means:\n• {}", lines.join("\n• ")),
                        danger: false,
                        buttons: vec![button("dialog-close", "Cancel", 1), button("dialog-yes", "Save", 0)],
                        yes: Yes::SaveSettings,
                    });
                }
            }
            "restart" => self.submit(AppPage::new(&arg).restart()),
            "stop" | "start" => {
                let Some(snapshot) = &self.snapshot else { return };
                let step = AppPage::new(&arg).start_or_stop(snapshot);
                self.follow(step);
            }
            "remove" => self.ask(AppPage::new(&arg).ask_remove()),
            "app-update" => {
                let Some(snapshot) = &self.snapshot else { return };
                let step = AppPage::new(&arg).update(snapshot);
                self.follow(step);
            }
            "search" => {
                let query = self.query.clone();
                self.keyboard(Editing::Search, "Search apps", Input::plain(query), false);
            }
            "install" => {
                let Some(detail) = &self.detail else { return };
                let effects = install::install(detail);
                self.submit(effects);
            }
            "install-confirm" => {
                let (Screen::Gate(InstallGate::Review(data)), Some(snapshot)) = (self.top().clone(), &self.snapshot) else { return };
                let review = Review::new(data, ReviewPurpose::Install);
                if review.needs_passphrase() {
                    self.keyboard(Editing::InstallPassphrase, "Device passphrase", Input::secret(), false);
                } else {
                    let step = review.approve(None, snapshot);
                    self.follow(step);
                }
            }
            "install-cancel" => {
                let Some(report) = &self.install else { return };
                let step = install::leave(report);
                self.follow(step);
            }
            "add-source" => self.keyboard(Editing::SourceAddress, "Source address", Input::plain("https://"), true),
            "trust-source" => {
                let Some(probe) = &self.probe else { return };
                let effects = sources::trust(probe);
                self.submit(effects);
            }
            "remove-source" => self.ask(sources::ask_remove(&arg, false)),
            "keep-key" => self.pop(),
            "trust-key" => {
                self.keyboard(Editing::TrustName, &format!("Type {arg} to trust its new key"), Input::plain(""), false);
            }
            "refresh-sources" => self.submit(sources::refresh()),
            "public-host" => {
                let current = self.hosting.as_ref().map(Hosting::host).unwrap_or_default();
                self.keyboard(Editing::PublicHost, "Your web address", Input::plain(current), true);
            }
            "hosting-save" => {
                let (Some(hosting), Some(snapshot)) = (&mut self.hosting, &self.snapshot) else { return };
                // The draft may be older than the providers connected now.
                hosting.sync(snapshot);
                let step = hosting.save(snapshot);
                self.follow(step);
            }
            "hosting-provider" => {
                let Some(site) = self.hosting.as_ref().map(|hosting| hosting.site.clone()) else { return };
                self.push(Screen::Providers);
                self.providers = Providers::adding(Some(site));
                self.picker_query.clear();
                self.submit(providers::load());
            }
            "reach-again" => {
                self.dialog = None;
                let effects = match self.found.take() {
                    Some(Found::Problems(check)) => check.check_again(),
                    Some(Found::Cgnat(cgnat)) => cgnat.check_again(),
                    _ => Vec::new(),
                };
                self.submit(effects);
            }
            "reach-continue" => {
                self.dialog = None;
                if let Some(Found::Problems(check)) = self.found.take() {
                    self.follow(check.continue_anyway());
                }
            }
            "reach-tor" => {
                self.dialog = None;
                if let (Some(Found::Cgnat(cgnat)), Some(snapshot)) = (self.found.take(), &self.snapshot) {
                    let effects = cgnat.choose(CgnatWay::Tor, snapshot);
                    self.submit(effects);
                }
            }
            "check-updates" => self.submit(updates::check_now()),
            "apply-updates" => {
                let Some(snapshot) = &self.snapshot else { return };
                let step = updates::update_apps(snapshot, &self.checked);
                self.follow(step);
            }
            "update-system" => {
                let Some(snapshot) = &self.snapshot else { return };
                let step = updates::update_system(snapshot);
                self.follow(step);
            }
            "auto-save" => {
                let effects = self.auto.as_ref().map(AutoUpdate::save).unwrap_or_default();
                self.submit(effects);
            }
            "undo" => {
                let step = self.rollbacks.roll_back(&arg);
                self.follow(step);
            }
            "backup" => {
                self.backups.start_create(false);
                if self.backups.stage == backup::Stage::Create {
                    self.clear_backup_boxes();
                    let first = if self.backups.asks_device_passphrase() { BackupBox::Device } else { BackupBox::First };
                    self.ask_backup_box(first, "");
                }
            }
            "ssh-add" => self.keyboard(Editing::SshKey, "Paste a public key", Input::plain("ssh-ed25519 "), true),
            "ssh-remove" => {
                if let Some(question) = self.ssh.ask_remove_key(&arg) {
                    self.ask(question);
                }
            }
            "wifi-join" => {
                let step = self.network.join(&arg);
                self.follow(step);
                if let Some(prompt) = &self.network.prompt {
                    let label = format!("Password for {}", prompt.ssid);
                    self.keyboard(Editing::WifiPassphrase, &label, Input::secret(), false);
                }
            }
            "device-unlock" => {
                let Some(snapshot) = &self.snapshot else { return };
                let step = unlock::open(snapshot);
                self.follow(step);
            }
            "drive-retry" => self.submit(drive::retry()),
            "provider-add" => {
                self.picker_query.clear();
                let effects = self.providers.add();
                self.submit(effects);
            }
            "provider-kind" => {
                let Some(provider) = self.snapshot.as_ref().and_then(|s| s.providers.iter().find(|p| p.name == arg).cloned()) else {
                    // Cancel on the key's card: back to choosing the company.
                    self.provider_kind = None;
                    self.clear_provider_boxes();
                    let step = self.providers.back();
                    self.follow(step);
                    return;
                };
                // Keys typed for one company are never sent to another.
                self.clear_provider_boxes();
                if self.providers.choose(&provider) {
                    self.provider_kind = Some(provider);
                } else if let Some(Err(why)) = self.providers.test.clone() {
                    self.say(&why, true);
                }
            }
            "provider-field" => {
                let Some(provider) = &self.provider_kind else { return };
                let fields = providers::fields(Some(provider));
                let Some(field) = arg.parse::<usize>().ok().and_then(|n| fields.get(n).copied()) else { return };
                let label = Self::provider_field_label(provider, field);
                let text = match field {
                    KeyField::Domain => Input::plain(self.provider_boxes.0.clone()),
                    KeyField::ApiKey | KeyField::SecretKey => Input::secret(),
                };
                self.keyboard(Editing::ProviderField(field), &label, text, true);
            }
            "provider-connect" => {
                let (Some(provider), Some(snapshot)) = (&self.provider_kind, &self.snapshot) else { return };
                let (domain, api_key, secret_key) = &mut self.provider_boxes;
                // A box that is missing is said on the card, by the flow's `test`.
                if let Ok(step) = self.providers.submit(provider, domain, api_key, secret_key, snapshot) {
                    self.follow(step);
                }
            }
            "provider-done" => {
                self.provider_kind = None;
                let step = self.providers.finish();
                self.follow(step);
            }
            "provider-remove" => {
                let Some(connected) = self.snapshot.as_ref().and_then(|s| s.connected.iter().find(|c| c.id == arg)) else { return };
                let question = providers::ask_remove(connected);
                self.ask(question);
            }
            _ => {}
        }
    }

    fn provider_field_label(provider: &Provider, field: KeyField) -> String {
        match field {
            KeyField::Domain => "The web address you bought, such as example.com".to_owned(),
            KeyField::ApiKey => provider.fields.first().map(|name| words::provider_field_name(name)).unwrap_or_else(|| "Key".to_owned()),
            KeyField::SecretKey => provider.fields.get(1).map(|name| words::provider_field_name(name)).unwrap_or_else(|| "Second key".to_owned()),
        }
    }

    fn open_target(&mut self, target: &str) {
        let (kind, name) = target.split_once(':').unwrap_or((target, ""));
        let name = name.to_owned();
        let screen = match kind {
            "app" => Screen::App(name),
            "settings" => Screen::Settings(name),
            "get-apps" => Screen::GetApps,
            "about" => {
                let (feed, pkg) = name.split_once('/').unwrap_or(("", &name));
                Screen::About(PackageId::new(feed, pkg))
            }
            "sources" => Screen::Sources,
            "add-source" => Screen::AddSource,
            // A source whose key changed opens on that decision; any other, on what it offers.
            "key" => {
                let Some(feed) = self.snapshot.as_ref().and_then(|s| s.feeds.iter().find(|feed| feed.name == name)) else { return };
                let step = sources::open(feed);
                self.follow(step);
                return;
            }
            "hosting" => {
                let Some(snapshot) = &self.snapshot else { return };
                match hosting::open(snapshot, &name) {
                    Ok(hosting) => self.hosting = Some(hosting),
                    Err(notice) => {
                        self.tell(notice);
                        return;
                    }
                }
                Screen::Hosting(name)
            }
            "providers" => Screen::Providers,
            "updates" => Screen::Updates,
            "auto" => Screen::AutoUpdate,
            "undo" => Screen::Undo,
            "logs" if name.is_empty() => Screen::Logs(None),
            "logs" => {
                // From the device logs the page keeps their choices; an app's log opens on its own.
                if !matches!(self.top(), Screen::Logs(None)) {
                    self.logs = match &self.snapshot {
                        Some(snapshot) => Logs::of(snapshot, &name),
                        None => Logs::device(),
                    };
                }
                Screen::Logs(Some(name))
            }
            "backups" => Screen::Backups,
            "network" => Screen::Network,
            "ssh" => Screen::SshKeys,
            "screen" => Screen::Display,
            _ => return,
        };
        self.push(screen);
    }

    // For screenshots and tests: names to navigate to.
    pub fn first_app(&self) -> String {
        self.snapshot.as_ref().and_then(|s| s.services.iter().find(|x| !matches!(x.origin, nodo_api::Origin::System)).or(s.services.first())).map(|s| s.id.clone()).unwrap_or_default()
    }

    pub fn first_catalog(&self) -> String {
        self.snapshot.as_ref().and_then(|s| s.catalog.iter().find(|p| p.status == PackageStatus::Available).or(s.catalog.first())).map(|p| format!("{}/{}", p.id.feed, p.id.name)).unwrap_or_default()
    }

    pub fn first_log(&self) -> String {
        self.snapshot.as_ref().and_then(|s| s.log_sources.first().cloned()).unwrap_or_default()
    }

    pub fn first_field(&self) -> String {
        let Some(form) = &self.settings else { return String::new() };
        let fields = &form.form.fields;
        (0..fields.len())
            .find(|&i| !form.references(i).is_empty())
            .or_else(|| fields.iter().position(|f| f.kind.takes_reference()))
            .or((!fields.is_empty()).then_some(0))
            .map(|i| fields[i].key.clone())
            .unwrap_or_default()
    }

    /// The PIN pad, or the "set a PIN first" answer, as a locked change would show them.
    pub fn show_pin_for_design(&mut self, pin_set: bool) {
        self.screen.pin_set = pin_set;
        self.ask_pin();
        if pin_set {
            self.pin.digits = "48".into();
        }
    }

    // ---------------------------------------------------------------------------------------
    // What the window shows.

    pub fn badges(&self) -> Vec<String> {
        let updates = self.snapshot.as_ref().map_or(0, Snapshot::pending_updates);
        vec![String::new(), if updates > 0 { updates.to_string() } else { String::new() }, String::new(), String::new()]
    }

    pub fn unlocked(&self) -> bool {
        self.screen.unlocked
    }

    pub fn pin_set(&self) -> bool {
        self.screen.pin_set
    }

    pub fn tab(&self) -> usize {
        self.tab
    }

    pub fn toast(&self) -> (String, bool) {
        let working = self.sent.iter().find_map(|sent| match sent {
            Sent::Batch(effects) => effects.iter().find_map(working_words),
            Sent::Call(_) => None,
        });
        match working {
            Some(what) => (format!("{what}…"), false),
            None => self.toast.as_ref().map(|(m, e, _)| (m.clone(), *e)).unwrap_or_default(),
        }
    }

    pub fn dialog_view(&self) -> MDialog {
        match &self.dialog {
            Some(dialog) => MDialog {
                shown: true,
                title: shared(&dialog.title),
                text: shared(&dialog.text),
                danger: dialog.danger,
                buttons: model(dialog.buttons.clone()),
            },
            None => MDialog { shown: false, title: "".into(), text: "".into(), danger: false, buttons: model(Vec::new()) },
        }
    }

    pub fn picker_view(&self) -> MPicker {
        let query = &self.picker_query;
        if let Some(picker) = &self.picker {
            let rows: Vec<MRow> = picker
                .flow
                .visible(query)
                .into_iter()
                .map(|(option, fits)| {
                    let (note, tone) = match (&fits, option.locked) {
                        (Err(why), _) => (why.clone(), 1),
                        (Ok(()), Some(lock)) if !lock.granted => {
                            (if lock.secret { "secret · needs your approval" } else { "not public · needs your approval" }.to_owned(), 0)
                        }
                        _ => (option.value.clone().filter(|v| !v.is_empty()).unwrap_or_else(|| option.label.clone()), 0),
                    };
                    MRow { mono: true, tone, ..link(format!("ref:{}", option.path()), option.path(), note, "", 0) }
                })
                .collect();
            let title = match picker.flow.mode {
                PickMode::Insert | PickMode::Append => "Insert a reference",
                PickMode::Replace(_) => "Replace the reference",
            };
            return MPicker {
                shown: true,
                title: title.into(),
                sub: shared(format!("into {}", picker.flow.field_label)),
                query: shared(query),
                rows: model(rows),
            };
        }
        // Choosing the company, while a provider is being added.
        if matches!(self.top(), Screen::Providers) && self.providers.stage == ProviderStage::Pick {
            let needle = query.to_lowercase();
            let rows: Vec<MRow> = self
                .snapshot
                .iter()
                .flat_map(|s| s.providers.iter())
                .filter(|p| needle.is_empty() || p.name.to_lowercase().contains(&needle))
                .map(|p| link(format!("provider-kind:{}", p.name), &p.name, if p.is_supported() { "" } else { "Can't be connected" }, "", 0))
                .collect();
            return MPicker {
                shown: true,
                title: "Add a provider".into(),
                sub: "Where your web address is registered".into(),
                query: shared(query),
                rows: model(rows),
            };
        }
        MPicker { shown: false, title: "".into(), sub: "".into(), query: "".into(), rows: model(Vec::new()) }
    }

    pub fn keyboard_view(&self) -> MKeyboard {
        match &self.keyboard {
            Some(k) => MKeyboard {
                shown: true,
                label: shared(&k.label),
                text: shared(k.text.display()),
                secret: k.text.is_secret(),
                mono: k.mono,
                error: shared(&k.error),
                can_paste: false,
            },
            None => MKeyboard {
                shown: false,
                label: "".into(),
                text: "".into(),
                secret: false,
                mono: false,
                error: "".into(),
                can_paste: false,
            },
        }
    }

    pub fn pin_view(&self) -> MPin {
        MPin {
            shown: self.pin.shown,
            title: "Enter the screen PIN".into(),
            sub: "to make changes".into(),
            digits: self.pin.digits.len() as i32,
            wrong: self.pin.wrong,
            busy: self.pin.busy,
        }
    }

    /// A button that changes something shows a lock while the screen is locked.
    fn change_button(&self, id: impl AsRef<str>, label: impl AsRef<str>, kind: i32) -> MButton {
        MButton { lock: !self.screen.unlocked, ..button(id, label, kind) }
    }

    pub fn page(&self) -> Page {
        let crumbs_of = |names: &[&str]| names.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        if let Some(why) = &self.offline {
            return Page {
                crumbs: Vec::new(),
                title: TABS[self.tab].into(),
                actions: Vec::new(),
                cards: vec![toned(card("The screen can't reach the Nodo's helper", why, vec![text("Management needs nodo-helper running with its screen socket. The tiles keep working.", 0)]), 3)],
            };
        }
        let Some(snap) = &self.snapshot else {
            return Page {
                crumbs: Vec::new(),
                title: TABS[self.tab].into(),
                actions: Vec::new(),
                cards: vec![card("Reading the Nodo…", "", Vec::new())],
            };
        };
        match self.top().clone() {
            Screen::Home => self.home(snap),
            Screen::Apps => self.apps(snap),
            Screen::App(name) => self.app(snap, &name),
            Screen::Settings(package) => self.settings_page(&package),
            Screen::GetApps => self.get_apps(snap),
            Screen::About(id) => self.about(&id),
            Screen::Gate(gate) => self.gate(&gate),
            Screen::Installing => self.installing(),
            Screen::Sources => self.sources(snap),
            Screen::AddSource => self.add_source(),
            Screen::KeyChanged(feed) => self.key_changed(&feed),
            Screen::Web => self.web(snap),
            Screen::Hosting(site) => self.hosting_page(snap, &site),
            Screen::Providers => self.providers_page(snap),
            Screen::System => self.system(snap),
            Screen::Updates => self.updates(snap),
            Screen::AutoUpdate => self.auto_update(),
            Screen::Undo => self.undo(),
            Screen::Logs(source) => self.logs_page(snap, source.as_deref()),
            Screen::Backups => self.backups_page(),
            Screen::Network => self.network_page(),
            Screen::SshKeys => self.ssh_page(),
            Screen::Display => Page {
                crumbs: crumbs_of(&["System"]),
                title: "Screen".into(),
                actions: Vec::new(),
                cards: vec![card(
                    "Nodo logo tile",
                    "Add it from Arrange like any panel; at the full size it is a wallpaper.",
                    vec![
                        segments("logo-style", "Style", &["Drawn".to_owned(), "Text".to_owned()], usize::from(self.prefs.logo_style == crate::screen_prefs::LogoStyle::Text)),
                        toggle("logo-shimmer:", "Shimmer", "The boot splash's glint, on the text style", self.prefs.logo_shimmer),
                        toggle("logo-name:", "Show the name", "", self.prefs.logo_name),
                    ],
                ), card(
                    "",
                    "",
                    vec![
                        segments("screen-brightness", "Brightness", &crate::screen_prefs::BRIGHTNESS_LABELS.map(String::from), self.prefs.brightness_index),
                        segments("screen-timeout", "Go back to the tiles after", &crate::screen_prefs::TIMEOUT_LABELS.map(String::from), self.prefs.timeout_index),
                        segments("screen-dim", "Dim when idle to", &crate::screen_prefs::DIM_LABELS.map(String::from), self.prefs.dim_index),
                        text(
                            if self.screen.pin_set {
                                "A screen PIN is set. Change it over SSH: nodo-tui --ctl screen-pin."
                            } else {
                                "No screen PIN yet: the screen can only look. Set one over SSH: nodo-tui --ctl screen-pin."
                            },
                            0,
                        ),
                    ],
                )],
            },
        }
    }

    fn crumbs(&self) -> Vec<String> {
        let mut list = vec![TABS[self.tab].to_owned()];
        for s in self.stack.iter().skip(1).take(self.stack.len().saturating_sub(2)) {
            list.push(match s {
                Screen::App(n) | Screen::Settings(n) | Screen::Hosting(n) => n.clone(),
                Screen::GetApps => "Get apps".into(),
                Screen::About(id) => id.name.clone(),
                Screen::Sources => "Sources".into(),
                Screen::Updates => "Updates".into(),
                Screen::Providers => "Domain providers".into(),
                Screen::Logs(_) => "Device logs".into(),
                _ => "…".into(),
            });
        }
        if self.stack.len() <= 1 {
            list.clear();
        }
        list
    }

    fn home(&self, snap: &Snapshot) -> Page {
        let mut cards = Vec::new();
        // The screen only manages a device that is set up, so first setup is never met here.
        let missing = snap.overview.drive.as_ref().filter(|_| snap.landing_once_set_up() == nodo_ui_core::queries::Landing::NoDrive);
        if let Some(drive) = missing {
            let mut rows = vec![text(&drive.message, 0)];
            for c in &drive.checks {
                let (mark, tone) = check_mark(c.state);
                rows.push(MRow { value: mark.into(), tone, title: shared(&c.text), sub: shared(&c.note), ..row("step") });
            }
            rows.push(buttons(vec![button("drive-retry", "Check again", 0)]));
            cards.push(toned(card("The data drive is missing", &drive.waiting, rows), 3));
        }
        // The helper can be locked with no encrypted app waiting, such as before the first one is
        // installed; saved keys like a domain provider's still need the passphrase.
        if snap.is_locked() {
            let locked: Vec<_> = snap.overview.unlock_targets.iter().filter(|t| t.encrypted).collect();
            let mut rows: Vec<MRow> = locked.iter().map(|t| line(&t.name, &t.note)).collect();
            rows.push(buttons(vec![button("device-unlock", "Unlock", 0)]));
            let sub = if locked.is_empty() {
                "Saved keys and secret settings wait for the device passphrase."
            } else {
                "Encrypted apps wait for the device passphrase."
            };
            cards.push(toned(card("Unlock after a restart", sub, rows), 1));
        }
        for a in &snap.overview.attention {
            let tone = match a.level {
                AttentionLevel::Critical => 3,
                AttentionLevel::Warning => 1,
                AttentionLevel::Info => 0,
            };
            cards.push(toned(card(&a.text, "", Vec::new()), tone));
        }
        let ready: Vec<_> = snap.updates.iter().filter(|u| !u.held).collect();
        if !ready.is_empty() {
            let names = ready.iter().map(|u| u.name.as_str()).collect::<Vec<_>>().join(", ");
            cards.push(card(
                format!("{} update{} ready", ready.len(), if ready.len() == 1 { " is" } else { "s are" }),
                format!("{names}. Each keeps its settings and can be undone."),
                vec![buttons(vec![button("open:updates", "Review updates", 0)])],
            ));
        }
        let mut rows = Vec::new();
        for s in &snap.services {
            let (label, tone) = state_label(s.state);
            rows.push(link(format!("open:app:{}", s.id), &s.id, &s.detail, label, tone));
        }
        rows.push(text(words::health_line(snap), 0));
        cards.push(card("✓  Everything else is running", "", rows));
        Page { crumbs: Vec::new(), title: "Your Nodo".into(), actions: Vec::new(), cards }
    }

    fn apps(&self, snap: &Snapshot) -> Page {
        let rows = snap
            .services
            .iter()
            .map(|s| {
                let (label, tone) = if AppPage::new(&s.id).has_update(snap) { ("Update ready", 1) } else { state_label(s.state) };
                link(format!("open:app:{}", s.id), &s.id, &s.detail, label, tone)
            })
            .collect();
        Page {
            crumbs: Vec::new(),
            title: "Installed".into(),
            actions: vec![button("open:get-apps", "Get apps", 1)],
            cards: vec![card("", "", rows)],
        }
    }

    fn app(&self, snap: &Snapshot, name: &str) -> Page {
        let page = AppPage::new(name);
        let service = snap.services.iter().find(|s| s.id == name);
        let mut rows = Vec::new();
        if let Some(s) = service {
            let (label, tone) = state_label(s.state);
            rows.push(MRow { value: shared(label), tone, ..line(name, &s.detail) });
        }
        if let Some(u) = snap.updates.iter().find(|u| u.name == name && !u.held) {
            rows.push(MRow { value: shared(format!("{} → {}", u.from, u.to)), tone: 1, ..line("An update is ready", &u.source) });
            rows.push(buttons(vec![self.change_button(format!("app-update:{name}"), "Update", 0)]));
        }
        let nav = vec![
            link(format!("open:hosting:{name}"), "Hosting", words::hosting_summary(snap, name), "", 0),
            link(format!("open:settings:{name}"), "Settings", "", "", 0),
            link(format!("open:logs:{name}"), "Logs", words::log_summary(snap, name), "", 0),
        ];
        let running = page.state(snap) == Some(nodo_api::ServiceState::Running);
        let mut actions = vec![self.change_button(format!("restart:{name}"), "Restart", 1)];
        actions.push(if running {
            self.change_button(format!("stop:{name}"), "Stop", 1)
        } else {
            self.change_button(format!("start:{name}"), "Start", 1)
        });
        if service.is_some_and(|s| !matches!(s.origin, nodo_api::Origin::System)) {
            actions.push(self.change_button(format!("remove:{name}"), "Remove…", 2));
        }
        Page { crumbs: self.crumbs(), title: name.into(), actions, cards: vec![card("", "", rows), card("", "", nav)] }
    }

    fn settings_page(&self, package: &str) -> Page {
        let mut rows = Vec::new();
        let Some(form) = self.settings.as_ref().filter(|form| form.form.package == package) else {
            let what = if self.settings_loaded { "This app has no settings." } else { "Reading settings…" };
            return Page { crumbs: self.crumbs(), title: "Settings".into(), actions: Vec::new(), cards: vec![card(what, "", Vec::new())] };
        };
        for (index, f) in form.form.fields.iter().enumerate() {
            let value = &form.draft[index];
            let changed = *value != f.value;
            match (&f.kind, value) {
                (FieldKind::Toggle, FieldValue::Bool(on)) => rows.push(MRow { changed, ..toggle(format!("bool:{}", f.key), &f.label, &f.help, *on) }),
                (FieldKind::Choice(options), v) => {
                    let current = if let FieldValue::Text(t) = v { t.clone() } else { String::new() };
                    let selected = options.iter().position(|o| *o == current).unwrap_or(0);
                    if options.len() <= 4 {
                        rows.push(MRow { changed, ..segments(&format!("choice:{}", f.key), &f.label, options, selected) });
                    } else {
                        rows.push(MRow { id: shared(format!("field:{}", f.key)), title: shared(&f.label), value: shared(current), changed, options: model(options.iter().map(shared).collect()), ..row("field") });
                    }
                }
                (kind, v) => {
                    let mono = matches!(kind, FieldKind::MoneroAddress | FieldKind::Url);
                    let mut r = MRow { id: shared(format!("field:{}", f.key)), title: shared(&f.label), changed, mono, placeholder: shared(&f.requirement), ..row("field") };
                    if !kind.takes_reference() {
                        r.selected = -1;
                    }
                    let preview = form.preview(index);
                    match v {
                        FieldValue::Text(t) => r.value = shared(t),
                        FieldValue::Pieces(pieces) => {
                            let mut chips = Vec::new();
                            let mut n = 0;
                            for piece in pieces {
                                match piece {
                                    Piece::Text(t) => chips.push(MChip { label: shared(t), value: SharedString::new(), broken: false, plain: true }),
                                    Piece::Ref { app, setting } => {
                                        let state = preview.and_then(|p| p.chips.get(n).cloned());
                                        n += 1;
                                        let broken = matches!(state, Some(Err(_)));
                                        // A long value would make the chip wrap; the popover shows it in full.
                                        let value = match state {
                                            Some(Ok(Some(v))) if v.chars().count() <= 28 => v,
                                            _ => String::new(),
                                        };
                                        chips.push(MChip { label: shared(format!("{app} · {setting}")), value: shared(value), broken, plain: false });
                                    }
                                }
                            }
                            r.chips = model(chips);
                        }
                        FieldValue::SecretSet => r.value = "••••••••  (set)".into(),
                        FieldValue::SecretUnset => r.value = SharedString::new(),
                        FieldValue::NewSecret(_) => r.value = "••••••••  (new)".into(),
                        FieldValue::Bool(b) => r.value = shared(b.to_string()),
                    }
                    // Why the field cannot be saved as it is, beside it; else the step that can be taken back.
                    let broken = preview.filter(|p| p.broken() > 0).map(|p| p.problem.clone().unwrap_or_else(|| "This reference does not resolve.".to_owned()));
                    if let Some(why) = form.error_for(index).or(broken) {
                        r.tone = 3;
                        r.sub = shared(why);
                    } else if let Some(undo) = form.undo_for(index) {
                        r.sub = shared(match undo.label.strip_prefix("Undo") {
                            Some(what) => format!("↺ Tap the field to undo{what}"),
                            None => format!("↺ Tap the field to undo: {}", undo.label),
                        });
                    }
                    rows.push(r);
                }
            }
        }
        let dirty = !form.changed_indices().is_empty();
        let mut save = self.change_button("settings-save", "Save", 0);
        save.enabled = dirty;
        let mut cancel = button("settings-cancel", "Cancel", 1);
        cancel.enabled = dirty;
        rows.push(buttons(vec![cancel, save]));
        Page {
            crumbs: self.crumbs(),
            title: format!("{package} settings"),
            actions: Vec::new(),
            cards: vec![card("", "Any text, number or address can hold a reference to another app's value: tap a field.", rows)],
        }
    }

    fn get_apps(&self, snap: &Snapshot) -> Page {
        let mut rows: Vec<MRow> = self
            .search
            .results(snap, &self.query)
            .into_iter()
            .map(|p| {
                let (status, tone) = match p.status {
                    PackageStatus::Installed => ("Installed", 0),
                    PackageStatus::UpdateReady => ("Update ready", 1),
                    PackageStatus::Available => ("", 0),
                };
                link(format!("open:about:{}/{}", p.id.feed, p.id.name), &p.id.name, &p.tagline, status, tone)
            })
            .collect();
        let found = words::counted(rows.len() as u64, "app");
        let from = match &self.search.feed_filter {
            Some(source) => format!("{found} from {source}"),
            None => format!("{found} from your sources"),
        };
        if rows.is_empty() {
            rows.push(text(words::empty_catalog_line(snap, &self.query), 0));
        }
        let search = MRow {
            id: "search".into(),
            title: "Search".into(),
            value: shared(&self.query),
            placeholder: "Search apps".into(),
            selected: -1,
            ..row("field")
        };
        Page {
            crumbs: self.crumbs(),
            title: "Get apps".into(),
            actions: vec![button("open:sources", "Sources", 1)],
            cards: vec![card("", "", vec![search]), card("", from, rows)],
        }
    }

    fn about(&self, id: &PackageId) -> Page {
        let Some(d) = self.detail.as_ref().filter(|detail| detail.id == *id) else {
            return Page { crumbs: self.crumbs(), title: id.name.clone(), actions: Vec::new(), cards: vec![card("Reading…", "", Vec::new())] };
        };
        let mut rows = vec![text(&d.about, 4), line("Version", format!("{} · {} · from {}", d.version, d.size, id.feed))];
        if !d.asks.is_empty() {
            rows.push(text("It will:", 0));
            for (a, note) in &d.asks {
                rows.push(line(format!("•  {a}"), note));
            }
        }
        for n in &d.needs {
            rows.push(MRow { value: shared(if n.met { "✓" } else { "needed" }), tone: if n.met { 2 } else { 1 }, ..line(format!("Needs {}", n.name), n.source.clone().unwrap_or_default()) });
        }
        if !d.installed {
            rows.push(buttons(vec![self.change_button("install", "Install", 0)]));
        }
        Page { crumbs: self.crumbs(), title: id.name.clone(), actions: Vec::new(), cards: vec![card("", "", rows)] }
    }

    fn gate(&self, gate: &InstallGate) -> Page {
        match gate {
            InstallGate::Review(r) => {
                let mut rows = vec![text("It will be able to:", 0)];
                for a in &r.asks {
                    rows.push(MRow { tone: if a.level == AskLevel::Caution { 1 } else { 0 }, ..line(format!("✓  {}", a.text), &a.note) });
                }
                for g in &r.guarantees {
                    rows.push(text(g, 0));
                }
                if r.needs_passphrase {
                    rows.push(text("It keeps data on the drive locked with your device passphrase, which comes next.", 0));
                }
                rows.push(buttons(vec![button("back", "Cancel", 1), self.change_button("install-confirm", "Install", 0)]));
                Page { crumbs: self.crumbs(), title: format!("Install {}?", r.id.name), actions: Vec::new(), cards: vec![card("", "", rows)] }
            }
            InstallGate::Rejected(r) => {
                let rows = r
                    .violations
                    .iter()
                    .map(|v| MRow { value: shared(if v.passed { "✓" } else { "✕" }), tone: if v.passed { 2 } else { 3 }, title: shared(&v.rule), sub: shared(&v.evidence), ..row("step") })
                    .collect();
                Page { crumbs: self.crumbs(), title: format!("{} can't be installed", r.id.name), actions: Vec::new(), cards: vec![toned(card("It breaks the Nodo's rules", "", rows), 3)] }
            }
            InstallGate::MissingCapability(c) => {
                let rows = vec![text(&c.message, 4), line("Your node", &c.current_node), text(&c.switch_note, 0)];
                Page { crumbs: self.crumbs(), title: format!("{} needs another app first", c.id.name), actions: Vec::new(), cards: vec![toned(card("", "", rows), 1)] }
            }
            // A bundle asks questions, some of them secrets to paste, which the screen has no keyboard for.
            InstallGate::Bundle(b) => {
                let mut rows = vec![text("It installs these apps together, and asks a few questions first. Install it from the terminal (nodo-tui over SSH), where you can answer them.", 0)];
                for m in &b.members {
                    rows.push(line(format!("•  {}", m.review.package), if m.installed.is_some() { "installed already" } else { "" }));
                }
                rows.push(buttons(vec![button("back", "OK", 0)]));
                Page { crumbs: self.crumbs(), title: format!("{} installs {} apps", b.package, b.members.len()), actions: Vec::new(), cards: vec![card("", "", rows)] }
            }
        }
    }

    fn installing(&self) -> Page {
        let Some(s) = &self.install else {
            return Page { crumbs: Vec::new(), title: "Installing".into(), actions: Vec::new(), cards: Vec::new() };
        };
        let mut rows: Vec<MRow> = s
            .steps
            .iter()
            .map(|st| {
                let (mark, tone) = match st.state {
                    StepState::Done => ("✓", 2),
                    StepState::Working => ("◌", 1),
                    StepState::Pending => ("·", 0),
                    StepState::Failed => ("✕", 3),
                };
                MRow { value: shared(mark), tone, title: shared(&st.name), sub: shared(&st.note), ..row("step") }
            })
            .collect();
        let done = s.steps.iter().filter(|st| st.state == StepState::Done).count() as f32;
        let total = s.steps.len().max(1) as f32;
        rows.insert(0, MRow { title: shared(format!("{} {}", s.id.name, s.version)), progress: done / total, value: shared(format!("{}%", (done / total * 100.0) as i32)), ..row("progress") });
        let footer = match &s.outcome {
            // Only where the backend can stop it; the helper's install cannot be.
            InstallOutcome::InProgress if s.can_cancel => buttons(vec![button("install-cancel", "Cancel", 1)]),
            InstallOutcome::InProgress => text("This can take a few minutes.", 0),
            InstallOutcome::Succeeded => buttons(vec![button(format!("open:app:{}", s.id.name), "Open", 0)]),
            InstallOutcome::Failed(e) => text(e, 3),
            InstallOutcome::Cancelled => text("Cancelled. Nothing was left behind.", 0),
        };
        rows.push(footer);
        Page { crumbs: Vec::new(), title: format!("Installing {}", s.id.name), actions: Vec::new(), cards: vec![card("", "", rows)] }
    }

    fn sources(&self, snap: &Snapshot) -> Page {
        let mut cards = Vec::new();
        for f in &snap.feeds {
            let offers = words::counted(u64::from(f.packages), "app");
            let mut rows = vec![MRow {
                value: shared(if f.key_state == KeyState::BuiltIn { "built in" } else { "" }),
                ..toggle(format!("feed:{}", f.name), &f.name, format!("{}  ·  {offers}", f.address), f.status != FeedStatus::Disabled)
            }];
            match &f.status {
                FeedStatus::Unreachable(why) => rows.push(text(format!("✕  Couldn't reach it: {why}"), 3)),
                FeedStatus::Blocked => rows.push(text("✕  Its key changed: check before you trust it", 3)),
                _ => {}
            }
            rows.push(long("Its key", &f.fingerprint));
            let mut b = Vec::new();
            if f.key_state == KeyState::Changed {
                b.push(button(format!("open:key:{}", f.name), "Check the new key", 0));
            }
            b.push(self.change_button(format!("remove-source:{}", f.name), "Remove", 2));
            rows.push(buttons(b));
            cards.push(card("", "", rows));
        }
        Page {
            crumbs: self.crumbs(),
            title: "Sources".into(),
            actions: vec![button("refresh-sources", "Check now", 1), self.change_button("open:add-source", "Add a source", 0)],
            cards,
        }
    }

    fn add_source(&self) -> Page {
        let mut cards = vec![card(
            "Add a source",
            "Paste the address the publisher gave you. Nothing is installed yet: the Nodo first shows what it offers and its key.",
            vec![
                MRow { id: "add-source".into(), title: "Address".into(), value: shared(&self.source_address), placeholder: "https://".into(), mono: true, selected: -1, ..row("field") },
            ],
        )];
        if let Some(p) = &self.probe {
            let offers = words::counted(u64::from(p.packages), "app");
            cards.push(card(
                format!("✓  {}", p.name),
                format!("It offers {offers}{}", if p.signed { "" } else { " (unsigned)" }),
                vec![
                    long("Its key", &p.fingerprint),
                    text("Check this matches what the publisher gave you, by a way you trust. If it doesn't match, don't add it.", 0),
                    buttons(vec![button("back", "Cancel", 1), self.change_button("trust-source", "Add source", 0)]),
                ],
            ));
        }
        Page { crumbs: self.crumbs(), title: "Add a source".into(), actions: Vec::new(), cards }
    }

    fn key_changed(&self, feed: &str) -> Page {
        let mut rows = vec![text(
            format!("This happens when a publisher replaces a lost key, and also when someone else takes over their address. Until you decide, the Nodo installs nothing new from {feed}."),
            0,
        )];
        if let Some(c) = &self.key_change {
            rows.push(long("Key you trust", &c.trusted));
            rows.push(long("New key", &c.now_signing));
        }
        rows.push(buttons(vec![self.change_button(format!("trust-key:{feed}"), "Trust the new key…", 1), button("keep-key", "Keep the old key", 0)]));
        Page { crumbs: self.crumbs(), title: "Check before you trust".into(), actions: Vec::new(), cards: vec![toned(card(format!("▲  {feed} is now signed with a different key"), "", rows), 1)] }
    }

    fn web(&self, snap: &Snapshot) -> Page {
        let rows: Vec<MRow> = snap
            .active_sites()
            .into_iter()
            .map(|s| link(format!("open:hosting:{}", s.package), &s.package, words::site_address(s), words::reach_words(s), 0))
            .collect();
        let mut cards = vec![card("Sites online", "", rows)];
        cards.push(card("", "", vec![text("To put another app online, open it in Apps.", 0)]));
        Page { crumbs: Vec::new(), title: "Web".into(), actions: vec![button("open:providers", "Domain providers", 1)], cards }
    }

    fn hosting_page(&self, snap: &Snapshot, site: &str) -> Page {
        let Some(page) = self.hosting.as_ref().filter(|hosting| hosting.site == site) else {
            return Page { crumbs: self.crumbs(), title: "Hosting".into(), actions: Vec::new(), cards: Vec::new() };
        };
        let local_url = snap.web.sites.iter().find(|x| x.package == site).map(|s| s.local_url.clone()).unwrap_or_default();
        let mut rows = Vec::new();
        let mut names = Vec::new();
        for offered in page.rows() {
            match offered {
                hosting::Row::Home => rows.push(toggle("expose:local", "Home network", format!("Phones and computers on your Wi-Fi.  {local_url}"), page.draft.local)),
                hosting::Row::Tor => rows.push(toggle("expose:onion", "Tor", "A private link that opens in Tor Browser. It doesn't reveal your home.", page.draft.onion)),
                hosting::Row::Internet => rows.push(toggle("expose:public", "Internet", "Anyone, at a web address you own.", page.draft.public)),
                // One box holds the whole address; the core splits it at the domain.
                hosting::Row::Domain => {}
                hosting::Row::Address => {
                    let domains: Vec<String> = snap.domain_choices().into_iter().map(|(domain, _)| domain).collect();
                    rows.push(MRow {
                        id: format!("public-host:{site}").into(),
                        title: "Web address".into(),
                        value: shared(page.host()),
                        placeholder: shared(format!("e.g. {}.{}", site, domains.first().cloned().unwrap_or_else(|| "example.com".into()))),
                        mono: true,
                        sub: shared(format!("Your domains: {}", domains.join(", "))),
                        selected: -1,
                        ..row("field")
                    });
                }
                hosting::Row::Alias(index) => {
                    let alias = &page.aliases[index];
                    names.push(match alias.mode {
                        nodo_api::AliasMode::Redirect => format!("{} (redirects here)", alias.host),
                        nodo_api::AliasMode::Serve => format!("{} (same site)", alias.host),
                    });
                }
                // The other names are shown here and changed in the terminal.
                hosting::Row::NewAlias => {}
                hosting::Row::AddProvider => {
                    rows.push(text("To use a web address you own, first add the company you bought it from.", 0));
                    rows.push(buttons(vec![button("hosting-provider", "Add a provider", 0)]));
                }
                hosting::Row::Save => {
                    if !names.is_empty() {
                        rows.push(line("Also answers at", names.join(", ")));
                    }
                    rows.push(buttons(vec![self.change_button("hosting-save", "Save", 0)]));
                }
            }
        }
        Page { crumbs: self.crumbs(), title: "Hosting".into(), actions: Vec::new(), cards: vec![card("", format!("Choose who can open {site}. Any combination works."), rows)] }
    }

    fn providers_page(&self, snap: &Snapshot) -> Page {
        let mut cards = Vec::new();
        for (c, label) in snap.connected.iter().zip(words::provider_labels(&snap.connected)) {
            let mut rows: Vec<MRow> = c
                .domains
                .iter()
                .map(|d| line(&d.name, if d.used_by.is_empty() { "Not used yet".to_owned() } else { format!("Used by {}", d.used_by.join(", ")) }))
                .collect();
            if let Some(p) = &c.problem {
                rows.push(text(format!("✕  {p}"), 3));
            }
            rows.push(buttons(vec![self.change_button(format!("provider-remove:{}", c.id), "Remove", 2)]));
            cards.push(card(label, "", rows));
        }
        match (&self.providers.stage, &self.provider_kind) {
            (ProviderStage::Key, Some(p)) => {
                let (domain, api_key, secret_key) = &self.provider_boxes;
                let mut rows = vec![text(&p.instructions, 0)];
                if let Some(Err(why)) = &self.providers.test {
                    rows.push(text(format!("✕  {why}"), 3));
                }
                for (n, field) in providers::fields(Some(p)).into_iter().enumerate() {
                    let value = match field {
                        KeyField::Domain => domain.clone(),
                        KeyField::ApiKey if !api_key.is_empty() => "••••••••".to_owned(),
                        KeyField::SecretKey if !secret_key.is_empty() => "••••••••".to_owned(),
                        KeyField::ApiKey | KeyField::SecretKey => String::new(),
                    };
                    rows.push(MRow {
                        id: shared(format!("provider-field:{n}")),
                        title: shared(Self::provider_field_label(p, field)),
                        value: shared(value),
                        placeholder: if field == KeyField::Domain { "example.com".into() } else { "Paste it here".into() },
                        selected: -1,
                        ..row("field")
                    });
                }
                rows.push(buttons(vec![button("provider-kind:", "Cancel", 1), self.change_button("provider-connect", "Connect", 0)]));
                cards.push(card(format!("Connect {}", p.name), "The key is stored as a secret and never shown again.", rows));
            }
            (ProviderStage::Done { provider, domains, held_by }, _) => {
                let mut rows: Vec<MRow> = domains.iter().map(|domain| line(domain, "Its key controls this domain")).collect();
                if domains.is_empty() {
                    rows.push(text("Its key controls no domain a site can use.", 0));
                }
                if !held_by.is_empty() {
                    rows.push(text(format!("Its other domains are used through {}, connected earlier.", held_by.join(", ")), 0));
                }
                rows.push(buttons(vec![button("provider-done", "Done", 0)]));
                cards.push(toned(card(format!("✓  {provider} is connected"), "", rows), 2));
            }
            _ => {}
        }
        if let Some(e) = &snap.providers_error {
            cards.push(toned(card("Couldn't read the providers", e, Vec::new()), 3));
        }
        Page { crumbs: self.crumbs(), title: "Domain providers".into(), actions: vec![self.change_button("provider-add", "Add a provider", 0)], cards }
    }

    fn system(&self, snap: &Snapshot) -> Page {
        let updates = snap.pending_updates();
        let mut cards = Vec::new();
        if let Some(base) = words::nodo_update(snap) {
            cards.push(card(
                "A Nodo update is ready",
                format!("{} → {}", base.from, base.to),
                vec![buttons(vec![button("open:updates", "Review", 0)])],
            ));
        }
        cards.push(card(
            "",
            "",
            vec![
                link("open:updates", "Updates", snap.system_note.clone().unwrap_or_else(|| words::system_row_summary(snap, "Updates")), if updates > 0 { format!("{updates} ready") } else { String::new() }, 1),
                link("open:auto", "Automatic updates", words::counted(snap.auto_update.enabled_count() as u64, "app"), "", 0),
                link("open:undo", "Undo an update", "", "", 0),
                link("open:backups", "Backups", "", "", 0),
                link("open:network", "Network", "", "", 0),
                link("open:logs:", "Device logs", "", "", 0),
                link("open:ssh", "SSH keys", "", "", 0),
                link("open:screen", "Screen", if self.screen.pin_set { "Screen PIN set" } else { "No screen PIN" }, "", 0),
                // Shown, not changed here: it is turned on and off over SSH.
                line("Web GUI", words::system_row_summary(snap, "Web GUI")),
            ],
        ));
        Page { crumbs: Vec::new(), title: "System".into(), actions: Vec::new(), cards }
    }

    fn updates(&self, snap: &Snapshot) -> Page {
        let mut cards = Vec::new();
        // The Nodo's own update is never part of the apps' batch.
        if let Some(base) = words::nodo_update(snap) {
            let note = if base.note == UpdateNote::RestartNeeded { "The Nodo restarts to finish it." } else { "" };
            cards.push(card(
                "A Nodo update is ready",
                format!("{} → {}", base.from, base.to),
                vec![text(note, 0), buttons(vec![self.change_button("update-system", "Update the Nodo", 0)])],
            ));
        }
        let mut list: Vec<MRow> = snap
            .update_rows()
            .into_iter()
            .filter(|u| u.name != SYSTEM_BASE)
            .map(|u| {
                let note = match &u.note {
                    UpdateNote::Ready => String::new(),
                    UpdateNote::RestartNeeded => "needs a restart".into(),
                    UpdateNote::NewPermission(p) => format!("asks for more: {p} (review in the TUI)"),
                    UpdateNote::Skipped => "skipped".into(),
                    UpdateNote::Blocked(b) => b.clone(),
                    UpdateNote::Withdrawal(w) => w.clone(),
                };
                let grows = u.grows.as_ref().map(|grows| format!("  ·  grows its storage, {grows} (in the TUI)")).unwrap_or_default();
                let selectable = matches!(u.note, UpdateNote::Ready | UpdateNote::RestartNeeded) && !u.held;
                if selectable {
                    toggle(format!("update:{}", u.name), &u.name, format!("{} → {}  ·  {}  {note}{grows}", u.from, u.to, u.source), self.checked.contains(&u.name))
                } else {
                    MRow { enabled: false, toggle: 0, value: shared(note), ..line(&u.name, format!("{} → {}", u.from, u.to)) }
                }
            })
            .collect();
        let ticked = self.checked.iter().filter(|name| *name != SYSTEM_BASE).count();
        let mut apply = self.change_button("apply-updates", format!("Update {ticked}"), 0);
        apply.enabled = ticked > 0;
        list.push(buttons(vec![apply]));
        cards.push(card("", snap.overview.updates_checked.clone(), list));
        Page { crumbs: self.crumbs(), title: "Updates".into(), actions: vec![button("check-updates", "Check now", 1)], cards }
    }

    fn auto_update(&self) -> Page {
        let Some(auto) = &self.auto else {
            return Page { crumbs: self.crumbs(), title: "Automatic updates".into(), actions: Vec::new(), cards: Vec::new() };
        };
        let a = &auto.draft;
        let selected = a.frequencies.iter().position(|f| *f == a.frequency).unwrap_or(0);
        let mut rows = vec![segments("auto-frequency", "How often", &a.frequencies, selected), line("When", format!("{}  ·  next {}", auto.window(), a.next_run))];
        for e in &a.packages {
            rows.push(toggle(format!("auto:{}", e.name), &e.name, if e.third_party { format!("{} · from a third party", e.note) } else { e.note.clone() }, e.enabled));
        }
        rows.push(text("Updates that need a restart always wait for you.", 0));
        let mut save = self.change_button("auto-save", "Save", 0);
        save.enabled = auto.changed();
        rows.push(buttons(vec![save]));
        Page { crumbs: self.crumbs(), title: "Automatic updates".into(), actions: Vec::new(), cards: vec![card("", "", rows)] }
    }

    fn undo(&self) -> Page {
        let flow = &self.rollbacks;
        let mut rows: Vec<MRow> = match (flow.options(), flow.view.is_some()) {
            ([], false) if flow.busy => vec![text("Reading…", 0)],
            ([], _) => vec![text("Nothing to undo: no app has an earlier version kept.", 0)],
            (options, _) => options
                .iter()
                .map(|r| match &r.blocked {
                    Some(why) => MRow { enabled: false, ..line(&r.package, format!("{} → {}  ·  {why}", r.current, r.previous)) },
                    None => link(format!("undo:{}", r.package), &r.package, format!("Back from {} to {}", r.current, r.previous), "Undo", 1),
                })
                .collect(),
        };
        if let Some(result) = &flow.result {
            use nodo_ui_core::words::Tone;
            rows.push(match nodo_ui_core::words::rollback_result(result) {
                (Tone::Good, words) => text(format!("✓  {words}"), 2),
                (Tone::Working, words) => text(format!("▲  {words}"), 1),
                (Tone::Bad, words) => text(format!("✕  {words}"), 3),
            });
        }
        if let Some(error) = &flow.error {
            rows.push(text(format!("✕  {error}"), 3));
        }
        Page { crumbs: self.crumbs(), title: "Undo an update".into(), actions: Vec::new(), cards: vec![card("", "Each app keeps the version from before its last update.", rows)] }
    }

    fn logs_page(&self, snap: &Snapshot, source: Option<&str>) -> Page {
        let Some(source) = source else {
            let rows = snap.log_sources.iter().map(|s| link(format!("open:logs:{s}"), s, "", "", 0)).collect();
            return Page { crumbs: self.crumbs(), title: "Device logs".into(), actions: Vec::new(), cards: vec![card("", "Search and saving to a file are in the TUI.", rows)] };
        };
        let mut rows = vec![toggle("problems", "Problems only", "", self.logs.errors_only)];
        let lines = self.logs.lines("");
        if lines.is_empty() {
            rows.push(text(self.logs.empty_text(), 0));
        }
        for l in lines.into_iter().rev().take(80) {
            rows.push(MRow { title: shared(&l.text), tone: if l.kind == LogKind::Error { 3 } else { 0 }, ..row("log") });
        }
        for s in &self.logs.view.suggestion {
            rows.push(text(s, 1));
        }
        Page { crumbs: self.crumbs(), title: format!("{source} log"), actions: Vec::new(), cards: vec![card("", "Newest first", rows)] }
    }

    fn backups_page(&self) -> Page {
        let flow = &self.backups;
        let mut rows = Vec::new();
        if flow.list.is_empty() {
            rows.push(text(if flow.busy { "Reading…" } else { "No backups yet." }, 0));
        }
        for b in &flow.list {
            rows.push(line(&b.name, words::size(b.size_bytes)));
        }
        if let Some(error) = &flow.error {
            rows.push(text(format!("✕  {error}"), 3));
        }
        rows.push(text("A backup holds your settings, secrets and the device key, encrypted with its own passphrase. App data on the drive is not included.", 0));
        rows.push(buttons(vec![self.change_button("backup", "Back up now", 0)]));
        Page { crumbs: self.crumbs(), title: "Backups".into(), actions: Vec::new(), cards: vec![card("", "", rows)] }
    }

    fn network_page(&self) -> Page {
        let flow = &self.network;
        let mut cards = Vec::new();
        let mut rows = Vec::new();
        match (&flow.wifi, &flow.wifi_read) {
            (Some(w), _) => {
                if let Some(why) = &w.unavailable {
                    rows.push(text(why, 0));
                } else {
                    rows.push(toggle("wifi:", "Wi-Fi", flow.joined().map(|ssid| format!("Joined {ssid}")).unwrap_or_default(), w.enabled));
                    if w.enabled {
                        for listed in flow.wifi_rows() {
                            let Some((ssid, security, saved)) = flow.wifi_row(listed) else { continue };
                            let joined = flow.joined() == Some(ssid.as_str());
                            let mut facts = Vec::new();
                            match listed {
                                nodo_ui_core::flows::network::WifiRow::InRange(index) => facts.push(format!("{} dBm", flow.networks[index].signal_dbm)),
                                nodo_ui_core::flows::network::WifiRow::Saved(_) => facts.push("not in range".to_owned()),
                            }
                            if security == nodo_api::WifiSecurity::Open {
                                facts.push("open".to_owned());
                            }
                            if saved {
                                facts.push("saved".to_owned());
                            }
                            let id = if joined { String::new() } else { format!("wifi-join:{ssid}") };
                            rows.push(link(id, &ssid, facts.join(" · "), if joined { "Joined" } else { "" }, 2));
                        }
                    }
                }
            }
            (None, RadioRead::Failed(why)) => rows.push(text(format!("✕  {why}"), 3)),
            (None, RadioRead::NoAnswer) => rows.push(text("No answer yet. The Nodo may be busy with something else.", 0)),
            (None, _) => rows.push(text("Reading Wi-Fi…", 0)),
        }
        if let (Some(busy), true) = (&flow.busy, flow.wifi.is_some()) {
            rows.push(text(busy, 0));
        }
        if let Some(error) = &flow.error {
            rows.push(text(format!("✕  {error}"), 3));
        }
        cards.push(card("Wi-Fi", "", rows));
        cards.push(card("Bluetooth", "", vec![text("Pairing is in the TUI for now.", 0)]));
        Page { crumbs: self.crumbs(), title: "Network".into(), actions: Vec::new(), cards }
    }

    fn ssh_page(&self) -> Page {
        let flow = &self.ssh;
        let mut rows = Vec::new();
        for k in &flow.keys {
            rows.push(MRow { mono: true, ..link(format!("ssh-remove:{}", k.fingerprint), &k.comment, &k.fingerprint, "Remove", 3) });
        }
        if flow.keys.is_empty() {
            rows.push(text(if flow.busy { "Reading…" } else { "No keys: nobody can log in over SSH." }, 0));
        }
        if let Some(error) = &flow.error {
            rows.push(text(format!("✕  {error}"), 3));
        }
        rows.push(buttons(vec![self.change_button("ssh-add", "Add a key", 0)]));
        Page { crumbs: self.crumbs(), title: "SSH keys".into(), actions: Vec::new(), cards: vec![card("", "Keys that may log in as root.", rows)] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodo_ui_core::mock::MockBackend;
    use slint::Model;

    const PIN: &str = "482913";

    fn manage_over(mock: MockBackend) -> Manage {
        let mut m = Manage::new(Some(EffectWorker::spawn(Box::new(mock))));
        m.open();
        m.drain_blocking();
        m
    }

    fn manage() -> Manage {
        manage_over(MockBackend::new())
    }

    /// The same device behind the touchscreen's socket, where changes need the screen PIN.
    fn locked_screen() -> Manage {
        let mut mock = MockBackend::new();
        mock.knobs.screen_pin = Some(PIN.to_owned());
        manage_over(mock)
    }

    fn rows(page: &Page) -> Vec<MRow> {
        page.cards.iter().flat_map(|c| c.rows.iter().collect::<Vec<_>>()).collect()
    }

    fn enter_pin(m: &mut Manage, pin: &str) {
        for digit in pin.chars() {
            m.key(&format!("pin:{digit}"));
        }
        m.drain_blocking();
    }

    /// Replaces what the keyboard holds and presses Done.
    fn type_and_done(m: &mut Manage, typed: &str) {
        while !m.keyboard_view().text.is_empty() {
            m.key("back");
        }
        m.key(typed);
        m.key("done");
        m.drain_blocking();
    }

    fn state_of(m: &Manage, app: &str) -> Option<nodo_api::ServiceState> {
        AppPage::new(app).state(m.snapshot.as_ref().expect("the device was read"))
    }

    #[test]
    fn opens_on_home_with_the_snapshot() {
        let m = manage();
        let page = m.page();
        assert_eq!(page.title, "Your Nodo");
        assert!(!page.cards.is_empty());
    }

    #[test]
    fn every_tab_has_a_page() {
        let mut m = manage();
        for t in 0..4 {
            m.tab_clicked(t);
            m.drain_blocking();
            assert!(!m.page().title.is_empty());
        }
    }

    #[test]
    fn system_rows_open_their_screens() {
        let mut m = manage();
        m.tab_clicked(3);
        for target in ["updates", "auto", "undo", "backups", "network", "logs:", "ssh", "screen"] {
            m.act(&format!("open:{target}"));
            m.drain_blocking();
            assert!(!m.page().title.is_empty(), "{target}");
            m.act("back");
            m.drain_blocking();
            assert_eq!(m.page().title, "System", "{target}");
        }
    }

    #[test]
    fn the_pin_pad_collects_six_digits_and_cancel_forgets_them() {
        let mut m = manage();
        m.screen.pin_set = true;
        m.ask_pin();
        for d in ["1", "2", "3"] {
            m.key(&format!("pin:{d}"));
        }
        assert_eq!(m.pin_view().digits, 3);
        m.key("pin:cancel");
        assert!(!m.pin_view().shown);
    }

    #[test]
    fn without_a_pin_a_change_says_how_to_set_one() {
        let mut m = manage();
        m.screen.pin_set = false;
        m.ask_pin();
        assert!(m.dialog_view().text.contains("screen-pin"));
    }

    #[test]
    fn a_change_the_locked_screen_refused_runs_again_once_the_pin_unlocks_it() {
        let mut m = locked_screen();
        assert!(m.pin_set() && !m.unlocked());
        assert_eq!(state_of(&m, "moneropay"), Some(nodo_api::ServiceState::Running));

        // A change the helper is asked for directly.
        m.act("stop:moneropay");
        m.drain_blocking();
        assert!(m.pin_view().shown, "the refusal asks for the PIN");
        assert_eq!(m.toast().0, "", "and is not said as a failure");
        assert_eq!(state_of(&m, "moneropay"), Some(nodo_api::ServiceState::Running));

        enter_pin(&mut m, "111111");
        assert!(m.pin_view().shown && m.pin_view().wrong);
        assert_eq!(state_of(&m, "moneropay"), Some(nodo_api::ServiceState::Running));

        enter_pin(&mut m, PIN);
        assert!(!m.pin_view().shown && m.unlocked());
        assert_eq!(state_of(&m, "moneropay"), Some(nodo_api::ServiceState::Stopped), "the same change ran again");
        assert!(!m.toast().1);

        // Unlocked, the next change needs no PIN.
        m.act("start:moneropay");
        m.drain_blocking();
        assert!(!m.pin_view().shown);
        assert_eq!(state_of(&m, "moneropay"), Some(nodo_api::ServiceState::Running));
    }

    #[test]
    fn a_refused_change_that_reaches_the_helper_as_text_asks_for_the_pin_too_and_cancel_drops_it() {
        let mut m = locked_screen();
        let on = |m: &Manage| m.snapshot.as_ref().unwrap().feeds.iter().find(|f| f.name == "community").unwrap().status != FeedStatus::Disabled;
        assert!(on(&m));
        m.toggled("feed:community", false);
        m.drain_blocking();
        assert!(m.pin_view().shown);
        m.key("pin:cancel");
        m.drain_blocking();
        assert!(!m.pin_view().shown);
        assert!(on(&m), "nothing was changed");
        assert_eq!(m.toast().0, "", "and giving up says nothing more");

        // Asked again and unlocked, it goes through.
        m.toggled("feed:community", false);
        m.drain_blocking();
        enter_pin(&mut m, PIN);
        assert!(!on(&m));
    }

    #[test]
    fn the_devices_own_lock_is_not_answered_with_the_pin_pad() {
        let mut mock = MockBackend::new();
        mock.knobs.screen_pin = Some(PIN.to_owned());
        mock.key_locked = true;
        let mut m = manage_over(mock);
        m.act("unlock");
        enter_pin(&mut m, PIN);
        assert!(m.unlocked());
        // The screen still shows the device as unlocked, as when it was locked since the last read.
        unlock::mark_unlocked(m.snapshot.as_mut().unwrap());
        m.tab_clicked(2);
        m.act("open:providers");
        m.act("provider-add");
        m.drain_blocking();
        m.act("provider-kind:Porkbun");
        assert_eq!(m.providers.stage, ProviderStage::Key);
        m.act("provider-field:0");
        type_and_done(&mut m, "pk1_mock");
        m.act("provider-field:1");
        type_and_done(&mut m, "sk1_mock");
        m.act("provider-connect");
        m.drain_blocking();
        assert!(!m.pin_view().shown, "the PIN would not lift it");
        assert!(matches!(&m.providers.test, Some(Err(why)) if why.contains("unlock the device")), "{:?}", m.providers.test);
        assert!(m.provider_boxes.1.is_empty() && m.provider_boxes.2.is_empty(), "nothing typed is kept");
    }

    #[test]
    fn a_form_filled_in_before_the_pin_is_not_typed_again() {
        let mut m = locked_screen();
        m.tab_clicked(3);
        m.act("open:ssh");
        m.drain_blocking();
        m.act("ssh-add");
        type_and_done(&mut m, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMockKeyMaterialForTheTouchscreenTest0000000 phone");
        assert!(m.pin_view().shown);
        enter_pin(&mut m, PIN);
        assert_eq!(m.ssh.keys.len(), 2, "{:?}", m.ssh.error);
        assert!(!m.ssh.busy);
    }

    #[test]
    fn a_field_that_does_not_fit_says_why_beside_it_and_is_not_saved() {
        let mut m = manage();
        m.act("open:settings:moneropay");
        m.drain_blocking();
        m.act("field-type:api_port");
        assert!(m.keyboard_view().shown);
        type_and_done(&mut m, "abc");
        let port = rows(&m.page()).into_iter().find(|r| r.id.as_str() == "field:api_port").expect("the field");
        assert_eq!((port.sub.as_str(), port.tone), ("Port must be a number from 1 to 65535.", 3));
        assert!(port.changed);

        m.act("settings-save");
        m.drain_blocking();
        assert_eq!(m.toast(), ("Fix the marked field before saving. Nothing was changed.".to_owned(), true));
        assert!(!m.settings.as_ref().unwrap().changed_indices().is_empty(), "the draft is kept to fix");

        // Fixed, it saves, and the core says what saving did.
        m.act("field-type:api_port");
        type_and_done(&mut m, "5001");
        m.act("settings-save");
        m.drain_blocking();
        assert_eq!(m.toast(), ("Saved. moneropay restarted and passed its health check.".to_owned(), false));
        assert!(m.settings.as_ref().unwrap().changed_indices().is_empty());
    }

    #[test]
    fn leaving_changed_settings_asks_first() {
        let mut m = manage();
        m.tab_clicked(1);
        m.act("open:app:moneropay");
        m.act("open:settings:moneropay");
        m.drain_blocking();
        m.toggled("bool:allow_lan", false);
        m.tab_clicked(0);
        assert_eq!(m.dialog_view().title.as_str(), "Discard your changes?");
        assert_eq!(m.page().title, "moneropay settings", "still there until answered");
        m.act("dialog-close");
        m.act("back");
        m.act("dialog-yes");
        assert_eq!(m.page().title, "moneropay");
    }

    #[test]
    fn an_update_that_asks_for_more_is_sent_to_the_terminal_and_not_applied() {
        let mut m = manage();
        let offered = |m: &Manage| m.snapshot.as_ref().unwrap().updates.iter().any(|u| u.name == "wallet-relay");
        assert!(offered(&m));
        // From the app's own page, the one way the screen ticks it.
        m.tab_clicked(1);
        m.act("open:app:wallet-relay");
        m.act("app-update:wallet-relay");
        assert_eq!(m.page().title, "Updates");
        assert_eq!(m.checked, ["wallet-relay"]);
        m.act("apply-updates");
        m.drain_blocking();
        let (said, error) = m.toast();
        assert!(said.contains("wallet-relay asks for a new permission") && said.contains("terminal"), "{said}");
        assert!(!error);
        assert_eq!(m.page().title, "Updates", "no review page opens here");
        assert!(offered(&m), "nothing was applied");

        // One that asks for nothing new is applied, and one that fails says so (the mock's
        // moneropay update fails its health check).
        m.act("update-system");
        m.drain_blocking();
        assert_eq!(m.toast(), ("Updated system base.".to_owned(), false));
        assert!(!m.snapshot.as_ref().unwrap().updates.iter().any(|u| u.name == SYSTEM_BASE));
        m.checked = vec!["moneropay".to_owned()];
        m.act("apply-updates");
        m.drain_blocking();
        assert_eq!(m.toast(), ("moneropay: moneropay 2.7.1 failed its health check".to_owned(), true));
    }

    #[test]
    fn an_update_that_grows_storage_is_sent_to_the_terminal_and_not_applied() {
        let mut mock = MockBackend::new();
        mock.updates.iter_mut().find(|u| u.name == "moneropay").unwrap().grows = Some("256 MB to 512 MB".to_owned());
        let mut m = manage_over(mock);
        m.tab_clicked(3);
        m.act("open:updates");
        assert!(m.checked.contains(&"moneropay".to_owned()));
        m.act("apply-updates");
        m.drain_blocking();
        let (said, _) = m.toast();
        assert!(said.contains("device passphrase") && said.contains("moneropay: 256 MB to 512 MB") && said.contains("terminal"), "{said}");
        assert!(m.snapshot.as_ref().unwrap().updates.iter().any(|u| u.name == "moneropay"), "nothing was applied");
    }

    #[test]
    fn a_new_key_is_trusted_only_when_the_sources_name_is_typed() {
        let mut m = manage();
        m.tab_clicked(1);
        m.act("open:get-apps");
        m.act("open:sources");
        m.act("open:key:old-tools");
        m.drain_blocking();
        assert_eq!(m.page().title, "Check before you trust");
        m.act("trust-key:old-tools");
        type_and_done(&mut m, "old tools");
        assert!(m.keyboard_view().shown, "the keyboard stays");
        assert_eq!(m.keyboard_view().error.as_str(), "That isn't the source's name. Type old-tools exactly.");
        assert_eq!(m.page().title, "Check before you trust");

        type_and_done(&mut m, "old-tools");
        assert!(!m.keyboard_view().shown);
        assert_eq!(m.page().title, "Sources");
        assert_eq!(m.toast(), ("old-tools now uses the new key.".to_owned(), false));
    }

    #[test]
    fn turning_wifi_off_while_it_is_the_only_link_asks_first() {
        let mut mock = MockBackend::new();
        mock.wifi.wired_up = false;
        let mut m = manage_over(mock);
        m.tab_clicked(3);
        m.act("open:network");
        m.drain_blocking();
        assert!(!m.network.networks.is_empty(), "the page looked for networks by itself");

        m.toggled("wifi:", false);
        let asked = m.dialog_view();
        assert_eq!(asked.title.as_str(), "Turn Wi-Fi off?");
        assert!(asked.text.contains("may cut the connection you are using"));
        let labels: Vec<(String, i32)> = asked.buttons.iter().map(|b| (b.label.to_string(), b.kind)).collect();
        assert_eq!(labels, [("Cancel".to_owned(), 0), ("Turn off".to_owned(), 2)], "Cancel is the one that stands out");
        m.act("dialog-close");
        m.drain_blocking();
        assert!(m.network.wifi.as_ref().unwrap().enabled, "cancelled, nothing happens");

        m.toggled("wifi:", false);
        m.act("dialog-yes");
        m.drain_blocking();
        assert!(!m.network.wifi.as_ref().unwrap().enabled);
    }

    #[test]
    fn an_open_network_is_joined_only_after_its_question_and_a_closed_one_after_its_passphrase() {
        let mut m = manage();
        m.tab_clicked(3);
        m.act("open:network");
        m.drain_blocking();

        m.act("wifi-join:Cafe Libre");
        assert_eq!(m.dialog_view().title.as_str(), "Join Cafe Libre, an open network?");
        assert!(!m.keyboard_view().shown);
        m.act("dialog-yes");
        m.drain_blocking();
        assert_eq!(m.network.joined(), Some("Cafe Libre"));

        m.act("wifi-join:neighbour");
        assert!(m.keyboard_view().shown && m.keyboard_view().secret);
        m.key("done");
        assert_eq!(m.keyboard_view().error.as_str(), "Type the passphrase of neighbour.");
        type_and_done(&mut m, "not ours at all");
        assert!(!m.keyboard_view().shown);
        assert_eq!(m.network.joined(), Some("neighbour"));
    }

    #[test]
    fn a_site_goes_on_the_internet_only_after_the_check_and_its_address_typed() {
        let mut m = manage();
        let public = |m: &Manage| m.snapshot.as_ref().unwrap().web.sites.iter().find(|s| s.package == "moneropay").unwrap().exposure.public;
        m.act("open:hosting:moneropay");
        m.toggled("expose:public", true);
        m.act("hosting-save");
        m.drain_blocking();
        // The names were saved and the check ran; the internet itself waits for the confirmation.
        assert!(!public(&m));
        let asked = m.dialog_view();
        assert_eq!(asked.title.as_str(), "Put moneropay on the internet?");
        assert!(asked.text.contains("https://moneropay.example.com"), "{}", asked.text);
        assert_eq!(asked.buttons.iter().map(|b| b.kind).collect::<Vec<_>>(), [0, 2], "Cancel stands out");

        m.act("dialog-yes");
        type_and_done(&mut m, "moneropay.example.org");
        assert_eq!(m.keyboard_view().error.as_str(), "That isn't the address. Type moneropay.example.com exactly.");
        assert!(!public(&m));

        type_and_done(&mut m, "moneropay.example.com");
        assert!(public(&m));
        assert_eq!(m.toast(), ("It is on the internet now.".to_owned(), false));
        assert_eq!(m.page().title, "Your Nodo", "the Hosting page is left");
    }

    #[test]
    fn a_failed_check_is_shown_and_does_not_lead_to_the_confirmation() {
        let mut mock = MockBackend::new();
        mock.web.sites.iter_mut().find(|s| s.package == "shop").unwrap().exposure.public = false;
        let mut m = manage_over(mock);
        m.act("open:hosting:shop");
        m.toggled("expose:public", true);
        m.act("hosting-save");
        m.drain_blocking();
        let shown = m.dialog_view();
        assert_eq!(shown.title.as_str(), "shop can't be reached from the internet yet");
        assert!(shown.text.contains("✕  Port 443 is not reachable from outside"));
        assert!(!shown.buttons.iter().any(|b| b.id.as_str() == "reach-continue" || b.id.as_str() == "dialog-yes"));
        // Checking again passes in the mock, and leads to the confirmation.
        m.act("reach-again");
        m.drain_blocking();
        assert_eq!(m.dialog_view().title.as_str(), "Put shop on the internet?");
    }

    #[test]
    fn a_sites_page_shows_the_other_names_it_answers_at() {
        let mut m = manage();
        m.act("open:hosting:shop");
        m.drain_blocking();
        let page = m.page();
        let row = rows(&page).into_iter().find(|r| r.title.as_str() == "Also answers at").expect("the row");
        assert_eq!(row.sub.as_str(), "example.com (redirects here)");
    }

    #[test]
    fn removing_an_app_asks_in_the_cores_words() {
        let mut m = manage();
        m.tab_clicked(1);
        m.act("open:app:moneropay");
        m.act("remove:moneropay");
        let asked = m.dialog_view();
        assert_eq!(asked.title.as_str(), "Remove moneropay?");
        assert!(asked.text.contains("A site still online has to be taken offline first, in Hosting."));
        assert!(asked.danger);
        m.act("dialog-yes");
        assert_eq!(m.page().title, "Installed", "confirming goes to the list of apps");
    }

    #[test]
    fn a_locked_app_says_so_without_naming_a_key() {
        let mut m = manage();
        m.act("start:wallet-relay");
        assert_eq!(m.toast(), ("wallet-relay is locked. Unlock the device from Home first.".to_owned(), true));
    }

    #[test]
    fn the_device_passphrase_is_asked_again_with_the_reason_after_a_wrong_one() {
        let mut m = manage();
        m.act("device-unlock");
        assert!(m.keyboard_view().shown && m.keyboard_view().secret);
        m.key("done");
        assert_eq!(m.keyboard_view().error.as_str(), "Enter the device passphrase.");
        type_and_done(&mut m, "not the passphrase");
        assert!(m.keyboard_view().shown);
        assert_eq!(m.keyboard_view().error.as_str(), "That passphrase did not unlock this device. Try again.");
        // The core's delay after a wrong try is kept.
        type_and_done(&mut m, nodo_ui_core::mock::MOCK_PASSPHRASE);
        assert!(m.keyboard_view().error.starts_with("Please wait"), "{}", m.keyboard_view().error);
        m.unlock.as_mut().unwrap().retry_at = None;
        type_and_done(&mut m, nodo_ui_core::mock::MOCK_PASSPHRASE);
        assert!(!m.keyboard_view().shown && m.unlock.is_none());
        assert!(!m.snapshot.as_ref().unwrap().is_locked());
    }

    #[test]
    fn a_backup_needs_its_passphrase_twice_and_long_enough() {
        let mut m = manage();
        m.tab_clicked(3);
        m.act("open:backups");
        m.drain_blocking();
        m.act("backup");
        assert!(m.keyboard_view().label.starts_with("Device passphrase"));
        type_and_done(&mut m, nodo_ui_core::mock::MOCK_PASSPHRASE);
        type_and_done(&mut m, "short");
        type_and_done(&mut m, "short");
        assert_eq!(m.keyboard_view().error.as_str(), "A backup passphrase needs at least 12 characters.");
        type_and_done(&mut m, "a long enough one");
        type_and_done(&mut m, "a long enough two");
        assert_eq!(m.keyboard_view().error.as_str(), "The two backup passphrases differ. Type them again.");
        type_and_done(&mut m, "a long enough one");
        type_and_done(&mut m, "a long enough one");
        assert!(!m.keyboard_view().shown);
        assert_eq!(m.toast(), ("Backup nodo-1762646400.nodobak written.".to_owned(), false), "{:?}", m.backups.error);
        assert!(!m.backups.busy && m.backups.stage == backup::Stage::List);
    }

    #[test]
    fn idle_returns_to_the_tiles() {
        let mut m = manage();
        m.idle_return = Duration::from_millis(1);
        std::thread::sleep(Duration::from_millis(5));
        m.poll();
        assert!(!m.open);
    }
}
