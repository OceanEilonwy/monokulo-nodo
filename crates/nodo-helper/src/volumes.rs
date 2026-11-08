//! Opens every encrypted volume with the single device passphrase, and creates new ones.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nodo_api::{ApiError, Response, Secret};
use nodo_gen::{
    mapper_name, service_user, validate, volume_directories, volume_mount, VOLUME_ROOT,
};
use zeroize::Zeroizing;

use crate::helper::{lock, Helper};
use crate::keys::{self, DeviceKey, KeyError, DEVICE_KEY_LEN};
use crate::system::{System, WriteOptions};

const SALT_AND_NONCE_LEN: usize = 40;
const MIN_PASSPHRASE_LEN: usize = 8;

const KEY_FILE: WriteOptions = WriteOptions {
    mode: 0o600,
    group: None,
};

/// Counted in characters, so a passphrase in any script meets the same rule.
fn check_new_passphrase(passphrase: &Secret) -> Result<(), ApiError> {
    if passphrase.char_count() < MIN_PASSPHRASE_LEN {
        return Err(ApiError::Failed(format!(
            "the passphrase must have at least {MIN_PASSPHRASE_LEN} characters"
        )));
    }
    Ok(())
}

fn gen_failed(e: nodo_gen::GenError) -> ApiError {
    ApiError::Failed(e.to_string())
}

/// Where a package's volume file is, and the drive it needs, if any.
pub(crate) struct VolumeLocation {
    pub file: PathBuf,
    pub drive: Option<PathBuf>,
}

impl<S: System> Helper<S> {
    /// On the data drive for a package approved to use it, so large encrypted volumes need not
    /// fit the system disk. Only ciphertext is ever written there.
    pub(crate) fn volume_location(&self, id: &str) -> Result<VolumeLocation, ApiError> {
        let name = format!("{id}.luks");
        Ok(match self.data_mount_of(id)? {
            Some(mount) => VolumeLocation {
                file: mount.join(".nodo-volumes").join(name),
                drive: Some(mount.to_path_buf()),
            },
            None => VolumeLocation {
                file: self.paths.volumes.join(name),
                drive: None,
            },
        })
    }

    /// Makes a freshly mounted volume ready for its service: `config`, `log` and `data`
    /// inside, and a root-owned top, so the service cannot replace `config` with a link that
    /// redirects the helper's writes. Volumes from before this layout kept data at the top,
    /// which moves into `data` within the same filesystem.
    fn prepare_volume(&self, id: &str, mount: &Path) -> Result<(), String> {
        let user = service_user(id).map_err(|e| e.to_string())?;
        let entries = self.sys.list_dir(mount).map_err(|e| e.0)?;
        if !entries.iter().any(|e| e == "data") {
            let data = mount.join("data");
            self.sys
                .ensure_dir(&data, 0o750, Some(&user))
                .map_err(|e| e.0)?;
            for entry in entries.iter().filter(|e| *e != "lost+found") {
                self.sys
                    .rename(&mount.join(entry), &data.join(entry))
                    .map_err(|e| e.0)?;
            }
        }
        for dir in volume_directories(id).map_err(|e| e.to_string())? {
            self.sys
                .ensure_dir(Path::new(&dir.path), dir.mode, dir.owner.as_deref())
                .map_err(|e| e.0)?;
        }
        self.sys.set_owner(mount, "root", 0o711).map_err(|e| e.0)
    }

