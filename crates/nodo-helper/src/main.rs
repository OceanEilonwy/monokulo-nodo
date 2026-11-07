//! `nodo-helper --allow-uid <uid> [--allow-uid <uid>...] [--socket-gid <gid>] [--socket <path>]`

#![forbid(unsafe_code)]

use std::fs;
use std::os::unix::fs::{chown, FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use nodo_helper::real::RealSystem;
use nodo_helper::{server, AllowedUids, FlatTomlSettings, Helper, NoCaddyYet};
use rustix::mm::{mlockall, MlockAllFlags};
use rustix::process::{set_dumpable_behavior, DumpableBehavior};

const DEFAULT_SOCKET: &str = "/run/nodo/helper.sock";

struct Options {
    allowed: Vec<u32>,
    socket: PathBuf,
    socket_gid: Option<u32>,
}

fn parse_options(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        allowed: Vec::new(),
        socket: DEFAULT_SOCKET.into(),
        socket_gid: None,
    };
    let mut args = args;
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--allow-uid" => {
                let uid = value()?.parse().map_err(|_| "--allow-uid needs a number")?;
                options.allowed.push(uid);
            }
            "--socket-gid" => {
                let gid = value()?
                    .parse()
                    .map_err(|_| "--socket-gid needs a number")?;
                options.socket_gid = Some(gid);
            }
            "--socket" => options.socket = value()?.into(),
            other => return Err(format!("unknown option: {other}")),
        }
    }
    if options.allowed.is_empty() {
        return Err("at least one --allow-uid is required".to_owned());
    }
    Ok(options)
}

/// Keeps the passphrase and keys out of swap and core dumps. Failure is logged, not fatal, since
/// a helper that will not start also keeps every encrypted service locked.
fn harden_process() {
    if let Err(e) = mlockall(MlockAllFlags::CURRENT | MlockAllFlags::FUTURE) {
        eprintln!("nodo-helper: could not lock memory: {e}");
    }
    if let Err(e) = set_dumpable_behavior(DumpableBehavior::NotDumpable) {
        eprintln!("nodo-helper: could not disable core dumps: {e}");
    }
}

/// Only ever removes an old socket, never a file that happens to have this name.
fn bind(path: &Path, gid: Option<u32>) -> std::io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_socket() {
            fs::remove_file(path)?;
        }
    }
    let listener = UnixListener::bind(path)?;
    // SO_PEERCRED is the real check. These permissions just narrow who can even connect.
    let mode = if gid.is_some() { 0o660 } else { 0o600 };
    chown(path, None, gid)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(listener)
}

fn main() -> ExitCode {
    let options = match parse_options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("nodo-helper: {e}");
            return ExitCode::from(2);
        }
    };
    harden_process();
    let listener = match bind(&options.socket, options.socket_gid) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!(
                "nodo-helper: cannot listen on {}: {e}",
                options.socket.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let helper = Helper::new(
        RealSystem,
        AllowedUids::new(options.allowed),
        Box::new(FlatTomlSettings),
        Box::new(NoCaddyYet),
    );
    if let Err(e) = server::serve(listener, Arc::new(helper)) {
        eprintln!("nodo-helper: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        parse_options(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn refuses_to_start_without_an_allowed_uid() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["--socket", "/tmp/x"]).is_err());
    }

    #[test]
    fn parses_every_option() {
        let options = parse(&[
            "--allow-uid",
            "1000",
            "--allow-uid",
            "1001",
            "--socket-gid",
            "5",
        ]);
        let options = options.ok().unwrap();
        assert_eq!(options.allowed, [1000, 1001]);
        assert_eq!(options.socket_gid, Some(5));
        assert_eq!(options.socket, PathBuf::from(DEFAULT_SOCKET));
    }

    #[test]
    fn rejects_bad_values_and_unknown_options() {
        assert!(parse(&["--allow-uid", "root"]).is_err());
        assert!(parse(&["--allow-uid"]).is_err());
        assert!(parse(&["--allow-uid", "1000", "--force"]).is_err());
    }

    #[test]
    fn bind_never_removes_a_file_that_is_not_a_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("helper.sock");
        fs::write(&path, b"keep").unwrap();
        assert!(bind(&path, None).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"keep");
    }

    #[test]
    fn bind_replaces_a_stale_socket_and_is_private_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("helper.sock");
        drop(UnixListener::bind(&path).unwrap());
        bind(&path, None).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
