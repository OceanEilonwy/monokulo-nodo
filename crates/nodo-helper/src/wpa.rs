//! wpa_supplicant's text: the control interface's replies and events, and the configuration file
//! the helper writes. Pure functions, so every rule is tested without a radio.
//!
//! The helper speaks to wpa_supplicant only over its control socket, never through `wpa_cli`,
//! whose arguments any local user could read from `/proc`. Names go over the socket and into the
//! file as hex, so no SSID can break out of its line.

use std::collections::BTreeMap;

use nodo_api::{WifiNetwork, WifiSecurity};
use sha1::Sha1;
use zeroize::Zeroizing;

/// The credential a saved network is joined with.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    Open,
    /// WPA2 only: the 256-bit key derived from the passphrase and the SSID, as 64 hex digits. The
    /// passphrase itself is not kept, so it cannot be read back for use on another network.
    Psk(Zeroizing<String>),
    /// WPA3, alone or beside WPA2: SAE needs the passphrase itself.
    Passphrase(Zeroizing<String>),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Credential::Open => "Open",
            Credential::Psk(_) => "Psk(<redacted>)",
            Credential::Passphrase(_) => "Passphrase(<redacted>)",
        })
    }
}

/// A network as wpa_supplicant is told about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkConfig {
    pub ssid: String,
    pub security: WifiSecurity,
    pub credential: Credential,
    /// Higher is tried first. The network joined last gets the highest.
    pub priority: u32,
}

/// The WPA2 pre-shared key, PBKDF2-HMAC-SHA1 of the passphrase salted with the SSID, 4096 rounds,
/// as IEEE 802.11i defines it.
pub fn derive_psk(passphrase: &str, ssid: &str) -> Zeroizing<String> {
    let mut key = Zeroizing::new([0u8; 32]);
    pbkdf2::pbkdf2_hmac::<Sha1>(passphrase.as_bytes(), ssid.as_bytes(), 4096, key.as_mut());
    let mut text = Zeroizing::new(String::with_capacity(64));
    for byte in key.iter() {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Why a passphrase cannot be used, or `None` when it can: WPA allows 8 to 63 printable ASCII
/// characters.
pub fn passphrase_problem(passphrase: &str) -> Option<&'static str> {
    if !passphrase.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Some("The passphrase may only hold letters, digits, spaces and the usual symbols.");
    }
    match passphrase.len() {
        0..=7 => Some("A Wi-Fi passphrase has at least 8 characters."),
        8..=63 => None,
        _ => Some("A Wi-Fi passphrase has at most 63 characters."),
    }
}

/// Why a network name cannot be used, or `None` when it can.
pub fn ssid_problem(ssid: &str) -> Option<&'static str> {
    if ssid.is_empty() {
        return Some("The network name is empty.");
    }
    if ssid.len() > 32 {
        return Some("A Wi-Fi network name has at most 32 bytes.");
    }
    if ssid.chars().any(char::is_control) {
        return Some("The network name holds control characters.");
    }
    None
}

/// The credential to keep for a network, from the passphrase the owner typed.
pub fn credential(security: WifiSecurity, ssid: &str, passphrase: Option<&str>) -> Credential {
    match (security, passphrase) {
        (WifiSecurity::Wpa2Personal, Some(p)) => Credential::Psk(derive_psk(p, ssid)),
        (WifiSecurity::Wpa3Personal | WifiSecurity::Wpa2Wpa3Personal, Some(p)) => {
            Credential::Passphrase(Zeroizing::new(p.to_owned()))
        }
        _ => Credential::Open,
    }
}

