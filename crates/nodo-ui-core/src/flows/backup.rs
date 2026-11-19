//! Backups, on the device and on a USB drive, and the two ways back from one: restoring onto a
//! device that is set up ([`Backups`]) and rebuilding a new device from a USB drive on first boot
//! ([`Restore`]).
//!
//! Both are a few steps in a fixed order, and each step is one action that takes what the owner
//! typed. A restore asks for the backup's passphrase, shows what the verified backup holds and
//! which services stop, and then needs the typed word. The helper checks every rule again.
//!
//! An action that checks what was typed answers `Err` when it refuses: `error` then says why,
//! and the `Err` names the field the owner types again when there is one (it was emptied, or it
//! is missing or wrong), so a frontend can put its cursor there.

use nodo_api::{BackupContents, BackupInfo, RestoreReport, UsbBackupList, UsbDrive};

use crate::effect::UsbView;
use crate::flows::setup::MIN_PASSPHRASE_CHARS;
use crate::flows::{Dest, Hint, Notice, Step};
use crate::model::Snapshot;
use crate::secret::{Input, Secret};
use crate::{Effect, Outcome};

/// The word typed before a restore. The helper also requires it, so the check here only saves a
/// round trip.
pub const RESTORE_WORD: &str = nodo_api::RESTORE_WORD;

/// The helper enforces the same minimum. Checking here saves a round trip and a KDF run.
pub const MIN_BACKUP_PASSPHRASE_CHARS: usize = 12;

/// What a backup holds, a line each.
pub const HOLDS: [&str; 4] = [
    "Package settings, secrets, certificates and Tor onion keys",
    "The device key, so encrypted storage can be restored",
    "The contents of unlocked encrypted storage",
    "Feeds, pinned keys, approvals and the verified package files",
];

/// What a backup leaves out, and how it is protected.
pub const HOLDS_NOTES: [&str; 2] =
    ["Never chain data. The node syncs it again.", "Encrypted with its own passphrase, at the same cost as the device key."];

/// What the owner should know about a backup's passphrase before choosing one.
pub const PASSPHRASE_NOTES: [&str; 3] = [
    "The backup passphrase opens this backup on any device. Keep it apart from the device.",
    "Use at least 12 characters. It is as valuable as the device passphrase.",
    "Nothing can open the backup without it.",
];

/// Said where a backup goes to a USB drive.
pub const USB_NOTE: &str = "The drive is unmounted when the backup is written. Wait for it before unplugging.";

/// Said where a backup's passphrase is asked for, before anything is restored.
pub const CHECKED_FIRST: &str = "The whole backup is checked first. A wrong passphrase or a damaged file restores nothing.";

/// What restoring a verified backup brings back or replaces, a line each. `fresh` on a device
/// that is being set up from it.
pub fn restores(contents: &BackupContents, fresh: bool) -> Vec<String> {
    let mut what: Vec<String> = contents.settings.iter().map(|id| format!("{id} settings")).collect();
    what.extend(contents.storage.iter().map(|id| format!("{id} encrypted storage")));
    if contents.onion_keys {
        what.push("Tor onion keys".to_owned());
    }
    if fresh {
        what.push("Feeds, their pinned keys, approvals and the packages installed from them".to_owned());
        if contents.has_device_key {
            what.push("Device key, so the backed-up storage opens".to_owned());
        }
    } else if contents.has_device_key {
        what.push("Device key, kept if this device's storage already uses its own".to_owned());
    }
    what
}

/// What a restore does to the running services, and the warning it is confirmed under.
pub fn restore_warning(contents: &BackupContents, fresh: bool) -> (String, &'static str) {
    if fresh {
        ("Each package is installed, its storage filled, then started.".to_owned(), "Set up this device from the backup")
    } else if contents.services_to_stop.is_empty() {
        ("Nothing needs to stop.".to_owned(), "This overwrites what is on the device now")
    } else {
        (format!("Stops while restoring: {}", contents.services_to_stop.join(", ")), "This overwrites what is on the device now")
    }
}

/// A USB drive's filesystem, its free space and whether it can be written, in a line.
pub fn usb_facts(drive: &UsbDrive, list: &UsbBackupList) -> String {
    let filesystem = match drive.filesystem.as_deref() {
        Some("vfat") => "FAT32",
        Some("exfat") => "exFAT",
        Some("ext4") => "ext4",
        Some(other) => other,
        None => "no filesystem",
    };
    let mut facts = format!("{filesystem} · {} free", crate::words::size(list.free_bytes));
    if list.read_only {
        facts.push_str(" · read-only");
    }
    facts
}

/// A box of one of the forms here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The device passphrase, which lets a new backup include the device key.
    DevicePassphrase,
    /// The new backup's own passphrase. Its second box is emptied with it.
    BackupPassphrase,
    /// The typed word, [`RESTORE_WORD`].
    Word,
    /// The device passphrase chosen for after the restore. On first boot its second box is
    /// emptied with it.
    NewDevicePassphrase,
}

/// What a refused form answers: the field to type again, when there is one.
pub type Refused = Option<Field>;

/// Where the Backups page is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    List,
    /// The device passphrase, when the device has one, then the backup passphrase twice.
    Create,
    /// The backup's passphrase. The helper verifies the whole file before showing what it holds.
    Unlock { name: String },
    /// What a restore replaces, the typed word, and a new device passphrase when one is needed
    /// ([`Backups::asks_new_device_passphrase`]). `passphrase` is the backup's, kept to restore.
    Confirm { name: String, passphrase: Secret, contents: BackupContents },
    Restored(RestoreReport),
}

