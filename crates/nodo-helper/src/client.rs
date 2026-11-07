//! The client half, used by the TUI. Calls block until the helper finishes, which for a restart
//! or settings change can take minutes.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use nodo_api::{ApiError, Backend, Request, Response};
use zeroize::Zeroize;

use crate::protocol::{self, CallResult, ProtocolError, MAX_RESPONSE};

pub struct HelperClient {
    /// Set when each call should open its own connection.
    path: Option<PathBuf>,
    stream: Option<UnixStream>,
}

impl HelperClient {
    /// Fails now if the helper is unreachable. Each call then connects afresh, because the helper
    /// closes idle connections and a TUI can sit idle for days.
    pub fn connect(path: &Path) -> io::Result<Self> {
        Ok(HelperClient {
            path: Some(path.to_owned()),
            stream: Some(UnixStream::connect(path)?),
        })
    }

    /// Uses one existing connection for every call.
    pub fn from_stream(stream: UnixStream) -> Self {
        HelperClient {
            path: None,
            stream: Some(stream),
        }
    }

    fn exchange(&mut self, request: &Request) -> Result<CallResult, ProtocolError> {
        if let Some(path) = &self.path {
            self.stream = Some(UnixStream::connect(path)?);
        }
        let stream = self.stream.as_mut().ok_or(ProtocolError::Malformed)?;
        let frame = protocol::encode_request(request)?;
        let sent = protocol::write_frame(stream, &frame);
        // A denied client is answered and disconnected without being read, so a failed write may
        // still have a real answer waiting.
        match protocol::read_frame(stream, MAX_RESPONSE) {
            Ok(Some(payload)) => protocol::decode_response(&payload),
            Ok(None) => {
                sent?;
                Err(ProtocolError::Malformed)
            }
            Err(e) => {
                sent?;
                Err(e)
            }
        }
    }
}

impl Backend for HelperClient {
    fn call(&mut self, mut request: Request) -> Result<Response, ApiError> {
        let result = self.exchange(&request);
        if let Request::Unlock { passphrase } = &mut request {
            passphrase.zeroize();
        }
        result.unwrap_or_else(|e| Err(ApiError::Failed(e.to_string())))
    }
}
