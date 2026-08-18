//! The HTTP server half: accepted requests, streaming response bodies, the
//! router, and the server that drives them.
//!
//! A response here is a body being written, not a body already in hand — which
//! is the difference from the client's [`HttpResponse`](super::HttpResponse) and
//! the reason the two halves are separate modules.

use super::*;

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
