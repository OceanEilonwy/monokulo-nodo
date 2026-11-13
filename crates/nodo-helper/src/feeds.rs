//! User-added feeds: pinning their signing keys, refreshing their indexes, and a changed key.
//!
//! Feeds are named on this device, and pins are kept by address in a `nodo_feed::PinStore`. A
//! key change blocks the feed until the user trusts the new key by typing the feed's name, which
//! the helper checks, together with both keys so the approval fits only this one change.
//!
//! The image also ships default feeds (`/usr/share/nodo/default-feeds.toml`), each with the
//! fingerprint of its key. One is added, already pinned to that key, the first time the feed
//! state is read on a device that never had it, so the device never asks the server which key to
//! trust. It is added once: a default the owner removed stays removed, and a fingerprint changed in
//! a later image never replaces a pin; a server that signs with another key goes through the
//! normal key change like any feed.

use std::collections::{BTreeMap, BTreeSet};

use nodo_api::{
    ApiError, CatalogEntry, FeedInfo, FeedKeyChange, FeedProbe, InstalledFromFeed,
    KeyChangeConfirmation, Response,
};
use nodo_feed::{Approval, Decision, KeyFingerprint, PinStore};
use nodo_install::Permissions;
use serde::{Deserialize, Serialize};

use crate::helper::{lock, Helper};
use crate::manage::failed;
use crate::pkgsys::IndexEntry;
use crate::system::System;

const MAX_ADDRESS: usize = 512;
const MAX_NAME: usize = 32;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FeedRecord {
    pub address: String,
    pub enabled: bool,
    pub added: u64,
    #[serde(default)]
    pub last_checked: Option<u64>,
    #[serde(default)]
    pub problem: Option<String>,
    /// The key the feed is signed with now, when it differs from the pin.
    #[serde(default)]
    pub offered_key: Option<String>,
    #[serde(default)]
    pub index: Vec<IndexEntry>,
    /// What the feed offers under its new key. Shown, never installed from.
    #[serde(default)]
    pub offered_index: Vec<IndexEntry>,
}

/// A package installed from a feed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InstalledRecord {
    pub feed: String,
    /// `version_revision`.
    pub version: String,
    /// The verified file of this version, which an update rolls back to.
    #[serde(default)]
    pub kept: Option<std::path::PathBuf>,
    /// The version before this one, which a rollback by hand puts back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<crate::updates::PreviousVersion>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FeedState {
    #[serde(default)]
    pub pins: PinStore,
    /// By name.
    #[serde(default)]
    pub feeds: BTreeMap<String, FeedRecord>,
    /// What the user approved for each package, by id.
    #[serde(default)]
    pub approved: BTreeMap<String, Permissions>,
    #[serde(default)]
    pub installed: BTreeMap<String, InstalledRecord>,
    /// Addresses of the image's default feeds this device was already given, whether or not they
    /// are still added. A removed default is remembered here, so it does not come back.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub defaults_seeded: BTreeSet<String>,
}

/// One feed the image ships with.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DefaultFeed {
    pub name: String,
    pub address: String,
    pub fingerprint: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultFeeds {
    #[serde(default)]
    feed: Vec<DefaultFeed>,
}

/// Reads the image's default feeds file.
pub(crate) fn parse_default_feeds(text: &str) -> Result<Vec<DefaultFeed>, String> {
    let parsed: DefaultFeeds = toml::from_str(text).map_err(|e| e.to_string())?;
    for feed in &parsed.feed {
        check_address(&feed.address).map_err(|e| format!("{}: {e}", feed.address))?;
        KeyFingerprint::new(&feed.fingerprint).map_err(|e| format!("{}: {e}", feed.address))?;
    }
    Ok(parsed.feed)
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
}

/// How long an enabled feed's index may go unread before the scheduled run reads it again. Reading
/// a feed only updates what the device lists as available; it never installs anything.
pub(crate) const FEED_RECHECK_SECS: u64 = 6 * 60 * 60;

