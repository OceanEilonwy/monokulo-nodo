#![allow(dead_code)]

use nodo_tui::app::App;
use nodo_tui::mock::MockBackend;
use nodo_tui::runtime::dispatch;
use nodo_tui::theme::Theme;
use nodo_tui::view::{self, layout::FitReport};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

pub fn draw(app: &App, width: u16, height: u16) -> (Buffer, FitReport) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    let mut report = FitReport::default();
    terminal
        .draw(|frame| {
            report = view::render(frame, app, &Theme::truecolor());
        })
        .expect("draw");
    (terminal.backend().buffer().clone(), report)
}

/// The screen as text, one line per row. Wide symbols occupy two cells, so their blank
/// continuation cell is skipped to keep the text as a person reads it.
pub fn text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

pub fn screen_text(app: &App) -> String {
    text(&draw(app, 100, 32).0)
}

pub fn send(app: &mut App, mock: &mut MockBackend, events: impl IntoIterator<Item = nodo_tui::app::Event>) {
    for event in events {
        dispatch(app, mock, event);
    }
}

pub fn type_text(app: &mut App, mock: &mut MockBackend, text: &str) {
    for c in text.chars() {
        dispatch(app, mock, nodo_tui::fixtures::press(crossterm::event::KeyCode::Char(c)));
    }
}
