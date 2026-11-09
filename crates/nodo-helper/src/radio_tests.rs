//! Wi-Fi and Bluetooth requests against fakes: a small wpa_supplicant that answers the control
//! protocol and checks credentials like an access point, and a bluetoothctl with canned devices.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use nodo_api::{
    ApiError, BluetoothStatus, PairingStage, Request, Response, Secret, WifiJoin, WifiLink, WifiSecurity, WifiStatus,
};

use crate::bluetooth::BluetoothIo;
use crate::fake::{helper, FakeSystem};
use crate::helper::Helper;
use crate::radio::Radios;
use crate::wifi::{WifiEvents, WifiIo};
use crate::wpa;

const USER: u32 = 1000;

/// An access point the fake radio can hear.
#[derive(Clone)]
pub struct FakeAp {
    pub ssid: String,
    pub flags: String,
    pub security: WifiSecurity,
    pub passphrase: String,
    pub signal: i32,
    pub address: String,
}

#[derive(Default)]
pub struct WifiState {
    pub interfaces: Vec<String>,
    pub supplicant: bool,
    pub installed_services: BTreeSet<String>,
    pub enabled_services: BTreeSet<String>,
    pub blocked: Option<bool>,
    pub files: BTreeMap<PathBuf, Vec<u8>>,
    pub aps: Vec<FakeAp>,
    networks: BTreeMap<u32, BTreeMap<String, String>>,
    next_id: u32,
    /// The network id and access point joined.
    connected: Option<(u32, usize)>,
    events: VecDeque<String>,
    pub commands: Vec<String>,
    pub wired: bool,
}

#[derive(Clone, Default)]
pub struct FakeWifi(pub Arc<Mutex<WifiState>>);

impl FakeWifi {
    pub fn state(&self) -> std::sync::MutexGuard<'_, WifiState> {
        self.0.lock().unwrap()
    }

    /// A radio on wlan0 with wpa_supplicant and the service installed, hearing two networks.
    pub fn with_radio() -> Self {
        let fake = FakeWifi::default();
        {
            let mut s = fake.state();
            s.interfaces = vec!["wlan0".into()];
            s.supplicant = true;
            s.installed_services.insert("nodo-wifi".into());
            s.wired = true;
            s.aps = vec![
                FakeAp {
                    ssid: "home".into(),
                    flags: "[WPA2-PSK-CCMP][ESS]".into(),
                    security: WifiSecurity::Wpa2Personal,
                    passphrase: "correct horse".into(),
                    signal: -40,
                    address: "192.168.77.50".into(),
                },
                FakeAp {
                    ssid: "w3".into(),
                    flags: "[WPA2-SAE-CCMP][ESS]".into(),
                    security: WifiSecurity::Wpa3Personal,
                    passphrase: "sae passphrase".into(),
                    signal: -60,
                    address: "192.168.78.50".into(),
                },
                FakeAp {
                    ssid: "cafe".into(),
                    flags: "[ESS]".into(),
                    security: WifiSecurity::Open,
                    passphrase: String::new(),
                    signal: -70,
                    address: "10.9.0.7".into(),
                },
                FakeAp {
                    ssid: "office".into(),
                    flags: "[WPA2-EAP-CCMP][ESS]".into(),
                    security: WifiSecurity::Enterprise,
                    passphrase: String::new(),
                    signal: -75,
                    address: String::new(),
                },
            ];
        }
        fake
    }
}

impl WifiState {
    fn running(&self) -> bool {
        self.enabled_services.contains("nodo-wifi")
    }

    /// Whether a network's fields would get through the access point's handshake.
    fn accepts(&self, fields: &BTreeMap<String, String>, ap: &FakeAp) -> bool {
        let psk = fields.get("psk").map(String::as_str).unwrap_or("");
        let key_mgmt = fields.get("key_mgmt").map(String::as_str).unwrap_or("");
        match ap.security {
            WifiSecurity::Open => key_mgmt == "NONE",
            WifiSecurity::Wpa2Personal => {
                key_mgmt.contains("WPA-PSK") && psk == wpa::derive_psk(&ap.passphrase, &ap.ssid).as_str()
            }
            WifiSecurity::Wpa3Personal => key_mgmt.contains("SAE") && psk == format!("\"{}\"", ap.passphrase),
            _ => false,
        }
    }

    fn ap_for(&self, fields: &BTreeMap<String, String>) -> Option<usize> {
        let ssid = fields.get("ssid")?;
        self.aps.iter().position(|ap| &wpa::hex(ap.ssid.as_bytes()) == ssid)
    }

