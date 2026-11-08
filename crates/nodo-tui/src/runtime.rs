//! The impure half: performing effects, owning the terminal, and the event loop.

use std::io::{self, Write};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use nodo_api::{ApiError, Backend, PublicConfirmation, Request, Response};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::{App, Effect, Event, ExposureAfter, TICKS_PER_SECOND};
use crate::model::*;
use crate::theme::Theme;
use crate::update::update;
use crate::view;

/// Reads everything the top-level screens show. Services come through the real `nodo_api` call.
pub fn load_snapshot<T: Backend + ScreenData>(backend: &mut T) -> Result<Snapshot, ApiError> {
    let services = match backend.call(Request::ListServices)? {
        Response::Services(services) => services,
        other => return Err(ApiError::Failed(format!("unexpected reply: {other:?}"))),
    };
    Ok(Snapshot {
        services,
        overview: backend.overview(),
        catalog: backend.catalog(),
        feeds: backend.feeds(),
        updates: backend.updates(),
        web: backend.web(),
        providers: backend.providers(),
        log_sources: backend.log_sources(),
        auto_update: backend.auto_update(),
    })
}

fn refresh<T: Backend + ScreenData>(backend: &mut T) -> Event {
    match load_snapshot(backend) {
        Ok(snapshot) => Event::Refreshed(Box::new(snapshot)),
        Err(error) => Event::Failed(format!("Could not read device state: {error}")),
    }
}

fn plain_result(result: Result<Response, ApiError>) -> Result<(), ApiError> {
    result.map(|_| ())
}

/// Performs one effect. Secrets only become plain text at the moment a request is built, since
/// `nodo_api::Request` holds them as `String`.
pub fn execute_effect<T: Backend + ScreenData>(backend: &mut T, effect: Effect) -> Vec<Event> {
    match effect {
        Effect::Refresh => vec![refresh(backend)],
        Effect::Start(id) => service_command(backend, Request::StartService { id }),
        Effect::Stop(id) => service_command(backend, Request::StopService { id }),
        Effect::Unlock(passphrase) => {
            let result = backend.call(Request::Unlock { passphrase: nodo_api::Secret::new(passphrase.expose()) });
            vec![Event::UnlockResult(result)]
        }
        Effect::OpenService(id) => match backend.settings_form(&id) {
            Some(form) => vec![Event::SettingsLoaded { package: id, form: Some(form) }],
            None => {
                let summary = backend.catalog().into_iter().find(|pkg| pkg.id.name == id);
                let detail = summary.and_then(|pkg| backend.package_detail(&pkg.id));
                vec![Event::DetailLoaded(detail)]
            }
        },
        Effect::LoadDetail(id) => vec![Event::DetailLoaded(backend.package_detail(&id))],
        Effect::CheckInstall(id) => vec![Event::GateLoaded(backend.install_gate(&id))],
        Effect::StartInstall(id) => match backend.start_install(&id) {
            Ok(snapshot) => vec![Event::InstallProgress(Some(snapshot))],
            Err(text) => vec![Event::Failed(text)],
        },
        Effect::PollInstall => vec![Event::InstallProgress(backend.poll_install())],
        Effect::CancelInstall => vec![Event::InstallProgress(backend.cancel_install()), refresh(backend)],
        Effect::ProbeFeed(address) => vec![Event::FeedProbed(backend.probe_feed(&address))],
        Effect::TrustFeed(probe) => {
            let result = backend.trust_feed(&probe).map(|()| probe.name);
            vec![Event::FeedTrusted(result), refresh(backend)]
        }
        Effect::RefreshFeeds => {
            backend.refresh_feeds();
            vec![refresh(backend)]
        }
        Effect::ToggleFeed(name) => {
            backend.toggle_feed(&name);
            vec![refresh(backend)]
        }
        Effect::RemoveFeed(name) => {
            backend.remove_feed(&name);
            vec![refresh(backend)]
        }
        Effect::LoadKeyChange(name) => vec![Event::KeyChangeLoaded(backend.key_change(&name))],
        Effect::TrustNewKey(name) => {
            let result = backend.trust_new_key(&name).map(|()| name);
            vec![Event::NewKeyTrusted(result), refresh(backend)]
        }
        Effect::OpenSettings(package) => {
            let form = backend.settings_form(&package);
            vec![Event::SettingsLoaded { package, form }]
        }
        Effect::LoadReferences { package, field_index, field_key, field_label } => {
            let list = backend.references(&package, &field_key);
            vec![Event::ReferencesLoaded { package, field_index, field_label, list }]
        }
        Effect::ApplySettings { package, values } => {
            // Values with no wire form are stored secrets, and those never change here.
            let changes = values
                .into_iter()
                .filter_map(|(name, value)| value.to_input().map(|value| nodo_api::SettingChange { name, value }))
                .collect();
            match backend.call(Request::ApplySettings { package: package.clone(), changes }) {
                Ok(Response::RolledBack(rollback)) => {
                    let explanation = backend.explain_rollback(&package, &rollback.reason);
                    vec![Event::SettingsRolledBack { package, explanation }, refresh(backend)]
                }
                result => vec![Event::SettingsApplied(plain_result(result)), refresh(backend)],
            }
        }
        Effect::CheckUpdates => {
            backend.check_updates();
            vec![refresh(backend)]
        }
        Effect::ReviewUpdate { name, batch, pending_reviews } => {
            vec![Event::UpdateReviewLoaded { review: backend.update_review(&name), batch, pending_reviews }]
        }
        Effect::ApplyUpdates(names) => {
            let report = backend.apply_updates(&names);
            vec![Event::UpdatesApplied(report), refresh(backend)]
        }
        Effect::SetHeld { name, held } => {
            backend.set_held(&name, held);
            vec![refresh(backend)]
        }
        Effect::SaveAutoUpdate(prefs) => {
            vec![Event::AutoUpdateSaved(backend.save_auto_update(&prefs)), refresh(backend)]
        }
        Effect::LoadLogs(source) => {
            let view = backend.logs(&source);
            vec![Event::LogsLoaded { source, view }]
        }
        Effect::Drive(action) => vec![Event::DriveResult(backend.drive_action(action)), refresh(backend)],
        Effect::Shell => vec![Event::Info("A shell is not available from this build yet.".to_owned())],
        Effect::SetExposure { site, exposure, after, confirmation } => {
            let request = Request::SetExposure {
                site: backend.site_id(&site),
                exposure,
                confirmation: confirmation.map(|typed_host| PublicConfirmation { typed_host }),
            };
            let result = plain_result(backend.call(request));
            vec![Event::ExposureSet { after, result }, refresh(backend)]
        }
        Effect::SetPublicHost { site, host } => {
            vec![Event::PublicHostSet(backend.set_public_host(&site, &host)), refresh(backend)]
        }
        Effect::SetPublicAccess { site, access } => match backend.set_public_access(&site, &access) {
            Ok(()) => Vec::new(),
            Err(text) => vec![Event::Failed(text)],
        },
        Effect::CheckReachability { site, going_public } => {
            let result = backend.reachability(&site);
            vec![Event::Reachability { site, going_public, result }]
        }
        Effect::TestProvider { provider, domain, api_key, secret_key } => {
            vec![Event::ProviderTested(backend.test_provider(&provider, &domain, &api_key, &secret_key))]
        }
        Effect::SaveProvider { provider, domain, api_key, secret_key } => {
            let result = backend.save_provider(&provider, &domain, &api_key, &secret_key);
            vec![Event::ProviderSaved(result), refresh(backend)]
        }
        Effect::ShowDocument(kind) => vec![Event::DocumentLoaded(backend.document(&kind))],
        Effect::CopyReport(subject) => match backend.copy_report(&subject) {
            Ok(text) => vec![Event::Info(text)],
            Err(text) => vec![Event::Failed(text)],
        },
    }
}

