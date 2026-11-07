//! Address classification for CGNAT detection and for refusing to publish unusable addresses.

use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IpClass {
    /// Routable on the public internet.
    Global,
    /// 100.64.0.0/10, shared by an ISP's customers.
    Cgnat,
    /// RFC 1918 or an IPv6 unique local address.
    Private,
    Loopback,
    LinkLocal,
    /// Reserved for documentation, so it can only come from a broken or lying source.
    Documentation,
    /// Multicast, unspecified, benchmarking, or any other range that is not unicast internet space.
    Reserved,
}

pub fn classify(ip: IpAddr) -> IpClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

pub fn is_global(ip: IpAddr) -> bool {
    classify(ip) == IpClass::Global
}

fn classify_v4(ip: Ipv4Addr) -> IpClass {
    let [a, b, c, _] = ip.octets();
    match (a, b, c) {
        _ if ip.is_loopback() => IpClass::Loopback,
        _ if ip.is_private() => IpClass::Private,
        _ if ip.is_link_local() => IpClass::LinkLocal,
        (100, 64..=127, _) => IpClass::Cgnat,
        (192, 0, 2) | (198, 51, 100) | (203, 0, 113) => IpClass::Documentation,
        (0, _, _) | (192, 0, 0) | (198, 18..=19, _) => IpClass::Reserved,
        (224..=255, _, _) => IpClass::Reserved,
        _ => IpClass::Global,
    }
}

fn classify_v6(ip: Ipv6Addr) -> IpClass {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return classify_v4(mapped);
    }
    let first = ip.segments()[0];
    match first {
        _ if ip.is_loopback() => IpClass::Loopback,
        _ if ip.is_unspecified() || ip.is_multicast() => IpClass::Reserved,
        0xfc00..=0xfdff => IpClass::Private,
        0xfe80..=0xfebf => IpClass::LinkLocal,
        0x2001 if ip.segments()[1] == 0x0db8 => IpClass::Documentation,
        // Only 2000::/3 is allocated as global unicast.
        0x2000..=0x3fff => IpClass::Global,
        _ => IpClass::Reserved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(text: &str) -> IpClass {
        classify(text.parse().unwrap())
    }

    #[test]
    fn cgnat_range_has_exact_bounds() {
        assert_eq!(class("100.63.255.255"), IpClass::Global);
        assert_eq!(class("100.64.0.0"), IpClass::Cgnat);
        assert_eq!(class("100.72.14.9"), IpClass::Cgnat);
        assert_eq!(class("100.127.255.255"), IpClass::Cgnat);
        assert_eq!(class("100.128.0.0"), IpClass::Global);
    }

    #[test]
    fn private_and_special_ranges() {
        for private in ["10.0.0.1", "172.16.0.1", "172.31.255.255", "192.168.1.1", "fd00::1"] {
            assert_eq!(class(private), IpClass::Private, "{private}");
        }
        assert_eq!(class("172.32.0.1"), IpClass::Global);
        assert_eq!(class("127.0.0.1"), IpClass::Loopback);
        assert_eq!(class("::1"), IpClass::Loopback);
        assert_eq!(class("169.254.1.1"), IpClass::LinkLocal);
        assert_eq!(class("fe80::1"), IpClass::LinkLocal);
        assert_eq!(class("203.0.113.42"), IpClass::Documentation);
        assert_eq!(class("2001:db8::1"), IpClass::Documentation);
        for reserved in ["0.0.0.0", "224.0.0.1", "255.255.255.255", "198.18.0.1", "::", "ff02::1"] {
            assert_eq!(class(reserved), IpClass::Reserved, "{reserved}");
        }
    }

    #[test]
    fn public_addresses_are_global() {
        assert!(is_global("93.184.216.34".parse().unwrap()));
        assert!(is_global("2606:2800:220:1::1".parse().unwrap()));
    }

    #[test]
    fn mapped_v6_is_judged_as_v4() {
        assert_eq!(class("::ffff:192.168.1.1"), IpClass::Private);
        assert_eq!(class("::ffff:100.64.0.1"), IpClass::Cgnat);
    }
}