    /// Doubles per consecutive failure, on top of the KDF's own cost per guess.
    fn unlock_delay(&self, failures: u32) -> Duration {
        let factor = 1u32
            .checked_shl(failures.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.timing
            .unlock_backoff_base
            .saturating_mul(factor)
            .min(self.timing.unlock_backoff_max)
    }

    fn read_keystore(&self) -> Result<Vec<u8>, ApiError> {
        self.sys
            .read_file(&self.paths.keystore)?
            .ok_or(ApiError::NotSetUp)
    }

    /// Every passphrase check goes through here, so unlocking and changing the passphrase share
    /// one failure count. The caller holds the count's lock across the delay, so parallel
    /// guesses queue and each pays its own delay.
    pub(crate) fn verify_passphrase(
        &self,
        failures: &mut u32,
        passphrase: &Secret,
        file: &[u8],
    ) -> Result<DeviceKey, ApiError> {
        match keys::unwrap_device_key(passphrase.expose().as_bytes(), file) {
            Ok(key) => {
                *failures = 0;
                Ok(key)
            }
            Err(KeyError::WrongPassphrase) => {
                *failures = failures.saturating_add(1);
                self.sys.sleep(self.unlock_delay(*failures));
                Err(ApiError::BadPassphrase)
            }
            Err(e) => Err(ApiError::Failed(e.to_string())),
        }
    }

    /// Opens every volume, keeps the secrets key in memory, and renders the settings services
    /// are waiting for.
    pub(crate) fn unlock(&self, passphrase: &Secret) -> Result<Response, ApiError> {
        let opened = {
            let mut failures = lock(&self.failed_unlocks);
            let file = self.read_keystore()?;
            let device_key = self.verify_passphrase(&mut failures, passphrase, &file)?;
            self.remember_device_key(&device_key);
            // Still holding the lock, so a concurrent unlock cannot race these mounts.
            self.open_all(&device_key)
        };
        let rendered = self.render_unlocked();
        match (opened, rendered) {
            (Ok(response), Ok(())) => Ok(response),
            (Ok(_), Err(problem)) => Err(ApiError::Failed(format!(
                "unlocked, but some settings could not be written: {problem}"
            ))),
            (Err(ApiError::Failed(volumes)), Err(problem)) => {
                Err(ApiError::Failed(format!("{volumes}. Settings: {problem}")))
            }
            (Err(e), _) => Err(e),
        }
    }

    /// Rewraps the same device key under a new passphrase. Volume keys derive from the device
    /// key, so every volume keeps working and nothing is reformatted.
    pub(crate) fn change_passphrase(
        &self,
        current: &Secret,
        new: &Secret,
    ) -> Result<Response, ApiError> {
        check_new_passphrase(new)?;
        let mut failures = lock(&self.failed_unlocks);
        let file = self.read_keystore()?;
        let device_key = self.verify_passphrase(&mut failures, current, &file)?;
        let mut random = [0u8; SALT_AND_NONCE_LEN];
        self.sys.random(&mut random)?;
        let rewrapped =
            keys::wrap_device_key(new.expose().as_bytes(), &device_key, &random, self.kdf)
                .map_err(|e| ApiError::Failed(e.to_string()))?;
        self.sys
            .atomic_write(&self.paths.keystore, &rewrapped, KEY_FILE)?;
        Ok(Response::Ok)
    }

    /// Opens what it can and reports the rest. One damaged volume must not keep every other
    /// service locked, and a repeat call retries only what is still closed.
    fn open_all(&self, device_key: &[u8; DEVICE_KEY_LEN]) -> Result<Response, ApiError> {
        let (packages, _) = self.packages()?;
        let encrypted: Vec<_> = packages
            .iter()
            .filter(|p| p.manifest.storage.encrypted)
            .collect();
        // Sandboxes bind volumes as the service user, so it must be able to pass through here.
        // It cannot list the directory, and each volume's own root is private to its user.
        if !encrypted.is_empty() {
            self.sys.ensure_dir(Path::new(VOLUME_ROOT), 0o711, None)?;
        }
        let mut failed = Vec::new();
        for package in &encrypted {
            if let Err(reason) = self.open_volume(device_key, package.id()) {
                failed.push(format!("{}: {reason}", package.id()));
            }
        }
        if failed.is_empty() {
            Ok(Response::Ok)
        } else {
            Err(ApiError::Failed(format!(
                "opened {} of {} volumes. {}",
                encrypted.len() - failed.len(),
                encrypted.len(),
                failed.join("; ")
            )))
        }
    }

    fn open_volume(&self, device_key: &[u8; DEVICE_KEY_LEN], id: &str) -> Result<(), String> {
        let location = self.volume_location(id).map_err(|e| e.to_string())?;
        let file = location.file;
        let name = mapper_name(id).map_err(|e| e.to_string())?;
        let mount = volume_mount(id).map_err(|e| e.to_string())?;
        let mount = Path::new(&mount);
        if self.sys.is_mounted(mount) {
            return Ok(());
        }
        // Never look for, or create, a volume underneath an absent drive's mount point.
        if let Some(drive) = location.drive.filter(|d| !self.sys.is_mounted(d)) {
            return Err(format!("waiting for the data drive at {}", drive.display()));
        }
        let opened_here = !self.sys.mapper_active(&name);
        if opened_here {
            if !self.sys.exists(&file) {
                return Err("its volume file is missing".to_owned());
            }
            let key = keys::volume_key(device_key, id);
            self.sys
                .luks_open(&file, &name, key.as_ref())
                .map_err(|e| e.0)?;
        }
        let device = Path::new("/dev/mapper").join(&name);
        // Root-only until mounted, so nothing can be written into the directory underneath.
        let result = self
            .sys
            .ensure_dir(mount, 0o700, None)
            .and_then(|()| self.sys.mount(&device, mount))
            .map_err(|e| e.0);
        if result.is_err() && opened_here {
            let _ = self.sys.luks_close(&name);
        }
        result?;
        self.prepare_volume(id, mount)
    }

    /// The data layout a package's volume holds, from the record at its top. A volume from
    /// before layouts were recorded is in layout 1.
    pub(crate) fn data_layout_of(&self, id: &str) -> u32 {
        let Ok(mount) = volume_mount(id) else { return 1 };
        self.sys
            .read_file(&Path::new(&mount).join(crate::backup::LAYOUT_FILE))
            .ok()
            .flatten()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| text.trim().parse().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(1)
    }

    /// Records the layout at the top of the volume, which only root can write.
    pub(crate) fn set_data_layout(&self, id: &str, layout: u32) -> Result<(), ApiError> {
        let mount = volume_mount(id).map_err(gen_failed)?;
        self.sys.atomic_write(
            &Path::new(&mount).join(crate::backup::LAYOUT_FILE),
            format!("{layout}\n").as_bytes(),
            WriteOptions {
                mode: 0o644,
                group: None,
            },
        )?;
        Ok(())
    }

    /// Grows a package's open volume to `size` bytes, without unmounting it. Takes the
    /// passphrase because resizing a LUKS2 mapping needs its key again.
    pub(crate) fn grow_volume(&self, id: &str, passphrase: &Secret, size: u64) -> Result<(), ApiError> {
        let name = mapper_name(id).map_err(gen_failed)?;
        let mount = volume_mount(id).map_err(gen_failed)?;
        if !self.sys.is_mounted(Path::new(&mount)) {
            return Err(ApiError::Locked(format!("{id}'s encrypted storage is locked")));
        }
        let device_key: DeviceKey = {
            let mut failures = lock(&self.failed_unlocks);
            let file = self.read_keystore()?;
            self.verify_passphrase(&mut failures, passphrase, &file)?
        };
        let file = self.volume_location(id)?.file;
        let have = self.sys.file_size(&file).unwrap_or(0);
        let parent = file.parent().unwrap_or(Path::new("/"));
        let (_, free) = self.sys.fs_usage(parent).unwrap_or((0, u64::MAX));
        if size.saturating_sub(have) > free {
            return Err(ApiError::Failed(format!(
                "there is not enough free space to grow {id}'s encrypted storage to {} MB",
                size >> 20
            )));
        }
        let key = keys::volume_key(&device_key, id);
        self.sys.luks_grow(&file, &name, key.as_ref(), size)?;
        Ok(())
    }

    /// First-time setup. Refuses to replace an existing key, which would lose every volume.
    pub fn init_device(&self, passphrase: &Secret) -> Result<(), ApiError> {
        check_new_passphrase(passphrase)?;
        {
            let _guard = lock(&self.failed_unlocks);
            if self.sys.read_file(&self.paths.keystore)?.is_some() {
                return Err(ApiError::Failed("this device is already set up".to_owned()));
            }
            let mut device_key = Zeroizing::new([0u8; DEVICE_KEY_LEN]);
            self.sys.random(device_key.as_mut())?;
            let mut random = [0u8; SALT_AND_NONCE_LEN];
            self.sys.random(&mut random)?;
            let file = keys::wrap_device_key(
                passphrase.expose().as_bytes(),
                &device_key,
                &random,
                self.kdf,
            )
            .map_err(|e| ApiError::Failed(e.to_string()))?;
            if let Some(dir) = self.paths.keystore.parent() {
                self.sys.ensure_dir(dir, 0o700, None)?;
            }
            self.sys
                .atomic_write(&self.paths.keystore, &file, KEY_FILE)?;
            // The user just typed the passphrase, so the device starts out unlocked.
            self.remember_device_key(&device_key);
        }
        // Secrets stored in clear before there was a key to seal them under are sealed now.
        self.render_unlocked().map_err(|problem| {
            ApiError::Failed(format!(
                "the device is set up, but some settings could not be written: {problem}"
            ))
        })
    }

    /// Creates and mounts a new volume for a package. It takes the passphrase because the device
    /// key is never kept in memory after an unlock.
    pub fn create_volume(&self, id: &str, passphrase: &Secret, size: u64) -> Result<(), ApiError> {
        validate::package_id(id).map_err(gen_failed)?;
        service_user(id).map_err(gen_failed)?;
        let name = mapper_name(id).map_err(gen_failed)?;
        let mount = volume_mount(id).map_err(gen_failed)?;
        let mount = Path::new(&mount);
        // Reachable from a client through installs, so it shares the unlock delay.
        let device_key: DeviceKey = {
            let mut failures = lock(&self.failed_unlocks);
            let file = self.read_keystore()?;
            self.verify_passphrase(&mut failures, passphrase, &file)?
        };
        self.remember_device_key(&device_key);
        let key = keys::volume_key(&device_key, id);
        let location = self.volume_location(id)?;
        let file = location.file;
        match &location.drive {
            Some(drive) if !self.sys.is_mounted(drive) => {
                return Err(ApiError::Failed(format!(
                    "the data drive is not mounted at {}",
                    drive.display()
                )))
            }
            Some(drive) => self
                .sys
                .ensure_dir(&drive.join(".nodo-volumes"), 0o700, None)?,
            None => self.sys.ensure_dir(&self.paths.volumes, 0o700, None)?,
        }
        // Fails when the file exists, so reinstalling a package can never format over old data.
        self.sys.allocate_file(&file, size)?;
        if let Err(e) = self.sys.luks_format(&file, key.as_ref()) {
            let _ = self.sys.remove_file(&file);
            return Err(e.into());
        }
        self.sys.luks_open(&file, &name, key.as_ref())?;
        let device = Path::new("/dev/mapper").join(&name);
        let result = self
            .sys
            .make_filesystem(&device)
            .and_then(|()| self.sys.ensure_dir(Path::new(VOLUME_ROOT), 0o711, None))
            .and_then(|()| self.sys.ensure_dir(mount, 0o700, None))
            .and_then(|()| self.sys.mount(&device, mount))
            .map_err(|e| e.0)
            .and_then(|()| self.prepare_volume(id, mount));
        if let Err(e) = result {
            let _ = self.sys.luks_close(&name);
            return Err(ApiError::Failed(e));
        }
        Ok(())
    }
}

#[cfg(test)]
mod growth_tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, ENCRYPTED};

    const PASS: &str = "correct horse battery";

    #[test]
    fn a_volume_grows_in_place_with_the_passphrase_and_keeps_its_layout_record() {
        let sys = FakeSystem::default();
        sys.add_manifest(ENCRYPTED);
        let h = helper(sys);
        h.init_device(&Secret::new(PASS)).unwrap();
        h.create_volume("vault", &Secret::new(PASS), 100).unwrap();
        assert_eq!(h.data_layout_of("vault"), 1);
        h.set_data_layout("vault", 3).unwrap();
        assert_eq!(h.data_layout_of("vault"), 3);
        assert_eq!(
            h.grow_volume("vault", &Secret::new("nope nope nope"), 300),
            Err(ApiError::BadPassphrase)
        );
        let full = h.grow_volume("vault", &Secret::new(PASS), 100_000).unwrap_err();
        assert!(full.to_string().contains("not enough free space"), "{full}");
        h.grow_volume("vault", &Secret::new(PASS), 300).unwrap();
        assert_eq!(h.sys.file_size(Path::new("/v/vault.luks")), Some(300));
        assert_eq!(h.sys.format_keys().len(), 1, "growing never formats");
        assert_eq!(h.data_layout_of("vault"), 3);
    }

    #[test]
    fn a_locked_volume_is_not_grown() {
        let sys = FakeSystem::default();
        sys.add_manifest(ENCRYPTED);
        let h = helper(sys);
        h.init_device(&Secret::new(PASS)).unwrap();
        h.create_volume("vault", &Secret::new(PASS), 1 << 20).unwrap();
        h.sys.reboot();
        assert!(matches!(
            h.grow_volume("vault", &Secret::new(PASS), 2 << 20),
            Err(ApiError::Locked(_))
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, ENCRYPTED, SHOP};

    const PASS: &str = "correct horse battery";
    const VAULT2: &str =
        "api_version = 1\nid = \"notes\"\nversion = \"1\"\nexec = [\"a\"]\n[storage]\nencrypted = true\n";

    fn s(text: &str) -> Secret {
        Secret::new(text)
    }

    fn setup() -> Helper<FakeSystem> {
        let sys = FakeSystem::default();
        sys.add_manifest(ENCRYPTED);
        sys.add_manifest(VAULT2);
        sys.add_manifest(SHOP);
        let h = helper(sys);
        h.init_device(&s(PASS)).unwrap();
        h.create_volume("vault", &s(PASS), 1 << 20).unwrap();
        h.create_volume("notes", &s(PASS), 1 << 20).unwrap();
        h.sys.reboot();
        h
    }

    fn unlock(h: &Helper<FakeSystem>, pass: &str) -> Result<Response, ApiError> {
        h.handle(
            1000,
            nodo_api::Request::Unlock {
                passphrase: s(pass),
            },
        )
    }

    #[test]
    fn one_passphrase_opens_every_volume() {
        let h = setup();
        assert!(!h.sys.mounted("/run/nodo/volumes/vault"));
        assert_eq!(unlock(&h, PASS), Ok(Response::Ok));
        assert!(h.sys.mounted("/run/nodo/volumes/vault"));
        assert!(h.sys.mounted("/run/nodo/volumes/notes"));
    }

    #[test]
    fn unencrypted_packages_get_no_volume() {
        let h = setup();
        unlock(&h, PASS).unwrap();
        assert!(!h.sys.calls().iter().any(|c| c.contains("nodo-shop")));
    }

    #[test]
    fn volumes_use_different_keys() {
        let h = setup();
        let keys = h.sys.format_keys();
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1]);
    }

    #[test]
    fn secrets_appear_only_on_the_key_channel() {
        let h = setup();
        unlock(&h, PASS).unwrap();
        let _ = unlock(&h, "wrong horse battery");
        let calls = h.sys.calls().join("\n");
        assert!(!calls.contains(PASS), "{calls}");
        assert!(!calls.contains("wrong horse"), "{calls}");
        let written = h.sys.get("/k/device.key").unwrap();
        assert!(!written.windows(PASS.len()).any(|w| w == PASS.as_bytes()));
    }

    #[test]
    fn wrong_passphrase_opens_nothing_and_delays_more_each_time() {
        let h = setup();
        h.sys.clear_calls();
        for _ in 0..3 {
            assert_eq!(unlock(&h, "nope nope nope"), Err(ApiError::BadPassphrase));
        }
        assert_eq!(
            h.sys.sleeps(),
            [
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8)
            ]
        );
        assert!(
            !h.sys.calls().iter().any(|c| c.starts_with("luks")),
            "{:?}",
            h.sys.calls()
        );
    }

    #[test]
    fn delay_is_capped_and_cleared_by_success() {
        let h = setup();
        for _ in 0..40 {
            let _ = unlock(&h, "nope nope nope");
        }
        assert_eq!(h.sys.sleeps().last(), Some(&Duration::from_secs(60)));
        unlock(&h, PASS).unwrap();
        let _ = unlock(&h, "nope nope nope");
        assert_eq!(h.sys.sleeps().last(), Some(&Duration::from_secs(2)));
    }

    #[test]
    fn correct_passphrase_is_not_delayed() {
        let h = setup();
        unlock(&h, PASS).unwrap();
        assert!(h.sys.sleeps().is_empty());
    }

    #[test]
    fn unlock_is_idempotent() {
        let h = setup();
        unlock(&h, PASS).unwrap();
        h.sys.clear_calls();
        assert_eq!(unlock(&h, PASS), Ok(Response::Ok));
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("luks open")));
    }

    #[test]
    fn repeat_unlock_still_needs_the_right_passphrase() {
        let h = setup();
        unlock(&h, PASS).unwrap();
        assert_eq!(unlock(&h, "nope nope nope"), Err(ApiError::BadPassphrase));
    }

    #[test]
    fn one_damaged_volume_does_not_block_the_others() {
        let h = setup();
        h.sys.fail("luks open /v/notes.luks");
        let Err(ApiError::Failed(message)) = unlock(&h, PASS) else {
            panic!()
        };
        assert!(message.contains("opened 1 of 2"), "{message}");
        assert!(message.contains("notes"), "{message}");
        assert!(h.sys.mounted("/run/nodo/volumes/vault"));
        assert!(!h.sys.mounted("/run/nodo/volumes/notes"));
    }

    #[test]
    fn mount_failure_closes_the_mapping_and_can_be_retried() {
        let h = setup();
        h.sys.fail_once("mount /dev/mapper/nodo-notes");
        assert!(matches!(unlock(&h, PASS), Err(ApiError::Failed(_))));
        assert!(h.sys.calls().contains(&"luks close nodo-notes".to_owned()));
        assert_eq!(unlock(&h, PASS), Ok(Response::Ok));
        assert!(h.sys.mounted("/run/nodo/volumes/notes"));
    }

    #[test]
    fn encrypted_package_without_a_volume_file_is_reported() {
        let h = setup();
        h.sys.remove("/v/notes.luks");
        let Err(ApiError::Failed(message)) = unlock(&h, PASS) else {
            panic!()
        };
        assert!(
            message.contains("notes: its volume file is missing"),
            "{message}"
        );
    }

    #[test]
    fn volume_renamed_to_another_package_does_not_open() {
        let h = setup();
        let vault = h.sys.get("/v/vault.luks").unwrap();
        h.sys.put("/v/notes.luks", &vault);
        let Err(ApiError::Failed(message)) = unlock(&h, PASS) else {
            panic!()
        };
        assert!(message.contains("notes"), "{message}");
    }

    #[test]
    fn missing_key_file_is_not_treated_as_a_bad_passphrase() {
        let h = setup();
        h.sys.remove("/k/device.key");
        assert_eq!(unlock(&h, PASS), Err(ApiError::NotSetUp));
        assert!(h.sys.sleeps().is_empty());
    }

    #[test]
    fn damaged_key_file_is_not_treated_as_a_bad_passphrase() {
        let h = setup();
        h.sys.put("/k/device.key", b"garbage");
        assert!(matches!(unlock(&h, PASS), Err(ApiError::Failed(_))));
        assert!(h.sys.sleeps().is_empty());
    }

    #[test]
    fn init_refuses_to_replace_an_existing_key() {
        let h = setup();
        let before = h.sys.get("/k/device.key").unwrap();
        assert!(h.init_device(&s("another passphrase")).is_err());
        assert_eq!(h.sys.get("/k/device.key").unwrap(), before);
    }

    #[test]
    fn init_refuses_a_short_passphrase_and_protects_the_key_file() {
        let h = helper(FakeSystem::default());
        assert!(h.init_device(&s("short")).is_err());
        assert_eq!(h.sys.get("/k/device.key"), None);
        h.init_device(&s(PASS)).unwrap();
        assert!(h
            .sys
            .calls()
            .contains(&"write /k/device.key mode=600 group=-".to_owned()));
    }

    #[test]
    fn create_refuses_to_format_over_an_existing_volume() {
        let h = setup();
        let before = h.sys.format_keys().len();
        assert!(h.create_volume("vault", &s(PASS), 1 << 20).is_err());
        assert_eq!(h.sys.format_keys().len(), before);
    }

    #[test]
    fn create_needs_the_passphrase_and_a_valid_id() {
        let h = setup();
        assert_eq!(
            h.create_volume("newapp", &s("nope nope nope"), 1 << 20),
            Err(ApiError::BadPassphrase)
        );
        assert!(h.create_volume("../evil", &s(PASS), 1 << 20).is_err());
        assert_eq!(h.sys.get("/v/newapp.luks"), None);
    }

    #[test]
    fn new_volume_has_its_layout_and_a_root_owned_top() {
        let h = setup();
        h.create_volume("newapp", &s(PASS), 1 << 20).unwrap();
        let calls = h.sys.calls();
        for expected in [
            "mkdir /run/nodo/volumes/newapp/config 755 -",
            "mkdir /run/nodo/volumes/newapp/log 750 _newapp",
            "mkdir /run/nodo/volumes/newapp/data 750 _newapp",
            "chown /run/nodo/volumes/newapp root 711",
        ] {
            assert!(
                calls.contains(&expected.to_owned()),
                "{expected}: {calls:?}"
            );
        }
    }

    #[test]
    fn a_volume_from_before_the_layout_moves_its_data_inside() {
        let h = setup();
        h.sys.put("/run/nodo/volumes/vault/blocks.db", b"chain");
        h.sys.put("/run/nodo/volumes/vault/lost+found/x", b"");
        unlock(&h, PASS).unwrap();
        assert_eq!(
            h.sys.get("/run/nodo/volumes/vault/data/blocks.db").unwrap(),
            b"chain"
        );
        assert_eq!(h.sys.get("/run/nodo/volumes/vault/blocks.db"), None);
        assert!(h.sys.get("/run/nodo/volumes/vault/lost+found/x").is_some());
        // Once in the new layout, nothing moves again.
        h.sys.reboot();
        h.sys.put("/run/nodo/volumes/vault/data/keep", b"");
        h.sys
            .put("/run/nodo/volumes/vault/config/settings.toml", b"");
        unlock(&h, PASS).unwrap();
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("rename")));
    }

    #[test]
    fn unlocking_keeps_the_secrets_key_and_setup_starts_unlocked() {
        let h = setup();
        assert!(
            h.secrets_key().is_some(),
            "setup and installs take the passphrase"
        );
        let fresh = helper(FakeSystem::default());
        assert!(fresh.secrets_key().is_none());
        let h = Helper {
            sys: h.sys,
            ..fresh
        };
        assert!(h.secrets_key().is_none());
        assert_eq!(unlock(&h, "nope nope nope"), Err(ApiError::BadPassphrase));
        assert!(h.secrets_key().is_none());
        unlock(&h, PASS).unwrap();
        assert!(h.secrets_key().is_some());
    }

    #[test]
    fn failed_format_leaves_no_volume_file() {
        let h = setup();
        h.sys.fail("luks format");
        assert!(h.create_volume("newapp", &s(PASS), 1 << 20).is_err());
        assert_eq!(h.sys.get("/v/newapp.luks"), None);
    }

    fn change(h: &Helper<FakeSystem>, current: &str, new: &str) -> Result<Response, ApiError> {
        h.handle(
            1000,
            nodo_api::Request::ChangePassphrase {
                current: s(current),
                new: s(new),
            },
        )
    }

    #[test]
    fn changed_passphrase_opens_the_same_volumes_and_the_old_one_stops_working() {
        let h = setup();
        let formats = h.sys.format_keys().len();
        assert_eq!(change(&h, PASS, "a brand new passphrase"), Ok(Response::Ok));
        assert_eq!(unlock(&h, PASS), Err(ApiError::BadPassphrase));
        assert_eq!(unlock(&h, "a brand new passphrase"), Ok(Response::Ok));
        assert!(h.sys.mounted("/run/nodo/volumes/vault"));
        assert_eq!(h.sys.format_keys().len(), formats, "nothing is reformatted");
    }

    #[test]
    fn change_needs_the_current_passphrase_and_shares_the_unlock_delay() {
        let h = setup();
        let before = h.sys.get("/k/device.key").unwrap();
        assert_eq!(
            change(&h, "nope nope nope", "a brand new passphrase"),
            Err(ApiError::BadPassphrase)
        );
        assert_eq!(unlock(&h, "nope nope nope"), Err(ApiError::BadPassphrase));
        assert_eq!(
            h.sys.sleeps(),
            [Duration::from_secs(2), Duration::from_secs(4)]
        );
        assert_eq!(h.sys.get("/k/device.key").unwrap(), before);
    }

    #[test]
    fn change_refuses_a_short_new_passphrase_before_checking_anything() {
        let h = setup();
        assert!(matches!(
            change(&h, PASS, "short"),
            Err(ApiError::Failed(_))
        ));
        assert!(h.sys.sleeps().is_empty());
        assert_eq!(unlock(&h, PASS), Ok(Response::Ok));
    }

    #[test]
    fn init_through_the_api_then_refuses_a_second_time() {
        let h = helper(FakeSystem::default());
        let init = |pass: &str| {
            h.handle(
                1000,
                nodo_api::Request::InitEncryption {
                    passphrase: s(pass),
                },
            )
        };
        assert_eq!(init(PASS), Ok(Response::Ok));
        assert!(init("another long passphrase").is_err());
    }

    #[test]
    fn short_passphrase_rule_counts_characters_not_bytes() {
        let h = helper(FakeSystem::default());
        // Seven characters, fourteen bytes.
        assert!(h.init_device(&s("ääääääá")).is_err());
        assert!(h.init_device(&s("ääääääää")).is_ok());
    }

    #[test]
    fn delay_overflow_is_safe() {
        let h = helper(FakeSystem::default());
        assert_eq!(h.unlock_delay(u32::MAX), Duration::from_secs(60));
    }
}
