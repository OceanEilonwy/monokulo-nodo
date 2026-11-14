//! `--screenshot SCENE PATH`: renders one screen, with sample panels like the design's, to a PNG
//! file, without a display. For reviewing the screen against the design and for the PR.

use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use nodo_manifest::PanelSource;
use nodo_panels::{Layout, Offered, PanelKey, PanelOutput, Placement, SizeRule, Status};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::Rgb8Pixel;
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize};

use crate::geometry;
use crate::state::{App, Button, Mode};
use crate::view::Presenter;
use crate::{ButtonView, DesignBoard, DesignBoard2, MainWindow};
use slint::Model;

pub const SCENES: [&str; 9] = ["view", "arrange", "drag", "hidden", "add", "confirm", "progress", "failed", "off"];

/// Design boards for the touch management screens and wallpapers (ui/design.slint), mock data
/// only. "wp-panels" is the real tile screen with the proposed Manage button added.
pub const DESIGN_SCENES: [&str; 23] = [
    "wp-panels", "wp-logo", "wp-logo-panels", "m-screen", "m-home", "m-apps", "m-app", "m-settings",
    "m-settings-kbd", "m-get-apps", "m-install", "m-web", "m-hosting", "m-sources", "m-add-source",
    "m-system", "m-updates", "m-network", "m-backups", "m-blocked", "u-pin", "u-pin-wrong", "u-lockout",
];
/// Also a design scene, kept out of the array above to keep its length readable in reviews.
pub const NO_PIN_SCENE: &str = "u-no-pin";

/// Touch screen GUI v2 (ui/design2.slint): every screen with proper Slint controls, mock data.
pub const G2_SCENES: &[&str] = &[
    "g-widgets", "g-wp-logo", "g-wp-logo-panels", "g-home", "g-unlock", "g-apps", "g-get-apps", "g-about",
    "g-install", "g-installing", "g-app", "g-remove", "g-settings", "g-settings-pick", "g-settings-kbd",
    "g-rolled-back", "g-sources", "g-add-source", "g-source-check", "g-source-errors", "g-key-changed",
    "g-remove-builtin", "g-web", "g-hosting", "g-hosting-domain", "g-providers", "g-add-provider",
    "g-provider-key", "g-provider-done", "g-checks", "g-confirm", "g-online", "g-blocked-isp", "g-system",
    "g-updates", "g-auto", "g-undo", "g-update-rolled-back", "g-logs", "g-backups", "g-backing-up",
    "g-restore", "g-network", "g-wifi-join", "g-ssh-keys", "g-screen", "g-rejected", "g-capability",
    "g-no-drive", "g-pin", "g-pin-wrong", "g-lockout", "g-no-pin",
];

fn is_design(name: &str) -> bool {
    name == NO_PIN_SCENE || (DESIGN_SCENES.contains(&name) && name != "wp-panels")
}

