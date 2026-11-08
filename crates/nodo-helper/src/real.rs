//! The real machine. Programs run with a fixed environment and argument vectors, never a shell.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::os::unix::fs::{chown, DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rustix::fs::{fallocate, FallocateFlags};

use crate::system::{Probe, SvAction, SysError, SysResult, System, WriteOptions};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ERROR_LEN: usize = 512;
/// The account settings code runs as. Created by the package that installs the helper.
pub const SETTINGS_USER: &str = "_nodo_settings";
const WORKER_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TOOL_OUTPUT: u64 = 64 * 1024;

/// `pbkdf2` at the minimum cost, because a volume key is 64 random bytes and has nothing to
/// stretch. Argon2 here would make opening many volumes at once take far longer, and need its
/// memory once per volume, to protect a key no one can guess.
pub fn luks_format_args(file: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "luksFormat",
        "--batch-mode",
        "--type",
        "luks2",
        "--cipher",
        "aes-xts-plain64",
        "--key-size",
        "512",
        "--hash",
        "sha256",
        "--pbkdf",
        "pbkdf2",
        "--pbkdf-force-iterations",
        "1000",
        "--key-file=-",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    args.push(file.as_os_str().to_owned());
    args
}

pub fn luks_open_args(file: &Path, name: &str) -> Vec<OsString> {
    vec![
        "open".into(),
        "--type".into(),
        "luks2".into(),
        "--key-file=-".into(),
        file.as_os_str().to_owned(),
        name.into(),
    ]
}

fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear()
        .env("PATH", "/usr/bin:/usr/sbin:/bin:/sbin")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn failure(program: &str, stderr: &[u8], stdout: &[u8]) -> SysError {
    let source = if stderr.iter().all(u8::is_ascii_whitespace) {
        stdout
    } else {
        stderr
    };
    let text = String::from_utf8_lossy(source);
    let text: String = text.trim().chars().take(MAX_ERROR_LEN).collect();
    SysError(format!("{program}: {text}"))
}

