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

/// Sleeps in short steps so that SIGTERM is noticed quickly. False once asked to stop.
fn wait(stop: &AtomicBool, total: Duration) -> bool {
    let step = Duration::from_millis(50);
    let mut left = total;
    while !left.is_zero() {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let d = left.min(step);
        std::thread::sleep(d);
        left -= d;
    }
    !stop.load(Ordering::Relaxed)
}

fn run(o: &Options, stop: &AtomicBool) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut tty = OpenOptions::new().write(true).open(&o.tty)?;
    let (cols, rows) = size(tty.as_raw_fd());
    let layout = Layout::new(cols, rows);
    let logo = logo();
    let rest = cells(&logo);
    let status = if o.stopping { "shutting down…" } else { "starting up…" };
    tty.write_all(full_draw(&layout, &rest, status).as_bytes())?;
    tty.flush()?;
    if o.still {
        return Ok(());
    }
    loop {
        if !wait(stop, REST) {
            return Ok(());
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
            if !wait(stop, FRAME) {
                // Finish on the logo at rest, not halfway through a glint.
                tty.write_all(diff_draw(&layout, &shown, &rest).as_bytes())?;
                return tty.flush();
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
