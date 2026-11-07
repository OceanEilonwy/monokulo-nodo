//! Strict validation of everything that reaches a script or firewall rule.
//!
//! These are allow-lists. Quoting is a second layer, not the reason a value is safe.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GenError {
    #[error("invalid package id")]
    Id,
    #[error("invalid exec: {0}")]
    Exec(&'static str),
    #[error("invalid path: {0}")]
    Path(&'static str),
    #[error("invalid port")]
    Port,
    #[error("invalid web service: {0}")]
    Web(&'static str),
    #[error("invalid grant: {0}")]
    Grant(&'static str),
}

/// The service user is `_<id>`, and Linux user names stop at 32 characters.
const MAX_ID_LEN: usize = 31;
const MAX_PATH_LEN: usize = 256;
const MAX_ARG_LEN: usize = 4096;
const MAX_ARGS: usize = 256;

/// `[a-z][a-z0-9-]*`, with no trailing hyphen or doubled hyphen.
pub fn package_id(id: &str) -> Result<(), GenError> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.ends_with('-')
        && !id.contains("--");
    if ok {
        Ok(())
    } else {
        Err(GenError::Id)
    }
}

/// One path component. A leading `-` or `.` is refused so no component can read as an option or
/// a hidden file, which also rules out `.` and `..`.
fn component(c: &str) -> bool {
    c.starts_with(|ch: char| ch.is_ascii_alphanumeric() || ch == '_')
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// An absolute path with no `..`, empty components, or characters needing a quote.
pub fn absolute_path(path: &str) -> Result<(), GenError> {
    let rest = path
        .strip_prefix('/')
        .ok_or(GenError::Path("must be absolute"))?;
    relative_path(rest)
}

/// A path inside the package directory, under the same rules.
pub fn relative_path(path: &str) -> Result<(), GenError> {
    if path.len() > MAX_PATH_LEN {
        return Err(GenError::Path("too long"));
    }
    if path.split('/').all(component) {
        Ok(())
    } else {
        Err(GenError::Path("contains an unsafe component"))
    }
}

/// Arguments reach the program verbatim through quoting. Control characters are refused because
/// no legitimate argument has them and they corrupt logs and diagnostics.
pub fn exec_args(args: &[String]) -> Result<(), GenError> {
    if args.len() > MAX_ARGS {
        return Err(GenError::Exec("too many arguments"));
    }
    for arg in args {
        if arg.len() > MAX_ARG_LEN {
            return Err(GenError::Exec("argument too long"));
        }
        if arg.chars().any(char::is_control) {
            return Err(GenError::Exec("argument contains a control character"));
        }
    }
    Ok(())
}

/// A web service name, used in socket paths: `[a-z0-9][a-z0-9_-]*`.
pub fn web_name(name: &str) -> Result<(), GenError> {
    let ok = name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(GenError::Web("invalid name"))
    }
}

pub fn port(port: u16) -> Result<u16, GenError> {
    if port == 0 {
        Err(GenError::Port)
    } else {
        Ok(port)
    }
}
