//! Management on the touchscreen: Home, Apps, Web and System, over nodo-tui's helper backend.
//!
//! The data and every helper call are nodo-tui's (`ScreenData`, `HelperBackend`), so the screen
//! and the terminal show and do the same things. Calls run on a worker thread, because a restart
//! or an update can take minutes; results come back as closures applied on the UI thread.
//!
//! The screen talks to the helper on its own socket, where the helper answers anything that only
//! reads and refuses changes until the screen is unlocked with the screen PIN (nodo-helper's
//! `screen` module). The screen asks for the PIN when a change comes back `Locked`.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use nodo_api::{ApiError, Backend, Request, Response, ScreenStatus};
use nodo_tui::model::*;
use nodo_tui::secret::Secret;
use slint::{ModelRc, SharedString, VecModel};

use crate::{MButton, MCard, MChip, MDialog, MKeyboard, MPicker, MPin, MRow};

/// Everything a backend offers the screens.
pub trait Data: Backend + ScreenData + Send {}
impl<T: Backend + ScreenData + Send> Data for T {}

type Job = Box<dyn FnOnce(&mut dyn Data) -> Done + Send>;
type Done = Box<dyn FnOnce(&mut Manage) + Send>;

/// Runs helper calls one at a time, off the UI thread.
pub struct Worker {
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

impl Worker {
    pub fn start(mut data: Box<dyn Data>) -> Self {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (done_tx, done) = mpsc::channel::<Done>();
        std::thread::spawn(move || {
            for job in job_rx {
                let result = job(data.as_mut());
                if done_tx.send(result).is_err() {
                    return;
                }
            }
        });
        Self { jobs, done }
    }

    /// Runs a job synchronously: for screenshots and tests, where there is no event loop.
    pub fn drain_blocking(&self, manage: &mut Manage) {
        while manage.pending > 0 {
            match self.done.recv_timeout(Duration::from_secs(5)) {
                Ok(done) => {
                    manage.pending -= 1;
                    done(manage);
                }
                Err(_) => break,
            }
        }
    }
}

const TABS: [&str; 4] = ["Home", "Apps", "Web", "System"];

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
    ScreenSettings,
}

/// What the keyboard is editing, and what Done does with it.
#[derive(Debug, Clone)]
enum Editing {
    Field(String),
    Search,
    PickerSearch,
    SourceAddress,
    TrustName(String),
    DevicePassphrase,
    InstallPassphrase(PackageId),
    PublicHost(String),
    SshKey,
    BackupPassphrase,
    WifiPassphrase(String),
    ProviderField(usize),
}

struct Keyboard {
    editing: Editing,
    label: String,
    text: String,
    secret: bool,
    mono: bool,
    shift: bool,
    error: String,
}

/// A settings page being edited: the form as read, and the owner's unsaved changes.
#[derive(Default)]
struct Edit {
    form: Option<SettingsForm>,
    /// The form was asked for and answered, with or without settings.
    loaded: bool,
    changes: BTreeMap<String, FieldValue>,
    /// What a field held before "Type a value instead", for its undo.
    before: BTreeMap<String, FieldValue>,
    consequences: BTreeMap<String, Vec<String>>,
}

/// A change the owner asked for while the screen was locked; done once the PIN unlocks it.
type Retry = Box<dyn FnOnce(&mut Manage) + Send>;

struct Picker {
    title: String,
    sub: String,
    query: String,
    field: Option<String>,
    rows: Vec<(String, String, String)>,
}

#[derive(Default)]
struct Pin {
    shown: bool,
    digits: String,
    wrong: bool,
    busy: bool,
}

pub struct Manage {
    pub open: bool,
    tab: usize,
    stack: Vec<Screen>,
    snapshot: Option<Snapshot>,
    screen: ScreenStatus,
    pending: usize,
    jobs: Option<Sender<Job>>,
    busy: Option<String>,
    toast: Option<(String, bool, Instant)>,
    dialog: Option<(String, String, bool, Vec<MButton>)>,
    dialog_actions: BTreeMap<String, Retry>,
    keyboard: Option<Keyboard>,
    picker: Option<Picker>,
    pin: Pin,
    retry: Option<Retry>,
    edit: Edit,
    query: String,
    detail: Option<PackageDetail>,
    install: Option<InstallSnapshot>,
    probe: Option<FeedProbe>,
    source_address: String,
    key_change: Option<KeyChange>,
    logs: Option<LogView>,
    problems_only: bool,
    rollbacks: Vec<nodo_api::RollbackOption>,
    backups: Result<Vec<nodo_api::BackupInfo>, String>,
    ssh_keys: Result<Vec<nodo_api::SshKey>, String>,
    wifi: Option<nodo_api::WifiStatus>,
    networks: Vec<nodo_api::WifiNetwork>,
    selected_updates: Vec<String>,
    auto: Option<AutoUpdatePrefs>,
    hosting: Option<(String, nodo_api::Exposure, String)>,
    provider_kind: Option<Provider>,
    provider_values: Vec<String>,
    last_touch: Instant,
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

/// The page the screen shows: breadcrumb, title, header actions and cards.
pub struct Page {
    pub crumbs: Vec<String>,
    pub title: String,
    pub actions: Vec<MButton>,
    pub cards: Vec<MCard>,
}

impl Manage {
    pub fn new(worker: Option<&Worker>) -> Self {
        Self {
            version: 0,
            offline: None,
            open: false,
            tab: 0,
            stack: vec![Screen::Home],
            snapshot: None,
            screen: ScreenStatus { pin_set: false, unlocked: false },
            pending: 0,
            jobs: worker.map(|w| w.jobs.clone()),
            busy: None,
            toast: None,
            dialog: None,
            dialog_actions: BTreeMap::new(),
            keyboard: None,
            picker: None,
            pin: Pin::default(),
            retry: None,
            edit: Edit::default(),
            query: String::new(),
            detail: None,
            install: None,
            probe: None,
            source_address: String::new(),
            key_change: None,
            logs: None,
            problems_only: true,
            rollbacks: Vec::new(),
            backups: Ok(Vec::new()),
            ssh_keys: Ok(Vec::new()),
            wifi: None,
            networks: Vec::new(),
            selected_updates: Vec::new(),
            auto: None,
            hosting: None,
            provider_kind: None,
            provider_values: Vec::new(),
            last_touch: Instant::now(),
            idle_return: Duration::from_secs(120),
            prefs: crate::screen_prefs::Prefs::default(),
        }
    }

