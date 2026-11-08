//! Encrypted backups and restores, with the device key.
//!
//! Backups are written into one root-only directory under generated names, or into the
//! `monokulo-nodo` folder of a removable drive (`usb.rs`), so a request never names a path to
//! write. A backup includes the unwrapped device key when the user gives the device passphrase,
//! because without it encrypted storage cannot be restored after the passphrase is lost. It also
//! holds the feeds with their pinned keys, the approvals, update choices and the verified package
//! files kept for rollback, so a new device can be rebuilt from it alone.
//!
//! A restore needs the typed word, verifies the whole file into a fresh staging directory first,
//! then stops every service whose files it is about to replace, moves the files into place,
//! renders settings again and starts what was running before.
//!
//! On a device that is not set up yet, a restore does the setup instead: it wraps the backup's
//! device key with the new device passphrase, puts back feeds, pins and approvals, reinstalls
//! each package from its kept verified file (from its feed only when the backup has no file),
//! creates its volume, fills it from the backup and starts it. The device ends up unlocked.

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nodo_api::{
    ApiError, BackupContents, BackupInfo, Response, RestoreReport, Secret, RESTORE_WORD,
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
/// Feeds, pinned keys, approvals and what each package was installed from.
const FEEDS_SOURCE: &str = "feeds";
/// The auto-update policy and holds.
const UPDATES_SOURCE: &str = "updates";
/// The verified package files kept for rollback, under the packages category.
const KEPT_SOURCE: &str = "kept";
/// The data layout a volume's data is in, at the top of the volume, root-owned.
pub(crate) const LAYOUT_FILE: &str = ".nodo-layout";
const KEY_FILE: WriteOptions = WriteOptions {
    mode: 0o600,
    group: None,
};
const MIN_DEVICE_PASSPHRASE: usize = 8;

pub(crate) fn name_ok(name: &str) -> bool {
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

/// A finished backup file.
pub(crate) struct Written {
    pub name: String,
    pub size: u64,
    pub created: u64,
    pub summary: Summary,
}

/// Why writing a backup file failed, with the system error number when there was one, so a
/// full or unplugged drive can be named as such.
pub(crate) struct WriteFailure {
    pub errno: Option<i32>,
    pub error: ApiError,
}

/// Remembers the error number of the last failed write, which the backup format turns into text.
struct ErrnoWriter<W: Write> {
    inner: W,
    errno: Rc<Cell<Option<i32>>>,
}

impl<W: Write> Write for ErrnoWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf).inspect_err(|e| self.errno.set(e.raw_os_error()))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush().inspect_err(|e| self.errno.set(e.raw_os_error()))
    }
}

