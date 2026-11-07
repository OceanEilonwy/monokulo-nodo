//! Credential handling. A [`Secret`] has no `Display` or `Serialize`, and its `Debug` is redacted, so
//! it cannot reach a log or an error through formatting by accident.

use serde::Deserialize;
use std::fmt;

const REDACTED: &str = "[redacted]";
const MAX_DETAIL_CHARS: usize = 300;

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// Cleans text that came back from a provider before it can be stored or shown.
///
/// Providers may echo a key in an error message, and the text ends up on a terminal, so this removes
/// known secrets and control characters and caps the length.
#[derive(Clone)]
pub struct Scrubber {
    secrets: Vec<Secret>,
}

impl fmt::Debug for Scrubber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Scrubber")
    }
}

impl Scrubber {
    pub fn new(secrets: &[&Secret]) -> Self {
        Self { secrets: secrets.iter().map(|s| (*s).clone()).collect() }
    }

    pub fn clean(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for secret in &self.secrets {
            if !secret.expose().is_empty() {
                out = out.replace(secret.expose(), REDACTED);
            }
        }
        out.chars().filter(|c| !c.is_control()).take(MAX_DETAIL_CHARS).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_value() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert!(!format!("{:?}", Some(secret)).contains("hunter2"));
    }

    #[test]
    fn scrubber_removes_secrets_and_terminal_escapes() {
        let key = Secret::new("hunter2");
        let cleaned = Scrubber::new(&[&key]).clean("bad key hunter2\x1b[31m\n");
        assert_eq!(cleaned, "bad key [redacted][31m");
    }

    #[test]
    fn scrubber_caps_length() {
        let cleaned = Scrubber::new(&[]).clean(&"x".repeat(10_000));
        assert_eq!(cleaned.chars().count(), MAX_DETAIL_CHARS);
    }
}
