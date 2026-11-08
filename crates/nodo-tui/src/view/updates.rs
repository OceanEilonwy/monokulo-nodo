//! Updates, auto-update preferences, update results and logs.

use ratatui::text::Line;

use super::layout::{Item, Page, Panel};
use super::{checkbox, hints, keyed, mark, pad, tab_bar};
use crate::app::{App, AutoUpdateState, LogsState, Tab, UpdateResultsState, UpdatesState};
use crate::model::*;
use crate::theme::Theme;

/// Header text for the Updates screen, counted from saved preferences and not the draft.
fn auto_update_summary(count: usize) -> String {
    match count {
        0 => "Auto-update off".to_owned(),
        1 => "Auto-update on for 1 package".to_owned(),
        n => format!("Auto-update on for {n} packages"),
    }
}

pub fn updates(app: &App, t: &Theme, state: &UpdatesState) -> Page {
    let rows = app.update_rows();
    let mut items = Vec::new();
    let mut held_header = false;
    for (index, item) in rows.iter().enumerate() {
        if item.held && !held_header {
            items.push(Item::Blank);
            items.push(Item::Text(Line::from(t.d(" Held back"))));
            held_header = true;
        }
        let selected = index == state.selected;
        let ticked = state.checked.contains(&item.name);
        let versions = pad(&format!("{} → {}", item.from, item.to), 20);
        let lead = mark(selected, &format!(" {} {} {versions}", checkbox(ticked), pad(&item.name, 13)));
        let source = if item.third_party { t.a(item.source.clone()) } else { t.n(item.source.clone()) };
        let left = Line::from(vec![if item.held { t.d(lead) } else { t.n(lead) }, source]);
        let note = match &item.note {
            UpdateNote::Ready => t.n("Ready"),
            UpdateNote::RestartNeeded => t.d("Restart needed"),
            UpdateNote::NewPermission(_) => t.a("▲ Asks for new permission"),
            UpdateNote::Skipped => t.d("You skipped this version"),
            UpdateNote::Blocked(reason) => t.a(format!("▲ {reason}")),
        };
        items.push(if selected {
            Item::Selected(left, Line::from(note))
        } else {
            Item::Row(left, Line::from(note))
        });
    }
    if rows.is_empty() {
        items.push(Item::Text(Line::from(t.d(" Everything is up to date."))));
    }

    let mut page = Page::new(
        tab_bar(app, t, Tab::Updates),
        Line::from(t.d(auto_update_summary(app.data.auto_update.enabled_count()))),
    )
    .panel(Panel::new("Available", items));
    for item in rows.iter() {
        if let UpdateNote::NewPermission(permission) = &item.note {
            page = page.panel(Panel::accent(
                format!("{} asks for more", item.name),
                vec![
                    Item::Text(Line::from(vec![
                        t.a("▲"),
                        t.n(format!(" New permission: {}.", lower_first(permission))),
                    ])),
                    Item::Text(Line::from(t.d(
                        "Selecting this update will ask you to review and approve that change first.",
                    ))),
                ],
            ));
        }
    }
    page.panel(
        Panel::new(
            "How updates apply",
            vec![
                Item::Text(Line::from(t.d(
                    "Each package restarts, then runs its health check. If that fails, its previous version returns.",
                ))),
                Item::Text(Line::from(t.d(
                    "The node restarts last. Encrypted packages stay unlocked unless the system restarts.",
                ))),
                Item::Text(Line::from(t.d(app.data.overview.updates_checked.clone()))),
            ],
        )
        .grow(),
    )
    .footer(Item::Text(hints(
        t,
        &[
            ("Space", "select"),
            ("↵", "update selected"),
            ("c", "changelog"),
            ("r", "check now"),
            ("h", "hold version"),
            ("a", "auto-update"),
        ],
    )))
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

const ALWAYS_WAITS: [&str; 6] = [
    "New permissions requested",
    "Feed signing key changed",
    "Update fails install checks",
    "A version you put on hold",
    "System restart needed",
    "Device locked or disk low",
];

const IF_IT_FAILS: [&str; 4] = [
    "Previous version and data return",
    "That version is held",
    "Home shows what happened",
    "Nothing tries that version again until you choose to",
];

pub fn auto_update(t: &Theme, state: &AutoUpdateState) -> Page {
    let draft = &state.draft;
    let frequency_selected = state.selected == 0;
    let frequency = Line::from(vec![
        t.n(mark(frequency_selected, &format!(" {}", pad("Check frequency", 18)))),
        t.n(format!("‹ {} ›", draft.frequency)),
    ]);
    let window_value = match &state.editing_window {
        Some(input) => vec![t.n(input.display()), t.s(" ")],
        None => vec![t.n(format!("{} to {} device time", draft.window_start, draft.window_end))],
    };
    let mut window = vec![t.d(format!(" {}", pad("Install window", 18)))];
    window.extend(window_value);
    let mut schedule = vec![
        if frequency_selected {
            Item::Selected(frequency, Line::default())
        } else {
            Item::Row(frequency, Line::default())
        },
        Item::Text(Line::from(window)),
    ];
    if let Some(error) = &state.error {
        schedule.push(Item::Text(Line::from(t.e(format!(" ✕ {error}")))));
    }

    let focused = state.focused_package();
    let packages = draft
        .packages
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let selected = focused == Some(index);
            let left = Line::from(vec![
                t.n(mark(selected, &format!(" {} {}", checkbox(entry.enabled), pad(&entry.name, 15)))),
                if entry.third_party { t.a(entry.source.clone()) } else { t.n(entry.source.clone()) },
            ]);
            let note = if entry.shows_warning(selected) {
                t.a("▲ Runs new code without your review")
            } else {
                t.d(entry.note.clone())
            };
            if selected {
                Item::Selected(left, Line::from(note))
            } else {
                Item::Row(left, Line::from(note))
            }
        })
        .collect();

    let bullets = |symbol: &str, lines: &[&str]| -> Vec<Item> {
        lines.iter().map(|line| Item::Text(Line::from(vec![t.i(symbol.to_owned()), t.n(format!(" {line}"))]))).collect()
    };
    let keys: &[(&str, &str)] = if state.editing_window.is_some() {
        &[("type", "like 03:00 to 05:00"), ("↵", "done"), ("Esc", "cancel")]
    } else {
        &[("↑↓", "move"), ("Space", "toggle"), ("w", "change window"), ("h", "history"), ("↵", "save"), ("Esc", "back")]
    };
    Page::new(
        Line::from(vec![t.n("Updates "), t.d("› "), t.b("Auto-update")]),
        Line::from(t.d(state.saved.next_run.clone())),
    )
    .panel(Panel::new("Schedule", schedule))
    .panel(Panel::new("Update automatically", packages))
    .columns(vec![
        (1, vec![Panel::new("Always waits for you", bullets("●", &ALWAYS_WAITS)).grow()]),
        (1, vec![Panel::new("If an update fails", bullets("▸", &IF_IT_FAILS)).grow()]),
    ])
    .footer(Item::Text(hints(t, keys)))
}

