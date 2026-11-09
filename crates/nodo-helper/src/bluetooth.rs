//! Bluetooth through BlueZ, for the TUI's Network screen.
//!
//! The helper runs `bluetoothctl`, which talks to bluetoothd over the system D-Bus, with fixed
//! arguments and an address it checked. Bluetooth is off until the owner turns it on: bluetoothd
//! is not linked and the controller is soft-blocked with rfkill. Turning it on unblocks the
//! controller, links D-Bus and bluetoothd, powers the controller and switches it to not pairable
//! and not discoverable, so nothing nearby can start a pairing. Only a pairing the owner starts
//! from the TUI runs; it trusts and connects the device when it succeeds. Off powers the
//! controller down, takes bluetoothd down with a `down` file and blocks the controller again.
//! D-Bus stays, since other services may use it.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nodo_api::{ApiError, BluetoothDevice, BluetoothPairing, BluetoothStatus, PairingStage, Response};

use crate::helper::{lock, Helper};

pub const BLUETOOTH_SERVICE: &str = "bluetoothd";
pub const DBUS_SERVICE: &str = "dbus";
const SCAN_SECONDS: u32 = 8;
const MAX_DEVICES: usize = 32;
const START_TIMEOUT: Duration = Duration::from_secs(15);

/// What the helper needs from the machine for Bluetooth, so the rules are tested without one.
pub trait BluetoothIo: Send + Sync {
    /// The kernel has a Bluetooth controller.
    fn controller_present(&self) -> bool;
    /// bluetoothctl is installed.
    fn installed(&self) -> bool;
    fn set_blocked(&self, blocked: bool) -> Result<(), String>;
    fn service_installed(&self, name: &str) -> bool;
    fn service_enabled(&self, name: &str) -> bool;
    fn service(&self, name: &str, up: bool) -> Result<(), String>;
    /// Runs bluetoothctl with these arguments and returns what it printed.
    fn ctl(&self, args: &[&str], timeout: Duration) -> Result<String, String>;
    /// Pairs with the device, then trusts and connects it, reporting what the owner must do.
    fn pair(&self, address: &str, report: &mut dyn FnMut(PairingStage)) -> Result<(), String>;
    fn sleep(&self, duration: Duration);
}

/// `AA:BB:CC:DD:EE:FF`, in either case.
pub fn valid_address(address: &str) -> bool {
    address.len() == 17
        && address.split(':').count() == 6
        && address.split(':').all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Removes terminal colour codes and the prompts bluetoothctl prints.
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            }
            '\u{1}' | '\u{2}' | '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// `show`: the controller's address and whether it is powered. `None` without a controller.
pub fn parse_show(text: &str) -> Option<(String, bool)> {
    let text = plain(text);
    // The first line may carry bluetoothctl's prompt before it. Change notices (`[CHG]`) name the
    // controller too, and are skipped.
    let address = text
        .lines()
        .filter(|l| !l.contains("[CHG]") && !l.contains("[NEW]") && !l.contains("[DEL]"))
        .find_map(|l| l.split("Controller ").nth(1)?.split_whitespace().next().filter(|a| valid_address(a)))?;
    let powered = text.lines().any(|l| l.trim() == "Powered: yes");
    Some((address.to_uppercase(), powered))
}

/// `devices`: `Device AA:BB:CC:DD:EE:FF Name` per line.
pub fn parse_devices(text: &str) -> Vec<(String, String)> {
    let mut seen = std::collections::BTreeSet::new();
    plain(text)
        .lines()
        .filter(|l| !l.contains("[CHG]") && !l.contains("[DEL]"))
        .filter_map(|l| l.split_once("Device ").map(|(_, rest)| rest))
        .filter_map(|rest| {
            let (address, name) = rest.split_once(' ').unwrap_or((rest, ""));
            valid_address(address).then(|| (address.to_uppercase(), name.trim().to_owned()))
        })
        .filter(|(address, _)| seen.insert(address.clone()))
        .collect()
}

