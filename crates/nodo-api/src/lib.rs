//! The narrow interface between the unprivileged TUI and the root helper.
//!
//! The helper only performs the actions listed in `Request`. Nothing here accepts a shell
//! command, a file path to write, or arbitrary system configuration.
//!
//! Postcard identifies enum variants and struct fields by position. Any change other than adding
//! a variant at the end of an enum needs a new [`PROTOCOL_VERSION`].

#![forbid(unsafe_code)]

pub mod client;
pub mod protocol;
mod secret;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use secret::Secret;

/// Version 2 replaced plain passphrase strings with [`Secret`], typed setting values, structured
/// rollbacks and confirmed exposure changes.
pub const PROTOCOL_VERSION: u8 = 2;

/// The helper's socket on the device.
pub const DEFAULT_SOCKET: &str = "/run/nodo/helper.sock";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceState {
    Running,
    Stopped,
    Failed,
    /// Waiting for the device passphrase.
    Locked,
    /// Waiting for a dependency or data drive.
    Waiting,
}

/// Where a service's package came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// Part of the signed base image.
    System,
    /// Installed from a user-added feed. `None` when the installer has not recorded which one.
    Feed(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub id: String,
    pub state: ServiceState,
    pub detail: String,
    /// Roles the package provides, such as `monero-daemon`.
    pub roles: Vec<String>,
    pub origin: Origin,
}

/// Exposures are independent, so a site can use any combination.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exposure {
    pub local: bool,
    pub onion: bool,
    pub public: bool,
}

/// A package can serve several sites, so a site is named by both.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SiteId {
    pub package: String,
    pub site: String,
}

/// Proof that the user typed the public host name to confirm going public. The helper compares
/// it with the host it has stored for the site, so a client cannot skip the step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicConfirmation {
    pub typed_host: String,
}

/// Proof that the user saw a read-only check find damage and chose to repair. `check_id` must
/// name the helper's most recent check, which must be recent and must have found problems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairConfirmation {
    pub check_id: u64,
    pub accept_data_loss_risk: bool,
}

/// A literal setting value. Text is never scanned for references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingLiteral {
    Text(String),
    Int(i64),
    Bool(bool),
}

