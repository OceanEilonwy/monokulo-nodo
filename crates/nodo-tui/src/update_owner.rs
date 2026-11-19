//! Installing a bundle of apps, and an app's setup page for the owner.

use crossterm::event::{KeyCode, KeyEvent};
use nodo_api::ApiError;

use super::{edit_input, fail, notice, pop, push, step};
use crate::app::*;
use nodo_ui_core::Effect;

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
    match state.step.clone() {
        BundleStep::Review => {
            let cancel = state.cancel_index();
            match key.code {
                KeyCode::Esc => pop(app),
                KeyCode::Down | KeyCode::Tab => step(&mut state.focus, cancel + 2, true),
                KeyCode::Up | KeyCode::BackTab => step(&mut state.focus, cancel + 2, false),
                KeyCode::Left if state.focus > cancel => state.focus = cancel,
                KeyCode::Right if state.focus >= cancel => state.focus = cancel + 1,
                KeyCode::PageDown | KeyCode::PageUp | KeyCode::Home => scroll(&mut state.scroll, key.code),
                KeyCode::Char(' ') | KeyCode::Enter if state.focus < cancel => {
                    let on = &mut state.drive[state.focus];
                    *on = !*on;
                }
                KeyCode::Enter if state.focus == cancel => pop(app),
                KeyCode::Enter => return after_review(state),
                _ => {}
            }
        }
        BundleStep::Answers => {
            let open = state.open_asks();
            match key.code {
                KeyCode::Esc => {
                    state.step = BundleStep::Review;
                    state.focus = state.cancel_index() + 1;
                    state.error = None;
                }
                KeyCode::Down | KeyCode::Tab => step(&mut state.focus, open.len(), true),
                KeyCode::Up | KeyCode::BackTab => step(&mut state.focus, open.len(), false),
                KeyCode::Enter if state.focus + 1 < open.len() => state.focus += 1,
                KeyCode::Enter => {
                    if let Some(&empty) = open.iter().find(|&&i| state.answers[i].is_empty()) {
                        state.error = Some(format!("Fill in {} first.", state.review.asks[empty].label));
                        state.focus = open.iter().position(|&i| i == empty).unwrap_or(0);
                        return Vec::new();
                    }
                    state.error = None;
                    return after_answers(state);
                }
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
                state.error = None;
                if state.open_asks().is_empty() {
                    state.step = BundleStep::Review;
                    state.focus = state.cancel_index() + 1;
                } else {
                    state.step = BundleStep::Answers;
                    state.focus = 0;
                }
            }
            KeyCode::Enter if state.passphrase.is_empty() => state.error = Some("Type the device passphrase.".to_owned()),
            KeyCode::Enter => return start(state),
            _ => {
                edit_input(&mut state.passphrase, key);
            }
        },
        BundleStep::Installing => {}
        BundleStep::Done(report) => match key.code {
            KeyCode::PageDown | KeyCode::PageUp | KeyCode::Home => scroll(&mut state.scroll, key.code),
            KeyCode::Down => state.scroll += 1,
            KeyCode::Up => state.scroll = state.scroll.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('o') if report.problem.is_none() => {
                let head = report.package.clone();
                pop(app);
                return open_owner_page(app, &head);
            }
            KeyCode::Esc | KeyCode::Enter => pop(app),
            _ => {}
        },
    }
    Vec::new()
}

fn after_review(state: &mut BundleState) -> Vec<Effect> {
    state.error = None;
    state.scroll = 0;
    if !state.open_asks().is_empty() {
        state.step = BundleStep::Answers;
        state.focus = 0;
        return Vec::new();
    }
    after_answers(state)
}

fn after_answers(state: &mut BundleState) -> Vec<Effect> {
    if state.review.needs_passphrase {
        state.step = BundleStep::Passphrase;
        return Vec::new();
    }
    start(state)
}

