//! Reusable async networking primitives layered over the protocol helpers.
//!
//! The original loopback surface remains available for small Kira FFI examples.
//! This module is the Rust-facing API used by larger hosts: it owns cancellation,
//! pooled HTTP clients, streaming server bodies, routing, DNS, UDP, and richer
//! WebSocket sessions without exposing Tokio or Hyper implementation details at
//! call sites.

use std::convert::Infallible;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use http_body::Frame;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::client::legacy::Client as PooledClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, accept_async, connect_async};

use crate::runtime::NetworkError;

type BoxResponseBody = BoxBody<Bytes, Infallible>;
type HandlerFuture =
    std::pin::Pin<Box<dyn Future<Output = Result<HttpServerResponse, NetworkError>> + Send>>;
type Handler = Arc<dyn Fn(HttpServerRequest) -> HandlerFuture + Send + Sync>;

/// A cancellation source that can be cloned by a server, its connections, and
/// the Kira-facing owner of the operation.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    state: Arc<CancellationState>,
}

#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationToken {
    /// Creates a token in the not-cancelled state.
    pub fn new() -> Self {
        Self {
            state: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }

    /// Requests cancellation and wakes every waiter.
    pub fn cancel(&self) {
        if !self.state.cancelled.swap(true, Ordering::SeqCst) {
            self.state.notify.notify_waiters();
        }
    }

    /// Returns whether cancellation has already been requested.
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::SeqCst)
    }

    /// Waits until cancellation is requested.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.state.notify.notified().await;
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Selects the HTTP version used by the pooled TCP client or router server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpVersion {
    /// HTTP/1.1 over TCP.
    Http1,
    /// HTTP/2 prior-knowledge (h2c) over TCP.
    Http2,
    /// HTTP/3 over QUIC; use the dedicated HTTP/3 API when it is available.
    Http3,
}

/// Configuration for [`HttpClient`].
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    /// The HTTP version and transport mode.
    pub version: HttpVersion,
    /// Maximum time spent waiting for a response, or `None` for no deadline.
    pub request_timeout: Option<Duration>,
    /// Maximum response body size accepted by [`HttpResponse::bytes`].
    pub max_response_body: usize,
    /// How long an idle pooled connection remains reusable.
    pub pool_idle_timeout: Duration,
    /// Maximum number of idle connections retained for one host.
    pub pool_max_idle_per_host: usize,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            version: HttpVersion::Http1,
            request_timeout: Some(Duration::from_secs(30)),
            max_response_body: 16 * 1024 * 1024,
            pool_idle_timeout: Duration::from_secs(30),
            pool_max_idle_per_host: 8,
        }
    }
}

/// A buffered HTTP request for the pooled client.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
}

impl HttpRequest {
    /// Creates a request after parsing `uri`.
    pub fn new(method: Method, uri: &str) -> Result<Self, NetworkError> {
        Ok(Self {
            method,
            uri: uri.parse().map_err(|_| NetworkError::InvalidUri)?,
            headers: HeaderMap::new(),
            body: Bytes::new(),
        })
    }

    /// Creates a GET request.
    pub fn get(uri: &str) -> Result<Self, NetworkError> {
        Self::new(Method::GET, uri)
    }

    /// Replaces the request body.
    pub fn with_body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    /// Adds a typed header and returns the updated request.
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Adds a header parsed from text.
    pub fn with_header_text(self, name: &str, value: &str) -> Result<Self, NetworkError> {
        let name = name.parse().map_err(|_| NetworkError::Header)?;
        let value = value.parse().map_err(|_| NetworkError::Header)?;
        Ok(self.with_header(name, value))
    }

    pub(crate) fn into_parts(self) -> (Method, Uri, HeaderMap, Bytes) {
        (self.method, self.uri, self.headers, self.body)
    }
}

/// A streaming HTTP response returned by [`HttpClient`].
pub struct HttpResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Incoming,
    bytes_read: usize,
    max_body: usize,
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("bytes_read", &self.bytes_read)
            .field("max_body", &self.max_body)
            .finish_non_exhaustive()
    }
}

impl HttpResponse {
    fn new(response: http::Response<Incoming>, max_body: usize) -> Self {
        let (parts, body) = response.into_parts();
        Self {
            status: parts.status,
            headers: parts.headers,
            body,
            bytes_read: 0,
            max_body,
        }
    }

    /// Returns the HTTP status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Reads the next data frame without buffering the whole response.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, NetworkError> {
        loop {
            let Some(frame) = self.body.frame().await else {
                return Ok(None);
            };
            let frame = frame.map_err(|_| NetworkError::Io)?;
            let Ok(data) = frame.into_data() else {
                continue;
            };
            self.bytes_read = self
                .bytes_read
                .checked_add(data.len())
                .ok_or(NetworkError::BodyTooLarge)?;
            if self.bytes_read > self.max_body {
                return Err(NetworkError::BodyTooLarge);
            }
            return Ok(Some(data));
        }
    }

    /// Consumes the response into one bounded byte buffer.
    pub async fn bytes(mut self) -> Result<Bytes, NetworkError> {
        let mut body = Vec::with_capacity(self.bytes_read.min(self.max_body));
        while let Some(chunk) = self.next_chunk().await? {
            body.extend_from_slice(&chunk);
        }
        Ok(Bytes::from(body))
    }
}

