//! Screen data that `nodo-api` does not carry yet, behind the `ScreenData` trait.
//!
//! `nodo-api` stays small and only has services, unlock, settings and exposure. Everything else a
//! screen shows comes through this trait. `MockBackend` implements it today, and each method is a
//! candidate `nodo_api::Request` for the real helper.

use nodo_api::{Exposure, SettingInput, SettingLiteral, SiteId};

use crate::secret::Secret;

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
    Choice(Vec<String>),
    MoneroAddress,
    Secret,
    Toggle,
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
}

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
    Checked { checks: Vec<Check>, router: Option<RouterHelp>, plan: Option<PublicPlan> },
    Cgnat { wan_address: String, wan_range: String, internet_address: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicAccess {
    pub require_password: bool,
    pub rate_limit: bool,
    pub hide_admin: bool,
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
    pub log_sources: Vec<String>,
    pub auto_update: AutoUpdatePrefs,
}

/// Screen data outside `nodo_api`. Errors are text because they are only ever shown.
pub trait ScreenData {
    fn overview(&self) -> Overview;
    fn catalog(&self) -> Vec<PackageSummary>;
    fn package_detail(&self, id: &PackageId) -> Option<PackageDetail>;
    fn install_gate(&self, id: &PackageId) -> InstallGate;
    fn start_install(&mut self, id: &PackageId) -> Result<InstallSnapshot, String>;
    fn poll_install(&mut self) -> Option<InstallSnapshot>;
    fn cancel_install(&mut self) -> Option<InstallSnapshot>;

    fn feeds(&self) -> Vec<Feed>;
    fn probe_feed(&self, address: &str) -> Result<FeedProbe, String>;
    fn trust_feed(&mut self, probe: &FeedProbe) -> Result<(), String>;
    fn refresh_feeds(&mut self);
    fn toggle_feed(&mut self, name: &str);
    fn remove_feed(&mut self, name: &str);
    fn key_change(&self, feed: &str) -> Option<KeyChange>;
    fn trust_new_key(&mut self, feed: &str) -> Result<(), String>;

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
    fn apply_updates(&mut self, names: &[String]) -> UpdateReport;
    fn set_held(&mut self, name: &str, held: bool);
    fn auto_update(&self) -> AutoUpdatePrefs;
    fn save_auto_update(&mut self, prefs: &AutoUpdatePrefs) -> Result<(), String>;
    /// Returns the text to show the user about where the report went.
    fn copy_report(&mut self, subject: &str) -> Result<String, String>;

    fn logs(&self, source: &str) -> LogView;
    fn log_sources(&self) -> Vec<String>;
    fn drive_action(&mut self, action: DriveAction) -> Result<Document, String>;

    fn web(&self) -> WebOverview;
    fn providers(&self) -> Vec<Provider>;
    fn test_provider(
        &mut self,
        provider: &str,
        domain: &str,
        api_key: &Secret,
        secret_key: &Secret,
    ) -> Result<String, String>;
    fn save_provider(
        &mut self,
        provider: &str,
        domain: &str,
        api_key: &Secret,
        secret_key: &Secret,
    ) -> Result<(), String>;
    fn reachability(&mut self, site: &str) -> Reachability;
    fn set_public_host(&mut self, site: &str, host: &str) -> Result<(), String>;
    fn set_public_access(&mut self, site: &str, access: &PublicAccess) -> Result<(), String>;

    fn document(&self, kind: &DocKind) -> Document;
}
