//! Performing effects: the helper calls behind every `Effect`, and the thread that makes them.
//!
//! Everything here takes the backend as `Backend + ScreenData`, sized or not, so an interface can
//! hold it as a named type or behind `dyn Data`.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use nodo_api::{ApiError, Backend, PublicConfirmation, Request, Response};

use crate::effect::{Effect, ExposureAfter, Outcome, RollbackView, UsbView};
use crate::model::*;

/// Both halves of a backend, and able to move to the worker's thread: what an interface holds
/// when it does not name the backend's type.
pub trait Data: Backend + ScreenData + Send {}
impl<T: Backend + ScreenData + Send> Data for T {}

/// Reads everything the top-level screens show. Services come through the real `nodo_api` call.
pub fn load_snapshot<T: Backend + ScreenData + ?Sized>(backend: &mut T) -> Result<Snapshot, ApiError> {
    let services = match backend.call(Request::ListServices)? {
        Response::Services(services) => services,
        other => return Err(ApiError::Failed(format!("unexpected reply: {other:?}"))),
    };
    let (providers, providers_error) = split(backend.providers());
    let web = backend.web();
    let mut overview = backend.overview();
    // DNS that stopped updating takes public sites offline at the next change of home address, and
    // nobody opens Web to find out, so Home says so.
    overview.attention.extend(web.dns_problems.iter().map(|p| p.attention()));
    Ok(Snapshot {
        capabilities: backend.capabilities(),
        services,
        overview,
        catalog: backend.catalog(),
        feeds: backend.feeds(),
        updates: backend.updates(),
        web,
        providers,
        providers_error,
        connected: backend.connected_providers(),
        log_sources: backend.log_sources(),
        auto_update: backend.auto_update(),
        unavailable: backend.unavailable(),
        system_note: backend.system_note(),
        owner_pages: backend.owner_pages(),
        // Asked last, and never a reason to fail the read: a helper that does not know the
        // request leaves it unknown.
        web_gui: match backend.call(Request::WebGuiStatus) {
            Ok(Response::WebGui(status)) => Some(status),
            _ => None,
        },
    })
}

/// A list, or no list and why, as the snapshot keeps them.
fn split<T>(result: Result<Vec<T>, String>) -> (Vec<T>, Option<String>) {
    match result {
        Ok(list) => (list, None),
        Err(e) => (Vec::new(), Some(e)),
    }
}

fn refresh<T: Backend + ScreenData + ?Sized>(backend: &mut T) -> Outcome {
    match load_snapshot(backend) {
        Ok(snapshot) => Outcome::Refreshed(Box::new(snapshot)),
        Err(error) => Outcome::Failed(format!("Could not read device state: {error}")),
    }
}

fn plain_result(result: Result<Response, ApiError>) -> Result<(), ApiError> {
    result.map(|_| ())
}

