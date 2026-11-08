use std::path::Path;

use nodo_api::{ApiError, Origin, Response, ServiceState, ServiceStatus};
use nodo_gen::volume_mount;

use crate::catalog::Package;
use crate::helper::{lock, Helper};
use crate::system::{SvAction, System};

/// A hostile `stop_timeout_secs` must not hold the service lock indefinitely.
const MAX_STOP_WAIT_SECS: u32 = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SvStatus {
    Running {
        uptime_secs: Option<u64>,
    },
    /// Not running, and the supervisor was told to keep it that way.
    Down,
    /// Not running, though the supervisor wants it up.
    Crashed,
    Unknown(String),
}

impl SvStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, SvStatus::Running { .. })
    }

    pub fn uptime_secs(&self) -> Option<u64> {
        match self {
            SvStatus::Running { uptime_secs } => *uptime_secs,
            _ => None,
        }
    }
}

/// Parses output such as `run: shop: (pid 12) 7s; run: log: (pid 9) 90s`. Only the part before
/// the semicolon describes the service, since the rest is its logger.
pub(crate) fn parse_sv_status(text: &str) -> SvStatus {
    let main = text
        .lines()
        .next()
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if main.starts_with("run: ") {
        let uptime_secs = main
            .split_once(") ")
            .and_then(|(_, rest)| rest.split_once('s'))
            .and_then(|(number, _)| number.parse().ok());
        SvStatus::Running { uptime_secs }
    } else if main.starts_with("down: ") {
        if main.contains("want up") {
            SvStatus::Crashed
        } else {
            SvStatus::Down
        }
    } else if main.starts_with("finish: ") {
        SvStatus::Crashed
    } else {
        SvStatus::Unknown(main.to_owned())
    }
}

/// Reads `DATA_MOUNT` from a service's `conf`, written by root at install time.
fn conf_data_mount(conf: &str) -> Option<&str> {
    conf.lines()
        .filter_map(|line| line.trim().strip_prefix("DATA_MOUNT="))
        .next_back()
        .map(|value| value.trim_matches(|c| c == '"' || c == '\''))
        .filter(|value| !value.is_empty())
}

impl<S: System> Helper<S> {
    pub(crate) fn stop_wait(package: &Package) -> u32 {
        package
            .manifest
            .stop_timeout_secs
            .clamp(1, MAX_STOP_WAIT_SECS)
    }

    pub(crate) fn is_locked(&self, package: &Package) -> Result<bool, ApiError> {
        if !package.manifest.storage.encrypted {
            return Ok(false);
        }
        let mount = volume_mount(package.id()).map_err(|e| ApiError::Failed(e.to_string()))?;
        Ok(!self.sys.is_mounted(Path::new(&mount)))
    }

    fn missing_data_mount(&self, package: &Package) -> Option<String> {
        let conf = self.paths.sv_dir.join(package.id()).join("conf");
        let bytes = self.sys.read_file(&conf).ok()??;
        let text = String::from_utf8(bytes).ok()?;
        let mount = conf_data_mount(&text)?;
        (!self.sys.is_mounted(Path::new(mount))).then(|| mount.to_owned())
    }

    /// Locked and waiting come first. The run script is alive during its retry delay, so `sv`
    /// says `run` for a service that has not actually started.
    fn status_of(&self, package: &Package) -> ServiceStatus {
        let (state, detail) = self.state_of(package);
        ServiceStatus {
            id: package.id().to_owned(),
            state,
            detail,
            roles: package.manifest.provides.clone(),
            origin: package.origin(),
        }
    }

    pub(crate) fn state_of(&self, package: &Package) -> (ServiceState, String) {
        let id = package.id().to_owned();
        let status = self.sys.sv_status(&id).map(|text| parse_sv_status(&text));
        if matches!(self.is_locked(package), Ok(true)) {
            return (
                ServiceState::Locked,
                "waiting for the device passphrase".to_owned(),
            );
        }
        let down = matches!(status, Ok(SvStatus::Down));
        if let (false, Some(mount)) = (down, self.missing_data_mount(package)) {
            return (
                ServiceState::Waiting,
                format!("waiting for {mount} to be mounted"),
            );
        }
        match status {
            Ok(SvStatus::Running {
                uptime_secs: Some(up),
            }) => (ServiceState::Running, format!("up for {up}s")),
            Ok(SvStatus::Running { uptime_secs: None }) => (ServiceState::Running, String::new()),
            Ok(SvStatus::Down) => (ServiceState::Stopped, String::new()),
            Ok(SvStatus::Crashed) => (ServiceState::Failed, "not running".to_owned()),
            Ok(SvStatus::Unknown(line)) => (ServiceState::Failed, line),
            Err(e) => (ServiceState::Failed, e.0),
        }
    }

