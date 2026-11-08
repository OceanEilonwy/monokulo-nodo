//! Backups and restore. There is no design file for this screen, so it follows the others: a
//! breadcrumb header, bordered panels, the safe choice first, typed confirmation for anything
//! that overwrites, and key hints in the footer.

use ratatui::text::Line;

use super::hints;
use super::layout::{Item, Page, Panel};
use crate::app::{BackupState, BackupStep, RESTORE_WORD};
use crate::theme::Theme;

fn size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1e6)
    } else {
        format!("{:.0} KB", (bytes as f64 / 1e3).max(1.0))
    }
}

fn field(t: &Theme, label: &str, value: String, focused: bool) -> Item {
    let left = Line::from(if focused { t.b(label.to_owned()) } else { t.d(label.to_owned()) });
    let mut spans = vec![t.n(value)];
    if focused {
        spans.push(t.s(" "));
    }
    Item::Row(left, Line::from(spans))
}

fn bullet(t: &Theme, ok: bool, text: &str) -> Item {
    let symbol = if ok { t.o("✓") } else { t.i("●") };
    Item::Text(Line::from(vec![symbol, t.n(format!(" {text}"))]))
}

fn errors(t: &Theme, state: &BackupState) -> Vec<Item> {
    state.error.iter().map(|e| Item::Text(Line::from(t.e(format!("✕ {e}"))))).collect()
}

fn holds(t: &Theme) -> Panel {
    Panel::new(
        "A backup holds",
        vec![
            bullet(t, true, "Package settings and secrets"),
            bullet(t, true, "The device key, so encrypted storage can be restored"),
            bullet(t, true, "The contents of unlocked encrypted storage"),
            bullet(t, true, "Tor onion keys"),
            Item::Blank,
            Item::Text(Line::from(t.d("Never chain data. The node syncs it again."))),
            Item::Text(Line::from(t.d("Encrypted with its own passphrase, at the same cost as the device key."))),
        ],
    )
}

pub fn backup(t: &Theme, state: &BackupState) -> Page {
    let header = |step: &str| Line::from(vec![t.n("Home "), t.d("› "), t.b(step.to_owned())]);
    let right = Line::from(t.d(if state.busy { "Working…" } else { "" }));
    match &state.step {
        BackupStep::List => {
            let mut rows: Vec<Item> = state
                .list
                .iter()
                .enumerate()
                .map(|(index, b)| {
                    let left = Line::from(t.n(format!(" {}", b.name)));
                    let right = Line::from(t.d(format!("{} ", size(b.size_bytes))));
                    if index == state.selected { Item::Selected(left, right) } else { Item::Row(left, right) }
                })
                .collect();
            if state.list.is_empty() && !state.busy {
                rows.push(Item::Text(Line::from(t.d(" No backups yet. Press n to make one."))));
            }
            rows.extend(errors(t, state));
            Page::new(header("Backups"), right)
                .panel(Panel::new("Backups on this device", rows).grow())
                .panel(holds(t))
                .panel(Panel::new(
                    "Restoring",
                    vec![
                        Item::Text(Line::from(t.n("Restoring replaces settings, secrets and storage with the backup's."))),
                        Item::Text(Line::from(t.d("The services it replaces stop first and start again after."))),
                    ],
                ))
                .footer(Item::Text(hints(t, &[("↑↓", "move"), ("n", "new backup"), ("r", "restore"), ("Esc", "back")])))
        }
        BackupStep::Create { device, first, second, field: focus } => {
            let mut items = Vec::new();
            if state.device_set_up {
                items.push(field(t, "Device passphrase", device.display(), *focus == 0));
            }
            items.push(field(t, "Backup passphrase", first.display(), *focus == 1));
            items.push(field(t, "Again", second.display(), *focus == 2));
            items.extend(errors(t, state));
            let about = vec![
                bullet(t, false, "The backup passphrase opens this backup on any device. Keep it apart from the device."),
                bullet(t, false, "Use at least 12 characters. It is as valuable as the device passphrase."),
                bullet(t, false, "Nothing can open the backup without it."),
            ];
            Page::new(header("New backup"), right)
                .panel(Panel::accent("Passphrases", items))
                .panel(Panel::new("About the backup passphrase", about).grow())
                .footer(Item::Text(hints(t, &[("Tab", "next field"), ("↵", "next or make backup"), ("Esc", "cancel")])))
        }
        BackupStep::Unlock { name, passphrase } => {
            let mut items = vec![field(t, "Backup passphrase", passphrase.display(), true)];
            items.extend(errors(t, state));
            Page::new(header("Restore"), right)
                .panel(Panel::new("Backup", vec![Item::Text(Line::from(t.n(name.clone())))]))
                .panel(Panel::accent("Open this backup", items))
                .panel(Panel::new(
                    "Nothing changes yet",
                    vec![Item::Text(Line::from(t.d(
                        "The whole backup is checked first. A wrong passphrase or a damaged file restores nothing.",
                    )))],
                ).grow())
                .footer(Item::Text(hints(t, &[("↵", "check backup"), ("Esc", "cancel")])))
        }
        BackupStep::Confirm { name, contents, typed, device, field: focus, .. } => {
            let mut what = Vec::new();
            for id in &contents.settings {
                what.push(bullet(t, true, &format!("{id} settings")));
            }
            for id in &contents.storage {
                what.push(bullet(t, true, &format!("{id} encrypted storage")));
            }
            if contents.onion_keys {
                what.push(bullet(t, true, "Tor onion keys"));
            }
            if contents.has_device_key {
                what.push(bullet(t, true, "Device key, kept if this device's storage already uses its own"));
            }
            let stops = if contents.services_to_stop.is_empty() {
                "Nothing needs to stop.".to_owned()
            } else {
                format!("Stops while restoring: {}", contents.services_to_stop.join(", "))
            };
            let mut confirm = vec![field(t, &format!("Type {RESTORE_WORD}"), typed.display(), *focus == 0)];
            if contents.has_device_key {
                confirm.push(field(t, "New device passphrase", device.display(), *focus == 1));
            }
            confirm.extend(errors(t, state));
            Page::new(header("Restore"), Line::from(t.a(format!("✓ {name} verified"))))
                .panel(Panel::new("It will replace", what).grow())
                .panel(Panel::accent("Services", vec![Item::Text(Line::from(t.a(format!("▲ {stops}"))))]))
                .panel(Panel::error("This overwrites what is on the device now", confirm))
                .footer(Item::Text(hints(t, &[("Tab", "next field"), ("↵", "restore"), ("Esc", "cancel, safe default")])))
        }
        BackupStep::Restored(report) => {
            let mut items: Vec<Item> = report.restored.iter().map(|r| bullet(t, true, r)).collect();
            for skipped in &report.skipped {
                items.push(Item::Text(Line::from(vec![t.a("▲"), t.n(format!(" {skipped}"))])));
            }
            let started = if report.restarted.is_empty() {
                "No services had to stop.".to_owned()
            } else {
                format!("Started again: {}", report.restarted.join(", "))
            };
            Page::new(header("Restore"), Line::from(t.o("✓ Restored")))
                .panel(Panel::new("Restored", items).grow())
                .panel(Panel::new("Services", vec![Item::Text(Line::from(t.d(started)))]))
                .footer(Item::Text(hints(t, &[("↵", "done")])))
        }
    }
}
