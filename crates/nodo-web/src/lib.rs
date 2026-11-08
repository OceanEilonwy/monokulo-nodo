//! Web exposure for monokulo-nodo: proxy, Tor and firewall config generation, dynamic DNS, and the
//! connection checks behind the Web screens.
//!
//! Everything that talks to the outside goes through [`http::HttpClient`], so the library opens no
//! sockets, except [`router`], which needs UDP for NAT-PMP and SSDP. The `nodo-mdns` binary owns the
//! multicast DNS sockets. The `nodo-ddns` binary supplies
//! the only real HTTP client.

#![forbid(unsafe_code)]

pub mod acme;
pub mod caddy;
pub mod cert;
pub mod daemon;
pub mod detect;
pub mod dns;
pub mod http;
pub mod ip;
pub mod local;
pub mod mdns;
pub mod names;
pub mod net;
pub mod reachability;
pub mod router;
pub mod secret;
pub mod trust;
