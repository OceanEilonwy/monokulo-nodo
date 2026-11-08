//! SSH keys that may log in as root. Lines are checked strictly, and options such as
//! `command=` are refused, so a key added here can only ever be a plain login key.

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine;
use nodo_api::{ApiError, Response, SshKey};
use sha2::{Digest, Sha256};

use crate::helper::{lock, Helper};
use crate::system::{System, WriteOptions};

const ALGORITHMS: &[&str] = &[
    "ssh-ed25519",
    "sk-ssh-ed25519@openssh.com",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ecdsa-sha2-nistp256@openssh.com",
    "ssh-rsa",
];
const MAX_LINE: usize = 16 * 1024;
const MAX_COMMENT: usize = 100;
const MAX_KEYS: usize = 64;
const KEY_FILE: WriteOptions = WriteOptions {
    mode: 0o600,
    group: None,
};

/// A parsed key line, and the line as it appears in the file.
struct Parsed {
    key: SshKey,
    line: String,
}

/// The key blob names its own algorithm, so a mismatch means the line was altered or pasted
/// wrongly.
fn blob_algorithm(blob: &[u8]) -> Option<&str> {
    let len = u32::from_be_bytes(blob.get(..4)?.try_into().ok()?) as usize;
    std::str::from_utf8(blob.get(4..4 + len)?).ok()
}

fn fingerprint(blob: &[u8]) -> String {
    format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(blob)))
}

/// Parses a line typed or pasted by the user. Only `algorithm base64 [comment]` is accepted.
fn parse_new(line: &str) -> Result<Parsed, String> {
    let line = line.trim();
    if line.len() > MAX_LINE || line.chars().any(|c| c.is_control()) {
        return Err("that is not a single public key line".to_owned());
    }
    let mut fields = line.splitn(3, ' ');
    let algorithm = fields.next().unwrap_or("");
    if !ALGORITHMS.contains(&algorithm) {
        return Err(
            "the line must start with a key type such as ssh-ed25519; options are not allowed"
                .to_owned(),
        );
    }
    let blob = STANDARD
        .decode(fields.next().unwrap_or(""))
        .map_err(|_| "the key data is not valid".to_owned())?;
    if blob_algorithm(&blob) != Some(algorithm) {
        return Err("the key data does not match its type".to_owned());
    }
    let comment: String = fields
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(MAX_COMMENT)
        .collect();
    let comment = comment.trim().to_owned();
    let mut clean = format!("{algorithm} {}", STANDARD.encode(&blob));
    if !comment.is_empty() {
        clean.push(' ');
        clean.push_str(&comment);
    }
    Ok(Parsed {
        key: SshKey {
            algorithm: algorithm.to_owned(),
            fingerprint: fingerprint(&blob),
            comment,
        },
        line: clean,
    })
}

/// Reads an existing line, which may carry options written by someone else. Unreadable lines
/// are kept in the file but not listed.
fn parse_existing(line: &str) -> Option<SshKey> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let at = tokens.iter().position(|t| ALGORITHMS.contains(t))?;
    let blob = STANDARD.decode(tokens.get(at + 1)?).ok()?;
    Some(SshKey {
        algorithm: tokens[at].to_owned(),
        fingerprint: fingerprint(&blob),
        comment: tokens[at + 2..]
            .join(" ")
            .chars()
            .take(MAX_COMMENT)
            .collect(),
    })
}

impl<S: System> Helper<S> {
    fn key_lines(&self) -> Result<Vec<String>, ApiError> {
        Ok(self
            .sys
            .read_file(&self.paths.authorized_keys)?
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default())
    }

    fn write_keys(&self, lines: &[String]) -> Result<(), ApiError> {
        if let Some(dir) = self.paths.authorized_keys.parent() {
            self.sys.ensure_dir(dir, 0o700, None)?;
        }
        let mut text = lines.join("\n");
        text.push('\n');
        self.sys
            .atomic_write(&self.paths.authorized_keys, text.as_bytes(), KEY_FILE)?;
        Ok(())
    }

    pub(crate) fn list_ssh_keys(&self) -> Result<Response, ApiError> {
        let keys = self
            .key_lines()?
            .iter()
            .filter_map(|line| parse_existing(line))
            .collect();
        Ok(Response::SshKeys(keys))
    }