struct Offscreen {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn panel(package: &str, id: &str, title: &str, min: [u8; 2], max: [u8; 2], initial: [u8; 2], refresh: u64) -> Offered {
    Offered {
        key: PanelKey::new(package, id),
        title: title.into(),
        refresh: Duration::from_secs(refresh),
        rule: SizeRule { min, max, initial },
        source: PanelSource::Device {},
        script: Ok(String::new()),
    }
}

/// The design's screen: the node, payments, bell, wallet scanner, device and push relay on the
/// screen, with updates, tor and refunds left to add.
pub fn sample(now: Instant) -> App {
    let offered = vec![
        panel("cuprated", "sync", "Monero node", [2, 1], [4, 2], [4, 2], 10),
        panel("mk-pay", "today", "Payments today", [2, 1], [4, 2], [4, 2], 10),
        panel("mk-bell", "bell", "Bell", [2, 1], [2, 1], [2, 1], 5),
        panel("mk-lws", "scanner", "Wallet scanner", [2, 1], [4, 1], [2, 1], 10),
        panel("nodo", "device", "Device", [2, 1], [4, 2], [2, 2], 5),
        panel("mk-push-relay", "relay", "Push relay", [2, 1], [4, 1], [4, 1], 30),
        panel("nodo", "updates", "Updates", [2, 1], [4, 1], [2, 1], 60),
        panel("nodo", "tor", "Tor", [1, 1], [2, 1], [1, 1], 30),
        panel("mk-pay", "refunds", "Refunds", [2, 1], [4, 2], [2, 1], 60),
    ];
    let place = |package: &str, id: &str, x, y, w, h| Placement { key: PanelKey::new(package, id), x, y, w, h };
    let layout = Layout {
        placements: vec![
            place("cuprated", "sync", 0, 0, 4, 2),
            place("mk-pay", "today", 4, 0, 4, 2),
            place("mk-bell", "bell", 0, 2, 2, 1),
            place("mk-lws", "scanner", 2, 2, 2, 1),
            place("nodo", "device", 4, 2, 2, 2),
            place("mk-push-relay", "relay", 0, 3, 4, 1),
        ],
    };
    let mut app = App::new(offered, layout, now);
    let out = |big: &str, unit: &str, status| PanelOutput { big: Some(big.into()), unit: (!unit.is_empty()).then(|| unit.into()), status: Some(status), ..PanelOutput::default() };
    let samples = [
        (
            "cuprated",
            "sync",
            PanelOutput {
                bar: Some(0.61),
                rows: vec![("height".into(), "2,121,400 / 3,478,000".into()), ("left".into(), "about 9 h".into())],
                series: vec![16.0, 22.0, 29.0, 36.0, 40.0, 50.0, 59.0, 65.0, 72.0, 77.0, 85.0, 90.0, 95.0, 100.0],
                ..out("61%", "synced", Status::Busy)
            },
        ),
        (
            "mk-pay",
            "today",
            PanelOutput {
                rows: vec![("paid".into(), "37".into()), ("waiting".into(), "2".into())],
                series: vec![3.0, 5.0, 2.0, 7.0, 9.0, 6.0, 11.0, 13.0, 10.0, 8.0, 12.0, 6.0],
                ..out("4.82", "XMR", Status::Ok)
            },
        ),
        ("mk-bell", "bell", PanelOutput { note: Some("last 0.8 s ago".into()), ..out("Ringing", "", Status::Ok) }),
        ("mk-lws", "scanner", PanelOutput { note: Some("0 blocks behind".into()), ..out("214", "accounts", Status::Ok) }),
        (
            "nodo",
            "device",
            PanelOutput {
                bar: Some(0.27),
                rows: vec![("ssd".into(), "541 GB / 2.0 TB".into()), ("mem".into(), "6.1 / 31 GB".into()), ("up".into(), "3 d 4 h".into())],
                ..out("52", "°C", Status::Ok)
            },
        ),
        ("mk-push-relay", "relay", PanelOutput { note: Some("last push 4 min ago".into()), ..out("3", "devices", Status::Ok) }),
    ];
    for (package, id, output) in samples {
        if let Some(state) = app.states.get_mut(&PanelKey::new(package, id)) {
            state.record(Ok(output), now, Duration::from_secs(60));
        }
    }
    app
}

fn centre(r: geometry::Rect) -> (f32, f32) {
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

/// Puts the sample into the state a scene shows.
pub fn scene(name: &str, now: Instant) -> Result<App, String> {
    let mut app = sample(now);
    let device = 4;
    match name {
        "view" => {}
        "arrange" | "drag" | "hidden" | "add" => {
            app.act(Button::Arrange, now);
            let p = &app.layout.placements[device];
            let (x, y) = centre(geometry::cells(p.x, p.y, 1, 1));
            app.touch_down(x, y, now);
            match name {
                "arrange" => app.touch_up(),
                "drag" => {
                    let to = geometry::cells(6, 2, 1, 1);
                    app.touch_move(to.x + to.w / 2.0 + 30.0, to.y + to.h / 2.0 + 20.0);
                }
                "hidden" => {
                    app.touch_up();
                    app.act(Button::Hide, now);
                }
                _ => {
                    app.touch_up();
                    app.act(Button::Add, now);
                }
            }
        }
        "confirm" => {
            app.act(Button::ShutDown, now);
        }
        "progress" | "failed" | "off" => {
            app.act(Button::ShutDown, now);
            app.mode = Mode::ShuttingDown;
            if name == "failed" {
                app.power_closed(Some("cannot reach the shutdown service (connection refused)".into()));
            } else {
                let lines = [
                    "services 6",
                    "stopping mk-push-relay",
                    "stopping mk-bell",
                    "stopping mk-pay",
                    "stopping mk-lws",
                    "stopping sshd",
                    "stopping cuprated",
                    "stopped mk-push-relay 400",
                    "stopped mk-bell 600",
                    "stopped mk-pay 1100",
                    "stopped mk-lws 2300",
                    "stopped sshd 200",
                ];
                for line in lines {
                    app.power_event(nodo_power::Event::parse(line).ok_or_else(|| format!("bad progress line {line:?}"))?);
                }
                if name == "off" {
                    for line in ["stopped cuprated 38000", "syncing", "synced 900", "off"] {
                        app.power_event(nodo_power::Event::parse(line).ok_or_else(|| format!("bad progress line {line:?}"))?);
                    }
                }
            }
        }
        other => return Err(format!("no scene {other:?}; scenes: {}", SCENES.join(", "))),
    }
    Ok(app)
}

/// Renders a scene into RGB pixels, 1920 x 1080.
pub fn pixels(name: &str) -> Result<Vec<Rgb8Pixel>, String> {
    // The platform can be set once per process: the first scene makes the window, later ones reuse it.
    let window = PLATFORM_WINDOW.with(|w| {
        w.borrow_mut()
            .get_or_insert_with(|| {
                let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
                let _ = slint::platform::set_platform(Box::new(Offscreen { window: window.clone() }));
                window
            })
            .clone()
    });
    window.set_size(PhysicalSize::new(geometry::WIDTH as u32, geometry::HEIGHT as u32));
    let mut buffer = vec![Rgb8Pixel::default(); (geometry::WIDTH * geometry::HEIGHT) as usize];
    if G2_SCENES.contains(&name) {
        let board = DesignBoard2::new().map_err(|e| e.to_string())?;
        board.set_scene(name.into());
        board.show().map_err(|e| e.to_string())?;
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut buffer, geometry::WIDTH as usize);
        });
        board.hide().map_err(|e| e.to_string())?;
        return Ok(buffer);
    }
    if is_design(name) {
        let board = DesignBoard::new().map_err(|e| e.to_string())?;
        board.set_scene(name.into());
        board.show().map_err(|e| e.to_string())?;
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut buffer, geometry::WIDTH as usize);
        });
        board.hide().map_err(|e| e.to_string())?;
        return Ok(buffer);
    }
    let ui = MainWindow::new().map_err(|e| e.to_string())?;
    ui.set_host("monokulo-nodo".into());
    ui.set_address("192.168.1.155".into());
    let now = Instant::now();
    let app = scene(if name == "wp-panels" { "view" } else { name }, now)?;
    let mut presenter = Presenter::new(&ui);
    presenter.show(&ui, &app, now, ("thu 13 nov", "14:32"));
    if name == "wp-panels" {
        add_manage_button(&ui);
    }
    ui.show().map_err(|e| e.to_string())?;
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, geometry::WIDTH as usize);
    });
    ui.hide().map_err(|e| e.to_string())?;
    Ok(buffer)
}

