//! Per-feed signing key pins and version floors.
//!
//! This models trust on first use. The first time a feed is added its key is pinned. From then on a
//! different key, or a version below the highest seen from that feed, is blocked until the user
//! approves that exact change. All of this is plain data and logic: the caller owns persistence,
//! shows the reasons to the user, and supplies the key after verifying the signature with it.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::version::compare_versions;

/// MD5 sized, which is what xbps shows as a key fingerprint. Stronger ones are longer.
const MIN_FINGERPRINT_HEX: usize = 32;
const MAX_FINGERPRINT_HEX: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KeyError {
    #[error("a key fingerprint must be {MIN_FINGERPRINT_HEX} to {MAX_FINGERPRINT_HEX} hex digits")]
    Invalid,
}

/// A key fingerprint in one canonical spelling, so that formatting differences never look like a
/// key change and a real key change cannot be disguised as a formatting difference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct KeyFingerprint(String);

impl KeyFingerprint {
    pub fn new(text: &str) -> Result<Self, KeyError> {
        let hex: String = text.chars().filter(|c| *c != ':').map(|c| c.to_ascii_lowercase()).collect();
        let valid = hex.chars().all(|c| c.is_ascii_hexdigit())
            && hex.len() >= MIN_FINGERPRINT_HEX
            && hex.len() <= MAX_FINGERPRINT_HEX;
        if !valid {
            return Err(KeyError::Invalid);
        }
        Ok(KeyFingerprint(hex))
    }
}

impl fmt::Display for KeyFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for KeyFingerprint {
    type Error = KeyError;

    fn try_from(text: String) -> Result<Self, KeyError> {
        KeyFingerprint::new(&text)
    }
}

