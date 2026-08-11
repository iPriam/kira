//! Reusable HTTP/3 client and server primitives.
//!
//! HTTP/3 is intentionally kept separate from the pooled TCP client: QUIC
//! connections have certificate and endpoint configuration that should be
//! explicit at the call site. The API still uses the same buffered request,
//! bounded streaming response, cancellation, and router conventions as the
//! HTTP/1.1 and HTTP/2 layer.

use std::convert::TryFrom;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, Bytes};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use quinn::{ClientConfig, Endpoint, EndpointConfig, ServerConfig};
use tokio::task::JoinHandle;

use crate::api::{CancellationToken, HttpRequest};
use crate::runtime::NetworkError;

type ClientStream = h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;
type ServerSendStream = h3::server::RequestStream<h3_quinn::SendStream<Bytes>, Bytes>;
type ServerRecvStream = h3::server::RequestStream<h3_quinn::RecvStream, Bytes>;
type HandlerFuture =
    Pin<Box<dyn Future<Output = Result<Http3ServerResponse, NetworkError>> + Send>>;
type Handler = Arc<dyn Fn(Http3ServerRequest) -> HandlerFuture + Send + Sync>;

/// TLS material and ALPN settings for an HTTP/3 server.
#[derive(Debug, Clone)]
pub struct Http3ServerConfig {
    /// DER-encoded certificate chain, starting with the leaf certificate.
    pub certificate_der: Vec<u8>,
    /// DER-encoded private key matching the leaf certificate.
    pub private_key_der: Vec<u8>,
    /// ALPN protocols advertised during the QUIC handshake.
    pub alpn_protocols: Vec<Vec<u8>>,
}

impl Http3ServerConfig {
    /// Creates a server configuration with the standard HTTP/3 ALPN.
    pub fn new(certificate_der: Vec<u8>, private_key_der: Vec<u8>) -> Result<Self, NetworkError> {
        if certificate_der.is_empty() || private_key_der.is_empty() {
            return Err(NetworkError::InvalidConfig);
        }
        Ok(Self {
            certificate_der,
            private_key_der,
            alpn_protocols: vec![b"h3".to_vec()],
        })
    }

    /// Generates a short-lived localhost certificate for tests and examples.
    pub fn self_signed_localhost() -> Result<Self, NetworkError> {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
            .map_err(|_| NetworkError::Protocol)?;
        Self::new(
            generated.cert.der().to_vec(),
            generated.key_pair.serialize_der(),
        )
    }

    fn to_quinn_config(&self) -> Result<ServerConfig, NetworkError> {
        use quinn::crypto::rustls::QuicServerConfig;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};

        if self.alpn_protocols.is_empty() {
            return Err(NetworkError::InvalidConfig);
        }
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| NetworkError::Protocol)?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(self.certificate_der.clone())],
            PrivateKeyDer::try_from(self.private_key_der.clone())
                .map_err(|_| NetworkError::Protocol)?,
        )
        .map_err(|_| NetworkError::Protocol)?;
        tls.alpn_protocols = self.alpn_protocols.clone();
        Ok(ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(tls).map_err(|_| NetworkError::Protocol)?,
        )))
    }
}

/// Root certificates and connection policy for an HTTP/3 client.
#[derive(Debug, Clone)]
pub struct Http3ClientConfig {
    /// DNS name used for certificate verification and the QUIC SNI.
    pub server_name: String,
    /// DER-encoded trust anchors. At least one is required.
    pub root_certificates: Vec<Vec<u8>>,
    /// Deadline for connecting or completing one request.
    pub operation_timeout: Option<Duration>,
    /// Maximum response body accepted by [`Http3Response::bytes`].
    pub max_response_body: usize,
    /// ALPN protocols offered during the QUIC handshake.
    pub alpn_protocols: Vec<Vec<u8>>,
}