fn service_command<T: Backend + ScreenData>(backend: &mut T, request: Request) -> Vec<Event> {
    let mut events = Vec::new();
    if let Err(error) = backend.call(request) {
        events.push(Event::Failed(error.to_string()));
    }
    events.push(refresh(backend));
    events
}

/// Feeds an event through `update` and performs every resulting effect until none are left.
pub fn dispatch<T: Backend + ScreenData>(app: &mut App, backend: &mut T, event: Event) {
    let mut queue = std::collections::VecDeque::from([event]);
    while let Some(event) = queue.pop_front() {
        let mut effects = std::collections::VecDeque::from(update(app, event));
        while let Some(effect) = effects.pop_front() {
            let events = execute_effect(backend, effect);
            // A step towards public never runs after its prerequisite failed. Other batches, such
            // as taking every site offline, carry on so one failure does not leave the rest up.
            if events.iter().any(reports_failure) && effects.front().is_some_and(moves_towards_public) {
                effects.clear();
            }
            queue.extend(events);
        }
    }
}

fn reports_failure(event: &Event) -> bool {
    matches!(event, Event::Failed(_) | Event::ExposureSet { result: Err(_), .. })
}

fn moves_towards_public(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::CheckReachability { going_public: true, .. }
            | Effect::SetExposure { after: ExposureAfter::WentPublic, .. }
    )
}

/// How the terminal is put into and out of TUI mode, separated so tests can observe it.
pub trait TerminalMode: Send + Sync + 'static {
    fn enter(&self) -> io::Result<()>;
    /// Must be safe to call more than once, and from a panic hook.
    fn leave(&self);
}

/// Writes the sequence that puts a terminal back, ignoring errors, because the peer may be gone.
pub fn write_restore(out: &mut impl Write) {
    let _ = execute!(out, DisableBracketedPaste, LeaveAlternateScreen, Show);
    let _ = out.flush();
}

pub struct CrosstermMode;

impl TerminalMode for CrosstermMode {
    fn enter(&self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste, Hide)
    }

    fn leave(&self) {
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

/// Runs the TUI until the user quits or a termination signal arrives.
pub fn run<T: Backend + ScreenData>(mut app: App, mut backend: T, theme: Theme) -> io::Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }

    let _guard = TerminalGuard::enter(CrosstermMode)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    let (width, height) = crossterm::terminal::size()?;
    dispatch(&mut app, &mut backend, Event::Resize(width, height));
    if app.data == Snapshot::default() {
        for event in execute_effect(&mut backend, Effect::Refresh) {
            dispatch(&mut app, &mut backend, event);
        }
    }

    let tick = Duration::from_millis(1000 / TICKS_PER_SECOND);
    let mut last_tick = Instant::now();
    while !app.should_quit && !stop.load(Ordering::Relaxed) {
        terminal.draw(|frame| {
            view::render(frame, &app, &theme);
        })?;
        let wait = tick.saturating_sub(last_tick.elapsed());
        if event::poll(wait)? {
            match event::read()? {
                // Some terminals also report key releases, which must not count as presses.
                event::Event::Key(key) if key.kind != KeyEventKind::Release => {
                    dispatch(&mut app, &mut backend, Event::Key(key));
                }
                event::Event::Paste(text) => dispatch(&mut app, &mut backend, Event::Paste(text)),
                event::Event::Resize(width, height) => {
                    dispatch(&mut app, &mut backend, Event::Resize(width, height));
                }
                _ => {}
            }
        }
        if last_tick.elapsed() >= tick {
            last_tick = Instant::now();
            dispatch(&mut app, &mut backend, Event::Tick);
        }
    }
    Ok(())
}
