//! Renders the web flow v2 proposal (`nodo_tui::proposal`) with the real TUI and writes each
//! screen as a design-canvas board: the ratatui buffer converted cell by cell into a monospace
//! grid. Every run of cells keeps its exact foreground, background and bold/underline/reverse,
//! and occupies exactly its number of cells, so nothing appears that a terminal can't show.
//!
//!     cargo run -p nodo-tui --example boards -- docs/design/tui
//!     cargo run -p nodo-tui --example boards -- --real docs/design/tui   (the real web screens)
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
/// 15 px makes JetBrains Mono's 0.6 em advance a whole 9 px, so cells meet without seams.
const FONT_PX: f32 = 15.0;
const ROW_PX: f32 = 19.0;
/// The Powerline caps are drawn from the embedded Symbols Nerd Font subset, sized so they span
/// the whole cell like a terminal draws them.
const CAP_PX: f32 = 19.0;
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
                // A symbol the font may draw wider stays in a cell of its own, so it can't push
                // the rest of the run out of line.
                if !text.is_empty() && (!c.symbol().is_ascii() || !text.is_ascii()) {
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
            let mut css = format!("width:{}px;color:{fg};background:{bg}", (f32::from(cells) * FONT_PX * 0.6).round());
            if style.2.contains(Modifier::BOLD) {
                css.push_str(";font-weight:700");
            }
            if style.2.contains(Modifier::UNDERLINED) {
                css.push_str(";text-decoration:underline");
            }
            if style.2.contains(Modifier::DIM) {
                css.push_str(";opacity:.6");
            }
            // A terminal fits each glyph into its cell. The Symbols Nerd Font caps are 1 em wide
            // with their ink in one half (x 951..2171 for U+E0B6, -122..1097 for U+E0B4, of 2048),
            // so they are scaled and shifted to fill exactly one cell, as a terminal draws them.
            // The flat side reaches 1 px into the fill so anti-aliasing leaves no seam; the round
            // side ends exactly on the cell's edge.
            let cap = |ink_left: f32, ink_right: f32| {
                let cell = FONT_PX * 0.6;
                let k = (cell + 1.0) / ((ink_right - ink_left) / 2048.0 * CAP_PX);
                let bleed_left = if ink_left < 0.0 { 1.0 } else { 0.0 };
                let shift = -(ink_left / 2048.0 * CAP_PX) * k - bleed_left;
                format!("<b style=\"transform:translateX({shift:.2}px) scaleX({k:.4})\">{}</b>", escape(&text))
            };
            // Terminals draw the block elements as exact cell geometry rather than from the font
            // (alacritty, kitty, foot, wezterm all do), so they are drawn here as filled quarters.
            let quarters: Option<&[(&str, &str)]> = match text.as_str() {
                "▘" => Some(&[("left top", "50% 50%")]),
                "▝" => Some(&[("right top", "50% 50%")]),
                "▖" => Some(&[("left bottom", "50% 50%")]),
                "▗" => Some(&[("right bottom", "50% 50%")]),
                "▀" => Some(&[("left top", "100% 50%")]),
                "▄" => Some(&[("left bottom", "100% 50%")]),
                "▌" => Some(&[("left top", "50% 100%")]),
                "▐" => Some(&[("right top", "50% 100%")]),
                "█" => Some(&[("left top", "100% 100%")]),
                _ => None,
            };
            if let Some(parts) = quarters {
                let layers: Vec<String> =
                    parts.iter().map(|(pos, size)| format!("linear-gradient({fg},{fg}) {pos} / {size} no-repeat")).collect();
                let css = format!("width:{}px;background:{},{bg}", (f32::from(cells) * FONT_PX * 0.6).round(), layers.join(","));
                let _ = write!(out, "<span style=\"{css}\"></span>");
                continue;
            }
            let (class, inner) = if text == "\u{E0B6}" {
                (" class=\"cap\"", cap(951.0, 2171.0))
            } else if text == "\u{E0B4}" {
                (" class=\"cap\"", cap(-122.0, 1097.0))
            } else {
                ("", escape(&text))
            };
            let _ = write!(out, "<span{class} style=\"{css}\">{inner}</span>");
        }
        out.push_str("</div>");
    }
    out
}

fn board(scene: Scene, width: u16, height: u16, theme: &Theme) -> (String, u32, u32) {
    let buffer = proposal::render(scene, width, height, theme);
    board_from(&buffer, scene.title(), scene.slug(), scene.clickable(), width, height, theme)
}

