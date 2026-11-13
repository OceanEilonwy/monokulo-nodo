//! nodo-splash: the Nodo logo on the built-in screen while the device starts or stops
//! (docs/SCREEN.md, "Boot splash").
//!
//! Started in the background by runit's first stage, and by its third with `--stopping`. It draws
//! on tty1 until SIGTERM, which nodo-screen's run script sends before it takes the console, and
//! leaves the picture there when it stops.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use nodo_splash::{cells, diff_draw, frame, full_draw, logo, preview, sweep, Layout};

const USAGE: &str = "usage: nodo-splash [--stopping] [--tty PATH] [--pidfile PATH] [--still] | --preview";

/// The glint takes about a second, then the logo rests.
const FRAME: Duration = Duration::from_millis(83);
const REST: Duration = Duration::from_secs(3);

struct Options {
    stopping: bool,
    tty: PathBuf,
    pidfile: Option<PathBuf>,
    still: bool,
    preview: bool,
}

fn options() -> Result<Options, String> {
    let mut o = Options { stopping: false, tty: "/dev/tty1".into(), pidfile: None, still: false, preview: false };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stopping" => o.stopping = true,
            "--tty" => o.tty = args.next().ok_or(USAGE)?.into(),
            "--pidfile" => o.pidfile = Some(args.next().ok_or(USAGE)?.into()),
            "--still" => o.still = true,
            "--preview" => o.preview = true,
            _ => return Err(USAGE.into()),
        }
    }
    Ok(o)
}

/// The console's size, or the T6N's 120 x 33 when it cannot be read.
fn size(fd: i32) -> (usize, usize) {
    // SAFETY: TIOCGWINSZ writes one winsize into the provided value.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        (ws.ws_col.into(), ws.ws_row.into())
    } else {
        (120, 33)
    }
}

/// The screen's own console font, then plain Terminus 16x32: both have the half blocks and the
/// ellipsis. Before the framebuffer console exists `setfont` fails, which is why this runs again
/// after every resize. Failures are ignored: the kernel's Terminus still has the half blocks.
const FONTS: [&str; 2] = ["/usr/share/nodo/nodo-screen-16x32.psf.gz", "/usr/share/kbd/consolefonts/ter-v32n.psf.gz"];

fn load_font(tty: &std::path::Path) {
    for font in FONTS {
        if !std::path::Path::new(font).exists() {
            continue;
        }
        let loaded = std::process::Command::new("setfont")
            .arg("-C")
            .arg(tty)
            .arg(font)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if loaded {
            return;
        }
    }
}

/// How a wait ended.
#[derive(PartialEq)]
enum Waited {
    Done,
    Stop,
    /// The console changed size: redraw everything for the new layout.
    Resized,
}

/// Sleeps in short steps so that SIGTERM and a console resize are noticed quickly.
///
/// The splash starts in runit's first stage, before udev loads the display driver, so it first
/// draws on the kernel's placeholder console. When the panel's framebuffer console arrives (about
/// 8 s into boot on the T6N) the console grows to 120 x 33 and keeps the old text in its top left
/// corner, which is why the logo has to be measured again and redrawn.
fn wait(stop: &AtomicBool, fd: i32, layout: &Layout, total: Duration) -> Waited {
    let step = Duration::from_millis(50);
    let mut left = total;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Waited::Stop;
        }
        if size(fd) != (layout.cols, layout.rows) {
            return Waited::Resized;
        }
        if left.is_zero() {
            return Waited::Done;
        }
        let d = left.min(step);
        std::thread::sleep(d);
        left -= d;
    }
}

fn run(o: &Options, stop: &AtomicBool) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut tty = OpenOptions::new().write(true).open(&o.tty)?;
    let fd = tty.as_raw_fd();
    let logo = logo();
    let rest = cells(&logo);
    let status = if o.stopping { "shutting down…" } else { "starting up…" };
    'draw: loop {
        load_font(&o.tty);
        let (cols, rows) = size(fd);
        let layout = Layout::new(cols, rows);
        tty.write_all(full_draw(&layout, &rest, status).as_bytes())?;
        tty.flush()?;
        if o.still {
            return Ok(());
        }
        loop {
            match wait(stop, fd, &layout, REST) {
                Waited::Stop => return Ok(()),
                Waited::Resized => continue 'draw,
                Waited::Done => {}
            }
            let mut shown = rest;
            for t in sweep() {
                let next = cells(&frame(&logo, Some(t)));
                let out = diff_draw(&layout, &shown, &next);
                if !out.is_empty() {
                    tty.write_all(out.as_bytes())?;
                    tty.flush()?;
                }
                shown = next;
                match wait(stop, fd, &layout, FRAME) {
                    Waited::Done => {}
                    Waited::Resized => continue 'draw,
                    Waited::Stop => {
                        // Finish on the logo at rest, not halfway through a glint.
                        tty.write_all(diff_draw(&layout, &shown, &rest).as_bytes())?;
                        return tty.flush();
                    }
                }
            }
        }
    }
}

fn main() -> ExitCode {
    let o = match options() {
        Ok(o) => o,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    if o.preview {
        let logo = logo();
        print!("{}\n{}", preview(&logo), preview(&frame(&logo, Some(56))));
        return ExitCode::SUCCESS;
    }
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT, signal_hook::consts::SIGHUP] {
        if let Err(e) = signal_hook::flag::register(signal, Arc::clone(&stop)) {
            eprintln!("nodo-splash: {e}");
            return ExitCode::FAILURE;
        }
    }
    if let Some(path) = &o.pidfile {
        if let Err(e) = std::fs::write(path, format!("{}\n", std::process::id())) {
            eprintln!("nodo-splash: {}: {e}", path.display());
        }
    }
    let result = run(&o, &stop);
    if let Some(path) = &o.pidfile {
        let _ = std::fs::remove_file(path);
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nodo-splash: {}: {e}", o.tty.display());
            ExitCode::FAILURE
        }
    }
}
