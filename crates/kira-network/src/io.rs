//! Raw asynchronous TCP I/O used to prove the transport seam independently of
//! HTTP framing.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::runtime::NetworkError;

const PAYLOAD: &[u8] = b"kira-async-io";

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

/// Accepts one connection, echoes the payload, and verifies the client read it.
pub(crate) async fn roundtrip(
    listener: std::net::TcpListener,
    port: u16,
) -> Result<i64, NetworkError> {
    let listener = TcpListener::from_std(listener)?;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut payload = vec![0; PAYLOAD.len()];
        stream.read_exact(&mut payload).await?;
        if payload != PAYLOAD {
            return Err(NetworkError::Protocol);
        }
        stream.write_all(PAYLOAD).await?;
        Ok::<(), NetworkError>(())
    });
    let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
    client.write_all(PAYLOAD).await?;
    let mut echoed = vec![0; PAYLOAD.len()];
    client.read_exact(&mut echoed).await?;
    if echoed != PAYLOAD {
        return Err(NetworkError::Protocol);
    }
    server.await.map_err(|_| NetworkError::Protocol)??;
    Ok(1)
}