    /// Takes finished helper calls. Call often from the UI thread.
    pub fn poll(&mut self, worker: Option<&Worker>) {
        if let Some(worker) = worker {
            while let Ok(done) = worker.done.try_recv() {
                self.pending = self.pending.saturating_sub(1);
                done(self);
                self.version += 1;
            }
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
        if matches!(self.top(), Screen::Installing)
            && self.install.as_ref().is_some_and(|i| i.outcome == InstallOutcome::InProgress)
            && self.pending == 0
        {
            self.run(|d| {
                let snap = d.poll_install();
                Box::new(move |m: &mut Manage| {
                    if let Some(s) = snap {
                        m.install = Some(s);
                    }
                })
            });
        }
    }

    fn run(&mut self, job: impl FnOnce(&mut dyn Data) -> Done + Send + 'static) {
        if let Some(jobs) = &self.jobs {
            if jobs.send(Box::new(job)).is_ok() {
                self.pending += 1;
            }
        }
    }

    /// Runs a helper call that changes something. A `Locked` answer asks for the PIN and runs
    /// the same call again once it unlocks.
    fn change(&mut self, what: &str, job: impl Fn(&mut dyn Data) -> Result<String, ApiError> + Send + Clone + 'static) {
        self.busy = Some(what.to_owned());
        let again = job.clone();
        self.run(move |d| {
            let result = job(d);
            Box::new(move |m: &mut Manage| {
                m.busy = None;
                match result {
                    Ok(message) => {
                        if !message.is_empty() {
                            m.say(&message, false);
                        }
                        m.reload();
                    }
                    Err(ApiError::Locked(reason)) => {
                        m.retry = Some(Box::new(move |m: &mut Manage| m.change("Working", again)));
                        m.ask_pin(&reason);
                    }
                    Err(e) => m.say(&e.to_string(), true),
                }
            })
        });
    }

    fn say(&mut self, message: &str, error: bool) {
        self.toast = Some((message.to_owned(), error, Instant::now()));
    }

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

    pub fn close(&mut self) {
        self.open = false;
        self.keyboard = None;
        self.picker = None;
        self.dialog = None;
        self.pin = Pin::default();
        self.run(|d| {
            let _ = d.call(Request::ScreenLock);
            Box::new(|_: &mut Manage| {})
        });
    }

    /// Reads the snapshot and whatever the current screen needs.
    pub fn reload(&mut self) {
        self.run(|d| {
            let snap = load_snapshot(d).ok();
            let screen = match d.call(Request::ScreenStatus) {
                Ok(Response::Screen(s)) => Some(s),
                _ => None,
            };
            Box::new(move |m: &mut Manage| {
                if snap.is_some() {
                    m.snapshot = snap;
                }
                if let Some(s) = screen {
                    m.screen = s;
                }
            })
        });
        self.load_screen();
    }

    fn top(&self) -> &Screen {
        self.stack.last().unwrap_or(&Screen::Home)
    }

    fn push(&mut self, screen: Screen) {
        self.stack.push(screen);
        self.load_screen();
    }

    fn back(&mut self) {
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

    /// Fetches what a screen shows beyond the snapshot.
    fn load_screen(&mut self) {
        match self.top().clone() {
            Screen::Settings(package) => {
                self.edit = Edit::default();
                self.run(move |d| {
                    let form = d.settings_form(&package);
                    Box::new(move |m: &mut Manage| {
                        m.edit.form = form;
                        m.edit.loaded = true;
                    })
                });
            }
            Screen::About(id) => {
                self.detail = None;
                self.run(move |d| {
                    let detail = d.package_detail(&id);
                    Box::new(move |m: &mut Manage| m.detail = detail)
                });
            }
            Screen::Logs(Some(source)) => {
                self.run(move |d| {
                    let view = d.logs(&source);
                    Box::new(move |m: &mut Manage| m.logs = Some(view))
                });
            }
            Screen::Undo => self.run(|d| {
                let list = match d.call(Request::ListRollbacks) {
                    Ok(Response::Rollbacks(list)) => list,
                    _ => Vec::new(),
                };
                Box::new(move |m: &mut Manage| m.rollbacks = list)
            }),
            Screen::Backups => self.run(|d| {
                let list = d.backups();
                Box::new(move |m: &mut Manage| m.backups = list)
            }),
            Screen::SshKeys => self.run(|d| {
                let list = d.ssh_keys();
                Box::new(move |m: &mut Manage| m.ssh_keys = list)
            }),
            Screen::Network => self.run(|d| {
                let wifi = match d.call(Request::WifiStatus) {
                    Ok(Response::Wifi(w)) => Some(w),
                    _ => None,
                };
                let nets = match d.call(Request::ScanWifi) {
                    Ok(Response::WifiNetworks(n)) => n,
                    _ => Vec::new(),
                };
                Box::new(move |m: &mut Manage| {
                    m.wifi = wifi;
                    m.networks = nets;
                })
            }),
            Screen::AutoUpdate => {
                self.auto = self.snapshot.as_ref().map(|s| s.auto_update.clone());
            }
            Screen::Updates => {
                self.selected_updates = self
                    .snapshot
                    .as_ref()
                    .map(|s| s.updates.iter().filter(|u| u.preselected && !u.held).map(|u| u.name.clone()).collect())
                    .unwrap_or_default();
            }
            Screen::KeyChanged(feed) => self.run(move |d| {
                let change = d.key_change(&feed);
                Box::new(move |m: &mut Manage| m.key_change = change)
            }),
            _ => {}
        }
    }

    fn ask_pin(&mut self, _reason: &str) {
        if !self.screen.pin_set {
            self.dialog(
                "Set a screen PIN first",
                "The screen can only look until a screen PIN is set. Set one over SSH with \
                 `nodo-tui --ctl screen-pin`, or in the TUI under System › Screen PIN.",
                false,
                vec![button("dialog-close", "OK", 0)],
            );
            self.retry = None;
            return;
        }
        self.pin = Pin { shown: true, ..Pin::default() };
    }

    fn dialog(&mut self, title: &str, text: &str, danger: bool, list: Vec<MButton>) {
        self.dialog = Some((title.to_owned(), text.to_owned(), danger, list));
    }

    /// A confirmation whose `confirm_id` button runs `then`.
    fn confirm(&mut self, title: &str, text: &str, danger: bool, label: &str, then: Retry) {
        self.dialog_actions.clear();
        self.dialog_actions.insert("dialog-yes".into(), then);
        self.dialog(
            title,
            text,
            danger,
            vec![button("dialog-close", "Cancel", 1), button("dialog-yes", label, if danger { 2 } else { 0 })],
        );
    }

    fn keyboard(&mut self, editing: Editing, label: &str, initial: &str, secret: bool, mono: bool) {
        self.keyboard = Some(Keyboard {
            editing,
            label: label.to_owned(),
            text: initial.to_owned(),
            secret,
            mono,
            shift: false,
            error: String::new(),
        });
    }

    // ---------------------------------------------------------------------------------------
    // Input from the window.

    pub fn tab_clicked(&mut self, tab: usize) {
        self.touched();
        self.go_tab(tab);
    }

    pub fn crumb(&mut self, index: usize) {
        self.touched();
        self.stack.truncate(index + 1);
        self.load_screen();
    }

    pub fn segment(&mut self, id: &str, n: usize) {
        self.touched();
        if id == "auto-frequency" {
            if let Some(a) = &mut self.auto {
                if let Some(f) = a.frequencies.get(n) {
                    a.frequency = f.clone();
                }
            }
        } else if id == "screen-timeout" {
            self.prefs.timeout_index = n;
            self.idle_return = self.prefs.timeout();
            self.prefs.save();
        } else if id == "screen-dim" {
            self.prefs.dim_index = n;
            self.prefs.save();
        } else if let Some(key) = id.strip_prefix("choice:") {
            if let Some(form) = &self.edit.form {
                if let Some(FieldKind::Choice(options)) = form.fields.iter().find(|f| f.key == key).map(|f| &f.kind) {
                    if let Some(v) = options.get(n) {
                        self.edit.changes.insert(key.to_owned(), FieldValue::Text(v.clone()));
                    }
                }
            }
        }
    }

    pub fn toggled(&mut self, id: &str, on: bool) {
        self.touched();
        let (what, rest) = id.split_once(':').unwrap_or((id, ""));
        let rest = rest.to_owned();
        match what {
            "bool" => {
                self.edit.changes.insert(rest, FieldValue::Bool(on));
            }
            "feed" => {
                self.change("Switching the source", move |d| {
                    d.toggle_feed(&rest);
                    Ok(String::new())
                });
            }
            "update" => {
                if on {
                    if !self.selected_updates.contains(&rest) {
                        self.selected_updates.push(rest);
                    }
                } else {
                    self.selected_updates.retain(|n| *n != rest);
                }
            }
            "auto" => {
                if let Some(a) = &mut self.auto {
                    if let Some(e) = a.packages.iter_mut().find(|e| e.name == rest) {
                        e.enabled = on;
                    }
                }
            }
            "expose" => {
                if let Some((_, exposure, _)) = &mut self.hosting {
                    match rest.as_str() {
                        "local" => exposure.local = on,
                        "onion" => exposure.onion = on,
                        _ => exposure.public = on,
                    }
                }
            }
            "problems" => self.problems_only = on,
            "wifi" => self.change("Switching Wi-Fi", move |d| {
                d.call(Request::SetWifiEnabled { enabled: on })?;
                Ok(String::new())
            }),
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
            "back" => {
                kb.text.pop();
            }
            "cancel" => self.keyboard = None,
            "done" => self.keyboard_done(),
            "paste" => {}
            t => {
                let t = if kb.shift { t.to_uppercase() } else { t.to_owned() };
                kb.text.push_str(&t);
                kb.shift = false;
                if matches!(kb.editing, Editing::Field(_)) && kb.text.ends_with("$(") {
                    // `$(` starts a reference: the generic picker opens on what was typed so far.
                    let Editing::Field(key) = kb.editing.clone() else { return };
                    kb.text.truncate(kb.text.len() - 2);
                    self.open_references(&key);
                }
                if let (Some(p), Some(kb)) = (&mut self.picker, &self.keyboard) {
                    if matches!(kb.editing, Editing::PickerSearch) {
                        p.query = kb.text.clone();
                    }
                }
            }
        }
    }

    fn pin_key(&mut self, k: &str) {
        match k {
            "cancel" => {
                self.pin = Pin::default();
                self.retry = None;
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
                    self.run(move |d| {
                        let result = d.call(Request::ScreenUnlock { pin });
                        Box::new(move |m: &mut Manage| {
                            m.pin.busy = false;
                            match result {
                                Ok(Response::Screen(s)) => {
                                    m.screen = s;
                                    m.pin = Pin::default();
                                    if let Some(retry) = m.retry.take() {
                                        retry(m);
                                    }
                                }
                                Err(ApiError::NotSetUp) => {
                                    m.pin = Pin::default();
                                    m.screen.pin_set = false;
                                    m.ask_pin("");
                                }
                                _ => m.pin.wrong = true,
                            }
                        })
                    });
                }
            }
            _ => {}
        }
    }

    fn keyboard_done(&mut self) {
        let Some(kb) = self.keyboard.take() else { return };
        let value = kb.text.clone();
        match kb.editing {
            Editing::Field(key) => {
                let before = self.field_value(&key);
                if let Some(FieldValue::Reference { .. }) = &before {
                    self.edit.before.insert(key.clone(), before.unwrap());
                }
                let kind = self.field(&key).map(|f| f.kind.clone());
                let new = match kind {
                    Some(FieldKind::Secret) => FieldValue::NewSecret(Secret::new(value)),
                    _ => FieldValue::Text(value),
                };
                self.edit.changes.insert(key, new);
            }
            Editing::Search => self.query = value,
            Editing::PickerSearch => {
                if let Some(p) = &mut self.picker {
                    p.query = value;
                }
            }
            Editing::SourceAddress => {
                self.source_address = value.clone();
                self.probe = None;
                self.run(move |d| {
                    let probe = d.probe_feed(&value);
                    Box::new(move |m: &mut Manage| match probe {
                        Ok(p) => m.probe = Some(p),
                        Err(e) => m.say(&e, true),
                    })
                });
            }
            Editing::TrustName(feed) => {
                if let Some(change) = self.key_change.clone() {
                    let typed = value;
                    let _ = feed;
                    self.change("Trusting the new key", move |d| {
                        d.trust_new_key(&change, &typed).map_err(ApiError::Failed)?;
                        Ok("The new key is trusted".into())
                    });
                    self.back();
                }
            }
            Editing::DevicePassphrase => {
                let passphrase = nodo_api::Secret::new(value);
                self.change("Unlocking", move |d| {
                    d.call(Request::Unlock { passphrase: passphrase.clone() })?;
                    Ok("Unlocked".into())
                });
            }
            Editing::InstallPassphrase(id) => {
                let gate = match self.top() {
                    Screen::Gate(InstallGate::Review(r)) => r.approval.clone(),
                    _ => None,
                };
                self.start_install(id, gate, Some(Secret::new(value)));
            }
            Editing::PublicHost(site) => {
                if let Some((s, _, host)) = &mut self.hosting {
                    if *s == site {
                        *host = value;
                    }
                }
            }
            Editing::SshKey => {
                self.change("Adding the key", move |d| {
                    d.add_ssh_key(&value).map_err(ApiError::Failed)?;
                    Ok("Key added".into())
                });
            }
            Editing::BackupPassphrase => {
                let pass = Secret::new(value);
                self.change("Backing up", move |d| {
                    let made = d.create_backup(None, &pass).map_err(ApiError::Failed)?;
                    Ok(format!("Backup saved: {}", made.info.name))
                });
            }
            Editing::WifiPassphrase(ssid) => {
                let security = self
                    .networks
                    .iter()
                    .find(|n| n.ssid == ssid)
                    .map_or(nodo_api::WifiSecurity::Wpa2Personal, |n| n.security);
                let pass = nodo_api::Secret::new(value);
                self.change("Joining", move |d| {
                    d.call(Request::JoinWifi(nodo_api::WifiJoin {
                        ssid: ssid.clone(),
                        security,
                        passphrase: Some(pass.clone()),
                        accept_open: false,
                    }))?;
                    Ok(format!("Joining {ssid}"))
                });
            }
            Editing::ProviderField(i) => {
                if self.provider_values.len() <= i {
                    self.provider_values.resize(i + 1, String::new());
                }
                self.provider_values[i] = value;
            }
        }
    }

    fn field(&self, key: &str) -> Option<&Field> {
        self.edit.form.as_ref()?.fields.iter().find(|f| f.key == key)
    }

    fn field_value(&self, key: &str) -> Option<FieldValue> {
        self.edit.changes.get(key).cloned().or_else(|| self.field(key).map(|f| f.value.clone()))
    }

    fn open_references(&mut self, key: &str) {
        let Screen::Settings(package) = self.top().clone() else { return };
        let label = self.field(key).map(|f| f.label.clone()).unwrap_or_default();
        let key = key.to_owned();
        let k2 = key.clone();
        self.run(move |d| {
            let list = d.references(&package, &k2);
            Box::new(move |m: &mut Manage| {
                let mut rows = Vec::new();
                for o in &list.options {
                    m.edit.consequences.insert(o.path.clone(), o.consequences.clone());
                    rows.push((o.path.clone(), o.value.clone(), o.consequences.join(" · ")));
                }
                let mut sub = format!("into {label} ({})", list.expects);
                if list.hidden_wrong_type > 0 {
                    sub.push_str(&format!(" · {} of another type not shown", list.hidden_wrong_type));
                }
                if list.hidden_secrets > 0 {
                    sub.push_str(&format!(" · {} secrets need your approval in the TUI", list.hidden_secrets));
                }
                m.picker = Some(Picker { title: "Insert a reference".into(), sub, query: String::new(), field: Some(key), rows });
            })
        });
    }

    fn start_install(&mut self, id: PackageId, approval: Option<nodo_api::PermissionApproval>, passphrase: Option<Secret>) {
        self.busy = Some("Installing".into());
        self.run(move |d| {
            let result = d.start_install(&id, approval.as_ref(), passphrase.as_ref());
            Box::new(move |m: &mut Manage| {
                m.busy = None;
                match result {
                    Ok(snap) => {
                        m.install = Some(snap);
                        m.push(Screen::Installing);
                    }
                    Err(e) if e.contains("PIN") || e.contains("locked") => {
                        m.ask_pin(&e);
                    }
                    Err(e) => m.say(&e, true),
                }
            })
        });
    }

    pub fn act(&mut self, id: &str) {
        self.touched();
        let (verb, arg) = id.split_once(':').unwrap_or((id, ""));
        let arg = arg.to_owned();
        match verb {
            "screen" => self.close(),
            "lock" => self.change("Locking", |d| {
                d.call(Request::ScreenLock)?;
                Ok(String::new())
            }),
            "unlock" => self.ask_pin(""),
            "power" => {}
            "dialog-close" => {
                self.dialog = None;
                self.dialog_actions.clear();
            }
            "dialog-yes" => {
                self.dialog = None;
                if let Some(then) = self.dialog_actions.remove("dialog-yes") {
                    then(self);
                }
                self.dialog_actions.clear();
            }
            "picker-close" => {
                self.picker = None;
                if matches!(self.keyboard.as_ref().map(|k| &k.editing), Some(Editing::PickerSearch)) {
                    self.keyboard = None;
                }
            }
            "picker-search" => {
                let q = self.picker.as_ref().map(|p| p.query.clone()).unwrap_or_default();
                self.keyboard(Editing::PickerSearch, "Search", &q, false, false);
            }
            "back" => self.back(),
            "open" => self.open_target(&arg),
            "ref" => {
                if let Some(p) = self.picker.take() {
                    if let Some(key) = p.field {
                        let value = p.rows.iter().find(|r| r.0 == arg).map(|r| r.1.clone()).unwrap_or_default();
                        if let Some(old) = self.field_value(&key) {
                            self.edit.before.entry(key.clone()).or_insert(old);
                        }
                        self.edit.changes.insert(key, FieldValue::Reference { path: arg, resolved: value });
                    }
                }
                self.keyboard = None;
            }
            "field" => self.field_menu(&arg),
            "field-type" => {
                let current = match self.field_value(&arg) {
                    Some(FieldValue::Text(t)) => t,
                    Some(FieldValue::Reference { resolved, .. }) => resolved,
                    _ => String::new(),
                };
                let (label, secret, mono) = self
                    .field(&arg)
                    .map(|f| (f.label.clone(), f.kind == FieldKind::Secret, matches!(f.kind, FieldKind::MoneroAddress | FieldKind::Url)))
                    .unwrap_or_default();
                self.dialog = None;
                self.keyboard(Editing::Field(arg), &label, if secret { "" } else { &current }, secret, mono);
            }
            "field-ref" => {
                self.dialog = None;
                self.open_references(&arg);
            }
            "field-remove" => {
                self.dialog = None;
                if let Some(old) = self.field_value(&arg) {
                    self.edit.before.insert(arg.clone(), old);
                }
                self.edit.changes.insert(arg, FieldValue::Text(String::new()));
            }
            "field-undo" => {
                self.dialog = None;
                if let Some(old) = self.edit.before.remove(&arg) {
                    if self.field(&arg).is_some_and(|f| f.value == old) {
                        self.edit.changes.remove(&arg);
                    } else {
                        self.edit.changes.insert(arg, old);
                    }
                }
            }
            "settings-cancel" => {
                self.edit.changes.clear();
                self.edit.before.clear();
            }
            "settings-save" => self.save_settings(),
            "restart" | "stop" | "start" => {
                let name = arg.clone();
                let verb = verb.to_owned();
                self.change(&format!("{verb} {name}"), move |d| {
                    let id = name.clone();
                    d.call(match verb.as_str() {
                        "restart" => Request::RestartService { id },
                        "stop" => Request::StopService { id },
                        _ => Request::StartService { id },
                    })?;
                    Ok(String::new())
                });
            }
            "remove" => {
                let name = arg.clone();
                self.confirm(
                    &format!("Remove {name}?"),
                    "It stops and its files are removed. Its data stays on the drive, so installing it \
                     again brings everything back.",
                    true,
                    "Remove",
                    Box::new(move |m: &mut Manage| {
                        let n = name.clone();
                        m.change("Removing", move |d| {
                            d.call(Request::RemovePackage { package: n.clone() })?;
                            Ok(format!("{n} removed"))
                        });
                        m.go_tab(1);
                    }),
                );
            }
            "search" => {
                let q = self.query.clone();
                self.keyboard(Editing::Search, "Search apps", &q, false, false);
            }
            "install" => {
                let Screen::About(id) = self.top().clone() else { return };
                self.run(move |d| {
                    let gate = d.install_gate(&id);
                    Box::new(move |m: &mut Manage| m.push(Screen::Gate(gate)))
                });
            }
            "install-confirm" => {
                let Screen::Gate(InstallGate::Review(review)) = self.top().clone() else { return };
                if review.needs_passphrase {
                    self.keyboard(Editing::InstallPassphrase(review.id.clone()), "Device passphrase", "", true, false);
                } else {
                    self.start_install(review.id.clone(), review.approval.clone(), None);
                }
            }
            "install-cancel" => {
                self.run(|d| {
                    let s = d.cancel_install();
                    Box::new(move |m: &mut Manage| {
                        if let Some(s) = s {
                            m.install = Some(s);
                        }
                    })
                });
            }
            "add-source" => self.keyboard(Editing::SourceAddress, "Source address", "https://", false, true),
            "trust-source" => {
                if let Some(probe) = self.probe.clone() {
                    self.change("Adding the source", move |d| {
                        d.trust_feed(&probe).map_err(ApiError::Failed)?;
                        Ok(format!("{} added", probe.name))
                    });
                    self.probe = None;
                    self.back();
                }
            }
            "remove-source" => {
                let name = arg.clone();
                self.confirm(
                    &format!("Remove {name}?"),
                    "Apps installed from it keep running but get no updates from it.",
                    true,
                    "Remove",
                    Box::new(move |m: &mut Manage| {
                        let n = name.clone();
                        m.change("Removing the source", move |d| {
                            d.remove_feed(&n);
                            Ok(String::new())
                        });
                    }),
                );
            }
            "keep-key" => self.back(),
            "trust-key" => self.keyboard(Editing::TrustName(arg.clone()), &format!("Type {arg} to trust its new key"), "", false, false),
            "refresh-sources" => self.change("Checking sources", |d| {
                d.refresh_feeds();
                Ok(String::new())
            }),
            "public-host" => {
                let current = self.hosting.as_ref().map(|h| h.2.clone()).unwrap_or_default();
                self.keyboard(Editing::PublicHost(arg), "Your web address", &current, false, true);
            }
            "hosting-save" => self.save_hosting(),
            "check-updates" => self.change("Checking for updates", |d| {
                d.check_updates();
                Ok(String::new())
            }),
            "apply-updates" => {
                let names = self.selected_updates.clone();
                if names.is_empty() {
                    return;
                }
                self.change("Updating", move |d| {
                    let report = d.apply_updates(&names, &[]);
                    if let Some(f) = report.failure {
                        return Err(ApiError::Failed(format!("{}: {}", f.name, f.headline)));
                    }
                    Ok(format!("{} updated", report.results.iter().filter(|r| r.applied).count()))
                });
            }
            "auto-save" => {
                if let Some(prefs) = self.auto.clone() {
                    self.change("Saving", move |d| {
                        d.save_auto_update(&prefs).map_err(ApiError::Failed)?;
                        Ok("Saved".into())
                    });
                }
            }
            "undo" => {
                let Some(r) = self.rollbacks.iter().find(|r| r.package == arg).cloned() else { return };
                self.confirm(
                    &format!("Undo the update to {}?", r.package),
                    &format!("{} goes back from {} to {}, with the settings it had then.", r.package, r.current, r.previous),
                    false,
                    "Undo",
                    Box::new(move |m: &mut Manage| {
                        let r = r.clone();
                        m.change("Undoing", move |d| {
                            d.call(Request::RollbackPackage { package: r.package.clone(), to: r.previous.clone() })?;
                            Ok(format!("{} is back on {}", r.package, r.previous))
                        });
                    }),
                );
            }
            "backup" => self.keyboard(Editing::BackupPassphrase, "A passphrase for this backup", "", true, false),
            "ssh-add" => self.keyboard(Editing::SshKey, "Paste a public key", "ssh-ed25519 ", false, true),
            "ssh-remove" => {
                let fp = arg.clone();
                self.confirm(
                    "Remove this SSH key?",
                    "Whoever holds it can no longer log in.",
                    true,
                    "Remove",
                    Box::new(move |m: &mut Manage| {
                        let fp = fp.clone();
                        m.change("Removing the key", move |d| {
                            d.remove_ssh_key(&fp).map_err(ApiError::Failed)?;
                            Ok(String::new())
                        });
                    }),
                );
            }
            "wifi-join" => {
                let ssid = arg.clone();
                self.keyboard(Editing::WifiPassphrase(ssid.clone()), &format!("Password for {ssid}"), "", true, false);
            }
            "device-unlock" => self.keyboard(Editing::DevicePassphrase, "Device passphrase", "", true, false),
            "drive-retry" => self.change("Checking the drive", |d| {
                d.drive_action(DriveAction::Retry).map_err(ApiError::Failed)?;
                Ok(String::new())
            }),
            "provider-kind" => {
                let p = self.snapshot.as_ref().and_then(|s| s.providers.iter().find(|p| p.name == arg).cloned());
                self.provider_values = Vec::new();
                self.provider_kind = p;
                self.picker = None;
            }
            "provider-field" => {
                let i: usize = arg.parse().unwrap_or(0);
                let label = self.provider_kind.as_ref().and_then(|p| p.fields.get(i).cloned()).unwrap_or_default();
                let cur = self.provider_values.get(i).cloned().unwrap_or_default();
                self.keyboard(Editing::ProviderField(i), &label, &cur, true, true);
            }
            "provider-connect" => {
                let Some(p) = self.provider_kind.clone() else { return };
                let values = self.provider_values.clone();
                self.change("Connecting", move |d| {
                    let a = Secret::new(values.first().cloned().unwrap_or_default());
                    let b = Secret::new(values.get(1).cloned().unwrap_or_default());
                    let c = d.add_provider(&p.name, None, &a, &b).map_err(ApiError::Failed)?;
                    Ok(format!("{} connected: {} domains", c.name, c.domains.len()))
                });
                self.provider_kind = None;
            }
            "provider-add" => {
                let rows = self
                    .snapshot
                    .as_ref()
                    .map(|s| s.providers.iter().filter(|p| p.is_supported()).map(|p| (p.name.clone(), String::new(), String::new())).collect())
                    .unwrap_or_default();
                self.picker = Some(Picker {
                    title: "Add a provider".into(),
                    sub: "Where your web address is registered".into(),
                    query: String::new(),
                    field: None,
                    rows,
                });
            }
            "provider-remove" => {
                let pid = arg.clone();
                self.confirm(
                    "Remove this provider?",
                    "Sites that use its domains must move first.",
                    true,
                    "Remove",
                    Box::new(move |m: &mut Manage| {
                        let pid = pid.clone();
                        m.change("Removing", move |d| {
                            d.remove_provider(&pid).map_err(ApiError::Failed)?;
                            Ok(String::new())
                        });
                    }),
                );
            }
            _ => {}
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
            "key" => Screen::KeyChanged(name),
            "hosting" => {
                let site = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.web.sites.iter().find(|x| x.package == name))
                    .map(|s| (s.package.clone(), s.exposure.clone(), s.public_host.clone().unwrap_or_default()))
                    .unwrap_or((name.clone(), nodo_api::Exposure { local: false, onion: false, public: false }, String::new()));
                self.hosting = Some(site);
                Screen::Hosting(name)
            }
            "providers" => Screen::Providers,
            "updates" => Screen::Updates,
            "auto" => Screen::AutoUpdate,
            "undo" => Screen::Undo,
            "logs" => Screen::Logs(if name.is_empty() { None } else { Some(name) }),
            "backups" => Screen::Backups,
            "network" => Screen::Network,
            "ssh" => Screen::SshKeys,
            "screen" => Screen::ScreenSettings,
            _ => return,
        };
        self.push(screen);
    }

