//! Short wordings every interface shows the same way: an app's state, how long it has been up,
//! who can open a site, what a row of the System page comes to, a certificate's standing, sizes
//! and dates.
//!
//! They only read: the device's `Snapshot` or one value from it. Wording that belongs to a flow
//! (a question, a refusal, a consequence) is with that flow in `flows`. Nothing here knows
//! colours or symbols; where a wording has a standing, a [`Tone`] says which, and each interface
//! draws that its own way.

use nodo_api::{CertState, ServiceState};

use crate::model::{CertRow, ConnectedProvider, FeedStatus, Site, Snapshot, UpdateItem, SYSTEM_BASE};

/// How a wording stands, for an interface to colour or mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    /// Under way, neither good nor bad yet.
    Working,
    Bad,
}

// Apps

/// An app's state in plain words, and whether it is a problem.
pub fn app_state(snapshot: &Snapshot, id: &str) -> (String, bool) {
    let Some(svc) = snapshot.services.iter().find(|svc| svc.id == id) else {
        return ("Not running".to_owned(), false);
    };
    let base = match svc.state {
        ServiceState::Running => {
            let mut parts = vec!["Running".to_owned()];
            parts.extend(svc.uptime_secs.map(|secs| format!("up {}", uptime(secs))));
            parts.extend((!svc.detail.is_empty()).then(|| svc.detail.clone()));
            parts.join(" · ")
        }
        ServiceState::Stopped => "Stopped".to_owned(),
        ServiceState::Failed => "Keeps stopping".to_owned(),
        ServiceState::Locked => "Locked until you unlock".to_owned(),
        ServiceState::Waiting => "Waiting for another app".to_owned(),
    };
    (base, svc.state == ServiceState::Failed)
}

/// How long an app has been up, in its two largest units, such as `3h 12m` or `2d 5h`.
pub fn uptime(secs: u64) -> String {
    let (days, hours, minutes) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60);
    let pair = |big: u64, big_unit: &str, small: u64, small_unit: &str| match small {
        0 => format!("{big}{big_unit}"),
        _ => format!("{big}{big_unit} {small}{small_unit}"),
    };
    match secs {
        s if s < 60 => "under a minute".to_owned(),
        s if s < 3600 => format!("{minutes}m"),
        s if s < 86400 => pair(hours, "h", minutes, "m"),
        _ => pair(days, "d", hours, "h"),
    }
}

/// What an app is for, from its source's description.
pub fn tagline(snapshot: &Snapshot, id: &str) -> String {
    snapshot.catalog.iter().find(|pkg| pkg.id.name == id).map(|pkg| pkg.tagline.clone()).unwrap_or_default()
}

/// Where an app's web page can be opened from, on its own page: `Home network, Tor`.
pub fn hosting_summary(snapshot: &Snapshot, name: &str) -> String {
    let Some(site) = snapshot.web.sites.iter().find(|s| s.package == name) else {
        return "No web page".to_owned();
    };
    let mut parts = Vec::new();
    if site.exposure.local {
        parts.push("Home network");
    }
    if site.exposure.onion {
        parts.push("Tor");
    }
    if site.exposure.public {
        parts.push("internet");
    }
    if parts.is_empty() {
        return "Not online".to_owned();
    }
    let joined = parts.join(", ");
    let mut chars = joined.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// What an app's log is worth opening for.
pub fn log_summary(snapshot: &Snapshot, name: &str) -> String {
    match snapshot.services.iter().find(|svc| svc.id == name).map(|svc| svc.state) {
        Some(ServiceState::Failed) => "It keeps stopping; the log says why".to_owned(),
        _ => "What it has been doing".to_owned(),
    }
}

/// What an empty list of apps on offer says: a search that matched nothing, or why the catalog
/// itself is empty (no sources, sources still being read, or a source that could not be read).
/// `query` is the search as typed.
pub fn empty_catalog_line(snapshot: &Snapshot, query: &str) -> String {
    if !snapshot.catalog.is_empty() || !query.is_empty() {
        return "Nothing matches that search.".to_owned();
    }
    let enabled: Vec<_> = snapshot.feeds.iter().filter(|f| f.status != FeedStatus::Disabled).collect();
    if let Some((name, problem)) = enabled.iter().find_map(|f| match &f.status {
        FeedStatus::Unreachable(problem) => Some((&f.name, problem)),
        _ => None,
    }) {
        return format!("Could not read the source {name}: {problem}");
    }
    if enabled.iter().any(|f| f.checked.is_empty()) {
        return "Checking sources…".to_owned();
    }
    if enabled.is_empty() {
        return "No sources are on. Add or turn one on under Sources.".to_owned();
    }
    "The sources offer no apps yet.".to_owned()
}

// Home and System

/// The device's health in one line: storage, temperature, uptime.
pub fn health_line(snapshot: &Snapshot) -> String {
    snapshot.overview.device.iter().take(3).map(|(label, value)| format!("{label} {value}")).collect::<Vec<_>>().join("   ·   ")
}

/// The rows of the System page, in order.
pub const SYSTEM_ROWS: [&str; 5] = ["Updates", "Backups", "Network", "Device logs", "About this Nodo"];

/// The update of the Nodo itself, when one is offered and not held back.
pub fn nodo_update(snapshot: &Snapshot) -> Option<&UpdateItem> {
    snapshot.updates.iter().find(|item| item.name == SYSTEM_BASE && !item.held)
}

/// What a row of the System page comes to, beside its name. `row` is one of [`SYSTEM_ROWS`].
pub fn system_row_summary(snapshot: &Snapshot, row: &str) -> String {
    match row {
        "Updates" => {
            let apps = snapshot.updates.iter().filter(|u| u.name != SYSTEM_BASE && !u.held).count();
            let nodo = nodo_update(snapshot).is_some();
            let mut parts = Vec::new();
            if apps > 0 {
                parts.push(if apps == 1 { "1 for an app".to_owned() } else { format!("{apps} for apps") });
            }
            if nodo {
                parts.push("1 for the Nodo".to_owned());
            }
            let auto = snapshot.auto_update.enabled_count();
            let automatic = if auto == 0 { "automatic updates off".to_owned() } else { format!("automatic for {auto}") };
            if parts.is_empty() {
                format!("Up to date · {automatic}")
            } else {
                format!("{} · {automatic}", parts.join(", "))
            }
        }
        "Backups" => "Make one now, or on a schedule".to_owned(),
        "Network" => "Cable, Wi-Fi and Bluetooth".to_owned(),
        "Device logs" => "What the Nodo and its apps have been doing".to_owned(),
        _ => snapshot.overview.device.iter().take(2).map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(" · "),
    }
}

// Web

/// Who can open a site, in words.
pub fn reach_words(site: &Site) -> &'static str {
    let e = site.exposure;
    match (e.local, e.onion, e.public) {
        (_, _, true) if e.local || e.onion => "Home and internet",
        (_, _, true) => "Internet",
        (true, true, false) => "Home network and Tor",
        (true, false, false) => "Home network",
        (false, true, false) => "Tor",
        (false, false, false) => "Not online",
    }
}