    fn try_join(&mut self, id: u32) {
        let Some(fields) = self.networks.get(&id).cloned() else { return };
        match self.ap_for(&fields) {
            None => self.events.push_back("<3>CTRL-EVENT-NETWORK-NOT-FOUND".into()),
            Some(ap) if self.accepts(&fields, &self.aps[ap]) => {
                self.connected = Some((id, ap));
                self.events.push_back(format!(
                    "<3>CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:0{ap}:00 completed [id={id} id_str=]"
                ));
            }
            Some(ap) => {
                let ssid = self.aps[ap].ssid.clone();
                self.events.push_back(format!(
                    "<3>CTRL-EVENT-SSID-TEMP-DISABLED id={id} ssid=\"{ssid}\" auth_failures=1 duration=10 reason=WRONG_KEY"
                ));
            }
        }
    }

    /// Reads the configuration file again, as `RECONFIGURE` does, and joins the best match.
    fn reconfigure(&mut self, path: &Path) {
        self.networks.clear();
        self.connected = None;
        let text = String::from_utf8(self.files.get(path).cloned().unwrap_or_default()).unwrap();
        let mut current: Option<BTreeMap<String, String>> = None;
        for line in text.lines() {
            match line.trim() {
                "network={" => current = Some(BTreeMap::new()),
                "}" => {
                    let fields = current.take().unwrap();
                    self.networks.insert(self.next_id, fields);
                    self.next_id += 1;
                }
                other => {
                    if let (Some(fields), Some((k, v))) = (current.as_mut(), other.split_once('=')) {
                        fields.insert(k.to_owned(), v.to_owned());
                    }
                }
            }
        }
        let mut best: Option<(u32, u32)> = None;
        for (id, fields) in &self.networks {
            let priority = fields.get("priority").and_then(|p| p.parse().ok()).unwrap_or(0);
            if self.ap_for(fields).is_some_and(|ap| self.accepts(fields, &self.aps[ap]))
                && best.is_none_or(|(_, p)| priority > p)
            {
                best = Some((*id, priority));
            }
        }
        if let Some((id, _)) = best {
            self.try_join(id);
            self.events.clear();
        }
    }
}

struct FakeEvents(FakeWifi);

impl WifiEvents for FakeEvents {
    fn next(&mut self, _timeout: Duration) -> Option<String> {
        self.0.state().events.pop_front()
    }
}

impl WifiIo for FakeWifi {
    fn interfaces(&self) -> Vec<String> {
        self.state().interfaces.clone()
    }
    fn supplicant_installed(&self) -> bool {
        self.state().supplicant
    }
    fn wired_up(&self) -> bool {
        self.state().wired
    }
    fn set_blocked(&self, blocked: bool) -> Result<(), String> {
        self.state().blocked = Some(blocked);
        Ok(())
    }
    fn service_installed(&self, name: &str) -> bool {
        self.state().installed_services.contains(name)
    }
    fn service_enabled(&self, name: &str) -> bool {
        self.state().enabled_services.contains(name)
    }
    fn service(&self, name: &str, up: bool) -> Result<(), String> {
        let mut s = self.state();
        if up {
            s.enabled_services.insert(name.to_owned());
            // wpa_supplicant reads its file at start.
            s.reconfigure(Path::new("/w/wpa_supplicant.conf"));
        } else {
            s.enabled_services.remove(name);
            s.connected = None;
        }
        Ok(())
    }
    fn request(&self, _iface: &str, command: &str) -> Result<String, String> {
        let mut s = self.state();
        s.commands.push(command.to_owned());
        if !s.running() {
            return Err("wpa_supplicant is not running on wlan0".into());
        }
        let words: Vec<&str> = command.splitn(4, ' ').collect();
        let reply = match words.as_slice() {
            ["PING"] => "PONG\n".to_owned(),
            ["STATUS"] => match s.connected {
                Some((id, ap)) => {
                    let ap = &s.aps[ap];
                    let key_mgmt = if ap.security == WifiSecurity::Open { "NONE" } else { "WPA2-PSK" };
                    format!(
                        "bssid=02:00:00:00:01:00\nfreq=2412\nssid={}\nid={id}\nkey_mgmt={key_mgmt}\nwpa_state=COMPLETED\nip_address={}\n",
                        ap.ssid, ap.address
                    )
                }
                None => "wpa_state=DISCONNECTED\n".to_owned(),
            },
            ["SIGNAL_POLL"] => "RSSI=-42\nLINKSPEED=54\nFREQUENCY=2412\n".to_owned(),
            ["SCAN"] => {
                s.events.push_back("<2>CTRL-EVENT-SCAN-STARTED ".into());
                s.events.push_back("<2>CTRL-EVENT-SCAN-RESULTS ".into());
                "OK\n".to_owned()
            }
            ["SCAN_RESULTS"] => {
                let mut out = "bssid / frequency / signal level / flags / ssid\n".to_owned();
                for (i, ap) in s.aps.iter().enumerate() {
                    out.push_str(&format!("02:00:00:00:0{i}:00\t2412\t{}\t{}\t{}\n", ap.signal, ap.flags, ap.ssid));
                }
                out
            }
            ["ADD_NETWORK"] => {
                let id = s.next_id;
                s.next_id += 1;
                s.networks.insert(id, BTreeMap::new());
                format!("{id}\n")
            }
            ["SET_NETWORK", id, key, value] => {
                let id: u32 = id.parse().unwrap();
                s.networks.get_mut(&id).unwrap().insert((*key).to_owned(), (*value).to_owned());
                "OK\n".to_owned()
            }
            ["SELECT_NETWORK", id] => {
                s.connected = None;
                s.try_join(id.parse().unwrap());
                "OK\n".to_owned()
            }
            ["REMOVE_NETWORK", id] => {
                let id: u32 = id.parse().unwrap();
                s.networks.remove(&id);
                if s.connected.is_some_and(|(c, _)| c == id) {
                    s.connected = None;
                }
                "OK\n".to_owned()
            }
            ["RECONFIGURE"] => {
                s.reconfigure(Path::new("/w/wpa_supplicant.conf"));
                "OK\n".to_owned()
            }
            ["ENABLE_NETWORK", "all"] => "OK\n".to_owned(),
            _ => "UNKNOWN COMMAND\n".to_owned(),
        };
        Ok(reply)
    }
    fn attach(&self, _iface: &str) -> Result<Box<dyn WifiEvents>, String> {
        if !self.state().running() {
            return Err("not running".into());
        }
        self.state().events.clear();
        Ok(Box::new(FakeEvents(self.clone())))
    }
    fn write_private(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        self.state().files.insert(path.to_owned(), bytes.to_vec());
        Ok(())
    }
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        self.state().files.get(path).cloned()
    }
    fn sleep(&self, _duration: Duration) {}
    fn now(&self) -> u64 {
        1_700_000_000
    }
}

