//! Application state, plus the events that change it and the effects it asks for.
//!
//! `update::update` turns `(state, event)` into a new state and a list of `Effect`s without any
//! I/O. `runtime::execute` performs each effect against the backend and turns the result back into
//! events. Tests can therefore drive every flow without a terminal or a device.

use crossterm::event::KeyEvent;
use nodo_api::{ApiError, Exposure, Response};

use crate::model::*;
use crate::secret::{Input, Secret};

/// Below this size the layout cannot show enough context to act safely, so input is refused.
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 16;

pub const TICKS_PER_SECOND: u64 = 4;
/// Upper bound of the unlock retry delay. The helper must enforce its own limit as well, because
/// this one only protects against an impatient user at this screen.
pub const MAX_UNLOCK_DELAY_SECONDS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Packages,
    Web,
    Feeds,
    Updates,
    Logs,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::Home, Tab::Packages, Tab::Web, Tab::Feeds, Tab::Updates, Tab::Logs];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Home => "Home",
            Tab::Packages => "Packages",
            Tab::Web => "Web",
            Tab::Feeds => "Feeds",
            Tab::Updates => "Updates",
            Tab::Logs => "Logs",
        }
    }
}

/// One entry per design file, so fixtures and tests can enumerate the whole design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenId {
    Main,
    Search,
    Detail,
    Review,
    Installing,
    Unlock,
    Feeds,
    AddFeed,
    FeedKey,
    Settings,
    Reference,
    ApplyFailed,
    Updates,
    UpdateFailed,
    Logs,
    Rejected,
    Capability,
    NoDrive,
    WebSites,
    WebExposure,
    WebProviders,
    WebCheck,
    WebConfirm,
    WebCgnat,
    AutoUpdate,
    WebDomain,
    WebCertificate,
    /// First boot, and SSH keys afterwards. It has no design file, so it is not in `ALL`.
    Setup,
    /// Backups and restore, on the device and on a USB drive.
    Backup,
}

impl ScreenId {
    pub const ALL: [ScreenId; 28] = [
        ScreenId::Main,
        ScreenId::Search,
        ScreenId::Detail,
        ScreenId::Review,
        ScreenId::Installing,
        ScreenId::Unlock,
        ScreenId::Feeds,
        ScreenId::AddFeed,
        ScreenId::FeedKey,
        ScreenId::Settings,
        ScreenId::Reference,
        ScreenId::ApplyFailed,
        ScreenId::Updates,
        ScreenId::UpdateFailed,
        ScreenId::Logs,
        ScreenId::Rejected,
        ScreenId::Capability,
        ScreenId::NoDrive,
        ScreenId::WebSites,
        ScreenId::WebExposure,
        ScreenId::WebProviders,
        ScreenId::WebCheck,
        ScreenId::WebConfirm,
        ScreenId::WebCgnat,
        ScreenId::AutoUpdate,
        ScreenId::WebDomain,
        ScreenId::WebCertificate,
        ScreenId::Backup,
    ];

