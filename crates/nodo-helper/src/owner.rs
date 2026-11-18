//! Owner pages: a package's setup checklist, fetched from its own service on loopback, checked,
//! and filled in with what the helper knows itself (sites, services, domains, backups).
//!
//! The package's JSON is untrusted. It is read with a size and time limit, parsed strictly
//! (`nodo_manifest::owner_page`), checked for lengths and control characters, and only then are
//! the device's own values put into its text. A package that does not answer gets a page that
//! says so, never an opaque error.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::Duration;

use nodo_api::{
    auto_state, ApiError, CertState, CertificateReport, DnsRecord, DomainProvider, Exposure,
    OwnerCheck, OwnerCheckState, OwnerItem, OwnerPageView, OwnerStep, OwnerStepState,
    ReachabilityDetail, Response, Secret, ServiceState, SiteFacts, WebStatus,
};
use nodo_manifest::owner_page::{self, CheckState, ItemDoc, PageDoc, StepState};
use serde::{Deserialize, Serialize};

use crate::catalog::Package;
use crate::exposure::{ExposureState, PublicHosts};
use crate::helper::{lock, Helper};
use crate::system::System;

/// The package may run network checks before answering.
pub(crate) const PAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// What the newest backup was, recorded when it is made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LastBackup {
    /// Unix seconds.
    pub created: u64,
    pub has_device_key: bool,
    /// On a removable drive rather than in the device's backup folder.
    pub usb: bool,
}

/// No control characters, at most `max` characters: for text from outside the helper.
fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

fn check(
    label: impl Into<String>,
    state: OwnerCheckState,
    detail: impl Into<String>,
) -> OwnerCheck {
    let detail = detail.into();
    OwnerCheck {
        label: label.into(),
        state,
        detail: (!detail.is_empty()).then_some(detail),
    }
}

