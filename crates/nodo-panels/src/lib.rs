//! Panels for the device's screen (docs/SCREEN.md): finding the panels installed packages offer,
//! fetching their data, running their Lua functions, keeping their history and arranging them.
//!
//! Nothing here draws. `nodo-screen` draws what [`PanelState`] holds, so all of this can be tested
//! without a terminal.

pub mod layout;
pub mod output;
pub mod script;
pub mod source;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nodo_manifest::{Manifest, Panel, PanelSource};

pub use layout::{Layout, PanelKey, Placement, SizeRule};
pub use output::{PanelOutput, Status};
pub use script::{run_panel, RunError};
pub use source::{fetch, FetchError};

/// History kept per panel, oldest first.
pub const MAX_HISTORY: usize = output::MAX_SERIES;
const MAX_SCRIPT_BYTES: u64 = 256 * 1024;

/// The base system's own package name for built-in panels. `nodo` is a reserved id, so no feed
/// package can take it.
pub const BUILTIN_PACKAGE: &str = "nodo";

/// Where installed packages keep their manifests and files.
#[derive(Debug, Clone)]
pub struct Roots {
    pub system_manifests: PathBuf,
    pub app_manifests: PathBuf,
    /// One directory per base package, holding its Lua files.
    pub system_packages: PathBuf,
    /// One directory per installed app.
    pub apps: PathBuf,
}

impl Default for Roots {
    fn default() -> Self {
        Roots {
            system_manifests: "/usr/share/nodo/manifests".into(),
            app_manifests: "/var/lib/nodo/manifests".into(),
            system_packages: "/usr/share/nodo/packages".into(),
            apps: "/opt/nodo/apps".into(),
        }
    }
}

/// A panel some package offers, with its script already read.
#[derive(Debug, Clone)]
pub struct Offered {
    pub key: PanelKey,
    pub title: String,
    pub refresh: Duration,
    pub rule: SizeRule,
    pub source: PanelSource,
    /// The Lua file, or why it could not be read.
    pub script: Result<String, String>,
}

impl Offered {
    fn new(package: &str, panel: &Panel, script: Result<String, String>) -> Self {
        Offered {
            key: PanelKey::new(package, &panel.id),
            title: panel.title.clone(),
            refresh: Duration::from_secs(u64::from(panel.refresh_secs)),
            rule: SizeRule { min: panel.min_size, max: panel.max_size, initial: panel.initial_size() },
            source: panel.source.clone(),
            script,
        }
    }
}

/// The base system's device panel, built into the screen.
pub fn builtin_panels() -> Vec<Offered> {
    let device = Panel {
        id: "device".into(),
        title: "Device".into(),
        script: "device.lua".into(),
        refresh_secs: 10,
        min_size: [2, 1],
        max_size: [4, 2],
        default_size: Some([2, 2]),
        source: PanelSource::Device {},
    };
    vec![Offered::new(BUILTIN_PACKAGE, &device, Ok(include_str!("device.lua").to_string()))]
}

