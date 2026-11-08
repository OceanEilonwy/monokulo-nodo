//! The real device: every call goes to the root helper over its socket.
//!
//! Where the helper answers `NotImplemented`, screens get empty data and the area is recorded in
//! `unavailable`, so they say so instead of showing anything made up.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use nodo_api::client::HelperClient;
use nodo_api::{
    ApiError, Backend, DeviceOverview, EncryptionState, NodeStatus, Origin, Request, Response, ServiceState,
    SettingCurrent, SettingField, SettingKind, SettingLiteral, SiteId,
};

use crate::model::*;
use crate::secret::Secret;

const LOG_LINES: u32 = 300;

pub struct HelperBackend {
    client: RefCell<HelperClient>,
    /// Screens name a site by one string. This maps it back to the helper's id.
    sites: RefCell<BTreeMap<String, SiteId>>,
    unavailable: RefCell<Vec<(Area, String)>>,
    /// The most recent read-only drive check, which a repair must quote.
    drive_check: Cell<Option<u64>>,
    pub(crate) manage: RefCell<crate::helper_manage::Cache>,
}

fn origin_label(origin: &Origin) -> String {
    match origin {
        Origin::System => "system".to_owned(),
        Origin::Feed(Some(feed)) => feed.clone(),
        Origin::Feed(None) => "app".to_owned(),
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn duration(secs: u64) -> String {
    match secs {
        s if s < 60 => "less than a minute".to_owned(),
        s if s < 3600 => counted(s / 60, "minute"),
        s if s < 2 * 86400 => counted(s / 3600, "hour"),
        s => counted(s / 86400, "day"),
    }
}

fn state_label(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Running => "running",
        ServiceState::Stopped => "stopped",
        ServiceState::Failed => "failed",
        ServiceState::Locked => "locked",
        ServiceState::Waiting => "waiting",
    }
}

fn reference_path(text: &str) -> Option<&str> {
    text.strip_prefix("$(")?.strip_suffix(')')
}

/// Maps the helper's form field to the TUI's. Secrets only ever arrive as set or unset.
pub fn field(f: SettingField) -> Field {
    let kind = match f.kind {
        SettingKind::Text => FieldKind::Text,
        SettingKind::Int { min, max } => FieldKind::Number { min, max },
        SettingKind::Bool => FieldKind::Toggle,
        SettingKind::Choice(choices) => FieldKind::Choice(choices),
        SettingKind::MoneroAddress => FieldKind::MoneroAddress,
        SettingKind::Url => FieldKind::Url,
        SettingKind::Secret => FieldKind::Secret,
    };
    let toggle = kind == FieldKind::Toggle;
    let value = match f.current {
        SettingCurrent::SecretSet => FieldValue::SecretSet,
        SettingCurrent::SecretUnset => FieldValue::SecretUnset,
        SettingCurrent::Literal(SettingLiteral::Bool(b)) => FieldValue::Bool(b),
        SettingCurrent::Literal(SettingLiteral::Int(n)) => FieldValue::Text(n.to_string()),
        SettingCurrent::Literal(SettingLiteral::Text(text)) if toggle => FieldValue::Bool(text == "true"),
        SettingCurrent::Literal(SettingLiteral::Text(text)) => FieldValue::Text(text),
        SettingCurrent::Reference { target, setting, resolved } => FieldValue::Reference {
            path: format!("{target}.{setting}"),
            resolved: resolved.unwrap_or_else(|| "does not resolve".to_owned()),
        },
        SettingCurrent::Default => match f.default.as_deref() {
            Some(text) => match reference_path(text) {
                Some(path) => FieldValue::Reference { path: path.to_owned(), resolved: String::new() },
                None if toggle => FieldValue::Bool(text == "true"),
                None => FieldValue::Text(text.to_owned()),
            },
            None if kind == FieldKind::Secret => FieldValue::SecretUnset,
            None if toggle => FieldValue::Bool(false),
            None => FieldValue::Text(String::new()),
        },
    };
    let requirement = match (&f.required, &f.default) {
        (true, _) => "Required".to_owned(),
        (false, Some(default)) => format!("Default {default}"),
        (false, None) => "Optional".to_owned(),
    };
    Field { key: f.name, label: f.label, help: f.help, requirement, kind, value }
}

/// How long ago a Unix time was, in the largest whole unit.
fn ago(at: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let secs = now.saturating_sub(at);
    let (n, unit) = match secs {
        0..=119 => return "Just now".to_owned(),
        120..=7199 => (secs / 60, "minutes"),
        7200..=172_799 => (secs / 3600, "hours"),
        _ => (secs / 86_400, "days"),
    };
    format!("{n} {unit} ago")
}

fn check_label(id: &str, status: nodo_api::CheckStatus) -> &'static str {
    use nodo_api::CheckStatus::{Failed, Passed};
    match (id, status) {
        ("domain_and_credentials", Passed) => "Domain and API key work",
        ("domain_and_credentials", _) => "Domain or API key did not work",
        ("public_ip", Passed) => "Public IP found",
        ("public_ip", _) => "Public IP could not be confirmed",
        ("router_address", Passed) => "Router has a real public IP",
        ("router_address", Failed) => "Router does not have the public IP",
        ("router_address", _) => "Router could not be asked for its address",
        ("temporary_record", Passed) => "Temporary DNS record works",
        ("temporary_record", _) => "Temporary DNS record failed",
        ("port_80", Passed) => "Port 80 is reachable from outside",
        ("port_80", Failed) => "Port 80 is not reachable from outside",
        ("port_80", _) => "Port 80 is unverified",
        ("port_443", Passed) => "Port 443 is reachable from outside",
        ("port_443", Failed) => "Port 443 is not reachable from outside",
        ("port_443", _) => "Port 443 is unverified",
        _ => "Check",
    }
}

