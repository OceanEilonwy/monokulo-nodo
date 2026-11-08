//! Keys and results for the Onion, Restore and Rollbacks screens.
//!
//! The same rules as the rest of `update`: safe choices first, typed confirmation before anything
//! is overwritten, and the helper checks every rule again.

use crossterm::event::{KeyCode, KeyEvent};
use nodo_api::UpdateOutcome;

use super::{edit_input, fail, notice, open_logs, pop, push, step};
use crate::app::*;
use crate::secret::Input;

// Entry points

pub(super) fn open_onion(app: &mut App) -> Vec<Effect> {
    push(app, Screen::Onion(OnionState::new()));
    vec![Effect::LoadOnion]
}

pub(super) fn open_restore(app: &mut App) -> Vec<Effect> {
    push(app, Screen::Restore(RestoreState::new()));
    vec![Effect::LoadRestoreDrives]
}

pub(super) fn open_rollbacks(app: &mut App) -> Vec<Effect> {
    push(app, Screen::Rollbacks(RollbacksState::new()));
    vec![Effect::LoadRollbacks]
}

// Onion

/// The node's onion address only ever serves the restricted RPC, which cuprated always has. A site
/// that works from a single address cannot add an onion one beside another.
pub(super) fn onion_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Onion(state) = &mut app.screen else {
        return Vec::new();
    };
    if key.code == KeyCode::Esc {
        pop(app);
        return vec![Effect::Refresh];
    }
    if state.busy {
        return Vec::new();
    }
    let rows = state.rows();
    match key.code {
        KeyCode::Down | KeyCode::Up => step(&mut state.selected, rows.len(), key.code == KeyCode::Down),
        KeyCode::Char('g') => {
            state.busy = true;
            state.error = None;
            return vec![Effect::LoadOnion];
        }
        KeyCode::Char('s') => {
            if let (Some(OnionRow::Node), Some(node)) =
                (rows.get(state.selected), state.status.as_ref().and_then(|s| s.node.as_ref()))
            {
                return vec![Effect::OpenSettings(node.provider.clone())];
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            let Some(status) = state.status.clone() else { return Vec::new() };
            match rows.get(state.selected) {
                Some(OnionRow::Node) => {
                    let Some(node) = &status.node else { return Vec::new() };
                    // cuprated always serves restricted RPC. Another node that does not say which
                    // port is restricted cannot go on Tor, and no setting changes that.
                    if !node.enabled && !node.restricted_rpc {
                        state.error = Some(format!(
                            "{} does not say which port serves its restricted RPC, so it cannot have \
                             an onion address. The onion address only ever serves the restricted RPC.",
                            node.provider
                        ));
                        return Vec::new();
                    }
                    state.busy = true;
                    state.error = None;
                    return vec![Effect::SetNodeOnion(!node.enabled)];
                }
                Some(OnionRow::Site(index)) => {
                    let Some(site) = status.sites.get(*index) else { return Vec::new() };
                    if site.locked && !site.exposure.onion {
                        state.error = Some(format!("{} is locked. Unlock the device first.", site.id.package));
                        return Vec::new();
                    }
                    let mut exposure = site.exposure;
                    exposure.onion = !exposure.onion;
                    if exposure.onion && !site.multi_address && (exposure.local || exposure.public) {
                        state.error = Some(format!(
                            "{} works from a single address. Turn its other address off under Web first.",
                            site.id.package
                        ));
                        return Vec::new();
                    }
                    state.busy = true;
                    state.error = None;
                    return vec![Effect::SetSiteExposure { site: site.id.clone(), exposure }];
                }
                None => {}
            }
        }
        _ => {}
    }
    Vec::new()
}

// Restore

pub(super) fn restore_input(step: &mut RestoreStep) -> Option<&mut Input> {
    match step {
        RestoreStep::Passphrase { passphrase, .. } => Some(passphrase),
        RestoreStep::Confirm { typed, first, second, field, .. } => Some(match field {
            0 => typed,
            1 => first,
            _ => second,
        }),
        _ => None,
    }
}

