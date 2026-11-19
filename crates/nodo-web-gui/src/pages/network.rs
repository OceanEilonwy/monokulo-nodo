//! Network (`flows::network::Network`): Wi-Fi and Bluetooth, the networks in range and the
//! saved ones, and devices.
//!
//! The flow is kept in the session, since it holds what a scan found and the network whose
//! passphrase is being asked. The radios are read again whenever the page is opened. While a
//! join waits for an address or a pairing runs, the page asks `…/status` once a second, which
//! calls the flow's `poll`; that route does not count as the owner's activity.
//!
//! This page is served over the network it changes. Where the browser may be reaching the Nodo
//! over the Wi-Fi it is about to turn off or forget, the page says so; the question before it
//! is the flow's.

use axum::response::Response;
use axum::Form;
use maud::{html, Markup};
use nodo_api::{PairingStage, WifiLink, WifiSecurity};
use nodo_ui_core::flows::network::{Network, RadioRead};
use nodo_ui_core::flows::Step;
use nodo_ui_core::screen::Tab;

use crate::core_wishlist::typed_secret;
use crate::ctx::{Ctx, Out, Work};
use crate::dest;
use crate::layout::{self, Page};
use crate::runner::{follow_plain, Reply};

type Posted = Vec<(String, String)>;

fn posted<'a>(form: &'a Posted, name: &str) -> &'a str {
    form.iter().find(|(key, _)| key == name).map_or("", |(_, value)| value.as_str())
}

fn kept(work: &Work<'_>) -> Network {
    work.session.inner().stash.network.clone().unwrap_or_default()
}

fn keep(work: &Work<'_>, flow: &Network) {
    work.session.inner().stash.network = Some(flow.clone());
}

/// Reads both radios again, keeping what the flow had to say about the last action.
fn read_radios(work: &mut Work<'_>, flow: &mut Network) -> Reply {
    let said = flow.error.take();
    let effects = flow.ask_again();
    work.session.inner().stash.network_asked = work.now;
    let reply = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome));
    if flow.error.is_none() {
        flow.error = said;
    }
    reply
}

fn security_words(security: WifiSecurity) -> &'static str {
    match security {
        WifiSecurity::Open => "open, not encrypted",
        WifiSecurity::Wpa2Personal => "WPA2",
        WifiSecurity::Wpa3Personal => "WPA3",
        WifiSecurity::Wpa2Wpa3Personal => "WPA2/WPA3",
        WifiSecurity::Legacy => "WEP or WPA, broken",
        WifiSecurity::Enterprise => "WPA Enterprise",
    }
}

fn radio_note(read: &RadioRead) -> Option<String> {
    match read {
        RadioRead::Asking => Some("Reading…".to_owned()),
        RadioRead::NoAnswer => Some("No answer yet.".to_owned()),
        RadioRead::Failed(why) => Some(why.clone()),
        RadioRead::Read => None,
    }
}