/// The proposed "Manage" button, left of the top bar's other buttons (design only).
fn add_manage_button(ui: &MainWindow) {
    let model = ui.get_buttons();
    let mut buttons: Vec<ButtonView> = model.iter().collect();
    let Some(first) = buttons.iter().filter(|b| b.y < 140.0).min_by(|a, b| a.x.total_cmp(&b.x)).cloned() else { return };
    for b in buttons.iter_mut().filter(|b| b.label == "Arrange") {
        b.icon = 0;
    }
    let w = 236.0;
    buttons.push(ButtonView { x: first.x - w - geometry::BUTTON_GAP, w, label: "Manage".into(), count: "".into(), sub: "".into(), kind: 2, icon: 1, ..first });
    let left = first.x - w - geometry::BUTTON_GAP;
    ui.set_header_text_width(left - geometry::PAD_X - geometry::BUTTON_GAP);
    ui.set_buttons(slint::ModelRc::new(slint::VecModel::from(buttons)));
}

thread_local! {
    static PLATFORM_WINDOW: std::cell::RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { std::cell::RefCell::new(None) };
}

pub fn render(name: &str, path: &Path) -> Result<(), String> {
    let buffer = pixels(name)?;
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), geometry::WIDTH as u32, geometry::HEIGHT as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = buffer.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    writer.write_image_data(&bytes).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_renders_and_is_mostly_black() {
        // Slint's platform is per process: every scene in one test.
        for name in SCENES {
            let buffer = pixels(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            let lit = buffer.iter().filter(|p| p.r > 0 || p.g > 0 || p.b > 0).count();
            assert!(lit > 1000, "{name} drew something");
            assert!(lit < buffer.len() / 3, "{name} keeps an OLED's pixels mostly off ({lit})");
        }
        assert!(scene("nothing", Instant::now()).is_err());
        // The touch management design boards render too (mock data; they light more of the screen).
        for name in DESIGN_SCENES.iter().copied().chain([NO_PIN_SCENE]).chain(G2_SCENES.iter().copied()) {
            let buffer = pixels(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            let lit = buffer.iter().filter(|p| p.r > 0 || p.g > 0 || p.b > 0).count();
            assert!(lit > 1000, "{name} drew something");
        }
    }
}
