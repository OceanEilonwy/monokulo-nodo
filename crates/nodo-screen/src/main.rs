//! nodo-screen: package panels on the device's built-in screen (docs/SCREEN.md).
//!
//! Runs on tty1 as `_nodo_screen`. Draws with ratatui, reads keys from the terminal and touches
//! from the touchscreen's evdev device, and refreshes panels on a worker thread so that a slow
//! service never stalls the screen.

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

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use nodo_panels::{Layout, Offered, PanelKey, PanelOutput, Roots};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::app::App;
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
    execute!(stdout, EnterAlternateScreen, crossterm::cursor::Hide)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.clear()?;

    let host = hostname();
    let mut app = App::new(offered, layout, Instant::now());
    let mut in_flight = std::collections::BTreeSet::new();
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

            let size = terminal.size()?;
            let area = Rect::new(0, 0, size.width, size.height);
            let mut save = false;
            while let Ok(Input::Touch(t)) = input_rx.try_recv() {
                let to_cell = |x: f64, y: f64| {
                    let (u, v) = config.touch.map(x, y);
                    let column = ((u * f64::from(area.width)) as u16).min(area.width.saturating_sub(1));
                    let row = ((v * f64::from(area.height)) as u16).min(area.height.saturating_sub(1));
                    (column, row)
                };
                match t {
                    touch::Touch::Down(x, y) => {
                        let (c, r) = to_cell(x, y);
                        save |= app.touch_down(area, c, r, Instant::now());
                    }
                    touch::Touch::Move(x, y) => {
                        let (c, r) = to_cell(x, y);
                        app.touch_move(area, c, r);
                    }
                    touch::Touch::Up => app.touch_up(),
                }
            }
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        save |= app.key(key.code, key.modifiers);
                    }
                }
            }
            app.tick(Instant::now());
            if save {
                if let Err(e) = app.layout.save(&options.layout) {
                    app.message = Some("could not save the layout".into());
                    log(&format!("saving {}: {e}", options.layout.display()));
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

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, crossterm::cursor::Show)?;
    result
}