impl FeedState {
    /// Whether an enabled feed was never read, or was last read `FEED_RECHECK_SECS` or more ago.
    pub fn refresh_due(&self, now: u64) -> bool {
        self.feeds.values().filter(|f| f.enabled).any(|f| {
            f.last_checked
                .is_none_or(|at| now.saturating_sub(at) >= FEED_RECHECK_SECS)
        })
    }

    pub fn feed(&self, name: &str) -> Result<&FeedRecord, ApiError> {
        self.feeds
            .get(name)
            .ok_or_else(|| ApiError::NotFound(format!("feed {name}")))
    }

    fn name_for(&self, address: &str) -> Option<&str> {
        self.feeds
            .iter()
            .find(|(_, f)| f.address == address)
            .map(|(n, _)| n.as_str())
    }

    /// Adds each default feed this device has never been given, pinned to the shipped key.
    /// Returns whether the state changed.
    pub fn seed_defaults(&mut self, defaults: &[DefaultFeed], now: u64) -> bool {
        let mut changed = false;
        for default in defaults {
            if !self.defaults_seeded.insert(default.address.clone()) {
                continue;
            }
            changed = true;
            let Ok(key) = KeyFingerprint::new(&default.fingerprint) else {
                continue;
            };
            // The owner added this address before, and pinned the key they confirmed: theirs
            // stays, whatever the image ships.
            if self.name_for(&default.address).is_some()
                || self.pins.feeds.contains_key(&default.address)
            {
                continue;
            }
            if self.pins.add_feed(&default.address, &key) != Decision::Pinned {
                continue;
            }
            let name = if plain_name(&default.name) && !self.feeds.contains_key(&default.name) {
                default.name.clone()
            } else {
                self.new_name(&default.address)
            };
            self.feeds.insert(
                name,
                FeedRecord {
                    address: default.address.clone(),
                    enabled: true,
                    added: now,
                    ..FeedRecord::default()
                },
            );
        }
        changed
    }

    /// A short name from the address's last segment, made unique on this device.
    fn new_name(&self, address: &str) -> String {
        let last = address
            .trim_end_matches('/')
            .rsplit(['/', ':'])
            .find(|s| !s.is_empty())
            .unwrap_or("feed");
        let mut base: String = last
            .chars()
            .map(|c| c.to_ascii_lowercase())
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        base = base.trim_matches('-').chars().take(MAX_NAME).collect();
        if base.is_empty() {
            base = "feed".to_owned();
        }
        let mut name = base.clone();
        let mut n = 2;
        while self.feeds.contains_key(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        name
    }
}

/// How xbps shows a key, so the user can compare it with `xbps-query -vL` or the publisher.
pub(crate) fn shown_key(key: &str) -> String {
    let hex = key.replace(':', "");
    hex.as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

/// An `https://` or `http://` address, or an absolute directory such as a USB stick. Nothing
/// that could be read as an option or climb out of the directory.
fn check_address(address: &str) -> Result<(), ApiError> {
    let bad = |why: &str| Err(ApiError::Failed(format!("that feed address {why}")));
    if address.is_empty() || address.len() > MAX_ADDRESS {
        return bad("is empty or too long");
    }
    if address
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return bad("contains spaces or control characters");
    }
    let path = if let Some(rest) = address
        .strip_prefix("https://")
        .or_else(|| address.strip_prefix("http://"))
    {
        let host = rest.split('/').next().unwrap_or_default();
        if host.is_empty() || host.starts_with('-') || host.contains('@') {
            return bad("has no usable host");
        }
        rest.split_once('/').map(|(_, p)| p).unwrap_or_default()
    } else if address.starts_with('/') {
        address
    } else {
        return bad("must start with https://, http:// or /");
    };
    if path.split('/').any(|s| s == ".." || s == ".") {
        return bad("may not contain . or .. segments");
    }
    Ok(())
}

impl<S: System> Helper<S> {
    /// The feed state, with any default feed this device has never had added to it.
    fn feeds_state(&self) -> Result<FeedState, ApiError> {
        let mut state: FeedState = self.load_state(&self.manage.paths.feeds)?;
        let defaults = self.default_feeds();
        if state.seed_defaults(&defaults, self.manage.packages.now()) {
            // Seeding gives the same result each time, so a failed write is tried again on the
            // next read rather than failing this one.
            if let Err(e) = self.save_feeds(&state) {
                eprintln!("nodo-helper: could not record the default feeds: {e}");
            }
        }
        Ok(state)
    }