/// Rebuilds a device that is not set up from a USB backup: choose the drive, then the backup,
/// open it with its passphrase, see what comes back, type the word and a new device passphrase
/// twice. Nothing changes until then, and a failure leaves the device not set up.
pub(super) fn restore_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let ticks = app.ticks;
    let Screen::Restore(state) = &mut app.screen else {
        return Vec::new();
    };
    // Leaving the drive list is always safe, even while it is being read again. A late answer
    // then finds another screen and is dropped.
    if state.busy && key.code == KeyCode::Esc && state.step == RestoreStep::Drives {
        pop(app);
        return Vec::new();
    }
    if state.busy {
        return Vec::new();
    }
    let mut effects = Vec::new();
    let mut next = None;
    let mut leave = false;
    let mut finished = false;
    match &mut state.step {
        RestoreStep::Drives => match key.code {
            KeyCode::Down | KeyCode::Up => step(&mut state.selected, state.drives.len(), key.code == KeyCode::Down),
            KeyCode::Char('g') => {
                state.busy = true;
                effects.push(Effect::LoadRestoreDrives);
            }
            KeyCode::Enter => match state.drives.get(state.selected) {
                Some(drive) if drive.problem.is_some() => {
                    let problem = drive.problem.clone().unwrap_or_default();
                    state.error = Some(format!("{} cannot be used: {problem}", drive.device));
                    return effects;
                }
                Some(drive) => {
                    let device = drive.device.clone();
                    state.busy = true;
                    state.backups = None;
                    effects.push(Effect::LoadRestoreBackups(device.clone()));
                    next = Some(RestoreStep::Backups { device });
                }
                None => {
                    state.error = Some("No USB drive is plugged in. Plug one in and press g.".to_owned());
                    return effects;
                }
            },
            KeyCode::Esc => leave = true,
            _ => {}
        },
        RestoreStep::Backups { device } => {
            let len = state.backups.as_ref().map_or(0, |b| b.backups.len());
            match key.code {
                KeyCode::Down | KeyCode::Up => step(&mut state.selected, len, key.code == KeyCode::Down),
                KeyCode::Char('g') => {
                    state.busy = true;
                    effects.push(Effect::LoadRestoreBackups(device.clone()));
                }
                KeyCode::Enter => {
                    match state.backups.as_ref().and_then(|b| b.backups.get(state.selected)) {
                        Some(backup) => {
                            next = Some(RestoreStep::Passphrase {
                                device: device.clone(),
                                name: backup.name.clone(),
                                passphrase: Input::secret(),
                            })
                        }
                        None => {
                            state.error = Some("This drive holds no monokulo-nodo backups.".to_owned());
                            return effects;
                        }
                    }
                }
                KeyCode::Esc => {
                    state.selected = 0;
                    next = Some(RestoreStep::Drives);
                }
                _ => {}
            }
        }
        RestoreStep::Passphrase { device, name, passphrase } => match key.code {
            KeyCode::Esc => {
                let device = device.clone();
                next = Some(RestoreStep::Backups { device });
            }
            KeyCode::Enter if passphrase.is_empty() => {
                state.error = Some("Type the backup passphrase.".to_owned());
                return effects;
            }
            KeyCode::Enter => {
                state.busy = true;
                state.error = None;
                effects.push(Effect::InspectRestore {
                    device: device.clone(),
                    name: name.clone(),
                    passphrase: passphrase.take_secret(),
                });
            }
            _ => {
                edit_input(passphrase, key);
            }
        },
        RestoreStep::Confirm { device, name, passphrase, typed, first, second, field, .. } => match key.code {
            KeyCode::Esc => {
                let device = device.clone();
                next = Some(RestoreStep::Backups { device });
            }
            KeyCode::Tab | KeyCode::Down => *field = (*field + 1) % 3,
            KeyCode::BackTab | KeyCode::Up => *field = (*field + 2) % 3,
            KeyCode::Enter => {
                if typed.text().trim() != RESTORE_WORD {
                    state.error = Some(format!("Type {RESTORE_WORD} to confirm."));
                    *field = 0;
                } else if first.secret_value().expose().chars().count() < MIN_PASSPHRASE_CHARS {
                    state.error = Some(format!(
                        "Choose a new device passphrase of at least {MIN_PASSPHRASE_CHARS} characters."
                    ));
                    *field = 1;
                } else if first.secret_value() != second.secret_value() {
                    state.error = Some("The two device passphrases differ. Type them again.".to_owned());
                    first.clear();
                    second.clear();
                    *field = 1;
                } else {
                    state.busy = true;
                    second.clear();
                    effects.push(Effect::RunRestore {
                        device: device.clone(),
                        name: name.clone(),
                        passphrase: passphrase.clone(),
                        new_device_passphrase: first.take_secret(),
                        typed: RESTORE_WORD.to_owned(),
                    });
                    next = Some(RestoreStep::Restoring { device: device.clone(), name: name.clone(), started: ticks });
                }
                if next.is_none() {
                    return effects;
                }
            }
            _ => {
                let input = match field {
                    0 => typed,
                    1 => first,
                    _ => second,
                };
                edit_input(input, key);
            }
        },
        RestoreStep::Restoring { .. } => {}
        RestoreStep::Failed { device, name, .. } => match key.code {
            KeyCode::Enter => {
                next = Some(RestoreStep::Passphrase {
                    device: device.clone(),
                    name: name.clone(),
                    passphrase: Input::secret(),
                })
            }
            KeyCode::Esc => {
                state.selected = 0;
                state.busy = true;
                effects.push(Effect::LoadRestoreDrives);
                next = Some(RestoreStep::Drives);
            }
            _ => {}
        },
        RestoreStep::Done(_) => {
            if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                finished = true;
            }
        }
    }
    if let Some(step) = next {
        state.error = None;
        state.step = step;
    }
    if finished {
        // Set up from the backup. SSH keys are offered next, as after a first-boot setup.
        pop(app);
        if let Screen::Setup(setup) = &mut app.screen {
            *setup = SetupState::keys();
        }
        return vec![Effect::LoadSshKeys, Effect::Refresh];
    }
    if leave {
        pop(app);
    }
    effects
}

