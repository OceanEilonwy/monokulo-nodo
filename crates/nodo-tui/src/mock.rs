//! A stateful in-memory device with the design's sample data, for `--mock` and for tests.

use std::collections::{BTreeMap, HashMap};

use nodo_api::{
    ApiError, Backend, Exposure, Origin, Request, Response, Rollback, RollbackStage, ServiceState, ServiceStatus,
    SettingInput, SettingLiteral,
};

use crate::model::*;
use crate::secret::Secret;

/// The passphrase the mock device accepts. It is sample data and protects nothing.
pub const MOCK_PASSPHRASE: &str = "monokulo";

/// A port the mock's monero-daemon occupies, used to demonstrate a settings rollback.
const CLASHING_PORT: &str = "18081";

/// One install step every this many polls, slow enough to watch at the runtime's tick rate.
const POLLS_PER_STEP: u32 = 4;

fn s(text: &str) -> String {
    text.to_owned()
}

fn service(id: &str, state: ServiceState, detail: &str) -> ServiceStatus {
    let origin = if id == "cuprated" { Origin::System } else { Origin::Feed(Some(s("community"))) };
    ServiceStatus { id: s(id), state, detail: s(detail), roles: Vec::new(), origin }
}

fn need(name: &str, source: Option<&str>) -> Need {
    Need { name: s(name), met: true, source: source.map(s) }
}

pub fn sample_address() -> String {
    let mut address = s("48edfHu7V9Z84Y");
    address.extend(std::iter::repeat_n('x', 95 - address.len()));
    address
}

struct InstallJob {
    snapshot: InstallSnapshot,
    current: usize,
    polls: u32,
    fails_at_end: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProvider {
    pub provider: String,
    pub domain: String,
}

pub struct MockBackend {
    /// Every `Backend` request received, so tests can assert what would reach the helper.
    pub calls: Vec<Request>,
    pub services: Vec<ServiceStatus>,
    pub catalog: Vec<PackageSummary>,
    pub feeds: Vec<Feed>,
    pub updates: Vec<UpdateItem>,
    pub web: WebOverview,
    pub drive: Option<DriveFailure>,
    pub forms: HashMap<String, SettingsForm>,
    pub public_access: BTreeMap<String, PublicAccess>,
    pub stored_provider: Option<StoredProvider>,
    pub auto_update: AutoUpdatePrefs,
    /// Makes applying a site's protections fail, to exercise that failure path.
    pub fail_public_access: bool,
    /// First boot: no device passphrase yet.
    pub setup_needed: bool,
    pub ssh_keys: Vec<nodo_api::SshKey>,
    /// Makes every helper call this slow, to show the screen stays responsive meanwhile.
    pub delay: Option<std::time::Duration>,
    /// The node's onion service.
    pub node_onion: nodo_api::NodeOnion,
    pub rollbacks: Vec<nodo_api::RollbackOption>,
    /// Update events, including failed updates that were rolled back.
    pub update_history: Vec<nodo_api::UpdateEvent>,
    /// Makes a USB restore fail with this reason, as a pulled drive would.
    pub restore_failure: Option<String>,
    reach_attempts: HashMap<String, u32>,
    job: Option<InstallJob>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            calls: Vec::new(),
            services: sample_services(),
            catalog: sample_catalog(),
            feeds: sample_feeds(),
            updates: sample_updates(),
            web: sample_web(),
            drive: None,
            forms: sample_forms(),
            public_access: BTreeMap::new(),
            stored_provider: Some(StoredProvider { provider: s("Porkbun"), domain: s("example.com") }),
            auto_update: sample_auto_update(),
            fail_public_access: false,
            setup_needed: false,
            delay: None,
            ssh_keys: vec![nodo_api::SshKey {
                algorithm: s("ssh-ed25519"),
                fingerprint: s("SHA256:mockmockmockmockmockmockmockmockmockmockmo"),
                comment: s("laptop"),
            }],
            node_onion: nodo_api::NodeOnion {
                provider: s("cuprated"),
                enabled: true,
                restricted_rpc: true,
                port: 18081,
                address: Some(s("nod…xyz.onion:18081")),
            },
            rollbacks: vec![
                nodo_api::RollbackOption {
                    package: s("moneropay"),
                    current: s("2.7.0_1"),
                    previous: s("2.6.1_1"),
                    blocked: None,
                },
                nodo_api::RollbackOption {
                    package: s("wallet-relay"),
                    current: s("0.4.0_1"),
                    previous: s("0.3.2_1"),
                    blocked: Some(s("its data is in layout 2")),
                },
            ],
            update_history: vec![nodo_api::UpdateEvent {
                at: 1_794_009_600,
                package: s("bell"),
                from: s("1.2.0_1"),
                to: s("1.3.0_1"),
                kind: nodo_api::UpdateEventKind::RolledBack { reason: s("bell did not pass its health check") },
            }],
            restore_failure: None,
            reach_attempts: HashMap::new(),
            job: None,
        }
    }

    /// Onion exposure as the helper would report it, from the sites' saved exposure.
    pub fn onion_status(&self) -> nodo_api::OnionStatus {
        let sites = self
            .web
            .sites
            .iter()
            .map(|site| nodo_api::SiteOnion {
                id: nodo_api::SiteId { package: site.package.clone(), site: s("web") },
                exposure: site.exposure,
                // moneropay is tied to one base URL in the sample data.
                multi_address: site.package != "moneropay",
                locked: site.locked,
                url: site.exposure.onion.then(|| site.onion_url.clone()),
            })
            .collect();
        nodo_api::OnionStatus { tor_installed: true, tor_running: true, node: Some(self.node_onion.clone()), sites }
    }

    /// The data drive scenario: the node cannot start and everything else waits for it.
    pub fn with_missing_drive() -> Self {
        let mut mock = Self::new();
        mock.drive = Some(sample_drive_failure());
        for svc in &mut mock.services {
            svc.state = if svc.id == "cuprated" { ServiceState::Stopped } else { ServiceState::Waiting };
            svc.detail = s("waiting for the data drive");
        }
        mock
    }

    fn finish_unlock(&mut self) {
        for svc in &mut self.services {
            if svc.state == ServiceState::Locked || svc.id == "bell" {
                svc.state = ServiceState::Running;
                svc.detail.clear();
            }
        }
        for site in &mut self.web.sites {
            site.locked = false;
        }
    }

    fn is_locked(&self) -> bool {
        self.services.iter().any(|svc| svc.state == ServiceState::Locked)
    }
}

