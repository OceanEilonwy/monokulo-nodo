//! Application state, plus the events that change it and the effects it asks for.
//!
//! `update::update` turns `(state, event)` into a new state and a list of `Effect`s without any
//! I/O. `nodo_ui_core::exec::execute_effect` performs each effect against the backend, and its
//! outcomes come back as `Event::Done`. Tests can therefore drive every flow without a terminal
//! or a device.

use crossterm::event::KeyEvent;
use nodo_api::Exposure;

use nodo_ui_core::effect::UsbView;
use nodo_ui_core::flows::install::{Review, ReviewPurpose, Search};
use nodo_ui_core::flows::logs::Logs;
use nodo_ui_core::flows::services::AppPage;
use nodo_ui_core::flows::settings::{ApplyFailed, Picker, Settings};
use nodo_ui_core::flows::setup::Setup;
use nodo_ui_core::flows::unlock::Unlock;
use nodo_ui_core::flows::updates::{AutoUpdate, Batch, Rollbacks};
use nodo_ui_core::flows::web::certs::Certificate;
use nodo_ui_core::flows::web::hosting::{self, Hosting};
use nodo_ui_core::flows::web::public::{Cgnat, PublicCheck, PublicConfirm};
use nodo_ui_core::flows::{Notice, Question};
use nodo_ui_core::model::*;
use nodo_ui_core::queries::HomeItem;
use nodo_ui_core::screen::{ScreenId, Tab};
use nodo_ui_core::secret::{Input, Secret};
use nodo_ui_core::{Effect, Outcome};

/// Below this size the layout cannot show enough context to act safely, so input is refused.
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 16;

pub const TICKS_PER_SECOND: u64 = 4;
/// Get apps: the search box, which of the box and the list has the focus, and the chosen row.
/// What narrows the list besides the words typed is the core's `Search`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    pub flow: Search,
    pub query: Input,
    /// Letters filter while the query has focus and act as shortcuts once the list has focus.
    pub list_focused: bool,
    pub selected: usize,
}

impl std::ops::Deref for SearchState {
    type Target = Search;

    fn deref(&self) -> &Search {
        &self.flow
    }
}

impl std::ops::DerefMut for SearchState {
    fn deref_mut(&mut self) -> &mut Search {
        &mut self.flow
    }
}

/// The review page: what is reviewed and what approving does are the core's `Review`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewState {
    pub flow: Review,
    /// False until the user moves to it, so a stray Enter cancels.
    pub confirm_focused: bool,
    /// Shown after confirming a package whose new encrypted storage needs the device passphrase.
    pub passphrase: Option<Input>,
}

impl ReviewState {
    pub fn new(data: ReviewData, purpose: ReviewPurpose) -> Self {
        Self::of(Review::new(data, purpose))
    }

    pub fn of(flow: Review) -> Self {
        Self { flow, confirm_focused: false, passphrase: None }
    }
}

impl std::ops::Deref for ReviewState {
    type Target = Review;

    fn deref(&self) -> &Review {
        &self.flow
    }
}

impl std::ops::DerefMut for ReviewState {
    fn deref_mut(&mut self) -> &mut Review {
        &mut self.flow
    }
}

/// The unlock page: the core's rules, the passphrase box and when the next try is allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockState {
    pub flow: Unlock,
    pub input: Input,
    /// The tick from which a try is allowed again, after a wrong passphrase.
    pub retry_at_tick: u64,
}

impl UnlockState {
    /// `resume` is why a screen sent the owner here; see `Unlock::resume`.
    pub fn new(resume: Option<String>) -> Self {
        Self { flow: Unlock::new(resume), input: Input::secret(), retry_at_tick: 0 }
    }
}

impl Default for UnlockState {
    /// Written out because a derived default would make the passphrase input a plain one.
    fn default() -> Self {
        Self::new(None)
    }
}

impl std::ops::Deref for UnlockState {
    type Target = Unlock;

    fn deref(&self) -> &Unlock {
        &self.flow
    }
}

impl std::ops::DerefMut for UnlockState {
    fn deref_mut(&mut self) -> &mut Unlock {
        &mut self.flow
    }
}