    /// The image's default feeds. A missing file means none; a broken one is logged and ignored,
    /// so it can never stop the owner's own feeds from working.
    fn default_feeds(&self) -> Vec<DefaultFeed> {
        let path = &self.manage.paths.default_feeds;
        let bytes = match self.sys.read_file(path) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Vec::new(),
            Err(e) => {
                eprintln!("nodo-helper: {}: {e}", path.display());
                return Vec::new();
            }
        };
        match std::str::from_utf8(&bytes)
            .map_err(|e| e.to_string())
            .and_then(parse_default_feeds)
        {
            Ok(feeds) => feeds,
            Err(e) => {
                eprintln!("nodo-helper: ignoring {}: {e}", path.display());
                Vec::new()
            }
        }
    }

    pub(crate) fn save_feeds(&self, state: &FeedState) -> Result<(), ApiError> {
        self.save_state(&self.manage.paths.feeds, state)
    }

    /// Reads the feed state, lets `f` change it, and writes it back if `f` succeeded.
    pub(crate) fn with_feeds<T>(
        &self,
        f: impl FnOnce(&mut FeedState) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let _guard = lock(&self.manage.state);
        let mut state = self.feeds_state()?;
        let out = f(&mut state)?;
        self.save_feeds(&state)?;
        Ok(out)
    }

    pub(crate) fn read_feeds(&self) -> Result<FeedState, ApiError> {
        let _guard = lock(&self.manage.state);
        self.feeds_state()
    }

    pub(crate) fn list_feeds(&self) -> Result<Response, ApiError> {
        Ok(Response::Feeds(feed_infos(&self.read_feeds()?)))
    }

    pub(crate) fn probe_feed(&self, address: &str) -> Result<Response, ApiError> {
        check_address(address)?;
        let index = self.manage.packages.index(address).map_err(failed)?;
        let state = self.read_feeds()?;
        let name = match state.name_for(address) {
            Some(name) => name.to_owned(),
            None => state.new_name(address),
        };
        Ok(Response::FeedProbe(FeedProbe {
            name,
            address: address.to_owned(),
            fingerprint: shown_key(&index.fingerprint.to_string()),
            packages: index.packages.len() as u32,
        }))
    }

    /// The feed is read again here, so a key that changed after the probe is caught: the user
    /// approved the key they were shown, and only that key is pinned.
    pub(crate) fn add_feed(&self, address: &str, fingerprint: &str) -> Result<Response, ApiError> {
        check_address(address)?;
        let expected = KeyFingerprint::new(fingerprint)
            .map_err(|_| ApiError::NeedsConfirmation("the confirmed key is not valid".into()))?;
        let index = self.manage.packages.index(address).map_err(failed)?;
        if index.fingerprint != expected {
            return Err(ApiError::NeedsConfirmation(format!(
                "the feed is now signed by {}, not the key you confirmed. Check it again.",
                shown_key(&index.fingerprint.to_string())
            )));
        }
        let now = self.manage.packages.now();
        self.with_feeds(|state| {
            if let Some(name) = state.name_for(address) {
                return Err(ApiError::Failed(format!(
                    "this feed is already added as {name}"
                )));
            }
            match state.pins.add_feed(address, &expected) {
                Decision::Blocked(_) => {
                    return Err(ApiError::Failed(
                        "this address was pinned to another key before".into(),
                    ))
                }
                Decision::Pinned | Decision::Allow => {}
            }
            let name = state.new_name(address);
            state.feeds.insert(
                name,
                FeedRecord {
                    address: address.to_owned(),
                    enabled: true,
                    added: now,
                    last_checked: Some(now),
                    index: index.packages,
                    ..FeedRecord::default()
                },
            );
            Ok(Response::Feeds(feed_infos(state)))
        })
    }