// Rollbacks

pub(super) fn rollbacks_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Rollbacks(state) = &mut app.screen else {
        return Vec::new();
    };
    if key.code == KeyCode::Esc {
        pop(app);
        return vec![Effect::Refresh];
    }
    if state.busy {
        return Vec::new();
    }
    let options = state.view.as_ref().map(|v| v.options.clone()).unwrap_or_default();
    let chosen = options.get(state.selected).cloned();
    match key.code {
        KeyCode::Down | KeyCode::Up => step(&mut state.selected, options.len(), key.code == KeyCode::Down),
        KeyCode::Char('g') => {
            state.busy = true;
            state.error = None;
            return vec![Effect::LoadRollbacks];
        }
        KeyCode::Char('l') => {
            if let Some(option) = chosen {
                return open_logs(app, &option.package);
            }
        }
        KeyCode::Enter | KeyCode::Char('r') => {
            let Some(option) = chosen else {
                state.error = Some("Nothing can be rolled back now.".to_owned());
                return Vec::new();
            };
            if let Some(reason) = &option.blocked {
                state.error = Some(format!("{} cannot roll back now: {reason}.", option.package));
                return Vec::new();
            }
            app.modal = Some(Modal {
                title: format!("Roll {} back?", option.package),
                body: vec![
                    format!("{} goes from {} back to {}.", option.package, option.current, option.previous),
                    "Its data is copied first and put back if the old version fails.".to_owned(),
                    format!("{} is held, so automatic updates leave it alone.", option.current),
                ],
                confirm_label: "Roll back".to_owned(),
                confirm_focused: false,
                action: ModalAction::RollBack { package: option.package, to: option.previous },
            });
        }
        _ => {}
    }
    Vec::new()
}