/// Performs one effect. Secrets only become plain text at the moment a request is built, since
/// `nodo_api::Request` holds them as `String`.
pub fn execute_effect<T: Backend + ScreenData + ?Sized>(backend: &mut T, effect: Effect) -> Vec<Outcome> {
    match effect {
        Effect::InstallBundle { review, answers, data_drive, passphrase } => {
            let result = backend.install_bundle(&review, answers, data_drive, passphrase.as_ref());
            vec![Outcome::BundleInstalled(result), refresh(backend)]
        }
        Effect::LoadOwnerPage { package, passphrase } => {
            let result = backend.owner_page(&package, &passphrase);
            vec![Outcome::OwnerPageLoaded { package, result }]
        }
        Effect::SetWebGui(enabled) => {
            let result = match backend.call(Request::SetWebGui(enabled)) {
                Ok(Response::WebGui(status)) => Ok(status),
                Ok(other) => Err(ApiError::Failed(format!("unexpected reply: {other:?}"))),
                Err(error) => Err(error),
            };
            vec![Outcome::WebGuiSet { enabled, result }, refresh(backend)]
        }
        Effect::Refresh => vec![refresh(backend)],
        Effect::Start(id) => service_command(backend, Request::StartService { id }),
        Effect::Stop(id) => service_command(backend, Request::StopService { id }),
        Effect::Restart(id) => service_command(backend, Request::RestartService { id }),
        Effect::RemovePackage(package) => {
            let mut events = Vec::new();
            match backend.call(Request::RemovePackage { package: package.clone() }) {
                Ok(_) => events.push(Outcome::Info(format!("{package} was removed. Its data stays on the drive; installing it again brings it back."))),
                Err(error) => events.push(Outcome::Failed(error.to_string())),
            }
            events.push(refresh(backend));
            events
        }
        Effect::Unlock(passphrase) => {
            let result = backend.call(Request::Unlock { passphrase: nodo_api::Secret::new(passphrase.expose()) });
            vec![Outcome::UnlockResult(result)]
        }
        Effect::OpenService(id) => match backend.settings_form(&id) {
            Some(form) => vec![Outcome::SettingsLoaded { package: id, form: Some(form) }],
            None => {
                let summary = backend.catalog().into_iter().find(|pkg| pkg.id.name == id);
                let detail = summary.and_then(|pkg| backend.package_detail(&pkg.id));
                vec![Outcome::DetailLoaded(detail)]
            }
        },
        Effect::LoadDetail(id) => vec![Outcome::DetailLoaded(backend.package_detail(&id))],
        Effect::CheckInstall(id) => vec![Outcome::GateLoaded(backend.install_gate(&id))],
        Effect::StartInstall { id, approval, passphrase } => match backend.start_install(&id, approval.as_ref(), passphrase.as_ref()) {
            Ok(snapshot) => vec![Outcome::InstallProgress(Some(snapshot))],
            // As an outcome, so the install page that is already showing ends with the reason.
            Err(text) => {
                let mut snapshot = InstallSnapshot::starting(&id, "");
                snapshot.outcome = InstallOutcome::Failed(text);
                vec![Outcome::InstallProgress(Some(snapshot))]
            }
        },
        Effect::PollInstall => vec![Outcome::InstallProgress(backend.poll_install())],
        Effect::CancelInstall => vec![Outcome::InstallProgress(backend.cancel_install()), refresh(backend)],
        Effect::ProbeFeed(address) => vec![Outcome::FeedProbed(backend.probe_feed(&address))],
        Effect::TrustFeed(probe) => {
            let result = backend.trust_feed(&probe).map(|()| probe.shown().to_owned());
            vec![Outcome::FeedTrusted(result), refresh(backend)]
        }
        Effect::RefreshFeeds => {
            let result = backend.refresh_feeds();
            failed_then_refresh(backend, result)
        }
        Effect::ToggleFeed(name) => {
            let result = backend.toggle_feed(&name);
            failed_then_refresh(backend, result)
        }
        Effect::RemoveFeed(name) => {
            let result = backend.remove_feed(&name);
            failed_then_refresh(backend, result)
        }
        Effect::RenameFeed { name, label } => {
            let address = backend.feeds().into_iter().find(|feed| feed.name == name).map(|feed| feed.address.trim_start_matches("https://").to_owned());
            let result = backend.rename_feed(&name, label.as_deref()).map(|()| label.or(address).unwrap_or(name));
            vec![Outcome::FeedRenamed(result), refresh(backend)]
        }
        Effect::LoadKeyChange(name) => vec![Outcome::KeyChangeLoaded(backend.key_change(&name))],
        Effect::TrustNewKey { change, typed } => {
            let result = backend.trust_new_key(&change, &typed).map(|()| change.feed.clone());
            vec![Outcome::NewKeyTrusted(result), refresh(backend)]
        }
        Effect::OpenSettings(package) => {
            let form = backend.settings_form(&package);
            vec![Outcome::SettingsLoaded { package, form }]
        }
        Effect::LoadReferences { package } => {
            let list = match backend.call(Request::ListReferences { package: package.clone() }) {
                Ok(Response::References(options)) => crate::helper_backend::reference_list(options),
                _ => ReferenceList::default(),
            };
            vec![Outcome::ReferencesLoaded { package, list }]
        }
        Effect::PreviewSetting { package, field_index, key, value } => {
            // Secrets are never previewed, and a stored secret has no value to send.
            let Some(input) = value.to_input().filter(|input| !matches!(input, nodo_api::SettingInput::Secret(_))) else {
                return Vec::new();
            };
            let change = nodo_api::SettingChange { name: key, value: input };
            match backend.call(Request::PreviewSetting { package: package.clone(), change }) {
                Ok(Response::SettingPreview(preview)) => {
                    let preview = crate::helper_backend::field_preview(&value, preview);
                    vec![Outcome::SettingPreviewed { package, field_index, preview }]
                }
                _ => Vec::new(),
            }
        }
        Effect::Grant { consumer, provider, setting } => {
            let grant = nodo_api::SettingGrant { consumer, provider: provider.clone(), setting: setting.clone() };
            let result = plain_result(backend.call(Request::SetGrant { grant, approved: true }));
            vec![Outcome::Granted { provider, setting, result }]
        }
        Effect::ApplySettings { package, values } => {
            // Values with no wire form are stored secrets, and those never change here.
            let changes = values
                .into_iter()
                .filter_map(|(name, value)| {
                    let value = match value {
                        Some(value) => value.to_input()?,
                        None => nodo_api::SettingInput::Reset,
                    };
                    Some(nodo_api::SettingChange { name, value })
                })
                .collect();
            match backend.call(Request::ApplySettings { package: package.clone(), changes }) {
                Ok(Response::RolledBack(rollback)) => {
                    let explanation = backend.explain_rollback(&package, &rollback.reason);
                    vec![Outcome::SettingsRolledBack { package, explanation }, refresh(backend)]
                }
                result => vec![Outcome::SettingsApplied(plain_result(result)), refresh(backend)],
            }
        }
        Effect::CheckUpdates => {
            let result = backend.check_updates();
            // The snapshot first, so the result can say how many updates there are now.
            vec![refresh(backend), Outcome::UpdatesChecked(result)]
        }
        Effect::ReviewUpdate { name, batch, pending_reviews, approvals } => {
            vec![Outcome::UpdateReviewLoaded { review: backend.update_review(&name), batch, pending_reviews, approvals }]
        }
        Effect::ApplyUpdates { names, approvals } => {
            let report = backend.apply_updates(&names, &approvals);
            vec![Outcome::UpdatesApplied(report), refresh(backend)]
        }
        Effect::SetHeld { name, held } => {
            let result = backend.set_held(&name, held);
            failed_then_refresh(backend, result)
        }
        Effect::SaveAutoUpdate(prefs) => {
            vec![Outcome::AutoUpdateSaved(backend.save_auto_update(&prefs)), refresh(backend)]
        }
        Effect::LoadLogs(source) => {
            let view = backend.logs(&source);
            vec![Outcome::LogsLoaded { source, view }]
        }
        Effect::Drive(action) => vec![Outcome::DriveResult(backend.drive_action(action)), refresh(backend)],
        Effect::Shell => vec![Outcome::Info("A shell is not available from this build yet.".to_owned())],
        Effect::SetExposure { site, exposure, after, confirmation } => {
            let request = Request::SetExposure {
                site: backend.site_id(&site),
                exposure,
                confirmation: confirmation.map(|typed_host| PublicConfirmation { typed_host }),
            };
            let result = plain_result(backend.call(request));
            vec![Outcome::ExposureSet { site, after, result }, refresh(backend)]
        }
        Effect::SetPublicHost { site, host, aliases, confirm_added, then } => {
            let result = backend.set_public_host(&site, &host, &aliases, &confirm_added);
            vec![Outcome::PublicHostSet { site, result, then }, refresh(backend)]
        }
        Effect::SetPublicAccess { site, access } => match backend.set_public_access(&site, &access) {
            Ok(()) => Vec::new(),
            Err(text) => vec![Outcome::Failed(text)],
        },
        Effect::CheckReachability { site, going_public } => {
            let result = backend.reachability(&site);
            vec![Outcome::Reachability { site, going_public, result }]
        }
        Effect::AddProvider { provider, domain, api_key, secret_key } => {
            let result = backend.add_provider(&provider, domain.as_deref(), &api_key, &secret_key);
            vec![Outcome::ProviderAdded(result), refresh(backend)]
        }
        Effect::LoadProviders => vec![Outcome::ProvidersLoaded(backend.providers())],
        Effect::RemoveProvider(id) => vec![Outcome::ProviderRemoved(backend.remove_provider(&id)), refresh(backend)],
        Effect::RefreshProvider(id) => vec![Outcome::ProviderRefreshed(backend.refresh_provider(&id)), refresh(backend)],
        Effect::SaveAcme { settings, dns01 } => {
            // The credentials go first, since local names under the domain need DNS-01.
            let stored = match dns01 {
                Some((first, second)) => backend.save_dns01(&first, &second),
                None => Ok(()),
            };
            let result = stored.and_then(|()| backend.save_acme(&settings));
            vec![Outcome::AcmeSaved(result), refresh(backend)]
        }
        Effect::CheckLocalNames => vec![Outcome::LocalNamesChecked(backend.check_local_names())],
        Effect::ShowDocument(kind) => vec![Outcome::DocumentLoaded(backend.document(&kind))],
        Effect::CopyReport(subject) => match backend.copy_report(&subject) {
            Ok(text) => vec![Outcome::Info(text)],
            Err(text) => vec![Outcome::Failed(text)],
        },
        Effect::InitEncryption(passphrase) => {
            let request = Request::InitEncryption { passphrase: nodo_api::Secret::new(passphrase.expose()) };
            let result = plain_result(backend.call(request));
            vec![Outcome::SetupDone(result), refresh(backend)]
        }
        Effect::LoadSshKeys => vec![Outcome::SshKeysLoaded(backend.ssh_keys())],
        Effect::AddSshKey(line) => {
            let result = backend.add_ssh_key(&line);
            vec![Outcome::SshKeyChanged(result), Outcome::SshKeysLoaded(backend.ssh_keys())]
        }
        Effect::RemoveSshKey(fingerprint) => {
            let result = backend.remove_ssh_key(&fingerprint);
            vec![Outcome::SshKeyChanged(result), Outcome::SshKeysLoaded(backend.ssh_keys())]
        }
        Effect::LoadBackups => vec![Outcome::BackupsLoaded(backend.backups()), Outcome::UsbLoaded(load_usb(backend))],
        Effect::CreateBackup { device_passphrase, backup_passphrase, usb } => {
            let result = match usb {
                Some(device) => backend.create_usb_backup(&device, device_passphrase.as_ref(), &backup_passphrase),
                None => backend.create_backup(device_passphrase.as_ref(), &backup_passphrase),
            };
            vec![Outcome::BackupCreated(result)]
        }
        Effect::InspectBackup { name, passphrase, usb } => {
            let result = match usb {
                Some(device) => backend.inspect_usb_backup(&device, &name, &passphrase),
                None => backend.inspect_backup(&name, &passphrase),
            };
            vec![Outcome::BackupInspected(result)]
        }
        Effect::RestoreBackup { name, passphrase, new_device_passphrase, typed, usb } => {
            let result = match usb {
                Some(device) => {
                    backend.restore_usb_backup(&device, &name, &passphrase, new_device_passphrase.as_ref(), &typed)
                }
                None => backend.restore_backup(&name, &passphrase, new_device_passphrase.as_ref(), &typed),
            };
            vec![Outcome::BackupRestored(result), refresh(backend)]
        }
        Effect::ApplyUpdatesWithPassphrase { names, approvals, passphrase } => {
            let report = backend.apply_updates_with_passphrase(&names, &approvals, &passphrase);
            vec![Outcome::UpdatesApplied(report), refresh(backend)]
        }
        Effect::LoadOnion => vec![Outcome::OnionLoaded(onion_status(backend))],
        Effect::SetNodeOnion(enabled) => {
            let result = backend.call(Request::SetNodeOnion { enabled }).map(|_| ()).map_err(|e| e.to_string());
            vec![Outcome::OnionChanged(result), Outcome::OnionLoaded(onion_status(backend)), refresh(backend)]
        }
        Effect::SetSiteExposure { site, exposure } => {
            let request = Request::SetExposure { site, exposure, confirmation: None };
            let result = backend.call(request).map(|_| ()).map_err(|e| e.to_string());
            vec![Outcome::OnionChanged(result), Outcome::OnionLoaded(onion_status(backend)), refresh(backend)]
        }
        Effect::LoadRestoreDrives => vec![Outcome::RestoreDrives(backend.usb_drives())],
        Effect::LoadRestoreBackups(device) => vec![Outcome::RestoreBackups(backend.usb_backups(&device))],
        Effect::InspectRestore { device, name, passphrase } => {
            vec![Outcome::RestoreInspected(backend.inspect_usb_backup(&device, &name, &passphrase))]
        }
        Effect::RunRestore { device, name, passphrase, new_device_passphrase, typed } => {
            let result =
                backend.restore_usb_backup(&device, &name, &passphrase, Some(&new_device_passphrase), &typed);
            vec![Outcome::RestoreFinished(result), refresh(backend)]
        }
        Effect::LoadRollbacks => vec![Outcome::RollbacksLoaded(rollback_view(backend))],
        Effect::RollBack { package, to } => {
            let result = match backend.call(Request::RollbackPackage { package, to }) {
                Ok(Response::UpdateResults(mut results)) if !results.is_empty() => Ok(results.remove(0)),
                Ok(other) => Err(format!("unexpected reply: {other:?}")),
                Err(e) => Err(e.to_string()),
            };
            vec![Outcome::RolledBack(result)]
        }
        Effect::LoadNetwork => {
            vec![Outcome::NetworkLoaded { wifi: wifi_status(backend), bluetooth: bluetooth_status(backend) }]
        }
        Effect::PollWifi => vec![Outcome::WifiChanged { result: wifi_status(backend), quiet: true }],
        Effect::SetWifi(enabled) => vec![wifi_changed(backend.call(Request::SetWifiEnabled { enabled }))],
        Effect::ScanWifi => {
            let result = match backend.call(Request::ScanWifi) {
                Ok(Response::WifiNetworks(list)) => Ok(list),
                Ok(other) => Err(format!("unexpected reply: {other:?}")),
                Err(e) => Err(e.to_string()),
            };
            vec![Outcome::WifiScanned(result), Outcome::WifiChanged { result: wifi_status(backend), quiet: true }]
        }
        Effect::JoinWifi { ssid, security, passphrase, accept_open } => {
            let join = nodo_api::WifiJoin {
                ssid,
                security,
                passphrase: passphrase.map(|p| nodo_api::Secret::new(p.expose())),
                accept_open,
            };
            vec![wifi_changed(backend.call(Request::JoinWifi(join)))]
        }
        Effect::ForgetWifi(ssid) => vec![wifi_changed(backend.call(Request::ForgetWifi { ssid }))],
        Effect::SetBluetooth(enabled) => vec![bluetooth_changed(backend.call(Request::SetBluetoothEnabled { enabled }))],
        Effect::ScanBluetooth => vec![bluetooth_changed(backend.call(Request::ScanBluetooth))],
        Effect::PairBluetooth(address) => vec![bluetooth_changed(backend.call(Request::PairBluetooth { address }))],
        Effect::PollBluetooth => vec![Outcome::BluetoothChanged { result: bluetooth_status(backend), quiet: true }],
        Effect::ConnectBluetooth { address, connect } => {
            vec![bluetooth_changed(backend.call(Request::ConnectBluetooth { address, connect }))]
        }
        Effect::RemoveBluetooth(address) => vec![bluetooth_changed(backend.call(Request::RemoveBluetooth { address }))],
    }
}

