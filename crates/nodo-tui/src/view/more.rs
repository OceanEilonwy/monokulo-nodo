//! The Onion, Restore and Rollbacks screens, drawn from `docs/design/tui/Onion.dc.html`,
//! `Restore.dc.html` and `Rollbacks.dc.html`: a breadcrumb header, bordered panels, the safe
//! choice first, typed confirmation before anything is overwritten, and key hints in the footer.

use nodo_api::{UpdateEventKind, UpdateOutcome};
use ratatui::text::Line;

use super::layout::{Item, Page, Panel};
use super::{checkbox, hints, mark, pad};
use crate::app::{App, OnionRow, OnionState, RestoreState, RestoreStep, RollbacksState, RESTORE_WORD, TICKS_PER_SECOND};
use crate::theme::Theme;

pub(super) fn crumbs(t: &Theme, parts: &[&str]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index + 1 == parts.len() {
            spans.push(t.b((*part).to_owned()));
        } else {
            spans.push(t.n(format!("{part} ")));
            spans.push(t.d("› "));
        }
    }
    Line::from(spans)
}

pub(super) fn error_items(t: &Theme, error: &Option<String>) -> Vec<Item> {
    error.iter().map(|e| Item::Text(Line::from(t.e(format!("✕ {e}"))))).collect()
}

pub(super) fn bullet(t: &Theme, text: &str) -> Item {
    Item::Text(Line::from(vec![t.i("●"), t.n(format!(" {text}"))]))
}

pub(super) fn row(selected: bool, left: Line<'static>, right: Line<'static>) -> Item {
    if selected {
        Item::Selected(left, right)
    } else {
        Item::Row(left, right)
    }
}

fn size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.1} MB", bytes as f64 / 1e6)
    }
}

// Onion

pub fn onion(t: &Theme, state: &OnionState) -> Page {
    let header = crumbs(t, &["Web", "Onion addresses"]);
    let Some(status) = &state.status else {
        let mut items = vec![Item::Text(Line::from(t.d(if state.busy { " Asking Tor…" } else { " Nothing to show." })))];
        items.extend(error_items(t, &state.error));
        return Page::new(header, Line::from(t.d("")))
            .panel(Panel::new("Onion addresses", items).grow())
            .footer(Item::Text(hints(t, &[("g", "look again"), ("Esc", "back")])));
    };
    let right = if !status.tor_installed {
        Line::from(t.a("▲ Tor is not installed"))
    } else if status.tor_running {
        Line::from(t.o("● Tor running"))
    } else {
        Line::from(t.d("○ Tor stopped"))
    };
    let rows = state.rows();
    let mut node_items = Vec::new();
    let mut site_items = Vec::new();
    for (index, which) in rows.iter().enumerate() {
        let selected = index == state.selected;
        match which {
            OnionRow::Node => {
                let Some(node) = &status.node else { continue };
                let left = format!(" {} {}", checkbox(node.enabled), pad(&node.provider, 14));
                let address = match (&node.address, node.enabled, node.restricted_rpc) {
                    (Some(address), true, _) => t.o(address.clone()),
                    (None, true, true) => t.a("waiting for Tor to make the address"),
                    (_, _, false) => t.a("no restricted RPC port to serve"),
                    (_, false, true) => t.d("off"),
                };
                node_items.push(row(
                    selected,
                    Line::from(vec![t.n(mark(selected, &left)), t.d(format!("restricted RPC, port {}", node.port))]),
                    Line::from(vec![address, t.n(" ")]),
                ));
            }
            OnionRow::Site(i) => {
                let Some(site) = status.sites.get(*i) else { continue };
                let name = if site.id.site == "web" {
                    site.id.package.clone()
                } else {
                    format!("{} › {}", site.id.package, site.id.site)
                };
                let left = format!(" {} {}", checkbox(site.exposure.onion), pad(&name, 22));
                let mut others = Vec::new();
                if site.exposure.local {
                    others.push("local");
                }
                if site.exposure.public {
                    others.push("public");
                }
                let also = if others.is_empty() { String::new() } else { format!("also {}", others.join(", ")) };
                let address = match (&site.url, site.exposure.onion, site.locked) {
                    (_, _, true) => t.a("◆ locked"),
                    (Some(url), true, _) => t.o(url.clone()),
                    (None, true, _) => t.a("waiting for Tor"),
                    (_, false, _) if !site.multi_address && !others.is_empty() => t.d("single address"),
                    (_, false, _) => t.d("off"),
                };
                site_items.push(row(
                    selected,
                    Line::from(vec![t.n(mark(selected, &left)), t.d(also)]),
                    Line::from(vec![address, t.n(" ")]),
                ));
            }
        }
    }
    if node_items.is_empty() {
        node_items.push(Item::Text(Line::from(t.d(" No node is installed."))));
    }
    if site_items.is_empty() {
        site_items.push(Item::Text(Line::from(t.d(" No package offers a site yet."))));
    }
    site_items.extend(error_items(t, &state.error));
    let mut about = vec![
        bullet(t, "An onion address is reached through Tor only. It never shows your IP and needs no open port."),
        bullet(t, "The node's address serves only its restricted RPC, which wallets use. Never the full RPC."),
        bullet(t, "Admin paths of sites are blocked on onion addresses. Keys are kept in backups."),
    ];
    if !status.tor_installed {
        about.push(Item::Text(Line::from(vec![t.a("▲ "), t.n("Install the tor package to get onion addresses.")])));
    }
    Page::new(header, right)
        .panel(Panel::accent("Node", node_items))
        .panel(Panel::new("Sites", site_items).grow())
        .panel(Panel::new("About onion addresses", about))
        .footer(Item::Text(hints(
            t,
            &[("↑↓", "move"), ("Space", "turn on or off"), ("s", "node settings"), ("g", "look again"), ("Esc", "back")],
        )))
}

