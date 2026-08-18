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

mod client;
mod server;
mod socket;

pub use client::*;
pub use server::*;
pub use socket::*;

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
