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
