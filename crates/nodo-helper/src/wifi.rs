//! Wi-Fi through wpa_supplicant, for the TUI's Network screen.
//!
//! nodo-tools ships the `nodo-wifi` runit service, which runs wpa_supplicant on the first
//! wireless interface (the first `/sys/class/net/*/phy80211` in name order) with the file this
//! module writes, `/var/lib/nodo/wifi/wpa_supplicant.conf`. Wi-Fi is off until the owner turns it
//! on: the service is not linked and the radio is soft-blocked with rfkill. Turning it on unblocks
//! the radio and links the service; off takes the service down, with a `down` file so it stays
//! down across reboots, and blocks the radio again. The helper blocks the radio at start too while
//! Wi-Fi is off, since rfkill state does not survive a reboot.
//!
//! Passphrases are stored root-only in `/var/lib/nodo/wifi` (0700). For a WPA2-only network the
//! helper keeps only the derived key (PBKDF2 of the passphrase and the name), never the passphrase;
//! WPA3 needs the passphrase itself. They are not sealed with the device key, because Wi-Fi may be
//! how the owner reaches the device to unlock it after a reboot. They are left out of backups.
//!
//! A network is saved only once it accepted the passphrase: the helper adds it to the running
//! wpa_supplicant, selects it and waits for the connection or a refusal, removes it again on a
//! refusal, and only then writes the file. dhcpcd, which manages every interface, then leases
//! an address on the wireless interface. Its default route metric is 1000 plus the interface
//! index, plus 2000 for a wireless one, so wired stays the main connection and Wi-Fi only carries
//! traffic while the wired link is down.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nodo_api::{ApiError, Response, SavedWifi, WifiJoin, WifiLink, WifiSecurity, WifiStatus};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::helper::{lock, Helper};
use crate::wpa::{self, Credential, JoinEvent, NetworkConfig};

pub const WIFI_SERVICE: &str = "nodo-wifi";
/// Void's own service, which would fight nodo-wifi over the interface.
pub const VOID_SERVICE: &str = "wpa_supplicant";

/// How long a join may take before the helper gives up: association, the handshake, and a retry.
const JOIN_TIMEOUT: Duration = Duration::from_secs(30);
const ADDRESS_TIMEOUT: Duration = Duration::from_secs(30);
const SCAN_TIMEOUT: Duration = Duration::from_secs(15);
const START_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiPaths {
    /// Saved networks with their credentials. Root-only.
    pub store: PathBuf,
    /// What wpa_supplicant reads, rendered from the store. Root-only.
    pub config: PathBuf,
    /// Where wpa_supplicant puts its control sockets, one per interface.
    pub ctrl_dir: PathBuf,
}

impl Default for WifiPaths {
    fn default() -> Self {
        WifiPaths {
            store: "/var/lib/nodo/wifi/networks.toml".into(),
            config: "/var/lib/nodo/wifi/wpa_supplicant.conf".into(),
            ctrl_dir: "/run/nodo/wpa".into(),
        }
    }
}

/// Events from wpa_supplicant's control socket, after `ATTACH`.
pub trait WifiEvents {
    /// The next event, or `None` when `timeout` passed without one.
    fn next(&mut self, timeout: Duration) -> Option<String>;
}

/// What the helper needs from the machine for Wi-Fi, so the rules are tested without a radio.
pub trait WifiIo: Send + Sync {
    /// Wireless interfaces, in name order.
    fn interfaces(&self) -> Vec<String>;
    fn supplicant_installed(&self) -> bool;
    /// A wired interface has a link.
    fn wired_up(&self) -> bool;
    /// Soft-blocks or unblocks every Wi-Fi radio with rfkill.
    fn set_blocked(&self, blocked: bool) -> Result<(), String>;
    fn service_installed(&self, name: &str) -> bool;
    /// Linked and not held down.
    fn service_enabled(&self, name: &str) -> bool;
    fn service(&self, name: &str, up: bool) -> Result<(), String>;
    /// One command over the interface's control socket, and its reply. Commands can hold a
    /// credential, so an implementation never puts the command into an error or a log.
    fn request(&self, iface: &str, command: &str) -> Result<String, String>;
    fn attach(&self, iface: &str) -> Result<Box<dyn WifiEvents>, String>;
    /// Writes a root-only file atomically.
    fn write_private(&self, path: &std::path::Path, bytes: &[u8]) -> Result<(), String>;
    fn read(&self, path: &std::path::Path) -> Option<Vec<u8>>;
    fn sleep(&self, duration: Duration);
    fn now(&self) -> u64;
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default, rename = "network")]
    networks: Vec<StoredNetwork>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredNetwork {
    ssid: String,
    security: StoredSecurity,
    /// The derived key for WPA2, the passphrase for WPA3, empty for open networks.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    secret: String,
    added_at: u64,
}

