//! What an interface asks the device for (`Effect`) and what comes back (`Outcome`).
//!
//! Neither holds anything of a terminal or a toolkit. `exec::execute_effect` performs an effect
//! against the backend and returns its outcomes.

use nodo_api::{ApiError, Exposure, Response};

use crate::model::*;
use crate::secret::Secret;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExposureAfter {
    Back,
    Stay,
    WentPublic,
}

/// A request for I/O. `Debug` is safe to print: anything secret is held in a `Secret`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Refresh,
    Start(String),
    Stop(String),
    Restart(String),
    /// Removes an installed app. Its data stays on the drive, so installing it again brings it back.
    RemovePackage(String),
    Unlock(Secret),
    OpenService(String),
    LoadDetail(PackageId),
    CheckInstall(PackageId),
    StartInstall { id: PackageId, approval: Option<nodo_api::PermissionApproval>, passphrase: Option<Secret> },
    PollInstall,
    CancelInstall,
    ProbeFeed(String),
    TrustFeed(FeedProbe),
    RefreshFeeds,
    ToggleFeed(String),
    RemoveFeed(String),
    /// A feed by its name, and the name to show for it; `None` goes back to its own.
    RenameFeed { name: String, label: Option<String> },
    LoadKeyChange(String),
    TrustNewKey { change: KeyChange, typed: String },
    OpenSettings(String),
    LoadReferences { package: String },
    PreviewSetting { package: String, field_index: usize, key: String, value: FieldValue },
    Grant { consumer: String, provider: String, setting: String },
    /// The changed fields. `None` puts a setting back to having no value of its own: an emptied
    /// optional field.
    ApplySettings { package: String, values: Vec<(String, Option<FieldValue>)> },
    CheckUpdates,
    ReviewUpdate {
        name: String,
        batch: Vec<String>,
        pending_reviews: Vec<String>,
        approvals: Vec<(String, nodo_api::PermissionApproval)>,
    },
    ApplyUpdates { names: Vec<String>, approvals: Vec<(String, nodo_api::PermissionApproval)> },
    SetHeld { name: String, held: bool },
    SaveAutoUpdate(AutoUpdatePrefs),
    LoadLogs(String),
    Drive(DriveAction),
    Shell,
    /// `confirmation` is the host the user typed, which the helper requires to go public.
    SetExposure { site: String, exposure: Exposure, after: ExposureAfter, confirmation: Option<String> },
    /// Saves the host and every other name of a site. `then` runs only once that worked, so a
    /// refused name never goes on to the checks or to going public with the old names.
    /// `confirm_added` are the new names the owner confirmed putting on a site that is public.
    SetPublicHost { site: String, host: String, aliases: Vec<nodo_api::PublicAlias>, confirm_added: Vec<String>, then: Vec<Effect> },
    SetPublicAccess { site: String, access: PublicAccess },
    CheckReachability { site: String, going_public: bool },
    /// `domain` only for a provider that cannot list its domains.
    AddProvider { provider: String, domain: Option<String>, api_key: Secret, secret_key: Secret },
    /// Reads the provider catalog again, as the add flow opens.
    LoadProviders,
    RemoveProvider(String),
    RefreshProvider(String),
    /// `dns01` holds the provider's separate certificate credentials, when it needs them.
    SaveAcme { settings: nodo_api::AcmeSettingsInput, dns01: Option<(Secret, Secret)> },
    CheckLocalNames,
    ShowDocument(DocKind),
    CopyReport(String),
    InitEncryption(Secret),
    LoadSshKeys,
    AddSshKey(String),
    RemoveSshKey(String),
    /// The device's backups, and the first USB drive with its backups.
    LoadBackups,
    /// `usb` names a removable drive to use instead of the device's own backup folder.
    CreateBackup { device_passphrase: Option<Secret>, backup_passphrase: Secret, usb: Option<String> },
    InspectBackup { name: String, passphrase: Secret, usb: Option<String> },
    RestoreBackup {
        name: String,
        passphrase: Secret,
        new_device_passphrase: Option<Secret>,
        typed: String,
        usb: Option<String>,
    },
    /// Like `ApplyUpdates`, with the device passphrase for updates that grow encrypted storage.
    ApplyUpdatesWithPassphrase {
        names: Vec<String>,
        approvals: Vec<(String, nodo_api::PermissionApproval)>,
        passphrase: Secret,
    },
    LoadOnion,
    SetNodeOnion(bool),
    SetSiteExposure { site: nodo_api::SiteId, exposure: Exposure },
    LoadRestoreDrives,
    LoadRestoreBackups(String),
    InspectRestore { device: String, name: String, passphrase: Secret },
    RunRestore { device: String, name: String, passphrase: Secret, new_device_passphrase: Secret, typed: String },
    LoadRollbacks,
    RollBack { package: String, to: String },
    /// Both radios' status.
    LoadNetwork,
    /// Wi-Fi's status alone, quietly, while an address is awaited.
    PollWifi,
    SetWifi(bool),
    ScanWifi,
    JoinWifi { ssid: String, security: nodo_api::WifiSecurity, passphrase: Option<Secret>, accept_open: bool },
    ForgetWifi(String),
    SetBluetooth(bool),
    ScanBluetooth,
    PairBluetooth(String),
    /// Bluetooth's status, quietly, while a pairing runs.
    PollBluetooth,
    ConnectBluetooth { address: String, connect: bool },
    RemoveBluetooth(String),
    /// Installs a reviewed bundle with the owner's answers and data drive choices.
    InstallBundle {
        review: Box<nodo_api::BundleReview>,
        answers: Vec<nodo_api::BundleAnswer>,
        data_drive: Vec<String>,
        passphrase: Option<Secret>,
    },
    /// Fetches an app's setup page with the device passphrase.
    LoadOwnerPage { package: String, passphrase: Secret },
    /// Turns the web GUI on or off. Only the main socket takes it.
    SetWebGui(bool),
}