/// The address to show for a site: its public one when it is on the internet, else its address
/// on the home network, else its onion address. Empty when it is not online.
pub fn site_address(site: &Site) -> String {
    if site.exposure.public {
        if let Some(host) = &site.public_host {
            return format!("https://{host}");
        }
    }
    if site.exposure.local && !site.local_url.is_empty() {
        return site.local_url.clone();
    }
    if site.exposure.onion && !site.onion_url.is_empty() {
        return site.onion_url.clone();
    }
    String::new()
}

/// One certificate in a few words, and how it stands.
pub fn cert_status(row: &CertRow) -> (Tone, String) {
    match row.state {
        CertState::Valid => match row.days_left {
            Some(days) => (Tone::Good, format!("Valid, {days} days left")),
            None => (Tone::Good, "Valid".to_owned()),
        },
        CertState::Obtaining | CertState::Waiting => (Tone::Working, "Getting a certificate".to_owned()),
        CertState::Failed => (Tone::Bad, "Not issued".to_owned()),
        CertState::Expired => (Tone::Bad, "Expired".to_owned()),
    }
}

/// Each connected provider's name, numbered when the owner connected two accounts at one company.
pub fn provider_labels(connected: &[ConnectedProvider]) -> Vec<String> {
    connected
        .iter()
        .enumerate()
        .map(|(index, provider)| {
            let earlier = connected[..index].iter().filter(|p| p.name == provider.name).count();
            if earlier == 0 { provider.name.clone() } else { format!("{} {}", provider.name, earlier + 1) }
        })
        .collect()
}

/// The name of a field of a provider's key, as that company calls it.
pub fn provider_field_name(field: &str) -> String {
    match field {
        "api_key" => "API key".to_owned(),
        "secret_key" => "Secret key".to_owned(),
        "api_token" => "API token".to_owned(),
        "personal_access_token" => "Access token".to_owned(),
        "username" => "Username".to_owned(),
        "access_key_id" => "AccessKey ID".to_owned(),
        "access_key_secret" => "AccessKey secret".to_owned(),
        other => other.replace('_', " "),
    }
}

// Sources and keys

/// A source read over plain HTTP. What it offers is still checked against its pinned key, but
/// the person adding it should know what HTTP leaves open.
pub fn is_plain_http(address: &str) -> bool {
    address.starts_with("http://")
}

/// `1 app`, `3 apps`.
pub fn counted(n: u64, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// A key fingerprint as groups of eight digits, to compare by eye.
pub fn fingerprint_groups(fp: &str) -> String {
    // A "SHA256:" or similar label is not part of the key; its digits must not join the groups.
    let body = fp.rsplit_once(": ").map_or(fp, |(_, rest)| rest);
    let body = body.split_once("256:").map_or(body, |(_, rest)| rest);
    let hex: String = body.chars().filter(char::is_ascii_hexdigit).collect();
    let pieces: Vec<String> = hex.as_bytes().chunks(8).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
    pieces.join("  ")
}

// Sizes, drives and dates

/// A size in the unit that reads best: `4.8 MB`, `32.0 GB`, never less than `1 KB`.
pub fn size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1e6)
    } else {
        format!("{:.0} KB", (bytes as f64 / 1e3).max(1.0))
    }
}

