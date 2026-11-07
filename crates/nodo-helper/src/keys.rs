//! Key handling for encrypted storage.
//!
//! ```text
//! passphrase --Argon2id--> wrapping key --XChaCha20-Poly1305--> device key (stored wrapped)
//! device key --HKDF-SHA-256, by package id--> volume key --> LUKS2 keyslot
//! ```
//!
//! One passphrase therefore opens every volume, volumes never share a key, and changing the
//! passphrase only rewrites the wrapped device key. Volume keys are derived rather than stored,
//! so the wrapped device key is the only secret on disk.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::Sha256;
use thiserror::Error;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"NODOKEY1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
pub const DEVICE_KEY_LEN: usize = 32;
pub const VOLUME_KEY_LEN: usize = 64;
const HEADER_LEN: usize = MAGIC.len() + 12 + SALT_LEN + NONCE_LEN;
const FILE_LEN: usize = HEADER_LEN + DEVICE_KEY_LEN + TAG_LEN;

/// Upper limits for values read from disk, so a damaged file cannot demand absurd resources.
const MAX_MEMORY_KIB: u32 = 4 * 1024 * 1024;
const MAX_ITERATIONS: u32 = 64;
const MAX_LANES: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub lanes: u32,
}

impl Default for KdfParams {
    /// Costly enough to slow offline guessing, and well within the target board's memory.
    fn default() -> Self {
        KdfParams {
            memory_kib: 256 * 1024,
            iterations: 3,
            lanes: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KeyError {
    /// Wrong passphrase, or a key file altered since it was written. Authenticated encryption
    /// cannot tell these apart.
    #[error("the passphrase did not open the device key")]
    WrongPassphrase,
    #[error("the device key file is damaged")]
    Corrupt,
    #[error("key derivation failed")]
    Kdf,
}

pub type DeviceKey = Zeroizing<[u8; DEVICE_KEY_LEN]>;
pub type VolumeKey = Zeroizing<[u8; VOLUME_KEY_LEN]>;

fn wrapping_key(
    passphrase: &[u8],
    salt: &[u8],
    params: KdfParams,
) -> Result<Zeroizing<[u8; 32]>, KeyError> {
    let params = Params::new(params.memory_kib, params.iterations, params.lanes, Some(32))
        .map_err(|_| KeyError::Kdf)?;
    let mut out = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase, salt, out.as_mut())
        .map_err(|_| KeyError::Kdf)?;
    Ok(out)
}

/// Encrypts the device key. `salt_and_nonce` must be fresh random bytes.
pub fn wrap_device_key(
    passphrase: &[u8],
    device_key: &[u8; DEVICE_KEY_LEN],
    salt_and_nonce: &[u8; SALT_LEN + NONCE_LEN],
    params: KdfParams,
) -> Result<Vec<u8>, KeyError> {
    let (salt, nonce) = salt_and_nonce.split_at(SALT_LEN);
    let mut file = Vec::with_capacity(FILE_LEN);
    file.extend_from_slice(MAGIC);
    file.extend_from_slice(&params.memory_kib.to_be_bytes());
    file.extend_from_slice(&params.iterations.to_be_bytes());
    file.extend_from_slice(&params.lanes.to_be_bytes());
    file.extend_from_slice(salt);
    file.extend_from_slice(nonce);

    let kek = wrapping_key(passphrase, salt, params)?;
    let cipher = XChaCha20Poly1305::new_from_slice(kek.as_ref()).map_err(|_| KeyError::Kdf)?;
    // The header is authenticated, so lowered KDF parameters are detected rather than obeyed.
    let sealed = cipher
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: device_key,
                aad: &file,
            },
        )
        .map_err(|_| KeyError::Kdf)?;
    file.extend_from_slice(&sealed);
    Ok(file)
}

