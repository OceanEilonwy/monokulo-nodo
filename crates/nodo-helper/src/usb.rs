//! Backups on removable USB drives.
//!
//! Only a drive the kernel reports as removable, or one attached over USB, is ever used. A drive
//! is refused when any filesystem on it is mounted elsewhere, a device mapper or swap holds it,
//! it carries the `NODO_BLOCKCHAIN` label, or it is the configured data drive. That keeps the
//! system disk and the chain data out of reach whatever a request names.
//!
//! Each request mounts the drive on a private mount point with `nosuid,nodev,noexec`, as one of
//! the filesystems listed in [`SUPPORTED`], does its work and unmounts again. Reading is always
//! read-only. A backup is written under a hidden temporary name, synced, renamed and the folder
//! synced, so a drive pulled out half-way never holds a partial backup under a real name.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use nodo_api::{ApiError, BackupContents, BackupInfo, Response, Secret, UsbBackupList, UsbDrive};

use crate::backup::{name_ok, Written};
use crate::helper::{lock, Helper};
use crate::system::System;

/// Filesystems backups may use. FAT32 and exFAT are what drives come formatted with.
pub const SUPPORTED: &[&str] = &["vfat", "exfat", "ext4"];
/// The folder on the drive that holds backups. Nothing outside it is read or written.
pub const USB_DIR: &str = "monokulo-nodo";
const DATA_DRIVE_LABEL: &str = "NODO_BLOCKCHAIN";
/// The largest file FAT32 can hold.
const FAT_MAX_FILE: u64 = 4 * 1024 * 1024 * 1024 - 1;
/// Room left for directory entries and the format's own overhead.
const SPACE_MARGIN: u64 = 4 * 1024 * 1024;

const ENXIO: i32 = 6;
const EIO: i32 = 5;
const ENODEV: i32 = 19;
const EFBIG: i32 = 27;
const ENOSPC: i32 = 28;
const EROFS: i32 = 30;
const ESHUTDOWN: i32 = 108;
const EDQUOT: i32 = 122;
const ENOMEDIUM: i32 = 123;

pub const FULL: &str = "The USB drive is full";
pub const READ_ONLY: &str =
    "The USB drive is read-only. Check its write-protect switch, or use another drive.";
pub const UNPLUGGED: &str = "The USB drive was unplugged or stopped answering while the backup \
     was written. No backup was kept on it. Plug it in and try again.";

/// One partition, or a disk without partitions, as the kernel and `blkid` describe it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockInfo {
    pub name: String,
    /// The whole disk it is on, which is `name` itself for a disk without partitions.
    pub disk: String,
    pub removable: bool,
    /// Attached over USB, which covers USB disks that do not call themselves removable.
    pub usb: bool,
    pub read_only: bool,
    pub size_bytes: u64,
    /// The GPT partition name.
    pub partlabel: Option<String>,
    pub fs_type: Option<String>,
    pub label: Option<String>,
    pub model: String,
    /// Mounted anywhere but the backup mount point, held by a mapping, or used as swap.
    pub in_use: bool,
}

/// What the helper needs from the machine for removable drives, so the rules are tested
/// without hardware.
pub trait RemovableMedia: Send + Sync {
    fn scan(&self, own_mount: &Path) -> Vec<BlockInfo>;
    fn mount(&self, info: &BlockInfo, target: &Path, read_only: bool) -> Result<(), String>;
    /// `lazy` detaches a mount whose device has gone away.
    fn unmount(&self, target: &Path, lazy: bool) -> Result<(), String>;
    /// Whether the kernel still has the device, which tells an unplugged drive apart.
    fn present(&self, name: &str) -> bool;
}

/// Why backups cannot use this device, or `None` when they can.
pub fn refusal(info: &BlockInfo, all: &[BlockInfo], data_drive: Option<&str>) -> Option<String> {
    if !(info.removable || info.usb) {
        return Some(format!("{} is not a removable drive", info.name));
    }
    let same_disk = || all.iter().filter(|b| b.disk == info.disk);
    let data_disk = data_drive.and_then(|d| all.iter().find(|b| b.name == d || b.disk == d)).map(|b| &b.disk);
    if data_disk == Some(&info.disk)
        || same_disk().any(|b| {
            b.partlabel.as_deref() == Some(DATA_DRIVE_LABEL) || b.label.as_deref() == Some(DATA_DRIVE_LABEL)
        })
    {
        return Some("this is the data drive, which is never used for backups".to_owned());
    }
    if same_disk().any(|b| b.in_use) {
        return Some("a filesystem on this drive is in use by the system, so it is left alone".to_owned());
    }
    match info.fs_type.as_deref() {
        None => Some("no filesystem was found. Format it as FAT32, exFAT or ext4 on a computer first.".to_owned()),
        Some(fs) if !SUPPORTED.contains(&fs) => {
            Some(format!("its {fs} filesystem is not supported. Use FAT32, exFAT or ext4."))
        }
        Some(_) => None,
    }
}

