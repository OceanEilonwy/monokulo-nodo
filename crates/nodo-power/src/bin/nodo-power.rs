//! nodo-power: the root side of the screen's Shut down button (docs/SCREEN.md).
//!
//! Listens on /run/nodo-power/socket, which is root:_nodo_screen 0660. A connection from root or
//! from _nodo_screen may send `poweroff`; the service then stops every enabled runit service, syncs
//! and turns the device off, writing each step back. Any other request, or a connection from any
//! other account, is refused.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nodo_manifest::Manifest;
use nodo_power::{enabled_services, long_stops, read_request, shut_down, stop_all, Event, System, OFF_MARKER, SOCKET, SPLASH_SKIP};

const SCREEN_ACCOUNT: &str = "_nodo_screen";
const SERVICE_DIR: &str = "/var/service";

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

fn log(line: &str) {
    eprintln!("nodo-power: {line}");
}

struct Runit;

impl System for Runit {
    fn begin(&self) {
        if let Err(e) = std::fs::write(SPLASH_SKIP, b"") {
            log(&format!("{SPLASH_SKIP}: {e}"));
        }
    }

    fn services(&self) -> io::Result<Vec<String>> {
        enabled_services(Path::new(SERVICE_DIR))
    }

    fn stop(&self, service: &str, timeout: Duration) -> bool {
        let dir = Path::new(SERVICE_DIR).join(service);
        let status = Command::new("sv")
            .arg("-w")
            .arg(timeout.as_secs().to_string())
            .arg("stop")
            .arg(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let stopped = status.is_ok_and(|s| s.success());
        log(&format!("{service}: {}", if stopped { "stopped" } else { "did not stop in time" }));
        stopped
    }

    fn sync(&self) {
        // SAFETY: sync takes no arguments and cannot fail.
        unsafe { libc::sync() };
    }

    fn poweroff(&self) -> io::Result<()> {
        std::fs::write(OFF_MARKER, b"")?;
        log("turning off");
        let status = Command::new("poweroff").stdin(Stdio::null()).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("poweroff exited with {status}")))
        }
    }
}

fn account_ids(name: &str) -> Option<(libc::uid_t, libc::gid_t)> {
    let name = CString::new(name).ok()?;
    // SAFETY: getpwnam returns a pointer to static storage or null; it is read at once, on the one
    // thread that runs before any connection is accepted.
    unsafe {
        let pw = libc::getpwnam(name.as_ptr());
        (!pw.is_null()).then(|| ((*pw).pw_uid, (*pw).pw_gid))
    }
}

fn peer_uid(stream: &UnixStream) -> io::Result<libc::uid_t> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills a ucred of the given length.
    let rc = unsafe { libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len) };
    if rc == 0 {
        Ok(cred.uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn handle(mut stream: UnixStream, screen_uid: libc::uid_t) {
    let send_error = |stream: &mut UnixStream, text: &str| {
        let _ = writeln!(stream, "{}", Event::Error(text.into()).line());
    };
    match peer_uid(&stream) {
        Ok(uid) if uid == 0 || uid == screen_uid => {}
        Ok(uid) => {
            log(&format!("refused a connection from uid {uid}"));
            return send_error(&mut stream, "not allowed");
        }
        Err(e) => return log(&format!("peer credentials: {e}")),
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    if let Err(reason) = read_request(&mut stream) {
        log(&reason);
        return send_error(&mut stream, &reason);
    }
    if SHUTTING_DOWN.swap(true, Ordering::SeqCst) {
        return send_error(&mut stream, "already shutting down");
    }
    log("shutting down, as asked from the screen");
    let mut out = stream.try_clone().ok();
    let result = shut_down(&Runit, &mut |event| {
        if let Some(stream) = out.as_mut() {
            if writeln!(stream, "{}", event.line()).is_err() {
                // The screen went away. The shutdown goes on regardless.
                out = None;
            }
        }
    });
    if let Err(e) = result {
        log(&format!("shutdown failed: {e}"));
        SHUTTING_DOWN.store(false, Ordering::SeqCst);
        // An ordinary shutdown later shows the boot splash again.
        let _ = std::fs::remove_file(SPLASH_SKIP);
        send_error(&mut stream, &e.to_string());
    }
}

/// Each installed package's declared stop time, by id (its service's name).
fn declared_stops() -> BTreeMap<String, u32> {
    let mut stops = BTreeMap::new();
    for (dir, system) in [("/usr/share/nodo/manifests", true), ("/var/lib/nodo/manifests", false)] {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let parsed = if system { Manifest::parse_system(&text) } else { Manifest::parse(&text) };
            match parsed {
                Ok(m) => {
                    stops.insert(m.id.clone(), m.stop_timeout_secs);
                }
                Err(e) => log(&format!("{}: {e}", path.display())),
            }
        }
    }
    stops
}

/// runit stage 3, before its general stop (`/etc/runit/shutdown.d/06-nodo-long-stop.sh`): stops the
/// node and every package that declares a stop longer than runit's 7 s, waiting as long as each
/// declares, so that a poweroff or reboot over SSH, or a critical temperature, never kills one
/// while it saves. Skipped when the screen's Shut down button already stopped everything.
fn before_shutdown() -> ExitCode {
    if Path::new(OFF_MARKER).exists() {
        println!("nodo-power: the services were already stopped from the screen");
        return ExitCode::SUCCESS;
    }
    let enabled = match enabled_services(Path::new(SERVICE_DIR)) {
        Ok(enabled) => enabled,
        Err(e) => {
            println!("nodo-power: cannot list the services: {e}");
            return ExitCode::FAILURE;
        }
    };
    let stops = long_stops(&enabled, &declared_stops());
    for (service, wait) in &stops {
        println!("nodo-power: stopping {service}, waiting up to {} s", wait.as_secs());
    }
    for (service, stopped) in stop_all(&Runit, &stops) {
        println!("nodo-power: {service} {}", if stopped { "stopped" } else { "did not stop in time" });
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [flag] if flag == "--before-shutdown" => return before_shutdown(),
        _ => {
            eprintln!("usage: nodo-power [--before-shutdown] (a runit service; see docs/SCREEN.md)");
            return ExitCode::from(2);
        }
    }
    let Some((screen_uid, screen_gid)) = account_ids(SCREEN_ACCOUNT) else {
        log("there is no _nodo_screen account; nothing may ask for a shutdown");
        return ExitCode::FAILURE;
    };
    let socket = Path::new(SOCKET);
    let _ = std::fs::remove_file(socket);
    let listener = match UnixListener::bind(socket) {
        Ok(listener) => listener,
        Err(e) => {
            log(&format!("cannot listen on {SOCKET}: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let path = CString::new(SOCKET).expect("no NUL");
    // SAFETY: chown on a path we just created.
    if unsafe { libc::chown(path.as_ptr(), 0, screen_gid) } != 0 || std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660)).is_err() {
        log("cannot give the socket to root:_nodo_screen 0660");
        return ExitCode::FAILURE;
    }
    log(&format!("listening on {SOCKET}"));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || handle(stream, screen_uid));
            }
            Err(e) => log(&format!("accept: {e}")),
        }
    }
    ExitCode::SUCCESS
}
