//! Short wordings every interface shows the same way: an app's state, how long it has been up,
//! who can open a site, what a row of the System page comes to, a certificate's standing, Home's
//! cards and the unlock page's lines, the counts beside the tabs, an update's row, sizes and
//! dates.
//!
//! They only read: the device's `Snapshot` or one value from it. Wording that belongs to a flow
//! (a question, a refusal, a consequence) is with that flow in `flows`. Nothing here knows
//! colours or symbols; where a wording has a standing, a [`Tone`] says which, and each interface
//! draws that its own way.

use nodo_api::{CertState, ServiceState};

use crate::model::{CertRow, ConnectedProvider, FeedStatus, FieldKind, PackageStatus, Site, Snapshot, UpdateItem, UpdateNote, SYSTEM_BASE};
use crate::queries::HomeItem;
use crate::screen::Tab;

/// How a wording stands, for an interface to colour or mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    /// Under way, neither good nor bad yet.
    Working,
    Bad,
}

// Device

/// How busy the processor is, in thousandths of its capacity: the one-minute load average over
/// the cores, at most 1000 when more is waiting than the cores can run.
pub fn cpu_busy_permille(cpu: &nodo_api::CpuStatus) -> u32 {
    let capacity = u64::from(cpu.cores.max(1)) * 1000;
    let busy = (u64::from(cpu.load_average_milli) * 1000 / capacity).min(1000);
    u32::try_from(busy).unwrap_or(1000)
}