/// `info ADDRESS`: the device's name, icon and state.
pub fn parse_info(address: &str, fallback_name: &str, text: &str) -> BluetoothDevice {
    let text = plain(text);
    let field = |name: &str| {
        text.lines().find_map(|l| l.trim().strip_prefix(name).and_then(|v| v.strip_prefix(':')).map(|v| v.trim().to_owned()))
    };
    let yes = |name: &str| field(name).is_some_and(|v| v == "yes");
    // Newer BlueZ prints `RSSI: 0xffffffc4 (-60)`, older `RSSI: -60`.
    let rssi = field("RSSI").and_then(|v| {
        let number = v.rsplit_once('(').map_or(v.as_str(), |(_, r)| r.trim_end_matches(')')).trim().to_owned();
        number.parse::<i16>().ok()
    });
    let name = field("Name")
        .or_else(|| field("Alias"))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| if fallback_name.is_empty() { address.to_owned() } else { fallback_name.to_owned() });
    BluetoothDevice {
        address: address.to_owned(),
        name: name.chars().filter(|c| !c.is_control()).take(64).collect(),
        kind: field("Icon").filter(|k| !k.is_empty()),
        paired: yes("Paired"),
        trusted: yes("Trusted"),
        connected: yes("Connected"),
        rssi,
    }
}

fn failed(text: impl Into<String>) -> ApiError {
    ApiError::Failed(text.into())
}

fn running(stage: &PairingStage) -> bool {
    matches!(stage, PairingStage::Pairing | PairingStage::TypePasskey(_) | PairingStage::ComparePasskey(_))
}

impl<S: crate::system::System> Helper<S> {
    fn bt(&self) -> &dyn crate::bluetooth::BluetoothIo {
        self.radios.bluetooth.as_ref()
    }

    fn bluetooth_unavailable(&self) -> Option<String> {
        let io = self.bt();
        if !io.controller_present() {
            return Some("No Bluetooth controller was found.".to_owned());
        }
        if !io.installed() || !io.service_installed(BLUETOOTH_SERVICE) {
            return Some("BlueZ is not installed, so Bluetooth cannot be used.".to_owned());
        }
        if !io.service_installed(DBUS_SERVICE) {
            return Some("D-Bus is not installed, which BlueZ needs.".to_owned());
        }
        None
    }

    pub(crate) fn bluetooth_status(&self) -> Result<BluetoothStatus, ApiError> {
        let io = self.bt();
        let mut status = BluetoothStatus {
            unavailable: self.bluetooth_unavailable(),
            enabled: io.service_enabled(BLUETOOTH_SERVICE),
            powered: false,
            controller: None,
            devices: Vec::new(),
            pairing: lock(&self.radios.pairing).clone(),
        };
        if status.unavailable.is_some() || !status.enabled {
            return Ok(status);
        }
        let short = Duration::from_secs(5);
        if let Some((address, powered)) = io.ctl(&["show"], short).ok().and_then(|t| parse_show(&t)) {
            status.controller = Some(address);
            status.powered = powered;
        }
        let listed = io.ctl(&["devices"], short).map(|t| parse_devices(&t)).unwrap_or_default();
        for (address, name) in listed.into_iter().take(MAX_DEVICES) {
            let info = io.ctl(&["info", &address], short).unwrap_or_default();
            status.devices.push(parse_info(&address, &name, &info));
        }
        status.devices.sort_by(|a, b| {
            b.paired.cmp(&a.paired).then(b.rssi.unwrap_or(i16::MIN).cmp(&a.rssi.unwrap_or(i16::MIN))).then(a.name.cmp(&b.name))
        });
        Ok(status)
    }

    pub(crate) fn set_bluetooth_enabled(&self, enabled: bool) -> Result<Response, ApiError> {
        let _guard = lock(&self.radios.bluetooth_lock);
        let io = self.bt();
        let short = Duration::from_secs(10);
        if enabled {
            if let Some(why) = self.bluetooth_unavailable() {
                return Err(failed(why));
            }
            io.set_blocked(false).map_err(|e| failed(format!("could not unblock Bluetooth: {e}")))?;
            io.service(DBUS_SERVICE, true).map_err(|e| failed(format!("could not start D-Bus: {e}")))?;
            io.service(BLUETOOTH_SERVICE, true).map_err(|e| failed(format!("could not start bluetoothd: {e}")))?;
            let start = Instant::now();
            while io.ctl(&["show"], short).ok().and_then(|t| parse_show(&t)).is_none() {
                if start.elapsed() >= START_TIMEOUT {
                    return Err(failed("bluetoothd started but does not see the controller. Its log says why."));
                }
                io.sleep(Duration::from_millis(500));
            }
            io.ctl(&["power", "on"], short).map_err(|e| failed(format!("could not power Bluetooth on: {e}")))?;
            // Nothing nearby may start a pairing or find this device. Pairings the owner starts
            // still work.
            io.ctl(&["pairable", "off"], short).map_err(failed)?;
            io.ctl(&["discoverable", "off"], short).map_err(failed)?;
        } else {
            if io.service_enabled(BLUETOOTH_SERVICE) {
                let _ = io.ctl(&["power", "off"], short);
                io.service(BLUETOOTH_SERVICE, false).map_err(|e| failed(format!("could not stop bluetoothd: {e}")))?;
            }
            io.set_blocked(true).map_err(|e| failed(format!("could not block Bluetooth: {e}")))?;
        }
        Ok(Response::Bluetooth(self.bluetooth_status()?))
    }

