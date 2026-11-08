//! What a device trusts: its pinned signing keys and the highest snapshot serial it has accepted.
//!
//! The base image ships [`Pins`], which seed this state on first boot and set a serial floor, so
//! losing `/var/lib/nodo` cannot reopen the door to replaying older snapshots than the image itself.
//! After that, keys change only through a [`KeyStatement`] signed by the keys being replaced, or by
//! an explicit user approval naming both the old and the new keys.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::manifest::{
    KeyStatement, ManifestError, PublicKey, SignatureFile, SnapshotManifest, KEY_STATEMENT_CONTEXT, MANIFEST_CONTEXT,
};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TrustError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("snapshot serial {offered} is not newer than {current}")]
    Downgrade { current: u64, offered: u64 },
    #[error("a different manifest was already accepted with serial {serial}")]
    Conflict { serial: u64 },
    #[error("manifest is for channel {offered}, this device follows {expected}")]
    WrongChannel { expected: String, offered: String },
    #[error("manifest expired at {expires}")]
    Expired { expires: u64 },
    #[error("snapshot has no repository for {0}")]
    NoRepository(String),
    #[error("key statement {offered} is not newer than {current}")]
    StaleKeyStatement { current: u64, offered: u64 },
    #[error("this approval no longer matches the trusted keys")]
    StaleApproval,
}

/// Shipped read-only in the base image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pins {
    pub channel: String,
    pub keys: BTreeSet<PublicKey>,
    pub threshold: u32,
    /// The serial of the snapshot the image was built from.
    pub min_serial: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accepted {
    /// A newer snapshot. The state now records it.
    New(Box<SnapshotManifest>),
    /// The same manifest again, which is normal between publications.
    Unchanged,
}

/// A user's approval of a signing key change they verified some other way. It names the keys it
/// replaces, so it cannot be replayed after the keys change again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyApproval {
    pub from: BTreeSet<PublicKey>,
    pub to: PublicKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustState {
    pub channel: String,
    pub keys: BTreeSet<PublicKey>,
    pub threshold: u32,
    #[serde(default)]
    pub key_sequence: u64,
    /// The highest serial accepted, or the image's floor before the first one.
    pub serial: u64,
    /// Hash of the accepted manifest, so a second, different manifest with the same serial is
    /// treated as the attack it is rather than as "unchanged".
    #[serde(default)]
    pub manifest_sha256: Option<String>,
    #[serde(default)]
    pub snapshot: Option<String>,
}

