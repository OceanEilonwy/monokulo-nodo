//! Runs generated scripts for real under `/bin/sh`, with every external command replaced by a
//! stub that records its arguments. Needs no root and touches nothing outside a temp directory.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Records its name, stdin target and arguments, one per line.
const RECORDER: &str = r#"#!/bin/sh
{
	echo "--- ${0##*/} stdin=$(readlink /proc/self/fd/0)"
	for arg in "$@"; do
		printf '[%s]\n' "$arg"
	done
} >>"$STUB_LOG"
"#;

const MOUNTPOINT: &str = r#"#!/bin/sh
{
	echo "--- mountpoint"
	for arg in "$@"; do
		printf '[%s]\n' "$arg"
	done
} >>"$STUB_LOG"
[ "$STUB_MOUNTED" = yes ]
"#;

#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub code: Option<i32>,
    pub stdout: String,
    pub log: String,
}

pub struct Scenario<'a> {
    /// Contents of `./conf`, if the service has one.
    pub conf: Option<&'a str>,
    /// Whether the stubbed `mountpoint` reports a mount.
    pub mounted: bool,
}

/// Executes `script`, returning what it invoked. The temp directory is the working directory, so
/// anything an injection manages to run with `touch canary` is visible to the caller through
/// the returned canary check.
pub fn run_stubbed(script: &str, scenario: &Scenario) -> (Outcome, bool) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let cwd = dir.path().join("sv");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    for name in ["chpst", "install", "sleep", "pkill", "svlogd"] {
        stub(&bin.join(name), RECORDER);
    }
    stub(&bin.join("mountpoint"), MOUNTPOINT);
    if let Some(conf) = scenario.conf {
        fs::write(cwd.join("conf"), conf).unwrap();
    }
    let script_path = dir.path().join("script");
    fs::write(&script_path, script).unwrap();
    let log = dir.path().join("log");

    let path = format!("{}:/usr/bin:/bin", bin.display());
    let out = Command::new("/bin/sh")
        .arg(&script_path)
        .current_dir(&cwd)
        .env_clear()
        .env("PATH", path)
        .env("STUB_LOG", &log)
        .env("STUB_MOUNTED", if scenario.mounted { "yes" } else { "no" })
        .stdin(Stdio::piped())
        .output()
        .unwrap();
    let canary = cwd.join("canary").exists() || dir.path().join("canary").exists();
    let logged = fs::read_to_string(&log).unwrap_or_default();
    (
        Outcome {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            log: logged,
        },
        canary,
    )
}

fn stub(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