/// `YYYY-MM-DD` of unix seconds, in UTC.
pub(crate) fn date(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn ago(now: u64, then: u64) -> String {
    match now.saturating_sub(then) {
        s if s < 120 => "just now".to_owned(),
        s if s < 7200 => format!("{} minutes ago", s / 60),
        s if s < 2 * 86_400 => format!("{} hours ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

/// Where a package's data directory is on the host: the source its sandbox binds as its data.
fn data_dir(package: &Package, drive: Option<&std::path::Path>) -> Option<String> {
    if package.manifest.storage.encrypted {
        return nodo_gen::volume_mount(package.id())
            .ok()
            .map(|mount| format!("{mount}/data"));
    }
    match drive {
        Some(mount) => Some(mount.join(package.id()).display().to_string()),
        None => package.layout().ok().map(|layout| layout.data_dir),
    }
}

/// Replaces the device's own values in a package's text: `{device_address}`, `{public_address}`
/// and `{data_dir:<package>}`. Any other brace is left as it is.
fn substitute(text: &str, tokens: &Tokens) -> String {
    if !text.contains('{') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let replaced = if let Some(after) = tail.strip_prefix("{device_address}") {
            Some((tokens.device_address.clone(), after))
        } else if let Some(after) = tail.strip_prefix("{public_address}") {
            Some((tokens.public_address.clone(), after))
        } else if let Some(inner) = tail.strip_prefix("{data_dir:") {
            inner.find('}').and_then(|end| {
                let id = &inner[..end];
                nodo_manifest::is_valid_id(id).then(|| {
                    let path = tokens
                        .data_dirs
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_owned());
                    (path, &inner[end + 1..])
                })
            })
        } else {
            None
        };
        match replaced {
            Some((value, after)) => {
                out.push_str(&value);
                rest = after;
            }
            None => {
                out.push('{');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The values a package's text may ask for.
struct Tokens {
    device_address: String,
    public_address: String,
    /// Installed packages' data directories, by id.
    data_dirs: BTreeMap<String, String>,
}

fn substitute_doc(doc: &mut PageDoc, tokens: &Tokens) {
    for step in &mut doc.steps {
        for item in &mut step.items {
            match item {
                ItemDoc::Text { text } => *text = substitute(text, tokens),
                ItemDoc::Value { label, value } => {
                    *label = substitute(label, tokens);
                    *value = substitute(value, tokens);
                }
                _ => {}
            }
        }
    }
}

/// What the helper knows, gathered once per page. Each part may be missing, which the items it
/// feeds say rather than fail the page.
struct Facts {
    packages: Vec<Package>,
    exposure: ExposureState,
    hosts: PublicHosts,
    web: Result<WebStatus, String>,
    certificates: Result<CertificateReport, String>,
    providers: Result<Vec<DomainProvider>, String>,
    reachability: Option<(u64, ReachabilityDetail)>,
    now: u64,
}

impl Facts {
    fn package(&self, id: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.id() == id)
    }

    /// The public address: what dynamic DNS last saw, or what the last reachability check found.
    fn public_ip(&self) -> Option<String> {
        self.reachability
            .as_ref()
            .and_then(|(_, r)| r.public_ip.clone())
            .or_else(|| self.web.as_ref().ok().and_then(|w| w.public_ip.clone()))
    }

    /// The connected provider and domain a host is under: the longest domain wins.
    fn provider_of(&self, host: &str) -> Option<(String, String)> {
        let providers = self.providers.as_ref().ok()?;
        providers
            .iter()
            .flat_map(|p| {
                p.domains
                    .iter()
                    .filter(|d| d.held_by.is_none())
                    .map(move |d| (p.name.clone(), d.name.clone()))
            })
            .filter(|(_, d)| host == d || host.ends_with(&format!(".{d}")))
            .max_by_key(|(_, d)| d.len())
    }
}

impl<S: System> Helper<S> {
    pub(crate) fn remember_reachability(&self, detail: &ReachabilityDetail) {
        *lock(&self.last_reachability) = Some((self.manage.packages.now(), detail.clone()));
    }

    /// Records the backup just made. A failure to record is not the backup's failure.
    pub(crate) fn record_backup(&self, created: u64, has_device_key: bool, usb: bool) {
        let record = LastBackup {
            created,
            has_device_key,
            usb,
        };
        if let Err(e) = self.save_state(&self.manage.paths.last_backup, &record) {
            eprintln!("nodo-helper: could not record the backup for owner pages: {e}");
        }
    }

    pub(crate) fn owner_page(&self, id: &str, passphrase: &Secret) -> Result<Response, ApiError> {
        let package = self.package(id)?;
        {
            let mut failures = lock(&self.failed_unlocks);
            let file = self.read_keystore()?;
            self.verify_passphrase(&mut failures, passphrase, &file)?;
        }
        let page = package
            .manifest
            .owner_page
            .clone()
            .ok_or_else(|| ApiError::Failed(format!("{id} has no setup page")))?;
        let (port, path) = page
            .target()
            .ok_or_else(|| ApiError::Failed(format!("{id}'s setup page address is invalid")))?;
        let facts = self.owner_facts()?;
        let fetched = self
            .sys
            .http_get_page(port, &path, owner_page::MAX_BODY_BYTES, PAGE_TIMEOUT);
        let view = match fetched {
            Err(e) => self.not_answering(id, &page.title, &clean(&e.0, 200), &facts),
            Ok(body) => match self.read_page(&body, &facts) {
                Ok(doc) => self.fill(id, doc, &facts),
                Err(problem) => self.broken_page(id, &page.title, &problem, &facts),
            },
        };
        Ok(Response::OwnerPage(view))
    }

    fn owner_facts(&self) -> Result<Facts, ApiError> {
        let (packages, _) = self.packages()?;
        let (exposure, _): (ExposureState, _) = self.load_toml(&self.paths.exposure_state)?;
        let (hosts, _): (PublicHosts, _) = self.load_toml(&self.paths.public_hosts)?;
        let input = self.current_web_input();
        let web = match &input {
            Ok(i) => self.exposure.status(i).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        let certificates = match &input {
            Ok(i) => self.exposure.certificates(i).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        let providers = match &input {
            Ok(i) => self.exposure.domain_providers(i).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        Ok(Facts {
            packages,
            exposure,
            hosts,
            web,
            certificates,
            providers,
            reachability: lock(&self.last_reachability).clone(),
            now: self.manage.packages.now(),
        })
    }

    /// Parses and checks the package's answer, then puts in the device's own values and checks
    /// it again, since a value can make a field too long.
    fn read_page(&self, body: &[u8], facts: &Facts) -> Result<PageDoc, String> {
        let mut doc: PageDoc =
            serde_json::from_slice(body).map_err(|e| clean(&e.to_string(), 300))?;
        doc.check()?;
        let drive = self.paths.drive.as_ref().map(|d| d.mount_point.as_path());
        let bindings = self.load_bindings().map_err(|e| e.to_string())?;
        let data_dirs = facts
            .packages
            .iter()
            .filter_map(|p| {
                let on_drive = drive.filter(|_| bindings.data_drive.contains(p.id()));
                data_dir(p, on_drive).map(|dir| (p.id().to_owned(), dir))
            })
            .collect();
        let device_address = facts
            .reachability
            .as_ref()
            .and_then(|(_, r)| r.device_ip.clone())
            .or_else(|| self.exposure.device_address())
            .unwrap_or_else(|| "unknown".to_owned());
        let tokens = Tokens {
            device_address,
            public_address: facts.public_ip().unwrap_or_else(|| "unknown".to_owned()),
            data_dirs,
        };
        substitute_doc(&mut doc, &tokens);
        doc.check()
            .map_err(|e| format!("after putting in this Nodo's values: {e}"))?;
        Ok(doc)
    }

    fn fill(&self, id: &str, doc: PageDoc, facts: &Facts) -> OwnerPageView {
        let steps = doc
            .steps
            .into_iter()
            .map(|step| {
                let items: Vec<OwnerItem> = step
                    .items
                    .into_iter()
                    .map(|item| self.item(id, item, facts))
                    .collect();
                let state = match step.state {
                    StepState::Done => OwnerStepState::Done,
                    StepState::Todo => OwnerStepState::Todo,
                    StepState::Problem => OwnerStepState::Problem,
                    StepState::Unknown => OwnerStepState::Unknown,
                    StepState::Auto => auto_state(&items),
                };
                OwnerStep {
                    title: step.title,
                    state,
                    items,
                }
            })
            .collect();
        OwnerPageView {
            package: id.to_owned(),
            title: doc.title,
            intro: doc.intro,
            steps,
        }
    }

    fn item(&self, owner: &str, item: ItemDoc, facts: &Facts) -> OwnerItem {
        match item {
            ItemDoc::Text { text } => OwnerItem::Text(text),
            ItemDoc::Value { label, value } => OwnerItem::Value { label, value },
            ItemDoc::Check {
                label,
                state,
                detail,
            } => OwnerItem::Check(OwnerCheck {
                label,
                state: match state {
                    CheckState::Ok => OwnerCheckState::Ok,
                    CheckState::Failed => OwnerCheckState::Failed,
                    CheckState::Waiting => OwnerCheckState::Waiting,
                    CheckState::Unknown => OwnerCheckState::Unknown,
                },
                detail,
            }),
            ItemDoc::Qr { label, data } => OwnerItem::Qr { label, data },
            ItemDoc::Site { package, site } => match facts.package(&package) {
                None => OwnerItem::Check(check(
                    format!("{package} · {site}"),
                    OwnerCheckState::Failed,
                    format!("{package} is not installed"),
                )),
                Some(p) => match p.manifest.web.iter().find(|w| w.name == site) {
                    None => OwnerItem::Check(check(
                        format!("{package} · {site}"),
                        OwnerCheckState::Failed,
                        format!("{package} has no site named {site}"),
                    )),
                    Some(web) => OwnerItem::Site(self.site_facts(
                        &package,
                        &site,
                        web.suggested_subdomain.as_deref(),
                        facts,
                    )),
                },
            },
            ItemDoc::Service { package } => OwnerItem::Service {
                check: self.service_check(&package, facts),
                package,
            },
            ItemDoc::Domain { domain } => {
                let (provider, check) = domain_check(&domain, facts);
                OwnerItem::Domain {
                    domain,
                    provider,
                    check,
                }
            }
            ItemDoc::Backup {} => OwnerItem::Backup {
                check: self.backup_check(owner, facts),
            },
        }
    }

    fn site_facts(
        &self,
        package: &str,
        site: &str,
        suggested_sub: Option<&str>,
        facts: &Facts,
    ) -> SiteFacts {
        let chosen = facts.hosts.get(package).and_then(|s| s.get(site)).map(|e| e.host().to_owned());
        let exposure: Exposure = facts
            .exposure
            .get(package)
            .and_then(|s| s.get(site))
            .copied()
            .unwrap_or_default();
        let first_domain = facts.providers.as_ref().ok().and_then(|p| {
            p.iter()
                .flat_map(|p| &p.domains)
                .find(|d| d.held_by.is_none())
                .map(|d| d.name.clone())
        });
        let suggested = suggested_sub.map(|sub| match &first_domain {
            Some(domain) => format!("{sub}.{domain}"),
            None => sub.to_owned(),
        });
        let host = chosen.clone().or(suggested);
        let hosting = format!("Apps › {package} › Hosting");
        let ip = facts.public_ip();
        let provider = chosen.as_deref().and_then(|h| facts.provider_of(h));
        let mut checks = Vec::new();
        checks.push(match &chosen {
            Some(h) => check("Internet address chosen", OwnerCheckState::Ok, h.clone()),
            None => check(
                "Internet address chosen",
                OwnerCheckState::Waiting,
                format!("Choose it in {hosting}."),
            ),
        });
        checks.push(if exposure.public {
            check("Online on the internet", OwnerCheckState::Ok, "")
        } else {
            check(
                "Online on the internet",
                OwnerCheckState::Waiting,
                format!("Tick Internet in {hosting}."),
            )
        });
        // This Nodo serves the internet over IPv4 only (dynamic DNS publishes no AAAA), so the
        // record is an A record.
        let mut records = Vec::new();
        if let (Some(h), Some(ip)) = (&chosen, &ip) {
            let zone = provider
                .as_ref()
                .map(|(_, d)| d.clone())
                .unwrap_or_else(|| guess_zone(h));
            let name = if *h == zone {
                "@".to_owned()
            } else {
                h.strip_suffix(&format!(".{zone}")).unwrap_or(h).to_owned()
            };
            records.push(DnsRecord {
                kind: "A".to_owned(),
                name,
                zone,
                value: ip.clone(),
            });
        }
        checks.push(match (&chosen, &ip) {
            (None, _) => check(
                "Name points here",
                OwnerCheckState::Waiting,
                "No internet address yet.",
            ),
            (Some(_), None) => check(
                "Name points here",
                OwnerCheckState::Unknown,
                format!("This Nodo's public address is not known yet. Run the check in {hosting}."),
            ),
            (Some(h), Some(ip)) => {
                let found: Vec<IpAddr> = self.sys.resolve_host(h);
                let wanted: Option<IpAddr> = ip.parse().ok();
                if found.is_empty() {
                    check(
                        "Name points here",
                        OwnerCheckState::Waiting,
                        "No answer yet. A new record can take a while to show.",
                    )
                } else if wanted.is_some_and(|w| found.contains(&w)) {
                    check(
                        "Name points here",
                        OwnerCheckState::Ok,
                        format!("{h} resolves to {ip}"),
                    )
                } else {
                    let list: Vec<String> = found.iter().map(ToString::to_string).collect();
                    check(
                        "Name points here",
                        OwnerCheckState::Failed,
                        format!("{h} resolves to {}, not {ip}", list.join(", ")),
                    )
                }
            }
        });
        checks.push(match (&chosen, &facts.certificates) {
            (None, _) => check("Certificate", OwnerCheckState::Waiting, "None yet."),
            (Some(_), Err(e)) => check("Certificate", OwnerCheckState::Unknown, clean(e, 200)),
            (Some(h), Ok(report)) => match report.hosts.iter().find(|c| &c.host == h) {
                None => check("Certificate", OwnerCheckState::Waiting, "None yet."),
                Some(cert) => match cert.state {
                    CertState::Valid => check(
                        "Certificate",
                        OwnerCheckState::Ok,
                        cert.not_after
                            .map(|t| format!("Valid until {}", date(t)))
                            .unwrap_or_default(),
                    ),
                    CertState::Failed | CertState::Expired => check(
                        "Certificate",
                        OwnerCheckState::Failed,
                        cert.last_error
                            .as_ref()
                            .map(|e| clean(&e.summary, 200))
                            .unwrap_or_else(|| "It expired.".to_owned()),
                    ),
                    CertState::Obtaining => check(
                        "Certificate",
                        OwnerCheckState::Waiting,
                        "Being obtained now.",
                    ),
                    CertState::Waiting => {
                        check("Certificate", OwnerCheckState::Waiting, "Not tried yet.")
                    }
                },
            },
        });
        checks.push(reachability_check(facts, &hosting));
        SiteFacts {
            package: package.to_owned(),
            site: site.to_owned(),
            suggested: chosen.is_none() && host.is_some(),
            host,
            public: exposure.public,
            records,
            automatic_dns: provider.map(|(name, _)| name),
            checks,
        }
    }

    fn service_check(&self, id: &str, facts: &Facts) -> OwnerCheck {
        let label = format!("{id} is running");
        let Some(package) = facts.package(id) else {
            return check(
                label,
                OwnerCheckState::Failed,
                format!("{id} is not installed."),
            );
        };
        let (state, detail, _) = self.state_of(package);
        match state {
            ServiceState::Running => match package
                .manifest
                .health
                .as_ref()
                .map(crate::health::probe_for)
            {
                Some(Ok(probe)) if !self.sys.probe(&probe) => check(
                    label,
                    OwnerCheckState::Failed,
                    format!("It runs, but its health check does not pass. See Apps › {id} › Logs."),
                ),
                _ => check(label, OwnerCheckState::Ok, ""),
            },
            ServiceState::Stopped => check(
                label,
                OwnerCheckState::Failed,
                format!("It is stopped. Start it on Apps › {id}."),
            ),
            ServiceState::Failed => check(
                label,
                OwnerCheckState::Failed,
                format!("It keeps stopping. See Apps › {id} › Logs."),
            ),
            ServiceState::Locked => check(
                label,
                OwnerCheckState::Waiting,
                "It waits for the Nodo to be unlocked.",
            ),
            ServiceState::Waiting => check(label, OwnerCheckState::Waiting, clean(&detail, 200)),
        }
    }

    fn backup_check(&self, owner: &str, facts: &Facts) -> OwnerCheck {
        const LABEL: &str = "Backup with the device key";
        let record: Option<LastBackup> = self
            .sys
            .read_file(&self.manage.paths.last_backup)
            .ok()
            .flatten()
            .and_then(|bytes| toml::from_str(std::str::from_utf8(&bytes).ok()?).ok());
        let installed_at = self
            .read_feeds()
            .ok()
            .and_then(|s| s.installed.get(owner).and_then(|r| r.installed_at));
        let make =
            "Make one in System › Backups, with the device passphrase so it holds the device key.";
        match record {
            Some(b) => {
                let place = if b.usb {
                    "on a USB drive"
                } else {
                    "on this Nodo"
                };
                if !b.has_device_key {
                    check(
                        LABEL,
                        OwnerCheckState::Waiting,
                        format!(
                            "The newest backup ({}) does not hold the device key. {make}",
                            date(b.created)
                        ),
                    )
                } else if installed_at.is_some_and(|at| b.created < at) {
                    check(
                        LABEL,
                        OwnerCheckState::Waiting,
                        format!(
                            "The newest backup ({}) is from before {owner} was installed. {make}",
                            date(b.created)
                        ),
                    )
                } else {
                    check(
                        LABEL,
                        OwnerCheckState::Ok,
                        format!(
                            "Made {} {place}, {}",
                            date(b.created),
                            ago(facts.now, b.created)
                        ),
                    )
                }
            }
            None => {
                let newest = match self.list_backups() {
                    Ok(Response::Backups(list)) => list.first().map(|b| b.created),
                    _ => None,
                };
                match newest {
                    Some(created) => check(
                        LABEL,
                        OwnerCheckState::Waiting,
                        format!("A backup from {} is here, but whether it holds the device key was not recorded. {make}", date(created)),
                    ),
                    None => check(LABEL, OwnerCheckState::Waiting, format!("No backup yet. {make}")),
                }
            }
        }
    }

    fn not_answering(&self, id: &str, title: &str, why: &str, facts: &Facts) -> OwnerPageView {
        let items = vec![
            OwnerItem::Text(format!(
                "{id} did not answer, so its setup steps cannot be shown. It may not be running, or still be starting. \
                 Its log says why: see Apps › {id} › Logs."
            )),
            OwnerItem::Service { package: id.to_owned(), check: self.service_check(id, facts) },
            OwnerItem::Check(check("Its setup page answers", OwnerCheckState::Failed, why.to_owned())),
        ];
        problem_view(id, title, format!("{id} is not answering"), items)
    }

    fn broken_page(&self, id: &str, title: &str, problem: &str, _facts: &Facts) -> OwnerPageView {
        let items = vec![
            OwnerItem::Text(format!(
                "{id} answered, but not with a setup page this Nodo can show. Tell its publisher; an update may fix it."
            )),
            OwnerItem::Check(check("Its setup page is valid", OwnerCheckState::Failed, clean(problem, 300))),
        ];
        problem_view(
            id,
            title,
            format!("{id}'s setup page cannot be shown"),
            items,
        )
    }
}

fn problem_view(id: &str, title: &str, step: String, items: Vec<OwnerItem>) -> OwnerPageView {
    OwnerPageView {
        package: id.to_owned(),
        title: title.to_owned(),
        intro: None,
        steps: vec![OwnerStep {
            title: step,
            state: OwnerStepState::Problem,
            items,
        }],
    }
}

/// The zone a host is probably under when no connected provider holds it: its last two labels.
fn guess_zone(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    labels[labels.len().saturating_sub(2)..].join(".")
}

fn domain_check(domain: &str, facts: &Facts) -> (Option<String>, OwnerCheck) {
    let label = format!("{domain} is connected");
    match &facts.providers {
        Err(e) => (None, check(label, OwnerCheckState::Unknown, clean(e, 200))),
        Ok(providers) => {
            let holder = providers.iter().find(|p| {
                p.domains
                    .iter()
                    .any(|d| d.name == domain && d.held_by.is_none())
            });
            match holder {
                Some(p) => (
                    Some(p.name.clone()),
                    check(label, OwnerCheckState::Ok, format!("Through {}: its records and certificates are automatic.", p.name)),
                ),
                None => (
                    None,
                    check(
                        label,
                        OwnerCheckState::Waiting,
                        format!("No domain provider holds {domain}. Connect one in Web › Domain providers."),
                    ),
                ),
            }
        }
    }
}

/// The router check, from the last reachability check the owner ran; running it again here
/// would take too long, so the page says how old it is.
fn reachability_check(facts: &Facts, hosting: &str) -> OwnerCheck {
    const LABEL: &str = "Router lets the internet in";
    let Some((at, detail)) = &facts.reachability else {
        return check(
            LABEL,
            OwnerCheckState::Unknown,
            format!("Not checked since the Nodo started. Run the check in {hosting}."),
        );
    };
    let when = format!("From the last check, {}.", ago(facts.now, *at));
    match detail.verdict.as_str() {
        "ready" => check(LABEL, OwnerCheckState::Ok, when),
        "ports_unverified" => check(LABEL, OwnerCheckState::Unknown, format!("Nothing outside could test the ports. {when}")),
        "needs_port_forward" => {
            check(LABEL, OwnerCheckState::Failed, format!("Ports 80 and 443 need forwarding to this Nodo. {when}"))
        }
        "blocked_by_cgnat" => check(
            LABEL,
            OwnerCheckState::Failed,
            format!("Your provider shares one public address between customers, so nothing can reach in. {when}"),
        ),
        _ => check(LABEL, OwnerCheckState::Failed, format!("The check failed. {when}")),
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