    fn bluetooth_ready(&self) -> Result<(), ApiError> {
        if let Some(why) = self.bluetooth_unavailable() {
            return Err(failed(why));
        }
        if !self.bt().service_enabled(BLUETOOTH_SERVICE) {
            return Err(failed("Bluetooth is off. Turn it on first."));
        }
        Ok(())
    }

    pub(crate) fn scan_bluetooth(&self) -> Result<Response, ApiError> {
        self.bluetooth_ready()?;
        {
            let _guard = lock(&self.radios.bluetooth_lock);
            let timeout = SCAN_SECONDS.to_string();
            // The scan runs until the timeout, then bluetoothctl exits.
            self.bt()
                .ctl(&["--timeout", &timeout, "scan", "on"], Duration::from_secs(u64::from(SCAN_SECONDS) + 10))
                .map_err(|e| failed(format!("the scan failed: {e}")))?;
        }
        Ok(Response::Bluetooth(self.bluetooth_status()?))
    }

    fn checked_address(address: &str) -> Result<String, ApiError> {
        if valid_address(address) {
            Ok(address.to_uppercase())
        } else {
            Err(failed("That is not a Bluetooth address."))
        }
    }

    /// Starts pairing on its own thread and answers at once. The thread holds the Bluetooth lock,
    /// so nothing else changes Bluetooth while the owner types a passkey.
    pub(crate) fn pair_bluetooth(&self, address: &str) -> Result<Response, ApiError> {
        let address = Self::checked_address(address)?;
        self.bluetooth_ready()?;
        {
            let mut pairing = lock(&self.radios.pairing);
            if pairing.as_ref().is_some_and(|p| running(&p.stage)) {
                return Err(failed("A pairing is already running. Wait for it to finish."));
            }
            *pairing = Some(BluetoothPairing { address: address.clone(), stage: PairingStage::Pairing });
        }
        let io = self.radios.bluetooth.clone();
        let state = self.radios.pairing.clone();
        let busy = self.radios.bluetooth_lock.clone();
        let set = move |stage: PairingStage| {
            let mut pairing = lock(&state);
            if let Some(p) = pairing.as_mut() {
                p.stage = stage;
            }
        };
        let worker = move || {
            let _guard = lock(&busy);
            let mut report = |stage: PairingStage| set(stage);
            let result = io.pair(&address, &mut report);
            report(match result {
                Ok(()) => PairingStage::Done,
                Err(e) => PairingStage::Failed(e),
            });
        };
        if self.radios.pair_inline {
            worker();
        } else {
            std::thread::spawn(worker);
        }
        Ok(Response::Bluetooth(self.bluetooth_status()?))
    }

    pub(crate) fn connect_bluetooth(&self, address: &str, connect: bool) -> Result<Response, ApiError> {
        let address = Self::checked_address(address)?;
        self.bluetooth_ready()?;
        {
            let _guard = lock(&self.radios.bluetooth_lock);
            let verb = if connect { "connect" } else { "disconnect" };
            let output = self.bt().ctl(&[verb, &address], Duration::from_secs(30)).map_err(failed)?;
            let text = plain(&output);
            if connect && !text.contains("Connection successful") {
                let reason = text.lines().find(|l| l.contains("Failed")).unwrap_or("no answer").trim().to_owned();
                return Err(failed(format!("Could not connect: {reason}")));
            }
        }
        Ok(Response::Bluetooth(self.bluetooth_status()?))
    }

