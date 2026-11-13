//! nodo-screen: package panels on the device's built-in screen (docs/SCREEN.md).
//!
//! Runs on tty1 as `_nodo_screen`. Draws with ratatui, reads keys and the mouse from the terminal
//! and touches from the touchscreen's evdev device, and refreshes panels on a worker thread so that
//! a slow service never stalls the screen. Another copy can run over SSH (`--no-touch`, with the
//! mouse); each picks up the layout the other saves.

mod app;
mod bigtext;
mod config;
mod draw;
mod touch;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use nodo_panels::{Layout, Offered, PanelKey, PanelOutput, Roots};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::app::{App, Effect, Mode};
use crate::config::Config;
use crate::draw::Theme;

const USAGE: &str = "usage: nodo-screen [--layout PATH] [--config PATH] [--no-touch] [--once]";

struct Options {
    layout: PathBuf,
    config: PathBuf,
    touch: bool,
    /// Refresh every panel once, print the results and exit: for checking packages' panels.
    once: bool,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        layout: "/var/lib/nodo-screen/layout.json".into(),
        config: "/etc/nodo/screen.toml".into(),
        touch: true,
        once: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--layout" => options.layout = args.next().ok_or(USAGE)?.into(),
            "--config" => options.config = args.next().ok_or(USAGE)?.into(),
            "--no-touch" => options.touch = false,
            "--once" => options.once = true,
            _ => return Err(USAGE.into()),
        }
    }
    Ok(options)
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
        let result = nodo_panels::refresh(&job.panel, &job.history, std::path::Path::new("/"));
        if done.send(Done { key: job.panel.key, result }).is_err() {
            return;
        }
    }
}

enum Input {
    Touch(touch::Touch),
    Power(nodo_power::Event),
    /// The connection to nodo-power ended, with the error if it failed.
    PowerClosed(Option<String>),
}

/// Asks nodo-power to shut the device down, passing its progress to the main loop.
fn power_off(input: Sender<Input>) {
    thread::spawn(move || {
        let result = nodo_power::request_poweroff(std::path::Path::new(nodo_power::SOCKET), |e| {
            let _ = input.send(Input::Power(e));
        });
        let error = result.err().map(|e| format!("cannot reach the shutdown service ({e})"));
        if let Some(error) = &error {
            log(error);
        }
        let _ = input.send(Input::PowerClosed(error));
    });
}

fn modified(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Puts the terminal back as it was: for a normal exit and for a panic.
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen, crossterm::cursor::Show);
}

fn log(line: &str) {
    // runit's log service collects stderr; the terminal is the screen, so nothing is printed there.
    eprintln!("nodo-screen: {line}");
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname").map(|h| h.trim().to_string()).unwrap_or_else(|_| "nodo".into())
}

/// "thu 13 nov 14:32" in the device's local time.
fn clock() -> String {
    // SAFETY: time and localtime_r write only into the provided values; strftime writes at most
    // `buf.len()` bytes and returns how many it wrote.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return String::new();
        }
        let mut buf = [0u8; 64];
        let n = libc::strftime(buf.as_mut_ptr().cast(), buf.len(), c"%a %d %b  %H:%M".as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).to_lowercase()
    }
}