impl Backend for MockBackend {
    fn call(&mut self, request: Request) -> Result<Response, ApiError> {
        if let Some(delay) = self.delay {
            std::thread::sleep(delay);
        }
        self.calls.push(request.clone());
        match request {
            Request::ListServices => Ok(Response::Services(self.services.clone())),
            Request::StartService { id } | Request::RestartService { id } => {
                let svc = self
                    .services
                    .iter_mut()
                    .find(|svc| svc.id == id)
                    .ok_or_else(|| ApiError::NotFound(id.clone()))?;
                if svc.state == ServiceState::Locked {
                    return Err(ApiError::Failed(format!("{id} is waiting for the device passphrase")));
                }
                svc.state = ServiceState::Running;
                svc.detail.clear();
                Ok(Response::Ok)
            }
            Request::StopService { id } => {
                let svc = self
                    .services
                    .iter_mut()
                    .find(|svc| svc.id == id)
                    .ok_or_else(|| ApiError::NotFound(id.clone()))?;
                svc.state = ServiceState::Stopped;
                svc.detail.clear();
                Ok(Response::Ok)
            }
            Request::Unlock { passphrase } => {
                if passphrase.expose() == MOCK_PASSPHRASE {
                    self.finish_unlock();
                    Ok(Response::Ok)
                } else {
                    Err(ApiError::BadPassphrase)
                }
            }
            Request::ApplySettings { package, changes } => {
                let form =
                    self.forms.get_mut(&package).ok_or_else(|| ApiError::NotFound(package.clone()))?;
                let clash = SettingInput::Literal(SettingLiteral::Text(s(CLASHING_PORT)));
                if changes.iter().any(|change| change.name == "api_port" && change.value == clash) {
                    return Ok(Response::RolledBack(Rollback {
                        stage: RollbackStage::HealthCheck,
                        reason: format!("listen tcp :{CLASHING_PORT}: bind: address already in use"),
                        recovered: true,
                        recovery_problem: None,
                    }));
                }
                for change in changes {
                    let Some(field) = form.fields.iter_mut().find(|field| field.key == change.name) else {
                        return Err(ApiError::Failed(format!("unknown setting: {}", change.name)));
                    };
                    field.value = match change.value {
                        SettingInput::Secret(_) => FieldValue::SecretSet,
                        SettingInput::Literal(SettingLiteral::Bool(value)) => FieldValue::Bool(value),
                        SettingInput::Literal(SettingLiteral::Int(value)) => FieldValue::Text(value.to_string()),
                        SettingInput::Literal(SettingLiteral::Text(value)) => FieldValue::Text(value),
                        SettingInput::Reference { target, setting } => {
                            let path = format!("{target}.{setting}");
                            let resolved = sample_references()
                                .options
                                .iter()
                                .find(|option| option.path == path)
                                .map_or_else(String::new, |option| option.value.clone());
                            FieldValue::Reference { path, resolved }
                        }
                        SettingInput::Reset => field.value.clone(),
                    };
                }
                Ok(Response::Ok)
            }
            Request::SetExposure { site, exposure, confirmation } => {
                let entry = self
                    .web
                    .sites
                    .iter_mut()
                    .find(|entry| entry.package == site.site || (site.site == "web" && entry.package == site.package))
                    .ok_or_else(|| ApiError::NotFound(format!("{}/{}", site.package, site.site)))?;
                // The helper's rule, so flows that skip the typed host fail here as they would there.
                if exposure.public && !entry.exposure.public {
                    let host = entry.public_host.clone().unwrap_or_default();
                    if confirmation.map(|c| c.typed_host) != Some(host.clone()) {
                        return Err(ApiError::NeedsConfirmation(format!("type {host} exactly to confirm")));
                    }
                }
                entry.exposure = exposure;
                Ok(Response::Ok)
            }
            Request::InitEncryption { passphrase } => {
                if !self.setup_needed {
                    return Err(ApiError::Failed(s("this device is already set up")));
                }
                if passphrase.char_count() < 8 {
                    return Err(ApiError::Failed(s("the passphrase must have at least 8 characters")));
                }
                self.setup_needed = false;
                Ok(Response::Ok)
            }
            Request::OnionStatus => Ok(Response::Onion(self.onion_status())),
            Request::SetNodeOnion { enabled } => {
                if enabled && !self.node_onion.restricted_rpc {
                    return Err(ApiError::Failed(s("cuprated does not say which port serves its restricted RPC")));
                }
                self.node_onion.enabled = enabled;
                self.node_onion.address = enabled.then(|| s("nod…xyz.onion:18081"));
                Ok(Response::Ok)
            }
            Request::ListRollbacks => Ok(Response::Rollbacks(self.rollbacks.clone())),
            Request::UpdateStatus => Ok(Response::UpdateStatus(nodo_api::UpdateStatus {
                candidates: Vec::new(),
                last_check: None,
                attention: Vec::new(),
                history: self.update_history.clone(),
                next_window: None,
            })),
            Request::RollbackPackage { package, to } => {
                let index = self
                    .rollbacks
                    .iter()
                    .position(|r| r.package == package)
                    .ok_or_else(|| ApiError::Failed(format!("{package} has no earlier version kept")))?;
                let option = self.rollbacks[index].clone();
                if option.previous != to {
                    return Err(ApiError::NeedsConfirmation(format!("{package} would go back to {}", option.previous)));
                }
                if let Some(reason) = option.blocked {
                    return Err(ApiError::Failed(format!("{package} cannot roll back now: {reason}")));
                }
                self.rollbacks.remove(index);
                Ok(Response::UpdateResults(vec![nodo_api::UpdateResult {
                    package,
                    from: option.current,
                    to: option.previous,
                    outcome: nodo_api::UpdateOutcome::Updated,
                    log: Vec::new(),
                }]))
            }
            other => Err(ApiError::NotImplemented(format!("{} in the mock", nodo_api::action_name(&other)))),
        }
    }
}

impl ScreenData for MockBackend {
    fn backups(&self) -> Result<Vec<nodo_api::BackupInfo>, String> {
        Ok(vec![
            nodo_api::BackupInfo { name: s("nodo-1762560000.nodobak"), size_bytes: 4_812_300, created: 1_762_560_000 },
            nodo_api::BackupInfo { name: s("nodo-1761955200.nodobak"), size_bytes: 4_501_100, created: 1_761_955_200 },
        ])
    }

    fn create_backup(
        &mut self,
        _device_passphrase: Option<&Secret>,
        backup_passphrase: &Secret,
    ) -> Result<nodo_api::BackupCreated, String> {
        if backup_passphrase.expose().chars().count() < 12 {
            return Err(s("a backup passphrase needs at least 12 characters"));
        }
        let info = nodo_api::BackupInfo { name: s("nodo-1762646400.nodobak"), size_bytes: 4_900_000, created: 1_762_646_400 };
        Ok(nodo_api::BackupCreated { info, entries: 42, has_device_key: true, missing: Vec::new(), locked: Vec::new() })
    }

    fn inspect_backup(&mut self, name: &str, passphrase: &Secret) -> Result<nodo_api::BackupContents, String> {
        if passphrase.expose() != MOCK_PASSPHRASE {
            return Err(s("the passphrase did not open this backup, or its start is damaged"));
        }
        Ok(nodo_api::BackupContents {
            name: name.to_owned(),
            entries: 42,
            has_device_key: true,
            settings: vec![s("cuprated"), s("moneropay")],
            storage: vec![s("wallet-relay")],
            onion_keys: true,
            services_to_stop: vec![s("moneropay"), s("wallet-relay"), s("tor")],
        })
    }

    fn restore_backup(
        &mut self,
        _name: &str,
        _passphrase: &Secret,
        _new_device_passphrase: Option<&Secret>,
        typed: &str,
    ) -> Result<nodo_api::RestoreReport, String> {
        if typed != nodo_api::RESTORE_WORD {
            return Err(s("type restore to confirm"));
        }
        Ok(nodo_api::RestoreReport {
            restored: vec![s("settings"), s("moneropay settings"), s("wallet-relay storage"), s("Tor onion keys")],
            skipped: vec![s("device key: this device's encrypted storage uses its own key, which was kept")],
            stopped: vec![s("moneropay"), s("wallet-relay"), s("tor")],
            restarted: vec![s("moneropay"), s("wallet-relay"), s("tor")],
        })
    }

    fn usb_drives(&self) -> Result<Vec<nodo_api::UsbDrive>, String> {
        Ok(vec![nodo_api::UsbDrive {
            device: s("sda1"),
            label: Some(s("BACKUP")),
            model: s("SanDisk Ultra Fit"),
            filesystem: Some(s("vfat")),
            size_bytes: 32_008_000_000,
            read_only: false,
            problem: None,
        }])
    }

    fn usb_backups(&self, device: &str) -> Result<nodo_api::UsbBackupList, String> {
        Ok(nodo_api::UsbBackupList {
            device: device.to_owned(),
            backups: vec![nodo_api::BackupInfo {
                name: s("nodo-1762473600.nodobak"),
                size_bytes: 4_655_200,
                created: 1_762_473_600,
            }],
            free_bytes: 30_112_000_000,
            read_only: false,
        })
    }

    fn create_usb_backup(
        &mut self,
        _device: &str,
        device_passphrase: Option<&Secret>,
        backup_passphrase: &Secret,
    ) -> Result<nodo_api::BackupCreated, String> {
        self.create_backup(device_passphrase, backup_passphrase)
    }

    fn inspect_usb_backup(
        &mut self,
        _device: &str,
        name: &str,
        passphrase: &Secret,
    ) -> Result<nodo_api::BackupContents, String> {
        self.inspect_backup(name, passphrase)
    }

    fn restore_usb_backup(
        &mut self,
        _device: &str,
        name: &str,
        passphrase: &Secret,
        new_device_passphrase: Option<&Secret>,
        typed: &str,
    ) -> Result<nodo_api::RestoreReport, String> {
        if let Some(reason) = &self.restore_failure {
            return Err(reason.clone());
        }
        let report = self.restore_backup(name, passphrase, new_device_passphrase, typed)?;
        self.setup_needed = false;
        Ok(report)
    }