    pub(crate) fn add_ssh_key(&self, line: &str) -> Result<Response, ApiError> {
        let parsed = parse_new(line).map_err(ApiError::Failed)?;
        let _guard = lock(&self.service_lock);
        let mut lines = self.key_lines()?;
        let existing: Vec<SshKey> = lines.iter().filter_map(|l| parse_existing(l)).collect();
        if existing
            .iter()
            .any(|k| k.fingerprint == parsed.key.fingerprint)
        {
            return Err(ApiError::Failed("that key can already log in".to_owned()));
        }
        if existing.len() >= MAX_KEYS {
            return Err(ApiError::Failed("too many keys".to_owned()));
        }
        lines.push(parsed.line);
        self.write_keys(&lines)?;
        Ok(Response::Ok)
    }

    /// The last key cannot be removed, because a headless device would then be reachable only
    /// from its console.
    pub(crate) fn remove_ssh_key(&self, fingerprint: &str) -> Result<Response, ApiError> {
        let _guard = lock(&self.service_lock);
        let lines = self.key_lines()?;
        let total = lines.iter().filter(|l| parse_existing(l).is_some()).count();
        let keep: Vec<String> = lines
            .iter()
            .filter(|l| parse_existing(l).is_none_or(|k| k.fingerprint != fingerprint))
            .cloned()
            .collect();
        if keep.len() == lines.len() {
            return Err(ApiError::NotFound("no key has that fingerprint".to_owned()));
        }
        if total <= 1 {
            return Err(ApiError::Failed(
                "this is the only key that can log in. Add another before removing it".to_owned(),
            ));
        }
        self.write_keys(&keep)?;
        Ok(Response::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem};

    const FILE: &str = "/root/.ssh/authorized_keys";

    fn key(seed: u8) -> String {
        let mut blob = Vec::new();
        blob.extend_from_slice(&11u32.to_be_bytes());
        blob.extend_from_slice(b"ssh-ed25519");
        blob.extend_from_slice(&32u32.to_be_bytes());
        blob.extend_from_slice(&[seed; 32]);
        format!("ssh-ed25519 {}", STANDARD.encode(blob))
    }

    fn listed(h: &Helper<FakeSystem>) -> Vec<SshKey> {
        let Ok(Response::SshKeys(keys)) = h.list_ssh_keys() else {
            panic!()
        };
        keys
    }

    #[test]
    fn adds_lists_and_removes_keys() {
        let h = helper(FakeSystem::default());
        h.add_ssh_key(&format!("{} laptop", key(1))).unwrap();
        h.add_ssh_key(&format!("  {} phone\u{2713}  ", key(2)))
            .unwrap();
        let keys = listed(&h);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].comment, "laptop");
        assert_eq!(keys[1].comment, "phone");
        assert!(keys[0].fingerprint.starts_with("SHA256:"));
        assert!(h
            .sys
            .calls()
            .contains(&format!("write {FILE} mode=600 group=-")));
        h.remove_ssh_key(&keys[0].fingerprint).unwrap();
        assert_eq!(listed(&h).len(), 1);
    }

    #[test]
    fn refuses_options_garbage_duplicates_and_mismatched_types() {
        let h = helper(FakeSystem::default());
        h.add_ssh_key(&key(1)).unwrap();
        let mismatched = key(1).replacen("ssh-ed25519", "ssh-rsa", 1);
        for bad in [
            format!("command=\"sh\" {}", key(3)),
            format!("{}\nssh-ed25519 AAAA", key(3)),
            "ssh-ed25519 !!!".to_owned(),
            "ssh-dss AAAA".to_owned(),
            mismatched,
            key(1),
            String::new(),
            format!("{} del\u{7f}", key(3)),
        ] {
            assert!(h.add_ssh_key(&bad).is_err(), "{bad}");
        }
        assert_eq!(listed(&h).len(), 1);
    }

    #[test]
    fn last_key_stays_and_unknown_lines_survive_a_rewrite() {
        let sys = FakeSystem::default();
        sys.put(
            FILE,
            format!("# note\nfrom=\"10.0.0.0/8\" {} admin\n", key(5)).as_bytes(),
        );
        let h = helper(sys);
        let keys = listed(&h);
        assert_eq!(keys.len(), 1);
        assert!(h.remove_ssh_key(&keys[0].fingerprint).is_err());
        assert!(matches!(
            h.remove_ssh_key("SHA256:nope"),
            Err(ApiError::NotFound(_))
        ));
        h.add_ssh_key(&key(6)).unwrap();
        let text = String::from_utf8(h.sys.get(FILE).unwrap()).unwrap();
        assert!(text.starts_with("# note\nfrom=\"10.0.0.0/8\""), "{text}");
        h.remove_ssh_key(&keys[0].fingerprint).unwrap();
        let text = String::from_utf8(h.sys.get(FILE).unwrap()).unwrap();
        assert!(text.contains("# note") && !text.contains("from="), "{text}");
    }
}
