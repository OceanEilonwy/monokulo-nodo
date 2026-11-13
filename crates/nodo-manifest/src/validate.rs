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
///
/// `monokulo` is here so that no feed app can pass itself off as the project. Its own apps install
/// from a feed like any other under the `mk-` prefix (`mk-pay`, `mk-bell`, ...). The appliance's
/// own packages use the `nodo-` prefix (and formerly `monokulo-`), which the feed gate refuses.
pub const RESERVED_IDS: &[&str] = &[
    "cuprated",
    "monerod",
    "monokulo",
    "adm",
    "apps",
    "bin",
    "caddy",
    "daemon",
    "dbus",
    "lp",
    "mail",
    "monero-daemon",
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
/// A file name inside the package's config directory. No separators, and no leading dot, so
/// it can neither leave the directory nor hide or replace the settings store.
pub fn is_valid_config_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

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
        if let Some(config) = &self.config {
            if self.settings.is_none() {
                c.add("config", "needs a settings file to fill it");
            }
            if !is_valid_package_path(&config.template) {
                c.add("config.template", "must be a canonical relative path inside the package");
            }
            if !is_valid_config_name(&config.name) {
                c.add("config.name", "must be a plain file name");
            }
        }

        if let Some(size) = self.storage.size_mib {
            if !self.storage.encrypted {
                c.add("storage.size_mib", "only applies to encrypted storage");
            } else if !(crate::MIN_VOLUME_MIB..=crate::MAX_VOLUME_MIB).contains(&size) {
                c.add(
                    "storage.size_mib",
                    format!("must be from {} to {}", crate::MIN_VOLUME_MIB, crate::MAX_VOLUME_MIB),
                );
            }
        }
        if self.storage.data_layout == Some(0) {
            c.add("storage.data_layout", "starts at 1");
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

        self.check_panels(&mut c);

        c.issues
    }

    /// Ports this package's own service listens on, so a panel can only read its own package.
    fn own_ports(&self) -> BTreeSet<u16> {
        let mut ports: BTreeSet<u16> = self.network.inbound.iter().map(|inbound| inbound.port).collect();
        for web in &self.web {
            if let Some(port) = web.upstream.strip_prefix("127.0.0.1:").and_then(parse_port) {
                ports.insert(port);
            }
        }
        match &self.health {
            Some(HealthCheck::Http { url }) => ports.extend(loopback_port(url)),
            Some(HealthCheck::Tcp { port }) => {
                ports.insert(*port);
            }
            None => {}
        }
        ports
    }

    fn check_panels(&self, c: &mut Checker) {
        if self.panels.len() > crate::MAX_PANELS {
            c.add("panels", format!("has more than {} entries", crate::MAX_PANELS));
        }
        let own = self.own_ports();
        let mut ids = BTreeSet::new();
        for (index, panel) in self.panels.iter().enumerate() {
            let field = |name: &str| format!("panels[{index}].{name}");
            if !is_token(&panel.id, MAX_ID_LEN) {
                c.add(field("id"), "must be lowercase letters, digits and single hyphens");
            } else if !ids.insert(panel.id.as_str()) {
                c.add(field("id"), "is a duplicate");
            }
            if !is_display_text(&panel.title, crate::MAX_PANEL_TITLE_LEN) {
                c.add(
                    field("title"),
                    format!("must be 1 to {} printable characters on one line", crate::MAX_PANEL_TITLE_LEN),
                );
            }
            if !is_valid_package_path(&panel.script) || !panel.script.ends_with(".lua") {
                c.add(field("script"), "must be a canonical relative .lua path inside the package");
            }
            if !(crate::MIN_PANEL_REFRESH_SECS..=crate::MAX_PANEL_REFRESH_SECS).contains(&panel.refresh_secs) {
                c.add(
                    field("refresh_secs"),
                    format!("must be from {} to {}", crate::MIN_PANEL_REFRESH_SECS, crate::MAX_PANEL_REFRESH_SECS),
                );
            }
            c.check(field("min_size"), check_panel_size(panel.min_size));
            c.check(field("max_size"), check_panel_size(panel.max_size));
            if !fits_within(panel.min_size, panel.max_size) {
                c.add(field("max_size"), "must be at least min_size in both directions");
            }
            if let Some(size) = panel.default_size {
                if !fits_within(panel.min_size, size) || !fits_within(size, panel.max_size) {
                    c.add(field("default_size"), "must be from min_size to max_size");
                }
            }
            match &panel.source {
                crate::PanelSource::Http { url } => c.check(field("source.http.url"), check_panel_url(url, &own)),
                crate::PanelSource::Jsonrpc { url, method } => {
                    c.check(field("source.jsonrpc.url"), check_panel_url(url, &own));
                    if !is_method_name(method) {
                        c.add(field("source.jsonrpc.method"), "must be 1 to 64 letters, digits, '_' or '.'");
                    }
                }
                // Only the base system's signed manifests, parsed without validation, may read the
                // device directly.
                crate::PanelSource::Device {} => c.add(field("source"), "device is for the base system only"),
            }
        }
    }
}

/// The port of a loopback URL that passes [`check_health_url`].
fn loopback_port(url: &str) -> Option<u16> {
    check_health_url(url).ok()?;
    let authority = url.strip_prefix("http://")?.split('/').next()?;
    parse_port(authority.rsplit_once(':')?.1)
}

fn check_panel_url(url: &str, own: &BTreeSet<u16>) -> Result<(), String> {
    check_health_url(url)?;
    let port = loopback_port(url).ok_or("has an invalid port")?;
    if !own.contains(&port) {
        return Err(format!(
            "port {port} is not one of this package's own (network.inbound, web upstreams or health)"
        ));
    }
    Ok(())
}

fn check_panel_size([columns, rows]: [u8; 2]) -> Result<(), String> {
    let [max_columns, max_rows] = crate::PANEL_GRID;
    if !(1..=max_columns).contains(&columns) || !(1..=max_rows).contains(&rows) {
        return Err(format!("must be [columns, rows] within [1, 1] to [{max_columns}, {max_rows}]"));
    }
    Ok(())
}

fn fits_within(small: [u8; 2], large: [u8; 2]) -> bool {
    small[0] <= large[0] && small[1] <= large[1]
}

fn is_method_name(method: &str) -> bool {
    (1..=64).contains(&method.len()) && method.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.'))
}

/// One line of printable text: no control characters, so it cannot draw over the screen.
fn is_display_text(text: &str, max_chars: usize) -> bool {
    let count = text.chars().count();
    (1..=max_chars).contains(&count) && !text.chars().any(char::is_control) && text.trim() == text
}