    fn overview(&self) -> Overview {
        Overview {
            node: NodeInfo {
                role: s("monero-daemon"),
                provider: s("cuprated 0.1.0-preview"),
                state: s("syncing 61%"),
                height: s("2,121,400 of 3,478,000"),
                peers: s("32 out, 9 in"),
            },
            device: vec![
                (s("SSD"), s("412 GB of 1.9 TB")),
                (s("System"), s("0.4 GB of 7.6 GB")),
                (s("Memory"), s("6.1 GB of 31 GB")),
                (s("Temp"), s("52 °C")),
                (s("Uptime"), s("3 days")),
            ],
            service_origins: vec![
                (s("cuprated"), s("monero-daemon")),
                (s("tor"), s("system")),
                (s("moneropay"), s("official")),
                (s("wallet-relay"), s("community")),
                (s("bell"), s("community")),
                (s("monerod"), s("official")),
            ],
            drive: self.drive.clone(),
            attention: Vec::new(),
            restarted: s("Restarted 4 minutes ago"),
            unlock_targets: if self.is_locked() {
                vec![
                    UnlockTarget { name: s("monokulo-pay"), encrypted: true, note: s("Encrypted storage") },
                    UnlockTarget { name: s("wallet-relay"), encrypted: true, note: s("Encrypted storage") },
                    UnlockTarget { name: s("bell"), encrypted: false, note: s("Starts after wallet-relay") },
                ]
            } else {
                Vec::new()
            },
            updates_checked: s("Last checked 2 hours ago"),
            setup_needed: self.setup_needed,
        }
    }

    fn catalog(&self) -> Vec<PackageSummary> {
        self.catalog.clone()
    }

    fn package_detail(&self, id: &PackageId) -> Option<PackageDetail> {
        let summary = self.catalog.iter().find(|pkg| &pkg.id == id)?;
        let name = &summary.id.name;
        let (about, license, public_settings, encrypted) = if name == "moneropay" {
            (
                s("Payment gateway for Monero. Creates invoices and reports payments through a local API."),
                s("MIT"),
                vec![s("moneropay.api_url")],
                true,
            )
        } else {
            (summary.description.join(" "), s("MIT"), Vec::new(), matches!(name.as_str(), "wallet-relay" | "monokulo-pay"))
        };
        Some(PackageDetail {
            id: id.clone(),
            installed: summary.status != PackageStatus::Available,
            signature_verified: true,
            about,
            version: summary.version.clone(),
            size: summary.size.clone(),
            license,
            needs: summary.needs.clone(),
            public_settings,
            asks: vec![
                (s("Runs as"), format!("its own user, _{name}")),
                (s("Sandbox"), s("Only its own data and declared paths")),
                (s("Local network"), s("Reach daemon RPC on this device")),
                (s("Incoming ports"), s("5000, local network only")),
                (s("Internet"), s("None")),
            ],
            encrypted_storage: encrypted,
        })
    }

    fn install_gate(&self, id: &PackageId) -> InstallGate {
        let version = self
            .catalog
            .iter()
            .find(|pkg| &pkg.id == id)
            .map_or_else(|| s("0.0.0"), |pkg| pkg.version.clone());
        match id.name.as_str() {
            "fast-sync-helper" => InstallGate::Rejected(RejectionData {
                id: id.clone(),
                version,
                violations: vec![
                    Violation { passed: false, rule: s("Runs a setup script as root"), evidence: s("./INSTALL") },
                    Violation {
                        passed: false,
                        rule: s("Writes outside its own folder"),
                        evidence: s("/etc/cron.d/fast-sync-helper"),
                    },
                    Violation { passed: false, rule: s("Tries to replace a system package"), evidence: s("glibc 2.42") },
                    Violation {
                        passed: true,
                        rule: s("Signature matches the feed's key"),
                        evidence: s("A valid signature shows who published it, not that it is safe."),
                    },
                ],
            }),
            "zmq-indexer" => InstallGate::MissingCapability(CapabilityData {
                id: id.clone(),
                version,
                message: s("zmq-indexer needs ZMQ. Your current node, cuprated, does not provide it."),
                current_node: s("cuprated"),
                alternative_node: s("monerod"),
                matrix: vec![
                    (s("daemon-rpc"), true, true),
                    (s("zmq"), false, true),
                    (s("i2p"), false, true),
                    (s("Synced on this device"), true, false),
                ],
                compatible_note: s("none found in your feeds"),
                switch_note: s("about 300 GB more and a full sync"),
            }),
            _ => InstallGate::Review(sample_review(id, &version)),
        }
    }

    fn start_install(
        &mut self,
        id: &PackageId,
        _approval: Option<&nodo_api::PermissionApproval>,
        _passphrase: Option<&Secret>,
    ) -> Result<InstallSnapshot, String> {
        let version = self
            .catalog
            .iter()
            .find(|pkg| &pkg.id == id)
            .map(|pkg| pkg.version.clone())
            .ok_or_else(|| format!("unknown package {}", id.name))?;
        let snapshot = install_snapshot(id, &version, 0);
        self.job = Some(InstallJob {
            snapshot: snapshot.clone(),
            current: 0,
            polls: 0,
            fails_at_end: id.name == "paywall-relay",
        });
        Ok(snapshot)
    }

    fn poll_install(&mut self) -> Option<InstallSnapshot> {
        let job = self.job.as_mut()?;
        if job.snapshot.outcome != InstallOutcome::InProgress {
            return Some(job.snapshot.clone());
        }
        job.polls += 1;
        if job.polls < POLLS_PER_STEP {
            return Some(job.snapshot.clone());
        }
        job.polls = 0;
        job.current += 1;
        let total = job.snapshot.steps.len();
        let id = job.snapshot.id.clone();
        let version = job.snapshot.version.clone();
        if job.fails_at_end && job.current == total {
            job.snapshot.steps[total - 1].state = StepState::Failed;
            job.snapshot.outcome =
                InstallOutcome::Failed(s("Health check failed. What this install added has been removed."));
        } else {
            job.snapshot = install_snapshot(&id, &version, job.current);
            if job.current == total {
                job.snapshot.outcome = InstallOutcome::Succeeded;
                if let Some(pkg) = self.catalog.iter_mut().find(|pkg| pkg.id == id) {
                    pkg.status = PackageStatus::Installed;
                }
                if !self.auto_update.packages.iter().any(|entry| entry.name == id.name) {
                    let third_party = id.feed != "official" && id.feed != "monokulo";
                    self.auto_update.packages.push(AutoUpdateEntry::new(
                        &id.name,
                        &id.feed,
                        third_party,
                        "Installed just now",
                    ));
                }
            }
        }
        Some(job.snapshot.clone())
    }

    fn cancel_install(&mut self) -> Option<InstallSnapshot> {
        let job = self.job.as_mut()?;
        job.snapshot.outcome = InstallOutcome::Cancelled;
        let snapshot = job.snapshot.clone();
        self.job = None;
        Some(snapshot)
    }

    fn feeds(&self) -> Vec<Feed> {
        self.feeds.clone()
    }

    fn probe_feed(&self, address: &str) -> Result<FeedProbe, String> {
        if !address.starts_with("https://") {
            return Err(s("A feed address must start with https://"));
        }
        let name = address.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
        let name = name.strip_suffix("-feed").unwrap_or(name);
        if name.is_empty() || address.contains("unreachable") {
            return Err(s("No feed answered at that address."));
        }
        Ok(FeedProbe {
            name: s(name),
            address: s(address),
            packages: 9,
            signed: true,
            fingerprint: s("SHA256:c41e 8d07 2fa9 66b3 19e0 75cd 0b42 e8a1"),
        })
    }

    fn trust_feed(&mut self, probe: &FeedProbe) -> Result<(), String> {
        if self.feeds.iter().any(|feed| feed.name == probe.name) {
            return Err(format!("A feed named {} already exists.", probe.name));
        }
        self.feeds.push(Feed {
            name: probe.name.clone(),
            packages: probe.packages,
            key_state: KeyState::Pinned,
            checked: s("just now"),
            status: FeedStatus::Healthy,
            address: probe.address.clone(),
            fingerprint: probe.fingerprint.clone(),
            added: s("7 November 2026"),
            installed_from: Vec::new(),
        });
        Ok(())
    }