fn once(offered: &[Offered]) -> ExitCode {
    let mut failed = false;
    for panel in offered {
        let name = format!("{}/{}", panel.key.package, panel.key.panel);
        match nodo_panels::refresh(panel, &[], std::path::Path::new("/")) {
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

fn main() -> ExitCode {
    let options = match options() {
        Ok(options) => options,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
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
    let (layout, dropped) = Layout::load(&options.layout, &nodo_panels::size_rules(&offered));
    for line in &dropped {
        log(&format!("layout: {line}"));
    }
    match run(offered, layout, &options, config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log(&e.to_string());
            ExitCode::FAILURE
        }
    }
}

fn run(offered: Vec<Offered>, layout: Layout, options: &Options, config: Config) -> io::Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }

    // The Linux console needs its palette set; anything else is assumed to have true colour.
    let console = std::env::var("TERM").is_ok_and(|t| t == "linux");
    let theme = if console { Theme::console() } else { Theme::truecolor() };

    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (done_tx, done_rx) = mpsc::channel::<Done>();
    thread::spawn(move || worker(job_rx, done_tx));

    let (input_tx, input_rx) = mpsc::channel::<Input>();
    if options.touch {
        match touch::find_device(std::path::Path::new("/")) {
            Some(device) => {
                let stop = Arc::clone(&stop);
                let input_tx = input_tx.clone();
                thread::spawn(move || {
                    let result = touch::run(&device, |t| input_tx.send(Input::Touch(t)).is_ok() && !stop.load(Ordering::Relaxed));
                    if let Err(e) = result {
                        log(&format!("touchscreen {}: {e}", device.display()));
                    }
                });
            }
            None => log("no touchscreen found; arrange with the keyboard"),
        }
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if console {
        stdout.write_all(draw::palette_sequence().as_bytes())?;
    }
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, crossterm::cursor::Hide)?;
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.clear()?;

    let host = hostname();
    let mut app = App::new(offered, layout, Instant::now());
    let mut in_flight = std::collections::BTreeSet::new();
    let rules = nodo_panels::size_rules(&app.offered);
    // Another copy of the screen (over SSH, say) may save the layout: take it up when it changes.
    let mut layout_seen = modified(&options.layout);
    let mut layout_checked = Instant::now();
    let result = (|| -> io::Result<()> {
        while !stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            // Queue panels that are due and not already being refreshed. Only placed panels run.
            for placement in &app.layout.placements {
                let key = &placement.key;
                let (Some(panel), Some(state)) = (app.offered(key), app.states.get(key)) else { continue };
                if state.next_due <= now && !in_flight.contains(key) {
                    in_flight.insert(key.clone());
                    let _ = job_tx.send(Job { panel: panel.clone(), history: state.history() });
                }
            }
            while let Ok(done) = done_rx.try_recv() {
                in_flight.remove(&done.key);
                let refresh = app.offered(&done.key).map_or(Duration::from_secs(10), |o| o.refresh);
                if let Some(state) = app.states.get_mut(&done.key) {
                    if let Err(reason) = &done.result {
                        log(&format!("{}/{}: {reason}", done.key.package, done.key.panel));
                    }
                    state.record(done.result, Instant::now(), refresh);
                }
            }
            if app.mode == Mode::View && now.duration_since(layout_checked) >= Duration::from_secs(1) {
                layout_checked = now;
                let seen = modified(&options.layout);
                if seen != layout_seen {
                    layout_seen = seen;
                    let (layout, dropped) = Layout::load(&options.layout, &rules);
                    for line in &dropped {
                        log(&format!("layout: {line}"));
                    }
                    app.replace_layout(layout);
                }
            }

            let size = terminal.size()?;
            let area = Rect::new(0, 0, size.width, size.height);
            let mut effects = Vec::new();
            while let Ok(input) = input_rx.try_recv() {
                let to_cell = |x: f64, y: f64| {
                    let (u, v) = config.touch.map(x, y);
                    let column = ((u * f64::from(area.width)) as u16).min(area.width.saturating_sub(1));
                    let row = ((v * f64::from(area.height)) as u16).min(area.height.saturating_sub(1));
                    (column, row)
                };
                match input {
                    Input::Touch(touch::Touch::Down(x, y)) => {
                        let (c, r) = to_cell(x, y);
                        effects.push(app.touch_down(area, c, r, Instant::now()));
                    }
                    Input::Touch(touch::Touch::Move(x, y)) => {
                        let (c, r) = to_cell(x, y);
                        app.touch_move(area, c, r);
                    }
                    Input::Touch(touch::Touch::Up) => app.touch_up(),
                    Input::Power(event) => app.power_event(event),
                    Input::PowerClosed(error) => app.power_closed(error),
                }
            }
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => effects.push(app.key(key.code, key.modifiers, Instant::now())),
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => effects.push(app.touch_down(area, mouse.column, mouse.row, Instant::now())),
                        MouseEventKind::Drag(MouseButton::Left) => app.touch_move(area, mouse.column, mouse.row),
                        MouseEventKind::Up(MouseButton::Left) => app.touch_up(),
                        _ => {}
                    },
                    _ => {}
                }
            }
            app.tick(Instant::now());
            for effect in effects {
                match effect {
                    Effect::None => {}
                    Effect::SaveLayout => {
                        if let Err(e) = app.layout.save(&options.layout) {
                            app.message = Some("could not save the layout".into());
                            log(&format!("saving {}: {e}", options.layout.display()));
                        }
                        layout_seen = modified(&options.layout);
                    }
                    Effect::PowerOff => {
                        log("shutting down, as asked on the screen");
                        power_off(input_tx.clone());
                    }
                }
            }
            let clock = clock();
            terminal.draw(|frame| {
                let area = frame.area();
                draw::draw(&app, area, frame.buffer_mut(), &theme, &host, &clock, Instant::now());
            })?;
        }
        Ok(())
    })();

    if app.mode == Mode::ShuttingDown && app.shutdown.off {
        // The device is turning off: leave "Safe to unplug" on the screen.
        return result;
    }
    restore_terminal();
    result
}
