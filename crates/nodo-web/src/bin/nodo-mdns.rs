//! Answers multicast DNS for local site names with the device's local address.
//!
//! Usage: `nodo-mdns --address <local IPv4> --names <file>` to respond, or
//! `nodo-mdns --query <name> [--via <IPv4>]` to look a name up, which helps diagnose a LAN.
//!
//! The names file has one `.local` name per line. Restart the service to load a new list.

use nodo_web::mdns::{build_query, parse_answers, parse_query, Reply, Responder, GROUP, PORT};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// RFC 6762 section 11: mDNS packets are sent with IP TTL 255, so receivers can tell they are link-local.
const MDNS_TTL: u32 = 255;

fn responder_socket(local: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Another responder on the device may already use port 5353.
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    socket.join_multicast_v4(&GROUP, &local)?;
    // Otherwise the socket would also receive the group on interfaces that other sockets joined.
    socket.set_multicast_all_v4(false)?;
    socket.set_multicast_if_v4(&local)?;
    socket.set_multicast_ttl_v4(MDNS_TTL)?;
    socket.set_ttl_v4(MDNS_TTL)?;
    Ok(socket.into())
}

fn respond(local: Ipv4Addr, names_file: PathBuf) -> Result<(), String> {
    let text = std::fs::read_to_string(&names_file).map_err(|e| format!("cannot read the names file: {e}"))?;
    let names: Vec<String> = text.lines().map(str::to_owned).collect();
    let responder = Responder::new(local, &names);
    if responder.names().is_empty() {
        return Err("no valid .local names to answer for".to_owned());
    }
    let socket = responder_socket(local).map_err(|e| format!("cannot open the mDNS socket: {e}"))?;
    let group = SocketAddr::V4(SocketAddrV4::new(GROUP, PORT));
    eprintln!("nodo-mdns: answering for {} names with {local}", responder.names().len());

    // RFC 6762 section 8.3: announce at least twice, one second apart.
    if let Some(announcement) = responder.announcement() {
        for _ in 0..2 {
            let _ = socket.send_to(&announcement, group);
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    let mut buffer = [0u8; 9000];
    loop {
        let Ok((len, from)) = socket.recv_from(&mut buffer) else { continue };
        let Some(query) = parse_query(&buffer[..len]) else { continue };
        let legacy = from.port() != PORT;
        let Some((answer, reply)) = responder.answer(&query, legacy) else { continue };
        let target = match reply {
            Reply::Multicast => group,
            Reply::Unicast | Reply::Legacy => from,
        };
        let _ = socket.send_to(&answer, target);
    }
}

fn query(name: &str, via: Option<Ipv4Addr>) -> Result<(), String> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(|e| e.to_string())?;
    if let Some(via) = via {
        socket.set_multicast_if_v4(&via).map_err(|e| e.to_string())?;
    }
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)).into()).map_err(|e| e.to_string())?;
    let socket: UdpSocket = socket.into();
    let id = std::process::id() as u16;
    socket
        .send_to(&build_query(id, &name.to_ascii_lowercase()), SocketAddrV4::new(GROUP, PORT))
        .map_err(|e| format!("cannot send the query: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut buffer = [0u8; 9000];
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        socket.set_read_timeout(Some(left.max(Duration::from_millis(1)))).map_err(|e| e.to_string())?;
        let Ok(len) = socket.recv(&mut buffer) else { break };
        if let Some((_, address)) = parse_answers(&buffer[..len]).into_iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
            println!("{address}");
            return Ok(());
        }
    }
    Err(format!("no answer for {}", name.escape_default()))
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (mut address, mut names, mut lookup, mut via) = (None, None, None, None);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or_else(|| format!("{} needs a value", arg.escape_default()))?;
        let ip = || value.parse::<Ipv4Addr>().map_err(|_| "not an IPv4 address".to_owned());
        match arg.as_str() {
            "--address" => address = Some(ip()?),
            "--via" => via = Some(ip()?),
            "--names" => names = Some(PathBuf::from(&value)),
            "--query" => lookup = Some(value),
            _ => return Err(format!("unknown argument: {}", arg.escape_default())),
        }
    }
    match (address, names, lookup) {
        (Some(address), Some(names), None) => respond(address, names),
        (None, None, Some(name)) => query(&name, via),
        _ => Err("usage: nodo-mdns --address <IPv4> --names <file> | --query <name> [--via <IPv4>]".to_owned()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("nodo-mdns: {message}");
            ExitCode::from(2)
        }
    }
}
