//! Accepts connections, authenticates them with SO_PEERCRED, and answers framed requests.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use nodo_api::protocol::{self, CallResult, ProtocolError, MAX_REQUEST};
use nodo_api::{action_name, ApiError, Request};
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

/// Which socket a connection arrived on, and with it who may use it and for what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Socket {
    /// The main socket: every allowed uid, and everything an owner may do.
    Main,
    /// The touchscreen's socket: only this uid, within what [`crate::screen`] allows.
    Screen(u32),
    /// The web GUI's socket: only this uid, within what [`crate::webgui`] allows.
    Web(u32),
}

impl Socket {
    /// For the log, before the action name.
    fn label(self, uid: u32) -> String {
        match self {
            Socket::Main => format!("uid {uid}"),
            Socket::Screen(_) => "screen".to_owned(),
            Socket::Web(_) => "web".to_owned(),
        }
    }

    fn admits<S: System>(self, helper: &Helper<S>, uid: u32) -> bool {
        match self {
            Socket::Main => helper.allows(uid),
            Socket::Screen(only) | Socket::Web(only) => uid == only,
        }
    }

    fn handle<S: System>(self, helper: &Helper<S>, uid: u32, request: Request) -> CallResult {
        match self {
            Socket::Main => helper.handle(uid, request),
            Socket::Screen(_) => helper.handle_screen(uid, request),
            Socket::Web(_) => helper.handle_web(uid, request),
        }
    }
}

/// Serves one connection on the main socket.
pub fn serve_connection<S: System>(
    helper: &Helper<S>,
    stream: UnixStream,
    uid: u32,
) -> Result<(), ProtocolError> {
    serve_on(helper, stream, uid, Socket::Main)
}

/// Serves one connection on the screen's socket. Only `screen_uid` is answered, and only within
/// what [`crate::screen`] allows.
pub fn serve_screen_connection<S: System>(
    helper: &Helper<S>,
    stream: UnixStream,
    uid: u32,
    screen_uid: u32,
) -> Result<(), ProtocolError> {
    serve_on(helper, stream, uid, Socket::Screen(screen_uid))
}

/// Serves one connection on the web GUI's socket. Only `web_uid` is answered, and only within
/// what [`crate::webgui`] allows.
pub fn serve_web_connection<S: System>(
    helper: &Helper<S>,
    stream: UnixStream,
    uid: u32,
    web_uid: u32,
) -> Result<(), ProtocolError> {
    serve_on(helper, stream, uid, Socket::Web(web_uid))
}

/// Serves one connection. A denied peer gets its answer before any of its bytes are read, so an
/// unauthorized user cannot reach the parser at all.
fn serve_on<S: System>(
    helper: &Helper<S>,
    mut stream: UnixStream,
    uid: u32,
    socket: Socket,
) -> Result<(), ProtocolError> {
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    if !socket.admits(helper, uid) {
        match socket {
            Socket::Main => eprintln!("nodo-helper: denied connection from uid {uid}"),
            Socket::Screen(_) => eprintln!("nodo-helper: denied screen connection from uid {uid}"),
            Socket::Web(_) => eprintln!("nodo-helper: denied web connection from uid {uid}"),
        }
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
        let result = socket.handle(helper, uid, request);
        eprintln!("nodo-helper: {} {action}: {}", socket.label(uid), outcome(&result));
        respond(&mut stream, &result)?;
        // After the answer is out, so the client learns what the update did.
        helper.restart_if_updated();
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
    serve_with(listener, helper, Socket::Main)
}

/// Like [`serve`], for the screen's socket: every connection is handled as the screen.
pub fn serve_screen<S: System + 'static>(
    listener: UnixListener,
    helper: Arc<Helper<S>>,
    screen_uid: u32,
) -> io::Result<()> {
    serve_with(listener, helper, Socket::Screen(screen_uid))
}

/// Like [`serve`], for the web GUI's socket: every connection is handled as the web GUI.
pub fn serve_web<S: System + 'static>(
    listener: UnixListener,
    helper: Arc<Helper<S>>,
    web_uid: u32,
) -> io::Result<()> {
    serve_with(listener, helper, Socket::Web(web_uid))
}