/// A new value for one setting. A reference and a literal can never be confused, whatever the
/// text holds, because they are different variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingInput {
    Literal(SettingLiteral),
    /// `$(target.setting)`, where `target` is a package id or a role.
    Reference {
        target: String,
        setting: String,
    },
    Secret(Secret),
    /// Go back to the package's default.
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingChange {
    pub name: String,
    pub value: SettingInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingKind {
    Text,
    Int { min: Option<i64>, max: Option<i64> },
    Bool,
    Choice(Vec<String>),
    MoneroAddress,
    Url,
    Secret,
}

/// What a setting holds now. Secrets are only ever reported as set or unset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingCurrent {
    /// No value stored, so the default applies, if there is one.
    Default,
    Literal(SettingLiteral),
    Reference {
        target: String,
        setting: String,
        /// The value it resolves to now, unless it is a secret or does not resolve.
        resolved: Option<String>,
    },
    SecretSet,
    SecretUnset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingField {
    pub name: String,
    pub label: String,
    pub help: String,
    pub required: bool,
    pub kind: SettingKind,
    pub current: SettingCurrent,
    /// The default as text, or a `$(...)` reference. Never present for a secret.
    pub default: Option<String>,
    /// The name other packages reference this setting by, when it is public.
    pub public_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsForm {
    pub package: String,
    pub fields: Vec<SettingField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RollbackStage {
    Restart,
    HealthCheck,
}

/// New settings were applied, failed, and were taken back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollback {
    pub stage: RollbackStage,
    pub reason: String,
    /// Whether the service is healthy again on its old settings.
    pub recovered: bool,
    /// Set when it is not, saying what went wrong the second time.
    pub recovery_problem: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncryptionState {
    /// First boot: no device passphrase exists yet.
    NotSetUp,
    Locked,
    Unlocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceOverview {
    pub hostname: String,
    pub kernel: String,
    pub uptime_secs: u64,
    pub memory_total_kib: u64,
    pub memory_available_kib: u64,
    pub disk_total_bytes: u64,
    pub disk_free_bytes: u64,
    pub encryption: EncryptionState,
    /// Whether any SSH key may log in. The setup screen offers to add one when none can.
    pub ssh_keys: u32,
    /// The data drive, when the board has one.
    pub drive: Option<DriveStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInfo {
    pub id: String,
    pub version: String,
    pub origin: Origin,
    pub roles: Vec<String>,
    pub needs: Vec<String>,
    pub encrypted_storage: bool,
    pub has_settings: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageDetail {
    pub info: PackageInfo,
    pub sites: Vec<String>,
    /// Inbound ports, as `18080/tcp lan`.
    pub inbound: Vec<String>,
    pub outbound_internet: bool,
    pub public_settings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedInfo {
    pub name: String,
    pub address: String,
    pub fingerprint: String,
    pub enabled: bool,
    pub packages: u32,
    pub key_changed: bool,
    pub last_checked: Option<String>,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub package: String,
    pub from: String,
    pub to: String,
    pub feed: String,
    pub held: bool,
    pub needs_new_permissions: bool,
    pub needs_restart: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoUpdatePrefs {
    pub frequency: String,
    /// Device-local `HH:MM`.
    pub window_start: String,
    pub window_end: String,
    /// Packages opted in. Everything else is off, which is the default.
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogChunk {
    pub source: String,
    pub lines: Vec<String>,
    /// More lines exist before the first one returned.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveStatus {
    pub label: String,
    pub mount_point: String,
    pub present: bool,
    pub mounted: bool,
    pub filesystem: Option<String>,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveReport {
    /// Quote this in a [`RepairConfirmation`].
    pub check_id: u64,
    pub clean: bool,
    pub output: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteInfo {
    pub id: SiteId,
    pub exposure: Exposure,
    pub multi_address: bool,
    pub public_host: Option<String>,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachabilityCheck {
    pub passed: bool,
    pub text: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReachabilityReport {
    Checked {
        checks: Vec<ReachabilityCheck>,
    },
    /// The ISP shares one public address between customers, so inbound connections cannot work.
    Cgnat {
        wan_address: String,
        internet_address: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub name: String,
    pub dynamic_dns: bool,
    pub configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshKey {
    pub algorithm: String,
    /// `SHA256:` and the unpadded base64 digest, as `ssh-keygen -l` prints it.
    pub fingerprint: String,
    pub comment: String,
}

/// What the node's RPC reports about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeStatus {
    /// The package providing `monero-daemon`.
    pub provider: String,
    pub height: u64,
    /// The height the network is at, or 0 when the node does not know yet.
    pub target_height: u64,
    pub outgoing_peers: u32,
    pub incoming_peers: u32,
    pub synchronized: bool,
}

/// A role, the installed packages providing it, and the user's choice among them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleBinding {
    pub role: String,
    pub providers: Vec<String>,
    pub bound: Option<String>,
}

/// Approval for `consumer` to reference a secret or non-public `setting` of `provider`. The
/// provider is always a package id, never a role, so a role moving does not carry approval.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SettingGrant {
    pub consumer: String,
    pub provider: String,
    pub setting: String,
}

/// One site's addresses as the proxy serves them now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSiteStatus {
    pub id: SiteId,
    pub local_url: Option<String>,
    pub onion_url: Option<String>,
    pub public_url: Option<String>,
    /// Local exposure is on, but there is no local address to serve it on.
    pub local_waiting: bool,
    /// The site asked for rate limiting and the proxy cannot enforce it.
    pub rate_limit_unenforced: bool,
}

/// What the Web screens show beyond the saved exposure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebStatus {
    pub sites: Vec<WebSiteStatus>,
    /// The second LAN address local sites use.
    pub local_address: Option<String>,
    /// Why local sites are not being served, when some are waiting.
    pub local_unavailable: Option<String>,
    pub local_remedy: Option<String>,
    pub domain: Option<String>,
    pub provider: Option<String>,
    pub ddns_running: bool,
    pub public_ip: Option<String>,
    /// Unix seconds of the last DNS change the dynamic DNS service made.
    pub dns_changed_at: Option<u64>,
    /// Records the dynamic DNS service published for names no longer in use.
    pub stale_records: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dns01Support {
    Bundled,
    BundledSeparateCredentials(String),
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsProviderDetail {
    pub id: String,
    pub name: String,
    pub dynamic_dns: bool,
    /// Why dynamic DNS is not available, when it is not.
    pub unavailable_reason: Option<String>,
    pub dns01: Dns01Support,
    pub caveat: Option<String>,
    /// The credential fields `SetDnsProvider` expects, in order.
    pub fields: Vec<String>,
    pub configured: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckStatus {
    Passed,
    Failed,
    /// Could not be determined. Never shown as a pass.
    Unknown,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachabilityItem {
    pub id: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachabilityDetail {
    pub checks: Vec<ReachabilityItem>,
    /// `ready`, `ports_unverified`, `needs_port_forward`, `blocked_by_cgnat` or `failed`.
    pub verdict: String,
    pub public_ip: Option<String>,
    pub router_wan: Option<String>,
    pub device_ip: Option<String>,
    pub gateway: Option<String>,
    /// Steps for testing the ports from outside by hand, when nothing outside tested them.
    pub manual_check: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    ListServices,
    StartService {
        id: String,
    },
    StopService {
        id: String,
    },
    RestartService {
        id: String,
    },
    /// Opens all encrypted storage with the single device passphrase.
    Unlock {
        passphrase: Secret,
    },
    /// Validates, applies, health checks, and rolls back on failure.
    ApplySettings {
        package: String,
        changes: Vec<SettingChange>,
    },
    /// Turning `public` on needs `confirmation`. Turning anything off never does.
    SetExposure {
        site: SiteId,
        exposure: Exposure,
        confirmation: Option<PublicConfirmation>,
    },
    /// First boot. Creates the device key, and refuses if one exists.
    InitEncryption {
        passphrase: Secret,
    },
    /// Rewraps the device key. Volumes keep their keys, so nothing is reformatted.
    ChangePassphrase {
        current: Secret,
        new: Secret,
    },
    DeviceOverview,
    ListPackages,
    PackageDetail {
        id: String,
    },
    SettingsForm {
        package: String,
    },
    ListFeeds,
    ListUpdates,
    GetAutoUpdate,
    SetAutoUpdate(AutoUpdatePrefs),
    LogSources,
    ReadLog {
        source: String,
        max_lines: u32,
    },
    DriveStatus,
    /// Read-only. Its report's id is what a repair must quote.
    CheckDrive,
    RepairDrive(RepairConfirmation),
    ListSites,
    /// The host the user brought for a site. Going public must then quote it.
    SetPublicHost {
        site: SiteId,
        host: String,
    },
    CheckReachability {
        site: SiteId,
    },
    ListProviders,
    /// Creates the service user, writes the generated service and enables it. The manifest must
    /// already be in a root-owned manifest directory. A package with encrypted storage needs the
    /// device passphrase the first time, to create its volume.
    InstallService {
        id: String,
        passphrase: Option<Secret>,
    },
    ListSshKeys,
    AddSshKey {
        line: String,
    },
    RemoveSshKey {
        fingerprint: String,
    },
    /// Mounts the data drive as `/etc/fstab` describes it, for retrying after a repair.
    MountDrive,
    /// Height and peers of the node providing `monero-daemon`, from its RPC.
    NodeStatus,
    ListRoleBindings,
    /// Chooses which package provides a role, or `None` to clear the choice. Every package
    /// referencing the role is re-rendered and restarted, and all of it rolls back together.
    SetRoleBinding {
        role: String,
        package: Option<String>,
    },
    ListGrants,
    /// Approves or revokes one package reading another's secret or non-public setting. The
    /// consumer is re-rendered and restarted, and rolls back on failure.
    SetGrant {
        grant: SettingGrant,
        approved: bool,
    },
    /// Keeps a package's data on the data drive. For encrypted storage this is where its volume
    /// file goes. Only before the package is installed, since nothing is moved.
    UseDataDrive {
        package: String,
        enabled: bool,
    },
    /// Addresses, the local address, the domain and dynamic DNS, as the proxy has them now.
    WebStatus,
    /// The provider catalog with each provider's dynamic DNS and DNS-01 support.
    ListDnsProviders,
    /// Stores the domain and provider credentials, root-only, and restarts dynamic DNS if a site
    /// is public. `fields` follow [`DnsProviderDetail::fields`].
    SetDnsProvider {
        domain: String,
        provider: String,
        fields: Vec<(String, Secret)>,
    },
    /// Reads and writes a temporary record with the stored credentials, then removes it.
    TestDnsProvider,
    /// Deletes DNS records that dynamic DNS published for names no longer in use. Never automatic.
    RetireStaleRecords,
    /// Like `CheckReachability`, with unknown results kept apart from passes and the manual check.
    CheckReachabilityDetail {
        site: SiteId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Services(Vec<ServiceStatus>),
    /// Settings were restored because the service failed to restart or its health check.
    RolledBack(Rollback),
    Overview(DeviceOverview),
    Packages(Vec<PackageInfo>),
    PackageDetail(PackageDetail),
    SettingsForm(SettingsForm),
    Feeds(Vec<FeedInfo>),
    Updates(Vec<UpdateInfo>),
    AutoUpdate(AutoUpdatePrefs),
    LogSources(Vec<String>),
    Log(LogChunk),
    Drive(Option<DriveStatus>),
    DriveReport(DriveReport),
    Sites(Vec<SiteInfo>),
    Reachability(ReachabilityReport),
    Providers(Vec<ProviderInfo>),
    SshKeys(Vec<SshKey>),
    NodeStatus(NodeStatus),
    RoleBindings(Vec<RoleBinding>),
    Grants(Vec<SettingGrant>),
    WebStatus(WebStatus),
    DnsProviders(Vec<DnsProviderDetail>),
    /// What a provider test did, for the screen.
    ProviderTest(String),
    RetiredRecords(Vec<String>),
    ReachabilityDetail(ReachabilityDetail),
}

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiError {
    #[error("this user may not perform that action")]
    Denied,
    #[error("that passphrase did not unlock this device")]
    BadPassphrase,
    #[error("unknown package or service: {0}")]
    NotFound(String),
    #[error("{0}")]
    Failed(String),
    /// The backing system does not exist yet. Clients show this rather than any placeholder data.
    #[error("not available yet: {0}")]
    NotImplemented(String),
    #[error("too many attempts, try again in {retry_after_secs} seconds")]
    RateLimited { retry_after_secs: u64 },
    /// The action needs a confirmation the request did not carry, or carried wrongly.
    #[error("{0}")]
    NeedsConfirmation(String),
    #[error("encrypted storage has not been set up")]
    NotSetUp,
    /// The action needs secrets or storage that open only with the device passphrase.
    #[error("locked: {0}")]
    Locked(String),
}

/// Implemented by the helper client and by a mock for TUI development and tests.
pub trait Backend {
    fn call(&mut self, request: Request) -> Result<Response, ApiError>;
}

/// The name of a request, for logs. Logging the request itself would write secrets and setting
/// values into them.
pub fn action_name(request: &Request) -> &'static str {
    match request {
        Request::ListServices => "list-services",
        Request::StartService { .. } => "start-service",
        Request::StopService { .. } => "stop-service",
        Request::RestartService { .. } => "restart-service",
        Request::Unlock { .. } => "unlock",
        Request::ApplySettings { .. } => "apply-settings",
        Request::SetExposure { .. } => "set-exposure",
        Request::InitEncryption { .. } => "init-encryption",
        Request::ChangePassphrase { .. } => "change-passphrase",
        Request::DeviceOverview => "device-overview",
        Request::ListPackages => "list-packages",
        Request::PackageDetail { .. } => "package-detail",
        Request::SettingsForm { .. } => "settings-form",
        Request::ListFeeds => "list-feeds",
        Request::ListUpdates => "list-updates",
        Request::GetAutoUpdate => "get-auto-update",
        Request::SetAutoUpdate(_) => "set-auto-update",
        Request::LogSources => "log-sources",
        Request::ReadLog { .. } => "read-log",
        Request::DriveStatus => "drive-status",
        Request::CheckDrive => "check-drive",
        Request::RepairDrive(_) => "repair-drive",
        Request::ListSites => "list-sites",
        Request::SetPublicHost { .. } => "set-public-host",
        Request::CheckReachability { .. } => "check-reachability",
        Request::ListProviders => "list-providers",
        Request::InstallService { .. } => "install-service",
        Request::ListSshKeys => "list-ssh-keys",
        Request::AddSshKey { .. } => "add-ssh-key",
        Request::RemoveSshKey { .. } => "remove-ssh-key",
        Request::MountDrive => "mount-drive",
        Request::NodeStatus => "node-status",
        Request::ListRoleBindings => "list-role-bindings",
        Request::SetRoleBinding { .. } => "set-role-binding",
        Request::ListGrants => "list-grants",
        Request::SetGrant { .. } => "set-grant",
        Request::UseDataDrive { .. } => "use-data-drive",
        Request::WebStatus => "web-status",
        Request::ListDnsProviders => "list-dns-providers",
        Request::SetDnsProvider { .. } => "set-dns-provider",
        Request::TestDnsProvider => "test-dns-provider",
        Request::RetireStaleRecords => "retire-stale-records",
        Request::CheckReachabilityDetail { .. } => "check-reachability-detail",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_secrets() {
        let requests = [
            Request::Unlock {
                passphrase: Secret::new("hunter2"),
            },
            Request::InitEncryption {
                passphrase: Secret::new("hunter2"),
            },
            Request::ChangePassphrase {
                current: Secret::new("hunter2"),
                new: Secret::new("hunter3"),
            },
            Request::InstallService {
                id: "a".into(),
                passphrase: Some(Secret::new("hunter2")),
            },
            Request::ApplySettings {
                package: "a".into(),
                changes: vec![SettingChange {
                    name: "k".into(),
                    value: SettingInput::Secret(Secret::new("hunter2")),
                }],
            },
        ];
        for request in requests {
            let text = format!("{request:?}");
            assert!(!text.contains("hunter"), "{text}");
            assert!(!action_name(&request).contains("hunter"));
        }
    }
}