/// What is being typed on a setup page, and the selected key. The page's rules and the keys
/// themselves are the core's `Setup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupStep {
    /// The passphrase twice. `confirming` is true once the first is entered.
    Passphrase { first: Input, second: Input, confirming: bool },
    Keys { selected: usize, adding: Option<Input> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupState {
    pub flow: Setup,
    /// Always the step of `flow.stage`.
    pub step: SetupStep,
}

impl SetupState {
    pub fn passphrase() -> Self {
        let step = SetupStep::Passphrase { first: Input::secret(), second: Input::secret(), confirming: false };
        Self { flow: Setup::passphrase(), step }
    }

    pub fn keys() -> Self {
        Self { flow: Setup::keys(), step: SetupStep::Keys { selected: 0, adding: None } }
    }
}

impl std::ops::Deref for SetupState {
    type Target = Setup;

    fn deref(&self) -> &Setup {
        &self.flow
    }
}

impl std::ops::DerefMut for SetupState {
    fn deref_mut(&mut self) -> &mut Setup {
        &mut self.flow
    }
}

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

/// Onion exposure: the node's restricted RPC first, then every site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnionState {
    pub status: Option<nodo_api::OnionStatus>,
    pub selected: usize,
    pub error: Option<String>,
    pub busy: bool,
}

impl OnionState {
    pub fn new() -> Self {
        Self { status: None, selected: 0, error: None, busy: true }
    }

    /// The rows in order: the node, when one is installed, then the sites.
    pub fn rows(&self) -> Vec<OnionRow> {
        let Some(status) = &self.status else { return Vec::new() };
        let node = status.node.iter().map(|_| OnionRow::Node);
        node.chain((0..status.sites.len()).map(OnionRow::Site)).collect()
    }
}

impl Default for OnionState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnionRow {
    Node,
    /// An index into `OnionStatus::sites`.
    Site(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreStep {
    /// Every removable drive, with why one cannot be used.
    Drives,
    /// The backups on the chosen drive.
    Backups { device: String },
    /// The backup passphrase. The helper verifies the whole file before showing what it holds.
    Passphrase { device: String, name: String, passphrase: Input },
    /// What comes back, the typed word and a new device passphrase, twice.
    Confirm {
        device: String,
        name: String,
        passphrase: Secret,
        contents: nodo_api::BackupContents,
        typed: Input,
        first: Input,
        second: Input,
        field: usize,
    },
    /// The helper is restoring. `started` is the tick it began at.
    Restoring { device: String, name: String, started: u64 },
    /// Nothing was set up. Enter tries the same backup again.
    Failed { device: String, name: String, reason: String },
    Done(nodo_api::RestoreReport),
}

/// The first-boot restore from a USB drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreState {
    pub drives: Vec<nodo_api::UsbDrive>,
    pub backups: Option<nodo_api::UsbBackupList>,
    pub selected: usize,
    pub step: RestoreStep,
    pub error: Option<String>,
    pub busy: bool,
}

impl RestoreState {
    pub fn new() -> Self {
        Self { drives: Vec::new(), backups: None, selected: 0, step: RestoreStep::Drives, error: None, busy: true }
    }

    pub fn drive(&self, device: &str) -> Option<&nodo_api::UsbDrive> {
        self.drives.iter().find(|d| d.device == device)
    }
}

impl Default for RestoreState {
    fn default() -> Self {
        Self::new()
    }
}

/// The page that undoes an update: the chosen row. The list, what is running and how the last
/// rollback went are the core's `Rollbacks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbacksState {
    pub flow: Rollbacks,
    /// Over the versions that can be put back.
    pub selected: usize,
}

impl RollbacksState {
    pub fn new() -> Self {
        Self { flow: Rollbacks::new(), selected: 0 }
    }
}

impl Default for RollbacksState {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for RollbacksState {
    type Target = Rollbacks;

    fn deref(&self) -> &Rollbacks {
        &self.flow
    }
}

impl std::ops::DerefMut for RollbacksState {
    fn deref_mut(&mut self) -> &mut Rollbacks {
        &mut self.flow
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkFocus {
    Wifi,
    Bluetooth,
}

/// A row of the Wi-Fi list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiRow {
    /// An index into the last scan.
    InRange(usize),
    /// An index into the saved networks, for one the last scan did not find.
    Saved(usize),
}

/// The passphrase for the network being joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiPrompt {
    pub ssid: String,
    pub security: nodo_api::WifiSecurity,
    pub passphrase: Input,
}

/// Where reading one radio's state from the helper stands, kept apart from the status itself so
/// a panel with nothing to show yet can tell asking from failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadioRead {
    Asking,
    /// Asked [`NO_ANSWER_TICKS`] ago and still no answer. The helper does one thing at a time,
    /// so the answer may only be waiting behind something slower.
    NoAnswer,
    Failed(String),
    Read,
}

/// How long the Network screen waits for the radios before saying there is no answer yet.
pub const NO_ANSWER_TICKS: u64 = 10 * TICKS_PER_SECOND;