// Restore

fn drive_name(drive: &nodo_api::UsbDrive) -> String {
    let mut name = if drive.model.is_empty() { "USB drive".to_owned() } else { drive.model.clone() };
    if let Some(label) = &drive.label {
        name.push_str(&format!(", {label}"));
    }
    name
}

fn field(t: &Theme, label: &str, value: String, focused: bool) -> Item {
    let left = Line::from(if focused { t.b(label.to_owned()) } else { t.d(label.to_owned()) });
    let mut spans = vec![t.n(value)];
    if focused {
        spans.push(t.s(" "));
    }
    Item::Row(left, Line::from(spans))
}

fn steps_panel(t: &Theme, current: usize) -> Panel {
    let names = ["Choose the drive", "Choose the backup", "Open it with the backup passphrase", "Confirm and restore"];
    let items = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let symbol = if index < current {
                t.o("✓")
            } else if index == current {
                t.a("▸")
            } else {
                t.d("○")
            };
            Item::Text(Line::from(vec![symbol, t.n(format!(" {name}"))]))
        })
        .collect();
    Panel::new("Steps", items)
}

pub fn restore(app: &App, t: &Theme, state: &RestoreState) -> Page {
    let header = |step: &str| crumbs(t, &["Set up this device", "Restore from a USB backup", step]);
    let busy = Line::from(t.d(if state.busy { "Working…" } else { "" }));
    let errors = error_items(t, &state.error);
    match &state.step {
        RestoreStep::Drives => {
            let mut items: Vec<Item> = state
                .drives
                .iter()
                .enumerate()
                .map(|(index, drive)| {
                    let selected = index == state.selected;
                    let left = Line::from(t.n(mark(selected, &format!(" {}  {}", pad(&drive.device, 6), drive_name(drive)))));
                    let right = match &drive.problem {
                        Some(problem) => Line::from(vec![t.a(format!("▲ {problem}")), t.n(" ")]),
                        None => Line::from(t.d(format!("{} ", size(drive.size_bytes)))),
                    };
                    row(selected, left, right)
                })
                .collect();
            if state.drives.is_empty() && !state.busy {
                items.push(Item::Text(Line::from(t.d(" No USB drive is plugged in. Plug one in and press g."))));
            }
            items.extend(errors);
            Page::new(header("Drive"), busy)
                .panel(Panel::accent("USB drives", items).grow())
                .panel(steps_panel(t, 0))
                .panel(Panel::new(
                    "Restoring sets up this device",
                    vec![
                        bullet(t, "Packages come back from the verified copies in the backup, without their feeds."),
                        bullet(t, "Storage, secrets, certificates, onion keys, feeds and approvals come back."),
                        bullet(t, "The system disk and the blockchain drive are never listed or written."),
                    ],
                ))
                .footer(Item::Text(hints(t, &[("↑↓", "move"), ("↵", "choose"), ("g", "look again"), ("Esc", "back to setup")])))
        }
        RestoreStep::Backups { device } => {
            let mut items = Vec::new();
            if let Some(list) = &state.backups {
                for (index, backup) in list.backups.iter().enumerate() {
                    let selected = index == state.selected;
                    items.push(row(
                        selected,
                        Line::from(t.n(mark(selected, &format!(" {}", backup.name)))),
                        Line::from(t.d(format!("{} ", size(backup.size_bytes)))),
                    ));
                }
                if list.backups.is_empty() {
                    items.push(Item::Text(Line::from(t.d(" This drive holds no monokulo-nodo backups."))));
                }
            } else if state.busy {
                items.push(Item::Text(Line::from(t.d(" Reading the drive…"))));
            }
            items.extend(errors);
            let title = match state.drive(device) {
                Some(drive) => format!("Backups on {} · {}", drive.device, drive_name(drive)),
                None => format!("Backups on {device}"),
            };
            Page::new(header("Backup"), busy)
                .panel(Panel::accent(title, items).grow())
                .panel(steps_panel(t, 1))
                .panel(Panel::new(
                    "The drive is read only",
                    vec![Item::Text(Line::from(t.d(
                        "It is mounted read-only to list and read the backup, and unmounted again.",
                    )))],
                ))
                .footer(Item::Text(hints(t, &[("↑↓", "move"), ("↵", "choose"), ("g", "look again"), ("Esc", "other drive")])))
        }
        RestoreStep::Passphrase { device, name, passphrase } => {
            let mut items = vec![field(t, "Backup passphrase", passphrase.display(), true)];
            items.extend(errors);
            Page::new(header("Passphrase"), busy)
                .panel(Panel::new("Backup", vec![Item::Text(Line::from(t.n(format!("{name}, on {device}"))))]))
                .panel(Panel::accent("Open this backup", items))
                .panel(steps_panel(t, 2))
                .panel(
                    Panel::new(
                        "Nothing changes yet",
                        vec![Item::Text(Line::from(t.d(
                            "The whole backup is checked first. A wrong passphrase or a damaged file restores nothing.",
                        )))],
                    )
                    .grow(),
                )
                .footer(Item::Text(hints(t, &[("↵", "check backup"), ("Esc", "other backup")])))
        }
        RestoreStep::Confirm { name, contents, typed, first, second, field: focus, .. } => {
            let mut what = Vec::new();
            for id in &contents.settings {
                what.push(Item::Text(Line::from(vec![t.o("✓"), t.n(format!(" {id} settings"))])));
            }
            for id in &contents.storage {
                what.push(Item::Text(Line::from(vec![t.o("✓"), t.n(format!(" {id} encrypted storage"))])));
            }
            if contents.onion_keys {
                what.push(Item::Text(Line::from(vec![t.o("✓"), t.n(" Tor onion keys")])));
            }
            what.push(Item::Text(Line::from(vec![t.o("✓"), t.n(" Feeds, pinned keys, approvals and their packages")])));
            if contents.has_device_key {
                what.push(Item::Text(Line::from(vec![t.o("✓"), t.n(" Device key, so the backed-up storage opens")])));
            }
            let mut confirm = vec![
                field(t, &format!("Type {RESTORE_WORD}"), typed.display(), *focus == 0),
                field(t, "New device passphrase", first.display(), *focus == 1),
                field(t, "Again", second.display(), *focus == 2),
            ];
            confirm.extend(errors);
            Page::new(header("Confirm"), Line::from(t.o(format!("✓ {name} verified"))))
                .panel(Panel::new("It will restore", what).grow())
                .panel(Panel::error("Set up this device from the backup", confirm))
                .footer(Item::Text(hints(t, &[("Tab", "next field"), ("↵", "restore"), ("Esc", "cancel, safe default")])))
        }
        RestoreStep::Restoring { device, name, started } => {
            let seconds = app.ticks.saturating_sub(*started) / TICKS_PER_SECOND;
            let spinner = ["◐", "◓", "◑", "◒"][(app.ticks % 4) as usize];
            let items = vec![
                Item::Text(Line::from(vec![t.a(spinner), t.n(format!(" Restoring {name} from {device}"))])),
                Item::Text(Line::from(t.d(format!("   {}:{:02} so far", seconds / 60, seconds % 60)))),
                Item::Blank,
                Item::Text(Line::from(t.n("The helper checks the backup again, writes the device key wrapped in the"))),
                Item::Text(Line::from(t.n("new passphrase, then installs each package, fills its storage and starts it."))),
            ];
            Page::new(header("Restoring"), Line::from(t.d("Working…")))
                .panel(Panel::accent("Restoring", items).grow())
                .panel(Panel::new(
                    "Keep the drive plugged in",
                    vec![Item::Text(Line::from(vec![
                        t.a("▲ "),
                        t.n("Pulling it out now stops the restore. The device then stays not set up."),
                    ]))],
                ))
                .footer(Item::Text(hints(t, &[("", "Large storage takes minutes. Keys wait until it ends.")])))
        }
        RestoreStep::Failed { device, name, reason } => Page::new(header("Failed"), Line::from(t.e("✕ Not restored")))
            .panel(Panel::error(
                "The restore did not finish",
                vec![
                    Item::Text(Line::from(t.n(format!("{name}, on {device}")))),
                    Item::Blank,
                    Item::Text(Line::from(t.e(format!("✕ {reason}")))),
                ],
            ))
            .panel(
                Panel::new(
                    "What now",
                    vec![
                        bullet(t, "The device is still not set up, so nothing half-restored is used."),
                        bullet(t, "Check the drive is plugged in and the passphrase is the backup's, then try again."),
                        bullet(t, "Or go back to setup with Esc twice and set up a new device instead."),
                    ],
                )
                .grow(),
            )
            .footer(Item::Text(hints(t, &[("↵", "try again"), ("Esc", "choose a drive")]))),
        RestoreStep::Done(report) => {
            let mut items: Vec<Item> =
                report.restored.iter().map(|r| Item::Text(Line::from(vec![t.o("✓"), t.n(format!(" {r}"))]))).collect();
            for skipped in &report.skipped {
                items.push(Item::Text(Line::from(vec![t.a("▲"), t.n(format!(" {skipped}"))])));
            }
            let started = if report.restarted.is_empty() {
                "No services needed starting.".to_owned()
            } else {
                format!("Started: {}", report.restarted.join(", "))
            };
            Page::new(header("Done"), Line::from(t.o("✓ Restored")))
                .panel(Panel::new("Restored", items).grow())
                .panel(Panel::new("Services", vec![Item::Text(Line::from(t.d(started)))]))
                .footer(Item::Text(hints(t, &[("↵", "SSH keys next")])))
        }
    }
}