fn run(program: &str, mut cmd: Command) -> SysResult<()> {
    let out = cmd
        .output()
        .map_err(|e| SysError(format!("{program}: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(failure(program, &out.stderr, &out.stdout))
    }
}

/// Runs a program with a key on stdin, the only channel that is not visible to other users.
fn run_with_key<I, A>(program: &str, args: I, key: &[u8]) -> SysResult<()>
where
    I: IntoIterator<Item = A>,
    A: AsRef<OsStr>,
{
    let mut cmd = command(program);
    cmd.args(args).stdin(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| SysError(format!("{program}: {e}")))?;
    let write_result = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(key),
        None => Err(io::Error::other("no stdin")),
    };
    let out = child
        .wait_with_output()
        .map_err(|e| SysError(format!("{program}: {e}")))?;
    if !out.status.success() {
        return Err(failure(program, &out.stderr, &out.stdout));
    }
    write_result.map_err(|e| SysError(format!("{program}: {e}")))
}

/// Finds a `name:x:<id>` field in `/etc/passwd` or `/etc/group` text.
fn numeric_id(database: &str, name: &str, field: usize) -> Option<u32> {
    database.lines().find_map(|line| {
        let mut fields = line.split(':');
        if fields.next()? != name {
            return None;
        }
        fields.nth(field - 1)?.parse().ok()
    })
}

fn uid_of(user: &str) -> SysResult<(u32, u32)> {
    let passwd = fs::read_to_string("/etc/passwd")?;
    let uid = numeric_id(&passwd, user, 2);
    let gid = numeric_id(&passwd, user, 3);
    uid.zip(gid)
        .ok_or_else(|| SysError(format!("no such user: {user}")))
}

fn gid_of(group: &str) -> SysResult<u32> {
    let groups = fs::read_to_string("/etc/group")?;
    numeric_id(&groups, group, 2).ok_or_else(|| SysError(format!("no such group: {group}")))
}

/// Runs a program and returns its exit code and combined output, bounded.
fn run_output(program: &str, mut cmd: Command) -> SysResult<(Option<i32>, String)> {
    let out = cmd
        .output()
        .map_err(|e| SysError(format!("{program}: {e}")))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text.truncate(MAX_TOOL_OUTPUT as usize);
    Ok((out.status.code(), text))
}

/// The filesystem check tools disagree on exit codes, so each is read on its own terms.
/// Returns whether the filesystem is clean afterwards, or `None` when the tool itself failed.
pub fn fsck_outcome(fs_type: &str, repair: bool, code: Option<i32>) -> Option<bool> {
    match (fs_type, repair, code) {
        ("xfs", _, Some(0)) => Some(true),
        ("xfs", false, Some(1)) => Some(false),
        ("ext4", false, Some(0)) => Some(true),
        ("ext4", false, Some(1 | 2 | 4)) => Some(false),
        ("ext4", true, Some(0..=2)) => Some(true),
        ("ext4", true, Some(4)) => Some(false),
        _ => None,
    }
}

pub fn fsck_args(
    fs_type: &str,
    repair: bool,
    device: &Path,
) -> Option<(&'static str, Vec<OsString>)> {
    let (program, flags): (&str, &[&str]) = match (fs_type, repair) {
        ("xfs", false) => ("xfs_repair", &["-n"]),
        ("xfs", true) => ("xfs_repair", &[]),
        ("ext4", false) => ("e2fsck", &["-f", "-n"]),
        ("ext4", true) => ("e2fsck", &["-f", "-y"]),
        _ => return None,
    };
    let mut args: Vec<OsString> = flags.iter().map(OsString::from).collect();
    args.push(device.as_os_str().to_owned());
    Some((program, args))
}

fn mountinfo_contains(mountinfo: &str, path: &str) -> bool {
    mountinfo
        .lines()
        .any(|line| line.split(' ').nth(4) == Some(path))
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The real [`System`]. Needs root for nearly everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealSystem;

impl System for RealSystem {
    fn sv(&self, action: SvAction, id: &str, wait_secs: u32) -> SysResult<()> {
        let mut cmd = command("sv");
        cmd.arg("-w")
            .arg(wait_secs.to_string())
            .arg(action.as_arg())
            .arg(id);
        cmd.env("SVDIR", "/var/service");
        run("sv", cmd)
    }

    fn sv_status(&self, id: &str) -> SysResult<String> {
        let mut cmd = command("sv");
        cmd.arg("status").arg(id).env("SVDIR", "/var/service");
        let out = cmd.output().map_err(|e| SysError(format!("sv: {e}")))?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(text)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn read_file(&self, path: &Path) -> SysResult<Option<Vec<u8>>> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SysError(format!("{}: {e}", path.display()))),
        }
    }

    fn list_dir(&self, path: &Path) -> SysResult<Vec<String>> {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(SysError(format!("{}: {e}", path.display()))),
        };
        let mut names = Vec::new();
        for entry in entries {
            if let Ok(name) = entry?.file_name().into_string() {
                names.push(name);
            }
        }
        Ok(names)
    }

    fn atomic_write(&self, path: &Path, bytes: &[u8], options: WriteOptions) -> SysResult<()> {
        let dir = path
            .parent()
            .ok_or_else(|| SysError("path has no directory".to_owned()))?;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(".{name}.{}.{unique}.tmp", std::process::id()));
        let result = (|| -> SysResult<()> {
            // Private until the final mode is set, and create_new refuses a planted symlink.
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(bytes)?;
            if let Some(group) = options.group {
                chown(&temp, None, Some(gid_of(group)?))?;
            }
            file.set_permissions(fs::Permissions::from_mode(options.mode))?;
            file.sync_all()?;
            fs::rename(&temp, path)?;
            // The rename only survives a power cut once the directory itself is on disk.
            File::open(dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|e| SysError(format!("{}: {e}", path.display())))
    }

    fn remove_file(&self, path: &Path) -> SysResult<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SysError(format!("{}: {e}", path.display()))),
        }
    }

    fn ensure_dir(&self, path: &Path, mode: u32, owner: Option<&str>) -> SysResult<()> {
        let context = |e: io::Error| SysError(format!("{}: {e}", path.display()));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(context)?;
        }
        match fs::DirBuilder::new().mode(mode).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(context(e)),
        }
        // Applied even to an existing directory, since the umask affects the mode at creation
        // and an old directory may have looser permissions.
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(context)?;
        if let Some(user) = owner {
            let (uid, gid) = uid_of(user)?;
            chown(path, Some(uid), Some(gid)).map_err(context)?;
        }
        Ok(())
    }

    fn set_owner(&self, path: &Path, user: &str, mode: u32) -> SysResult<()> {
        let (uid, gid) = uid_of(user)?;
        chown(path, Some(uid), Some(gid))?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    fn nft_check(&self, path: &Path) -> SysResult<()> {
        let mut cmd = command("nft");
        cmd.arg("-c").arg("-f").arg(path);
        run("nft", cmd)
    }

    fn nft_load(&self, path: &Path) -> SysResult<()> {
        let mut cmd = command("nft");
        cmd.arg("-f").arg(path);
        run("nft", cmd)
    }

    fn allocate_file(&self, path: &Path, size: u64) -> SysResult<()> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        // Real blocks rather than a sparse file, which would turn a full disk into I/O errors
        // inside the volume long after creation.
        fallocate(&file, FallocateFlags::empty(), 0, size).map_err(io::Error::from)?;
        file.sync_all()?;
        Ok(())
    }

    fn luks_format(&self, file: &Path, key: &[u8]) -> SysResult<()> {
        run_with_key("cryptsetup", luks_format_args(file), key)
    }

    fn luks_open(&self, file: &Path, name: &str, key: &[u8]) -> SysResult<()> {
        run_with_key("cryptsetup", luks_open_args(file, name), key)
    }

    fn luks_close(&self, name: &str) -> SysResult<()> {
        let mut cmd = command("cryptsetup");
        cmd.arg("close").arg(name);
        run("cryptsetup", cmd)
    }

    fn mapper_active(&self, name: &str) -> bool {
        Path::new("/dev/mapper").join(name).exists()
    }

    fn make_filesystem(&self, device: &Path) -> SysResult<()> {
        let mut cmd = command("mkfs.ext4");
        cmd.arg("-q").arg("-m").arg("0").arg(device);
        run("mkfs.ext4", cmd)
    }

    fn mount(&self, device: &Path, target: &Path) -> SysResult<()> {
        let mut cmd = command("mount");
        cmd.arg("-t")
            .arg("ext4")
            .arg("-o")
            .arg("nodev,nosuid,noexec")
            .arg(device)
            .arg(target);
        run("mount", cmd)
    }

    fn is_mounted(&self, path: &Path) -> bool {
        let Some(path) = path.to_str() else {
            return false;
        };
        fs::read_to_string("/proc/self/mountinfo")
            .map(|info| mountinfo_contains(&info, path))
            .unwrap_or(false)
    }

    fn symlink(&self, target: &Path, link: &Path) -> SysResult<()> {
        std::os::unix::fs::symlink(target, link)
            .map_err(|e| SysError(format!("{}: {e}", link.display())))
    }

    fn read_link(&self, path: &Path) -> Option<std::path::PathBuf> {
        let meta = fs::symlink_metadata(path).ok()?;
        if !meta.file_type().is_symlink() {
            return None;
        }
        fs::read_link(path).ok()
    }

    fn user_exists(&self, name: &str) -> bool {
        let users = fs::read_to_string("/etc/passwd").unwrap_or_default();
        let groups = fs::read_to_string("/etc/group").unwrap_or_default();
        // A group of that name counts too, since useradd would fail on it or join it.
        numeric_id(&users, name, 2).is_some() || numeric_id(&groups, name, 2).is_some()
    }

    fn create_user(&self, name: &str, home: &Path) -> SysResult<()> {
        let mut cmd = command("useradd");
        cmd.args([
            "--system",
            "--user-group",
            "--no-create-home",
            "--shell",
            "/sbin/nologin",
        ])
        .args(["--comment", "nodo service"])
        .arg("--home-dir")
        .arg(home)
        .arg(name);
        run("useradd", cmd)
    }

    fn fs_usage(&self, path: &Path) -> SysResult<(u64, u64)> {
        let stat =
            rustix::fs::statvfs(path).map_err(|e| SysError(format!("{}: {e}", path.display())))?;
        let unit = stat.f_frsize;
        Ok((
            stat.f_blocks.saturating_mul(unit),
            stat.f_bavail.saturating_mul(unit),
        ))
    }

    fn fs_type(&self, device: &Path) -> Option<String> {
        let mut cmd = command("blkid");
        cmd.args(["-o", "value", "-s", "TYPE"]).arg(device);
        let (code, text) = run_output("blkid", cmd).ok()?;
        let fs = text.trim();
        (code == Some(0) && !fs.is_empty() && !fs.contains(char::is_whitespace))
            .then(|| fs.to_owned())
    }

    fn fs_check(&self, device: &Path, fs_type: &str, repair: bool) -> SysResult<(bool, String)> {
        let (program, args) = fsck_args(fs_type, repair, device)
            .ok_or_else(|| SysError(format!("cannot check {fs_type}")))?;
        let mut cmd = command(program);
        cmd.args(args);
        let (code, output) = run_output(program, cmd)?;
        match fsck_outcome(fs_type, repair, code) {
            Some(clean) => Ok((clean, output)),
            None => Err(failure(program, output.as_bytes(), b"")),
        }
    }

    fn mount_fstab(&self, mount_point: &Path) -> SysResult<()> {
        let mut cmd = command("mount");
        cmd.arg(mount_point);
        run("mount", cmd)
    }

    fn settings_worker(&self, input: &[u8]) -> SysResult<Vec<u8>> {
        let mut cmd = Command::new("/proc/self/exe");
        cmd.arg("--settings-worker")
            .env_clear()
            .env("LC_ALL", "C")
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if rustix::process::geteuid().is_root() {
            // std clears supplementary groups when dropping from root to this uid.
            let (uid, gid) = uid_of(SETTINGS_USER)
                .map_err(|_| SysError(format!("the {SETTINGS_USER} account is missing")))?;
            if uid == 0 || gid == 0 {
                return Err(SysError(format!("{SETTINGS_USER} must not be root")));
            }
            use std::os::unix::process::CommandExt;
            cmd.uid(uid).gid(gid);
        }
        run_bounded(
            cmd,
            input,
            WORKER_TIMEOUT,
            crate::worker::MAX_WORKER_IO as u64,
        )
    }

    fn probe(&self, probe: &Probe) -> bool {
        let port = match probe {
            Probe::Tcp { port } | Probe::Http { port, .. } => *port,
        };
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let Ok(mut stream) = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) else {
            return false;
        };
        let Probe::Http { path, .. } = probe else {
            return true;
        };
        let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
        let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
        let request =
            format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        if stream.write_all(request.as_bytes()).is_err() {
            return false;
        }
        // Bounded, since the service answering this is untrusted.
        let mut line = String::new();
        let limited = (&mut stream).take(256);
        if BufReader::new(limited).read_line(&mut line).is_err() {
            return false;
        }
        http_success(&line)
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn random(&self, out: &mut [u8]) -> SysResult<()> {
        getrandom::fill(out).map_err(|e| SysError(format!("no secure randomness: {e}")))
    }

    fn rename(&self, from: &Path, to: &Path) -> SysResult<()> {
        // rename(2) would silently replace an existing target.
        if fs::symlink_metadata(to).is_ok() {
            return Err(SysError(format!("{} already exists", to.display())));
        }
        fs::rename(from, to).map_err(|e| SysError(format!("{}: {e}", from.display())))
    }

    fn http_get_local(&self, port: u16, path: &str, limit: usize) -> SysResult<Vec<u8>> {
        if port == 0 || !path.starts_with('/') || !path.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(SysError("invalid local address".to_owned()));
        }
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let mut stream = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT)
            .map_err(|e| SysError(format!("port {port}: {e}")))?;
        stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
        stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
        let request =
            format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes())?;
        // Bounded, since whoever answers is untrusted. Headers count toward the limit too.
        let mut response = Vec::new();
        (&mut stream)
            .take(limit as u64 + 8192)
            .read_to_end(&mut response)?;
        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| SysError("malformed HTTP response".to_owned()))?;
        let status = String::from_utf8_lossy(&response[..split]);
        let status = status.lines().next().unwrap_or_default();
        if !http_success(status) {
            return Err(SysError(format!(
                "port {port} answered {}",
                status
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(64)
                    .collect::<String>()
            )));
        }
        let body = response.split_off(split + 4);
        if body.len() > limit {
            return Err(SysError("response too large".to_owned()));
        }
        Ok(body)
    }
}

