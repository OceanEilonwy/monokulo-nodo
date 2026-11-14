//! Accepts connections, authenticates them with SO_PEERCRED, and answers framed requests.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use nodo_api::protocol::{self, CallResult, ProtocolError, MAX_REQUEST};
use nodo_api::{action_name, ApiError};
use rustix::net::sockopt::socket_peercred;

use crate::helper::Helper;
use crate::system::System;

const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECTIONS: usize = 16;

/// The uid the kernel recorded when the client connected. A client cannot forge this, and passing
/// the socket to another process does not change it.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let cred = socket_peercred(stream)?;
    Ok(cred.uid.as_raw())
}

fn respond(stream: &mut UnixStream, result: &CallResult) -> Result<(), ProtocolError> {
    protocol::write_frame(stream, &protocol::encode_response(result)?)
}

/// Serves one connection. A denied peer gets its answer before any of its bytes are read, so an
/// unauthorized user cannot reach the parser at all.
pub fn serve_connection<S: System>(
    helper: &Helper<S>,
    mut stream: UnixStream,
    uid: u32,
) -> Result<(), ProtocolError> {
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    if !helper.allows(uid) {
        eprintln!("nodo-helper: denied connection from uid {uid}");
        return respond(&mut stream, &Err(ApiError::Denied));
    }
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    loop {
        let Some(frame) = protocol::read_frame(&mut stream, MAX_REQUEST)? else {
            return Ok(());
        };
        let request = match protocol::decode_request(&frame) {
            Ok(request) => request,
            Err(e) => {
                let reason = e.to_string();
                respond(&mut stream, &Err(ApiError::Failed(reason)))?;
                return Err(e);
            }
        };
        drop(frame);
        // Only the action name, as requests can hold the passphrase and setting values.
        let action = action_name(&request);
        let result = helper.handle(uid, request);
        eprintln!("nodo-helper: uid {uid} {action}: {}", outcome(&result));
        respond(&mut stream, &result)?;
    }
}

/// Serves one connection on the screen's socket. Only `screen_uid` is answered, and only within
/// what [`crate::screen`] allows.
pub fn serve_screen_connection<S: System>(
    helper: &Helper<S>,
    mut stream: UnixStream,
    uid: u32,
    screen_uid: u32,
) -> Result<(), ProtocolError> {
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    if uid != screen_uid {
        eprintln!("nodo-helper: denied screen connection from uid {uid}");
        return respond(&mut stream, &Err(ApiError::Denied));
    }
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    loop {
        let Some(frame) = protocol::read_frame(&mut stream, MAX_REQUEST)? else {
            return Ok(());
        };
        let request = match protocol::decode_request(&frame) {
            Ok(request) => request,
            Err(e) => {
                respond(&mut stream, &Err(ApiError::Failed(e.to_string())))?;
                return Err(e);
            }
        };
        drop(frame);
        let action = action_name(&request);
        let result = helper.handle_screen(uid, request);
        eprintln!("nodo-helper: screen {action}: {}", outcome(&result));
        respond(&mut stream, &result)?;
    }
}

/// The kind of result only. Error messages can name settings and paths, and are for the client.
fn outcome(result: &CallResult) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(ApiError::Denied) => "denied",
        Err(ApiError::BadPassphrase) => "bad passphrase",
        Err(ApiError::NotFound(_)) => "not found",
        Err(ApiError::Failed(_)) => "failed",
        Err(ApiError::NotImplemented(_)) => "not implemented",
        Err(ApiError::RateLimited { .. }) => "rate limited",
        Err(ApiError::NeedsConfirmation(_)) => "needs confirmation",
        Err(ApiError::NotSetUp) => "not set up",
        Err(ApiError::Locked(_)) => "locked",
    }
}

struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Runs forever, one thread per connection. The cap stops an allowed but misbehaving client from
/// exhausting threads with idle connections.
pub fn serve<S: System + 'static>(
    listener: UnixListener,
    helper: Arc<Helper<S>>,
) -> io::Result<()> {
    serve_with(listener, helper, None)
}

/// Like [`serve`], for the screen's socket: every connection is handled as the screen.
pub fn serve_screen<S: System + 'static>(
    listener: UnixListener,
    helper: Arc<Helper<S>>,
    screen_uid: u32,
) -> io::Result<()> {
    serve_with(listener, helper, Some(screen_uid))
}