impl Http3ClientConfig {
    /// Creates a client configuration for a server name.
    pub fn new(server_name: impl Into<String>) -> Self {
        Self {
            server_name: server_name.into(),
            root_certificates: Vec::new(),
            operation_timeout: Some(Duration::from_secs(30)),
            max_response_body: 16 * 1024 * 1024,
            alpn_protocols: vec![b"h3".to_vec()],
        }
    }

    /// Adds one DER-encoded trust anchor.
    pub fn with_root_certificate(mut self, certificate_der: impl Into<Vec<u8>>) -> Self {
        self.root_certificates.push(certificate_der.into());
        self
    }

    fn to_quinn_config(&self) -> Result<ClientConfig, NetworkError> {
        use quinn::crypto::rustls::QuicClientConfig;
        use rustls::pki_types::CertificateDer;

        if self.server_name.is_empty()
            || self.root_certificates.is_empty()
            || self.alpn_protocols.is_empty()
            || self.max_response_body == 0
        {
            return Err(NetworkError::InvalidConfig);
        }
        let mut roots = rustls::RootCertStore::empty();
        for certificate in &self.root_certificates {
            roots
                .add(CertificateDer::from(certificate.clone()))
                .map_err(|_| NetworkError::Protocol)?;
        }
        let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| NetworkError::Protocol)?
        .with_root_certificates(roots)
        .with_no_client_auth();
        tls.alpn_protocols = self.alpn_protocols.clone();
        Ok(ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(tls).map_err(|_| NetworkError::Protocol)?,
        )))
    }
}

/// A streaming HTTP/3 response.
pub struct Http3Response {
    status: StatusCode,
    headers: HeaderMap,
    body: ClientStream,
    bytes_read: usize,
    max_body: usize,
}

impl std::fmt::Debug for Http3Response {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Http3Response")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("bytes_read", &self.bytes_read)
            .field("max_body", &self.max_body)
            .finish_non_exhaustive()
    }
}

impl Http3Response {
    fn new(response: http::Response<()>, body: ClientStream, max_body: usize) -> Self {
        let (parts, _) = response.into_parts();
        Self {
            status: parts.status,
            headers: parts.headers,
            body,
            bytes_read: 0,
            max_body,
        }
    }

    /// Returns the response status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Reads the next response data frame.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, NetworkError> {
        let Some(mut chunk) = self
            .body
            .recv_data()
            .await
            .map_err(|_| NetworkError::Protocol)?
        else {
            return Ok(None);
        };
        let chunk = chunk.copy_to_bytes(chunk.remaining());
        self.bytes_read = self
            .bytes_read
            .checked_add(chunk.len())
            .ok_or(NetworkError::BodyTooLarge)?;
        if self.bytes_read > self.max_body {
            return Err(NetworkError::BodyTooLarge);
        }
        Ok(Some(chunk))
    }

    /// Consumes the response into one bounded buffer.
    pub async fn bytes(mut self) -> Result<Bytes, NetworkError> {
        let mut body = Vec::with_capacity(self.bytes_read.min(self.max_body));
        while let Some(chunk) = self.next_chunk().await? {
            body.extend_from_slice(&chunk);
        }
        Ok(Bytes::from(body))
    }
}

/// A reusable HTTP/3 client over one multiplexed QUIC connection.
pub struct Http3Client {
    endpoint: Endpoint,
    sender: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    driver: JoinHandle<()>,
    config: Http3ClientConfig,
}