    fn refresh_feeds(&mut self) {
        for feed in &mut self.feeds {
            if feed.status == FeedStatus::Healthy {
                feed.checked = s("just now");
            }
        }
    }

    fn toggle_feed(&mut self, name: &str) {
        if let Some(feed) = self.feeds.iter_mut().find(|feed| feed.name == name) {
            feed.status = match feed.status {
                FeedStatus::Disabled => FeedStatus::Healthy,
                _ => FeedStatus::Disabled,
            };
        }
    }

    fn remove_feed(&mut self, name: &str) {
        self.feeds.retain(|feed| feed.name != name);
    }

    fn key_change(&self, feed: &str) -> Option<KeyChange> {
        let entry = self.feeds.iter().find(|entry| entry.name == feed)?;
        (entry.key_state == KeyState::Changed).then(|| KeyChange {
            feed: s(feed),
            trusted: entry.fingerprint.clone(),
            now_signing: s("SHA256:e93a 17c0 5bd2 80f4 1a6c 39e7 42db a106"),
            affected: vec![
                (s("hash-tools 1.3.0"), s("Keeps running. No updates.")),
                (s("peer-map 0.9.1"), s("Keeps running. No updates.")),
            ],
        })
    }

    fn trust_new_key(&mut self, change: &KeyChange, typed: &str) -> Result<(), String> {
        let feed = change.feed.as_str();
        if typed != feed {
            return Err(format!("Type {feed} exactly to trust its new key."));
        }
        let change = self.key_change(feed).ok_or_else(|| format!("{feed} has no pending key change"))?;
        if let Some(entry) = self.feeds.iter_mut().find(|entry| entry.name == feed) {
            entry.fingerprint = change.now_signing;
            entry.key_state = KeyState::Pinned;
            entry.status = FeedStatus::Healthy;
        }
        Ok(())
    }

    fn settings_form(&self, package: &str) -> Option<SettingsForm> {
        self.forms.get(package).cloned()
    }

    fn references(&self, package: &str, field: &str) -> ReferenceList {
        if package == "moneropay" && field == "daemon_rpc" {
            sample_references()
        } else {
            ReferenceList { expects: s("a value"), options: Vec::new(), hidden_wrong_type: 0, hidden_secrets: 0 }
        }
    }

    fn explain_rollback(&self, package: &str, reason: &str) -> RollbackExplanation {
        RollbackExplanation {
            steps: vec![
                (true, s("Settings validated and saved")),
                (true, s("Service restarted")),
                (false, s("Health check failed after 30 seconds")),
                (true, s("Previous settings restored, service healthy")),
            ],
            headline: s(reason),
            hint: if reason.contains(CLASHING_PORT) {
                format!("API port {CLASHING_PORT} is used by monero-daemon. Pick a different port.")
            } else {
                format!("Read the {package} log for more detail.")
            },
            blamed_field: reason.contains(CLASHING_PORT).then(|| s("api_port")),
        }
    }

    fn updates(&self) -> Vec<UpdateItem> {
        self.updates.clone()
    }

    fn check_updates(&mut self) {}

    fn update_review(&self, name: &str) -> Option<ReviewData> {
        let item = self.updates.iter().find(|item| item.name == name)?;
        let UpdateNote::NewPermission(permission) = &item.note else {
            return None;
        };
        let id = PackageId::new("community", name);
        let mut review = sample_review(&id, &item.to);
        review.asks.retain(|ask| ask.level == AskLevel::Caution && ask.text == *permission);
        Some(review)
    }

    fn apply_updates(&mut self, names: &[String], _approvals: &[(String, nodo_api::PermissionApproval)]) -> UpdateReport {
        let mut results = Vec::new();
        let mut failure = None;
        for name in names {
            let Some(item) = self.updates.iter().find(|item| &item.name == name).cloned() else {
                continue;
            };
            if name == "moneropay" {
                results.push(UpdateResult {
                    name: name.clone(),
                    applied: false,
                    version: format!("still on {}", item.from),
                    note: format!("{} rolled back", item.to),
                });
                failure = Some(UpdateFailure {
                    name: name.clone(),
                    headline: format!("moneropay {} failed its health check", item.to),
                    kept_version: item.from.clone(),
                    restored: format!("Version {} and its data were restored.", item.from),
                    running: true,
                    log_line: s("database migration 14: table \"invoices\" already exists"),
                    verdict: s("This looks like a package problem. Your settings were not the cause."),
                });
            } else {
                let note = if item.note == UpdateNote::RestartNeeded {
                    s("Applied, restart when ready")
                } else {
                    s("Applied")
                };
                results.push(UpdateResult { name: name.clone(), applied: true, version: item.to, note });
                self.updates.retain(|entry| &entry.name != name);
            }
        }
        // Successes first, so the rollback that needs a decision sits next to its explanation.
        results.sort_by_key(|result| !result.applied);
        UpdateReport { results, failure }
    }

    fn apply_updates_with_passphrase(
        &mut self,
        names: &[String],
        approvals: &[(String, nodo_api::PermissionApproval)],
        passphrase: &Secret,
    ) -> UpdateReport {
        self.calls.push(Request::ApplyUpdatesWithPassphrase {
            updates: Vec::new(),
            passphrase: nodo_api::Secret::new(passphrase.expose()),
        });
        if passphrase.expose() != MOCK_PASSPHRASE {
            let results = names
                .iter()
                .map(|name| UpdateResult {
                    name: name.clone(),
                    applied: false,
                    version: String::new(),
                    note: s("that passphrase did not unlock this device"),
                })
                .collect();
            return UpdateReport { results, failure: None };
        }
        self.apply_updates(names, approvals)
    }

    fn set_held(&mut self, name: &str, held: bool) {
        if let Some(item) = self.updates.iter_mut().find(|item| item.name == name) {
            item.held = held;
            if held {
                item.note = UpdateNote::Skipped;
                item.preselected = false;
            } else if item.note == UpdateNote::Skipped {
                item.note = UpdateNote::Ready;
            }
        }
    }

    fn auto_update(&self) -> AutoUpdatePrefs {
        self.auto_update.clone()
    }

    fn save_auto_update(&mut self, prefs: &AutoUpdatePrefs) -> Result<(), String> {
        let mut saved = prefs.clone();
        saved.next_run = next_run(&saved.frequency, &saved.window_start);
        self.auto_update = saved;
        Ok(())
    }

    fn copy_report(&mut self, subject: &str) -> Result<String, String> {
        Ok(format!("Mock device: the {subject} report was not copied anywhere."))
    }

    fn logs(&self, source: &str) -> LogView {
        let info = |text: &str| LogLine { kind: LogKind::Info, text: s(text) };
        let error = |text: &str| LogLine { kind: LogKind::Error, text: s(text) };
        let bell_failing = self.is_locked();
        match source {
            "bell" if bell_failing => LogView {
                lines: vec![
                    info("14:02:11 INFO  starting version 1.2.0"),
                    info("14:02:11 INFO  reading /etc/bell/config.toml"),
                    info("14:02:12 INFO  connecting to wallet-relay"),
                    error("14:02:17 ERROR connection refused: wallet-relay is locked"),
                    info("14:02:17 INFO  exiting with status 1"),
                    LogLine { kind: LogKind::Marker, text: s("── restarted by supervisor, attempt 3 ──") },
                    info("14:02:47 INFO  starting version 1.2.0"),
                    info("14:02:48 INFO  connecting to wallet-relay"),
                    error("14:02:53 ERROR connection refused: wallet-relay is locked"),
                    info("14:02:53 INFO  exiting with status 1"),
                ],
                suggestion: vec![
                    s("bell depends on wallet-relay, which is locked."),
                    s("Press `u` to unlock the device. bell will start automatically."),
                ],
            },
            "moneropay" => LogView {
                lines: vec![
                    info("14:01:02 INFO  starting version 2.7.0"),
                    info("14:01:03 INFO  listening on 127.0.0.1:5000"),
                    info("14:01:03 INFO  daemon rpc reachable at 127.0.0.1:18081"),
                ],
                suggestion: Vec::new(),
            },
            other => LogView {
                lines: vec![
                    info(&format!("14:00:01 INFO  {other} log start")),
                    info(&format!("14:00:02 INFO  {other} running normally")),
                ],
                suggestion: Vec::new(),
            },
        }
    }

