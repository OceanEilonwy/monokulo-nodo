//! Types for the board's radios: Wi-Fi through wpa_supplicant and Bluetooth through BlueZ.
//!
//! Both radios are off until the owner turns them on, and nothing pairs or joins without the
//! owner asking. Wired networking stays the main connection: a Wi-Fi lease gets a higher route
//! metric than the wired one, so it only carries traffic while the wired link is down.

use serde::{Deserialize, Serialize};

use crate::Secret;

/// How a Wi-Fi network protects its traffic, from what it announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WifiSecurity {
    /// No encryption: anyone nearby can read the traffic.
    Open,
    Wpa2Personal,
    Wpa3Personal,
    /// WPA2 and WPA3 both offered. The device joins with WPA3.
    Wpa2Wpa3Personal,
    /// WEP or WPA without WPA2, which are broken and never joined.
    Legacy,
    /// 802.1X with per-user logins, which this device does not join.
    Enterprise,
}

impl WifiSecurity {
    /// Short words for lists.
    pub fn label(self) -> &'static str {
        match self {
            WifiSecurity::Open => "open",
            WifiSecurity::Wpa2Personal => "WPA2",
            WifiSecurity::Wpa3Personal => "WPA3",
            WifiSecurity::Wpa2Wpa3Personal => "WPA2/WPA3",
            WifiSecurity::Legacy => "WEP/WPA",
            WifiSecurity::Enterprise => "enterprise",
        }
    }

    /// Whether joining needs a passphrase.
    pub fn needs_passphrase(self) -> bool {
        matches!(self, WifiSecurity::Wpa2Personal | WifiSecurity::Wpa3Personal | WifiSecurity::Wpa2Wpa3Personal)
    }

    /// Whether this device can join it at all.
    pub fn supported(self) -> bool {
        !matches!(self, WifiSecurity::Legacy | WifiSecurity::Enterprise)
    }
}

/// One network a scan found. Access points sharing a name are shown once, at the best signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal_dbm: i32,
    pub frequency_mhz: u32,
    pub security: WifiSecurity,
    pub saved: bool,
    pub connected: bool,
}

/// A network the device joins by itself when it is in range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWifi {
    pub ssid: String,
    pub security: WifiSecurity,
    /// Unix seconds.
    pub added_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WifiLink {
    /// The radio is off.
    Off,
    /// On, and not joined to any network.
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiStatus {
    /// Why Wi-Fi cannot be used on this device, such as no wireless hardware or wpa_supplicant
    /// missing. `None` when it can.
    pub unavailable: Option<String>,
    pub interface: Option<String>,
    /// The owner turned Wi-Fi on. Off by default.
    pub enabled: bool,
    pub link: WifiLink,
    pub ssid: Option<String>,
    pub signal_dbm: Option<i32>,
    pub frequency_mhz: Option<u32>,
    pub security: Option<WifiSecurity>,
    /// The IPv4 address the network's DHCP server gave, once it has.
    pub address: Option<String>,
    /// A wired link is up. It stays the main connection while it is.
    pub wired_up: bool,
    /// The saved networks, in the order they were added.
    pub saved: Vec<SavedWifi>,
}

/// Joins a network and saves it once the passphrase is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiJoin {
    pub ssid: String,
    /// As the scan reported it. The helper checks it again against its own scan when it can.
    pub security: WifiSecurity,
    /// WPA2 and WPA3 networks: 8 to 63 printable ASCII characters. Stored root-only.
    pub passphrase: Option<Secret>,
    /// Open networks only: the owner was told the traffic is not encrypted.
    pub accept_open: bool,
}

/// One device BlueZ knows: found by a scan, or paired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BluetoothDevice {
    /// `AA:BB:CC:DD:EE:FF`.
    pub address: String,
    /// The device's name, or its address when it has none.
    pub name: String,
    /// BlueZ's icon name, such as `input-keyboard` or `audio-headset`.
    pub kind: Option<String>,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub rssi: Option<i16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingStage {
    Pairing,
    /// Type this passkey on the device, such as a keyboard, then press its Enter key.
    TypePasskey(String),
    /// The device shows this number too. Pairing goes on; compare them.
    ComparePasskey(String),
    /// Paired, trusted and connected.
    Done,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BluetoothPairing {
    pub address: String,
    pub stage: PairingStage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BluetoothStatus {
    /// Why Bluetooth cannot be used here, such as no controller or BlueZ missing.
    pub unavailable: Option<String>,
    /// The owner turned Bluetooth on. Off by default.
    pub enabled: bool,
    /// The controller is powered.
    pub powered: bool,
    /// The controller's own address.
    pub controller: Option<String>,
    /// Paired devices first, then the ones the last scan found.
    pub devices: Vec<BluetoothDevice>,
    /// The pairing the owner started last, while it runs and until the next one.
    pub pairing: Option<BluetoothPairing>,
}