#[derive(Clone)]
pub struct FakeDevice {
    pub name: String,
    pub icon: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub rssi: i16,
}

#[derive(Default)]
pub struct BtState {
    pub controller: bool,
    pub installed: bool,
    pub installed_services: BTreeSet<String>,
    pub enabled_services: BTreeSet<String>,
    pub blocked: Option<bool>,
    pub powered: bool,
    pub known: BTreeMap<String, FakeDevice>,
    /// What a scan finds.
    pub nearby: BTreeMap<String, FakeDevice>,
    pub commands: Vec<String>,
    /// Pairing waits here after showing its passkey, when set.
    pub gate: Option<mpsc::Receiver<()>>,
}

#[derive(Clone, Default)]
pub struct FakeBluetooth(pub Arc<Mutex<BtState>>);

impl FakeBluetooth {
    pub fn state(&self) -> std::sync::MutexGuard<'_, BtState> {
        self.0.lock().unwrap()
    }

    pub fn with_controller() -> Self {
        let fake = FakeBluetooth::default();
        {
            let mut s = fake.state();
            s.controller = true;
            s.installed = true;
            s.installed_services.extend(["bluetoothd".to_owned(), "dbus".to_owned()]);
            let device = |name: &str, icon: &str, rssi| FakeDevice {
                name: name.into(),
                icon: icon.into(),
                paired: false,
                trusted: false,
                connected: false,
                rssi,
            };
            s.nearby.insert("AA:BB:CC:DD:EE:01".into(), device("K380 Keyboard", "input-keyboard", -50));
            s.nearby.insert("AA:BB:CC:DD:EE:02".into(), device("Headphones", "audio-headset", -65));
        }
        fake
    }
}