impl Drop for StoredNetwork {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.secret);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StoredSecurity {
    Open,
    Wpa2,
    Wpa3,
    Wpa2Wpa3,
}

impl StoredSecurity {
    fn api(self) -> WifiSecurity {
        match self {
            StoredSecurity::Open => WifiSecurity::Open,
            StoredSecurity::Wpa2 => WifiSecurity::Wpa2Personal,
            StoredSecurity::Wpa3 => WifiSecurity::Wpa3Personal,
            StoredSecurity::Wpa2Wpa3 => WifiSecurity::Wpa2Wpa3Personal,
        }
    }

    fn from_api(security: WifiSecurity) -> Option<Self> {
        match security {
            WifiSecurity::Open => Some(StoredSecurity::Open),
            WifiSecurity::Wpa2Personal => Some(StoredSecurity::Wpa2),
            WifiSecurity::Wpa3Personal => Some(StoredSecurity::Wpa3),
            WifiSecurity::Wpa2Wpa3Personal => Some(StoredSecurity::Wpa2Wpa3),
            WifiSecurity::Legacy | WifiSecurity::Enterprise => None,
        }
    }
}

impl StoredNetwork {
    fn config(&self, priority: u32) -> NetworkConfig {
        let secret = Zeroizing::new(self.secret.clone());
        let credential = match self.security {
            StoredSecurity::Open => Credential::Open,
            StoredSecurity::Wpa2 => Credential::Psk(secret),
            StoredSecurity::Wpa3 | StoredSecurity::Wpa2Wpa3 => Credential::Passphrase(secret),
        };
        NetworkConfig { ssid: self.ssid.clone(), security: self.security.api(), credential, priority }
    }
}

fn failed(text: impl Into<String>) -> ApiError {
    ApiError::Failed(text.into())
}

/// Removes the network it was given from the running wpa_supplicant unless disarmed, so an
/// error part way through a join leaves nothing behind.
struct Trial<'a> {
    io: &'a dyn WifiIo,
    iface: &'a str,
    id: Option<String>,
}

impl Drop for Trial<'_> {
    fn drop(&mut self) {
        if let Some(id) = &self.id {
            let _ = self.io.request(self.iface, &format!("REMOVE_NETWORK {id}"));
            // SELECT_NETWORK disabled the others. Reading the file again brings them back.
            let _ = self.io.request(self.iface, "RECONFIGURE");
        }
    }
}

impl<S: crate::system::System> Helper<S> {
    fn wifi_io(&self) -> &dyn WifiIo {
        self.radios.wifi.as_ref()
    }

    fn wifi_interface(&self) -> Option<String> {
        self.wifi_io().interfaces().into_iter().next()
    }

    /// Why Wi-Fi cannot be used, or `None` with the interface it uses.
    fn wifi_unavailable(&self) -> Result<String, String> {
        let io = self.wifi_io();
        let Some(iface) = self.wifi_interface() else {
            return Err("No Wi-Fi hardware was found.".to_owned());
        };
        if !io.supplicant_installed() {
            return Err("wpa_supplicant is not installed, so Wi-Fi cannot be used.".to_owned());
        }
        if !io.service_installed(WIFI_SERVICE) {
            return Err("The nodo-wifi service is missing. Reinstall nodo-tools.".to_owned());
        }
        if io.service_enabled(VOID_SERVICE) {
            return Err(
                "The wpa_supplicant service is enabled outside the helper. Disable it so the helper can manage Wi-Fi."
                    .to_owned(),
            );
        }
        Ok(iface)
    }

    fn load_wifi_store(&self) -> Result<Store, ApiError> {
        match self.wifi_io().read(&self.radios.wifi_paths.store) {
            None => Ok(Store::default()),
            Some(bytes) => {
                let text = Zeroizing::new(
                    String::from_utf8(bytes).map_err(|_| failed("the saved Wi-Fi networks are corrupt"))?,
                );
                toml::from_str(&text).map_err(|_| failed("the saved Wi-Fi networks are corrupt"))
            }
        }
    }