fn body_hash(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

impl TrustState {
    pub fn from_pins(pins: &Pins) -> Self {
        TrustState {
            channel: pins.channel.clone(),
            keys: pins.keys.clone(),
            threshold: pins.threshold.max(1),
            key_sequence: 0,
            serial: pins.min_serial,
            manifest_sha256: None,
            snapshot: None,
        }
    }

    /// Called at every start with the image's pins. A newer image can only raise the floor.
    pub fn raise_floor(&mut self, pins: &Pins) {
        if pins.min_serial > self.serial {
            self.serial = pins.min_serial;
            self.manifest_sha256 = None;
        }
    }

    /// Decides whether a manifest is acceptable without changing anything.
    ///
    /// `now` is `None` while the clock is not synchronised. Expiry is then not enforced, because a
    /// device that booted with a wrong clock must still be able to update; the serial floor still
    /// prevents replays.
    pub fn check(
        &self,
        body: &[u8],
        signatures: &[u8],
        arch: &str,
        now: Option<u64>,
    ) -> Result<Accepted, TrustError> {
        let sigs = SignatureFile::parse(signatures)?;
        sigs.verify(MANIFEST_CONTEXT, body, &self.keys, self.threshold as usize)?;
        let manifest = SnapshotManifest::parse(body)?;
        if manifest.channel != self.channel {
            return Err(TrustError::WrongChannel { expected: self.channel.clone(), offered: manifest.channel });
        }
        if let Some(now) = now {
            if now > manifest.expires {
                return Err(TrustError::Expired { expires: manifest.expires });
            }
        }
        if manifest.repos_for(arch).is_empty() {
            return Err(TrustError::NoRepository(arch.to_owned()));
        }
        let hash = body_hash(body);
        if manifest.serial == self.serial {
            return match &self.manifest_sha256 {
                Some(accepted) if *accepted == hash => Ok(Accepted::Unchanged),
                Some(_) => Err(TrustError::Conflict { serial: manifest.serial }),
                // The floor from the image, with no manifest accepted at it yet.
                None => Err(TrustError::Downgrade { current: self.serial, offered: manifest.serial }),
            };
        }
        if manifest.serial < self.serial {
            return Err(TrustError::Downgrade { current: self.serial, offered: manifest.serial });
        }
        Ok(Accepted::New(Box::new(manifest)))
    }

    /// Like [`check`](Self::check), and records a newer manifest as accepted.
    pub fn accept(
        &mut self,
        body: &[u8],
        signatures: &[u8],
        arch: &str,
        now: Option<u64>,
    ) -> Result<Accepted, TrustError> {
        let result = self.check(body, signatures, arch, now)?;
        if let Accepted::New(manifest) = &result {
            self.serial = manifest.serial;
            self.manifest_sha256 = Some(body_hash(body));
            self.snapshot = Some(manifest.snapshot.clone());
        }
        Ok(result)
    }

    /// Rotates keys. A device that was offline through several rotations applies the statements
    /// in order, each one signed by the keys the previous one installed.
    pub fn apply_key_statement(&mut self, body: &[u8], signatures: &[u8]) -> Result<(), TrustError> {
        let sigs = SignatureFile::parse(signatures)?;
        sigs.verify(KEY_STATEMENT_CONTEXT, body, &self.keys, self.threshold as usize)?;
        let statement = KeyStatement::parse(body)?;
        if statement.sequence <= self.key_sequence {
            return Err(TrustError::StaleKeyStatement { current: self.key_sequence, offered: statement.sequence });
        }
        self.keys = statement.keys.into_iter().collect();
        self.threshold = statement.threshold;
        self.key_sequence = statement.sequence;
        Ok(())
    }

    /// What the user would approve to trust a key that signed a manifest this device rejected.
    pub fn approval_for(&self, offered: PublicKey) -> KeyApproval {
        KeyApproval { from: self.keys.clone(), to: offered }
    }

    /// Only call this from a real user decision. It replaces all keys with the approved one.
    pub fn apply_approval(&mut self, approval: &KeyApproval) -> Result<(), TrustError> {
        if approval.from != self.keys {
            return Err(TrustError::StaleApproval);
        }
        self.keys = BTreeSet::from([approval.to]);
        self.threshold = 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use super::*;
    use crate::manifest::{PackagePin, RepoEntry};

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn pins(keys: &[&SigningKey]) -> Pins {
        Pins {
            channel: "stable".into(),
            keys: keys.iter().map(|k| PublicKey::from(*k)).collect(),
            threshold: 1,
            min_serial: 3,
        }
    }

    fn manifest(serial: u64) -> SnapshotManifest {
        SnapshotManifest {
            format: 1,
            channel: "stable".into(),
            serial,
            snapshot: format!("2026.{serial}"),
            published: 1000,
            expires: 2000,
            needs_restart: false,
            repo: vec![RepoEntry {
                arch: "aarch64".into(),
                name: "void".into(),
                path: format!("snapshots/{serial}/aarch64"),
                repodata_sha256: "00".repeat(32),
            }],
            packages: vec![PackagePin { name: "monokulo-base".into(), version: "0.1.0_1".into() }],
        }
    }

    fn signed(m: &SnapshotManifest, keys: &[&SigningKey]) -> (Vec<u8>, Vec<u8>) {
        let body = m.to_toml().into_bytes();
        let mut sigs = SignatureFile::default();
        for k in keys {
            sigs.sign(MANIFEST_CONTEXT, &body, k);
        }
        (body, sigs.to_toml().into_bytes())
    }

    fn statement(seq: u64, new: &[&SigningKey], threshold: u32, signers: &[&SigningKey]) -> (Vec<u8>, Vec<u8>) {
        let st = KeyStatement {
            format: 1,
            sequence: seq,
            keys: new.iter().map(|k| PublicKey::from(*k)).collect(),
            threshold,
        };
        let body = st.to_toml().into_bytes();
        let mut sigs = SignatureFile::default();
        for k in signers {
            sigs.sign(KEY_STATEMENT_CONTEXT, &body, k);
        }
        (body, sigs.to_toml().into_bytes())
    }

    #[test]
    fn accepts_a_newer_signed_manifest() {
        let mut state = TrustState::from_pins(&pins(&[&key(1)]));
        let (body, sig) = signed(&manifest(4), &[&key(1)]);
        assert!(matches!(state.accept(&body, &sig, "aarch64", Some(1500)).unwrap(), Accepted::New(_)));
        assert_eq!(state.serial, 4);
        assert_eq!(state.accept(&body, &sig, "aarch64", Some(1500)).unwrap(), Accepted::Unchanged);
    }

    #[test]
    fn refuses_downgrades_and_replays() {
        let mut state = TrustState::from_pins(&pins(&[&key(1)]));
        let (b5, s5) = signed(&manifest(5), &[&key(1)]);
        state.accept(&b5, &s5, "aarch64", None).unwrap();
        let (b4, s4) = signed(&manifest(4), &[&key(1)]);
        assert_eq!(
            state.accept(&b4, &s4, "aarch64", None).unwrap_err(),
            TrustError::Downgrade { current: 5, offered: 4 }
        );
        assert_eq!(state.serial, 5, "a refused manifest changes nothing");
    }

    #[test]
    fn the_image_floor_blocks_older_manifests_even_with_no_state() {
        let state = TrustState::from_pins(&pins(&[&key(1)]));
        for serial in [1, 3] {
            let (b, s) = signed(&manifest(serial), &[&key(1)]);
            assert!(matches!(state.check(&b, &s, "aarch64", None), Err(TrustError::Downgrade { .. })), "{serial}");
        }
    }

    #[test]
    fn a_new_image_raises_but_never_lowers_the_floor() {
        let mut state = TrustState::from_pins(&pins(&[&key(1)]));
        let mut p = pins(&[&key(1)]);
        p.min_serial = 10;
        state.raise_floor(&p);
        assert_eq!(state.serial, 10);
        p.min_serial = 2;
        state.raise_floor(&p);
        assert_eq!(state.serial, 10);
    }

    #[test]
    fn a_second_manifest_with_the_same_serial_is_a_conflict() {
        let mut state = TrustState::from_pins(&pins(&[&key(1)]));
        let (b, s) = signed(&manifest(4), &[&key(1)]);
        state.accept(&b, &s, "aarch64", None).unwrap();
        let mut other = manifest(4);
        other.snapshot = "evil".into();
        let (b2, s2) = signed(&other, &[&key(1)]);
        assert_eq!(state.check(&b2, &s2, "aarch64", None).unwrap_err(), TrustError::Conflict { serial: 4 });
    }

    #[test]
    fn expiry_is_enforced_only_with_a_trusted_clock() {
        let state = TrustState::from_pins(&pins(&[&key(1)]));
        let (b, s) = signed(&manifest(4), &[&key(1)]);
        assert_eq!(state.check(&b, &s, "aarch64", Some(2001)).unwrap_err(), TrustError::Expired { expires: 2000 });
        assert!(state.check(&b, &s, "aarch64", Some(2000)).is_ok());
        assert!(state.check(&b, &s, "aarch64", None).is_ok());
    }

    #[test]
    fn refuses_other_channels_architectures_and_keys() {
        let state = TrustState::from_pins(&pins(&[&key(1)]));
        let mut m = manifest(4);
        m.channel = "testing".into();
        let (b, s) = signed(&m, &[&key(1)]);
        assert!(matches!(state.check(&b, &s, "aarch64", None), Err(TrustError::WrongChannel { .. })));
        let (b, s) = signed(&manifest(4), &[&key(1)]);
        assert!(matches!(state.check(&b, &s, "x86_64", None), Err(TrustError::NoRepository(_))));
        let (b, s) = signed(&manifest(4), &[&key(2)]);
        assert!(matches!(
            state.check(&b, &s, "aarch64", None),
            Err(TrustError::Manifest(ManifestError::UnknownKey { .. }))
        ));
    }

    #[test]
    fn key_rotation_follows_a_chain_of_statements() {
        let mut state = TrustState::from_pins(&pins(&[&key(1)]));
        let (b, s) = statement(1, &[&key(2)], 1, &[&key(1)]);
        state.apply_key_statement(&b, &s).unwrap();
        let (b, s) = statement(2, &[&key(3), &key(4)], 2, &[&key(2)]);
        state.apply_key_statement(&b, &s).unwrap();
        assert_eq!(state.threshold, 2);

        let (b, s) = signed(&manifest(4), &[&key(1)]);
        assert!(state.check(&b, &s, "aarch64", None).is_err(), "the retired key no longer signs");
        let (b, s) = signed(&manifest(4), &[&key(3)]);
        assert!(state.check(&b, &s, "aarch64", None).is_err(), "one of two keys is not enough");
        let (b, s) = signed(&manifest(4), &[&key(3), &key(4)]);
        assert!(state.check(&b, &s, "aarch64", None).is_ok());
    }

    #[test]
    fn key_statements_need_the_current_keys_and_a_newer_sequence() {
        let mut state = TrustState::from_pins(&pins(&[&key(1)]));
        let (b, s) = statement(1, &[&key(9)], 1, &[&key(9)]);
        assert!(state.apply_key_statement(&b, &s).is_err(), "a key cannot vouch for itself");
        let (b, s) = statement(2, &[&key(2)], 1, &[&key(1)]);
        state.apply_key_statement(&b, &s).unwrap();
        // An attacker holding the retired key replays an older rotation back to it.
        let (b, s) = statement(1, &[&key(1)], 1, &[&key(2)]);
        assert_eq!(
            state.apply_key_statement(&b, &s).unwrap_err(),
            TrustError::StaleKeyStatement { current: 2, offered: 1 }
        );
        assert_eq!(state.keys, BTreeSet::from([PublicKey::from(&key(2))]));
    }

    #[test]
    fn user_approval_replaces_keys_only_while_it_matches() {
        let mut state = TrustState::from_pins(&pins(&[&key(1)]));
        let approval = state.approval_for(PublicKey::from(&key(5)));
        let (b, s) = statement(1, &[&key(2)], 1, &[&key(1)]);
        let mut rotated = state.clone();
        rotated.apply_key_statement(&b, &s).unwrap();
        assert_eq!(rotated.apply_approval(&approval).unwrap_err(), TrustError::StaleApproval);
        state.apply_approval(&approval).unwrap();
        let (b, s) = signed(&manifest(4), &[&key(5)]);
        assert!(state.check(&b, &s, "aarch64", None).is_ok());
    }

    #[test]
    fn state_round_trips_through_toml() {
        let mut state = TrustState::from_pins(&pins(&[&key(1)]));
        let (b, s) = signed(&manifest(4), &[&key(1)]);
        state.accept(&b, &s, "aarch64", None).unwrap();
        let text = toml::to_string(&state).unwrap();
        assert_eq!(toml::from_str::<TrustState>(&text).unwrap(), state);
    }
}
