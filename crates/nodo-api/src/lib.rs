//! The narrow interface between the unprivileged TUI and the root helper.
//!
//! The helper only performs the actions listed in `Request`. Nothing here accepts a shell
//! command, a file path to write, or arbitrary system configuration.
//!
//! Postcard identifies enum variants and struct fields by position. Any change other than adding
//! a variant at the end of an enum needs a new [`PROTOCOL_VERSION`].

#![forbid(unsafe_code)]

pub mod client;
mod manage;
pub mod protocol;
mod radio;
mod secret;
mod system;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use manage::*;
pub use radio::*;
pub use secret::Secret;
pub use system::*;

/// Version 2 replaced plain passphrase strings with [`Secret`], typed setting values, structured
/// rollbacks and confirmed exposure changes. Version 3 added [`ServiceStatus::uptime_secs`].
pub const PROTOCOL_VERSION: u8 = 3;

/// The helper's socket on the device.
pub const DEFAULT_SOCKET: &str = "/run/nodo/helper.sock";
/// The touchscreen's own socket. Only its account may connect, and the helper limits what it may do.
pub const SCREEN_SOCKET: &str = "/run/nodo/screen.sock";

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
    /// How long a running service has been up, in seconds, when the supervisor reports it. The
    /// interface words it, so `detail` does not repeat it.
    pub uptime_secs: Option<u64>,
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

/// A DNS provider account the owner connected. The owner can connect several, and each site's
/// public address is under one of their domains. Credentials stay sealed in the helper and are never
/// sent to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainProvider {
    /// Stable for the life of the connection, such as `cloudflare` or `cloudflare-2`.
    pub id: String,
    /// The catalog id, see [`DnsProviderDetail::id`].
    pub kind: String,
    pub name: String,
    pub domains: Vec<ProviderDomain>,
    /// Whether the provider's API lists the domains a key controls. When it cannot, the owner adds
    /// each domain by name and the helper proves the key works for it.
    pub lists_domains: bool,
    /// Unix seconds of the last successful listing.
    pub checked_at: Option<u64>,
    /// Why the last listing failed, already scrubbed of credentials.
    pub problem: Option<String>,
    pub dns01: Dns01Support,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDomain {
    pub name: String,
    /// The sites whose public address is under this domain.
    pub used_by: Vec<SiteId>,
    /// Another connected provider that holds this domain too. Records and certificates go through the
    /// provider that was connected first, so here the domain is shown but cannot be picked.
    pub held_by: Option<String>,
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
    /// The provider's API lists the domains a key controls, so `AddDomainProvider` needs no domain.
    #[serde(default)]
    pub lists_domains: bool,
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

/// What the owner chooses about certificates. Everything else is chosen by the helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeSettingsInput {
    /// Where Let's Encrypt sends expiry notices. Optional, but without it nobody hears of a
    /// renewal that keeps failing.
    pub email: Option<String>,
    /// Let's Encrypt's staging server, whose certificates browsers do not trust.
    pub staging: bool,
    /// Names local sites `<site>.<local_domain>` with a real certificate, instead of `.local`.
    pub local_domain: Option<String>,
}