    /// Writes the store and the file wpa_supplicant reads from it.
    fn save_wifi_store(&self, store: &Store) -> Result<(), ApiError> {
        let io = self.wifi_io();
        let text = Zeroizing::new(toml::to_string(store).map_err(|e| failed(e.to_string()))?);
        io.write_private(&self.radios.wifi_paths.store, text.as_bytes()).map_err(failed)?;
        self.write_wifi_config(store)
    }

    fn write_wifi_config(&self, store: &Store) -> Result<(), ApiError> {
        let networks: Vec<NetworkConfig> =
            store.networks.iter().enumerate().map(|(i, n)| n.config(i as u32 + 1)).collect();
        let ctrl = self.radios.wifi_paths.ctrl_dir.to_string_lossy().into_owned();
        let text = wpa::render_config(&ctrl, &networks);
        self.wifi_io().write_private(&self.radios.wifi_paths.config, text.as_bytes()).map_err(failed)
    }

    /// The control socket answers, waiting for it after a start.
    fn wait_for_supplicant(&self, iface: &str, timeout: Duration) -> bool {
        let io = self.wifi_io();
        let start = Instant::now();
        loop {
            if io.request(iface, "PING").is_ok_and(|r| r.trim() == "PONG") {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            io.sleep(Duration::from_millis(250));
        }
    }

    pub(crate) fn wifi_status(&self) -> Result<WifiStatus, ApiError> {
        let io = self.wifi_io();
        let store = self.load_wifi_store()?;
        let saved = store
            .networks
            .iter()
            .map(|n| SavedWifi { ssid: n.ssid.clone(), security: n.security.api(), added_at: n.added_at })
            .collect();
        let mut status = WifiStatus {
            unavailable: None,
            interface: self.wifi_interface(),
            enabled: io.service_enabled(WIFI_SERVICE),
            link: WifiLink::Off,
            ssid: None,
            signal_dbm: None,
            frequency_mhz: None,
            security: None,
            address: None,
            wired_up: io.wired_up(),
            saved,
        };
        let iface = match self.wifi_unavailable() {
            Ok(iface) => iface,
            Err(why) => {
                status.unavailable = Some(why);
                return Ok(status);
            }
        };
        if !status.enabled {
            return Ok(status);
        }
        status.link = WifiLink::Disconnected;
        let Ok(reply) = io.request(&iface, "STATUS") else {
            return Ok(status);
        };
        let pairs = wpa::parse_pairs(&reply);
        let state = pairs.get("wpa_state").map(String::as_str).unwrap_or("");
        status.link = match state {
            "COMPLETED" => WifiLink::Connected,
            "AUTHENTICATING" | "ASSOCIATING" | "ASSOCIATED" | "4WAY_HANDSHAKE" | "GROUP_HANDSHAKE" => {
                WifiLink::Connecting
            }
            _ => WifiLink::Disconnected,
        };
        if status.link == WifiLink::Disconnected {
            return Ok(status);
        }
        status.ssid = pairs.get("ssid").and_then(|s| wpa::unescape_ssid(s));
        status.frequency_mhz = pairs.get("freq").and_then(|f| f.parse().ok());
        status.security = pairs.get("key_mgmt").map(|k| wpa::security_from_key_mgmt(k));
        if status.link == WifiLink::Connected {
            status.address = pairs.get("ip_address").filter(|a| !a.is_empty()).cloned();
            if let Ok(poll) = io.request(&iface, "SIGNAL_POLL") {
                status.signal_dbm = wpa::parse_pairs(&poll).get("RSSI").and_then(|r| r.parse().ok());
            }
        }
        Ok(status)
    }

    pub(crate) fn set_wifi_enabled(&self, enabled: bool) -> Result<Response, ApiError> {
        let _guard = lock(&self.radios.wifi_lock);
        let io = self.wifi_io();
        if enabled {
            let iface = self.wifi_unavailable().map_err(failed)?;
            self.write_wifi_config(&self.load_wifi_store()?)?;
            io.set_blocked(false).map_err(|e| failed(format!("could not unblock the Wi-Fi radio: {e}")))?;
            io.service(WIFI_SERVICE, true).map_err(|e| failed(format!("could not start Wi-Fi: {e}")))?;
            if !self.wait_for_supplicant(&iface, START_TIMEOUT) {
                return Err(failed("Wi-Fi was started but wpa_supplicant does not answer. Its log says why."));
            }
        } else {
            if io.service_installed(WIFI_SERVICE) {
                io.service(WIFI_SERVICE, false).map_err(|e| failed(format!("could not stop Wi-Fi: {e}")))?;
            }
            io.set_blocked(true).map_err(|e| failed(format!("could not block the Wi-Fi radio: {e}")))?;
        }
        Ok(Response::Wifi(self.wifi_status()?))
    }

    /// The interface, once Wi-Fi is on and wpa_supplicant answers.
    fn running_interface(&self) -> Result<String, ApiError> {
        let iface = self.wifi_unavailable().map_err(failed)?;
        if !self.wifi_io().service_enabled(WIFI_SERVICE) {
            return Err(failed("Wi-Fi is off. Turn it on first."));
        }
        if !self.wait_for_supplicant(&iface, Duration::from_secs(3)) {
            return Err(failed("wpa_supplicant does not answer yet. Try again in a moment."));
        }
        Ok(iface)
    }

    fn scan_results(&self, iface: &str) -> Vec<nodo_api::WifiNetwork> {
        self.wifi_io().request(iface, "SCAN_RESULTS").map(|r| wpa::parse_scan_results(&r)).unwrap_or_default()
    }

    pub(crate) fn scan_wifi(&self) -> Result<Response, ApiError> {
        let iface = self.running_interface()?;
        let io = self.wifi_io();
        let mut events = io.attach(&iface).map_err(failed)?;
        let reply = io.request(&iface, "SCAN").map_err(failed)?;
        // FAIL-BUSY means a scan is already running, whose results serve as well.
        if reply.trim() != "OK" && reply.trim() != "FAIL-BUSY" {
            return Err(failed(format!("the scan did not start: {}", reply.trim())));
        }
        let start = Instant::now();
        while start.elapsed() < SCAN_TIMEOUT {
            match events.next(SCAN_TIMEOUT.saturating_sub(start.elapsed())) {
                Some(e) if e.contains("CTRL-EVENT-SCAN-RESULTS") => break,
                Some(e) if e.contains("CTRL-EVENT-SCAN-FAILED") => return Err(failed("the scan failed")),
                Some(_) => {}
                None => break,
            }
        }
        let store = self.load_wifi_store()?;
        let joined = self.wifi_status()?.ssid;
        let mut list = self.scan_results(&iface);
        for network in &mut list {
            network.saved = store.networks.iter().any(|n| n.ssid == network.ssid);
            network.connected = joined.as_deref() == Some(network.ssid.as_str());
        }
        Ok(Response::WifiNetworks(list))
    }

    pub(crate) fn join_wifi(&self, join: &WifiJoin) -> Result<Response, ApiError> {
        if let Some(problem) = wpa::ssid_problem(&join.ssid) {
            return Err(failed(problem));
        }
        let _guard = lock(&self.radios.wifi_lock);
        let iface = self.running_interface()?;
        let io = self.wifi_io();
        // The security this device sees wins over what the request says, so a request cannot
        // join a WPA3 network with WPA2, or send a passphrase to a network that turned open.
        let security = self
            .scan_results(&iface)
            .into_iter()
            .find(|n| n.ssid == join.ssid)
            .map_or(join.security, |n| n.security);
        let stored = StoredSecurity::from_api(security).ok_or_else(|| {
            failed(match security {
                WifiSecurity::Enterprise => format!("{} needs a per-user login (WPA Enterprise), which this device does not support.", join.ssid),
                _ => format!("{} uses WEP or WPA, which are broken. Change the router to WPA2 or WPA3.", join.ssid),
            })
        })?;
        let passphrase = join.passphrase.as_ref().map(|p| p.expose()).filter(|p| !p.is_empty());
        if security == WifiSecurity::Open {
            if passphrase.is_some() {
                return Err(failed(format!("{} is an open network now and takes no passphrase. Scan again.", join.ssid)));
            }
            if !join.accept_open {
                return Err(ApiError::NeedsConfirmation(format!(
                    "{} is an open network: anyone nearby can read what the device sends over it. Confirm to join anyway.",
                    join.ssid
                )));
            }
        } else {
            let passphrase = passphrase.ok_or_else(|| failed(format!("{} needs its passphrase.", join.ssid)))?;
            if let Some(problem) = wpa::passphrase_problem(passphrase) {
                return Err(failed(problem));
            }
        }
        let mut store = self.load_wifi_store()?;
        let mut network = NetworkConfig {
            ssid: join.ssid.clone(),
            security,
            credential: wpa::credential(security, &join.ssid, passphrase),
            priority: store.networks.len() as u32 + 1,
        };
        self.try_network(&iface, &network)?;
        // Accepted. The network joined last is tried first from now on.
        store.networks.retain(|n| n.ssid != join.ssid);
        let secret = match &network.credential {
            Credential::Open => String::new(),
            Credential::Psk(s) | Credential::Passphrase(s) => s.as_str().to_owned(),
        };
        store.networks.push(StoredNetwork { ssid: join.ssid.clone(), security: stored, secret, added_at: io.now() });
        network.priority = store.networks.len() as u32;
        self.save_wifi_store(&store)?;
        // The running wpa_supplicant already holds the network. Bring the others back for when it
        // is out of range, without dropping the connection.
        let _ = io.request(&iface, "ENABLE_NETWORK all");
        let start = Instant::now();
        loop {
            let status = self.wifi_status()?;
            if status.address.is_some() || start.elapsed() >= ADDRESS_TIMEOUT {
                return Ok(Response::Wifi(status));
            }
            io.sleep(Duration::from_millis(500));
        }
    }

    /// Adds the network to the running wpa_supplicant, selects it and waits. Removes it again
    /// unless it connected.
    fn try_network(&self, iface: &str, network: &NetworkConfig) -> Result<(), ApiError> {
        let io = self.wifi_io();
        let mut events = io.attach(iface).map_err(failed)?;
        let id = io.request(iface, "ADD_NETWORK").map_err(failed)?.trim().to_owned();
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(failed("wpa_supplicant did not add the network"));
        }
        let mut trial = Trial { io, iface, id: Some(id.clone()) };
        for (key, value) in wpa::network_fields(network) {
            let command = Zeroizing::new(format!("SET_NETWORK {id} {key} {}", value.as_str()));
            let reply = io.request(iface, &command).map_err(failed)?;
            if reply.trim() != "OK" {
                return Err(failed(format!("wpa_supplicant refused the network's {key}")));
            }
        }
        io.request(iface, &format!("SELECT_NETWORK {id}")).map_err(failed)?;
        let start = Instant::now();
        let mut not_found = false;
        let outcome = loop {
            let left = JOIN_TIMEOUT.saturating_sub(start.elapsed());
            if left.is_zero() {
                break None;
            }
            let Some(event) = events.next(left) else { break None };
            match wpa::join_event(&event, &id) {
                JoinEvent::Connected => break Some(Ok(())),
                JoinEvent::WrongKey => {
                    break Some(Err(failed(format!("{} refused the passphrase. Nothing was saved.", network.ssid))))
                }
                JoinEvent::Refused(reason) => {
                    break Some(Err(failed(format!(
                        "{} did not let the device join ({reason}). Nothing was saved.",
                        network.ssid
                    ))))
                }
                JoinEvent::NotFound => not_found = true,
                JoinEvent::Other => {}
            }
        };
        match outcome {
            Some(Ok(())) => {
                trial.id = None;
                Ok(())
            }
            Some(Err(e)) => Err(e),
            None if not_found => Err(failed(format!("{} is not in range. Nothing was saved.", network.ssid))),
            None => Err(failed(format!("Joining {} took too long. Nothing was saved.", network.ssid))),
        }
    }