/// The Backups page: the device's backups and a USB drive's, making one, and restoring one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backups {
    pub list: Vec<BackupInfo>,
    /// The USB drive, `None` while none is plugged in.
    pub usb: Option<UsbView>,
    pub stage: Stage,
    pub error: Option<String>,
    pub busy: bool,
    /// The device key exists, so a backup asks for the device passphrase to include it.
    pub device_set_up: bool,
    /// The removable drive the step under way uses, `None` for the device's own backups.
    pub target: Option<String>,
    /// The device is not set up: only a USB drive's backups are offered, none can be made, and a
    /// restore sets the whole device up.
    pub fresh: bool,
    /// The passphrase the backup being opened was asked with, until the helper answers.
    opening: Option<Secret>,
}

impl Backups {
    pub fn new(device_set_up: bool) -> Self {
        Self {
            list: Vec::new(),
            usb: None,
            stage: Stage::List,
            error: None,
            busy: true,
            device_set_up,
            target: None,
            fresh: false,
            opening: None,
        }
    }

    /// The page on the device as `snapshot` shows it.
    pub fn of(snapshot: &Snapshot) -> Self {
        Self::new(!snapshot.overview.setup_needed)
    }

    /// The page on a device that is not set up.
    pub fn fresh() -> Self {
        Self { fresh: true, ..Self::new(false) }
    }

    /// What a new page asks the helper for.
    pub fn load() -> Vec<Effect> {
        vec![Effect::LoadBackups]
    }

    /// Reads both lists again.
    pub fn reload(&mut self) -> Vec<Effect> {
        if self.busy {
            return Vec::new();
        }
        self.busy = true;
        Self::load()
    }

    /// Backups on the USB drive, when one can be used.
    pub fn usb_list(&self) -> &[BackupInfo] {
        self.usb.as_ref().and_then(|u| u.list.as_ref()).map(|l| l.backups.as_slice()).unwrap_or(&[])
    }

    /// The USB drive that can be written to and read from, by its device name.
    pub fn usb_device(&self) -> Option<&str> {
        self.usb.as_ref().filter(|u| u.list.is_some()).map(|u| u.drive.device.as_str())
    }

    /// A new backup asks for the device passphrase too.
    pub fn asks_device_passphrase(&self) -> bool {
        self.device_set_up
    }

    /// The restore being confirmed asks for a new device passphrase: the backup holds a device
    /// key, or the device is not set up and needs a passphrase of its own.
    pub fn asks_new_device_passphrase(&self) -> bool {
        match &self.stage {
            Stage::Confirm { contents, .. } => contents.has_device_key || self.fresh,
            _ => false,
        }
    }

    /// Starts a new backup, on the USB drive when `on_usb`. A device that is not set up has
    /// nothing to back up.
    pub fn start_create(&mut self, on_usb: bool) {
        if self.busy || self.fresh || self.stage != Stage::List {
            return;
        }
        let usb = self.usb_device().map(str::to_owned);
        if on_usb && usb.is_none() {
            self.error = Some("Plug in a USB drive formatted as FAT32, exFAT or ext4 first.".to_owned());
            return;
        }
        self.target = if on_usb { usb } else { None };
        self.error = None;
        self.stage = Stage::Create;
    }

    /// Makes the backup with the passphrases typed. The backup's passphrase is typed twice, and
    /// two that differ are both emptied. `device` is read only when
    /// [`Backups::asks_device_passphrase`].
    pub fn create(&mut self, device: &mut Input, first: &mut Input, second: &mut Input) -> Result<Vec<Effect>, Refused> {
        if self.busy || self.stage != Stage::Create {
            return Ok(Vec::new());
        }
        let backup = first.secret_value();
        if backup.expose().chars().count() < MIN_BACKUP_PASSPHRASE_CHARS {
            self.error = Some(format!("A backup passphrase needs at least {MIN_BACKUP_PASSPHRASE_CHARS} characters."));
            return Err(None);
        }
        if backup != second.secret_value() {
            self.error = Some("The two backup passphrases differ. Type them again.".to_owned());
            first.clear();
            second.clear();
            return Err(Some(Field::BackupPassphrase));
        }
        if self.device_set_up && device.is_empty() {
            self.error = Some("Enter the device passphrase, so the backup includes the device key.".to_owned());
            return Err(Some(Field::DevicePassphrase));
        }
        self.error = None;
        self.busy = true;
        let device_passphrase = self.device_set_up.then(|| device.take_secret());
        second.clear();
        Ok(vec![Effect::CreateBackup { device_passphrase, backup_passphrase: first.take_secret(), usb: self.target.clone() }])
    }

    /// Chooses a backup to restore, the USB drive's when `on_usb`. Its passphrase is asked next.
    /// A name that is not on that list is not chosen.
    pub fn open(&mut self, name: &str, on_usb: bool) {
        if self.busy || self.stage != Stage::List {
            return;
        }
        let listed = if on_usb { self.usb_list() } else { &self.list };
        if !listed.iter().any(|backup| backup.name == name) {
            return;
        }
        self.target = if on_usb { self.usb_device().map(str::to_owned) } else { None };
        self.error = None;
        self.stage = Stage::Unlock { name: name.to_owned() };
    }