pub fn results(t: &Theme, state: &UpdateResultsState) -> Page {
    let report = &state.report;
    let applied = report.results.iter().filter(|result| result.applied).count();
    let rolled_back = report.results.len() - applied;
    let summary = if rolled_back == 0 {
        t.o(format!("{applied} updated"))
    } else {
        t.a(format!("{applied} updated, {rolled_back} rolled back"))
    };
    let rows = report
        .results
        .iter()
        .map(|result| {
            let body = format!("{} {}", pad(&result.name, 13), result.version);
            if result.applied {
                Item::Row(Line::from(vec![t.o("✓"), t.n(format!(" {body}"))]), Line::from(t.d(result.note.clone())))
            } else {
                Item::Selected(Line::from(format!("▸✕ {body}")), Line::from(result.note.clone()))
            }
        })
        .collect();
    let mut page = Page::new(
        Line::from(vec![t.n("Updates "), t.d("› "), t.b("Results")]),
        Line::from(summary),
    )
    .panel(Panel::new("Results", rows));
    if let Some(failure) = &report.failure {
        let mut restored = vec![t.n(format!("{} {} is ", failure.restored, failure.name))];
        restored.push(if failure.running { t.o("running") } else { t.e("✕ not running") });
        restored.push(t.n("."));
        page = page.panel(Panel::error(
            failure.headline.clone(),
            vec![
                Item::Text(Line::from(restored)),
                Item::Blank,
                Item::Text(Line::from(t.e(failure.log_line.clone()))),
                Item::Text(Line::from(t.d(failure.verdict.clone()))),
            ],
        ));
        let keep = format!("Keep {} and hold this version", failure.kept_version);
        let options: [(&str, &str); 4] = [
            (&keep, ""),
            ("Try the update again", ""),
            ("Copy a report for the publisher", "no keys or secrets included"),
            ("Read the full log", ""),
        ];
        let choices = options
            .iter()
            .enumerate()
            .map(|(index, (label, note))| {
                let selected = index == state.choice;
                let line = Line::from(vec![
                    t.n(mark(selected, &format!(" {label}"))),
                    t.n(if note.is_empty() { "" } else { "  " }),
                    t.d(*note),
                ]);
                if selected {
                    Item::Selected(line, Line::default())
                } else {
                    Item::Text(line)
                }
            })
            .collect();
        page = page.panel(Panel::new("What you can do", choices).grow());
        page.footer(Item::Text(hints(t, &[("↑↓", "choose"), ("↵", "confirm"), ("Esc", "back to updates")])))
    } else {
        page.footer(Item::Text(hints(t, &[("↵", "done"), ("Esc", "back to updates")])))
    }
}

