//! Keys and results for the Network screen: Wi-Fi and Bluetooth.
//!
//! Both radios stay off until the owner turns them on here. Joining an open network, forgetting
//! one, removing a device, and turning Wi-Fi off while it may carry this very session all ask
//! first, with the focus on Cancel. The helper checks every rule again.

use crossterm::event::{KeyCode, KeyEvent};
use nodo_api::{PairingStage, WifiLink, WifiSecurity};

use super::{notice, pop, push, step};
use crate::app::*;
use crate::secret::Input;

pub(super) fn open_network(app: &mut App) -> Vec<Effect> {
    push(app, Screen::Network(NetworkState::new()));
    vec![Effect::LoadNetwork]
}

pub(super) fn network_input(state: &mut NetworkState) -> Option<&mut Input> {
    state.prompt.as_mut().map(|p| &mut p.passphrase)
}

fn ask(app: &mut App, title: String, body: Vec<String>, confirm: &str, action: ModalAction) {
    app.modal = Some(Modal { title, body, confirm_label: confirm.to_owned(), confirm_focused: false, action });
}

pub(super) fn network_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Network(state) = &mut app.screen else {
        return Vec::new();
    };
    if key.code == KeyCode::Esc {
        if state.prompt.take().is_some() {
            state.error = None;
            return Vec::new();
        }
        pop(app);
        return vec![Effect::Refresh];
    }
    if state.busy.is_some() {
        return Vec::new();
    }
    if let Some(prompt) = state.prompt.as_mut() {
        if key.code == KeyCode::Enter {
            if prompt.passphrase.is_empty() {
                state.error = Some(format!("Type the passphrase of {}.", prompt.ssid));
                return Vec::new();
            }
            let prompt = state.prompt.take().expect("checked above");
            let mut passphrase = prompt.passphrase;
            state.error = None;
            state.busy = Some(format!("Joining {}…", prompt.ssid));
            return vec![Effect::JoinWifi {
                ssid: prompt.ssid,
                security: prompt.security,
                passphrase: Some(passphrase.take_secret()),
                accept_open: false,
            }];
        }
        super::edit_input(&mut prompt.passphrase, key);
        return Vec::new();
    }
    match key.code {
        KeyCode::Left | KeyCode::Right => {
            state.focus = if state.focus == NetworkFocus::Wifi { NetworkFocus::Bluetooth } else { NetworkFocus::Wifi };
            state.error = None;
        }
        KeyCode::Down | KeyCode::Up => {
            let down = key.code == KeyCode::Down;
            match state.focus {
                NetworkFocus::Wifi => {
                    let len = state.wifi_rows().len();
                    step(&mut state.wifi_selected, len, down)
                }
                NetworkFocus::Bluetooth => {
                    let len = state.bluetooth.as_ref().map_or(0, |b| b.devices.len());
                    step(&mut state.bt_selected, len, down)
                }
            }
        }
        KeyCode::Char('g') => {
            state.error = None;
            state.busy = Some("Reading the radios…".to_owned());
            return vec![Effect::LoadNetwork];
        }
        KeyCode::Char('o') => return toggle_radio(app),
        KeyCode::Char('s') => {
            state.error = None;
            return match state.focus {
                NetworkFocus::Wifi if state.wifi.as_ref().is_some_and(|w| w.enabled) => {
                    state.busy = Some("Looking for Wi-Fi networks…".to_owned());
                    vec![Effect::ScanWifi]
                }
                NetworkFocus::Bluetooth if state.bluetooth.as_ref().is_some_and(|b| b.enabled) => {
                    state.busy = Some("Looking for Bluetooth devices for 8 seconds…".to_owned());
                    vec![Effect::ScanBluetooth]
                }
                _ => {
                    state.error = Some("Turn it on first: press o.".to_owned());
                    Vec::new()
                }
            };
        }
        KeyCode::Enter => {
            return match state.focus {
                NetworkFocus::Wifi => join_selected(app),
                NetworkFocus::Bluetooth => pair_or_connect(app),
            }
        }
        KeyCode::Char('f') if state.focus == NetworkFocus::Wifi => {
            let Some((ssid, _, saved)) = state.wifi_rows().get(state.wifi_selected).and_then(|r| state.wifi_row(*r))
            else {
                return Vec::new();
            };
            if !saved {
                state.error = Some(format!("{ssid} is not saved."));
                return Vec::new();
            }
            let mut body = vec![format!("The device will not join {ssid} again, and its saved key is deleted.")];
            if state.joined() == Some(ssid.as_str()) && !state.wifi.as_ref().is_some_and(|w| w.wired_up) {
                body.push("The wired link is down, so this may cut the connection you are using.".to_owned());
            }
            ask(app, format!("Forget {ssid}?"), body, "Forget", ModalAction::ForgetWifi(ssid));
        }
        KeyCode::Char('x') if state.focus == NetworkFocus::Bluetooth => {
            let Some(device) = state.bluetooth.as_ref().and_then(|b| b.devices.get(state.bt_selected)).cloned() else {
                return Vec::new();
            };
            ask(
                app,
                format!("Remove {}?", device.name),
                vec![
                    format!("{} is unpaired and forgotten.", device.name),
                    "Pairing it again needs the device in its pairing mode.".to_owned(),
                ],
                "Remove",
                ModalAction::RemoveBluetooth(device.address),
            );
        }
        _ => {}
    }
    Vec::new()
}

