//! nodo-gui: package panels on the device's built-in screen (docs/SCREEN.md).
//!
//! Draws straight to the panel through KMS with Slint's software renderer and reads the
//! touchscreen through libinput, as `_nodo_screen`. The panels themselves (manifests, Lua, layout)
//! are nodo-panels', shared with nodo-screen, the terminal variant used over SSH; each picks up the
//! layout the other saves.

mod backlight;
mod config;
mod geometry;
mod manage;
mod screen_prefs;
mod screenshot;
mod state;
mod view;

slint::include_modules!();

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use nodo_panels::{Layout, Offered, PanelKey, PanelOutput, Roots, SizeRule};
use slint::platform::Key as SlintKey;
use slint::{SharedString, Timer, TimerMode};

use crate::config::Config;
use crate::state::{App, Effect, Key, Mode};
use crate::view::Presenter;

const USAGE: &str = "usage: nodo-gui [--layout PATH] [--config PATH] [--once] [--screenshot SCENE PATH]\n\
scenes: view, arrange, drag, hidden, add, confirm, progress, failed, off";

struct Options {
    layout: PathBuf,
    config: PathBuf,
    /// Refresh every panel once, print the results and exit: for checking packages' panels.
    once: bool,
    /// Render one screen with sample panels to a PNG file and exit: for the design review.
    screenshot: Option<(String, PathBuf)>,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        layout: "/var/lib/nodo-screen/layout.json".into(),
        config: "/etc/nodo/screen.toml".into(),
        once: false,
        screenshot: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--layout" => options.layout = args.next().ok_or(USAGE)?.into(),
            "--config" => options.config = args.next().ok_or(USAGE)?.into(),
            "--once" => options.once = true,
            "--screenshot" => {
                let scene = args.next().ok_or(USAGE)?;
                let path = args.next().ok_or(USAGE)?;
                options.screenshot = Some((scene, path.into()));
            }
            _ => return Err(USAGE.into()),
        }
    }
    Ok(options)
}

fn log(line: &str) {
    // runit's log service collects stderr.
    eprintln!("nodo-gui: {line}");
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname").map(|h| h.trim().to_string()).unwrap_or_else(|_| "nodo".into())
}

/// The device's first IPv4 address that is not loopback, for the top bar.
fn address() -> String {
    // SAFETY: getifaddrs fills a list that freeifaddrs releases; each entry is only read while the
    // list is alive, and ifa_addr is checked for null and for AF_INET before it is cast.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return String::new();
        }
        let mut found = String::new();
        let mut entry = list;
        while !entry.is_null() {
            let addr = (*entry).ifa_addr;
            if !addr.is_null() && i32::from((*addr).sa_family) == libc::AF_INET {
                let sin = &*(addr as *const libc::sockaddr_in);
                let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if !ip.is_loopback() {
                    found = ip.to_string();
                    break;
                }
            }
            entry = (*entry).ifa_next;
        }
        libc::freeifaddrs(list);
        found
    }
}

/// ("thu 13 nov", "14:32") in the device's local time.
fn clock() -> (String, String) {
    // SAFETY: time and localtime_r write only into the provided values; strftime writes at most
    // `buf.len()` bytes and returns how many it wrote.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return (String::new(), String::new());
        }
        let format = |f: &std::ffi::CStr| {
            let mut buf = [0u8; 64];
            let n = libc::strftime(buf.as_mut_ptr().cast(), buf.len(), f.as_ptr(), &tm);
            String::from_utf8_lossy(&buf[..n]).to_lowercase()
        };
        (format(c"%a %d %b"), format(c"%H:%M"))
    }
}

/// Work for the refresh thread, and what it sends back.
struct Job {
    panel: Offered,
    history: Vec<f64>,
}

struct Done {
    key: PanelKey,
    result: Result<PanelOutput, String>,
}

fn worker(jobs: Receiver<Job>, done: Sender<Done>) {
    for job in jobs {
        let result = nodo_panels::refresh(&job.panel, &job.history, Path::new("/"));
        if done.send(Done { key: job.panel.key, result }).is_err() {
            return;
        }
    }
}

enum Power {
    Event(nodo_power::Event),
    /// The connection to nodo-power ended, with the error if it failed.
    Closed(Option<String>),
}

