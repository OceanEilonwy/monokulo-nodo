//! Encrypted backups and restores, with the device key.
//!
//! Backups are written into one root-only directory under generated names, so a request never
//! names a path to write. A backup includes the unwrapped device key when the user gives the
//! device passphrase, because without it encrypted storage cannot be restored after the
//! passphrase is lost.
//!
//! A restore needs the typed word, verifies the whole file into a fresh staging directory first,
//! then stops every service whose files it is about to replace, moves the files into place,
//! renders settings again and starts what was running before.

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nodo_api::{
    ApiError, BackupContents, BackupCreated, BackupInfo, Response, RestoreReport, Secret,
    RESTORE_WORD,
};
use nodo_backup::{Category, DirTarget, Kind, Plan, Source, Summary, CHAIN_PATHS};
use nodo_gen::Profile;
use zeroize::Zeroizing;

use crate::helper::{lock, Helper};
use crate::keys::{self, DEVICE_KEY_LEN};
use crate::manage::failed;
use crate::service::parse_sv_status;
use crate::system::{SvAction, System, WriteOptions};

const SUFFIX: &str = ".nodobak";
const TOR: &str = "tor";
const CADDY: &str = "nodo-caddy";
/// The archive name of Caddy's storage, under the secrets category since it holds private keys.
const CADDY_SOURCE: &str = "caddy";
const KEY_FILE: WriteOptions = WriteOptions {
    mode: 0o600,
    group: None,
};

fn name_ok(name: &str) -> bool {
    name.len() <= 64
        && name.ends_with(SUFFIX)
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn io(e: std::io::Error) -> ApiError {
    failed(e)
}

fn backup_error(e: nodo_backup::BackupError) -> ApiError {
    failed(e)
}

/// What a verified backup holds, by package.
#[derive(Debug, Default)]
struct Contents {
    settings: Vec<String>,
    storage: Vec<String>,
    onion: bool,
    certificates: bool,
    device_key: bool,
}

fn contents(summary: &Summary) -> Contents {
    let mut c = Contents {
        device_key: summary.has_device_key(),
        ..Contents::default()
    };
    for entry in &summary.entries {
        let parts: Vec<&str> = entry.path.split('/').collect();
        let found = match parts.as_slice() {
            ["settings", "nodo", file] if entry.kind == Kind::File => file
                .strip_suffix(".toml")
                .map(|id| (&mut c.settings, id.to_owned())),
            ["settings", app, ..] => app
                .strip_prefix("app-")
                .map(|id| (&mut c.settings, id.to_owned())),
            ["storage", id, ..] => Some((&mut c.storage, (*id).to_owned())),
            ["onion", ..] => {
                c.onion = true;
                None
            }
            ["secrets", CADDY_SOURCE, ..] => {
                c.certificates = true;
                None
            }
            _ => None,
        };
        if let Some((list, id)) = found {
            if nodo_gen::validate::package_id(&id).is_ok() && !list.contains(&id) {
                list.push(id);
            }
        }
    }
    c
}

/// Gives a restored tree the owner of the directory it replaced, without following links.
fn chown_tree(path: &Path, uid: u32, gid: u32) -> std::io::Result<()> {
    std::os::unix::fs::lchown(path, Some(uid), Some(gid))?;
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            chown_tree(&entry?.path(), uid, gid)?;
        }
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), ApiError> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(io)
}

impl<S: System> Helper<S> {
    fn backup_path(&self, name: &str) -> Result<PathBuf, ApiError> {
        if !name_ok(name) {
            return Err(ApiError::NotFound("invalid backup name".to_owned()));
        }
        Ok(self.manage.paths.backups.join(name))
    }

    fn backup_kdf(&self) -> nodo_backup::KdfParams {
        // The same cost as the device key, within what the backup format accepts.
        nodo_backup::KdfParams {
            memory_kib: self.kdf.memory_kib.max(8 * 1024),
            iterations: self.kdf.iterations.max(1),
            lanes: self.kdf.lanes.max(1),
        }
    }

