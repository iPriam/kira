//! Tokio operation ownership and the nonblocking C surface.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::runtime::{Builder, Handle};
use tokio::task::AbortHandle;

use crate::request::{Cursor, ResponseData};
use crate::{http, io, websocket};

/// A stable operation identifier passed through Kira as an `Int`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperationId(u64);

impl OperationId {
    /// The invalid handle reserved for failed starts and absent values.
    pub const INVALID: Self = Self(0);

    /// Converts the handle to the Kira `Int` representation.
    pub const fn as_i64(self) -> i64 {
        self.0 as i64
    }

    /// Converts a Kira handle, rejecting negative values as an invalid id.
    pub const fn from_i64(value: i64) -> Self {
        if value <= 0 {
            Self::INVALID
        } else {
            Self(value as u64)
        }
    }
}

/// The three states exposed by `kira_network_poll`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollState {
    /// The background future has not finished.
    Pending,
    /// The future returned a successful value.
    Ready,
    /// The future returned a network error.
    Failed,
}

/// Why a networking operation could not be started or completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkError {
    /// The Tokio runtime could not be initialized.
    RuntimeInit,
    /// The operation id does not name a live operation.
    UnknownHandle,
    /// A requested local socket could not be bound.
    Bind,
    /// A client could not connect to its server.
    Connect,
    /// A protocol handshake or frame was invalid.
    Protocol,
    /// A transport read or write failed.
    Io,
    /// A result was requested before the future completed.
    NotReady,
    /// A requested server has no certificate to share with an HTTP/3 client.
    MissingCertificate,
    /// An operation id space overflowed.
    IdExhausted,
    /// A URI could not be parsed.
    InvalidUri,
    /// A request exceeded its configured deadline.
    Timeout,
    /// An operation or request was cancelled.
    Canceled,
    /// A body exceeded its configured size limit.
    BodyTooLarge,
    /// Name resolution failed or returned no addresses.
    Dns,
    /// A header value or header configuration was invalid.
    Header,
    /// The requested protocol or transport is not available in this API.
    Unsupported,
    /// A configuration value was invalid.
    InvalidConfig,
    /// Bytes a caller asked for as text are not valid UTF-8.
    Encoding,
}

impl NetworkError {
    /// Every error this crate can report.
    ///
    /// Listed so a check over the C ABI can be exhaustive: the public header
    /// has to name a constant for each of these, and a variant added without
    /// one is caught by the test that walks this rather than by whoever first
    /// reads `-118` out of a Kira program.
    pub const ALL: [Self; 18] = [
        Self::RuntimeInit,
        Self::UnknownHandle,
        Self::Bind,
        Self::Connect,
        Self::Protocol,
        Self::Io,
        Self::NotReady,
        Self::MissingCertificate,
        Self::IdExhausted,
        Self::InvalidUri,
        Self::Timeout,
        Self::Canceled,
        Self::BodyTooLarge,
        Self::Dns,
        Self::Header,
        Self::Unsupported,
        Self::InvalidConfig,
        Self::Encoding,
    ];

    /// The stable negative code returned through the C ABI.
    pub const fn code(self) -> i64 {
        match self {
            Self::RuntimeInit => -100,
            Self::UnknownHandle => -101,
            Self::Bind => -102,
            Self::Connect => -103,
            Self::Protocol => -104,
            Self::Io => -105,
            Self::NotReady => -106,
            Self::MissingCertificate => -107,
            Self::IdExhausted => -108,
            Self::InvalidUri => -109,
            Self::Timeout => -110,
            Self::Canceled => -111,
            Self::BodyTooLarge => -112,
            Self::Dns => -113,
            Self::Header => -114,
            Self::Unsupported => -115,
            Self::InvalidConfig => -116,
            Self::Encoding => -117,
        }
    }
}

