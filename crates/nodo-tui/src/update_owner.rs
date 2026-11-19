//! Keys and results for installing a bundle of apps, and for an app's setup page for the owner.
//!
//! The steps, what each one checks and what a result means are the core's `Bundle` and
//! `OwnerPage`. This file maps keys to their actions and keeps the focus and the scroll.

use crossterm::event::{KeyCode, KeyEvent};
use nodo_api::ApiError;

use super::{edit_input, follow, notice, push, step};
use crate::app::*;
use nodo_ui_core::flows::bundle::{self, BundleStep};
use nodo_ui_core::flows::owner_page::{self, OwnerPage};
use nodo_ui_core::flows::{NoticeKind, Step};
use nodo_ui_core::{Effect, Outcome};

/// Rows a page key scrolls.
const PAGE: usize = 10;

fn scroll(at: &mut usize, key: KeyCode) {
    match key {
        KeyCode::PageDown => *at += PAGE,
        KeyCode::PageUp => *at = at.saturating_sub(PAGE),
        KeyCode::Home => *at = 0,
        _ => {}
    }
}

/// The review of a bundle opens on its own screen, with Cancel focused so a stray Enter cancels.
pub(super) fn open_bundle(app: &mut App, review: nodo_api::BundleReview) {
    push(app, Screen::Bundle(Box::new(BundleState::new(review))));
}

pub(super) fn bundle_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Bundle(state) = &mut app.screen else { return Vec::new() };
    let cancel = state.cancel_index();
    let mut next = Step::Stay;
    match state.flow.step.clone() {
        BundleStep::Review => match key.code {
            KeyCode::Esc => next = state.flow.back(),
            KeyCode::Down | KeyCode::Tab => step(&mut state.focus, cancel + 2, true),
            KeyCode::Up | KeyCode::BackTab => step(&mut state.focus, cancel + 2, false),
            KeyCode::Left if state.focus > cancel => state.focus = cancel,
            KeyCode::Right if state.focus >= cancel => state.focus = cancel + 1,
            KeyCode::PageDown | KeyCode::PageUp | KeyCode::Home => scroll(&mut state.scroll, key.code),
            KeyCode::Char(' ') | KeyCode::Enter if state.focus < cancel => {
                let app_id = state.flow.review.data_drive[state.focus].clone();
                state.flow.toggle_drive(&app_id);
            }
            KeyCode::Enter if state.focus == cancel => next = state.flow.back(),
            KeyCode::Enter => {
                state.scroll = 0;
                next = Step::run(state.flow.proceed());
                if state.flow.step == BundleStep::Answers {
                    state.focus = 0;
                }
            }
            _ => {}
        },
        BundleStep::Answers => {
            let open = state.flow.open_asks();
            match key.code {
                KeyCode::Esc => {
                    next = state.flow.back();
                    state.focus = cancel + 1;
                }
                KeyCode::Down | KeyCode::Tab => step(&mut state.focus, open.len(), true),
                KeyCode::Up | KeyCode::BackTab => step(&mut state.focus, open.len(), false),
                KeyCode::Enter if state.focus + 1 < open.len() => state.focus += 1,
                KeyCode::Enter => match state.flow.answer(&state.answers) {
                    Ok(effects) => next = Step::run(effects),
                    // The focus goes to the ask without an answer.
                    Err(empty) => state.focus = open.iter().position(|&i| i == empty).unwrap_or(0),
                },
                _ => {
                    if let Some(&index) = open.get(state.focus) {
                        edit_input(&mut state.answers[index], key);
                    }
                }
            }
        }
        BundleStep::Passphrase => match key.code {
            KeyCode::Esc => {
                state.passphrase.clear();
                next = state.flow.back();
                state.focus = if state.flow.step == BundleStep::Review { cancel + 1 } else { 0 };
            }
            KeyCode::Enter => next = Step::run(state.flow.submit_passphrase(&mut state.passphrase)),
            _ => {
                edit_input(&mut state.passphrase, key);
            }
        },
        BundleStep::Installing => {}
        BundleStep::Done(report) => match key.code {
            KeyCode::PageDown | KeyCode::PageUp | KeyCode::Home => scroll(&mut state.scroll, key.code),
            KeyCode::Down => state.scroll += 1,
            KeyCode::Up => state.scroll = state.scroll.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('o') if report.problem.is_none() => next = state.flow.finish(&app.data),
            KeyCode::Esc | KeyCode::Enter => next = state.flow.back(),
            _ => {}
        },
    }
    if state.flow.step == BundleStep::Installing {
        state.scroll = 0;
    }
    follow(app, next)
}

