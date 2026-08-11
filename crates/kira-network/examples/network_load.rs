//! Exercises pooled HTTP concurrency, streaming request bodies, and cancellation.

use std::time::Duration;

use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt, stream};
use http::{Method, StatusCode};
use kira_network::{
    CancellationToken, HttpClient, HttpClientConfig, HttpRouter, HttpServer, HttpServerResponse,
    HttpVersion, NetworkError, loopback,
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("network load example failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), NetworkError> {
    let mut router = HttpRouter::default();
    router.route(Method::POST, "/echo", |request| async move {
        Ok(HttpServerResponse::ok(request.bytes().await?))
    });
    router.route(Method::GET, "/slow", |_request| async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(HttpServerResponse::ok(Bytes::from_static(b"late")))
    });
    let server = HttpServer::bind(loopback(0), HttpVersion::Http1, router).await?;
    let address = server.local_addr()?;
    let server_token = CancellationToken::new();
    let server_task = tokio::spawn(server.run(server_token.clone()));
    let client = HttpClient::new(HttpClientConfig {
        pool_max_idle_per_host: 16,
        ..HttpClientConfig::default()
    })?;

    let bodies = stream::iter(0..64u8)
        .map(|index| {
            let client = client.clone();
            async move {
                let request = kira_network::HttpRequest::new(
                    Method::POST,
                    &format!("http://{address}/echo"),
                )?;
                let chunks = stream::iter([Bytes::from_static(b"load-"), Bytes::from(vec![index])]);
                let response = client.request_streaming(request, chunks).await?;
                if response.status() != StatusCode::OK {
                    return Err(NetworkError::Protocol);
                }
                response.bytes().await
            }
        })
        .buffer_unordered(8)
        .try_collect::<Vec<_>>();
    let responses = bodies.await?;
    if responses.len() != 64 {
        return Err(NetworkError::Protocol);
    }

    let canceled = CancellationToken::new();
    canceled.cancel();
    let request = kira_network::HttpRequest::get(&format!("http://{address}/slow"))?;
    if !matches!(
        client.request_with_cancellation(request, &canceled).await,
        Err(NetworkError::Canceled)
    ) {
        return Err(NetworkError::Protocol);
    }

    server_token.cancel();
    server_task.await.map_err(|_| NetworkError::Protocol)??;
    println!("pooled streamed requests: 64; cancellation: ok");
    Ok(())
}