fn toggle_radio(app: &mut App) -> Vec<Effect> {
    let Screen::Network(state) = &mut app.screen else {
        return Vec::new();
    };
    state.error = None;
    match state.focus {
        NetworkFocus::Wifi => {
            let Some(wifi) = &state.wifi else { return Vec::new() };
            if let Some(why) = &wifi.unavailable {
                state.error = Some(why.clone());
                return Vec::new();
            }
            if wifi.enabled && wifi.link == WifiLink::Connected && !wifi.wired_up {
                ask(
                    app,
                    "Turn Wi-Fi off?".to_owned(),
                    vec!["The wired link is down, so this may cut the connection you are using.".to_owned()],
                    "Turn off",
                    ModalAction::WifiOff,
                );
                return Vec::new();
            }
            let on = !wifi.enabled;
            state.busy = Some(if on { "Turning Wi-Fi on…" } else { "Turning Wi-Fi off…" }.to_owned());
            if !on {
                state.networks.clear();
                state.wifi_selected = 0;
            }
            vec![Effect::SetWifi(on)]
        }
        NetworkFocus::Bluetooth => {
            let Some(bt) = &state.bluetooth else { return Vec::new() };
            if let Some(why) = &bt.unavailable {
                state.error = Some(why.clone());
                return Vec::new();
            }
            let on = !bt.enabled;
            state.busy = Some(if on { "Turning Bluetooth on…" } else { "Turning Bluetooth off…" }.to_owned());
            vec![Effect::SetBluetooth(on)]
        }
    }
}

