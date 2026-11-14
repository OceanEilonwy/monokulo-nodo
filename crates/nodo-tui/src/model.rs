//! Screen data that `nodo-api` does not carry yet, behind the `ScreenData` trait.
//!
//! `nodo-api` stays small and only has services, unlock, settings and exposure. Everything else a
//! screen shows comes through this trait. `MockBackend` implements it today, and each method is a
//! candidate `nodo_api::Request` for the real helper.

use nodo_api::{Exposure, SettingInput, SettingLiteral, SiteId};

use crate::secret::Secret;

/// A count with its noun, singular for one: "1 package", "3 packages". Nouns here are regular.
pub fn counted(n: u64, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Two feeds can publish the same name, so a package is only identified by the pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageId {
    pub feed: String,
    pub name: String,
}

impl PackageId {
    pub fn new(feed: &str, name: &str) -> Self {
        Self { feed: feed.to_owned(), name: name.to_owned() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageStatus {
    Installed,
    Available,
    UpdateReady,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub name: String,
    pub met: bool,
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSummary {
    pub id: PackageId,
    pub status: PackageStatus,
    pub version: String,
    pub size: String,
    pub tagline: String,
    pub description: Vec<String>,
    pub needs: Vec<Need>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDetail {
    pub id: PackageId,
    pub installed: bool,
    pub signature_verified: bool,
    pub about: String,
    pub version: String,
    pub size: String,
    pub license: String,
    pub needs: Vec<Need>,
    pub public_settings: Vec<String>,
    pub asks: Vec<(String, String)>,
    /// Encrypted storage opens with the single device passphrase; there is no per-package one.
    pub encrypted_storage: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskLevel {
    Normal,
    Caution,
    Encrypted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub level: AskLevel,
    pub text: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewData {
    pub id: PackageId,
    pub version: String,
    pub third_party: bool,
    pub asks: Vec<Ask>,
    pub guarantees: Vec<String>,
    /// What confirming approves, quoted back to the helper, which installs only if the package
    /// still asks for exactly this.
    pub approval: Option<nodo_api::PermissionApproval>,
    /// Its encrypted storage is new, so confirming asks for the device passphrase.
    pub needs_passphrase: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub passed: bool,
    pub rule: String,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectionData {
    pub id: PackageId,
    pub version: String,
    pub violations: Vec<Violation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityData {
    pub id: PackageId,
    pub version: String,
    pub message: String,
    pub current_node: String,
    pub alternative_node: String,
    /// Capability name, then whether the current and the alternative node provide it.
    pub matrix: Vec<(String, bool, bool)>,
    pub compatible_note: String,
    pub switch_note: String,
}

/// What must happen before a package can install. Only `Review` leads to an install, and the
/// rejected case deliberately has no override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallGate {
    Review(ReviewData),
    Rejected(RejectionData),
    MissingCapability(CapabilityData),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Done,
    Working,
    Pending,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallStep {
    pub name: String,
    pub note: String,
    pub state: StepState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    InProgress,
    Succeeded,
    /// The install was undone, and the text says why.
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSnapshot {
    pub id: PackageId,
    pub version: String,
    pub steps: Vec<InstallStep>,
    pub details: Vec<String>,
    pub outcome: InstallOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    BuiltIn,
    Pinned,
    Changed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedStatus {
    Healthy,
    Blocked,
    Unreachable(String),
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    pub name: String,
    pub packages: u32,
    pub key_state: KeyState,
    pub checked: String,
    pub status: FeedStatus,
    pub address: String,
    pub fingerprint: String,
    pub added: String,
    pub installed_from: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedProbe {
    pub name: String,
    pub address: String,
    pub packages: u32,
    pub signed: bool,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChange {
    pub feed: String,
    pub trusted: String,
    pub now_signing: String,
    pub affected: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    Url,
    Port,
    Text,
    Number { min: Option<i64>, max: Option<i64> },
    Choice(Vec<String>),
    MoneroAddress,
    Secret,
    Toggle,
}

impl FieldKind {
    /// Whether the field can take a value picked from another app. A secret is never public, and
    /// a toggle or a choice is quicker to set by hand, so those three offer no picker.
    pub fn takes_reference(&self) -> bool {
        matches!(self, Self::Url | Self::Port | Self::Text | Self::Number { .. } | Self::MoneroAddress)
    }
}

/// A setting's value. A stored secret is only ever known as set or unset, so the backend's real
/// value never enters this process. `NewSecret` holds what the user has just typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Text(String),
    Reference { path: String, resolved: String },
    Bool(bool),
    SecretUnset,
    SecretSet,
    NewSecret(Secret),
}

impl FieldValue {
    /// The typed form sent to the helper. Typed text is always a literal, even if it looks like
    /// `$(...)`, because only the reference picker makes a reference. Stored secrets have no
    /// wire form; callers only send changes.
    pub fn to_input(&self) -> Option<SettingInput> {
        match self {
            Self::Text(text) => Some(SettingInput::Literal(SettingLiteral::Text(text.clone()))),
            Self::Reference { path, .. } => {
                let (target, setting) = path.split_once('.')?;
                Some(SettingInput::Reference { target: target.to_owned(), setting: setting.to_owned() })
            }
            Self::Bool(value) => Some(SettingInput::Literal(SettingLiteral::Bool(*value))),
            Self::NewSecret(secret) => Some(SettingInput::Secret(nodo_api::Secret::new(secret.expose()))),
            Self::SecretUnset | Self::SecretSet => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub help: String,
    pub requirement: String,
    pub kind: FieldKind,
    pub value: FieldValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsForm {
    pub package: String,
    pub fields: Vec<Field>,
    pub public_settings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceOption {
    pub path: String,
    pub value: String,
    /// Shown when the path is a role, because a role survives switching provider.
    pub role_note: Option<(String, String)>,
    pub consequences: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceList {
    pub expects: String,
    pub options: Vec<ReferenceOption>,
    pub hidden_wrong_type: u32,
    pub hidden_secrets: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackExplanation {
    pub steps: Vec<(bool, String)>,
    pub headline: String,
    pub hint: String,
    pub blamed_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateNote {
    Ready,
    RestartNeeded,
    NewPermission(String),
    Skipped,
    /// Always waits for the user, for the reason given: a changed key or failed checks.
    Blocked(String),
    /// The publisher withdrew the system snapshot this device is on, named here. Updating
    /// moves this project's packages back to the offered snapshot's versions.
    Withdrawal(String),
}

/// The row and auto-update entry for the system snapshot, next to the packages.
pub const SYSTEM_BASE: &str = "system base";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateItem {
    pub name: String,
    pub from: String,
    pub to: String,
    pub source: String,
    pub third_party: bool,
    pub note: UpdateNote,
    pub held: bool,
    pub preselected: bool,
    /// The new version needs a larger encrypted volume, as words such as "256 MB to 512 MB".
    /// Growing it needs the device passphrase, which the screen asks for before applying.
    pub grows: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateResult {
    pub name: String,
    pub applied: bool,
    pub version: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateFailure {
    pub name: String,
    pub headline: String,
    pub kept_version: String,
    pub restored: String,
    pub running: bool,
    pub log_line: String,
    pub verdict: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateReport {
    pub results: Vec<UpdateResult>,
    pub failure: Option<UpdateFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoUpdateEntry {
    pub name: String,
    pub source: String,
    pub third_party: bool,
    pub enabled: bool,
    pub note: String,
}

impl AutoUpdateEntry {
    /// The only constructor, so a package is never opted in to auto-update by omission.
    pub fn new(name: &str, source: &str, third_party: bool, note: &str) -> Self {
        Self {
            name: name.to_owned(),
            source: source.to_owned(),
            third_party,
            enabled: false,
            note: note.to_owned(),
        }
    }

    /// Unattended updates from a feed nobody vetted run unreviewed code, so say so whenever the
    /// user is looking at the choice or has already made it.
    pub fn shows_warning(&self, focused: bool) -> bool {
        self.third_party && (focused || self.enabled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutoUpdatePrefs {
    pub frequency: String,
    pub frequencies: Vec<String>,
    /// Device-local `HH:MM` times.
    pub window_start: String,
    pub window_end: String,
    pub next_run: String,
    pub packages: Vec<AutoUpdateEntry>,
}

impl AutoUpdatePrefs {
    pub fn enabled_count(&self) -> usize {
        self.packages.iter().filter(|entry| entry.enabled).count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Info,
    Error,
    Marker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub kind: LogKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogView {
    pub lines: Vec<LogLine>,
    pub suggestion: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Passed,
    Failed,
    /// Could not be determined, such as a port nobody outside tested. Never shown as a pass.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub state: CheckState,
    pub text: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveFailure {
    pub message: String,
    pub checks: Vec<Check>,
    pub waiting: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveAction {
    Retry,
    ReadOnlyCheck,
    Repair,
    Diagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeInfo {
    pub role: String,
    pub provider: String,
    pub state: String,
    pub height: String,
    pub peers: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockTarget {
    pub name: String,
    pub encrypted: bool,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionLevel {
    Info,
    Warning,
    Critical,
}

/// A line for Home's "Needs attention", from the health monitor or a failed update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attention {
    pub level: AttentionLevel,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overview {
    pub node: NodeInfo,
    pub device: Vec<(String, String)>,
    /// Service id to its role or origin, such as `monero-daemon` or `community`.
    pub service_origins: Vec<(String, String)>,
    pub drive: Option<DriveFailure>,
    pub restarted: String,
    pub unlock_targets: Vec<UnlockTarget>,
    pub updates_checked: String,
    /// First boot: no device passphrase exists yet, so setup comes before anything else.
    pub setup_needed: bool,
    pub attention: Vec<Attention>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub package: String,
    pub exposure: Exposure,
    pub local_url: String,
    pub onion_url: String,
    /// Host name, without a scheme, because this is the text the user types to go public.
    pub public_host: Option<String>,
    pub locked: bool,
    /// Rate limiting is always on for public sites, except where the proxy reports it cannot
    /// enforce it for this site.
    pub rate_limit_unenforced: bool,
    /// Local exposure is on, but the device has no local address to serve it on.
    pub local_waiting: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebOverview {
    pub sites: Vec<Site>,
    pub domain: String,
    pub provider: String,
    pub certificates: String,
    pub public_ip: String,
    pub dns_matches: bool,
    pub dns_changed: String,
    pub reachable: bool,
    /// Why local sites are not being served, and what to do about it.
    pub local_problem: Option<String>,
    pub local_remedy: Option<String>,
    /// Why the DNS records of local names under the domain lag behind the local address.
    pub local_records_note: Option<String>,
    /// `http://nodo.local`, where LAN devices get the local CA root, while it is served.
    pub trust_url: Option<String>,
    /// One row per served host, public and local.
    pub certs: Vec<CertRow>,
    pub acme: AcmeView,
}

impl WebOverview {
    pub fn cert(&self, host: &str) -> Option<&CertRow> {
        self.certs.iter().find(|c| c.host == host)
    }

    /// The certificate rows of one site, as the screens key sites.
    pub fn site_certs<'a>(&'a self, site: &'a str) -> impl Iterator<Item = &'a CertRow> + 'a {
        self.certs.iter().filter(move |c| c.site == site)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertKind {
    Public,
    /// A local site named under the owner's domain.
    LocalDomain,
    /// A `.local` name with the device's own CA.
    LocalInternal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertRow {
    pub site: String,
    pub host: String,
    pub kind: CertKind,
    pub state: nodo_api::CertState,
    pub issuer: String,
    pub days_left: Option<u64>,
    /// Such as "3 days ago".
    pub renewed: Option<String>,
    /// `HTTP-01`, `TLS-ALPN-01` or `DNS-01`.
    pub challenge: Option<String>,
    pub problem: Option<String>,
    pub problem_detail: Option<String>,
}

/// The certificate choices and what the helper picked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AcmeView {
    pub email: String,
    pub staging: bool,
    /// Set when a test ACME server replaces Let's Encrypt, to its address.
    pub test_server: Option<String>,
    /// DNS-01 through the provider, so no port is needed.
    pub dns01: bool,
    /// The method in words, or why ports are needed.
    pub challenge: String,
    /// Extra fields the provider's certificate module needs, such as GoDaddy's key and secret.
    pub dns01_fields: Vec<String>,
    pub dns01_stored: bool,
    pub local_domain: Option<String>,
    pub local_address: Option<String>,
    /// SHA-256 of the local CA root, for checking it when trusting it on a computer.
    pub local_ca: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Yes,
    Account,
    Limited,
    PortsNeeded,
    NoApi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub name: String,
    pub dynamic_dns: Capability,
    pub https_without_ports: Capability,
    pub instructions: String,
    /// The provider's credential fields, in the order the helper wants them.
    pub fields: Vec<String>,
    /// A note on certificates without open ports, such as different credentials.
    pub dns01_note: Option<String>,
    /// The provider's API lists the domains a key controls, so the owner never types the domain.
    pub lists_domains: bool,
}

/// A provider account the owner connected, with the domains its key controls.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectedProvider {
    /// The helper's id, such as `cloudflare` or `cloudflare-2`.
    pub id: String,
    pub name: String,
    pub domains: Vec<ConnectedDomain>,
    pub lists_domains: bool,
    /// Why the last look at its domains failed, or a domain a site uses that it no longer lists.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectedDomain {
    pub name: String,
    /// The apps whose internet address is under this domain.
    pub used_by: Vec<String>,
    /// Another connected provider that holds this domain and is used for it instead.
    pub held_by: Option<String>,
}

impl Provider {
    /// Providers without an API are unsupported outright. There is no guided DNS move for them.
    pub fn is_supported(&self) -> bool {
        self.dynamic_dns != Capability::NoApi
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterHelp {
    pub admin_page: String,
    pub forward_http: String,
    pub forward_https: String,
    pub device_ip: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicPlan {
    pub url: String,
    pub host: String,
    pub dns_record: String,
    pub firewall: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reachability {
    /// `manual` explains how to test the ports from outside, when nothing outside tested them.
    Checked { checks: Vec<Check>, router: Option<RouterHelp>, plan: Option<PublicPlan>, manual: Vec<String> },
    Cgnat { wan_address: String, wan_range: String, internet_address: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicAccess {
    pub require_password: bool,
}

/// Read-only text shown in an overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocKind {
    Changelog(String),
    AutoUpdateHistory,
    Versions(String),
    FileList(String),
    InstallLog(String),
    FeedPackages(String),
    ProviderKeys(String),
    RouterGuides,
    PublicIpFromIsp,
    Ipv6,
    RelayTunnel,
}

/// The state every top-level screen reads from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub services: Vec<nodo_api::ServiceStatus>,
    pub overview: Overview,
    pub catalog: Vec<PackageSummary>,
    pub feeds: Vec<Feed>,
    pub updates: Vec<UpdateItem>,
    pub web: WebOverview,
    pub providers: Vec<Provider>,
    /// Why the provider catalog could not be read, so the add flow says so instead of offering
    /// an empty list.
    pub providers_error: Option<String>,
    /// The provider accounts the owner connected.
    pub connected: Vec<ConnectedProvider>,
    pub log_sources: Vec<String>,
    pub auto_update: AutoUpdatePrefs,
    /// Areas whose backing system does not exist yet, with the reason. Their screens say so
    /// instead of showing sample data.
    pub unavailable: Vec<(Area, String)>,
    /// One line about system snapshot updates for the Updates screen, when there is anything
    /// to say: the installed snapshot, the last check, or why it failed.
    pub system_note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    Catalog,
    Feeds,
    Updates,
    AutoUpdate,
    Providers,
    Reachability,
}

impl Snapshot {
    pub fn unavailable(&self, area: Area) -> Option<&str> {
        self.unavailable.iter().find(|(a, _)| *a == area).map(|(_, reason)| reason.as_str())
    }
}

/// Screen data outside `nodo_api`. Errors are text because they are only ever shown.
pub trait ScreenData {
    fn overview(&self) -> Overview;
    fn catalog(&self) -> Vec<PackageSummary>;
    fn package_detail(&self, id: &PackageId) -> Option<PackageDetail>;
    fn install_gate(&self, id: &PackageId) -> InstallGate;
    fn start_install(
        &mut self,
        id: &PackageId,
        approval: Option<&nodo_api::PermissionApproval>,
        passphrase: Option<&Secret>,
    ) -> Result<InstallSnapshot, String>;
    fn poll_install(&mut self) -> Option<InstallSnapshot>;
    fn cancel_install(&mut self) -> Option<InstallSnapshot>;

    fn feeds(&self) -> Vec<Feed>;
    fn probe_feed(&self, address: &str) -> Result<FeedProbe, String>;
    fn trust_feed(&mut self, probe: &FeedProbe) -> Result<(), String>;
    fn refresh_feeds(&mut self);
    fn toggle_feed(&mut self, name: &str);
    fn remove_feed(&mut self, name: &str);
    fn key_change(&self, feed: &str) -> Option<KeyChange>;
    /// `typed` is the feed name as the user typed it. The helper checks it, and both keys.
    fn trust_new_key(&mut self, change: &KeyChange, typed: &str) -> Result<(), String>;

    fn settings_form(&self, package: &str) -> Option<SettingsForm>;
    fn references(&self, package: &str, field: &str) -> ReferenceList;
    fn explain_rollback(&self, package: &str, reason: &str) -> RollbackExplanation;
    /// The helper's name for a site the screens know by one string. Sites are keyed by package
    /// in the mock, where every package serves one site.
    fn site_id(&self, key: &str) -> SiteId {
        SiteId { package: key.to_owned(), site: key.to_owned() }
    }

    fn updates(&self) -> Vec<UpdateItem>;
    fn check_updates(&mut self);
    fn update_review(&self, name: &str) -> Option<ReviewData>;
    /// `approvals` are the reviews the user confirmed, for updates that ask for more.
    fn apply_updates(&mut self, names: &[String], approvals: &[(String, nodo_api::PermissionApproval)]) -> UpdateReport;
    /// The same, with the device passphrase, for updates that grow encrypted storage.
    fn apply_updates_with_passphrase(
        &mut self,
        names: &[String],
        approvals: &[(String, nodo_api::PermissionApproval)],
        _passphrase: &Secret,
    ) -> UpdateReport {
        self.apply_updates(names, approvals)
    }
    /// The line about system snapshot updates, read with the updates.
    fn system_note(&self) -> Option<String> {
        None
    }
    fn set_held(&mut self, name: &str, held: bool);
    fn auto_update(&self) -> AutoUpdatePrefs;
    fn save_auto_update(&mut self, prefs: &AutoUpdatePrefs) -> Result<(), String>;
    /// Returns the text to show the user about where the report went.
    fn copy_report(&mut self, subject: &str) -> Result<String, String>;

    fn logs(&self, source: &str) -> LogView;
    fn log_sources(&self) -> Vec<String>;
    fn drive_action(&mut self, action: DriveAction) -> Result<Document, String>;

    fn web(&self) -> WebOverview;
    /// The companies a provider account can be added at, or why they could not be read.
    fn providers(&self) -> Result<Vec<Provider>, String>;
    /// The provider accounts connected, with their domains.
    fn connected_providers(&self) -> Vec<ConnectedProvider>;
    /// Connects one more provider account. The helper lists the domains the key controls, which also
    /// proves the key works; `domain` is only for a provider that cannot list them.
    fn add_provider(
        &mut self,
        provider: &str,
        domain: Option<&str>,
        api_key: &Secret,
        secret_key: &Secret,
    ) -> Result<ConnectedProvider, String>;
    /// Refused by the helper while an app's internet address is under one of its domains.
    fn remove_provider(&mut self, id: &str) -> Result<(), String>;
    /// Looks at the provider's domains again.
    fn refresh_provider(&mut self, id: &str) -> Result<ConnectedProvider, String>;
    fn reachability(&mut self, site: &str) -> Reachability;
    fn set_public_host(&mut self, site: &str, host: &str) -> Result<(), String>;
    fn set_public_access(&mut self, site: &str, access: &PublicAccess) -> Result<(), String>;
    fn save_acme(&mut self, _settings: &nodo_api::AcmeSettingsInput) -> Result<(), String> {
        Err("Certificate settings are not available here.".to_owned())
    }
    /// The provider's separate certificate credentials, as two fields such as key and secret.
    fn save_dns01(&mut self, _first: &Secret, _second: &Secret) -> Result<(), String> {
        Err("Certificate settings are not available here.".to_owned())
    }
    fn check_local_names(&mut self) -> Result<Vec<nodo_api::LocalNameCheck>, String> {
        Err("Checking local names is not available here.".to_owned())
    }

    fn document(&self, kind: &DocKind) -> Document;

    fn ssh_keys(&self) -> Result<Vec<nodo_api::SshKey>, String>;
    fn add_ssh_key(&mut self, line: &str) -> Result<(), String>;
    fn remove_ssh_key(&mut self, fingerprint: &str) -> Result<(), String>;
    /// See [`Snapshot::unavailable`]. Read after the other snapshot calls.
    fn unavailable(&self) -> Vec<(Area, String)> {
        Vec::new()
    }

    fn backups(&self) -> Result<Vec<nodo_api::BackupInfo>, String> {
        Err("Backups are not available here.".to_owned())
    }
    fn create_backup(
        &mut self,
        _device_passphrase: Option<&Secret>,
        _backup_passphrase: &Secret,
    ) -> Result<nodo_api::BackupCreated, String> {
        Err("Backups are not available here.".to_owned())
    }
    fn inspect_backup(&mut self, _name: &str, _passphrase: &Secret) -> Result<nodo_api::BackupContents, String> {
        Err("Backups are not available here.".to_owned())
    }
    fn restore_backup(
        &mut self,
        _name: &str,
        _passphrase: &Secret,
        _new_device_passphrase: Option<&Secret>,
        _typed: &str,
    ) -> Result<nodo_api::RestoreReport, String> {
        Err("Backups are not available here.".to_owned())
    }

    fn usb_drives(&self) -> Result<Vec<nodo_api::UsbDrive>, String> {
        Err("USB drives are not available here.".to_owned())
    }
    fn usb_backups(&self, _device: &str) -> Result<nodo_api::UsbBackupList, String> {
        Err("USB drives are not available here.".to_owned())
    }
    fn create_usb_backup(
        &mut self,
        _device: &str,
        _device_passphrase: Option<&Secret>,
        _backup_passphrase: &Secret,
    ) -> Result<nodo_api::BackupCreated, String> {
        Err("USB drives are not available here.".to_owned())
    }
    fn inspect_usb_backup(
        &mut self,
        _device: &str,
        _name: &str,
        _passphrase: &Secret,
    ) -> Result<nodo_api::BackupContents, String> {
        Err("USB drives are not available here.".to_owned())
    }
    fn restore_usb_backup(
        &mut self,
        _device: &str,
        _name: &str,
        _passphrase: &Secret,
        _new_device_passphrase: Option<&Secret>,
        _typed: &str,
    ) -> Result<nodo_api::RestoreReport, String> {
        Err("USB drives are not available here.".to_owned())
    }
}