impl From<KeyFingerprint> for String {
    fn from(key: KeyFingerprint) -> String {
        key.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockReason {
    UnknownFeed { feed: String },
    KeyChanged { feed: String, pinned: KeyFingerprint, offered: KeyFingerprint },
    Downgrade { feed: String, package: String, highest: String, offered: String },
    /// The version cannot be ordered, so it cannot be shown not to be a downgrade.
    UncomparableVersion { feed: String, package: String, version: String },
}

impl BlockReason {
    /// What the user would approve to lift this block, if it can be lifted.
    pub fn approval(&self) -> Option<Approval> {
        match self {
            BlockReason::KeyChanged { feed, pinned, offered } => {
                Some(Approval::KeyChange { feed: feed.clone(), from: pinned.clone(), to: offered.clone() })
            }
            BlockReason::Downgrade { feed, package, highest, offered } => Some(Approval::Downgrade {
                feed: feed.clone(),
                package: package.clone(),
                from: highest.clone(),
                to: offered.clone(),
            }),
            BlockReason::UnknownFeed { .. } | BlockReason::UncomparableVersion { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// The feed was new and its key is now pinned.
    Pinned,
    Allow,
    /// Nothing may proceed. All reasons are listed so the user sees the whole picture at once.
    Blocked(Vec<BlockReason>),
}

/// Approval of one exact change. It names both sides, so an approval given for one key or version
/// cannot be replayed to accept a later, different one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Approval {
    KeyChange { feed: String, from: KeyFingerprint, to: KeyFingerprint },
    Downgrade { feed: String, package: String, from: String, to: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ApprovalError {
    #[error("feed '{0}' is not pinned")]
    UnknownFeed(String),
    #[error("this approval no longer matches the feed's state")]
    Stale,
    #[error("'{0}' is not a version that can be compared")]
    UncomparableVersion(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedPin {
    pub key: KeyFingerprint,
    /// The floor for each package: the highest `version_revision` installed from this feed.
    #[serde(default)]
    pub highest: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinStore {
    #[serde(default)]
    pub feeds: BTreeMap<String, FeedPin>,
}

impl PinStore {
    pub fn new() -> Self {
        PinStore::default()
    }

    /// Adds a feed. First contact pins the key. Adding the feed again can never replace the pin:
    /// removing a feed is the caller's explicit action, and doing so also clears its floors.
    pub fn add_feed(&mut self, feed: &str, key: &KeyFingerprint) -> Decision {
        match self.feeds.get(feed) {
            None => {
                let pin = FeedPin { key: key.clone(), highest: BTreeMap::new() };
                self.feeds.insert(feed.to_string(), pin);
                Decision::Pinned
            }
            Some(pin) if pin.key == *key => Decision::Allow,
            Some(pin) => Decision::Blocked(vec![BlockReason::KeyChanged {
                feed: feed.to_string(),
                pinned: pin.key.clone(),
                offered: key.clone(),
            }]),
        }
    }

    /// Decides whether a package from a feed may be installed. Changes nothing.
    pub fn check_package(&self, feed: &str, key: &KeyFingerprint, package: &str, version: &str) -> Decision {
        let Some(pin) = self.feeds.get(feed) else {
            return Decision::Blocked(vec![BlockReason::UnknownFeed { feed: feed.to_string() }]);
        };
        let mut reasons = Vec::new();
        if pin.key != *key {
            reasons.push(BlockReason::KeyChanged {
                feed: feed.to_string(),
                pinned: pin.key.clone(),
                offered: key.clone(),
            });
        }
        let uncomparable = || BlockReason::UncomparableVersion {
            feed: feed.to_string(),
            package: package.to_string(),
            version: version.to_string(),
        };
        match pin.highest.get(package) {
            None => {
                // There is no floor yet, but this version would become one.
                if compare_versions(version, version).is_none() {
                    reasons.push(uncomparable());
                }
            }
            Some(highest) => match compare_versions(version, highest) {
                None => reasons.push(uncomparable()),
                Some(Ordering::Less) => reasons.push(BlockReason::Downgrade {
                    feed: feed.to_string(),
                    package: package.to_string(),
                    highest: highest.clone(),
                    offered: version.to_string(),
                }),
                Some(_) => {}
            },
        }
        if reasons.is_empty() {
            Decision::Allow
        } else {
            Decision::Blocked(reasons)
        }
    }

    /// Records a completed install, raising the package's floor. A lower version never lowers it:
    /// that only happens through an approved downgrade.
    pub fn record_installed(&mut self, feed: &str, package: &str, version: &str) -> Result<(), ApprovalError> {
        let pin = self.feeds.get_mut(feed).ok_or_else(|| ApprovalError::UnknownFeed(feed.to_string()))?;
        if compare_versions(version, version).is_none() {
            return Err(ApprovalError::UncomparableVersion(version.to_string()));
        }
        let raises = match pin.highest.get(package) {
            None => true,
            Some(highest) => compare_versions(version, highest) == Some(Ordering::Greater),
        };
        if raises {
            pin.highest.insert(package.to_string(), version.to_string());
        }
        Ok(())
    }

    /// Applies an approval the user has given. Only call this from a real user decision.
    pub fn apply_approval(&mut self, approval: &Approval) -> Result<(), ApprovalError> {
        match approval {
            Approval::KeyChange { feed, from, to } => {
                let pin = self.feeds.get_mut(feed).ok_or_else(|| ApprovalError::UnknownFeed(feed.clone()))?;
                if pin.key != *from {
                    return Err(ApprovalError::Stale);
                }
                // Floors survive, because a new key is exactly when a rollback is most likely.
                pin.key = to.clone();
            }
            Approval::Downgrade { feed, package, from, to } => {
                let pin = self.feeds.get_mut(feed).ok_or_else(|| ApprovalError::UnknownFeed(feed.clone()))?;
                if pin.highest.get(package) != Some(from) {
                    return Err(ApprovalError::Stale);
                }
                if compare_versions(to, from).is_none() {
                    return Err(ApprovalError::UncomparableVersion(to.clone()));
                }
                pin.highest.insert(package.clone(), to.clone());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = "https://feed.example/repo";

    fn key(byte: char) -> KeyFingerprint {
        KeyFingerprint::new(&byte.to_string().repeat(32)).unwrap()
    }

    fn pinned() -> PinStore {
        let mut store = PinStore::new();
        assert_eq!(store.add_feed(FEED, &key('a')), Decision::Pinned);
        store
    }

    fn only_reason(decision: Decision) -> BlockReason {
        match decision {
            Decision::Blocked(mut reasons) if reasons.len() == 1 => reasons.remove(0),
            other => panic!("expected one block reason, got {other:?}"),
        }
    }

    #[test]
    fn fingerprints_have_one_spelling() {
        let colons = "AA:BB:".repeat(8) + "AA:BB:CC:DD:EE:FF:00:11";
        let plain = "aabb".repeat(8) + "aabbccddeeff0011";
        assert_eq!(KeyFingerprint::new(&colons).unwrap(), KeyFingerprint::new(&plain).unwrap());
        for bad in ["", "abc", "zz".repeat(16).as_str(), "a".repeat(129).as_str(), "a b".repeat(16).as_str()] {
            assert_eq!(KeyFingerprint::new(bad), Err(KeyError::Invalid), "{bad:?}");
        }
    }

    #[test]
    fn stored_fingerprints_are_revalidated() {
        assert!(serde_plain_key("\"nothex\"").is_err());
        assert!(serde_plain_key(&format!("\"{}\"", "a".repeat(32))).is_ok());
    }

    fn serde_plain_key(text: &str) -> Result<KeyFingerprint, toml::de::Error> {
        #[derive(Deserialize)]
        struct Holder {
            key: KeyFingerprint,
        }
        toml::from_str::<Holder>(&format!("key = {text}")).map(|h| h.key)
    }

    #[test]
    fn first_add_pins_and_repeat_add_is_allowed() {
        let mut store = pinned();
        assert_eq!(store.feeds[FEED].key, key('a'));
        assert_eq!(store.add_feed(FEED, &key('a')), Decision::Allow);
    }

    #[test]
    fn adding_with_a_changed_key_blocks_and_keeps_the_pin() {
        let mut store = pinned();
        let reason = only_reason(store.add_feed(FEED, &key('b')));
        assert_eq!(reason, BlockReason::KeyChanged { feed: FEED.into(), pinned: key('a'), offered: key('b') });
        assert_eq!(store.feeds[FEED].key, key('a'));
    }

    #[test]
    fn pins_are_per_feed() {
        let mut store = pinned();
        assert_eq!(store.add_feed("https://other.example", &key('b')), Decision::Pinned);
        assert_eq!(store.check_package("https://other.example", &key('b'), "shop", "1.0_1"), Decision::Allow);
        assert!(matches!(store.check_package(FEED, &key('b'), "shop", "1.0_1"), Decision::Blocked(_)));
    }

    #[test]
    fn unknown_feeds_are_blocked_without_an_approval_path() {
        let reason = only_reason(PinStore::new().check_package(FEED, &key('a'), "shop", "1.0_1"));
        assert_eq!(reason, BlockReason::UnknownFeed { feed: FEED.into() });
        assert_eq!(reason.approval(), None);
    }

    #[test]
    fn allows_new_packages_upgrades_and_reinstalls() {
        let mut store = pinned();
        assert_eq!(store.check_package(FEED, &key('a'), "shop", "1.0_1"), Decision::Allow);
        store.record_installed(FEED, "shop", "1.0_1").unwrap();
        assert_eq!(store.check_package(FEED, &key('a'), "shop", "1.0_1"), Decision::Allow);
        assert_eq!(store.check_package(FEED, &key('a'), "shop", "1.0_2"), Decision::Allow);
        assert_eq!(store.check_package(FEED, &key('a'), "shop", "1.1_1"), Decision::Allow);
    }

    #[test]
    fn blocks_downgrades() {
        let mut store = pinned();
        store.record_installed(FEED, "shop", "2.0_3").unwrap();
        for lower in ["2.0_2", "1.9_9", "2.0rc1_9"] {
            let reason = only_reason(store.check_package(FEED, &key('a'), "shop", lower));
            assert_eq!(
                reason,
                BlockReason::Downgrade {
                    feed: FEED.into(),
                    package: "shop".into(),
                    highest: "2.0_3".into(),
                    offered: lower.into(),
                }
            );
        }
    }

    #[test]
    fn floors_are_per_package() {
        let mut store = pinned();
        store.record_installed(FEED, "shop", "9.0_1").unwrap();
        assert_eq!(store.check_package(FEED, &key('a'), "other", "1.0_1"), Decision::Allow);
    }

    #[test]
    fn overflowing_versions_cannot_pass_as_comparable() {
        let mut store = pinned();
        store.record_installed(FEED, "shop", "2.0_1").unwrap();
        let reason = only_reason(store.check_package(FEED, &key('a'), "shop", "4294967297_1"));
        assert!(matches!(reason, BlockReason::UncomparableVersion { .. }));
        assert_eq!(reason.approval(), None);
        assert!(matches!(
            store.check_package(FEED, &key('a'), "new", "4294967297_1"),
            Decision::Blocked(_)
        ));
        assert!(store.record_installed(FEED, "shop", "4294967297_1").is_err());
    }

    #[test]
    fn reports_a_changed_key_and_a_downgrade_together() {
        let mut store = pinned();
        store.record_installed(FEED, "shop", "2.0_1").unwrap();
        let Decision::Blocked(reasons) = store.check_package(FEED, &key('b'), "shop", "1.0_1") else {
            panic!("expected a block");
        };
        assert_eq!(reasons.len(), 2);
    }

    #[test]
    fn checking_changes_nothing() {
        let mut store = pinned();
        store.record_installed(FEED, "shop", "2.0_1").unwrap();
        let before = store.clone();
        store.check_package(FEED, &key('b'), "shop", "1.0_1");
        store.check_package(FEED, &key('a'), "shop", "3.0_1");
        assert_eq!(store, before);
    }

    #[test]
    fn recording_never_lowers_a_floor() {
        let mut store = pinned();
        store.record_installed(FEED, "shop", "2.0_1").unwrap();
        store.record_installed(FEED, "shop", "1.0_1").unwrap();
        assert_eq!(store.feeds[FEED].highest["shop"], "2.0_1");
        assert!(matches!(store.record_installed("https://ghost", "shop", "1_1"), Err(ApprovalError::UnknownFeed(_))));
    }

    #[test]
    fn approving_a_key_change_replaces_the_pin_and_keeps_floors() {
        let mut store = pinned();
        store.record_installed(FEED, "shop", "2.0_1").unwrap();
        let approval = only_reason(store.add_feed(FEED, &key('b'))).approval().unwrap();
        store.apply_approval(&approval).unwrap();
        assert_eq!(store.check_package(FEED, &key('b'), "shop", "2.0_1"), Decision::Allow);
        assert!(matches!(store.check_package(FEED, &key('a'), "shop", "2.0_1"), Decision::Blocked(_)));
        let reason = only_reason(store.check_package(FEED, &key('b'), "shop", "1.0_1"));
        assert!(matches!(reason, BlockReason::Downgrade { .. }));
    }

    #[test]
    fn approving_a_downgrade_moves_the_floor() {
        let mut store = pinned();
        store.record_installed(FEED, "shop", "2.0_1").unwrap();
        let approval = only_reason(store.check_package(FEED, &key('a'), "shop", "1.0_1")).approval().unwrap();
        store.apply_approval(&approval).unwrap();
        assert_eq!(store.check_package(FEED, &key('a'), "shop", "1.0_1"), Decision::Allow);
        assert!(matches!(store.check_package(FEED, &key('a'), "shop", "0.9_1"), Decision::Blocked(_)));
    }

    #[test]
    fn approvals_cannot_be_replayed_or_redirected() {
        let mut store = pinned();
        let to_b = Approval::KeyChange { feed: FEED.into(), from: key('a'), to: key('b') };
        store.apply_approval(&to_b).unwrap();
        assert_eq!(store.apply_approval(&to_b), Err(ApprovalError::Stale), "already used");

        let wrong_from = Approval::KeyChange { feed: FEED.into(), from: key('c'), to: key('d') };
        assert_eq!(store.apply_approval(&wrong_from), Err(ApprovalError::Stale));
        assert_eq!(store.feeds[FEED].key, key('b'));

        let unknown = Approval::KeyChange { feed: "https://ghost".into(), from: key('a'), to: key('b') };
        assert!(matches!(store.apply_approval(&unknown), Err(ApprovalError::UnknownFeed(_))));

        store.record_installed(FEED, "shop", "2.0_1").unwrap();
        let stale = Approval::Downgrade { feed: FEED.into(), package: "shop".into(), from: "1.5_1".into(), to: "1.0_1".into() };
        assert_eq!(store.apply_approval(&stale), Err(ApprovalError::Stale));
        let garbage = Approval::Downgrade { feed: FEED.into(), package: "shop".into(), from: "2.0_1".into(), to: "99999999999_1".into() };
        assert!(matches!(store.apply_approval(&garbage), Err(ApprovalError::UncomparableVersion(_))));
        assert_eq!(store.feeds[FEED].highest["shop"], "2.0_1");
    }

    #[test]
    fn the_store_round_trips_through_toml() {
        let mut store = pinned();
        store.record_installed(FEED, "shop", "2.0_1").unwrap();
        let text = toml::to_string(&store).unwrap();
        assert_eq!(toml::from_str::<PinStore>(&text).unwrap(), store);
    }
}
