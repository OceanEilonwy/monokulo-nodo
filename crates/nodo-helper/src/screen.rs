//! The touchscreen's own access: it may always look, and may change things only while the
//! owner has unlocked it with the screen PIN.
//!
//! nodo-gui runs unprivileged as its own account and connects on a socket of its own. Whoever
//! stands at the device can touch the screen, so the helper, not the screen, decides what it may
//! do:
//!
//! - Requests that only read are always answered.
//! - Everything else needs the screen unlocked. `ScreenUnlock` checks the PIN and unlocks for
//!   [`Timing::screen_idle`]; every allowed change restarts that clock, and `ScreenLock` ends it.
//! - The PIN is set or cleared only from the main socket (SSH or the TUI) with the device
//!   passphrase. Without a PIN the screen can only look.
//! - There is no attempt limit, by the owner's decision: only someone at the device can type on
//!   the screen. Each check still takes at least [`PIN_CHECK_DELAY`], so a compromised screen
//!   process cannot run through the million six-digit PINs quickly.
//!
//! The PIN is stored as an Argon2id hash with its own salt, in a root-only file.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::{Algorithm, Argon2, Params, Version};
use nodo_api::{ApiError, Request, Response, ScreenStatus, Secret};
use zeroize::Zeroizing;

use crate::helper::{lock, Helper};
use crate::system::{System, WriteOptions};

/// The shortest time one PIN check takes, right or wrong.
pub const PIN_CHECK_DELAY: Duration = Duration::from_secs(1);
const PIN_LEN: usize = 6;
const MAGIC: &[u8; 8] = b"NODOPIN1";
const SALT_LEN: usize = 16;
const HASH_LEN: usize = 32;

/// When the screen was last allowed a change, while it is unlocked.
#[derive(Default)]
pub struct ScreenGate {
    unlocked_since: Mutex<Option<Instant>>,
}

/// Requests the screen may always send: they only read, check or list.
pub fn is_read_only(request: &Request) -> bool {
    use Request::*;
    matches!(
        request,
        ListServices
            | DeviceOverview
            | ListPackages
            | PackageDetail { .. }
            | SettingsForm { .. }
            | ListFeeds
            | ListUpdates
            | GetAutoUpdate
            | LogSources
            | ReadLog { .. }
            | DriveStatus
            | CheckDrive
            | ListSites
            | ListProviders
            | ListSshKeys
            | NodeStatus
            | ListRoleBindings
            | ListGrants
            | ProbeFeed { .. }
            | RefreshFeeds
            | FeedKeyChange { .. }
            | Catalog
            | ReviewInstall { .. }
            | UpdateStatus
            | CheckUpdates
            | ReviewUpdate { .. }
            | Health
            | ListBackups
            | InspectBackup { .. }
            | WebStatus
            | ListDnsProviders
            | ListDomainProviders
            | AcmeSettings
            | CertificateStatus
            | LocalSiteStatus
            | ListUsbDrives
            | ListUsbBackups { .. }
            | InspectUsbBackup { .. }
            | SystemUpdateStatus
            | CheckSystemUpdate
            | UpdateStorageGrowth
            | OnionStatus
            | ListRollbacks
            | WifiStatus
            | ScanWifi
            | BluetoothStatus
            | ScanBluetooth
            | ScreenStatus
    )
}

