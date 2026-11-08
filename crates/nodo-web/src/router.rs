//! Reads the router's WAN address, for CGNAT detection. Read-only by construction: the only NAT-PMP
//! request built here is opcode 0 (external address) and the only UPnP action is
//! `GetExternalIPAddress`. Nothing here can open, change or delete a port mapping.
//!
//! Both protocols are unauthenticated and any LAN host can answer, so replies are only accepted from
//! the default gateway, and a UPnP description must be served by the gateway itself. A wrong answer can
//! still only change which warning the screens show, never what is published.
//!
//! This module and the `nodo-ddns` binary are the only places that open sockets. The HTTP side of UPnP
//! goes through the injected [`HttpClient`].

use crate::http::{HttpClient, HttpRequest, Method};
use crate::reachability::RouterWan;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

pub const NATPMP_PORT: u16 = 5351;
pub const SSDP_ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
const IGD_TYPE: &str = "urn:schemas-upnp-org:device:InternetGatewayDevice:1";
const WAN_SERVICES: [&str; 3] = [
    "urn:schemas-upnp-org:service:WANIPConnection:2",
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
];
const MAX_DESCRIPTION_BYTES: usize = 256 * 1024;

/// Reads the IPv4 default gateway from the text of `/proc/net/route`.
pub fn default_gateway(route_table: &str) -> Option<Ipv4Addr> {
    route_table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (destination, gateway, flags) = (fields.get(1)?, fields.get(2)?, fields.get(3)?);
        let flags = u16::from_str_radix(flags, 16).ok()?;
        // RTF_UP and RTF_GATEWAY.
        if *destination != "00000000" || flags & 0x3 != 0x3 {
            return None;
        }
        // The kernel prints the address in host byte order, which is little-endian on every supported board.
        let raw = u32::from_str_radix(gateway, 16).ok()?;
        Some(Ipv4Addr::from(raw.swap_bytes()))
    })
}

pub fn system_gateway() -> Option<Ipv4Addr> {
    default_gateway(&std::fs::read_to_string("/proc/net/route").ok()?)
}

/// RFC 6886 section 3.2: version 0, opcode 0.
pub const NATPMP_EXTERNAL_ADDRESS_REQUEST: [u8; 2] = [0, 0];

/// Parses a NAT-PMP external address response. `None` for anything malformed or unsuccessful.
pub fn parse_natpmp(response: &[u8]) -> Option<Ipv4Addr> {
    let result_code = u16::from_be_bytes([*response.get(2)?, *response.get(3)?]);
    let ok = response.len() >= 12 && response[0] == 0 && response[1] == 128 && result_code == 0;
    ok.then(|| Ipv4Addr::new(response[8], response[9], response[10], response[11]))
}

/// Asks the gateway over NAT-PMP, retrying as RFC 6886 suggests but for a shorter total time.
pub fn natpmp_external_address(gateway: SocketAddrV4) -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // A connected UDP socket only receives datagrams from that address.
    socket.connect(gateway).ok()?;
    let mut buffer = [0u8; 16];
    for wait_ms in [250, 500, 1000] {
        socket.send(&NATPMP_EXTERNAL_ADDRESS_REQUEST).ok()?;
        socket.set_read_timeout(Some(Duration::from_millis(wait_ms))).ok()?;
        if let Ok(len) = socket.recv(&mut buffer) {
            return parse_natpmp(&buffer[..len]);
        }
    }
    None
}

pub fn ssdp_search() -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {IGD_TYPE}\r\n\r\n"
    )
}

/// The `LOCATION` header of an SSDP reply, if it is a plain HTTP URL on the gateway itself.
pub fn ssdp_location(reply: &str, gateway: Ipv4Addr) -> Option<String> {
    let location = reply.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("location").then(|| value.trim().to_owned())
    })?;
    (url_host(&location)? == gateway.to_string()).then_some(location)
}

/// Host of an `http://host[:port]/path` URL. Other schemes and user info are refused.
fn url_host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("http://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    Some(authority.rsplit_once(':').map_or(authority, |(host, _)| host))
}

