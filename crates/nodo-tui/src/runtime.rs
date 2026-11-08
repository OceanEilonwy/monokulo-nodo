//! The impure half: performing effects, owning the terminal, and the event loop.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
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

use crate::app::{App, Effect, Event, ExposureAfter, UsbView, TICKS_PER_SECOND};
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
        unavailable: backend.unavailable(),
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
        Effect::StartInstall { id, approval, passphrase } => match backend.start_install(&id, approval.as_ref(), passphrase.as_ref()) {
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
        Effect::TrustNewKey { change, typed } => {
            let result = backend.trust_new_key(&change, &typed).map(|()| change.feed.clone());
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
        Effect::ReviewUpdate { name, batch, pending_reviews, approvals } => {
            vec![Event::UpdateReviewLoaded { review: backend.update_review(&name), batch, pending_reviews, approvals }]
        }
        Effect::ApplyUpdates { names, approvals } => {
            let report = backend.apply_updates(&names, &approvals);
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
        Effect::SaveAcme { settings, dns01 } => {
            // The credentials go first, since local names under the domain need DNS-01.
            let stored = match dns01 {
                Some((first, second)) => backend.save_dns01(&first, &second),
                None => Ok(()),
            };
            let result = stored.and_then(|()| backend.save_acme(&settings));
            vec![Event::AcmeSaved(result), refresh(backend)]
        }
        Effect::CheckLocalNames => vec![Event::LocalNamesChecked(backend.check_local_names())],
        Effect::ShowDocument(kind) => vec![Event::DocumentLoaded(backend.document(&kind))],
        Effect::CopyReport(subject) => match backend.copy_report(&subject) {
            Ok(text) => vec![Event::Info(text)],
            Err(text) => vec![Event::Failed(text)],
        },
        Effect::InitEncryption(passphrase) => {
            let request = Request::InitEncryption { passphrase: nodo_api::Secret::new(passphrase.expose()) };
            let result = plain_result(backend.call(request));
            vec![Event::SetupDone(result), refresh(backend)]
        }
        Effect::LoadSshKeys => vec![Event::SshKeysLoaded(backend.ssh_keys())],
        Effect::AddSshKey(line) => {
            let result = backend.add_ssh_key(&line);
            vec![Event::SshKeyChanged(result), Event::SshKeysLoaded(backend.ssh_keys())]
        }
        Effect::RemoveSshKey(fingerprint) => {
            let result = backend.remove_ssh_key(&fingerprint);
            vec![Event::SshKeyChanged(result), Event::SshKeysLoaded(backend.ssh_keys())]
        }
        Effect::LoadBackups => vec![Event::BackupsLoaded(backend.backups()), Event::UsbLoaded(load_usb(backend))],
        Effect::CreateBackup { device_passphrase, backup_passphrase, usb } => {
            let result = match usb {
                Some(device) => backend.create_usb_backup(&device, device_passphrase.as_ref(), &backup_passphrase),
                None => backend.create_backup(device_passphrase.as_ref(), &backup_passphrase),
            };
            vec![Event::BackupCreated(result)]
        }
        Effect::InspectBackup { name, passphrase, usb } => {
            let result = match usb {
                Some(device) => backend.inspect_usb_backup(&device, &name, &passphrase),
                None => backend.inspect_backup(&name, &passphrase),
            };
            vec![Event::BackupInspected(result)]
        }
        Effect::RestoreBackup { name, passphrase, new_device_passphrase, typed, usb } => {
            let result = match usb {
                Some(device) => {
                    backend.restore_usb_backup(&device, &name, &passphrase, new_device_passphrase.as_ref(), &typed)
                }
                None => backend.restore_backup(&name, &passphrase, new_device_passphrase.as_ref(), &typed),
            };
            vec![Event::BackupRestored(result), refresh(backend)]
        }
        Effect::LoadOnion => vec![Event::OnionLoaded(onion_status(backend))],
        Effect::SetNodeOnion(enabled) => {
            let result = backend.call(Request::SetNodeOnion { enabled }).map(|_| ()).map_err(|e| e.to_string());
            vec![Event::OnionChanged(result), Event::OnionLoaded(onion_status(backend)), refresh(backend)]
        }
        Effect::SetSiteExposure { site, exposure } => {
            let request = Request::SetExposure { site, exposure, confirmation: None };
            let result = backend.call(request).map(|_| ()).map_err(|e| e.to_string());
            vec![Event::OnionChanged(result), Event::OnionLoaded(onion_status(backend)), refresh(backend)]
        }
        Effect::LoadRestoreDrives => vec![Event::RestoreDrives(backend.usb_drives())],
        Effect::LoadRestoreBackups(device) => vec![Event::RestoreBackups(backend.usb_backups(&device))],
        Effect::InspectRestore { device, name, passphrase } => {
            vec![Event::RestoreInspected(backend.inspect_usb_backup(&device, &name, &passphrase))]
        }
        Effect::RunRestore { device, name, passphrase, new_device_passphrase, typed } => {
            let result =
                backend.restore_usb_backup(&device, &name, &passphrase, Some(&new_device_passphrase), &typed);
            vec![Event::RestoreFinished(result), refresh(backend)]
        }
        Effect::LoadRollbacks => vec![Event::RollbacksLoaded(rollback_view(backend))],
        Effect::RollBack { package, to } => {
            let result = match backend.call(Request::RollbackPackage { package, to }) {
                Ok(Response::UpdateResults(mut results)) if !results.is_empty() => Ok(results.remove(0)),
                Ok(other) => Err(format!("unexpected reply: {other:?}")),
                Err(e) => Err(e.to_string()),
            };
            vec![Event::RolledBack(result)]
        }
    }
}

fn onion_status<T: Backend + ScreenData>(backend: &mut T) -> Result<nodo_api::OnionStatus, String> {
    match backend.call(Request::OnionStatus) {
        Ok(Response::Onion(status)) => Ok(status),
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Versions that can be put back, failed updates that were rolled back, and a system fallback.
/// Only the first is needed; the others are left out when the helper cannot say.
fn rollback_view<T: Backend + ScreenData>(backend: &mut T) -> Result<crate::app::RollbackView, String> {
    let options = match backend.call(Request::ListRollbacks) {
        Ok(Response::Rollbacks(list)) => list,
        Ok(other) => return Err(format!("unexpected reply: {other:?}")),
        Err(e) => return Err(e.to_string()),
    };
    let mut events = Vec::new();
    if let Ok(Response::UpdateStatus(status)) = backend.call(Request::UpdateStatus) {
        for event in status.attention.into_iter().chain(status.history) {
            let rolled = matches!(
                event.kind,
                nodo_api::UpdateEventKind::RolledBack { .. } | nodo_api::UpdateEventKind::RollbackFailed { .. }
            );
            if rolled && !events.contains(&event) {
                events.push(event);
            }
        }
    }
    events.sort_by_key(|e: &nodo_api::UpdateEvent| std::cmp::Reverse(e.at));
    let boot_fallback = match backend.call(Request::Health) {
        Ok(Response::Health(health)) => health.boot_fallback,
        _ => None,
    };
    Ok(crate::app::RollbackView { options, events, boot_fallback })
}

/// The first usable USB drive with its backups, or the first drive and why it cannot be used.
fn load_usb<T: Backend + ScreenData>(backend: &mut T) -> Result<UsbView, String> {
    let drives = backend.usb_drives()?;
    let drive = drives
        .iter()
        .find(|d| d.problem.is_none())
        .or_else(|| drives.first())
        .cloned()
        .ok_or_else(|| "No USB drive is plugged in.".to_owned())?;
    let list = match drive.problem {
        None => Some(backend.usb_backups(&drive.device)?),
        Some(_) => None,
    };
    Ok(UsbView { drive, list })
}

fn service_command<T: Backend + ScreenData>(backend: &mut T, request: Request) -> Vec<Event> {
    let mut events = Vec::new();
    if let Err(error) = backend.call(request) {
        events.push(Event::Failed(error.to_string()));
    }
    events.push(refresh(backend));
    events
}

/// Performs one batch of effects in order, handing each effect's events to `deliver`.
fn run_batch<T: Backend + ScreenData>(backend: &mut T, batch: Vec<Effect>, mut deliver: impl FnMut(Vec<Event>)) {
    let mut effects = VecDeque::from(batch);
    while let Some(effect) = effects.pop_front() {
        let events = execute_effect(backend, effect);
        // A step towards public never runs after its prerequisite failed. Other batches, such
        // as taking every site offline, carry on so one failure does not leave the rest up.
        if events.iter().any(reports_failure) && effects.front().is_some_and(moves_towards_public) {
            effects.clear();
        }
        deliver(events);
    }
}

/// Feeds an event through `update` and performs every resulting effect until none are left, on
/// this thread. Tests use this; the terminal runtime uses [`EffectWorker`].
pub fn dispatch<T: Backend + ScreenData>(app: &mut App, backend: &mut T, event: Event) {
    let mut queue = VecDeque::from([event]);
    while let Some(event) = queue.pop_front() {
        let effects = update(app, event);
        run_batch(backend, effects, |events| queue.extend(events));
    }
}

enum FromWorker {
    Events(Vec<Event>),
    BatchDone,
}

/// Owns the backend on its own thread, so a helper call that takes minutes never stops the
/// screen from drawing or keys from being read.
pub struct EffectWorker {
    batches: Sender<Vec<Effect>>,
    results: Receiver<FromWorker>,
    in_flight: usize,
}

impl EffectWorker {
    pub fn spawn<T: Backend + ScreenData + Send + 'static>(mut backend: T) -> Self {
        let (batches, inbox) = mpsc::channel::<Vec<Effect>>();
        let (outbox, results) = mpsc::channel();
        thread::spawn(move || {
            for batch in inbox {
                run_batch(&mut backend, batch, |events| {
                    let _ = outbox.send(FromWorker::Events(events));
                });
                if outbox.send(FromWorker::BatchDone).is_err() {
                    return;
                }
            }
        });
        Self { batches, results, in_flight: 0 }
    }

    pub fn busy(&self) -> bool {
        self.in_flight > 0
    }

    pub fn submit(&mut self, effects: Vec<Effect>) {
        if !effects.is_empty() && self.batches.send(effects).is_ok() {
            self.in_flight += 1;
        }
    }

    fn take(&mut self, message: FromWorker, events: &mut Vec<Event>) {
        match message {
            FromWorker::Events(more) => events.extend(more),
            FromWorker::BatchDone => self.in_flight = self.in_flight.saturating_sub(1),
        }
    }

    /// Events that are ready now, without waiting.
    pub fn ready(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(message) = self.results.try_recv() {
            self.take(message, &mut events);
        }
        events
    }

    /// Waits up to `timeout` for the worker to go idle, returning what arrived meanwhile.
    pub fn wait_idle(&mut self, timeout: Duration) -> Vec<Event> {
        let deadline = Instant::now() + timeout;
        let mut events = Vec::new();
        while self.busy() {
            match self.results.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(message) => self.take(message, &mut events),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        events
    }
}

/// Feeds an event through `update` and hands the effects to the worker. Their results come back
/// later through [`EffectWorker::ready`].
pub fn dispatch_async(app: &mut App, worker: &mut EffectWorker, event: Event) {
    let effects = update(app, event);
    worker.submit(effects);
    app.working = worker.busy();
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

/// Runs the TUI until the user quits or a termination signal arrives. The backend lives on a
/// worker thread, so slow helper calls never freeze the screen.
pub fn run<T: Backend + ScreenData + Send + 'static>(mut app: App, backend: T, theme: Theme) -> io::Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }

    let _guard = TerminalGuard::enter(CrosstermMode)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    let mut worker = EffectWorker::spawn(backend);
    let (width, height) = crossterm::terminal::size()?;
    dispatch_async(&mut app, &mut worker, Event::Resize(width, height));
    if app.data == Snapshot::default() {
        worker.submit(vec![Effect::Refresh]);
    }

    let tick = Duration::from_millis(1000 / TICKS_PER_SECOND);
    let mut last_tick = Instant::now();
    while !app.should_quit && !stop.load(Ordering::Relaxed) {
        for event in worker.ready() {
            dispatch_async(&mut app, &mut worker, event);
        }
        app.working = worker.busy();
        terminal.draw(|frame| {
            view::render(frame, &app, &theme);
        })?;
        let wait = tick.saturating_sub(last_tick.elapsed());
        if event::poll(wait)? {
            match event::read()? {
                // Some terminals also report key releases, which must not count as presses.
                event::Event::Key(key) if key.kind != KeyEventKind::Release => {
                    dispatch_async(&mut app, &mut worker, Event::Key(key));
                }
                event::Event::Paste(text) => dispatch_async(&mut app, &mut worker, Event::Paste(text)),
                event::Event::Resize(width, height) => {
                    dispatch_async(&mut app, &mut worker, Event::Resize(width, height));
                }
                _ => {}
            }
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
    Ok(())
}
