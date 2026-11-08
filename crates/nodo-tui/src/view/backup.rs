//! Backups and restore, on the device and on a USB drive, drawn from `docs/design/tui/Backup.dc.html`:
//! a breadcrumb header, bordered panels, the safe choice first, typed confirmation for anything
//! that overwrites, and key hints in the footer. On first boot the same screen restores a whole
//! device from a USB drive.

use ratatui::text::Line;

use super::hints;
use super::layout::{Item, Page, Panel};
use crate::app::{BackupState, BackupStep, RESTORE_WORD};
use crate::theme::Theme;

fn size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else if bytes >= 1_000_000 {
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
            bullet(t, true, "Package settings, secrets, certificates and Tor onion keys"),
            bullet(t, true, "The device key, so encrypted storage can be restored"),
            bullet(t, true, "The contents of unlocked encrypted storage"),
            bullet(t, true, "Feeds, pinned keys, approvals and the verified package files"),
            Item::Blank,
            Item::Text(Line::from(t.d("Never chain data. The node syncs it again."))),
            Item::Text(Line::from(t.d("Encrypted with its own passphrase, at the same cost as the device key."))),
        ],
    )
}

pub fn backup(t: &Theme, state: &BackupState) -> Page {
    let root = if state.fresh { "Set up this device " } else { "Home " };
    let header = |step: &str| Line::from(vec![t.n(root.to_owned()), t.d("› "), t.b(step.to_owned())]);
    let right = Line::from(t.d(if state.busy { "Working…" } else { "" }));
    let usb_name = || {
        state
            .usb
            .as_ref()
            .filter(|u| Some(&u.drive.device) == state.target.as_ref())
            .map(|u| drive_name(&u.drive))
            .unwrap_or_else(|| "the USB drive".to_owned())
    };
    match &state.step {
        BackupStep::List if state.fresh => {
            let mut rows = usb_rows(t, state);
            rows.extend(errors(t, state));
            Page::new(header("Restore from a backup"), right)
                .panel(Panel::accent(usb_title(state), rows).grow())
                .panel(Panel::new(
                    "Restoring sets up this device",
                    vec![
                        bullet(t, false, "The backup passphrase opens the backup. Then choose a new device passphrase."),
                        bullet(t, false, "Packages come back from the verified copies in the backup, without their feeds."),
                        bullet(t, false, "Storage, secrets, certificates, feeds and approvals come back. It ends unlocked."),
                    ],
                ))
                .footer(Item::Text(hints(t, &[("↑↓", "move"), ("↵", "restore"), ("g", "look again"), ("Esc", "back to setup")])))
        }
        BackupStep::List => {
            let mut rows: Vec<Item> = state
                .list
                .iter()
                .enumerate()
                .map(|(index, b)| backup_row(t, b, index == state.selected && !state.usb_focused))
                .collect();
            if state.list.is_empty() && !state.busy {
                rows.push(Item::Text(Line::from(t.d(" No backups yet. Press n to make one."))));
            }
            rows.extend(errors(t, state));
            Page::new(header("Backups"), right)
                .panel(Panel::new("Backups on this device", rows).grow())
                .panel(Panel::new(usb_title(state), usb_rows(t, state)))
                .panel(holds(t))
                .footer(Item::Text(hints(
                    t,
                    &[("↑↓", "move"), ("Tab", "device or USB"), ("n", "new backup"), ("u", "back up to USB"), ("r", "restore"), ("Esc", "back")],
                )))
        }
        BackupStep::Create { device, first, second, field: focus } => {
            let mut items = Vec::new();
            if state.device_set_up {
                items.push(field(t, "Device passphrase", device.display(), *focus == 0));
            }
            items.push(field(t, "Backup passphrase", first.display(), *focus == 1));
            items.push(field(t, "Again", second.display(), *focus == 2));
            items.extend(errors(t, state));
            let mut about = vec![
                bullet(t, false, "The backup passphrase opens this backup on any device. Keep it apart from the device."),
                bullet(t, false, "Use at least 12 characters. It is as valuable as the device passphrase."),
                bullet(t, false, "Nothing can open the backup without it."),
            ];
            let title = if state.target.is_some() {
                about.push(bullet(t, false, "The drive is unmounted when the backup is written. Wait for it before unplugging."));
                format!("New backup on {}", usb_name())
            } else {
                "New backup".to_owned()
            };
            Page::new(header(&title), right)
                .panel(Panel::accent("Passphrases", items))
                .panel(Panel::new("About the backup passphrase", about).grow())
                .footer(Item::Text(hints(t, &[("Tab", "next field"), ("↵", "next or make backup"), ("Esc", "cancel")])))
        }
        BackupStep::Unlock { name, passphrase } => {
            let mut items = vec![field(t, "Backup passphrase", passphrase.display(), true)];
            items.extend(errors(t, state));
            let place = if state.target.is_some() { format!("{name}, on {}", usb_name()) } else { name.clone() };
            Page::new(header("Restore"), right)
                .panel(Panel::new("Backup", vec![Item::Text(Line::from(t.n(place)))]))
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
            if state.fresh {
                what.push(bullet(t, true, "Feeds, their pinned keys, approvals and the packages installed from them"));
                if contents.has_device_key {
                    what.push(bullet(t, true, "Device key, so the backed-up storage opens"));
                }
            } else if contents.has_device_key {
                what.push(bullet(t, true, "Device key, kept if this device's storage already uses its own"));
            }
            let asks_device = contents.has_device_key || state.fresh;
            let mut confirm = vec![field(t, &format!("Type {RESTORE_WORD}"), typed.display(), *focus == 0)];
            if asks_device {
                confirm.push(field(t, "New device passphrase", device.display(), *focus == 1));
            }
            confirm.extend(errors(t, state));
            let (services, tone_title) = if state.fresh {
                ("Each package is installed, its storage filled, then started.".to_owned(), "Set up this device from the backup")
            } else if contents.services_to_stop.is_empty() {
                ("Nothing needs to stop.".to_owned(), "This overwrites what is on the device now")
            } else {
                (
                    format!("Stops while restoring: {}", contents.services_to_stop.join(", ")),
                    "This overwrites what is on the device now",
                )
            };
            let title = if state.fresh { "It will restore" } else { "It will replace" };
            Page::new(header("Restore"), Line::from(t.a(format!("✓ {name} verified"))))
                .panel(Panel::new(title, what).grow())
                .panel(Panel::accent("Services", vec![Item::Text(Line::from(t.a(format!("▲ {services}"))))]))
                .panel(Panel::error(tone_title, confirm))
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
                format!("Started: {}", report.restarted.join(", "))
            };
            let next = if state.fresh { "SSH keys next" } else { "done" };
            Page::new(header("Restore"), Line::from(t.o("✓ Restored")))
                .panel(Panel::new("Restored", items).grow())
                .panel(Panel::new("Services", vec![Item::Text(Line::from(t.d(started)))]))
                .footer(Item::Text(hints(t, &[("↵", next)])))
        }
    }
}

