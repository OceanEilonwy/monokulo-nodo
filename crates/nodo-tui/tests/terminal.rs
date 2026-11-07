//! Terminal restoration, on normal exit and on panic, both observed through a fake and proven on
//! a real pseudo-terminal using util-linux `script`. The real-terminal tests skip where `script`
//! is not installed.

use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use nodo_tui::runtime::{write_restore, CrosstermMode, TerminalGuard, TerminalMode};

struct Recording {
    entered: Arc<AtomicUsize>,
    left: Arc<AtomicUsize>,
    fail_enter: bool,
}

impl TerminalMode for Recording {
    fn enter(&self) -> std::io::Result<()> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        if self.fail_enter {
            return Err(std::io::Error::other("no terminal"));
        }
        Ok(())
    }

    fn leave(&self) {
        self.left.fetch_add(1, Ordering::SeqCst);
    }
}

fn recording(fail_enter: bool) -> (Recording, Arc<AtomicUsize>) {
    let left = Arc::new(AtomicUsize::new(0));
    (Recording { entered: Arc::new(AtomicUsize::new(0)), left: Arc::clone(&left), fail_enter }, left)
}

#[test]
fn dropping_the_guard_restores_the_terminal() {
    let (mode, left) = recording(false);
    let guard = TerminalGuard::enter(mode).unwrap();
    assert_eq!(left.load(Ordering::SeqCst), 0);
    drop(guard);
    assert_eq!(left.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failed_enter_restores_and_reports_the_error() {
    let (mode, left) = recording(true);
    assert!(TerminalGuard::enter(mode).is_err());
    assert_eq!(left.load(Ordering::SeqCst), 1);
}

#[test]
fn a_panic_restores_through_both_the_hook_and_the_guard() {
    let (mode, left) = recording(false);
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _guard = TerminalGuard::enter(mode).unwrap();
        panic!("expected by this test");
    }));
    assert!(result.is_err());
    assert!(left.load(Ordering::SeqCst) >= 2, "hook and drop both ran");
}

#[test]
fn the_restore_sequence_leaves_the_alternate_screen_and_shows_the_cursor() {
    let mut out = Vec::new();
    write_restore(&mut out);
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("\u{1b}[?1049l"), "{out:?}");
    assert!(out.contains("\u{1b}[?25h"), "{out:?}");
    assert!(out.contains("\u{1b}[?2004l"), "{out:?}");
}

struct Broken;

impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("the SSH session is gone"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("the SSH session is gone"))
    }
}

#[test]
fn restoring_to_a_dead_terminal_does_not_panic() {
    write_restore(&mut Broken);
}

/// Runs a shell line inside a new pseudo-terminal and returns everything it wrote. Input is sent
/// after a pause, because keys must arrive once raw mode is on, and stdin then stays open for a
/// while because script(1) ends the session on EOF.
fn in_pty(script_line: &str, input: &'static [u8]) -> Option<String> {
    let mut child = match Command::new("script")
        .args(["-qfec", script_line, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            eprintln!("skipping: script(1) is not installed");
            return None;
        }
    };
    let mut stdin = child.stdin.take().expect("piped");
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(800));
        let _ = stdin.write_all(input);
        let _ = stdin.flush();
        std::thread::sleep(std::time::Duration::from_secs(1));
    });
    let output = child.wait_with_output().expect("wait");
    writer.join().expect("join");
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Not a test of its own: `a_real_terminal_is_restored_after_a_panic` runs it inside a PTY.
#[test]
#[ignore = "only meaningful inside a pseudo-terminal"]
fn panic_probe() {
    let _guard = TerminalGuard::enter(CrosstermMode).unwrap();
    panic!("panic probe");
}

fn assert_terminal_is_sane(output: &str) {
    let enter = output.find("\u{1b}[?1049h").expect("entered the alternate screen");
    let leave = output.rfind("\u{1b}[?1049l").expect("left the alternate screen");
    assert!(enter < leave, "{output:?}");
    assert!(output[leave..].contains("\u{1b}[?25h"), "the cursor is shown again: {output:?}");
    // stty ran after the program ended. Raw mode would show as -icanon and -echo.
    let settings = &output[leave..];
    assert!(settings.contains(" icanon"), "{settings:?}");
    assert!(!settings.contains("-icanon"), "{settings:?}");
    assert!(!settings.contains("-echo "), "{settings:?}");
}

#[test]
fn a_real_terminal_is_restored_after_a_panic() {
    let exe = std::env::current_exe().unwrap();
    let line = format!("{} --ignored --exact panic_probe --nocapture; stty -a", exe.display());
    let Some(output) = in_pty(&line, b"") else {
        return;
    };
    assert!(output.contains("panic probe"), "the panic message is still shown: {output:?}");
    assert_terminal_is_sane(&output);
}

#[test]
fn a_real_terminal_is_restored_after_quitting_the_binary() {
    let line = format!("{} --mock; echo exit=$?; stty -a", env!("CARGO_BIN_EXE_nodo-tui"));
    let Some(output) = in_pty(&line, b"q") else {
        return;
    };
    assert!(output.contains("exit=0"), "{output:?}");
    assert_terminal_is_sane(&output);
}
