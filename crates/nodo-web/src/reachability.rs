//! The connection checks behind the Reachability check and CGNAT screens.
//!
//! Two inputs can't be produced on this device alone, so they are traits:
//!
//! - [`PortProbe`]: whether a port answers from outside can only be seen from outside. No such vantage
//!   point exists yet, so [`NoExternalVantage`] answers "not checked", and the report says the ports are
//!   unverified rather than claiming they work.
//! - [`RouterWan`]: the router's WAN address has to come from the router. Nothing implements this yet
//!   either. UPnP is off by default in the Web flow, so the address may simply be unknown.

use crate::detect::{detect, Family, IpSource};
use crate::dns::DnsProvider;
use crate::http::HttpClient;
use crate::ip::{classify, IpClass};
use serde::Serialize;
use std::net::IpAddr;

const REQUIRED_PORTS: [u16; 2] = [80, 443];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortStatus {
    Reachable,
    TimedOut,
    Refused,
    /// Nobody outside was able to test it.
    NotChecked,
}

pub trait PortProbe {
    fn probe(&self, public_ip: IpAddr, port: u16) -> PortStatus;
}

/// The honest answer until an external prober exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoExternalVantage;

impl PortProbe for NoExternalVantage {
    fn probe(&self, _public_ip: IpAddr, _port: u16) -> PortStatus {
        PortStatus::NotChecked
    }
}

pub trait RouterWan {
    /// `None` when the router cannot be asked or does not say.
    fn wan_address(&self) -> Option<IpAddr>;
}

/// For a device that has no way to ask its router.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnknownRouter;

impl RouterWan for UnknownRouter {
    fn wan_address(&self) -> Option<IpAddr> {
        None
    }
}

/// What the addresses say about the connection. `Unknown` never counts as a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ConnectionAssessment {
    /// The router holds the address the internet sees, so forwarding can work.
    PublicAddress { router_wan: IpAddr, observed: IpAddr },
    /// The ISP shares one public address. The Web flow's CGNAT screen shows this evidence.
    Cgnat { router_wan: IpAddr, observed: Option<IpAddr> },
    /// The router is behind another router, such as an ISP modem. Forwarding would be needed on both.
    PrivateWan { router_wan: IpAddr, observed: Option<IpAddr> },
    /// The router has a public address, but the internet sees a different one, so something else is
    /// translating in between.
    UpstreamNat { router_wan: IpAddr, observed: IpAddr },
    /// The router could not be asked. Port forwarding may or may not be possible.
    Unknown { observed: Option<IpAddr> },
}

impl ConnectionAssessment {
    /// True when port forwarding on the user's router cannot make the site public.
    pub fn blocks_public(&self) -> bool {
        matches!(self, Self::Cgnat { .. } | Self::UpstreamNat { .. })
    }
}

