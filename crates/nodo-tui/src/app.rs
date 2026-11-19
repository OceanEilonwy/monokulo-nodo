//! Application state, plus the events that change it and the effects it asks for.
//!
//! `update::update` turns `(state, event)` into a new state and a list of `Effect`s without any
//! I/O. `nodo_ui_core::exec::execute_effect` performs each effect against the backend, and its
//! outcomes come back as `Event::Done`. Tests can therefore drive every flow without a terminal
//! or a device.

use crossterm::event::KeyEvent;
use nodo_api::Exposure;

use nodo_ui_core::effect::{RollbackView, UsbView};
use nodo_ui_core::flows::install::{Review, ReviewPurpose, Search};
use nodo_ui_core::flows::services::AppPage;
use nodo_ui_core::flows::setup::Setup;
use nodo_ui_core::flows::unlock::Unlock;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbacksState {
    pub view: Option<RollbackView>,
    /// Over `view.options`.
    pub selected: usize,
    pub error: Option<String>,
    pub busy: bool,
    /// The last rollback's outcome, shown until the next action.
    pub result: Option<nodo_api::UpdateResult>,
}

impl RollbacksState {
    pub fn new() -> Self {
        Self { view: None, selected: 0, error: None, busy: true, result: None }
    }
}

impl Default for RollbacksState {
    fn default() -> Self {
        Self::new()
    }
}

/// Which list on the Network screen has the focus.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsState {
    pub form: SettingsForm,
    pub draft: Vec<FieldValue>,
    pub selected: usize,
    /// A secret being typed. Other fields are typed in `editor`.
    pub editing: Option<Input>,
    /// Text and reference chips being typed in the selected field.
    pub editor: Option<nodo_ui_core::refs::PieceEditor>,
    /// What the other apps offer to reference, once loaded.
    pub catalog: Option<ReferenceList>,
    /// The latest preview of each field's draft, from the helper.
    pub previews: Vec<Option<FieldPreview>>,
    /// The selected field's chip whose menu is open, and the highlighted action.
    pub chip_menu: Option<ChipMenu>,
    /// One step back, offered as "↺ …" under the field while it applies.
    pub undo: Option<Undo>,
    /// The save confirmation, listing what the changed references allow.
    pub confirm: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipMenu {
    /// Which reference of the field, counting from 0.
    pub chip: usize,
    /// 0 Replace…, 1 Type a value instead, 2 Remove.
    pub choice: usize,
}

pub const CHIP_ACTIONS: [&str; 3] = ["Replace…", "Type a value instead", "Remove"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undo {
    pub field: usize,
    /// The value before the step, and the value right after it: the link shows only while the
    /// field still holds what the step made, so it never undoes something else.
    pub before: FieldValue,
    pub after: FieldValue,
    pub label: String,
}

impl SettingsState {
    pub fn new(form: SettingsForm) -> Self {
        let draft: Vec<FieldValue> = form.fields.iter().map(|field| field.value.clone()).collect();
        let previews = vec![None; draft.len()];
        Self {
            form,
            draft,
            selected: 0,
            editing: None,
            editor: None,
            catalog: None,
            previews,
            chip_menu: None,
            undo: None,
            confirm: None,
        }
    }

    pub fn changed_indices(&self) -> Vec<usize> {
        (0..self.draft.len()).filter(|&i| self.draft[i] != self.form.fields[i].value).collect()
    }

    pub fn error_for(&self, index: usize) -> Option<String> {
        let field = &self.form.fields[index];
        let empty = self.draft[index] == FieldValue::Text(String::new());
        // An optional field left empty has no value, which is always allowed; only what is typed
        // has to fit the kind.
        if !field.required && empty {
            return None;
        }
        // A required one must have a value. Only a number's parse caught this before, so a fresh
        // install with an empty required name or choice saved, and the app could not start. An
        // unset secret is left to the helper: it may be one the helper makes when it saves.
        if empty {
            return Some("Required: give it a value.".to_owned());
        }
        validate(&field.kind, &self.draft[index])
    }

    /// The preview of a field, if it is for the value the field holds now.
    pub fn preview(&self, index: usize) -> Option<&FieldPreview> {
        self.previews.get(index)?.as_ref().filter(|p| p.for_value.as_ref() == Some(&self.draft[index]))
    }

    /// References that do not resolve, over every field, as far as the previews know.
    pub fn broken(&self) -> usize {
        (0..self.draft.len()).filter_map(|i| self.preview(i)).map(FieldPreview::broken).sum()
    }

    /// The undo step for a field, while the field still holds what that step made.
    pub fn undo_for(&self, index: usize) -> Option<&Undo> {
        self.undo.as_ref().filter(|u| u.field == index && self.draft[index] == u.after)
    }
}

/// Field validation, kept pure so that it can be tested and so save can refuse early.
pub fn validate(kind: &FieldKind, value: &FieldValue) -> Option<String> {
    // A value with references is checked by the helper's preview, against what it resolves to.
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

/// Where a picked reference goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    /// At the cursor of the field's editor, or as the whole value when nothing is being typed.
    Insert,
    /// In place of this reference of the field.
    Replace(usize),
}

/// The one reference picker, for every field: search everything the other apps offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceState {
    pub package: String,
    pub field_index: usize,
    pub field_label: String,
    pub field_kind: FieldKind,
    pub mode: PickMode,
    /// Whether the reference would sit beside other text or references.
    pub assembled: bool,
    pub list: ReferenceList,
    pub query: Input,
    pub list_focused: bool,
    pub selected: usize,
    /// A locked setting the user chose, waiting for their approval.
    pub granting: Option<ReferenceOption>,
}

