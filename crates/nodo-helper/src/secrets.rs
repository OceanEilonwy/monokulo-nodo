//! Secrets sealed under the device key.
//!
//! ```text
//! device key --HKDF-SHA-256 "nodo-secrets-v1"--> secrets key (memory only, after an unlock)
//! secret --XChaCha20-Poly1305, random nonce, AD = package and setting--> "v1." base64(nonce || ciphertext)
//! ```
//!
//! A sealed value is kept in the package's settings store in place of the clear one. The
//! secrets key exists only in the helper's locked memory, between an unlock and the helper's
//! exit, so after a reboot no secret is readable until the device passphrase is entered again.
//! Rendered settings that contain secrets go to memory under `/run/nodo/secrets` for the same
//! reason, and services that need them wait there, as they do for encrypted storage.
//!
//! The associated data binds a sealed value to its package and setting, so copying one into
//! another package's store or another setting does not open.

use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::apply::Write;
use crate::helper::{lock, Helper};
use crate::keys::DEVICE_KEY_LEN;
use crate::system::System;

const PREFIX: &str = "v1.";
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const AD_CONTEXT: &[u8] = b"nodo-secret-v1\0";
/// Far above any setting, which is limited to a few kilobytes, so a damaged store cannot make
/// the helper decode something huge.
const MAX_SEALED_LEN: usize = 64 * 1024;

/// The key secrets are sealed under. Derived rather than the device key itself, so what stays in
/// memory after an unlock cannot open volumes.
pub struct SecretsKey(Zeroizing<[u8; 32]>);

impl SecretsKey {
    pub fn derive(device_key: &[u8; DEVICE_KEY_LEN]) -> SecretsKey {
        let mut key = Zeroizing::new([0u8; 32]);
        Hkdf::<Sha256>::new(None, device_key)
            .expand(b"nodo-secrets-v1", key.as_mut())
            .expect("32 bytes is within HKDF's output limit");
        SecretsKey(key)
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new_from_slice(self.0.as_ref()).expect("the key is 32 bytes")
    }
}

impl std::fmt::Debug for SecretsKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretsKey(..)")
    }
}

fn associated_data(package: &str, setting: &str) -> Vec<u8> {
    let mut ad = AD_CONTEXT.to_vec();
    ad.extend_from_slice(package.as_bytes());
    ad.push(0);
    ad.extend_from_slice(setting.as_bytes());
    ad
}

/// Seals one value. `nonce` must be fresh random bytes.
pub fn seal(
    key: &SecretsKey,
    package: &str,
    setting: &str,
    value: &str,
    nonce: &[u8; NONCE_LEN],
) -> Result<String, String> {
    let ad = associated_data(package, setting);
    let sealed = key
        .cipher()
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: value.as_bytes(),
                aad: &ad,
            },
        )
        .map_err(|_| "sealing failed".to_owned())?;
    let mut bytes = Vec::with_capacity(NONCE_LEN + sealed.len());
    bytes.extend_from_slice(nonce);
    bytes.extend_from_slice(&sealed);
    Ok(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

/// Opens a sealed value. A wrong key, a value moved from another package or setting, and any
/// altered byte all fail the same way.
pub fn open(
    key: &SecretsKey,
    package: &str,
    setting: &str,
    sealed: &str,
) -> Result<Zeroizing<String>, String> {
    let damaged = || format!("the sealed value of {package}.{setting} does not open");
    let encoded = sealed.strip_prefix(PREFIX).ok_or_else(damaged)?;
    if encoded.len() > MAX_SEALED_LEN {
        return Err(damaged());
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| damaged())?;
    if bytes.len() < NONCE_LEN + TAG_LEN {
        return Err(damaged());
    }
    let (nonce, ciphertext) = bytes.split_at(NONCE_LEN);
    let ad = associated_data(package, setting);
    let plain = Zeroizing::new(
        key.cipher()
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: &ad,
                },
            )
            .map_err(|_| damaged())?,
    );
    let text = std::str::from_utf8(&plain).map_err(|_| damaged())?;
    Ok(Zeroizing::new(text.to_owned()))
}

impl<S: System> Helper<S> {
    pub(crate) fn secrets_key(&self) -> Option<Arc<SecretsKey>> {
        lock(&self.secrets).clone()
    }

    pub(crate) fn remember_device_key(&self, device_key: &[u8; DEVICE_KEY_LEN]) {
        *lock(&self.secrets) = Some(Arc::new(SecretsKey::derive(device_key)));
    }