    pub(crate) fn list_backups(&self) -> Result<Response, ApiError> {
        let mut list = Vec::new();
        let dir = match fs::read_dir(&self.manage.paths.backups) {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Response::Backups(list))
            }
            Err(e) => return Err(io(e)),
        };
        for entry in dir.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else { continue };
            if name_ok(&name) && meta.is_file() {
                list.push(BackupInfo {
                    name,
                    size_bytes: meta.len(),
                    created: meta.mtime().max(0) as u64,
                });
            }
        }
        list.sort_by(|a, b| b.created.cmp(&a.created).then(b.name.cmp(&a.name)));
        Ok(Response::Backups(list))
    }

    /// The unwrapped device key, after checking the passphrase like an unlock does.
    fn device_key_for_backup(
        &self,
        passphrase: Option<&Secret>,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, ApiError> {
        let mut failures = lock(&self.failed_unlocks);
        let Some(file) = self.sys.read_file(&self.paths.keystore)? else {
            return Ok(None);
        };
        let passphrase = passphrase.ok_or_else(|| {
            ApiError::NeedsConfirmation(
                "the device passphrase is needed, so the backup can restore encrypted storage"
                    .to_owned(),
            )
        })?;
        let key = self.verify_passphrase(&mut failures, passphrase, &file)?;
        Ok(Some(Zeroizing::new(key.to_vec())))
    }

    fn backup_plan(
        &self,
        device_key: Option<Zeroizing<Vec<u8>>>,
    ) -> Result<(Plan, Vec<String>), ApiError> {
        let p = &self.manage.paths;
        let mut sources = vec![
            Source {
                category: Category::Settings,
                name: "nodo".into(),
                path: self.paths.settings_store.clone(),
            },
            Source {
                category: Category::Settings,
                name: "exposure".into(),
                path: self.paths.exposure_state.clone(),
            },
            // Role bindings, approved grants and data drive choices.
            Source {
                category: Category::Settings,
                name: "bindings".into(),
                path: self.paths.bindings.clone(),
            },
            Source {
                category: Category::Secrets,
                name: "nodo".into(),
                path: p.secrets.clone(),
            },
            Source {
                category: Category::Onion,
                name: TOR.into(),
                path: p.onion.clone(),
            },
            // Certificates survive a restore without asking Let's Encrypt again, which has rate
            // limits, and devices that trust the local CA keep trusting it.
            Source {
                category: Category::Secrets,
                name: CADDY_SOURCE.into(),
                path: p.caddy_data.clone(),
            },
        ];
        let mut locked = Vec::new();
        let (packages, _) = self.packages()?;
        for package in &packages {
            let id = package.id();
            if package.profile == Profile::App {
                sources.push(Source {
                    category: Category::Settings,
                    name: format!("app-{id}"),
                    path: p.app_config.join(id),
                });
            }
            if package.manifest.storage.encrypted {
                let mount = p.volume_root.join(id);
                if self.sys.is_mounted(&mount) {
                    sources.push(Source {
                        category: Category::Storage,
                        name: id.to_owned(),
                        path: mount,
                    });
                } else {
                    locked.push(id.to_owned());
                }
            }
        }
        let plan = Plan {
            sources,
            device_key,
            exclude: CHAIN_PATHS.iter().map(PathBuf::from).collect(),
        };
        Ok((plan, locked))
    }

    pub(crate) fn create_backup(
        &self,
        device_passphrase: Option<&Secret>,
        backup_passphrase: &Secret,
    ) -> Result<Response, ApiError> {
        if backup_passphrase.char_count() < nodo_backup::MIN_PASSPHRASE_LEN {
            return Err(ApiError::Failed(format!(
                "a backup passphrase needs at least {} characters",
                nodo_backup::MIN_PASSPHRASE_LEN
            )));
        }
        let device_key = self.device_key_for_backup(device_passphrase)?;
        let _work = lock(&self.manage.work);
        let (plan, locked) = self.backup_plan(device_key)?;
        private_dir(&self.manage.paths.backups)?;
        let now = self.manage.packages.now();
        let mut n = 0;
        let (name, path, file) = loop {
            let name = match n {
                0 => format!("nodo-{now}{SUFFIX}"),
                _ => format!("nodo-{now}-{n}{SUFFIX}"),
            };
            let path = self.manage.paths.backups.join(&name);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => break (name, path, file),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => n += 1,
                Err(e) => return Err(io(e)),
            }
        };
        let written = (|| {
            let mut out = BufWriter::new(&file);
            let summary = nodo_backup::backup(
                &plan,
                backup_passphrase.expose().as_bytes(),
                self.backup_kdf(),
                &mut out,
                now,
            )
            .map_err(backup_error)?;
            out.flush().map_err(io)?;
            drop(out);
            file.sync_all().map_err(io)?;
            Ok(summary)
        })();
        let summary = match written {
            Ok(summary) => summary,
            Err(e) => {
                let _ = fs::remove_file(&path);
                return Err(e);
            }
        };
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Ok(Response::BackupCreated(BackupCreated {
            info: BackupInfo {
                name,
                size_bytes: size,
                created: now,
            },
            entries: summary.entries.len() as u32,
            has_device_key: summary.has_device_key(),
            missing: summary
                .missing
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            locked,
        }))
    }

    /// Services whose files a restore of `c` replaces, which are installed here.
    fn affected_services(&self, c: &Contents) -> Result<Vec<String>, ApiError> {
        let (packages, _) = self.packages()?;
        let mut out: Vec<String> = packages
            .iter()
            .map(|p| p.id().to_owned())
            .filter(|id| c.settings.contains(id) || c.storage.contains(id))
            .collect();
        if c.onion && self.sys.exists(&self.paths.sv_dir.join(TOR)) {
            out.push(TOR.to_owned());
        }
        if c.certificates && self.sys.exists(&self.paths.sv_dir.join(CADDY)) {
            out.push(CADDY.to_owned());
        }
        Ok(out)
    }

    pub(crate) fn inspect_backup(&self, name: &str, passphrase: &Secret) -> Result<Response, ApiError> {
        let path = self.backup_path(name)?;
        let file = File::open(&path).map_err(|_| ApiError::NotFound(name.to_owned()))?;
        let summary = nodo_backup::verify(BufReader::new(file), passphrase.expose().as_bytes())
            .map_err(backup_error)?;
        let c = contents(&summary);
        Ok(Response::BackupContents(BackupContents {
            name: name.to_owned(),
            entries: summary.entries.len() as u32,
            has_device_key: c.device_key,
            services_to_stop: self.affected_services(&c)?,
            settings: c.settings,
            storage: c.storage,
            onion_keys: c.onion,
        }))
    }

    pub(crate) fn delete_backup(&self, name: &str) -> Result<Response, ApiError> {
        let path = self.backup_path(name)?;
        fs::remove_file(&path).map_err(|_| ApiError::NotFound(name.to_owned()))?;
        self.list_backups()
    }

    pub(crate) fn restore_backup(
        &self,
        name: &str,
        passphrase: &Secret,
        new_device_passphrase: Option<&Secret>,
        typed: &str,
    ) -> Result<Response, ApiError> {
        if typed != RESTORE_WORD {
            return Err(ApiError::NeedsConfirmation(format!(
                "type {RESTORE_WORD} to confirm. Restoring replaces settings, secrets and storage."
            )));
        }
        if let Some(p) = new_device_passphrase {
            if p.char_count() < 8 {
                return Err(ApiError::Failed(
                    "the new device passphrase must have at least 8 characters".into(),
                ));
            }
        }
        let path = self.backup_path(name)?;
        let _work = lock(&self.manage.work);
        let file = File::open(&path).map_err(|_| ApiError::NotFound(name.to_owned()))?;
        let staging = self
            .manage
            .paths
            .restore_staging
            .join(format!(".restore-{}", self.manage.packages.now()));
        let _ = fs::remove_dir_all(&staging);
        let mut target = DirTarget::create(&staging).map_err(backup_error)?;
        // Verifies the entire file before writing anything; any failure leaves no staging.
        let summary = nodo_backup::restore(
            BufReader::new(file),
            passphrase.expose().as_bytes(),
            &mut target,
        )
        .map_err(backup_error)?;
        let result = self.apply_restore(&staging, &summary, target.device_key.take(), new_device_passphrase);
        // The staging copy holds secrets and keys, so it never outlives the restore.
        let _ = fs::remove_dir_all(&staging);
        result.map(Response::Restored)
    }

    fn apply_restore(
        &self,
        staging: &Path,
        summary: &Summary,
        device_key: Option<Zeroizing<Vec<u8>>>,
        new_device_passphrase: Option<&Secret>,
    ) -> Result<RestoreReport, ApiError> {
        let c = contents(summary);
        if device_key.is_some() && new_device_passphrase.is_none() {
            return Err(ApiError::NeedsConfirmation(
                "this backup holds a device key, so choose a new device passphrase for it".into(),
            ));
        }
        let mut report = RestoreReport {
            restored: Vec::new(),
            skipped: Vec::new(),
            stopped: Vec::new(),
            restarted: Vec::new(),
        };

        // Stop first, and only what was running is started again afterwards.
        for id in self.affected_services(&c)? {
            let running = self
                .sys
                .sv_status(&id)
                .map(|t| parse_sv_status(&t).is_running())
                .unwrap_or(false);
            if !running {
                continue;
            }
            let timeout = self
                .package(&id)
                .map(|p| p.manifest.stop_timeout_secs)
                .unwrap_or(30);
            self.sys
                .sv(SvAction::ForceStop, &id, timeout.saturating_add(5))?;
            report.stopped.push(id);
        }
        let outcome = self.place_restored(staging, &c, device_key, new_device_passphrase, &mut report);
        for id in report.stopped.clone() {
            if self.sys.sv(SvAction::Start, &id, 30).is_ok() {
                report.restarted.push(id);
            }
        }
        outcome.map(|()| report)
    }

    fn place_restored(
        &self,
        staging: &Path,
        c: &Contents,
        device_key: Option<Zeroizing<Vec<u8>>>,
        new_device_passphrase: Option<&Secret>,
        report: &mut RestoreReport,
    ) -> Result<(), ApiError> {
        let p = &self.manage.paths;
        let tree = |from: &Path, to: &Path| -> Result<(), ApiError> {
            private_dir(to)?;
            self.manage.packages.restore_tree(from, to, None).map_err(failed)
        };
        let staged = |rel: &str| staging.join(rel);

        if let Some(key) = device_key {
            self.restore_device_key(&key, new_device_passphrase, report)?;
        }
        if staged("settings/nodo").is_dir() {
            tree(&staged("settings/nodo"), &self.paths.settings_store)?;
            report.restored.push("settings".into());
        }
        if staged("settings/bindings").is_file() {
            let bytes = fs::read(staged("settings/bindings")).map_err(io)?;
            self.sys.atomic_write(&self.paths.bindings, &bytes, KEY_FILE)?;
            report.restored.push("role bindings and grants".into());
        }
        if staged("settings/exposure").is_file() {
            let bytes = fs::read(staged("settings/exposure")).map_err(io)?;
            self.sys
                .atomic_write(&self.paths.exposure_state, &bytes, KEY_FILE)?;
            report
                .restored
                .push("site exposure (applies when the proxy next reloads)".into());
        }
        if staged("secrets/nodo").exists() {
            tree(&staged("secrets/nodo"), &p.secrets)?;
            report.restored.push("secrets".into());
        }
        if staged("onion/tor").exists() {
            let owner = fs::metadata(&p.onion).ok().map(|m| (m.uid(), m.gid()));
            tree(&staged("onion/tor"), &p.onion)?;
            if let Some((uid, gid)) = owner {
                chown_tree(&p.onion, uid, gid).map_err(io)?;
            }
            report.restored.push("Tor onion keys".into());
        }
        let caddy_staged = staged(&format!("secrets/{CADDY_SOURCE}"));
        if caddy_staged.exists() {
            // Caddy's home decides the owner, since the data directory may not exist yet.
            let home = p.caddy_data.ancestors().nth(2).unwrap_or(&p.caddy_data);
            let owner = fs::metadata(home).ok().map(|m| (m.uid(), m.gid()));
            tree(&caddy_staged, &p.caddy_data)?;
            if let Some((uid, gid)) = owner {
                for dir in p.caddy_data.ancestors().take(2) {
                    std::os::unix::fs::lchown(dir, Some(uid), Some(gid)).map_err(io)?;
                }
                chown_tree(&p.caddy_data, uid, gid).map_err(io)?;
            }
            report.restored.push("certificates".into());
        }
        for id in &c.storage {
            let mount = p.volume_root.join(id);
            if !self.sys.is_mounted(&mount) {
                report
                    .skipped
                    .push(format!("{id} storage: not installed or locked here"));
                continue;
            }
            // `config` is rendered again below. `data` and `log` keep the owner and mode the
            // volume gives them here, since the backup may come from another device.
            let from = staged(&format!("storage/{id}"));
            for sub in ["data", "log"] {
                let (source, target) = (from.join(sub), mount.join(sub));
                if !source.is_dir() || !target.is_dir() {
                    continue;
                }
                let meta = fs::metadata(&target).map_err(io)?;
                self.manage.packages.restore_tree(&source, &target, None).map_err(failed)?;
                chown_tree(&target, meta.uid(), meta.gid()).map_err(io)?;
                fs::set_permissions(&target, fs::Permissions::from_mode(meta.mode() & 0o7777))
                    .map_err(io)?;
            }
            report.restored.push(format!("{id} storage"));
        }
        // Rendered files are made again from the restored values, with this device's owners.
        let (packages, _) = self.packages()?;
        for package in packages.iter().filter(|p| c.settings.contains(&p.id().to_owned())) {
            if package.manifest.settings.is_none() {
                continue;
            }
            match self.render_settings(package) {
                Ok(()) => report.restored.push(format!("{} settings", package.id())),
                Err(e) => report
                    .skipped
                    .push(format!("{} settings could not be rendered: {e}", package.id())),
            }
        }
        for id in &c.settings {
            if self.package(id).is_err() {
                report
                    .skipped
                    .push(format!("{id} settings: not installed here, kept for when it is"));
            }
        }
        Ok(())
    }

    /// Settings files rendered again from the restored store, sealed secrets included when
    /// the device is unlocked, with this device's owners and inside the volume for encrypted
    /// packages.
    fn render_settings(&self, package: &crate::catalog::Package) -> Result<(), ApiError> {
        let (packages, _) = self.packages()?;
        let plan = self.with_settings(&packages, |ctx| self.settings.plan(ctx, package, &[]))?;
        for file in plan.files {
            crate::apply::Write::from(file).apply(&self.sys)?;
        }
        Ok(())
    }

    /// A device whose volumes use its own key keeps it: replacing it would lock them for good.
    /// Otherwise the backup's key is wrapped with the new passphrase, so its storage opens.
    fn restore_device_key(
        &self,
        key: &[u8],
        new_passphrase: Option<&Secret>,
        report: &mut RestoreReport,
    ) -> Result<(), ApiError> {
        let Some(passphrase) = new_passphrase else {
            return Ok(());
        };
        let key: [u8; DEVICE_KEY_LEN] = key
            .try_into()
            .map_err(|_| failed("the backup's device key has the wrong length"))?;
        let key = Zeroizing::new(key);
        let has_volumes = self
            .sys
            .list_dir(&self.paths.volumes)?
            .iter()
            .any(|n| n.ends_with(".luks"));
        if self.sys.exists(&self.paths.keystore) && has_volumes {
            report.skipped.push(
                "device key: this device's encrypted storage uses its own key, which was kept"
                    .into(),
            );
            return Ok(());
        }
        let _guard = lock(&self.failed_unlocks);
        let mut random = [0u8; 40];
        self.sys.random(&mut random)?;
        let file = keys::wrap_device_key(passphrase.expose().as_bytes(), &key, &random, self.kdf)
            .map_err(failed)?;
        if let Some(dir) = self.paths.keystore.parent() {
            self.sys.ensure_dir(dir, 0o700, None)?;
        }
        self.sys.atomic_write(&self.paths.keystore, &file, KEY_FILE)?;
        report
            .restored
            .push("device key, with the new device passphrase".into());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_names_are_plain_files_in_the_backup_directory() {
        assert!(name_ok("nodo-1700000000.nodobak"));
        assert!(name_ok("nodo-1700000000-2.nodobak"));
        for bad in ["../x.nodobak", "x", ".nodobak", "a/b.nodobak", "nodo.toml", "-x .nodobak"] {
            assert!(!name_ok(bad), "{bad}");
        }
    }
}