/// Wi-Fi and Bluetooth: both radios, the networks in range and the saved ones, and devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkState {
    pub wifi: Option<nodo_api::WifiStatus>,
    pub wifi_read: RadioRead,
    pub bluetooth_read: RadioRead,
    /// The tick the radios were last asked for.
    pub asked_at: u64,
    /// The last scan, strongest first.
    pub networks: Vec<nodo_api::WifiNetwork>,
    pub bluetooth: Option<nodo_api::BluetoothStatus>,
    pub focus: NetworkFocus,
    pub wifi_selected: usize,
    pub bt_selected: usize,
    pub prompt: Option<WifiPrompt>,
    pub error: Option<String>,
    /// What the helper is doing, while it works.
    pub busy: Option<String>,
}

impl NetworkState {
    pub fn new() -> Self {
        Self {
            wifi: None,
            wifi_read: RadioRead::Asking,
            bluetooth_read: RadioRead::Asking,
            asked_at: 0,
            networks: Vec::new(),
            bluetooth: None,
            focus: NetworkFocus::Wifi,
            wifi_selected: 0,
            bt_selected: 0,
            prompt: None,
            error: None,
            busy: Some("Reading the radios…".to_owned()),
        }
    }

    /// Networks in range, then saved networks the last scan did not find.
    pub fn wifi_rows(&self) -> Vec<WifiRow> {
        let mut rows: Vec<WifiRow> = (0..self.networks.len()).map(WifiRow::InRange).collect();
        if let Some(status) = &self.wifi {
            for (index, saved) in status.saved.iter().enumerate() {
                if !self.networks.iter().any(|n| n.ssid == saved.ssid) {
                    rows.push(WifiRow::Saved(index));
                }
            }
        }
        rows
    }

    /// The name, security and saved state of a Wi-Fi row.
    pub fn wifi_row(&self, row: WifiRow) -> Option<(String, nodo_api::WifiSecurity, bool)> {
        match row {
            WifiRow::InRange(i) => self.networks.get(i).map(|n| (n.ssid.clone(), n.security, n.saved || self.is_saved(&n.ssid))),
            WifiRow::Saved(i) => self.wifi.as_ref()?.saved.get(i).map(|n| (n.ssid.clone(), n.security, true)),
        }
    }

    pub fn is_saved(&self, ssid: &str) -> bool {
        self.wifi.as_ref().is_some_and(|w| w.saved.iter().any(|s| s.ssid == ssid))
    }

    /// The network the device is joined to, if any.
    pub fn joined(&self) -> Option<&str> {
        let wifi = self.wifi.as_ref()?;
        (wifi.link == nodo_api::WifiLink::Connected).then_some(())?;
        wifi.ssid.as_deref()
    }

    /// Whether a radio with nothing to show failed or went unanswered, so asking again helps.
    pub fn can_ask_again(&self) -> bool {
        let stuck = |read: &RadioRead| matches!(read, RadioRead::NoAnswer | RadioRead::Failed(_));
        (self.wifi.is_none() && stuck(&self.wifi_read)) || (self.bluetooth.is_none() && stuck(&self.bluetooth_read))
    }

    /// Asks for both radios again, counting the wait from tick `now`.
    pub fn ask_again(&mut self, now: u64) -> Vec<Effect> {
        self.error = None;
        self.busy = Some("Reading the radios…".to_owned());
        self.wifi_read = RadioRead::Asking;
        self.bluetooth_read = RadioRead::Asking;
        self.asked_at = now;
        vec![Effect::LoadNetwork]
    }

    pub fn pairing_running(&self) -> bool {
        self.bluetooth.as_ref().and_then(|b| b.pairing.as_ref()).is_some_and(|p| {
            matches!(
                p.stage,
                nodo_api::PairingStage::Pairing
                    | nodo_api::PairingStage::TypePasskey(_)
                    | nodo_api::PairingStage::ComparePasskey(_)
            )
        })
    }
}

