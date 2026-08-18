//! The pooled HTTP client: its configuration, its request and response types,
//! and the client itself.
//!
//! Split from the server half rather than sharing a file with it: the two meet
//! only at the type aliases in the parent, and a reader chasing a request's
//! lifetime should not have to skip a router to follow it.

use super::*;

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

        let (method, uri, headers, buffered) = request.into_parts();
        // This path connects with a plain `TcpStream` and speaks cleartext. An
        // `https` URI would otherwise fall through the `unwrap_or(80)` below and
        // be sent unencrypted: the caller asked for TLS and would get none, with
        // nothing in the result to say so.
        if uri.scheme_str() != Some("http") {
            return Err(NetworkError::Unsupported);
        }
        // A request built with `with_body` and then sent through the streaming
        // path had its buffered bytes silently dropped. Carrying two bodies is a
        // configuration mistake rather than a question of which one wins.
        if !buffered.is_empty() {
            return Err(NetworkError::InvalidConfig);
        }
        let authority = uri.authority().ok_or(NetworkError::InvalidUri)?;
        let host = authority.host();
        let port = authority.port_u16().unwrap_or(80);
        let authority_text = authority.as_str().to_owned();
        let stream = TcpStream::connect((host, port))
            .await
            .map_err(|_| NetworkError::Connect)?;
        let body_stream = chunks
            .map(|chunk| Ok::<_, Infallible>(Frame::data(chunk)))
            .boxed();
        let body = StreamBody::new(body_stream);
        let mut message = http::Request::new(body);
        *message.method_mut() = method;
        *message.headers_mut() = headers;
        // `hyper::client::conn` is the low-level API: unlike the pooled client
        // it adds no `Host` header and does not rewrite the target. HTTP/1.1
        // requires both — a `Host` header and an origin-form request target —
        // and a compliant server answers 400 to a request missing them.
        // HTTP/2 carries the authority in `:authority` and wants the URI whole,
        // so it keeps what the caller wrote.
        if matches!(self.config.version, HttpVersion::Http1) {
            if !message.headers().contains_key(http::header::HOST) {
                let value =
                    HeaderValue::from_str(&authority_text).map_err(|_| NetworkError::Header)?;
                message.headers_mut().insert(http::header::HOST, value);
            }
            let target = uri
                .path_and_query()
                .map(|part| part.as_str())
                .unwrap_or("/");
            *message.uri_mut() = target.parse().map_err(|_| NetworkError::InvalidUri)?;
        } else {
            *message.uri_mut() = uri;
        }
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
