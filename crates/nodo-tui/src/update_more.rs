//! Keys and results for the Onion, Restore and Rollbacks screens.
//!
//! The same rules as the rest of `update`: safe choices first, typed confirmation before anything
//! is overwritten, and the helper checks every rule again.

use crossterm::event::{KeyCode, KeyEvent};

use super::{edit_input, follow, open_logs, push, step};
use nodo_ui_core::secret::Input;
use crate::app::*;
use nodo_ui_core::flows::backup::{self, Restore};
use nodo_ui_core::flows::updates::Rollbacks;
use nodo_ui_core::flows::Step;
use nodo_ui_core::flows::web::onion::{Onion, Row};
use nodo_ui_core::{Effect, Outcome};

// Entry points

pub(super) fn open_onion(app: &mut App) -> Vec<Effect> {
    push(app, Screen::Onion(OnionState::new()));
    Onion::load()
}

pub(super) fn open_restore(app: &mut App) -> Vec<Effect> {
    push(app, Screen::Restore(RestoreState::new()));
    Restore::load()
}

pub(super) fn open_rollbacks(app: &mut App) -> Vec<Effect> {
    push(app, Screen::Rollbacks(RollbacksState::new()));
    Rollbacks::load()
}

// Onion

pub(super) fn onion_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Onion(state) = &mut app.screen else {
        return Vec::new();
    };
    if key.code == KeyCode::Esc {
        let step = state.leave();
        return follow(app, step);
    }
    if state.busy {
        return Vec::new();
    }
    let rows = state.rows();
    let chosen = rows.get(state.selected).copied();
    match key.code {
        KeyCode::Down | KeyCode::Up => step(&mut state.selected, rows.len(), key.code == KeyCode::Down),
        KeyCode::Char('g') => return state.flow.reload(),
        KeyCode::Char('s') if chosen == Some(Row::Node) => return state.node_settings(),
        KeyCode::Enter | KeyCode::Char(' ') => match chosen {
            Some(Row::Node) => return state.flow.toggle_node(),
            Some(Row::Site(index)) => {
                let id = state.status.as_ref().and_then(|status| status.sites.get(index)).map(|site| site.id.clone());
                if let Some(id) = id {
                    return state.flow.toggle_site(&id);
                }
            }
            None => {}
        },
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

/// The first-boot restore: the keys of each step. What each step asks, checks and does is the
/// core's `Restore`.
pub(super) fn restore_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    use backup::Field;
    let ticks = app.ticks;
    let Screen::Restore(state) = &mut app.screen else {
        return Vec::new();
    };
    // Leaving the drive list is always safe, even while it is being read again.
    if state.flow.busy && !(key.code == KeyCode::Esc && state.step == RestoreStep::Drives) {
        return Vec::new();
    }
    let mut next = Step::Stay;
    match &mut state.step {
        RestoreStep::Drives => match key.code {
            KeyCode::Down | KeyCode::Up => step(&mut state.selected, state.flow.drives.len(), key.code == KeyCode::Down),
            KeyCode::Char('g') => next = Step::run(state.flow.reload()),
            KeyCode::Enter => {
                // With no drive listed there is no name, and the core says so.
                let device = state.flow.drives.get(state.selected).map(|drive| drive.device.clone()).unwrap_or_default();
                next = Step::run(state.flow.choose_drive(&device));
            }
            KeyCode::Esc => next = state.flow.back(),
            _ => {}
        },
        RestoreStep::Backups { .. } => {
            let len = state.flow.backups.as_ref().map_or(0, |b| b.backups.len());
            match key.code {
                KeyCode::Down | KeyCode::Up => step(&mut state.selected, len, key.code == KeyCode::Down),
                KeyCode::Char('g') => next = Step::run(state.flow.reload()),
                KeyCode::Enter => {
                    let name = state.flow.backups.as_ref().and_then(|b| b.backups.get(state.selected)).map(|b| b.name.clone());
                    state.flow.choose_backup(&name.unwrap_or_default());
                }
                KeyCode::Esc => {
                    state.selected = 0;
                    next = state.flow.back();
                }
                _ => {}
            }
        }
        RestoreStep::Passphrase { passphrase, .. } => match key.code {
            KeyCode::Esc => next = state.flow.back(),
            KeyCode::Enter => next = Step::run(state.flow.inspect(passphrase)),
            _ => {
                edit_input(passphrase, key);
            }
        },
        RestoreStep::Confirm { typed, first, second, field, .. } => match key.code {
            KeyCode::Esc => next = state.flow.back(),
            KeyCode::Tab | KeyCode::Down => *field = (*field + 1) % 3,
            KeyCode::BackTab | KeyCode::Up => *field = (*field + 2) % 3,
            KeyCode::Enter => match state.flow.restore(typed.text(), first, second) {
                Ok(effects) => next = Step::run(effects),
                Err(Some(Field::Word)) => *field = 0,
                Err(Some(_)) => *field = 1,
                Err(None) => {}
            },
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
        RestoreStep::Failed { .. } => match key.code {
            KeyCode::Enter => state.flow.retry(),
            KeyCode::Esc => {
                state.selected = 0;
                next = state.flow.back();
            }
            _ => {}
        },
        RestoreStep::Done(_) => {
            if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                next = state.flow.finish();
            }
        }
    }
    state.sync(ticks);
    follow(app, next)
}

// Rollbacks
// Rollbacks

pub(super) fn rollbacks_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Rollbacks(state) = &mut app.screen else {
        return Vec::new();
    };
    if key.code == KeyCode::Esc {
        let step = state.leave();
        return follow(app, step);
    }
    if state.busy {
        return Vec::new();
    }
    let count = state.options().len();
    let chosen = state.options().get(state.selected).map(|option| option.package.clone());
    match key.code {
        KeyCode::Down | KeyCode::Up => step(&mut state.selected, count, key.code == KeyCode::Down),
        KeyCode::Char('g') => return state.flow.reload(),
        KeyCode::Char('l') => {
            if let Some(package) = chosen {
                return open_logs(app, &package);
            }
        }
        KeyCode::Enter | KeyCode::Char('r') => {
            // With nothing listed there is no name, and the core says so.
            let step = state.flow.roll_back(chosen.as_deref().unwrap_or_default());
            return follow(app, step);
        }
        _ => {}
    }
    Vec::new()
}

pub(super) fn on_result(app: &mut App, event: Outcome) -> Vec<Effect> {
    match event {
        Outcome::OnionLoaded(_) | Outcome::OnionChanged(_) => {
            if let Screen::Onion(state) = &mut app.screen {
                let step = state.flow.apply(&event);
                let len = state.rows().len();
                state.selected = state.selected.min(len.saturating_sub(1));
                return follow(app, step);
            }
        }
        Outcome::RestoreDrives(_) | Outcome::RestoreBackups(_) | Outcome::RestoreInspected(_) | Outcome::RestoreFinished(_) => {
            let ticks = app.ticks;
            if let Screen::Restore(state) = &mut app.screen {
                let next = state.flow.apply(&event);
                state.sync(ticks);
                match event {
                    Outcome::RestoreDrives(Ok(_)) => state.selected = state.selected.min(state.flow.drives.len().saturating_sub(1)),
                    Outcome::RestoreBackups(Ok(_)) => state.selected = 0,
                    _ => {}
                }
                return follow(app, next);
            }
        }
        _ => {}
    }
    Vec::new()
}
