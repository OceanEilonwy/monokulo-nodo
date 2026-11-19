//! The impure half: performing effects, owning the terminal, and the event loop.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::panic;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, KeyEventKind,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use nodo_api::Backend;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::{App, Event, MouseKind, Screen, TICKS_PER_SECOND};
use crate::input::{Input, Next};
use nodo_ui_core::exec::{run_batch, EffectWorker};
use nodo_ui_core::model::*;
use nodo_ui_core::Effect;
use crate::theme::Theme;
use crate::update::update;
use crate::view;

/// Feeds an event through `update` and performs every resulting effect until none are left, on
/// this thread. Tests use this; the terminal runtime uses [`EffectWorker`].
pub fn dispatch<T: Backend + ScreenData + ?Sized>(app: &mut App, backend: &mut T, event: Event) {
    let mut queue = VecDeque::from([event]);
    while let Some(event) = queue.pop_front() {
        let effects = update(app, event);
        run_batch(backend, effects, |outcomes| queue.extend(outcomes.into_iter().map(Event::Done)));
    }
    // Every effect has run, so no button is still waiting.
    app.pending = None;
}

/// Feeds an event through `update` and hands the effects to the worker. Their results come back
/// later through [`EffectWorker::ready`].
pub fn dispatch_async(app: &mut App, worker: &mut EffectWorker, event: Event) {
    let effects = update(app, event);
    worker.submit(effects);
    sync_working(app, worker);
}

/// Mirrors the worker's state into the app. Once it is idle, no work a button started can still be
/// running, so the pending button is cleared even if its result never named it.
fn sync_working(app: &mut App, worker: &EffectWorker) {
    app.working = worker.busy();
    if !app.working {
        app.pending = None;
    }
}

/// How the terminal is put into and out of TUI mode, separated so tests can observe it.
pub trait TerminalMode: Send + Sync + 'static {
    fn enter(&self) -> io::Result<()>;
    /// Must be safe to call more than once, and from a panic hook.
    fn leave(&self);
}

/// Writes the sequence that puts a terminal back, ignoring errors, because the peer may be gone.
/// The keyboard flags are popped first, because a terminal keeps them per screen. Popping them is
/// harmless where none were pushed: terminals without the kitty protocol ignore it.
#[allow(clippy::let_underscore_must_use)]
pub fn write_restore(out: &mut impl Write) {
    // Ignored: the terminal may have hung up, and there is nobody left to tell.
    let _ = execute!(out, PopKeyboardEnhancementFlags, DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen, Show);
    // Ignored for the same reason.
    let _ = out.flush();
}

pub struct CrosstermMode;

impl TerminalMode for CrosstermMode {
    /// Asks for the kitty keyboard protocol's disambiguated keys, so that terminals which speak it
    /// report Ctrl-. (which opens the settings picker). Only the first level is asked for: keys
    /// keep arriving as presses, Enter, Tab and Backspace keep their usual codes, and terminals
    /// without the protocol ignore the request. It is not queried first, because a terminal that
    /// never answers would hold up the start.
    fn enter(&self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture, Hide)?;
        // Ignored: the flags are a nicety, and a terminal that refuses them still works without.
        #[allow(clippy::let_underscore_must_use)]
        let _ = execute!(io::stdout(), PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
        Ok(())
    }

    fn leave(&self) {
        // Ignored: this runs on the way out and from the panic hook, where nothing can report it.
        #[allow(clippy::let_underscore_must_use)]
        let _ = disable_raw_mode();
        write_restore(&mut io::stdout());
    }
}

/// Restores the terminal on drop, which covers normal exit, early return and unwinding.
pub struct TerminalGuard<M: TerminalMode> {
    mode: Arc<M>,
}

impl<M: TerminalMode> TerminalGuard<M> {
    pub fn enter(mode: M) -> io::Result<Self> {
        let mode = Arc::new(mode);
        if let Err(error) = mode.enter() {
            mode.leave();
            return Err(error);
        }
        install_panic_hook(Arc::clone(&mode));
        Ok(Self { mode })
    }
}

impl<M: TerminalMode> Drop for TerminalGuard<M> {
    fn drop(&mut self) {
        self.mode.leave();
    }
}

/// Restores first and then runs the previous hook, so the panic message lands on the normal
/// screen instead of vanishing with the alternate one. This also covers `panic = "abort"`, where
/// `Drop` never runs.
fn install_panic_hook<M: TerminalMode>(mode: Arc<M>) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        mode.leave();
        previous(info);
    }));
}

/// Why [`run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The user quit, a termination signal arrived or the terminal hung up.
    Quit,
    /// An update replaced this program on disk. The terminal is restored; the caller starts the
    /// new program in this process.
    Restart,
}

/// How often the event loop looks at whether an update replaced this program.
const REPLACED_CHECK: Duration = Duration::from_secs(5);

/// The program file this process was started from, as the kernel names it. Once the file is
/// replaced, the kernel adds " (deleted)" to the old name.
fn installed_program() -> Option<std::path::PathBuf> {
    let link = std::fs::read_link("/proc/self/exe").ok()?;
    let text = link.to_str()?;
    Some(text.strip_suffix(" (deleted)").unwrap_or(text).into())
}

/// Whether the file at this process's program path is no longer the one it runs: an update
/// installed a new nodo-tui. A program that was only removed is not replaced.
pub fn program_replaced() -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(running) = std::fs::metadata("/proc/self/exe") else { return false };
    let Some(path) = installed_program() else { return false };
    std::fs::metadata(path).is_ok_and(|on_disk| (on_disk.dev(), on_disk.ino()) != (running.dev(), running.ino()))
}