/// A board from any rendered buffer: the proposal's scenes, or the real screens with `--real`.
fn board_from(buffer: &Buffer, title: &str, slug: &str, clickable: &str, width: u16, height: u16, theme: &Theme) -> (String, u32, u32) {
    let grid_w = (f32::from(width) * FONT_PX * 0.6).ceil() as u32;
    let grid_h = (f32::from(height) * ROW_PX).ceil() as u32;
    let w = grid_w + 2 * PAD_PX;
    // Narrow boards wrap their caption onto more lines.
    let caption = if width < 100 { CAPTION_PX + 48 } else { CAPTION_PX };
    let h = grid_h + 2 * PAD_PX + caption;
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
@font-face{{font-family:'NodoCaps';src:url('{caps_font}') format('woff2')}}
.term{{font-family:'JetBrains Mono','NodoCaps',ui-monospace,monospace;font-size:{FONT_PX}px;line-height:{ROW_PX}px;white-space:normal}}
.row{{display:block;height:{ROW_PX}px;margin:0;padding:0;overflow:hidden}}
.row span{{display:inline-block;height:{ROW_PX}px;overflow:visible;vertical-align:top;white-space:pre}}
.cap{{overflow:visible;position:relative;z-index:1}}
.cap{{font-family:'NodoCaps';font-size:{CAP_PX}px;line-height:{ROW_PX}px}}
.cap b{{display:inline-block;transform-origin:0 0;font-weight:400}}
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
        title = escape(title),
        slug = slug,
        ground = hex(theme.ground, "#101214"),
        grid = grid(buffer, theme),
        caps_font = std::env::var("NODO_BOARD_CAPS_FONT").unwrap_or_else(|_| "fonts/nodo-caps.woff2".into()),
        clickable = if clickable.is_empty() {
            String::new()
        } else {
            format!(
                "<div style=\"font-size:13px;color:#D7DADC\">{} <span style=\"color:#A3AAB0\">(Mouse: left click, the scroll wheel, and hover as crossterm reports pointer moves.)</span></div>\n",
                escape(clickable)
            )
        },
    );
    (html, w, h)
}

/// Every real screen, from its fixture, at 120×36 and 80×24: `real-tui-<slug>` boards and text.
pub fn real_all(out: &std::path::Path) {
    use nodo_tui::app::{Screen, ScreenId};
    use nodo_tui::fixtures::fixture;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let theme = Theme::truecolor().with_rounded(true);
    let mut scenes: Vec<(String, String, nodo_tui::app::App)> = Vec::new();
    for id in ScreenId::ALL.into_iter().chain([ScreenId::Setup]) {
        let (app, _) = fixture(id);
        scenes.push((format!("real-tui-{}", id.slug()), format!("{} (real)", id.slug()), app));
    }
    let (mut app, _) = fixture(ScreenId::Feeds);
    app.screen = Screen::Feeds { selected: 0, keys: true };
    scenes.push(("real-tui-feeds-keys".into(), "Sources with keys shown (real)".into(), app));
    let mut index = Vec::new();
    for (slug, title, app) in &scenes {
        for (width, height, suffix) in [(120u16, 36u16, ""), (80, 24, "-80x24")] {
            let mut app = app.clone();
            app.size = (width, height);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
            terminal.draw(|frame| {
                nodo_tui::view::render(frame, &app, &theme);
            })
            .expect("draw");
            let buffer = terminal.backend().buffer().clone();
            let (html, w, h) = board_from(&buffer, title, slug, "", width, height, &theme);
            let name = format!("{slug}{suffix}.dc.html");
            std::fs::write(out.join(&name), html).expect("write board");
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
            std::fs::write(out.join(format!("{slug}{suffix}.txt")), text).expect("write text");
            index.push(format!("{name} {w} {h}"));
        }
    }
    std::fs::write(out.join("real-tui-boards.txt"), index.join("\n") + "\n").expect("write index");
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--real-all") {
        let out: PathBuf = std::env::args().nth(2).unwrap_or_else(|| "docs/design/tui".into()).into();
        std::fs::create_dir_all(&out).expect("create the output directory");
        real_all(&out);
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--real") {
        let out: PathBuf = std::env::args().nth(2).unwrap_or_else(|| "docs/design/tui".into()).into();
        std::fs::create_dir_all(&out).expect("create the output directory");
        real_screens(&out);
        return;
    }
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
    for scene in [Scene::Overview, Scene::Who, Scene::NetChecks, Scene::V3Web, Scene::V3HostingInternet, Scene::T2Home, Scene::T2GetApps, Scene::T2SourceKeyChanged, Scene::T2Sources, Scene::T2AppPage, Scene::T2System, Scene::T2Updates, Scene::T2Backups, Scene::T2NetworkWifi] {
        emit(scene, 80, 24, "-80x24");
    }
    // Board names and sizes, one per line, for laying them out on the canvas.
    std::fs::write(out.join("web2-boards.txt"), index.join("\n") + "\n").expect("write index");
}