/// Every panel installed packages offer, plus the built-in ones, and the problems found on the way
/// (for the log). A base package wins over an app with the same id, as in the helper.
pub fn discover(roots: &Roots) -> (Vec<Offered>, Vec<String>) {
    let mut offered = builtin_panels();
    let mut problems = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (dir, files, system) in [(&roots.system_manifests, &roots.system_packages, true), (&roots.app_manifests, &roots.apps, false)] {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
        paths.sort();
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else {
                problems.push(format!("cannot read {}", path.display()));
                continue;
            };
            // Base manifests are signed with the image; app manifests came from a feed and are
            // checked again here, so a damaged file cannot widen what a panel reaches.
            let parsed = if system { Manifest::parse_system(&text) } else { Manifest::parse(&text) };
            let manifest = match parsed {
                Ok(manifest) => manifest,
                Err(error) => {
                    problems.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            if manifest.panels.is_empty() || !seen.insert(manifest.id.clone()) {
                continue;
            }
            let package_dir = files.join(&manifest.id);
            for panel in &manifest.panels {
                offered.push(Offered::new(&manifest.id, panel, read_script(&package_dir, &panel.script)));
            }
        }
    }
    (offered, problems)
}

fn read_script(package_dir: &Path, relative: &str) -> Result<String, String> {
    if !nodo_manifest::is_valid_package_path(relative) {
        return Err("the panel file's path is not inside the package".into());
    }
    let path = package_dir.join(relative);
    let meta = std::fs::metadata(&path).map_err(|_| "the panel file is missing".to_string())?;
    if !meta.is_file() || meta.len() > MAX_SCRIPT_BYTES {
        return Err("the panel file is not a file of at most 256 KiB".into());
    }
    std::fs::read_to_string(&path).map_err(|_| "the panel file is not readable text".to_string())
}

/// The size rules of every offered panel, for [`Layout`].
pub fn size_rules(offered: &[Offered]) -> BTreeMap<PanelKey, SizeRule> {
    offered.iter().map(|o| (o.key.clone(), o.rule)).collect()
}

/// What the screen knows about one panel between runs.
#[derive(Debug, Clone)]
pub struct PanelState {
    pub last: Option<PanelOutput>,
    /// When `last` was produced.
    pub last_ok: Option<Instant>,
    /// Why the latest run failed, if it did.
    pub failure: Option<String>,
    pub history: VecDeque<f64>,
    pub next_due: Instant,
}

impl PanelState {
    pub fn new(now: Instant) -> Self {
        PanelState { last: None, last_ok: None, failure: None, history: VecDeque::new(), next_due: now }
    }

    /// Records a run's result and schedules the next.
    pub fn record(&mut self, result: Result<PanelOutput, String>, now: Instant, refresh: Duration) {
        match result {
            Ok(output) => {
                if let Some(sample) = output.sample {
                    if self.history.len() == MAX_HISTORY {
                        self.history.pop_front();
                    }
                    self.history.push_back(sample);
                }
                self.last = Some(output);
                self.last_ok = Some(now);
                self.failure = None;
            }
            Err(reason) => self.failure = Some(reason),
        }
        self.next_due = now + refresh;
    }

    /// The last output is shown dimmed once a run after it has failed.
    pub fn is_stale(&self) -> bool {
        self.last.is_some() && self.failure.is_some()
    }

    pub fn history(&self) -> Vec<f64> {
        self.history.iter().copied().collect()
    }
}

/// One complete refresh of a panel: fetch, then run its function. Run off the drawing thread.
pub fn refresh(panel: &Offered, history: &[f64], device_root: &Path) -> Result<PanelOutput, String> {
    let script = panel.script.as_ref().map_err(Clone::clone)?;
    let data = fetch(&panel.source, device_root).map_err(|e| format!("the service {e}"))?;
    let name = format!("{}/{}", panel.key.package, panel.key.panel);
    run_panel(script, &name, &data, history).map_err(|e| format!("the panel {e}"))
}

/// "12 s", "4 min", "3 h", for how stale a panel is.
pub fn age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs / 60),
        _ => format!("{} h", secs / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_panel_runs_on_real_looking_data() {
        let device = &builtin_panels()[0];
        let data = serde_json::json!({
            "temp_c": 52.5, "mem_total": 32_000_000_000u64, "mem_used": 6_400_000_000u64,
            "ssd_total": 2_000_000_000_000u64, "ssd_used": 440_000_000_000u64, "uptime_secs": 259_200
        });
        let out = run_panel(device.script.as_ref().unwrap(), "nodo/device", &data, &[]).unwrap();
        assert_eq!(out.big.as_deref(), Some("52 °C"));
        assert_eq!(out.status, Some(Status::Ok));
        assert!(out.rows.iter().any(|(label, value)| label == "ssd" && value.contains("22%")), "{:?}", out.rows);
        assert!(out.rows.iter().any(|(label, value)| label == "up" && value == "3 days"), "{:?}", out.rows);
        // With nothing readable it still shows, rather than failing.
        let empty = run_panel(device.script.as_ref().unwrap(), "nodo/device", &serde_json::json!({}), &[]).unwrap();
        assert_eq!(empty.big.as_deref(), Some("--"));
        // Hot is a warning.
        let hot = run_panel(device.script.as_ref().unwrap(), "nodo/device", &serde_json::json!({ "temp_c": 86 }), &[]).unwrap();
        assert_eq!(hot.status, Some(Status::Warn));
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn discovers_panels_from_installed_packages() {
        let root = std::env::temp_dir().join(format!("nodo-panels-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let roots = Roots {
            system_manifests: root.join("sys/manifests"),
            app_manifests: root.join("var/manifests"),
            system_packages: root.join("sys/packages"),
            apps: root.join("opt/apps"),
        };
        let panel = "[[panels]]\nid = \"sales\"\ntitle = \"Sales\"\nscript = \"panels/sales.lua\"\nrefresh_secs = 10\n\
            min_size = [2, 1]\nmax_size = [4, 2]\nsource = { http = { url = \"http://127.0.0.1:8081/stats\" } }\n";
        let shop = format!(
            "api_version = 1\nid = \"shop\"\nversion = \"1.0\"\nexec = [\"bin/shop\"]\n\
             health = {{ http = {{ url = \"http://127.0.0.1:8081/status\" }} }}\n{panel}"
        );
        write(&roots.app_manifests.join("shop.toml"), &shop);
        write(&roots.apps.join("shop/panels/sales.lua"), "return { panel = function(d) return { big = d.sales } end }");
        // A damaged app manifest is skipped and reported, never trusted.
        write(&roots.app_manifests.join("bad.toml"), "api_version = 1\nid = \"bad\"\n");
        // An app whose panel file is missing still appears, and says why it cannot run.
        write(&roots.app_manifests.join("lost.toml"), &shop.replace("\"shop\"", "\"lost\""));
        let (offered, problems) = discover(&roots);
        let keys: Vec<String> = offered.iter().map(|o| format!("{}/{}", o.key.package, o.key.panel)).collect();
        assert_eq!(keys, ["nodo/device", "lost/sales", "shop/sales"]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        let lost = &offered[1];
        assert_eq!(lost.script.as_ref().unwrap_err(), "the panel file is missing");
        let shop_panel = &offered[2];
        assert_eq!(shop_panel.refresh, Duration::from_secs(10));
        assert!(shop_panel.script.is_ok());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn state_keeps_history_and_goes_stale_on_failure() {
        let t0 = Instant::now();
        let refresh = Duration::from_secs(10);
        let mut state = PanelState::new(t0);
        let sample = |n: f64| PanelOutput { sample: Some(n), big: Some(format!("{n}")), ..PanelOutput::default() };
        for i in 0..(MAX_HISTORY + 5) {
            state.record(Ok(sample(i as f64)), t0, refresh);
        }
        assert_eq!(state.history.len(), MAX_HISTORY);
        assert_eq!(state.history.front(), Some(&5.0), "oldest dropped first");
        assert!(!state.is_stale());
        state.record(Err("the service timed out".into()), t0 + Duration::from_secs(20), refresh);
        assert!(state.is_stale(), "keeps the last output, dimmed");
        assert_eq!(state.last.as_ref().unwrap().big.as_deref(), Some(&*format!("{}", (MAX_HISTORY + 4) as f64)));
        assert_eq!(state.next_due, t0 + Duration::from_secs(30));
        assert_eq!(age(Duration::from_secs(150)), "2 min");
    }
}