    fn log_sources(&self) -> Vec<String> {
        let mut sources: Vec<String> = ["cuprated", "bell", "moneropay", "tor", "wallet-relay"]
            .iter()
            .filter(|id| self.services.iter().any(|svc| svc.id == **id))
            .map(|id| s(id))
            .collect();
        sources.push(s("System"));
        sources.push(s("Updates"));
        sources
    }

    fn drive_action(&mut self, action: DriveAction) -> Result<Document, String> {
        match action {
            DriveAction::Retry => {
                if self.drive.is_some() {
                    Err(s("Mount failed: structure needs cleaning"))
                } else {
                    Ok(Document { title: s("Retry mount"), lines: vec![s("The drive is mounted.")] })
                }
            }
            DriveAction::ReadOnlyCheck => Ok(Document {
                title: s("Read-only drive check"),
                lines: vec![
                    s("xfs_repair -n /dev/nvme0n1p1"),
                    s("Phase 1: found the superblock"),
                    s("Phase 2: the log needs replaying"),
                    s("No changes were made."),
                ],
            }),
            DriveAction::Repair => {
                self.drive = None;
                self.services = sample_services();
                Ok(Document {
                    title: s("Repair finished"),
                    lines: vec![s("The filesystem was repaired and mounted at /media/monero.")],
                })
            }
            DriveAction::Diagnostics => Ok(Document {
                title: s("Diagnostics"),
                lines: vec![
                    s("nvme0n1: 1.9 TB, SMART passed"),
                    s("nvme0n1p1: label NODO_BLOCKCHAIN, xfs"),
                    s("/media/monero: not mounted"),
                    s("kernel: XFS (nvme0n1p1): Corruption of in-memory data detected"),
                ],
            }),
        }
    }

    fn web(&self) -> WebOverview {
        self.web.clone()
    }

    fn providers(&self) -> Vec<Provider> {
        sample_providers()
    }

    fn test_provider(
        &mut self,
        provider: &str,
        domain: &str,
        api_key: &Secret,
        secret_key: &Secret,
    ) -> Result<String, String> {
        let stored = self
            .stored_provider
            .as_ref()
            .is_some_and(|stored| stored.provider == provider && stored.domain == domain);
        if domain.is_empty() || (!stored && (api_key.is_empty() || secret_key.is_empty())) {
            return Err(s("Enter the domain and both keys first."));
        }
        Ok(s("Test passed: read and wrote a temporary record"))
    }

    fn save_provider(
        &mut self,
        provider: &str,
        domain: &str,
        api_key: &Secret,
        secret_key: &Secret,
    ) -> Result<(), String> {
        self.test_provider(provider, domain, api_key, secret_key)?;
        self.stored_provider = Some(StoredProvider { provider: s(provider), domain: s(domain) });
        self.web.provider = s(provider);
        self.web.domain = s(domain);
        Ok(())
    }

    fn save_acme(&mut self, settings: &nodo_api::AcmeSettingsInput) -> Result<(), String> {
        if settings.email.as_deref().is_some_and(|e| !e.contains('@')) {
            return Err(s("that email address is not valid"));
        }
        if let Some(domain) = &settings.local_domain {
            if !self.web.acme.dns01 {
                return Err(s("local names under your domain need certificates through your DNS provider"));
            }
            if !domain.ends_with(&self.web.domain) {
                return Err(format!("local names must be under {}", self.web.domain));
            }
        }
        let acme = &mut self.web.acme;
        acme.email = settings.email.clone().unwrap_or_default();
        acme.staging = settings.staging;
        acme.local_domain = settings.local_domain.clone();
        for site in &mut self.web.sites {
            site.local_url = match &settings.local_domain {
                Some(domain) => format!("https://{}.{domain}", site.package),
                None => format!("https://{}.local", site.package),
            };
        }
        Ok(())
    }

    fn save_dns01(&mut self, first: &Secret, second: &Secret) -> Result<(), String> {
        if first.is_empty() || second.is_empty() {
            return Err(s("Enter both the API key and the secret."));
        }
        self.web.acme.dns01_stored = true;
        Ok(())
    }

    fn check_local_names(&mut self) -> Result<Vec<nodo_api::LocalNameCheck>, String> {
        let Some(domain) = self.web.acme.local_domain.clone() else {
            return Ok(Vec::new());
        };
        let address = self.web.acme.local_address.clone().unwrap_or_default();
        Ok(self
            .web
            .sites
            .iter()
            .filter(|site| site.exposure.local)
            .map(|site| nodo_api::LocalNameCheck {
                host: format!("{}.{domain}", site.package),
                address: address.clone(),
                published: vec![address.clone()],
                resolves_to: vec![address.clone()],
                verdict: nodo_api::LocalNameVerdict::Resolves,
                explanation: format!("{}.{domain} resolves to {address} on this network.", site.package),
            })
            .collect())
    }

    fn reachability(&mut self, site: &str) -> Reachability {
        let attempt = self.reach_attempts.entry(s(site)).or_insert(0);
        *attempt += 1;
        let attempt = *attempt;
        if site == "monokulo-pay" {
            return Reachability::Cgnat {
                wan_address: s("100.72.14.9"),
                wan_range: s("100.64.0.0/10"),
                internet_address: s("203.0.113.42"),
            };
        }
        let host = self
            .web
            .sites
            .iter()
            .find(|entry| entry.package == site)
            .and_then(|entry| entry.public_host.clone())
            .unwrap_or_default();
        // The first attempt fails so the port forwarding guidance can be seen; "check again" passes.
        let ports_open = attempt > 1 || site != "shop";
        reachability_checks(&host, ports_open)
    }

    fn set_public_host(&mut self, site: &str, host: &str) -> Result<(), String> {
        if !host.ends_with(&format!(".{}", self.web.domain)) {
            return Err(format!("The address must be under {}.", self.web.domain));
        }
        let entry = self
            .web
            .sites
            .iter_mut()
            .find(|entry| entry.package == site)
            .ok_or_else(|| format!("unknown site {site}"))?;
        entry.public_host = Some(s(host));
        Ok(())
    }

    fn set_public_access(&mut self, site: &str, access: &PublicAccess) -> Result<(), String> {
        if self.fail_public_access {
            return Err(s("Could not apply the site's protections."));
        }
        self.public_access.insert(s(site), access.clone());
        Ok(())
    }

    fn ssh_keys(&self) -> Result<Vec<nodo_api::SshKey>, String> {
        Ok(self.ssh_keys.clone())
    }

    fn add_ssh_key(&mut self, line: &str) -> Result<(), String> {
        let mut fields = line.split_whitespace();
        let (Some(algorithm), Some(data)) = (fields.next(), fields.next()) else {
            return Err(s("that is not a single public key line"));
        };
        if !algorithm.starts_with("ssh-") && !algorithm.starts_with("ecdsa-") {
            return Err(s("the line must start with a key type such as ssh-ed25519"));
        }
        self.ssh_keys.push(nodo_api::SshKey {
            algorithm: algorithm.to_owned(),
            fingerprint: format!("SHA256:{}", data.chars().rev().take(43).collect::<String>()),
            comment: fields.collect::<Vec<_>>().join(" "),
        });
        Ok(())
    }

    fn remove_ssh_key(&mut self, fingerprint: &str) -> Result<(), String> {
        if self.ssh_keys.len() <= 1 {
            return Err(s("this is the only key that can log in. Add another before removing it"));
        }
        self.ssh_keys.retain(|key| key.fingerprint != fingerprint);
        Ok(())
    }

