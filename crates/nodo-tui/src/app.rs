//! Application state, plus the events that change it and the effects it asks for.
//!
//! `update::update` turns `(state, event)` into a new state and a list of `Effect`s without any
//! I/O. `nodo_ui_core::exec::execute_effect` performs each effect against the backend, and its
//! outcomes come back as `Event::Done`. Tests can therefore drive every flow without a terminal
//! or a device.

use crossterm::event::KeyEvent;
use nodo_api::Exposure;

use nodo_ui_core::flows::backup::{self, Backups, Restore};
use nodo_ui_core::flows::install::{Review, ReviewPurpose, Search};
use nodo_ui_core::flows::logs::Logs;
use nodo_ui_core::flows::network::{self, Network};
use nodo_ui_core::flows::services::AppPage;
use nodo_ui_core::flows::settings::{ApplyFailed, Picker, Settings};
use nodo_ui_core::flows::setup::Setup;
use nodo_ui_core::flows::unlock::Unlock;
use nodo_ui_core::flows::updates::{AutoUpdate, Batch, Rollbacks};
use nodo_ui_core::flows::web::certs::Certificate;
use nodo_ui_core::flows::web::domain::{self, Domain};
use nodo_ui_core::flows::web::hosting::{self, Hosting};
use nodo_ui_core::flows::web::onion::Onion;
use nodo_ui_core::flows::web::providers::{KeyField, Providers};
use nodo_ui_core::flows::web::public::{Cgnat, PublicCheck, PublicConfirm};
use nodo_ui_core::flows::{Confirm, Notice, Question};
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

/// The boxes of the Backups page's step and which one has the focus. Always the step of
/// `flow.stage`, with a copy of what that stage shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupStep {
    List,
    /// The device passphrase, when the device has one, then the backup passphrase twice.
    Create { device: Input, first: Input, second: Input, field: usize },
    /// The backup passphrase.
    Unlock { name: String, passphrase: Input },
    /// The typed word, and a new device passphrase when the restore asks for one.
    Confirm { name: String, contents: nodo_api::BackupContents, typed: Input, device: Input, field: usize },
    Restored(nodo_api::RestoreReport),
}

/// The Backups page: the chosen rows and the boxes. The lists, the order of the steps and what
/// each one checks are the core's `Backups`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupState {
    pub flow: Backups,
    pub selected: usize,
    pub step: BackupStep,
    /// The USB list has the focus rather than the device's own list.
    pub usb_focused: bool,
    pub usb_selected: usize,
}

impl BackupState {
    pub fn new(device_set_up: bool) -> Self {
        Self::of(Backups::new(device_set_up))
    }

    /// The first-boot restore, which only offers USB drives.
    pub fn fresh() -> Self {
        Self { usb_focused: true, ..Self::of(Backups::fresh()) }
    }

    pub fn of(flow: Backups) -> Self {
        Self { flow, selected: 0, step: BackupStep::List, usb_focused: false, usb_selected: 0 }
    }

    /// Brings the boxes in line with the step the flow is on. A step that stays keeps its boxes.
    pub fn sync(&mut self) {
        let same = matches!(
            (&self.flow.stage, &self.step),
            (backup::Stage::List, BackupStep::List)
                | (backup::Stage::Create, BackupStep::Create { .. })
                | (backup::Stage::Unlock { .. }, BackupStep::Unlock { .. })
                | (backup::Stage::Confirm { .. }, BackupStep::Confirm { .. })
                | (backup::Stage::Restored(_), BackupStep::Restored(_))
        );
        if same {
            return;
        }
        self.step = match &self.flow.stage {
            backup::Stage::List => BackupStep::List,
            backup::Stage::Create => BackupStep::Create {
                device: Input::secret(),
                first: Input::secret(),
                second: Input::secret(),
                field: if self.flow.asks_device_passphrase() { 0 } else { 1 },
            },
            backup::Stage::Unlock { name } => BackupStep::Unlock { name: name.clone(), passphrase: Input::secret() },
            backup::Stage::Confirm { name, contents, .. } => BackupStep::Confirm {
                name: name.clone(),
                contents: contents.clone(),
                typed: Input::plain(""),
                device: Input::secret(),
                field: 0,
            },
            backup::Stage::Restored(report) => BackupStep::Restored(report.clone()),
        };
    }
}

impl std::ops::Deref for BackupState {
    type Target = Backups;

    fn deref(&self) -> &Backups {
        &self.flow
    }
}

