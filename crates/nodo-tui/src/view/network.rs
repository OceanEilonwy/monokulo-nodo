//! The Network screen, drawn from `docs/design/tui/Network.dc.html`: Wi-Fi and Bluetooth in two
//! bordered panels, the focused one in the accent colour, a passphrase field while joining, and
//! key hints in the footer.

use crossterm::event::KeyCode;
use nodo_api::{BluetoothStatus, PairingStage, WifiLink, WifiSecurity, WifiStatus};
use ratatui::text::{Line, Span};

use super::layout::{Item, Page, Panel};
use super::more::{bullet, crumbs, error_items, row};
use super::{checkbox, hints, mark, pad};
use crate::app::{NetworkFocus, NetworkState, RadioRead, WifiRow};
use crate::hit::{Click, Target};
use crate::theme::Theme;

/// Signal strength as bars, so it reads without numbers too.
fn bars(dbm: i32) -> &'static str {
    match dbm {
        d if d >= -55 => "▂▄▆█",
        d if d >= -67 => "▂▄▆ ",
        d if d >= -78 => "▂▄  ",
        _ => "▂   ",
    }
}

fn security_span(t: &Theme, security: WifiSecurity) -> Span<'static> {
    let text = pad(security.label(), 11);
    match security {
        WifiSecurity::Open => t.a(text),
        WifiSecurity::Legacy | WifiSecurity::Enterprise => t.d(text),
        _ => t.n(text),
    }
}

/// What the device makes of a Bluetooth icon name.
fn kind(icon: Option<&str>) -> &'static str {
    match icon.unwrap_or("") {
        k if k.starts_with("input-keyboard") => "keyboard",
        k if k.starts_with("input-mouse") => "mouse",
        k if k.starts_with("input-gaming") => "game pad",
        k if k.starts_with("audio-headset") || k.starts_with("audio-headphones") => "headphones",
        k if k.starts_with("audio") => "speaker",
        k if k.starts_with("phone") => "phone",
        k if k.starts_with("computer") => "computer",
        _ => "device",
    }
}

fn wifi_status_line(t: &Theme, wifi: &WifiStatus) -> Item {
    if let Some(why) = &wifi.unavailable {
        return Item::Text(Line::from(vec![t.a(" ▲ "), t.n(why.clone())]));
    }
    if !wifi.enabled {
        return Item::Row(
            Line::from(vec![t.n(" [ ] off  "), t.d("Off. Press o to turn it on.")]),
            Line::from(t.d(format!("{} ", wifi.interface.as_deref().unwrap_or("")))),
        );
    }
    let state: Vec<Span<'static>> = match (wifi.link, &wifi.ssid) {
        (WifiLink::Connected, Some(ssid)) => {
            let mut spans = vec![t.o("● "), t.n(format!("Joined {ssid}"))];
            if let Some(dbm) = wifi.signal_dbm {
                spans.push(t.d(format!("  {dbm} dBm")));
            }
            match &wifi.address {
                Some(address) => spans.push(t.n(format!("  {address}"))),
                None => spans.push(t.a("  waiting for an address")),
            }
            spans
        }
        (WifiLink::Connecting, ssid) => {
            vec![t.a("◌ "), t.n(format!("Joining {}…", ssid.as_deref().unwrap_or("a network")))]
        }
        _ => vec![t.d("○ "), t.d("Not joined to any network")],
    };
    let mut left = vec![t.n(format!(" {} on   ", checkbox(true)))];
    left.extend(state);
    Item::Row(Line::from(left), Line::from(t.d(format!("{} ", wifi.interface.as_deref().unwrap_or("")))))
}

/// What a panel with no status yet shows: still asking, or why there is nothing, with a way to
/// ask again. Returns whether it offered "Try again", which is then a click target.
fn not_read(t: &Theme, read: &RadioRead, items: &mut Vec<Item>) -> bool {
    let why = match read {
        RadioRead::Asking | RadioRead::Read => {
            items.push(Item::Text(Line::from(t.d(" Asking the helper…"))));
            return false;
        }
        // Neutral on purpose: the helper may only be busy with an earlier, slower request.
        RadioRead::NoAnswer => t.a(" ▲ No answer from the helper yet."),
        RadioRead::Failed(e) => t.e(format!(" ✕ The helper could not say: {e}")),
    };
    items.push(Item::Text(Line::from(why)));
    items.push(Item::Text(Line::from(vec![t.n(" "), t.a(TRY_AGAIN), t.d("   or press r")])));
    true
}