    /// The design file's stem in lowercase, which also names `--screen` values and snapshots.
    pub fn slug(self) -> &'static str {
        match self {
            ScreenId::Main => "main",
            ScreenId::Search => "search",
            ScreenId::Detail => "detail",
            ScreenId::Review => "review",
            ScreenId::Installing => "installing",
            ScreenId::Unlock => "unlock",
            ScreenId::Feeds => "feeds",
            ScreenId::AddFeed => "addfeed",
            ScreenId::FeedKey => "feedkey",
            ScreenId::Settings => "settings",
            ScreenId::Reference => "reference",
            ScreenId::ApplyFailed => "applyfailed",
            ScreenId::Updates => "updates",
            ScreenId::UpdateFailed => "updatefailed",
            ScreenId::Logs => "logs",
            ScreenId::Rejected => "rejected",
            ScreenId::Capability => "capability",
            ScreenId::NoDrive => "nodrive",
            ScreenId::WebSites => "websites",
            ScreenId::WebExposure => "webexposure",
            ScreenId::WebProviders => "webproviders",
            ScreenId::WebCheck => "webcheck",
            ScreenId::WebConfirm => "webconfirm",
            ScreenId::WebCgnat => "webcgnat",
            ScreenId::AutoUpdate => "autoupdate",
            ScreenId::WebDomain => "webdomain",
            ScreenId::WebCertificate => "webcertificate",
            ScreenId::Setup => "setup",
            ScreenId::Backup => "backup",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.slug() == slug)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    pub query: Input,
    /// Letters filter while the query has focus and act as shortcuts once the list has focus.
    pub list_focused: bool,
    pub selected: usize,
    pub feed_filter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewState {
    pub data: ReviewData,
    /// False until the user moves to it, so a stray Enter cancels.
    pub confirm_focused: bool,
    pub purpose: ReviewPurpose,
    /// Shown after confirming a package whose new encrypted storage needs the device passphrase.
    pub passphrase: Option<Input>,
}

impl ReviewState {
    pub fn new(data: ReviewData, purpose: ReviewPurpose) -> Self {
        Self { data, confirm_focused: false, purpose, passphrase: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewPurpose {
    Install,
    /// `approvals` gathers each confirmed review, which the helper requires for those updates.
    Update { batch: Vec<String>, pending_reviews: Vec<String>, approvals: Vec<(String, nodo_api::PermissionApproval)> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockState {
    pub input: Input,
    pub error: Option<String>,
    pub failures: u32,
    pub retry_at_tick: u64,
}

impl Default for UnlockState {
    /// Written out because a derived default would make the passphrase input a plain one.
    fn default() -> Self {
        Self { input: Input::secret(), error: None, failures: 0, retry_at_tick: 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupStep {
    /// The passphrase twice. `confirming` is true once the first is entered.
    Passphrase { first: Input, second: Input, confirming: bool },
    Keys { keys: Vec<nodo_api::SshKey>, selected: usize, adding: Option<Input> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupState {
    pub step: SetupStep,
    pub error: Option<String>,
    pub busy: bool,
}

impl SetupState {
    pub fn passphrase() -> Self {
        let step = SetupStep::Passphrase { first: Input::secret(), second: Input::secret(), confirming: false };
        Self { step, error: None, busy: false }
    }

    pub fn keys() -> Self {
        Self { step: SetupStep::Keys { keys: Vec::new(), selected: 0, adding: None }, error: None, busy: true }
    }
}

/// The helper enforces this too. Checking here saves a round trip and a KDF run.
pub const MIN_PASSPHRASE_CHARS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupStep {
    List,
    /// The device passphrase, when the device has one, then the backup passphrase twice.
    Create { device: Input, first: Input, second: Input, field: usize },
    /// The backup passphrase. The helper verifies the whole file before showing what it holds.
    Unlock { name: String, passphrase: Input },
    /// What a restore replaces, the typed word, and a new device passphrase when it holds a key.
    Confirm { name: String, passphrase: Secret, contents: nodo_api::BackupContents, typed: Input, device: Input, field: usize },
    Restored(nodo_api::RestoreReport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupState {
    pub list: Vec<nodo_api::BackupInfo>,
    pub selected: usize,
    pub step: BackupStep,
    pub error: Option<String>,
    pub busy: bool,
    /// The device key exists, so a backup asks for the device passphrase to include it.
    pub device_set_up: bool,
    /// The USB drive, `None` while none is plugged in.
    pub usb: Option<UsbView>,
    /// The USB list has the focus rather than the device's own list.
    pub usb_focused: bool,
    pub usb_selected: usize,
    /// The removable drive the step under way uses, `None` for the device's own backups.
    pub target: Option<String>,
    /// First boot: restoring a whole device from a USB drive instead of setting it up.
    pub fresh: bool,
}

impl BackupState {
    pub fn new(device_set_up: bool) -> Self {
        Self {
            list: Vec::new(),
            selected: 0,
            step: BackupStep::List,
            error: None,
            busy: true,
            device_set_up,
            usb: None,
            usb_focused: false,
            usb_selected: 0,
            target: None,
            fresh: false,
        }
    }

    /// The first-boot restore, which only offers USB drives.
    pub fn fresh() -> Self {
        Self { usb_focused: true, fresh: true, ..Self::new(false) }
    }

    /// Backups on the USB drive, when one can be used.
    pub fn usb_list(&self) -> &[nodo_api::BackupInfo] {
        self.usb.as_ref().and_then(|u| u.list.as_ref()).map(|l| l.backups.as_slice()).unwrap_or(&[])
    }
}

/// The helper also requires this word, so the check here only saves a round trip.
pub const RESTORE_WORD: &str = nodo_api::RESTORE_WORD;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddFeedState {
    Address { input: Input, error: Option<String> },
    Confirm { probe: FeedProbe, trust_focused: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedKeyState {
    pub change: KeyChange,
    /// 0 keeps the old key, 1 trusts the new one, 2 removes the feed.
    pub choice: usize,
    pub typing: Option<Input>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsState {
    pub form: SettingsForm,
    pub draft: Vec<FieldValue>,
    pub selected: usize,
    pub editing: Option<Input>,
}

impl SettingsState {
    pub fn new(form: SettingsForm) -> Self {
        let draft = form.fields.iter().map(|field| field.value.clone()).collect();
        Self { form, draft, selected: 0, editing: None }
    }

    pub fn changed_indices(&self) -> Vec<usize> {
        (0..self.draft.len()).filter(|&i| self.draft[i] != self.form.fields[i].value).collect()
    }

    pub fn error_for(&self, index: usize) -> Option<String> {
        validate(&self.form.fields[index].kind, &self.draft[index])
    }
}

/// Field validation, kept pure so that it can be tested and so save can refuse early.
pub fn validate(kind: &FieldKind, value: &FieldValue) -> Option<String> {
    let FieldValue::Text(text) = value else {
        return None;
    };
    match kind {
        FieldKind::Port => match text.parse::<u16>() {
            Ok(port) if port > 0 => None,
            _ => Some("Port must be a number from 1 to 65535.".to_owned()),
        },
        FieldKind::Number { min, max } => match text.parse::<i64>() {
            Ok(n) if min.is_none_or(|min| n >= min) && max.is_none_or(|max| n <= max) => None,
            Ok(_) => Some(format!(
                "Must be from {} to {}.",
                min.map_or("any".to_owned(), |n| n.to_string()),
                max.map_or("any".to_owned(), |n| n.to_string())
            )),
            Err(_) => Some("Must be a whole number.".to_owned()),
        },
        FieldKind::Url => (!(text.starts_with("http://") || text.starts_with("https://")))
            .then(|| "Must start with http:// or https://.".to_owned()),
        FieldKind::MoneroAddress => {
            const BASE58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
            match text.chars().count() {
                n if n < 95 => Some("Address is too short. A Monero address has 95 characters.".to_owned()),
                n if n > 95 => Some("Address is too long. A Monero address has 95 characters.".to_owned()),
                _ if !text.chars().all(|c| BASE58.contains(c)) => {
                    Some("Address contains characters a Monero address cannot have.".to_owned())
                }
                _ => None,
            }
        }
        FieldKind::Choice(_) | FieldKind::Secret | FieldKind::Toggle | FieldKind::Text => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceState {
    pub package: String,
    pub field_index: usize,
    pub field_label: String,
    pub list: ReferenceList,
    pub query: Input,
    pub list_focused: bool,
    pub selected: usize,
}

impl ReferenceState {
    pub fn visible(&self) -> Vec<&ReferenceOption> {
        self.list.options.iter().filter(|option| option.path.contains(self.query.text())).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRow {
    pub label: String,
    pub from: String,
    pub to: String,
    pub blamed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyFailedState {
    pub package: String,
    pub explanation: RollbackExplanation,
    pub rows: Vec<DraftRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatesState {
    pub selected: usize,
    pub checked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoUpdateState {
    pub saved: AutoUpdatePrefs,
    pub draft: AutoUpdatePrefs,
    /// 0 is the frequency selector, and the package rows follow it.
    pub selected: usize,
    pub editing_window: Option<Input>,
    pub error: Option<String>,
}

impl AutoUpdateState {
    pub fn new(prefs: AutoUpdatePrefs) -> Self {
        Self { saved: prefs.clone(), draft: prefs, selected: 0, editing_window: None, error: None }
    }

    pub fn focused_package(&self) -> Option<usize> {
        self.selected.checked_sub(1)
    }
}

/// Parses `HH:MM to HH:MM`. A window may cross midnight, so only an empty one is refused.
pub fn parse_window(text: &str) -> Option<(String, String)> {
    fn time(text: &str) -> Option<String> {
        let (hours, minutes) = text.trim().split_once(':')?;
        if hours.len() != 2 || minutes.len() != 2 {
            return None;
        }
        let (hours, minutes) = (hours.parse::<u8>().ok()?, minutes.parse::<u8>().ok()?);
        (hours < 24 && minutes < 60).then(|| format!("{hours:02}:{minutes:02}"))
    }
    let (start, end) = text.split_once(" to ")?;
    let (start, end) = (time(start)?, time(end)?);
    (start != end).then_some((start, end))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateResultsState {
    pub report: UpdateReport,
    pub choice: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsState {
    pub selected: usize,
    pub view: LogView,
    pub errors_only: bool,
    pub follow: bool,
    pub find: Input,
    pub finding: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityState {
    pub data: CapabilityData,
    pub choice: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposureState {
    pub site: String,
    pub draft: Exposure,
    pub selected: usize,
    pub editing_host: Option<Input>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFocus {
    List,
    Domain,
    ApiKey,
    SecretKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvidersState {
    pub selected: usize,
    pub focus: ProviderFocus,
    pub domain: Input,
    pub api_key: Input,
    pub secret_key: Input,
    pub test: Option<Result<String, String>>,
}

impl ProvidersState {
    pub fn new(domain: &str) -> Self {
        Self {
            selected: 0,
            focus: ProviderFocus::List,
            domain: Input::plain(domain),
            api_key: Input::secret(),
            secret_key: Input::secret(),
            test: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebCheckState {
    pub site: String,
    pub checks: Vec<Check>,
    pub router: Option<RouterHelp>,
    pub plan: Option<PublicPlan>,
    /// True when this check is a step towards going public, not just a test from the overview.
    pub going_public: bool,
    pub manual: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmFocus {
    Password,
    Address,
    Cancel,
    GoPublic,
}

impl ConfirmFocus {
    pub const ORDER: [ConfirmFocus; 4] = [
        ConfirmFocus::Password,
        ConfirmFocus::Address,
        ConfirmFocus::Cancel,
        ConfirmFocus::GoPublic,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebConfirmState {
    pub site: String,
    pub plan: PublicPlan,
    pub access: PublicAccess,
    pub focus: ConfirmFocus,
    pub typed: Input,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainFocus {
    Email,
    Staging,
    DotLocal,
    UnderDomain,
    Dns01First,
    Dns01Second,
}

impl DomainFocus {
    /// Fields that take typing rather than a toggle.
    pub fn is_text(self) -> bool {
        matches!(self, DomainFocus::Email | DomainFocus::UnderDomain | DomainFocus::Dns01First | DomainFocus::Dns01Second)
    }
}

/// Domains and certificates: the ACME email, staging, local names and DNS-01 credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainState {
    pub focus: DomainFocus,
    pub email: Input,
    pub staging: bool,
    pub under_domain: bool,
    pub local_domain: Input,
    pub dns01_first: Input,
    pub dns01_second: Input,
    pub checks: Option<Result<Vec<nodo_api::LocalNameCheck>, String>>,
    pub error: Option<String>,
}

impl DomainState {
    pub fn new(acme: &AcmeView, domain: &str) -> Self {
        let suggested = if domain.contains('.') { format!("home.{domain}") } else { String::new() };
        Self {
            focus: DomainFocus::Email,
            email: Input::plain(acme.email.clone()),
            staging: acme.staging,
            under_domain: acme.local_domain.is_some(),
            local_domain: Input::plain(acme.local_domain.clone().unwrap_or(suggested)),
            dns01_first: Input::secret(),
            dns01_second: Input::secret(),
            checks: None,
            error: None,
        }
    }

    pub fn order(acme: &AcmeView) -> Vec<DomainFocus> {
        let mut order = vec![DomainFocus::Email, DomainFocus::Staging, DomainFocus::DotLocal, DomainFocus::UnderDomain];
        if !acme.dns01_fields.is_empty() {
            order.extend([DomainFocus::Dns01First, DomainFocus::Dns01Second]);
        }
        order
    }
}

/// A public or local host while Caddy gets its certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateState {
    pub site: String,
    pub host: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgnatState {
    pub site: String,
    pub wan_address: String,
    pub wan_range: String,
    pub internet_address: String,
    pub choice: usize,
    pub going_public: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Home { selected: usize },
    NoDrive { choice: usize },
    Search(SearchState),
    Detail(PackageDetail),
    Review(ReviewState),
    Installing(InstallSnapshot),
    Unlock(UnlockState),
    Feeds { selected: usize },
    AddFeed(AddFeedState),
    FeedKey(FeedKeyState),
    Settings(SettingsState),
    Reference(ReferenceState),
    ApplyFailed(ApplyFailedState),
    Updates(UpdatesState),
    AutoUpdate(AutoUpdateState),
    UpdateResults(UpdateResultsState),
    Logs(LogsState),
    Rejected(RejectionData),
    Capability(CapabilityState),
    Web { selected: usize },
    WebExposure(ExposureState),
    WebProviders(ProvidersState),
    WebCheck(WebCheckState),
    WebConfirm(WebConfirmState),
    WebCgnat(CgnatState),
    WebDomain(DomainState),
    WebCertificate(CertificateState),
    Setup(SetupState),
    Backup(BackupState),
}

impl Screen {
    pub fn id(&self) -> ScreenId {
        match self {
            Screen::Home { .. } => ScreenId::Main,
            Screen::NoDrive { .. } => ScreenId::NoDrive,
            Screen::Search(_) => ScreenId::Search,
            Screen::Detail(_) => ScreenId::Detail,
            Screen::Review(_) => ScreenId::Review,
            Screen::Installing(_) => ScreenId::Installing,
            Screen::Unlock(_) => ScreenId::Unlock,
            Screen::Feeds { .. } => ScreenId::Feeds,
            Screen::AddFeed(_) => ScreenId::AddFeed,
            Screen::FeedKey(_) => ScreenId::FeedKey,
            Screen::Settings(_) => ScreenId::Settings,
            Screen::Reference(_) => ScreenId::Reference,
            Screen::ApplyFailed(_) => ScreenId::ApplyFailed,
            Screen::Updates(_) => ScreenId::Updates,
            Screen::AutoUpdate(_) => ScreenId::AutoUpdate,
            Screen::UpdateResults(_) => ScreenId::UpdateFailed,
            Screen::Logs(_) => ScreenId::Logs,
            Screen::Rejected(_) => ScreenId::Rejected,
            Screen::Capability(_) => ScreenId::Capability,
            Screen::Web { .. } => ScreenId::WebSites,
            Screen::WebExposure(_) => ScreenId::WebExposure,
            Screen::WebProviders(_) => ScreenId::WebProviders,
            Screen::WebCheck(_) => ScreenId::WebCheck,
            Screen::WebConfirm(_) => ScreenId::WebConfirm,
            Screen::WebCgnat(_) => ScreenId::WebCgnat,
            Screen::WebDomain(_) => ScreenId::WebDomain,
            Screen::WebCertificate(_) => ScreenId::WebCertificate,
            Screen::Setup(_) => ScreenId::Setup,
            Screen::Backup(_) => ScreenId::Backup,
        }
    }

    pub fn tab(&self) -> Option<Tab> {
        match self {
            Screen::Home { .. } | Screen::NoDrive { .. } => Some(Tab::Home),
            Screen::Search(_) => Some(Tab::Packages),
            Screen::Web { .. } => Some(Tab::Web),
            Screen::Feeds { .. } => Some(Tab::Feeds),
            Screen::Updates(_) => Some(Tab::Updates),
            Screen::Logs(_) => Some(Tab::Logs),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalAction {
    RemoveFeed(String),
    AllSitesOffline,
    RepairDrive,
    DiscardSettings,
    DiscardAutoUpdate,
    RemoveSshKey(String),
}

/// A yes-or-no question whose focus always starts on the safe answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modal {
    pub title: String,
    pub body: Vec<String>,
    pub confirm_label: String,
    pub confirm_focused: bool,
    pub action: ModalAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    pub doc: Document,
    pub scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub screen: Screen,
    pub stack: Vec<Screen>,
    pub data: Snapshot,
    pub size: (u16, u16),
    pub notice: Option<Notice>,
    pub modal: Option<Modal>,
    pub overlay: Option<Overlay>,
    pub should_quit: bool,
    pub ticks: u64,
    /// The backend is busy with a request. Restarts and settings changes can take minutes.
    pub working: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            screen: Screen::Home { selected: 0 },
            stack: Vec::new(),
            data: Snapshot::default(),
            size: (100, 32),
            notice: None,
            modal: None,
            overlay: None,
            should_quit: false,
            ticks: 0,
            working: false,
        }
    }

    pub fn too_small(&self) -> bool {
        self.size.0 < MIN_WIDTH || self.size.1 < MIN_HEIGHT
    }

    /// Held-back versions are not offered, so they do not count towards the badge.
    pub fn pending_updates(&self) -> usize {
        self.data.updates.iter().filter(|item| !item.held).count()
    }

    pub fn is_locked(&self) -> bool {
        self.data.services.iter().any(|svc| svc.state == nodo_api::ServiceState::Locked)
    }

    /// Available updates first, then held ones, matching the rows the Updates screen shows.
    pub fn update_rows(&self) -> Vec<&UpdateItem> {
        let (held, available): (Vec<_>, Vec<_>) = self.data.updates.iter().partition(|item| item.held);
        available.into_iter().chain(held).collect()
    }

    pub fn search_results(&self, state: &SearchState) -> Vec<&PackageSummary> {
        let query = state.query.text().to_lowercase();
        self.data
            .catalog
            .iter()
            .filter(|pkg| state.feed_filter.as_ref().is_none_or(|feed| &pkg.id.feed == feed))
            .filter(|pkg| {
                pkg.id.name.to_lowercase().contains(&query) || pkg.tagline.to_lowercase().contains(&query)
            })
            .collect()
    }
}

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
    LoadKeyChange(String),
    TrustNewKey { change: KeyChange, typed: String },
    OpenSettings(String),
    LoadReferences { package: String, field_index: usize, field_key: String, field_label: String },
    ApplySettings { package: String, values: Vec<(String, FieldValue)> },
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
    SetPublicHost { site: String, host: String },
    SetPublicAccess { site: String, access: PublicAccess },
    CheckReachability { site: String, going_public: bool },
    TestProvider { provider: String, domain: String, api_key: Secret, secret_key: Secret },
    SaveProvider { provider: String, domain: String, api_key: Secret, secret_key: Secret },
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
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Key(KeyEvent),
    Paste(String),
    Tick,
    Resize(u16, u16),
    Refreshed(Box<Snapshot>),
    Failed(String),
    Info(String),
    UnlockResult(Result<Response, ApiError>),
    DetailLoaded(Option<PackageDetail>),
    GateLoaded(InstallGate),
    InstallProgress(Option<InstallSnapshot>),
    FeedProbed(Result<FeedProbe, String>),
    FeedTrusted(Result<String, String>),
    KeyChangeLoaded(Option<KeyChange>),
    NewKeyTrusted(Result<String, String>),
    SettingsLoaded { package: String, form: Option<SettingsForm> },
    ReferencesLoaded { package: String, field_index: usize, field_label: String, list: ReferenceList },
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
    ExposureSet { after: ExposureAfter, result: Result<(), ApiError> },
    PublicHostSet(Result<(), String>),
    Reachability { site: String, going_public: bool, result: Reachability },
    ProviderTested(Result<String, String>),
    ProviderSaved(Result<(), String>),
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
}

/// The USB drive the backup screen shows: the first one plugged in, with its backups when it
/// can be used, or why it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbView {
    pub drive: nodo_api::UsbDrive,
    pub list: Option<nodo_api::UsbBackupList>,
}
