//! A QUIC bidirectional message channel for local peers.
//!
//! HTTP/3 answers a request and closes; a shell talking to a component wants a
//! connection that stays open and carries typed messages both ways. This module
//! is that transport: a server binds a QUIC endpoint and accepts connections,
//! each client opens one bidirectional stream, and both sides exchange
//! length-prefixed frames over it for as long as the connection lives. It is the
//! QUIC replacement for a local socket, not another HTTP surface.
//!
//! # Trust between local processes
//!
//! The server and its clients are separate processes, so the in-memory
//! certificate the HTTP/3 loopback tests share is unavailable here. Instead the
//! server writes its generated certificate to a path the caller names, and a
//! client reads that path and pins it as its single trust anchor. The trust
//! decision is a real one — a client verifies the certificate the server holds
//! the key to — rather than a verifier that accepts anything, and the private
//! path the certificate lives at is the secret, not the code.
//!
//! # The nonblocking C surface
//!
//! Like the rest of the crate, nothing here blocks the Kira thread. Starting a
//! server or a client returns a handle immediately; the QUIC work runs on the
//! shared Tokio runtime. A caller sends by handing over bytes and receives by
//! selecting the next buffered frame and reading it out, so a Kira loop polls
//! this the same way it polls everything else.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use quinn::{ClientConfig, Connection, Endpoint, EndpointConfig, ServerConfig};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::runtime::{self, NetworkError};

/// The ALPN this transport negotiates. Distinct from `h3`, so a QUIC endpoint
/// speaking this is never mistaken for one speaking HTTP/3.
const ALPN: &[u8] = b"kira-wt";

/// The largest frame accepted in either direction, matching the crate's other
/// body ceilings. A peer that announces a longer frame is dropped rather than
/// allowed to name an allocation.
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// One selected inbound frame, held as its raw bytes and read out from a cursor.
///
/// A text caller decodes Unicode scalars from these bytes; a binary caller takes
/// them one at a time — so the same frame serves a message read either way, and
/// a binary message is never corrupted by a text decode it did not ask for.
#[derive(Default)]
struct Selection {
    bytes: Vec<u8>,
    cursor: usize,
}

/// A live bidirectional channel: one QUIC stream, a queue each way, the frame
/// currently selected for reading, and the frame being staged to send.
struct Channel {
    outbound: mpsc::UnboundedSender<Vec<u8>>,
    inbound: Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    selection: Mutex<Selection>,
    /// Bytes staged for the next frame, appended one at a time and sent whole by
    /// `send_flush` — the byte-oriented path a binary message (Kten) takes,
    /// where a text frame's NUL-terminated string cannot carry arbitrary bytes.
    outgoing: Mutex<Vec<u8>>,
    closed: Arc<AtomicBool>,
    reader: Mutex<Option<AbortHandle>>,
    writer: Mutex<Option<AbortHandle>>,
    connection: Mutex<Option<Connection>>,
}

/// A bound server: its endpoint, the port it took, and the queue of channels
/// its accept loop has established but no caller has taken yet.
struct Server {
    endpoint: Endpoint,
    port: u16,
    accepted: Mutex<mpsc::UnboundedReceiver<u64>>,
    accept_loop: Mutex<Option<AbortHandle>>,
}

/// The transport's handles, apart from the one-shot operation registry because
/// a channel is long-lived state a caller returns to, not a value that completes
/// once.
struct WebTransport {
    next_id: AtomicU64,
    servers: Mutex<HashMap<u64, Arc<Server>>>,
    channels: Mutex<HashMap<u64, Arc<Channel>>>,
}

static WEB_TRANSPORT: OnceLock<WebTransport> = OnceLock::new();

fn state() -> &'static WebTransport {
    WEB_TRANSPORT.get_or_init(|| WebTransport {
        next_id: AtomicU64::new(1),
        servers: Mutex::new(HashMap::new()),
        channels: Mutex::new(HashMap::new()),
    })
}