    /// Opens the chosen backup with its passphrase, to see what it holds. Nothing is asked while
    /// the box is empty. The box keeps what was typed until the helper has answered.
    pub fn inspect(&mut self, passphrase: &Input) -> Vec<Effect> {
        let Stage::Unlock { name } = &self.stage else {
            return Vec::new();
        };
        if self.busy || passphrase.is_empty() {
            return Vec::new();
        }
        self.busy = true;
        self.error = None;
        let passphrase = passphrase.secret_value();
        self.opening = Some(passphrase.clone());
        vec![Effect::InspectBackup { name: name.clone(), passphrase, usb: self.target.clone() }]
    }

    /// Restores the backup being confirmed. `typed` must be [`RESTORE_WORD`]. `device` is read
    /// only when [`Backups::asks_new_device_passphrase`].
    pub fn restore(&mut self, typed: &str, device: &mut Input) -> Result<Vec<Effect>, Refused> {
        let asks_device = self.asks_new_device_passphrase();
        let Stage::Confirm { name, passphrase, .. } = &self.stage else {
            return Ok(Vec::new());
        };
        if self.busy {
            return Ok(Vec::new());
        }
        if typed != RESTORE_WORD {
            self.error = Some(format!("Type {RESTORE_WORD} to confirm."));
            return Err(Some(Field::Word));
        }
        if asks_device && device.secret_value().expose().chars().count() < MIN_PASSPHRASE_CHARS {
            self.error = Some(format!(
                "Choose a new device passphrase of at least {MIN_PASSPHRASE_CHARS} characters for the backup's key."
            ));
            return Err(Some(Field::NewDevicePassphrase));
        }
        self.error = None;
        self.busy = true;
        let new_device_passphrase = asks_device.then(|| device.take_secret());
        Ok(vec![Effect::RestoreBackup {
            name: name.clone(),
            passphrase: passphrase.clone(),
            new_device_passphrase,
            typed: typed.to_owned(),
            usb: self.target.clone(),
        }])
    }

    /// Gives up the step under way and returns to the lists. Nothing was changed.
    pub fn cancel(&mut self) {
        if self.busy {
            return;
        }
        self.error = None;
        self.opening = None;
        self.stage = Stage::List;
    }

    /// Leaves the page. The device is read again, since a restore changes what is installed.
    pub fn leave(&self) -> Step {
        Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])])
    }

    /// The owner has read what was restored. On a device that was not set up, that ends setup.
    pub fn finish(&mut self) -> Step {
        if self.busy || !matches!(self.stage, Stage::Restored(_)) {
            return Step::Stay;
        }
        if self.fresh {
            return set_up_from_backup();
        }
        self.error = None;
        self.stage = Stage::List;
        self.busy = true;
        Step::Run(Self::load())
    }

    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        match outcome {
            Outcome::BackupsLoaded(Ok(list)) => self.list = list.clone(),
            // A device that is not set up has no backups of its own, which is no error.
            Outcome::BackupsLoaded(Err(e)) => {
                if !self.fresh {
                    self.error = Some(e.clone());
                }
            }
            Outcome::UsbLoaded(result) => {
                self.busy = false;
                self.usb = result.clone().ok();
            }
            Outcome::BackupCreated(Ok(created)) => {
                self.stage = Stage::List;
                self.busy = true;
                let place = if self.target.take().is_some() { " to the USB drive" } else { "" };
                let mut text = format!("Backup {} written{place}.", created.info.name);
                if !created.locked.is_empty() {
                    text.push_str(&format!(" Locked storage was left out: {}.", created.locked.join(", ")));
                }
                return Step::All(vec![Step::Done(Notice::ok(text)), Step::Run(Self::load())]);
            }
            Outcome::BackupInspected(Ok(contents)) => {
                self.busy = false;
                if let Stage::Unlock { name } = &self.stage {
                    self.stage = Stage::Confirm {
                        name: name.clone(),
                        passphrase: self.opening.take().unwrap_or_default(),
                        contents: contents.clone(),
                    };
                }
            }
            Outcome::BackupRestored(Ok(report)) => {
                self.busy = false;
                self.stage = Stage::Restored(report.clone());
            }
            Outcome::BackupCreated(Err(e)) | Outcome::BackupInspected(Err(e)) | Outcome::BackupRestored(Err(e)) => {
                self.busy = false;
                self.opening = None;
                self.error = Some(e.clone());
            }
            _ => {}
        }
        Step::Stay
    }
}

/// The device was set up from a backup: SSH keys are offered next, as after a first-boot setup.
fn set_up_from_backup() -> Step {
    Step::All(vec![Step::Go(Dest::SetUpFromBackup), Step::Run(vec![Effect::LoadSshKeys, Effect::Refresh])])
}

/// Where the first-boot restore is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreStage {
    /// Every removable drive, with why one cannot be used.
    Drives,
    /// The backups on the chosen drive.
    Backups { device: String },
    /// The backup's passphrase. The helper verifies the whole file before showing what it holds.
    Passphrase { device: String, name: String },
    /// What comes back, the typed word and a new device passphrase, twice. `passphrase` is the
    /// backup's, kept to restore.
    Confirm { device: String, name: String, passphrase: Secret, contents: BackupContents },
    /// The helper is restoring. A frontend shows how long it has been.
    Restoring { device: String, name: String },
    /// Nothing was set up. The same backup can be tried again.
    Failed { device: String, name: String, reason: String },
    Done(RestoreReport),
}