    fn field_menu(&mut self, key: &str) {
        let Some(f) = self.field(key).cloned() else { return };
        match &f.kind {
            FieldKind::Toggle | FieldKind::Choice(_) => return,
            _ => {}
        }
        let current = self.field_value(key);
        let mut list = Vec::new();
        if self.edit.before.contains_key(key) {
            let back = match self.edit.before.get(key) {
                Some(FieldValue::Reference { path, .. }) => format!("↺ Use {path} again"),
                Some(FieldValue::Text(t)) => format!("↺ Back to {t}"),
                _ => "↺ Undo".into(),
            };
            list.push(button(format!("field-undo:{key}"), back, 3));
        }
        let (title, text) = match &current {
            Some(FieldValue::Reference { path, resolved }) => {
                list.push(button(format!("field-remove:{key}"), "Remove", 2));
                list.push(button(format!("field-type:{key}"), "✎ Type a value instead", 1));
                list.push(button(format!("field-ref:{key}"), "Replace…", 0));
                (path.clone(), if resolved.is_empty() { "This reference has no value now.".to_owned() } else { format!("Now: {resolved}") })
            }
            _ => {
                if f.kind.takes_reference() {
                    list.push(button(format!("field-ref:{key}"), "{ } Insert a reference", 1));
                }
                list.push(button(format!("field-type:{key}"), "✎ Type a value", 0));
                (f.label.clone(), if f.help.is_empty() { f.requirement.clone() } else { f.help.clone() })
            }
        };
        list.insert(0, button("dialog-close", "Close", 3));
        self.dialog(&title, &text, false, list);
    }