// Rollbacks

/// The UTC date, which unlike a relative time reads the same in every snapshot.
fn when(at: u64) -> String {
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

pub fn rollbacks(t: &Theme, state: &RollbacksState) -> Page {
    let header = crumbs(t, &["System", "Updates", "Undo an update"]);
    let right = Line::from(t.d(if state.busy { "Working…" } else { "" }));
    let Some(view) = &state.view else {
        let mut items = vec![Item::Text(Line::from(t.d(if state.busy { " Reading…" } else { " Nothing to show." })))];
        items.extend(error_items(t, &state.error));
        return Page::new(header, right)
            .panel(Panel::new("Undo an update", items).grow())
            .footer(Item::Text(hints(t, &[("g", "look again"), ("Esc", "back")])));
    };
    let mut options = Vec::new();
    for (index, option) in view.options.iter().enumerate() {
        let selected = index == state.selected;
        let left = format!(" {}{} → {}", pad(&option.package, 16), pad(&option.current, 12), option.previous);
        let right = match &option.blocked {
            Some(reason) => Line::from(vec![t.a(format!("▲ {reason}")), t.n(" ")]),
            None => Line::from(vec![t.o("can roll back"), t.n(" ")]),
        };
        options.push(row(selected, Line::from(t.n(mark(selected, &left))), right));
    }
    if options.is_empty() {
        options.push(Item::Text(Line::from(t.d(" Nothing to roll back. A package keeps its previous version after an update."))));
    }
    if let Some(result) = &state.result {
        options.push(Item::Blank);
        options.push(Item::Text(Line::from(match &result.outcome {
            UpdateOutcome::Updated => t.o(format!("✓ {} is back on {}.", result.package, result.to)),
            UpdateOutcome::RolledBack { reason, recovered: true, .. } => {
                t.a(format!("▲ {} stayed on {}: {reason}", result.package, result.from))
            }
            UpdateOutcome::RolledBack { reason, .. } => {
                t.e(format!("✕ {} failed and did not come back: {reason}", result.package))
            }
            UpdateOutcome::Refused(reason) | UpdateOutcome::Postponed(reason) => {
                t.a(format!("▲ {}: {reason}", result.package))
            }
        })));
    }
    options.extend(error_items(t, &state.error));

    let mut happened = Vec::new();
    for event in &view.events {
        let (symbol, text) = match &event.kind {
            UpdateEventKind::RolledBack { reason } => (t.a("▲"), format!("stayed on {}: {reason}", event.from)),
            UpdateEventKind::RollbackFailed { reason, rollback_error } => {
                (t.e("✕"), format!("did not come back: {reason}; {rollback_error}"))
            }
            _ => continue,
        };
        happened.push(Item::Row(
            Line::from(vec![symbol, t.n(format!(" {} {} → {} ", event.package, event.from, event.to)), t.d(text)]),
            Line::from(t.d(format!("{} ", when(event.at)))),
        ));
    }
    if happened.is_empty() {
        happened.push(Item::Text(Line::from(t.d(" No update has failed."))));
    }
    let system = match &view.boot_fallback {
        Some(reason) => Item::Text(Line::from(vec![t.a("▲ "), t.n(format!("Back on the previous system: {reason}"))])),
        None => Item::Text(Line::from(t.d(" The system runs the version it was updated to. No fallback happened."))),
    };
    Page::new(header, right)
        .panel(Panel::accent("Undo an update", options).grow())
        .panel(Panel::new("Updates that were rolled back", happened))
        .panel(Panel::new("System", vec![system]))
        .footer(Item::Text(hints(t, &[("↑↓", "move"), ("↵", "roll back"), ("l", "log"), ("g", "look again"), ("Esc", "back")])))
}