/// The processor in one line: `23% · 8 cores · 52 °C`, without the temperature when the board
/// reports none.
pub fn cpu_summary(cpu: &nodo_api::CpuStatus) -> String {
    let percent = (cpu_busy_permille(cpu) + 5) / 10;
    let cores = match cpu.cores {
        1 => "1 core".to_owned(),
        n => format!("{n} cores"),
    };
    match cpu.temperature_millicelsius {
        Some(milli) => format!("{percent}% · {cores} · {} °C", (milli + 500).div_euclid(1000)),
        None => format!("{percent}% · {cores}"),
    }
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
        FeedStatus::Unreachable(problem) => Some((f.shown(), problem)),
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
pub const SYSTEM_ROWS: [&str; 6] = ["Updates", "Backups", "Network", "Web GUI", "Device logs", "About this Nodo"];

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
        "Web GUI" => match &snapshot.web_gui {
            Some(status) => web_gui_summary(status),
            None => "These pages in a browser on the home network".to_owned(),
        },
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

/// Who uses one of a provider's domains, in a few words.
pub fn provider_domain_note(domain: &crate::model::ConnectedDomain) -> String {
    match (&domain.held_by, domain.used_by.is_empty()) {
        (Some(other), _) => format!("used through {other}, which was connected first"),
        (None, false) => format!("used by {}", domain.used_by.join(", ")),
        (None, true) => "not used by an app yet".to_owned(),
    }
}

/// A connection whose key the helper has not read yet has no hint to show.
pub const PROVIDER_KEY_NOT_READ: &str = "Not read yet. Look again while the Nodo is unlocked to show which key this is.";

/// Said under a key's hint: the hint is all that is ever shown.
pub const PROVIDER_KEY_STAYS_LOCKED: &str = "The rest of the key stays locked on this Nodo and is never shown.";

/// When a provider's domains were last read, or that they have not been since it was connected.
pub fn provider_checked(provider: &ConnectedProvider) -> String {
    provider.checked.clone().unwrap_or_else(|| "Not since it was connected".to_owned())
}

/// The domains of a provider's account its key cannot change yet, in one line for a list.
pub fn provider_blocked_line(blocked: &[(String, String)]) -> String {
    let names: Vec<&str> = blocked.iter().map(|(name, _)| name.as_str()).collect();
    format!("Can't be used yet: {}", names.join(", "))
}

/// What to switch on for the domains a key cannot change yet: each different reason once, in the
/// order met, as a sentence that ends with looking again.
pub fn provider_blocked_remedies(blocked: &[(String, String)]) -> Vec<String> {
    let mut reasons: Vec<&str> = Vec::new();
    for (_, reason) in blocked {
        if !reasons.contains(&reason.as_str()) {
            reasons.push(reason);
        }
    }
    reasons.into_iter().map(|reason| format!("{reason}, then look again.")).collect()
}

/// Said on a provider's details when no domain is known to be blocked: where a missing domain
/// is, or why this provider's domains are typed.
pub fn provider_missing_domain(provider: &ConnectedProvider) -> String {
    if provider.lists_domains {
        format!("A domain missing here is one this key may not change. Allow it in your {} account, then look again.", provider.name)
    } else {
        format!("{} can't list its domains, so each is added by name.", provider.name)
    }
}

/// What a busy button's work is doing, in a word, for work with several slow steps: saving a
/// site's names or who can open it, then checking that the internet reaches it. A frontend that
/// keeps a button busy until all the work it started is done shows this in place of the label,
/// so the owner can see it is still going. `None` for any other work, whose button keeps its
/// label.
pub fn busy_step(effects: &[crate::Effect]) -> Option<&'static str> {
    use crate::Effect;
    // A batch that saves and then checks spends nearly all its time checking.
    if effects.iter().any(|effect| matches!(effect, Effect::CheckReachability { .. })) {
        return Some("Checking…");
    }
    effects
        .iter()
        .any(|effect| matches!(effect, Effect::SetPublicHost { .. } | Effect::SetExposure { .. }))
        .then_some("Saving…")
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

// Home, the unlock page and the tabs

/// One of Home's cards about something that needs the owner: its heading, what it means, and
/// the words on its one button. `None` for an app's row.
pub fn home_card(snapshot: &Snapshot, item: &HomeItem) -> Option<(String, String, &'static str)> {
    Some(match item {
        HomeItem::Unlock => (
            "Your Nodo is locked".to_owned(),
            if snapshot.services.iter().any(|svc| svc.state == ServiceState::Locked) {
                "Apps' data waits for the device passphrase. They start once you unlock.".to_owned()
            } else {
                "Saved keys and secret settings, such as a domain provider's, need the device passphrase.".to_owned()
            },
            "Unlock",
        ),
        HomeItem::Failing(id) => (
            format!("{id} keeps stopping"),
            "Its log usually says why. Settings or an update can be undone from its page.".to_owned(),
            "Show log",
        ),
        HomeItem::Updates => {
            let names: Vec<String> = snapshot
                .update_rows()
                .iter()
                .filter(|u| !u.held)
                .map(|u| if u.name == SYSTEM_BASE { "the Nodo".to_owned() } else { u.name.clone() })
                .collect();
            let count = names.len();
            (
                if count == 1 { "1 update is ready".to_owned() } else { format!("{count} updates are ready") },
                format!("{}. Each keeps its settings and can be undone.", names.join(", ")),
                "Review updates",
            )
        }
        HomeItem::App(_) => return None,
    })
}

/// What the unlock page says under its heading: why the passphrase is asked, then the apps that
/// start once it is given, when there are any. `resume` is what sent the owner there, in the
/// words of `Dest::Unlock`; without it the page says when the Nodo restarted and what waits.
pub fn unlock_lines(snapshot: &Snapshot, resume: Option<&str>) -> Vec<String> {
    let overview = &snapshot.overview;
    let waiting: Vec<&str> =
        overview.unlock_targets.iter().filter(|target| target.encrypted).map(|target| target.name.as_str()).collect();
    let first = match resume {
        Some(reason) => format!("{reason} Type it to unlock the Nodo and carry on."),
        None => {
            let restarted =
                if overview.restarted.is_empty() { "The Nodo restarted.".to_owned() } else { format!("{}.", overview.restarted.trim_end_matches('.')) };
            let what = if waiting.is_empty() { "Saved keys and secret settings stay" } else { "Your apps' data is" };
            format!("{restarted} {what} locked until you type the device passphrase.")
        }
    };
    let mut lines = vec![first];
    if !waiting.is_empty() {
        lines.push(format!("{} start after you unlock.", waiting.join(", ")));
    }
    lines
}

/// The count beside a tab's name: updates for apps on Apps, for the Nodo itself on System.
/// Held-back versions are not offered, so they do not count.
pub fn tab_badge(snapshot: &Snapshot, tab: Tab) -> usize {
    let count = |nodo: bool| snapshot.updates.iter().filter(|u| !u.held && (u.name == SYSTEM_BASE) == nodo).count();
    match tab {
        Tab::Apps => count(false),
        Tab::System => count(true),
        Tab::Home | Tab::Web => 0,
    }
}

/// Where an app's page opens: its public name, or its address on the home network.
pub fn open_url(site: &Site) -> Option<String> {
    site.public_host
        .clone()
        .map(|host| format!("https://{host}"))
        .or_else(|| (!site.local_url.is_empty() && site.exposure.local).then(|| site.local_url.clone()))
}

// Updates, Get apps and settings

/// What the Nodo's own update needs, beside its version.
pub fn nodo_update_note(update: &UpdateItem) -> &'static str {
    if update.note == UpdateNote::RestartNeeded { "needs a restart" } else { "no restart needed" }
}