    fn save_settings(&mut self) {
        let Screen::Settings(package) = self.top().clone() else { return };
        if self.edit.changes.is_empty() {
            return;
        }
        let broken = self
            .edit
            .changes
            .values()
            .chain(self.edit.form.iter().flat_map(|f| f.fields.iter().map(|x| &x.value)))
            .any(|v| matches!(v, FieldValue::Reference { resolved, .. } if resolved.is_empty()));
        if broken {
            self.say("Fix the broken reference to save", true);
            return;
        }
        let mut lines: Vec<String> = Vec::new();
        for v in self.edit.changes.values() {
            if let FieldValue::Reference { path, .. } = v {
                lines.extend(self.edit.consequences.get(path).cloned().unwrap_or_default());
            }
        }
        let changes: Vec<(String, FieldValue)> = self.edit.changes.clone().into_iter().collect();
        let apply = move |m: &mut Manage| {
            let package = package.clone();
            let changes = changes.clone();
            m.busy = Some("Saving".into());
            m.run(move |d| {
                let list: Vec<nodo_api::SettingChange> = changes
                    .iter()
                    .filter_map(|(name, v)| v.to_input().map(|value| nodo_api::SettingChange { name: name.clone(), value }))
                    .collect();
                let result = d.call(Request::ApplySettings { package: package.clone(), changes: list });
                let explained = match &result {
                    Ok(Response::RolledBack(r)) => Some(d.explain_rollback(&package, &r.reason)),
                    _ => None,
                };
                Box::new(move |m: &mut Manage| {
                    m.busy = None;
                    match (result, explained) {
                        (_, Some(e)) => {
                            let text = format!("{}\n\n{}", e.headline, e.hint);
                            m.dialog("The new settings were undone", &text, true, vec![button("dialog-close", "Edit again", 0)]);
                        }
                        (Ok(_), None) => {
                            m.say("Saved", false);
                            m.edit.changes.clear();
                            m.edit.before.clear();
                            m.load_screen();
                        }
                        (Err(ApiError::Locked(reason)), None) => {
                            m.ask_pin(&reason);
                        }
                        (Err(e), None) => m.say(&e.to_string(), true),
                    }
                })
            });
        };
        if lines.is_empty() {
            apply(self);
        } else {
            lines.dedup();
            let text = format!("Saving also means:\n• {}", lines.join("\n• "));
            self.confirm("Save these settings?", &text, false, "Save", Box::new(apply));
        }
    }