/// Rebuilds a device that is not set up from a USB backup: choose the drive, then the backup,
/// open it with its passphrase, see what comes back, type the word and a new device passphrase
/// twice. Nothing changes until then, and a failure leaves the device not set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restore {
    pub drives: Vec<UsbDrive>,
    pub backups: Option<UsbBackupList>,
    pub stage: RestoreStage,
    pub error: Option<String>,
    pub busy: bool,
    /// The passphrase the backup being opened was asked with, until the helper answers.
    opening: Option<Secret>,
    /// The error that points at something to do, and what. See [`Restore::error_hint`].
    hint: Option<(String, Hint)>,
}

impl Default for Restore {
    fn default() -> Self {
        Self::new()
    }
}

impl Restore {
    pub fn new() -> Self {
        Self { drives: Vec::new(), backups: None, stage: RestoreStage::Drives, error: None, busy: true, opening: None, hint: None }
    }

    /// What the owner can do about `error`, when it points at something: look for drives again.
    pub fn error_hint(&self) -> Option<Hint> {
        let (text, hint) = self.hint.as_ref()?;
        (self.error.as_ref() == Some(text)).then_some(*hint)
    }

    /// What a new page asks the helper for.
    pub fn load() -> Vec<Effect> {
        vec![Effect::LoadRestoreDrives]
    }

    pub fn drive(&self, device: &str) -> Option<&UsbDrive> {
        self.drives.iter().find(|d| d.device == device)
    }

    /// Looks again: for drives, or for the backups on the chosen drive.
    pub fn reload(&mut self) -> Vec<Effect> {
        if self.busy {
            return Vec::new();
        }
        let effect = match &self.stage {
            RestoreStage::Drives => Effect::LoadRestoreDrives,
            RestoreStage::Backups { device } => Effect::LoadRestoreBackups(device.clone()),
            _ => return Vec::new(),
        };
        self.busy = true;
        vec![effect]
    }

    /// Chooses the drive to restore from, by its device name. One with a problem is refused with
    /// that problem.
    pub fn choose_drive(&mut self, device: &str) -> Vec<Effect> {
        if self.busy || self.stage != RestoreStage::Drives {
            return Vec::new();
        }
        let Some(drive) = self.drives.iter().find(|d| d.device == device) else {
            let text = "No USB drive is plugged in. Plug one in and look again.";
            self.error = Some(text.to_owned());
            self.hint = Some((text.to_owned(), Hint::LookForDrives));
            return Vec::new();
        };
        if let Some(problem) = &drive.problem {
            self.error = Some(format!("{} cannot be used: {problem}", drive.device));
            return Vec::new();
        }
        let device = drive.device.clone();
        self.busy = true;
        self.backups = None;
        self.error = None;
        self.stage = RestoreStage::Backups { device: device.clone() };
        vec![Effect::LoadRestoreBackups(device)]
    }

    /// Chooses the backup to restore, by name. Its passphrase is asked next.
    pub fn choose_backup(&mut self, name: &str) {
        let RestoreStage::Backups { device } = &self.stage else {
            return;
        };
        if self.busy {
            return;
        }
        if !self.backups.as_ref().is_some_and(|list| list.backups.iter().any(|backup| backup.name == name)) {
            self.error = Some("This drive holds no monokulo-nodo backups.".to_owned());
            return;
        }
        self.error = None;
        self.stage = RestoreStage::Passphrase { device: device.clone(), name: name.to_owned() };
    }

    /// Opens the chosen backup with its passphrase, which is taken out of the box.
    pub fn inspect(&mut self, passphrase: &mut Input) -> Vec<Effect> {
        let RestoreStage::Passphrase { device, name } = &self.stage else {
            return Vec::new();
        };
        if self.busy {
            return Vec::new();
        }
        if passphrase.is_empty() {
            self.error = Some("Type the backup passphrase.".to_owned());
            return Vec::new();
        }
        self.busy = true;
        self.error = None;
        let passphrase = passphrase.take_secret();
        self.opening = Some(passphrase.clone());
        vec![Effect::InspectRestore { device: device.clone(), name: name.clone(), passphrase }]
    }

    /// Restores the backup being confirmed and sets the device up with the new passphrase.
    /// `typed` must be [`RESTORE_WORD`]; spaces around it are ignored. Two new passphrases that
    /// differ are both emptied.
    pub fn restore(&mut self, typed: &str, first: &mut Input, second: &mut Input) -> Result<Vec<Effect>, Refused> {
        let RestoreStage::Confirm { device, name, passphrase, .. } = &self.stage else {
            return Ok(Vec::new());
        };
        if self.busy {
            return Ok(Vec::new());
        }
        if typed.trim() != RESTORE_WORD {
            self.error = Some(format!("Type {RESTORE_WORD} to confirm."));
            return Err(Some(Field::Word));
        }
        if first.secret_value().expose().chars().count() < MIN_PASSPHRASE_CHARS {
            self.error = Some(format!("Choose a new device passphrase of at least {MIN_PASSPHRASE_CHARS} characters."));
            return Err(Some(Field::NewDevicePassphrase));
        }
        if first.secret_value() != second.secret_value() {
            self.error = Some("The two device passphrases differ. Type them again.".to_owned());
            first.clear();
            second.clear();
            return Err(Some(Field::NewDevicePassphrase));
        }
        let (device, name, passphrase) = (device.clone(), name.clone(), passphrase.clone());
        self.busy = true;
        self.error = None;
        second.clear();
        self.stage = RestoreStage::Restoring { device: device.clone(), name: name.clone() };
        Ok(vec![Effect::RunRestore {
            device,
            name,
            passphrase,
            new_device_passphrase: first.take_secret(),
            typed: RESTORE_WORD.to_owned(),
        }])
    }

