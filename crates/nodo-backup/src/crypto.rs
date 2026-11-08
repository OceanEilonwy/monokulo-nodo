//! The encrypted container: a plaintext header, then the payload sealed with the STREAM
//! construction over XChaCha20-Poly1305, keyed by Argon2id from the passphrase.
//!
//! ```text
//! "NODOBAK1" | header length (u32 BE) | header (TOML) | chunk_0 | ... | chunk_last
//! ```
//!
//! Every chunk authenticates the whole header as associated data, so lowering the KDF cost or
//! swapping the salt is detected. STREAM marks the final chunk, so a truncated file is refused
//! rather than restored partially. All primitives come from the RustCrypto crates; nothing here
//! invents its own.

use std::io::{self, Read, Write};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::aead::{KeyInit, Payload};
use chacha20poly1305::XChaCha20Poly1305;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::BackupError;

pub const MAGIC: &[u8; 8] = b"NODOBAK1";
pub const CHUNK: usize = 64 * 1024;
const TAG: usize = 16;
const SALT_LEN: usize = 16;
/// XChaCha20's 24-byte nonce minus STREAM's 5-byte counter and flag.
const STREAM_NONCE_LEN: usize = 19;
const MAX_HEADER: usize = 4096;
pub const MIN_PASSPHRASE_LEN: usize = 12;
/// Limits for values read from a file, so a damaged or hostile header cannot demand absurd work.
const MAX_MEMORY_KIB: u32 = 4 * 1024 * 1024;
const MAX_ITERATIONS: u32 = 64;
const MAX_LANES: u32 = 64;
/// Lower bounds too, so a forged header cannot make the next guess cheap. Authentication would
/// catch the change anyway, but only after paying for the derivation.
const MIN_MEMORY_KIB: u32 = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub lanes: u32,
}

impl Default for KdfParams {
    /// The device key file uses the same cost, so a backup is no easier to guess than the device.
    fn default() -> Self {
        KdfParams { memory_kib: 256 * 1024, iterations: 3, lanes: 1 }
    }
}

impl KdfParams {
    fn check(&self) -> Result<(), BackupError> {
        let ok = (MIN_MEMORY_KIB..=MAX_MEMORY_KIB).contains(&self.memory_kib)
            && (1..=MAX_ITERATIONS).contains(&self.iterations)
            && (1..=MAX_LANES).contains(&self.lanes);
        if ok {
            Ok(())
        } else {
            Err(BackupError::Corrupt("key derivation parameters are out of range".into()))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub format: u32,
    pub created: u64,
    pub kdf: KdfParams,
    pub salt: String,
    pub nonce: String,
}

fn derive(passphrase: &[u8], salt: &[u8], params: KdfParams) -> Result<Zeroizing<[u8; 32]>, BackupError> {
    params.check()?;
    let p = Params::new(params.memory_kib, params.iterations, params.lanes, Some(32))
        .map_err(|_| BackupError::Corrupt("bad key derivation parameters".into()))?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, p)
        .hash_password_into(passphrase, salt, key.as_mut())
        .map_err(|_| BackupError::Corrupt("key derivation failed".into()))?;
    Ok(key)
}

fn seal_failed(_: chacha20poly1305::aead::Error) -> BackupError {
    BackupError::Io("encryption failed".into())
}

/// Encrypts everything written to it. [`finish`](Self::finish) must be called, or the file has
/// no final chunk and every restore refuses it.
pub struct SealWriter<W: Write> {
    out: W,
    aad: Vec<u8>,
    enc: Option<EncryptorBE32<XChaCha20Poly1305>>,
    buf: Zeroizing<Vec<u8>>,
}

impl<W: Write> SealWriter<W> {
    pub fn new(mut out: W, passphrase: &[u8], params: KdfParams, created: u64) -> Result<Self, BackupError> {
        if passphrase.chars_len() < MIN_PASSPHRASE_LEN {
            return Err(BackupError::WeakPassphrase);
        }
        let mut salt = [0u8; SALT_LEN];
        let mut nonce = [0u8; STREAM_NONCE_LEN];
        getrandom::fill(&mut salt).map_err(|e| BackupError::Io(e.to_string()))?;
        getrandom::fill(&mut nonce).map_err(|e| BackupError::Io(e.to_string()))?;
        let header = Header { format: 1, created, kdf: params, salt: hex::encode(salt), nonce: hex::encode(nonce) };
        let text = toml::to_string(&header).map_err(|e| BackupError::Io(e.to_string()))?;
        let mut aad = Vec::with_capacity(12 + text.len());
        aad.extend_from_slice(MAGIC);
        aad.extend_from_slice(&(text.len() as u32).to_be_bytes());
        aad.extend_from_slice(text.as_bytes());
        out.write_all(&aad)?;
        let key = derive(passphrase, &salt, params)?;
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| BackupError::Io("key".into()))?;
        let enc = EncryptorBE32::from_aead(cipher, nonce.as_ref().into());
        Ok(SealWriter { out, aad, enc: Some(enc), buf: Zeroizing::new(Vec::with_capacity(CHUNK)) })
    }

    fn flush_chunk(&mut self) -> Result<(), BackupError> {
        let enc = self.enc.as_mut().ok_or_else(|| BackupError::Io("already finished".into()))?;
        let sealed = enc.encrypt_next(Payload { msg: &self.buf, aad: &self.aad }).map_err(seal_failed)?;
        self.out.write_all(&sealed)?;
        self.buf.clear();
        Ok(())
    }

