//! Async loopback networking used by Kira's end-to-end examples.
//!
//! The crate owns real protocol implementations and a small C-compatible
//! operation surface. The C surface is deliberately nonblocking: starting a
//! server or client schedules a Tokio future and returns an operation handle;
//! callers poll that handle until its result is ready. That shape lets a Kira
//! `async function` keep control of its own scheduler while the protocol work
//! is driven by Tokio on its runtime thread.

mod api;
mod http;
mod http3_api;
mod io;
mod runtime;
mod websocket;

pub use api::{
    AsyncUdpSocket, BodySender, CancellationToken, DnsResolver, HttpClient, HttpClientConfig,
    HttpRequest, HttpResponse, HttpRouter, HttpServer, HttpServerRequest, HttpServerResponse,
    HttpVersion, WebSocketClient, WebSocketConfig, WebSocketListener, WebSocketMessage,
    WebSocketSession, bind_websocket, bind_websocket_listener, loopback,
};
pub use http3_api::{
    Http3Client, Http3ClientConfig, Http3Response, Http3Router, Http3Server, Http3ServerConfig,
    Http3ServerRequest, Http3ServerResponse,
};
pub use runtime::{NetworkError, OperationId, PollState};

/// Starts an HTTP/1.1 loopback server and returns its operation handle.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_http1_server() -> i64 {
    runtime::start_http1_server().map_or_else(runtime::error_code, OperationId::as_i64)
}

/// Starts an HTTP/1.1 loopback client for `port` and returns its operation handle.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_http1_client(port: u16) -> i64 {
    runtime::start_http1_client(port).map_or_else(runtime::error_code, OperationId::as_i64)
}

/// Starts an HTTP/2 loopback server and returns its operation handle.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_http2_server() -> i64 {
    runtime::start_http2_server().map_or_else(runtime::error_code, OperationId::as_i64)
}

/// Starts an HTTP/2 loopback client for `port` and returns its operation handle.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_http2_client(port: u16) -> i64 {
    runtime::start_http2_client(port).map_or_else(runtime::error_code, OperationId::as_i64)
}

/// Starts an HTTP/3 loopback server and returns its operation handle.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_http3_server() -> i64 {
    runtime::start_http3_server().map_or_else(runtime::error_code, OperationId::as_i64)
}

/// Starts an HTTP/3 loopback client for `port` and returns its operation handle.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_http3_client(port: u16) -> i64 {
    runtime::start_http3_client(port).map_or_else(runtime::error_code, OperationId::as_i64)
}

/// Starts a WebSocket loopback server and returns its operation handle.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_websocket_server() -> i64 {
    runtime::start_websocket_server().map_or_else(runtime::error_code, OperationId::as_i64)
}

/// Starts a WebSocket loopback client for `port` and returns its operation handle.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_websocket_client(port: u16) -> i64 {
    runtime::start_websocket_client(port).map_or_else(runtime::error_code, OperationId::as_i64)
}

/// Starts an async TCP echo round trip and returns its operation handle.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_io_roundtrip() -> i64 {
    runtime::start_io_roundtrip().map_or_else(runtime::error_code, OperationId::as_i64)
}

/// Returns the loopback port a server operation bound, or a negative error code.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_server_port(handle: i64) -> i64 {
    runtime::server_port(OperationId::from_i64(handle)).unwrap_or_else(runtime::error_code)
}

/// Returns zero while an operation is pending, one when it succeeded, and a
/// negative value when it failed.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_poll(handle: i64) -> i32 {
    match runtime::poll(OperationId::from_i64(handle)) {
        Ok(PollState::Pending) => 0,
        Ok(PollState::Ready) => 1,
        Ok(PollState::Failed) => -1,
        Err(error) => runtime::error_code(error) as i32,
    }
}

/// Returns the completed value, or a negative error code when it is not ready.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_result(handle: i64) -> i64 {
    runtime::result(OperationId::from_i64(handle)).unwrap_or_else(runtime::error_code)
}

/// Cancels an operation. Unknown handles are ignored because cancellation is
/// an idempotent cleanup operation at the C boundary.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_cancel(handle: i64) {
    runtime::close(OperationId::from_i64(handle));
}

/// Releases an operation handle. This remains an alias for cancellation so
/// existing Kira programs keep their cleanup behavior.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_close(handle: i64) {
    kira_network_cancel(handle);
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        kira_network_cancel, kira_network_close, kira_network_http1_client,
        kira_network_http1_server, kira_network_http2_client, kira_network_http2_server,
        kira_network_http3_client, kira_network_http3_server, kira_network_io_roundtrip,
        kira_network_poll, kira_network_result, kira_network_server_port,
        kira_network_websocket_client, kira_network_websocket_server,
    };

    const TIMEOUT: Duration = Duration::from_secs(10);

    fn wait_for(handle: i64) -> Result<i64, i64> {
        if handle <= 0 {
            return Err(handle);
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let state = kira_network_poll(handle);
            if state == 0 {
                if Instant::now() >= deadline {
                    kira_network_close(handle);
                    return Err(-1);
                }
                std::thread::yield_now();
                continue;
            }
            let result = kira_network_result(handle);
            kira_network_close(handle);
            return if state == 1 && result > 0 {
                Ok(result)
            } else {
                Err(result)
            };
        }
    }

    fn run_pair(
        server: extern "C" fn() -> i64,
        client: extern "C" fn(u16) -> i64,
    ) -> Result<(), i64> {
        let server_handle = server();
        if server_handle <= 0 {
            return Err(server_handle);
        }
        let port = kira_network_server_port(server_handle);
        if port <= 0 || port > i64::from(u16::MAX) {
            kira_network_close(server_handle);
            return Err(port);
        }
        let client_handle = client(port as u16);
        if client_handle <= 0 {
            kira_network_close(server_handle);
            return Err(client_handle);
        }
        wait_for(client_handle)?;
        wait_for(server_handle)?;
        Ok(())
    }

    #[test]
    fn every_async_loopback_protocol_completes() {
        assert_eq!(
            run_pair(kira_network_http1_server, kira_network_http1_client),
            Ok(())
        );
        assert_eq!(
            run_pair(kira_network_http2_server, kira_network_http2_client),
            Ok(())
        );
        assert_eq!(
            run_pair(kira_network_http3_server, kira_network_http3_client),
            Ok(())
        );
        assert_eq!(
            run_pair(kira_network_websocket_server, kira_network_websocket_client),
            Ok(())
        );
        assert_eq!(wait_for(kira_network_io_roundtrip()), Ok(1));
    }

    #[test]
    fn cancellation_removes_the_operation_handle() {
        let handle = kira_network_io_roundtrip();
        assert!(handle > 0);

        kira_network_cancel(handle);

        assert_eq!(kira_network_poll(handle), -101);
        kira_network_close(handle);
    }
}
