//! Semantic checks that TOML parsing cannot express.
//!
//! A manifest comes from an untrusted package and its fields end up in usernames, paths, firewall
//! rules and proxy config, so every check is an allow list rather than a list of known bad values.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{HealthCheck, Manifest, Protocol};

/// Leaves room for a prefix within the 32 character limit on Linux usernames, because each package
/// gets its own service user.
pub const MAX_ID_LEN: usize = 24;
pub const MAX_NAME_LEN: usize = 63;
pub const MAX_VERSION_LEN: usize = 64;
pub const MAX_PATH_LEN: usize = 255;
const MAX_ARG_LEN: usize = 1024;
const MAX_EXEC_ARGS: usize = 64;
const MAX_LIST_LEN: usize = 64;
const MAX_STOP_TIMEOUT_SECS: u32 = 3600;

/// Packages run unprivileged and never receive `CAP_NET_BIND_SERVICE`, so they cannot own these.
pub const MIN_PACKAGE_PORT: u16 = 1024;

/// Ids that would collide with system users, the appliance itself, or well known roles. A package
/// named after a role could otherwise capture references meant for the role's provider.
pub const RESERVED_IDS: &[&str] = &[
    "cuprated",
    "monerod",
    "adm",
    "apps",
    "bin",
    "caddy",
    "daemon",
    "dbus",
    "lp",
    "mail",
    "monero-daemon",
    "monokulo",
    "news",
    "nobody",
    "nodo",
    "nogroup",
    "operator",
    "root",
    "shutdown",
    "sshd",
    "sudo",
    "sync",
    "sys",
    "tor",
    "tty",
    "users",
    "uucp",
    "wheel",
];

/// One problem found in a manifest. Validation reports every problem, not only the first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestIssue {
    /// The offending field, such as `web[0].upstream`.
    pub field: String,
    pub problem: String,
}

impl fmt::Display for ManifestIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.problem)
    }
}

fn is_token(text: &str, max_len: usize) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= max_len
        && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1] != b'-'
        && !text.contains("--")
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// A package id, usable as a username, a path segment, an xbps package name and a DNS label.
pub fn is_valid_id(id: &str) -> bool {
    is_token(id, MAX_ID_LEN) && !RESERVED_IDS.contains(&id)
}

/// A role, capability or web service name.
pub fn is_valid_name(name: &str) -> bool {
    is_token(name, MAX_NAME_LEN)
}

/// A version in the form xbps accepts: `-` and `_` would corrupt the `name-version_revision` pkgver.
pub fn is_valid_version(version: &str) -> bool {
    let bytes = version.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_VERSION_LEN
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().any(u8::is_ascii_digit)
        && bytes.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'~'))
}

/// A relative path that stays inside the package directory however it is joined. It must be in
/// canonical form, so checks elsewhere never disagree with the kernel about what it names.
pub fn is_valid_package_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= MAX_PATH_LEN && path.split('/').all(is_safe_segment)
}

fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.starts_with('-')
        && segment.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
}

/// Strict decimal port: `u16::from_str` alone would also accept `+80` and `0080`.
fn parse_port(text: &str) -> Option<u16> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) || text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}

fn check_port(port: u16) -> Result<(), String> {
    if port < MIN_PACKAGE_PORT {
        return Err(format!("port {port} is below {MIN_PACKAGE_PORT}, which packages may not use"));
    }
    Ok(())
}

/// `/admin` or `/admin/*`. The value lands in generated proxy config, so nothing with meaning to a
/// URL parser or config syntax is allowed, and `..` could step around the rule it declares.
fn is_valid_url_path(path: &str, allow_trailing_wildcard: bool) -> bool {
    if path.len() > MAX_PATH_LEN || !path.starts_with('/') {
        return false;
    }
    if path == "/" {
        return true;
    }
    let trimmed = match path.strip_suffix("/*") {
        Some(rest) if allow_trailing_wildcard => rest,
        _ => path,
    };
    if trimmed.is_empty() {
        return true;
    }
    trimmed.split('/').skip(1).all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~'))
    })
}

fn check_upstream(id: &str, name: &str, upstream: &str) -> Result<(), String> {
    if let Some(rest) = upstream.strip_prefix("127.0.0.1:") {
        let port = parse_port(rest).ok_or_else(|| "has an invalid port".to_string())?;
        return check_port(port);
    }
    let socket = format!("unix:/run/apps/{id}/{name}.sock");
    if upstream == socket {
        return Ok(());
    }
    Err(format!("must be '{socket}' or '127.0.0.1:<port>'"))
}

fn check_health_url(url: &str) -> Result<(), String> {
    let rest = url.strip_prefix("http://").ok_or("must start with http://")?;
    let (authority, path) = match rest.find('/') {
        Some(at) => rest.split_at(at),
        None => (rest, "/"),
    };
    let port = ["127.0.0.1:", "localhost:", "[::1]:"]
        .iter()
        .find_map(|host| authority.strip_prefix(host))
        .ok_or("must be a loopback address with an explicit port")?;
    let port = parse_port(port).ok_or("has an invalid port")?;
    check_port(port)?;
    if !is_valid_url_path(path, false) {
        return Err("has an invalid path".to_string());
    }
    Ok(())
}