impl std::ops::DerefMut for BackupState {
    fn deref_mut(&mut self) -> &mut Backups {
        &mut self.flow
    }
}

/// The onion page: the chosen row./// The onion page: the chosen row. What Tor reports and what each switch does are the core's
/// `Onion`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnionState {
    pub flow: Onion,
    /// An index into the flow's `rows`.
    pub selected: usize,
}

impl OnionState {
    pub fn new() -> Self {
        Self { flow: Onion::new(), selected: 0 }
    }
}

impl Default for OnionState {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for OnionState {
    type Target = Onion;

    fn deref(&self) -> &Onion {
        &self.flow
    }
}

/// The boxes of the first-boot restore's step and which one has the focus. Always the step of
/// `flow.stage`, with a copy of what that stage shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreStep {
    /// Every removable drive, with why one cannot be used.
    Drives,
    /// The backups on the chosen drive.
    Backups { device: String },
    /// The backup passphrase.
    Passphrase { device: String, name: String, passphrase: Input },
    /// What comes back, the typed word and a new device passphrase, twice.
    Confirm {
        device: String,
        name: String,
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

/// The first-boot restore from a USB drive: the chosen row and the boxes. The drives, the order
/// of the steps and what each one checks are the core's `Restore`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreState {
    pub flow: Restore,
    pub selected: usize,
    pub step: RestoreStep,
}

impl RestoreState {
    pub fn new() -> Self {
        Self { flow: Restore::new(), selected: 0, step: RestoreStep::Drives }
    }

    /// Brings the boxes in line with the step the flow is on. A step that stays keeps its boxes.
    /// `now` is the tick, for a restore that starts.
    pub fn sync(&mut self, now: u64) {
        use backup::RestoreStage as Stage;
        let same = matches!(
            (&self.flow.stage, &self.step),
            (Stage::Drives, RestoreStep::Drives)
                | (Stage::Backups { .. }, RestoreStep::Backups { .. })
                | (Stage::Passphrase { .. }, RestoreStep::Passphrase { .. })
                | (Stage::Confirm { .. }, RestoreStep::Confirm { .. })
                | (Stage::Restoring { .. }, RestoreStep::Restoring { .. })
                | (Stage::Failed { .. }, RestoreStep::Failed { .. })
                | (Stage::Done(_), RestoreStep::Done(_))
        );
        if same {
            return;
        }
        self.step = match &self.flow.stage {
            Stage::Drives => RestoreStep::Drives,
            Stage::Backups { device } => RestoreStep::Backups { device: device.clone() },
            Stage::Passphrase { device, name } => {
                RestoreStep::Passphrase { device: device.clone(), name: name.clone(), passphrase: Input::secret() }
            }
            Stage::Confirm { device, name, contents, .. } => RestoreStep::Confirm {
                device: device.clone(),
                name: name.clone(),
                contents: contents.clone(),
                typed: Input::plain(""),
                first: Input::secret(),
                second: Input::secret(),
                field: 0,
            },
            Stage::Restoring { device, name } => {
                RestoreStep::Restoring { device: device.clone(), name: name.clone(), started: now }
            }
            Stage::Failed { device, name, reason } => {
                RestoreStep::Failed { device: device.clone(), name: name.clone(), reason: reason.clone() }
            }
            Stage::Done(report) => RestoreStep::Done(report.clone()),
        };
    }
}

impl Default for RestoreState {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for RestoreState {
    type Target = Restore;

    fn deref(&self) -> &Restore {
        &self.flow
    }
}

impl std::ops::DerefMut for RestoreState {
    fn deref_mut(&mut self) -> &mut Restore {
        &mut self.flow
    }
}

/// The page that undoes an update: the chosen row./// The page that undoes an update: the chosen row. The list, what is running and how the last
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

/// How long the Network screen waits for the radios before saying there is no answer yet: the
/// core's wait, in ticks.
pub const NO_ANSWER_TICKS: u64 = network::NO_ANSWER_SECONDS * TICKS_PER_SECOND;

/// The Network page: which radio and which row are chosen, the passphrase box and when the
/// radios were asked. The radios, the lists and every rule about them are the core's `Network`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkState {
    pub flow: Network,
    /// The tick the radios were last asked for.
    pub asked_at: u64,
    pub focus: NetworkFocus,
    pub wifi_selected: usize,
    pub bt_selected: usize,
    /// The passphrase of the network being joined, typed while the flow's `prompt` is set.
    pub passphrase: Input,
}

impl NetworkState {
    pub fn new() -> Self {
        Self {
            flow: Network::new(),
            asked_at: 0,
            focus: NetworkFocus::Wifi,
            wifi_selected: 0,
            bt_selected: 0,
            passphrase: Input::secret(),
        }
    }