    /// Reads every enabled feed's index. A changed key blocks the feed; nothing from it is used
    /// until the user decides.
    pub(crate) fn refresh_feeds(&self) -> Result<Response, ApiError> {
        let targets: Vec<(String, String)> = self
            .read_feeds()?
            .feeds
            .iter()
            .filter(|(_, f)| f.enabled)
            .map(|(n, f)| (n.clone(), f.address.clone()))
            .collect();
        let results: Vec<_> = targets
            .into_iter()
            .map(|(name, address)| {
                let result = self.manage.packages.index(&address);
                (name, result)
            })
            .collect();
        let now = self.manage.packages.now();
        self.with_feeds(|state| {
            for (name, result) in results {
                let pinned = state
                    .feeds
                    .get(&name)
                    .and_then(|f| state.pins.feeds.get(&f.address))
                    .map(|p| p.key.clone());
                let Some(record) = state.feeds.get_mut(&name) else {
                    continue;
                };
                record.last_checked = Some(now);
                match result {
                    Err(problem) => record.problem = Some(problem),
                    Ok(index) => {
                        record.problem = None;
                        if pinned.as_ref() == Some(&index.fingerprint) {
                            record.offered_key = None;
                            record.offered_index = Vec::new();
                            record.index = index.packages;
                        } else {
                            // The old index stays, so installed packages keep their versions
                            // and nothing new from the other key appears.
                            record.offered_key = Some(index.fingerprint.to_string());
                            record.offered_index = index.packages;
                        }
                    }
                }
            }
            Ok(Response::Feeds(feed_infos(state)))
        })
    }

    /// Reads every enabled feed's index now, for the helper's start.
    pub fn refresh_feeds_now(&self) -> Result<(), ApiError> {
        self.refresh_feeds().map(|_| ())
    }

    /// Reads the feeds' indexes when one was never read (a default feed just seeded, say) or has
    /// not been read for `FEED_RECHECK_SECS`. Returns whether it read them.
    pub fn refresh_feeds_if_due(&self) -> Result<bool, ApiError> {
        let now = self.manage.packages.now();
        if !self.read_feeds()?.refresh_due(now) {
            return Ok(false);
        }
        self.refresh_feeds()?;
        Ok(true)
    }

    pub(crate) fn set_feed_enabled(&self, name: &str, enabled: bool) -> Result<Response, ApiError> {
        self.with_feeds(|state| {
            state
                .feeds
                .get_mut(name)
                .ok_or_else(|| ApiError::NotFound(format!("feed {name}")))?
                .enabled = enabled;
            Ok(Response::Feeds(feed_infos(state)))
        })
    }

    /// Removing a feed drops its pin and floors. Installed packages keep running, without
    /// updates, and a feed added again starts from a fresh pin.
    pub(crate) fn remove_feed(&self, name: &str) -> Result<Response, ApiError> {
        self.with_feeds(|state| {
            let record = state
                .feeds
                .remove(name)
                .ok_or_else(|| ApiError::NotFound(format!("feed {name}")))?;
            state.pins.feeds.remove(&record.address);
            Ok(Response::Feeds(feed_infos(state)))
        })
    }

    pub(crate) fn feed_key_change(&self, name: &str) -> Result<Response, ApiError> {
        let state = self.read_feeds()?;
        let record = state.feed(name)?;
        let offered = record
            .offered_key
            .clone()
            .ok_or_else(|| ApiError::Failed(format!("{name}'s signing key has not changed")))?;
        let pinned = state
            .pins
            .feeds
            .get(&record.address)
            .map(|p| p.key.to_string())
            .unwrap_or_default();
        Ok(Response::FeedKeyChange(FeedKeyChange {
            feed: name.to_owned(),
            address: record.address.clone(),
            pinned: shown_key(&pinned),
            offered: shown_key(&offered),
            affected: installed_from(&state, name),
        }))
    }