pub fn logs(app: &App, t: &Theme, state: &LogsState) -> Page {
    let mut sources = Vec::new();
    for (index, source) in app.data.log_sources.iter().enumerate() {
        let failed = app
            .data
            .services
            .iter()
            .any(|svc| &svc.id == source && svc.state == nodo_api::ServiceState::Failed);
        let label = if failed { format!("{source} ✕") } else { source.clone() };
        let selected = index == state.selected;
        let is_service = app.data.services.iter().any(|svc| &svc.id == source);
        let text = mark(selected, &format!(" {label}"));
        sources.push(if selected {
            Item::Selected(Line::from(text), Line::default())
        } else if is_service {
            Item::Text(Line::from(t.n(text)))
        } else {
            Item::Text(Line::from(t.d(text)))
        });
    }

    let needle = state.find.text().to_lowercase();
    let lines: Vec<Item> = state
        .view
        .lines
        .iter()
        .filter(|line| !state.errors_only || line.kind == LogKind::Error)
        .filter(|line| needle.is_empty() || line.text.to_lowercase().contains(&needle))
        .map(|line| {
            Item::Text(Line::from(match line.kind {
                LogKind::Info => t.d(line.text.clone()),
                LogKind::Error => t.e(line.text.clone()),
                LogKind::Marker => t.a(line.text.clone()),
            }))
        })
        .collect();
    let title = app.data.log_sources.get(state.selected).cloned().unwrap_or_else(|| "Log".to_owned());
    let mut right_column = vec![Panel::new(title, lines).grow()];
    if !state.view.suggestion.is_empty() {
        right_column.push(Panel::accent(
            "Suggestion",
            state.view.suggestion.iter().map(|line| Item::Text(keyed(t, line))).collect(),
        ));
    }
    let follow = if state.follow { t.o("● Following live") } else { t.d("○ Paused") };
    let keys = if state.finding {
        hints(t, &[("type", "to find"), ("↵", "keep filter"), ("Esc", "clear")])
    } else {
        hints(
            t,
            &[("↑↓", "service"), ("/", "find"), ("e", "errors only"), ("f", "follow"), ("y", "copy with secrets removed")],
        )
    };
    let mut page = Page::new(tab_bar(app, t, Tab::Logs), Line::from(follow))
        .columns(vec![(23, vec![Panel::new("Service", sources).grow()]), (77, right_column)]);
    if state.finding || !state.find.is_empty() {
        page = page.footer(Item::Text(Line::from(vec![t.a("/ "), t.n(state.find.display()), t.s(" ")])));
    }
    page.footer(Item::Text(keys))
}