    fn document(&self, kind: &DocKind) -> Document {
        match kind {
            DocKind::Changelog(name) => Document {
                title: format!("{name} changelog"),
                lines: vec![s("Fixes and small improvements. See the publisher's notes for details.")],
            },
            DocKind::AutoUpdateHistory => Document {
                title: s("Auto-update history"),
                lines: vec![
                    s("3 November 03:12  moneropay 2.6.9 → 2.7.0  applied"),
                    s("26 October 03:08  monokulo-pay 1.3.2 → 1.4.0  applied"),
                ],
            },
            DocKind::Versions(name) => Document {
                title: format!("{name} versions"),
                lines: self
                    .catalog
                    .iter()
                    .filter(|pkg| &pkg.id.name == name)
                    .map(|pkg| format!("{}  from the {} feed", pkg.version, pkg.id.feed))
                    .collect(),
            },
            DocKind::FileList(name) => Document {
                title: format!("{name} files"),
                lines: vec![
                    format!("/opt/nodo/apps/{name}/bin/{name}"),
                    format!("/opt/nodo/apps/{name}/manifest.toml"),
                ],
            },
            DocKind::InstallLog(name) => Document {
                title: format!("{name} install log"),
                lines: vec![
                    s("verified signature with the pinned key"),
                    s("no root scripts, all paths allowed"),
                ],
            },
            DocKind::FeedPackages(name) => Document {
                title: format!("Packages in {name}"),
                lines: vec![s("hash-tools"), s("peer-map"), s("block-stats")],
            },
            DocKind::ProviderKeys(name) => Document {
                title: format!("{name} API keys"),
                lines: vec![
                    format!("Sign in to your {name} account and open its API settings."),
                    s("Create a key limited to DNS for this domain, then paste both values here."),
                ],
            },
            DocKind::RouterGuides => Document {
                title: s("Router guides"),
                lines: vec![
                    s("Open your router's admin page and find Port Forwarding, NAT or Virtual Servers."),
                    s("Add the two TCP rules shown, then run the check again."),
                ],
            },
            DocKind::PublicIpFromIsp => Document {
                title: s("Ask your ISP for a public IP"),
                lines: vec![
                    s("Ask for a public or static IPv4 address, and say that you are behind CGNAT."),
                    s("Some ISPs offer it free or as an add-on."),
                ],
            },
            DocKind::Ipv6 => Document {
                title: s("Use IPv6"),
                lines: vec![s("If your ISP provides IPv6, only IPv6 visitors can reach you this way.")],
            },
            DocKind::RelayTunnel => Document {
                title: s("Connect your own relay tunnel"),
                lines: vec![s("This needs a server you control, with its own public address.")],
            },
        }
    }
}

fn sample_services() -> Vec<ServiceStatus> {
    vec![
        service("cuprated", ServiceState::Running, ""),
        service("tor", ServiceState::Running, ""),
        service("moneropay", ServiceState::Running, ""),
        service("wallet-relay", ServiceState::Locked, "storage needs unlocking"),
        service("bell", ServiceState::Failed, "restarted 3 times"),
        service("monerod", ServiceState::Stopped, "alternative node"),
    ]
}

fn summary(
    feed: &str,
    name: &str,
    status: PackageStatus,
    (version, size): (&str, &str),
    tagline: &str,
    description: &[&str],
    needs: Vec<Need>,
) -> PackageSummary {
    PackageSummary {
        id: PackageId::new(feed, name),
        status,
        version: s(version),
        size: s(size),
        tagline: s(tagline),
        description: description.iter().map(|line| s(line)).collect(),
        needs,
    }
}

fn sample_catalog() -> Vec<PackageSummary> {
    use PackageStatus::{Available, Installed};
    let rpc = || vec![need("daemon-rpc", Some("from cuprated"))];
    let mut catalog = vec![
        summary(
            "official",
            "moneropay",
            Available,
            ("2.7.0", "14 MB"),
            "Payment gateway for Monero",
            &["Payment gateway for Monero.", "Runs against your local node."],
            vec![need("daemon-rpc", Some("from cuprated")), need("wallet-rpc", Some("installed with this package"))],
        ),
        summary("official", "cuprated", Installed, ("0.1.0", "38 MB"), "Monero node in Rust", &["The default Monero node."], vec![]),
        summary("official", "monerod", Available, ("0.18.4", "62 MB"), "The reference Monero node", &["The alternative Monero node."], vec![]),
        summary("official", "tor", Installed, ("0.4.8", "9 MB"), "Onion routing", &["Provides onion addresses."], vec![]),
        summary("official", "wallet-rpc", Available, ("0.18.4", "21 MB"), "Wallet RPC server", &["Wallet RPC for local packages."], rpc()),
        summary(
            "monokulo",
            "monokulo-pay",
            Installed,
            ("1.4.0", "11 MB"),
            "Accept payments from your node",
            &["Accept payments from your node."],
            rpc(),
        ),
        summary("monokulo", "bell", Installed, ("1.2.0", "3 MB"), "Alerts when a wallet receives", &["Alerts when a wallet receives funds."], rpc()),
        summary("monokulo", "node-exporter", Available, ("1.1.0", "6 MB"), "Node metrics, Monokulo build", &["Node metrics for local dashboards."], rpc()),
        summary(
            "community",
            "paywall-relay",
            Available,
            ("0.3.1", "5 MB"),
            "Pay-to-use relay access",
            &["Pay-to-use relay access."],
            rpc(),
        ),
        summary("community", "wallet-relay", Installed, ("0.4.2", "7 MB"), "Relay for remote wallets", &["Relay for remote wallets."], rpc()),
        summary("community", "fast-sync-helper", Available, ("1.0.0", "2 MB"), "Claims to speed up sync", &["Claims to speed up sync."], vec![]),
        summary(
            "community",
            "zmq-indexer",
            Available,
            ("0.8.0", "8 MB"),
            "Indexes blocks over ZMQ",
            &["Indexes blocks over ZMQ."],
            vec![Need { name: s("zmq"), met: false, source: Some(s("not provided by cuprated")) }],
        ),
        summary("community", "node-exporter", Available, ("0.9.0", "6 MB"), "Node metrics, community build", &["Node metrics for local dashboards."], rpc()),
    ];
    // Filler brings each feed up to the count the Feeds screen shows.
    for (feed, count, start) in [("official", 7, 1), ("monokulo", 3, 8), ("community", 18, 11)] {
        for number in start..start + count {
            let name = format!("sample-tool-{number:02}");
            catalog.push(summary(feed, &name, Available, ("1.0.0", "1 MB"), "Sample package", &["Sample package."], vec![]));
        }
    }
    catalog.sort_by_key(|pkg| match pkg.id.feed.as_str() {
        "official" => 0,
        "monokulo" => 1,
        _ => 2,
    });
    catalog
}

fn sample_review(id: &PackageId, version: &str) -> ReviewData {
    let ask = |level, text: &str, note: &str| Ask { level, text: s(text), note: s(note) };
    let third_party = id.feed != "official" && id.feed != "monokulo";
    let mut asks = vec![
        ask(AskLevel::Normal, "Use its own user and data folder", "Standard"),
        ask(AskLevel::Normal, "Read monero-daemon.public_rpc_url", "Public setting"),
        ask(AskLevel::Normal, "Reach daemon RPC on this device", "Local only"),
    ];
    if id.name == "wallet-relay" {
        asks.extend([
            ask(AskLevel::Caution, "Accept connections on port 18120", "Reachable from your network"),
            ask(AskLevel::Caution, "Connect out to any internet address", "Broad access"),
            ask(AskLevel::Encrypted, "Keep data in encrypted storage", "Locks at reboot"),
        ]);
    }
    ReviewData {
        id: id.clone(),
        version: s(version),
        third_party,
        asks,
        guarantees: vec![
            s("Cannot read other packages' data or secrets"),
            s("Cannot run setup scripts as root or replace system packages"),
        ],
        approval: None,
        needs_passphrase: false,
    }
}

/// The install steps with everything before `working` done. `working == steps.len()` is finished.
pub fn install_snapshot(id: &PackageId, version: &str, working: usize) -> InstallSnapshot {
    let name = &id.name;
    let plan = [
        ("Download package", "14 MB".to_owned()),
        ("Verify feed signature", format!("{} key", id.feed)),
        ("Check package contents", s("no root scripts, paths allowed")),
        ("Install files", format!("/opt/nodo/apps/{name}")),
        ("Create sandbox and firewall rules", String::new()),
        ("Set up encrypted storage", String::new()),
        ("Configure settings", String::new()),
        ("Start and run health check", String::new()),
    ];
    let steps = plan
        .into_iter()
        .enumerate()
        .map(|(index, (step, note))| InstallStep {
            name: s(step),
            note,
            state: match index.cmp(&working) {
                std::cmp::Ordering::Less => StepState::Done,
                std::cmp::Ordering::Equal => StepState::Working,
                std::cmp::Ordering::Greater => StepState::Pending,
            },
        })
        .collect();
    InstallSnapshot {
        id: id.clone(),
        version: s(version),
        steps,
        details: if working >= 4 {
            vec![
                format!("created user _{name}"),
                format!("sandbox allows /var/lib/apps/{name}"),
                format!("firewall allows _{name} to 127.0.0.1:18081"),
                s("checking rules before loading them"),
            ]
        } else {
            vec![s("fetching and checking the package")]
        },
        outcome: InstallOutcome::InProgress,
    }
}