    pub(crate) fn trust_feed_key(
        &self,
        name: &str,
        confirmation: &KeyChangeConfirmation,
    ) -> Result<Response, ApiError> {
        if confirmation.typed_feed_name != name {
            return Err(ApiError::NeedsConfirmation(format!(
                "type the feed name {name} exactly to trust its new key"
            )));
        }
        let parse = |text: &str| {
            KeyFingerprint::new(text)
                .map_err(|_| ApiError::NeedsConfirmation("a confirmed key is not valid".into()))
        };
        let (from, to) = (parse(&confirmation.from)?, parse(&confirmation.to)?);
        self.with_feeds(|state| {
            let record = state.feed(name)?.clone();
            if record.offered_key.as_deref() != Some(to.to_string().as_str()) {
                return Err(ApiError::NeedsConfirmation(
                    "the feed's key changed again since you looked. Check it again.".into(),
                ));
            }
            let approval = Approval::KeyChange {
                feed: record.address.clone(),
                from,
                to,
            };
            state.pins.apply_approval(&approval).map_err(|e| {
                ApiError::NeedsConfirmation(format!("{e}. Check the keys again."))
            })?;
            if let Some(record) = state.feeds.get_mut(name) {
                record.offered_key = None;
                record.index = std::mem::take(&mut record.offered_index);
            }
            Ok(Response::Feeds(feed_infos(state)))
        })
    }

    pub(crate) fn catalog(&self) -> Result<Response, ApiError> {
        let state = self.read_feeds()?;
        let mut entries = Vec::new();
        for (name, record) in state.feeds.iter().filter(|(_, f)| f.enabled) {
            for entry in &record.index {
                let installed = state
                    .installed
                    .get(&entry.name)
                    .filter(|i| i.feed == *name)
                    .map(|i| i.version.clone());
                entries.push(CatalogEntry {
                    feed: name.clone(),
                    name: entry.name.clone(),
                    version: entry.version.clone(),
                    installed,
                    size_bytes: entry.size,
                });
            }
        }
        Ok(Response::Catalog(entries))
    }
}

fn installed_from(state: &FeedState, feed: &str) -> Vec<InstalledFromFeed> {
    state
        .installed
        .iter()
        .filter(|(_, i)| i.feed == feed)
        .map(|(id, i)| InstalledFromFeed {
            package: id.clone(),
            version: i.version.clone(),
        })
        .collect()
}

fn feed_infos(state: &FeedState) -> Vec<FeedInfo> {
    state
        .feeds
        .iter()
        .map(|(name, f)| FeedInfo {
            name: name.clone(),
            address: f.address.clone(),
            fingerprint: state
                .pins
                .feeds
                .get(&f.address)
                .map(|p| shown_key(&p.key.to_string()))
                .unwrap_or_default(),
            enabled: f.enabled,
            packages: f.index.len() as u32,
            key_changed: f.offered_key.is_some(),
            last_checked: f.last_checked.map(|t| t.to_string()),
            problem: f.problem.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_urls_or_absolute_directories() {
        for good in [
            "https://feed.example/apps",
            "http://192.168.1.5:8000/feed/",
            "/media/usb/feed",
        ] {
            assert!(check_address(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "feed.example",
            "-oProxyCommand=x",
            "https://",
            "https://-x/",
            "https://user@host/",
            "/media/../etc",
            "https://h/a/../b",
            "https://h/a b",
            "/x\n",
            "ftp://h/",
        ] {
            assert!(check_address(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn names_come_from_the_address_and_never_collide() {
        let mut state = FeedState::default();
        assert_eq!(state.new_name("https://github.com/ex/Monero_Tools/"), "monero-tools");
        state.feeds.insert("monero-tools".into(), FeedRecord::default());
        assert_eq!(state.new_name("https://other/monero-tools"), "monero-tools-2");
        assert_eq!(state.new_name("https://h/"), "h");
        assert_eq!(state.new_name("/srv/feed"), "feed");
    }

    #[test]
    fn keys_show_as_xbps_does() {
        assert_eq!(shown_key("d73e73fc"), "d7:3e:73:fc");
    }
}