fn pin_hash(pin: &[u8], salt: &[u8], memory_kib: u32) -> Result<Zeroizing<[u8; HASH_LEN]>, ApiError> {
    let params = Params::new(memory_kib, 3, 1, Some(HASH_LEN))
        .map_err(|e| ApiError::Failed(format!("PIN hashing: {e}")))?;
    let mut out = Zeroizing::new([0u8; HASH_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(pin, salt, out.as_mut())
        .map_err(|e| ApiError::Failed(format!("PIN hashing: {e}")))?;
    Ok(out)
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn valid_pin(pin: &Secret) -> Result<(), ApiError> {
    let text = pin.expose();
    if text.len() == PIN_LEN && text.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(ApiError::Failed(format!("the screen PIN is {PIN_LEN} digits")))
    }
}

impl<S: System> Helper<S> {
    /// Memory for the PIN hash: the device KDF's, capped, so a check stays near a second.
    fn pin_memory_kib(&self) -> u32 {
        self.kdf.memory_kib.min(64 * 1024)
    }

    fn pin_file(&self) -> Result<Option<Vec<u8>>, ApiError> {
        Ok(self.sys.read_file(&self.paths.screen_pin)?)
    }

    pub(crate) fn screen_status(&self) -> ScreenStatus {
        let pin_set = matches!(self.pin_file(), Ok(Some(_)));
        let mut since = lock(&self.screen.unlocked_since);
        if since.is_some_and(|t| t.elapsed() >= self.timing.screen_idle) || !pin_set {
            *since = None;
        }
        ScreenStatus { pin_set, unlocked: since.is_some() }
    }

    fn screen_unlock(&self, pin: &Secret) -> Result<Response, ApiError> {
        let started = Instant::now();
        let result = self.check_pin(pin);
        // The same floor whether the PIN was right, wrong or malformed.
        if let Some(rest) = PIN_CHECK_DELAY.checked_sub(started.elapsed()) {
            self.sys.sleep(rest);
        }
        result?;
        *lock(&self.screen.unlocked_since) = Some(Instant::now());
        Ok(Response::Screen(self.screen_status()))
    }

    fn check_pin(&self, pin: &Secret) -> Result<(), ApiError> {
        let Some(file) = self.pin_file()? else {
            return Err(ApiError::NotSetUp);
        };
        if valid_pin(pin).is_err() {
            return Err(ApiError::BadPassphrase);
        }
        let ok = file.len() == MAGIC.len() + SALT_LEN + HASH_LEN && file.starts_with(MAGIC) && {
            let salt = &file[MAGIC.len()..MAGIC.len() + SALT_LEN];
            let stored = &file[MAGIC.len() + SALT_LEN..];
            let hash = pin_hash(pin.expose().as_bytes(), salt, self.pin_memory_kib())?;
            same(hash.as_ref(), stored)
        };
        if ok {
            Ok(())
        } else {
            Err(ApiError::BadPassphrase)
        }
    }

    /// Sets or clears the PIN. Checks the device passphrase the way an unlock does.
    fn set_screen_pin(&self, passphrase: &Secret, pin: Option<&Secret>) -> Result<Response, ApiError> {
        {
            let mut failures = lock(&self.failed_unlocks);
            let file = self.read_keystore()?;
            self.verify_passphrase(&mut failures, passphrase, &file)?;
        }
        *lock(&self.screen.unlocked_since) = None;
        let Some(pin) = pin else {
            if self.sys.exists(&self.paths.screen_pin) {
                self.sys.remove_file(&self.paths.screen_pin)?;
            }
            return Ok(Response::Ok);
        };
        valid_pin(pin)?;
        let mut salt = [0u8; SALT_LEN];
        self.sys.random(&mut salt)?;
        let hash = pin_hash(pin.expose().as_bytes(), &salt, self.pin_memory_kib())?;
        let mut file = Vec::with_capacity(MAGIC.len() + SALT_LEN + HASH_LEN);
        file.extend_from_slice(MAGIC);
        file.extend_from_slice(&salt);
        file.extend_from_slice(hash.as_ref());
        self.sys.atomic_write(&self.paths.screen_pin, &file, WriteOptions { mode: 0o600, group: None })?;
        Ok(Response::Ok)
    }

    /// The main socket's handling of the screen requests: SSH and the TUI may set the PIN and
    /// read the status, but locking and unlocking belong to the screen.
    pub(crate) fn screen_request_from_owner(&self, request: Request) -> Result<Response, ApiError> {
        match request {
            Request::ScreenStatus => Ok(Response::Screen(self.screen_status())),
            Request::SetScreenPin { passphrase, pin } => self.set_screen_pin(&passphrase, pin.as_ref()),
            Request::ScreenLock => {
                *lock(&self.screen.unlocked_since) = None;
                Ok(Response::Ok)
            }
            Request::ScreenUnlock { .. } => {
                Err(ApiError::Failed("only the touchscreen unlocks itself".to_owned()))
            }
            _ => unreachable!("only screen requests are passed here"),
        }
    }

    /// Everything the screen's account sends comes through here.
    pub fn handle_screen(&self, uid: u32, request: Request) -> Result<Response, ApiError> {
        match request {
            Request::ScreenStatus => Ok(Response::Screen(self.screen_status())),
            Request::ScreenUnlock { pin } => self.screen_unlock(&pin),
            Request::ScreenLock => {
                *lock(&self.screen.unlocked_since) = None;
                Ok(Response::Ok)
            }
            Request::SetScreenPin { .. } => Err(ApiError::Denied),
            request if is_read_only(&request) => self.handle_trusted(uid, request),
            // After a restart the device passphrase is the proof, so unlocking storage needs no PIN.
            request @ Request::Unlock { .. } => self.handle_trusted(uid, request),
            request => {
                let status = self.screen_status();
                if !status.pin_set {
                    return Err(ApiError::Locked(
                        "the screen can only look until a screen PIN is set over SSH or in the TUI"
                            .to_owned(),
                    ));
                }
                if !status.unlocked {
                    return Err(ApiError::Locked("enter the screen PIN to make changes".to_owned()));
                }
                *lock(&self.screen.unlocked_since) = Some(Instant::now());
                self.handle_trusted(uid, request)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::helper;
    use crate::helper::Timing;

    const SCREEN: u32 = 4242;

    fn secret(s: &str) -> Secret {
        Secret::new(s)
    }

    fn with_pin() -> Helper<crate::fake::FakeSystem> {
        let h = helper(crate::fake::FakeSystem::default());
        h.init_device(&secret("correct horse")).unwrap();
        h.handle(1000, Request::SetScreenPin { passphrase: secret("correct horse"), pin: Some(secret("482913")) })
            .unwrap();
        h
    }

    #[test]
    fn without_a_pin_the_screen_can_only_look() {
        let h = helper(crate::fake::FakeSystem::default());
        assert!(h.handle_screen(SCREEN, Request::ListServices).is_ok());
        let err = h.handle_screen(SCREEN, Request::StopService { id: "shop".into() }).unwrap_err();
        assert!(matches!(err, ApiError::Locked(ref m) if m.contains("SSH or in the TUI")), "{err:?}");
        assert_eq!(h.handle_screen(SCREEN, Request::ScreenUnlock { pin: secret("000000") }).unwrap_err(), ApiError::NotSetUp);
    }

    #[test]
    fn a_change_needs_the_pin_and_the_unlock_lasts_until_locked() {
        let h = with_pin();
        let locked = h.handle_screen(SCREEN, Request::StopService { id: "shop".into() }).unwrap_err();
        assert!(matches!(locked, ApiError::Locked(_)));
        assert_eq!(h.handle_screen(SCREEN, Request::ScreenUnlock { pin: secret("111111") }).unwrap_err(), ApiError::BadPassphrase);
        match h.handle_screen(SCREEN, Request::ScreenUnlock { pin: secret("482913") }).unwrap() {
            Response::Screen(s) => assert!(s.pin_set && s.unlocked),
            other => panic!("{other:?}"),
        }
        let after = h.handle_screen(SCREEN, Request::StopService { id: "shop".into() });
        assert!(!matches!(after, Err(ApiError::Locked(_))), "{after:?}");
        h.handle_screen(SCREEN, Request::ScreenLock).unwrap();
        assert!(matches!(h.handle_screen(SCREEN, Request::StopService { id: "shop".into() }), Err(ApiError::Locked(_))));
    }

    #[test]
    fn there_is_no_attempt_limit_but_every_check_takes_the_floor() {
        let h = with_pin();
        for _ in 0..20 {
            assert_eq!(h.handle_screen(SCREEN, Request::ScreenUnlock { pin: secret("999999") }).unwrap_err(), ApiError::BadPassphrase);
        }
        // Still unlocks: no lockout.
        assert!(h.handle_screen(SCREEN, Request::ScreenUnlock { pin: secret("482913") }).is_ok());
        let floors = h.sys.sleeps().iter().filter(|d| **d > Duration::ZERO && **d <= PIN_CHECK_DELAY).count();
        assert!(floors >= 21, "each check should pay the delay, saw {floors}");
    }

    #[test]
    fn the_unlock_ends_after_the_idle_time() {
        let mut h = with_pin();
        h.timing = Timing { screen_idle: Duration::from_millis(30), ..Timing::default() };
        h.handle_screen(SCREEN, Request::ScreenUnlock { pin: secret("482913") }).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        assert!(matches!(h.handle_screen(SCREEN, Request::StopService { id: "shop".into() }), Err(ApiError::Locked(_))));
    }

    #[test]
    fn the_screen_cannot_set_its_own_pin_and_the_owner_needs_the_passphrase() {
        let h = with_pin();
        let from_screen = h.handle_screen(SCREEN, Request::SetScreenPin { passphrase: secret("correct horse"), pin: None });
        assert_eq!(from_screen.unwrap_err(), ApiError::Denied);
        let wrong = h.handle(1000, Request::SetScreenPin { passphrase: secret("nope"), pin: Some(secret("123456")) });
        assert_eq!(wrong.unwrap_err(), ApiError::BadPassphrase);
        let short = h.handle(1000, Request::SetScreenPin { passphrase: secret("correct horse"), pin: Some(secret("12a4")) });
        assert!(matches!(short, Err(ApiError::Failed(_))));
        h.handle(1000, Request::SetScreenPin { passphrase: secret("correct horse"), pin: None }).unwrap();
        assert!(!h.screen_status().pin_set);
    }

    #[test]
    fn the_stored_pin_is_a_salted_hash_not_the_digits() {
        let h = with_pin();
        let file = h.sys.read_file(&h.paths.screen_pin).unwrap().unwrap();
        assert!(file.starts_with(MAGIC));
        assert!(!file.windows(6).any(|w| w == b"482913"));
    }

    #[test]
    fn device_unlock_after_a_restart_needs_no_pin() {
        let h = with_pin();
        let r = h.handle_screen(SCREEN, Request::Unlock { passphrase: secret("correct horse") });
        assert!(!matches!(r, Err(ApiError::Locked(_))), "{r:?}");
    }
}