/// A pooled HTTP/1.1 or HTTP/2 client with deadlines and cancellation.
#[derive(Clone)]
pub struct HttpClient {
    inner: PooledClient<HttpConnector, Full<Bytes>>,
    config: HttpClientConfig,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    /// Builds a pooled client from configuration.
    pub fn new(config: HttpClientConfig) -> Result<Self, NetworkError> {
        if config.max_response_body == 0 || config.pool_max_idle_per_host == 0 {
            return Err(NetworkError::InvalidConfig);
        }
        if config.version == HttpVersion::Http3 {
            return Err(NetworkError::Unsupported);
        }
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        let mut builder = PooledClient::builder(TokioExecutor::new());
        builder
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(config.pool_idle_timeout)
            .pool_max_idle_per_host(config.pool_max_idle_per_host);
        if config.version == HttpVersion::Http2 {
            builder.http2_only(true);
        }
        Ok(Self {
            inner: builder.build(connector),
            config,
        })
    }

    /// Sends a request using this client's configured timeout.
    pub async fn request(&self, request: HttpRequest) -> Result<HttpResponse, NetworkError> {
        let token = CancellationToken::new();
        self.request_with_cancellation(request, &token).await
    }

    /// Sends a request and races it against a caller-owned cancellation token.
    pub async fn request_with_cancellation(
        &self,
        request: HttpRequest,
        token: &CancellationToken,
    ) -> Result<HttpResponse, NetworkError> {
        let mut message = http::Request::new(Full::new(request.body));
        *message.method_mut() = request.method;
        *message.uri_mut() = request.uri;
        *message.headers_mut() = request.headers;
        let future = self.inner.request(message);
        let response = if let Some(timeout) = self.config.request_timeout {
            tokio::select! {
                _ = token.cancelled() => return Err(NetworkError::Canceled),
                result = tokio::time::timeout(timeout, future) => {
                    result
                        .map_err(|_| NetworkError::Timeout)?
                        .map_err(|_| NetworkError::Connect)?
                }
            }
        } else {
            tokio::select! {
                _ = token.cancelled() => return Err(NetworkError::Canceled),
                result = future => result.map_err(|_| NetworkError::Connect)?,
            }
        };
        Ok(HttpResponse::new(response, self.config.max_response_body))
    }

    /// Sends a GET request through the pool.
    pub async fn get(&self, uri: &str) -> Result<HttpResponse, NetworkError> {
        self.request(HttpRequest::get(uri)?).await
    }

    /// Sends a request whose body is produced incrementally.
    ///
    /// Streaming requests use a dedicated connection because Hyper's legacy
    /// pooled client requires one concrete body type for every request. The
    /// response remains bounded and streaming exactly like [`request`].
    pub async fn request_streaming<S>(
        &self,
        request: HttpRequest,
        chunks: S,
    ) -> Result<HttpResponse, NetworkError>
    where
        S: Stream<Item = Bytes> + Send + 'static,
    {
        let token = CancellationToken::new();
        self.request_streaming_with_cancellation(request, chunks, &token)
            .await
    }

    /// Sends a streamed request and races the connection against cancellation.
    pub async fn request_streaming_with_cancellation<S>(
        &self,
        request: HttpRequest,
        chunks: S,
        token: &CancellationToken,
    ) -> Result<HttpResponse, NetworkError>
    where
        S: Stream<Item = Bytes> + Send + 'static,
    {
        if self.config.version == HttpVersion::Http3 {
            return Err(NetworkError::Unsupported);
        }
        let future = self.send_streaming(request, chunks);
        if let Some(timeout) = self.config.request_timeout {
            tokio::select! {
                _ = token.cancelled() => Err(NetworkError::Canceled),
                result = tokio::time::timeout(timeout, future) => {
                    result.map_err(|_| NetworkError::Timeout)?
                }
            }
        } else {
            tokio::select! {
                _ = token.cancelled() => Err(NetworkError::Canceled),
                result = future => result,
            }
        }
    }

    async fn send_streaming<S>(
        &self,
        request: HttpRequest,
        chunks: S,
    ) -> Result<HttpResponse, NetworkError>
    where
        S: Stream<Item = Bytes> + Send + 'static,
    {
        use hyper::client::conn::{http1, http2};

        let (method, uri, headers, _) = request.into_parts();
        let authority = uri.authority().ok_or(NetworkError::InvalidUri)?;
        let host = authority.host();
        let port = authority.port_u16().unwrap_or(80);
        let stream = TcpStream::connect((host, port))
            .await
            .map_err(|_| NetworkError::Connect)?;
        let body_stream = chunks
            .map(|chunk| Ok::<_, Infallible>(Frame::data(chunk)))
            .boxed();
        let body = StreamBody::new(body_stream);
        let mut message = http::Request::new(body);
        *message.method_mut() = method;
        *message.uri_mut() = uri;
        *message.headers_mut() = headers;
        let response = match self.config.version {
            HttpVersion::Http1 => {
                let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
                    .await
                    .map_err(|_| NetworkError::Protocol)?;
                tokio::spawn(async move {
                    let _ = connection.await;
                });
                sender
                    .send_request(message)
                    .await
                    .map_err(|_| NetworkError::Io)?
            }
            HttpVersion::Http2 => {
                let (mut sender, connection) =
                    http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
                        .await
                        .map_err(|_| NetworkError::Protocol)?;
                tokio::spawn(async move {
                    let _ = connection.await;
                });
                sender
                    .send_request(message)
                    .await
                    .map_err(|_| NetworkError::Io)?
            }
            HttpVersion::Http3 => return Err(NetworkError::Unsupported),
        };
        Ok(HttpResponse::new(response, self.config.max_response_body))
    }
}

