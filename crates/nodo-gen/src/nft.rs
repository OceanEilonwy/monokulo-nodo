use std::collections::BTreeSet;
use std::fmt::Write;
use std::net::IpAddr;

use nodo_manifest::{Protocol, Scope};

use crate::spec::ServiceSpec;

const PRIVATE_V4: &str = "{ 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16 }";
const PRIVATE_V6: &str = "{ fc00::/7, fe80::/10 }";

fn proto(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Tcp => "tcp",
        Protocol::Udp => "udp",
    }
}

fn port_set(ports: &BTreeSet<u16>) -> String {
    let list: Vec<String> = ports.iter().map(u16::to_string).collect();
    match list.as_slice() {
        [one] => one.clone(),
        many => format!("{{ {} }}", many.join(", ")),
    }
}

/// The fragment for `/etc/nftables.d`. Every value here is a validated user name or a typed
/// number or address, so no rule text comes straight from a manifest.
pub fn nft_fragment(spec: &ServiceSpec) -> String {
    let mut input = Vec::new();
    for (port, protocol, scope) in &spec.inbound {
        let p = proto(*protocol);
        match scope {
            // The base ruleset already accepts everything arriving on loopback.
            Scope::Local => {}
            Scope::Internet => input.push(format!("{p} dport {port} accept")),
            Scope::Lan => {
                input.push(format!("ip saddr {PRIVATE_V4} {p} dport {port} accept"));
                input.push(format!("ip6 saddr {PRIVATE_V6} {p} dport {port} accept"));
            }
        }
    }

    let who = format!("meta skuid \"{}\"", spec.user);
    let mut output = Vec::new();
    // Replies pass through the service's own output rules, so a listener without this rule could
    // answer nobody past the final drop, and no LAN client past the private range block.
    if spec.has_listener {
        output.push(format!("{who} ct state established,related accept"));
    }
    if spec.unrestricted_loopback {
        output.push(format!("{who} oifname \"lo\" accept"));
    } else {
        if !spec.loopback_tcp.is_empty() {
            let ports = port_set(&spec.loopback_tcp);
            output.push(format!("{who} oifname \"lo\" tcp dport {ports} accept"));
        }
        if !spec.loopback_udp.is_empty() {
            let ports = port_set(&spec.loopback_udp);
            output.push(format!("{who} oifname \"lo\" udp dport {ports} accept"));
        }
    }
    for grant in &spec.lan {
        let family = match grant.address {
            IpAddr::V4(_) => "ip",
            IpAddr::V6(_) => "ip6",
        };
        output.push(format!(
            "{who} {family} daddr {}/{} {} dport {} accept",
            grant.address,
            grant.prefix,
            proto(grant.protocol),
            grant.port,
        ));
    }
    if spec.internet {
        output.push(format!("{who} ip daddr {PRIVATE_V4} drop"));
        // `meta l4proto tcp accept` below also matches IPv6, so the block must cover it too.
        output.push(format!("{who} ip6 daddr {PRIVATE_V6} drop"));
        output.push(format!("{who} meta l4proto tcp accept"));
    }
    output.push(format!("{who} drop"));

    let mut out = String::from(
        "# Loaded after /etc/nftables.conf, which sets the default-deny input policy.\n\
         table inet monokulo {\n",
    );
    if !input.is_empty() {
        out.push_str("\tchain input {\n");
        for rule in &input {
            let _ = writeln!(out, "\t\t{rule}");
        }
        out.push_str("\t}\n\n");
    }
    out.push_str("\tchain output {\n");
    for rule in &output {
        let _ = writeln!(out, "\t\t{rule}");
    }
    out.push_str("\t}\n}\n");
    out
}
