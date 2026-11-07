//! An in-memory machine for tests. It enforces what the real one would: for instance LUKS only
//! opens with the key it was formatted with.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use crate::exposure::NoCaddyYet;
use crate::helper::{AllowedUids, Helper, Paths, Timing};
use crate::keys::KdfParams;
use crate::settings::FlatTomlSettings;
use crate::system::{Probe, SvAction, SysError, SysResult, System, WriteOptions};

pub const SHOP: &str = r#"api_version = 1
id = "shop"
version = "1"
exec = ["bin/shop"]
settings = "settings.lua"
stop_timeout_secs = 30

[health.tcp]
port = 8080

[[web]]
name = "web"
upstream = "127.0.0.1:8080"
multi_address = true

[[web]]
name = "single"
upstream = "127.0.0.1:8081"
"#;

pub const ENCRYPTED: &str = r#"api_version = 1
id = "vault"
version = "1"
exec = ["bin/vault"]

[storage]
encrypted = true
"#;

#[derive(Default)]
struct State {
    files: BTreeMap<String, Vec<u8>>,
    calls: Vec<String>,
    /// Prefix and how many matching calls still succeed before it starts failing.
    failures: Vec<Failure>,
    status: HashMap<String, String>,
    status_script: HashMap<String, VecDeque<String>>,
    probes: VecDeque<bool>,
    sleeps: Vec<Duration>,
    mounted: BTreeSet<String>,
    mapped: BTreeSet<String>,
    format_keys: Vec<Vec<u8>>,
    counter: u8,
}

struct Failure {
    prefix: String,
    skip: usize,
    remaining: Option<usize>,
}

#[derive(Default)]
pub struct FakeSystem(Mutex<State>);

impl FakeSystem {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    /// Records an attempted action, failing if a test asked for that. Reads go through `check`
    /// only, so the log shows actions rather than every lookup.
    fn record(&self, call: String) -> SysResult<()> {
        self.state().calls.push(call.clone());
        self.check(&call)
    }

    fn check(&self, call: &str) -> SysResult<()> {
        let mut state = self.state();
        for failure in &mut state.failures {
            if !call.starts_with(&failure.prefix) {
                continue;
            }
            if failure.skip > 0 {
                failure.skip -= 1;
                continue;
            }
            match &mut failure.remaining {
                Some(0) => continue,
                Some(n) => *n -= 1,
                None => {}
            }
            return Err(SysError(format!("injected failure: {call}")));
        }
        Ok(())
    }

    pub fn fail(&self, prefix: &str) {
        self.state().failures.push(Failure {
            prefix: prefix.to_owned(),
            skip: 0,
            remaining: None,
        });
    }

    pub fn fail_once(&self, prefix: &str) {
        self.state().failures.push(Failure {
            prefix: prefix.to_owned(),
            skip: 0,
            remaining: Some(1),
        });
    }

    /// Lets `skip` matching calls through, then fails the rest.
    pub fn fail_after(&self, prefix: &str, skip: usize) {
        self.state().failures.push(Failure {
            prefix: prefix.to_owned(),
            skip,
            remaining: None,
        });
    }

    pub fn put(&self, path: &str, bytes: &[u8]) {
        self.state().files.insert(path.to_owned(), bytes.to_vec());
    }

    pub fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.state().files.get(path).cloned()
    }

    pub fn remove(&self, path: &str) {
        self.state().files.remove(path);
    }

    fn manifest_id(text: &str) -> String {
        nodo_manifest::Manifest::parse(text).unwrap().id
    }

    pub fn add_manifest(&self, text: &str) {
        let id = Self::manifest_id(text);
        self.put(&format!("/m/app/{id}.toml"), text.as_bytes());
    }

    pub fn add_system_manifest(&self, text: &str) {
        let id = Self::manifest_id(text);
        self.put(&format!("/m/sys/{id}.toml"), text.as_bytes());
    }

    pub fn calls(&self) -> Vec<String> {
        self.state().calls.clone()
    }

    pub fn clear_calls(&self) {
        self.state().calls.clear();
    }

    pub fn sleeps(&self) -> Vec<Duration> {
        self.state().sleeps.clone()
    }

    pub fn set_status(&self, id: &str, text: &str) {
        self.state().status.insert(id.to_owned(), text.to_owned());
    }

    /// `sv status` answers with these in turn, then with the set or default status.
    pub fn script_status(&self, id: &str, lines: &[&str]) {
        let lines = lines.iter().map(|l| (*l).to_owned()).collect();
        self.state().status_script.insert(id.to_owned(), lines);
    }

    /// Probes answer with these in turn, then succeed.
    pub fn script_probes(&self, results: &[bool]) {
        self.state().probes = results.iter().copied().collect();
    }

    pub fn set_mounted(&self, path: &str) {
        self.state().mounted.insert(path.to_owned());
    }

    pub fn mounted(&self, path: &str) -> bool {
        self.state().mounted.contains(path)
    }

    pub fn format_keys(&self) -> Vec<Vec<u8>> {
        self.state().format_keys.clone()
    }

    /// Loses everything a reboot loses: mounts and open mappings.
    pub fn reboot(&self) {
        let mut state = self.state();
        state.mounted.clear();
        state.mapped.clear();
        state.calls.clear();
    }
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

impl System for FakeSystem {
    fn sv(&self, action: SvAction, id: &str, wait_secs: u32) -> SysResult<()> {
        self.record(format!("sv {} {id} -w{wait_secs}", action.as_arg()))
    }