    fn save_hosting(&mut self) {
        let Some((site, exposure, host)) = self.hosting.clone() else { return };
        let going_public = exposure.public
            && !self.snapshot.as_ref().and_then(|s| s.web.sites.iter().find(|x| x.package == site)).is_some_and(|s| s.exposure.public);
        if going_public && host.is_empty() {
            self.say("Choose your web address first", true);
            return;
        }
        let apply = move |m: &mut Manage| {
            let (site, exposure, host) = (site.clone(), exposure.clone(), host.clone());
            m.change("Saving hosting", move |d| {
                let id = d.site_id(&site);
                if !host.is_empty() && exposure.public {
                    d.set_public_host(&site, &host).map_err(ApiError::Failed)?;
                }
                let confirmation = exposure.public.then(|| nodo_api::PublicConfirmation { typed_host: host.clone() });
                d.call(Request::SetExposure { site: id, exposure: exposure.clone(), confirmation })?;
                Ok("Saved".into())
            });
        };
        if going_public {
            let text = format!(
                "Anyone on the internet will be able to open https://{} . Visitors can see your home's internet address.",
                self.hosting.as_ref().map(|h| h.2.clone()).unwrap_or_default()
            );
            self.confirm("Put it on the internet?", &text, false, "Put it online", Box::new(apply));
        } else {
            apply(self);
        }
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
        let fields = self.edit.form.as_ref().map(|f| f.fields.clone()).unwrap_or_default();
        fields
            .iter()
            .find(|f| matches!(f.value, FieldValue::Reference { .. }))
            .or_else(|| fields.iter().find(|f| f.kind.takes_reference()))
            .or(fields.first())
            .map(|f| f.key.clone())
            .unwrap_or_default()
    }

    /// The PIN pad, or the "set a PIN first" answer, as a locked change would show them.
    pub fn show_pin_for_design(&mut self, pin_set: bool) {
        self.screen.pin_set = pin_set;
        self.ask_pin("");
        if pin_set {
            self.pin.digits = "48".into();
        }
    }

    // ---------------------------------------------------------------------------------------
    // What the window shows.

    pub fn badges(&self) -> Vec<String> {
        let updates = self.snapshot.as_ref().map_or(0, |s| s.updates.iter().filter(|u| !u.held).count());
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
        let mut t = self.toast.as_ref().map(|(m, e, _)| (m.clone(), *e)).unwrap_or_default();
        if let Some(busy) = &self.busy {
            t = (format!("{busy}…"), false);
        }
        t
    }

    pub fn dialog_view(&self) -> MDialog {
        match &self.dialog {
            Some((title, text, danger, list)) => MDialog {
                shown: true,
                title: shared(title),
                text: shared(text),
                danger: *danger,
                buttons: model(list.clone()),
            },
            None => MDialog { shown: false, title: "".into(), text: "".into(), danger: false, buttons: model(Vec::new()) },
        }
    }

    pub fn picker_view(&self) -> MPicker {
        let Some(p) = &self.picker else {
            return MPicker { shown: false, title: "".into(), sub: "".into(), query: "".into(), rows: model(Vec::new()) };
        };
        let q = p.query.to_lowercase();
        let rows: Vec<MRow> = p
            .rows
            .iter()
            .filter(|(a, b, c)| q.is_empty() || a.to_lowercase().contains(&q) || b.to_lowercase().contains(&q) || c.to_lowercase().contains(&q))
            .map(|(path, value, note)| {
                let id = if p.field.is_some() { format!("ref:{path}") } else { format!("provider-kind:{path}") };
                let _ = note;
                let sub = value.clone();
                MRow { mono: p.field.is_some(), ..link(id, path, sub, "", 0) }
            })
            .collect();
        MPicker { shown: true, title: shared(&p.title), sub: shared(&p.sub), query: shared(&p.query), rows: model(rows) }
    }