    pub(crate) fn remove_bluetooth(&self, address: &str) -> Result<Response, ApiError> {
        let address = Self::checked_address(address)?;
        self.bluetooth_ready()?;
        {
            let _guard = lock(&self.radios.bluetooth_lock);
            let output = self.bt().ctl(&["remove", &address], Duration::from_secs(15)).map_err(failed)?;
            if plain(&output).contains("not available") {
                return Err(ApiError::NotFound(address));
            }
            let mut pairing = lock(&self.radios.pairing);
            if pairing.as_ref().is_some_and(|p| p.address == address && !running(&p.stage)) {
                *pairing = None;
            }
        }
        Ok(Response::Bluetooth(self.bluetooth_status()?))
    }

    pub fn apply_bluetooth_state(&self) -> Result<(), ApiError> {
        let io = self.bt();
        if io.controller_present() && !io.service_enabled(BLUETOOTH_SERVICE) {
            io.set_blocked(true).map_err(failed)?;
        }
        Ok(())
    }
}

/// The machine's side: sysfs, runit and bluetoothctl.
pub struct RealBluetoothIo {
    pub sv_dir: PathBuf,
    pub service_dir: PathBuf,
}

impl Default for RealBluetoothIo {
    fn default() -> Self {
        RealBluetoothIo { sv_dir: "/etc/sv".into(), service_dir: "/var/service".into() }
    }
}

const BLUETOOTHCTL: &str = "/usr/bin/bluetoothctl";
const PAIR_TIMEOUT: Duration = Duration::from_secs(90);

/// Reads a child's output on a thread, so the caller can wait with a deadline.
fn reader(mut out: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        while let Ok(n) = out.read(&mut buffer) {
            if n == 0 || tx.send(String::from_utf8_lossy(&buffer[..n]).into_owned()).is_err() {
                break;
            }
        }
    });
    rx
}

/// A digit run right after `marker`, such as the passkey in `Passkey: 123456`.
fn number_after(text: &str, marker: &str) -> Option<String> {
    let rest = &text[text.rfind(marker)? + marker.len()..];
    let digits: String = rest.trim_start().chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty()).then_some(digits)
}