/// The plain sentence for a failed write to the drive.
pub fn write_problem(errno: Option<i32>, unplugged: bool, detail: &str) -> String {
    if unplugged {
        return UNPLUGGED.to_owned();
    }
    match errno {
        Some(ENOSPC | EDQUOT) => format!("{FULL}. Free some space on it, or use a larger drive."),
        Some(EFBIG) => format!("{FULL} for one file this size: FAT32 holds at most 4 GB per file. Use exFAT or ext4."),
        Some(EROFS) => READ_ONLY.to_owned(),
        Some(EIO | ENXIO | ENODEV | ESHUTDOWN | ENOMEDIUM) => UNPLUGGED.to_owned(),
        _ => format!("Writing to the USB drive failed: {detail}"),
    }
}

fn to_api(info: &BlockInfo, problem: Option<String>) -> UsbDrive {
    UsbDrive {
        device: info.name.clone(),
        label: info.label.clone(),
        model: info.model.clone(),
        filesystem: info.fs_type.clone(),
        size_bytes: info.size_bytes,
        read_only: info.read_only,
        problem,
    }
}

fn device_name_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= 32 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Mounted until dropped, or until [`Mounted::finish`] reports how the unmount went.
pub(crate) struct Mounted<'a> {
    media: &'a dyn RemovableMedia,
    target: PathBuf,
    active: bool,
}

impl Mounted<'_> {
    pub fn path(&self) -> &Path {
        &self.target
    }

    fn finish(mut self) -> Result<(), ApiError> {
        self.active = false;
        if self.media.unmount(&self.target, false).is_ok() {
            return Ok(());
        }
        self.media
            .unmount(&self.target, true)
            .map_err(|e| ApiError::Failed(format!("the USB drive could not be unmounted: {e}")))
    }
}

impl Drop for Mounted<'_> {
    fn drop(&mut self) {
        if self.active && self.media.unmount(&self.target, false).is_err() {
            let _ = self.media.unmount(&self.target, true);
        }
    }
}

/// Opens a file on the drive without following a link.
fn open_nofollow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

/// The backup folder, which must be a real directory and not a link someone put there.
fn backup_dir(root: &Path, create: bool) -> Result<Option<PathBuf>, ApiError> {
    let dir = root.join(USB_DIR);
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => Ok(Some(dir)),
        Ok(_) => Err(ApiError::Failed(format!("{USB_DIR} on the USB drive is not a folder"))),
        Err(e) if e.kind() == io::ErrorKind::NotFound && create => {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .map_err(|e| ApiError::Failed(write_problem(e.raw_os_error(), false, &e.to_string())))?;
            Ok(Some(dir))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ApiError::Failed(format!("the USB drive could not be read: {e}"))),
    }
}

fn list_dir(dir: &Path) -> Vec<BackupInfo> {
    let mut list = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        let Ok(meta) = entry.path().symlink_metadata() else { continue };
        if name_ok(&name) && meta.is_file() {
            list.push(BackupInfo { name, size_bytes: meta.len(), created: meta.mtime().max(0) as u64 });
        }
    }
    list.sort_by(|a, b| b.created.cmp(&a.created).then(b.name.cmp(&a.name)));
    list
}

impl<S: System> Helper<S> {
    fn usb_scan(&self) -> (Vec<BlockInfo>, Option<String>) {
        let all = self.manage.media.scan(&self.manage.paths.usb_mount);
        let data_drive = self.paths.drive.as_ref().and_then(|d| {
            let real = fs::canonicalize(&d.device).unwrap_or_else(|_| d.device.clone());
            real.file_name().and_then(|n| n.to_str()).map(str::to_owned)
        });
        (all, data_drive)
    }