/// A request accepted by [`HttpServer`], with a streaming body.
pub struct HttpServerRequest {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Incoming,
    bytes_read: usize,
    max_body: usize,
}

impl HttpServerRequest {
    fn new(request: http::Request<Incoming>, max_body: usize) -> Self {
        let (parts, body) = request.into_parts();
        Self {
            method: parts.method,
            uri: parts.uri,
            headers: parts.headers,
            body,
            bytes_read: 0,
            max_body,
        }
    }

    /// Returns the request method.
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// Returns the request URI.
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// Returns request headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Reads the next request body frame.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, NetworkError> {
        loop {
            let Some(frame) = self.body.frame().await else {
                return Ok(None);
            };
            let frame = frame.map_err(|_| NetworkError::Io)?;
            let Ok(data) = frame.into_data() else {
                continue;
            };
            self.bytes_read = self
                .bytes_read
                .checked_add(data.len())
                .ok_or(NetworkError::BodyTooLarge)?;
            if self.bytes_read > self.max_body {
                return Err(NetworkError::BodyTooLarge);
            }
            return Ok(Some(data));
        }
    }

    /// Consumes the bounded request body.
    pub async fn bytes(mut self) -> Result<Bytes, NetworkError> {
        let mut body = Vec::with_capacity(self.bytes_read.min(self.max_body));
        while let Some(chunk) = self.next_chunk().await? {
            body.extend_from_slice(&chunk);
        }
        Ok(Bytes::from(body))
    }
}

/// A sender for a streaming [`HttpServerResponse`] body.
#[derive(Debug, Clone)]
pub struct BodySender {
    sender: mpsc::Sender<Bytes>,
}

impl BodySender {
    /// Sends one body chunk, waiting for bounded channel capacity.
    pub async fn send(&self, chunk: impl Into<Bytes>) -> Result<(), NetworkError> {
        self.sender
            .send(chunk.into())
            .await
            .map_err(|_| NetworkError::Canceled)
    }
}

enum ResponseBody {
    Bytes(Bytes),
    Stream(mpsc::Receiver<Bytes>),
}

/// A response returned by a router handler.
pub struct HttpServerResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: ResponseBody,
}

impl std::fmt::Debug for HttpServerResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpServerResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

impl HttpServerResponse {
    /// Creates a buffered response.
    pub fn new(status: StatusCode, body: impl Into<Bytes>) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            body: ResponseBody::Bytes(body.into()),
        }
    }

    /// Creates a successful response.
    pub fn ok(body: impl Into<Bytes>) -> Self {
        Self::new(StatusCode::OK, body)
    }

    /// Creates a 404 response.
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, Bytes::from_static(b"not found"))
    }

    /// Creates a 500 response.
    pub fn internal_error() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(b"internal server error"),
        )
    }

    /// Creates a bounded streaming response and its producer.
    pub fn streaming(status: StatusCode) -> (Self, BodySender) {
        let (sender, receiver) = mpsc::channel(8);
        (
            Self {
                status,
                headers: HeaderMap::new(),
                body: ResponseBody::Stream(receiver),
            },
            BodySender { sender },
        )
    }

    /// Adds a typed response header.
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Adds a response header parsed from text.
    pub fn with_header_text(self, name: &str, value: &str) -> Result<Self, NetworkError> {
        let name = name.parse().map_err(|_| NetworkError::Header)?;
        let value = value.parse().map_err(|_| NetworkError::Header)?;
        Ok(self.with_header(name, value))
    }

    fn into_hyper(self) -> http::Response<BoxResponseBody> {
        let mut response = http::Response::new(match self.body {
            ResponseBody::Bytes(body) => Full::new(body).boxed(),
            ResponseBody::Stream(receiver) => {
                let chunks = stream::unfold(receiver, |mut receiver| async {
                    receiver
                        .recv()
                        .await
                        .map(|chunk| (Ok::<_, Infallible>(Frame::data(chunk)), receiver))
                });
                BodyExt::boxed(StreamBody::new(chunks))
            }
        });
        *response.status_mut() = self.status;
        *response.headers_mut() = self.headers;
        response
    }
}

#[derive(Clone)]
struct Route {
    method: Method,
    path: String,
    handler: Handler,
}

/// An exact-method-and-path HTTP router.
#[derive(Clone, Default)]
pub struct HttpRouter {
    routes: Arc<Vec<Route>>,
    fallback: Option<Handler>,
}

impl std::fmt::Debug for HttpRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpRouter")
            .field("route_count", &self.routes.len())
            .field("has_fallback", &self.fallback.is_some())
            .finish()
    }
}