impl BluetoothIo for RealBluetoothIo {
    fn controller_present(&self) -> bool {
        std::fs::read_dir("/sys/class/bluetooth")
            .map(|entries| entries.filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().starts_with("hci")))
            .unwrap_or(false)
    }

    fn installed(&self) -> bool {
        std::path::Path::new(BLUETOOTHCTL).exists()
    }

    fn set_blocked(&self, blocked: bool) -> Result<(), String> {
        crate::radio::rfkill("bluetooth", blocked)
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

    fn ctl(&self, args: &[&str], timeout: Duration) -> Result<String, String> {
        let mut child = Command::new(BLUETOOTHCTL)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("bluetoothctl: {e}"))?;
        let rx = reader(child.stdout.take().ok_or("no output")?);
        let start = Instant::now();
        let mut text = String::new();
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(chunk) => text.push_str(&chunk),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if start.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err("bluetoothctl did not finish in time".to_owned());
            }
        }
        let _ = child.wait();
        Ok(text)
    }

    /// Drives an interactive bluetoothctl with a KeyboardDisplay agent: a passkey to type on a
    /// keyboard is reported, a number to compare is reported and accepted (the owner asked for
    /// this pairing), a legacy PIN is answered with 0000, and a passkey the Nodo would have to
    /// type is refused, since it has no keyboard of its own here.
    fn pair(&self, address: &str, report: &mut dyn FnMut(PairingStage)) -> Result<(), String> {
        if !valid_address(address) {
            return Err("bad address".to_owned());
        }
        let mut child = Command::new(BLUETOOTHCTL)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("bluetoothctl: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("no input")?;
        let rx = reader(child.stdout.take().ok_or("no output")?);
        let mut say = |line: &str| stdin.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string());
        let result = (|| {
            say("agent KeyboardDisplay")?;
            say("default-agent")?;
            say(&format!("pair {address}"))?;
            let start = Instant::now();
            let mut seen = String::new();
            let mut answered = 0usize;
            loop {
                if start.elapsed() >= PAIR_TIMEOUT {
                    return Err("The device did not finish pairing in time.".to_owned());
                }
                let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) else { continue };
                seen.push_str(&plain(&chunk));
                let fresh = &seen[answered..];
                if fresh.contains("Pairing successful") {
                    break;
                }
                if let Some(at) = fresh.find("Failed to pair") {
                    let reason = fresh[at..].lines().next().unwrap_or("").trim().to_owned();
                    return Err(reason);
                }
                if fresh.contains("Request passkey") {
                    return Err("The device asks for a passkey to be typed on the Nodo, which is not supported. Try the device's other pairing mode.".to_owned());
                }
                if fresh.contains("Request PIN code") {
                    say("0000")?;
                    answered = seen.len();
                } else if let Some(n) = number_after(fresh, "Confirm passkey") {
                    report(PairingStage::ComparePasskey(n));
                    say("yes")?;
                    answered = seen.len();
                } else if let Some(n) = number_after(fresh, "Passkey:") {
                    report(PairingStage::TypePasskey(n));
                    answered = seen.len();
                }
            }
            say(&format!("trust {address}"))?;
            say(&format!("connect {address}"))?;
            let start = Instant::now();
            let mut seen = String::new();
            while start.elapsed() < Duration::from_secs(20) {
                if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
                    seen.push_str(&plain(&chunk));
                }
                // A device that is paired and trusted connects by itself later, so a failed
                // first connection is not a failed pairing.
                if seen.contains("Connection successful") || seen.contains("Failed to connect") {
                    break;
                }
            }
            Ok(())
        })();
        let _ = say("quit");
        drop(stdin);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            if child.try_wait().ok().flatten().is_some() {
                return result;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        result
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_addresses() {
        assert!(valid_address("AA:BB:CC:DD:EE:FF"));
        assert!(valid_address("aa:bb:cc:dd:ee:0f"));
        assert!(!valid_address("AA:BB:CC:DD:EE"));
        assert!(!valid_address("AA:BB:CC:DD:EE:FG"));
        assert!(!valid_address("AA:BB:CC:DD:EE:FF; rm"));
        assert!(!valid_address("--help"));
    }

    #[test]
    fn parses_bluetoothctl_output() {
        let show = "\u{1b}[0;94m[bluetooth]\u{1b}[0m# Controller 00:1A:7D:DA:71:13 (public)\n\tName: nodo\n\tPowered: yes\n\tDiscoverable: no\n";
        assert_eq!(parse_show(show), Some(("00:1A:7D:DA:71:13".to_owned(), true)));
        assert_eq!(parse_show("No default controller available\n"), None);
        let devices = "Device AA:BB:CC:DD:EE:FF Logitech K380\nDevice 11:22:33:44:55:66 11-22-33-44-55-66\n[CHG] Controller x\nDevice bad name\n";
        assert_eq!(
            parse_devices(devices),
            vec![
                ("AA:BB:CC:DD:EE:FF".to_owned(), "Logitech K380".to_owned()),
                ("11:22:33:44:55:66".to_owned(), "11-22-33-44-55-66".to_owned())
            ]
        );
        let info = "Device AA:BB:CC:DD:EE:FF (random)\n\tName: Logitech K380\n\tAlias: Logitech K380\n\tIcon: input-keyboard\n\tPaired: yes\n\tTrusted: yes\n\tConnected: no\n\tRSSI: 0xffffffc4 (-60)\n";
        let device = parse_info("AA:BB:CC:DD:EE:FF", "", info);
        assert_eq!(device.name, "Logitech K380");
        assert_eq!(device.kind.as_deref(), Some("input-keyboard"));
        assert!(device.paired && device.trusted && !device.connected);
        assert_eq!(device.rssi, Some(-60));
        let bare = parse_info("11:22:33:44:55:66", "", "Device 11:22:33:44:55:66\n\tRSSI: -71\n");
        assert_eq!((bare.name.as_str(), bare.rssi, bare.paired), ("11:22:33:44:55:66", Some(-71), false));
    }

    #[test]
    fn finds_passkeys() {
        assert_eq!(number_after("[agent] Passkey: 123456\n", "Passkey:").as_deref(), Some("123456"));
        assert_eq!(number_after("[agent] Confirm passkey 004211 (yes/no): ", "Confirm passkey").as_deref(), Some("004211"));
        assert_eq!(number_after("Passkey: none", "Passkey:"), None);
    }
}
