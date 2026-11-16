//! The one way the TUI reads its terminal.
//!
//! A TUI has to stop when the user leaves, and the user can leave in ways that never send a key:
//! runit or `kill` sends SIGTERM, Ctrl-C sends SIGINT, and a dropped ssh connection hangs the
//! terminal up. A hung-up terminal is the trap. It stays readable forever while every read returns
//! end-of-file, and crossterm 0.29's default input source retries that read in a tight loop that
//! never returns (crossterm-rs/crossterm#793, fix proposed in #1067). The TUI then spins a core at
//! 100% and never reaches the code that would notice a signal, so it ignores SIGHUP and SIGTERM
//! and runs on with nobody to see it. SIGHUP does not always arrive either: the kernel sends it only
//! to the session leader and the foreground job, not to a TUI started in the background, with
//! `setsid` or under `nohup`.
//!
//! [`Input::next`] avoids all of that by blocking in a single `poll(2)` on two descriptors: the
//! terminal and a self-pipe that the signals write to. A hangup, or a SIGTERM, SIGHUP or SIGINT,
//! wakes it at once and ends the program, and it only asks crossterm for an event once there is
//! input to read. Calling `crossterm::event::poll` or `read` anywhere else would bring the trap
//! back, so clippy.toml forbids them outside this module and CI denies that lint.
//!
//! crossterm's `use-dev-tty` input source (see Cargo.toml) covers what is left: a hangup in the
//! moment between this poll and crossterm's read. That source gives up at end-of-file once its
//! timeout runs out, where the default one would spin forever. It has a trap of its own, though: with
//! a zero timeout it does nothing at all, neither reading the terminal nor handing over events it
//! already parsed. So crossterm is only ever asked with [`CROSSTERM_WAIT`].

use std::io::{self, IsTerminal, Read};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGWINCH};

/// How long crossterm may wait when asked for an event. It must not be zero (see above), and it
/// bounds the time crossterm could spend on a terminal that hangs up while it reads. It is spent
/// asleep in poll(2), and only once there is input or after a wake-up.
const CROSSTERM_WAIT: Duration = Duration::from_millis(1);

/// The signals that end the TUI.
const STOP_SIGNALS: [libc::c_int; 3] = [SIGTERM, SIGHUP, SIGINT];

/// What the terminal had to say within the time given to [`Input::next`].
#[derive(Debug)]
pub enum Next {
    /// The user pressed a key, clicked, pasted or resized the terminal.
    Event(Event),
    /// Nothing to act on yet. This can come early, so callers keep their own clock.
    Idle,
    /// The TUI must end: a termination signal arrived or the terminal hung up.
    Stop,
}

/// Reads the terminal and listens for the stop signals. There should be one, for the life of the
/// TUI: the signal handlers it installs stay for the rest of the process.
pub struct Input {
    terminal: OwnedFd,
    wake: UnixStream,
    stop: Arc<AtomicBool>,
}

impl Input {
    /// Watches the terminal crossterm reads from, which is standard input when it is a terminal and
    /// `/dev/tty` otherwise, and takes over SIGTERM, SIGHUP and SIGINT. Their default action, which
    /// ends the process at once, would leave the terminal in raw mode on the alternate screen; here
    /// the first one makes [`Input::next`] return [`Next::Stop`], so the caller unwinds and restores
    /// the terminal.
    pub fn open() -> io::Result<Self> {
        let stdin = io::stdin();
        let terminal = if stdin.is_terminal() {
            stdin.as_fd().try_clone_to_owned()?
        } else {
            std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty")?.into()
        };

        let (wake, wake_writer) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        wake_writer.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        for signal in STOP_SIGNALS {
            // signal-hook's two-step shutdown, in the order its documentation requires: the first
            // signal sets the flag and the TUI unwinds; a second one, with the flag already set,
            // exits at once. Whatever else goes wrong, a repeated SIGTERM or Ctrl-C still ends it.
            signal_hook::flag::register_conditional_shutdown(signal, 128 + signal, Arc::clone(&stop))?;
            signal_hook::flag::register(signal, Arc::clone(&stop))?;
        }
        // Each signal wakes the poll. SIGWINCH has to as well: crossterm only notices a resize
        // while it is the one waiting, so without this a resize would wait for the next timeout.
        for signal in STOP_SIGNALS.into_iter().chain([SIGWINCH]) {
            signal_hook::low_level::pipe::register(signal, wake_writer.try_clone()?)?;
        }
        Ok(Self { terminal, wake, stop })
    }

    /// Waits up to `timeout` for the next thing to act on. It never spins: it sleeps in the kernel
    /// until there is input, a signal or a hangup, or the time is up.
    pub fn next(&mut self, timeout: Duration) -> io::Result<Next> {
        if self.stop.load(Ordering::Relaxed) {
            return Ok(Next::Stop);
        }
        // One read can bring several events, and crossterm keeps the rest. They are already read,
        // so the terminal descriptor would not wake the poll for them.
        if let Some(event) = crossterm_event()? {
            return Ok(Next::Event(event));
        }

        let mut fds = [
            libc::pollfd { fd: self.terminal.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: self.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: fds is a valid array of two pollfds for the duration of the call.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) } < 0 {
            let error = io::Error::last_os_error();
            // A signal interrupted the wait. Its byte is in the self-pipe for the next call.
            return if error.kind() == io::ErrorKind::Interrupted { Ok(Next::Idle) } else { Err(error) };
        }

        if fds[1].revents != 0 {
            let mut drained = [0u8; 64];
            while matches!((&self.wake).read(&mut drained), Ok(n) if n > 0) {}
            if self.stop.load(Ordering::Relaxed) {
                return Ok(Next::Stop);
            }
        }
        if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Ok(Next::Stop);
        }
        // Input arrived, or SIGWINCH did and crossterm has a resize to report.
        if fds.iter().any(|fd| fd.revents != 0) {
            if let Some(event) = crossterm_event()? {
                return Ok(Next::Event(event));
            }
        }
        Ok(Next::Idle)
    }
}

/// The next event crossterm has, read or already parsed, if any.
// The one place allowed to call crossterm's event::poll and event::read, see clippy.toml.
#[allow(clippy::disallowed_methods)]
fn crossterm_event() -> io::Result<Option<Event>> {
    if event::poll(CROSSTERM_WAIT)? {
        return Ok(Some(event::read()?));
    }
    Ok(None)
}

impl Drop for Input {
    /// Arms the immediate exit, so from here on the stop signals end the process as their default
    /// actions would. Unregistering the handlers instead would leave the signals ignored, because
    /// signal-hook does not put the default actions back.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