impl HttpRouter {
    /// Adds an asynchronous exact route.
    pub fn route<F, Fut>(&mut self, method: Method, path: impl Into<String>, handler: F)
    where
        F: Fn(HttpServerRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<HttpServerResponse, NetworkError>> + Send + 'static,
    {
        Arc::make_mut(&mut self.routes).push(Route {
            method,
            path: path.into(),
            handler: Arc::new(move |request| Box::pin(handler(request))),
        });
    }

    /// Sets the handler used when no exact route matches.
    pub fn fallback<F, Fut>(&mut self, handler: F)
    where
        F: Fn(HttpServerRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<HttpServerResponse, NetworkError>> + Send + 'static,
    {
        self.fallback = Some(Arc::new(move |request| Box::pin(handler(request))));
    }

    async fn dispatch(&self, request: HttpServerRequest) -> HttpServerResponse {
        let handler = self
            .routes
            .iter()
            .find(|route| route.method == request.method && route.path == request.uri.path())
            .map(|route| Arc::clone(&route.handler))
            .or_else(|| self.fallback.clone());
        match handler {
            Some(handler) => handler(request).await.unwrap_or_else(|error| match error {
                NetworkError::BodyTooLarge => HttpServerResponse::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Bytes::from_static(b"request body too large"),
                ),
                _ => HttpServerResponse::internal_error(),
            }),
            None => HttpServerResponse::not_found(),
        }
    }
}

/// A concurrent HTTP/1.1 or HTTP/2 router server.
pub struct HttpServer {
    listener: TcpListener,
    version: HttpVersion,
    router: HttpRouter,
    max_request_body: usize,
}

impl std::fmt::Debug for HttpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpServer")
            .field("version", &self.version)
            .field("local_addr", &self.listener.local_addr())
            .field("router", &self.router)
            .finish_non_exhaustive()
    }
}

impl HttpServer {
    /// Binds a server on `address`.
    pub async fn bind(
        address: SocketAddr,
        version: HttpVersion,
        router: HttpRouter,
    ) -> Result<Self, NetworkError> {
        if version == HttpVersion::Http3 {
            return Err(NetworkError::Unsupported);
        }
        let listener = TcpListener::bind(address).await?;
        Ok(Self {
            listener,
            version,
            router,
            max_request_body: 16 * 1024 * 1024,
        })
    }

    /// Changes the maximum request body accepted by handlers.
    pub fn with_max_request_body(mut self, max: usize) -> Result<Self, NetworkError> {
        if max == 0 {
            return Err(NetworkError::InvalidConfig);
        }
        self.max_request_body = max;
        Ok(self)
    }

    /// Returns the bound address.
    pub fn local_addr(&self) -> Result<SocketAddr, NetworkError> {
        self.listener.local_addr().map_err(NetworkError::from)
    }

    /// Accepts connections until `token` is cancelled.
    pub async fn run(self, token: CancellationToken) -> Result<(), NetworkError> {
        let listener = self.listener;
        let router = Arc::new(self.router);
        let version = self.version;
        let max_request_body = self.max_request_body;
        loop {
            tokio::select! {
                _ = token.cancelled() => return Ok(()),
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    let router = Arc::clone(&router);
                    let token = token.clone();
                    tokio::spawn(async move {
                        let service = service_fn(move |request: http::Request<Incoming>| {
                            let router = Arc::clone(&router);
                            async move {
                                let request = HttpServerRequest::new(request, max_request_body);
                                Ok::<_, Infallible>(router.dispatch(request).await.into_hyper())
                            }
                        });
                        match version {
                            HttpVersion::Http1 => {
                                let connection = hyper::server::conn::http1::Builder::new()
                                    .serve_connection(TokioIo::new(stream), service);
                                tokio::select! {
                                    _ = token.cancelled() => {}
                                    _ = connection => {}
                                }
                            }
                            HttpVersion::Http2 => {
                                let connection = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                                    .serve_connection(TokioIo::new(stream), service);
                                tokio::select! {
                                    _ = token.cancelled() => {}
                                    _ = connection => {}
                                }
                            }
                            HttpVersion::Http3 => {}
                        }
                    });
                }
            }
        }
    }
}

/// Resolves a host and service through the platform resolver.
#[derive(Debug, Default, Clone, Copy)]
pub struct DnsResolver;

impl DnsResolver {
    /// Resolves all addresses currently returned for `host:port`.
    pub async fn lookup(host: &str, port: u16) -> Result<Vec<SocketAddr>, NetworkError> {
        let addresses: Vec<_> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| NetworkError::Dns)?
            .collect();
        if addresses.is_empty() {
            return Err(NetworkError::Dns);
        }
        Ok(addresses)
    }

    /// Resolves a host while observing caller cancellation.
    pub async fn lookup_with_cancellation(
        host: &str,
        port: u16,
        token: &CancellationToken,
    ) -> Result<Vec<SocketAddr>, NetworkError> {
        tokio::select! {
            _ = token.cancelled() => Err(NetworkError::Canceled),
            result = Self::lookup(host, port) => result,
        }
    }
}

/// A Tokio UDP socket with typed send and receive errors.
pub struct AsyncUdpSocket {
    socket: UdpSocket,
}

