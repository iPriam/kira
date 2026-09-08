//! Reusable HTTP/3 client and server primitives.
//!
//! HTTP/3 is intentionally kept separate from the pooled TCP client: QUIC
//! connections have certificate and endpoint configuration that should be
//! explicit at the call site. The API still uses the same buffered request,
//! bounded streaming response, cancellation, and router conventions as the
//! HTTP/1.1 and HTTP/2 layer.
//!
//! The two halves are split the way they are used: a program that dials an
//! HTTP/3 service reads [`client`], one that answers reads [`server`], and the
//! imports they share sit here so neither file repeats them.

use std::convert::TryFrom;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, Bytes};
use h3::error::StreamError;
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

mod client;
mod server;

pub use client::*;
pub use server::*;

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

    /// A server that answers without reading the request body still answers.
    ///
    /// Dropping the request body reader terminates the receiving side of the
    /// request stream, so the client's remaining writes and its finish fail.
    /// That is the peer saying it has what it needs, not a failed request.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_server_that_answers_without_reading_the_body_still_answers() {
        let server_config = Http3ServerConfig::self_signed_localhost().expect("certificate");
        let client_config = Http3ClientConfig::new("localhost")
            .with_root_certificate(server_config.certificate_der.clone());
        let mut router = Http3Router::default();
        // The body is never read: the handler answers from the request line
        // alone, which is what a refusal or a redirect does.
        router.route(Method::POST, "/ignored", |_request| async move {
            Ok(Http3ServerResponse::ok(Bytes::from_static(b"answered")))
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
        // Large enough that the answer arrives while the body is still going.
        let body = Bytes::from(vec![b'x'; 8 * 1024 * 1024]);
        let request = HttpRequest::new(Method::POST, &format!("https://{address}/ignored"))
            .expect("request")
            .with_body(body);
        let response = client.request(request).await.expect("response");
        assert_eq!(
            response.bytes().await.expect("body"),
            Bytes::from_static(b"answered")
        );
        token.cancel();
        server_task
            .await
            .expect("server task")
            .expect("server result");
    }
}