pub fn unwrap_device_key(passphrase: &[u8], file: &[u8]) -> Result<DeviceKey, KeyError> {
    if file.len() != FILE_LEN || &file[..MAGIC.len()] != MAGIC {
        return Err(KeyError::Corrupt);
    }
    let word = |at: usize| {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&file[at..at + 4]);
        u32::from_be_bytes(bytes)
    };
    let params = KdfParams {
        memory_kib: word(MAGIC.len()),
        iterations: word(MAGIC.len() + 4),
        lanes: word(MAGIC.len() + 8),
    };
    if params.memory_kib > MAX_MEMORY_KIB
        || params.iterations > MAX_ITERATIONS
        || params.lanes > MAX_LANES
    {
        return Err(KeyError::Corrupt);
    }
    let salt = &file[MAGIC.len() + 12..MAGIC.len() + 12 + SALT_LEN];
    let nonce = &file[MAGIC.len() + 12 + SALT_LEN..HEADER_LEN];
    let kek = wrapping_key(passphrase, salt, params).map_err(|_| KeyError::Corrupt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(kek.as_ref()).map_err(|_| KeyError::Corrupt)?;
    let plain = Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: &file[HEADER_LEN..],
                    aad: &file[..HEADER_LEN],
                },
            )
            .map_err(|_| KeyError::WrongPassphrase)?,
    );
    let mut key = Zeroizing::new([0u8; DEVICE_KEY_LEN]);
    key.copy_from_slice(&plain);
    Ok(key)
}

/// The key for one package's volume. Binding the id means a volume file moved to another
/// package's name will not open.
pub fn volume_key(device_key: &[u8; DEVICE_KEY_LEN], package_id: &str) -> VolumeKey {
    let mut info = b"nodo-volume-v1\0".to_vec();
    info.extend_from_slice(package_id.as_bytes());
    let mut key = Zeroizing::new([0u8; VOLUME_KEY_LEN]);
    Hkdf::<Sha256>::new(None, device_key)
        .expand(&info, key.as_mut())
        .expect("64 bytes is within HKDF's output limit");
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: KdfParams = KdfParams {
        memory_kib: 64,
        iterations: 1,
        lanes: 1,
    };
    const DEVICE: [u8; 32] = [7; 32];
    const RANDOM: [u8; SALT_LEN + NONCE_LEN] = [9; SALT_LEN + NONCE_LEN];

    fn wrapped() -> Vec<u8> {
        wrap_device_key(b"correct horse", &DEVICE, &RANDOM, FAST).unwrap()
    }

    #[test]
    fn round_trip() {
        let key = unwrap_device_key(b"correct horse", &wrapped()).unwrap();
        assert_eq!(*key, DEVICE);
    }

    #[test]
    fn wrong_passphrase_fails() {
        assert_eq!(
            unwrap_device_key(b"wrong", &wrapped()).unwrap_err(),
            KeyError::WrongPassphrase
        );
    }

    #[test]
    fn file_does_not_contain_the_key_or_passphrase() {
        let file = wrapped();
        assert!(!file.windows(32).any(|w| w == DEVICE));
        assert!(!file.windows(13).any(|w| w == b"correct horse"));
    }

    #[test]
    fn every_byte_is_authenticated() {
        let good = wrapped();
        for at in 0..good.len() {
            let mut bad = good.clone();
            bad[at] ^= 1;
            assert!(
                unwrap_device_key(b"correct horse", &bad).is_err(),
                "byte {at}"
            );
        }
    }

    #[test]
    fn damaged_files_are_rejected_without_running_the_kdf() {
        assert_eq!(
            unwrap_device_key(b"x", b"short").unwrap_err(),
            KeyError::Corrupt
        );
        let mut huge = wrapped();
        huge[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            unwrap_device_key(b"correct horse", &huge).unwrap_err(),
            KeyError::Corrupt
        );
    }

    #[test]
    fn volume_keys_differ_per_package_and_are_stable() {
        let a = volume_key(&DEVICE, "a");
        assert_eq!(*a, *volume_key(&DEVICE, "a"));
        assert_ne!(*a, *volume_key(&DEVICE, "b"));
        assert_ne!(*a, *volume_key(&[8; 32], "a"));
    }
}
