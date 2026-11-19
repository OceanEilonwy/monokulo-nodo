//! Keys, the clock and results for the Network screen: Wi-Fi and Bluetooth.
//!
//! What each key's action checks, asks first and does is the core's `Network`. This file maps
//! keys to those actions by the chosen row, keeps the selection inside the lists and turns the
//! tick into the seconds the core counts in.

use crossterm::event::{KeyCode, KeyEvent};

use super::{follow, push, step};
use crate::app::*;
use nodo_ui_core::flows::Step;
use nodo_ui_core::secret::Input;
use nodo_ui_core::{Effect, Outcome};

pub(super) fn open_network(app: &mut App) -> Vec<Effect> {
    let mut state = NetworkState::new();
    let effects = state.ask_again(app.ticks);
    push(app, Screen::Network(state));
    effects
}

pub(super) fn network_input(state: &mut NetworkState) -> Option<&mut Input> {
    state.flow.prompt.is_some().then_some(&mut state.passphrase)
}

pub(super) fn network_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Network(state) = &mut app.screen else {
        return Vec::new();
    };
    if key.code == KeyCode::Esc {
        if state.flow.cancel_prompt() {
            state.passphrase.clear();
            return Vec::new();
        }
        let next = state.flow.leave();
        return follow(app, next);
    }
    // Works even while the first answer is still awaited, since that may never come.
    if key.code == KeyCode::Char('r') && state.can_ask_again() {
        return state.ask_again(app.ticks);
    }
    if state.flow.busy.is_some() {
        return Vec::new();
    }
    if state.flow.prompt.is_some() {
        if key.code == KeyCode::Enter {
            return state.flow.submit_passphrase(&mut state.passphrase);
        }
        super::edit_input(&mut state.passphrase, key);
        return Vec::new();
    }
    let wifi = state.focus == NetworkFocus::Wifi;
    // The chosen row by name. With nothing listed there is no name, and the core says so.
    let ssid = state.wifi_rows().get(state.wifi_selected).and_then(|row| state.wifi_row(*row)).map(|(ssid, ..)| ssid);
    let device = state.flow.bluetooth.as_ref().and_then(|b| b.devices.get(state.bt_selected)).map(|d| d.address.clone());
    let next = match key.code {
        KeyCode::Left | KeyCode::Right => {
            state.focus = if wifi { NetworkFocus::Bluetooth } else { NetworkFocus::Wifi };
            state.flow.error = None;
            Step::Stay
        }
        KeyCode::Down | KeyCode::Up => {
            let down = key.code == KeyCode::Down;
            if wifi {
                let len = state.wifi_rows().len();
                step(&mut state.wifi_selected, len, down);
            } else {
                let len = state.flow.bluetooth.as_ref().map_or(0, |b| b.devices.len());
                step(&mut state.bt_selected, len, down);
            }
            Step::Stay
        }
        KeyCode::Char('g') => return state.ask_again(app.ticks),
        KeyCode::Char('o') if wifi => {
            let next = state.flow.toggle_wifi();
            // The scan goes with the radio, and the cursor with the scan.
            if next.effects().contains(&Effect::SetWifi(false)) {
                state.wifi_selected = 0;
            }
            next
        }
        KeyCode::Char('o') => Step::run(state.flow.toggle_bluetooth()),
        KeyCode::Char('s') if wifi => Step::run(state.flow.scan_wifi()),
        KeyCode::Char('s') => Step::run(state.flow.scan_bluetooth()),
        KeyCode::Enter if wifi => {
            let next = state.flow.join(&ssid.unwrap_or_default());
            if state.flow.prompt.is_some() {
                state.passphrase = Input::secret();
            }
            next
        }
        KeyCode::Enter => Step::run(state.flow.pair_or_connect(&device.unwrap_or_default())),
        KeyCode::Char('f') if wifi => state.flow.ask_forget(&ssid.unwrap_or_default()),
        KeyCode::Char('x') if !wifi => state.flow.ask_remove_bluetooth(&device.unwrap_or_default()),
        _ => Step::Stay,
    };
    follow(app, next)
}

/// Polls while something happens in the background, by the core's cadence, and tells the core
/// how long the radios have been asked for.
pub(super) fn on_tick(app: &mut App) -> Vec<Effect> {
    let Screen::Network(state) = &mut app.screen else {
        return Vec::new();
    };
    state.flow.no_answer_after(app.ticks.saturating_sub(state.asked_at) / TICKS_PER_SECOND);
    if app.ticks % TICKS_PER_SECOND != 0 {
        return Vec::new();
    }
    state.flow.poll(app.ticks / TICKS_PER_SECOND)
}

pub(super) fn on_result(app: &mut App, event: Outcome) -> Vec<Effect> {
    let Screen::Network(state) = &mut app.screen else {
        return Vec::new();
    };
    let next = state.flow.apply(&event);
    if matches!(event, Outcome::WifiScanned(Ok(_))) {
        state.wifi_selected = 0;
    }
    state.clamp();
    follow(app, next)
}