impl Default for NetworkState {
    fn default() -> Self {
        Self::new()
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

/// The Settings form: which field is chosen and what is being typed in it. The form, its draft
/// and every rule about them are the core's `Settings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsState {
    pub flow: Settings,
    pub selected: usize,
    /// A secret being typed. Other fields are typed in `editor`.
    pub editing: Option<Input>,
    /// Text and reference chips being typed in the selected field.
    pub editor: Option<nodo_ui_core::refs::PieceEditor>,
    /// The selected field's chip whose menu is open, and the highlighted action.
    pub chip_menu: Option<ChipMenu>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipMenu {
    /// Which reference of the field, counting from 0.
    pub chip: usize,
    /// 0 Replace…, 1 Type a value instead, 2 Remove.
    pub choice: usize,
}

pub const CHIP_ACTIONS: [&str; 3] = ["Replace…", "Type a value instead", "Remove"];

impl SettingsState {
    pub fn new(form: SettingsForm) -> Self {
        Self { flow: Settings::new(form), selected: 0, editing: None, editor: None, chip_menu: None }
    }
}

impl std::ops::Deref for SettingsState {
    type Target = Settings;

    fn deref(&self) -> &Settings {
        &self.flow
    }
}

impl std::ops::DerefMut for SettingsState {
    fn deref_mut(&mut self) -> &mut Settings {
        &mut self.flow
    }
}

/// The reference picker: the search box, which of the box and the list has the focus, and the
/// chosen row. What is listed, what fits and the approval of a locked setting are the core's
/// `Picker`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceState {
    pub flow: Picker,
    pub query: Input,
    pub list_focused: bool,
    pub selected: usize,
}

impl ReferenceState {
    /// The options the search shows, in the order they are listed, each with whether it fits.
    pub fn visible(&self) -> Vec<(&ReferenceOption, Result<(), String>)> {
        self.flow.visible(self.query.text())
    }
}

impl std::ops::Deref for ReferenceState {
    type Target = Picker;

    fn deref(&self) -> &Picker {
        &self.flow
    }
}

impl std::ops::DerefMut for ReferenceState {
    fn deref_mut(&mut self) -> &mut Picker {
        &mut self.flow
    }
}

/// The Updates page: the chosen row, the updates ticked, and the passphrase box when a batch
/// waits for it. What ticking, checking and updating do are the core's `flows::updates`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatesState {
    pub selected: usize,
    pub checked: Vec<String>,
    /// Asked before applying a batch in which an update grows encrypted storage.
    pub passphrase: Option<UpdatePassphrase>,
}

/// The device passphrase being typed for a batch of updates, and the batch it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePassphrase {
    pub batch: Batch,
    pub input: Input,
}

/// The automatic-update choices: the chosen row and the window being typed. The choices and
/// what is checked about them are the core's `AutoUpdate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoUpdateState {
    pub flow: AutoUpdate,
    /// 0 is the frequency selector, and the package rows follow it.
    pub selected: usize,
    pub editing_window: Option<Input>,
}

impl AutoUpdateState {
    pub fn new(prefs: AutoUpdatePrefs) -> Self {
        Self { flow: AutoUpdate::new(prefs), selected: 0, editing_window: None }
    }

    pub fn focused_package(&self) -> Option<usize> {
        self.selected.checked_sub(1)
    }
}

impl std::ops::Deref for AutoUpdateState {
    type Target = AutoUpdate;

    fn deref(&self) -> &AutoUpdate {
        &self.flow
    }
}

impl std::ops::DerefMut for AutoUpdateState {
    fn deref_mut(&mut self) -> &mut AutoUpdate {
        &mut self.flow
    }
}

/// What a batch of updates came to, and the chosen way on after a failure. What each way does
/// is the core's `flows::updates`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateResultsState {
    pub report: UpdateReport,
    pub choice: usize,
}

/// The logs page: which source is chosen and the search box. The lines and what narrows them
/// are the core's `Logs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsState {
    pub flow: Logs,
    /// An index into the device's log sources.
    pub selected: usize,
    pub find: Input,
    pub finding: bool,
}

impl LogsState {
    pub fn new(flow: Logs, selected: usize) -> Self {
        Self { flow, selected, find: Input::plain(""), finding: false }
    }
}

impl std::ops::Deref for LogsState {
    type Target = Logs;

    fn deref(&self) -> &Logs {
        &self.flow
    }
}

impl std::ops::DerefMut for LogsState {
    fn deref_mut(&mut self) -> &mut Logs {
        &mut self.flow
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityState {
    pub data: CapabilityData,
    pub choice: usize,
}

/// A site's Hosting page: the cursor, the boxes being typed in and the open domain list. The
/// draft and every rule about it are the core's `Hosting`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposureState {
    pub flow: Hosting,
    /// An index into [`ExposureState::order`].
    pub selected: usize,
    /// The part of the public address before the domain, as it is typed. `sync` hands it to the
    /// draft.
    pub sub: Input,
    /// The domain list is open.
    pub picking: bool,
    /// Index into the open domain list: the domains, then "Add a provider".
    pub pick: usize,
    pub details: bool,
    /// The new name being put together: the part before its domain, empty for the domain itself.
    pub alias_sub: Input,
    /// The new name's domain, one of [`App::domain_choices`].
    pub alias_domain: String,
    pub alias_mode: nodo_api::AliasMode,
    /// The open domain list is for the new name, not the main address.
    pub picking_alias: bool,
}

/// What can have focus on the Hosting screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostingFocus {
    Home,
    Tor,
    Internet,
    Domain,
    Address,
    /// One of the other names, by index into the draft's `aliases`: x removes it.
    Alias(usize),
    AliasDomain,
    AliasAddress,
    AliasMode,
    AliasAdd,
    AddProvider,
    Details,
    Save,
}