fn serve_with<S: System + 'static>(
    listener: UnixListener,
    helper: Arc<Helper<S>>,
    screen_uid: Option<u32>,
) -> io::Result<()> {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("nodo-helper: accept failed: {e}");
                continue;
            }
        };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            eprintln!("nodo-helper: too many connections");
            continue;
        }
        let slot = Slot(active.clone());
        let helper = helper.clone();
        std::thread::spawn(move || {
            let _slot = slot;
            let result = peer_uid(&stream)
                .map_err(ProtocolError::from)
                .and_then(|uid| match screen_uid {
                    Some(screen) => serve_screen_connection(&helper, stream, uid, screen),
                    None => serve_connection(&helper, stream, uid),
                });
            if let Err(e) = result {
                eprintln!("nodo-helper: connection ended: {e}");
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, SHOP};
    use crate::helper::AllowedUids;
    use nodo_api::client::HelperClient;
    use nodo_api::{Backend, Request, Response};
    use rustix::process::geteuid;
    use std::io::{Read, Write};

    fn own_uid() -> u32 {
        geteuid().as_raw()
    }

    fn spawn_server(allowed: AllowedUids) -> (UnixStream, std::thread::JoinHandle<()>) {
        let (client, server) = UnixStream::pair().unwrap();
        let handle = std::thread::spawn(move || {
            let sys = FakeSystem::default();
            sys.add_manifest(SHOP);
            let mut h = helper(sys);
            h.allowed = allowed;
            let uid = peer_uid(&server).unwrap();
            let _ = serve_connection(&h, server, uid);
        });
        (client, handle)
    }

    #[test]
    fn peercred_reports_this_process() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&a).unwrap(), own_uid());
    }

    #[test]
    fn allowed_peer_is_served_over_many_requests() {
        let (stream, handle) = spawn_server(AllowedUids::new([own_uid()]));
        let mut client = HelperClient::from_stream(stream);
        let Ok(Response::Services(list)) = client.call(Request::ListServices) else {
            panic!()
        };
        assert_eq!(list[0].id, "shop");
        assert_eq!(
            client.call(Request::StartService { id: "shop".into() }),
            Ok(Response::Ok)
        );
        assert!(matches!(
            client.call(Request::StartService { id: "ghost".into() }),
            Err(ApiError::NotFound(_))
        ));
        drop(client);
        handle.join().unwrap();
    }

    #[test]
    fn denied_peer_gets_denied_without_being_read() {
        let (mut stream, handle) = spawn_server(AllowedUids::new([own_uid() + 1]));
        // Send nothing. A server that read first would hang here until its timeout.
        let frame = protocol::read_frame(&mut stream, protocol::MAX_RESPONSE)
            .unwrap()
            .unwrap();
        assert_eq!(
            protocol::decode_response(&frame).unwrap(),
            Err(ApiError::Denied)
        );
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty());
        handle.join().unwrap();
    }

    #[test]
    fn denied_client_call_reports_denied() {
        let (stream, handle) = spawn_server(AllowedUids::default());
        let mut client = HelperClient::from_stream(stream);
        assert_eq!(client.call(Request::ListServices), Err(ApiError::Denied));
        handle.join().unwrap();
    }

    #[test]
    fn garbage_gets_an_error_then_the_connection_closes() {
        let (mut stream, handle) = spawn_server(AllowedUids::new([own_uid()]));
        protocol::write_frame(&mut stream, &[protocol::PROTOCOL_VERSION, 250, 250]).unwrap();
        let frame = protocol::read_frame(&mut stream, protocol::MAX_RESPONSE)
            .unwrap()
            .unwrap();
        assert!(matches!(
            protocol::decode_response(&frame).unwrap(),
            Err(ApiError::Failed(_))
        ));
        assert!(protocol::read_frame(&mut stream, protocol::MAX_RESPONSE)
            .unwrap()
            .is_none());
        handle.join().unwrap();
    }

    #[test]
    fn oversized_frame_closes_the_connection() {
        let (mut stream, handle) = spawn_server(AllowedUids::new([own_uid()]));
        stream.write_all(&u32::MAX.to_be_bytes()).unwrap();
        handle.join().unwrap();
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty());
    }

    #[test]
    fn serves_over_a_real_socket_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("helper.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        let mut h = helper(sys);
        h.allowed = AllowedUids::new([own_uid()]);
        let h = Arc::new(h);
        std::thread::spawn(move || {
            let _ = serve(listener, h);
        });
        let mut client = HelperClient::connect(&path).unwrap();
        // A second call opens a new connection, as the helper may have closed an idle one.
        for _ in 0..2 {
            assert!(matches!(
                client.call(Request::ListServices),
                Ok(Response::Services(_))
            ));
        }
    }
}
