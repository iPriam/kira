//! Runs the reusable Rust API for every supported async transport.

use bytes::Bytes;
use http::{Method, StatusCode};
use kira_network::{
    AsyncUdpSocket, CancellationToken, DnsResolver, Http3Client, Http3ClientConfig, Http3Router,
    Http3Server, Http3ServerConfig, Http3ServerResponse, HttpClient, HttpClientConfig, HttpRequest,
    HttpRouter, HttpServer, HttpServerResponse, HttpVersion, NetworkError, WebSocketClient,
    WebSocketConfig, WebSocketListener, WebSocketMessage, loopback,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("async protocol example failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), NetworkError> {
    http_pair(HttpVersion::Http1).await?;
    http_pair(HttpVersion::Http2).await?;
    http3_pair().await?;
    websocket_pair().await?;
    udp_and_dns().await?;
    raw_async_io().await?;
    println!("HTTP/1.1, HTTP/2, HTTP/3, WebSocket, UDP/DNS, and raw I/O: ok");
    Ok(())
}

async fn http_pair(version: HttpVersion) -> Result<(), NetworkError> {
    let mut router = HttpRouter::default();
    router.route(Method::GET, "/", |_request| async move {
        Ok(HttpServerResponse::ok(Bytes::from_static(b"http")))
    });
    let server = HttpServer::bind(loopback(0), version, router).await?;
    let address = server.local_addr()?;
    let token = CancellationToken::new();
    let server_task = tokio::spawn(server.run(token.clone()));
    let client = HttpClient::new(HttpClientConfig {
        version,
        ..HttpClientConfig::default()
    })?;
    let response = client
        .get(&format!("http://{address}/"))
        .await
        .map_err(|_| NetworkError::Protocol)?;
    if response.status() != StatusCode::OK || response.bytes().await? != Bytes::from_static(b"http")
    {
        return Err(NetworkError::Protocol);
    }
    token.cancel();
    server_task.await.map_err(|_| NetworkError::Protocol)??;
    Ok(())
}

async fn http3_pair() -> Result<(), NetworkError> {
    let server_config = Http3ServerConfig::self_signed_localhost()?;
    let client_config = Http3ClientConfig::new("localhost")
        .with_root_certificate(server_config.certificate_der.clone());
    let mut router = Http3Router::default();
    router.route(Method::GET, "/", |_request| async move {
        Ok(Http3ServerResponse::ok(Bytes::from_static(b"http3")))
    });
    let server = Http3Server::bind(loopback(0), server_config, router).await?;
    let address = server.local_addr()?;
    let token = CancellationToken::new();
    let server_task = tokio::spawn(server.run(token.clone()));
    let client = Http3Client::connect(address, client_config).await?;
    let response = client
        .request(HttpRequest::get(&format!("https://{address}/"))?)
        .await?;
    if response.status() != StatusCode::OK
        || response.bytes().await? != Bytes::from_static(b"http3")
    {
        return Err(NetworkError::Protocol);
    }
    token.cancel();
    server_task.await.map_err(|_| NetworkError::Protocol)??;
    Ok(())
}

async fn websocket_pair() -> Result<(), NetworkError> {
    let listener = WebSocketListener::bind(loopback(0), WebSocketConfig::default()).await?;
    let address = listener.local_addr()?;
    let server_task = tokio::spawn(async move {
        let mut session = listener.accept().await?;
        if session.next().await? != WebSocketMessage::Text("hello".to_owned()) {
            return Err(NetworkError::Protocol);
        }
        session
            .send(WebSocketMessage::Text("world".to_owned()))
            .await?;
        if session.next().await? != WebSocketMessage::Close {
            return Err(NetworkError::Protocol);
        }
        Ok::<(), NetworkError>(())
    });
    let mut client =
        WebSocketClient::connect(format!("ws://{address}/"), WebSocketConfig::default()).await?;
    client
        .send(WebSocketMessage::Text("hello".to_owned()))
        .await?;
    if client.next().await? != WebSocketMessage::Text("world".to_owned()) {
        return Err(NetworkError::Protocol);
    }
    client.close().await?;
    server_task.await.map_err(|_| NetworkError::Protocol)??;
    Ok(())
}

async fn udp_and_dns() -> Result<(), NetworkError> {
    let receiver = AsyncUdpSocket::bind(loopback(0)).await?;
    let sender = AsyncUdpSocket::bind(loopback(0)).await?;
    let target = receiver.local_addr()?;
    sender.send_to(b"udp", target).await?;
    let mut buffer = [0; 16];
    let (length, _) = receiver.recv_from(&mut buffer).await?;
    if &buffer[..length] != b"udp" {
        return Err(NetworkError::Protocol);
    }
    if DnsResolver::lookup("localhost", 80).await?.is_empty() {
        return Err(NetworkError::Dns);
    }
    Ok(())
}

async fn raw_async_io() -> Result<(), NetworkError> {
    let listener = TcpListener::bind(loopback(0)).await?;
    let address = listener.local_addr()?;
    let server_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut payload = [0; 4];
        stream.read_exact(&mut payload).await?;
        if payload != *b"echo" {
            return Err(NetworkError::Protocol);
        }
        stream.write_all(&payload).await?;
        Ok::<(), NetworkError>(())
    });
    let mut client = TcpStream::connect(address).await?;
    client.write_all(b"echo").await?;
    let mut echoed = [0; 4];
    client.read_exact(&mut echoed).await?;
    if echoed != *b"echo" {
        return Err(NetworkError::Protocol);
    }
    server_task.await.map_err(|_| NetworkError::Protocol)??;
    Ok(())
}
