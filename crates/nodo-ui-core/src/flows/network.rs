//! Wi-Fi and Bluetooth: both radios, the networks in range and the saved ones, and devices.
//!
//! Both radios stay off until the owner turns them on here. Joining an open network, forgetting
//! one, removing a device, and turning Wi-Fi off while it may carry this very session all ask
//! first, starting on Cancel. The helper checks every rule again.
//!
//! The helper answers a change with the radio's new state, and a pairing or a join goes on in
//! the background after that. A frontend that shows the page keeps a clock: it tells the flow
//! how long the radios have been asked for ([`Network::no_answer_after`]) and, once a second,
//! asks it what to read again ([`Network::poll`]).

use nodo_api::{BluetoothStatus, PairingStage, WifiLink, WifiNetwork, WifiSecurity, WifiStatus};

use crate::flows::{Confirm, Dest, Notice, Question, Step};
use crate::secret::Input;
use crate::{Effect, Outcome};

/// How long the page waits for the radios before saying there is no answer yet.
pub const NO_ANSWER_SECONDS: u64 = 10;

/// How often a join that has no address yet is read again. A pairing is read every second.
pub const WIFI_POLL_SECONDS: u64 = 2;

const READING: &str = "Reading the radios…";
const MAY_CUT: &str = "The wired link is down, so this may cut the connection you are using.";

/// Where reading one radio's state from the helper stands, kept apart from the status itself so
/// a panel with nothing to show yet can tell asking from failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadioRead {
    Asking,
    /// Asked [`NO_ANSWER_SECONDS`] ago and still no answer. The helper does one thing at a time,
    /// so the answer may only be waiting behind something slower.
    NoAnswer,
    Failed(String),
    Read,
}

/// A row of the Wi-Fi list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiRow {
    /// An index into the last scan.
    InRange(usize),
    /// An index into the saved networks, for one the last scan did not find.
    Saved(usize),
}

/// The network whose passphrase is being asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub ssid: String,
    pub security: WifiSecurity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub wifi: Option<WifiStatus>,
    pub wifi_read: RadioRead,
    pub bluetooth_read: RadioRead,
    /// The last scan, strongest first.
    pub networks: Vec<WifiNetwork>,
    pub bluetooth: Option<BluetoothStatus>,
    /// Set while a network's passphrase is asked for; nothing else is done until it is given
    /// ([`Network::submit_passphrase`]) or given up ([`Network::cancel_prompt`]).
    pub prompt: Option<Prompt>,
    pub error: Option<String>,
    /// What the helper is doing, while it works.
    pub busy: Option<String>,
}

impl Default for Network {
    fn default() -> Self {
        Self::new()
    }
}

impl Network {
    /// The page before anything is read. `ask_again` reads the radios.
    pub fn new() -> Self {
        Self {
            wifi: None,
            wifi_read: RadioRead::Asking,
            bluetooth_read: RadioRead::Asking,
            networks: Vec::new(),
            bluetooth: None,
            prompt: None,
            error: None,
            busy: Some(READING.to_owned()),
        }
    }

    // Queries

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
    pub fn wifi_row(&self, row: WifiRow) -> Option<(String, WifiSecurity, bool)> {
        match row {
            WifiRow::InRange(i) => self.networks.get(i).map(|n| (n.ssid.clone(), n.security, n.saved || self.is_saved(&n.ssid))),
            WifiRow::Saved(i) => self.wifi.as_ref()?.saved.get(i).map(|n| (n.ssid.clone(), n.security, true)),
        }
    }

    /// The security and saved state of a listed network, by name. A scan lists a name once.
    fn listed(&self, ssid: &str) -> Option<(WifiSecurity, bool)> {
        self.wifi_rows().into_iter().filter_map(|row| self.wifi_row(row)).find(|(name, ..)| name == ssid).map(|(_, s, saved)| (s, saved))
    }

    pub fn is_saved(&self, ssid: &str) -> bool {
        self.wifi.as_ref().is_some_and(|w| w.saved.iter().any(|s| s.ssid == ssid))
    }

    /// The network the device is joined to, if any.
    pub fn joined(&self) -> Option<&str> {
        let wifi = self.wifi.as_ref()?;
        (wifi.link == WifiLink::Connected).then_some(())?;
        wifi.ssid.as_deref()
    }

    /// Whether a radio with nothing to show failed or went unanswered, so asking again helps.
    pub fn can_ask_again(&self) -> bool {
        let stuck = |read: &RadioRead| matches!(read, RadioRead::NoAnswer | RadioRead::Failed(_));
        (self.wifi.is_none() && stuck(&self.wifi_read)) || (self.bluetooth.is_none() && stuck(&self.bluetooth_read))
    }