    pub(crate) fn forget_wifi(&self, ssid: &str) -> Result<Response, ApiError> {
        let _guard = lock(&self.radios.wifi_lock);
        let mut store = self.load_wifi_store()?;
        let before = store.networks.len();
        store.networks.retain(|n| n.ssid != ssid);
        if store.networks.len() == before {
            return Err(ApiError::NotFound(ssid.to_owned()));
        }
        self.save_wifi_store(&store)?;
        if let Ok(iface) = self.wifi_unavailable() {
            if self.wifi_io().service_enabled(WIFI_SERVICE) {
                // Reading the file again drops the network, and the connection to it.
                let _ = self.wifi_io().request(&iface, "RECONFIGURE");
            }
        }
        Ok(Response::Wifi(self.wifi_status()?))
    }

    /// At start: rfkill state does not survive a reboot, so a radio the owner turned off is
    /// blocked again, and the file wpa_supplicant reads is brought up to date.
    pub fn apply_wifi_state(&self) -> Result<(), ApiError> {
        let io = self.wifi_io();
        if self.wifi_interface().is_none() {
            return Ok(());
        }
        if io.service_enabled(WIFI_SERVICE) {
            self.write_wifi_config(&self.load_wifi_store()?)
        } else {
            io.set_blocked(true).map_err(failed)
        }
    }
}