    pub(crate) fn list_services(&self) -> Result<Response, ApiError> {
        let (packages, broken) = self.packages()?;
        let mut statuses: Vec<ServiceStatus> = packages
            .iter()
            .map(|package| self.status_of(package))
            .collect();
        statuses.extend(broken.into_iter().map(|b| ServiceStatus {
            id: b.id,
            state: ServiceState::Failed,
            detail: b.reason,
            roles: Vec::new(),
            origin: Origin::Feed(None),
        }));
        statuses.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Response::Services(statuses))
    }

    pub(crate) fn start_service(&self, id: &str) -> Result<Response, ApiError> {
        self.service_action(SvAction::Start, id)
    }

    pub(crate) fn stop_service(&self, id: &str) -> Result<Response, ApiError> {
        self.service_action(SvAction::ForceStop, id)
    }

    pub(crate) fn restart_service(&self, id: &str) -> Result<Response, ApiError> {
        self.service_action(SvAction::ForceRestart, id)
    }

    fn service_action(&self, action: SvAction, id: &str) -> Result<Response, ApiError> {
        let package = self.package(id)?;
        let _guard = lock(&self.service_lock);
        self.sys
            .sv(action, package.id(), Self::stop_wait(&package))?;
        Ok(Response::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, ENCRYPTED, SHOP};

    fn parsed(text: &str) -> SvStatus {
        parse_sv_status(text)
    }

    #[test]
    fn parses_real_sv_output() {
        assert_eq!(
            parsed("run: shop: (pid 12) 7s; run: log: (pid 9) 90s\n"),
            SvStatus::Running {
                uptime_secs: Some(7)
            }
        );
        assert_eq!(
            parsed("run: shop: (pid 12) 7s, want down, got TERM\n"),
            SvStatus::Running {
                uptime_secs: Some(7)
            }
        );
        assert_eq!(parsed("down: shop: 9s, normally up\n"), SvStatus::Down);
        assert_eq!(
            parsed("down: shop: 1s, normally up, want up\n"),
            SvStatus::Crashed
        );
        assert_eq!(
            parsed("finish: shop: (pid 3) 0s, want up\n"),
            SvStatus::Crashed
        );
        assert!(matches!(
            parsed("fail: shop: unable to change to service directory: file does not exist\n"),
            SvStatus::Unknown(_)
        ));
    }

    /// The logger's state after the semicolon must not be read as the service's.
    #[test]
    fn logger_state_is_ignored() {
        assert_eq!(
            parsed("down: shop: 9s, normally up; run: log: (pid 9) 90s"),
            SvStatus::Down
        );
    }

    #[test]
    fn start_stop_restart_use_the_manifest_stop_timeout() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        let h = helper(sys);
        h.start_service("shop").unwrap();
        h.stop_service("shop").unwrap();
        h.restart_service("shop").unwrap();
        let calls: Vec<String> = h
            .sys
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("sv "))
            .collect();
        assert_eq!(
            calls,
            [
                "sv start shop -w30",
                "sv force-stop shop -w30",
                "sv force-restart shop -w30"
            ]
        );
    }

    #[test]
    fn app_cannot_load_as_a_reserved_system_package() {
        let sys = FakeSystem::default();
        sys.put(
            "/m/app/cuprated.toml",
            b"api_version = 1\nid = \"cuprated\"\nversion = \"1\"\nexec = [\"bin/fake\"]\n",
        );
        let h = helper(sys);
        assert!(h.stop_service("cuprated").is_err());
        assert!(h.sys.calls().iter().all(|call| !call.starts_with("sv ")));
    }

    #[test]
    fn hostile_stop_timeout_is_clamped() {
        let sys = FakeSystem::default();
        sys.put(
            "/m/sys/slow.toml",
            b"api_version = 1\nid = \"slow\"\nversion = \"1\"\nexec = [\"a\"]\nstop_timeout_secs = 4294967295\n",
        );
        let h = helper(sys);
        h.stop_service("slow").unwrap();
        assert!(h
            .sys
            .calls()
            .contains(&"sv force-stop slow -w600".to_owned()));
    }

    #[test]
    fn unknown_or_base_services_are_not_found() {
        let h = helper(FakeSystem::default());
        for id in ["sshd", "../sshd", "-x"] {
            assert!(
                matches!(h.stop_service(id), Err(ApiError::NotFound(_))),
                "{id}"
            );
        }
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("sv ")));
    }

    #[test]
    fn sv_failure_is_reported() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.fail("sv start");
        assert!(matches!(
            helper(sys).start_service("shop"),
            Err(ApiError::Failed(_))
        ));
    }

    fn states(h: &Helper<FakeSystem>) -> Vec<(String, ServiceState)> {
        match h.list_services().unwrap() {
            Response::Services(list) => list.into_iter().map(|s| (s.id, s.state)).collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn listing_covers_every_state() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.put("/m/app/gone.toml", b"not a manifest");
        sys.add_manifest(ENCRYPTED);
        let h = helper(sys);
        assert_eq!(
            states(&h),
            [
                ("gone".to_owned(), ServiceState::Failed),
                ("shop".to_owned(), ServiceState::Running),
                ("vault".to_owned(), ServiceState::Locked),
            ]
        );
        h.sys.set_status("shop", "down: shop: 9s, normally up\n");
        assert_eq!(states(&h)[1].1, ServiceState::Stopped);
        h.sys
            .set_status("shop", "down: shop: 1s, normally up, want up\n");
        assert_eq!(states(&h)[1].1, ServiceState::Failed);
    }

    /// The run script sleeps before exiting, so `sv` reports `run` the whole time it waits.
    #[test]
    fn locked_and_waiting_win_over_a_running_status() {
        let sys = FakeSystem::default();
        sys.add_manifest(ENCRYPTED);
        sys.add_manifest(SHOP);
        sys.put("/sv/shop/conf", b"DATA_MOUNT=/mnt/data\n");
        let h = helper(sys);
        let list = states(&h);
        assert_eq!(list[0].1, ServiceState::Waiting);
        assert_eq!(list[1].1, ServiceState::Locked);
        h.sys.set_mounted("/mnt/data");
        h.sys.set_mounted("/run/nodo/volumes/vault");
        let list = states(&h);
        assert_eq!(list[0].1, ServiceState::Running);
        assert_eq!(list[1].1, ServiceState::Running);
    }

    #[test]
    fn a_service_stopped_on_purpose_is_not_waiting() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.put("/sv/shop/conf", b"DATA_MOUNT=/mnt/data\n");
        sys.set_status("shop", "down: shop: 9s, normally up\n");
        assert_eq!(states(&helper(sys))[0].1, ServiceState::Stopped);
    }
}