/// Finds the control URL of the first WAN connection service in a device description, resolved
/// against the description's own URL. It must stay on the same host.
pub fn control_url(description: &str, location: &str) -> Option<(String, &'static str)> {
    let base = &location[..location.find("://").map(|i| i + 3)?];
    let authority = location[base.len()..].split('/').next()?;
    let origin = format!("{base}{authority}");
    for service_type in WAN_SERVICES {
        for block in description.split("<service>").skip(1) {
            let block = block.split("</service>").next().unwrap_or(block);
            if tag(block, "serviceType").as_deref() != Some(service_type) {
                continue;
            }
            let path = tag(block, "controlURL")?;
            let url = if path.starts_with("http://") {
                path
            } else if path.starts_with('/') {
                format!("{origin}{path}")
            } else {
                format!("{origin}/{path}")
            };
            return (url_host(&url) == url_host(location)).then_some((url, service_type));
        }
    }
    None
}

fn tag(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{name}>"))? + start;
    Some(text[start..end].trim().to_owned())
}

pub fn soap_request(control: &str, service_type: &str) -> HttpRequest {
    let body = format!(
        "<?xml version=\"1.0\"?>\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:GetExternalIPAddress xmlns:u=\"{service_type}\"/></s:Body></s:Envelope>\r\n"
    );
    let mut request = HttpRequest::new(Method::Post, control)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("SOAPAction", format!("\"{service_type}#GetExternalIPAddress\""));
    request.body = Some(body.into_bytes());
    request
}

pub fn parse_soap_address(body: &str) -> Option<IpAddr> {
    tag(body, "NewExternalIPAddress")?.parse().ok()
}

/// UPnP over the injected HTTP client. `search_target` is the SSDP multicast address in production.
pub fn upnp_external_address(http: &dyn HttpClient, gateway: Ipv4Addr, search_target: SocketAddr) -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.send_to(ssdp_search().as_bytes(), search_target).ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut buffer = [0u8; 2048];
    let location = loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        socket.set_read_timeout(Some(left.max(Duration::from_millis(1)))).ok()?;
        let (len, from) = socket.recv_from(&mut buffer).ok()?;
        if from.ip() != IpAddr::V4(gateway) && from != search_target {
            continue;
        }
        if let Some(location) = ssdp_location(&String::from_utf8_lossy(&buffer[..len]), gateway) {
            break location;
        }
    };
    let response = http.send(&HttpRequest::new(Method::Get, location.clone())).ok()?;
    if !response.is_success() || response.body.len() > MAX_DESCRIPTION_BYTES {
        return None;
    }
    let (control, service_type) = control_url(std::str::from_utf8(&response.body).ok()?, &location)?;
    let reply = http.send(&soap_request(&control, service_type)).ok()?;
    if !reply.is_success() || reply.body.len() > MAX_DESCRIPTION_BYTES {
        return None;
    }
    parse_soap_address(std::str::from_utf8(&reply.body).ok()?)
}

/// Asks the gateway with NAT-PMP, then UPnP. Takes up to about seven seconds when neither answers.
pub struct GatewayQuery<'a> {
    pub gateway: Ipv4Addr,
    pub http: &'a dyn HttpClient,
}