/// Both radios. While something is under way, htmx replaces this once a second.
fn status(flow: &Network) -> Markup {
    let here = dest::NETWORK;
    let button = |action: &str, label: &str, class: &str| html! { button class=(class) type="submit" name="do" value=(action) { (label) } };
    let polling = flow.pairing_running() || flow.waiting_for_address();
    let joined = flow.joined().map(str::to_owned);
    let wired = flow.wifi.as_ref().is_some_and(|wifi| wifi.wired_up);
    html! {
        div.stack #radios hx-get=[polling.then(|| format!("{here}/status"))] hx-trigger=[polling.then_some("every 1s")] hx-swap="outerHTML" {
            @if let Some(busy) = &flow.busy { p.notice { "▸ " (busy) } }
            section.card {
                div.head {
                    h2 { "Wi-Fi" }
                    form.actions method="post" action=(here) {
                        @if let Some(wifi) = &flow.wifi {
                            @if wifi.enabled { (button("scan-wifi", "Look for networks", "btn")) }
                            (button("wifi", if wifi.enabled { "● On" } else { "○ Off" }, "btn"))
                        }
                    }
                }
                @if let Some(note) = radio_note(&flow.wifi_read) { p.dim { (note) } }
                @if let Some(wifi) = &flow.wifi {
                    @if let Some(why) = &wifi.unavailable { p.dim { (why) } }
                    p {
                        @match wifi.link {
                            WifiLink::Off => { "Off." },
                            WifiLink::Disconnected => { "On, not joined to a network." },
                            WifiLink::Connecting => { "Joining " (wifi.ssid.clone().unwrap_or_default()) "…" },
                            WifiLink::Connected => {
                                "Joined to " strong { (wifi.ssid.clone().unwrap_or_default()) }
                                @match &wifi.address { Some(address) => { " · " span.mono { (address) } }, None => { " · waiting for an address" }, }
                            },
                        }
                        span.dim { @if wired { "   The cable is connected too." } @else { "   The cable is not connected." } }
                    }
                    // Said plainly, because this page may be reaching the Nodo over that Wi-Fi.
                    @if joined.is_some() && !wired {
                        p.warn { "▲ If your browser reaches this Nodo over " (joined.clone().unwrap_or_default()) ", turning Wi-Fi off or forgetting that network cuts this page off. Only the cable or the Nodo's own screen brings it back." }
                    }
                }
                div.rows {
                    @for row in flow.wifi_rows() {
                        @if let Some((ssid, security, saved)) = flow.wifi_row(row) {
                            div.row {
                                span.name { (ssid) }
                                span.what {
                                    (security_words(security))
                                    @if joined.as_deref() == Some(ssid.as_str()) { " · " span.ok { "joined" } } @else if saved { " · saved" }
                                }
                                span.end {
                                    form.actions method="post" action=(here) {
                                        (layout::hidden("ssid", &ssid))
                                        @if joined.as_deref() != Some(ssid.as_str()) { (button("join", "Join", "btn")) }
                                    }
                                    @if saved {
                                        form method="post" action=(here) hx-post=(here) hx-target="#dialog" hx-swap="innerHTML" {
                                            (layout::hidden("ssid", &ssid))
                                            (button("forget", "Forget…", "btn danger"))
                                        }
                                    }
                                }
                                span.go {}
                            }
                        }
                    }
                }
            }
            section.card {
                div.head {
                    h2 { "Bluetooth" }
                    form.actions method="post" action=(here) {
                        @if let Some(bluetooth) = &flow.bluetooth {
                            @if bluetooth.enabled { (button("scan-bluetooth", "Look for devices", "btn")) }
                            (button("bluetooth", if bluetooth.enabled { "● On" } else { "○ Off" }, "btn"))
                        }
                    }
                }
                @if let Some(note) = radio_note(&flow.bluetooth_read) { p.dim { (note) } }
                @if let Some(bluetooth) = &flow.bluetooth {
                    @if let Some(why) = &bluetooth.unavailable { p.dim { (why) } }
                    // A pairing under way, with the code the two devices agree on.
                    @if let Some(pairing) = &bluetooth.pairing {
                        @match &pairing.stage {
                            PairingStage::Pairing => p { "Pairing with " span.mono { (pairing.address) } "…" },
                            PairingStage::TypePasskey(code) => p.warn { "Type " strong.mono { (code) } " on the device, then Enter." },
                            PairingStage::ComparePasskey(code) => p.warn { "Check that the device shows " strong.mono { (code) } " and accept it there." },
                            PairingStage::Done | PairingStage::Failed(_) => {},
                        }
                    }
                    div.rows {
                        @for device in &bluetooth.devices {
                            div.row {
                                span.name { (device.name) }
                                span.what {
                                    span.mono { (device.address) } @if let Some(kind) = &device.kind { " · " (kind) }
                                    @if device.connected { " · " span.ok { "connected" } } @else if device.paired { " · paired" }
                                }
                                span.end {
                                    form.actions method="post" action=(here) {
                                        (layout::hidden("address", &device.address))
                                        (button("device", if !device.paired { "Pair" } else if device.connected { "Disconnect" } else { "Connect" }, "btn"))
                                    }
                                    @if device.paired {
                                        form method="post" action=(here) hx-post=(here) hx-target="#dialog" hx-swap="innerHTML" {
                                            (layout::hidden("address", &device.address))
                                            (button("remove-device", "Remove…", "btn danger"))
                                        }
                                    }
                                }
                                span.go {}
                            }
                        }
                    }
                }
            }
        }
    }
}