/// What an update's row says after its versions. The bool marks one that wants the owner's eye.
pub fn update_note(item: &UpdateItem) -> (String, bool) {
    match (&item.note, item.held) {
        (UpdateNote::Skipped, _) => ("you skipped this version".to_owned(), false),
        (_, true) => ("held back".to_owned(), false),
        (UpdateNote::Ready, _) if item.grows.is_some() => ("needs the device passphrase".to_owned(), true),
        (UpdateNote::Ready, _) => (String::new(), false),
        (UpdateNote::RestartNeeded, _) => ("needs a restart".to_owned(), false),
        (UpdateNote::NewPermission(_), _) => ("▲ asks for something new; you'll review it first".to_owned(), true),
        (UpdateNote::Blocked(reason), _) => (format!("▲ {reason}"), true),
        (UpdateNote::Withdrawal(_), _) => ("▲ replaces a withdrawn version".to_owned(), true),
    }
}

/// How putting a package back to its earlier version went, without a sign: `Good` when it is
/// back, `Working` when it stayed where it was or was refused, `Bad` when it did not come back.
pub fn rollback_result(result: &nodo_api::UpdateResult) -> (Tone, String) {
    use nodo_api::UpdateOutcome;
    match &result.outcome {
        UpdateOutcome::Updated => (Tone::Good, format!("{} is back on {}.", result.package, result.to)),
        UpdateOutcome::RolledBack { reason, recovered: true, .. } => (Tone::Working, format!("{} stayed on {}: {reason}", result.package, result.from)),
        UpdateOutcome::RolledBack { reason, .. } => (Tone::Bad, format!("{} failed and did not come back: {reason}", result.package)),
        UpdateOutcome::Refused(reason) | UpdateOutcome::Postponed(reason) => {
            (Tone::Working, format!("{} was not rolled back, nothing changed: {reason}", result.package))
        }
    }
}

/// What an update that was rolled back said, in a line: the app's own last words when it wrote
/// any, else why the device rolled it back. The reason is the device's and is never quoted as the
/// app's. `None` when neither is known.
pub fn update_failure_said(failure: &crate::model::UpdateFailure) -> Option<String> {
    if !failure.log_line.is_empty() {
        Some(format!("{} said: \"{}\"", failure.name, failure.log_line))
    } else if !failure.reason.is_empty() {
        Some(format!("Why: {}", failure.reason))
    } else {
        None
    }
}

/// One update of a batch, as its results list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateResultRow {
    pub applied: bool,
    /// The name and the version it is on now.
    pub text: String,
    /// What was said about it. For one that was not applied, why, after "Not updated:".
    pub note: String,
}