    fn sv_status(&self, id: &str) -> SysResult<String> {
        self.check(&format!("status {id}"))?;
        let mut state = self.state();
        if let Some(line) = state
            .status_script
            .get_mut(id)
            .and_then(VecDeque::pop_front)
        {
            return Ok(line);
        }
        Ok(state
            .status
            .get(id)
            .cloned()
            .unwrap_or_else(|| format!("run: {id}: (pid 1) 100s; run: log: (pid 2) 100s\n")))
    }

    fn exists(&self, path: &Path) -> bool {
        self.state().files.contains_key(&text(path))
    }

    fn read_file(&self, path: &Path) -> SysResult<Option<Vec<u8>>> {
        self.check(&format!("read {}", text(path)))?;
        Ok(self.state().files.get(&text(path)).cloned())
    }

    fn list_dir(&self, path: &Path) -> SysResult<Vec<String>> {
        self.check(&format!("list {}", text(path)))?;
        let prefix = format!("{}/", text(path));
        Ok(self
            .state()
            .files
            .keys()
            .filter_map(|file| file.strip_prefix(&prefix))
            .filter(|rest| !rest.contains('/'))
            .map(str::to_owned)
            .collect())
    }

    fn atomic_write(&self, path: &Path, bytes: &[u8], options: WriteOptions) -> SysResult<()> {
        self.record(format!(
            "write {} mode={:o} group={}",
            text(path),
            options.mode,
            options.group.unwrap_or("-")
        ))?;
        self.state().files.insert(text(path), bytes.to_vec());
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> SysResult<()> {
        self.record(format!("remove {}", text(path)))?;
        self.state().files.remove(&text(path));
        Ok(())
    }

    fn ensure_dir(&self, path: &Path, mode: u32, owner: Option<&str>) -> SysResult<()> {
        self.record(format!(
            "mkdir {} {mode:o} {}",
            text(path),
            owner.unwrap_or("-")
        ))
    }

    fn set_owner(&self, path: &Path, user: &str, mode: u32) -> SysResult<()> {
        self.record(format!("chown {} {user} {mode:o}", text(path)))
    }

    fn nft_check(&self, path: &Path) -> SysResult<()> {
        self.record(format!("nft check {}", text(path)))
    }

    fn nft_load(&self, path: &Path) -> SysResult<()> {
        self.record(format!("nft load {}", text(path)))
    }

    fn allocate_file(&self, path: &Path, size: u64) -> SysResult<()> {
        self.record(format!("allocate {} {size}", text(path)))?;
        let mut state = self.state();
        if state.files.contains_key(&text(path)) {
            return Err(SysError("file exists".to_owned()));
        }
        state.files.insert(text(path), Vec::new());
        Ok(())
    }

    fn luks_format(&self, file: &Path, key: &[u8]) -> SysResult<()> {
        self.record(format!("luks format {}", text(file)))?;
        let mut state = self.state();
        state.format_keys.push(key.to_vec());
        state.files.insert(text(file), key.to_vec());
        Ok(())
    }

    fn luks_open(&self, file: &Path, name: &str, key: &[u8]) -> SysResult<()> {
        self.record(format!("luks open {} {name}", text(file)))?;
        let mut state = self.state();
        if state.files.get(&text(file)).map(Vec::as_slice) != Some(key) {
            return Err(SysError(
                "No key available with this passphrase.".to_owned(),
            ));
        }
        state.mapped.insert(name.to_owned());
        Ok(())
    }

    fn luks_close(&self, name: &str) -> SysResult<()> {
        self.record(format!("luks close {name}"))?;
        self.state().mapped.remove(name);
        Ok(())
    }

    fn mapper_active(&self, name: &str) -> bool {
        self.state().mapped.contains(name)
    }

    fn make_filesystem(&self, device: &Path) -> SysResult<()> {
        self.record(format!("mkfs {}", text(device)))
    }

    fn mount(&self, device: &Path, target: &Path) -> SysResult<()> {
        self.record(format!("mount {} {}", text(device), text(target)))?;
        self.state().mounted.insert(text(target));
        Ok(())
    }

    fn is_mounted(&self, path: &Path) -> bool {
        self.state().mounted.contains(&text(path))
    }

    fn probe(&self, probe: &Probe) -> bool {
        let mut state = self.state();
        state.calls.push(format!("probe {probe:?}"));
        state.probes.pop_front().unwrap_or(true)
    }

    fn sleep(&self, duration: Duration) {
        self.state().sleeps.push(duration);
    }

    fn random(&self, out: &mut [u8]) -> SysResult<()> {
        let mut state = self.state();
        state.counter = state.counter.wrapping_add(1);
        out.fill(state.counter);
        Ok(())
    }
}

pub const TEST_PATHS: fn() -> Paths = || Paths {
    system_manifests: "/m/sys".into(),
    app_manifests: "/m/app".into(),
    sv_dir: "/sv".into(),
    nft_dir: "/nft".into(),
    nft_main: "/nft/main.conf".into(),
    keystore: "/k/device.key".into(),
    volumes: "/v".into(),
    exposure_state: "/s/exposure.toml".into(),
};

/// A helper over `sys` that allows uid 1000, with a KDF cheap enough for tests.
pub fn helper(sys: FakeSystem) -> Helper<FakeSystem> {
    Helper::new(
        sys,
        AllowedUids::new([1000]),
        Box::new(FlatTomlSettings),
        Box::new(NoCaddyYet),
    )
    .with_paths(TEST_PATHS())
    .with_kdf(KdfParams {
        memory_kib: 64,
        iterations: 1,
        lanes: 1,
    })
    .with_timing(Timing {
        health_attempts: 5,
        ..Timing::default()
    })
}