    /// A pairing waits for the device or for the owner.
    pub fn pairing_running(&self) -> bool {
        self.bluetooth.as_ref().and_then(|b| b.pairing.as_ref()).is_some_and(|p| {
            matches!(p.stage, PairingStage::Pairing | PairingStage::TypePasskey(_) | PairingStage::ComparePasskey(_))
        })
    }

    /// A join is under way, or is done but has no address yet.
    pub fn waiting_for_address(&self) -> bool {
        self.wifi
            .as_ref()
            .is_some_and(|w| w.link == WifiLink::Connecting || (w.link == WifiLink::Connected && w.address.is_none()))
    }

    // Reading and polling

    /// Asks for both radios. It works even while an earlier answer is awaited, since that may
    /// never come. The frontend counts the wait from now.
    pub fn ask_again(&mut self) -> Vec<Effect> {
        self.error = None;
        self.busy = Some(READING.to_owned());
        self.wifi_read = RadioRead::Asking;
        self.bluetooth_read = RadioRead::Asking;
        vec![Effect::LoadNetwork]
    }

    /// The radios were asked `waited` whole seconds ago. One still unanswered after
    /// [`NO_ANSWER_SECONDS`] says so instead of asking forever.
    pub fn no_answer_after(&mut self, waited: u64) {
        if waited >= NO_ANSWER_SECONDS {
            for read in [&mut self.wifi_read, &mut self.bluetooth_read] {
                if *read == RadioRead::Asking {
                    *read = RadioRead::NoAnswer;
                }
            }
        }
    }

    /// What to read again while something goes on in the background: a pairing, every second,
    /// or a join without an address, every [`WIFI_POLL_SECONDS`]. A frontend calls it once a
    /// second with `clock`, a count of whole seconds from any start.
    pub fn poll(&self, clock: u64) -> Vec<Effect> {
        if self.busy.is_some() {
            return Vec::new();
        }
        if self.pairing_running() {
            return vec![Effect::PollBluetooth];
        }
        if self.waiting_for_address() && clock % WIFI_POLL_SECONDS == 0 {
            return vec![Effect::PollWifi];
        }
        Vec::new()
    }

    /// Whether an action waits: the helper works, or a passphrase is being asked for.
    fn held(&self) -> bool {
        self.busy.is_some() || self.prompt.is_some()
    }

    // Wi-Fi

    /// Turns Wi-Fi on or off. Turning it off while it is the only link asks first.
    pub fn toggle_wifi(&mut self) -> Step {
        if self.held() {
            return Step::Stay;
        }
        self.error = None;
        let Some(wifi) = &self.wifi else { return Step::Stay };
        if let Some(why) = &wifi.unavailable {
            self.error = Some(why.clone());
            return Step::Stay;
        }
        if wifi.enabled && wifi.link == WifiLink::Connected && !wifi.wired_up {
            return Step::Ask(Question::careful("Turn Wi-Fi off?", vec![MAY_CUT.to_owned()], "Turn off", Confirm::WifiOff));
        }
        let on = !wifi.enabled;
        self.busy = Some(if on { "Turning Wi-Fi on…" } else { "Turning Wi-Fi off…" }.to_owned());
        if !on {
            self.networks.clear();
        }
        Step::Run(vec![Effect::SetWifi(on)])
    }

    /// Looks for networks in range.
    pub fn scan_wifi(&mut self) -> Vec<Effect> {
        if self.held() {
            return Vec::new();
        }
        self.error = None;
        if !self.wifi.as_ref().is_some_and(|w| w.enabled) {
            self.error = Some("Turn it on first: press o.".to_owned());
            return Vec::new();
        }
        self.busy = Some("Looking for Wi-Fi networks…".to_owned());
        vec![Effect::ScanWifi]
    }