fn next_id() -> Result<u64, NetworkError> {
    let value = state().next_id.fetch_add(1, Ordering::Relaxed);
    if value == 0 || value > i64::MAX as u64 {
        return Err(NetworkError::IdExhausted);
    }
    Ok(value)
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

// --- Transport ---------------------------------------------------------------

/// Keeps a channel alive across a long, quiet turn.
///
/// A harness answering one turn can be inside a model call for far longer than
/// QUIC's default idle timeout, and nothing flows on the connection while it is.
/// Left alone the connection would drop mid-answer, so the keep-alive sends a
/// ping well inside that window — driven by the runtime, not the caller, so it
/// fires even while the caller is blocked — and the idle timeout is widened to a
/// span no real turn outlives.
fn transport_config() -> Arc<quinn::TransportConfig> {
    let mut config = quinn::TransportConfig::default();
    config.keep_alive_interval(Some(Duration::from_secs(5)));
    config.max_idle_timeout(Some(
        Duration::from_secs(300)
            .try_into()
            .expect("five minutes is a valid idle timeout"),
    ));
    Arc::new(config)
}

// --- TLS ---------------------------------------------------------------------

/// The server's QUIC configuration, from a freshly generated localhost
/// certificate whose DER is returned so the caller can publish it.
fn server_config() -> Result<(ServerConfig, Vec<u8>), NetworkError> {
    use quinn::crypto::rustls::QuicServerConfig;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let (certificate_der, private_key_der) = crate::api::tls::self_signed_localhost()?;
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|_| NetworkError::Protocol)?
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(certificate_der.clone())],
        PrivateKeyDer::try_from(private_key_der).map_err(|_| NetworkError::Protocol)?,
    )
    .map_err(|_| NetworkError::Protocol)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(tls).map_err(|_| NetworkError::Protocol)?,
    ));
    config.transport_config(transport_config());
    Ok((config, certificate_der))
}

/// A client's QUIC configuration, trusting `certificate_der` and nothing else.
fn client_config(certificate_der: Vec<u8>) -> Result<ClientConfig, NetworkError> {
    use quinn::crypto::rustls::QuicClientConfig;
    use rustls::pki_types::CertificateDer;

    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(certificate_der))
        .map_err(|_| NetworkError::Protocol)?;
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|_| NetworkError::Protocol)?
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(tls).map_err(|_| NetworkError::Protocol)?,
    ));
    config.transport_config(transport_config());
    Ok(config)
}

// --- Framing -----------------------------------------------------------------

/// Runs a channel's reader and writer over one bidirectional stream.
///
/// The writer sends a zero-length frame first: a QUIC bidirectional stream does
/// not exist for the peer until its opener writes, so this is what makes the
/// server's `accept_bi` resolve before the client has anything to say. Both
/// loops end by marking the channel closed, so a drained receiver that finds the
/// flag set reports the close rather than an empty queue.
///
/// The connection itself is held by the [`Channel`], not here, because dropping
/// the last handle to it closes it out from under these streams.
fn drive(
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    closed: Arc<AtomicBool>,
    mut outbound: mpsc::UnboundedReceiver<Vec<u8>>,
    inbound: mpsc::UnboundedSender<Vec<u8>>,
) -> (AbortHandle, AbortHandle) {
    let write_closed = Arc::clone(&closed);
    let writer = tokio::spawn(async move {
        let mut send = send;
        if send.write_all(&0u32.to_be_bytes()).await.is_err() {
            write_closed.store(true, Ordering::SeqCst);
            return;
        }
        while let Some(message) = outbound.recv().await {
            let length = message.len() as u32;
            if send.write_all(&length.to_be_bytes()).await.is_err()
                || send.write_all(&message).await.is_err()
            {
                break;
            }
        }
        let _ = send.finish();
        write_closed.store(true, Ordering::SeqCst);
    });

    let read_closed = Arc::clone(&closed);
    let reader = tokio::spawn(async move {
        let mut recv = recv;
        loop {
            let mut header = [0u8; 4];
            if recv.read_exact(&mut header).await.is_err() {
                break;
            }
            let length = u32::from_be_bytes(header) as usize;
            if length == 0 {
                continue;
            }
            if length > MAX_FRAME {
                break;
            }
            let mut buffer = vec![0u8; length];
            if recv.read_exact(&mut buffer).await.is_err() {
                break;
            }
            if inbound.send(buffer).is_err() {
                break;
            }
        }
        read_closed.store(true, Ordering::SeqCst);
    });

    (writer.abort_handle(), reader.abort_handle())
}