    pub(crate) fn list_usb_drives(&self) -> Result<Response, ApiError> {
        let (all, data_drive) = self.usb_scan();
        let drives = all
            .iter()
            .filter(|b| b.removable || b.usb)
            .map(|b| to_api(b, refusal(b, &all, data_drive.as_deref())))
            .collect();
        Ok(Response::UsbDrives(drives))
    }

    /// The named drive, checked again now: a request may quote a name from an earlier list.
    fn usb_drive(&self, device: &str) -> Result<BlockInfo, ApiError> {
        if !device_name_ok(device) {
            return Err(ApiError::NotFound("invalid drive name".to_owned()));
        }
        let (all, data_drive) = self.usb_scan();
        let info = all
            .iter()
            .find(|b| b.name == device)
            .ok_or_else(|| ApiError::NotFound(format!("no drive named {device} is connected")))?;
        match refusal(info, &all, data_drive.as_deref()) {
            Some(reason) => Err(ApiError::Failed(format!("{device} cannot be used: {reason}"))),
            None => Ok(info.clone()),
        }
    }

    fn mount_usb(&self, info: &BlockInfo, read_only: bool) -> Result<Mounted<'_>, ApiError> {
        let target = self.manage.paths.usb_mount.clone();
        // Left mounted when the helper stopped part-way or the drive was pulled out.
        if self.sys.is_mounted(&target) {
            let _ = self.manage.media.unmount(&target, true);
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&target)
            .map_err(|e| ApiError::Failed(e.to_string()))?;
        self.manage.media.mount(info, &target, read_only).map_err(|e| {
            if e.contains("write-protected") || e.contains("read-only") {
                ApiError::Failed(READ_ONLY.to_owned())
            } else {
                ApiError::Failed(format!("the USB drive could not be mounted: {e}"))
            }
        })?;
        Ok(Mounted { media: self.manage.media.as_ref(), target, active: true })
    }

    pub(crate) fn list_usb_backups(&self, device: &str) -> Result<Response, ApiError> {
        let _work = lock(&self.manage.work);
        let info = self.usb_drive(device)?;
        let mounted = self.mount_usb(&info, true)?;
        let backups = backup_dir(mounted.path(), false)?.map(|d| list_dir(&d)).unwrap_or_default();
        let free = self.manage.packages.free_bytes(mounted.path());
        mounted.finish()?;
        Ok(Response::UsbBackups(UsbBackupList {
            device: device.to_owned(),
            backups,
            free_bytes: free,
            read_only: info.read_only,
        }))
    }

    pub(crate) fn create_usb_backup(
        &self,
        device: &str,
        device_passphrase: Option<&Secret>,
        backup_passphrase: &Secret,
    ) -> Result<Response, ApiError> {
        self.check_backup_passphrase(backup_passphrase)?;
        let device_key = self.device_key_for_backup(device_passphrase)?;
        let _work = lock(&self.manage.work);
        let info = self.usb_drive(device)?;
        if info.read_only {
            return Err(ApiError::Failed(READ_ONLY.to_owned()));
        }
        let (plan, locked) = self.backup_plan(device_key)?;
        let needed = self.backup_estimate(&plan);
        let mounted = self.mount_usb(&info, false)?;
        let dir = backup_dir(mounted.path(), true)?.expect("created");
        // An earlier write that was cut short leaves its temporary file. It is never a backup.
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".nodo-") && name.ends_with(".partial") {
                let _ = fs::remove_file(entry.path());
            }
        }
        let free = self.manage.packages.free_bytes(mounted.path());
        if info.fs_type.as_deref() == Some("vfat") && needed > FAT_MAX_FILE {
            return Err(ApiError::Failed(format!(
                "This backup needs about {} MB, more than the 4 GB one FAT32 file can hold. Use a drive \
                 formatted as exFAT or ext4.",
                needed / 1_000_000
            )));
        }
        if needed.saturating_add(SPACE_MARGIN) > free {
            return Err(ApiError::Failed(format!(
                "{FULL}: this backup needs about {} MB and {} MB are free.",
                needed.div_ceil(1_000_000),
                free / 1_000_000
            )));
        }
        let now = self.manage.packages.now();
        let written = self.write_backup_file(&dir, &plan, backup_passphrase, now).map_err(|failure| {
            let unplugged = !self.manage.media.present(&info.name);
            if unplugged || failure.errno.is_some() {
                ApiError::Failed(write_problem(failure.errno, unplugged, &failure.error.to_string()))
            } else {
                failure.error
            }
        });
        let written = match written {
            Ok(w) => w,
            Err(e) => {
                drop(mounted);
                return Err(e);
            }
        };
        mounted.finish()?;
        Ok(written.into_response(locked))
    }

    fn open_usb_backup(&self, mounted: &Mounted<'_>, name: &str) -> Result<File, ApiError> {
        if !name_ok(name) {
            return Err(ApiError::NotFound("invalid backup name".to_owned()));
        }
        let dir = backup_dir(mounted.path(), false)?
            .ok_or_else(|| ApiError::NotFound(format!("the USB drive has no {USB_DIR} folder")))?;
        open_nofollow(&dir.join(name)).map_err(|_| ApiError::NotFound(name.to_owned()))
    }

    pub(crate) fn inspect_usb_backup(&self, device: &str, name: &str, passphrase: &Secret) -> Result<Response, ApiError> {
        let _work = lock(&self.manage.work);
        let info = self.usb_drive(device)?;
        let mounted = self.mount_usb(&info, true)?;
        let file = self.open_usb_backup(&mounted, name)?;
        let summary = nodo_backup::verify(BufReader::new(file), passphrase.expose().as_bytes())
            .map_err(|e| ApiError::Failed(e.to_string()))?;
        mounted.finish()?;
        let contents: BackupContents = self.contents_of(name, &summary)?;
        Ok(Response::BackupContents(contents))
    }

    pub(crate) fn restore_usb_backup(
        &self,
        device: &str,
        name: &str,
        passphrase: &Secret,
        new_device_passphrase: Option<&Secret>,
        typed: &str,
    ) -> Result<Response, ApiError> {
        Self::check_restore_request(new_device_passphrase, typed)?;
        let _work = lock(&self.manage.work);
        let info = self.usb_drive(device)?;
        let mounted = self.mount_usb(&info, true)?;
        let file = self.open_usb_backup(&mounted, name)?;
        // Verified and staged while mounted. The drive is free again before anything is placed.
        let staged = self.stage_restore(file, passphrase);
        let unmounted = mounted.finish();
        let staged = staged?;
        let result = self.apply_staged(&staged, new_device_passphrase);
        let _ = fs::remove_dir_all(&staged.dir);
        unmounted?;
        result.map(Response::Restored)
    }
}