    /// Joins a listed network, by name. One with a passphrase asks for it (`prompt`), an open
    /// one asks whether to join at all, and the kinds the device cannot or will not join are
    /// refused with the reason.
    pub fn join(&mut self, ssid: &str) -> Step {
        if self.held() {
            return Step::Stay;
        }
        if !self.wifi.as_ref().is_some_and(|w| w.enabled) {
            self.error = Some("Wi-Fi is off. Press o to turn it on.".to_owned());
            return Step::Stay;
        }
        let Some((security, _)) = self.listed(ssid) else {
            self.error = Some("Press s to look for networks.".to_owned());
            return Step::Stay;
        };
        if self.joined() == Some(ssid) {
            self.error = Some(format!("The device is already joined to {ssid}."));
            return Step::Stay;
        }
        match security {
            WifiSecurity::Enterprise => {
                self.error = Some(format!("{ssid} needs a per-user login (WPA Enterprise), which this device does not support."));
            }
            WifiSecurity::Legacy => {
                self.error = Some(format!("{ssid} uses WEP or WPA, which are broken. Change the router to WPA2 or WPA3."));
            }
            WifiSecurity::Open => {
                return Step::Ask(Question::careful(
                    format!("Join {ssid}, an open network?"),
                    vec![
                        format!("{ssid} is not encrypted: anyone nearby can read what the device sends over it."),
                        "SSH and HTTPS stay encrypted, but names it looks up and other plain traffic do not.".to_owned(),
                    ],
                    "Join anyway",
                    Confirm::JoinOpenWifi(ssid.to_owned()),
                ));
            }
            _ => {
                self.error = None;
                self.prompt = Some(Prompt { ssid: ssid.to_owned(), security });
            }
        }
        Step::Stay
    }

    /// Joins the network being asked about with the passphrase typed, which is taken out of the
    /// box. An empty box is refused.
    pub fn submit_passphrase(&mut self, passphrase: &mut Input) -> Vec<Effect> {
        if self.busy.is_some() {
            return Vec::new();
        }
        let Some(prompt) = &self.prompt else {
            return Vec::new();
        };
        if passphrase.is_empty() {
            self.error = Some(format!("Type the passphrase of {}.", prompt.ssid));
            return Vec::new();
        }
        let Some(Prompt { ssid, security }) = self.prompt.take() else {
            return Vec::new();
        };
        self.error = None;
        self.busy = Some(format!("Joining {ssid}…"));
        vec![Effect::JoinWifi { ssid, security, passphrase: Some(passphrase.take_secret()), accept_open: false }]
    }

    /// Gives up the passphrase being asked for. False when none was.
    pub fn cancel_prompt(&mut self) -> bool {
        if self.prompt.take().is_none() {
            return false;
        }
        self.error = None;
        true
    }

    /// Asks before a saved network is forgotten, by name.
    pub fn ask_forget(&mut self, ssid: &str) -> Step {
        if self.held() {
            return Step::Stay;
        }
        let Some((_, saved)) = self.listed(ssid) else {
            return Step::Stay;
        };
        if !saved {
            self.error = Some(format!("{ssid} is not saved."));
            return Step::Stay;
        }
        let mut body = vec![format!("The device will not join {ssid} again, and its saved key is deleted.")];
        if self.joined() == Some(ssid) && !self.wifi.as_ref().is_some_and(|w| w.wired_up) {
            body.push(MAY_CUT.to_owned());
        }
        Step::Ask(Question::careful(format!("Forget {ssid}?"), body, "Forget", Confirm::ForgetWifi(ssid.to_owned())))
    }

    // Bluetooth

    pub fn toggle_bluetooth(&mut self) -> Vec<Effect> {
        if self.held() {
            return Vec::new();
        }
        self.error = None;
        let Some(bt) = &self.bluetooth else { return Vec::new() };
        if let Some(why) = &bt.unavailable {
            self.error = Some(why.clone());
            return Vec::new();
        }
        let on = !bt.enabled;
        self.busy = Some(if on { "Turning Bluetooth on…" } else { "Turning Bluetooth off…" }.to_owned());
        vec![Effect::SetBluetooth(on)]
    }

    /// Looks for devices in their pairing mode.
    pub fn scan_bluetooth(&mut self) -> Vec<Effect> {
        if self.held() {
            return Vec::new();
        }
        self.error = None;
        if !self.bluetooth.as_ref().is_some_and(|b| b.enabled) {
            self.error = Some("Turn it on first: press o.".to_owned());
            return Vec::new();
        }
        self.busy = Some("Looking for Bluetooth devices for 8 seconds…".to_owned());
        vec![Effect::ScanBluetooth]
    }

    /// What Enter does on a device, by address: pairs one that is not paired, and connects or
    /// disconnects one that is. The passkey of a pairing arrives in `bluetooth.pairing`.
    pub fn pair_or_connect(&mut self, address: &str) -> Vec<Effect> {
        if self.held() {
            return Vec::new();
        }
        let Some(bt) = &self.bluetooth else { return Vec::new() };
        if !bt.enabled {
            self.error = Some("Bluetooth is off. Press o to turn it on.".to_owned());
            return Vec::new();
        }
        let Some(device) = bt.devices.iter().find(|d| d.address == address).cloned() else {
            self.error = Some("Press s to look for devices. Put the device in its pairing mode first.".to_owned());
            return Vec::new();
        };
        self.error = None;
        if !device.paired {
            self.busy = Some(format!("Pairing with {}…", device.name));
            return vec![Effect::PairBluetooth(device.address)];
        }
        let connect = !device.connected;
        self.busy = Some(format!("{} {}…", if connect { "Connecting" } else { "Disconnecting" }, device.name));
        vec![Effect::ConnectBluetooth { address: device.address, connect }]
    }

