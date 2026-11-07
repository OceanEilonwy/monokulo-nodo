//! Web exposure for monokulo-nodo: proxy, Tor and firewall config generation, dynamic DNS, and the
//! connection checks behind the Web screens.
//!
//! Everything that talks to the outside goes through [`http::HttpClient`], so the library itself
//! opens no sockets. The `nodo-ddns` binary supplies the only real client.

#![forbid(unsafe_code)]

pub mod caddy;
pub mod daemon;
pub mod detect;
pub mod dns;
pub mod http;
pub mod ip;
pub mod names;
pub mod reachability;
pub mod secret;