impl Written {
    fn into_response(self, locked: Vec<String>) -> Response {
        Response::BackupCreated(nodo_api::BackupCreated {
            info: BackupInfo { name: self.name, size_bytes: self.size, created: self.created },
            entries: self.summary.entries.len() as u32,
            has_device_key: self.summary.has_device_key(),
            missing: self.summary.missing.iter().map(|p| p.display().to_string()).collect(),
            locked,
        })
    }
}

/// The real machine: sysfs, `/proc/self/mountinfo`, `blkid`, `mount` and `umount`.
pub struct RealMedia;

fn read_trim(path: &Path) -> String {
    fs::read_to_string(path).map(|s| s.trim().to_owned()).unwrap_or_default()
}

fn run(program: &str, args: &[OsString]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/usr/sbin:/bin:/sbin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let text: String = String::from_utf8_lossy(&out.stderr).trim().chars().take(300).collect();
        Err(format!("{program}: {text}"))
    }
}

/// `maj:min` of every mounted device, with where it is mounted.
fn mounted_devices() -> Vec<(String, String)> {
    let info = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    info.lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let dev = fields.nth(2)?;
            let _root = fields.next()?;
            let point = fields.next()?;
            Some((dev.to_owned(), point.replace("\\040", " ")))
        })
        .collect()
}

/// Parses `blkid -o export` output.
pub fn parse_blkid(text: &str) -> (Option<String>, Option<String>, Option<String>) {
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            .map(|v| v.chars().filter(|c| !c.is_control()).take(64).collect::<String>())
            .filter(|v| !v.is_empty())
    };
    (get("TYPE"), get("LABEL"), get("PART_ENTRY_NAME"))
}