fn start(state: &mut BundleState) -> Vec<Effect> {
    let passphrase = state.review.needs_passphrase.then(|| state.passphrase.take_secret());
    state.step = BundleStep::Installing;
    state.error = None;
    state.scroll = 0;
    vec![Effect::InstallBundle {
        review: Box::new(state.review.clone()),
        answers: state.answers_for_helper(),
        data_drive: state.chosen_drive(),
        passphrase,
    }]
}

pub(super) fn on_bundle_installed(app: &mut App, result: Result<nodo_api::BundleReport, String>) {
    let Screen::Bundle(state) = &mut app.screen else {
        match result {
            Ok(report) => match &report.problem {
                None => notice(app, NoticeKind::Ok, format!("{} and its apps are installed.", report.package)),
                Some(problem) => fail(app, problem.clone()),
            },
            Err(text) => fail(app, text),
        }
        return;
    };
    match result {
        Ok(report) => {
            state.step = BundleStep::Done(report);
            state.scroll = 0;
        }
        Err(text) if text == ApiError::BadPassphrase.to_string() => {
            state.step = BundleStep::Passphrase;
            state.error = Some("That passphrase did not unlock this Nodo. Nothing was installed.".to_owned());
        }
        Err(text) => {
            state.step = BundleStep::Review;
            state.focus = state.cancel_index() + 1;
            state.error = Some(text);
        }
    }
}

/// Opens an app's setup page, which first asks for the device passphrase.
pub(super) fn open_owner_page(app: &mut App, package: &str) -> Vec<Effect> {
    let Some((_, title)) = app.data.owner_pages.iter().find(|(p, _)| p == package).cloned() else {
        notice(app, NoticeKind::Info, format!("{package} has no setup page."));
        return Vec::new();
    };
    push(app, Screen::OwnerPage(Box::new(OwnerPageState::new(package, &title))));
    Vec::new()
}

pub(super) fn owner_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::OwnerPage(state) = &mut app.screen else { return Vec::new() };
    if key.code == KeyCode::Esc {
        // The page, and with it the passphrase held for `r`, is dropped and wiped.
        pop(app);
        return Vec::new();
    }
    let Some(view) = &state.view else {
        match key.code {
            KeyCode::Enter if state.loading => {}
            KeyCode::Enter if state.passphrase.is_empty() => state.error = Some("Type the device passphrase.".to_owned()),
            KeyCode::Enter => {
                let passphrase = state.passphrase.take_secret();
                state.held = Some(passphrase.clone());
                state.loading = true;
                state.error = None;
                return vec![Effect::LoadOwnerPage { package: state.package.clone(), passphrase }];
            }
            _ => {
                edit_input(&mut state.passphrase, key);
            }
        }
        return Vec::new();
    };
    let targets = copy_targets(view);
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
        KeyCode::Char('r') if !state.loading => {
            if let Some(passphrase) = state.held.clone() {
                state.loading = true;
                return vec![Effect::LoadOwnerPage { package: state.package.clone(), passphrase }];
            }
        }
        _ => {}
    }
    Vec::new()
}

pub(super) fn on_owner_page(app: &mut App, package: String, result: Result<nodo_api::OwnerPageView, ApiError>) {
    let Screen::OwnerPage(state) = &mut app.screen else { return };
    if state.package != package {
        return;
    }
    state.loading = false;
    match result {
        Ok(view) => {
            let count = copy_targets(&view).len();
            state.focus = state.focus.min(count.saturating_sub(1));
            state.copied = None;
            state.view = Some(view);
            state.error = None;
        }
        Err(ApiError::BadPassphrase) => {
            state.held = None;
            state.view = None;
            state.passphrase.clear();
            state.error = Some("That passphrase did not unlock this Nodo.".to_owned());
        }
        Err(e) => {
            if state.view.is_none() {
                state.held = None;
            }
            state.error = Some(e.to_string());
        }
    }
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