    /// Asks for both radios again, counting the wait from tick `now`.
    pub fn ask_again(&mut self, now: u64) -> Vec<Effect> {
        self.asked_at = now;
        self.flow.ask_again()
    }

    /// The owner confirmed a question of this page. The scan goes with the radio, and the cursor
    /// with the scan.
    pub fn confirmed(&mut self, confirm: &Confirm) {
        self.flow.confirmed(confirm);
        if matches!(confirm, Confirm::WifiOff) {
            self.wifi_selected = 0;
        }
    }

    /// Keeps both selections inside their lists.
    pub fn clamp(&mut self) {
        let rows = self.flow.wifi_rows().len();
        if self.wifi_selected >= rows {
            self.wifi_selected = rows.saturating_sub(1);
        }
        let devices = self.flow.bluetooth.as_ref().map_or(0, |b| b.devices.len());
        if self.bt_selected >= devices {
            self.bt_selected = devices.saturating_sub(1);
        }
    }
}

impl Default for NetworkState {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for NetworkState {
    type Target = Network;

    fn deref(&self) -> &Network {
        &self.flow
    }
}

impl std::ops::DerefMut for NetworkState {
    fn deref_mut(&mut self) -> &mut Network {
        &mut self.flow
    }
}

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

impl From<KeyField> for ProviderFocus {
    fn from(field: KeyField) -> Self {
        match field {
            KeyField::Domain => ProviderFocus::Domain,
            KeyField::ApiKey => ProviderFocus::ApiKey,
            KeyField::SecretKey => ProviderFocus::SecretKey,
        }
    }
}

/// The provider screens: the cursor and the boxes of the key step. Where the screens are and
/// what each step does are the core's `Providers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvidersState {
    pub flow: Providers,
    /// The chosen row: a connected provider in the list, a company of the catalog while adding.
    pub selected: usize,
    pub focus: ProviderFocus,
    pub domain: Input,
    pub api_key: Input,
    pub secret_key: Input,
}

impl ProvidersState {
    /// The add flow, at choosing the company.
    pub fn new(domain: &str) -> Self {
        Self::of(Providers::adding(None), domain)
    }

    /// The list of providers added.
    pub fn list(domain: &str) -> Self {
        Self::of(Providers::list(), domain)
    }

    pub fn of(flow: Providers, domain: &str) -> Self {
        Self {
            flow,
            selected: 0,
            focus: ProviderFocus::List,
            domain: Input::plain(domain),
            api_key: Input::secret(),
            secret_key: Input::secret(),
        }
    }

    /// Empties the boxes of the key step. Keys typed for one provider must never be sent to
    /// another, so every way into and out of that step starts from this.
    pub fn empty_boxes(&mut self) {
        self.domain = Input::plain("");
        self.api_key = Input::secret();
        self.secret_key = Input::secret();
        self.focus = ProviderFocus::List;
    }
}

impl std::ops::Deref for ProvidersState {
    type Target = Providers;

    fn deref(&self) -> &Providers {
        &self.flow
    }
}

impl std::ops::DerefMut for ProvidersState {
    fn deref_mut(&mut self) -> &mut Providers {
        &mut self.flow
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

/// Domains and certificates: the field that has the focus and what is typed. The choices, what
/// Save sends and what comes back are the core's `Domain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainState {
    pub flow: Domain,
    /// One of the core's `domain::fields`.
    pub focus: domain::Field,
    pub email: Input,
    pub local_domain: Input,
    pub dns01_first: Input,
    pub dns01_second: Input,
}

impl DomainState {
    pub fn new(acme: &AcmeView, domain: &str) -> Self {
        Self {
            flow: Domain::new(acme),
            focus: domain::Field::Email,
            email: Input::plain(acme.email.clone()),
            local_domain: Input::plain(domain::suggested_local_domain(acme, domain)),
            dns01_first: Input::secret(),
            dns01_second: Input::secret(),
        }
    }
}

impl std::ops::Deref for DomainState {
    type Target = Domain;

    fn deref(&self) -> &Domain {
        &self.flow
    }
}

impl std::ops::DerefMut for DomainState {
    fn deref_mut(&mut self) -> &mut Domain {
        &mut self.flow
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