impl std::fmt::Display for NetworkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::RuntimeInit => "network runtime initialization failed",
            Self::UnknownHandle => "unknown network operation handle",
            Self::Bind => "socket bind failed",
            Self::Connect => "network connection failed",
            Self::Protocol => "network protocol operation failed",
            Self::Io => "network I/O failed",
            Self::NotReady => "network operation is not ready",
            Self::MissingCertificate => "network certificate is missing",
            Self::IdExhausted => "network operation id space is exhausted",
            Self::InvalidUri => "URI is invalid",
            Self::Timeout => "network operation timed out",
            Self::Canceled => "network operation was canceled",
            Self::BodyTooLarge => "network body exceeds its configured limit",
            Self::Dns => "DNS resolution failed",
            Self::Header => "network header is invalid",
            Self::Unsupported => "network operation is unsupported",
            Self::InvalidConfig => "network configuration is invalid",
            Self::Encoding => "network bytes are not UTF-8 text",
        })
    }
}

impl std::error::Error for NetworkError {}

impl From<std::io::Error> for NetworkError {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}

/// Converts an operation error into the C ABI's negative result code.
pub fn error_code(error: NetworkError) -> i64 {
    error.code()
}

/// What a finished operation produced.
///
/// A loopback operation answers with one number, and a request answers with a
/// whole response. Both are the same handle to a caller, which is what lets one
/// poll, one result, and one cancel serve every operation this crate starts.
#[derive(Debug)]
pub(crate) enum OperationValue {
    /// The completed value of an operation whose answer is a number.
    Code(i64),
    /// The response an HTTP request received.
    Response(Box<ResponseData>),
}

impl OperationValue {
    /// The number `kira_network_result` reports for this value.
    fn code(&self) -> i64 {
        match self {
            Self::Code(value) => *value,
            Self::Response(response) => i64::from(response.status()),
        }
    }
}

#[derive(Debug)]
enum OperationStatus {
    Pending,
    Ready(OperationValue),
    Failed(NetworkError),
}

#[derive(Debug)]
struct Operation {
    status: Arc<Mutex<OperationStatus>>,
    port: Option<u16>,
    is_server: bool,
    abort: AbortHandle,
    /// Where the next response read starts, for the operations that have one.
    ///
    /// The cursor belongs to the operation rather than to the reader because
    /// the C surface hands out no reader: a caller holds the same handle it
    /// polled, and reading advances state that has to survive between calls.
    cursor: Mutex<Cursor>,
}

#[derive(Debug, Clone)]
struct ServerInfo {
    operation: OperationId,
    certificate: Option<Arc<[u8]>>,
}

#[derive(Debug)]
struct NetworkRuntime {
    handle: Handle,
    next_id: AtomicU64,
    operations: Mutex<HashMap<OperationId, Operation>>,
    servers: Mutex<HashMap<u16, ServerInfo>>,
}

static RUNTIME: OnceLock<Result<NetworkRuntime, NetworkError>> = OnceLock::new();

fn runtime() -> Result<&'static NetworkRuntime, NetworkError> {
    RUNTIME
        .get_or_init(|| {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            let spawned = std::thread::Builder::new()
                .name("kira-network-tokio".to_owned())
                .spawn(move || {
                    let built = Builder::new_multi_thread()
                        .enable_io()
                        .enable_time()
                        .build()
                        .map_err(|_| NetworkError::RuntimeInit);
                    match built {
                        Ok(runtime) => {
                            let send = sender.send(Ok(runtime.handle().clone()));
                            if send.is_ok() {
                                runtime.block_on(std::future::pending::<()>())
                            }
                        }
                        Err(error) => {
                            let _ = sender.send(Err(error));
                        }
                    }
                });
            if spawned.is_err() {
                return Err(NetworkError::RuntimeInit);
            }
            match receiver.recv() {
                Ok(Ok(handle)) => Ok(NetworkRuntime {
                    handle,
                    next_id: AtomicU64::new(1),
                    operations: Mutex::new(HashMap::new()),
                    servers: Mutex::new(HashMap::new()),
                }),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(NetworkError::RuntimeInit),
            }
        })
        .as_ref()
        .map_err(|error| *error)
}

fn next_id(runtime: &NetworkRuntime) -> Result<OperationId, NetworkError> {
    let value = runtime.next_id.fetch_add(1, Ordering::Relaxed);
    if value == 0 || value > i64::MAX as u64 {
        return Err(NetworkError::IdExhausted);
    }
    Ok(OperationId(value))
}