impl std::fmt::Debug for AsyncUdpSocket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AsyncUdpSocket")
            .field("local_addr", &self.socket.local_addr())
            .finish()
    }
}

impl AsyncUdpSocket {
    /// Binds a UDP socket.
    pub async fn bind(address: SocketAddr) -> Result<Self, NetworkError> {
        Ok(Self {
            socket: UdpSocket::bind(address).await?,
        })
    }

    /// Returns the local socket address.
    pub fn local_addr(&self) -> Result<SocketAddr, NetworkError> {
        self.socket.local_addr().map_err(NetworkError::from)
    }

    /// Sends one datagram.
    pub async fn send_to(&self, payload: &[u8], target: SocketAddr) -> Result<usize, NetworkError> {
        self.socket
            .send_to(payload, target)
            .await
            .map_err(NetworkError::from)
    }

    /// Receives one datagram.
    pub async fn recv_from(&self, buffer: &mut [u8]) -> Result<(usize, SocketAddr), NetworkError> {
        self.socket
            .recv_from(buffer)
            .await
            .map_err(NetworkError::from)
    }

    /// Receives one datagram while observing caller cancellation.
    pub async fn recv_from_with_cancellation(
        &self,
        buffer: &mut [u8],
        token: &CancellationToken,
    ) -> Result<(usize, SocketAddr), NetworkError> {
        tokio::select! {
            _ = token.cancelled() => Err(NetworkError::Canceled),
            result = self.socket.recv_from(buffer) => result.map_err(NetworkError::from),
        }
    }

    /// Receives one datagram before the supplied deadline.
    pub async fn recv_from_with_timeout(
        &self,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<(usize, SocketAddr), NetworkError> {
        tokio::time::timeout(timeout, self.socket.recv_from(buffer))
            .await
            .map_err(|_| NetworkError::Timeout)?
            .map_err(NetworkError::from)
    }
}

/// WebSocket configuration for timeouts and reconnect attempts.
#[derive(Debug, Clone)]
pub struct WebSocketConfig {
    /// Maximum time for one send or receive operation.
    pub operation_timeout: Option<Duration>,
    /// Number of reconnect attempts after the initial connection fails.
    pub reconnect_attempts: usize,
    /// Delay between reconnect attempts.
    pub reconnect_delay: Duration,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            operation_timeout: Some(Duration::from_secs(30)),
            reconnect_attempts: 2,
            reconnect_delay: Duration::from_millis(100),
        }
    }
}

/// A WebSocket frame surfaced without exposing Tungstenite's message type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketMessage {
    /// A UTF-8 text frame.
    Text(String),
    /// A binary frame.
    Binary(Bytes),
    /// A ping frame.
    Ping(Bytes),
    /// A pong frame.
    Pong(Bytes),
    /// A close frame or a cleanly closed stream.
    Close,
}

impl From<Message> for WebSocketMessage {
    fn from(message: Message) -> Self {
        match message {
            Message::Text(text) => Self::Text(text.to_string()),
            Message::Binary(data) => Self::Binary(data),
            Message::Ping(data) => Self::Ping(data),
            Message::Pong(data) => Self::Pong(data),
            Message::Close(_) | Message::Frame(_) => Self::Close,
        }
    }
}

fn into_tungstenite(message: WebSocketMessage) -> Message {
    match message {
        WebSocketMessage::Text(text) => Message::Text(text.into()),
        WebSocketMessage::Binary(data) => Message::Binary(data),
        WebSocketMessage::Ping(data) => Message::Ping(data),
        WebSocketMessage::Pong(data) => Message::Pong(data),
        WebSocketMessage::Close => Message::Close(None),
    }
}

type WebSocketTransport = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A reconnecting WebSocket client session.
pub struct WebSocketClient {
    url: String,
    config: WebSocketConfig,
    socket: WebSocketTransport,
}