impl ExposureState {
    /// `host` is the site's saved public host; `domains` are [`App::domain_choices`].
    pub fn new(site: &str, draft: Exposure, host: Option<&str>, domains: &[(String, String)]) -> Self {
        Self::of(Hosting::new(site, draft, host, domains), domains)
    }

    pub fn of(flow: Hosting, domains: &[(String, String)]) -> Self {
        Self {
            selected: 0,
            sub: Input::plain(flow.sub.clone()),
            picking: false,
            pick: 0,
            details: false,
            alias_sub: Input::plain(String::new()),
            alias_domain: domains.first().map(|(d, _)| d.clone()).unwrap_or_default(),
            alias_mode: nodo_api::AliasMode::Redirect,
            picking_alias: false,
            flow,
        }
    }

    /// Hands what is typed to the draft and brings both in line with the providers connected
    /// now. Every key on the page starts with it.
    pub fn sync(&mut self, data: &Snapshot) {
        self.flow.set_sub(self.sub.text());
        self.flow.sync(data);
        self.alias_domain = hosting::domain_or_first(&self.alias_domain, &data.domain_choices());
    }

    /// The focus order: the rows the draft offers, with the new name's four controls and the
    /// details toggle, which are this screen's own.
    pub fn order(&self) -> Vec<HostingFocus> {
        let mut order = Vec::new();
        let mut save = false;
        for row in self.flow.rows() {
            match row {
                hosting::Row::Home => order.push(HostingFocus::Home),
                hosting::Row::Tor => order.push(HostingFocus::Tor),
                hosting::Row::Internet => order.push(HostingFocus::Internet),
                hosting::Row::Domain => order.push(HostingFocus::Domain),
                hosting::Row::Address => order.push(HostingFocus::Address),
                hosting::Row::Alias(index) => order.push(HostingFocus::Alias(index)),
                hosting::Row::NewAlias => {
                    order.extend([HostingFocus::AliasDomain, HostingFocus::AliasAddress, HostingFocus::AliasMode, HostingFocus::AliasAdd])
                }
                hosting::Row::AddProvider => order.push(HostingFocus::AddProvider),
                hosting::Row::Save => save = true,
            }
        }
        order.push(HostingFocus::Details);
        if save {
            order.push(HostingFocus::Save);
        }
        order
    }

    pub fn focus(&self) -> HostingFocus {
        let order = self.order();
        order[self.selected.min(order.len() - 1)]
    }

    /// The public host the choices make, from the box as it is typed.
    pub fn host(&self) -> String {
        hosting::join_host(self.sub.text(), &self.flow.domain)
    }

    /// The new name the choices make, the same way: an empty part is the domain itself.
    pub fn alias_host(&self) -> String {
        hosting::join_host(self.alias_sub.text(), &self.alias_domain)
    }

    /// The domain list is open, for either address.
    pub fn list_open(&self) -> bool {
        self.picking || self.picking_alias
    }
}

impl std::ops::Deref for ExposureState {
    type Target = Hosting;

    fn deref(&self) -> &Hosting {
        &self.flow
    }
}

