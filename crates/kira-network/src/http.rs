//! HTTP/1.1, HTTP/2, and HTTP/3 loopback protocol operations.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, Bytes};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::client::conn::{http1, http2};
use hyper::server::conn::{http1 as server_http1, http2 as server_http2};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use quinn::{ClientConfig, Endpoint, EndpointConfig, ServerConfig};
use tokio::net::{TcpListener, TcpStream};

use crate::runtime::NetworkError;

const BODY: &[u8] = b"kira-network";

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

async fn respond(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let status = if request.method() == http::Method::GET && request.uri().path() == "/" {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    let mut response = Response::new(Full::new(Bytes::copy_from_slice(BODY)));
    *response.status_mut() = status;
    Ok(response)
}

/// Serves one HTTP/1.1 request and checks that the peer consumed the response.
pub(crate) async fn serve_http1(listener: std::net::TcpListener) -> Result<i64, NetworkError> {
    let listener = TcpListener::from_std(listener)?;
    let (stream, _) = listener.accept().await?;
    server_http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service_fn(respond))
        .await
        .map_err(|_| NetworkError::Protocol)?;
    Ok(1)
}

/// Sends one HTTP/1.1 request and verifies its status and body.
pub(crate) async fn client_http1(port: u16) -> Result<i64, NetworkError> {
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|_| NetworkError::Protocol)?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("http://localhost/")
        .body(Empty::<Bytes>::new())
        .map_err(|_| NetworkError::Protocol)?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|_| NetworkError::Protocol)?;
    verify_response(response).await
}

/// Serves one cleartext HTTP/2 request.
pub(crate) async fn serve_http2(listener: std::net::TcpListener) -> Result<i64, NetworkError> {
    let listener = TcpListener::from_std(listener)?;
    let (stream, _) = listener.accept().await?;
    server_http2::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), service_fn(respond))
        .await
        .map_err(|_| NetworkError::Protocol)?;
    Ok(1)
}

/// Sends one cleartext HTTP/2 request and verifies its status and body.
pub(crate) async fn client_http2(port: u16) -> Result<i64, NetworkError> {
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
    let (mut sender, connection) = http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
        .await
        .map_err(|_| NetworkError::Protocol)?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let request = Request::builder()
        .method(http::Method::GET)
        .uri("http://localhost/")
        .body(Empty::<Bytes>::new())
        .map_err(|_| NetworkError::Protocol)?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|_| NetworkError::Protocol)?;
    verify_response(response).await
}

async fn verify_response(response: Response<Incoming>) -> Result<i64, NetworkError> {
    if response.status() != StatusCode::OK {
        return Err(NetworkError::Protocol);
    }
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|_| NetworkError::Protocol)?
        .to_bytes();
    if body.as_ref() == BODY {
        Ok(1)
    } else {
        Err(NetworkError::Protocol)
    }
}

/// Binds an HTTP/3 endpoint with a short-lived self-signed localhost certificate.
pub(crate) fn bind_http3() -> Result<(std::net::UdpSocket, u16, Vec<u8>, ServerConfig), NetworkError>
{
    use quinn::crypto::rustls::QuicServerConfig;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .map_err(|_| NetworkError::Protocol)?;
    let certificate = generated.cert.der().to_vec();
    let key = generated.key_pair.serialize_der();
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|_| NetworkError::Protocol)?
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(certificate.clone())],
        PrivateKeyDer::try_from(key).map_err(|_| NetworkError::Protocol)?,
    )
    .map_err(|_| NetworkError::Protocol)?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let config = ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(tls).map_err(|_| NetworkError::Protocol)?,
    ));
    let socket = std::net::UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .map_err(|_| NetworkError::Bind)?;
    socket
        .set_nonblocking(true)
        .map_err(|_| NetworkError::Bind)?;
    let port = socket.local_addr().map_err(|_| NetworkError::Bind)?.port();
    Ok((socket, port, certificate, config))
}