pub(super) fn on_bundle_installed(app: &mut App, outcome: &Outcome) -> Vec<Effect> {
    let Screen::Bundle(state) = &mut app.screen else {
        let next = bundle::apply(outcome);
        return follow(app, next);
    };
    let next = state.flow.apply(outcome);
    match state.flow.step {
        BundleStep::Done(_) => state.scroll = 0,
        // A refusal returns to the review, on Continue.
        BundleStep::Review => state.focus = state.cancel_index() + 1,
        _ => {}
    }
    follow(app, next)
}

/// Opens an app's setup page, which first asks for the device passphrase.
pub(super) fn open_owner_page(app: &mut App, package: &str) -> Vec<Effect> {
    let next = owner_page::open(&app.data, package);
    follow(app, next)
}

pub(super) fn show_owner_page(app: &mut App, page: OwnerPage) {
    push(app, Screen::OwnerPage(Box::new(OwnerPageState::of(page))));
}

pub(super) fn owner_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::OwnerPage(state) = &mut app.screen else { return Vec::new() };
    if key.code == KeyCode::Esc {
        // The page, and with it the passphrase held for `r`, is dropped and wiped.
        return follow(app, Step::Go(nodo_ui_core::flows::Dest::Back));
    }
    if state.flow.view.is_none() {
        match key.code {
            KeyCode::Enter => return state.flow.unlock(&mut state.passphrase),
            _ => {
                edit_input(&mut state.passphrase, key);
            }
        }
        return Vec::new();
    }
    let targets = state.flow.copy_targets();
    match key.code {
        KeyCode::Down => {
            state.scroll += 1;
            state.follow = false;
        }
        KeyCode::Up => {
            state.scroll = state.scroll.saturating_sub(1);
            state.follow = false;
        }
        KeyCode::PageDown | KeyCode::PageUp | KeyCode::Home => {
            scroll(&mut state.scroll, key.code);
            state.follow = false;
        }
        KeyCode::Tab if !targets.is_empty() => {
            state.focus = (state.focus + 1) % targets.len();
            state.follow = true;
        }
        KeyCode::BackTab if !targets.is_empty() => {
            state.focus = (state.focus + targets.len() - 1) % targets.len();
            state.follow = true;
        }
        KeyCode::Char('c') | KeyCode::Enter => {
            if let Some((label, value)) = targets.get(state.focus) {
                app.clipboard = Some(value.clone());
                state.copied = Some(state.focus);
                let label = label.clone();
                notice(app, NoticeKind::Ok, format!("Copied {label}. If your terminal does not allow it, select the text."));
            }
        }
        KeyCode::Char('r') => return state.flow.refresh(),
        _ => {}
    }
    Vec::new()
}

pub(super) fn on_owner_page(app: &mut App, outcome: &Outcome) -> Vec<Effect> {
    let Screen::OwnerPage(state) = &mut app.screen else { return Vec::new() };
    let Outcome::OwnerPageLoaded { package, result } = outcome else { return Vec::new() };
    if state.flow.package != *package {
        return Vec::new();
    }
    let next = state.flow.apply(outcome);
    match result {
        Ok(_) => {
            let count = state.flow.copy_targets().len();
            state.focus = state.focus.min(count.saturating_sub(1));
            state.copied = None;
        }
        // The gate shows again, with an empty box.
        Err(ApiError::BadPassphrase) => state.passphrase.clear(),
        Err(_) => {}
    }
    follow(app, next)
}

/// A click on a bundle's toggle or button, or a setup page's Copy, moves the focus there.
pub(super) fn pick(app: &mut App, index: usize) {
    match &mut app.screen {
        Screen::Bundle(state) => state.focus = index,
        Screen::OwnerPage(state) => {
            state.focus = index;
            state.follow = false;
        }
        _ => {}
    }
}