impl RemovableMedia for RealMedia {
    fn scan(&self, own_mount: &Path) -> Vec<BlockInfo> {
        let mounts = mounted_devices();
        let swaps = fs::read_to_string("/proc/swaps").unwrap_or_default();
        let own = own_mount.to_string_lossy();
        let mut out = Vec::new();
        let Ok(disks) = fs::read_dir("/sys/block") else { return out };
        for disk in disks.flatten() {
            let disk_name = disk.file_name().to_string_lossy().into_owned();
            // Virtual devices, optical drives and SD or eMMC slots, which hold the boot media on
            // these boards, are never candidates.
            if ["loop", "ram", "zram", "dm-", "md", "sr", "nbd", "mmcblk", "fd"]
                .iter()
                .any(|p| disk_name.starts_with(p))
                || !device_name_ok(&disk_name)
            {
                continue;
            }
            let sys = PathBuf::from("/sys/block").join(&disk_name);
            let removable = read_trim(&sys.join("removable")) == "1";
            let usb = fs::canonicalize(&sys).map(|p| p.to_string_lossy().contains("/usb")).unwrap_or(false);
            let disk_ro = read_trim(&sys.join("ro")) == "1";
            let model = format!("{} {}", read_trim(&sys.join("device/vendor")), read_trim(&sys.join("device/model")))
                .trim()
                .to_owned();
            let mut parts: Vec<String> = fs::read_dir(&sys)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(&disk_name) && sys.join(n).join("partition").exists())
                .collect();
            parts.sort();
            let used = |dir: &Path, name: &str| {
                let dev = read_trim(&dir.join("dev"));
                let mounted = mounts.iter().any(|(d, point)| *d == dev && point != own.as_ref());
                let held = fs::read_dir(dir.join("holders")).map(|mut h| h.next().is_some()).unwrap_or(false);
                let swap = swaps.lines().skip(1).any(|l| l.split_whitespace().next() == Some(&format!("/dev/{name}")));
                mounted || held || swap
            };
            let disk_used = used(&sys, &disk_name);
            let names = if parts.is_empty() { vec![disk_name.clone()] } else { parts };
            for name in names {
                let dir = if name == disk_name { sys.clone() } else { sys.join(&name) };
                let size = read_trim(&dir.join("size")).parse::<u64>().unwrap_or(0) * 512;
                let blkid = run("blkid", &["-p".into(), "-o".into(), "export".into(), format!("/dev/{name}").into()])
                    .unwrap_or_default();
                let (fs_type, label, partlabel) = parse_blkid(&blkid);
                out.push(BlockInfo {
                    in_use: disk_used || used(&dir, &name),
                    read_only: disk_ro || read_trim(&dir.join("ro")) == "1",
                    disk: disk_name.clone(),
                    name,
                    removable,
                    usb,
                    size_bytes: size,
                    partlabel,
                    fs_type,
                    label,
                    model: model.clone(),
                });
            }
        }
        out
    }

    fn mount(&self, info: &BlockInfo, target: &Path, read_only: bool) -> Result<(), String> {
        let fs_type = info.fs_type.as_deref().filter(|f| SUPPORTED.contains(f)).ok_or("unsupported filesystem")?;
        let mut options = String::from("nosuid,nodev,noexec,noatime");
        // FAT has no owners, so everything on it belongs to root and only root can read it.
        if matches!(fs_type, "vfat" | "exfat") {
            options.push_str(",uid=0,gid=0,fmask=0177,dmask=0077");
        }
        options.push_str(if read_only { ",ro" } else { ",rw" });
        run(
            "mount",
            &[
                "-t".into(),
                fs_type.into(),
                "-o".into(),
                options.into(),
                format!("/dev/{}", info.name).into(),
                target.into(),
            ],
        )
        .map(|_| ())
    }

    fn unmount(&self, target: &Path, lazy: bool) -> Result<(), String> {
        let mut args: Vec<OsString> = Vec::new();
        if lazy {
            args.push("-l".into());
        }
        args.push(target.into());
        run("umount", &args).map(|_| ())
    }

    fn present(&self, name: &str) -> bool {
        Path::new("/sys/class/block").join(name).exists()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct FakeMedia(pub Arc<Mutex<FakeMediaState>>);

    #[derive(Default)]
    pub struct FakeMediaState {
        pub devices: Vec<BlockInfo>,
        pub calls: Vec<String>,
        pub mounted: bool,
        pub fail_mount: Option<String>,
        pub gone: bool,
    }

    impl RemovableMedia for FakeMedia {
        fn scan(&self, _own: &Path) -> Vec<BlockInfo> {
            self.0.lock().unwrap().devices.clone()
        }
        fn mount(&self, info: &BlockInfo, target: &Path, read_only: bool) -> Result<(), String> {
            let mut s = self.0.lock().unwrap();
            s.calls.push(format!("mount {} {} ro={read_only}", info.name, target.display()));
            if let Some(e) = &s.fail_mount {
                return Err(e.clone());
            }
            s.mounted = true;
            Ok(())
        }
        fn unmount(&self, target: &Path, lazy: bool) -> Result<(), String> {
            let mut s = self.0.lock().unwrap();
            s.calls.push(format!("umount {} lazy={lazy}", target.display()));
            s.mounted = false;
            Ok(())
        }
        fn present(&self, _name: &str) -> bool {
            !self.0.lock().unwrap().gone
        }
    }

    pub fn stick() -> BlockInfo {
        BlockInfo {
            name: "sdb1".into(),
            disk: "sdb".into(),
            removable: true,
            usb: true,
            fs_type: Some("vfat".into()),
            label: Some("BACKUP".into()),
            model: "SanDisk Ultra".into(),
            size_bytes: 32_000_000_000,
            ..BlockInfo::default()
        }
    }

    fn system_disk() -> BlockInfo {
        BlockInfo {
            name: "vda1".into(),
            disk: "vda".into(),
            fs_type: Some("ext4".into()),
            in_use: true,
            ..BlockInfo::default()
        }
    }

    #[test]
    fn only_removable_unused_drives_with_a_supported_filesystem_are_usable() {
        let all = vec![stick(), system_disk()];
        assert_eq!(refusal(&stick(), &all, None), None);
        assert!(refusal(&system_disk(), &all, None).unwrap().contains("not a removable drive"));
        let ntfs = BlockInfo { fs_type: Some("ntfs".into()), ..stick() };
        assert!(refusal(&ntfs, std::slice::from_ref(&ntfs), None).unwrap().contains("not supported"));
        let blank = BlockInfo { fs_type: None, ..stick() };
        assert!(refusal(&blank, std::slice::from_ref(&blank), None).unwrap().contains("no filesystem"));
    }

    #[test]
    fn a_usb_disk_that_holds_a_mounted_filesystem_is_never_used() {
        // A USB-attached disk booted from, or with another partition mounted.
        let other = BlockInfo { name: "sdb2".into(), in_use: true, ..stick() };
        let all = vec![stick(), other];
        assert!(refusal(&stick(), &all, None).unwrap().contains("in use by the system"));
    }

    #[test]
    fn the_data_drive_is_refused_by_label_and_by_configuration() {
        let chain = BlockInfo { name: "sdb2".into(), partlabel: Some("NODO_BLOCKCHAIN".into()), ..stick() };
        assert!(refusal(&stick(), &[stick(), chain], None).unwrap().contains("data drive"));
        let labelled = BlockInfo { label: Some("NODO_BLOCKCHAIN".into()), ..stick() };
        assert!(refusal(&labelled, std::slice::from_ref(&labelled), None).unwrap().contains("data drive"));
        assert!(refusal(&stick(), &[stick()], Some("sdb1")).unwrap().contains("data drive"));
        assert!(refusal(&stick(), &[stick()], Some("sdb")).unwrap().contains("data drive"));
    }

    #[test]
    fn write_failures_get_plain_sentences() {
        assert!(write_problem(Some(28), false, "").starts_with(FULL));
        assert_eq!(write_problem(Some(30), false, ""), READ_ONLY);
        assert_eq!(write_problem(Some(5), false, ""), UNPLUGGED);
        assert_eq!(write_problem(None, true, "x"), UNPLUGGED);
        assert!(write_problem(Some(27), false, "").contains("4 GB"));
        assert!(write_problem(Some(1), false, "denied").contains("denied"));
    }

    #[test]
    fn drive_names_are_kernel_names_only() {
        for good in ["sda", "sdb1", "nvme0n1p1"] {
            assert!(device_name_ok(good), "{good}");
        }
        for bad in ["", "../sda", "sda 1", "/dev/sda", "SDA"] {
            assert!(!device_name_ok(bad), "{bad}");
        }
    }

    #[test]
    fn blkid_output_is_read_by_key() {
        let (fs, label, part) = parse_blkid("DEVNAME=/dev/sdb1\nLABEL=BACKUP\nTYPE=vfat\nPART_ENTRY_NAME=x\n");
        assert_eq!((fs.as_deref(), label.as_deref(), part.as_deref()), (Some("vfat"), Some("BACKUP"), Some("x")));
        assert_eq!(parse_blkid(""), (None, None, None));
    }
}