/// Asks nodo-power to shut the device down, passing its progress back.
fn power_off(tx: Sender<Power>) {
    thread::spawn(move || {
        let result = nodo_power::request_poweroff(Path::new(nodo_power::SOCKET), |e| {
            let _ = tx.send(Power::Event(e));
        });
        let error = result.err().map(|e| format!("cannot reach the shutdown service ({e})"));
        if let Some(error) = &error {
            log(error);
        }
        let _ = tx.send(Power::Closed(error));
    });
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn once(offered: &[Offered]) -> ExitCode {
    let mut failed = false;
    for panel in offered {
        let name = format!("{}/{}", panel.key.package, panel.key.panel);
        match nodo_panels::refresh(panel, &[], Path::new("/")) {
            Ok(output) => println!("{name}: {output:?}"),
            Err(reason) => {
                failed = true;
                println!("{name}: FAILED {reason}");
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Turns a key the window reports into one the screen knows.
fn key_of(text: &str, shift: bool) -> Option<Key> {
    let named = |k: SlintKey| SharedString::from(k) == text;
    Some(if named(SlintKey::Return) {
        Key::Enter
    } else if named(SlintKey::Escape) {
        Key::Escape
    } else if named(SlintKey::Tab) {
        if shift {
            Key::BackTab
        } else {
            Key::Tab
        }
    } else if named(SlintKey::Backtab) {
        Key::BackTab
    } else if named(SlintKey::LeftArrow) {
        Key::Left
    } else if named(SlintKey::RightArrow) {
        Key::Right
    } else if named(SlintKey::UpArrow) {
        Key::Up
    } else if named(SlintKey::DownArrow) {
        Key::Down
    } else {
        let mut chars = text.chars();
        let c = chars.next()?;
        if chars.next().is_some() || c.is_control() {
            return None;
        }
        Key::Char(if shift { c.to_ascii_uppercase() } else { c })
    })
}

/// Everything the window's callbacks and timer share.
struct Runtime {
    app: App,
    presenter: Presenter,
    options: Options,
    rules: std::collections::BTreeMap<PanelKey, SizeRule>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    in_flight: BTreeSet<PanelKey>,
    power_tx: Sender<Power>,
    power_rx: Receiver<Power>,
    backlight: Option<backlight::Device>,
    dimmer: backlight::Dimmer,
    /// A touch that woke the screen does nothing else until the finger lifts.
    waking: bool,
    layout_seen: Option<SystemTime>,
    layout_checked: Instant,
    stop: Arc<AtomicBool>,
    manage: manage::Manage,
    worker: Option<manage::Worker>,
    /// The management version last shown, so its models are only rebuilt on change.
    manage_shown: Option<u64>,
}

impl Runtime {
    fn set_backlight(&self, level: backlight::Level) {
        if let Some(device) = &self.backlight {
            if let Err(e) = device.set(level, self.dimmer.percent) {
                log(&format!("backlight: {e}"));
            }
        }
    }

    fn apply(&mut self, effect: Effect) {
        match effect {
            Effect::None => {}
            Effect::SaveLayout => {
                if let Err(e) = self.app.layout.save(&self.options.layout) {
                    self.app.message = Some("could not save the layout".into());
                    log(&format!("saving {}: {e}", self.options.layout.display()));
                }
                self.layout_seen = modified(&self.options.layout);
            }
            Effect::PowerOff => {
                log("shutting down, as asked on the screen");
                power_off(self.power_tx.clone());
            }
            Effect::OpenManage => self.manage.open(),
        }
    }

    /// Any input: wakes a dim screen, in which case it does nothing else.
    fn woke(&mut self) -> bool {
        match self.dimmer.input(Instant::now()) {
            Some(level) => {
                self.set_backlight(level);
                true
            }
            None => false,
        }
    }

    fn pointer_down(&mut self, x: f32, y: f32) {
        if self.woke() {
            self.waking = true;
            return;
        }
        let effect = self.app.touch_down(x, y, Instant::now());
        self.apply(effect);
    }

    fn pointer_move(&mut self, x: f32, y: f32) {
        if !self.waking {
            self.app.touch_move(x, y);
        }
    }

    fn pointer_up(&mut self) {
        if !std::mem::take(&mut self.waking) {
            self.app.touch_up();
        }
    }

    fn key(&mut self, key: Key) {
        if self.woke() {
            return;
        }
        let effect = self.app.key(key, Instant::now());
        self.apply(effect);
    }

    /// Ten times a second: refresh panels that are due, take their results, follow a shutdown,
    /// pick up a layout saved elsewhere, and dim.
    fn tick(&mut self) {
        let now = Instant::now();
        for placement in &self.app.layout.placements {
            let key = &placement.key;
            let (Some(panel), Some(state)) = (self.app.offered(key), self.app.states.get(key)) else { continue };
            if state.next_due <= now && !self.in_flight.contains(key) {
                self.in_flight.insert(key.clone());
                let _ = self.jobs.send(Job { panel: panel.clone(), history: state.history() });
            }
        }
        while let Ok(done) = self.done.try_recv() {
            self.in_flight.remove(&done.key);
            let refresh = self.app.offered(&done.key).map_or(Duration::from_secs(10), |o| o.refresh);
            if let Some(state) = self.app.states.get_mut(&done.key) {
                if let Err(reason) = &done.result {
                    log(&format!("{}/{}: {reason}", done.key.package, done.key.panel));
                }
                state.record(done.result, Instant::now(), refresh);
            }
        }
        while let Ok(power) = self.power_rx.try_recv() {
            match power {
                Power::Event(event) => self.app.power_event(event),
                Power::Closed(error) => self.app.power_closed(error),
            }
        }
        if self.app.mode == Mode::View && now.duration_since(self.layout_checked) >= Duration::from_secs(1) {
            self.layout_checked = now;
            let seen = modified(&self.options.layout);
            if seen != self.layout_seen {
                self.layout_seen = seen;
                let (layout, dropped) = Layout::load(&self.options.layout, &self.rules);
                for line in &dropped {
                    log(&format!("layout: {line}"));
                }
                self.app.replace_layout(layout);
            }
        }
        self.app.tick(now);
        self.manage.poll(self.worker.as_ref());
        if let Some(level) = self.dimmer.tick(now, self.app.mode == Mode::View && !self.manage.open) {
            self.set_backlight(level);
        }
    }
}

fn main() -> ExitCode {
    let options = match options() {
        Ok(options) => options,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    if let Some((scene, path)) = &options.screenshot {
        return match screenshot::render(scene, path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("nodo-gui: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let (offered, problems) = nodo_panels::discover(&Roots::default());
    for problem in &problems {
        log(problem);
    }
    if options.once {
        return once(&offered);
    }
    let (config, config_problem) = Config::load(&options.config);
    if let Some(problem) = config_problem {
        log(&problem);
    }
    let rules = nodo_panels::size_rules(&offered);
    let (layout, dropped) = Layout::load(&options.layout, &rules);
    for line in &dropped {
        log(&format!("layout: {line}"));
    }
    match run(offered, layout, options, config, rules) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log(&e);
            ExitCode::FAILURE
        }
    }
}

fn run(offered: Vec<Offered>, layout: Layout, options: Options, config: Config, rules: std::collections::BTreeMap<PanelKey, SizeRule>) -> Result<(), String> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&stop)).map_err(|e| e.to_string())?;
    }
    let ui = MainWindow::new().map_err(|e| format!("cannot open the screen: {e}"))?;
    ui.set_host(hostname().into());
    ui.set_address(address().into());

    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (done_tx, done_rx) = mpsc::channel::<Done>();
    thread::spawn(move || worker(job_rx, done_tx));
    let (power_tx, power_rx) = mpsc::channel::<Power>();

    let backlight = backlight::Device::find(Path::new("/sys/class/backlight"));
    let dimmer = backlight::Dimmer::new(&config.backlight, Instant::now());
    let layout_seen = modified(&options.layout);
    let socket = std::env::var("NODO_SCREEN_SOCKET").unwrap_or_else(|_| nodo_api::SCREEN_SOCKET.to_owned());
    let (worker, offline) = match nodo_tui::helper_backend::HelperBackend::connect(Path::new(&socket)) {
        Ok(backend) => (Some(manage::Worker::start(Box::new(backend))), None),
        Err(e) => {
            log(&format!("management: cannot reach {socket}: {e}"));
            (None, Some(format!("{socket}: {e}")))
        }
    };
    let mut manage = manage::Manage::new(worker.as_ref());
    if let Some(why) = offline {
        manage.set_offline(why);
    }
    manage.prefs = screen_prefs::Prefs::load(options.layout.with_file_name("gui.toml"));
    manage.idle_return = manage.prefs.timeout();
    let runtime = Rc::new(RefCell::new(Runtime {
        manage,
        worker,
        manage_shown: None,
        app: App::new(offered, layout, Instant::now()),
        presenter: Presenter::new(&ui),
        options,
        rules,
        jobs: job_tx,
        done: done_rx,
        in_flight: BTreeSet::new(),
        power_tx,
        power_rx,
        backlight,
        dimmer,
        waking: false,
        layout_seen,
        layout_checked: Instant::now(),
        stop,
    }));
    runtime.borrow().set_backlight(backlight::Level::Full);

    let show = {
        let weak = ui.as_weak();
        let runtime = Rc::clone(&runtime);
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut r = runtime.borrow_mut();
            let (day, time) = clock();
            let Runtime { app, presenter, manage, manage_shown, .. } = &mut *r;
            presenter.show(&ui, app, Instant::now(), (&day, &time));
            show_manage(&ui, manage, manage_shown);
        }
    };
    let show = Rc::new(show);

    {
        let runtime = Rc::clone(&runtime);
        let show = Rc::clone(&show);
        ui.on_pointer_down(move |x, y| {
            runtime.borrow_mut().pointer_down(x, y);
            show();
        });
    }
    {
        let runtime = Rc::clone(&runtime);
        let show = Rc::clone(&show);
        ui.on_pointer_move(move |x, y| {
            runtime.borrow_mut().pointer_move(x, y);
            show();
        });
    }
    {
        let runtime = Rc::clone(&runtime);
        let show = Rc::clone(&show);
        ui.on_pointer_up(move || {
            runtime.borrow_mut().pointer_up();
            show();
        });
    }
    {
        let runtime = Rc::clone(&runtime);
        let show = Rc::clone(&show);
        ui.on_key(move |text, shift| {
            let Some(key) = key_of(&text, shift) else { return false };
            runtime.borrow_mut().key(key);
            show();
            true
        });
    }

    // Management's callbacks: Slint reports the action, Manage decides.
    {
        let runtime = Rc::clone(&runtime);
        let show = Rc::clone(&show);
        ui.on_m_act(move |id| {
            {
                let mut r = runtime.borrow_mut();
                if id == "power" {
                    r.manage.close();
                    let effect = r.app.act(state::Button::ShutDown, Instant::now());
                    r.apply(effect);
                } else {
                    r.manage.act(&id);
                }
            }
            show();
        });
    }
    {
        let runtime = Rc::clone(&runtime);
        let show = Rc::clone(&show);
        ui.on_m_toggled(move |id, on| {
            runtime.borrow_mut().manage.toggled(&id, on);
            show();
        });
    }
    {
        let runtime = Rc::clone(&runtime);
        let show = Rc::clone(&show);
        ui.on_m_segment(move |id, n| {
            runtime.borrow_mut().manage.segment(&id, n.max(0) as usize);
            show();
        });
    }
    {
        let runtime = Rc::clone(&runtime);
        let show = Rc::clone(&show);
        ui.on_m_tab_clicked(move |t| {
            runtime.borrow_mut().manage.tab_clicked(t.max(0) as usize);
            show();
        });
    }
    {
        let runtime = Rc::clone(&runtime);
        let show = Rc::clone(&show);
        ui.on_m_crumb(move |i| {
            runtime.borrow_mut().manage.crumb(i.max(0) as usize);
            show();
        });
    }
    {
        let runtime = Rc::clone(&runtime);
        let show = Rc::clone(&show);
        ui.on_m_key(move |k| {
            runtime.borrow_mut().manage.key(&k);
            show();
        });
    }

    let timer = Timer::default();
    {
        let runtime = Rc::clone(&runtime);
        let show = Rc::clone(&show);
        timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
            let quit = {
                let mut r = runtime.borrow_mut();
                r.tick();
                // Once the device is off, the last screen stays until the power goes: runit may
                // ask the program to stop, and the panel would otherwise fall back to the console.
                r.stop.load(Ordering::Relaxed) && !r.app.shutdown.off
            };
            show();
            if quit {
                let _ = slint::quit_event_loop();
            }
        });
    }
    show();
    ui.run().map_err(|e| e.to_string())?;
    runtime.borrow().set_backlight(backlight::Level::Full);
    Ok(())
}