    /// After an unlock: seals secrets still stored in clear, then renders the settings that
    /// live in memory, which services are waiting for. Nothing is restarted: a waiting service
    /// starts by itself, and one already running was rendered the same way before.
    ///
    /// Must be called without `failed_unlocks` held, because installs take the service lock
    /// before it.
    pub(crate) fn render_unlocked(&self) -> Result<(), String> {
        let _guard = lock(&self.service_lock);
        let (packages, _) = self.packages().map_err(|e| e.to_string())?;
        let mut problems = Vec::new();
        match self.with_settings(&packages, |ctx| self.settings.seal_stored(ctx)) {
            Ok(0) => {}
            Ok(count) => eprintln!("nodo-helper: sealed {count} secrets stored in clear"),
            Err(e) => problems.push(format!("sealing stored secrets: {e}")),
        }
        for package in &packages {
            if package.manifest.settings.is_none() || matches!(self.is_locked(package), Ok(true)) {
                continue;
            }
            let plan =
                match self.with_settings(&packages, |ctx| self.settings.plan(ctx, package, &[])) {
                    Ok(plan) => plan,
                    Err(e) => {
                        problems.push(format!("{}: {e}", package.id()));
                        continue;
                    }
                };
            if !plan.service.sealed && !package.manifest.storage.encrypted {
                continue;
            }
            for file in plan.files {
                let write = Write::from(file);
                let current = self
                    .sys
                    .read_file(&write.path)
                    .ok()
                    .flatten()
                    .map(Zeroizing::new);
                if current.as_deref().map(Vec::as_slice) == Some(write.bytes.as_slice()) {
                    continue;
                }
                if let Err(e) = write.apply(&self.sys) {
                    problems.push(format!("{}: {e}", package.id()));
                    break;
                }
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: [u8; NONCE_LEN] = [5; NONCE_LEN];

    fn key(byte: u8) -> SecretsKey {
        SecretsKey::derive(&[byte; DEVICE_KEY_LEN])
    }

    #[test]
    fn round_trip() {
        let sealed = seal(&key(1), "shop", "token", "s3cret", &NONCE).unwrap();
        assert!(sealed.starts_with(PREFIX));
        assert_eq!(
            open(&key(1), "shop", "token", &sealed).unwrap().as_str(),
            "s3cret"
        );
    }

    #[test]
    fn sealed_text_holds_neither_the_secret_nor_the_key() {
        let sealed = seal(&key(1), "shop", "token", "s3cret-value", &NONCE).unwrap();
        assert!(!sealed.contains("s3cret"));
        let decoded = URL_SAFE_NO_PAD.decode(&sealed[PREFIX.len()..]).unwrap();
        assert!(!decoded.windows(6).any(|w| w == b"s3cret"));
    }

    #[test]
    fn another_device_key_does_not_open() {
        let sealed = seal(&key(1), "shop", "token", "s3cret", &NONCE).unwrap();
        assert!(open(&key(2), "shop", "token", &sealed).is_err());
    }

    #[test]
    fn a_value_moved_to_another_package_or_setting_does_not_open() {
        let sealed = seal(&key(1), "shop", "token", "s3cret", &NONCE).unwrap();
        assert!(open(&key(1), "evil", "token", &sealed).is_err());
        assert!(open(&key(1), "shop", "label", &sealed).is_err());
        // The separator keeps `a` + `b.c` apart from `a.b` + `c`.
        let split = seal(&key(1), "a", "bc", "x", &NONCE).unwrap();
        assert!(open(&key(1), "ab", "c", &split).is_err());
    }

    #[test]
    fn every_byte_is_authenticated() {
        let sealed = seal(&key(1), "shop", "token", "s3cret", &NONCE).unwrap();
        let bytes = URL_SAFE_NO_PAD.decode(&sealed[PREFIX.len()..]).unwrap();
        for at in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[at] ^= 1;
            let text = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(&bad));
            assert!(open(&key(1), "shop", "token", &text).is_err(), "byte {at}");
        }
    }

    #[test]
    fn malformed_values_are_refused() {
        for text in ["", "v1.", "v1.!!!", "plain", "v2.AAAA", "v1.AAAA"] {
            assert!(open(&key(1), "shop", "token", text).is_err(), "{text}");
        }
        let huge = format!("{PREFIX}{}", "A".repeat(MAX_SEALED_LEN + 1));
        assert!(open(&key(1), "shop", "token", &huge).is_err());
    }

    #[test]
    fn the_secrets_key_differs_from_the_device_and_volume_keys() {
        let device = [3u8; DEVICE_KEY_LEN];
        let secrets = SecretsKey::derive(&device);
        assert_ne!(*secrets.0, device);
        assert_ne!(
            secrets.0.as_slice(),
            &crate::keys::volume_key(&device, "nodo-secrets-v1")[..32]
        );
        assert_eq!(format!("{secrets:?}"), "SecretsKey(..)");
    }
}