    /// Asks before a device is unpaired and forgotten, by address.
    pub fn ask_remove_bluetooth(&self, address: &str) -> Step {
        if self.held() {
            return Step::Stay;
        }
        let Some(device) = self.bluetooth.as_ref().and_then(|b| b.devices.iter().find(|d| d.address == address)) else {
            return Step::Stay;
        };
        Step::Ask(Question::careful(
            format!("Remove {}?", device.name),
            vec![
                format!("{} is unpaired and forgotten.", device.name),
                "Pairing it again needs the device in its pairing mode.".to_owned(),
            ],
            "Remove",
            Confirm::RemoveBluetooth(device.address.clone()),
        ))
    }

    // Questions, leaving and results

    /// The owner confirmed one of this page's questions: the change is shown as running.
    pub fn confirmed(&mut self, confirm: &Confirm) {
        let busy = match confirm {
            Confirm::JoinOpenWifi(ssid) => format!("Joining {ssid}…"),
            Confirm::ForgetWifi(ssid) => format!("Forgetting {ssid}…"),
            Confirm::WifiOff => {
                self.networks.clear();
                "Turning Wi-Fi off…".to_owned()
            }
            Confirm::RemoveBluetooth(_) => "Removing the device…".to_owned(),
            _ => return,
        };
        self.error = None;
        self.busy = Some(busy);
    }

