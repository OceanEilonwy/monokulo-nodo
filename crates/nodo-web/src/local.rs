//! The device's second LAN address, used only for local sites.
//!
//! The router forwards internet traffic to the device's main address. Local sites are served only on
//! a separate address, so a forwarded connection can never be addressed to them, whatever source
//! address a masquerading router gives it. The `nodo-localnet` service obtains the address and writes
//! its state to [`STATE_FILE`]. When there is no address, local sites stay unavailable. They are never
//! served on the main address or on another port instead.

use serde::Serialize;
use std::net::Ipv4Addr;

pub const STATE_FILE: &str = "/run/nodo-localnet/state";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum LocalAddress {
    Ready { address: Ipv4Addr },
    Unavailable { reason: LocalUnavailable },
}

impl Default for LocalAddress {
    fn default() -> Self {
        Self::Unavailable { reason: LocalUnavailable::NotStarted }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum LocalUnavailable {
    /// The service has not reported yet.
    NotStarted,
    /// Local sites are switched off for the device.
    Disabled,
    WaitingForLease,
    /// The DHCP server gave no second lease, or the lease ended.
    NoLease,
    /// The main interface is Wi-Fi. Access points usually drop frames from a second MAC address.
    WirelessParent,
    /// No interface carries the default route, so there is no LAN to join.
    NoParent,
    /// The interface could not be created. The detail is the interface name or command that failed.
    SetupFailed(String),
    /// The address is not a private IPv4 address inside the LAN ranges.
    UnsuitableAddress(Ipv4Addr),
}

impl LocalUnavailable {
    /// One line for the screens, saying why local sites are not available.
    pub fn summary(&self) -> String {
        match self {
            Self::NotStarted => "The local address has not started yet.".to_owned(),
            Self::Disabled => "Local sites are switched off.".to_owned(),
            Self::WaitingForLease => "Waiting for the router to give the local address.".to_owned(),
            Self::NoLease => "The router gave no second address for local sites.".to_owned(),
            Self::WirelessParent => "This device is on Wi-Fi, which refuses a second hardware address.".to_owned(),
            Self::NoParent => "This device is not connected to a network.".to_owned(),
            Self::SetupFailed(detail) => format!("The local address could not be set up: {detail}"),
            Self::UnsuitableAddress(ip) => format!("{ip} is not a private address on this network."),
        }
    }

    /// What the screens tell the user to do. A static address works where DHCP or a second MAC does not.
    pub fn remedy(&self) -> &'static str {
        match self {
            Self::NotStarted | Self::WaitingForLease => "Wait a moment. Local sites start when the address is ready.",
            Self::Disabled => "Turn on local sites in Settings.",
            Self::NoLease => {
                "Your router did not give this device a second address. Allow more DHCP clients, or set a \
                 static local address outside the router's DHCP range."
            }
            Self::WirelessParent => {
                "This device is on Wi-Fi, which usually refuses a second hardware address. Set a static local \
                 address outside the router's DHCP range, or connect a network cable."
            }
            Self::NoParent => "Connect this device to your network.",
            Self::SetupFailed(_) => "Set a static local address outside the router's DHCP range.",
            Self::UnsuitableAddress(_) => "Choose a private address on your local network, such as 192.168.1.60.",
        }
    }
}

/// Parses the `key=value` lines `nodo-localnet` writes. Anything unexpected is unavailable, never ready.
pub fn parse_state(text: &str) -> LocalAddress {
    let get = |key: &str| {
        text.lines().find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')).map(str::trim))
    };
    let reason = match get("status") {
        Some("ready") => match get("address").and_then(|a| a.parse::<Ipv4Addr>().ok()) {
            Some(address) => return LocalAddress::Ready { address },
            None => LocalUnavailable::SetupFailed("no address in state".to_owned()),
        },
        Some("disabled") => LocalUnavailable::Disabled,
        Some("waiting") => LocalUnavailable::WaitingForLease,
        Some("no-lease") => LocalUnavailable::NoLease,
        Some("wireless") => LocalUnavailable::WirelessParent,
        Some("no-parent") => LocalUnavailable::NoParent,
        Some("failed") => LocalUnavailable::SetupFailed(
            get("detail").unwrap_or_default().chars().filter(|c| c.is_ascii_graphic() || *c == ' ').take(80).collect(),
        ),
        _ => LocalUnavailable::NotStarted,
    };
    LocalAddress::Unavailable { reason }
}

pub fn read_state() -> LocalAddress {
    std::fs::read_to_string(STATE_FILE).map(|text| parse_state(&text)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_needs_an_address() {
        assert_eq!(
            parse_state("status=ready\naddress=192.168.1.60\ninterface=nodo-local\n"),
            LocalAddress::Ready { address: Ipv4Addr::new(192, 168, 1, 60) }
        );
        assert!(matches!(parse_state("status=ready\n"), LocalAddress::Unavailable { .. }));
        assert!(matches!(parse_state("status=ready\naddress=fe80::1\n"), LocalAddress::Unavailable { .. }));
    }

    #[test]
    fn every_failure_has_a_reason_and_a_remedy() {
        for (text, reason) in [
            ("status=no-lease", LocalUnavailable::NoLease),
            ("status=wireless", LocalUnavailable::WirelessParent),
            ("status=waiting", LocalUnavailable::WaitingForLease),
            ("status=no-parent", LocalUnavailable::NoParent),
            ("status=disabled", LocalUnavailable::Disabled),
            ("", LocalUnavailable::NotStarted),
            ("status=bogus", LocalUnavailable::NotStarted),
        ] {
            let LocalAddress::Unavailable { reason: got } = parse_state(text) else { panic!("{text}") };
            assert_eq!(got, reason, "{text}");
            assert!(!got.remedy().is_empty());
        }
        assert_eq!(
            parse_state("status=failed\ndetail=ip link \u{1b}[31madd\n"),
            LocalAddress::Unavailable { reason: LocalUnavailable::SetupFailed("ip link [31madd".to_owned()) }
        );
    }

    #[test]
    fn serializes_for_the_screens() {
        let value = serde_json::to_value(LocalAddress::Unavailable { reason: LocalUnavailable::WirelessParent }).unwrap();
        assert_eq!(value["status"], "unavailable");
        assert_eq!(value["reason"]["kind"], "wireless_parent");
    }
}