fn wifi_status<T: Backend + ScreenData + ?Sized>(backend: &mut T) -> Result<nodo_api::WifiStatus, String> {
    match backend.call(Request::WifiStatus) {
        Ok(Response::Wifi(status)) => Ok(status),
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

fn bluetooth_status<T: Backend + ScreenData + ?Sized>(backend: &mut T) -> Result<nodo_api::BluetoothStatus, String> {
    match backend.call(Request::BluetoothStatus) {
        Ok(Response::Bluetooth(status)) => Ok(status),
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

fn wifi_changed(result: Result<Response, nodo_api::ApiError>) -> Outcome {
    let result = match result {
        Ok(Response::Wifi(status)) => Ok(status),
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(e.to_string()),
    };
    Outcome::WifiChanged { result, quiet: false }
}

fn bluetooth_changed(result: Result<Response, nodo_api::ApiError>) -> Outcome {
    let result = match result {
        Ok(Response::Bluetooth(status)) => Ok(status),
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(e.to_string()),
    };
    Outcome::BluetoothChanged { result, quiet: false }
}

fn onion_status<T: Backend + ScreenData + ?Sized>(backend: &mut T) -> Result<nodo_api::OnionStatus, String> {
    match backend.call(Request::OnionStatus) {
        Ok(Response::Onion(status)) => Ok(status),
        Ok(other) => Err(format!("unexpected reply: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Versions that can be put back, failed updates that were rolled back, and a system fallback.
/// Only the first is needed; the others are left out when the helper cannot say.
fn rollback_view<T: Backend + ScreenData + ?Sized>(backend: &mut T) -> Result<RollbackView, String> {
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
    Ok(RollbackView { options, events, boot_fallback })
}

/// The first usable USB drive with its backups, or the first drive and why it cannot be used.
fn load_usb<T: Backend + ScreenData + ?Sized>(backend: &mut T) -> Result<UsbView, String> {
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

/// The failure, if any, as a notice, and then the device state again either way.
fn failed_then_refresh<T: Backend + ScreenData + ?Sized>(backend: &mut T, result: Result<(), String>) -> Vec<Outcome> {
    let mut events: Vec<Outcome> = result.err().map(Outcome::Failed).into_iter().collect();
    events.push(refresh(backend));
    events
}

fn service_command<T: Backend + ScreenData + ?Sized>(backend: &mut T, request: Request) -> Vec<Outcome> {
    let mut events = Vec::new();
    if let Err(error) = backend.call(request) {
        events.push(Outcome::Failed(error.to_string()));
    }
    events.push(refresh(backend));
    events
}

/// Performs one batch of effects in order, handing each effect's outcomes to `deliver`.
pub fn run_batch<T: Backend + ScreenData + ?Sized>(backend: &mut T, batch: Vec<Effect>, mut deliver: impl FnMut(Vec<Outcome>)) {
    let mut effects = VecDeque::from(batch);
    while let Some(effect) = effects.pop_front() {
        let outcomes = execute_effect(backend, effect);
        // A step towards public never runs after its prerequisite failed. Other batches, such
        // as taking every site offline, carry on so one failure does not leave the rest up.
        if outcomes.iter().any(reports_failure) && effects.front().is_some_and(moves_towards_public) {
            effects.clear();
        }
        deliver(outcomes);
    }
}

fn reports_failure(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Failed(_) | Outcome::ExposureSet { result: Err(_), .. })
}

fn moves_towards_public(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::CheckReachability { going_public: true, .. }
            | Effect::SetExposure { after: ExposureAfter::WentPublic, .. }
    )
}

enum ToWorker {
    Batch(Vec<Effect>),
    Call(Request),
}

enum FromWorker {
    Outcomes(Vec<Outcome>),
    /// The batch is over, with the refusals for want of an unlock that the helper gave during it.
    BatchDone(Vec<ApiError>),
    Called(Result<Response, ApiError>),
}

/// One thing the worker finished, for an interface that takes a batch's outcomes together
/// ([`EffectWorker::finished`]). They come in the order the work was handed in.
#[derive(Debug, Clone, PartialEq)]
pub enum Finished {
    /// A batch of effects: every outcome, in order, and each different `ApiError::Locked` the
    /// helper answered while it ran (`ScreenData::take_refusals`). An interface that can lift
    /// what was locked keeps the effects, and submits them again once it has.
    Batch { outcomes: Vec<Outcome>, refusals: Vec<ApiError> },
    /// The answer to a request made with [`EffectWorker::call`].
    Reply(Result<Response, ApiError>),
}

/// Owns the backend on its own thread, so a helper call that takes minutes never stops the
/// screen from drawing or keys from being read.
///
/// Results are read one of two ways, and an interface keeps to one: [`EffectWorker::ready`] and
/// [`EffectWorker::wait_idle`] give outcomes as each effect ends, and
/// [`EffectWorker::finished`] and [`EffectWorker::wait_finished`] give each batch whole, with
/// the refusals it met.
pub struct EffectWorker {
    work: Sender<ToWorker>,
    results: Receiver<FromWorker>,
    in_flight: usize,
    /// Outcomes of the batch under way, until it is over, for `finished`.
    partial: Vec<Outcome>,
}

impl EffectWorker {
    /// Takes the backend boxed, so one behind `dyn Data` works as well as a named type.
    pub fn spawn<T: Backend + ScreenData + Send + ?Sized + 'static>(mut backend: Box<T>) -> Self {
        let (work, inbox) = mpsc::channel::<ToWorker>();
        let (outbox, results) = mpsc::channel();
        thread::spawn(move || {
            for job in inbox {
                let done = match job {
                    ToWorker::Batch(batch) => {
                        // A refusal from before this batch is not this batch's.
                        drop(backend.take_refusals());
                        run_batch(&mut *backend, batch, |events| {
                            // Fails only once the screen has gone, and the send below then ends the thread.
                            #[allow(clippy::let_underscore_must_use)]
                            let _ = outbox.send(FromWorker::Outcomes(events));
                        });
                        FromWorker::BatchDone(backend.take_refusals())
                    }
                    ToWorker::Call(request) => FromWorker::Called(backend.call(request)),
                };
                if outbox.send(done).is_err() {
                    return;
                }
            }
        });
        Self { work, results, in_flight: 0, partial: Vec::new() }
    }

    pub fn busy(&self) -> bool {
        self.in_flight > 0
    }

    pub fn submit(&mut self, effects: Vec<Effect>) {
        if !effects.is_empty() && self.work.send(ToWorker::Batch(effects)).is_ok() {
            self.in_flight += 1;
        }
    }

    /// Makes one request that is no effect, in its turn after the work already handed in: an
    /// interface's own, such as the built-in screen's status, unlock and lock. Its answer comes
    /// through [`EffectWorker::finished`].
    pub fn call(&mut self, request: Request) {
        if self.work.send(ToWorker::Call(request)).is_ok() {
            self.in_flight += 1;
        }
    }

    fn take(&mut self, message: FromWorker, events: &mut Vec<Outcome>) {
        match message {
            FromWorker::Outcomes(more) => events.extend(more),
            FromWorker::BatchDone(_) | FromWorker::Called(_) => self.in_flight = self.in_flight.saturating_sub(1),
        }
    }

    /// Outcomes that are ready now, without waiting.
    pub fn ready(&mut self) -> Vec<Outcome> {
        let mut events = Vec::new();
        while let Ok(message) = self.results.try_recv() {
            self.take(message, &mut events);
        }
        events
    }

    /// Waits up to `timeout` for the worker to go idle, returning what arrived meanwhile.
    pub fn wait_idle(&mut self, timeout: Duration) -> Vec<Outcome> {
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

    fn take_whole(&mut self, message: FromWorker, done: &mut Vec<Finished>) {
        match message {
            FromWorker::Outcomes(more) => self.partial.extend(more),
            FromWorker::BatchDone(refusals) => {
                self.in_flight = self.in_flight.saturating_sub(1);
                done.push(Finished::Batch { outcomes: std::mem::take(&mut self.partial), refusals });
            }
            FromWorker::Called(reply) => {
                self.in_flight = self.in_flight.saturating_sub(1);
                done.push(Finished::Reply(reply));
            }
        }
    }

    /// The batches and requests that are over now, without waiting.
    pub fn finished(&mut self) -> Vec<Finished> {
        let mut done = Vec::new();
        while let Ok(message) = self.results.try_recv() {
            self.take_whole(message, &mut done);
        }
        done
    }

    /// Waits up to `timeout` for the next batch or request to be over, and returns it with
    /// whatever else is over by then. Empty when nothing is under way or the time ran out.
    pub fn wait_finished(&mut self, timeout: Duration) -> Vec<Finished> {
        let deadline = Instant::now() + timeout;
        let mut done = Vec::new();
        while self.busy() && done.is_empty() {
            match self.results.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(message) => self.take_whole(message, &mut done),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        done.extend(self.finished());
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;

    #[test]
    fn a_whole_batch_comes_with_the_refusals_it_met_and_a_request_with_its_answer() {
        let mut mock = MockBackend::new();
        mock.knobs.screen_pin = Some("123456".to_owned());
        let mut worker = EffectWorker::spawn(Box::new(mock));
        let stop = vec![Effect::Stop("moneropay".to_owned())];

        // A change on the locked screen: its failure in words, and the refusal as it was typed.
        worker.submit(stop.clone());
        let done = worker.wait_finished(Duration::from_secs(5));
        let [Finished::Batch { outcomes, refusals }] = done.as_slice() else { panic!("one batch: {done:?}") };
        assert!(matches!(outcomes.as_slice(), [Outcome::Failed(text), Outcome::Refreshed(_)] if text.contains("screen PIN")));
        assert_eq!(refusals, &[ApiError::Locked("enter the screen PIN to make changes".to_owned())]);

        // Something that only reads meets none, and an earlier refusal is not carried over.
        worker.submit(vec![Effect::Refresh]);
        let done = worker.wait_finished(Duration::from_secs(5));
        assert!(matches!(done.as_slice(), [Finished::Batch { refusals, .. }] if refusals.is_empty()));

        // The screen's own request, then the same change again.
        worker.call(Request::ScreenUnlock { pin: nodo_api::Secret::new("123456") });
        worker.submit(stop);
        let mut done = Vec::new();
        while worker.busy() {
            done.extend(worker.wait_finished(Duration::from_secs(5)));
        }
        let [Finished::Reply(Ok(Response::Screen(status))), Finished::Batch { outcomes, refusals }] = done.as_slice() else {
            panic!("the answer, then the batch: {done:?}")
        };
        assert!(status.unlocked && refusals.is_empty());
        assert!(matches!(outcomes.as_slice(), [Outcome::Refreshed(_)]));
    }

    #[test]
    fn outcomes_as_they_come_are_unchanged_by_requests_and_refusals() {
        let mut mock = MockBackend::new();
        mock.knobs.screen_pin = Some("123456".to_owned());
        let mut worker = EffectWorker::spawn(Box::new(mock));
        worker.call(Request::ScreenStatus);
        worker.submit(vec![Effect::Stop("moneropay".to_owned())]);
        let outcomes = worker.wait_idle(Duration::from_secs(5));
        assert!(!worker.busy());
        assert!(matches!(outcomes.as_slice(), [Outcome::Failed(_), Outcome::Refreshed(_)]));
    }

    #[test]
    fn a_backend_behind_dyn_reads_the_same_snapshot_and_runs_on_the_worker() {
        let named = load_snapshot(&mut MockBackend::new()).expect("the mock answers");
        let mut boxed: Box<dyn Data> = Box::new(MockBackend::new());
        assert_eq!(load_snapshot(&mut *boxed).expect("the mock answers"), named);
        assert_eq!(execute_effect(&mut *boxed, Effect::Refresh), vec![Outcome::Refreshed(Box::new(named.clone()))]);

        let mut worker = EffectWorker::spawn(boxed);
        worker.submit(vec![Effect::Refresh]);
        assert!(worker.busy());
        let outcomes = worker.wait_idle(Duration::from_secs(5));
        assert!(!worker.busy());
        assert_eq!(outcomes, vec![Outcome::Refreshed(Box::new(named))]);
    }
}