fn drive_name(drive: &nodo_api::UsbDrive) -> String {
    let mut name = if drive.model.is_empty() { "USB drive".to_owned() } else { drive.model.clone() };
    if let Some(label) = &drive.label {
        name.push_str(&format!(", {label}"));
    }
    name
}

fn usb_title(state: &BackupState) -> String {
    match &state.usb {
        Some(usb) => format!("USB drive · {}", drive_name(&usb.drive)),
        None => "USB drive".to_owned(),
    }
}

fn filesystem(name: Option<&str>) -> &str {
    match name {
        Some("vfat") => "FAT32",
        Some("exfat") => "exFAT",
        Some("ext4") => "ext4",
        Some(other) => other,
        None => "no filesystem",
    }
}

fn backup_row(t: &Theme, b: &nodo_api::BackupInfo, selected: bool) -> Item {
    let left = Line::from(t.n(format!(" {}", b.name)));
    let right = Line::from(t.d(format!("{} ", size(b.size_bytes))));
    if selected { Item::Selected(left, right) } else { Item::Row(left, right) }
}

/// The USB drive's backups, or why there are none to show.
fn usb_rows(t: &Theme, state: &BackupState) -> Vec<Item> {
    let Some(usb) = &state.usb else {
        let text = if state.busy { " Looking for USB drives…" } else { " No USB drive is plugged in. Plug one in and press g." };
        return vec![Item::Text(Line::from(t.d(text)))];
    };
    if let Some(problem) = &usb.drive.problem {
        return vec![Item::Text(Line::from(vec![t.a(" ▲"), t.n(format!(" {}: {problem}", usb.drive.device))]))];
    }
    let mut rows: Vec<Item> = state
        .usb_list()
        .iter()
        .enumerate()
        .map(|(index, b)| backup_row(t, b, state.usb_focused && index == state.usb_selected))
        .collect();
    if rows.is_empty() {
        rows.push(Item::Text(Line::from(t.d(if state.fresh {
            " This drive holds no monokulo-nodo backups."
        } else {
            " No backups on this drive yet. Press u to write one."
        }))));
    }
    if let Some(list) = &usb.list {
        let mut info = format!(" {} · {} free", filesystem(usb.drive.filesystem.as_deref()), size(list.free_bytes));
        if list.read_only {
            info.push_str(" · read-only");
        }
        rows.push(Item::Text(Line::from(t.d(info))));
    }
    rows
}
