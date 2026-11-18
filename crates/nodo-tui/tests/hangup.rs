//! The TUI must end when its user leaves without pressing a key: its terminal hangs up, as after a
//! dropped ssh connection, or it gets SIGTERM. crossterm's default input source used to spin on a
//! hung-up terminal at full CPU and never return, so the TUI ignored SIGHUP and SIGTERM and ran on
//! with nobody to see it (see nodo_tui::input). These tests run `nodo-tui --mock` on a real
//! pseudo-terminal.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// A new pseudo-terminal: (controller, terminal). Both are close-on-exec from the start: tests run
/// in parallel, and a controller inherited by another test's child would keep this terminal from
/// ever hanging up.
fn pty() -> (OwnedFd, OwnedFd) {
    let flags = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
    // SAFETY: plain calls on a descriptor owned here; ptsname_r gets a valid buffer.
    unsafe {
        let controller = libc::posix_openpt(flags);
        assert!(controller >= 0, "posix_openpt: {}", std::io::Error::last_os_error());
        let controller = OwnedFd::from_raw_fd(controller);
        assert_eq!(libc::grantpt(controller.as_raw_fd()), 0);
        assert_eq!(libc::unlockpt(controller.as_raw_fd()), 0);
        let mut name = [0 as libc::c_char; 128];
        assert_eq!(libc::ptsname_r(controller.as_raw_fd(), name.as_mut_ptr(), name.len()), 0);
        let terminal = libc::open(name.as_ptr(), flags);
        assert!(terminal >= 0, "open terminal: {}", std::io::Error::last_os_error());
        (controller, OwnedFd::from_raw_fd(terminal))
    }
}

/// Reads and drops whatever the program draws, for `period`, so it never blocks on a full buffer.
fn drain(controller: &OwnedFd, period: Duration) {
    let end = Instant::now() + period;
    let mut buffer = [0u8; 65536];
    while Instant::now() < end {
        let mut pollfd = libc::pollfd { fd: controller.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd, and a buffer valid for its length.
        unsafe {
            if libc::poll(&mut pollfd, 1, 50) > 0 && libc::read(controller.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) <= 0 {
                return;
            }
        }
    }
}

fn wait_for_exit(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let end = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Some(status);
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The CPU time a process has used so far, user and system, from /proc.
fn cpu_time(pid: u32) -> Duration {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("read /proc stat");
    // The fields after the command name, which is in parentheses and may hold spaces: utime and
    // stime are the 14th and 15th fields of the whole line.
    let fields: Vec<&str> = stat.rsplit_once(')').expect("stat format").1.split_whitespace().collect();
    let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    // SAFETY: sysconf has no preconditions.
    let per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    Duration::from_millis(ticks * 1000 / per_second)
}

/// How the user leaves.
#[derive(Clone, Copy)]
enum Leave {
    /// The terminal hangs up while it is the TUI's controlling terminal, which also sends SIGHUP.
    HangUpWithSignal,
    /// The terminal hangs up but no signal arrives, as for a TUI started in the background or with
    /// `setsid`.
    HangUpSilently,
    /// runit or `kill` asks the TUI to stop; the terminal stays.
    Terminate,
}

/// Starts `nodo-tui --mock` on a new terminal, lets it draw, leaves as `leave` says and returns how
/// the TUI exited, failing if it spun while idle or was still running two seconds after.
fn leave_the_tui(leave: Leave) -> ExitStatus {
    let (controller, terminal) = pty();
    let mut command = Command::new(env!("CARGO_BIN_EXE_nodo-tui"));
    command
        .arg("--mock")
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(terminal.try_clone().unwrap()))
        .stdout(Stdio::from(terminal.try_clone().unwrap()))
        .stderr(Stdio::null());
    if matches!(leave, Leave::HangUpWithSignal) {
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    } else {
        // Its own process group, away from the terminal running the tests.
        command.process_group(0);
    }
    let mut child = command.spawn().expect("spawn nodo-tui");
    drop(command);
    drop(terminal);

    // Waiting for input must sleep, not spin, also once input has come and gone: after a key, over a
    // second of idling, it may use a little CPU for its ticks and nothing like a core.
    drain(&controller, Duration::from_millis(1000));
    let down = b"\x1b[B";
    // SAFETY: the buffer is valid for its length.
    assert_eq!(unsafe { libc::write(controller.as_raw_fd(), down.as_ptr().cast(), down.len()) }, down.len() as isize);
    drain(&controller, Duration::from_millis(200));
    let before = cpu_time(child.id());
    drain(&controller, Duration::from_millis(1000));
    let idle = cpu_time(child.id()) - before;
    assert!(idle < Duration::from_millis(250), "the idle TUI used {idle:?} of CPU in one second");
    assert!(child.try_wait().unwrap().is_none(), "the TUI is still running before the user leaves");
    // Far longer than it takes, so a loaded machine cannot fail it, and far shorter than forever.
    let limit = Duration::from_secs(2);
    let deadline = Instant::now() + limit;
    match leave {
        Leave::HangUpWithSignal | Leave::HangUpSilently => drop(controller),
        Leave::Terminate => {
            // SAFETY: plain kill(2) on our own child.
            assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) }, 0);
            // Keeps reading, so the restore sequence it writes on the way out cannot block it. This
            // ends when the program closes the terminal, which is a moment before it can be reaped.
            drain(&controller, limit);
        }
    }
    wait_for_exit(&mut child, deadline.saturating_duration_since(Instant::now())).unwrap_or_else(|| {
        // Ignored: only tidying up before the panic below, which is the failure being reported.
        #[allow(clippy::let_underscore_must_use)]
        let _ = child.kill();
        #[allow(clippy::let_underscore_must_use)]
        let _ = child.wait();
        panic!("the TUI was still running {limit:?} after the user left");
    })
}

#[test]
fn the_tui_ends_when_its_controlling_terminal_hangs_up() {
    leave_the_tui(Leave::HangUpWithSignal);
}

#[test]
fn the_tui_ends_when_its_terminal_hangs_up_without_a_signal() {
    leave_the_tui(Leave::HangUpSilently);
}

#[test]
fn the_tui_ends_cleanly_on_sigterm() {
    let status = leave_the_tui(Leave::Terminate);
    assert!(status.success(), "it unwound and restored the terminal instead of dying: {status:?}");
}