impl RouterWan for GatewayQuery<'_> {
    fn wan_address(&self) -> Option<IpAddr> {
        natpmp_external_address(SocketAddrV4::new(self.gateway, NATPMP_PORT))
            .map(IpAddr::V4)
            .or_else(|| upnp_external_address(self.http, self.gateway, SocketAddr::V4(SSDP_ADDR)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::FakeHttp;
    use crate::http::HttpResponse;

    const ROUTES: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n\
eth0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n";

    #[test]
    fn finds_the_default_gateway() {
        assert_eq!(default_gateway(ROUTES), Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(default_gateway("Iface\tDestination\n"), None);
        let down = ROUTES.replace("\t0003\t", "\t0002\t");
        assert_eq!(default_gateway(&down), None, "a route that is not up is ignored");
    }

    #[test]
    fn natpmp_parsing_requires_a_successful_reply() {
        let mut reply = [0u8, 128, 0, 0, 0, 0, 0, 9, 100, 72, 14, 9];
        assert_eq!(parse_natpmp(&reply), Some(Ipv4Addr::new(100, 72, 14, 9)));
        reply[3] = 3;
        assert_eq!(parse_natpmp(&reply), None, "non-zero result code");
        assert_eq!(parse_natpmp(&[0, 128, 0]), None);
        assert_eq!(parse_natpmp(&[0, 129, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4]), None, "mapping reply, not address");
    }

    #[test]
    fn natpmp_query_over_a_real_socket() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = match server.local_addr().unwrap() {
            SocketAddr::V4(v4) => v4,
            SocketAddr::V6(_) => unreachable!(),
        };
        let handle = std::thread::spawn(move || {
            let mut buffer = [0u8; 16];
            let (len, from) = server.recv_from(&mut buffer).unwrap();
            server.send_to(&[0, 128, 0, 0, 0, 0, 0, 1, 203, 0, 113, 42], from).unwrap();
            buffer[..len].to_vec()
        });
        assert_eq!(natpmp_external_address(address), Some(Ipv4Addr::new(203, 0, 113, 42)));
        assert_eq!(handle.join().unwrap(), NATPMP_EXTERNAL_ADDRESS_REQUEST, "only the read-only request is sent");
    }

    #[test]
    fn natpmp_gives_up_when_nothing_answers() {
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let SocketAddr::V4(address) = silent.local_addr().unwrap() else { unreachable!() };
        let started = Instant::now();
        assert_eq!(natpmp_external_address(address), None);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    const GW: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 1);

    #[test]
    fn ssdp_locations_must_be_http_on_the_gateway() {
        let reply = |location: &str| format!("HTTP/1.1 200 OK\r\nST: {IGD_TYPE}\r\nLocation: {location}\r\n\r\n");
        assert_eq!(
            ssdp_location(&reply("http://192.168.1.1:5000/rootDesc.xml"), GW).as_deref(),
            Some("http://192.168.1.1:5000/rootDesc.xml")
        );
        for bad in ["http://192.168.1.66/desc.xml", "https://192.168.1.1/desc.xml", "http://x@192.168.1.1/", "file:///etc"] {
            assert_eq!(ssdp_location(&reply(bad), GW), None, "{bad}");
        }
    }

    const DESCRIPTION: &str = "<root><device><serviceList>\
<service><serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1</serviceType><controlURL>/l3f</controlURL></service>\
<service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>\
<controlURL>/ctl/IPConn</controlURL></service></serviceList></device></root>";

    #[test]
    fn control_urls_resolve_on_the_same_host() {
        let location = "http://192.168.1.1:5000/rootDesc.xml";
        assert_eq!(
            control_url(DESCRIPTION, location),
            Some(("http://192.168.1.1:5000/ctl/IPConn".to_owned(), "urn:schemas-upnp-org:service:WANIPConnection:1"))
        );
        let elsewhere = DESCRIPTION.replace("/ctl/IPConn", "http://evil.test/ctl");
        assert_eq!(control_url(&elsewhere, location), None);
    }

    #[test]
    fn the_soap_request_only_reads_the_address() {
        let request = soap_request("http://192.168.1.1:5000/ctl/IPConn", WAN_SERVICES[1]);
        let body = String::from_utf8(request.body.clone().unwrap()).unwrap();
        assert!(body.contains("GetExternalIPAddress"));
        assert!(!body.contains("AddPortMapping") && !body.contains("DeletePortMapping"));
        assert_eq!(
            crate::http::fake::header(&request, "SOAPAction"),
            Some("\"urn:schemas-upnp-org:service:WANIPConnection:1#GetExternalIPAddress\"")
        );
        let reply = "<s:Envelope><s:Body><u:GetExternalIPAddressResponse>\
<NewExternalIPAddress>100.72.14.9</NewExternalIPAddress></u:GetExternalIPAddressResponse></s:Body></s:Envelope>";
        assert_eq!(parse_soap_address(reply), Some("100.72.14.9".parse().unwrap()));
        assert_eq!(parse_soap_address("<NewExternalIPAddress></NewExternalIPAddress>"), None);
    }

    #[test]
    fn upnp_flow_over_a_real_ssdp_socket() {
        let responder = UdpSocket::bind("127.0.0.1:0").unwrap();
        let target = responder.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 1024];
            let (_, from) = responder.recv_from(&mut buffer).unwrap();
            let reply = format!("HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.1:5000/rootDesc.xml\r\nST: {IGD_TYPE}\r\n\r\n");
            responder.send_to(reply.as_bytes(), from).unwrap();
        });
        let http = FakeHttp::new();
        http.respond(200, DESCRIPTION);
        http.respond_with(HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: b"<NewExternalIPAddress>100.72.14.9</NewExternalIPAddress>".to_vec(),
        });
        assert_eq!(upnp_external_address(http.as_ref(), GW, target), Some("100.72.14.9".parse().unwrap()));
        assert_eq!(http.request(0).url, "http://192.168.1.1:5000/rootDesc.xml");
        assert_eq!(http.request(1).url, "http://192.168.1.1:5000/ctl/IPConn");
        http.assert_drained();
    }
}