impl BluetoothIo for FakeBluetooth {
    fn controller_present(&self) -> bool {
        self.state().controller
    }
    fn installed(&self) -> bool {
        self.state().installed
    }
    fn set_blocked(&self, blocked: bool) -> Result<(), String> {
        self.state().blocked = Some(blocked);
        Ok(())
    }
    fn service_installed(&self, name: &str) -> bool {
        self.state().installed_services.contains(name)
    }
    fn service_enabled(&self, name: &str) -> bool {
        self.state().enabled_services.contains(name)
    }
    fn service(&self, name: &str, up: bool) -> Result<(), String> {
        let mut s = self.state();
        s.commands.push(format!("service {name} {}", if up { "up" } else { "down" }));
        if up {
            s.enabled_services.insert(name.to_owned());
        } else {
            s.enabled_services.remove(name);
        }
        Ok(())
    }
    fn ctl(&self, args: &[&str], _timeout: Duration) -> Result<String, String> {
        let mut s = self.state();
        s.commands.push(args.join(" "));
        if !s.enabled_services.contains("bluetoothd") {
            return Err("Waiting to connect to bluetoothd...".into());
        }
        let out = match args {
            ["show"] => format!(
                "Controller 00:11:22:33:44:55 (public)\n\tName: nodo\n\tPowered: {}\n\tDiscoverable: no\n",
                if s.powered { "yes" } else { "no" }
            ),
            ["power", on] => {
                s.powered = *on == "on";
                format!("Changing power {on} succeeded\n")
            }
            ["pairable", "off"] => "Changing pairable off succeeded\n".into(),
            ["discoverable", "off"] => "Changing discoverable off succeeded\n".into(),
            ["devices"] => s.known.iter().map(|(a, d)| format!("Device {a} {}\n", d.name)).collect(),
            ["info", address] => match s.known.get(*address) {
                Some(d) => format!(
                    "Device {address} (public)\n\tName: {}\n\tIcon: {}\n\tPaired: {}\n\tTrusted: {}\n\tConnected: {}\n\tRSSI: 0xffffffc4 ({})\n",
                    d.name,
                    d.icon,
                    if d.paired { "yes" } else { "no" },
                    if d.trusted { "yes" } else { "no" },
                    if d.connected { "yes" } else { "no" },
                    d.rssi
                ),
                None => format!("Device {address} not available\n"),
            },
            ["--timeout", _, "scan", "on"] => {
                let nearby = s.nearby.clone();
                s.known.extend(nearby);
                "Discovery started\n".into()
            }
            ["connect", address] => match s.known.get_mut(*address) {
                Some(d) => {
                    d.connected = true;
                    "Attempting to connect\nConnection successful\n".into()
                }
                None => format!("Device {address} not available\n"),
            },
            ["disconnect", address] => {
                if let Some(d) = s.known.get_mut(*address) {
                    d.connected = false;
                }
                "Successful disconnected\n".into()
            }
            ["remove", address] => match s.known.remove(*address) {
                Some(_) => "Device has been removed\n".into(),
                None => format!("Device {address} not available\n"),
            },
            _ => return Err(format!("unexpected: {args:?}")),
        };
        Ok(out)
    }
    fn pair(&self, address: &str, report: &mut dyn FnMut(PairingStage)) -> Result<(), String> {
        self.state().commands.push(format!("pair {address}"));
        if !self.state().known.contains_key(address) {
            return Err("Failed to pair: org.bluez.Error.AuthenticationFailed".into());
        }
        report(PairingStage::TypePasskey("123456".into()));
        let gate = self.state().gate.take();
        if let Some(gate) = gate {
            let _ = gate.recv_timeout(Duration::from_secs(10));
        }
        let mut s = self.state();
        let device = s.known.get_mut(address).unwrap();
        device.paired = true;
        device.trusted = true;
        device.connected = true;
        Ok(())
    }
    fn sleep(&self, _duration: Duration) {}
}

/// Radios with no hardware, which the shared test helper uses so no test touches the machine.
pub fn no_radios() -> Radios {
    radios(FakeWifi::default(), FakeBluetooth::default())
}

fn radios(wifi: FakeWifi, bt: FakeBluetooth) -> Radios {
    let mut radios = Radios::new(Box::new(wifi), Arc::new(bt));
    radios.wifi_paths.store = "/w/networks.toml".into();
    radios.wifi_paths.config = "/w/wpa_supplicant.conf".into();
    radios
}

fn setup() -> (Helper<FakeSystem>, FakeWifi, FakeBluetooth) {
    let wifi = FakeWifi::with_radio();
    let bt = FakeBluetooth::with_controller();
    let mut r = radios(wifi.clone(), bt.clone());
    r.pair_inline = true;
    (helper(FakeSystem::default()).with_radios(r), wifi, bt)
}

fn wifi_status(h: &Helper<FakeSystem>) -> WifiStatus {
    match h.handle(USER, Request::WifiStatus) {
        Ok(Response::Wifi(status)) => status,
        other => panic!("{other:?}"),
    }
}

fn bt_status(h: &Helper<FakeSystem>) -> BluetoothStatus {
    match h.handle(USER, Request::BluetoothStatus) {
        Ok(Response::Bluetooth(status)) => status,
        other => panic!("{other:?}"),
    }
}

fn join(ssid: &str, security: WifiSecurity, passphrase: Option<&str>, accept_open: bool) -> Request {
    Request::JoinWifi(WifiJoin {
        ssid: ssid.into(),
        security,
        passphrase: passphrase.map(Secret::new),
        accept_open,
    })
}