fn sample_feeds() -> Vec<Feed> {
    let feed = |name: &str, packages, key_state, status, address: &str, fingerprint: &str, from: &[&str]| Feed {
        name: s(name),
        packages,
        key_state,
        checked: s("2 hours ago"),
        status,
        address: s(address),
        fingerprint: s(fingerprint),
        added: s("12 October 2026"),
        installed_from: from.iter().map(|name| s(name)).collect(),
    };
    vec![
        feed(
            "official",
            12,
            KeyState::BuiltIn,
            FeedStatus::Healthy,
            "https://feeds.monokulo.example/official",
            "SHA256:2d7c 11ab 54ef 09c3 7e18 b460 91fa cd32",
            &["cuprated", "tor", "moneropay"],
        ),
        feed(
            "monokulo",
            6,
            KeyState::Pinned,
            FeedStatus::Healthy,
            "https://github.com/example/monokulo-feed",
            "SHA256:7f3a 91c2 0e4d b856 4c11 de90 aa72 3b6f",
            &["monokulo-pay", "bell"],
        ),
        feed(
            "community",
            23,
            KeyState::Pinned,
            FeedStatus::Healthy,
            "https://github.com/example/community-feed",
            "SHA256:5a02 cc71 e83d 1f96 2b4e 70ad 39c8 f115",
            &["wallet-relay"],
        ),
        feed(
            "old-tools",
            4,
            KeyState::Changed,
            FeedStatus::Blocked,
            "https://github.com/example/old-tools-feed",
            "SHA256:08bd 42f1 a7c3 91de 66e0 d214 f8a9 03c7",
            &["hash-tools", "peer-map"],
        ),
        feed(
            "experiments",
            3,
            KeyState::Pinned,
            FeedStatus::Unreachable(s("unreachable since yesterday")),
            "https://github.com/example/experiments-feed",
            "SHA256:b19e 6604 d2fa 87c1 3e50 ac97 12db 4e68",
            &[],
        ),
    ]
}

fn sample_forms() -> HashMap<String, SettingsForm> {
    let field = |key: &str, label: &str, help: &str, requirement: &str, kind, value| Field {
        key: s(key),
        label: s(label),
        help: s(help),
        requirement: s(requirement),
        kind,
        value,
    };
    let levels = || vec![s("error"), s("warn"), s("info"), s("debug")];
    let moneropay = SettingsForm {
        package: s("moneropay"),
        fields: vec![
            field(
                "daemon_rpc",
                "Daemon RPC",
                "The node this package talks to.",
                "Required. Expects a URL.",
                FieldKind::Url,
                FieldValue::Text(s("http://127.0.0.1:18081")),
            ),
            field(
                "api_port",
                "API port",
                "Port for the local payment API.",
                "Required. Provided by the package.",
                FieldKind::Port,
                FieldValue::Text(s("5000")),
            ),
            field(
                "log_level",
                "Log level",
                "How much the service writes to its log.",
                "Optional. Defaults to info.",
                FieldKind::Choice(levels()),
                FieldValue::Text(s("info")),
            ),
            field(
                "payment_address",
                "Payment address",
                "Where received funds are sent.",
                "Required. Provided by the package.",
                FieldKind::MoneroAddress,
                FieldValue::Text(sample_address()),
            ),
            field(
                "webhook_secret",
                "Webhook secret",
                "Signs the webhooks this package sends.",
                "Optional. Never shown after saving.",
                FieldKind::Secret,
                FieldValue::SecretSet,
            ),
            field(
                "allow_lan",
                "Allow LAN access",
                "Lets other devices on your network reach the API.",
                "Optional.",
                FieldKind::Toggle,
                FieldValue::Bool(true),
            ),
        ],
        public_settings: vec![s("moneropay.api_url")],
    };
    let wallet_relay = SettingsForm {
        package: s("wallet-relay"),
        fields: vec![
            field(
                "relay_port",
                "Relay port",
                "Port remote wallets connect to.",
                "Required.",
                FieldKind::Port,
                FieldValue::Text(s("18120")),
            ),
            field(
                "log_level",
                "Log level",
                "How much the service writes to its log.",
                "Optional. Defaults to info.",
                FieldKind::Choice(levels()),
                FieldValue::Text(s("info")),
            ),
        ],
        public_settings: Vec::new(),
    };
    HashMap::from([(s("moneropay"), moneropay), (s("wallet-relay"), wallet_relay)])
}

pub fn sample_references() -> ReferenceList {
    let daemon_consequences = |port: &str| {
        vec![
            s("Make moneropay depend on monero-daemon, so it starts afterward."),
            format!("Let moneropay's sandbox reach 127.0.0.1:{port}."),
            s("Update and restart moneropay whenever the value changes."),
        ]
    };
    let role = || {
        Some((s("monero-daemon is a role."), s("It stays correct if you switch between cuprated and monerod.")))
    };
    ReferenceList {
        expects: s("a URL"),
        options: vec![
            ReferenceOption {
                path: s("monero-daemon.public_rpc_url"),
                value: s("http://127.0.0.1:18081"),
                role_note: role(),
                consequences: daemon_consequences("18081"),
            },
            ReferenceOption {
                path: s("monero-daemon.restricted_rpc_url"),
                value: s("http://127.0.0.1:18089"),
                role_note: role(),
                consequences: daemon_consequences("18089"),
            },
            ReferenceOption {
                path: s("wallet-rpc.url"),
                value: s("http://127.0.0.1:34512"),
                role_note: None,
                consequences: vec![
                    s("Make moneropay depend on wallet-rpc, so it starts afterward."),
                    s("Let moneropay's sandbox reach 127.0.0.1:34512."),
                    s("Update and restart moneropay whenever the value changes."),
                ],
            },
        ],
        hidden_wrong_type: 4,
        hidden_secrets: 2,
    }
}

fn sample_updates() -> Vec<UpdateItem> {
    vec![
        UpdateItem {
            name: s("moneropay"),
            from: s("2.7.0"),
            to: s("2.7.1"),
            source: s("official"),
            third_party: false,
            note: UpdateNote::Ready,
            held: false,
            preselected: true,
            grows: None,
        },
        UpdateItem {
            name: s("system base"),
            from: s("2026.11"),
            to: s("2026.12"),
            source: s("tested snapshot"),
            third_party: false,
            note: UpdateNote::RestartNeeded,
            held: false,
            preselected: true,
            grows: None,
        },
        UpdateItem {
            name: s("wallet-relay"),
            from: s("0.4.2"),
            to: s("0.5.0"),
            source: s("community"),
            third_party: true,
            note: UpdateNote::NewPermission(s("Connect out to any internet address")),
            held: false,
            preselected: false,
            grows: None,
        },
        UpdateItem {
            name: s("cuprated"),
            from: s("0.1.0"),
            to: s("0.2.0"),
            source: s("official"),
            third_party: false,
            note: UpdateNote::Skipped,
            held: true,
            preselected: false,
            grows: None,
        },
    ]
}

fn next_run(frequency: &str, start: &str) -> String {
    if frequency == "Daily" {
        format!("Next run tonight at {start}")
    } else {
        format!("Next run Sunday at {start}")
    }
}

fn sample_auto_update() -> AutoUpdatePrefs {
    let enabled = |name: &str, source: &str, note: &str| AutoUpdateEntry {
        enabled: true,
        ..AutoUpdateEntry::new(name, source, false, note)
    };
    AutoUpdatePrefs {
        frequency: s("Daily"),
        frequencies: vec![s("Daily"), s("Weekly")],
        window_start: s("03:00"),
        window_end: s("05:00"),
        next_run: next_run("Daily", "03:00"),
        packages: vec![
            enabled("moneropay", "official", "Last updated 4 days ago"),
            enabled("cuprated", "official", "Node restarts, dependents wait"),
            enabled("monokulo-pay", "monokulo", "Last updated 12 days ago"),
            AutoUpdateEntry::new("wallet-relay", "community", true, ""),
            AutoUpdateEntry::new("system base", "official", false, "Off means notify only"),
        ],
    }
}