fn register<F>(
    future: F,
    port: Option<u16>,
    is_server: bool,
    certificate: Option<Arc<[u8]>>,
) -> Result<OperationId, NetworkError>
where
    F: Future<Output = Result<OperationValue, NetworkError>> + Send + 'static,
{
    let runtime = runtime()?;
    let id = next_id(runtime)?;
    let status = Arc::new(Mutex::new(OperationStatus::Pending));
    let status_for_task = Arc::clone(&status);
    let task = runtime.handle.spawn(async move {
        let result = future.await;
        let next = match result {
            Ok(value) => OperationStatus::Ready(value),
            Err(error) => OperationStatus::Failed(error),
        };
        if let Ok(mut current) = status_for_task.lock() {
            *current = next;
        }
    });
    let operation = Operation {
        status,
        port,
        is_server,
        abort: task.abort_handle(),
        cursor: Mutex::new(Cursor::default()),
    };
    let mut operations = runtime
        .operations
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    operations.insert(id, operation);
    drop(operations);
    if let Some(port) = port {
        let mut servers = runtime
            .servers
            .lock()
            .map_err(|_| NetworkError::RuntimeInit)?;
        servers.insert(
            port,
            ServerInfo {
                operation: id,
                certificate,
            },
        );
    }
    Ok(id)
}

fn register_server<F>(
    future: F,
    port: u16,
    certificate: Option<Arc<[u8]>>,
) -> Result<OperationId, NetworkError>
where
    F: Future<Output = Result<i64, NetworkError>> + Send + 'static,
{
    register(
        async move { future.await.map(OperationValue::Code) },
        Some(port),
        true,
        certificate,
    )
}

fn register_client<F>(future: F) -> Result<OperationId, NetworkError>
where
    F: Future<Output = Result<i64, NetworkError>> + Send + 'static,
{
    register(
        async move { future.await.map(OperationValue::Code) },
        None,
        false,
        None,
    )
}

/// Registers a request whose completed value is a whole response.
pub(crate) fn register_request<F>(future: F) -> Result<OperationId, NetworkError>
where
    F: Future<Output = Result<ResponseData, NetworkError>> + Send + 'static,
{
    register(
        async move {
            future
                .await
                .map(|response| OperationValue::Response(Box::new(response)))
        },
        None,
        false,
        None,
    )
}

/// Starts an HTTP/1.1 server future after binding its TCP listener.
pub fn start_http1_server() -> Result<OperationId, NetworkError> {
    let (listener, port) = http::bind_tcp()?;
    register_server(http::serve_http1(listener), port, None)
}

/// Starts an HTTP/1.1 client future.
pub fn start_http1_client(port: u16) -> Result<OperationId, NetworkError> {
    register_client(http::client_http1(port))
}

/// Starts an HTTP/2 cleartext server future after binding its TCP listener.
pub fn start_http2_server() -> Result<OperationId, NetworkError> {
    let (listener, port) = http::bind_tcp()?;
    register_server(http::serve_http2(listener), port, None)
}

/// Starts an HTTP/2 cleartext client future.
pub fn start_http2_client(port: u16) -> Result<OperationId, NetworkError> {
    register_client(http::client_http2(port))
}

/// Starts an HTTP/3 server future after binding a QUIC endpoint.
pub fn start_http3_server() -> Result<OperationId, NetworkError> {
    let (socket, port, certificate, config) = http::bind_http3()?;
    register_server(
        http::serve_http3(socket, config),
        port,
        Some(Arc::from(certificate)),
    )
}

/// Starts an HTTP/3 client using the certificate published by the matching server.
pub fn start_http3_client(port: u16) -> Result<OperationId, NetworkError> {
    let certificate = server_certificate(port)?;
    register_client(http::client_http3(port, certificate))
}

/// Starts the HTTPS loopback server, publishing its certificate for clients.
pub fn start_https_server() -> Result<OperationId, NetworkError> {
    let (listener, port, certificate, config) = http::bind_https()?;
    register_server(
        http::serve_https(listener, config),
        port,
        Some(Arc::from(certificate)),
    )
}