/// The real web screens (web flow v3 as built), rendered through `view::render` from the mock
/// device, for comparing with the approved proposal boards.
pub fn real_screens(out: &std::path::Path) {
    use nodo_tui::app::{ProviderStage, Screen, ScreenId};
    use nodo_tui::fixtures::fixture;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let theme = Theme::truecolor().with_rounded(true);
    let render = |app: &nodo_tui::app::App, width: u16, height: u16| {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
        terminal.draw(|frame| {
            nodo_tui::view::render(frame, app, &theme);
        })
        .expect("draw");
        terminal.backend().buffer().clone()
    };
    let mut scenes: Vec<(&str, &str, nodo_tui::app::App)> = Vec::new();
    scenes.push(("real-web3-sites", "Web: the sites that are online (real)", fixture(ScreenId::WebSites).0));
    let (mut app, _) = fixture(ScreenId::WebExposure);
    if let Screen::WebExposure(state) = &mut app.screen {
        state.draft.onion = false;
    }
    scenes.push(("real-web3-hosting-internet", "Hosting with the internet ticked (real)", app));
    let (mut app, _) = fixture(ScreenId::WebExposure);
    app.data.web.provider.clear();
    if let Screen::WebExposure(state) = &mut app.screen {
        state.has_provider = false;
        state.draft.onion = false;
    }
    scenes.push(("real-web3-no-provider", "Internet ticked, no provider yet (real)", app));
    scenes.push(("real-web3-add-provider", "Add a provider: choose the company (real)", fixture(ScreenId::WebProviders).0));
    let (mut app, _) = fixture(ScreenId::WebProviders);
    if let Screen::WebProviders(state) = &mut app.screen {
        state.stage = ProviderStage::Key;
        state.test = None;
        state.return_to = Some("shop".to_owned());
    }
    scenes.push(("real-web3-provider-key", "Connect Porkbun (real)", app));
    let (mut app, _) = fixture(ScreenId::WebProviders);
    if let Screen::WebProviders(state) = &mut app.screen {
        state.stage = ProviderStage::Done { provider: "Porkbun".to_owned(), domains: vec!["example.com".to_owned(), "example.org".to_owned()] };
        state.return_to = Some("shop".to_owned());
    }
    scenes.push(("real-web3-provider-done", "Provider connected (real)", app));
    // Two connected providers: Porkbun with example.com, Gandi with the domains its key lists.
    let with_gandi = |id: ScreenId| {
        use nodo_tui::model::ScreenData;
        let (mut app, mut mock) = fixture(id);
        let key = nodo_tui::secret::Secret::new("k");
        mock.add_provider("Gandi", None, &key, &key).expect("mock provider");
        app.data = nodo_tui::runtime::load_snapshot(&mut mock).expect("snapshot");
        app
    };
    let mut app = with_gandi(ScreenId::WebExposure);
    if let Screen::WebExposure(state) = &mut app.screen {
        state.draft.onion = false;
        state.has_provider = true;
        state.picking = true;
        state.pick = 1;
    }
    scenes.push(("real-web3-pick-domain", "Choosing a domain from every provider (real)", app));
    let mut app = with_gandi(ScreenId::WebProviders);
    if let Screen::WebProviders(state) = &mut app.screen {
        state.stage = ProviderStage::List;
        state.selected = 1;
        state.test = None;
    }
    scenes.push(("real-web3-providers", "Domain providers: every account and its domains (real)", app));
    scenes.push(("real-web3-check", "Checks found a problem (real)", fixture(ScreenId::WebCheck).0));
    scenes.push(("real-web3-confirm", "One confirm, starting on Cancel (real)", fixture(ScreenId::WebConfirm).0));
    scenes.push(("real-web3-blocked", "The internet can't reach your Nodo (real)", fixture(ScreenId::WebCgnat).0));
    scenes.push(("real-web3-online", "Online, getting the secure connection (real)", fixture(ScreenId::WebCertificate).0));
    let mut index = Vec::new();
    for (slug, title, app) in &scenes {
        for (width, height, suffix) in [(120u16, 36u16, ""), (80, 24, "-80x24")] {
            if !suffix.is_empty() && !matches!(*slug, "real-web3-sites" | "real-web3-hosting-internet" | "real-web3-confirm" | "real-web3-providers") {
                continue;
            }
            let buffer = render(app, width, height);
            let (html, w, h) = board_from(&buffer, title, slug, "", width, height, &theme);
            let name = format!("{slug}{suffix}.dc.html");
            std::fs::write(out.join(&name), html).expect("write board");
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
            std::fs::write(out.join(format!("{slug}{suffix}.txt")), text).expect("write text");
            index.push(format!("{name} {w} {h}"));
        }
    }
    std::fs::write(out.join("real-web3-boards.txt"), index.join("\n") + "\n").expect("write index");
}