/// The machine's side: sysfs, runit and the control socket.
pub struct RealWifiIo {
    pub sv_dir: PathBuf,
    pub service_dir: PathBuf,
    pub ctrl_dir: PathBuf,
    /// Where the helper binds its end of the control socket. Root-only.
    pub client_dir: PathBuf,
}

impl Default for RealWifiIo {
    fn default() -> Self {
        RealWifiIo {
            sv_dir: "/etc/sv".into(),
            service_dir: "/var/service".into(),
            ctrl_dir: WifiPaths::default().ctrl_dir,
            client_dir: "/run/nodo/wpa-client".into(),
        }
    }
}

const CTRL_TIMEOUT: Duration = Duration::from_secs(10);

/// One end of a control socket, bound to its own path and removed when dropped.
struct CtrlSocket {
    socket: std::os::unix::net::UnixDatagram,
    path: PathBuf,
}

impl CtrlSocket {
    fn open(io: &RealWifiIo, iface: &str) -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        if iface.contains('/') || iface.starts_with('.') {
            return Err("bad interface name".to_owned());
        }
        crate::radio::private_dir(&io.client_dir)?;
        let path = io.client_dir.join(format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_file(&path);
        let socket = std::os::unix::net::UnixDatagram::bind(&path).map_err(|e| format!("control socket: {e}"))?;
        let ctrl = CtrlSocket { socket, path };
        ctrl.socket
            .connect(io.ctrl_dir.join(iface))
            .map_err(|e| format!("wpa_supplicant is not running on {iface}: {e}"))?;
        ctrl.socket.set_read_timeout(Some(CTRL_TIMEOUT)).map_err(|e| e.to_string())?;
        Ok(ctrl)
    }