impl Effect {
    /// Whether performing it can take minutes: a download, a whole backup read or written, an
    /// app restarted with new settings, a check from outside. A frontend that cannot make its
    /// owner wait for the answer (a web request) runs these in the background and shows what is
    /// running; every other effect answers in a moment.
    pub fn is_long(&self) -> bool {
        match self {
            Effect::StartInstall { .. }
            | Effect::InstallBundle { .. }
            | Effect::CheckUpdates
            | Effect::ApplyUpdates { .. }
            | Effect::ApplyUpdatesWithPassphrase { .. }
            | Effect::ApplySettings { .. }
            | Effect::RollBack { .. }
            | Effect::CheckReachability { .. }
            | Effect::CreateBackup { .. }
            | Effect::InspectBackup { .. }
            | Effect::RestoreBackup { .. }
            | Effect::InspectRestore { .. }
            | Effect::RunRestore { .. } => true,
            Effect::Drive(action) => matches!(action, DriveAction::ReadOnlyCheck | DriveAction::Repair),
            Effect::SetExposure { after, .. } => *after == ExposureAfter::WentPublic,
            // What runs once the names are saved counts too.
            Effect::SetPublicHost { then, .. } => then.iter().any(Effect::is_long),
            _ => false,
        }
    }
}

/// What an effect came to. A frontend hands each one to the state that asked for it.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Refreshed(Box<Snapshot>),
    Failed(String),
    Info(String),
    UnlockResult(Result<Response, ApiError>),
    DetailLoaded(Option<PackageDetail>),
    GateLoaded(InstallGate),
    InstallProgress(Option<InstallSnapshot>),
    /// How Check now went. It comes after the snapshot that holds what it found, and the sources
    /// as they were read.
    UpdatesChecked(CheckReport),
    FeedProbed(Result<FeedProbe, String>),
    FeedTrusted(Result<String, String>),
    /// The name the feed is shown by now.
    FeedRenamed(Result<String, String>),
    KeyChangeLoaded(Option<KeyChange>),
    NewKeyTrusted(Result<String, String>),
    SettingsLoaded { package: String, form: Option<SettingsForm> },
    ReferencesLoaded { package: String, list: ReferenceList },
    SettingPreviewed { package: String, field_index: usize, preview: FieldPreview },
    Granted { provider: String, setting: String, result: Result<(), ApiError> },
    SettingsApplied(Result<(), ApiError>),
    SettingsRolledBack { package: String, explanation: RollbackExplanation },
    UpdateReviewLoaded {
        review: Option<ReviewData>,
        batch: Vec<String>,
        pending_reviews: Vec<String>,
        approvals: Vec<(String, nodo_api::PermissionApproval)>,
    },
    UpdatesApplied(UpdateReport),
    AutoUpdateSaved(Result<(), String>),
    LogsLoaded { source: String, view: LogView },
    DriveResult(Result<Document, String>),
    ExposureSet { site: String, after: ExposureAfter, result: Result<(), ApiError> },
    PublicHostSet { site: String, result: Result<(), String>, then: Vec<Effect> },
    Reachability { site: String, going_public: bool, result: Reachability },
    ProviderAdded(Result<ConnectedProvider, String>),
    ProvidersLoaded(Result<Vec<Provider>, String>),
    ProviderRemoved(Result<(), String>),
    ProviderRefreshed(Result<ConnectedProvider, String>),
    AcmeSaved(Result<(), String>),
    LocalNamesChecked(Result<Vec<nodo_api::LocalNameCheck>, String>),
    DocumentLoaded(Document),
    SetupDone(Result<(), ApiError>),
    SshKeysLoaded(Result<Vec<nodo_api::SshKey>, String>),
    SshKeyChanged(Result<(), String>),
    BackupsLoaded(Result<Vec<nodo_api::BackupInfo>, String>),
    BackupCreated(Result<nodo_api::BackupCreated, String>),
    BackupInspected(Result<nodo_api::BackupContents, String>),
    BackupRestored(Result<nodo_api::RestoreReport, String>),
    UsbLoaded(Result<UsbView, String>),
    OnionLoaded(Result<nodo_api::OnionStatus, String>),
    OnionChanged(Result<(), String>),
    RestoreDrives(Result<Vec<nodo_api::UsbDrive>, String>),
    RestoreBackups(Result<nodo_api::UsbBackupList, String>),
    RestoreInspected(Result<nodo_api::BackupContents, String>),
    RestoreFinished(Result<nodo_api::RestoreReport, String>),
    RollbacksLoaded(Result<RollbackView, String>),
    RolledBack(Result<nodo_api::UpdateResult, String>),
    NetworkLoaded {
        wifi: Result<nodo_api::WifiStatus, String>,
        bluetooth: Result<nodo_api::BluetoothStatus, String>,
    },
    /// After a Wi-Fi change. `quiet` results come from polling and never raise a notice.
    WifiChanged { result: Result<nodo_api::WifiStatus, String>, quiet: bool },
    WifiScanned(Result<Vec<nodo_api::WifiNetwork>, String>),
    BluetoothChanged { result: Result<nodo_api::BluetoothStatus, String>, quiet: bool },
    BundleInstalled(Result<nodo_api::BundleReport, String>),
    OwnerPageLoaded { package: String, result: Result<nodo_api::OwnerPageView, ApiError> },
    /// What the web GUI's switch came to: what was asked for, and how the web GUI stands now.
    WebGuiSet { enabled: bool, result: Result<nodo_api::WebGuiStatus, ApiError> },
}

/// The USB drive the backup screen shows: the first one plugged in, with its backups when it
/// can be used, or why it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbView {
    pub drive: nodo_api::UsbDrive,
    pub list: Option<nodo_api::UsbBackupList>,
}

/// What the Rollbacks screen shows: rollbacks that happened and versions that can be put back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollbackView {
    pub options: Vec<nodo_api::RollbackOption>,
    /// Failed updates that were rolled back, newest first.
    pub events: Vec<nodo_api::UpdateEvent>,
    /// The system went back to its previous version at boot, and why.
    pub boot_fallback: Option<String>,
}