/// Registers a channel over an established stream and returns its handle.
fn register_channel(
    connection: Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
) -> Result<u64, NetworkError> {
    let id = next_id()?;
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
    let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
    let closed = Arc::new(AtomicBool::new(false));
    let (writer, reader) = drive(send, recv, Arc::clone(&closed), outbound_rx, inbound_tx);
    let channel = Arc::new(Channel {
        outbound: outbound_tx,
        inbound: Mutex::new(inbound_rx),
        selection: Mutex::new(Selection::default()),
        outgoing: Mutex::new(Vec::new()),
        closed,
        reader: Mutex::new(Some(reader)),
        writer: Mutex::new(Some(writer)),
        connection: Mutex::new(Some(connection)),
    });
    state()
        .channels
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?
        .insert(id, channel);
    Ok(id)
}

// --- Server ------------------------------------------------------------------

/// Binds a server, publishes its certificate to `cert_path`, and starts
/// accepting connections. Returns the server handle.
pub fn server(cert_path: &str) -> Result<i64, NetworkError> {
    let handle = runtime::tokio_handle()?;
    let (config, certificate_der) = server_config()?;
    std::fs::write(cert_path, &certificate_der).map_err(|_| NetworkError::Io)?;

    let socket = std::net::UdpSocket::bind(loopback(0)).map_err(|_| NetworkError::Bind)?;
    socket.set_nonblocking(true).map_err(|_| NetworkError::Bind)?;
    let endpoint = {
        let _guard = handle.enter();
        Endpoint::new(
            EndpointConfig::default(),
            Some(config),
            socket,
            Arc::new(quinn::TokioRuntime),
        )
        .map_err(|_| NetworkError::Bind)?
    };
    let port = endpoint.local_addr().map_err(NetworkError::from)?.port();

    let (accept_tx, accept_rx) = mpsc::unbounded_channel();
    let accept_endpoint = endpoint.clone();
    let accept_loop = handle.spawn(async move {
        while let Some(incoming) = accept_endpoint.accept().await {
            let accept_tx = accept_tx.clone();
            tokio::spawn(async move {
                let Ok(connection) = incoming.await else {
                    return;
                };
                let Ok((send, recv)) = connection.accept_bi().await else {
                    return;
                };
                if let Ok(id) = register_channel(connection, send, recv) {
                    let _ = accept_tx.send(id);
                }
            });
        }
    });

    let id = next_id()?;
    let server = Arc::new(Server {
        endpoint,
        port,
        accepted: Mutex::new(accept_rx),
        accept_loop: Mutex::new(Some(accept_loop.abort_handle())),
    });
    state()
        .servers
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?
        .insert(id, server);
    Ok(id as i64)
}

/// The port a server bound.
pub fn server_port(id: i64) -> Result<i64, NetworkError> {
    let servers = state().servers.lock().map_err(|_| NetworkError::RuntimeInit)?;
    let server = servers.get(&(id as u64)).ok_or(NetworkError::UnknownHandle)?;
    Ok(i64::from(server.port))
}