    pub fn write_all(&mut self, mut data: &[u8]) -> Result<(), BackupError> {
        while !data.is_empty() {
            // A full buffer is only sealed once more data arrives, so the last chunk is never
            // sealed as an ordinary one.
            if self.buf.len() == CHUNK {
                self.flush_chunk()?;
            }
            let take = (CHUNK - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<W, BackupError> {
        let enc = self.enc.take().ok_or_else(|| BackupError::Io("already finished".into()))?;
        let sealed = enc.encrypt_last(Payload { msg: &self.buf, aad: &self.aad }).map_err(seal_failed)?;
        self.out.write_all(&sealed)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

trait CharsLen {
    fn chars_len(&self) -> usize;
}

impl CharsLen for [u8] {
    fn chars_len(&self) -> usize {
        std::str::from_utf8(self).map(|s| s.chars().count()).unwrap_or(self.len())
    }
}

/// Decrypts and authenticates chunk by chunk. A read error means the file was tampered with,
/// truncated or the passphrase is wrong; the caller must discard anything already read.
pub struct OpenReader<R: Read> {
    input: R,
    aad: Vec<u8>,
    dec: Option<DecryptorBE32<XChaCha20Poly1305>>,
    plain: Zeroizing<Vec<u8>>,
    pos: usize,
    /// One byte read ahead, to know whether the current chunk is the last.
    peeked: Option<u8>,
    done: bool,
    pub header: Header,
}

fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

impl<R: Read> OpenReader<R> {
    pub fn new(mut input: R, passphrase: &[u8]) -> Result<Self, BackupError> {
        let mut fixed = [0u8; 12];
        if read_up_to(&mut input, &mut fixed)? != 12 || &fixed[..8] != MAGIC {
            return Err(BackupError::NotABackup);
        }
        let len = u32::from_be_bytes([fixed[8], fixed[9], fixed[10], fixed[11]]) as usize;
        if len == 0 || len > MAX_HEADER {
            return Err(BackupError::Corrupt("header length".into()));
        }
        let mut text = vec![0u8; len];
        if read_up_to(&mut input, &mut text)? != len {
            return Err(BackupError::Corrupt("truncated header".into()));
        }
        let header: Header = std::str::from_utf8(&text)
            .ok()
            .and_then(|t| toml::from_str(t).ok())
            .ok_or_else(|| BackupError::Corrupt("unreadable header".into()))?;
        if header.format != 1 {
            return Err(BackupError::Corrupt(format!("unsupported format {}", header.format)));
        }
        let salt = hex::decode(&header.salt).ok().filter(|s| s.len() == SALT_LEN);
        let nonce = hex::decode(&header.nonce).ok().filter(|n| n.len() == STREAM_NONCE_LEN);
        let (Some(salt), Some(nonce)) = (salt, nonce) else {
            return Err(BackupError::Corrupt("bad salt or nonce".into()));
        };
        let key = derive(passphrase, &salt, header.kdf)?;
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| BackupError::Io("key".into()))?;
        let dec = DecryptorBE32::from_aead(cipher, nonce.as_slice().into());
        let mut aad = fixed.to_vec();
        aad.extend_from_slice(&text);
        let mut reader =
            OpenReader { input, aad, dec: Some(dec), plain: Zeroizing::new(Vec::new()), pos: 0, peeked: None, done: false, header };
        // Opening the first chunk now separates a wrong passphrase from later tampering.
        reader.next_chunk().map_err(|e| match e {
            BackupError::Tampered => BackupError::WrongPassphrase,
            other => other,
        })?;
        Ok(reader)
    }

    fn next_chunk(&mut self) -> Result<(), BackupError> {
        let mut sealed = vec![0u8; CHUNK + TAG];
        let mut filled = 0;
        if let Some(b) = self.peeked.take() {
            sealed[0] = b;
            filled = 1;
        }
        filled += read_up_to(&mut self.input, &mut sealed[filled..])?;
        sealed.truncate(filled);
        let mut probe = [0u8; 1];
        let last = read_up_to(&mut self.input, &mut probe)? == 0;
        if !last {
            self.peeked = Some(probe[0]);
            if filled != CHUNK + TAG {
                return Err(BackupError::Tampered);
            }
        }
        if filled < TAG {
            return Err(BackupError::Tampered);
        }
        let payload = Payload { msg: &sealed, aad: &self.aad };
        let plain = if last {
            let dec = self.dec.take().ok_or(BackupError::Tampered)?;
            self.done = true;
            dec.decrypt_last(payload)
        } else {
            self.dec.as_mut().ok_or(BackupError::Tampered)?.decrypt_next(payload)
        }
        .map_err(|_| BackupError::Tampered)?;
        self.plain = Zeroizing::new(plain);
        self.pos = 0;
        Ok(())
    }

    /// Reads exactly `buf.len()` bytes of plaintext.
    pub fn read_exact(&mut self, mut buf: &mut [u8]) -> Result<(), BackupError> {
        while !buf.is_empty() {
            if self.pos == self.plain.len() {
                if self.done {
                    return Err(BackupError::Corrupt("unexpected end of backup".into()));
                }
                self.next_chunk()?;
                continue;
            }
            let take = (self.plain.len() - self.pos).min(buf.len());
            buf[..take].copy_from_slice(&self.plain[self.pos..self.pos + take]);
            self.pos += take;
            buf = &mut buf[take..];
        }
        Ok(())
    }

    /// True once the final chunk has been authenticated and fully read.
    pub fn at_end(&mut self) -> Result<bool, BackupError> {
        while self.pos == self.plain.len() && !self.done {
            self.next_chunk()?;
        }
        Ok(self.done && self.pos == self.plain.len())
    }
}