fn serve_with<S: System + 'static>(
    listener: UnixListener,
    helper: Arc<Helper<S>>,
    socket: Socket,
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
                .and_then(|uid| serve_on(&helper, stream, uid, socket));
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
    use nodo_api::{Backend, Response};
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

    /// A set-up device with the web GUI on, serving one connection on the web GUI's socket.
    fn spawn_web_server(web_uid: u32) -> (UnixStream, std::thread::JoinHandle<()>) {
        let (client, server) = UnixStream::pair().unwrap();
        let handle = std::thread::spawn(move || {
            let sys = FakeSystem::default();
            sys.add_manifest(SHOP);
            let h = helper(sys);
            h.init_device(&nodo_api::Secret::new("correct horse battery")).unwrap();
            h.handle(1000, Request::SetWebGui(true)).unwrap();
            let uid = peer_uid(&server).unwrap();
            let _ = serve_web_connection(&h, server, uid, web_uid);
        });
        (client, handle)
    }

    #[test]
    fn the_web_socket_answers_only_the_web_guis_account() {
        // Another account, even one the main socket would serve, is denied before it is read.
        let (mut stream, handle) = spawn_web_server(own_uid() + 1);
        let frame = protocol::read_frame(&mut stream, protocol::MAX_RESPONSE).unwrap().unwrap();
        assert_eq!(protocol::decode_response(&frame).unwrap(), Err(ApiError::Denied));
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty());
        handle.join().unwrap();

        let (stream, handle) = spawn_web_server(own_uid() + 1);
        let mut client = HelperClient::from_stream(stream);
        assert_eq!(
            client.call(Request::WebGuiLogin { passphrase: nodo_api::Secret::new("correct horse battery") }),
            Err(ApiError::Denied)
        );
        handle.join().unwrap();
    }

    #[test]
    fn the_web_socket_serves_its_account_through_the_gate() {
        let (stream, handle) = spawn_web_server(own_uid());
        let mut client = HelperClient::from_stream(stream);
        // Closed: the typed refusal reaches the client whole, so it can tell it from any other.
        let refused = client.call(Request::ListServices).unwrap_err();
        assert!(refused.is_web_login_required(), "{refused:?}");
        let Ok(Response::WebGui(closed)) = client.call(Request::WebGuiStatus) else { panic!() };
        assert!(closed.enabled && !closed.open);
        assert_eq!(
            client.call(Request::WebGuiLogin { passphrase: nodo_api::Secret::new("nope nope nope") }),
            Err(ApiError::BadPassphrase)
        );
        let Ok(Response::WebGui(opened)) =
            client.call(Request::WebGuiLogin { passphrase: nodo_api::Secret::new("correct horse battery") })
        else {
            panic!()
        };
        assert!(opened.open);
        let Ok(Response::Services(list)) = client.call(Request::ListServices) else { panic!() };
        assert_eq!(list[0].id, "shop");
        // The switch is not the web GUI's, and the main socket's account rules do not apply here.
        assert_eq!(client.call(Request::SetWebGui(false)), Err(ApiError::Denied));
        assert_eq!(client.call(Request::WebGuiLogout), Ok(Response::Ok));
        assert!(client.call(Request::ListServices).unwrap_err().is_web_login_required());
        drop(client);
        handle.join().unwrap();
    }

    #[test]
    fn each_socket_keeps_to_its_own_rules() {
        // The same uid, three sockets: only the main one lets it act without a login or a PIN.
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        let mut h = helper(sys);
        h.allowed = AllowedUids::new([own_uid()]);
        let uid = own_uid();
        let stop = || Request::StopService { id: "shop".into() };
        assert!(Socket::Main.admits(&h, uid) && Socket::Screen(uid).admits(&h, uid) && Socket::Web(uid).admits(&h, uid));
        assert!(!Socket::Screen(uid + 1).admits(&h, uid) && !Socket::Web(uid + 1).admits(&h, uid));
        assert!(!Socket::Main.admits(&h, uid + 1));
        assert_eq!(Socket::Main.handle(&h, uid, stop()), Ok(Response::Ok));
        assert!(matches!(Socket::Screen(uid).handle(&h, uid, stop()), Err(ApiError::Locked(_))));
        assert!(Socket::Web(uid).handle(&h, uid, stop()).unwrap_err().is_web_login_required());
        assert_eq!((Socket::Main.label(7), Socket::Screen(7).label(7), Socket::Web(7).label(7)), ("uid 7".into(), "screen".into(), "web".into()));
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