fn join_selected(app: &mut App) -> Vec<Effect> {
    let Screen::Network(state) = &mut app.screen else {
        return Vec::new();
    };
    if !state.wifi.as_ref().is_some_and(|w| w.enabled) {
        state.error = Some("Wi-Fi is off. Press o to turn it on.".to_owned());
        return Vec::new();
    }
    let Some((ssid, security, _)) = state.wifi_rows().get(state.wifi_selected).and_then(|r| state.wifi_row(*r)) else {
        state.error = Some("Press s to look for networks.".to_owned());
        return Vec::new();
    };
    if state.joined() == Some(ssid.as_str()) {
        state.error = Some(format!("The device is already joined to {ssid}."));
        return Vec::new();
    }
    match security {
        WifiSecurity::Enterprise => {
            state.error = Some(format!("{ssid} needs a per-user login (WPA Enterprise), which this device does not support."));
            Vec::new()
        }
        WifiSecurity::Legacy => {
            state.error = Some(format!("{ssid} uses WEP or WPA, which are broken. Change the router to WPA2 or WPA3."));
            Vec::new()
        }
        WifiSecurity::Open => {
            ask(
                app,
                format!("Join {ssid}, an open network?"),
                vec![
                    format!("{ssid} is not encrypted: anyone nearby can read what the device sends over it."),
                    "SSH and HTTPS stay encrypted, but names it looks up and other plain traffic do not.".to_owned(),
                ],
                "Join anyway",
                ModalAction::JoinOpenWifi(ssid),
            );
            Vec::new()
        }
        _ => {
            state.error = None;
            state.prompt = Some(WifiPrompt { ssid, security, passphrase: Input::secret() });
            Vec::new()
        }
    }
}

fn pair_or_connect(app: &mut App) -> Vec<Effect> {
    let Screen::Network(state) = &mut app.screen else {
        return Vec::new();
    };
    let Some(bt) = &state.bluetooth else { return Vec::new() };
    if !bt.enabled {
        state.error = Some("Bluetooth is off. Press o to turn it on.".to_owned());
        return Vec::new();
    }
    let Some(device) = bt.devices.get(state.bt_selected).cloned() else {
        state.error = Some("Press s to look for devices. Put the device in its pairing mode first.".to_owned());
        return Vec::new();
    };
    state.error = None;
    if !device.paired {
        state.busy = Some(format!("Pairing with {}…", device.name));
        return vec![Effect::PairBluetooth(device.address)];
    }
    let connect = !device.connected;
    state.busy = Some(format!("{} {}…", if connect { "Connecting" } else { "Disconnecting" }, device.name));
    vec![Effect::ConnectBluetooth { address: device.address, connect }]
}

pub(super) fn modal_action(app: &mut App, action: ModalAction) -> Vec<Effect> {
    let Screen::Network(state) = &mut app.screen else {
        return Vec::new();
    };
    state.error = None;
    match action {
        ModalAction::JoinOpenWifi(ssid) => {
            state.busy = Some(format!("Joining {ssid}…"));
            vec![Effect::JoinWifi { ssid, security: WifiSecurity::Open, passphrase: None, accept_open: true }]
        }
        ModalAction::ForgetWifi(ssid) => {
            state.busy = Some(format!("Forgetting {ssid}…"));
            vec![Effect::ForgetWifi(ssid)]
        }
        ModalAction::WifiOff => {
            state.busy = Some("Turning Wi-Fi off…".to_owned());
            state.networks.clear();
            state.wifi_selected = 0;
            vec![Effect::SetWifi(false)]
        }
        ModalAction::RemoveBluetooth(address) => {
            state.busy = Some("Removing the device…".to_owned());
            vec![Effect::RemoveBluetooth(address)]
        }
        _ => Vec::new(),
    }
}

/// Polls while something happens in the background: a pairing waiting for the owner, or a
/// network that has not given an address yet.
pub(super) fn on_tick(app: &App) -> Vec<Effect> {
    let Screen::Network(state) = &app.screen else {
        return Vec::new();
    };
    if state.busy.is_some() || app.ticks % TICKS_PER_SECOND != 0 {
        return Vec::new();
    }
    if state.pairing_running() {
        return vec![Effect::PollBluetooth];
    }
    let waiting = state.wifi.as_ref().is_some_and(|w| {
        w.link == WifiLink::Connecting || (w.link == WifiLink::Connected && w.address.is_none())
    });
    if waiting && app.ticks % (2 * TICKS_PER_SECOND) == 0 {
        return vec![Effect::PollWifi];
    }
    Vec::new()
}