fn sample_web() -> WebOverview {
    let site = |package: &str, exposure, onion: &str, host: Option<&str>, locked| Site {
        package: s(package),
        exposure,
        local_url: format!("https://{package}.local"),
        onion_url: s(onion),
        public_host: host.map(s),
        locked,
        rate_limit_unenforced: false,
        local_waiting: false,
    };
    WebOverview {
        sites: vec![
            site(
                "shop",
                Exposure { local: true, onion: true, public: true },
                "http://abc…xyz.onion",
                Some("shop.example.com"),
                false,
            ),
            site(
                "monokulo-pay",
                Exposure { local: true, onion: true, public: false },
                "http://def…uvw.onion",
                Some("pay.example.com"),
                false,
            ),
            site(
                "moneropay",
                Exposure { local: true, onion: false, public: false },
                "http://ghi…rst.onion",
                Some("moneropay.example.com"),
                false,
            ),
            site("wallet-relay", Exposure::default(), "http://jkl…opq.onion", None, true),
        ],
        domain: s("example.com"),
        provider: s("Porkbun"),
        certificates: s("✓ Renew in 61 days"),
        public_ip: s("203.0.113.42"),
        dns_matches: true,
        dns_changed: s("3 days ago"),
        reachable: true,
        local_problem: None,
        local_remedy: None,
        local_records_note: None,
        trust_url: Some(s("http://nodo.local")),
        certs: sample_certs(),
        acme: AcmeView {
            email: s("me@example.com"),
            staging: false,
            test_server: None,
            dns01: true,
            challenge: s("DNS-01 through Porkbun"),
            dns01_fields: Vec::new(),
            dns01_stored: false,
            local_domain: None,
            local_address: Some(s("192.168.1.60")),
            local_ca: Some(s("3F:9A:21:C4:7E:05:B8:D2:61:4A:93:0C:EE:17:5B:28:A6:D9:40:7F:12:C3:8B:65:E0:94:2D:71:BC:58:0A:F6")),
        },
    }
}

fn sample_certs() -> Vec<CertRow> {
    let local = |site: &str| CertRow {
        site: s(site),
        host: format!("{site}.local"),
        kind: CertKind::LocalInternal,
        state: nodo_api::CertState::Valid,
        issuer: s("this device's local CA"),
        days_left: Some(6),
        renewed: Some(s("18 hours ago")),
        challenge: None,
        problem: None,
        problem_detail: None,
    };
    vec![
        CertRow {
            site: s("shop"),
            host: s("shop.example.com"),
            kind: CertKind::Public,
            state: nodo_api::CertState::Valid,
            issuer: s("Let's Encrypt (R11)"),
            days_left: Some(61),
            renewed: Some(s("29 days ago")),
            challenge: Some(s("DNS-01")),
            problem: None,
            problem_detail: None,
        },
        local("shop"),
        local("monokulo-pay"),
        local("moneropay"),
    ]
}

fn sample_providers() -> Vec<Provider> {
    use Capability::{Account, Limited, NoApi, PortsNeeded, Yes};
    let provider = |name: &str, ddns, https, instructions: String| Provider {
        name: s(name),
        dynamic_dns: ddns,
        https_without_ports: https,
        instructions,
        fields: vec![s("api_key"), s("secret_key")],
        dns01_note: None,
    };
    let keys = |name: &str| format!("Create an API key in your {name} account, then enable API access for this domain.");
    let account = |name: &str| {
        format!("{name} only allows API access on some accounts. Create a key, then run the test to see if yours can.")
    };
    let unsupported = |name: &str| format!("{name} has no DNS API, so Monokulo cannot manage records there.");
    vec![
        provider("Porkbun", Yes, Yes, keys("Porkbun")),
        provider("Cloudflare", Yes, Yes, keys("Cloudflare")),
        provider("GoDaddy", Account, Account, account("GoDaddy")),
        provider("Namecheap", Account, Account, account("Namecheap")),
        provider("IONOS", Yes, Yes, keys("IONOS")),
        provider("Gandi", Yes, Yes, keys("Gandi")),
        provider("Name.com", Yes, Yes, keys("Name.com")),
        provider("Alibaba Cloud", Yes, Yes, keys("Alibaba Cloud")),
        provider(
            "Hostinger",
            Limited,
            PortsNeeded,
            s("Hostinger's API covers basic records only, so ports 80 and 443 must be open for HTTPS."),
        ),
        provider("Hover", NoApi, NoApi, unsupported("Hover")),
        provider("Squarespace", NoApi, NoApi, unsupported("Squarespace")),
    ]
}

pub fn sample_drive_failure() -> DriveFailure {
    let check = |state, text: &str, note: &str| Check { state, text: s(text), note: s(note) };
    DriveFailure {
        message: s("The SSD labelled NODO_BLOCKCHAIN is not mounted at /media/monero. cuprated will not start, so chain data cannot fill the system disk."),
        checks: vec![
            check(CheckState::Passed, "NVMe drive detected", "nvme0n1, 1.9 TB"),
            check(CheckState::Passed, "Partition found", "NODO_BLOCKCHAIN, xfs"),
            check(CheckState::Failed, "Mount failed", "structure needs cleaning"),
        ],
        waiting: s("moneropay, wallet-relay, bell. They start after the node returns."),
    }
}

pub fn reachability_checks(host: &str, ports_open: bool) -> Reachability {
    let check = |state, text: &str, note: &str| Check { state, text: s(text), note: s(note) };
    let port = |port: u16| {
        if ports_open {
            check(CheckState::Passed, &format!("Port {port} is reachable from outside"), "Reachable")
        } else {
            check(CheckState::Failed, &format!("Port {port} is not reachable from outside"), "Timed out")
        }
    };
    Reachability::Checked {
        checks: vec![
            check(CheckState::Passed, "Domain and API key work", "example.com"),
            check(CheckState::Passed, "Public IP found", "203.0.113.42"),
            check(CheckState::Passed, "Router has a real public IP", "No CGNAT detected"),
            check(CheckState::Passed, "Temporary DNS record works", "Removed after this check"),
            port(80),
            port(443),
        ],
        router: (!ports_open).then(|| RouterHelp {
            admin_page: s("http://192.168.1.1"),
            forward_http: s("to 192.168.1.50:80"),
            forward_https: s("to 192.168.1.50:443"),
            device_ip: s("192.168.1.50"),
        }),
        plan: ports_open.then(|| PublicPlan {
            url: format!("https://{host}"),
            host: s(host),
            dns_record: format!("{host}  A  203.0.113.42"),
            firewall: s("Opens 80 and 443"),
        }),
        manual: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_counts_match_the_design() {
        let catalog = sample_catalog();
        assert_eq!(catalog.len(), 41);
        assert_eq!(catalog.iter().filter(|pkg| pkg.id.feed == "official").count(), 12);
        assert_eq!(catalog.iter().filter(|pkg| pkg.id.feed == "monokulo").count(), 6);
        assert_eq!(catalog.iter().filter(|pkg| pkg.id.feed == "community").count(), 23);
    }

    #[test]
    fn unsupported_providers_are_exactly_hover_and_squarespace() {
        let unsupported: Vec<String> =
            sample_providers().into_iter().filter(|p| !p.is_supported()).map(|p| p.name).collect();
        assert_eq!(unsupported, ["Hover", "Squarespace"]);
    }

    #[test]
    fn unlock_only_accepts_the_mock_passphrase() {
        let mut mock = MockBackend::new();
        assert_eq!(
            mock.call(Request::Unlock { passphrase: nodo_api::Secret::new("wrong") }),
            Err(ApiError::BadPassphrase)
        );
        assert!(mock.is_locked());
        assert_eq!(mock.call(Request::Unlock { passphrase: nodo_api::Secret::new(MOCK_PASSPHRASE) }), Ok(Response::Ok));
        assert!(!mock.is_locked());
    }
}