    /// One step back: from the backups to the drives, from a passphrase or the confirmation to
    /// the backups, from a failure to the drives, which are read again. From the drives it leaves
    /// the page, which is allowed even while they are being read: a late answer then finds
    /// another page and is dropped.
    pub fn back(&mut self) -> Step {
        if self.stage == RestoreStage::Drives {
            return Step::Go(Dest::Back);
        }
        if self.busy {
            return Step::Stay;
        }
        let mut effects = Vec::new();
        let next = match &self.stage {
            RestoreStage::Backups { .. } => RestoreStage::Drives,
            RestoreStage::Passphrase { device, .. } | RestoreStage::Confirm { device, .. } => {
                RestoreStage::Backups { device: device.clone() }
            }
            RestoreStage::Failed { .. } => {
                self.busy = true;
                effects = Self::load();
                RestoreStage::Drives
            }
            RestoreStage::Done(_) => return self.finish(),
            RestoreStage::Drives | RestoreStage::Restoring { .. } => return Step::Stay,
        };
        self.error = None;
        self.opening = None;
        self.stage = next;
        Step::run(effects)
    }

    /// After a failure: asks for the same backup's passphrase again.
    pub fn retry(&mut self) {
        if let RestoreStage::Failed { device, name, .. } = &self.stage {
            self.stage = RestoreStage::Passphrase { device: device.clone(), name: name.clone() };
            self.error = None;
        }
    }

    /// The owner has read what was restored: setup carries on with the SSH keys.
    pub fn finish(&self) -> Step {
        if self.busy || !matches!(self.stage, RestoreStage::Done(_)) {
            return Step::Stay;
        }
        set_up_from_backup()
    }

    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        match outcome {
            Outcome::RestoreDrives(result) => {
                self.busy = false;
                match result {
                    Ok(drives) => {
                        self.drives = drives.clone();
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e.clone()),
                }
            }
            Outcome::RestoreBackups(result) => {
                self.busy = false;
                match result {
                    Ok(list) => self.backups = Some(list.clone()),
                    Err(e) => {
                        self.stage = RestoreStage::Drives;
                        self.error = Some(e.clone());
                    }
                }
            }
            Outcome::RestoreInspected(result) => {
                self.busy = false;
                let opened = self.opening.take();
                if let RestoreStage::Passphrase { device, name } = &self.stage {
                    match result {
                        Ok(contents) => {
                            self.stage = RestoreStage::Confirm {
                                device: device.clone(),
                                name: name.clone(),
                                passphrase: opened.unwrap_or_default(),
                                contents: contents.clone(),
                            };
                        }
                        Err(e) => self.error = Some(e.clone()),
                    }
                }
            }
            Outcome::RestoreFinished(result) => {
                self.busy = false;
                let (device, name) = match &self.stage {
                    RestoreStage::Restoring { device, name } => (device.clone(), name.clone()),
                    _ => (String::new(), String::new()),
                };
                self.stage = match result {
                    Ok(report) => RestoreStage::Done(report.clone()),
                    Err(reason) => RestoreStage::Failed { device, name, reason: reason.clone() },
                };
            }
            _ => {}
        }
        Step::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_backup_holds_and_what_a_restore_replaces_are_said_once() {
        assert!(PASSPHRASE_NOTES[1].contains(&format!("at least {MIN_BACKUP_PASSPHRASE_CHARS} characters")));
        let contents = BackupContents {
            name: "nodo-1.nodobak".into(),
            entries: 3,
            settings: vec!["cuprated".into()],
            storage: vec!["wallet-relay".into()],
            onion_keys: true,
            has_device_key: true,
            services_to_stop: vec!["cuprated".into()],
        };
        let nothing_running = BackupContents { services_to_stop: Vec::new(), ..contents.clone() };
        assert_eq!(
            restores(&contents, false),
            ["cuprated settings", "wallet-relay encrypted storage", "Tor onion keys", "Device key, kept if this device's storage already uses its own"]
        );
        assert!(restores(&contents, true).contains(&"Device key, so the backed-up storage opens".to_owned()));
        assert_eq!(restore_warning(&contents, false), ("Stops while restoring: cuprated".to_owned(), "This overwrites what is on the device now"));
        assert_eq!(restore_warning(&nothing_running, false).0, "Nothing needs to stop.");
        assert_eq!(restore_warning(&contents, true).1, "Set up this device from the backup");
    }
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::{MockBackend, MOCK_PASSPHRASE};

    const DEVICE_BACKUP: &str = "nodo-1762560000.nodobak";
    const USB_BACKUP: &str = "nodo-1762473600.nodobak";

    fn secret(text: &str) -> Input {
        let mut input = Input::secret();
        text.chars().for_each(|c| input.push(c));
        input
    }

    /// Performs the effects and hands every outcome to the page, returning the steps.
    fn run(mock: &mut MockBackend, page: &mut Backups, effects: Vec<Effect>) -> Vec<Step> {
        let outcomes: Vec<Outcome> = effects.into_iter().flat_map(|effect| execute_effect(mock, effect)).collect();
        outcomes.iter().map(|outcome| page.apply(outcome)).collect()
    }