/// Pushes management's page to the window, only when something changed since the last time.
pub(crate) fn show_manage(ui: &MainWindow, manage: &manage::Manage, shown: &mut Option<u64>) {
    if !manage.open {
        if shown.is_some() {
            ui.set_manage_open(false);
            *shown = None;
        }
        return;
    }
    if *shown == Some(manage.version) {
        return;
    }
    let new_screen = shown.is_none();
    *shown = Some(manage.version);
    let page = manage.page();
    let model = |v: Vec<SharedString>| slint::ModelRc::new(slint::VecModel::from(v));
    ui.set_m_tab(manage.tab() as i32);
    ui.set_m_badges(model(manage.badges().into_iter().map(SharedString::from).collect()));
    ui.set_m_unlocked(manage.unlocked());
    ui.set_m_pin_set(manage.pin_set());
    ui.set_m_crumbs(model(page.crumbs.into_iter().map(SharedString::from).collect()));
    ui.set_m_title(page.title.into());
    ui.set_m_actions(slint::ModelRc::new(slint::VecModel::from(page.actions)));
    ui.set_m_cards(slint::ModelRc::new(slint::VecModel::from(page.cards)));
    let (toast, error) = manage.toast();
    ui.set_m_toast(toast.into());
    ui.set_m_toast_error(error);
    ui.set_m_dialog(manage.dialog_view());
    ui.set_m_picker(manage.picker_view());
    ui.set_m_keyboard(manage.keyboard_view());
    ui.set_m_pin(manage.pin_view());
    if new_screen {
        ui.set_m_scroll(0.0);
    }
    ui.set_manage_open(true);
}
