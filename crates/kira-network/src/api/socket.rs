//! The address-and-socket surface: DNS resolution, UDP, and WebSocket sessions.
//!
//! Grouped because each one hands back a live endpoint rather than a completed
//! exchange, so they share the cancellation and timeout shapes the HTTP halves
//! do not.

use super::*;

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