impl std::ops::DerefMut for ExposureState {
    fn deref_mut(&mut self) -> &mut Hosting {
        &mut self.flow
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFocus {
    List,
    Domain,
    ApiKey,
    SecretKey,
}

/// Where the domain provider screens are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderStage {
    /// The providers added, with a button to add one.
    List,
    /// Choosing the company.
    Pick,
    /// The domain and key for the chosen company.
    Key,
    /// Connected. Holds the provider's name, the domains its key controls that sites can use, and
    /// the providers already holding the rest (the same account connected twice, most often).
    Done { provider: String, domains: Vec<String>, held_by: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvidersState {
    pub stage: ProviderStage,
    /// The chosen company, an index into the provider catalog.
    pub selected: usize,
    pub focus: ProviderFocus,
    pub domain: Input,
    pub api_key: Input,
    pub secret_key: Input,
    pub test: Option<Result<String, String>>,
    /// The site whose Hosting screen opened this, to go back to once a provider is added.
    pub return_to: Option<String>,
}

impl ProvidersState {
    /// The add flow, at choosing the company.
    pub fn new(domain: &str) -> Self {
        Self {
            stage: ProviderStage::Pick,
            selected: 0,
            focus: ProviderFocus::List,
            domain: Input::plain(domain),
            api_key: Input::secret(),
            secret_key: Input::secret(),
            test: None,
            return_to: None,
        }
    }

    /// The list of providers added.
    pub fn list(domain: &str) -> Self {
        Self { stage: ProviderStage::List, ..Self::new(domain) }
    }
}

/// The checks that found a problem: whether the details are open. The checks and what can be
/// done about them are the core's `PublicCheck`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebCheckState {
    pub flow: PublicCheck,
    pub details: bool,
}

impl std::ops::Deref for WebCheckState {
    type Target = PublicCheck;

    fn deref(&self) -> &PublicCheck {
        &self.flow
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmFocus {
    Cancel,
    GoPublic,
}

/// The confirmation before going public: which button has the focus. What is confirmed is the
/// core's `PublicConfirm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebConfirmState {
    pub flow: PublicConfirm,
    pub focus: ConfirmFocus,
    pub details: bool,
}

impl WebConfirmState {
    /// The focus starts on Cancel, as the core says the answer does.
    pub fn new(flow: PublicConfirm) -> Self {
        let focus = if flow.cancel_is_default() { ConfirmFocus::Cancel } else { ConfirmFocus::GoPublic };
        Self { flow, focus, details: false }
    }
}

impl std::ops::Deref for WebConfirmState {
    type Target = PublicConfirm;

    fn deref(&self) -> &PublicConfirm {
        &self.flow
    }
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

/// The CGNAT guidance: the chosen way on. What each way does is the core's `Cgnat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgnatState {
    pub flow: Cgnat,
    /// An index into the core's `CgnatWay::ALL`.
    pub choice: usize,
    pub details: bool,
}

impl std::ops::Deref for CgnatState {
    type Target = Cgnat;

    fn deref(&self) -> &Cgnat {
        &self.flow
    }
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
    /// Sources (feeds). `keys` shows each one's signing key.
    Feeds { selected: usize, keys: bool },
    AddFeed(AddFeedState),
    FeedKey(FeedKeyState),
    Settings(SettingsState),
    Reference(ReferenceState),
    ApplyFailed(ApplyFailed),
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
    /// A public or local host while Caddy gets its certificate.
    WebCertificate(Certificate),
    /// The local CA's fingerprint and the page where LAN devices get its root.
    WebTrust,
    Setup(SetupState),
    Backup(BackupState),
    Onion(OnionState),
    Restore(RestoreState),
    Rollbacks(RollbacksState),
    Network(NetworkState),
    Apps { selected: usize },
    AppPage(AppPageState),
    AppRemove(AppRemoveState),
    System { selected: usize },
    Bundle(Box<crate::app::BundleState>),
    OwnerPage(Box<OwnerPageState>),
}

/// One installed app's page: its status and update, then its setup page when it has one, Hosting,
/// Settings and Logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPageState {
    /// Which app, with what its buttons do.
    pub page: AppPage,
    pub selected: usize,
    /// At 80 columns the page actions fold into "⋯ More", which opens them as a list.
    pub more_open: bool,
}

impl AppPageState {
    pub fn new(name: &str) -> Self {
        Self { page: AppPage::new(name), selected: 0, more_open: false }
    }
}

impl std::ops::Deref for AppPageState {
    type Target = AppPage;

    fn deref(&self) -> &AppPage {
        &self.page
    }
}

/// Where installing a bundle stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleStep {
    /// The apps it installs with their permissions, the grants and the data drive choices.
    Review,
    /// The settings the bundle asks for once, before anything installs.
    Answers,
    /// The device passphrase, for new encrypted storage or a secret answer while locked.
    Passphrase,
    /// The helper is installing.
    Installing,
    Done(nodo_api::BundleReport),
}

/// Installing a suite of apps together, from the review of the head that declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleState {
    pub review: nodo_api::BundleReview,
    pub step: BundleStep,
    /// In the review: an index into the data drive toggles, then Cancel, then Continue. In the
    /// questions: the field.
    pub focus: usize,
    /// Per `review.data_drive`: keep that app's data on the data drive. On by default when one
    /// is present.
    pub drive: Vec<bool>,
    /// One per ask, in order. A secret ask is a masked field.
    pub answers: Vec<Input>,
    pub passphrase: Input,
    pub error: Option<String>,
    /// Rows scrolled past in a long review or result.
    pub scroll: usize,
}