/// Feeds `input`, collects bounded output, and kills the child if it outlives `timeout`.
fn run_bounded(
    mut cmd: Command,
    input: &[u8],
    timeout: Duration,
    limit: u64,
) -> SysResult<Vec<u8>> {
    let mut child = cmd
        .spawn()
        .map_err(|e| SysError(format!("settings worker: {e}")))?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let output = std::thread::scope(|scope| {
        scope.spawn(move || {
            if let Some(mut stdin) = stdin {
                let _ = stdin.write_all(input);
            }
        });
        let out = scope.spawn(move || {
            let mut bytes = Vec::new();
            if let Some(stdout) = stdout {
                let _ = stdout.take(limit + 1).read_to_end(&mut bytes);
            }
            bytes
        });
        let err = scope.spawn(move || {
            let mut bytes = Vec::new();
            if let Some(stderr) = stderr {
                let _ = stderr.take(4096).read_to_end(&mut bytes);
            }
            bytes
        });
        let deadline = std::time::Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(SysError("settings code took too long".to_owned()));
                }
                Err(e) => break Err(SysError(format!("settings worker: {e}"))),
            }
        };
        let out = out.join().unwrap_or_default();
        let err = err.join().unwrap_or_default();
        status.map(|status| (status, out, err))
    });
    let (status, out, err) = output?;
    if !status.success() {
        return Err(failure("settings worker", &err, b""));
    }
    if out.len() as u64 > limit {
        return Err(SysError("settings worker output too large".to_owned()));
    }
    Ok(out)
}