pub(super) fn roll_back(app: &mut App, package: String, to: String) -> Vec<Effect> {
    if let Screen::Rollbacks(state) = &mut app.screen {
        state.busy = true;
        state.error = None;
        state.result = None;
    }
    vec![Effect::RollBack { package, to }]
}

// Results

pub(super) fn on_result(app: &mut App, event: Event) -> Vec<Effect> {
    match event {
        Event::OnionLoaded(result) => {
            if let Screen::Onion(state) = &mut app.screen {
                state.busy = false;
                match result {
                    Ok(status) => {
                        state.status = Some(status);
                        let len = state.rows().len();
                        state.selected = state.selected.min(len.saturating_sub(1));
                    }
                    Err(e) => state.error = Some(e),
                }
            }
        }
        Event::OnionChanged(result) => {
            if let Screen::Onion(state) = &mut app.screen {
                match result {
                    Ok(()) => notice(app, NoticeKind::Ok, "Onion exposure changed."),
                    Err(e) => state.error = Some(e),
                }
            }
        }
        Event::RestoreDrives(result) => {
            if let Screen::Restore(state) = &mut app.screen {
                state.busy = false;
                match result {
                    Ok(drives) => {
                        state.selected = state.selected.min(drives.len().saturating_sub(1));
                        state.drives = drives;
                        state.error = None;
                    }
                    Err(e) => state.error = Some(e),
                }
            }
        }
        Event::RestoreBackups(result) => {
            if let Screen::Restore(state) = &mut app.screen {
                state.busy = false;
                match result {
                    Ok(list) => {
                        state.selected = 0;
                        state.backups = Some(list);
                    }
                    Err(e) => {
                        state.step = RestoreStep::Drives;
                        state.error = Some(e);
                    }
                }
            }
        }
        Event::RestoreInspected(result) => {
            if let Screen::Restore(state) = &mut app.screen {
                state.busy = false;
                if let RestoreStep::Passphrase { device, name, passphrase } = &mut state.step {
                    match result {
                        Ok(contents) => {
                            state.step = RestoreStep::Confirm {
                                device: device.clone(),
                                name: name.clone(),
                                passphrase: passphrase.take_secret(),
                                contents,
                                typed: Input::plain(""),
                                first: Input::secret(),
                                second: Input::secret(),
                                field: 0,
                            };
                        }
                        Err(e) => {
                            passphrase.clear();
                            state.error = Some(e);
                        }
                    }
                }
            }
        }
        Event::RestoreFinished(result) => {
            if let Screen::Restore(state) = &mut app.screen {
                state.busy = false;
                let (device, name) = match &state.step {
                    RestoreStep::Restoring { device, name, .. } => (device.clone(), name.clone()),
                    _ => (String::new(), String::new()),
                };
                state.step = match result {
                    Ok(report) => RestoreStep::Done(report),
                    Err(reason) => RestoreStep::Failed { device, name, reason },
                };
            }
        }
        Event::RollbacksLoaded(result) => {
            if let Screen::Rollbacks(state) = &mut app.screen {
                state.busy = false;
                match result {
                    Ok(view) => {
                        state.selected = state.selected.min(view.options.len().saturating_sub(1));
                        state.view = Some(view);
                    }
                    Err(e) => state.error = Some(e),
                }
            }
        }
        Event::RolledBack(result) => {
            if let Screen::Rollbacks(state) = &mut app.screen {
                match result {
                    Ok(outcome) => {
                        let text = match &outcome.outcome {
                            UpdateOutcome::Updated => {
                                Some(format!("{} is back on {}.", outcome.package, outcome.to))
                            }
                            _ => None,
                        };
                        state.result = Some(outcome);
                        state.busy = true;
                        if let Some(text) = text {
                            notice(app, NoticeKind::Ok, text);
                        }
                        return vec![Effect::LoadRollbacks, Effect::Refresh];
                    }
                    Err(e) => {
                        state.busy = false;
                        fail(app, e);
                    }
                }
            }
        }
        _ => {}
    }
    Vec::new()
}