/// The next channel a server has accepted, `0` when none is waiting, or a
/// negative code when the handle is unknown.
pub fn accept(id: i64) -> Result<i64, NetworkError> {
    let servers = state().servers.lock().map_err(|_| NetworkError::RuntimeInit)?;
    let server = servers.get(&(id as u64)).ok_or(NetworkError::UnknownHandle)?;
    let mut accepted = server.accepted.lock().map_err(|_| NetworkError::RuntimeInit)?;
    match accepted.try_recv() {
        Ok(channel) => Ok(channel as i64),
        Err(mpsc::error::TryRecvError::Empty) => Ok(0),
        Err(mpsc::error::TryRecvError::Disconnected) => Err(NetworkError::Canceled),
    }
}

// --- Client ------------------------------------------------------------------

/// Connects to a server on `port`, trusting the certificate at `cert_path`, and
/// returns a channel handle immediately.
///
/// The connection and its stream come up on the runtime; until they do, sends
/// buffer and receives report nothing. A handshake that fails marks the channel
/// closed, which a caller sees the next time it receives.
pub fn connect(port: u16, cert_path: &str) -> Result<i64, NetworkError> {
    let handle = runtime::tokio_handle()?;
    let certificate_der = std::fs::read(cert_path).map_err(|_| NetworkError::MissingCertificate)?;
    let config = client_config(certificate_der)?;

    let id = next_id()?;
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let closed = Arc::new(AtomicBool::new(false));

    let channel = Arc::new(Channel {
        outbound: outbound_tx,
        inbound: Mutex::new(inbound_rx),
        selection: Mutex::new(Selection::default()),
        outgoing: Mutex::new(Vec::new()),
        closed: Arc::clone(&closed),
        reader: Mutex::new(None),
        writer: Mutex::new(None),
        connection: Mutex::new(None),
    });
    state()
        .channels
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?
        .insert(id, Arc::clone(&channel));

    handle.spawn(async move {
        let outcome: Result<(Connection, quinn::SendStream, quinn::RecvStream), ()> = async {
            let mut endpoint = Endpoint::client(loopback(0)).map_err(|_| ())?;
            endpoint.set_default_client_config(config);
            let connection = endpoint
                .connect(loopback(port), "localhost")
                .map_err(|_| ())?
                .await
                .map_err(|_| ())?;
            let (send, recv) = connection.open_bi().await.map_err(|_| ())?;
            Ok((connection, send, recv))
        }
        .await;

        let Ok((connection, send, recv)) = outcome else {
            closed.store(true, Ordering::SeqCst);
            return;
        };
        let (writer, reader) = drive(send, recv, Arc::clone(&closed), outbound_rx, inbound_tx);
        if let Ok(mut slot) = channel.writer.lock() {
            *slot = Some(writer);
        }
        if let Ok(mut slot) = channel.reader.lock() {
            *slot = Some(reader);
        }
        if let Ok(mut slot) = channel.connection.lock() {
            *slot = Some(connection);
        }
    });

    Ok(id as i64)
}

// --- Channel I/O -------------------------------------------------------------

fn channel(id: i64) -> Result<Arc<Channel>, NetworkError> {
    let channels = state()
        .channels
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    channels
        .get(&(id as u64))
        .cloned()
        .ok_or(NetworkError::UnknownHandle)
}

/// Queues one text message to send over a channel.
pub fn send(id: i64, text: &str) -> Result<(), NetworkError> {
    let channel = channel(id)?;
    channel
        .outbound
        .send(text.as_bytes().to_vec())
        .map_err(|_| NetworkError::Canceled)
}