    /// Leaves the page. The device is read again, since its addresses may have changed.
    pub fn leave(&self) -> Step {
        Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])])
    }

    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        let mut message = Step::Stay;
        match outcome {
            Outcome::NetworkLoaded { wifi, bluetooth } => {
                self.busy = None;
                // A radio with nothing shown yet says why in its panel; one already shown keeps
                // its last status, with the error under it.
                match wifi {
                    Ok(status) => {
                        self.wifi = Some(status.clone());
                        self.wifi_read = RadioRead::Read;
                    }
                    Err(e) => {
                        if self.wifi.is_some() {
                            self.error = Some(e.clone());
                        }
                        self.wifi_read = RadioRead::Failed(e.clone());
                    }
                }
                match bluetooth {
                    Ok(status) => {
                        self.bluetooth = Some(status.clone());
                        self.bluetooth_read = RadioRead::Read;
                    }
                    Err(e) => {
                        if self.bluetooth.is_some() {
                            self.error = Some(e.clone());
                        }
                        self.bluetooth_read = RadioRead::Failed(e.clone());
                    }
                }
            }
            Outcome::WifiChanged { result, quiet } => {
                let quiet = *quiet;
                if !quiet {
                    self.busy = None;
                }
                match result {
                    Ok(status) => {
                        let joining =
                            !quiet && self.wifi.as_ref().map(|w| (w.link, w.ssid.clone())) != Some((status.link, status.ssid.clone()));
                        let gone: Vec<String> = self
                            .wifi
                            .iter()
                            .flat_map(|w| w.saved.iter())
                            .filter(|old| !status.saved.iter().any(|s| s.ssid == old.ssid))
                            .map(|old| old.ssid.clone())
                            .collect();
                        if !quiet && !gone.is_empty() {
                            message = Step::Done(Notice::ok(format!("Forgot {}.", gone.join(", "))));
                        }
                        if joining && status.link == WifiLink::Connected {
                            let ssid = status.ssid.clone().unwrap_or_default();
                            let text = match &status.address {
                                Some(address) => format!("Joined {ssid}. Its address is {address}."),
                                None => format!("Joined {ssid}. Waiting for an address."),
                            };
                            message = Step::Done(Notice::ok(text));
                        }
                        for network in &mut self.networks {
                            network.saved = status.saved.iter().any(|s| s.ssid == network.ssid);
                            network.connected =
                                status.link == WifiLink::Connected && status.ssid.as_deref() == Some(network.ssid.as_str());
                        }
                        self.wifi = Some(status.clone());
                    }
                    Err(e) if !quiet => self.error = Some(e.clone()),
                    Err(_) => {}
                }
            }
            Outcome::WifiScanned(result) => {
                self.busy = None;
                match result {
                    Ok(list) => {
                        if list.is_empty() {
                            self.error = Some("No Wi-Fi networks are in range.".to_owned());
                        }
                        self.networks = list.clone();
                    }
                    Err(e) => self.error = Some(e.clone()),
                }
            }
            Outcome::BluetoothChanged { result, quiet } => {
                if !quiet {
                    self.busy = None;
                }
                match result {
                    Ok(status) => {
                        let before = self.bluetooth.as_ref().and_then(|b| b.pairing.as_ref()).map(|p| &p.stage);
                        if let Some(pairing) = status.pairing.as_ref().filter(|p| before != Some(&p.stage)) {
                            let name = status
                                .devices
                                .iter()
                                .find(|d| d.address == pairing.address)
                                .map_or(pairing.address.clone(), |d| d.name.clone());
                            match &pairing.stage {
                                PairingStage::Done => {
                                    message = Step::Done(Notice::ok(format!("Paired with {name}. It is trusted and connected.")));
                                }
                                PairingStage::Failed(why) => {
                                    message = Step::Failed(Notice::error(format!("Pairing with {name} failed: {why}")));
                                }
                                _ => {}
                            }
                        }
                        self.bluetooth = Some(status.clone());
                    }
                    Err(e) if !quiet => self.error = Some(e.clone()),
                    Err(_) => {}
                }
            }
            _ => {}
        }
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::MockBackend;

    const KEYBOARD: &str = "AA:BB:CC:00:00:01";
    const HEADPHONES: &str = "AA:BB:CC:00:00:02";

    fn secret(text: &str) -> Input {
        let mut input = Input::secret();
        text.chars().for_each(|c| input.push(c));
        input
    }

    /// Performs the effects and hands every outcome to the page, returning the steps that say
    /// something.
    fn run(mock: &mut MockBackend, page: &mut Network, effects: Vec<Effect>) -> Vec<Step> {
        let outcomes: Vec<Outcome> = effects.into_iter().flat_map(|effect| execute_effect(mock, effect)).collect();
        outcomes.iter().map(|outcome| page.apply(outcome)).filter(|step| *step != Step::Stay).collect()
    }

    /// Follows a question as a frontend does once the owner confirms it.
    fn confirm(mock: &mut MockBackend, page: &mut Network, step: Step) -> Vec<Step> {
        let Step::Ask(question) = step else { panic!("{step:?}") };
        assert!(question.cancel_is_default, "the answer starts on Cancel");
        page.confirmed(&question.confirm);
        assert!(page.busy.is_some());
        let snapshot = load_snapshot(mock).expect("the mock answers");
        run(mock, page, question.confirm.step(&snapshot).effects())
    }

    fn open(mock: &mut MockBackend) -> Network {
        let mut page = Network::new();
        assert_eq!(page.busy.as_deref(), Some("Reading the radios…"));
        let effects = page.ask_again();
        assert_eq!(effects, [Effect::LoadNetwork]);
        run(mock, &mut page, effects);
        assert_eq!((page.wifi_read.clone(), page.bluetooth_read.clone(), page.busy.clone()), (RadioRead::Read, RadioRead::Read, None));
        page
    }

    fn scanned(mock: &mut MockBackend) -> Network {
        let mut page = open(mock);
        let effects = page.scan_wifi();
        assert_eq!(effects, [Effect::ScanWifi]);
        run(mock, &mut page, effects);
        page
    }

    fn names(page: &Network) -> Vec<(String, bool)> {
        page.wifi_rows().into_iter().filter_map(|row| page.wifi_row(row)).map(|(ssid, _, saved)| (ssid, saved)).collect()
    }

    #[test]
    fn the_rows_are_the_networks_in_range_then_the_saved_ones_out_of_range() {
        let mut mock = MockBackend::new();
        let page = open(&mut mock);
        // Before a scan only the saved networks are known.
        assert_eq!(page.wifi_rows(), [WifiRow::Saved(0), WifiRow::Saved(1)]);
        let page = scanned(&mut mock);
        assert_eq!(
            names(&page),
            [
                ("home-5G".to_owned(), true),
                ("Cafe Libre".to_owned(), false),
                ("neighbour".to_owned(), false),
                ("office-staff".to_owned(), false),
                ("travel-hotspot".to_owned(), true),
            ]
        );
        assert_eq!(page.wifi_rows().last(), Some(&WifiRow::Saved(1)));
        assert_eq!(page.joined(), Some("home-5G"));
    }

    #[test]
    fn joining_asks_for_the_passphrase_and_says_where_the_device_landed() {
        let mut mock = MockBackend::new();
        let mut page = scanned(&mut mock);
        assert_eq!(page.join("home-5G"), Step::Stay);
        assert_eq!(page.error.as_deref(), Some("The device is already joined to home-5G."));
        assert_eq!(page.join("no such network"), Step::Stay);
        assert_eq!(page.error.as_deref(), Some("Press s to look for networks."));

        assert_eq!(page.join("neighbour"), Step::Stay);
        assert_eq!(page.prompt, Some(Prompt { ssid: "neighbour".to_owned(), security: WifiSecurity::Wpa3Personal }));
        assert!(page.scan_wifi().is_empty(), "nothing else is done while the passphrase is asked for");
        assert!(page.submit_passphrase(&mut Input::secret()).is_empty());
        assert_eq!(page.error.as_deref(), Some("Type the passphrase of neighbour."));

        let mut wrong = secret("a wrong guess");
        let effects = page.submit_passphrase(&mut wrong);
        assert!(wrong.is_empty() && page.prompt.is_none());
        assert_eq!(page.busy.as_deref(), Some("Joining neighbour…"));
        assert!(run(&mut mock, &mut page, effects).is_empty());
        assert!(page.error.as_deref().unwrap().contains("refused the passphrase"));

        assert_eq!(page.join("neighbour"), Step::Stay);
        let effects = page.submit_passphrase(&mut secret("not ours at all"));
        let [Effect::JoinWifi { ssid, passphrase: Some(_), accept_open: false, .. }] = effects.as_slice() else { panic!("{effects:?}") };
        assert_eq!(ssid, "neighbour");
        let said = run(&mut mock, &mut page, effects);
        assert_eq!(said, [Step::Done(Notice::ok("Joined neighbour. Its address is 192.168.1.57."))]);
        assert_eq!(page.joined(), Some("neighbour"));
        assert!(names(&page).contains(&("neighbour".to_owned(), true)), "it is saved now");

        // Giving the prompt up changes nothing.
        assert_eq!(page.join("home-5G"), Step::Stay);
        assert!(page.cancel_prompt() && page.prompt.is_none() && !page.cancel_prompt());
    }

    #[test]
    fn the_kinds_the_device_does_not_join_are_refused_with_the_reason() {
        let mut mock = MockBackend::new();
        let mut page = scanned(&mut mock);
        assert_eq!(page.join("office-staff"), Step::Stay);
        assert!(page.error.as_deref().unwrap().contains("WPA Enterprise"));
        page.networks[2].security = WifiSecurity::Legacy;
        assert_eq!(page.join("neighbour"), Step::Stay);
        assert!(page.error.as_deref().unwrap().contains("WEP or WPA, which are broken"));
        assert!(page.prompt.is_none() && page.busy.is_none());

        page.wifi.as_mut().unwrap().enabled = false;
        assert_eq!(page.join("neighbour"), Step::Stay);
        assert_eq!(page.error.as_deref(), Some("Wi-Fi is off. Press o to turn it on."));
        assert!(page.scan_wifi().is_empty());
        assert_eq!(page.error.as_deref(), Some("Turn it on first: press o."));
    }

    #[test]
    fn an_open_network_is_joined_only_after_the_question() {
        let mut mock = MockBackend::new();
        let mut page = scanned(&mut mock);
        let step = page.join("Cafe Libre");
        let Step::Ask(question) = &step else { panic!("{step:?}") };
        assert_eq!(question.title, "Join Cafe Libre, an open network?");
        assert_eq!(question.confirm_label, "Join anyway");
        assert_eq!(question.confirm, Confirm::JoinOpenWifi("Cafe Libre".to_owned()));
        assert!(page.busy.is_none() && page.joined() == Some("home-5G"), "nothing was done yet");
        let said = confirm(&mut mock, &mut page, step);
        assert_eq!(said, [Step::Done(Notice::ok("Joined Cafe Libre. Its address is 192.168.1.57."))]);
    }

    #[test]
    fn turning_wifi_off_asks_first_only_when_it_is_the_only_link() {
        let mut mock = MockBackend::new();
        let mut page = scanned(&mut mock);
        // The wire is up: no question.
        let step = page.toggle_wifi();
        assert_eq!(step, Step::Run(vec![Effect::SetWifi(false)]));
        assert!(page.networks.is_empty(), "the scan is dropped with the radio");
        run(&mut mock, &mut page, step.effects());
        assert!(!page.wifi.as_ref().unwrap().enabled);
        let step = page.toggle_wifi();
        assert_eq!(step, Step::Run(vec![Effect::SetWifi(true)]));
        assert_eq!(page.busy.as_deref(), Some("Turning Wi-Fi on…"));

        let mut mock = MockBackend::new();
        mock.wifi.wired_up = false;
        let mut page = scanned(&mut mock);
        let step = page.toggle_wifi();
        let Step::Ask(question) = &step else { panic!("{step:?}") };
        assert_eq!((question.title.as_str(), question.confirm.clone()), ("Turn Wi-Fi off?", Confirm::WifiOff));
        assert!(question.body[0].starts_with("The wired link is down"));
        assert!(page.busy.is_none() && !page.networks.is_empty());
        confirm(&mut mock, &mut page, step);
        assert!(!mock.wifi.enabled && page.networks.is_empty());

        // A radio the device does not have says why.
        page.wifi.as_mut().unwrap().unavailable = Some("No Wi-Fi adapter was found.".to_owned());
        assert_eq!(page.toggle_wifi(), Step::Stay);
        assert_eq!(page.error.as_deref(), Some("No Wi-Fi adapter was found."));
    }

    #[test]
    fn forgetting_a_network_asks_and_warns_when_it_carries_the_connection() {
        let mut mock = MockBackend::new();
        mock.wifi.wired_up = false;
        let mut page = scanned(&mut mock);
        assert_eq!(page.ask_forget("Cafe Libre"), Step::Stay);
        assert_eq!(page.error.as_deref(), Some("Cafe Libre is not saved."));
        let Step::Ask(far) = page.ask_forget("travel-hotspot") else { panic!("saved") };
        assert_eq!(far.body.len(), 1, "forgetting a network out of range cuts nothing");

        let step = page.ask_forget("home-5G");
        let Step::Ask(question) = &step else { panic!("{step:?}") };
        assert_eq!((question.title.as_str(), question.confirm_label.as_str()), ("Forget home-5G?", "Forget"));
        assert_eq!(question.body.len(), 2, "it is the only link");
        let said = confirm(&mut mock, &mut page, step);
        assert_eq!(said, [Step::Done(Notice::ok("Forgot home-5G."))]);
        assert!(!page.is_saved("home-5G"));
    }

    #[test]
    fn bluetooth_pairs_connects_disconnects_and_removes_by_address() {
        let mut mock = MockBackend::new();
        let mut page = open(&mut mock);
        assert!(page.pair_or_connect("00:00:00:00:00:00").is_empty());
        assert!(page.error.as_deref().unwrap().starts_with("Press s to look for devices."));

        let effects = page.pair_or_connect(HEADPHONES);
        assert_eq!(effects, [Effect::ConnectBluetooth { address: HEADPHONES.to_owned(), connect: true }]);
        assert_eq!(page.busy.as_deref(), Some("Connecting WH-1000XM4…"));
        run(&mut mock, &mut page, effects);
        let effects = page.pair_or_connect(KEYBOARD);
        assert_eq!(effects, [Effect::ConnectBluetooth { address: KEYBOARD.to_owned(), connect: false }]);
        assert_eq!(page.busy.as_deref(), Some("Disconnecting K380 Keyboard…"));
        run(&mut mock, &mut page, effects);

        // A device the scan found is paired, and the pairing is followed every second.
        let effects = page.scan_bluetooth();
        assert_eq!(effects, [Effect::ScanBluetooth]);
        run(&mut mock, &mut page, effects);
        let found = page.bluetooth.as_ref().unwrap().devices.iter().find(|d| !d.paired).cloned().expect("the scan found one");
        let effects = page.pair_or_connect(&found.address);
        assert_eq!(effects, [Effect::PairBluetooth(found.address.clone())]);
        assert_eq!(page.busy, Some(format!("Pairing with {}…", found.name)));
        let said = run(&mut mock, &mut page, effects);
        assert!(!page.pairing_running() && page.poll(1).is_empty());
        assert_eq!(said, [Step::Done(Notice::ok(format!("Paired with {}. It is trusted and connected.", found.name)))]);

        let step = page.ask_remove_bluetooth(&found.address);
        let Step::Ask(question) = &step else { panic!("{step:?}") };
        assert_eq!(question.title, format!("Remove {}?", found.name));
        assert_eq!(question.confirm, Confirm::RemoveBluetooth(found.address.clone()));
        confirm(&mut mock, &mut page, step);
        assert!(!page.bluetooth.as_ref().unwrap().devices.iter().any(|d| d.address == found.address && d.paired));
        assert_eq!(page.ask_remove_bluetooth("00:00:00:00:00:00"), Step::Stay);
    }

    #[test]
    fn a_failed_pairing_says_why_once() {
        let mut page = Network::new();
        let mut status = crate::mock::sample_bluetooth();
        page.bluetooth = Some(status.clone());
        page.busy = None;
        // While the pairing runs it is read again every second.
        status.pairing = Some(nodo_api::BluetoothPairing { address: HEADPHONES.to_owned(), stage: PairingStage::Pairing });
        assert_eq!(page.apply(&Outcome::BluetoothChanged { result: Ok(status.clone()), quiet: false }), Step::Stay);
        assert!(page.pairing_running());
        assert_eq!((page.poll(1), page.poll(2)), (vec![Effect::PollBluetooth], vec![Effect::PollBluetooth]));
        status.pairing =
            Some(nodo_api::BluetoothPairing { address: HEADPHONES.to_owned(), stage: PairingStage::Failed("it refused".to_owned()) });
        let outcome = Outcome::BluetoothChanged { result: Ok(status), quiet: true };
        assert_eq!(page.apply(&outcome), Step::Failed(Notice::error("Pairing with WH-1000XM4 failed: it refused")));
        assert_eq!(page.apply(&outcome), Step::Stay, "the same stage read again says nothing");
    }

    #[test]
    fn bluetooth_off_is_said_and_turned_on() {
        let mut mock = MockBackend::new();
        mock.bluetooth.enabled = false;
        let mut page = open(&mut mock);
        assert!(page.pair_or_connect(KEYBOARD).is_empty());
        assert_eq!(page.error.as_deref(), Some("Bluetooth is off. Press o to turn it on."));
        assert!(page.scan_bluetooth().is_empty());
        assert_eq!(page.toggle_bluetooth(), [Effect::SetBluetooth(true)]);
        assert_eq!(page.busy.as_deref(), Some("Turning Bluetooth on…"));
    }

    #[test]
    fn radios_that_do_not_answer_say_so_and_can_be_asked_again() {
        let mut page = Network::new();
        assert_eq!(page.ask_again(), [Effect::LoadNetwork]);
        page.no_answer_after(NO_ANSWER_SECONDS - 1);
        assert_eq!(page.wifi_read, RadioRead::Asking);
        assert!(!page.can_ask_again());
        page.no_answer_after(NO_ANSWER_SECONDS);
        assert_eq!((page.wifi_read.clone(), page.bluetooth_read.clone()), (RadioRead::NoAnswer, RadioRead::NoAnswer));
        assert!(page.can_ask_again());
        assert!(page.scan_wifi().is_empty(), "other actions wait for the answer");
        assert_eq!(page.ask_again(), [Effect::LoadNetwork], "asking again does not");
        assert_eq!(page.wifi_read, RadioRead::Asking);

        // A failure is kept apart from the status: nothing shown yet says why in its panel.
        let mut mock = MockBackend::new();
        mock.knobs.radios_failure = Some("connection refused".to_owned());
        let effects = page.ask_again();
        run(&mut mock, &mut page, effects);
        assert!(matches!(&page.wifi_read, RadioRead::Failed(e) if e.contains("connection refused")));
        assert!(page.error.is_none() && page.can_ask_again());
        // One already shown keeps its status, with the error under it.
        let mut shown = open(&mut MockBackend::new());
        let effects = shown.ask_again();
        run(&mut mock, &mut shown, effects);
        assert!(shown.wifi.is_some() && shown.error.as_deref().is_some_and(|e| e.contains("connection refused")));
        assert!(!shown.can_ask_again());
    }

    #[test]
    fn a_join_without_an_address_is_read_again_every_other_second() {
        let mut page = open(&mut MockBackend::new());
        assert!(page.poll(0).is_empty(), "nothing is under way");
        page.wifi.as_mut().unwrap().address = None;
        assert!(page.waiting_for_address());
        assert_eq!(page.poll(4), [Effect::PollWifi]);
        assert!(page.poll(5).is_empty());
        page.busy = Some("Looking for Wi-Fi networks…".to_owned());
        assert!(page.poll(6).is_empty(), "not while the helper works");
        page.busy = None;

        // A quiet read changes the status without a word and without ending what runs.
        let mut status = page.wifi.clone().unwrap();
        status.address = Some("192.168.1.57".to_owned());
        page.busy = Some("Looking for Wi-Fi networks…".to_owned());
        assert_eq!(page.apply(&Outcome::WifiChanged { result: Ok(status), quiet: true }), Step::Stay);
        assert!(page.busy.is_some() && !page.waiting_for_address());
        assert_eq!(page.apply(&Outcome::WifiChanged { result: Err("busy".to_owned()), quiet: true }), Step::Stay);
        assert_eq!(page.error, None);
    }

    #[test]
    fn leaving_reads_the_device_again_and_other_outcomes_are_ignored() {
        let mut page = open(&mut MockBackend::new());
        assert_eq!(page.leave(), Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])]));
        let before = page.clone();
        assert_eq!(page.apply(&Outcome::Failed("something else".into())), Step::Stay);
        page.confirmed(&Confirm::RepairDrive);
        assert_eq!(page, before);
    }
}