fn file(wifi: &FakeWifi, path: &str) -> String {
    String::from_utf8(wifi.state().files.get(Path::new(path)).cloned().unwrap_or_default()).unwrap()
}

fn on(h: &Helper<FakeSystem>) {
    assert!(matches!(h.handle(USER, Request::SetWifiEnabled { enabled: true }), Ok(Response::Wifi(_))));
}

#[test]
fn wifi_is_off_until_turned_on_and_blocked_at_start() {
    let (h, wifi, _) = setup();
    let status = wifi_status(&h);
    assert!(!status.enabled);
    assert_eq!(status.link, WifiLink::Off);
    assert_eq!(status.interface.as_deref(), Some("wlan0"));
    assert!(status.unavailable.is_none());
    assert!(status.wired_up);
    h.apply_radio_state();
    assert_eq!(wifi.state().blocked, Some(true));
    // Nothing reached wpa_supplicant, and requests that need it say to turn it on.
    assert!(matches!(h.handle(USER, Request::ScanWifi), Err(ApiError::Failed(m)) if m.contains("Turn it on")));
    let err = h.handle(USER, join("home", WifiSecurity::Wpa2Personal, Some("correct horse"), false));
    assert!(matches!(err, Err(ApiError::Failed(m)) if m.contains("Turn it on")));
}

#[test]
fn wifi_says_why_it_cannot_be_used() {
    let h = helper(FakeSystem::default());
    let status = wifi_status(&h);
    assert_eq!(status.unavailable.as_deref(), Some("No Wi-Fi hardware was found."));
    assert!(h.handle(USER, Request::SetWifiEnabled { enabled: true }).is_err());

    let (h, wifi, _) = setup();
    wifi.state().supplicant = false;
    assert!(wifi_status(&h).unavailable.unwrap().contains("wpa_supplicant is not installed"));
    wifi.state().supplicant = true;
    wifi.state().enabled_services.insert("wpa_supplicant".into());
    assert!(wifi_status(&h).unavailable.unwrap().contains("outside the helper"));
    assert!(h.handle(USER, Request::SetWifiEnabled { enabled: true }).is_err());
    assert!(!wifi.state().enabled_services.contains("nodo-wifi"));
}

#[test]
fn turning_wifi_on_and_off_unblocks_starts_stops_and_blocks() {
    let (h, wifi, _) = setup();
    on(&h);
    {
        let s = wifi.state();
        assert_eq!(s.blocked, Some(false));
        assert!(s.enabled_services.contains("nodo-wifi"));
    }
    let config = file(&wifi, "/w/wpa_supplicant.conf");
    assert!(config.contains("ctrl_interface=DIR=/run/nodo/wpa GROUP=root"), "{config}");
    assert_eq!(wifi_status(&h).link, WifiLink::Disconnected);
    let Ok(Response::Wifi(status)) = h.handle(USER, Request::SetWifiEnabled { enabled: false }) else { panic!() };
    assert_eq!(status.link, WifiLink::Off);
    assert!(!status.enabled);
    assert_eq!(wifi.state().blocked, Some(true));
    assert!(!wifi.state().enabled_services.contains("nodo-wifi"));
}

#[test]
fn scan_lists_networks_with_signal_security_and_saved() {
    let (h, _, _) = setup();
    on(&h);
    let Ok(Response::WifiNetworks(list)) = h.handle(USER, Request::ScanWifi) else { panic!() };
    let names: Vec<(&str, i32, WifiSecurity)> = list.iter().map(|n| (n.ssid.as_str(), n.signal_dbm, n.security)).collect();
    assert_eq!(
        names,
        [
            ("home", -40, WifiSecurity::Wpa2Personal),
            ("w3", -60, WifiSecurity::Wpa3Personal),
            ("cafe", -70, WifiSecurity::Open),
            ("office", -75, WifiSecurity::Enterprise)
        ]
    );
    assert!(list.iter().all(|n| !n.saved && !n.connected));
    h.handle(USER, join("home", WifiSecurity::Wpa2Personal, Some("correct horse"), false)).unwrap();
    let Ok(Response::WifiNetworks(list)) = h.handle(USER, Request::ScanWifi) else { panic!() };
    assert!(list[0].saved && list[0].connected);
}