/// The new program to start in place of this one, after [`Exit::Restart`].
pub fn replacement_program() -> Option<std::path::PathBuf> {
    installed_program().filter(|path| path.is_file())
}

/// A moment when starting again loses nothing: on one of the four tabs, with nothing typed,
/// asked or running.
pub fn at_rest(app: &App, worker_busy: bool) -> bool {
    !worker_busy
        && !app.working
        && app.pending.is_none()
        && app.modal.is_none()
        && app.overlay.is_none()
        && matches!(app.screen, Screen::Home { .. } | Screen::Apps { .. } | Screen::Web { .. } | Screen::System { .. })
}

/// Runs the TUI until the user quits, a termination signal arrives or the terminal hangs up. The
/// backend lives on a worker thread, so slow helper calls never freeze the screen. With `restart_when_replaced`, it
/// also returns [`Exit::Restart`] when an update replaced this program and nothing is under way,
/// so an open TUI never keeps running an old version.
pub fn run<T: Backend + ScreenData + Send + 'static>(
    mut app: App,
    backend: T,
    theme: Theme,
    restart_when_replaced: bool,
) -> io::Result<Exit> {
    // Declared before the guard, so it is dropped after the terminal is restored.
    let mut input = Input::open()?;
    let _guard = TerminalGuard::enter(CrosstermMode)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    // ratatui 0.30's Terminal::clear asks the terminal where its cursor is, which a terminal that
    // never answers turns into an error; clearing directly needs no answer.
    crossterm::execute!(terminal.backend_mut(), crossterm::terminal::Clear(crossterm::terminal::ClearType::All))?;

    let mut worker = EffectWorker::spawn(Box::new(backend));
    let (width, height) = crossterm::terminal::size()?;
    dispatch_async(&mut app, &mut worker, Event::Resize(width, height));
    if app.data == Snapshot::default() {
        worker.submit(vec![Effect::Refresh]);
    }

    let tick = Duration::from_millis(1000 / TICKS_PER_SECOND);
    let mut last_tick = Instant::now();
    let mut last_replaced_check = Instant::now();
    while !app.should_quit {
        if restart_when_replaced && last_replaced_check.elapsed() >= REPLACED_CHECK {
            last_replaced_check = Instant::now();
            if at_rest(&app, worker.busy()) && program_replaced() {
                return Ok(Exit::Restart);
            }
        }
        for outcome in worker.ready() {
            dispatch_async(&mut app, &mut worker, Event::Done(outcome));
        }
        sync_working(&mut app, &worker);
        if let Some(text) = app.clipboard.take() {
            // Between frames, so it never lands inside one.
            // A terminal that cannot take this cannot take the next frame either, so it ends the
            // loop the same way a failed draw does.
            let out = terminal.backend_mut();
            out.write_all(crate::clipboard::osc52(&text).as_bytes())?;
            out.flush()?;
        }
        terminal.draw(|frame| {
            view::render(frame, &app, &theme);
        })?;
        let wait = tick.saturating_sub(last_tick.elapsed());
        match input.next(wait)? {
            Next::Stop => break,
            Next::Idle => {}
            Next::Event(terminal_event) => match terminal_event {
                // Some terminals also report key releases, which must not count as presses.
                event::Event::Key(key) if key.kind != KeyEventKind::Release => {
                    dispatch_async(&mut app, &mut worker, Event::Key(key));
                }
                event::Event::Paste(text) => dispatch_async(&mut app, &mut worker, Event::Paste(text)),
                event::Event::Mouse(mouse) => {
                    let kind = match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => Some(MouseKind::Down),
                        MouseEventKind::Moved | MouseEventKind::Drag(MouseButton::Left) => Some(MouseKind::Moved),
                        MouseEventKind::ScrollUp => Some(MouseKind::ScrollUp),
                        MouseEventKind::ScrollDown => Some(MouseKind::ScrollDown),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        // A move only matters when it changes which target is under the pointer, so
                        // moves within one target, or over nothing, cost nothing.
                        let changes = kind != MouseKind::Moved || {
                            let under = crate::hit::hit_at(&app.hits.borrow(), mouse.column, mouse.row).map(|hit| hit.rect);
                            under != app.hover
                        };
                        if changes {
                            dispatch_async(&mut app, &mut worker, Event::Mouse { kind, column: mouse.column, row: mouse.row });
                        }
                    }
                }
                event::Event::Resize(width, height) => {
                    dispatch_async(&mut app, &mut worker, Event::Resize(width, height));
                }
                _ => {}
            },
        }
        if last_tick.elapsed() >= tick {
            last_tick = Instant::now();
            // Ticks only poll. While the worker is busy their effects would just queue behind it.
            let effects = update(&mut app, Event::Tick);
            if !worker.busy() {
                worker.submit(effects);
            }
        }
    }
    Ok(Exit::Quit)
}

#[cfg(test)]
mod restart_tests {
    use super::*;

    #[test]
    fn restarts_only_at_rest_on_a_tab() {
        let mut app = App::new();
        app.screen = Screen::System { selected: 0 };
        assert!(at_rest(&app, false));
        assert!(!at_rest(&app, true), "a request is still running");
        app.working = true;
        assert!(!at_rest(&app, false));
        app.working = false;
        app.screen = Screen::Updates(crate::app::UpdatesState { selected: 0, checked: vec![], passphrase: None });
        assert!(!at_rest(&app, false), "the owner is in the middle of something");
    }

    #[test]
    fn this_test_program_was_not_replaced() {
        assert!(!program_replaced());
        assert!(replacement_program().is_some());
    }
}