/// Serves two concurrent HTTP/3 request streams and returns a response body.
pub(crate) async fn serve_http3(
    socket: std::net::UdpSocket,
    config: ServerConfig,
) -> Result<i64, NetworkError> {
    let endpoint = Endpoint::new(
        EndpointConfig::default(),
        Some(config),
        socket,
        Arc::new(quinn::TokioRuntime),
    )
    .map_err(|_| NetworkError::Bind)?;
    let incoming = endpoint.accept().await.ok_or(NetworkError::Connect)?;
    let connection = incoming.await.map_err(|_| NetworkError::Protocol)?;
    let mut h3_connection = h3::server::Connection::new(h3_quinn::Connection::new(connection))
        .await
        .map_err(|_| NetworkError::Protocol)?;
    for _ in 0..2 {
        let resolver = h3_connection
            .accept()
            .await
            .map_err(|_| NetworkError::Protocol)?
            .ok_or(NetworkError::Protocol)?;
        let (request, mut stream) = resolver
            .resolve_request()
            .await
            .map_err(|_| NetworkError::Protocol)?;
        if request.method() != http::Method::GET || !matches!(request.uri().path(), "/" | "/second")
        {
            return Err(NetworkError::Protocol);
        }
        stream
            .send_response(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(())
                    .map_err(|_| NetworkError::Protocol)?,
            )
            .await
            .map_err(|_| NetworkError::Protocol)?;
        stream
            .send_data(Bytes::copy_from_slice(BODY))
            .await
            .map_err(|_| NetworkError::Protocol)?;
        stream.finish().await.map_err(|_| NetworkError::Protocol)?;
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), h3_connection.accept()).await;
    drop(endpoint);
    Ok(1)
}

/// Sends one HTTP/3 request and verifies its response.
pub(crate) async fn client_http3(port: u16, certificate: Arc<[u8]>) -> Result<i64, NetworkError> {
    use quinn::crypto::rustls::QuicClientConfig;
    use rustls::pki_types::CertificateDer;

    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(certificate.to_vec()))
        .map_err(|_| NetworkError::Protocol)?;
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|_| NetworkError::Protocol)?
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let config = ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(tls).map_err(|_| NetworkError::Protocol)?,
    ));
    let mut endpoint = Endpoint::client(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))?;
    endpoint.set_default_client_config(config);
    let connection = endpoint
        .connect(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            "localhost",
        )
        .map_err(|_| NetworkError::Connect)?
        .await
        .map_err(|_| NetworkError::Connect)?;
    let (mut driver, send_request) = h3::client::new(h3_quinn::Connection::new(connection))
        .await
        .map_err(|_| NetworkError::Protocol)?;
    let driver_task = tokio::spawn(async move {
        let _ = std::future::poll_fn(|context| driver.poll_close(context)).await;
    });
    let first = send_http3_request(send_request.clone(), "/");
    let second = send_http3_request(send_request, "/second");
    let (first, second) = tokio::join!(first, second);
    let result = first.and(second).map(|_| 1);
    driver_task.abort();
    endpoint.close(0u32.into(), b"done");
    result
}

async fn send_http3_request(
    mut sender: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    path: &str,
) -> Result<i64, NetworkError> {
    let request = Request::builder()
        .method(http::Method::GET)
        .uri(format!("https://localhost{path}"))
        .body(())
        .map_err(|_| NetworkError::Protocol)?;
    let mut stream = sender
        .send_request(request)
        .await
        .map_err(|_| NetworkError::Protocol)?;
    stream.finish().await.map_err(|_| NetworkError::Protocol)?;
    let response = stream
        .recv_response()
        .await
        .map_err(|_| NetworkError::Protocol)?;
    if response.status() != StatusCode::OK {
        return Err(NetworkError::Protocol);
    }
    let mut body = Vec::new();
    while let Some(chunk) = stream
        .recv_data()
        .await
        .map_err(|_| NetworkError::Protocol)?
    {
        let mut chunk = chunk;
        body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
    }
    if body == BODY {
        Ok(1)
    } else {
        Err(NetworkError::Protocol)
    }
}