impl BundleState {
    pub fn new(review: nodo_api::BundleReview) -> Self {
        let drive = vec![review.data_drive_present; review.data_drive.len()];
        let answers = review.asks.iter().map(|a| if a.secret { Input::secret() } else { Input::plain("") }).collect();
        let focus = drive.len();
        Self { review, step: BundleStep::Review, focus, drive, answers, passphrase: Input::secret(), error: None, scroll: 0 }
    }

    /// The asks the owner answers: those for apps not installed yet.
    pub fn open_asks(&self) -> Vec<usize> {
        (0..self.review.asks.len()).filter(|&i| !self.review.asks[i].skipped).collect()
    }

    /// The review's Cancel button, after the toggles; Continue is the one after it.
    pub fn cancel_index(&self) -> usize {
        self.drive.len()
    }

    /// The apps this install puts their data on the data drive for.
    pub fn chosen_drive(&self) -> Vec<String> {
        self.review.data_drive.iter().zip(&self.drive).filter(|(_, on)| **on).map(|(id, _)| id.clone()).collect()
    }

    /// The answers as the helper takes them, every value a secret.
    pub fn answers_for_helper(&self) -> Vec<nodo_api::BundleAnswer> {
        self.open_asks()
            .into_iter()
            .map(|i| {
                let ask = &self.review.asks[i];
                nodo_api::BundleAnswer {
                    package: ask.package.clone(),
                    setting: ask.setting.clone(),
                    value: nodo_api::Secret::new(self.answers[i].secret_value().expose()),
                }
            })
            .collect()
    }
}

/// An app's setup page: the passphrase first, then the checklist. The passphrase stays in memory
/// only while the page is open, so `r` can fetch it again; leaving the page drops it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerPageState {
    pub package: String,
    /// The row's name on the app's page, such as "Setup".
    pub title: String,
    pub passphrase: Input,
    pub held: Option<Secret>,
    pub view: Option<nodo_api::OwnerPageView>,
    pub error: Option<String>,
    /// A fetch is under way.
    pub loading: bool,
    /// Rows scrolled past.
    pub scroll: usize,
    /// An index into the core's `documents::copy_targets`.
    pub focus: usize,
    /// The copy target last copied, for the "Copied" note.
    pub copied: Option<usize>,
    /// Keep the focused copy target in view (after Tab); arrows scroll freely.
    pub follow: bool,
}

impl OwnerPageState {
    pub fn new(package: &str, title: &str) -> Self {
        Self {
            package: package.to_owned(),
            title: title.to_owned(),
            passphrase: Input::secret(),
            held: None,
            view: None,
            error: None,
            loading: false,
            scroll: 0,
            focus: 0,
            copied: None,
            follow: false,
        }
    }
}

/// Removing an app asks first. The confirm button starts unfocused, so Enter cancels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRemoveState {
    pub name: String,
    /// What is asked and what confirming does, from the core. It is drawn as a page of its own.
    pub question: Question,
    pub confirm_focused: bool,
}

impl AppRemoveState {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_owned(), question: nodo_ui_core::flows::services::ask_remove(name), confirm_focused: false }
    }
}

