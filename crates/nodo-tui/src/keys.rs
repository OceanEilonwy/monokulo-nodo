//! The key that does what a message from the core points at.
//!
//! The core's words name no key, since the built-in screen and the web pages have none. Where a
//! message points at something to do, it carries a `Hint`, and this is where the terminal says
//! which key does it.

use nodo_ui_core::flows::{Hint, Notice};

/// The core's `text` as the terminal says it, with the key for what it points at.
pub fn with_key(text: &str, hint: Option<Hint>) -> String {
    match hint {
        None => text.to_owned(),
        Some(Hint::Unlock) => format!("{text} Press u to unlock this device."),
        // "Turn it on first." is an instruction already, so the key follows it directly.
        Some(Hint::TurnOn) => match text.strip_suffix(" first.") {
            Some(start) => format!("{start} first: press o."),
            None => format!("{text} Press o to turn it on."),
        },
        Some(Hint::ScanWifi) => "Press s to look for networks.".to_owned(),
        Some(Hint::ScanBluetooth) => "Press s to look for devices. Put the device in its pairing mode first.".to_owned(),
        Some(Hint::LookForDrives) => "No USB drive is plugged in. Plug one in and press g.".to_owned(),
        Some(Hint::Install) => text.replace("Installing downloads and checks it first.", "Press i to download and check it."),
    }
}

/// A notice with the key in its text.
pub fn notice(notice: Notice) -> Notice {
    Notice { text: with_key(&notice.text, notice.hint), hint: None, ..notice }
}

/// A flow's error with the key in its text.
pub fn error(error: &Option<String>, hint: Option<Hint>) -> Option<String> {
    error.as_ref().map(|text| with_key(text, hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_message_reads_as_it_did_when_the_core_named_the_key() {
        let said = [
            ("bell is locked.", Hint::Unlock, "bell is locked. Press u to unlock this device."),
            ("Wi-Fi is off.", Hint::TurnOn, "Wi-Fi is off. Press o to turn it on."),
            ("Bluetooth is off.", Hint::TurnOn, "Bluetooth is off. Press o to turn it on."),
            ("Turn it on first.", Hint::TurnOn, "Turn it on first: press o."),
            ("Look for networks first.", Hint::ScanWifi, "Press s to look for networks."),
            (
                "Look for devices first, with the device in its pairing mode.",
                Hint::ScanBluetooth,
                "Press s to look for devices. Put the device in its pairing mode first.",
            ),
            (
                "No USB drive is plugged in. Plug one in and look again.",
                Hint::LookForDrives,
                "No USB drive is plugged in. Plug one in and press g.",
            ),
            (
                "Offered by the feed extras. Installing downloads and checks it first. You then see everything it asks for before anything installs.",
                Hint::Install,
                "Offered by the feed extras. Press i to download and check it. You then see everything it asks for before anything installs.",
            ),
        ];
        for (text, hint, expected) in said {
            assert_eq!(with_key(text, Some(hint)), expected);
        }
        assert_eq!(with_key("Saved.", None), "Saved.");
        let told = notice(Notice::error("bell is locked.").hinting(Hint::Unlock));
        assert_eq!(told, Notice::error("bell is locked. Press u to unlock this device."));
    }
}