impl HelperBackend {
    /// Certificate rows keyed like the sites, and the ACME choices. Empty when the helper is too
    /// old to report them, so nothing is invented.
    fn certificates(&self) -> (Vec<CertRow>, AcmeView) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let report = match self.ask(Request::CertificateStatus) {
            Ok(Response::Certificates(report)) => Some(report),
            _ => None,
        };
        let names = self.sites.borrow().clone();
        let key = |id: &SiteId| {
            names.iter().find(|(_, v)| *v == id).map_or_else(|| id.package.clone(), |(k, _)| k.clone())
        };
        let certs = report
            .as_ref()
            .map(|r| {
                r.hosts
                    .iter()
                    .map(|h| CertRow {
                        site: key(&h.site),
                        host: h.host.clone(),
                        kind: match h.kind {
                            nodo_api::HostKind::Public => CertKind::Public,
                            nodo_api::HostKind::LocalDomain => CertKind::LocalDomain,
                            nodo_api::HostKind::LocalInternal => CertKind::LocalInternal,
                        },
                        state: h.state,
                        issuer: h.issuer.clone().unwrap_or_default(),
                        days_left: h.not_after.map(|t| t.saturating_sub(now) / 86_400),
                        renewed: h.last_renewal.map(ago),
                        challenge: h.challenge.as_deref().map(|c| c.to_ascii_uppercase()),
                        problem: h.last_error.as_ref().map(|e| e.summary.clone()),
                        problem_detail: h.last_error.as_ref().map(|e| e.detail.clone()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut acme = AcmeView::default();
        if let Ok(Response::AcmeSettings(info)) = self.ask(Request::AcmeSettings) {
            let (dns01, challenge) = match info.challenge {
                nodo_api::ChallengeChoice::Dns01 { provider } => (true, format!("DNS-01 through {provider}")),
                nodo_api::ChallengeChoice::PortsNeeded { reason } => (false, reason),
            };
            acme = AcmeView {
                email: info.email.unwrap_or_default(),
                staging: info.staging,
                test_server: info.test_server.then_some(info.directory),
                dns01,
                challenge,
                dns01_fields: info.dns01_fields,
                dns01_stored: info.dns01_stored,
                local_domain: info.local_domain,
                local_address: info.local_address,
                local_ca: report.and_then(|r| r.local_ca_fingerprint),
            };
        }
        (certs, acme)
    }

    fn dns_providers(&self) -> Vec<nodo_api::DnsProviderDetail> {
        match self.ask(Request::ListDnsProviders) {
            Ok(Response::DnsProviders(list)) => list,
            _ => Vec::new(),
        }
    }

    pub fn connect(path: &Path) -> io::Result<Self> {
        Ok(Self {
            client: RefCell::new(HelperClient::connect(path)?),
            sites: RefCell::new(BTreeMap::new()),
            unavailable: RefCell::new(Vec::new()),
            drive_check: Cell::new(None),
            manage: RefCell::new(crate::helper_manage::Cache::default()),
        })
    }

    pub(crate) fn ask(&self, request: Request) -> Result<Response, ApiError> {
        self.client.borrow_mut().call(request)
    }

    /// Records an area as unavailable when the helper says it is, and passes other errors on.
    pub(crate) fn missing(&self, area: Area, error: &ApiError) {
        if let ApiError::NotImplemented(reason) = error {
            let mut list = self.unavailable.borrow_mut();
            if !list.iter().any(|(a, _)| *a == area) {
                list.push((area, reason.clone()));
            }
        }
    }

    fn services(&self) -> Vec<nodo_api::ServiceStatus> {
        match self.ask(Request::ListServices) {
            Ok(Response::Services(list)) => list,
            _ => Vec::new(),
        }
    }

    fn packages(&self) -> Vec<nodo_api::PackageInfo> {
        match self.ask(Request::ListPackages) {
            Ok(Response::Packages(list)) => list,
            _ => Vec::new(),
        }
    }

    fn device_rows(o: &DeviceOverview) -> Vec<(String, String)> {
        let mut rows = vec![
            ("Host".to_owned(), o.hostname.clone()),
            ("System disk".to_owned(), format!("{} free of {}", gib(o.disk_free_bytes), gib(o.disk_total_bytes))),
            (
                "Memory".to_owned(),
                format!("{} available of {}", gib(o.memory_available_kib * 1024), gib(o.memory_total_kib * 1024)),
            ),
            ("Uptime".to_owned(), duration(o.uptime_secs)),
            ("Kernel".to_owned(), o.kernel.clone()),
        ];
        if let Some(drive) = o.drive.as_ref().filter(|d| d.mounted) {
            rows.insert(1, ("Data drive".to_owned(), format!("{} free of {}", gib(drive.free_bytes), gib(drive.total_bytes))));
        }
        rows
    }
}

impl Backend for HelperBackend {
    fn call(&mut self, request: Request) -> Result<Response, ApiError> {
        self.ask(request)
    }
}

impl ScreenData for HelperBackend {
    fn overview(&self) -> Overview {
        // A snapshot starts here, so earlier reasons are dropped and gathered again.
        self.unavailable.borrow_mut().clear();
        let o = match self.ask(Request::DeviceOverview) {
            Ok(Response::Overview(o)) => o,
            Ok(other) => return failed_overview(format!("unexpected reply: {other:?}")),
            Err(e) => return failed_overview(e.to_string()),
        };
        let services = self.services();
        let node = services
            .iter()
            .find(|s| s.roles.iter().any(|r| r == "monero-daemon"))
            .map(|s| {
                let (height, peers) = if s.state == ServiceState::Running {
                    match self.ask(Request::NodeStatus) {
                        Ok(Response::NodeStatus(status)) => node_rows(&status),
                        Ok(other) => unknown_node(format!("unexpected reply: {other:?}")),
                        Err(e) => unknown_node(e.to_string()),
                    }
                } else {
                    unknown_node(format!("{} is not running", s.id))
                };
                NodeInfo {
                    role: "monero-daemon".to_owned(),
                    provider: s.id.clone(),
                    state: state_label(s.state).to_owned(),
                    height,
                    peers,
                }
            })
            .unwrap_or_default();
        let service_origins = services
            .iter()
            .map(|s| (s.id.clone(), s.roles.first().cloned().unwrap_or_else(|| origin_label(&s.origin))))
            .collect();
        let health = crate::helper_manage::health(self);
        let drive = o.drive.as_ref().filter(|d| !d.present || !d.mounted).map(|d| {
            let waiting: Vec<String> =
                services.iter().filter(|s| s.state == ServiceState::Waiting).map(|s| s.id.clone()).collect();
            DriveFailure {
                message: if d.present {
                    format!("The data drive is connected but not mounted at {}.", d.mount_point)
                } else {
                    "The data drive is not connected.".to_owned()
                },
                checks: vec![
                    Check {
                        state: if d.present { CheckState::Passed } else { CheckState::Failed },
                        text: "Drive connected".to_owned(),
                        note: d.label.clone(),
                    },
                    Check {
                        state: if d.mounted { CheckState::Passed } else { CheckState::Failed },
                        text: format!("Mounted at {}", d.mount_point),
                        note: d.filesystem.clone().unwrap_or_else(|| "no filesystem found".to_owned()),
                    },
                ]
                .into_iter()
                .chain(crate::helper_manage::drive_checks(health.as_ref(), &d.mount_point))
                .collect(),
                waiting: if waiting.is_empty() { String::new() } else { format!("Waiting: {}", waiting.join(", ")) },
            }
        });
        let unlock_targets = if o.encryption == EncryptionState::Locked {
            self.packages()
                .into_iter()
                .filter(|p| p.encrypted_storage)
                .map(|p| UnlockTarget { name: p.id, encrypted: true, note: "Encrypted storage".to_owned() })
                .collect()
        } else {
            Vec::new()
        };
        let status = self.manage.borrow().status.clone();
        let mut device = Self::device_rows(&o);
        device.extend(crate::helper_manage::health_rows(health.as_ref()));
        Overview {
            node,
            device,
            service_origins,
            drive,
            restarted: match health.as_ref().and_then(|h| h.boot_fallback.as_ref()) {
                Some(_) => format!("Up for {}, on the previous system", duration(o.uptime_secs)),
                None => format!("Up for {}", duration(o.uptime_secs)),
            },
            unlock_targets,
            updates_checked: match status.as_ref().and_then(|s| s.last_check) {
                Some(at) => format!("Last checked {}.", crate::helper_manage::ago(at)),
                None => "Not checked yet. Press r to check now.".to_owned(),
            },
            setup_needed: o.encryption == EncryptionState::NotSetUp,
            attention: crate::helper_manage::attention(health.as_ref(), status.as_ref()),
        }
    }

    fn catalog(&self) -> Vec<PackageSummary> {
        crate::helper_manage::catalog(self, self.packages())
    }

    fn package_detail(&self, id: &PackageId) -> Option<PackageDetail> {
        let Ok(Response::PackageDetail(d)) = self.ask(Request::PackageDetail { id: id.name.clone() }) else {
            return crate::helper_manage::available_detail(self, id);
        };
        let mut asks: Vec<(String, String)> = d.inbound.iter().map(|rule| ("Inbound".to_owned(), rule.clone())).collect();
        if d.outbound_internet {
            asks.push(("Outbound".to_owned(), "internet".to_owned()));
        }
        Some(PackageDetail {
            id: id.clone(),
            installed: true,
            // Feed packages were verified against their pinned key when they were installed.
            signature_verified: true,
            about: if d.info.roles.is_empty() { String::new() } else { format!("Provides {}", d.info.roles.join(", ")) },
            version: d.info.version,
            size: String::new(),
            license: String::new(),
            needs: d.info.needs.into_iter().map(|name| Need { name, met: true, source: None }).collect(),
            public_settings: d.public_settings,
            asks,
            encrypted_storage: d.info.encrypted_storage,
        })
    }

    fn install_gate(&self, id: &PackageId) -> InstallGate {
        crate::helper_manage::install_gate(self, id)
    }

    /// The helper installs in one request, so the progress screen gets the finished steps.
    fn start_install(
        &mut self,
        id: &PackageId,
        approval: Option<&nodo_api::PermissionApproval>,
        passphrase: Option<&Secret>,
    ) -> Result<InstallSnapshot, String> {
        crate::helper_manage::start_install(self, id, approval, passphrase)
    }

    fn poll_install(&mut self) -> Option<InstallSnapshot> {
        None
    }

    fn cancel_install(&mut self) -> Option<InstallSnapshot> {
        None
    }

    fn feeds(&self) -> Vec<Feed> {
        crate::helper_manage::feeds(self)
    }

    fn probe_feed(&self, address: &str) -> Result<FeedProbe, String> {
        crate::helper_manage::probe_feed(self, address)
    }

    fn trust_feed(&mut self, probe: &FeedProbe) -> Result<(), String> {
        crate::helper_manage::trust_feed(self, probe)
    }

    fn refresh_feeds(&mut self) {
        let _ = self.ask(Request::RefreshFeeds);
    }

    fn toggle_feed(&mut self, name: &str) {
        crate::helper_manage::toggle_feed(self, name);
    }

    fn remove_feed(&mut self, name: &str) {
        let _ = self.ask(Request::RemoveFeed { name: name.to_owned() });
    }

    fn key_change(&self, feed: &str) -> Option<KeyChange> {
        crate::helper_manage::key_change(self, feed)
    }

    fn trust_new_key(&mut self, change: &KeyChange, typed: &str) -> Result<(), String> {
        crate::helper_manage::trust_new_key(self, change, typed)
    }

    fn settings_form(&self, package: &str) -> Option<SettingsForm> {
        let Ok(Response::SettingsForm(form)) = self.ask(Request::SettingsForm { package: package.to_owned() }) else {
            return None;
        };
        let public_settings = form.fields.iter().filter_map(|f| f.public_name.clone()).collect();
        Some(SettingsForm {
            package: form.package,
            fields: form.fields.into_iter().map(field).collect(),
            public_settings,
        })
    }

    /// Public settings of every other package with settings, by package id and by each role it
    /// provides. Secrets are never public, so none appear.
    fn references(&self, package: &str, _field: &str) -> ReferenceList {
        let mut options = Vec::new();
        for p in self.packages().into_iter().filter(|p| p.has_settings && p.id != package) {
            let Ok(Response::SettingsForm(form)) = self.ask(Request::SettingsForm { package: p.id.clone() }) else {
                continue;
            };
            for f in form.fields {
                let Some(public) = f.public_name.clone() else { continue };
                let value = match &field(f).value {
                    FieldValue::Text(text) => text.clone(),
                    FieldValue::Bool(b) => b.to_string(),
                    FieldValue::Reference { resolved, .. } => resolved.clone(),
                    _ => String::new(),
                };
                for role in &p.roles {
                    options.push(ReferenceOption {
                        path: format!("{role}.{public}"),
                        value: value.clone(),
                        role_note: Some((role.clone(), p.id.clone())),
                        consequences: vec![format!("{package} will depend on whichever package provides {role}.")],
                    });
                }
                options.push(ReferenceOption {
                    path: format!("{}.{public}", p.id),
                    value,
                    role_note: None,
                    consequences: vec![format!("{package} will depend on {}.", p.id)],
                });
            }
        }
        ReferenceList { expects: String::new(), options, hidden_wrong_type: 0, hidden_secrets: 0 }
    }

    fn explain_rollback(&self, package: &str, reason: &str) -> RollbackExplanation {
        RollbackExplanation {
            steps: vec![
                (true, "New settings checked and saved".to_owned()),
                (false, format!("{package} did not come back healthy")),
                (true, "Old settings restored".to_owned()),
            ],
            headline: format!("{package} failed with the new settings, so the old ones were put back."),
            hint: reason.to_owned(),
            blamed_field: None,
        }
    }

    fn updates(&self) -> Vec<UpdateItem> {
        crate::helper_manage::updates(self)
    }

    fn check_updates(&mut self) {
        crate::helper_manage::check_updates(self);
    }

    fn update_review(&self, name: &str) -> Option<ReviewData> {
        crate::helper_manage::update_review(self, name)
    }

    fn apply_updates(&mut self, names: &[String], approvals: &[(String, nodo_api::PermissionApproval)]) -> UpdateReport {
        crate::helper_manage::apply_updates(self, names, approvals, None)
    }

    fn apply_updates_with_passphrase(
        &mut self,
        names: &[String],
        approvals: &[(String, nodo_api::PermissionApproval)],
        passphrase: &Secret,
    ) -> UpdateReport {
        crate::helper_manage::apply_updates(self, names, approvals, Some(passphrase))
    }

    fn system_note(&self) -> Option<String> {
        crate::helper_manage::system_note(self)
    }

    fn set_held(&mut self, name: &str, held: bool) {
        crate::helper_manage::set_held(self, name, held);
    }

    fn auto_update(&self) -> AutoUpdatePrefs {
        crate::helper_manage::auto_update(self)
    }

    fn save_auto_update(&mut self, prefs: &AutoUpdatePrefs) -> Result<(), String> {
        crate::helper_manage::save_auto_update(self, prefs)
    }

    fn copy_report(&mut self, _subject: &str) -> Result<String, String> {
        Err("Copying reports is not available yet.".to_owned())
    }

    fn logs(&self, source: &str) -> LogView {
        match self.ask(Request::ReadLog { source: source.to_owned(), max_lines: LOG_LINES }) {
            Ok(Response::Log(chunk)) => LogView {
                lines: chunk
                    .lines
                    .into_iter()
                    .map(|text| {
                        let lower = text.to_ascii_lowercase();
                        let kind = if lower.contains("error") || lower.contains("panic") { LogKind::Error } else { LogKind::Info };
                        LogLine { kind, text }
                    })
                    .collect(),
                suggestion: Vec::new(),
            },
            Ok(_) => LogView::default(),
            Err(e) => LogView { lines: vec![LogLine { kind: LogKind::Error, text: e.to_string() }], suggestion: Vec::new() },
        }
    }

    fn log_sources(&self) -> Vec<String> {
        match self.ask(Request::LogSources) {
            Ok(Response::LogSources(list)) => list,
            _ => Vec::new(),
        }
    }

    fn drive_action(&mut self, action: DriveAction) -> Result<Document, String> {
        let doc = |title: &str, lines: Vec<String>| Document { title: title.to_owned(), lines };
        match action {
            DriveAction::Retry => match self.ask(Request::MountDrive) {
                Ok(_) => Ok(doc("Data drive", vec!["The data drive is mounted.".to_owned()])),
                Err(e) => Err(e.to_string()),
            },
            DriveAction::ReadOnlyCheck => match self.ask(Request::CheckDrive) {
                Ok(Response::DriveReport(report)) => {
                    self.drive_check.set(Some(report.check_id));
                    let verdict = if report.clean {
                        "No problems found."
                    } else {
                        "Problems found. Repair can fix them, and may lose some data."
                    };
                    let mut lines = vec![verdict.to_owned(), String::new()];
                    lines.extend(report.output);
                    Ok(doc("Read-only check", lines))
                }
                Ok(_) => Err("unexpected reply".to_owned()),
                Err(e) => Err(e.to_string()),
            },
            DriveAction::Repair => {
                // The modal already asked. The helper still refuses without a recent failed check.
                let check_id = self.drive_check.get().ok_or("Run the read-only check first.")?;
                let confirmation = nodo_api::RepairConfirmation { check_id, accept_data_loss_risk: true };
                match self.ask(Request::RepairDrive(confirmation)) {
                    Ok(Response::DriveReport(report)) => {
                        self.drive_check.set(None);
                        let mut lines = vec![if report.clean { "Repaired." } else { "The repair could not fix everything." }.to_owned()];
                        lines.extend(report.output);
                        Ok(doc("Repair", lines))
                    }
                    Ok(_) => Err("unexpected reply".to_owned()),
                    Err(e) => Err(e.to_string()),
                }
            }
            DriveAction::Diagnostics => match self.ask(Request::DriveStatus) {
                Ok(Response::Drive(Some(d))) => Ok(doc(
                    "Data drive",
                    vec![
                        format!("Device: {}", d.label),
                        format!("Connected: {}", if d.present { "yes" } else { "no" }),
                        format!("Mounted at {}: {}", d.mount_point, if d.mounted { "yes" } else { "no" }),
                        format!("Filesystem: {}", d.filesystem.unwrap_or_else(|| "unknown".to_owned())),
                    ],
                )),
                Ok(_) => Err("This device has no data drive.".to_owned()),
                Err(e) => Err(e.to_string()),
            },
        }
    }

    fn web(&self) -> WebOverview {
        let Ok(Response::Sites(list)) = self.ask(Request::ListSites) else {
            return WebOverview::default();
        };
        let status = match self.ask(Request::WebStatus) {
            Ok(Response::WebStatus(status)) => Some(status),
            _ => None,
        };
        let mut per_package: BTreeMap<String, usize> = BTreeMap::new();
        for site in &list {
            *per_package.entry(site.id.package.clone()).or_default() += 1;
        }
        let mut map = BTreeMap::new();
        let sites = list
            .into_iter()
            .map(|site| {
                let key = if per_package[&site.id.package] == 1 {
                    site.id.package.clone()
                } else {
                    format!("{}/{}", site.id.package, site.id.site)
                };
                map.insert(key.clone(), site.id.clone());
                let live = status.as_ref().and_then(|st| st.sites.iter().find(|s| s.id == site.id));
                Site {
                    package: key,
                    exposure: site.exposure,
                    local_url: live.and_then(|l| l.local_url.clone()).unwrap_or_default(),
                    onion_url: live.and_then(|l| l.onion_url.clone()).unwrap_or_default(),
                    public_host: site.public_host,
                    locked: site.locked,
                    rate_limit_unenforced: live.is_some_and(|l| l.rate_limit_unenforced),
                    local_waiting: live.is_some_and(|l| l.local_waiting),
                }
            })
            .collect();
        *self.sites.borrow_mut() = map;
        let (certs, mut acme) = self.certificates();
        let local = match self.ask(Request::LocalSiteStatus) {
            Ok(Response::LocalSiteStatus(local)) => local,
            _ => nodo_api::LocalSiteStatus::default(),
        };
        // The fingerprint the trust page serves, which is the one the owner compares.
        if local.ca_fingerprint.is_some() {
            acme.local_ca = local.ca_fingerprint.clone();
        }
        let Some(status) = status else {
            return WebOverview {
                sites,
                certs,
                acme,
                local_records_note: local.records_note,
                trust_url: local.trust_url,
                ..WebOverview::default()
            };
        };
        let provider = status.provider.as_deref().map(|id| {
            self.dns_providers().into_iter().find(|p| p.id == id).map_or_else(|| id.to_owned(), |p| p.name)
        });
        WebOverview {
            sites,
            domain: status.domain.unwrap_or_else(|| "Not connected".to_owned()),
            provider: provider.unwrap_or_default(),
            certificates: String::new(),
            public_ip: status.public_ip.unwrap_or_else(|| "Not checked".to_owned()),
            dns_matches: status.ddns_running && status.stale_records.is_empty(),
            dns_changed: status.dns_changed_at.map_or_else(|| "Never".to_owned(), ago),
            // Nothing outside has tested the ports, so they are never shown as reachable.
            reachable: false,
            local_problem: status.local_unavailable,
            local_remedy: status.local_remedy,
            local_records_note: local.records_note,
            trust_url: local.trust_url,
            certs,
            acme,
        }
    }


    fn providers(&self) -> Vec<Provider> {
        let list = self.dns_providers();
        if list.is_empty() {
            if let Err(e) = self.ask(Request::ListDnsProviders) {
                self.missing(Area::Providers, &e);
            }
        }
        list.into_iter()
            .map(|p| {
                let ddns = if !p.dynamic_dns {
                    Capability::NoApi
                } else if p.caveat.is_some() {
                    Capability::Account
                } else {
                    Capability::Yes
                };
                let (https, dns01_note) = match p.dns01 {
                    nodo_api::Dns01Support::Bundled => (Capability::Yes, None),
                    nodo_api::Dns01Support::BundledSeparateCredentials(note) => (Capability::Account, Some(note)),
                    nodo_api::Dns01Support::Unavailable => (Capability::PortsNeeded, None),
                };
                let instructions = match (&p.unavailable_reason, &p.caveat) {
                    (Some(reason), _) => reason.clone(),
                    (None, Some(caveat)) => format!("Create an API key in your {} account. {caveat}", p.name),
                    (None, None) => format!("Create an API key in your {} account.", p.name),
                };
                Provider { name: p.name, dynamic_dns: ddns, https_without_ports: https, instructions, fields: p.fields, dns01_note }
            })
            .collect()
    }

    fn test_provider(&mut self, provider: &str, domain: &str, api_key: &Secret, secret_key: &Secret) -> Result<String, String> {
        self.save_provider(provider, domain, api_key, secret_key)?;
        match self.ask(Request::TestDnsProvider) {
            Ok(Response::ProviderTest(text)) => Ok(text),
            Ok(_) => Err("unexpected reply".to_owned()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn save_provider(&mut self, provider: &str, domain: &str, api_key: &Secret, secret_key: &Secret) -> Result<(), String> {
        let detail = self
            .dns_providers()
            .into_iter()
            .find(|p| p.name == provider)
            .ok_or_else(|| format!("{provider} is not a known provider"))?;
        let fields = detail
            .fields
            .iter()
            .zip([api_key, secret_key])
            .map(|(name, value)| (name.clone(), nodo_api::Secret::new(value.expose())))
            .collect();
        let request = Request::SetDnsProvider { domain: domain.to_owned(), provider: detail.id, fields };
        self.ask(request).map(|_| ()).map_err(|e| e.to_string())
    }

    fn reachability(&mut self, site: &str) -> Reachability {
        let host = self.web().sites.into_iter().find(|s| s.package == site).and_then(|s| s.public_host);
        let detail = match self.ask(Request::CheckReachabilityDetail { site: self.site_id(site) }) {
            Ok(Response::ReachabilityDetail(detail)) => detail,
            other => {
                let note = match other {
                    Err(e) => e.to_string(),
                    Ok(_) => "unexpected reply".to_owned(),
                };
                return Reachability::Checked {
                    checks: vec![Check { state: CheckState::Failed, text: "The reachability check could not run".to_owned(), note }],
                    router: None,
                    plan: None,
                    manual: Vec::new(),
                };
            }
        };
        if detail.verdict == "blocked_by_cgnat" {
            return Reachability::Cgnat {
                wan_address: detail.router_wan.clone().unwrap_or_default(),
                wan_range: "100.64.0.0/10".to_owned(),
                internet_address: detail.public_ip.clone().unwrap_or_default(),
            };
        }
        let checks = detail
            .checks
            .iter()
            .filter(|c| c.status != nodo_api::CheckStatus::Skipped)
            .map(|c| Check {
                state: match c.status {
                    nodo_api::CheckStatus::Passed => CheckState::Passed,
                    nodo_api::CheckStatus::Failed => CheckState::Failed,
                    _ => CheckState::Unknown,
                },
                text: check_label(&c.id, c.status).to_owned(),
                note: c.detail.clone(),
            })
            .collect();
        let router = match (&detail.device_ip, &detail.gateway) {
            (Some(device), Some(gateway)) if detail.verdict != "ready" => Some(RouterHelp {
                admin_page: format!("http://{gateway}"),
                forward_http: format!("to {device}:80"),
                forward_https: format!("to {device}:443"),
                device_ip: device.clone(),
            }),
            _ => None,
        };
        let plan = match (detail.verdict.as_str(), host) {
            ("ready" | "ports_unverified", Some(host)) => Some(PublicPlan {
                url: format!("https://{host}"),
                dns_record: format!("{host}  A  {}", detail.public_ip.clone().unwrap_or_default()),
                host,
                firewall: "Opens 80 and 443".to_owned(),
            }),
            _ => None,
        };
        Reachability::Checked { checks, router, plan, manual: detail.manual_check }
    }

    fn set_public_host(&mut self, site: &str, host: &str) -> Result<(), String> {
        let request = Request::SetPublicHost { site: self.site_id(site), host: host.to_owned() };
        self.ask(request).map(|_| ()).map_err(|e| e.to_string())
    }

    fn save_acme(&mut self, settings: &nodo_api::AcmeSettingsInput) -> Result<(), String> {
        self.ask(Request::SetAcmeSettings(settings.clone())).map(|_| ()).map_err(|e| e.to_string())
    }

    fn save_dns01(&mut self, first: &Secret, second: &Secret) -> Result<(), String> {
        let fields = match self.ask(Request::AcmeSettings) {
            Ok(Response::AcmeSettings(info)) => info.dns01_fields,
            Ok(_) => return Err("unexpected reply".to_owned()),
            Err(e) => return Err(e.to_string()),
        };
        let fields = fields
            .into_iter()
            .zip([first, second])
            .map(|(name, value)| (name, nodo_api::Secret::new(value.expose())))
            .collect();
        self.ask(Request::SetDns01Credentials { fields }).map(|_| ()).map_err(|e| e.to_string())
    }

    fn check_local_names(&mut self) -> Result<Vec<nodo_api::LocalNameCheck>, String> {
        match self.ask(Request::CheckLocalNames) {
            Ok(Response::LocalNames(list)) => Ok(list),
            Ok(_) => Err("unexpected reply".to_owned()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set_public_access(&mut self, _site: &str, access: &PublicAccess) -> Result<(), String> {
        // Admin paths and rate limiting are always on. A password in front of a site is not built.
        if access.require_password {
            Err("A password in front of a site is not available yet. Turn it off to go public.".to_owned())
        } else {
            Ok(())
        }
    }

    fn document(&self, kind: &DocKind) -> Document {
        if let Some(doc) = crate::helper_manage::document(self, kind) {
            return doc;
        }
        Document { title: format!("{kind:?}"), lines: vec!["Not available yet.".to_owned()] }
    }

    fn ssh_keys(&self) -> Result<Vec<nodo_api::SshKey>, String> {
        match self.ask(Request::ListSshKeys) {
            Ok(Response::SshKeys(keys)) => Ok(keys),
            Ok(_) => Err("unexpected reply".to_owned()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn add_ssh_key(&mut self, line: &str) -> Result<(), String> {
        self.ask(Request::AddSshKey { line: line.to_owned() }).map(|_| ()).map_err(|e| e.to_string())
    }

    fn remove_ssh_key(&mut self, fingerprint: &str) -> Result<(), String> {
        self.ask(Request::RemoveSshKey { fingerprint: fingerprint.to_owned() }).map(|_| ()).map_err(|e| e.to_string())
    }

    fn unavailable(&self) -> Vec<(Area, String)> {
        self.unavailable.borrow().clone()
    }

    fn backups(&self) -> Result<Vec<nodo_api::BackupInfo>, String> {
        crate::helper_manage::backups(self)
    }

    fn create_backup(&mut self, device: Option<&Secret>, backup: &Secret) -> Result<nodo_api::BackupCreated, String> {
        crate::helper_manage::create_backup(self, device, backup)
    }

    fn inspect_backup(&mut self, name: &str, passphrase: &Secret) -> Result<nodo_api::BackupContents, String> {
        crate::helper_manage::inspect_backup(self, name, passphrase)
    }

    fn restore_backup(
        &mut self,
        name: &str,
        passphrase: &Secret,
        new_device: Option<&Secret>,
        typed: &str,
    ) -> Result<nodo_api::RestoreReport, String> {
        crate::helper_manage::restore_backup(self, name, passphrase, new_device, typed)
    }

    fn usb_drives(&self) -> Result<Vec<nodo_api::UsbDrive>, String> {
        crate::helper_manage::usb_drives(self)
    }

    fn usb_backups(&self, device: &str) -> Result<nodo_api::UsbBackupList, String> {
        crate::helper_manage::usb_backups(self, device)
    }

    fn create_usb_backup(
        &mut self,
        device: &str,
        device_passphrase: Option<&Secret>,
        backup: &Secret,
    ) -> Result<nodo_api::BackupCreated, String> {
        crate::helper_manage::create_usb_backup(self, device, device_passphrase, backup)
    }

    fn inspect_usb_backup(
        &mut self,
        device: &str,
        name: &str,
        passphrase: &Secret,
    ) -> Result<nodo_api::BackupContents, String> {
        crate::helper_manage::inspect_usb_backup(self, device, name, passphrase)
    }

    fn restore_usb_backup(
        &mut self,
        device: &str,
        name: &str,
        passphrase: &Secret,
        new_device: Option<&Secret>,
        typed: &str,
    ) -> Result<nodo_api::RestoreReport, String> {
        crate::helper_manage::restore_usb_backup(self, device, name, passphrase, new_device, typed)
    }

    fn site_id(&self, key: &str) -> SiteId {
        if let Some(id) = self.sites.borrow().get(key) {
            return id.clone();
        }
        match key.split_once('/') {
            Some((package, site)) => SiteId { package: package.to_owned(), site: site.to_owned() },
            None => SiteId { package: key.to_owned(), site: key.to_owned() },
        }
    }
}

fn failed_overview(reason: String) -> Overview {
    Overview { device: vec![("Helper".to_owned(), format!("could not read the device: {reason}"))], ..Overview::default() }
}

/// Digits grouped by thousands, as Home shows heights.
fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Height and peers for Home. The target is shown only while the node knows it is behind.
fn node_rows(status: &NodeStatus) -> (String, String) {
    let height = if status.target_height > status.height {
        let percent = status.height.saturating_mul(100) / status.target_height;
        format!("{} of {} ({percent}%)", grouped(status.height), grouped(status.target_height))
    } else if status.synchronized {
        format!("{} (synced)", grouped(status.height))
    } else {
        grouped(status.height)
    };
    (height, format!("{} out, {} in", status.outgoing_peers, status.incoming_peers))
}

fn unknown_node(reason: String) -> (String, String) {
    (format!("unknown: {reason}"), "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form_field(kind: SettingKind, current: SettingCurrent, default: Option<&str>) -> SettingField {
        SettingField {
            name: "n".into(),
            label: "N".into(),
            help: String::new(),
            required: false,
            kind,
            current,
            default: default.map(str::to_owned),
            public_name: None,
        }
    }

    #[test]
    fn node_status_reads_as_height_and_peers() {
        let mut status = NodeStatus {
            provider: "cuprated".into(),
            height: 2_121_400,
            target_height: 3_478_000,
            outgoing_peers: 32,
            incoming_peers: 9,
            synchronized: false,
        };
        assert_eq!(node_rows(&status), ("2,121,400 of 3,478,000 (60%)".into(), "32 out, 9 in".into()));
        status.target_height = 0;
        status.height = 1;
        status.synchronized = true;
        assert_eq!(node_rows(&status).0, "1 (synced)");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
    }

    #[test]
    fn uptime_reads_as_plain_english() {
        assert_eq!(duration(30), "less than a minute");
        assert_eq!(duration(60), "1 minute");
        assert_eq!(duration(150), "2 minutes");
        assert_eq!(duration(3600), "1 hour");
        assert_eq!(duration(2 * 86_400), "2 days");
    }

    #[test]
    fn fields_map_without_ever_holding_a_secret() {
        let f = field(form_field(SettingKind::Secret, SettingCurrent::SecretSet, None));
        assert_eq!((f.kind, f.value), (FieldKind::Secret, FieldValue::SecretSet));
        let f = field(form_field(SettingKind::Bool, SettingCurrent::Default, Some("true")));
        assert_eq!(f.value, FieldValue::Bool(true));
        let f = field(form_field(SettingKind::Int { min: Some(1), max: None }, SettingCurrent::Default, Some("$(a.b)")));
        assert_eq!(f.value, FieldValue::Reference { path: "a.b".into(), resolved: String::new() });
        assert_eq!(f.requirement, "Default $(a.b)");
        let f = field(form_field(
            SettingKind::Text,
            SettingCurrent::Literal(SettingLiteral::Text("$(x.y)".into())),
            None,
        ));
        assert_eq!(f.value, FieldValue::Text("$(x.y)".into()), "a stored literal stays literal");
    }
}