#[test]
fn a_wrong_passphrase_is_refused_and_nothing_is_saved() {
    let (h, wifi, _) = setup();
    on(&h);
    let err = h.handle(USER, join("home", WifiSecurity::Wpa2Personal, Some("wrong horse"), false)).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("home refused the passphrase. Nothing was saved."), "{text}");
    assert!(!text.contains("wrong horse"));
    assert!(!wifi.state().files.contains_key(Path::new("/w/networks.toml")));
    // The trial network was removed and the saved ones read back.
    let commands = wifi.state().commands.clone();
    let tail: Vec<&str> = commands.iter().rev().take(2).map(String::as_str).collect();
    assert_eq!(tail, ["RECONFIGURE", "REMOVE_NETWORK 0"]);
    assert!(wifi.state().networks.is_empty());
    assert_eq!(wifi_status(&h).link, WifiLink::Disconnected);
}

#[test]
fn the_right_passphrase_joins_saves_a_derived_key_and_reports_the_address() {
    let (h, wifi, _) = setup();
    on(&h);
    let Ok(Response::Wifi(status)) = h.handle(USER, join("home", WifiSecurity::Wpa2Personal, Some("correct horse"), false))
    else {
        panic!()
    };
    assert_eq!(status.link, WifiLink::Connected);
    assert_eq!(status.ssid.as_deref(), Some("home"));
    assert_eq!(status.address.as_deref(), Some("192.168.77.50"));
    assert_eq!(status.signal_dbm, Some(-42));
    assert_eq!(status.saved.len(), 1);
    let psk = wpa::derive_psk("correct horse", "home");
    let store = file(&wifi, "/w/networks.toml");
    let config = file(&wifi, "/w/wpa_supplicant.conf");
    assert!(store.contains(psk.as_str()) && config.contains(psk.as_str()));
    // A WPA2 passphrase is never kept, only the key derived from it.
    assert!(!store.contains("correct horse") && !config.contains("correct horse"));
    assert!(config.contains(&format!("ssid={}", wpa::hex(b"home"))));
    // Saved networks join again after wpa_supplicant starts over.
    h.handle(USER, Request::SetWifiEnabled { enabled: false }).unwrap();
    on(&h);
    assert_eq!(wifi_status(&h).ssid.as_deref(), Some("home"));
}

#[test]
fn wpa3_keeps_the_passphrase_sae_needs() {
    let (h, wifi, _) = setup();
    on(&h);
    // The request says WPA2, but the scan says WPA3, and the scan wins.
    let Ok(Response::Wifi(status)) = h.handle(USER, join("w3", WifiSecurity::Wpa2Personal, Some("sae passphrase"), false))
    else {
        panic!()
    };
    assert_eq!(status.ssid.as_deref(), Some("w3"));
    assert_eq!(status.saved[0].security, WifiSecurity::Wpa3Personal);
    assert!(file(&wifi, "/w/wpa_supplicant.conf").contains("\tkey_mgmt=SAE\n\tieee80211w=2\n"));
}

#[test]
fn open_networks_need_the_owner_to_accept_unencrypted_traffic() {
    let (h, wifi, _) = setup();
    on(&h);
    let err = h.handle(USER, join("cafe", WifiSecurity::Open, None, false)).unwrap_err();
    assert!(matches!(&err, ApiError::NeedsConfirmation(m) if m.contains("anyone nearby can read")), "{err:?}");
    assert!(!wifi.state().files.contains_key(Path::new("/w/networks.toml")));
    // A passphrase for a network that is open is refused rather than sent anywhere.
    let err = h.handle(USER, join("cafe", WifiSecurity::Wpa2Personal, Some("anything at all"), true)).unwrap_err();
    assert!(err.to_string().contains("open network now"), "{err}");
    let Ok(Response::Wifi(status)) = h.handle(USER, join("cafe", WifiSecurity::Open, None, true)) else { panic!() };
    assert_eq!(status.security, Some(WifiSecurity::Open));
    assert_eq!(status.address.as_deref(), Some("10.9.0.7"));
}

#[test]
fn bad_networks_and_passphrases_are_refused_before_anything_changes() {
    let (h, wifi, _) = setup();
    on(&h);
    wifi.state().commands.clear();
    for (request, words) in [
        (join("office", WifiSecurity::Wpa2Personal, Some("whatever1"), false), "per-user login"),
        (join("home", WifiSecurity::Wpa2Personal, Some("short"), false), "at least 8"),
        (join("home", WifiSecurity::Wpa2Personal, None, false), "needs its passphrase"),
        (join("", WifiSecurity::Open, None, true), "empty"),
        (join("a\nb", WifiSecurity::Open, None, true), "control characters"),
    ] {
        let err = h.handle(USER, request).unwrap_err().to_string();
        assert!(err.contains(words), "{err}");
    }
    assert!(!wifi.state().commands.iter().any(|c| c.starts_with("ADD_NETWORK")));
}

