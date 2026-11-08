//! First boot: the device passphrase, then SSH keys. Also reached later for keys alone.

use ratatui::text::Line;

use super::hints;
use super::layout::{Item, Page, Panel};
use crate::app::{SetupState, SetupStep, MIN_PASSPHRASE_CHARS};
use crate::theme::Theme;

fn error_items(t: &Theme, state: &SetupState) -> Vec<Item> {
    state.error.iter().map(|error| Item::Text(Line::from(t.e(format!("✕ {error}"))))).collect()
}

pub fn setup(t: &Theme, state: &SetupState) -> Page {
    match &state.step {
        SetupStep::Passphrase { first, second, confirming } => {
            let mut items = vec![Item::Row(
                Line::from(t.d("Passphrase")),
                Line::from(vec![t.n(first.display()), t.s(if *confirming { "" } else { " " })]),
            )];
            if *confirming {
                items.push(Item::Row(
                    Line::from(t.d("Again")),
                    Line::from(vec![t.n(second.display()), t.s(" ")]),
                ));
            }
            items.extend(error_items(t, state));
            if state.busy {
                items.push(Item::Text(Line::from(t.i("Setting up encrypted storage…"))));
            }
            let about = vec![
                Item::Text(Line::from(vec![t.i("▸"), t.n(" One passphrase opens all encrypted storage after a restart.")])),
                Item::Text(Line::from(vec![t.i("▸"), t.n(format!(" Use at least {MIN_PASSPHRASE_CHARS} characters. A few unrelated words work well."))])),
                Item::Text(Line::from(vec![t.i("▸"), t.n(" There is no default passphrase and no way to recover a lost one.")])),
            ];
            let keys: &[(&str, &str)] = if *confirming {
                &[("↵", "set passphrase"), ("Esc", "retype the first")]
            } else {
                &[("↵", "next"), ("Ctrl-R", "restore from a USB backup"), ("Ctrl-C", "quit")]
            };
            Page::new(Line::from(t.b("Set up this device")), Line::from(t.d("Step 1 of 2")))
                .panel(Panel::accent("Device passphrase", items))
                .panel(Panel::new("About the passphrase", about).grow())
                .footer(Item::Text(hints(t, keys)))
        }
        SetupStep::Keys { keys, selected, adding } => {
            let mut items: Vec<Item> = keys
                .iter()
                .enumerate()
                .map(|(index, key)| {
                    let left = Line::from(vec![t.n(format!("{} ", key.algorithm)), t.b(key.comment.clone())]);
                    let right = Line::from(t.d(key.fingerprint.clone()));
                    if index == *selected && adding.is_none() { Item::Selected(left, right) } else { Item::Row(left, right) }
                })
                .collect();
            if keys.is_empty() && !state.busy {
                items.push(Item::Text(Line::from(t.d("No key can log in over SSH yet."))));
            }
            if let Some(input) = adding {
                items.push(Item::Blank);
                items.push(Item::Row(Line::from(t.d("Paste a public key")), Line::from(vec![t.n(input.display()), t.s(" ")])));
            }
            items.extend(error_items(t, state));
            let about = vec![
                Item::Text(Line::from(vec![t.i("▸"), t.n(" Keys log in as root over SSH. Password login is off.")])),
                Item::Text(Line::from(vec![t.i("▸"), t.n(" Paste one line such as ssh-ed25519 AAAA… name. Options are not accepted.")])),
                Item::Text(Line::from(vec![t.i("▸"), t.n(" The last key cannot be removed, so a headless device stays reachable.")])),
            ];
            let footer = if adding.is_some() {
                hints(t, &[("↵", "add key"), ("Esc", "cancel")])
            } else {
                hints(t, &[("a", "add"), ("d", "remove"), ("↵", "done")])
            };
            Page::new(Line::from(t.b("SSH keys")), Line::from(t.d(if state.busy { "Working…" } else { "" })))
                .panel(Panel::accent("Keys that may log in", items).grow())
                .panel(Panel::new("About keys", about))
                .footer(Item::Text(footer))
        }
    }
}
