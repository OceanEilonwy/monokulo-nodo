//! Renders the web flow v2 proposal (`nodo_tui::proposal`) with the real TUI and writes each
//! screen as a design-canvas board: the ratatui buffer converted cell by cell into a monospace
//! grid. Every run of cells keeps its exact foreground, background and bold/underline/reverse,
//! and occupies exactly its number of cells, so nothing appears that a terminal can't show.
//!
//!     cargo run -p nodo-tui --example boards -- docs/design/tui
//!
//! Writes `<slug>.dc.html` for each scene at 120x36, and `<slug>-80x24.dc.html` for a few.

use std::fmt::Write as _;
use std::path::PathBuf;

use nodo_tui::proposal::{self, Scene};
use nodo_tui::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

/// Cell size on the board, in CSS pixels. JetBrains Mono's advance is 0.6 em.
const FONT_PX: f32 = 14.0;
const ROW_PX: f32 = 18.0;
const PAD_PX: u32 = 24;
const CAPTION_PX: u32 = 104;

fn hex(color: Color, fallback: &str) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
        Color::Reset => fallback.to_owned(),
        other => panic!("the truecolor theme only uses RGB, found {other:?}"),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One `<div>` per row, one `<span>` per run of cells with the same style.
fn grid(buffer: &Buffer, theme: &Theme) -> String {
    let ground = hex(theme.ground, "#101214");
    let fg_default = hex(theme.fg, "#D7DADC");
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        out.push_str("<div class=\"row\">");
        let mut x = 0;
        while x < area.width {
            let cell = &buffer[(x, y)];
            let style = (cell.fg, cell.bg, cell.modifier);
            let mut text = String::new();
            let mut cells = 0u16;
            while x < area.width {
                let c = &buffer[(x, y)];
                if (c.fg, c.bg, c.modifier) != style {
                    break;
                }
                let symbol = c.symbol();
                let width = UnicodeWidthStr::width(symbol).max(1) as u16;
                text.push_str(symbol);
                cells += width;
                x += width;
            }
            let (mut fg, mut bg) = (hex(style.0, &fg_default), hex(style.1, &ground));
            if style.2.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut css = format!("width:{cells}ch;color:{fg};background:{bg}");
            if style.2.contains(Modifier::BOLD) {
                css.push_str(";font-weight:700");
            }
            if style.2.contains(Modifier::UNDERLINED) {
                css.push_str(";text-decoration:underline");
            }
            if style.2.contains(Modifier::DIM) {
                css.push_str(";opacity:.6");
            }
            let _ = write!(out, "<span style=\"{css}\">{}</span>", escape(&text));
        }
        out.push_str("</div>\n");
    }
    out
}

fn board(scene: Scene, width: u16, height: u16, theme: &Theme) -> (String, u32, u32) {
    let buffer = proposal::render(scene, width, height, theme);
    let grid_w = (f32::from(width) * FONT_PX * 0.6).ceil() as u32;
    let grid_h = (f32::from(height) * ROW_PX).ceil() as u32;
    let w = grid_w + 2 * PAD_PX;
    let h = grid_h + 2 * PAD_PX + CAPTION_PX;
    let size = format!("{width}×{height}");
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{title}</title>
<script src="./support.js"></script>
</head>
<body>
<x-dc>
<helmet>
<link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;700&amp;display=swap" rel="stylesheet">
<style>
body{{margin:0}}
.term{{font-family:'JetBrains Mono',ui-monospace,monospace;font-size:{FONT_PX}px;line-height:{ROW_PX}px;white-space:pre}}
.row{{height:{ROW_PX}px;overflow:hidden}}
.row span{{display:inline-block;overflow:hidden;vertical-align:top}}
</style>
</helmet>
<div style="width:{w}px;height:{h}px;box-sizing:border-box;padding:{PAD_PX}px;background:#0B0C0D;color:#D7DADC;font-family:Inter,system-ui,sans-serif;display:flex;flex-direction:column;gap:12px">
<div style="display:flex;flex-direction:column;gap:4px">
<div style="font-size:18px;font-weight:700;color:#F2A33A">{title}</div>
<div style="font-size:13px;color:#A3AAB0">Rendered by nodo-tui (ratatui) at {size} — exactly what a terminal shows; your terminal's font and colours may vary slightly. Try it: <code>nodo-tui --mock --screen {slug}</code></div>
{clickable}</div>
<div class="term" style="width:{grid_w}px;height:{grid_h}px;background:{ground}">
{grid}</div>
</div>
</x-dc>
<script type="text/x-dc" data-dc-script data-props='{{"$preview":{{"width":{w},"height":{h}}}}}'>
class Component extends DCLogic {{
renderVals() {{ return {{}}; }}
}}
</script>
</body>
</html>
"#,
        title = escape(scene.title()),
        slug = scene.slug(),
        ground = hex(theme.ground, "#101214"),
        grid = grid(&buffer, theme),
        clickable = if scene.clickable().is_empty() {
            String::new()
        } else {
            format!(
                "<div style=\"font-size:13px;color:#D7DADC\">{} <span style=\"color:#A3AAB0\">(Mouse: left click, drag and the scroll wheel, as crossterm reports them; no hover.)</span></div>\n",
                escape(scene.clickable())
            )
        },
    );
    (html, w, h)
}

fn main() {
    let out: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "docs/design/tui".into())
        .into();
    std::fs::create_dir_all(&out).expect("create the output directory");
    let theme = Theme::truecolor();
    let mut index = Vec::new();
    let mut emit = |scene: Scene, width: u16, height: u16, suffix: &str| {
        let (html, w, h) = board(scene, width, height, &theme);
        let buffer = proposal::render(scene, width, height, &theme);
        let text: String = (0..height)
            .map(|y| {
                let mut line = String::new();
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    line.push_str(symbol);
                    x += UnicodeWidthStr::width(symbol).max(1) as u16;
                }
                line.trim_end().to_owned() + "\n"
            })
            .collect();
        std::fs::write(out.join(format!("{}{suffix}.txt", scene.slug())), text)
            .expect("write text");
        let name = format!("{}{suffix}.dc.html", scene.slug());
        std::fs::write(out.join(&name), html).expect("write board");
        index.push(format!("{name} {w} {h}"));
    };
    for scene in Scene::ALL {
        emit(scene, 120, 36, "");
    }
    for scene in [Scene::Overview, Scene::Who, Scene::NetChecks, Scene::V3Web, Scene::V3HostingInternet] {
        emit(scene, 80, 24, "-80x24");
    }
    // Board names and sizes, one per line, for laying them out on the canvas.
    std::fs::write(out.join("web2-boards.txt"), index.join("\n") + "\n").expect("write index");
}