pub fn assess_connection(router_wan: Option<IpAddr>, observed: Option<IpAddr>) -> ConnectionAssessment {
    let Some(router_wan) = router_wan else {
        return ConnectionAssessment::Unknown { observed };
    };
    match classify(router_wan) {
        IpClass::Cgnat => ConnectionAssessment::Cgnat { router_wan, observed },
        IpClass::Private | IpClass::LinkLocal | IpClass::Loopback => {
            ConnectionAssessment::PrivateWan { router_wan, observed }
        }
        _ => match observed {
            Some(seen) if seen == router_wan => ConnectionAssessment::PublicAddress { router_wan, observed: seen },
            Some(seen) => ConnectionAssessment::UpstreamNat { router_wan, observed: seen },
            None => ConnectionAssessment::Unknown { observed: None },
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckId {
    DomainAndCredentials,
    PublicIp,
    RouterAddress,
    TemporaryRecord,
    Port80,
    Port443,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    /// Could not be determined. Shown as a warning, never as a pass.
    Unknown,
    /// Not run, because an earlier result made it pointless.
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub id: CheckId,
    pub status: CheckStatus,
    pub detail: String,
}

/// The options on the CGNAT screen, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CgnatRemedy {
    TurnOnTorOnion,
    AskIspForPublicIp,
    UseIpv6,
    ConnectRelayTunnel,
}

pub const CGNAT_REMEDIES: [CgnatRemedy; 4] = [
    CgnatRemedy::TurnOnTorOnion,
    CgnatRemedy::AskIspForPublicIp,
    CgnatRemedy::UseIpv6,
    CgnatRemedy::ConnectRelayTunnel,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortForward {
    pub external_port: u16,
    pub device_ip: IpAddr,
    pub device_port: u16,
}

/// Facts about the device's own network, for the port forwarding instructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanInfo {
    pub device_ip: IpAddr,
    pub gateway: IpAddr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Verdict {
    /// Everything passed, so the Confirm screen can be shown.
    Ready,
    /// A forward is missing or wrong. `router_admin_url` is absent when the gateway is unknown.
    NeedsPortForward { router_admin_url: Option<String>, forwards: Vec<PortForward> },
    /// The checks that could run passed, but nobody outside tested the ports. The UI must say so.
    PortsUnverified,
    /// Forwarding on this connection cannot work.
    BlockedByCgnat { assessment: ConnectionAssessment, remedies: Vec<CgnatRemedy> },
    /// A provider or detection failure. See the checks.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReachabilityReport {
    pub checks: Vec<Check>,
    pub public_ip: Option<IpAddr>,
    pub connection: ConnectionAssessment,
    pub warnings: Vec<String>,
    pub verdict: Verdict,
}

pub struct ReachabilityInputs<'a> {
    pub provider: &'a dyn DnsProvider,
    pub http: &'a dyn HttpClient,
    pub sources: &'a [IpSource],
    pub router: &'a dyn RouterWan,
    pub ports: &'a dyn PortProbe,
    pub lan: Option<&'a LanInfo>,
}

/// Runs the checks in the order the Reachability screen lists them. It changes no state beyond the
/// temporary DNS record, so the site's other addresses keep working throughout.
pub fn run_check(inputs: &ReachabilityInputs<'_>) -> ReachabilityReport {
    let mut checks = Vec::new();
    let mut warnings = Vec::new();
    let check = |id, status, detail: String| Check { id, status, detail };

    let credentials = inputs.provider.read(&crate::dns::RecordName::apex(), crate::dns::RecordType::A);
    let credentials_ok = credentials.is_ok();
    checks.push(match credentials {
        Ok(_) => check(CheckId::DomainAndCredentials, CheckStatus::Passed, String::new()),
        Err(error) => check(CheckId::DomainAndCredentials, CheckStatus::Failed, error.to_string()),
    });

    let detection = detect(inputs.http, inputs.sources, Family::V4);
    let public_ip = detection.agreed_ip();
    checks.push(match public_ip {
        Some(ip) => check(CheckId::PublicIp, CheckStatus::Passed, ip.to_string()),
        None => check(CheckId::PublicIp, CheckStatus::Failed, "the IP services did not agree".to_owned()),
    });

    let connection = assess_connection(inputs.router.wan_address(), public_ip);
    checks.push(match &connection {
        ConnectionAssessment::PublicAddress { .. } => {
            check(CheckId::RouterAddress, CheckStatus::Passed, String::new())
        }
        ConnectionAssessment::Unknown { .. } => {
            check(CheckId::RouterAddress, CheckStatus::Unknown, "the router could not be asked".to_owned())
        }
        ConnectionAssessment::Cgnat { router_wan, .. }
        | ConnectionAssessment::PrivateWan { router_wan, .. }
        | ConnectionAssessment::UpstreamNat { router_wan, .. } => {
            check(CheckId::RouterAddress, CheckStatus::Failed, router_wan.to_string())
        }
    });

    let blocked = connection.blocks_public();
    if credentials_ok && !blocked {
        checks.push(match inputs.provider.test() {
            Ok(report) => {
                warnings.extend(report.warnings);
                check(CheckId::TemporaryRecord, CheckStatus::Passed, String::new())
            }
            Err(error) => check(CheckId::TemporaryRecord, CheckStatus::Failed, error.to_string()),
        });
    } else {
        checks.push(check(CheckId::TemporaryRecord, CheckStatus::Skipped, String::new()));
    }

    let mut port_results = Vec::new();
    for (id, port) in [CheckId::Port80, CheckId::Port443].into_iter().zip(REQUIRED_PORTS) {
        let probed = public_ip.filter(|_| !blocked).map(|ip| inputs.ports.probe(ip, port));
        port_results.push(probed);
        checks.push(match probed {
            None => check(id, CheckStatus::Skipped, String::new()),
            Some(PortStatus::Reachable) => check(id, CheckStatus::Passed, String::new()),
            Some(PortStatus::NotChecked) => check(id, CheckStatus::Unknown, "no outside vantage point".to_owned()),
            Some(PortStatus::TimedOut) => check(id, CheckStatus::Failed, "timed out".to_owned()),
            Some(PortStatus::Refused) => check(id, CheckStatus::Failed, "refused".to_owned()),
        });
    }

    let verdict = decide(&checks, &connection, &port_results, inputs.lan);
    ReachabilityReport { checks, public_ip, connection, warnings, verdict }
}

fn decide(
    checks: &[Check],
    connection: &ConnectionAssessment,
    ports: &[Option<PortStatus>],
    lan: Option<&LanInfo>,
) -> Verdict {
    if connection.blocks_public() {
        return Verdict::BlockedByCgnat { assessment: connection.clone(), remedies: CGNAT_REMEDIES.to_vec() };
    }
    let failed = |id: CheckId| checks.iter().any(|c| c.id == id && c.status == CheckStatus::Failed);
    if failed(CheckId::DomainAndCredentials) || failed(CheckId::PublicIp) || failed(CheckId::TemporaryRecord) {
        return Verdict::Failed;
    }
    if ports.iter().flatten().any(|p| matches!(p, PortStatus::TimedOut | PortStatus::Refused)) {
        return Verdict::NeedsPortForward {
            router_admin_url: lan.map(|l| format!("http://{}", l.gateway)),
            forwards: lan
                .map(|l| {
                    REQUIRED_PORTS
                        .iter()
                        .map(|port| PortForward { external_port: *port, device_ip: l.device_ip, device_port: *port })
                        .collect()
                })
                .unwrap_or_default(),
        };
    }
    if ports.iter().flatten().all(|p| *p == PortStatus::Reachable) && !ports.iter().any(Option::is_none) {
        Verdict::Ready
    } else {
        Verdict::PortsUnverified
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::default_sources;
    use crate::dns::{Change, DnsError, RecordName, RecordType, TestReport, UpsertOutcome};
    use crate::http::fake::FakeHttp;

    const PUBLIC: &str = "93.184.216.34";

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    struct Provider {
        read_fails: bool,
        test_fails: bool,
    }

    impl DnsProvider for Provider {
        fn min_ttl(&self) -> u32 {
            300
        }
        fn read(&self, _: &RecordName, _: RecordType) -> Result<Vec<IpAddr>, DnsError> {
            if self.read_fails {
                Err(DnsError::Unauthorized("no".to_owned()))
            } else {
                Ok(Vec::new())
            }
        }
        fn upsert(&self, _: &RecordName, _: IpAddr, _: u32) -> Result<UpsertOutcome, DnsError> {
            Ok(UpsertOutcome { change: Change::Created, warnings: Vec::new() })
        }
        fn delete(&self, _: &RecordName, _: RecordType) -> Result<(), DnsError> {
            Ok(())
        }
        fn test(&self) -> Result<TestReport, DnsError> {
            if self.test_fails {
                Err(DnsError::Server { status: 500 })
            } else {
                Ok(TestReport { record: "t".to_owned(), warnings: vec!["delegated".to_owned()] })
            }
        }
    }

    const FINE: Provider = Provider { read_fails: false, test_fails: false };

    struct Router(Option<&'static str>);
    impl RouterWan for Router {
        fn wan_address(&self) -> Option<IpAddr> {
            self.0.map(ip)
        }
    }

    struct Ports(PortStatus);
    impl PortProbe for Ports {
        fn probe(&self, _: IpAddr, _: u16) -> PortStatus {
            self.0
        }
    }

    fn agreeing_http() -> std::sync::Arc<FakeHttp> {
        let http = FakeHttp::new();
        for _ in default_sources().iter().filter(|s| s.family == Family::V4) {
            http.respond(200, PUBLIC);
        }
        http
    }

    fn status(report: &ReachabilityReport, id: CheckId) -> CheckStatus {
        report.checks.iter().find(|c| c.id == id).unwrap().status
    }

    fn run(provider: &Provider, router: &dyn RouterWan, ports: &dyn PortProbe, lan: Option<&LanInfo>) -> ReachabilityReport {
        let http = agreeing_http();
        let sources = default_sources();
        run_check(&ReachabilityInputs { provider, http: http.as_ref(), sources: &sources, router, ports, lan })
    }

    #[test]
    fn cgnat_is_detected_from_the_router_wan_address() {
        let assessment = assess_connection(Some(ip("100.72.14.9")), Some(ip(PUBLIC)));
        assert_eq!(assessment, ConnectionAssessment::Cgnat { router_wan: ip("100.72.14.9"), observed: Some(ip(PUBLIC)) });
        assert!(assessment.blocks_public());
    }

    #[test]
    fn range_edges_are_not_cgnat() {
        assert!(matches!(
            assess_connection(Some(ip("100.128.0.1")), Some(ip("100.128.0.1"))),
            ConnectionAssessment::PublicAddress { .. }
        ));
        assert!(matches!(assess_connection(Some(ip("100.63.0.1")), None), ConnectionAssessment::Unknown { .. }));
    }

    #[test]
    fn private_wan_is_double_nat() {
        let assessment = assess_connection(Some(ip("192.168.100.2")), Some(ip(PUBLIC)));
        assert!(matches!(assessment, ConnectionAssessment::PrivateWan { .. }));
        assert!(!assessment.blocks_public(), "forwarding on both routers can still work");
    }

    #[test]
    fn mismatch_with_a_public_wan_is_upstream_nat() {
        let assessment = assess_connection(Some(ip("93.184.216.1")), Some(ip(PUBLIC)));
        assert!(matches!(assessment, ConnectionAssessment::UpstreamNat { .. }));
        assert!(assessment.blocks_public());
    }

    #[test]
    fn an_unknown_router_is_never_a_pass() {
        assert_eq!(assess_connection(None, Some(ip(PUBLIC))), ConnectionAssessment::Unknown { observed: Some(ip(PUBLIC)) });
    }

    #[test]
    fn everything_passing_is_ready() {
        let report = run(&FINE, &Router(Some(PUBLIC)), &Ports(PortStatus::Reachable), None);
        assert_eq!(report.verdict, Verdict::Ready);
        assert_eq!(report.public_ip, Some(ip(PUBLIC)));
        assert_eq!(report.warnings, vec!["delegated"]);
        assert_eq!(status(&report, CheckId::Port443), CheckStatus::Passed);
    }

    #[test]
    fn without_a_vantage_point_ports_are_unverified_not_passed() {
        let report = run(&FINE, &UnknownRouter, &NoExternalVantage, None);
        assert_eq!(report.verdict, Verdict::PortsUnverified);
        assert_eq!(status(&report, CheckId::Port80), CheckStatus::Unknown);
        assert_eq!(status(&report, CheckId::RouterAddress), CheckStatus::Unknown);
    }

    #[test]
    fn closed_ports_give_forwarding_instructions() {
        let lan = LanInfo { device_ip: ip("192.168.1.50"), gateway: ip("192.168.1.1") };
        let report = run(&FINE, &Router(Some(PUBLIC)), &Ports(PortStatus::TimedOut), Some(&lan));
        let Verdict::NeedsPortForward { router_admin_url, forwards } = &report.verdict else {
            panic!("{:?}", report.verdict);
        };
        assert_eq!(router_admin_url.as_deref(), Some("http://192.168.1.1"));
        assert_eq!(forwards[1], PortForward { external_port: 443, device_ip: ip("192.168.1.50"), device_port: 443 });
    }

    #[test]
    fn cgnat_stops_the_temporary_record_and_lists_remedies_starting_with_onion() {
        let report = run(&FINE, &Router(Some("100.72.14.9")), &Ports(PortStatus::Reachable), None);
        let Verdict::BlockedByCgnat { assessment, remedies } = &report.verdict else {
            panic!("{:?}", report.verdict);
        };
        assert!(matches!(assessment, ConnectionAssessment::Cgnat { .. }));
        assert_eq!(remedies[0], CgnatRemedy::TurnOnTorOnion);
        assert_eq!(status(&report, CheckId::TemporaryRecord), CheckStatus::Skipped);
        assert_eq!(status(&report, CheckId::Port443), CheckStatus::Skipped);
    }

    #[test]
    fn provider_failures_fail_the_check() {
        let report = run(&Provider { read_fails: true, test_fails: false }, &Router(Some(PUBLIC)), &Ports(PortStatus::Reachable), None);
        assert_eq!(report.verdict, Verdict::Failed);
        assert_eq!(status(&report, CheckId::TemporaryRecord), CheckStatus::Skipped);

        let report = run(&Provider { read_fails: false, test_fails: true }, &Router(Some(PUBLIC)), &Ports(PortStatus::Reachable), None);
        assert_eq!(report.verdict, Verdict::Failed);
    }

    #[test]
    fn reports_serialize_for_the_screens() {
        let report = run(&FINE, &Router(Some("100.72.14.9")), &NoExternalVantage, None);
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["verdict"]["kind"], "blocked_by_cgnat");
        assert_eq!(value["verdict"]["assessment"]["router_wan"], "100.72.14.9");
    }
}