/// `SET_NETWORK` values, in order, for a network. The credential is among them, so callers never
/// log these.
pub fn network_fields(network: &NetworkConfig) -> Vec<(&'static str, Zeroizing<String>)> {
    let z = |s: &str| Zeroizing::new(s.to_owned());
    let mut fields = vec![("ssid", z(&hex(network.ssid.as_bytes())))];
    match network.security {
        WifiSecurity::Wpa3Personal => {
            fields.push(("key_mgmt", z("SAE")));
            fields.push(("ieee80211w", z("2")));
        }
        WifiSecurity::Wpa2Wpa3Personal => {
            fields.push(("key_mgmt", z("SAE WPA-PSK WPA-PSK-SHA256")));
            fields.push(("ieee80211w", z("1")));
        }
        WifiSecurity::Wpa2Personal => {
            fields.push(("key_mgmt", z("WPA-PSK WPA-PSK-SHA256")));
            fields.push(("ieee80211w", z("1")));
        }
        _ => fields.push(("key_mgmt", z("NONE"))),
    }
    if network.security.needs_passphrase() {
        // WPA2 and WPA3 need CCMP. TKIP is never offered, so a downgrade to it fails.
        fields.push(("proto", z("RSN")));
        fields.push(("pairwise", z("CCMP")));
    }
    match &network.credential {
        Credential::Psk(psk) => fields.push(("psk", psk.clone())),
        // A quoted value is a passphrase. Printable ASCII only, so no quote ends it early: the
        // parser takes everything up to the last quote.
        Credential::Passphrase(p) => fields.push(("psk", Zeroizing::new(format!("\"{}\"", p.as_str())))),
        Credential::Open => {}
    }
    fields.push(("priority", z(&network.priority.to_string())));
    fields
}

/// The whole configuration file. It holds credentials, so it is written root-only.
pub fn render_config(ctrl_dir: &str, networks: &[NetworkConfig]) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::new());
    out.push_str("# Written by nodo-helper. Changes here are replaced on the next Wi-Fi change.\n");
    out.push_str(&format!("ctrl_interface=DIR={ctrl_dir} GROUP=root\n"));
    out.push_str("update_config=0\nap_scan=1\nautoscan=exponential:3:300\n");
    // Saved networks are never probed for by name, so the device does not announce them.
    for network in networks {
        out.push_str("network={\n");
        for (key, value) in network_fields(network) {
            out.push_str(&format!("\t{key}={}\n", value.as_str()));
        }
        out.push_str("}\n");
    }
    out
}

/// The security a scan result's flags announce, such as `[WPA2-PSK-CCMP][RSN-SAE-CCMP][ESS]`.
pub fn security_from_flags(flags: &str) -> WifiSecurity {
    let rsn: Vec<&str> =
        flags.split(['[', ']']).filter(|f| f.starts_with("RSN-") || f.starts_with("WPA2-")).collect();
    let has = |needle: &str| rsn.iter().any(|f| f.split(['-', '+']).any(|part| part == needle));
    if flags.contains("EAP") {
        return WifiSecurity::Enterprise;
    }
    let sae = has("SAE");
    let psk = has("PSK");
    match (sae, psk) {
        (true, true) => WifiSecurity::Wpa2Wpa3Personal,
        (true, false) => WifiSecurity::Wpa3Personal,
        (false, true) => WifiSecurity::Wpa2Personal,
        _ if flags.contains("[WEP]") || flags.contains("[WPA-") => WifiSecurity::Legacy,
        _ => WifiSecurity::Open,
    }
}

/// Undoes wpa_supplicant's escaping of an SSID: `\\`, `\"`, `\n`, `\r`, `\t`, `\e` and `\xNN`.
/// `None` when the name is not UTF-8, which this device then does not offer.
pub fn unescape_ssid(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut chars = text.bytes();
    while let Some(b) = chars.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match chars.next()? {
            b'x' => {
                let hi = chars.next()?;
                let lo = chars.next()?;
                let digit = |c: u8| (c as char).to_digit(16);
                bytes.push((digit(hi)? * 16 + digit(lo)?) as u8);
            }
            b'n' => bytes.push(b'\n'),
            b'r' => bytes.push(b'\r'),
            b't' => bytes.push(b'\t'),
            b'e' => bytes.push(0x1b),
            other => bytes.push(other),
        }
    }
    String::from_utf8(bytes).ok()
}

