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
mod http3;
mod io;
mod request;
mod runtime;
mod websocket;

pub use api::{
    AsyncUdpSocket, BodySender, CancellationToken, DnsResolver, HttpClient, HttpClientConfig,
    HttpRequest, HttpResponse, HttpRouter, HttpServer, HttpServerRequest, HttpServerResponse,
    HttpVersion, WebSocketClient, WebSocketConfig, WebSocketListener, WebSocketMessage,
    WebSocketSession, bind_websocket, bind_websocket_listener, loopback,
};
pub use http3::{
    Http3Client, Http3ClientConfig, Http3Response, Http3Router, Http3Server, Http3ServerConfig,
    Http3ServerRequest, Http3ServerResponse,
};
pub use request::END_OF_SELECTION;
pub use runtime::{NetworkError, OperationId, PollState};

use std::ffi::{CStr, c_char};

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

/// Starts the HTTPS loopback server and returns its operation handle.
///
/// It serves until it is cancelled rather than completing, because a server has
/// no one exchange to finish on: a caller starts it, reads its port, sends what
/// it wants through it, and cancels the handle.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_https_server() -> i64 {
    runtime::start_https_server().map_or_else(runtime::error_code, OperationId::as_i64)
}

/// Reads a NUL-terminated C argument as UTF-8 text.
///
/// # Safety
///
/// `pointer` must be null or address a NUL-terminated string that stays valid
/// for the duration of the call.
unsafe fn borrowed<'a>(pointer: *const c_char) -> Result<&'a str, NetworkError> {
    if pointer.is_null() {
        return Err(NetworkError::InvalidConfig);
    }
    // SAFETY: the caller's contract is that `pointer` addresses a
    // NUL-terminated string that stays valid for the length of this call.
    let text = unsafe { CStr::from_ptr(pointer) };
    text.to_str().map_err(|_| NetworkError::Encoding)
}

/// The C result of an operation that answers only whether it worked.
fn completed(result: Result<(), NetworkError>) -> i64 {
    match result {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}

/// Opens a request for `method` and `url`, returning its request handle.
///
/// # Safety
///
/// Both arguments must be NUL-terminated strings valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_network_request_new(
    method: *const c_char,
    url: *const c_char,
) -> i64 {
    // SAFETY: both arguments are the caller's NUL-terminated strings, valid
    // for this call by the contract above.
    let arguments = unsafe { (borrowed(method), borrowed(url)) };
    match arguments {
        (Ok(method), Ok(url)) => request::new(method, url).unwrap_or_else(runtime::error_code),
        (Err(error), _) | (_, Err(error)) => error.code(),
    }
}

/// Adds one header to a request under construction.
///
/// # Safety
///
/// Both arguments must be NUL-terminated strings valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_network_request_header(
    handle: i64,
    name: *const c_char,
    value: *const c_char,
) -> i64 {
    // SAFETY: both arguments are the caller's NUL-terminated strings, valid
    // for this call by the contract above.
    let arguments = unsafe { (borrowed(name), borrowed(value)) };
    completed(match arguments {
        (Ok(name), Ok(value)) => request::add_header(handle, name, value),
        (Err(error), _) | (_, Err(error)) => Err(error),
    })
}

/// Replaces a request's body with the bytes of `text`.
///
/// # Safety
///
/// `text` must be a NUL-terminated string valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_network_request_body_text(handle: i64, text: *const c_char) -> i64 {
    // SAFETY: `text` is the caller's NUL-terminated string, valid for this
    // call by the contract above.
    let text = unsafe { borrowed(text) };
    completed(text.and_then(|text| request::set_body_text(handle, text)))
}

/// Appends one byte to a request's body, for a body that is not text.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_request_body_byte(handle: i64, byte: i32) -> i64 {
    completed(request::push_body_byte(handle, byte))
}

/// Sets the deadline for the whole request in milliseconds; zero removes it.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_request_timeout_ms(handle: i64, milliseconds: i64) -> i64 {
    completed(request::set_timeout_ms(handle, milliseconds))
}

/// Selects the HTTP version: `1` for HTTP/1.1, `2` for HTTP/2.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_request_version(handle: i64, version: i32) -> i64 {
    completed(request::set_version(handle, version))
}

/// Trusts the certificate published by the loopback server bound to `port`.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_request_trust_loopback(handle: i64, port: u16) -> i64 {
    completed(request::trust_loopback(handle, port))
}

/// Sends an assembled request and returns the operation handle for it.
///
/// The request handle is consumed: what a caller polls, results, reads and
/// cancels from here is the operation handle this returns.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_request_send(handle: i64) -> i64 {
    request::send(handle).map_or_else(runtime::error_code, OperationId::as_i64)
}

/// Discards a request that will never be sent. Unknown handles are ignored.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_request_discard(handle: i64) {
    request::discard(handle);
}

/// Returns a completed request's HTTP status code.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_response_status(handle: i64) -> i64 {
    request::status(OperationId::from_i64(handle)).unwrap_or_else(runtime::error_code)
}

/// Selects the response body for reading and returns its length in bytes.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_response_select_body(handle: i64) -> i64 {
    request::select_body(OperationId::from_i64(handle)).unwrap_or_else(runtime::error_code)
}

/// Selects a response header for reading, returning its length or `-1` when the
/// response carries no header of that name.
///
/// # Safety
///
/// `name` must be a NUL-terminated string valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_network_response_select_header(
    handle: i64,
    name: *const c_char,
) -> i64 {
    // SAFETY: `name` is the caller's NUL-terminated string, valid for this
    // call by the contract above.
    let name = unsafe { borrowed(name) };
    name.and_then(|name| request::select_header(OperationId::from_i64(handle), name))
        .unwrap_or_else(runtime::error_code)
}

/// Returns the length in bytes of the current selection.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_response_length(handle: i64) -> i64 {
    request::selection_length(OperationId::from_i64(handle)).unwrap_or_else(runtime::error_code)
}

/// Moves the read cursor back to the start of the current selection.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_response_rewind(handle: i64) -> i64 {
    request::rewind(OperationId::from_i64(handle)).unwrap_or_else(runtime::error_code)
}

/// Reads the next byte of the selection, or `-1` at its end.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_response_read_byte(handle: i64) -> i64 {
    request::read_byte(OperationId::from_i64(handle)).unwrap_or_else(runtime::error_code)
}

/// Reads the next Unicode scalar of the selection, or `-1` at its end.
#[unsafe(no_mangle)]
pub extern "C" fn kira_network_response_read_scalar(handle: i64) -> i64 {
    request::read_scalar(OperationId::from_i64(handle)).unwrap_or_else(runtime::error_code)
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
