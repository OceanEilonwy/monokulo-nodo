//! An in-memory machine for tests. It enforces what the real one would: for instance LUKS only
//! opens with the key it was formatted with.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use nodo_api::{
    ApiError, SettingChange, SettingCurrent, SettingField, SettingInput, SettingKind,
    SettingLiteral, SettingsForm,
};
use zeroize::Zeroizing;

use crate::catalog::Package;
use crate::exposure::NoCaddyYet;
use crate::helper::{AllowedUids, DriveConfig, Helper, Paths, Timing};
use crate::keys::KdfParams;
use crate::settings::{Plan, PlannedFile, SettingsContext, SettingsPolicy};
use crate::system::{Probe, SvAction, SysError, SysResult, System, WriteOptions};

/// Where the test policy keeps shop's values.
pub const STORE: &str = "/s/settings/shop.toml";

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
    links: BTreeMap<String, String>,
    users: BTreeSet<String>,
    fs_types: HashMap<String, String>,
    /// Whether each device's filesystem is damaged. A repair fixes it.
    damaged: BTreeSet<String>,
    /// Sizes of allocated volume files.
    sizes: HashMap<String, u64>,
    /// Total and free bytes every filesystem reports, when a test sets them.
    usage: Option<(u64, u64)>,
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

    pub fn set_usage(&self, total: u64, free: u64) {
        self.state().usage = Some((total, free));
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

    pub fn add_user(&self, name: &str) {
        self.state().users.insert(name.to_owned());
    }

    pub fn has_user(&self, name: &str) -> bool {
        self.state().users.contains(name)
    }

    pub fn link(&self, path: &str) -> Option<String> {
        self.state().links.get(path).cloned()
    }

    pub fn add_link(&self, path: &str, target: &str) {
        self.state()
            .links
            .insert(path.to_owned(), target.to_owned());
    }

    /// A block device with a filesystem, damaged or not.
    pub fn add_device(&self, path: &str, fs_type: &str, damaged: bool) {
        let mut state = self.state();
        state.files.insert(path.to_owned(), Vec::new());
        state.fs_types.insert(path.to_owned(), fs_type.to_owned());
        if damaged {
            state.damaged.insert(path.to_owned());
        }
    }

    /// Loses everything a reboot loses: mounts, open mappings and `/run`, which is memory.
    pub fn reboot(&self) {
        let mut state = self.state();
        state.mounted.clear();
        state.mapped.clear();
        state.calls.clear();
        state.files.retain(|path, _| !path.starts_with("/run/"));
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

    /// A directory exists when any file is inside it, as it would on disk.
    fn exists(&self, path: &Path) -> bool {
        let state = self.state();
        let dir = format!("{}/", text(path));
        state.files.contains_key(&text(path)) || state.files.keys().any(|f| f.starts_with(&dir))
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
        state.sizes.insert(text(path), size);
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

    fn symlink(&self, target: &Path, link: &Path) -> SysResult<()> {
        self.record(format!("symlink {} {}", text(link), text(target)))?;
        let mut state = self.state();
        if state.links.contains_key(&text(link)) || state.files.contains_key(&text(link)) {
            return Err(SysError("file exists".to_owned()));
        }
        state.links.insert(text(link), text(target));
        Ok(())
    }

    fn read_link(&self, path: &Path) -> Option<std::path::PathBuf> {
        self.state().links.get(&text(path)).map(Into::into)
    }

    fn user_exists(&self, name: &str) -> bool {
        self.state().users.contains(name)
    }

    fn create_user(&self, name: &str, home: &Path) -> SysResult<()> {
        self.record(format!("useradd {name} {}", text(home)))?;
        let mut state = self.state();
        if !state.users.insert(name.to_owned()) {
            return Err(SysError("user exists".to_owned()));
        }
        Ok(())
    }

    fn fs_usage(&self, path: &Path) -> SysResult<(u64, u64)> {
        self.check(&format!("statvfs {}", text(path)))?;
        Ok(self.state().usage.unwrap_or((1000, 400)))
    }

    fn fs_type(&self, device: &Path) -> Option<String> {
        self.state().fs_types.get(&text(device)).cloned()
    }

    fn fs_check(&self, device: &Path, fs_type: &str, repair: bool) -> SysResult<(bool, String)> {
        let verb = if repair { "repair" } else { "check" };
        self.record(format!("fsck {verb} {} {fs_type}", text(device)))?;
        let mut state = self.state();
        let damaged = state.damaged.contains(&text(device));
        if repair {
            state.damaged.remove(&text(device));
            return Ok((true, "repaired".to_owned()));
        }
        Ok((
            !damaged,
            if damaged { "bad inode 12" } else { "clean" }.to_owned(),
        ))
    }

    fn mount_fstab(&self, mount_point: &Path) -> SysResult<()> {
        self.record(format!("mount {}", text(mount_point)))?;
        self.state().mounted.insert(text(mount_point));
        Ok(())
    }

    fn settings_worker(&self, input: &[u8]) -> SysResult<Vec<u8>> {
        self.record("settings-worker".to_owned())?;
        let request: crate::worker::WorkerRequest =
            serde_json::from_slice(input).map_err(|e| SysError(e.to_string()))?;
        serde_json::to_vec(&crate::worker::run(&request)).map_err(|e| SysError(e.to_string()))
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

    fn rename(&self, from: &Path, to: &Path) -> SysResult<()> {
        self.record(format!("rename {} {}", text(from), text(to)))?;
        let mut state = self.state();
        if state.files.contains_key(&text(to)) {
            return Err(SysError("file exists".to_owned()));
        }
        let prefix = format!("{}/", text(from));
        let moved: Vec<String> = state
            .files
            .keys()
            .filter(|f| **f == text(from) || f.starts_with(&prefix))
            .cloned()
            .collect();
        for old in moved {
            let bytes = state.files.remove(&old).unwrap_or_default();
            let new = format!("{}{}", text(to), &old[text(from).len()..]);
            state.files.insert(new, bytes);
        }
        Ok(())
    }

    fn http_get_local(&self, port: u16, path: &str, _limit: usize) -> SysResult<Vec<u8>> {
        let call = format!("http {port}{path}");
        self.check(&call)?;
        self.state()
            .files
            .get(&call)
            .cloned()
            .ok_or_else(|| SysError(format!("port {port}: Connection refused")))
    }

    fn file_size(&self, path: &Path) -> Option<u64> {
        let state = self.state();
        state.files.get(&text(path))?;
        Some(state.sizes.get(&text(path)).copied().unwrap_or(0))
    }

    fn luks_grow(&self, file: &Path, name: &str, key: &[u8], size: u64) -> SysResult<()> {
        self.record(format!("luks grow {} {name} {size}", text(file)))?;
        let mut state = self.state();
        if state.files.get(&text(file)).map(Vec::as_slice) != Some(key) {
            return Err(SysError("No key available with this passphrase.".to_owned()));
        }
        if !state.mapped.contains(name) {
            return Err(SysError(format!("{name} is not active")));
        }
        state.sizes.insert(text(file), size);
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
    public_hosts: "/s/public-hosts.toml".into(),
    system_packages: "/p/sys".into(),
    settings_store: "/s/settings".into(),
    installed: "/s/installed".into(),
    service_dir: "/service".into(),
    authorized_keys: "/root/.ssh/authorized_keys".into(),
    drive: Some(DriveConfig {
        device: "/dev/disk/by-partlabel/NODO_BLOCKCHAIN".into(),
        mount_point: "/media/monero".into(),
    }),
    bindings: "/s/bindings.toml".into(),
};

/// Stores plain text values and writes them as a flat file, refusing any value containing
/// "refuse". Enough to drive the apply and rollback engine.
pub struct TestPolicy;

fn read_table(ctx: &SettingsContext, path: &Path) -> Result<toml::Table, ApiError> {
    match ctx.sys.read_file(path)? {
        Some(bytes) => toml::from_str(std::str::from_utf8(&bytes).unwrap())
            .map_err(|_| ApiError::Failed("corrupt".to_owned())),
        None => Ok(toml::Table::new()),
    }
}

impl SettingsPolicy for TestPolicy {
    fn form(&self, ctx: &SettingsContext, package: &Package) -> Result<SettingsForm, ApiError> {
        let store = ctx
            .paths
            .settings_store
            .join(format!("{}.toml", package.id()));
        let table = read_table(ctx, &store)?;
        Ok(SettingsForm {
            package: package.id().to_owned(),
            fields: table
                .iter()
                .map(|(name, value)| SettingField {
                    name: name.clone(),
                    label: name.clone(),
                    help: String::new(),
                    required: false,
                    kind: SettingKind::Text,
                    current: SettingCurrent::Literal(SettingLiteral::Text(
                        value.as_str().unwrap_or_default().to_owned(),
                    )),
                    default: None,
                    public_name: None,
                })
                .collect(),
        })
    }

    fn plan(
        &self,
        ctx: &SettingsContext,
        package: &Package,
        changes: &[SettingChange],
    ) -> Result<Plan, ApiError> {
        let store = ctx
            .paths
            .settings_store
            .join(format!("{}.toml", package.id()));
        let mut table = read_table(ctx, &store)?;
        for change in changes {
            let SettingInput::Literal(SettingLiteral::Text(value)) = &change.value else {
                return Err(ApiError::Failed("only text".to_owned()));
            };
            if value.contains("refuse") {
                return Err(ApiError::Failed("refused".to_owned()));
            }
            table.insert(change.name.clone(), toml::Value::String(value.clone()));
        }
        let bytes = toml::to_string(&table).unwrap().into_bytes();
        let layout = package.layout()?;
        let files = vec![
            PlannedFile {
                path: store,
                bytes: Zeroizing::new(bytes.clone()),
                mode: 0o600,
                group: None,
                parent_mode: 0o700,
            },
            PlannedFile {
                path: Path::new(&layout.config_dir).join("settings.toml"),
                bytes: Zeroizing::new(bytes),
                mode: 0o640,
                group: Some(format!("_{}", package.id())),
                parent_mode: 0o755,
            },
        ];
        Ok(Plan {
            files,
            service: Default::default(),
        })
    }

    /// Public values a test put at `/s/public/<package>/<name>`.
    fn public_value(
        &self,
        ctx: &SettingsContext,
        package: &Package,
        public_name: &str,
    ) -> Result<Option<String>, ApiError> {
        let path = format!("/s/public/{}/{public_name}", package.id());
        Ok(ctx
            .sys
            .read_file(Path::new(&path))?
            .map(|b| String::from_utf8_lossy(&b).into_owned()))
    }
}

/// A helper over `sys` that allows uid 1000, with a KDF cheap enough for tests.
pub fn helper(sys: FakeSystem) -> Helper<FakeSystem> {
    Helper::new(
        sys,
        AllowedUids::new([1000]),
        Box::new(TestPolicy),
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
    .with_radios(crate::radio_tests::no_radios())
}