fn http_success(status_line: &str) -> bool {
    let mut parts = status_line.split(' ');
    parts
        .next()
        .is_some_and(|version| version.starts_with("HTTP/"))
        && parts
            .next()
            .and_then(|code| code.parse::<u16>().ok())
            .is_some_and(|c| (200..400).contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_GROUP: WriteOptions = WriteOptions {
        mode: 0o640,
        group: None,
    };

    #[test]
    fn atomic_write_replaces_content_sets_mode_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        fs::write(&path, b"old").unwrap();
        RealSystem.atomic_write(&path, b"new", NO_GROUP).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_atomic_write_keeps_the_directory_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target");
        // A non-empty directory in the way makes the final rename fail.
        fs::create_dir(&path).unwrap();
        fs::write(path.join("child"), b"x").unwrap();
        assert!(RealSystem.atomic_write(&path, b"new", NO_GROUP).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(fs::read(path.join("child")).unwrap(), b"x");
    }

    #[test]
    fn atomic_write_replaces_a_symlink_instead_of_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, b"safe").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        RealSystem.atomic_write(&link, b"new", NO_GROUP).unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"safe");
        assert_eq!(fs::read(&link).unwrap(), b"new");
    }

    #[test]
    fn read_and_list_treat_missing_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert_eq!(RealSystem.read_file(&missing).unwrap(), None);
        assert!(RealSystem.list_dir(&missing).unwrap().is_empty());
        assert!(RealSystem.remove_file(&missing).is_ok());
    }

    #[test]
    fn allocate_refuses_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.luks");
        RealSystem.allocate_file(&path, 4096).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), 4096);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(RealSystem.allocate_file(&path, 4096).is_err());
    }

    #[test]
    fn ensure_dir_tightens_an_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b");
        RealSystem.ensure_dir(&path, 0o755, None).unwrap();
        RealSystem.ensure_dir(&path, 0o700, None).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn key_channel_delivers_binary_keys_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("got");
        // Newlines and NUL must survive, since volume keys are arbitrary bytes.
        let key = [0u8, b'\n', 255, b'\'', b'\n', 0];
        run_with_key("sh", ["-c", &format!("cat > '{}'", out.display())], &key).unwrap();
        assert_eq!(fs::read(&out).unwrap(), key);
    }

    #[test]
    fn program_errors_are_reported_and_bounded() {
        let long = "x".repeat(10_000);
        let script = format!("echo {long} >&2; exit 3");
        let mut cmd = command("sh");
        cmd.args(["-c", &script]);
        let e = run("sh", cmd).unwrap_err();
        assert!(e.0.len() < MAX_ERROR_LEN + 16, "{}", e.0.len());
    }

    /// Runs against a process that exits without reading, as a failing cryptsetup can.
    #[test]
    fn child_that_ignores_the_key_does_not_hang_or_kill_the_helper() {
        let e = run_with_key("sh", ["-c", "echo no >&2; exit 1"], &[0u8; 1 << 20]).unwrap_err();
        assert!(e.0.contains("no"), "{e}");
    }

    #[test]
    fn luks_arguments_never_hold_key_material() {
        let args = luks_format_args(Path::new("/v/a.luks"));
        assert!(args.contains(&OsString::from("--key-file=-")));
        assert_eq!(args.last().unwrap(), "/v/a.luks");
        let args = luks_open_args(Path::new("/v/a.luks"), "nodo-a");
        assert!(args.contains(&OsString::from("--key-file=-")));
        assert_eq!(args.last().unwrap(), "nodo-a");
    }

    #[test]
    fn parses_account_databases() {
        let passwd = "root:x:0:0::/root:/bin/sh\n_shop:x:990:980::/var/lib:/sbin/nologin\n";
        assert_eq!(numeric_id(passwd, "_shop", 2), Some(990));
        assert_eq!(numeric_id(passwd, "_shop", 3), Some(980));
        assert_eq!(numeric_id(passwd, "_sho", 2), None);
        assert_eq!(numeric_id("_shop:x:980:\n", "_shop", 2), Some(980));
    }

    #[test]
    fn mount_check_matches_the_whole_path() {
        let info = "36 35 98:0 / /run/nodo/volumes/vault rw,noatime master:1 - ext4 /dev/x rw\n";
        assert!(mountinfo_contains(info, "/run/nodo/volumes/vault"));
        assert!(!mountinfo_contains(info, "/run/nodo/volumes/vau"));
        assert!(!mountinfo_contains(info, "/run/nodo/volumes"));
    }

    #[test]
    fn fsck_exit_codes_are_read_per_tool() {
        assert_eq!(fsck_outcome("xfs", false, Some(0)), Some(true));
        assert_eq!(fsck_outcome("xfs", false, Some(1)), Some(false));
        assert_eq!(
            fsck_outcome("xfs", false, Some(2)),
            None,
            "dirty log is not a verdict"
        );
        assert_eq!(fsck_outcome("ext4", false, Some(4)), Some(false));
        assert_eq!(fsck_outcome("ext4", true, Some(1)), Some(true));
        assert_eq!(fsck_outcome("ext4", true, Some(8)), None);
        assert_eq!(
            fsck_outcome("ext4", false, None),
            None,
            "killed by a signal"
        );
        let (program, args) = fsck_args("xfs", false, Path::new("/dev/x")).unwrap();
        assert_eq!(program, "xfs_repair");
        assert_eq!(args, [OsString::from("-n"), OsString::from("/dev/x")]);
        assert!(fsck_args("ntfs", false, Path::new("/dev/x")).is_none());
    }

    #[test]
    fn bounded_runner_kills_a_child_that_hangs() {
        let mut cmd = command("sh");
        cmd.args(["-c", "sleep 30"]).stdin(Stdio::piped());
        let started = std::time::Instant::now();
        let e = run_bounded(cmd, b"", Duration::from_millis(200), 1024).unwrap_err();
        assert!(e.0.contains("too long"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn bounded_runner_round_trips_and_refuses_oversized_output() {
        let mut cmd = command("cat");
        cmd.stdin(Stdio::piped());
        assert_eq!(
            run_bounded(cmd, b"hello", Duration::from_secs(5), 1024).unwrap(),
            b"hello"
        );
        let mut cmd = command("cat");
        cmd.stdin(Stdio::piped());
        assert!(run_bounded(cmd, &[b'x'; 2048], Duration::from_secs(5), 1024).is_err());
    }

    #[test]
    fn http_status_is_read_strictly() {
        assert!(http_success("HTTP/1.1 200 OK\r\n"));
        assert!(http_success("HTTP/1.0 302 Found\r\n"));
        assert!(!http_success("HTTP/1.1 500 Oops\r\n"));
        assert!(!http_success("HTTP/1.1 404 Nope\r\n"));
        assert!(!http_success("200 OK\r\n"));
        assert!(!http_success(""));
    }

    #[test]
    fn probes_a_local_listener() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(RealSystem.probe(&Probe::Tcp { port }));
        let server = std::thread::spawn(move || {
            for reply in ["HTTP/1.0 200 OK\r\n\r\n", "HTTP/1.0 503 No\r\n\r\n"] {
                loop {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut buffer = [0u8; 256];
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    if n == 0 {
                        continue;
                    }
                    stream.write_all(reply.as_bytes()).unwrap();
                    break;
                }
            }
        });
        assert!(RealSystem.probe(&Probe::Http {
            port,
            path: "/".into()
        }));
        assert!(!RealSystem.probe(&Probe::Http {
            port,
            path: "/".into()
        }));
        server.join().unwrap();
    }

    /// Formatting a plain file and testing a key both work unprivileged, so this exercises the
    /// real argument vectors and the binary key channel. Opening a mapping does need root and
    /// stays unproven here. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "requires cryptsetup"]
    fn real_cryptsetup_accepts_format_and_key_channel() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("v.luks");
        RealSystem.allocate_file(&file, 32 * 1024 * 1024).unwrap();
        let mut key = [7u8; 64];
        key[3] = 0;
        key[9] = b'\n';
        RealSystem.luks_format(&file, &key).unwrap();
        let test = |key: &[u8]| {
            let args: Vec<OsString> = vec![
                "open".into(),
                "--test-passphrase".into(),
                "--type".into(),
                "luks2".into(),
                "--key-file=-".into(),
                file.as_os_str().to_owned(),
            ];
            run_with_key("cryptsetup", args, key)
        };
        test(&key).unwrap();
        let mut wrong = key;
        wrong[63] ^= 1;
        assert!(test(&wrong).is_err());
        // A key cut at its first NUL or newline would mean the slot holds a weak key.
        assert!(test(&key[..3]).is_err());
    }
}