fn check_exec_arg(id: &str, arg: &str) -> Result<(), String> {
    if arg.len() > MAX_ARG_LEN {
        return Err(format!("is longer than {MAX_ARG_LEN} bytes"));
    }
    if arg.chars().any(char::is_control) {
        return Err("contains a control character".to_string());
    }
    // An option's value may be a path, as in `--config=../x`, so look inside the common separators.
    // A URL's `//` introduces a host, not a path.
    let without_url_slashes = arg.replace("://", " ");
    for piece in without_url_slashes.split(['=', ':', ',']) {
        if piece.split('/').any(|segment| segment == "..") {
            return Err("contains a '..' path component".to_string());
        }
        if piece.starts_with('/')
            && !piece.starts_with(&format!("/opt/nodo/apps/{id}/"))
            && !piece.starts_with(&format!("/run/apps/{id}/"))
        {
            return Err("names an absolute path outside the package's directories".to_string());
        }
    }
    Ok(())
}

struct Checker {
    issues: Vec<ManifestIssue>,
}

impl Checker {
    fn add(&mut self, field: impl Into<String>, problem: impl Into<String>) {
        self.issues.push(ManifestIssue { field: field.into(), problem: problem.into() });
    }

    fn check(&mut self, field: impl Into<String>, result: Result<(), String>) {
        if let Err(problem) = result {
            self.add(field, problem);
        }
    }

    fn check_names(&mut self, field: &str, names: &[String]) {
        if names.len() > MAX_LIST_LEN {
            self.add(field, format!("has more than {MAX_LIST_LEN} entries"));
        }
        let mut seen = BTreeSet::new();
        for (index, name) in names.iter().enumerate() {
            if !is_valid_name(name) {
                self.add(format!("{field}[{index}]"), "is not a valid name");
            }
            if !seen.insert(name) {
                self.add(format!("{field}[{index}]"), "is a duplicate");
            }
        }
    }
}

impl Manifest {
    /// Returns every problem with this manifest. [`Manifest::parse`] already calls this, so it is
    /// only needed for manifests built in code or deserialized some other way.
    pub fn validate(&self) -> Vec<ManifestIssue> {
        let mut c = Checker { issues: Vec::new() };

        if !is_valid_id(&self.id) {
            c.add(
                "id",
                format!(
                    "must be lowercase letters, digits and single hyphens, start with a letter, be at \
                     most {MAX_ID_LEN} characters and not a reserved name"
                ),
            );
        }
        if !is_valid_version(&self.version) {
            c.add("version", "must contain a digit and only letters, digits, '.', '+' and '~'");
        }

        match self.exec.split_first() {
            None => c.add("exec", "must name an executable"),
            Some((program, args)) => {
                if !is_valid_package_path(program) {
                    c.add("exec[0]", "must be a canonical relative path inside the package");
                }
                if args.len() > MAX_EXEC_ARGS {
                    c.add("exec", format!("has more than {MAX_EXEC_ARGS} arguments"));
                }
                for (index, arg) in args.iter().enumerate() {
                    c.check(format!("exec[{}]", index + 1), check_exec_arg(&self.id, arg));
                }
            }
        }

        c.check_names("needs", &self.needs);
        c.check_names("provides", &self.provides);

        if let Some(settings) = &self.settings {
            if !is_valid_package_path(settings) || !settings.ends_with(".lua") {
                c.add("settings", "must be a canonical relative .lua path inside the package");
            }
        }

        let mut bound = BTreeSet::new();
        for (index, inbound) in self.network.inbound.iter().enumerate() {
            let field = format!("network.inbound[{index}].port");
            c.check(field.clone(), check_port(inbound.port));
            let is_tcp = matches!(inbound.protocol, Protocol::Tcp);
            if !bound.insert((inbound.port, is_tcp)) {
                c.add(field, "is declared twice for this protocol");
            }
        }

        if self.web.len() > MAX_LIST_LEN {
            c.add("web", format!("has more than {MAX_LIST_LEN} entries"));
        }
        let mut web_names = BTreeSet::new();
        for (index, web) in self.web.iter().enumerate() {
            if !is_valid_name(&web.name) {
                c.add(format!("web[{index}].name"), "is not a valid name");
            }
            if !web_names.insert(&web.name) {
                c.add(format!("web[{index}].name"), "is a duplicate");
            }
            c.check(format!("web[{index}].upstream"), check_upstream(&self.id, &web.name, &web.upstream));
            if let Some(subdomain) = &web.suggested_subdomain {
                if !is_valid_name(subdomain) {
                    c.add(format!("web[{index}].suggested_subdomain"), "is not a valid DNS label");
                }
            }
            if web.admin_paths.len() > MAX_LIST_LEN {
                c.add(format!("web[{index}].admin_paths"), format!("has more than {MAX_LIST_LEN} entries"));
            }
            for (path_index, path) in web.admin_paths.iter().enumerate() {
                if !is_valid_url_path(path, true) {
                    c.add(format!("web[{index}].admin_paths[{path_index}]"), "is not a plain absolute URL path");
                }
            }
        }

        if self.stop_timeout_secs == 0 || self.stop_timeout_secs > MAX_STOP_TIMEOUT_SECS {
            c.add("stop_timeout_secs", format!("must be between 1 and {MAX_STOP_TIMEOUT_SECS}"));
        }

        match &self.health {
            Some(HealthCheck::Http { url }) => c.check("health.http.url", check_health_url(url)),
            Some(HealthCheck::Tcp { port }) => c.check("health.tcp.port", check_port(*port)),
            None => {}
        }

        c.issues
    }
}