    fn loaded(mock: &mut MockBackend) -> Backups {
        let snapshot = load_snapshot(mock).expect("the mock answers");
        let mut page = Backups::of(&snapshot);
        assert!(page.busy && page.device_set_up);
        run(mock, &mut page, Backups::load());
        assert!(!page.busy);
        page
    }

    fn run_restore(mock: &mut MockBackend, page: &mut Restore, effects: Vec<Effect>) {
        for outcome in effects.into_iter().flat_map(|effect| execute_effect(mock, effect)) {
            assert_eq!(page.apply(&outcome), Step::Stay);
        }
    }

    #[test]
    fn the_page_lists_the_devices_backups_and_the_usb_drives() {
        let mut mock = MockBackend::new();
        let page = loaded(&mut mock);
        assert_eq!(page.list.len(), 2);
        assert_eq!(page.usb_device(), Some("sda1"));
        assert_eq!(page.usb_list().iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), [USB_BACKUP]);
    }

    #[test]
    fn a_backup_on_a_set_up_device_needs_the_device_passphrase_and_its_own_twice() {
        let mut mock = MockBackend::new();
        let mut page = loaded(&mut mock);
        page.start_create(false);
        assert_eq!(page.stage, Stage::Create);
        assert!(page.asks_device_passphrase());

        let (mut device, mut first, mut second) = (Input::secret(), secret("short"), secret("short"));
        assert_eq!(page.create(&mut device, &mut first, &mut second), Err(None));
        assert_eq!(page.error.as_deref(), Some("A backup passphrase needs at least 12 characters."));
        assert!(!first.is_empty(), "a short one is kept to be made longer");

        let (mut first, mut second) = (secret("backup passphrase one"), secret("backup passphrase two"));
        assert_eq!(page.create(&mut device, &mut first, &mut second), Err(Some(Field::BackupPassphrase)));
        assert!(page.error.as_deref().unwrap().contains("differ"));
        assert!(first.is_empty() && second.is_empty(), "two that differ are typed again");

        let (mut first, mut second) = (secret("backup passphrase one"), secret("backup passphrase one"));
        assert_eq!(page.create(&mut device, &mut first, &mut second), Err(Some(Field::DevicePassphrase)));
        assert!(!first.is_empty(), "nothing is taken while the form is refused");

        let mut device = secret("device pass");
        let effects = page.create(&mut device, &mut first, &mut second).expect("complete");
        let [Effect::CreateBackup { device_passphrase: Some(sent), backup_passphrase, usb: None }] = effects.as_slice() else {
            panic!("{effects:?}")
        };
        assert_eq!((sent.expose(), backup_passphrase.expose()), ("device pass", "backup passphrase one"));
        assert!(device.is_empty() && first.is_empty() && second.is_empty(), "the boxes are emptied");
        assert!(page.busy);

        let steps = run(&mut mock, &mut page, effects);
        let [Step::All(parts)] = steps.as_slice() else { panic!("{steps:?}") };
        assert_eq!(parts[0], Step::Done(Notice::ok("Backup nodo-1762646400.nodobak written.")));
        assert_eq!(parts[1], Step::Run(vec![Effect::LoadBackups]));
        assert_eq!(page.stage, Stage::List);
    }

    #[test]
    fn a_device_that_is_not_set_up_backs_up_without_a_device_passphrase() {
        let mut page = Backups::new(false);
        page.busy = false;
        page.start_create(false);
        assert!(!page.asks_device_passphrase());
        let (mut device, mut first, mut second) = (secret("ignored"), secret("backup passphrase one"), secret("backup passphrase one"));
        let effects = page.create(&mut device, &mut first, &mut second).expect("complete");
        assert!(matches!(effects.as_slice(), [Effect::CreateBackup { device_passphrase: None, usb: None, .. }]));
        assert!(!device.is_empty(), "a box that was not asked for is not read");
    }

    #[test]
    fn a_backup_to_usb_needs_a_drive_and_names_it() {
        let mut mock = MockBackend::new();
        let mut page = Backups::new(true);
        page.busy = false;
        page.start_create(true);
        assert_eq!(page.stage, Stage::List);
        assert!(page.error.as_deref().unwrap().starts_with("Plug in a USB drive"));

        let mut page = loaded(&mut mock);
        page.start_create(true);
        assert_eq!(page.target.as_deref(), Some("sda1"));
        let effects = page
            .create(&mut secret("device pass"), &mut secret("backup passphrase one"), &mut secret("backup passphrase one"))
            .expect("complete");
        assert!(matches!(effects.as_slice(), [Effect::CreateBackup { usb: Some(usb), .. }] if usb == "sda1"));
        let steps = run(&mut mock, &mut page, effects);
        let [Step::All(parts)] = steps.as_slice() else { panic!("{steps:?}") };
        assert_eq!(parts[0], Step::Done(Notice::ok("Backup nodo-1762646400.nodobak written to the USB drive.")));
        assert_eq!(page.target, None);
    }

    #[test]
    fn a_restore_opens_the_backup_shows_what_it_holds_and_needs_the_typed_word() {
        let mut mock = MockBackend::new();
        let mut page = loaded(&mut mock);
        page.open("not-on-the-list.nodobak", false);
        assert_eq!(page.stage, Stage::List, "only a listed backup is chosen");
        page.open(DEVICE_BACKUP, false);
        assert_eq!(page.stage, Stage::Unlock { name: DEVICE_BACKUP.to_owned() });
        assert!(page.inspect(&Input::secret()).is_empty(), "nothing is asked with an empty box");

        let wrong = secret("wrong");
        let effects = page.inspect(&wrong);
        run(&mut mock, &mut page, effects);
        assert!(page.error.as_deref().unwrap().contains("passphrase did not open"));
        assert!(matches!(page.stage, Stage::Unlock { .. }) && !page.busy);

        let effects = page.inspect(&secret(MOCK_PASSPHRASE));
        run(&mut mock, &mut page, effects);
        let Stage::Confirm { name, passphrase, contents } = &page.stage else { panic!("{:?}", page.stage) };
        assert_eq!((name.as_str(), passphrase.expose()), (DEVICE_BACKUP, MOCK_PASSPHRASE));
        assert_eq!(contents.services_to_stop, ["moneropay", "wallet-relay", "tor"]);
        assert!(page.asks_new_device_passphrase(), "the backup holds a device key");

        // The wrong word, then a passphrase too short, restore nothing.
        let mut device = secret("a new device passphrase");
        assert_eq!(page.restore("yes", &mut device), Err(Some(Field::Word)));
        assert_eq!(page.error.as_deref(), Some("Type restore to confirm."));
        assert_eq!(page.restore(RESTORE_WORD, &mut secret("short")), Err(Some(Field::NewDevicePassphrase)));
        assert!(!device.is_empty());

        let effects = page.restore(RESTORE_WORD, &mut device).expect("complete");
        let [Effect::RestoreBackup { typed, passphrase, new_device_passphrase: Some(new), usb: None, .. }] = effects.as_slice()
        else {
            panic!("{effects:?}")
        };
        assert_eq!((typed.as_str(), passphrase.expose(), new.expose()), ("restore", MOCK_PASSPHRASE, "a new device passphrase"));
        assert!(!format!("{effects:?}").contains(MOCK_PASSPHRASE), "secrets never print");
        run(&mut mock, &mut page, effects);
        assert!(matches!(page.stage, Stage::Restored(_)));

        // Reading the report returns to the lists, which are read again.
        assert_eq!(page.finish(), Step::Run(vec![Effect::LoadBackups]));
        assert_eq!(page.stage, Stage::List);
    }

    #[test]
    fn a_restore_from_usb_names_the_drive_and_cancelling_changes_nothing() {
        let mut mock = MockBackend::new();
        let mut page = loaded(&mut mock);
        page.open(DEVICE_BACKUP, true);
        assert_eq!(page.stage, Stage::List, "the device's backup is not on the drive");
        page.open(USB_BACKUP, true);
        let effects = page.inspect(&secret(MOCK_PASSPHRASE));
        assert!(matches!(effects.as_slice(), [Effect::InspectBackup { usb: Some(usb), .. }] if usb == "sda1"));
        run(&mut mock, &mut page, effects);
        page.cancel();
        assert_eq!(page.stage, Stage::List);
        assert_eq!(page.leave(), Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])]));
    }

    #[test]
    fn a_fresh_device_offers_no_backup_and_ends_set_up() {
        let mut mock = MockBackend::new();
        let mut page = Backups::fresh();
        // The device's own list failing is no error there.
        assert_eq!(page.apply(&Outcome::BackupsLoaded(Err("no backups directory".into()))), Step::Stay);
        assert_eq!(page.error, None);
        run(&mut mock, &mut page, Backups::load());
        page.start_create(false);
        assert_eq!(page.stage, Stage::List);
        page.open(USB_BACKUP, true);
        let effects = page.inspect(&secret(MOCK_PASSPHRASE));
        run(&mut mock, &mut page, effects);
        assert!(page.asks_new_device_passphrase());
        let effects = page.restore(RESTORE_WORD, &mut secret("a new device passphrase")).expect("complete");
        run(&mut mock, &mut page, effects);
        assert_eq!(
            page.finish(),
            Step::All(vec![Step::Go(Dest::SetUpFromBackup), Step::Run(vec![Effect::LoadSshKeys, Effect::Refresh])])
        );
    }

    #[test]
    fn nothing_is_asked_twice_while_the_helper_works() {
        let mut mock = MockBackend::new();
        let mut page = loaded(&mut mock);
        assert_eq!(page.reload(), [Effect::LoadBackups]);
        assert!(page.reload().is_empty());
        page.open(DEVICE_BACKUP, false);
        assert_eq!(page.stage, Stage::List);
        let before = page.clone();
        assert_eq!(page.apply(&Outcome::Failed("something else".into())), Step::Stay);
        assert_eq!(page, before);
    }

    fn first_boot(mock: &mut MockBackend) -> Restore {
        let mut page = Restore::new();
        assert!(page.busy);
        run_restore(mock, &mut page, Restore::load());
        assert!(!page.busy);
        page
    }

    #[test]
    fn the_first_boot_restore_goes_from_the_drive_to_the_set_up_device() {
        let mut mock = MockBackend::new();
        mock.setup_needed = true;
        let mut page = first_boot(&mut mock);
        assert_eq!(page.drives.len(), 1);
        assert!(page.drive("sda1").is_some());

        let effects = page.choose_drive("sda1");
        assert_eq!(effects, [Effect::LoadRestoreBackups("sda1".to_owned())]);
        run_restore(&mut mock, &mut page, effects);
        page.choose_backup("another.nodobak");
        assert_eq!(page.error.as_deref(), Some("This drive holds no monokulo-nodo backups."));
        page.choose_backup(USB_BACKUP);
        assert_eq!(page.stage, RestoreStage::Passphrase { device: "sda1".to_owned(), name: USB_BACKUP.to_owned() });

        assert!(page.inspect(&mut Input::secret()).is_empty());
        assert_eq!(page.error.as_deref(), Some("Type the backup passphrase."));
        let mut wrong = secret("wrong");
        let effects = page.inspect(&mut wrong);
        assert!(wrong.is_empty(), "a tried passphrase is not kept");
        run_restore(&mut mock, &mut page, effects);
        assert!(page.error.is_some() && matches!(page.stage, RestoreStage::Passphrase { .. }));

        let effects = page.inspect(&mut secret(MOCK_PASSPHRASE));
        run_restore(&mut mock, &mut page, effects);
        assert!(matches!(page.stage, RestoreStage::Confirm { .. }));

        let (mut first, mut second) = (secret("a new device passphrase"), secret("another device passphrase"));
        assert_eq!(page.restore("no", &mut first, &mut second), Err(Some(Field::Word)));
        assert_eq!(page.restore(" restore ", &mut secret("short"), &mut secret("short")), Err(Some(Field::NewDevicePassphrase)));
        assert_eq!(page.restore("restore", &mut first, &mut second), Err(Some(Field::NewDevicePassphrase)));
        assert!(first.is_empty() && second.is_empty(), "two that differ are typed again");
        assert!(mock.setup_needed, "nothing was changed");

        let (mut first, mut second) = (secret("a new device passphrase"), secret("a new device passphrase"));
        let effects = page.restore(" restore ", &mut first, &mut second).expect("complete");
        let [Effect::RunRestore { device, name, passphrase, new_device_passphrase, typed }] = effects.as_slice() else {
            panic!("{effects:?}")
        };
        assert_eq!((device.as_str(), name.as_str(), typed.as_str()), ("sda1", USB_BACKUP, "restore"));
        assert_eq!(passphrase.expose(), MOCK_PASSPHRASE, "the backup's passphrase is the one that opened it");
        assert_eq!(new_device_passphrase.expose(), "a new device passphrase");
        assert_eq!(page.stage, RestoreStage::Restoring { device: "sda1".to_owned(), name: USB_BACKUP.to_owned() });
        assert_eq!(page.back(), Step::Stay, "nothing is left while the helper restores");

        for outcome in effects.into_iter().flat_map(|effect| execute_effect(&mut mock, effect)) {
            page.apply(&outcome);
        }
        assert!(matches!(page.stage, RestoreStage::Done(_)));
        assert!(!mock.setup_needed);
        assert_eq!(
            page.finish(),
            Step::All(vec![Step::Go(Dest::SetUpFromBackup), Step::Run(vec![Effect::LoadSshKeys, Effect::Refresh])])
        );
    }

    #[test]
    fn a_failed_first_boot_restore_sets_nothing_up_and_can_be_tried_again() {
        let mut mock = MockBackend::new();
        mock.setup_needed = true;
        mock.knobs.restore_failure = Some("the drive was removed while reading".to_owned());
        let mut page = first_boot(&mut mock);
        let effects = page.choose_drive("sda1");
        run_restore(&mut mock, &mut page, effects);
        page.choose_backup(USB_BACKUP);
        let effects = page.inspect(&mut secret(MOCK_PASSPHRASE));
        run_restore(&mut mock, &mut page, effects);
        let effects =
            page.restore("restore", &mut secret("a new device passphrase"), &mut secret("a new device passphrase")).expect("complete");
        for outcome in effects.into_iter().flat_map(|effect| execute_effect(&mut mock, effect)) {
            page.apply(&outcome);
        }
        let RestoreStage::Failed { device, name, reason } = &page.stage else { panic!("{:?}", page.stage) };
        assert_eq!((device.as_str(), name.as_str()), ("sda1", USB_BACKUP));
        assert!(reason.contains("removed while reading"));
        assert!(mock.setup_needed);

        let mut again = page.clone();
        again.retry();
        assert_eq!(again.stage, RestoreStage::Passphrase { device: "sda1".to_owned(), name: USB_BACKUP.to_owned() });
        // Or back to the drives, which are read again.
        assert_eq!(page.back(), Step::Run(vec![Effect::LoadRestoreDrives]));
        assert!(page.stage == RestoreStage::Drives && page.busy);
    }

    #[test]
    fn a_drive_with_a_problem_is_refused_and_the_drive_list_can_always_be_left() {
        let mut mock = MockBackend::new();
        let mut page = first_boot(&mut mock);
        assert!(page.choose_drive("sdz9").is_empty());
        assert!(page.error.as_deref().unwrap().starts_with("No USB drive is plugged in."));
        page.drives[0].problem = Some("its filesystem is NTFS".to_owned());
        assert!(page.choose_drive("sda1").is_empty());
        assert_eq!(page.error.as_deref(), Some("sda1 cannot be used: its filesystem is NTFS"));

        assert_eq!(page.reload(), [Effect::LoadRestoreDrives]);
        assert!(page.busy);
        assert_eq!(page.back(), Step::Go(Dest::Back), "even while the drives are read again");

        // The backups of a drive that went away send back to the drives, with the reason.
        let mut page = first_boot(&mut mock);
        page.stage = RestoreStage::Backups { device: "sda1".to_owned() };
        page.apply(&Outcome::RestoreBackups(Err("sda1 is gone".to_owned())));
        assert_eq!((page.stage.clone(), page.error.as_deref()), (RestoreStage::Drives, Some("sda1 is gone")));
    }
}