/// The rows of the System page, in order.
pub const SYSTEM_ROWS: [&str; 5] = ["Updates", "Backups", "Network", "Device logs", "About this Nodo"];

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
            Screen::WebTrust => ScreenId::WebTrust,
            Screen::Setup(_) => ScreenId::Setup,
            Screen::Backup(_) => ScreenId::Backup,
            Screen::Onion(_) => ScreenId::Onion,
            Screen::Restore(_) => ScreenId::Restore,
            Screen::Rollbacks(_) => ScreenId::Rollbacks,
            Screen::Network(_) => ScreenId::Network,
            Screen::Apps { .. } => ScreenId::Apps,
            Screen::AppPage(_) => ScreenId::AppPage,
            Screen::AppRemove(_) => ScreenId::AppRemove,
            Screen::System { .. } => ScreenId::System,
            Screen::Bundle(_) => ScreenId::Bundle,
            Screen::OwnerPage(_) => ScreenId::OwnerPage,
        }
    }

    /// The tab a screen's top-level page belongs to: the screens the tab strip itself opens.
    pub fn tab(&self) -> Option<Tab> {
        match self {
            Screen::Home { .. } | Screen::NoDrive { .. } => Some(Tab::Home),
            Screen::Apps { .. } => Some(Tab::Apps),
            Screen::Web { .. } => Some(Tab::Web),
            Screen::System { .. } => Some(Tab::System),
            _ => None,
        }
    }

    /// The tab shown as current: every screen lives under one of the four.
    pub fn area(&self) -> Tab {
        match self {
            Screen::Home { .. } | Screen::NoDrive { .. } | Screen::Unlock(_) | Screen::Setup(_) | Screen::Restore(_) => Tab::Home,
            // An app's Hosting and its steps live on the app's page, under Apps.
            Screen::Web { .. } | Screen::WebDomain(_) | Screen::WebTrust | Screen::Onion(_) => Tab::Web,
            Screen::WebProviders(state) if state.return_to.is_none() => Tab::Web,
            Screen::System { .. }
            | Screen::Updates(_)
            | Screen::AutoUpdate(_)
            | Screen::UpdateResults(_)
            | Screen::Rollbacks(_)
            | Screen::Backup(_)
            | Screen::Network(_) => Tab::System,
            Screen::Logs(state) if state.app.is_none() => Tab::System,
            _ => Tab::Apps,
        }
    }
}

/// A question on top of the screen, and which of its two buttons has the focus. The wording and
/// what confirming does are the core's `Question`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modal {
    pub question: Question,
    pub confirm_focused: bool,
}

impl Modal {
    /// The focus starts on Cancel unless the question says its default is to go ahead.
    pub fn ask(question: Question) -> Self {
        let confirm_focused = !question.cancel_is_default;
        Self { question, confirm_focused }
    }
}

/// The question's own fields read as the modal's, which is how the views and tests name them.
impl std::ops::Deref for Modal {
    type Target = Question;

    fn deref(&self) -> &Question {
        &self.question
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    pub doc: Document,
    pub scroll: usize,
}

/// A button whose press started work that is still running: it is drawn with the hourglass and
/// cannot be pressed again until a result arrives, the screen changes or the worker goes idle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// What the button does, which is how the view finds it.
    pub click: crate::hit::Click,
    pub screen: ScreenId,
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
    /// The button whose work is running, if any.
    pub pending: Option<Pending>,
    /// Where the last drawn screen can be clicked. Set by the view, read by mouse events.
    pub hits: std::cell::RefCell<Vec<crate::hit::Hit>>,
    /// The click target under the mouse pointer, drawn with a hover fill.
    pub hover: Option<ratatui::layout::Rect>,
    /// Text to put on the terminal's clipboard (OSC 52). The runtime writes it and clears it.
    pub clipboard: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    // These only read the snapshot, where the rules live (`nodo_ui_core::queries`). They stay
    // here as short names for the views.

    /// The sites that are online somewhere: on the home network, Tor or the internet.
    pub fn active_sites(&self) -> Vec<&Site> {
        self.data.active_sites()
    }

    /// A domain provider is connected with at least one domain an app can use.
    pub fn has_provider(&self) -> bool {
        self.data.has_provider()
    }

    /// The domains apps can be put under, each with its provider's name.
    pub fn domain_choices(&self) -> Vec<(String, String)> {
        self.data.domain_choices()
    }

    pub fn home_items(&self) -> Vec<HomeItem> {
        self.data.home_items()
    }

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
            pending: None,
            hits: std::cell::RefCell::new(Vec::new()),
            hover: None,
            clipboard: None,
        }
    }

    pub fn too_small(&self) -> bool {
        self.size.0 < MIN_WIDTH || self.size.1 < MIN_HEIGHT
    }

    /// Held-back versions are not offered, so they do not count towards the badge.
    pub fn pending_updates(&self) -> usize {
        self.data.pending_updates()
    }

    pub fn is_locked(&self) -> bool {
        self.data.is_locked()
    }

    /// Available updates first, then held ones, matching the rows the Updates screen shows.
    pub fn update_rows(&self) -> Vec<&UpdateItem> {
        self.data.update_rows()
    }

    pub fn search_results(&self, state: &SearchState) -> Vec<&PackageSummary> {
        state.flow.results(&self.data, state.query.text())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    /// The left button went down.
    Down,
    Moved,
    ScrollUp,
    ScrollDown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Key(KeyEvent),
    /// A mouse event at a cell, as crossterm reports it. There is no hover event of its own: a
    /// move over a target is one.
    Mouse { kind: MouseKind, column: u16, row: u16 },
    Paste(String),
    Tick,
    Resize(u16, u16),
    /// What an effect came to.
    Done(Outcome),
}