/// `SCAN_RESULTS`: a header, then `bssid / frequency / signal level / flags / ssid` per line.
/// One row per name, at its strongest signal, strongest first. Hidden and non-UTF-8 names and
/// names with control characters are left out.
pub fn parse_scan_results(text: &str) -> Vec<WifiNetwork> {
    let mut best: BTreeMap<String, WifiNetwork> = BTreeMap::new();
    for line in text.lines().skip(1) {
        let mut parts = line.splitn(5, '\t');
        let (Some(_bssid), Some(freq), Some(signal), Some(flags), Some(ssid)) =
            (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some(ssid) = unescape_ssid(ssid) else { continue };
        if ssid_problem(&ssid).is_some() {
            continue;
        }
        let (Ok(frequency_mhz), Ok(signal_dbm)) = (freq.parse::<u32>(), signal.parse::<i32>()) else {
            continue;
        };
        let network = WifiNetwork {
            ssid: ssid.clone(),
            signal_dbm,
            frequency_mhz,
            security: security_from_flags(flags),
            saved: false,
            connected: false,
        };
        match best.get(&ssid) {
            Some(old) if old.signal_dbm >= signal_dbm => {}
            _ => {
                best.insert(ssid, network);
            }
        }
    }
    let mut list: Vec<WifiNetwork> = best.into_values().collect();
    list.sort_by(|a, b| b.signal_dbm.cmp(&a.signal_dbm).then_with(|| a.ssid.cmp(&b.ssid)));
    list
}

/// `key=value` lines, as `STATUS` and `SIGNAL_POLL` answer.
pub fn parse_pairs(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect()
}

/// The security `STATUS` reports for the joined network, from its `key_mgmt`.
pub fn security_from_key_mgmt(key_mgmt: &str) -> WifiSecurity {
    match key_mgmt {
        "SAE" | "FT-SAE" => WifiSecurity::Wpa3Personal,
        "NONE" => WifiSecurity::Open,
        k if k.contains("EAP") || k.contains("802.1X") => WifiSecurity::Enterprise,
        _ => WifiSecurity::Wpa2Personal,
    }
}

/// What an event from the control socket means for a join in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinEvent {
    Connected,
    WrongKey,
    /// Turned away for another reason, in wpa_supplicant's word.
    Refused(String),
    NotFound,
    Other,
}