fn clamp(state: &mut NetworkState) {
    let rows = state.wifi_rows().len();
    if state.wifi_selected >= rows {
        state.wifi_selected = rows.saturating_sub(1);
    }
    let devices = state.bluetooth.as_ref().map_or(0, |b| b.devices.len());
    if state.bt_selected >= devices {
        state.bt_selected = devices.saturating_sub(1);
    }
}

pub(super) fn on_result(app: &mut App, event: Event) -> Vec<Effect> {
    let Screen::Network(state) = &mut app.screen else {
        return Vec::new();
    };
    let mut message: Option<(NoticeKind, String)> = None;
    match event {
        Event::NetworkLoaded { wifi, bluetooth } => {
            state.busy = None;
            match wifi {
                Ok(status) => state.wifi = Some(status),
                Err(e) => state.error = Some(e),
            }
            match bluetooth {
                Ok(status) => state.bluetooth = Some(status),
                Err(e) => state.error = Some(e),
            }
        }
        Event::WifiChanged { result, quiet } => {
            if !quiet {
                state.busy = None;
            }
            match result {
                Ok(status) => {
                    let joining = !quiet && state.wifi.as_ref().map(|w| (w.link, w.ssid.clone()))
                        != Some((status.link, status.ssid.clone()));
                    let gone: Vec<String> = state
                        .wifi
                        .iter()
                        .flat_map(|w| w.saved.iter())
                        .filter(|old| !status.saved.iter().any(|s| s.ssid == old.ssid))
                        .map(|old| old.ssid.clone())
                        .collect();
                    if !quiet && !gone.is_empty() {
                        message = Some((NoticeKind::Ok, format!("Forgot {}.", gone.join(", "))));
                    }
                    if joining && status.link == WifiLink::Connected {
                        let ssid = status.ssid.clone().unwrap_or_default();
                        let text = match &status.address {
                            Some(address) => format!("Joined {ssid}. Its address is {address}."),
                            None => format!("Joined {ssid}. Waiting for an address."),
                        };
                        message = Some((NoticeKind::Ok, text));
                    }
                    for network in &mut state.networks {
                        network.saved = status.saved.iter().any(|s| s.ssid == network.ssid);
                        network.connected =
                            status.link == WifiLink::Connected && status.ssid.as_deref() == Some(network.ssid.as_str());
                    }
                    state.wifi = Some(status);
                }
                Err(e) if !quiet => state.error = Some(e),
                Err(_) => {}
            }
        }
        Event::WifiScanned(result) => {
            state.busy = None;
            match result {
                Ok(list) => {
                    if list.is_empty() {
                        state.error = Some("No Wi-Fi networks are in range.".to_owned());
                    }
                    state.networks = list;
                    state.wifi_selected = 0;
                }
                Err(e) => state.error = Some(e),
            }
        }
        Event::BluetoothChanged { result, quiet } => {
            if !quiet {
                state.busy = None;
            }
            match result {
                Ok(status) => {
                    let before = state.bluetooth.as_ref().and_then(|b| b.pairing.clone());
                    if let Some(pairing) = &status.pairing {
                        if before.as_ref().map(|p| &p.stage) != Some(&pairing.stage) {
                            let name = status
                                .devices
                                .iter()
                                .find(|d| d.address == pairing.address)
                                .map_or(pairing.address.clone(), |d| d.name.clone());
                            match &pairing.stage {
                                PairingStage::Done => {
                                    message = Some((NoticeKind::Ok, format!("Paired with {name}. It is trusted and connected.")))
                                }
                                PairingStage::Failed(why) => {
                                    message = Some((NoticeKind::Error, format!("Pairing with {name} failed: {why}")))
                                }
                                _ => {}
                            }
                        }
                    }
                    state.bluetooth = Some(status);
                }
                Err(e) if !quiet => state.error = Some(e),
                Err(_) => {}
            }
        }
        _ => {}
    }
    clamp(state);
    if let Some((kind, text)) = message {
        notice(app, kind, text);
    }
    Vec::new()
}
