//! What Wi-Fi and Bluetooth share: the helper's handles on them, rfkill, runit services and
//! root-only files.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nodo_api::BluetoothPairing;

use crate::bluetooth::{BluetoothIo, RealBluetoothIo};
use crate::wifi::{RealWifiIo, WifiIo, WifiPaths};

pub struct Radios {
    pub wifi: Box<dyn WifiIo>,
    pub wifi_paths: WifiPaths,
    pub bluetooth: Arc<dyn BluetoothIo>,
    /// The pairing the owner started last. Pairing runs on its own thread, because it waits
    /// for the owner to type a passkey on the device.
    pub(crate) pairing: Arc<Mutex<Option<BluetoothPairing>>>,
    /// One Wi-Fi change at a time.
    pub(crate) wifi_lock: Mutex<()>,
    /// One Bluetooth change at a time, pairing included.
    pub(crate) bluetooth_lock: Arc<Mutex<()>>,
    /// Pairs on the calling thread, for tests.
    pub pair_inline: bool,
}

impl Default for Radios {
    fn default() -> Self {
        Radios::new(Box::new(RealWifiIo::default()), Arc::new(RealBluetoothIo::default()))
    }
}

impl Radios {
    pub fn new(wifi: Box<dyn WifiIo>, bluetooth: Arc<dyn BluetoothIo>) -> Self {
        Radios {
            wifi,
            wifi_paths: WifiPaths::default(),
            bluetooth,
            pairing: Arc::new(Mutex::new(None)),
            wifi_lock: Mutex::new(()),
            bluetooth_lock: Arc::new(Mutex::new(())),
            pair_inline: false,
        }
    }
}

impl<S: crate::system::System> crate::helper::Helper<S> {
    pub fn with_radios(mut self, radios: Radios) -> Self {
        self.radios = radios;
        self
    }

    /// At start, blocks the radios the owner left off, since rfkill state does not survive a
    /// reboot. Both are off on a new device.
    pub fn apply_radio_state(&self) {
        if let Err(e) = self.apply_wifi_state() {
            eprintln!("nodo-helper: Wi-Fi at start: {e}");
        }
        if let Err(e) = self.apply_bluetooth_state() {
            eprintln!("nodo-helper: Bluetooth at start: {e}");
        }
    }
}

/// Soft-blocks or unblocks every radio of an rfkill type (`wlan`, `bluetooth`). A device without
/// any has nothing to do.
pub fn rfkill(kind: &str, blocked: bool) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir("/sys/class/rfkill") else { return Ok(()) };
    for entry in entries.filter_map(|e| e.ok()) {
        let dir = entry.path();
        if std::fs::read_to_string(dir.join("type")).is_ok_and(|t| t.trim() == kind) {
            std::fs::write(dir.join("soft"), if blocked { "1" } else { "0" })
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
    }
    Ok(())
}

/// A physical wired interface has a link: one with a device, no radio, and carrier.
pub fn wired_up() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else { return false };
    entries.filter_map(|e| e.ok()).map(|e| e.path()).any(|dir| {
        dir.join("device").exists()
            && !dir.join("phy80211").exists()
            && !dir.join("wireless").exists()
            && std::fs::read_to_string(dir.join("carrier")).is_ok_and(|c| c.trim() == "1")
    })
}

pub fn service_installed(sv_dir: &Path, name: &str) -> bool {
    sv_dir.join(name).join("run").exists()
}

/// Linked into runit's directory and not held down by a `down` file.
pub fn service_enabled(sv_dir: &Path, service_dir: &Path, name: &str) -> bool {
    service_dir.join(name).exists() && !sv_dir.join(name).join("down").exists()
}

/// Enables or disables a runit service. A `down` file keeps a disabled one down across reboots;
/// the link stays, because runsv can linger after its link goes and then ignore the next start.
pub fn service(sv_dir: &Path, service_dir: &Path, name: &str, up: bool) -> Result<(), String> {
    let dir = sv_dir.join(name);
    let link = service_dir.join(name);
    let down = dir.join("down");
    let sv = |action: &str| {
        let output = std::process::Command::new("sv")
            .args(["-w", "30", action, &link.to_string_lossy()])
            .output()
            .map_err(|e| format!("sv: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stdout).lines().next().unwrap_or("sv failed").to_owned())
        }
    };
    if up {
        match std::fs::remove_file(&down) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
            _ => {}
        }
        if !link.exists() {
            std::os::unix::fs::symlink(&dir, &link).map_err(|e| e.to_string())?;
        }
        // runsvdir notices a new link within five seconds.
        let mut last = String::new();
        for _ in 0..10 {
            match sv("up") {
                Ok(()) => return Ok(()),
                Err(e) => last = e,
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        Err(last)
    } else {
        if !link.exists() {
            return Ok(());
        }
        std::fs::write(&down, b"").map_err(|e| e.to_string())?;
        sv("down")
    }
}

/// A directory only root can enter.
pub fn private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}

/// Writes a root-only file under a temporary name, syncs it and renames it into place.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().ok_or("no parent directory")?;
    private_dir(dir)?;
    let tmp: PathBuf = dir.join(format!(".{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}