    pub fn keyboard_view(&self) -> MKeyboard {
        match &self.keyboard {
            Some(k) => MKeyboard {
                shown: true,
                label: shared(&k.label),
                text: shared(if k.secret { "•".repeat(k.text.chars().count()) } else { k.text.clone() }),
                secret: k.secret,
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
            Screen::Settings(package) => self.settings(&package),
            Screen::GetApps => self.get_apps(snap),
            Screen::About(id) => self.about(&id),
            Screen::Gate(gate) => self.gate(&gate),
            Screen::Installing => self.installing(),
            Screen::Sources => self.sources(snap),
            Screen::AddSource => self.add_source(),
            Screen::KeyChanged(feed) => self.key_changed(&feed),
            Screen::Web => self.web(snap),
            Screen::Hosting(site) => self.hosting_page(snap, &site),
            Screen::Providers => self.providers(snap),
            Screen::System => self.system(snap),
            Screen::Updates => self.updates(snap),
            Screen::AutoUpdate => self.auto_update(),
            Screen::Undo => self.undo(),
            Screen::Logs(source) => self.logs_page(snap, source.as_deref()),
            Screen::Backups => self.backups_page(),
            Screen::Network => self.network(),
            Screen::SshKeys => self.ssh_page(),
            Screen::ScreenSettings => Page {
                crumbs: crumbs_of(&["System"]),
                title: "Screen".into(),
                actions: Vec::new(),
                cards: vec![card(
                    "",
                    "",
                    vec![
                        segments("screen-timeout", "Go back to the tiles after", &crate::screen_prefs::TIMEOUT_LABELS.map(String::from), self.prefs.timeout_index),
                        segments("screen-dim", "Dim when idle to", &crate::screen_prefs::DIM_LABELS.map(String::from), self.prefs.dim_index),
                        text(
                            if self.screen.pin_set {
                                "A screen PIN is set. Change it over SSH (nodo-tui --ctl screen-pin) or in the TUI."
                            } else {
                                "No screen PIN yet: the screen can only look. Set one over SSH (nodo-tui --ctl screen-pin) or in the TUI."
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
        if let Some(drive) = &snap.overview.drive {
            let mut rows = vec![text(&drive.message, 0)];
            for c in &drive.checks {
                let (mark, tone) = match c.state {
                    CheckState::Passed => ("✓", 2),
                    CheckState::Failed => ("✕", 3),
                    CheckState::Unknown => ("·", 0),
                };
                rows.push(MRow { value: mark.into(), tone, title: shared(&c.text), sub: shared(&c.note), ..row("step") });
            }
            rows.push(buttons(vec![button("drive-retry", "Check again", 0)]));
            cards.push(toned(card("The data drive is missing", &drive.waiting, rows), 3));
        }
        let locked: Vec<_> = snap.overview.unlock_targets.iter().filter(|t| t.encrypted).collect();
        if !locked.is_empty() {
            let mut rows: Vec<MRow> = locked.iter().map(|t| line(&t.name, &t.note)).collect();
            rows.push(buttons(vec![button("device-unlock", "Unlock", 0)]));
            cards.push(toned(card("Unlock after a restart", "Encrypted apps wait for the device passphrase.", rows), 1));
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
        let health: Vec<String> = snap.overview.device.iter().take(3).map(|(k, v)| format!("{k} {v}")).collect();
        rows.push(text(health.join("   ·   "), 0));
        cards.push(card("✓  Everything else is running", "", rows));
        Page { crumbs: Vec::new(), title: "Your Nodo".into(), actions: Vec::new(), cards }
    }

    fn apps(&self, snap: &Snapshot) -> Page {
        let rows = snap
            .services
            .iter()
            .map(|s| {
                let update = snap.updates.iter().any(|u| u.name == s.id && !u.held);
                let (label, tone) = if update { ("Update ready", 1) } else { state_label(s.state) };
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
        let service = snap.services.iter().find(|s| s.id == name);
        let mut rows = Vec::new();
        if let Some(s) = service {
            let (label, tone) = state_label(s.state);
            rows.push(MRow { value: shared(label), tone, ..line(name, &s.detail) });
        }
        if let Some(u) = snap.updates.iter().find(|u| u.name == name && !u.held) {
            rows.push(MRow { value: shared(format!("{} → {}", u.from, u.to)), tone: 1, ..line("An update is ready", &u.source) });
            rows.push(buttons(vec![self.change_button("open:updates", "Update", 0)]));
        }
        let site = snap.web.sites.iter().find(|s| s.package == name);
        let hosting = site.map_or("Not online".to_owned(), |s| exposure_words(&s.exposure));
        let nav = vec![
            link(format!("open:hosting:{name}"), "Hosting", hosting, "", 0),
            link(format!("open:settings:{name}"), "Settings", "", "", 0),
            link(format!("open:logs:{name}"), "Logs", "", "", 0),
        ];
        let running = service.is_some_and(|s| s.state == nodo_api::ServiceState::Running);
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

    fn settings(&self, package: &str) -> Page {
        let mut rows = Vec::new();
        let Some(form) = &self.edit.form else {
            let what = if self.edit.loaded { "This app has no settings." } else { "Reading settings…" };
            return Page { crumbs: self.crumbs(), title: "Settings".into(), actions: Vec::new(), cards: vec![card(what, "", Vec::new())] };
        };
        let mut broken = 0;
        for f in &form.fields {
            let value = self.edit.changes.get(&f.key).unwrap_or(&f.value);
            let changed = self.edit.changes.contains_key(&f.key);
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
                    let mut r = MRow { id: shared(format!("field:{}", f.key)), title: shared(&f.label), changed, mono, placeholder: shared(&f.requirement), ..row(if mono { "long" } else { "field" }) };
                    r.kind = shared("field");
                    if !kind.takes_reference() {
                        r.selected = -1;
                    }
                    match v {
                        FieldValue::Text(t) => r.value = shared(t),
                        FieldValue::Reference { path, resolved } => {
                            let broken_ref = resolved.is_empty();
                            if broken_ref {
                                broken += 1;
                            }
                            let (app, setting) = path.split_once('.').unwrap_or((path, ""));
                            // A long value would make the chip wrap; the popover shows it in full.
                            let shown = if broken_ref || resolved.chars().count() > 28 { String::new() } else { resolved.clone() };
                            r.chips = model(vec![MChip { label: shared(format!("{app} · {setting}")), value: shared(shown), broken: broken_ref }]);
                        }
                        FieldValue::SecretSet => r.value = "••••••••  (set)".into(),
                        FieldValue::SecretUnset => r.value = SharedString::new(),
                        FieldValue::NewSecret(_) => r.value = "••••••••  (new)".into(),
                        FieldValue::Bool(b) => r.value = shared(b.to_string()),
                    }
                    if self.edit.before.contains_key(&f.key) {
                        r.sub = shared(match self.edit.before.get(&f.key) {
                            Some(FieldValue::Reference { path, .. }) => format!("↺ Use {path} again (tap the field)"),
                            _ => "↺ Tap the field to undo".to_owned(),
                        });
                    }
                    rows.push(r);
                }
            }
        }
        let dirty = !self.edit.changes.is_empty();
        let mut save = self.change_button("settings-save", "Save", 0);
        save.enabled = dirty && broken == 0;
        let mut cancel = button("settings-cancel", "Cancel", 1);
        cancel.enabled = dirty;
        if broken > 0 {
            rows.push(text(format!("Fix {broken} broken reference{} to save", if broken == 1 { "" } else { "s" }), 3));
        }
        rows.push(buttons(vec![cancel, save]));
        Page {
            crumbs: self.crumbs(),
            title: format!("{package} settings"),
            actions: Vec::new(),
            cards: vec![card("", "Any text, number or address can hold a reference to another app's value: tap a field.", rows)],
        }
    }

    fn get_apps(&self, snap: &Snapshot) -> Page {
        let q = self.query.to_lowercase();
        let rows: Vec<MRow> = snap
            .catalog
            .iter()
            .filter(|p| q.is_empty() || p.id.name.to_lowercase().contains(&q) || p.tagline.to_lowercase().contains(&q))
            .map(|p| {
                let (status, tone) = match p.status {
                    PackageStatus::Installed => ("Installed", 0),
                    PackageStatus::UpdateReady => ("Update ready", 1),
                    PackageStatus::Available => ("", 0),
                };
                link(format!("open:about:{}/{}", p.id.feed, p.id.name), &p.id.name, &p.tagline, status, tone)
            })
            .collect();
        let count = rows.len();
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
            cards: vec![card("", "", vec![search]), card("", format!("{count} apps from your sources"), rows)],
        }
    }

    fn about(&self, id: &PackageId) -> Page {
        let Some(d) = &self.detail else {
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
            InstallOutcome::InProgress => buttons(vec![button("install-cancel", "Cancel", 1)]),
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
            let mut rows = vec![MRow {
                value: shared(if f.key_state == KeyState::BuiltIn { "built in" } else { "" }),
                ..toggle(format!("feed:{}", f.name), &f.name, format!("{}  ·  {} apps", f.address, f.packages), f.status != FeedStatus::Disabled)
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
                MRow { id: "add-source".into(), title: "Address".into(), value: shared(&self.source_address), placeholder: "https://".into(), mono: true, ..row("field") },
            ],
        )];
        if let Some(p) = &self.probe {
            cards.push(card(
                format!("✓  {}", p.name),
                format!("It offers {} apps{}", p.packages, if p.signed { "" } else { " (unsigned)" }),
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
            .web
            .sites
            .iter()
            .filter(|s| s.exposure.local || s.exposure.onion || s.exposure.public)
            .map(|s| {
                let address = s.public_host.clone().map(|h| format!("https://{h}")).unwrap_or_else(|| if s.exposure.local { s.local_url.clone() } else { s.onion_url.clone() });
                link(format!("open:hosting:{}", s.package), &s.package, address, exposure_words(&s.exposure), 0)
            })
            .collect();
        let mut cards = vec![card("Sites online", "", rows)];
        cards.push(card("", "", vec![text("To put another app online, open it in Apps.", 0)]));
        Page { crumbs: Vec::new(), title: "Web".into(), actions: vec![button("open:providers", "Domain providers", 1)], cards }
    }

    fn hosting_page(&self, snap: &Snapshot, site: &str) -> Page {
        let (exposure, host) = self.hosting.as_ref().map(|h| (h.1.clone(), h.2.clone())).unwrap_or((nodo_api::Exposure { local: false, onion: false, public: false }, String::new()));
        let s = snap.web.sites.iter().find(|x| x.package == site);
        let mut rows = vec![
            toggle("expose:local", "Home network", format!("Phones and computers on your Wi-Fi.  {}", s.map(|s| s.local_url.clone()).unwrap_or_default()), exposure.local),
            toggle("expose:onion", "Tor", "A private link that opens in Tor Browser. It doesn't reveal your home.", exposure.onion),
            toggle("expose:public", "Internet", "Anyone, at a web address you own.", exposure.public),
        ];
        if exposure.public {
            if snap.connected.is_empty() {
                rows.push(text("To use a web address you own, first add the company you bought it from.", 0));
                rows.push(buttons(vec![button("open:providers", "Add a provider", 0)]));
            } else {
                let domains: Vec<String> = snap.connected.iter().flat_map(|c| c.domains.iter().map(|d| d.name.clone())).collect();
                rows.push(MRow {
                    id: format!("public-host:{site}").into(),
                    title: "Web address".into(),
                    value: shared(&host),
                    placeholder: shared(format!("e.g. {}.{}", site, domains.first().cloned().unwrap_or_else(|| "example.com".into()))),
                    mono: true,
                    sub: shared(format!("Your domains: {}", domains.join(", "))),
                    ..row("field")
                });
            }
        }
        rows.push(buttons(vec![self.change_button("hosting-save", "Save", 0)]));
        Page { crumbs: self.crumbs(), title: "Hosting".into(), actions: Vec::new(), cards: vec![card("", format!("Choose who can open {site}. Any combination works."), rows)] }
    }

    fn providers(&self, snap: &Snapshot) -> Page {
        let mut cards = Vec::new();
        for c in &snap.connected {
            let mut rows: Vec<MRow> = c
                .domains
                .iter()
                .map(|d| line(&d.name, if d.used_by.is_empty() { "Not used yet".to_owned() } else { format!("Used by {}", d.used_by.join(", ")) }))
                .collect();
            if let Some(p) = &c.problem {
                rows.push(text(format!("✕  {p}"), 3));
            }
            rows.push(buttons(vec![self.change_button(format!("provider-remove:{}", c.id), "Remove", 2)]));
            cards.push(card(&c.name, "", rows));
        }
        if let Some(p) = &self.provider_kind {
            let mut rows: Vec<MRow> = p
                .fields
                .iter()
                .enumerate()
                .map(|(i, f)| MRow {
                    id: shared(format!("provider-field:{i}")),
                    title: shared(f),
                    value: shared(if self.provider_values.get(i).is_some_and(|v| !v.is_empty()) { "••••••••" } else { "" }),
                    placeholder: "Paste it here".into(),
                    ..row("field")
                })
                .collect();
            rows.insert(0, text(&p.instructions, 0));
            rows.push(buttons(vec![button("provider-kind:", "Cancel", 1), self.change_button("provider-connect", "Connect", 0)]));
            cards.push(card(format!("Connect {}", p.name), "The key is stored as a secret and never shown again.", rows));
        }
        if let Some(e) = &snap.providers_error {
            cards.push(toned(card("Couldn't read the providers", e, Vec::new()), 3));
        }
        Page { crumbs: self.crumbs(), title: "Domain providers".into(), actions: vec![self.change_button("provider-add", "Add a provider", 0)], cards }
    }

    fn system(&self, snap: &Snapshot) -> Page {
        let updates = snap.updates.iter().filter(|u| !u.held).count();
        let mut cards = Vec::new();
        if let Some(base) = snap.updates.iter().find(|u| u.name == SYSTEM_BASE) {
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
                link("open:updates", "Updates", snap.system_note.clone().unwrap_or_default(), if updates > 0 { format!("{updates} ready") } else { String::new() }, 1),
                link("open:auto", "Automatic updates", format!("{} apps", snap.auto_update.enabled_count()), "", 0),
                link("open:undo", "Undo an update", "", "", 0),
                link("open:backups", "Backups", "", "", 0),
                link("open:network", "Network", "", "", 0),
                link("open:logs:", "Device logs", "", "", 0),
                link("open:ssh", "SSH keys", "", "", 0),
                link("open:screen", "Screen", if self.screen.pin_set { "Screen PIN set" } else { "No screen PIN" }, "", 0),
            ],
        ));
        Page { crumbs: Vec::new(), title: "System".into(), actions: Vec::new(), cards }
    }

    fn updates(&self, snap: &Snapshot) -> Page {
        let rows: Vec<MRow> = snap
            .updates
            .iter()
            .map(|u| {
                let note = match &u.note {
                    UpdateNote::Ready => String::new(),
                    UpdateNote::RestartNeeded => "needs a restart".into(),
                    UpdateNote::NewPermission(p) => format!("asks for more: {p} (review in the TUI)"),
                    UpdateNote::Skipped => "skipped".into(),
                    UpdateNote::Blocked(b) => b.clone(),
                    UpdateNote::Withdrawal(w) => w.clone(),
                };
                let selectable = matches!(u.note, UpdateNote::Ready | UpdateNote::RestartNeeded) && !u.held;
                let mut r = toggle(format!("update:{}", u.name), &u.name, format!("{} → {}  ·  {}  {}", u.from, u.to, u.source, note), self.selected_updates.contains(&u.name));
                if !selectable {
                    r = MRow { enabled: false, toggle: 0, value: shared(note), ..line(&u.name, format!("{} → {}", u.from, u.to)) };
                }
                r
            })
            .collect();
        let mut apply = self.change_button("apply-updates", format!("Update {}", self.selected_updates.len()), 0);
        apply.enabled = !self.selected_updates.is_empty();
        let mut list = rows;
        list.push(buttons(vec![apply]));
        Page { crumbs: self.crumbs(), title: "Updates".into(), actions: vec![button("check-updates", "Check now", 1)], cards: vec![card("", snap.overview.updates_checked.clone(), list)] }
    }

    fn auto_update(&self) -> Page {
        let Some(a) = &self.auto else {
            return Page { crumbs: self.crumbs(), title: "Automatic updates".into(), actions: Vec::new(), cards: Vec::new() };
        };
        let selected = a.frequencies.iter().position(|f| *f == a.frequency).unwrap_or(0);
        let mut rows = vec![segments("auto-frequency", "How often", &a.frequencies, selected), line("When", format!("{} to {}  ·  next {}", a.window_start, a.window_end, a.next_run))];
        for e in &a.packages {
            rows.push(toggle(format!("auto:{}", e.name), &e.name, if e.third_party { format!("{} · from a third party", e.note) } else { e.note.clone() }, e.enabled));
        }
        rows.push(text("Updates that need a restart always wait for you.", 0));
        rows.push(buttons(vec![self.change_button("auto-save", "Save", 0)]));
        Page { crumbs: self.crumbs(), title: "Automatic updates".into(), actions: Vec::new(), cards: vec![card("", "", rows)] }
    }

    fn undo(&self) -> Page {
        let rows: Vec<MRow> = if self.rollbacks.is_empty() {
            vec![text("Nothing to undo: no app has an earlier version kept.", 0)]
        } else {
            self.rollbacks
                .iter()
                .map(|r| match &r.blocked {
                    Some(why) => MRow { enabled: false, ..line(&r.package, format!("{} → {}  ·  {why}", r.current, r.previous)) },
                    None => link(format!("undo:{}", r.package), &r.package, format!("Back from {} to {}", r.current, r.previous), "Undo", 1),
                })
                .collect()
        };
        Page { crumbs: self.crumbs(), title: "Undo an update".into(), actions: Vec::new(), cards: vec![card("", "Each app keeps the version from before its last update.", rows)] }
    }

    fn logs_page(&self, snap: &Snapshot, source: Option<&str>) -> Page {
        let Some(source) = source else {
            let rows = snap.log_sources.iter().map(|s| link(format!("open:logs:{s}"), s, "", "", 0)).collect();
            return Page { crumbs: self.crumbs(), title: "Device logs".into(), actions: Vec::new(), cards: vec![card("", "Search and saving to a file are in the TUI.", rows)] };
        };
        let mut rows = vec![toggle("problems", "Problems only", "", self.problems_only)];
        if let Some(view) = &self.logs {
            for l in view.lines.iter().rev().filter(|l| !self.problems_only || l.kind == LogKind::Error).take(80) {
                rows.push(MRow { title: shared(&l.text), tone: if l.kind == LogKind::Error { 3 } else { 0 }, ..row("log") });
            }
            for s in &view.suggestion {
                rows.push(text(s, 1));
            }
        }
        Page { crumbs: self.crumbs(), title: format!("{source} log"), actions: Vec::new(), cards: vec![card("", "Newest first", rows)] }
    }

    fn backups_page(&self) -> Page {
        let mut rows = Vec::new();
        match &self.backups {
            Ok(list) if list.is_empty() => rows.push(text("No backups yet.", 0)),
            Ok(list) => {
                for b in list {
                    rows.push(line(&b.name, format!("{:.1} MB", b.size_bytes as f64 / 1e6)));
                }
            }
            Err(e) => rows.push(text(e, 3)),
        }
        rows.push(text("A backup holds your settings, secrets and the device key, encrypted with its own passphrase. App data on the drive is not included.", 0));
        rows.push(buttons(vec![self.change_button("backup", "Back up now", 0)]));
        Page { crumbs: self.crumbs(), title: "Backups".into(), actions: Vec::new(), cards: vec![card("", "", rows)] }
    }

    fn network(&self) -> Page {
        let mut cards = Vec::new();
        let mut rows = Vec::new();
        if let Some(w) = &self.wifi {
            if let Some(why) = &w.unavailable {
                rows.push(text(why, 0));
            } else {
                rows.push(toggle("wifi:", "Wi-Fi", w.ssid.clone().map(|s| format!("Joined {s}")).unwrap_or_default(), w.enabled));
                if w.enabled {
                    for n in &self.networks {
                        let id = if n.connected { String::new() } else { format!("wifi-join:{}", n.ssid) };
                        rows.push(link(id, &n.ssid, format!("{} dBm{}", n.signal_dbm, if n.saved { " · saved" } else { "" }), if n.connected { "Joined" } else { "" }, 2));
                    }
                }
            }
        } else {
            rows.push(text("Reading Wi-Fi…", 0));
        }
        cards.push(card("Wi-Fi", "", rows));
        cards.push(card("Bluetooth", "", vec![text("Pairing is in the TUI for now.", 0)]));
        Page { crumbs: self.crumbs(), title: "Network".into(), actions: Vec::new(), cards }
    }

    fn ssh_page(&self) -> Page {
        let mut rows = Vec::new();
        match &self.ssh_keys {
            Ok(list) => {
                for k in list {
                    rows.push(MRow { mono: true, ..link(format!("ssh-remove:{}", k.fingerprint), &k.comment, &k.fingerprint, "Remove", 3) });
                }
                if list.is_empty() {
                    rows.push(text("No keys: nobody can log in over SSH.", 0));
                }
            }
            Err(e) => rows.push(text(e, 3)),
        }
        rows.push(buttons(vec![self.change_button("ssh-add", "Add a key", 0)]));
        Page { crumbs: self.crumbs(), title: "SSH keys".into(), actions: Vec::new(), cards: vec![card("", "Keys that may log in as root.", rows)] }
    }
}

/// nodo-tui's `load_snapshot`, for a backend behind `dyn`.
fn load_snapshot(d: &mut dyn Data) -> Result<Snapshot, ApiError> {
    let services = match d.call(Request::ListServices)? {
        Response::Services(services) => services,
        other => return Err(ApiError::Failed(format!("unexpected reply: {other:?}"))),
    };
    let (providers, providers_error) = match d.providers() {
        Ok(list) => (list, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    Ok(Snapshot {
        services,
        overview: d.overview(),
        catalog: d.catalog(),
        feeds: d.feeds(),
        updates: d.updates(),
        web: d.web(),
        providers,
        providers_error,
        connected: d.connected_providers(),
        log_sources: d.log_sources(),
        auto_update: d.auto_update(),
        unavailable: d.unavailable(),
        system_note: d.system_note(),
    })
}

fn exposure_words(e: &nodo_api::Exposure) -> String {
    match (e.local, e.onion, e.public) {
        (false, false, false) => "Not online".into(),
        (true, false, false) => "Home network".into(),
        (false, true, false) => "Private link (Tor)".into(),
        (false, false, true) => "Internet".into(),
        (true, _, true) => "Home and internet".into(),
        (true, true, false) => "Home and Tor".into(),
        (false, true, true) => "Tor and internet".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manage() -> (Manage, Worker) {
        let worker = Worker::start(Box::new(nodo_tui::mock::MockBackend::new()));
        let mut m = Manage::new(Some(&worker));
        m.open();
        worker.drain_blocking(&mut m);
        (m, worker)
    }

    #[test]
    fn opens_on_home_with_the_snapshot() {
        let (m, _w) = manage();
        let page = m.page();
        assert_eq!(page.title, "Your Nodo");
        assert!(!page.cards.is_empty());
    }

    #[test]
    fn every_tab_has_a_page() {
        let (mut m, w) = manage();
        for t in 0..4 {
            m.tab_clicked(t);
            w.drain_blocking(&mut m);
            assert!(!m.page().title.is_empty());
        }
    }

    #[test]
    fn system_rows_open_their_screens() {
        let (mut m, w) = manage();
        m.tab_clicked(3);
        for target in ["updates", "auto", "undo", "backups", "network", "logs:", "ssh", "screen"] {
            m.act(&format!("open:{target}"));
            w.drain_blocking(&mut m);
            assert!(!m.page().title.is_empty(), "{target}");
            m.act("back");
        }
    }

    #[test]
    fn the_pin_pad_collects_six_digits_and_cancel_forgets_them() {
        let (mut m, _w) = manage();
        m.screen.pin_set = true;
        m.ask_pin("");
        for d in ["1", "2", "3"] {
            m.key(&format!("pin:{d}"));
        }
        assert_eq!(m.pin_view().digits, 3);
        m.key("pin:cancel");
        assert!(!m.pin_view().shown);
    }

    #[test]
    fn without_a_pin_a_change_says_how_to_set_one() {
        let (mut m, _w) = manage();
        m.screen.pin_set = false;
        m.ask_pin("");
        assert!(m.dialog_view().text.contains("screen-pin"));
    }

    #[test]
    fn idle_returns_to_the_tiles() {
        let (mut m, w) = manage();
        m.idle_return = Duration::from_millis(1);
        std::thread::sleep(Duration::from_millis(5));
        m.poll(Some(&w));
        assert!(!m.open);
    }
}