/// Classifies an event such as `<3>CTRL-EVENT-SSID-TEMP-DISABLED id=0 ssid="x" auth_failures=1
/// duration=10 reason=WRONG_KEY`, for the network with id `id`.
pub fn join_event(event: &str, id: &str) -> JoinEvent {
    let body = event.strip_prefix('<').and_then(|e| e.split_once('>')).map_or(event, |(_, rest)| rest);
    let field = |name: &str| {
        body.split_whitespace().find_map(|w| w.strip_prefix(name).and_then(|v| v.strip_prefix('=')))
    };
    if body.starts_with("CTRL-EVENT-CONNECTED") {
        // `id=` names the network it joined.
        let joined = body.split("id=").nth(1).and_then(|r| r.split_whitespace().next());
        return if joined.is_none_or(|j| j == id) { JoinEvent::Connected } else { JoinEvent::Other };
    }
    if body.starts_with("CTRL-EVENT-SSID-TEMP-DISABLED") && field("id") == Some(id) {
        return match field("reason") {
            Some("WRONG_KEY") => JoinEvent::WrongKey,
            Some(reason) => JoinEvent::Refused(reason.to_owned()),
            None => JoinEvent::Refused("unknown".to_owned()),
        };
    }
    if body.starts_with("CTRL-EVENT-NETWORK-NOT-FOUND") {
        return JoinEvent::NotFound;
    }
    if body.starts_with("CTRL-EVENT-SAE-UNKNOWN-PASSWORD-IDENTIFIER") {
        return JoinEvent::WrongKey;
    }
    JoinEvent::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_ieee_test_vector() {
        // IEEE 802.11i-2004, H.4.1.
        assert_eq!(
            derive_psk("password", "IEEE").as_str(),
            "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"
        );
        assert_eq!(
            derive_psk("ThisIsAPassword", "ThisIsASSID").as_str(),
            "0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af"
        );
    }

    #[test]
    fn checks_passphrases_and_names() {
        assert!(passphrase_problem("short").is_some());
        assert!(passphrase_problem("eight ch").is_none());
        assert!(passphrase_problem(&"x".repeat(63)).is_none());
        assert!(passphrase_problem(&"x".repeat(64)).is_some());
        assert!(passphrase_problem("caf\u{e9} au lait").is_some());
        assert!(passphrase_problem("tab\there12").is_some());
        assert!(ssid_problem("").is_some());
        assert!(ssid_problem(&"x".repeat(33)).is_some());
        assert!(ssid_problem("a\nb").is_some());
        assert!(ssid_problem("Caf\u{e9} \"Wi-Fi\"").is_none());
    }

    #[test]
    fn reads_security_from_scan_flags() {
        assert_eq!(security_from_flags("[WPA2-PSK-CCMP][ESS]"), WifiSecurity::Wpa2Personal);
        assert_eq!(security_from_flags("[RSN-PSK-CCMP][ESS]"), WifiSecurity::Wpa2Personal);
        assert_eq!(security_from_flags("[WPA2-SAE-CCMP][ESS]"), WifiSecurity::Wpa3Personal);
        assert_eq!(security_from_flags("[WPA2-PSK+SAE-CCMP][ESS]"), WifiSecurity::Wpa2Wpa3Personal);
        assert_eq!(security_from_flags("[WPA2-PSK-SHA256+SAE-CCMP][ESS]"), WifiSecurity::Wpa2Wpa3Personal);
        assert_eq!(security_from_flags("[WPA2-EAP-CCMP][ESS]"), WifiSecurity::Enterprise);
        assert_eq!(security_from_flags("[WEP][ESS]"), WifiSecurity::Legacy);
        assert_eq!(security_from_flags("[WPA-PSK-TKIP][ESS]"), WifiSecurity::Legacy);
        assert_eq!(security_from_flags("[WPA-PSK-TKIP][WPA2-PSK-CCMP][ESS]"), WifiSecurity::Wpa2Personal);
        assert_eq!(security_from_flags("[ESS]"), WifiSecurity::Open);
        assert_eq!(security_from_flags(""), WifiSecurity::Open);
    }

    #[test]
    fn unescapes_names() {
        assert_eq!(unescape_ssid(r#"caf\xc3\xa9 \"x\" \\"#).as_deref(), Some("caf\u{e9} \"x\" \\"));
        assert_eq!(unescape_ssid(r"bad\xff"), None);
        assert_eq!(unescape_ssid(r"cut\x4"), None);
    }

    #[test]
    fn parses_scan_results_one_row_per_name() {
        let text = "bssid / frequency / signal level / flags / ssid\n\
            02:00:00:00:01:00\t2412\t-30\t[WPA2-PSK-CCMP][ESS]\thome\n\
            02:00:00:00:02:00\t5180\t-70\t[WPA2-PSK-CCMP][ESS]\thome\n\
            02:00:00:00:03:00\t2437\t-55\t[ESS]\tcaf\\xc3\\xa9\n\
            02:00:00:00:04:00\t2462\t-40\t[WPA2-PSK-CCMP][ESS]\t\n\
            02:00:00:00:05:00\t2462\t-41\t[WPA2-PSK-CCMP][ESS]\tbad\\x01name\n\
            garbage line\n";
        let list = parse_scan_results(text);
        assert_eq!(list.len(), 2, "{list:?}");
        assert_eq!((list[0].ssid.as_str(), list[0].signal_dbm, list[0].frequency_mhz), ("home", -30, 2412));
        assert_eq!((list[1].ssid.as_str(), list[1].security), ("caf\u{e9}", WifiSecurity::Open));
    }

    #[test]
    fn renders_hex_names_and_hides_nothing_it_should_not_keep() {
        let wpa2 = NetworkConfig {
            ssid: "a\"b".into(),
            security: WifiSecurity::Wpa2Personal,
            credential: credential(WifiSecurity::Wpa2Personal, "a\"b", Some("correct horse")),
            priority: 1,
        };
        let wpa3 = NetworkConfig {
            ssid: "w3".into(),
            security: WifiSecurity::Wpa3Personal,
            credential: credential(WifiSecurity::Wpa3Personal, "w3", Some("sae secret")),
            priority: 2,
        };
        let open = NetworkConfig {
            ssid: "cafe".into(),
            security: WifiSecurity::Open,
            credential: Credential::Open,
            priority: 3,
        };
        let text = render_config("/run/nodo/wpa", &[wpa2, wpa3, open]);
        assert!(text.contains("ctrl_interface=DIR=/run/nodo/wpa GROUP=root"));
        assert!(text.contains("\tssid=612262\n"), "{}", text.as_str());
        assert!(text.contains(&format!("\tpsk={}\n", derive_psk("correct horse", "a\"b").as_str())));
        assert!(!text.contains("correct horse"), "the WPA2 passphrase itself is never kept");
        assert!(text.contains("\tkey_mgmt=SAE\n\tieee80211w=2\n"));
        assert!(text.contains("\tpsk=\"sae secret\"\n"));
        assert!(text.contains("\tkey_mgmt=NONE\n\tpriority=3\n"));
        assert!(text.contains("\tpairwise=CCMP\n"));
        assert!(!text.contains("TKIP"));
        assert!(!text.contains("scan_ssid"));
        assert_eq!(format!("{:?}", credential(WifiSecurity::Wpa3Personal, "x", Some("hunter22"))), "Passphrase(<redacted>)");
    }

    #[test]
    fn classifies_join_events() {
        assert_eq!(join_event("<3>CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:01:00 completed [id=4 id_str=]", "4"), JoinEvent::Connected);
        assert_eq!(join_event("<3>CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:01:00 completed [id=1 id_str=]", "4"), JoinEvent::Other);
        assert_eq!(
            join_event("<3>CTRL-EVENT-SSID-TEMP-DISABLED id=4 ssid=\"home\" auth_failures=1 duration=10 reason=WRONG_KEY", "4"),
            JoinEvent::WrongKey
        );
        assert_eq!(
            join_event("<3>CTRL-EVENT-SSID-TEMP-DISABLED id=4 ssid=\"home\" auth_failures=2 duration=20 reason=CONN_FAILED", "4"),
            JoinEvent::Refused("CONN_FAILED".into())
        );
        assert_eq!(
            join_event("<3>CTRL-EVENT-SSID-TEMP-DISABLED id=1 ssid=\"other\" auth_failures=1 duration=10 reason=WRONG_KEY", "4"),
            JoinEvent::Other
        );
        assert_eq!(join_event("<3>CTRL-EVENT-NETWORK-NOT-FOUND", "4"), JoinEvent::NotFound);
        assert_eq!(join_event("<3>CTRL-EVENT-SCAN-RESULTS ", "4"), JoinEvent::Other);
    }

    #[test]
    fn reads_status() {
        let pairs = parse_pairs("bssid=02:00:00:00:01:00\nfreq=2412\nssid=home\nid=0\nmode=station\nkey_mgmt=WPA2-PSK\nwpa_state=COMPLETED\nip_address=192.168.77.50\n");
        assert_eq!(pairs.get("wpa_state").map(String::as_str), Some("COMPLETED"));
        assert_eq!(security_from_key_mgmt("WPA2-PSK"), WifiSecurity::Wpa2Personal);
        assert_eq!(security_from_key_mgmt("SAE"), WifiSecurity::Wpa3Personal);
        assert_eq!(security_from_key_mgmt("NONE"), WifiSecurity::Open);
    }
}