/// A verified backup in its staging directory, which holds secrets and keys and is removed
/// once the restore is done.
pub(crate) struct Staged {
    pub dir: PathBuf,
    summary: Summary,
    device_key: Option<Zeroizing<Vec<u8>>>,
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
    pub(crate) fn device_key_for_backup(
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

    pub(crate) fn backup_plan(
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
                category: Category::Settings,
                name: FEEDS_SOURCE.into(),
                path: p.feeds.clone(),
            },
            Source {
                category: Category::Settings,
                name: UPDATES_SOURCE.into(),
                path: p.updates.clone(),
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
            Source {
                category: Category::Packages,
                name: KEPT_SOURCE.into(),
                path: p.kept.clone(),
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

    /// About how many bytes a backup of `plan` takes, for checking a drive's free space first.
    pub(crate) fn backup_estimate(&self, plan: &Plan) -> u64 {
        let data: u64 = plan
            .sources
            .iter()
            .map(|s| self.manage.packages.tree_size(&s.path))
            .sum();
        // Chunk tags, entry headers and the key: well under one percent, plus a fixed part.
        data + data / 64 + 64 * 1024
    }

    pub(crate) fn check_backup_passphrase(&self, passphrase: &Secret) -> Result<(), ApiError> {
        if passphrase.char_count() < nodo_backup::MIN_PASSPHRASE_LEN {
            return Err(ApiError::Failed(format!(
                "a backup passphrase needs at least {} characters",
                nodo_backup::MIN_PASSPHRASE_LEN
            )));
        }
        Ok(())
    }

    /// Writes a backup of `plan` into `dir` under a hidden temporary name, syncs it, renames it
    /// to a new `nodo-<time>.nodobak` and syncs the directory. Any failure removes the
    /// temporary file, so a backup name always means a complete file.
    pub(crate) fn write_backup_file(
        &self,
        dir: &Path,
        plan: &Plan,
        passphrase: &Secret,
        now: u64,
    ) -> Result<Written, WriteFailure> {
        let plain = |error: ApiError| WriteFailure { errno: None, error };
        let mut n = 0;
        let name = loop {
            let name = match n {
                0 => format!("nodo-{now}{SUFFIX}"),
                _ => format!("nodo-{now}-{n}{SUFFIX}"),
            };
            if fs::symlink_metadata(dir.join(&name)).is_err() {
                break name;
            }
            if n >= 100 {
                return Err(plain(failed("too many backups with the same time")));
            }
            n += 1;
        };
        let partial = dir.join(format!(".{name}.partial"));
        let errno = Rc::new(Cell::new(None));
        let result = (|| -> Result<Summary, WriteFailure> {
            let io_failure = |e: std::io::Error| WriteFailure {
                errno: e.raw_os_error(),
                error: io(e),
            };
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(&partial)
                .map_err(io_failure)?;
            let mut out = BufWriter::new(ErrnoWriter {
                inner: &file,
                errno: errno.clone(),
            });
            let summary = nodo_backup::backup(
                plan,
                passphrase.expose().as_bytes(),
                self.backup_kdf(),
                &mut out,
                now,
            )
            .map_err(|e| WriteFailure {
                errno: errno.get(),
                error: backup_error(e),
            })?;
            out.flush().map_err(io_failure)?;
            drop(out);
            // A drive pulled out usually shows here, when the cached writes reach it.
            file.sync_all().map_err(io_failure)?;
            drop(file);
            if fs::symlink_metadata(dir.join(&name)).is_ok() {
                return Err(plain(failed("a backup with the same name appeared")));
            }
            fs::rename(&partial, dir.join(&name)).map_err(io_failure)?;
            // Makes the rename itself durable. Some filesystems cannot sync a directory.
            if let Ok(d) = File::open(dir) {
                match d.sync_all() {
                    Err(e) if e.raw_os_error() != Some(22) => return Err(io_failure(e)),
                    _ => {}
                }
            }
            Ok(summary)
        })();
        match result {
            Ok(summary) => {
                let size = fs::metadata(dir.join(&name)).map(|m| m.len()).unwrap_or(0);
                Ok(Written {
                    name,
                    size,
                    created: now,
                    summary,
                })
            }
            Err(e) => {
                let _ = fs::remove_file(&partial);
                Err(e)
            }
        }
    }

    pub(crate) fn create_backup(
        &self,
        device_passphrase: Option<&Secret>,
        backup_passphrase: &Secret,
    ) -> Result<Response, ApiError> {
        self.check_backup_passphrase(backup_passphrase)?;
        let device_key = self.device_key_for_backup(device_passphrase)?;
        let _work = lock(&self.manage.work);
        let (plan, locked) = self.backup_plan(device_key)?;
        private_dir(&self.manage.paths.backups)?;
        let now = self.manage.packages.now();
        let written = self
            .write_backup_file(&self.manage.paths.backups, &plan, backup_passphrase, now)
            .map_err(|f| f.error)?;
        Ok(Response::BackupCreated(nodo_api::BackupCreated {
            info: BackupInfo {
                name: written.name,
                size_bytes: written.size,
                created: written.created,
            },
            entries: written.summary.entries.len() as u32,
            has_device_key: written.summary.has_device_key(),
            missing: written
                .summary
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

    pub(crate) fn contents_of(&self, name: &str, summary: &Summary) -> Result<BackupContents, ApiError> {
        let c = contents(summary);
        Ok(BackupContents {
            name: name.to_owned(),
            entries: summary.entries.len() as u32,
            has_device_key: c.device_key,
            services_to_stop: self.affected_services(&c)?,
            settings: c.settings,
            storage: c.storage,
            onion_keys: c.onion,
        })
    }

    pub(crate) fn inspect_backup(&self, name: &str, passphrase: &Secret) -> Result<Response, ApiError> {
        let path = self.backup_path(name)?;
        let file = File::open(&path).map_err(|_| ApiError::NotFound(name.to_owned()))?;
        let summary = nodo_backup::verify(BufReader::new(file), passphrase.expose().as_bytes())
            .map_err(backup_error)?;
        Ok(Response::BackupContents(self.contents_of(name, &summary)?))
    }

    pub(crate) fn delete_backup(&self, name: &str) -> Result<Response, ApiError> {
        let path = self.backup_path(name)?;
        fs::remove_file(&path).map_err(|_| ApiError::NotFound(name.to_owned()))?;
        self.list_backups()
    }

    pub(crate) fn check_restore_request(
        new_device_passphrase: Option<&Secret>,
        typed: &str,
    ) -> Result<(), ApiError> {
        if typed != RESTORE_WORD {
            return Err(ApiError::NeedsConfirmation(format!(
                "type {RESTORE_WORD} to confirm. Restoring replaces settings, secrets and storage."
            )));
        }
        if let Some(p) = new_device_passphrase {
            if p.char_count() < MIN_DEVICE_PASSPHRASE {
                return Err(ApiError::Failed(format!(
                    "the new device passphrase must have at least {MIN_DEVICE_PASSPHRASE} characters"
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn restore_backup(
        &self,
        name: &str,
        passphrase: &Secret,
        new_device_passphrase: Option<&Secret>,
        typed: &str,
    ) -> Result<Response, ApiError> {
        Self::check_restore_request(new_device_passphrase, typed)?;
        let path = self.backup_path(name)?;
        let _work = lock(&self.manage.work);
        let file = File::open(&path).map_err(|_| ApiError::NotFound(name.to_owned()))?;
        let staged = self.stage_restore(file, passphrase)?;
        let result = self.apply_staged(&staged, new_device_passphrase);
        // The staging copy holds secrets and keys, so it never outlives the restore.
        let _ = fs::remove_dir_all(&staged.dir);
        result.map(Response::Restored)
    }

    /// Verifies the entire file before writing anything, then unpacks it into a new staging
    /// directory. Any failure leaves no staging directory.
    pub(crate) fn stage_restore<R: Read + Seek>(&self, file: R, passphrase: &Secret) -> Result<Staged, ApiError> {
        let dir = self
            .manage
            .paths
            .restore_staging
            .join(format!(".restore-{}", self.manage.packages.now()));
        let _ = fs::remove_dir_all(&dir);
        let mut target = DirTarget::create(&dir).map_err(backup_error)?;
        let summary = nodo_backup::restore(BufReader::new(file), passphrase.expose().as_bytes(), &mut target)
            .map_err(backup_error)?;
        Ok(Staged {
            dir,
            summary,
            device_key: target.device_key.take(),
        })
    }

    pub(crate) fn apply_staged(
        &self,
        staged: &Staged,
        new_device_passphrase: Option<&Secret>,
    ) -> Result<RestoreReport, ApiError> {
        let c = contents(&staged.summary);
        if staged.device_key.is_some() && new_device_passphrase.is_none() {
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
        if !self.sys.exists(&self.paths.keystore) {
            let passphrase = new_device_passphrase.ok_or_else(|| {
                ApiError::NeedsConfirmation(
                    "this device is not set up yet, so choose a device passphrase for it".into(),
                )
            })?;
            self.restore_fresh(staged, &c, passphrase, &mut report)?;
            return Ok(report);
        }

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
        let outcome = self.place_restored(
            &staged.dir,
            &c,
            staged.device_key.clone(),
            new_device_passphrase,
            &mut report,
        );
        for id in report.stopped.clone() {
            if self.sys.sv(SvAction::Start, &id, 30).is_ok() {
                report.restarted.push(id);
            }
        }
        if report.restored.iter().any(|r| r == "feeds") {
            report.skipped.push(
                "feeds, approvals and packages: this device keeps its own, as they match what is installed"
                    .into(),
            );
        }
        outcome.map(|()| report)
    }

    /// Settings, secrets, onion keys and certificates, shared by both kinds of restore.
    fn place_common(&self, staging: &Path, report: &mut RestoreReport) -> Result<(), ApiError> {
        let p = &self.manage.paths;
        let tree = |from: &Path, to: &Path| -> Result<(), ApiError> {
            private_dir(to)?;
            self.manage.packages.restore_tree(from, to, None).map_err(failed)
        };
        let staged = |rel: &str| staging.join(rel);
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
        if staged(&format!("settings/{FEEDS_SOURCE}")).is_file() {
            report.restored.push("feeds".into());
        }
        Ok(())
    }

    /// Fills a mounted volume from the backup. `config` is rendered again. `data` and `log`
    /// keep the owner and mode the volume gives them here, since the backup may come from
    /// another device. The layout record comes along, so the data keeps its declared layout.
    fn fill_volume(&self, staging: &Path, id: &str) -> Result<(), ApiError> {
        let mount = self.manage.paths.volume_root.join(id);
        let from = staging.join(format!("storage/{id}"));
        for sub in ["data", "log"] {
            let (source, target) = (from.join(sub), mount.join(sub));
            if !source.is_dir() || !target.is_dir() {
                continue;
            }
            let meta = fs::metadata(&target).map_err(io)?;
            self.manage
                .packages
                .restore_tree(&source, &target, None)
                .map_err(failed)?;
            chown_tree(&target, meta.uid(), meta.gid()).map_err(io)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(meta.mode() & 0o7777))
                .map_err(io)?;
        }
        if let Ok(bytes) = fs::read(from.join(LAYOUT_FILE)) {
            self.sys
                .atomic_write(&mount.join(LAYOUT_FILE), &bytes, WriteOptions { mode: 0o644, group: None })?;
        }
        Ok(())
    }

    fn place_restored(
        &self,
        staging: &Path,
        c: &Contents,
        device_key: Option<Zeroizing<Vec<u8>>>,
        new_device_passphrase: Option<&Secret>,
        report: &mut RestoreReport,
    ) -> Result<(), ApiError> {
        if let Some(key) = device_key {
            self.restore_device_key(&key, new_device_passphrase, report)?;
        }
        self.place_common(staging, report)?;
        for id in &c.storage {
            let mount = self.manage.paths.volume_root.join(id);
            if !self.sys.is_mounted(&mount) {
                report
                    .skipped
                    .push(format!("{id} storage: not installed or locked here"));
                continue;
            }
            self.fill_volume(staging, id)?;
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

    /// The whole restore onto a device that is not set up: the device key under the new
    /// passphrase, every file the backup holds, then each package reinstalled, its volume made
    /// and filled, and its service started. Ends unlocked.
    fn restore_fresh(
        &self,
        staged: &Staged,
        c: &Contents,
        passphrase: &Secret,
        report: &mut RestoreReport,
    ) -> Result<(), ApiError> {
        let staging = &staged.dir;
        match &staged.device_key {
            Some(key) => {
                self.restore_device_key(key, Some(passphrase), report)?;
                let key: [u8; DEVICE_KEY_LEN] = key.as_slice().try_into().map_err(failed)?;
                // The owner just chose the passphrase, so the device starts out unlocked.
                self.remember_device_key(&key);
            }
            None => {
                self.init_device(passphrase)?;
                report
                    .skipped
                    .push("device key: the backup has none, so a new one was made".into());
            }
        }
        self.place_common(staging, report)?;
        let p = &self.manage.paths;
        let feeds_file = staging.join(format!("settings/{FEEDS_SOURCE}"));
        if feeds_file.is_file() {
            let bytes = fs::read(&feeds_file).map_err(io)?;
            if let Some(dir) = p.feeds.parent() {
                self.sys.ensure_dir(dir, 0o755, None)?;
            }
            self.sys.atomic_write(&p.feeds, &bytes, KEY_FILE)?;
        }
        let updates = staging.join(format!("settings/{UPDATES_SOURCE}"));
        if updates.is_dir() {
            private_dir(&p.updates)?;
            self.manage
                .packages
                .restore_tree(&updates, &p.updates, None)
                .map_err(failed)?;
            report.restored.push("update choices and holds".into());
        }
        let kept = staging.join(format!("packages/{KEPT_SOURCE}"));
        if kept.is_dir() {
            private_dir(&p.kept)?;
            self.manage
                .packages
                .restore_tree(&kept, &p.kept, None)
                .map_err(failed)?;
        }

        // Every package's files first, so references between them resolve when each one's
        // settings are planned.
        let state = self.read_feeds()?;
        let mut ready = Vec::new();
        for (id, record) in &state.installed {
            match self.reinstall_restored(id) {
                Ok(how) => {
                    report.restored.push(format!("{id} {} {how}", record.version));
                    ready.push(id.clone());
                }
                Err(e) => report.skipped.push(format!("{id}: {e}")),
            }
        }
        for id in &ready {
            let feed = state.installed.get(id).map(|r| r.feed.clone());
            match self.install_package_held(id, Some(passphrase), feed.as_deref()) {
                Ok(_) => {}
                Err(e) => {
                    report.skipped.push(format!("{id} could not be set up: {e}"));
                    continue;
                }
            }
            let encrypted = self.package(id).map(|p| p.manifest.storage.encrypted).unwrap_or(false);
            if encrypted && c.storage.contains(id) {
                match self.fill_volume(staging, id) {
                    Ok(()) => report.restored.push(format!("{id} storage")),
                    Err(e) => report.skipped.push(format!("{id} storage: {e}")),
                }
            }
            if let Ok(package) = self.package(id) {
                if package.manifest.settings.is_some() {
                    if let Err(e) = self.render_settings(&package) {
                        report.skipped.push(format!("{id} settings could not be rendered: {e}"));
                    }
                }
            }
            if self.release_held(id) {
                report.restarted.push(id.clone());
            } else {
                report.skipped.push(format!("{id} did not start; its log says why"));
            }
        }
        if let Err(problem) = self.render_unlocked() {
            report.skipped.push(format!("some settings could not be written: {problem}"));
        }
        let _ = self.refresh_health_config();
        // Caddy and Tor, when installed, pick up the restored keys and certificates.
        for id in [CADDY, TOR] {
            let running = self
                .sys
                .sv_status(id)
                .map(|t| parse_sv_status(&t).is_running())
                .unwrap_or(false);
            if running && self.sys.sv(SvAction::ForceRestart, id, 30).is_ok() {
                report.restarted.push(id.to_owned());
            }
        }
        Ok(())
    }

    /// Puts a package from the backup back on the device: its kept verified file when the
    /// backup has it, else the same version from its feed. The kept file is inspected again
    /// and must ask for exactly what was approved.
    fn reinstall_restored(&self, id: &str) -> Result<&'static str, ApiError> {
        nodo_gen::validate::package_id(id).map_err(failed)?;
        let state = self.read_feeds()?;
        let record = state
            .installed
            .get(id)
            .ok_or_else(|| failed("not in the backup's feed records"))?;
        let approved = state
            .approved
            .get(id)
            .ok_or_else(|| failed("the backup has no approval for it"))?;
        let pkgver = format!("{id}-{}", record.version);
        let kept = record
            .kept
            .clone()
            .filter(|k| k.starts_with(self.kept_dir(id)) && self.manage.packages.kept_exists(k));
        let how = match kept {
            Some(kept) => {
                let (found, permissions) = self.manage.packages.inspect_kept(&kept).map_err(failed)?;
                if found != pkgver {
                    return Err(failed(format!("the kept file is {found}, not {pkgver}")));
                }
                if !permissions.added_since(approved).is_empty() {
                    return Err(failed("the kept file asks for more than was approved"));
                }
                self.manage.packages.reinstall(&kept).map_err(failed)?;
                "from the verified copy in the backup"
            }
            None => {
                let feed = state.feed(&record.feed)?;
                let mut pins = state.pins.clone();
                let (outcome, kept) = self
                    .manage
                    .packages
                    .install(&mut pins, &feed.address, id, approved, &pkgver, &self.kept_dir(id))
                    .map_err(failed)?;
                if !matches!(outcome, nodo_install::Outcome::Installed { .. }) {
                    return Err(failed(format!("{pkgver} from {} needs a new review", record.feed)));
                }
                self.with_feeds(|state| {
                    state.pins = pins.clone();
                    if let Some(r) = state.installed.get_mut(id) {
                        r.kept = kept.clone();
                    }
                    Ok(())
                })?;
                "from its feed"
            }
        };
        let manifest_path = self.app_dir(id)?.join(nodo_feed::MANIFEST_FILE);
        let bytes = self
            .sys
            .read_file(&manifest_path)?
            .ok_or_else(|| failed(format!("{} is missing", manifest_path.display())))?;
        let text = String::from_utf8(bytes).map_err(|_| failed("manifest is not UTF-8"))?;
        let manifest = nodo_manifest::Manifest::parse(&text).map_err(failed)?;
        if manifest.id != id {
            return Err(failed("the manifest names another package"));
        }
        self.place_manifest(id, &self.app_dir(id)?, &manifest).map_err(failed)?;
        Ok(how)
    }

    /// Starts a service installed with [`Helper::install_package_held`]: the `down` marker goes,
    /// then the supervisor, which may take a few seconds to notice a new service, starts it.
    fn release_held(&self, id: &str) -> bool {
        let _ = self.sys.remove_file(&self.paths.sv_dir.join(id).join("down"));
        for attempt in 0..15 {
            if attempt > 0 {
                self.sys.sleep(std::time::Duration::from_secs(1));
            }
            if self.sys.sv(SvAction::Start, id, 30).is_ok() {
                return true;
            }
        }
        false
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
        for bad in ["../x.nodobak", "x", ".nodobak", "a/b.nodobak", "nodo.toml", "-x .nodobak", ".nodo-1.nodobak.partial"] {
            assert!(!name_ok(bad), "{bad}");
        }
    }
}