impl ReferenceState {
    /// The options the search shows, in the order they are listed, each with whether it fits.
    pub fn visible(&self) -> Vec<(&ReferenceOption, Result<(), String>)> {
        nodo_ui_core::refs::ordered(&self.field_kind, nodo_ui_core::refs::search(&self.list.options, self.query.text()), self.assembled)
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
    /// Asked before applying a batch in which an update grows encrypted storage.
    pub passphrase: Option<UpdatePassphrase>,
}

/// The device passphrase for a batch of updates, and the batch it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePassphrase {
    pub names: Vec<String>,
    pub approvals: Vec<(String, nodo_api::PermissionApproval)>,
    /// Each growing update in words, such as "vault: 256 MB to 512 MB".
    pub growing: Vec<String>,
    pub input: Input,
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
    /// Opened from an app's page: only that app's log, under Apps. Otherwise the device logs,
    /// under System, with every source as a filter.
    pub app: Option<String>,
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
    /// An index into [`ExposureState::order`].
    pub selected: usize,
    /// The part of the public address before the domain, such as `pay` in `pay.example.com`.
    pub sub: Input,
    /// The chosen domain, one of [`App::domain_choices`].
    pub domain: String,
    /// The domain list is open.
    pub picking: bool,
    /// Index into the open domain list: the domains, then "Add a provider".
    pub pick: usize,
    pub details: bool,
    /// A domain provider is connected, so the internet choice can pick a domain.
    pub has_provider: bool,
    /// The other names the site answers at, as they will be saved.
    pub aliases: Vec<nodo_api::PublicAlias>,
    /// The new name being put together: the part before its domain, empty for the domain itself.
    pub alias_sub: Input,
    /// The new name's domain, one of [`App::domain_choices`].
    pub alias_domain: String,
    pub alias_mode: nodo_api::AliasMode,
    /// The open domain list is for the new name, not the main address.
    pub picking_alias: bool,
    /// Why the new name could not be added.
    pub alias_error: Option<String>,
    /// The site was public when the screen opened. The helper then lets names be removed but not
    /// added, since going public confirmed the names it had.
    pub was_public: bool,
}

