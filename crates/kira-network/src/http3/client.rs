//! Dialing an HTTP/3 service: its trust configuration, its multiplexed
//! connection, and the streaming response it reads back.

use super::*;

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
    /// When the request this answers stops being allowed to run.
    ///
    /// Absolute rather than a duration, because it is the *request's* deadline
    /// and the headers have already spent part of it. Reading the body is the
    /// rest of the same operation, so it gets what remains rather than a fresh
    /// copy of the limit.
    deadline: Option<tokio::time::Instant>,
    /// The caller's cancellation, observed by every body read.
    token: CancellationToken,
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
    fn new(
        response: http::Response<()>,
        body: ClientStream,
        max_body: usize,
        deadline: Option<tokio::time::Instant>,
        token: CancellationToken,
    ) -> Self {
        let (parts, _) = response.into_parts();
        Self {
            deadline,
            token,
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
        // The wait a stalled peer would otherwise own. `send_request` ends when
        // the response headers arrive, so without these a server could answer
        // and then simply stop sending body frames, leaving the caller pending
        // for as long as it kept the QUIC connection open.
        let received = {
            let receiving = self.body.recv_data();
            match self.deadline {
                Some(deadline) => tokio::select! {
                    _ = self.token.cancelled() => return Err(NetworkError::Canceled),
                    result = tokio::time::timeout_at(deadline, receiving) => {
                        result.map_err(|_| NetworkError::Timeout)?
                    }
                },
                None => tokio::select! {
                    _ = self.token.cancelled() => return Err(NetworkError::Canceled),
                    result = receiving => result,
                },
            }
        };
        let Some(mut chunk) = received.map_err(|_| NetworkError::Protocol)? else {
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
        // Taken once, here, so the headers and the body share it. Measured from
        // before the send rather than from when the response arrived.
        let deadline = self
            .config
            .operation_timeout
            .map(|timeout| tokio::time::Instant::now() + timeout);
        let future = self.send_request(request, deadline, token.clone());
        match deadline {
            Some(deadline) => tokio::select! {
                _ = token.cancelled() => Err(NetworkError::Canceled),
                result = tokio::time::timeout_at(deadline, future) => {
                    result.map_err(|_| NetworkError::Timeout)?
                }
            },
            None => tokio::select! {
                _ = token.cancelled() => Err(NetworkError::Canceled),
                result = future => result,
            },
        }
    }

    async fn send_request(
        &self,
        request: HttpRequest,
        deadline: Option<tokio::time::Instant>,
        token: CancellationToken,
    ) -> Result<Http3Response, NetworkError> {
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
        // A server may answer without reading the request body — it needs
        // nothing from it, or it is refusing what is being offered. Doing so
        // terminates the receiving side of the request stream, and every write
        // left to make and the finish itself then fail. That is the peer saying
        // it has what it needs rather than a failed request: the answer is
        // still on its way, so stop sending and go and read it. Any other
        // failure is a transport failure and is reported as one.
        //
        // Left as a failure this is a race, because whether the answer beats
        // the last write decides it: a request the server never reads the body
        // of succeeds or fails depending on scheduling.
        let mut stopped = false;
        if !body.is_empty() {
            match stream.send_data(body).await {
                Ok(()) => {}
                Err(StreamError::RemoteTerminate { .. }) => stopped = true,
                Err(_) => return Err(NetworkError::Io),
            }
        }
        if !stopped {
            match stream.finish().await {
                Ok(()) | Err(StreamError::RemoteTerminate { .. }) => {}
                Err(_) => return Err(NetworkError::Io),
            }
        }
        let response = stream
            .recv_response()
            .await
            .map_err(|_| NetworkError::Protocol)?;
        Ok(Http3Response::new(
            response,
            stream,
            self.config.max_response_body,
            deadline,
            token,
        ))
    }
}

impl Drop for Http3Client {
    fn drop(&mut self) {
        self.endpoint.close(0u32.into(), b"client dropped");
        self.driver.abort();
    }
}