/// How public certificates are validated. The helper picks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChallengeChoice {
    /// A TXT record through the DNS provider's API. No inbound port is needed.
    Dns01 { provider: String },
    /// HTTP-01 or TLS-ALPN-01, which need ports 80 and 443 forwarded. `reason` says why DNS-01
    /// is not used.
    PortsNeeded { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeSettingsInfo {
    pub email: Option<String>,
    pub staging: bool,
    /// The ACME directory in use.
    pub directory: String,
    /// A test ACME server replaces Let's Encrypt on this device.
    pub test_server: bool,
    pub challenge: ChallengeChoice,
    /// The fields `SetDns01Credentials` takes for the connected provider. Empty when its
    /// certificate module uses the dynamic DNS credentials.
    pub dns01_fields: Vec<String>,
    pub dns01_stored: bool,
    pub local_domain: Option<String>,
    /// The second LAN address that local names point at.
    pub local_address: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CertState {
    /// Nothing has been tried yet.
    Waiting,
    Obtaining,
    /// A certificate from the configured issuer is in place and covers the host.
    Valid,
    Failed,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CertProblemKind {
    ChallengeFailed { challenges: Vec<String> },
    RateLimited,
    DnsNotPointingHere,
    CaaBlocked,
    DnsProvider,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertProblem {
    /// Unix seconds.
    pub at: u64,
    pub kind: CertProblemKind,
    pub summary: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostKind {
    Public,
    /// A local site named under the owner's domain, with a public certificate.
    LocalDomain,
    /// A `.local` name with this device's own CA.
    LocalInternal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCertificate {
    pub site: SiteId,
    pub host: String,
    pub kind: HostKind,
    pub state: CertState,
    pub issuer: Option<String>,
    /// Unix seconds.
    pub not_after: Option<u64>,
    pub last_renewal: Option<u64>,
    /// `http-01`, `tls-alpn-01` or `dns-01`, as last tried.
    pub challenge: Option<String>,
    pub last_error: Option<CertProblem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateReport {
    pub hosts: Vec<HostCertificate>,
    pub challenge: ChallengeChoice,
    pub staging: bool,
    pub test_server: bool,
    /// SHA-256 of the local CA's root, for checking it when installing it on a computer.
    pub local_ca_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LocalNameVerdict {
    Resolves,
    NotPublished,
    /// The provider has the record, but this network's resolver hides it. Usually a router's DNS
    /// rebinding protection, which drops private addresses from public answers.
    Blocked,
    WrongAddress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalNameCheck {
    pub host: String,
    /// The local address the record should hold.
    pub address: String,
    /// The addresses the provider has for the name.
    pub published: Vec<String>,
    /// What this device's resolver answered.
    pub resolves_to: Vec<String>,
    pub verdict: LocalNameVerdict,
    pub explanation: String,
}

/// Local sites as applied now, for the Web screens.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalSiteStatus {
    /// The address Caddy, mDNS and the firewall serve local sites on.
    pub address: Option<String>,
    /// `http://nodo.local`, while a `.local` site uses the local CA and the page is served.
    pub trust_url: Option<String>,
    /// SHA-256 of the local CA root that the trust page serves.
    pub ca_fingerprint: Option<String>,
    /// Why the DNS records of local names under the owner's domain do not point at the address,
    /// such as the device being locked since the address changed.
    pub records_note: Option<String>,
}

/// The node's own onion service, which serves only its restricted RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeOnion {
    /// The package providing `monero-daemon`.
    pub provider: String,
    pub enabled: bool,
    /// Whether the node serves its restricted RPC, the only RPC an onion address ever reaches.
    pub restricted_rpc: bool,
    pub port: u16,
    /// `host.onion:port`, once Tor has made the address.
    pub address: Option<String>,
}

/// One site's onion exposure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteOnion {
    pub id: SiteId,
    pub exposure: Exposure,
    pub multi_address: bool,
    pub locked: bool,
    /// `http://host.onion`, once Tor has made the address.
    pub url: Option<String>,
}

/// Tor exposure for the node and every site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnionStatus {
    pub tor_installed: bool,
    pub tor_running: bool,
    /// `None` when no node is installed.
    pub node: Option<NodeOnion>,
    pub sites: Vec<SiteOnion>,
}

/// A package the helper can take back to the version it ran before its last update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackOption {
    pub package: String,
    pub current: String,
    /// The version a rollback installs. Quote it in [`Request::RollbackPackage`].
    pub previous: String,
    /// Why the rollback cannot run now, such as data already converted to a newer layout.
    pub blocked: Option<String>,
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
    /// Reads a feed's index and reports its signing key. Trusts nothing and stores nothing.
    ProbeFeed {
        address: String,
    },
    /// Pins the feed's key. `fingerprint` is the key the user saw and compared, and the helper
    /// refuses if the feed is now signed by another.
    AddFeed {
        address: String,
        fingerprint: String,
    },
    RefreshFeeds,
    SetFeedEnabled {
        name: String,
        enabled: bool,
    },
    /// Installed packages stay, without updates.
    RemoveFeed {
        name: String,
    },
    FeedKeyChange {
        name: String,
    },
    TrustFeedKey {
        name: String,
        confirmation: KeyChangeConfirmation,
    },
    /// Every package the enabled feeds offer.
    Catalog,
    /// Downloads, verifies and inspects a package, and reports what it asks for. Installs nothing.
    ReviewInstall {
        feed: String,
        package: String,
    },
    /// Installs exactly the reviewed version, only if it asks for exactly the approved
    /// permissions. A package with encrypted storage needs the device passphrase the first time.
    InstallPackage {
        feed: String,
        package: String,
        approval: PermissionApproval,
        passphrase: Option<Secret>,
    },
    /// Removes an app installed from a feed: stops it and removes its files, manifest and the
    /// record that it is installed. Its data volumes stay on the drive, so installing it again
    /// brings them back. Refused while any of its sites is online.
    RemovePackage {
        package: String,
    },
    UpdateStatus,
    /// Refreshes the feeds and inspects every newer version, so permission changes are known.
    CheckUpdates,
    /// What an update asks for beyond what was approved.
    ReviewUpdate {
        package: String,
    },
    ApplyUpdates {
        updates: Vec<UpdateApproval>,
    },
    SetHold {
        package: String,
        version: String,
        held: bool,
    },
    /// Clears a failed update from Home.
    DismissUpdate {
        package: String,
    },
    /// One scheduled run of automatic updates. Only root, which is the scheduler, may ask.
    RunAutoUpdate,
    Health,
    ListBackups,
    /// Writes an encrypted backup into the helper's backup directory. The device passphrase is
    /// needed to include the device key, without which encrypted storage cannot be restored.
    CreateBackup {
        device_passphrase: Option<Secret>,
        backup_passphrase: Secret,
    },
    /// Verifies a backup completely and lists what it holds. Writes nothing.
    InspectBackup {
        name: String,
        passphrase: Secret,
    },
    /// `typed` must be [`RESTORE_WORD`]. Services that would be overwritten are stopped first.
    /// A backup holding the device key needs a new device passphrase to wrap it with.
    RestoreBackup {
        name: String,
        passphrase: Secret,
        new_device_passphrase: Option<Secret>,
        typed: String,
    },
    DeleteBackup {
        name: String,
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
    /// Every connected provider with its domains.
    ListDomainProviders,
    /// Connects one more provider account. The helper lists the domains the key controls, which
    /// also proves the key works. A provider that cannot list needs `domain`, which the helper
    /// checks with a temporary record. Credentials are sealed, root-only. `fields` follow
    /// [`DnsProviderDetail::fields`].
    AddDomainProvider {
        kind: String,
        fields: Vec<(String, Secret)>,
        domain: Option<String>,
    },
    /// Disconnects a provider. Refused while a public site's address is under one of its domains.
    RemoveDomainProvider {
        id: String,
    },
    /// Lists the provider's domains again.
    RefreshDomainProvider {
        id: String,
    },
    /// Adds a domain by name to a provider that cannot list them, after a temporary record proves
    /// the key can manage it.
    AddProviderDomain {
        id: String,
        domain: String,
    },
    /// Deletes DNS records that dynamic DNS published for names no longer in use. Never automatic.
    RetireStaleRecords,
    /// Like `CheckReachability`, with unknown results kept apart from passes and the manual check.
    CheckReachabilityDetail {
        site: SiteId,
    },
    /// The certificate settings, the challenge the helper chose and why, and the local names.
    AcmeSettings,
    /// Stores the owner's certificate choices and applies them to the proxy at once.
    SetAcmeSettings(AcmeSettingsInput),
    /// DNS-01 credentials for a provider whose certificate module needs different ones from
    /// dynamic DNS, such as GoDaddy's API key and secret. Stored sealed. Empty removes them.
    SetDns01Credentials {
        fields: Vec<(String, Secret)>,
    },
    /// Every served host's certificate: issuer, expiry, last renewal and the last error.
    CertificateStatus,
    /// Whether local names under the owner's domain are published and resolve on this network.
    CheckLocalNames,
    /// The local address may have changed. When it did, the helper applies it to Caddy, mDNS, the
    /// firewall and the DNS records of local names. Only root, which nodo-localnet runs as, may ask.
    LocalAddressChanged,
    /// The applied local address, the local CA trust page, and why local name records lag, if they do.
    LocalSiteStatus,
    /// Removable drives a backup could go to. The system disk and the data drive are never listed.
    ListUsbDrives,
    /// The backups on a removable drive. The drive is mounted read-only and unmounted again.
    ListUsbBackups {
        device: String,
    },
    /// Like [`Request::CreateBackup`], written to a removable drive under a temporary name, synced
    /// and then renamed, so an unplugged drive never holds a half-written backup under a real name.
    CreateUsbBackup {
        device: String,
        device_passphrase: Option<Secret>,
        backup_passphrase: Secret,
    },
    InspectUsbBackup {
        device: String,
        name: String,
        passphrase: Secret,
    },
    /// Like [`Request::RestoreBackup`], from a removable drive. On a device that is not set up yet
    /// this is the whole restore: device key, settings, feeds, packages and their storage.
    RestoreUsbBackup {
        device: String,
        name: String,
        passphrase: Secret,
        new_device_passphrase: Option<Secret>,
        typed: String,
    },
    /// Like [`Request::ApplyUpdates`], with the device passphrase, which an update needs when it
    /// asks for a larger encrypted volume.
    ApplyUpdatesWithPassphrase {
        updates: Vec<UpdateApproval>,
        passphrase: Secret,
    },
    /// The installed system snapshot, the newest one the last check found, and what the
    /// schedule will do with it.
    SystemUpdateStatus,
    /// Asks the update server for the channel's newest snapshot and checks its signatures.
    /// Changes nothing on the system.
    CheckSystemUpdate,
    /// Moves the system to the snapshot the last check found. `serial` is the one the owner was
    /// shown; if the channel moved on since, nothing changes and the answer says so.
    ApplySystemUpdate {
        serial: u64,
    },
    /// Turns automatic system updates on or off. Off by default: the schedule then only checks.
    SetSystemAutoUpdate {
        enabled: bool,
    },
    /// One scheduled run: checks when due and, only when automatic system updates are on,
    /// applies a snapshot that needs no restart inside the update window. Only root, which is
    /// the scheduler, may ask.
    RunSystemUpdate,
    /// Available package updates that need the device passphrase to grow encrypted storage.
    UpdateStorageGrowth,
    /// Onion exposure of the node and the sites, with the addresses Tor has made.
    OnionStatus,
    /// Serves the node's restricted RPC on an onion address, or stops it. Never the full RPC.
    SetNodeOnion {
        enabled: bool,
    },
    /// Packages whose previous version is kept and could be put back.
    ListRollbacks,
    /// Puts back the version a package ran before its last update. `to` must be the previous
    /// version. The data is copied first and put back if the old version fails its health check,
    /// and the version rolled back from is held.
    RollbackPackage {
        package: String,
        to: String,
    },
    /// Whether Wi-Fi is on, the network it is joined to with signal and address, and the saved
    /// networks.
    WifiStatus,
    /// Turns the Wi-Fi radio and wpa_supplicant on or off. Off by default; saved networks stay.
    SetWifiEnabled {
        enabled: bool,
    },
    /// Networks in range. Wi-Fi must be on.
    ScanWifi,
    /// Joins a network and saves it only once it accepted the passphrase. An open network needs
    /// `accept_open`. Answers with the status once an address arrived, or the time ran out.
    JoinWifi(WifiJoin),
    /// Removes a saved network and its passphrase, leaving it if joined.
    ForgetWifi {
        ssid: String,
    },
    BluetoothStatus,
    /// Turns the Bluetooth controller and BlueZ on or off. Off by default; pairings stay.
    SetBluetoothEnabled {
        enabled: bool,
    },
    /// Looks for devices for a few seconds. The device itself never becomes discoverable.
    ScanBluetooth,
    /// Starts pairing with a device the owner chose, then trusts and connects it. Answers at
    /// once; `BluetoothStatus` follows the pairing, with a passkey to type when one is needed.
    PairBluetooth {
        address: String,
    },
    /// Connects or disconnects a paired device.
    ConnectBluetooth {
        address: String,
        connect: bool,
    },
    /// Unpairs a device and forgets it.
    RemoveBluetooth {
        address: String,
    },
    /// Whether a screen PIN is set and whether changes from the screen are unlocked now.
    ScreenStatus,
    /// Unlocks changes from the touchscreen for a few idle minutes. Only the screen's account
    /// sends it. A wrong PIN answers `BadPassphrase` after a fixed delay; there is no attempt
    /// limit, because the screen is only reachable by someone at the device.
    ScreenUnlock {
        pin: Secret,
    },
    /// Locks changes from the screen again at once.
    ScreenLock,
    /// Sets (or with `None` clears) the screen PIN. Never from the screen itself: it needs the
    /// device passphrase and comes from SSH or the TUI.
    SetScreenPin {
        passphrase: Secret,
        pin: Option<Secret>,
    },
}

/// What the touchscreen may do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenStatus {
    /// A screen PIN exists. Without one the screen can only look.
    pub pin_set: bool,
    /// Changes from the screen are unlocked until it has been idle a few minutes.
    pub unlocked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Ok,
    /// Answer to `ScreenStatus`.
    Screen(ScreenStatus),
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
    FeedProbe(FeedProbe),
    FeedKeyChange(FeedKeyChange),
    Catalog(Vec<CatalogEntry>),
    InstallReview(InstallReview),
    /// The feed's pins refuse this version or key. Nothing was downloaded beyond the index.
    InstallBlocked {
        package: String,
        reasons: Vec<String>,
        key_changed: bool,
    },
    /// The package breaks the install rules. There is no override.
    InstallRejected {
        package: String,
        version: String,
        reasons: Vec<String>,
    },
    InstallReport(InstallReport),
    UpdateStatus(UpdateStatus),
    UpdateResults(Vec<UpdateResult>),
    Health(HealthSummary),
    Backups(Vec<BackupInfo>),
    BackupCreated(BackupCreated),
    BackupContents(BackupContents),
    Restored(RestoreReport),
    WebStatus(WebStatus),
    DnsProviders(Vec<DnsProviderDetail>),
    DomainProviders(Vec<DomainProvider>),
    DomainProvider(DomainProvider),
    /// What a provider test did, for the screen.
    ProviderTest(String),
    RetiredRecords(Vec<String>),
    ReachabilityDetail(ReachabilityDetail),
    AcmeSettings(AcmeSettingsInfo),
    Certificates(CertificateReport),
    LocalNames(Vec<LocalNameCheck>),
    LocalSiteStatus(LocalSiteStatus),
    UsbDrives(Vec<UsbDrive>),
    UsbBackups(UsbBackupList),
    SystemUpdate(SystemUpdateStatus),
    SystemUpdateApplied(SystemUpdateResult),
    StorageGrowth(Vec<StorageGrowth>),
    Onion(OnionStatus),
    Rollbacks(Vec<RollbackOption>),
    Wifi(WifiStatus),
    WifiNetworks(Vec<WifiNetwork>),
    Bluetooth(BluetoothStatus),
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
        Request::ProbeFeed { .. } => "probe-feed",
        Request::AddFeed { .. } => "add-feed",
        Request::RefreshFeeds => "refresh-feeds",
        Request::SetFeedEnabled { .. } => "set-feed-enabled",
        Request::RemoveFeed { .. } => "remove-feed",
        Request::FeedKeyChange { .. } => "feed-key-change",
        Request::TrustFeedKey { .. } => "trust-feed-key",
        Request::Catalog => "catalog",
        Request::ReviewInstall { .. } => "review-install",
        Request::InstallPackage { .. } => "install-package",
        Request::RemovePackage { .. } => "remove-package",
        Request::UpdateStatus => "update-status",
        Request::CheckUpdates => "check-updates",
        Request::ReviewUpdate { .. } => "review-update",
        Request::ApplyUpdates { .. } => "apply-updates",
        Request::SetHold { .. } => "set-hold",
        Request::DismissUpdate { .. } => "dismiss-update",
        Request::RunAutoUpdate => "run-auto-update",
        Request::Health => "health",
        Request::ListBackups => "list-backups",
        Request::CreateBackup { .. } => "create-backup",
        Request::InspectBackup { .. } => "inspect-backup",
        Request::RestoreBackup { .. } => "restore-backup",
        Request::DeleteBackup { .. } => "delete-backup",
        Request::WebStatus => "web-status",
        Request::ListDnsProviders => "list-dns-providers",
        Request::SetDnsProvider { .. } => "set-dns-provider",
        Request::TestDnsProvider => "test-dns-provider",
        Request::ListDomainProviders => "list-domain-providers",
        Request::AddDomainProvider { .. } => "add-domain-provider",
        Request::RemoveDomainProvider { .. } => "remove-domain-provider",
        Request::RefreshDomainProvider { .. } => "refresh-domain-provider",
        Request::AddProviderDomain { .. } => "add-provider-domain",
        Request::RetireStaleRecords => "retire-stale-records",
        Request::CheckReachabilityDetail { .. } => "check-reachability-detail",
        Request::AcmeSettings => "acme-settings",
        Request::SetAcmeSettings(_) => "set-acme-settings",
        Request::SetDns01Credentials { .. } => "set-dns01-credentials",
        Request::CertificateStatus => "certificate-status",
        Request::CheckLocalNames => "check-local-names",
        Request::LocalAddressChanged => "local-address-changed",
        Request::LocalSiteStatus => "local-site-status",
        Request::ListUsbDrives => "list-usb-drives",
        Request::ListUsbBackups { .. } => "list-usb-backups",
        Request::CreateUsbBackup { .. } => "create-usb-backup",
        Request::InspectUsbBackup { .. } => "inspect-usb-backup",
        Request::RestoreUsbBackup { .. } => "restore-usb-backup",
        Request::ApplyUpdatesWithPassphrase { .. } => "apply-updates-with-passphrase",
        Request::SystemUpdateStatus => "system-update-status",
        Request::CheckSystemUpdate => "check-system-update",
        Request::ApplySystemUpdate { .. } => "apply-system-update",
        Request::SetSystemAutoUpdate { .. } => "set-system-auto-update",
        Request::RunSystemUpdate => "run-system-update",
        Request::UpdateStorageGrowth => "update-storage-growth",
        Request::OnionStatus => "onion-status",
        Request::SetNodeOnion { .. } => "set-node-onion",
        Request::ListRollbacks => "list-rollbacks",
        Request::RollbackPackage { .. } => "rollback-package",
        Request::WifiStatus => "wifi-status",
        Request::SetWifiEnabled { .. } => "set-wifi-enabled",
        Request::ScanWifi => "scan-wifi",
        Request::JoinWifi(_) => "join-wifi",
        Request::ForgetWifi { .. } => "forget-wifi",
        Request::BluetoothStatus => "bluetooth-status",
        Request::SetBluetoothEnabled { .. } => "set-bluetooth-enabled",
        Request::ScanBluetooth => "scan-bluetooth",
        Request::PairBluetooth { .. } => "pair-bluetooth",
        Request::ConnectBluetooth { .. } => "connect-bluetooth",
        Request::RemoveBluetooth { .. } => "remove-bluetooth",
        Request::ScreenStatus => "screen-status",
        Request::ScreenUnlock { .. } => "screen-unlock",
        Request::ScreenLock => "screen-lock",
        Request::SetScreenPin { .. } => "set-screen-pin",
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
            Request::InstallPackage {
                feed: "f".into(),
                package: "a".into(),
                approval: PermissionApproval {
                    pkgver: "a-1_1".into(),
                    permissions: vec![],
                },
                passphrase: Some(Secret::new("hunter2")),
            },
            Request::CreateBackup {
                device_passphrase: Some(Secret::new("hunter2")),
                backup_passphrase: Secret::new("hunter3 long enough"),
            },
            Request::InspectBackup {
                name: "b".into(),
                passphrase: Secret::new("hunter2"),
            },
            Request::RestoreBackup {
                name: "b".into(),
                passphrase: Secret::new("hunter2"),
                new_device_passphrase: Some(Secret::new("hunter3")),
                typed: "restore".into(),
            },
            Request::SetDns01Credentials {
                fields: vec![("api_secret".into(), Secret::new("hunter2"))],
            },
            Request::AddDomainProvider {
                kind: "cloudflare".into(),
                fields: vec![("api_token".into(), Secret::new("hunter2"))],
                domain: None,
            },
            Request::CreateUsbBackup {
                device: "sdb1".into(),
                device_passphrase: Some(Secret::new("hunter2")),
                backup_passphrase: Secret::new("hunter3 long enough"),
            },
            Request::InspectUsbBackup {
                device: "sdb1".into(),
                name: "b".into(),
                passphrase: Secret::new("hunter2"),
            },
            Request::RestoreUsbBackup {
                device: "sdb1".into(),
                name: "b".into(),
                passphrase: Secret::new("hunter2"),
                new_device_passphrase: Some(Secret::new("hunter3")),
                typed: "restore".into(),
            },
            Request::ApplyUpdatesWithPassphrase {
                updates: vec![],
                passphrase: Secret::new("hunter2"),
            },
            Request::ApplySettings {
                package: "a".into(),
                changes: vec![SettingChange {
                    name: "k".into(),
                    value: SettingInput::Secret(Secret::new("hunter2")),
                }],
            },
            Request::JoinWifi(WifiJoin {
                ssid: "home".into(),
                security: WifiSecurity::Wpa2Personal,
                passphrase: Some(Secret::new("hunter2 long")),
                accept_open: false,
            }),
        ];
        for request in requests {
            let text = format!("{request:?}");
            assert!(!text.contains("hunter"), "{text}");
            assert!(!action_name(&request).contains("hunter"));
        }
    }
}
