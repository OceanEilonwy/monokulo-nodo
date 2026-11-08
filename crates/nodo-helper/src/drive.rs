//! The data drive: status, a read-only check, and a repair only after a check found damage.

use std::time::{Duration, Instant};

use nodo_api::{ApiError, DriveReport, DriveStatus, RepairConfirmation, Response};

use crate::helper::{lock, DriveCheck, DriveConfig, Helper};
use crate::system::System;

/// A repair must follow its check closely, so the user decides on what the drive looks like now.
const CHECK_VALID_FOR: Duration = Duration::from_secs(15 * 60);
const SUPPORTED: &[&str] = &["xfs", "ext4"];
const MAX_REPORT_LINES: usize = 200;

fn report_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .map(crate::info::sanitize_line)
        .take(MAX_REPORT_LINES)
        .collect()
}

impl<S: System> Helper<S> {
    fn drive_config(&self) -> Result<&DriveConfig, ApiError> {
        self.paths
            .drive
            .as_ref()
            .ok_or_else(|| ApiError::NotFound("this device has no data drive".to_owned()))
    }

    pub(crate) fn drive_status(&self) -> Option<DriveStatus> {
        let drive = self.paths.drive.as_ref()?;
        let present = self.sys.exists(&drive.device);
        let mounted = self.sys.is_mounted(&drive.mount_point);
        let (total_bytes, free_bytes) = if mounted {
            self.sys.fs_usage(&drive.mount_point).unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        Some(DriveStatus {
            label: drive
                .device
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            mount_point: drive.mount_point.display().to_string(),
            present,
            mounted,
            filesystem: if present {
                self.sys.fs_type(&drive.device)
            } else {
                None
            },
            total_bytes,
            free_bytes,
        })
    }

    /// Checking or repairing a mounted filesystem gives wrong answers or corrupts it, so both
    /// need the drive present, unmounted and of a known type.
    fn drive_ready_for_fsck(&self) -> Result<(&DriveConfig, String), ApiError> {
        let drive = self.drive_config()?;
        if !self.sys.exists(&drive.device) {
            return Err(ApiError::Failed(
                "the data drive is not connected".to_owned(),
            ));
        }
        if self.sys.is_mounted(&drive.mount_point) {
            return Err(ApiError::Failed(
                "the data drive is mounted, so it cannot be checked".to_owned(),
            ));
        }
        let fs = self.sys.fs_type(&drive.device).ok_or_else(|| {
            ApiError::Failed("no filesystem was found on the data drive".to_owned())
        })?;
        if !SUPPORTED.contains(&fs.as_str()) {
            return Err(ApiError::Failed(format!(
                "{fs} filesystems cannot be checked here"
            )));
        }
        Ok((drive, fs))
    }

    pub(crate) fn check_drive(&self) -> Result<Response, ApiError> {
        let _guard = lock(&self.service_lock);
        let (drive, fs) = self.drive_ready_for_fsck()?;
        let (clean, output) = self.sys.fs_check(&drive.device, &fs, false)?;
        let mut id = [0u8; 8];
        self.sys.random(&mut id)?;
        let check_id = u64::from_be_bytes(id);
        *lock(&self.last_drive_check) = Some(DriveCheck {
            id: check_id,
            clean,
            at: Instant::now(),
        });
        Ok(Response::DriveReport(DriveReport {
            check_id,
            clean,
            output: report_lines(&output),
        }))
    }

    /// Refused unless it quotes the most recent check, that check is recent and found damage,
    /// and the user accepted the risk. A check is used up by one repair.
    pub(crate) fn repair_drive(
        &self,
        confirmation: RepairConfirmation,
    ) -> Result<Response, ApiError> {
        let _guard = lock(&self.service_lock);
        if !confirmation.accept_data_loss_risk {
            return Err(ApiError::NeedsConfirmation(
                "a repair can lose data, so it must be confirmed".to_owned(),
            ));
        }
        let mut last = lock(&self.last_drive_check);
        match *last {
            Some(check) if check.id == confirmation.check_id => {
                if check.at.elapsed() > CHECK_VALID_FOR {
                    return Err(ApiError::NeedsConfirmation(
                        "that check is too old. Check the drive again first".to_owned(),
                    ));
                }
                if check.clean {
                    return Err(ApiError::Failed(
                        "the last check found no problems, so there is nothing to repair"
                            .to_owned(),
                    ));
                }
            }
            _ => {
                return Err(ApiError::NeedsConfirmation(
                    "check the drive first, then confirm the repair from that check".to_owned(),
                ))
            }
        }
        let (drive, fs) = self.drive_ready_for_fsck()?;
        *last = None;
        let (clean, output) = self.sys.fs_check(&drive.device, &fs, true)?;
        Ok(Response::DriveReport(DriveReport {
            check_id: confirmation.check_id,
            clean,
            output: report_lines(&output),
        }))
    }

    pub(crate) fn mount_drive(&self) -> Result<Response, ApiError> {
        let _guard = lock(&self.service_lock);
        let drive = self.drive_config()?;
        if !self.sys.is_mounted(&drive.mount_point) {
            self.sys.mount_fstab(&drive.mount_point)?;
        }
        Ok(Response::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem};

    const DEVICE: &str = "/dev/disk/by-partlabel/NODO_BLOCKCHAIN";

    fn setup(damaged: bool) -> Helper<FakeSystem> {
        let sys = FakeSystem::default();
        sys.add_device(DEVICE, "xfs", damaged);
        helper(sys)
    }

    fn check(h: &Helper<FakeSystem>) -> DriveReport {
        match h.check_drive() {
            Ok(Response::DriveReport(report)) => report,
            other => panic!("{other:?}"),
        }
    }

    fn repair(h: &Helper<FakeSystem>, check_id: u64, accept: bool) -> Result<Response, ApiError> {
        h.repair_drive(RepairConfirmation {
            check_id,
            accept_data_loss_risk: accept,
        })
    }

    fn repairs(h: &Helper<FakeSystem>) -> usize {
        h.sys
            .calls()
            .iter()
            .filter(|c| c.starts_with("fsck repair"))
            .count()
    }

    #[test]
    fn status_reports_presence_mount_and_filesystem() {
        let h = setup(false);
        let status = h.drive_status().unwrap();
        assert!(status.present);
        assert!(!status.mounted);
        assert_eq!(status.filesystem.as_deref(), Some("xfs"));
        assert_eq!(status.label, "NODO_BLOCKCHAIN");
    }

    #[test]
    fn repair_needs_a_recent_failed_check_and_explicit_consent() {
        let h = setup(true);
        assert!(matches!(
            repair(&h, 1, true),
            Err(ApiError::NeedsConfirmation(_))
        ));
        let report = check(&h);
        assert!(!report.clean);
        assert!(matches!(
            repair(&h, report.check_id, false),
            Err(ApiError::NeedsConfirmation(_))
        ));
        assert!(matches!(
            repair(&h, report.check_id.wrapping_add(1), true),
            Err(ApiError::NeedsConfirmation(_))
        ));
        assert_eq!(repairs(&h), 0);
        let Ok(Response::DriveReport(done)) = repair(&h, report.check_id, true) else {
            panic!()
        };
        assert!(done.clean);
        assert_eq!(repairs(&h), 1);
        // The check is used up.
        assert!(repair(&h, report.check_id, true).is_err());
        assert_eq!(repairs(&h), 1);
    }

    #[test]
    fn clean_drive_is_not_repaired() {
        let h = setup(false);
        let report = check(&h);
        assert!(report.clean);
        assert!(matches!(
            repair(&h, report.check_id, true),
            Err(ApiError::Failed(_))
        ));
        assert_eq!(repairs(&h), 0);
    }

    #[test]
    fn mounted_or_missing_drive_is_never_checked_or_repaired() {
        let h = setup(true);
        let report = check(&h);
        h.sys.set_mounted("/media/monero");
        assert!(h.check_drive().is_err());
        assert!(repair(&h, report.check_id, true).is_err());
        assert_eq!(repairs(&h), 0);

        let h = helper(FakeSystem::default());
        assert!(h.check_drive().is_err());
        assert!(!h.drive_status().unwrap().present);
    }

    #[test]
    fn unknown_filesystem_is_refused() {
        let sys = FakeSystem::default();
        sys.add_device(DEVICE, "ntfs", true);
        let h = helper(sys);
        assert!(h.check_drive().is_err());
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("fsck")));
    }

    #[test]
    fn mount_uses_fstab_and_is_idempotent() {
        let h = setup(false);
        h.mount_drive().unwrap();
        h.mount_drive().unwrap();
        let mounts = h
            .sys
            .calls()
            .iter()
            .filter(|c| *c == "mount /media/monero")
            .count();
        assert_eq!(mounts, 1);
    }
}