impl std::fmt::Debug for Http3Client {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Http3Client")
            .field("local_addr", &self.endpoint.local_addr())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Http3Client {
    /// Connects to an HTTP/3 endpoint with explicit certificate roots.
    pub async fn connect(
        address: SocketAddr,
        config: Http3ClientConfig,
    ) -> Result<Self, NetworkError> {
        let mut endpoint =
            Endpoint::client(SocketAddr::new(address.ip(), 0)).map_err(|_| NetworkError::Bind)?;
        endpoint.set_default_client_config(config.to_quinn_config()?);
        let connecting = endpoint
            .connect(address, &config.server_name)
            .map_err(|_| NetworkError::Connect)?;
        let connection = if let Some(timeout) = config.operation_timeout {
            tokio::time::timeout(timeout, connecting)
                .await
                .map_err(|_| NetworkError::Timeout)?
                .map_err(|_| NetworkError::Connect)?
        } else {
            connecting.await.map_err(|_| NetworkError::Connect)?
        };
        let (mut driver, sender) = h3::client::new(h3_quinn::Connection::new(connection))
            .await
            .map_err(|_| NetworkError::Protocol)?;
        let driver_task = tokio::spawn(async move {
            let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
        });
        Ok(Self {
            endpoint,
            sender,
            driver: driver_task,
            config,
        })
    }

    /// Returns the local UDP endpoint address.
    pub fn local_addr(&self) -> Result<SocketAddr, NetworkError> {
        self.endpoint.local_addr().map_err(NetworkError::from)
    }

    /// Sends one request over the shared HTTP/3 connection.
    pub async fn request(&self, request: HttpRequest) -> Result<Http3Response, NetworkError> {
        let token = CancellationToken::new();
        self.request_with_cancellation(request, &token).await
    }

    /// Sends a request while observing caller cancellation and the configured deadline.
    pub async fn request_with_cancellation(
        &self,
        request: HttpRequest,
        token: &CancellationToken,
    ) -> Result<Http3Response, NetworkError> {
        let future = self.send_request(request);
        if let Some(timeout) = self.config.operation_timeout {
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

    async fn send_request(&self, request: HttpRequest) -> Result<Http3Response, NetworkError> {
        let (method, uri, headers, body) = request.into_parts();
        let mut message = http::Request::builder()
            .method(method)
            .uri(uri)
            .version(http::Version::HTTP_3)
            .body(())
            .map_err(|_| NetworkError::InvalidUri)?;
        *message.headers_mut() = headers;
        let mut stream = self
            .sender
            .clone()
            .send_request(message)
            .await
            .map_err(|_| NetworkError::Protocol)?;
        if !body.is_empty() {
            stream.send_data(body).await.map_err(|_| NetworkError::Io)?;
        }
        stream.finish().await.map_err(|_| NetworkError::Io)?;
        let response = stream
            .recv_response()
            .await
            .map_err(|_| NetworkError::Protocol)?;
        Ok(Http3Response::new(
            response,
            stream,
            self.config.max_response_body,
        ))
    }
}

impl Drop for Http3Client {
    fn drop(&mut self) {
        self.endpoint.close(0u32.into(), b"client dropped");
        self.driver.abort();
    }
}

/// An incoming HTTP/3 request with a streaming body.
pub struct Http3ServerRequest {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: ServerRecvStream,
    bytes_read: usize,
    max_body: usize,
}

impl Http3ServerRequest {
    fn new(request: http::Request<()>, body: ServerRecvStream, max_body: usize) -> Self {
        let (parts, _) = request.into_parts();
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

    /// Reads one request body frame.
    pub async fn next_chunk(&mut self) -> Result<Option<Bytes>, NetworkError> {
        let Some(mut chunk) = self
            .body
            .recv_data()
            .await
            .map_err(|_| NetworkError::Protocol)?
        else {
            return Ok(None);
        };
        let chunk = chunk.copy_to_bytes(chunk.remaining());
        self.bytes_read = self
            .bytes_read
            .checked_add(chunk.len())
            .ok_or(NetworkError::BodyTooLarge)?;
        if self.bytes_read > self.max_body {
            return Err(NetworkError::BodyTooLarge);
        }
        Ok(Some(chunk))
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

#[derive(Clone)]
struct Route {
    method: Method,
    path: String,
    handler: Handler,
}

/// An exact-method-and-path HTTP/3 router.
#[derive(Clone, Default)]
pub struct Http3Router {
    routes: Arc<Vec<Route>>,
    fallback: Option<Handler>,
}

impl std::fmt::Debug for Http3Router {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Http3Router")
            .field("route_count", &self.routes.len())
            .field("has_fallback", &self.fallback.is_some())
            .finish()
    }
}

impl Http3Router {
    /// Adds an asynchronous exact route.
    pub fn route<F, Fut>(&mut self, method: Method, path: impl Into<String>, handler: F)
    where
        F: Fn(Http3ServerRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Http3ServerResponse, NetworkError>> + Send + 'static,
    {
        Arc::make_mut(&mut self.routes).push(Route {
            method,
            path: path.into(),
            handler: Arc::new(move |request| Box::pin(handler(request))),
        });
    }

    /// Sets a fallback handler for unmatched routes.
    pub fn fallback<F, Fut>(&mut self, handler: F)
    where
        F: Fn(Http3ServerRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Http3ServerResponse, NetworkError>> + Send + 'static,
    {
        self.fallback = Some(Arc::new(move |request| Box::pin(handler(request))));
    }

    async fn dispatch(&self, request: Http3ServerRequest) -> Http3ServerResponse {
        let handler = self
            .routes
            .iter()
            .find(|route| route.method == request.method && route.path == request.uri.path())
            .map(|route| Arc::clone(&route.handler))
            .or_else(|| self.fallback.clone());
        match handler {
            Some(handler) => handler(request).await.unwrap_or_else(|error| match error {
                NetworkError::BodyTooLarge => Http3ServerResponse::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Bytes::from_static(b"request body too large"),
                ),
                _ => Http3ServerResponse::internal_error(),
            }),
            None => Http3ServerResponse::not_found(),
        }
    }
}

/// A buffered or chunked HTTP/3 server response.
pub struct Http3ServerResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<Bytes>,
}

impl std::fmt::Debug for Http3ServerResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Http3ServerResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("chunk_count", &self.body.len())
            .finish()
    }
}

impl Http3ServerResponse {
    /// Creates a response with one body chunk.
    pub fn new(status: StatusCode, body: impl Into<Bytes>) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            body: vec![body.into()],
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

    /// Creates a response from multiple body chunks.
    pub fn streaming<I>(status: StatusCode, chunks: I) -> Self
    where
        I: IntoIterator<Item = Bytes>,
    {
        Self {
            status,
            headers: HeaderMap::new(),
            body: chunks.into_iter().collect(),
        }
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

    async fn send(self, mut stream: ServerSendStream) -> Result<(), NetworkError> {
        let mut response = http::Response::builder()
            .status(self.status)
            .version(http::Version::HTTP_3)
            .body(())
            .map_err(|_| NetworkError::Protocol)?;
        *response.headers_mut() = self.headers;
        stream
            .send_response(response)
            .await
            .map_err(|_| NetworkError::Protocol)?;
        for chunk in self.body {
            stream
                .send_data(chunk)
                .await
                .map_err(|_| NetworkError::Io)?;
        }
        stream.finish().await.map_err(|_| NetworkError::Io)
    }
}

/// A concurrent HTTP/3 router server.
pub struct Http3Server {
    endpoint: Endpoint,
    router: Http3Router,
    max_request_body: usize,
}

impl std::fmt::Debug for Http3Server {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Http3Server")
            .field("local_addr", &self.endpoint.local_addr())
            .field("router", &self.router)
            .finish_non_exhaustive()
    }
}

impl Http3Server {
    /// Binds a QUIC endpoint with explicit certificate configuration.
    pub async fn bind(
        address: SocketAddr,
        config: Http3ServerConfig,
        router: Http3Router,
    ) -> Result<Self, NetworkError> {
        let socket = std::net::UdpSocket::bind(address).map_err(|_| NetworkError::Bind)?;
        socket
            .set_nonblocking(true)
            .map_err(|_| NetworkError::Bind)?;
        let endpoint = Endpoint::new(
            EndpointConfig::default(),
            Some(config.to_quinn_config()?),
            socket,
            Arc::new(quinn::TokioRuntime),
        )
        .map_err(|_| NetworkError::Bind)?;
        Ok(Self {
            endpoint,
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

    /// Returns the bound UDP address.
    pub fn local_addr(&self) -> Result<SocketAddr, NetworkError> {
        self.endpoint.local_addr().map_err(NetworkError::from)
    }

    /// Accepts QUIC connections until cancelled.
    pub async fn run(self, token: CancellationToken) -> Result<(), NetworkError> {
        let endpoint = self.endpoint;
        let router = Arc::new(self.router);
        let max_request_body = self.max_request_body;
        loop {
            let incoming = tokio::select! {
                _ = token.cancelled() => {
                    endpoint.close(0u32.into(), b"server cancelled");
                    return Ok(())
                }
                incoming = endpoint.accept() => incoming.ok_or(NetworkError::Connect)?,
            };
            let router = Arc::clone(&router);
            let token = token.clone();
            tokio::spawn(async move {
                let connection = tokio::select! {
                    _ = token.cancelled() => return,
                    connection = incoming => match connection {
                        Ok(connection) => connection,
                        Err(_) => return,
                    },
                };
                let mut h3_connection = match h3::server::Connection::new(
                    h3_quinn::Connection::new(connection),
                )
                .await
                {
                    Ok(connection) => connection,
                    Err(_) => return,
                };
                loop {
                    let resolver = tokio::select! {
                        _ = token.cancelled() => return,
                        resolver = h3_connection.accept() => match resolver {
                            Ok(Some(resolver)) => resolver,
                            Ok(None) | Err(_) => return,
                        },
                    };
                    let router = Arc::clone(&router);
                    tokio::spawn(async move {
                        let Ok((request, stream)) = resolver.resolve_request().await else {
                            return;
                        };
                        let (send_stream, receive_stream) = stream.split();
                        let request =
                            Http3ServerRequest::new(request, receive_stream, max_request_body);
                        let response = router.dispatch(request).await;
                        let _ = response.send(send_stream).await;
                    });
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::*;

    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn http3_client_server_multiplexes_requests() {
        let server_config = Http3ServerConfig::self_signed_localhost().expect("certificate");
        let client_config = Http3ClientConfig::new("localhost")
            .with_root_certificate(server_config.certificate_der.clone());
        let mut router = Http3Router::default();
        router.route(Method::GET, "/one", |_request| async move {
            Ok(Http3ServerResponse::ok(Bytes::from_static(b"one")))
        });
        router.route(Method::POST, "/two", |request| async move {
            let body = request.bytes().await?;
            Ok(Http3ServerResponse::streaming(
                StatusCode::OK,
                [body, Bytes::from_static(b"-reply")],
            ))
        });
        let server = Http3Server::bind(loopback(0), server_config, router)
            .await
            .expect("server");
        let address = server.local_addr().expect("address");
        let token = CancellationToken::new();
        let server_task = tokio::spawn(server.run(token.clone()));
        let client = Http3Client::connect(address, client_config)
            .await
            .expect("client");
        let one = HttpRequest::get(&format!("https://{address}/one")).expect("request");
        let two = HttpRequest::new(Method::POST, &format!("https://{address}/two"))
            .expect("request")
            .with_body(Bytes::from_static(b"request"));
        let (one, two) = tokio::join!(client.request(one), client.request(two));
        assert_eq!(
            one.expect("one").bytes().await.expect("one body"),
            Bytes::from_static(b"one")
        );
        assert_eq!(
            two.expect("two").bytes().await.expect("two body"),
            Bytes::from_static(b"request-reply")
        );
        token.cancel();
        server_task
            .await
            .expect("server task")
            .expect("server result");
    }
}