const TRY_AGAIN: &str = "Try again";

fn wifi_panel(t: &Theme, state: &NetworkState) -> (Panel, bool) {
    let focused = state.focus == NetworkFocus::Wifi;
    let mut items = Vec::new();
    let Some(wifi) = &state.wifi else {
        let again = not_read(t, &state.wifi_read, &mut items);
        if focused {
            items.extend(error_items(t, &state.error));
        }
        return (if focused { Panel::accent("Wi-Fi", items) } else { Panel::new("Wi-Fi", items) }, again);
    };
    items.push(wifi_status_line(t, wifi));
    if let Some(prompt) = &state.prompt {
        items.push(Item::Blank);
        items.push(Item::Text(Line::from(vec![
            t.b(format!(" Join {}", prompt.ssid)),
            t.d(format!("  {}, the passphrase is stored on this device only", prompt.security.label())),
        ])));
        items.push(Item::Row(
            Line::from(vec![t.n(" Passphrase  "), t.n(prompt.passphrase.display()), t.s(" ")]),
            Line::from(vec![t.a("↵"), t.d(" join  "), t.a("Esc"), t.d(" cancel ")]),
        ));
    } else if wifi.enabled && wifi.unavailable.is_none() {
        let rows = state.wifi_rows();
        if rows.is_empty() {
            items.push(Item::Text(Line::from(t.d(" Press s to look for networks."))));
        }
        for (index, which) in rows.iter().enumerate() {
            let selected = focused && index == state.wifi_selected;
            let (left, right) = match which {
                WifiRow::InRange(i) => {
                    let Some(network) = state.networks.get(*i) else { continue };
                    let name = mark(selected, &format!(" {}", pad(&network.ssid, 24)));
                    let mut words = Vec::new();
                    if network.saved || state.is_saved(&network.ssid) {
                        words.push("saved");
                    }
                    if state.joined() == Some(network.ssid.as_str()) {
                        words.push("joined");
                    }
                    let right_span = if words.contains(&"joined") {
                        t.o(format!("{} ", words.join(" · ")))
                    } else {
                        t.d(format!("{} ", words.join(" · ")))
                    };
                    (
                        Line::from(vec![
                            t.n(name),
                            security_span(t, network.security),
                            t.i(bars(network.signal_dbm)),
                            t.d(format!("  {} dBm", network.signal_dbm)),
                        ]),
                        Line::from(right_span),
                    )
                }
                WifiRow::Saved(i) => {
                    let Some(saved) = wifi.saved.get(*i) else { continue };
                    let name = mark(selected, &format!(" {}", pad(&saved.ssid, 24)));
                    let joined = state.joined() == Some(saved.ssid.as_str());
                    (
                        Line::from(vec![t.n(name), security_span(t, saved.security)]),
                        Line::from(if joined { t.o("saved · joined ") } else { t.d("saved, not in range ") }),
                    )
                }
            };
            items.push(row(selected, left, right));
        }
    }
    if focused {
        items.extend(error_items(t, &state.error));
        (Panel::accent("Wi-Fi", items), false)
    } else {
        (Panel::new("Wi-Fi", items), false)
    }
}

fn pairing_line(t: &Theme, bt: &BluetoothStatus) -> Option<Item> {
    let pairing = bt.pairing.as_ref()?;
    let name = bt.devices.iter().find(|d| d.address == pairing.address).map_or(pairing.address.as_str(), |d| d.name.as_str());
    let line = match &pairing.stage {
        PairingStage::Pairing => Line::from(vec![t.a(" ◌ "), t.n(format!("Pairing with {name}…"))]),
        PairingStage::TypePasskey(key) => Line::from(vec![
            t.a(" ◆ "),
            t.n("Type "),
            t.b(key.clone()),
            t.n(format!(" on {name}, then press its Enter key.")),
        ]),
        PairingStage::ComparePasskey(key) => Line::from(vec![
            t.a(" ◆ "),
            t.n(format!("{name} should show ")),
            t.b(key.clone()),
            t.n(". If it does not, remove it: someone else may be pairing."),
        ]),
        PairingStage::Done => return None,
        PairingStage::Failed(why) => Line::from(t.e(format!(" ✕ Pairing with {name} failed: {why}"))),
    };
    Some(Item::Text(line))
}