/// Starts a WebSocket server future after binding its TCP listener.
pub fn start_websocket_server() -> Result<OperationId, NetworkError> {
    let (listener, port) = websocket::bind_tcp()?;
    register_server(websocket::serve(listener), port, None)
}

/// Starts a WebSocket client future.
pub fn start_websocket_client(port: u16) -> Result<OperationId, NetworkError> {
    register_client(websocket::client(port))
}

/// Starts a raw async TCP echo round trip.
pub fn start_io_roundtrip() -> Result<OperationId, NetworkError> {
    let (listener, port) = io::bind_tcp()?;
    register_client(io::roundtrip(listener, port))
}

/// The certificate published by the loopback server bound to `port`.
pub(crate) fn server_certificate(port: u16) -> Result<Arc<[u8]>, NetworkError> {
    let runtime = runtime()?;
    let servers = runtime
        .servers
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    servers
        .get(&port)
        .and_then(|server| server.certificate.clone())
        .ok_or(NetworkError::MissingCertificate)
}

/// Returns the port associated with a server operation.
pub fn server_port(handle: OperationId) -> Result<i64, NetworkError> {
    let runtime = runtime()?;
    let operations = runtime
        .operations
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    let operation = operations.get(&handle).ok_or(NetworkError::UnknownHandle)?;
    operation
        .port
        .map(i64::from)
        .ok_or(NetworkError::UnknownHandle)
}

/// Polls an operation without waiting for it.
pub fn poll(handle: OperationId) -> Result<PollState, NetworkError> {
    let runtime = runtime()?;
    let operations = runtime
        .operations
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    let operation = operations.get(&handle).ok_or(NetworkError::UnknownHandle)?;
    let status = operation
        .status
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    Ok(match *status {
        OperationStatus::Pending => PollState::Pending,
        OperationStatus::Ready(_) => PollState::Ready,
        OperationStatus::Failed(_) => PollState::Failed,
    })
}

/// Reads an operation's completed value.
pub fn result(handle: OperationId) -> Result<i64, NetworkError> {
    let runtime = runtime()?;
    let operations = runtime
        .operations
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    let operation = operations.get(&handle).ok_or(NetworkError::UnknownHandle)?;
    let status = operation
        .status
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    match &*status {
        OperationStatus::Pending => Err(NetworkError::NotReady),
        OperationStatus::Ready(value) => Ok(value.code()),
        OperationStatus::Failed(error) => Err(*error),
    }
}

/// Reads from a completed request's response, advancing its cursor.
///
/// Failing with `NotReady` before the future finishes rather than answering
/// from an empty response: a caller that skipped polling would otherwise read
/// zero bytes and take that for an empty body.
pub(crate) fn with_response<T>(
    handle: OperationId,
    action: impl FnOnce(&ResponseData, &mut Cursor) -> T,
) -> Result<T, NetworkError> {
    let runtime = runtime()?;
    let operations = runtime
        .operations
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    let operation = operations.get(&handle).ok_or(NetworkError::UnknownHandle)?;
    let status = operation
        .status
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    let mut cursor = operation
        .cursor
        .lock()
        .map_err(|_| NetworkError::RuntimeInit)?;
    match &*status {
        OperationStatus::Pending => Err(NetworkError::NotReady),
        OperationStatus::Failed(error) => Err(*error),
        OperationStatus::Ready(OperationValue::Code(_)) => Err(NetworkError::Unsupported),
        OperationStatus::Ready(OperationValue::Response(response)) => {
            Ok(action(response, &mut cursor))
        }
    }
}

/// Cancels an operation and removes its server publication when necessary.
pub fn close(handle: OperationId) {
    let Ok(runtime) = runtime() else {
        return;
    };
    let Ok(mut operations) = runtime.operations.lock() else {
        return;
    };
    let Some(operation) = operations.remove(&handle) else {
        return;
    };
    operation.abort.abort();
    drop(operations);
    if operation.is_server
        && let Some(port) = operation.port
        && let Ok(mut servers) = runtime.servers.lock()
        && servers
            .get(&port)
            .is_some_and(|server| server.operation == handle)
    {
        servers.remove(&port);
    }
}
