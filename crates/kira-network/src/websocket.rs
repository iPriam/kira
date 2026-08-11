//! Async WebSocket loopback operations.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};

use crate::runtime::NetworkError;

const MESSAGE: &str = "kira-websocket";

/// Binds a loopback TCP listener and returns the selected port.
pub(crate) fn bind_tcp() -> Result<(std::net::TcpListener, u16), NetworkError> {
    let listener = std::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .map_err(|_| NetworkError::Bind)?;
    listener
        .set_nonblocking(true)
        .map_err(|_| NetworkError::Bind)?;
    let port = listener
        .local_addr()
        .map_err(|_| NetworkError::Bind)?
        .port();
    Ok((listener, port))
}

/// Accepts one WebSocket and echoes one text message.
pub(crate) async fn serve(listener: std::net::TcpListener) -> Result<i64, NetworkError> {
    let listener = TcpListener::from_std(listener)?;
    let (stream, _) = listener.accept().await?;
    let mut socket = accept_async(stream)
        .await
        .map_err(|_| NetworkError::Protocol)?;
    let message = socket
        .next()
        .await
        .ok_or(NetworkError::Protocol)?
        .map_err(|_| NetworkError::Protocol)?;
    if message != Message::Text(MESSAGE.into()) {
        return Err(NetworkError::Protocol);
    }
    socket
        .send(Message::Text(MESSAGE.into()))
        .await
        .map_err(|_| NetworkError::Protocol)?;
    socket
        .close(None)
        .await
        .map_err(|_| NetworkError::Protocol)?;
    Ok(1)
}

/// Connects to one WebSocket and verifies the echoed text message.
pub(crate) async fn client(port: u16) -> Result<i64, NetworkError> {
    let url = format!("ws://localhost:{port}/");
    let (mut socket, _) = connect_async(url)
        .await
        .map_err(|_| NetworkError::Connect)?;
    socket
        .send(Message::Text(MESSAGE.into()))
        .await
        .map_err(|_| NetworkError::Protocol)?;
    let echoed = socket
        .next()
        .await
        .ok_or(NetworkError::Protocol)?
        .map_err(|_| NetworkError::Protocol)?;
    if echoed != Message::Text(MESSAGE.into()) {
        return Err(NetworkError::Protocol);
    }
    socket
        .close(None)
        .await
        .map_err(|_| NetworkError::Protocol)?;
    Ok(1)
}