/// What can have focus on the Hosting screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostingFocus {
    Home,
    Tor,
    Internet,
    Domain,
    Address,
    /// One of the other names, by index into [`ExposureState::aliases`]: x removes it.
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
    /// `host` is the site's saved public host; `domains` are [`App::domain_choices`]. The domain is
    /// the longest one the host is under, or the first.
    pub fn new(site: &str, draft: Exposure, host: Option<&str>, domains: &[(String, String)]) -> Self {
        let under = |host: &str, domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
        let domain = host
            .and_then(|host| domains.iter().filter(|(d, _)| under(host, d)).max_by_key(|(d, _)| d.len()))
            .or(domains.first())
            .map(|(d, _)| d.clone())
            .unwrap_or_default();
        let sub = host
            .filter(|host| under(host, &domain))
            .and_then(|host| host.strip_suffix(domain.as_str()))
            .map(|rest| rest.trim_end_matches('.').to_owned())
            .unwrap_or_else(|| site.split('-').next_back().unwrap_or(site).to_owned());
        Self {
            site: site.to_owned(),
            draft,
            selected: 0,
            sub: Input::plain(sub),
            domain,
            picking: false,
            pick: 0,
            details: false,
            has_provider: !domains.is_empty(),
            aliases: Vec::new(),
            alias_sub: Input::plain(String::new()),
            alias_domain: domains.first().map(|(d, _)| d.clone()).unwrap_or_default(),
            alias_mode: nodo_api::AliasMode::Redirect,
            picking_alias: false,
            alias_error: None,
            was_public: draft.public,
        }
    }

    /// The focus order, which depends on whether the internet is ticked and a provider exists.
    pub fn order(&self) -> Vec<HostingFocus> {
        let has_provider = self.has_provider;
        let mut order = vec![HostingFocus::Home, HostingFocus::Tor, HostingFocus::Internet];
        if self.draft.public {
            if has_provider {
                order.extend([HostingFocus::Domain, HostingFocus::Address]);
                order.extend((0..self.aliases.len()).map(HostingFocus::Alias));
                if !self.was_public {
                    order.extend([HostingFocus::AliasDomain, HostingFocus::AliasAddress, HostingFocus::AliasMode, HostingFocus::AliasAdd]);
                }
            } else {
                order.push(HostingFocus::AddProvider);
            }
        }
        order.push(HostingFocus::Details);
        if !self.draft.public || has_provider {
            order.push(HostingFocus::Save);
        }
        order
    }

    pub fn focus(&self) -> HostingFocus {
        let order = self.order();
        order[self.selected.min(order.len() - 1)]
    }

    /// The public host the choices make: the address part and the chosen domain.
    pub fn host(&self) -> String {
        let sub = self.sub.text().trim().trim_matches('.');
        if sub.is_empty() { self.domain.clone() } else { format!("{sub}.{}", self.domain) }
    }

    /// The new name the choices make, the same way: an empty part is the domain itself.
    pub fn alias_host(&self) -> String {
        let sub = self.alias_sub.text().trim().trim_matches('.');
        if sub.is_empty() { self.alias_domain.clone() } else { format!("{sub}.{}", self.alias_domain) }
    }

    /// The domain list is open, for either address.
    pub fn list_open(&self) -> bool {
        self.picking || self.picking_alias
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebCheckState {
    pub site: String,
    pub checks: Vec<Check>,
    pub router: Option<RouterHelp>,
    pub plan: Option<PublicPlan>,
    /// True when this check is a step towards going public, not just a test from the overview.
    pub going_public: bool,
    pub manual: Vec<String>,
    pub details: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmFocus {
    Cancel,
    GoPublic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebConfirmState {
    pub site: String,
    pub plan: PublicPlan,
    pub access: PublicAccess,
    pub focus: ConfirmFocus,
    pub details: bool,
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
    pub details: bool,
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
    /// An index into [`copy_targets`].
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

/// Everything on a setup page that can be copied, in the order drawn: values, QR codes' data and
/// the DNS records' values, each with its label.
pub fn copy_targets(view: &nodo_api::OwnerPageView) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for step in &view.steps {
        for item in &step.items {
            match item {
                nodo_api::OwnerItem::Value { label, value } => out.push((label.clone(), value.clone())),
                nodo_api::OwnerItem::Qr { label, data } => out.push((label.clone(), data.clone())),
                nodo_api::OwnerItem::Site(site) => {
                    if let Some(host) = &site.host {
                        out.push(("Address".to_owned(), host.clone()));
                    }
                    for record in &site.records {
                        out.push((format!("{} record {}", record.kind, record.name), record.value.clone()));
                    }
                }
                _ => {}
            }
        }
    }
    out
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