fn page(flow: &Network) -> Page {
    let here = dest::NETWORK;
    let body = html! {
        h1 { "Network" }
        p.lead { "The cable works by itself. Wi-Fi and Bluetooth stay off until you turn them on here." }
        @if let Some(error) = &flow.error { p.notice.error { "✕ " (error) } }
        @if let Some(prompt) = &flow.prompt {
            // The network chosen is the flow's; the form carries only its passphrase.
            section.card.attn #wifi-prompt {
                h2 { "Join " (prompt.ssid) }
                p.dim { (security_words(prompt.security)) }
                form.stack method="post" action=(here) {
                    div.field {
                        label for="wifi-passphrase" { "Wi-Fi passphrase" }
                        input type="password" id="wifi-passphrase" name="passphrase" autocomplete="off" autofocus;
                    }
                    div.actions.end {
                        button.btn type="submit" name="do" value="cancel-prompt" { "Cancel" }
                        button.btn.primary type="submit" name="do" value="passphrase" { "Join" }
                    }
                }
            }
        }
        (status(flow))
    };
    let page = Page::new(Tab::System, "Network", body).trail(&[("System", dest::SYSTEM), ("Network", dest::NETWORK)]);
    if flow.pairing_running() || flow.waiting_for_address() { page.waiting() } else { page }
}

/// `GET /system/network`: both radios, read again.
pub async fn show(ctx: Ctx) -> Response {
    ctx.with(dest::SYSTEM, |work| {
        let mut flow = kept(work);
        let reply = read_radios(work, &mut flow);
        if reply.login_required {
            return Out::Reply(reply);
        }
        let shown = page(&flow);
        // What the last action had to say is said once.
        flow.error = None;
        keep(work, &flow);
        Out::Page(shown)
    })
    .await
}

/// `GET /system/network/status`: the radios alone, while a join waits for its address or a
/// pairing runs. The flow says what to read again, by this process's clock in whole seconds.
/// A polling route.
pub async fn poll(ctx: Ctx) -> Response {
    ctx.with(dest::NETWORK, |work| {
        let mut flow = kept(work);
        let asked = work.session.inner().stash.network_asked;
        flow.no_answer_after(work.now.saturating_sub(asked).as_secs());
        let effects = flow.poll(work.now.as_secs());
        let reply = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome));
        if reply.login_required {
            return Out::Reply(reply);
        }
        // How a pairing or a join ended is a notice, kept for the page.
        work.session.inner().notices.extend(reply.notices);
        keep(work, &flow);
        Out::Fragment(status(&flow))
    })
    .await
}

/// `POST /system/network`: every button of the page, named by `do`. Networks and devices are
/// named by their SSID and address, which the flow looks up in what it read.
pub async fn act(ctx: Ctx, Form(form): Form<Posted>) -> Response {
    ctx.with(dest::NETWORK, move |work| {
        let mut flow = kept(work);
        // Something else may have left it marked busy; the radios say how things stand.
        if flow.wifi.is_none() && flow.bluetooth.is_none() {
            let _read = read_radios(work, &mut flow);
        }
        let (ssid, address) = (posted(&form, "ssid"), posted(&form, "address"));
        let step = match posted(&form, "do") {
            "wifi" => flow.toggle_wifi(),
            "scan-wifi" => Step::run(flow.scan_wifi()),
            "join" => flow.join(ssid),
            "passphrase" => Step::run(flow.submit_passphrase(&mut typed_secret(posted(&form, "passphrase")))),
            "cancel-prompt" => {
                flow.cancel_prompt();
                Step::Stay
            }
            "forget" => flow.ask_forget(ssid),
            "bluetooth" => Step::run(flow.toggle_bluetooth()),
            "scan-bluetooth" => Step::run(flow.scan_bluetooth()),
            "device" => Step::run(flow.pair_or_connect(address)),
            "remove-device" => flow.ask_remove_bluetooth(address),
            _ => Step::run(flow.ask_again()),
        };
        // A question is answered on its own page or dialog; everything else runs now.
        let reply = if matches!(step, Step::Ask(_)) { follow_plain(step) } else { work.run(step, &mut |outcome, _| flow.apply(outcome)) };
        keep(work, &flow);
        Out::Reply(reply)
    })
    .await
}