    fn send(&self, command: &str) -> Result<(), String> {
        self.socket.send(command.as_bytes()).map(|_| ()).map_err(|e| format!("control socket: {e}"))
    }

    fn recv(&self) -> Result<String, String> {
        let mut buffer = Zeroizing::new(vec![0u8; 16 * 1024]);
        let n = self.socket.recv(&mut buffer).map_err(|e| format!("wpa_supplicant did not answer: {e}"))?;
        Ok(String::from_utf8_lossy(&buffer[..n]).into_owned())
    }
}

impl Drop for CtrlSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct RealEvents(CtrlSocket);

impl WifiEvents for RealEvents {
    fn next(&mut self, timeout: Duration) -> Option<String> {
        let timeout = timeout.max(Duration::from_millis(10));
        self.0.socket.set_read_timeout(Some(timeout)).ok()?;
        self.0.recv().ok()
    }
}

impl Drop for RealEvents {
    fn drop(&mut self) {
        let _ = self.0.send("DETACH");
    }
}

impl WifiIo for RealWifiIo {
    fn interfaces(&self) -> Vec<String> {
        let mut list: Vec<String> = std::fs::read_dir("/sys/class/net")
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().join("phy80211").exists())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        list.sort();
        list
    }

    fn supplicant_installed(&self) -> bool {
        std::path::Path::new("/usr/bin/wpa_supplicant").exists()
    }

    fn wired_up(&self) -> bool {
        crate::radio::wired_up()
    }

    fn set_blocked(&self, blocked: bool) -> Result<(), String> {
        crate::radio::rfkill("wlan", blocked)
    }

    fn service_installed(&self, name: &str) -> bool {
        crate::radio::service_installed(&self.sv_dir, name)
    }

    fn service_enabled(&self, name: &str) -> bool {
        crate::radio::service_enabled(&self.sv_dir, &self.service_dir, name)
    }

    fn service(&self, name: &str, up: bool) -> Result<(), String> {
        crate::radio::service(&self.sv_dir, &self.service_dir, name, up)
    }

    fn request(&self, iface: &str, command: &str) -> Result<String, String> {
        let ctrl = CtrlSocket::open(self, iface)?;
        ctrl.send(command)?;
        ctrl.recv()
    }

    fn attach(&self, iface: &str) -> Result<Box<dyn WifiEvents>, String> {
        let ctrl = CtrlSocket::open(self, iface)?;
        ctrl.send("ATTACH")?;
        match ctrl.recv()?.trim() {
            "OK" => Ok(Box::new(RealEvents(ctrl))),
            other => Err(format!("wpa_supplicant refused to send events: {other}")),
        }
    }

    fn write_private(&self, path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
        crate::radio::write_private(path, bytes)
    }

    fn read(&self, path: &std::path::Path) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn now(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }
}
