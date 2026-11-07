//! The wire format: a 4-byte big-endian length, then a version byte and a postcard body.
//!
//! Postcard identifies enum variants by position, so `nodo_api` enums may only grow at the end.
//! Reordering them requires a new [`PROTOCOL_VERSION`].

use std::io::{self, Read, Write};

use nodo_api::{ApiError, Request, Response};
use serde::de::DeserializeOwned;
use thiserror::Error;
use zeroize::Zeroizing;

pub const PROTOCOL_VERSION: u8 = 1;
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
/// passphrase behind in memory.
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
    use nodo_api::{Exposure, ServiceState, ServiceStatus};

    #[test]
    fn every_request_round_trips() {
        let requests = [
            Request::ListServices,
            Request::StartService { id: "a".into() },
            Request::StopService { id: "a".into() },
            Request::RestartService { id: "a".into() },
            Request::Unlock {
                passphrase: "p\u{e4}ss \0 word".into(),
            },
            Request::ApplySettings {
                package: "a".into(),
                values: vec![("k".into(), "v".into())],
            },
            Request::SetExposure {
                package: "a".into(),
                site: "s".into(),
                exposure: Exposure {
                    local: true,
                    onion: false,
                    public: true,
                },
            },
        ];
        for request in requests {
            let bytes = encode_request(&request).unwrap();
            assert_eq!(decode_request(&bytes).unwrap(), request);
        }
    }

    #[test]
    fn every_response_round_trips() {
        let results: Vec<CallResult> = vec![
            Ok(Response::Ok),
            Ok(Response::Services(vec![ServiceStatus {
                id: "a".into(),
                state: ServiceState::Locked,
                detail: "d".into(),
            }])),
            Ok(Response::RolledBack { reason: "r".into() }),
            Err(ApiError::Denied),
            Err(ApiError::BadPassphrase),
            Err(ApiError::NotFound("n".into())),
            Err(ApiError::Failed("f".into())),
        ];
        for result in results {
            let bytes = encode_response(&result).unwrap();
            assert_eq!(decode_response(&bytes).unwrap(), result);
        }
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
            passphrase: "x".repeat(MAX_REQUEST),
        };
        assert!(matches!(
            encode_request(&request),
            Err(ProtocolError::TooLarge(_))
        ));
    }
}
