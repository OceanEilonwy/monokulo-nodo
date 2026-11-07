//! Permission review, install progress and unlock.

use ratatui::text::Line;

use super::layout::{Item, Page, Panel};
use super::{button_line, hints};
use crate::app::{App, ReviewPurpose, ReviewState, UnlockState};
use crate::model::*;
use crate::theme::Theme;

pub fn review(t: &Theme, state: &ReviewState) -> Page {
    let data = &state.data;
    let is_update = matches!(state.purpose, ReviewPurpose::Update { .. });
    let verb = if is_update { "Update" } else { "Install" };
    let risky = if is_update { "Approve and update" } else { "Install and configure" };
    let asks: Vec<Item> = data
        .asks
        .iter()
        .map(|ask| {
            let (symbol, note) = match ask.level {
                AskLevel::Normal => (t.i("●"), t.d(ask.note.clone())),
                AskLevel::Caution => (t.a("▲"), t.a(ask.note.clone())),
                AskLevel::Encrypted => (t.a("◆"), t.d(ask.note.clone())),
            };
            Item::Row(Line::from(vec![symbol, t.n(format!(" {}", ask.text))]), Line::from(note))
        })
        .collect();
    let mut items = asks;
    items.push(Item::Blank);
    for guarantee in &data.guarantees {
        items.push(Item::Text(Line::from(vec![t.o("✓"), t.n(format!(" {guarantee}"))])));
    }
    items.push(Item::Blank);
    items.push(Item::Text(Line::from(t.d("If an update asks for more than this, you will be asked again."))));

    let title = if is_update { "This update newly asks to" } else { "It is asking to" };
    let mut page = Page::new(
        Line::from(vec![t.n(format!("{verb} ")), t.b(data.id.name.clone()), t.n(format!(" {}", data.version))]),
        Line::from(if data.third_party {
            t.a(format!("{} feed", data.id.feed))
        } else {
            t.d(format!("{} feed", data.id.feed))
        }),
    );
    if data.third_party {
        page = page.panel(Panel::accent(
            "Third-party package",
            vec![Item::Text(Line::from(t.n(
                "This package comes from a feed you added. Its files passed the install checks, but nobody has reviewed its code for you.",
            )))],
        ));
    }
    page.panel(Panel::new(title, items).grow()).footer(Item::Row(
        button_line(t, "Cancel", risky, state.confirm_focused),
        hints(t, &[("←→", "choose"), ("↵", "confirm"), ("d", "file list"), ("Esc", "cancel")]),
    ))
}

pub fn installing(t: &Theme, snapshot: &InstallSnapshot) -> Page {
    let total = snapshot.steps.len();
    let current = snapshot.steps.iter().filter(|step| step.state == StepState::Done).count();
    let right = match &snapshot.outcome {
        InstallOutcome::InProgress => t.d(format!("Step {} of {total}", (current + 1).min(total))),
        InstallOutcome::Succeeded => t.o("✓ Installed"),
        InstallOutcome::Failed(_) => t.e("✕ Not installed"),
        InstallOutcome::Cancelled => t.d("Cancelled"),
    };
    let steps = snapshot
        .steps
        .iter()
        .map(|step| match step.state {
            StepState::Done => Item::Row(
                Line::from(vec![t.o("✓"), t.n(format!(" {}", step.name))]),
                Line::from(t.d(step.note.clone())),
            ),
            StepState::Working => {
                Item::Row(Line::from(vec![t.i("▸ "), t.b(step.name.clone())]), Line::from(t.i("working")))
            }
            StepState::Pending => Item::Text(Line::from(t.d(format!("○ {}", step.name)))),
            StepState::Failed => {
                Item::Row(Line::from(vec![t.e("✕ "), t.b(step.name.clone())]), Line::from(t.e("failed")))
            }
        })
        .collect();
    let last = snapshot.details.len().saturating_sub(1);
    let mut details: Vec<Item> = snapshot
        .details
        .iter()
        .enumerate()
        .map(|(index, line)| {
            Item::Text(Line::from(if index == last { t.n(line.clone()) } else { t.d(line.clone()) }))
        })
        .collect();
    if let InstallOutcome::Failed(reason) = &snapshot.outcome {
        details.push(Item::Text(Line::from(t.e(format!("✕ {reason}")))));
    }
    let footer = match snapshot.outcome {
        InstallOutcome::InProgress => Item::Row(
            Line::from(t.d("A failed step removes what this install added.")),
            hints(t, &[("Esc", "cancel"), ("v", "full log")]),
        ),
        InstallOutcome::Succeeded => Item::Row(
            Line::from(t.o("✓ Installed and healthy.")),
            hints(t, &[("↵", "configure"), ("v", "full log"), ("Esc", "done")]),
        ),
        _ => Item::Row(Line::default(), hints(t, &[("v", "full log"), ("Esc", "back")])),
    };
    Page::new(
        Line::from(vec![t.n("Installing "), t.b(snapshot.id.name.clone()), t.n(format!(" {}", snapshot.version))]),
        Line::from(right),
    )
    .panel(Panel::new("Progress", steps))
    .panel(Panel::new("Details", details).grow())
    .footer(footer)
}

pub fn unlock(app: &App, t: &Theme, state: &UnlockState) -> Page {
    let overview = &app.data.overview;
    let mut passphrase = vec![Item::Row(
        Line::from(t.d("Passphrase")),
        Line::from(vec![t.n(state.input.display()), t.s(" ")]),
    )];
    if let Some(error) = &state.error {
        passphrase.push(Item::Text(Line::from(t.e(format!("✕ {error}")))));
    }
    passphrase.push(Item::Text(Line::from(t.d("Attempts are delayed after each failure."))));

    let targets = overview
        .unlock_targets
        .iter()
        .map(|target| {
            let symbol = if target.encrypted { t.a("◆") } else { t.d("○") };
            Item::Row(
                Line::from(vec![symbol, t.n(format!(" {}", target.name))]),
                Line::from(t.d(target.note.clone())),
            )
        })
        .collect();
    let next = vec![
        Item::Text(Line::from(vec![t.i("▸"), t.n(" Each package's storage opens inside its own sandbox only.")])),
        Item::Text(Line::from(vec![t.i("▸"), t.n(" Services start in dependency order, and progress appears here.")])),
    ];
    Page::new(Line::from(t.b("Unlock this device")), Line::from(t.d(overview.restarted.clone())))
        .panel(Panel::accent("Device passphrase", passphrase))
        .panel(Panel::new("Enter once to unlock all of these", targets).grow())
        .panel(Panel::new("What happens next", next))
        .footer(Item::Text(Line::from(t.d(
            "A lost passphrase cannot be recovered. Restore from a backup to get the data back.",
        ))))
        .footer(Item::Text(hints(t, &[("↵", "unlock"), ("Esc", "leave locked")])))
}
