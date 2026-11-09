//! The wire format: a 4-byte big-endian length, then a version byte and a postcard body.
//!
//! Postcard identifies enum variants by position, so the enums in this crate may only grow at the
//! end. Any other change requires a new [`PROTOCOL_VERSION`].

use std::io::{self, Read, Write};

pub use crate::PROTOCOL_VERSION;
use crate::{ApiError, Request, Response};
use serde::de::DeserializeOwned;
use thiserror::Error;
use zeroize::Zeroizing;

pub const MAX_REQUEST: usize = 64 * 1024;
pub const MAX_RESPONSE: usize = 4 * 1024 * 1024;

pub type CallResult = Result<Response, ApiError>;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("connection error: {0}")]
    Io(#[from] io::Error),
    #[error("message of {0} bytes is too large")]
    TooLarge(usize),
    #[error("unsupported protocol version {0}")]
    Version(u8),
    #[error("malformed message")]
    Malformed,
}

pub fn write_frame(writer: &mut impl Write, payload: &[u8]) -> Result<(), ProtocolError> {
    let len = u32::try_from(payload.len()).map_err(|_| ProtocolError::TooLarge(payload.len()))?;
    writer.write_all(&len.to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

/// Reads one frame, or `None` on a clean end of stream. The limit applies before allocating, and
/// the buffer is wiped on drop because a request can carry the passphrase.
pub fn read_frame(
    reader: &mut impl Read,
    limit: usize,
) -> Result<Option<Zeroizing<Vec<u8>>>, ProtocolError> {
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(ProtocolError::Malformed),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > limit {
        return Err(ProtocolError::TooLarge(len));
    }
    let mut payload = Zeroizing::new(vec![0u8; len]);
    reader.read_exact(&mut payload)?;
    Ok(Some(payload))
}

fn body(payload: &[u8]) -> Result<&[u8], ProtocolError> {
    match payload.split_first() {
        Some((&PROTOCOL_VERSION, rest)) => Ok(rest),
        Some((&other, _)) => Err(ProtocolError::Version(other)),
        None => Err(ProtocolError::Malformed),
    }
}

fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T, ProtocolError> {
    postcard::from_bytes(body(payload)?).map_err(|_| ProtocolError::Malformed)
}

/// Serializes into a preallocated buffer, because a growing one would leave stray copies of the
/// secrets behind in memory.
pub fn encode_request(request: &Request) -> Result<Zeroizing<Vec<u8>>, ProtocolError> {
    let mut buffer = Zeroizing::new(vec![0u8; MAX_REQUEST]);
    buffer[0] = PROTOCOL_VERSION;
    let used = postcard::to_slice(request, &mut buffer[1..])
        .map_err(|_| ProtocolError::TooLarge(MAX_REQUEST))?
        .len();
    buffer.truncate(used + 1);
    Ok(buffer)
}

pub fn decode_request(payload: &[u8]) -> Result<Request, ProtocolError> {
    decode(payload)
}

pub fn encode_response(result: &CallResult) -> Result<Vec<u8>, ProtocolError> {
    let mut out = vec![PROTOCOL_VERSION];
    let encoded = postcard::to_stdvec(result).map_err(|_| ProtocolError::Malformed)?;
    out.extend_from_slice(&encoded);
    Ok(out)
}

pub fn decode_response(payload: &[u8]) -> Result<CallResult, ProtocolError> {
    decode(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    fn site() -> SiteId {
        SiteId {
            package: "a".into(),
            site: "s".into(),
        }
    }

    #[test]
    fn every_request_round_trips() {
        let requests = [
            Request::ListServices,
            Request::StartService { id: "a".into() },
            Request::StopService { id: "a".into() },
            Request::RestartService { id: "a".into() },
            Request::Unlock {
                passphrase: Secret::new("p\u{e4}ss \0 word"),
            },
            Request::ApplySettings {
                package: "a".into(),
                changes: vec![
                    SettingChange {
                        name: "k".into(),
                        value: SettingInput::Literal(SettingLiteral::Text("$(x.y)".into())),
                    },
                    SettingChange {
                        name: "n".into(),
                        value: SettingInput::Literal(SettingLiteral::Int(-3)),
                    },
                    SettingChange {
                        name: "r".into(),
                        value: SettingInput::Reference {
                            target: "monero-daemon".into(),
                            setting: "rpc-url".into(),
                        },
                    },
                    SettingChange {
                        name: "s".into(),
                        value: SettingInput::Secret(Secret::new("x")),
                    },
                    SettingChange {
                        name: "d".into(),
                        value: SettingInput::Reset,
                    },
                ],
            },
            Request::SetExposure {
                site: site(),
                exposure: Exposure {
                    local: true,
                    onion: false,
                    public: true,
                },
                confirmation: Some(PublicConfirmation {
                    typed_host: "shop.example.com".into(),
                }),
            },
            Request::InitEncryption {
                passphrase: Secret::new("p"),
            },
            Request::ChangePassphrase {
                current: Secret::new("a"),
                new: Secret::new("b"),
            },
            Request::DeviceOverview,
            Request::ListPackages,
            Request::PackageDetail { id: "a".into() },
            Request::SettingsForm {
                package: "a".into(),
            },
            Request::ListFeeds,
            Request::ListUpdates,
            Request::GetAutoUpdate,
            Request::SetAutoUpdate(AutoUpdatePrefs {
                frequency: "daily".into(),
                window_start: "02:00".into(),
                window_end: "05:00".into(),
                enabled: vec!["a".into()],
            }),
            Request::LogSources,
            Request::ReadLog {
                source: "a".into(),
                max_lines: 10,
            },
            Request::DriveStatus,
            Request::CheckDrive,
            Request::RepairDrive(RepairConfirmation {
                check_id: 7,
                accept_data_loss_risk: true,
            }),
            Request::ListSites,
            Request::SetPublicHost {
                site: site(),
                host: "h".into(),
            },
            Request::CheckReachability { site: site() },
            Request::ListProviders,
            Request::InstallService {
                id: "a".into(),
                passphrase: Some(Secret::new("p")),
            },
            Request::ListSshKeys,
            Request::AddSshKey {
                line: "ssh-ed25519 AAAA x".into(),
            },
            Request::RemoveSshKey {
                fingerprint: "SHA256:x".into(),
            },
            Request::MountDrive,
            Request::NodeStatus,
            Request::ListRoleBindings,
            Request::SetRoleBinding {
                role: "monero-daemon".into(),
                package: Some("monerod".into()),
            },
            Request::ListGrants,
            Request::SetGrant {
                grant: SettingGrant {
                    consumer: "a".into(),
                    provider: "b".into(),
                    setting: "token".into(),
                },
                approved: true,
            },
            Request::UseDataDrive {
                package: "a".into(),
                enabled: true,
            },
            Request::WebStatus,
            Request::ListDnsProviders,
            Request::SetDnsProvider {
                domain: "example.com".into(),
                provider: "cloudflare".into(),
                fields: vec![("api_token".into(), Secret::new("t"))],
            },
            Request::TestDnsProvider,
            Request::RetireStaleRecords,
            Request::CheckReachabilityDetail { site: site() },
            Request::AcmeSettings,
            Request::SetAcmeSettings(AcmeSettingsInput {
                email: Some("me@example.com".into()),
                staging: true,
                local_domain: Some("home.example.com".into()),
            }),
            Request::SetDns01Credentials {
                fields: vec![("api_key".into(), Secret::new("k")), ("api_secret".into(), Secret::new("s"))],
            },
            Request::CertificateStatus,
            Request::CheckLocalNames,
            Request::LocalAddressChanged,
            Request::LocalSiteStatus,
            Request::ListUsbDrives,
            Request::ListUsbBackups { device: "sdb1".into() },
            Request::CreateUsbBackup {
                device: "sdb1".into(),
                device_passphrase: None,
                backup_passphrase: Secret::new("b"),
            },
            Request::InspectUsbBackup {
                device: "sdb1".into(),
                name: "nodo-1.nodobak".into(),
                passphrase: Secret::new("b"),
            },
            Request::RestoreUsbBackup {
                device: "sdb1".into(),
                name: "nodo-1.nodobak".into(),
                passphrase: Secret::new("b"),
                new_device_passphrase: Some(Secret::new("d")),
                typed: "restore".into(),
            },
            Request::ApplyUpdatesWithPassphrase {
                updates: vec![UpdateApproval {
                    package: "a".into(),
                    to: "2_1".into(),
                    approval: None,
                }],
                passphrase: Secret::new("p"),
            },
            Request::SystemUpdateStatus,
            Request::CheckSystemUpdate,
            Request::ApplySystemUpdate { serial: 7 },
            Request::SetSystemAutoUpdate { enabled: true },
            Request::RunSystemUpdate,
            Request::UpdateStorageGrowth,
            Request::OnionStatus,
            Request::SetNodeOnion { enabled: true },
            Request::ListRollbacks,
            Request::RollbackPackage {
                package: "a".into(),
                to: "1_1".into(),
            },
            Request::WifiStatus,
            Request::SetWifiEnabled { enabled: true },
            Request::ScanWifi,
            Request::JoinWifi(WifiJoin {
                ssid: "caf\u{e9} \"x\"".into(),
                security: WifiSecurity::Wpa2Wpa3Personal,
                passphrase: Some(Secret::new("pass phrase")),
                accept_open: false,
            }),
            Request::JoinWifi(WifiJoin {
                ssid: "open".into(),
                security: WifiSecurity::Open,
                passphrase: None,
                accept_open: true,
            }),
            Request::ForgetWifi { ssid: "home".into() },
            Request::BluetoothStatus,
            Request::SetBluetoothEnabled { enabled: false },
            Request::ScanBluetooth,
            Request::PairBluetooth { address: "AA:BB:CC:DD:EE:FF".into() },
            Request::ConnectBluetooth { address: "AA:BB:CC:DD:EE:FF".into(), connect: true },
            Request::RemoveBluetooth { address: "AA:BB:CC:DD:EE:FF".into() },
        ];
        for request in requests {
            let bytes = encode_request(&request).unwrap();
            assert_eq!(decode_request(&bytes).unwrap(), request);
        }
    }

    #[test]
    fn every_response_round_trips() {
        let drive = DriveStatus {
            label: "NODO_BLOCKCHAIN".into(),
            mount_point: "/media/monero".into(),
            present: true,
            mounted: false,
            filesystem: Some("xfs".into()),
            total_bytes: 1,
            free_bytes: 0,
        };
        let info = PackageInfo {
            id: "a".into(),
            version: "1".into(),
            origin: Origin::Feed(Some("f".into())),
            roles: vec!["r".into()],
            needs: vec![],
            encrypted_storage: true,
            has_settings: false,
        };
        let results: Vec<CallResult> = vec![
            Ok(Response::Ok),
            Ok(Response::Services(vec![ServiceStatus {
                id: "a".into(),
                state: ServiceState::Locked,
                detail: "d".into(),
                roles: vec!["monero-daemon".into()],
                origin: Origin::System,
            }])),
            Ok(Response::RolledBack(Rollback {
                stage: RollbackStage::HealthCheck,
                reason: "r".into(),
                recovered: false,
                recovery_problem: Some("p".into()),
            })),
            Ok(Response::Overview(DeviceOverview {
                hostname: "h".into(),
                kernel: "k".into(),
                uptime_secs: 1,
                memory_total_kib: 2,
                memory_available_kib: 3,
                disk_total_bytes: 4,
                disk_free_bytes: 5,
                encryption: EncryptionState::NotSetUp,
                ssh_keys: 1,
                drive: Some(drive.clone()),
            })),
            Ok(Response::Packages(vec![info.clone()])),
            Ok(Response::PackageDetail(PackageDetail {
                info,
                sites: vec!["web".into()],
                inbound: vec!["18080/tcp lan".into()],
                outbound_internet: true,
                public_settings: vec!["rpc-port".into()],
            })),
            Ok(Response::SettingsForm(SettingsForm {
                package: "a".into(),
                fields: vec![SettingField {
                    name: "n".into(),
                    label: "N".into(),
                    help: String::new(),
                    required: true,
                    kind: SettingKind::Int {
                        min: Some(1),
                        max: None,
                    },
                    current: SettingCurrent::Reference {
                        target: "t".into(),
                        setting: "s".into(),
                        resolved: None,
                    },
                    default: Some("3".into()),
                    public_name: None,
                }],
            })),
            Ok(Response::Feeds(vec![])),
            Ok(Response::Updates(vec![])),
            Ok(Response::LogSources(vec!["a".into()])),
            Ok(Response::Log(LogChunk {
                source: "a".into(),
                lines: vec!["l".into()],
                truncated: true,
            })),
            Ok(Response::Drive(Some(drive))),
            Ok(Response::DriveReport(DriveReport {
                check_id: 3,
                clean: false,
                output: vec!["bad".into()],
            })),
            Ok(Response::Sites(vec![SiteInfo {
                id: site(),
                exposure: Exposure::default(),
                multi_address: false,
                public_host: None,
                locked: false,
            }])),
            Ok(Response::Reachability(ReachabilityReport::Cgnat {
                wan_address: "100.64.0.1".into(),
                internet_address: "1.2.3.4".into(),
            })),
            Ok(Response::Providers(vec![])),
            Ok(Response::SshKeys(vec![SshKey {
                algorithm: "ssh-ed25519".into(),
                fingerprint: "SHA256:x".into(),
                comment: "c".into(),
            }])),
            Ok(Response::WebStatus(crate::WebStatus {
                sites: vec![crate::WebSiteStatus {
                    id: SiteId { package: "shop".into(), site: "web".into() },
                    local_url: Some("https://shop.local".into()),
                    onion_url: None,
                    public_url: None,
                    local_waiting: false,
                    rate_limit_unenforced: true,
                }],
                local_address: Some("192.168.1.60".into()),
                local_unavailable: None,
                local_remedy: None,
                domain: Some("example.com".into()),
                provider: Some("cloudflare".into()),
                ddns_running: true,
                public_ip: Some("93.184.216.34".into()),
                dns_changed_at: Some(1),
                stale_records: vec!["old/A".into()],
            })),
            Ok(Response::DnsProviders(vec![crate::DnsProviderDetail {
                id: "godaddy".into(),
                name: "GoDaddy".into(),
                dynamic_dns: true,
                unavailable_reason: None,
                dns01: crate::Dns01Support::BundledSeparateCredentials("key".into()),
                caveat: Some("c".into()),
                fields: vec!["personal_access_token".into()],
                configured: false,
            }])),
            Ok(Response::ProviderTest("ok".into())),
            Ok(Response::RetiredRecords(vec!["shop/A".into()])),
            Ok(Response::ReachabilityDetail(crate::ReachabilityDetail {
                checks: vec![crate::ReachabilityItem {
                    id: "port_443".into(),
                    status: crate::CheckStatus::Unknown,
                    detail: "no outside vantage point".into(),
                }],
                verdict: "ports_unverified".into(),
                public_ip: None,
                router_wan: None,
                device_ip: None,
                gateway: None,
                manual_check: vec!["step".into()],
                warnings: Vec::new(),
            })),
            Err(ApiError::Denied),
            Err(ApiError::BadPassphrase),
            Err(ApiError::NotFound("n".into())),
            Err(ApiError::Failed("f".into())),
            Err(ApiError::NotImplemented("feeds".into())),
            Err(ApiError::RateLimited {
                retry_after_secs: 4,
            }),
            Err(ApiError::NeedsConfirmation("c".into())),
            Err(ApiError::NotSetUp),
            Ok(Response::NodeStatus(NodeStatus {
                provider: "cuprated".into(),
                height: 3_000_000,
                target_height: 3_100_000,
                outgoing_peers: 32,
                incoming_peers: 4,
                synchronized: false,
            })),
            Ok(Response::RoleBindings(vec![RoleBinding {
                role: "monero-daemon".into(),
                providers: vec!["cuprated".into(), "monerod".into()],
                bound: None,
            }])),
            Ok(Response::Grants(vec![SettingGrant {
                consumer: "a".into(),
                provider: "b".into(),
                setting: "s".into(),
            }])),
            Err(ApiError::Locked("unlock first".into())),
            Ok(Response::AcmeSettings(AcmeSettingsInfo {
                email: None,
                staging: false,
                directory: "https://acme-v02.api.letsencrypt.org/directory".into(),
                test_server: false,
                challenge: ChallengeChoice::PortsNeeded { reason: "no provider".into() },
                dns01_fields: vec!["api_key".into()],
                dns01_stored: false,
                local_domain: None,
                local_address: Some("192.168.1.60".into()),
            })),
            Ok(Response::Certificates(CertificateReport {
                hosts: vec![HostCertificate {
                    site: site(),
                    host: "shop.example.com".into(),
                    kind: HostKind::Public,
                    state: CertState::Failed,
                    issuer: Some("Let's Encrypt (R11)".into()),
                    not_after: Some(5),
                    last_renewal: None,
                    challenge: Some("http-01".into()),
                    last_error: Some(CertProblem {
                        at: 4,
                        kind: CertProblemKind::ChallengeFailed { challenges: vec!["http-01".into()] },
                        summary: "s".into(),
                        detail: "d".into(),
                    }),
                }],
                challenge: ChallengeChoice::Dns01 { provider: "Cloudflare".into() },
                staging: false,
                test_server: true,
                local_ca_fingerprint: None,
            })),
            Ok(Response::LocalNames(vec![LocalNameCheck {
                host: "shop.home.example.com".into(),
                address: "192.168.1.60".into(),
                published: vec!["192.168.1.60".into()],
                resolves_to: vec![],
                verdict: LocalNameVerdict::Blocked,
                explanation: "e".into(),
            }])),
            Ok(Response::LocalSiteStatus(LocalSiteStatus {
                address: Some("192.168.1.61".into()),
                trust_url: Some("http://nodo.local".into()),
                ca_fingerprint: Some("AB:CD".into()),
                records_note: Some("n".into()),
            })),
            Ok(Response::UsbDrives(vec![UsbDrive {
                device: "sdb1".into(),
                label: Some("BACKUP".into()),
                model: "SanDisk Ultra".into(),
                filesystem: Some("vfat".into()),
                size_bytes: 32_000_000_000,
                read_only: false,
                problem: None,
            }])),
            Ok(Response::UsbBackups(UsbBackupList {
                device: "sdb1".into(),
                backups: vec![BackupInfo {
                    name: "nodo-1.nodobak".into(),
                    size_bytes: 9,
                    created: 1,
                }],
                free_bytes: 5,
                read_only: true,
            })),
            Ok(Response::SystemUpdate(SystemUpdateStatus {
                unavailable: None,
                current_serial: 6,
                current_snapshot: Some("2026.12".into()),
                available: Some(SystemSnapshot {
                    serial: 7,
                    snapshot: "2026.11".into(),
                    needs_restart: false,
                    withdraws_current: true,
                }),
                last_check: Some(1),
                last_error: None,
                automatic: false,
                verdict: "waits for you".into(),
                last_applied: None,
            })),
            Ok(Response::SystemUpdateApplied(SystemUpdateResult {
                at: 2,
                serial: 7,
                snapshot: "2026.11".into(),
                changed: vec!["nodo-tools: 0.2_1 -> 0.1_1".into()],
                restarted: vec![],
                deferred: vec!["nodo-helper".into()],
                needs_restart: false,
                withdrew: Some("2026.12".into()),
                automatic: false,
            })),
            Ok(Response::StorageGrowth(vec![StorageGrowth {
                package: "vault".into(),
                to: "2.0_1".into(),
                from_bytes: 256 << 20,
                to_bytes: 320 << 20,
            }])),
            Ok(Response::Onion(OnionStatus {
                tor_installed: true,
                tor_running: false,
                node: Some(NodeOnion {
                    provider: "cuprated".into(),
                    enabled: true,
                    restricted_rpc: true,
                    port: 18089,
                    address: Some("abc.onion:18089".into()),
                }),
                sites: vec![SiteOnion {
                    id: site(),
                    exposure: Exposure::default(),
                    multi_address: true,
                    locked: false,
                    url: None,
                }],
            })),
            Ok(Response::Rollbacks(vec![RollbackOption {
                package: "a".into(),
                current: "2_1".into(),
                previous: "1_1".into(),
                blocked: Some("no".into()),
            }])),
            Ok(Response::Wifi(WifiStatus {
                unavailable: None,
                interface: Some("wlan0".into()),
                enabled: true,
                link: WifiLink::Connected,
                ssid: Some("home".into()),
                signal_dbm: Some(-51),
                frequency_mhz: Some(2412),
                security: Some(WifiSecurity::Wpa2Personal),
                address: Some("192.168.1.20".into()),
                wired_up: true,
                saved: vec![SavedWifi {
                    ssid: "home".into(),
                    security: WifiSecurity::Wpa3Personal,
                    added_at: 9,
                }],
            })),
            Ok(Response::WifiNetworks(vec![WifiNetwork {
                ssid: "home".into(),
                signal_dbm: -40,
                frequency_mhz: 5180,
                security: WifiSecurity::Enterprise,
                saved: false,
                connected: false,
            }])),
            Ok(Response::Bluetooth(BluetoothStatus {
                unavailable: Some("no controller".into()),
                enabled: true,
                powered: false,
                controller: Some("00:11:22:33:44:55".into()),
                devices: vec![BluetoothDevice {
                    address: "AA:BB:CC:DD:EE:FF".into(),
                    name: "Keyboard".into(),
                    kind: Some("input-keyboard".into()),
                    paired: true,
                    trusted: true,
                    connected: false,
                    rssi: Some(-60),
                }],
                pairing: Some(BluetoothPairing {
                    address: "AA:BB:CC:DD:EE:FF".into(),
                    stage: PairingStage::TypePasskey("123456".into()),
                }),
            })),
        ];
        for result in results {
            let bytes = encode_response(&result).unwrap();
            assert_eq!(decode_response(&bytes).unwrap(), result);
        }
    }

    /// Fixes the wire position of the first variants, so an accidental reorder fails here
    /// instead of on a device talking to an older helper.
    #[test]
    fn variant_positions_are_stable() {
        let bytes = encode_request(&Request::ListServices).unwrap();
        assert_eq!(bytes.as_slice(), [PROTOCOL_VERSION, 0]);
        let bytes = encode_request(&Request::Unlock {
            passphrase: Secret::new("x"),
        })
        .unwrap();
        assert_eq!(bytes.as_slice(), [PROTOCOL_VERSION, 4, 1, b'x']);
    }

    #[test]
    fn frames_round_trip_and_end_cleanly() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"hello").unwrap();
        let mut reader = wire.as_slice();
        assert_eq!(
            read_frame(&mut reader, 10).unwrap().unwrap().as_slice(),
            b"hello"
        );
        assert!(read_frame(&mut reader, 10).unwrap().is_none());
    }

    #[test]
    fn oversized_frame_is_refused_before_allocating() {
        let wire = u32::MAX.to_be_bytes();
        let result = read_frame(&mut wire.as_slice(), MAX_REQUEST);
        assert!(matches!(result, Err(ProtocolError::TooLarge(_))));
    }

    #[test]
    fn truncated_frames_are_errors() {
        assert!(read_frame(&mut [0u8, 0].as_slice(), 10).is_err());
        let mut wire = 5u32.to_be_bytes().to_vec();
        wire.push(1);
        assert!(read_frame(&mut wire.as_slice(), 10).is_err());
    }

    #[test]
    fn wrong_version_and_garbage_are_refused() {
        assert!(matches!(
            decode_request(&[9, 0]),
            Err(ProtocolError::Version(9))
        ));
        assert!(matches!(decode_request(&[]), Err(ProtocolError::Malformed)));
        assert!(matches!(
            decode_request(&[PROTOCOL_VERSION, 200, 200, 200]),
            Err(ProtocolError::Malformed)
        ));
    }

    #[test]
    fn oversized_request_fails_to_encode() {
        let request = Request::Unlock {
            passphrase: Secret::new("x".repeat(MAX_REQUEST)),
        };
        assert!(matches!(
            encode_request(&request),
            Err(ProtocolError::TooLarge(_))
        ));
    }
}