fn bluetooth_panel(t: &Theme, state: &NetworkState) -> (Panel, bool) {
    let focused = state.focus == NetworkFocus::Bluetooth;
    let mut items = Vec::new();
    let mut again = false;
    match &state.bluetooth {
        None => again = not_read(t, &state.bluetooth_read, &mut items),
        Some(bt) if bt.unavailable.is_some() => {
            items.push(Item::Text(Line::from(vec![t.a(" ▲ "), t.n(bt.unavailable.clone().unwrap_or_default())])));
        }
        Some(bt) if !bt.enabled => items.push(Item::Text(Line::from(vec![
            t.n(" [ ] off  "),
            t.d("Off. Press o to turn it on. Nothing can pair with the device while it is off."),
        ]))),
        Some(bt) => {
            let controller = bt.controller.clone().unwrap_or_default();
            items.push(Item::Row(
                Line::from(vec![
                    t.n(format!(" {} on   ", checkbox(true))),
                    if bt.powered { t.o("● ") } else { t.a("◌ ") },
                    t.n(if bt.powered { "Not discoverable, pairs only when you ask" } else { "Powering on…" }),
                ]),
                Line::from(t.d(format!("{controller} "))),
            ));
            items.extend(pairing_line(t, bt));
            if bt.devices.is_empty() {
                items.push(Item::Text(Line::from(t.d(" Put a device in its pairing mode, then press s to look for it."))));
            }
            for (index, device) in bt.devices.iter().enumerate() {
                let selected = focused && index == state.bt_selected;
                let name = mark(selected, &format!(" {}", pad(&device.name, 24)));
                let rssi = device.rssi.map(|r| format!("  {r} dBm")).unwrap_or_default();
                let mut words = Vec::new();
                if device.paired {
                    words.push("paired");
                }
                if device.connected {
                    words.push("connected");
                }
                if words.is_empty() {
                    words.push("found");
                }
                let right = if device.connected { t.o(format!("{} ", words.join(" · "))) } else { t.d(format!("{} ", words.join(" · "))) };
                items.push(row(
                    selected,
                    Line::from(vec![t.n(name), t.n(pad(kind(device.kind.as_deref()), 11)), t.d(rssi)]),
                    Line::from(right),
                ));
            }
        }
    }
    if focused {
        items.extend(error_items(t, &state.error));
        (Panel::accent("Bluetooth", items), again)
    } else {
        (Panel::new("Bluetooth", items), again)
    }
}

pub fn network(t: &Theme, state: &NetworkState) -> Page {
    let header = crumbs(t, &["Home", "Network"]);
    let right = match &state.wifi {
        Some(wifi) if wifi.wired_up => Line::from(t.o("● Wired connected")),
        Some(_) => Line::from(t.a("▲ Wired link down")),
        None => Line::from(t.d("")),
    };
    let about = vec![
        bullet(t, "Wired stays the main connection. Wi-Fi carries traffic only while the wired link is down."),
        bullet(t, "Wi-Fi keys are stored root-only on this device. For WPA2 only a derived key is kept."),
        bullet(t, "Bluetooth is never discoverable. Only pairings you start here can happen."),
    ];
    let (wifi, wifi_again) = wifi_panel(t, state);
    let (bluetooth, bluetooth_again) = bluetooth_panel(t, state);
    let mut page = Page::new(header, right)
        .panel(wifi.grow())
        .panel(bluetooth)
        .panel(Panel::new("About wireless", about));
    // One target per "Try again" drawn, in order, since both panels can show one.
    for shown in [wifi_again, bluetooth_again] {
        if shown {
            page = page.target(Target::text(TRY_AGAIN, Click::Key(KeyCode::Char('r'))));
        }
    }
    if let Some(busy) = &state.busy {
        page = page.footer(Item::Text(Line::from(t.d(format!("⧗ {busy}")))));
    }
    let keys: &[(&str, &str)] = match state.focus {
        NetworkFocus::Wifi => &[
            ("←→", "Wi-Fi/Bluetooth"),
            ("↑↓", "move"),
            ("↵", "join"),
            ("s", "scan"),
            ("o", "on/off"),
            ("f", "forget"),
            ("g", "look again"),
            ("Esc", "back"),
        ],
        NetworkFocus::Bluetooth => &[
            ("←→", "Wi-Fi/Bluetooth"),
            ("↑↓", "move"),
            ("↵", "pair or connect"),
            ("s", "scan"),
            ("o", "on/off"),
            ("x", "remove"),
            ("g", "look again"),
            ("Esc", "back"),
        ],
    };
    let mut keys = keys.to_vec();
    if state.can_ask_again() {
        keys.insert(keys.len() - 1, ("r", "try again"));
    }
    page.footer(Item::Text(hints(t, &keys)))
}