impl std::fmt::Debug for WebSocketClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebSocketClient")
            .field("url", &self.url)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl WebSocketClient {
    /// Connects, retrying according to `config`.
    pub async fn connect(
        url: impl Into<String>,
        config: WebSocketConfig,
    ) -> Result<Self, NetworkError> {
        let url = url.into();
        let mut attempts = 0;
        loop {
            let connect = connect_async(&url);
            let result = if let Some(timeout) = config.operation_timeout {
                match tokio::time::timeout(timeout, connect).await {
                    Ok(Ok(pair)) => Ok(pair),
                    Ok(Err(_)) => Err(NetworkError::Connect),
                    Err(_) => Err(NetworkError::Timeout),
                }
            } else {
                connect.await.map_err(|_| NetworkError::Connect)
            };
            match result {
                Ok((socket, _)) => {
                    return Ok(Self {
                        url,
                        config,
                        socket,
                    });
                }
                Err(_) if attempts < config.reconnect_attempts => {
                    attempts += 1;
                    tokio::time::sleep(config.reconnect_delay).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn send_inner(&mut self, message: Message) -> Result<(), NetworkError> {
        use futures_util::SinkExt;
        let send = self.socket.send(message);
        if let Some(timeout) = self.config.operation_timeout {
            tokio::time::timeout(timeout, send)
                .await
                .map_err(|_| NetworkError::Timeout)?
                .map_err(|_| NetworkError::Io)
        } else {
            send.await.map_err(|_| NetworkError::Io)
        }
    }

    /// Sends a frame.
    pub async fn send(&mut self, message: WebSocketMessage) -> Result<(), NetworkError> {
        self.send_inner(into_tungstenite(message)).await
    }

    /// Receives the next frame.
    pub async fn next(&mut self) -> Result<WebSocketMessage, NetworkError> {
        use futures_util::StreamExt;
        let next = self.socket.next();
        let message = if let Some(timeout) = self.config.operation_timeout {
            tokio::time::timeout(timeout, next)
                .await
                .map_err(|_| NetworkError::Timeout)?
        } else {
            next.await
        };
        message
            .ok_or(NetworkError::Canceled)?
            .map(WebSocketMessage::from)
            .map_err(|_| NetworkError::Io)
    }

    /// Sends a ping frame.
    pub async fn ping(&mut self, payload: impl Into<Bytes>) -> Result<(), NetworkError> {
        self.send(WebSocketMessage::Ping(payload.into())).await
    }

    /// Closes the session.
    pub async fn close(&mut self) -> Result<(), NetworkError> {
        self.send(WebSocketMessage::Close).await
    }
}

/// A server-side WebSocket session.
pub struct WebSocketSession {
    socket: WebSocketStream<TcpStream>,
    config: WebSocketConfig,
}

impl std::fmt::Debug for WebSocketSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebSocketSession")
            .finish_non_exhaustive()
    }
}

impl WebSocketSession {
    /// Accepts a WebSocket handshake from one TCP stream.
    pub async fn accept(stream: TcpStream) -> Result<Self, NetworkError> {
        Self::accept_with_config(stream, WebSocketConfig::default()).await
    }

    /// Accepts a handshake with per-operation timeout settings.
    pub async fn accept_with_config(
        stream: TcpStream,
        config: WebSocketConfig,
    ) -> Result<Self, NetworkError> {
        let socket = accept_async(stream)
            .await
            .map_err(|_| NetworkError::Protocol)?;
        Ok(Self { socket, config })
    }

    /// Sends a frame.
    pub async fn send(&mut self, message: WebSocketMessage) -> Result<(), NetworkError> {
        use futures_util::SinkExt;
        let send = self.socket.send(into_tungstenite(message));
        if let Some(timeout) = self.config.operation_timeout {
            tokio::time::timeout(timeout, send)
                .await
                .map_err(|_| NetworkError::Timeout)?
                .map_err(|_| NetworkError::Io)
        } else {
            send.await.map_err(|_| NetworkError::Io)
        }
    }

    /// Receives the next frame.
    pub async fn next(&mut self) -> Result<WebSocketMessage, NetworkError> {
        use futures_util::StreamExt;
        let next = self.socket.next();
        let message = if let Some(timeout) = self.config.operation_timeout {
            tokio::time::timeout(timeout, next)
                .await
                .map_err(|_| NetworkError::Timeout)?
        } else {
            next.await
        };
        message
            .ok_or(NetworkError::Canceled)?
            .map(WebSocketMessage::from)
            .map_err(|_| NetworkError::Io)
    }

    /// Closes the session.
    pub async fn close(&mut self) -> Result<(), NetworkError> {
        self.send(WebSocketMessage::Close).await
    }
}

/// Binds a TCP listener for a WebSocket server.
pub async fn bind_websocket_listener(address: SocketAddr) -> Result<TcpListener, NetworkError> {
    TcpListener::bind(address).await.map_err(NetworkError::from)
}

/// A WebSocket listener that applies one configuration to accepted sessions.
pub struct WebSocketListener {
    listener: TcpListener,
    config: WebSocketConfig,
}

impl std::fmt::Debug for WebSocketListener {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebSocketListener")
            .field("local_addr", &self.listener.local_addr())
            .field("config", &self.config)
            .finish()
    }
}

impl WebSocketListener {
    /// Binds a listener with the supplied session configuration.
    pub async fn bind(address: SocketAddr, config: WebSocketConfig) -> Result<Self, NetworkError> {
        Ok(Self {
            listener: TcpListener::bind(address).await?,
            config,
        })
    }

    /// Returns the listener's bound address.
    pub fn local_addr(&self) -> Result<SocketAddr, NetworkError> {
        self.listener.local_addr().map_err(NetworkError::from)
    }

    /// Accepts and handshakes the next WebSocket session.
    pub async fn accept(&self) -> Result<WebSocketSession, NetworkError> {
        let (stream, _) = self.listener.accept().await?;
        WebSocketSession::accept_with_config(stream, self.config.clone()).await
    }
}

/// Binds a configured WebSocket listener.
pub async fn bind_websocket(
    address: SocketAddr,
    config: WebSocketConfig,
) -> Result<WebSocketListener, NetworkError> {
    WebSocketListener::bind(address, config).await
}

/// Returns a loopback address suitable for examples and tests.
pub fn loopback(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn start_server(
        version: HttpVersion,
        router: HttpRouter,
    ) -> (
        SocketAddr,
        CancellationToken,
        tokio::task::JoinHandle<Result<(), NetworkError>>,
    ) {
        let server = HttpServer::bind(loopback(0), version, router)
            .await
            .expect("bind test server");
        let address = server.local_addr().expect("server address");
        let token = CancellationToken::new();
        let server_token = token.clone();
        let task = tokio::spawn(server.run(server_token));
        (address, token, task)
    }

    async fn stop_server(
        token: CancellationToken,
        task: tokio::task::JoinHandle<Result<(), NetworkError>>,
    ) {
        token.cancel();
        task.await
            .expect("server task join")
            .expect("server result");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn http1_routes_streams_and_handles_concurrent_requests() {
        let mut router = HttpRouter::default();
        router.route(Method::POST, "/echo", |request| async move {
            let body = request.bytes().await?;
            HttpServerResponse::ok(body).with_header_text("x-kira", "echo")
        });
        let (address, token, task) = start_server(HttpVersion::Http1, router).await;
        let client = HttpClient::new(HttpClientConfig::default()).expect("client");
        let first = HttpRequest::new(Method::POST, &format!("http://{address}/echo"))
            .expect("request")
            .with_body(Bytes::from_static(b"first"));
        let second = HttpRequest::new(Method::POST, &format!("http://{address}/echo"))
            .expect("request")
            .with_body(Bytes::from_static(b"second"));
        let (first, second) = tokio::join!(client.request(first), client.request(second));
        let first = first.expect("first response");
        let second = second.expect("second response");
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(first.headers().get("x-kira").expect("header"), "echo");
        assert_eq!(
            first.bytes().await.expect("first body"),
            Bytes::from_static(b"first")
        );
        assert_eq!(
            second.bytes().await.expect("second body"),
            Bytes::from_static(b"second")
        );
        stop_server(token, task).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn streaming_request_body_uses_async_backpressure() {
        let mut router = HttpRouter::default();
        router.route(Method::POST, "/echo", |request| async move {
            HttpServerResponse::ok(request.bytes().await?).with_header_text("x-kira", "streamed")
        });
        let (address, token, task) = start_server(HttpVersion::Http1, router).await;
        let client = HttpClient::new(HttpClientConfig::default()).expect("client");
        let request =
            HttpRequest::new(Method::POST, &format!("http://{address}/echo")).expect("request");
        let chunks = stream::iter([Bytes::from_static(b"first-"), Bytes::from_static(b"second")]);
        let response = client
            .request_streaming(request, chunks)
            .await
            .expect("streaming response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-kira").expect("header"),
            "streamed"
        );
        assert_eq!(
            response.bytes().await.expect("body"),
            b"first-second".as_slice()
        );
        stop_server(token, task).await;

        let mut router = HttpRouter::default();
        router.route(Method::POST, "/echo", |request| async move {
            Ok(HttpServerResponse::ok(request.bytes().await?))
        });
        let (address, token, task) = start_server(HttpVersion::Http2, router).await;
        let client = HttpClient::new(HttpClientConfig {
            version: HttpVersion::Http2,
            ..HttpClientConfig::default()
        })
        .expect("HTTP/2 client");
        let request =
            HttpRequest::new(Method::POST, &format!("http://{address}/echo")).expect("request");
        let response = client
            .request_streaming(request, stream::iter([Bytes::from_static(b"h2-stream")]))
            .await
            .expect("HTTP/2 streaming response");
        assert_eq!(
            response.bytes().await.expect("HTTP/2 body"),
            Bytes::from_static(b"h2-stream")
        );
        stop_server(token, task).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn websocket_listener_supports_binary_ping_and_close() {
        let listener = WebSocketListener::bind(loopback(0), WebSocketConfig::default())
            .await
            .expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            let mut session = listener.accept().await?;
            let message = session.next().await?;
            assert_eq!(
                message,
                WebSocketMessage::Binary(Bytes::from_static(b"binary"))
            );
            session
                .send(WebSocketMessage::Binary(Bytes::from_static(b"echo")))
                .await?;
            let message = session.next().await?;
            let WebSocketMessage::Ping(payload) = message else {
                return Err(NetworkError::Protocol);
            };
            session.send(WebSocketMessage::Pong(payload)).await?;
            if session.next().await? != WebSocketMessage::Close {
                return Err(NetworkError::Protocol);
            }
            Ok::<(), NetworkError>(())
        });
        let mut client = WebSocketClient::connect(
            format!("ws://{address}/"),
            WebSocketConfig {
                operation_timeout: Some(Duration::from_secs(2)),
                ..WebSocketConfig::default()
            },
        )
        .await
        .expect("client");
        client
            .send(WebSocketMessage::Binary(Bytes::from_static(b"binary")))
            .await
            .expect("binary send");
        assert_eq!(
            client.next().await.expect("binary echo"),
            WebSocketMessage::Binary(Bytes::from_static(b"echo"))
        );
        client
            .ping(Bytes::from_static(b"ping"))
            .await
            .expect("ping");
        assert_eq!(
            client.next().await.expect("pong"),
            WebSocketMessage::Pong(Bytes::from_static(b"ping"))
        );
        client.close().await.expect("close");
        server_task
            .await
            .expect("server task")
            .expect("server result");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn http2_routes_and_streams_response_body() {
        let mut router = HttpRouter::default();
        router.route(Method::GET, "/stream", |_request| async move {
            let (response, sender) = HttpServerResponse::streaming(StatusCode::OK);
            tokio::spawn(async move {
                let _ = sender.send(Bytes::from_static(b"one")).await;
                let _ = sender.send(Bytes::from_static(b"two")).await;
            });
            Ok(response)
        });
        let (address, token, task) = start_server(HttpVersion::Http2, router).await;
        let client = HttpClient::new(HttpClientConfig {
            version: HttpVersion::Http2,
            ..HttpClientConfig::default()
        })
        .expect("client");
        let mut response = client
            .get(&format!("http://{address}/stream"))
            .await
            .expect("response");
        let mut body = Vec::new();
        while let Some(chunk) = response.next_chunk().await.expect("chunk") {
            body.extend_from_slice(&chunk);
        }
        assert_eq!(body, b"onetwo");
        stop_server(token, task).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancellation_and_deadlines_abort_slow_requests() {
        let mut router = HttpRouter::default();
        router.route(Method::GET, "/slow", |_request| async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(HttpServerResponse::ok(Bytes::from_static(b"late")))
        });
        let (address, token, task) = start_server(HttpVersion::Http1, router).await;
        let client = HttpClient::new(HttpClientConfig {
            request_timeout: Some(Duration::from_secs(10)),
            ..HttpClientConfig::default()
        })
        .expect("client");
        let cancellation = CancellationToken::new();
        let request = HttpRequest::get(&format!("http://{address}/slow")).expect("request");
        cancellation.cancel();
        assert_eq!(
            client
                .request_with_cancellation(request, &cancellation)
                .await
                .expect_err("cancelled request"),
            NetworkError::Canceled
        );
        stop_server(token, task).await;

        let mut timeout_router = HttpRouter::default();
        timeout_router.route(Method::GET, "/slow", |_request| async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(HttpServerResponse::ok(Bytes::from_static(b"late")))
        });
        let (address, token, task) = start_server(HttpVersion::Http1, timeout_router).await;
        let client = HttpClient::new(HttpClientConfig {
            request_timeout: Some(Duration::from_millis(10)),
            ..HttpClientConfig::default()
        })
        .expect("client");
        assert_eq!(
            client
                .get(&format!("http://{address}/slow"))
                .await
                .expect_err("timed out request"),
            NetworkError::Timeout
        );
        stop_server(token, task).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn udp_roundtrip_and_dns_resolver_work() {
        let receiver = AsyncUdpSocket::bind(loopback(0)).await.expect("receiver");
        let sender = AsyncUdpSocket::bind(loopback(0)).await.expect("sender");
        let target = receiver.local_addr().expect("receiver address");
        sender
            .send_to(b"datagram", target)
            .await
            .expect("send datagram");
        let mut buffer = [0; 32];
        let (length, source) = receiver
            .recv_from(&mut buffer)
            .await
            .expect("receive datagram");
        assert_eq!(&buffer[..length], b"datagram");
        assert_eq!(source, sender.local_addr().expect("sender address"));
        assert!(
            !DnsResolver::lookup("localhost", 80)
                .await
                .expect("localhost resolution")
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn malformed_configuration_and_body_limits_are_reported() {
        assert!(matches!(
            HttpRequest::get("http://[invalid"),
            Err(NetworkError::InvalidUri)
        ));
        assert!(matches!(
            HttpClient::new(HttpClientConfig {
                max_response_body: 0,
                ..HttpClientConfig::default()
            }),
            Err(NetworkError::InvalidConfig)
        ));
        assert!(matches!(
            HttpClient::new(HttpClientConfig {
                version: HttpVersion::Http3,
                ..HttpClientConfig::default()
            }),
            Err(NetworkError::Unsupported)
        ));
        assert!(matches!(
            HttpServer::bind(loopback(0), HttpVersion::Http3, HttpRouter::default()).await,
            Err(NetworkError::Unsupported)
        ));

        let mut router = HttpRouter::default();
        router.route(Method::POST, "/limited", |request| async move {
            Ok(HttpServerResponse::ok(request.bytes().await?))
        });
        let server = HttpServer::bind(loopback(0), HttpVersion::Http1, router)
            .await
            .expect("limited server")
            .with_max_request_body(3)
            .expect("body limit");
        let address = server.local_addr().expect("address");
        let token = CancellationToken::new();
        let task = tokio::spawn(server.run(token.clone()));
        let client = HttpClient::new(HttpClientConfig::default()).expect("client");
        let response = client
            .request(
                HttpRequest::new(Method::POST, &format!("http://{address}/limited"))
                    .expect("request")
                    .with_body(Bytes::from_static(b"too large")),
            )
            .await
            .expect("body-limit response");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        token.cancel();
        task.await.expect("server task").expect("server result");

        let socket = AsyncUdpSocket::bind(loopback(0)).await.expect("UDP");
        let mut buffer = [0; 8];
        assert_eq!(
            socket
                .recv_from_with_timeout(&mut buffer, Duration::from_millis(10))
                .await
                .expect_err("UDP timeout"),
            NetworkError::Timeout
        );
        let canceled = CancellationToken::new();
        canceled.cancel();
        assert_eq!(
            socket
                .recv_from_with_cancellation(&mut buffer, &canceled)
                .await
                .expect_err("UDP cancellation"),
            NetworkError::Canceled
        );
    }
}