/// A removable drive by what its owner knows it as: its model, then its label.
pub fn drive_name(drive: &nodo_api::UsbDrive) -> String {
    let mut name = if drive.model.is_empty() { "USB drive".to_owned() } else { drive.model.clone() };
    if let Some(label) = &drive.label {
        name.push_str(&format!(", {label}"));
    }
    name
}

/// The day of a moment given in seconds since 1970, in UTC: `2026-11-19`.
pub fn day(at: u64) -> String {
    // Howard Hinnant's days-to-civil conversion.
    let z = (at / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::load_snapshot;
    use crate::mock::MockBackend;

    #[test]
    fn uptime_uses_the_two_largest_units() {
        assert_eq!(uptime(59), "under a minute");
        assert_eq!(uptime(45 * 60 + 30), "45m");
        assert_eq!(uptime(3 * 3600 + 12 * 60 + 59), "3h 12m");
        assert_eq!(uptime(86400), "1d");
        assert_eq!(uptime(2 * 86400 + 5 * 3600 + 59 * 60), "2d 5h");
    }

    #[test]
    fn an_apps_state_says_what_it_is_doing_and_whether_that_is_a_problem() {
        let mut snapshot = load_snapshot(&mut MockBackend::new()).expect("the mock answers");
        assert_eq!(app_state(&snapshot, "not installed"), ("Not running".to_owned(), false));
        let id = snapshot.services[0].id.clone();
        snapshot.services[0].state = ServiceState::Failed;
        assert_eq!(app_state(&snapshot, &id), ("Keeps stopping".to_owned(), true));
        assert_eq!(log_summary(&snapshot, &id), "It keeps stopping; the log says why");
        snapshot.services[0].state = ServiceState::Running;
        snapshot.services[0].uptime_secs = Some(3 * 3600 + 12 * 60);
        snapshot.services[0].detail = "synced".to_owned();
        assert_eq!(app_state(&snapshot, &id), ("Running · up 3h 12m · synced".to_owned(), false));
        snapshot.services[0].state = ServiceState::Locked;
        assert_eq!(app_state(&snapshot, &id).0, "Locked until you unlock");
    }

    #[test]
    fn a_sites_reach_and_address_follow_where_it_is_online() {
        let snapshot = load_snapshot(&mut MockBackend::new()).expect("the mock answers");
        let mut site = snapshot.web.sites[0].clone();
        site.exposure = nodo_api::Exposure { local: false, onion: false, public: false };
        assert_eq!((reach_words(&site), site_address(&site)), ("Not online", String::new()));
        assert_eq!(hosting_summary(&snapshot, "no such app"), "No web page");
        site.exposure.local = true;
        site.local_url = "https://shop.nodo.local".to_owned();
        assert_eq!((reach_words(&site), site_address(&site)), ("Home network", "https://shop.nodo.local".to_owned()));
        site.exposure.public = true;
        site.public_host = Some("shop.example.com".to_owned());
        assert_eq!((reach_words(&site), site_address(&site)), ("Home and internet", "https://shop.example.com".to_owned()));
        let mut snapshot = snapshot;
        let name = site.package.clone();
        site.exposure.onion = true;
        snapshot.web.sites[0] = site;
        assert_eq!(hosting_summary(&snapshot, &name), "Home network, Tor, internet");
    }

    #[test]
    fn the_system_rows_say_what_waits_behind_them() {
        let mut snapshot = load_snapshot(&mut MockBackend::new()).expect("the mock answers");
        for row in SYSTEM_ROWS {
            assert!(!system_row_summary(&snapshot, row).is_empty(), "{row}");
        }
        snapshot.updates.clear();
        assert!(system_row_summary(&snapshot, "Updates").starts_with("Up to date · "));
        assert_eq!(nodo_update(&snapshot), None);
    }

    #[test]
    fn small_wordings() {
        assert_eq!((counted(1, "app"), counted(3, "app")), ("1 app".to_owned(), "3 apps".to_owned()));
        assert!(is_plain_http("http://feed.example") && !is_plain_http("https://feed.example"));
        assert_eq!(fingerprint_groups("SHA256: 0123456789abcdef0123"), "01234567  89abcdef  0123");
        assert_eq!((size(500), size(4_812_300), size(32_008_000_000)), ("1 KB".to_owned(), "4.8 MB".to_owned(), "32.0 GB".to_owned()));
        assert_eq!((day(0), day(1_762_560_000)), ("1970-01-01".to_owned(), "2025-11-08".to_owned()));
        assert_eq!(provider_field_name("api_key"), "API key");
        assert_eq!(provider_field_name("zone_id"), "zone id");
        let empty = Snapshot::default();
        assert_eq!(empty_catalog_line(&empty, "wallet"), "Nothing matches that search.");
        assert_eq!(empty_catalog_line(&empty, ""), "No sources are on. Add or turn one on under Sources.");
    }
}