#[test]
fn a_network_out_of_range_is_not_saved() {
    let (h, wifi, _) = setup();
    on(&h);
    let err = h.handle(USER, join("elsewhere", WifiSecurity::Wpa2Personal, Some("whatever1"), false)).unwrap_err();
    assert!(err.to_string().contains("not in range"), "{err}");
    assert!(!wifi.state().files.contains_key(Path::new("/w/networks.toml")));
}

#[test]
fn forgetting_removes_the_network_and_its_key_and_leaves_it() {
    let (h, wifi, _) = setup();
    on(&h);
    h.handle(USER, join("home", WifiSecurity::Wpa2Personal, Some("correct horse"), false)).unwrap();
    h.handle(USER, join("cafe", WifiSecurity::Open, None, true)).unwrap();
    assert_eq!(wifi_status(&h).saved.len(), 2);
    assert_eq!(h.handle(USER, Request::ForgetWifi { ssid: "nope".into() }), Err(ApiError::NotFound("nope".into())));
    let Ok(Response::Wifi(status)) = h.handle(USER, Request::ForgetWifi { ssid: "home".into() }) else { panic!() };
    assert_eq!(status.saved.iter().map(|s| s.ssid.as_str()).collect::<Vec<_>>(), ["cafe"]);
    let psk = wpa::derive_psk("correct horse", "home");
    assert!(!file(&wifi, "/w/networks.toml").contains(psk.as_str()));
    assert!(!file(&wifi, "/w/wpa_supplicant.conf").contains(psk.as_str()));
    let Ok(Response::Wifi(status)) = h.handle(USER, Request::ForgetWifi { ssid: "cafe".into() }) else { panic!() };
    assert_eq!(status.link, WifiLink::Disconnected);
    assert!(status.saved.is_empty());
}

#[test]
fn radio_requests_are_denied_to_strangers_and_touch_nothing() {
    let (h, wifi, bt) = setup();
    for request in [
        Request::WifiStatus,
        Request::SetWifiEnabled { enabled: true },
        Request::ScanWifi,
        join("home", WifiSecurity::Wpa2Personal, Some("correct horse"), false),
        Request::ForgetWifi { ssid: "home".into() },
        Request::BluetoothStatus,
        Request::SetBluetoothEnabled { enabled: true },
        Request::ScanBluetooth,
        Request::PairBluetooth { address: "AA:BB:CC:DD:EE:01".into() },
        Request::ConnectBluetooth { address: "AA:BB:CC:DD:EE:01".into(), connect: true },
        Request::RemoveBluetooth { address: "AA:BB:CC:DD:EE:01".into() },
    ] {
        assert_eq!(h.handle(4242, request), Err(ApiError::Denied));
    }
    assert!(wifi.state().commands.is_empty() && wifi.state().blocked.is_none());
    assert!(bt.state().commands.is_empty() && bt.state().blocked.is_none());
}

#[test]
fn bluetooth_is_off_until_turned_on() {
    let (h, _, bt) = setup();
    let status = bt_status(&h);
    assert!(!status.enabled && !status.powered && status.unavailable.is_none());
    h.apply_radio_state();
    assert_eq!(bt.state().blocked, Some(true));
    for request in [Request::ScanBluetooth, Request::PairBluetooth { address: "AA:BB:CC:DD:EE:01".into() }] {
        assert!(matches!(h.handle(USER, request), Err(ApiError::Failed(m)) if m.contains("Turn it on")));
    }
    assert!(bt.state().commands.is_empty());
    let none = helper(FakeSystem::default());
    assert_eq!(bt_status(&none).unavailable.as_deref(), Some("No Bluetooth controller was found."));
}

#[test]
fn turning_bluetooth_on_keeps_it_unpairable_and_hidden() {
    let (h, _, bt) = setup();
    let Ok(Response::Bluetooth(status)) = h.handle(USER, Request::SetBluetoothEnabled { enabled: true }) else { panic!() };
    assert!(status.enabled && status.powered);
    assert_eq!(status.controller.as_deref(), Some("00:11:22:33:44:55"));
    assert_eq!(bt.state().blocked, Some(false));
    let commands = bt.state().commands.clone();
    let start: Vec<&str> = commands.iter().take(6).map(String::as_str).collect();
    assert_eq!(
        start,
        ["service dbus up", "service bluetoothd up", "show", "power on", "pairable off", "discoverable off"]
    );
    let Ok(Response::Bluetooth(status)) = h.handle(USER, Request::SetBluetoothEnabled { enabled: false }) else { panic!() };
    assert!(!status.enabled);
    assert_eq!(bt.state().blocked, Some(true));
    let s = bt.state();
    assert!(s.commands.iter().any(|c| c == "power off"));
    assert!(s.commands.iter().any(|c| c == "service bluetoothd down"));
    // D-Bus stays for whatever else uses it.
    assert!(s.enabled_services.contains("dbus"));
}

