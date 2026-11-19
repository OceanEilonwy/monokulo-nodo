//! Secret text and the text input widget's state.
//!
//! Neither type can reach the screen by accident: `Secret` has no `Display`, its `Debug` is
//! redacted, and `Input::display` masks secret inputs. The rendering code only ever calls
//! `display`.

use std::fmt;

use zeroize::Zeroizing;

/// Fixed-width mask for a stored secret, so the mask does not reveal its length.
pub const STORED_MASK: &str = "••••••••••••";

#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// Named so that every read of a secret is easy to find in review.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// An append-only text field. Cursor movement is left out on purpose: the fields here are short,
/// and a passphrase field that hides its text is easier to retype than to edit mid-string.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Input {
    buf: Secret,
    secret: bool,
}

impl Input {
    pub fn plain(initial: impl Into<String>) -> Self {
        Self { buf: Secret::new(initial), secret: false }
    }

    pub fn secret() -> Self {
        Self { buf: Secret::default(), secret: true }
    }

    pub fn push(&mut self, c: char) {
        if !c.is_control() {
            let mut next = Zeroizing::new(String::with_capacity(self.buf.0.len() + c.len_utf8()));
            next.push_str(&self.buf.0);
            next.push(c);
            self.buf = Secret(next);
        }
    }

    pub fn pop(&mut self) {
        self.buf.0.pop();
    }

    pub fn clear(&mut self) {
        self.buf = Secret::default();
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn is_secret(&self) -> bool {
        self.secret
    }

    /// The plain text of a non-secret input. A secret input yields nothing, so a secret cannot
    /// flow into a label or a notice through this path.
    pub fn text(&self) -> &str {
        if self.secret {
            ""
        } else {
            self.buf.expose()
        }
    }

    pub fn display(&self) -> String {
        if self.secret {
            "•".repeat(self.buf.expose().chars().count())
        } else {
            self.buf.expose().to_owned()
        }
    }

    pub fn secret_value(&self) -> Secret {
        self.buf.clone()
    }

    pub fn take_secret(&mut self) -> Secret {
        std::mem::take(&mut self.buf)
    }
}

impl fmt::Debug for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.secret {
            f.write_str("Input(<redacted>)")
        } else {
            write!(f, "Input({:?})", self.buf.expose())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_input_never_shows_its_value() {
        let mut input = Input::secret();
        for c in "hunter2".chars() {
            input.push(c);
        }
        assert_eq!(input.display(), "•••••••");
        assert_eq!(input.text(), "");
        assert!(!format!("{input:?}").contains("hunter2"));
        let secret = input.take_secret();
        assert_eq!(secret.expose(), "hunter2");
        assert!(!format!("{secret:?}").contains("hunter2"));
        assert!(input.is_empty());
    }

    #[test]
    fn inputs_ignore_control_characters() {
        let mut input = Input::plain("");
        input.push('\u{1b}');
        input.push('a');
        input.push('\n');
        assert_eq!(input.text(), "a");
    }
}