/// What a batch of updates that rolled nothing back came to (`UpdateReport::failure` is `None`).
///
/// Without a rollback, an update that did not apply was refused before anything changed: nothing
/// was put back, nothing is held, and the reason is the device's. So a refusal is its own
/// outcome: it is never said as a failure of the app, with no restore, no words of the app and no
/// report for a publisher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateResults {
    /// `Working` when nothing was installed and something was refused, which is not an error but
    /// needs the owner; `Good` otherwise.
    pub tone: Tone,
    pub headline: String,
    /// Each update of the batch. Empty when the one update asked for was refused: the headline
    /// and `unchanged` say all of it.
    pub rows: Vec<UpdateResultRow>,
    /// That nothing was changed for what was refused. `None` when nothing was refused.
    pub unchanged: Option<String>,
    /// What to do about a refusal. `None` when nothing was refused.
    pub advice: Option<&'static str>,
}

/// See [`UpdateResults`].
pub fn update_results(report: &crate::model::UpdateReport) -> UpdateResults {
    use crate::model::SYSTEM_BASE;
    let applied = report.results.iter().filter(|r| r.applied).count();
    let refused: Vec<_> = report.results.iter().filter(|r| !r.applied).collect();
    let advice = (!refused.is_empty()).then_some("Once what it says is sorted out, select the update again in Updates.");
    if let ([only], 0) = (refused.as_slice(), applied) {
        let name = if only.name == SYSTEM_BASE { "The Nodo" } else { only.name.as_str() };
        let stays = if only.version.is_empty() { format!("{name} stays as it was.") } else { format!("{name} is {}.", only.version) };
        return UpdateResults {
            tone: Tone::Working,
            headline: format!("Not updated: {}", only.note),
            rows: Vec::new(),
            unchanged: Some(format!("{stays} Nothing was changed.")),
            advice,
        };
    }
    let headline = if applied == 0 && !refused.is_empty() {
        "Nothing was updated".to_owned()
    } else if applied == 1 {
        "1 update is installed".to_owned()
    } else {
        format!("{applied} updates are installed")
    };
    let rows = report
        .results
        .iter()
        .map(|result| UpdateResultRow {
            applied: result.applied,
            text: format!("{} {}", result.name, result.version),
            note: match (result.applied, result.note.is_empty()) {
                (_, true) => String::new(),
                (true, false) => result.note.clone(),
                (false, false) => format!("Not updated: {}", result.note),
            },
        })
        .collect();
    UpdateResults {
        tone: if refused.is_empty() || applied > 0 { Tone::Good } else { Tone::Working },
        headline,
        rows,
        unchanged: (!refused.is_empty()).then(|| "Nothing was changed for those not updated.".to_owned()),
        advice,
    }
}

/// An app on offer, in a word: installed already, or with an update. Empty for one that is
/// neither.
pub fn status_word(status: PackageStatus) -> &'static str {
    match status {
        PackageStatus::Installed => "Installed",
        PackageStatus::UpdateReady => "Update ready",
        PackageStatus::Available => "",
    }
}

/// A reference as a field shows it: `app·setting`.
pub fn chip_label(app: &str, setting: &str) -> String {
    format!("{app}·{setting}")
}

/// What the reference picker says above its list, when some of it does not fit the field.
pub fn picker_hint(kind: &FieldKind) -> &'static str {
    match kind {
        FieldKind::Number { .. } | FieldKind::Port => "Numbers first; the others are dimmed because a number field can't hold them.",
        _ => "Those that fit first; the others are dimmed with why.",
    }
}

// The web GUI

