//! What a `Hint` of the core becomes on a page.
//!
//! The core's words name no key and no button. Where a message points at something to do it
//! carries a `Hint`, and each interface offers that its own way: the terminal names a key, and
//! these pages give a link to the page where it is done, or the button itself where the message
//! is shown on that page.

use maud::{html, Markup};
use nodo_ui_core::flows::Hint;

use crate::dest;

/// One of the two radios of the Network page, for a hint that says "turn it on".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Radio {
    Wifi,
    Bluetooth,
}

/// Where a hint leads from any page: the words of the link and its URL. `None` for a hint that
/// only means something on the page it was given on (an install is of one app).
pub fn link(hint: Hint) -> Option<(&'static str, &'static str)> {
    match hint {
        Hint::Unlock => Some(("Unlock the Nodo", dest::UNLOCK)),
        Hint::TurnOn | Hint::ScanWifi | Hint::ScanBluetooth => Some(("Open Network", dest::NETWORK)),
        Hint::LookForDrives => Some(("Open Backups", dest::BACKUPS)),
        Hint::Install => None,
    }
}

/// The link of a hint, for a notice shown on whatever page comes next.
pub fn linked(hint: Option<Hint>) -> Markup {
    html! {
        @if let Some((words, url)) = hint.and_then(link) { " " a href=(url) { (words) } }
    }
}

/// The button of a hint on the Network page, inside a form that posts there: the `do` it sends
/// and its words. `radio` is the one the refused action was about.
pub fn network_button(hint: Hint, radio: Radio) -> Option<(&'static str, &'static str)> {
    match (hint, radio) {
        (Hint::TurnOn, Radio::Wifi) => Some(("wifi", "Turn Wi-Fi on")),
        (Hint::TurnOn, Radio::Bluetooth) => Some(("bluetooth", "Turn Bluetooth on")),
        (Hint::ScanWifi, _) => Some(("scan-wifi", "Look for networks")),
        (Hint::ScanBluetooth, _) => Some(("scan-bluetooth", "Look for devices")),
        (Hint::Unlock | Hint::LookForDrives | Hint::Install, _) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hint_is_a_link_or_a_button_and_never_a_key() {
        assert_eq!(linked(Some(Hint::Unlock)).into_string(), r#" <a href="/unlock">Unlock the Nodo</a>"#);
        assert_eq!(linked(None).into_string(), "");
        assert_eq!(linked(Some(Hint::Install)).into_string(), "");
        assert_eq!(network_button(Hint::TurnOn, Radio::Bluetooth), Some(("bluetooth", "Turn Bluetooth on")));
        assert_eq!(network_button(Hint::ScanWifi, Radio::Bluetooth), Some(("scan-wifi", "Look for networks")));
    }
}