#[test]
fn scan_pair_connect_and_remove() {
    let (h, _, _) = setup();
    h.handle(USER, Request::SetBluetoothEnabled { enabled: true }).unwrap();
    assert!(bt_status(&h).devices.is_empty());
    let Ok(Response::Bluetooth(status)) = h.handle(USER, Request::ScanBluetooth) else { panic!() };
    let names: Vec<&str> = status.devices.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["K380 Keyboard", "Headphones"]);
    assert_eq!(status.devices[0].kind.as_deref(), Some("input-keyboard"));
    let Ok(Response::Bluetooth(status)) =
        h.handle(USER, Request::PairBluetooth { address: "aa:bb:cc:dd:ee:02".into() })
    else {
        panic!()
    };
    let pairing = status.pairing.unwrap();
    assert_eq!((pairing.address.as_str(), pairing.stage), ("AA:BB:CC:DD:EE:02", PairingStage::Done));
    let headphones = status.devices.iter().find(|d| d.address == "AA:BB:CC:DD:EE:02").unwrap();
    assert!(headphones.paired && headphones.trusted && headphones.connected);
    // Paired devices come first.
    assert_eq!(status.devices[0].address, "AA:BB:CC:DD:EE:02");
    let Ok(Response::Bluetooth(status)) =
        h.handle(USER, Request::ConnectBluetooth { address: "AA:BB:CC:DD:EE:02".into(), connect: false })
    else {
        panic!()
    };
    assert!(!status.devices[0].connected);
    let Ok(Response::Bluetooth(status)) = h.handle(USER, Request::RemoveBluetooth { address: "AA:BB:CC:DD:EE:02".into() })
    else {
        panic!()
    };
    assert!(status.devices.iter().all(|d| d.address != "AA:BB:CC:DD:EE:02"));
    assert!(status.pairing.is_none());
    assert_eq!(
        h.handle(USER, Request::RemoveBluetooth { address: "AA:BB:CC:DD:EE:02".into() }),
        Err(ApiError::NotFound("AA:BB:CC:DD:EE:02".into()))
    );
}

#[test]
fn a_failed_pairing_is_reported_and_bad_addresses_never_reach_bluetoothctl() {
    let (h, _, bt) = setup();
    h.handle(USER, Request::SetBluetoothEnabled { enabled: true }).unwrap();
    let Ok(Response::Bluetooth(status)) =
        h.handle(USER, Request::PairBluetooth { address: "AA:BB:CC:DD:EE:99".into() })
    else {
        panic!()
    };
    assert!(matches!(status.pairing.unwrap().stage, PairingStage::Failed(m) if m.contains("AuthenticationFailed")));
    bt.state().commands.clear();
    for address in ["--help", "AA:BB:CC:DD:EE", "AA:BB:CC:DD:EE:FF;x"] {
        assert!(h.handle(USER, Request::PairBluetooth { address: address.into() }).is_err());
        assert!(h.handle(USER, Request::RemoveBluetooth { address: address.into() }).is_err());
    }
    assert!(bt.state().commands.is_empty(), "{:?}", bt.state().commands);
}

#[test]
fn pairing_runs_on_its_own_and_shows_the_passkey_to_type() {
    let wifi = FakeWifi::with_radio();
    let bt = FakeBluetooth::with_controller();
    let h = helper(FakeSystem::default()).with_radios(radios(wifi, bt.clone()));
    h.handle(USER, Request::SetBluetoothEnabled { enabled: true }).unwrap();
    h.handle(USER, Request::ScanBluetooth).unwrap();
    let (release, gate) = mpsc::channel();
    bt.state().gate = Some(gate);
    let Ok(Response::Bluetooth(status)) =
        h.handle(USER, Request::PairBluetooth { address: "AA:BB:CC:DD:EE:01".into() })
    else {
        panic!()
    };
    assert!(status.pairing.is_some());
    let wait_for = |want: &PairingStage| {
        for _ in 0..500 {
            if bt_status(&h).pairing.is_some_and(|p| &p.stage == want) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("never reached {want:?}");
    };
    wait_for(&PairingStage::TypePasskey("123456".into()));
    // One pairing at a time.
    let err = h.handle(USER, Request::PairBluetooth { address: "AA:BB:CC:DD:EE:02".into() }).unwrap_err();
    assert!(err.to_string().contains("already running"));
    release.send(()).unwrap();
    wait_for(&PairingStage::Done);
}