/// The web GUI in a few words, for the row that leads to it: off, on with its address, or on
/// and waiting for the device's address on the home network.
pub fn web_gui_summary(status: &nodo_api::WebGuiStatus) -> String {
    match (&status.url, status.enabled) {
        (_, false) => "Off".to_owned(),
        (Some(url), true) => format!("On · {url}"),
        (None, true) => "On · waiting for the local address".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_line_is_load_over_cores_capped_with_the_temperature() {
        let cpu = |load_average_milli, cores, temperature_millicelsius| nodo_api::CpuStatus { load_average_milli, cores, temperature_millicelsius };
        assert_eq!(cpu_summary(&cpu(1840, 8, Some(52_400))), "23% · 8 cores · 52 °C");
        assert_eq!(cpu_busy_permille(&cpu(1840, 8, None)), 230);
        assert_eq!(cpu_summary(&cpu(3000, 1, None)), "100% · 1 core");
        assert_eq!(cpu_busy_permille(&cpu(3000, 1, None)), 1000);
        // A board that says no cores still divides by one.
        assert_eq!(cpu_busy_permille(&cpu(500, 0, Some(-1_600))), 500);
        assert_eq!(cpu_summary(&cpu(0, 4, Some(-1_600))), "0% · 4 cores · -2 °C");
    }
    use crate::exec::load_snapshot;
    use crate::mock::MockBackend;

    #[test]
    fn home_the_unlock_page_and_the_tabs_say_what_waits() {
        let snapshot = load_snapshot(&mut MockBackend::new()).expect("the mock answers");
        // The mock starts locked, with updates for apps and for the Nodo itself.
        let (head, body, label) = home_card(&snapshot, &HomeItem::Unlock).expect("a card");
        assert_eq!((head.as_str(), label), ("Your Nodo is locked", "Unlock"));
        assert!(body.starts_with("Apps' data waits"), "{body}");
        assert_eq!(home_card(&snapshot, &HomeItem::App("tor".into())), None);
        let (head, body, _) = home_card(&snapshot, &HomeItem::Updates).expect("a card");
        assert!(head.ends_with("updates are ready") && body.contains("the Nodo"), "{head}: {body}");

        let lines = unlock_lines(&snapshot, None);
        assert!(lines[0].ends_with("locked until you type the device passphrase."), "{lines:?}");
        let resumed = unlock_lines(&snapshot, Some("Connecting Porkbun stores its key locked with the device passphrase."));
        assert_eq!(resumed[0], "Connecting Porkbun stores its key locked with the device passphrase. Type it to unlock the Nodo and carry on.");
        assert_eq!(lines.len(), resumed.len(), "the apps that wait are named either way");

        let ready = snapshot.updates.iter().filter(|u| !u.held).count();
        assert!(tab_badge(&snapshot, Tab::System) == 1 && tab_badge(&snapshot, Tab::Apps) == ready - 1, "{ready}");
        assert_eq!(tab_badge(&snapshot, Tab::Home) + tab_badge(&snapshot, Tab::Web), 0);
    }

    #[test]
    fn an_updates_row_and_a_rollback_say_how_they_stand() {
        let snapshot = load_snapshot(&mut MockBackend::new()).expect("the mock answers");
        let mut item = snapshot.updates[0].clone();
        item.note = UpdateNote::Ready;
        item.held = false;
        item.grows = None;
        assert_eq!(update_note(&item), (String::new(), false));
        item.grows = Some("256 MB to 512 MB".into());
        assert_eq!(update_note(&item), ("needs the device passphrase".to_owned(), true));
        item.held = true;
        assert_eq!(update_note(&item), ("held back".to_owned(), false));
        item.note = UpdateNote::RestartNeeded;
        assert_eq!(nodo_update_note(&item), "needs a restart");

        let mut result =
            nodo_api::UpdateResult { package: "tor".into(), from: "2".into(), to: "1".into(), outcome: nodo_api::UpdateOutcome::Updated, log: Vec::new() };
        assert_eq!(rollback_result(&result), (Tone::Good, "tor is back on 1.".to_owned()));
        result.outcome = nodo_api::UpdateOutcome::Refused("it is in use".into());
        assert_eq!(rollback_result(&result), (Tone::Working, "tor was not rolled back, nothing changed: it is in use".to_owned()));

        assert_eq!(status_word(PackageStatus::UpdateReady), "Update ready");
        assert_eq!(chip_label("cuprated", "rpc_port"), "cuprated·rpc_port");
        assert!(picker_hint(&FieldKind::Port).starts_with("Numbers first"));
        let on = nodo_api::WebGuiStatus { enabled: true, url: Some("https://nodo.local".into()), waiting: false, open: false };
        assert_eq!(web_gui_summary(&on), "On · https://nodo.local");
        assert_eq!(web_gui_summary(&nodo_api::WebGuiStatus { url: None, waiting: true, ..on.clone() }), "On · waiting for the local address");
        assert_eq!(web_gui_summary(&nodo_api::WebGuiStatus { enabled: false, url: None, ..on }), "Off");
    }

    fn result(name: &str, applied: bool, version: &str, note: &str) -> crate::model::UpdateResult {
        crate::model::UpdateResult { name: name.into(), applied, version: version.into(), note: note.into() }
    }

    #[test]
    fn an_update_refused_before_anything_changed_is_its_own_outcome() {
        use crate::model::UpdateReport;
        // The one update asked for was refused: the reason is the headline, and nothing is listed.
        let alone = UpdateReport { results: vec![result("moneropay", false, "still on 2.7.0", "Free up 2.3 GB on the data drive, then try again.")], failure: None };
        let came_to = update_results(&alone);
        assert_eq!(came_to.tone, Tone::Working, "a refusal is not an error");
        assert_eq!(came_to.headline, "Not updated: Free up 2.3 GB on the data drive, then try again.");
        assert!(came_to.rows.is_empty());
        assert_eq!(came_to.unchanged.as_deref(), Some("moneropay is still on 2.7.0. Nothing was changed."));
        assert_eq!(came_to.advice, Some("Once what it says is sorted out, select the update again in Updates."));
        let said = format!("{came_to:?}");
        for wrong in ["restored", "said", "publisher", "put back", "on hold"] {
            assert!(!said.contains(wrong), "a refusal changed nothing, so no {wrong:?}: {said}");
        }
        // The device's own snapshot has a name of its own, and a refusal may come without a version.
        let system = UpdateReport { results: vec![result(crate::model::SYSTEM_BASE, false, "", "No room.")], failure: None };
        assert_eq!(update_results(&system).unchanged.as_deref(), Some("The Nodo stays as it was. Nothing was changed."));

        // Among others: each is listed, the refused ones with why, and the rest counted.
        let mixed = UpdateReport {
            results: vec![result("tor", true, "0.4.9", ""), result("bell", true, "1.2", "restarted"), result("moneropay", false, "still on 2.7.0", "No room.")],
            failure: None,
        };
        let came_to = update_results(&mixed);
        assert_eq!((came_to.tone, came_to.headline.as_str()), (Tone::Good, "2 updates are installed"));
        let rows: Vec<(bool, &str, &str)> = came_to.rows.iter().map(|row| (row.applied, row.text.as_str(), row.note.as_str())).collect();
        assert_eq!(rows, [(true, "tor 0.4.9", ""), (true, "bell 1.2", "restarted"), (false, "moneropay still on 2.7.0", "Not updated: No room.")]);
        assert_eq!(came_to.unchanged.as_deref(), Some("Nothing was changed for those not updated."));
        assert!(came_to.advice.is_some());

        // Several, all refused.
        let none = UpdateReport { results: vec![result("tor", false, "still on 1", "No room."), result("bell", false, "still on 2", "No room.")], failure: None };
        let came_to = update_results(&none);
        assert_eq!((came_to.tone, came_to.headline.as_str()), (Tone::Working, "Nothing was updated"));
        assert_eq!(came_to.rows.len(), 2);

        // Nothing refused: the count alone, and nothing to do.
        let fine = UpdateReport { results: vec![result("tor", true, "0.4.9", "")], failure: None };
        let came_to = update_results(&fine);
        assert_eq!((came_to.tone, came_to.headline.as_str()), (Tone::Good, "1 update is installed"));
        assert_eq!((came_to.unchanged, came_to.advice), (None, None));
    }

    #[test]
    fn a_rolled_back_update_quotes_the_app_or_gives_the_devices_reason_never_one_as_the_other() {
        let mut failure = crate::model::UpdateFailure {
            name: "moneropay".into(),
            headline: String::new(),
            kept_version: "2.7.0".into(),
            restored: String::new(),
            running: true,
            log_line: "database migration 14 failed".into(),
            reason: "moneropay did not pass its health check".into(),
            verdict: String::new(),
        };
        assert_eq!(update_failure_said(&failure).as_deref(), Some("moneropay said: \"database migration 14 failed\""));
        failure.log_line.clear();
        assert_eq!(update_failure_said(&failure).as_deref(), Some("Why: moneropay did not pass its health check"));
        failure.reason.clear();
        assert_eq!(update_failure_said(&failure), None);
    }

    #[test]
    fn a_providers_details_are_worded_once_for_every_interface() {
        use crate::model::ConnectedDomain;
        let domain = |used_by: &[&str], held_by: Option<&str>| ConnectedDomain {
            name: "example.com".into(),
            used_by: used_by.iter().map(|s| (*s).to_owned()).collect(),
            held_by: held_by.map(str::to_owned),
        };
        assert_eq!(provider_domain_note(&domain(&[], None)), "not used by an app yet");
        assert_eq!(provider_domain_note(&domain(&["shop", "pay"], None)), "used by shop, pay");
        assert_eq!(provider_domain_note(&domain(&["shop"], Some("Porkbun"))), "used through Porkbun, which was connected first");

        let off = "API Access is off for this domain. Switch it on";
        let blocked = vec![("example.net".to_owned(), off.to_owned()), ("example.org".to_owned(), off.to_owned()), ("b.example".to_owned(), "Unlock the domain".to_owned())];
        assert_eq!(provider_blocked_line(&blocked), "Can't be used yet: example.net, example.org, b.example");
        assert_eq!(
            provider_blocked_remedies(&blocked),
            [format!("{off}, then look again."), "Unlock the domain, then look again.".to_owned()],
            "each reason once, in the order met"
        );

        let mut provider = ConnectedProvider { name: "Porkbun".into(), lists_domains: true, ..ConnectedProvider::default() };
        assert_eq!(provider_checked(&provider), "Not since it was connected");
        assert!(provider_missing_domain(&provider).contains("Allow it in your Porkbun account"));
        provider.checked = Some("5 minutes ago".into());
        provider.lists_domains = false;
        assert_eq!(provider_checked(&provider), "5 minutes ago");
        assert_eq!(provider_missing_domain(&provider), "Porkbun can't list its domains, so each is added by name.");
    }

    #[test]
    fn a_busy_button_names_the_slow_step_its_work_is_on() {
        use crate::effect::ExposureAfter;
        use crate::Effect;
        let check = Effect::CheckReachability { site: "shop".into(), going_public: true };
        let exposure = Effect::SetExposure { site: "shop".into(), exposure: nodo_api::Exposure::default(), after: ExposureAfter::Stay, confirmation: None };
        let names = Effect::SetPublicHost { site: "shop".into(), host: "shop.example.com".into(), aliases: Vec::new(), confirm_added: Vec::new(), then: vec![check.clone()] };
        // Saving the names is its own step even with the checks waiting behind it.
        assert_eq!(busy_step(&[names]), Some("Saving…"));
        assert_eq!(busy_step(std::slice::from_ref(&exposure)), Some("Saving…"));
        // A batch that saves and then checks spends nearly all its time checking.
        assert_eq!(busy_step(&[exposure, check]), Some("Checking…"));
        assert_eq!(busy_step(&[Effect::Refresh, Effect::CheckUpdates]), None, "any other button keeps its label");
        assert_eq!(busy_step(&[]), None);
    }

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