/// Selects the next inbound frame for reading and returns its length in bytes,
/// `0` when none has arrived, or a negative code when the channel has closed and
/// no frame remains.
pub fn receive(id: i64) -> Result<i64, NetworkError> {
    let channel = channel(id)?;
    let mut inbound = channel.inbound.lock().map_err(|_| NetworkError::RuntimeInit)?;
    match inbound.try_recv() {
        Ok(bytes) => {
            // The frame is kept as its raw bytes, not a lossy UTF-8 string: a
            // binary message (Kten) would be corrupted by a text decode. A text
            // reader decodes scalars from these bytes; a binary reader takes
            // them one at a time.
            let length = bytes.len() as i64;
            let mut selection = channel
                .selection
                .lock()
                .map_err(|_| NetworkError::RuntimeInit)?;
            *selection = Selection { bytes, cursor: 0 };
            Ok(length)
        }
        Err(mpsc::error::TryRecvError::Empty) => {
            if channel.closed.load(Ordering::SeqCst) {
                Err(NetworkError::Canceled)
            } else {
                Ok(0)
            }
        }
        Err(mpsc::error::TryRecvError::Disconnected) => Err(NetworkError::Canceled),
    }
}

/// Reads the next Unicode scalar of the selected frame, or `-1` at its end.
pub fn read_scalar(id: i64) -> Result<i64, NetworkError> {
    let channel = channel(id)?;
    let mut selection = channel
        .selection
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    let cursor = selection.cursor;
    if cursor >= selection.bytes.len() {
        return Ok(crate::request::END_OF_SELECTION);
    }
    let (scalar, advance) = {
        let rest = &selection.bytes[cursor..];
        match std::str::from_utf8(rest).ok().and_then(|text| text.chars().next()) {
            Some(character) => (i64::from(u32::from(character)), character.len_utf8()),
            None => return Ok(crate::request::END_OF_SELECTION),
        }
    };
    selection.cursor = cursor + advance;
    Ok(scalar)
}

/// Reads the next raw byte of the selected frame, or `-1` at its end — the
/// binary counterpart to `read_scalar`, for a message read as bytes (Kten).
pub fn read_byte(id: i64) -> Result<i64, NetworkError> {
    let channel = channel(id)?;
    let mut selection = channel
        .selection
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    let cursor = selection.cursor;
    if cursor >= selection.bytes.len() {
        return Ok(crate::request::END_OF_SELECTION);
    }
    let byte = selection.bytes[cursor];
    selection.cursor = cursor + 1;
    Ok(i64::from(byte))
}

/// Appends one byte to the frame being staged for sending.
pub fn send_byte(id: i64, byte: i32) -> Result<(), NetworkError> {
    let channel = channel(id)?;
    let mut outgoing = channel.outgoing.lock().map_err(|_| NetworkError::RuntimeInit)?;
    outgoing.push((byte & 255) as u8);
    Ok(())
}

/// Sends the staged bytes as one frame and clears the stage.
pub fn send_flush(id: i64) -> Result<(), NetworkError> {
    let channel = channel(id)?;
    let frame = {
        let mut outgoing = channel.outgoing.lock().map_err(|_| NetworkError::RuntimeInit)?;
        std::mem::take(&mut *outgoing)
    };
    channel.outbound.send(frame).map_err(|_| NetworkError::Canceled)
}

/// Closes a server or a channel and forgets its handle. Unknown handles are
/// ignored, so cleanup is idempotent at the C boundary.
pub fn close(id: i64) {
    let key = id as u64;
    if let Ok(mut servers) = state().servers.lock()
        && let Some(server) = servers.remove(&key)
    {
        if let Ok(mut accept_loop) = server.accept_loop.lock()
            && let Some(handle) = accept_loop.take()
        {
            handle.abort();
        }
        server.endpoint.close(0u32.into(), b"server closed");
        return;
    }
    if let Ok(mut channels) = state().channels.lock()
        && let Some(channel) = channels.remove(&key)
    {
        if let Ok(mut writer) = channel.writer.lock()
            && let Some(handle) = writer.take()
        {
            handle.abort();
        }
        if let Ok(mut reader) = channel.reader.lock()
            && let Some(handle) = reader.take()
        {
            handle.abort();
        }
        if let Ok(mut connection) = channel.connection.lock()
            && let Some(connection) = connection.take()
        {
            connection.close(0u32.into(), b"channel closed");
        }
    }
}
