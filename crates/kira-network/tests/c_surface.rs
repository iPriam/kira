//! The C ABI as a C caller reaches it: through the library's exported symbols,
//! with no access to anything private behind them.
//!
//! An integration test rather than a unit test because that is what the surface
//! is for. A test that could see the operation table would be checking the
//! implementation of a boundary whose whole promise is that a caller cannot.

use std::ffi::CString;
use std::time::{Duration, Instant};

use kira_network::{
    END_OF_SELECTION, NetworkError, kira_network_cancel, kira_network_close,
    kira_network_http1_client, kira_network_http1_server, kira_network_http2_client,
    kira_network_http2_server, kira_network_http3_client, kira_network_http3_server,
    kira_network_https_server, kira_network_io_roundtrip, kira_network_poll,
    kira_network_request_body_byte, kira_network_request_body_text, kira_network_request_discard,
    kira_network_request_header, kira_network_request_new, kira_network_request_send,
    kira_network_request_timeout_ms, kira_network_request_trust_loopback,
    kira_network_request_version, kira_network_response_length, kira_network_response_read_byte,
    kira_network_response_read_scalar, kira_network_response_rewind,
    kira_network_response_select_body, kira_network_response_select_header,
    kira_network_response_status, kira_network_result, kira_network_server_port,
    kira_network_websocket_client, kira_network_websocket_server,
};

/// Long enough to survive a full-workspace run, where this test shares a
/// machine with every other test binary. The operation itself is a loopback
/// round trip and takes milliseconds; the deadline exists to fail rather
/// than hang, not to measure anything.
const TIMEOUT: Duration = Duration::from_secs(60);

/// How long the waiter sleeps between polls.
///
/// Sleeping rather than spinning, because the work being waited on runs on
/// this machine's other threads: a `yield_now` loop holds a core against
/// the runtime it is waiting for, which under load is how a round trip that
/// takes milliseconds misses a ten-second deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

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
            std::thread::sleep(POLL_INTERVAL);
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

fn run_pair(server: extern "C" fn() -> i64, client: extern "C" fn(u16) -> i64) -> Result<(), i64> {
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

/// Waits without releasing the handle, for an operation read afterwards.
fn wait_open(handle: i64) -> Result<i64, i64> {
    if handle <= 0 {
        return Err(handle);
    }
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let state = kira_network_poll(handle);
        if state == 0 {
            if Instant::now() >= deadline {
                return Err(-1);
            }
            std::thread::sleep(POLL_INTERVAL);
            continue;
        }
        let result = kira_network_result(handle);
        return if state == 1 { Ok(result) } else { Err(result) };
    }
}

/// Reads the current selection back as text, the way a Kira caller does.
fn selection_text(handle: i64) -> String {
    let mut text = String::new();
    loop {
        let scalar = kira_network_response_read_scalar(handle);
        if scalar == END_OF_SELECTION {
            return text;
        }
        assert!(scalar >= 0, "reading the response failed with {scalar}");
        let scalar = u32::try_from(scalar).expect("a scalar value fits in u32");
        text.push(char::from_u32(scalar).expect("the reader returns scalar values"));
    }
}

fn cstring(text: &str) -> CString {
    CString::new(text).expect("test text contains no NUL")
}

/// Starts the HTTPS server and returns its handle and port.
fn https_server() -> (i64, u16) {
    let handle = kira_network_https_server();
    assert!(handle > 0, "starting the HTTPS server failed with {handle}");
    let port = kira_network_server_port(handle);
    assert!(port > 0 && port <= i64::from(u16::MAX), "port was {port}");
    (handle, port as u16)
}

/// Sends one request to the loopback HTTPS server and returns its handle.
fn send_echo(port: u16, version: i32, trusted: bool) -> i64 {
    let method = cstring("POST");
    let url = cstring(&format!("https://127.0.0.1:{port}/echo"));
    // SAFETY: every pointer below addresses a `CString` that outlives the
    // call it is passed to, which is the whole contract of this surface.
    let request = unsafe { kira_network_request_new(method.as_ptr(), url.as_ptr()) };
    assert!(request > 0, "opening the request failed with {request}");
    assert_eq!(kira_network_request_version(request, version), 0);
    if trusted {
        assert_eq!(kira_network_request_trust_loopback(request, port), 0);
    }
    let name = cstring("x-kira-test");
    let value = cstring("carried");
    // SAFETY: as above, the strings outlive the call.
    let added = unsafe { kira_network_request_header(request, name.as_ptr(), value.as_ptr()) };
    assert_eq!(added, 0);
    let body = cstring("{\"hello\":\"kira\"}");
    // SAFETY: as above, the string outlives the call.
    let carried = unsafe { kira_network_request_body_text(request, body.as_ptr()) };
    assert_eq!(carried, 0);
    kira_network_request_send(request)
}

/// Every part a caller assembles has to arrive, on both TLS versions: the
/// server answers with what it received, so a method, header or body that
/// was dropped on the way fails here instead of returning the same 200.
#[test]
fn a_tls_request_carries_every_part_it_was_given() {
    for version in [1, 2] {
        let (server, port) = https_server();
        let operation = send_echo(port, version, true);
        assert_eq!(wait_open(operation), Ok(200), "HTTP/{version} over TLS");
        assert_eq!(kira_network_response_status(operation), 200);
        assert!(kira_network_response_select_body(operation) > 0);
        let text = selection_text(operation);
        assert!(text.contains("method=POST"), "{text}");
        assert!(text.contains("path=/echo"), "{text}");
        assert!(text.contains("x-kira-test=carried"), "{text}");
        assert!(text.contains(r#"body={"hello":"kira"}"#), "{text}");

        let name = cstring("content-length");
        // SAFETY: as above, the string outlives the call.
        let length = unsafe { kira_network_response_select_header(operation, name.as_ptr()) };
        assert!(length > 0, "the response carried no content-length");
        assert_eq!(
            selection_text(operation).parse::<usize>().ok(),
            Some(text.len())
        );

        kira_network_close(operation);
        kira_network_close(server);
    }
}

/// The certificate is checked. Without the loopback anchor the server's
/// generated certificate has nothing vouching for it, and the request has
/// to fail rather than complete against an unverified peer.
#[test]
fn an_untrusted_certificate_is_refused() {
    let (server, port) = https_server();

    let operation = send_echo(port, 1, false);

    assert!(operation > 0);
    assert!(
        matches!(wait_open(operation), Err(code) if code < 0),
        "an unverified certificate completed the request"
    );
    kira_network_close(operation);
    kira_network_close(server);
}

/// A body that is not text, a deadline that is not the default, and the
/// byte-wise reads with the cursor they share.
#[test]
fn the_request_surface_carries_bytes_deadlines_and_a_shared_cursor() {
    let (server, port) = https_server();
    let method = cstring("PUT");
    let url = cstring(&format!("https://127.0.0.1:{port}/bytes"));
    // SAFETY: both strings outlive the call.
    let request = unsafe { kira_network_request_new(method.as_ptr(), url.as_ptr()) };
    assert!(request > 0);
    assert_eq!(kira_network_request_trust_loopback(request, port), 0);
    assert_eq!(kira_network_request_timeout_ms(request, 30_000), 0);
    for byte in b"Kira" {
        assert_eq!(kira_network_request_body_byte(request, i32::from(*byte)), 0);
    }

    let operation = kira_network_request_send(request);
    assert_eq!(wait_open(operation), Ok(200));

    let length = kira_network_response_select_body(operation);
    assert!(length > 0);
    assert_eq!(
        kira_network_response_length(operation),
        length,
        "the length moved the cursor"
    );
    let first = kira_network_response_read_byte(operation);
    assert!(first >= 0);
    assert_eq!(kira_network_response_rewind(operation), 0);
    assert_eq!(kira_network_response_read_byte(operation), first);
    assert_eq!(kira_network_response_rewind(operation), 0);

    let mut body = Vec::new();
    loop {
        let byte = kira_network_response_read_byte(operation);
        if byte == END_OF_SELECTION {
            break;
        }
        body.push(u8::try_from(byte).expect("a byte value"));
    }
    let text = String::from_utf8(body).expect("the echo is text");
    assert!(text.contains("method=PUT"), "{text}");
    assert!(text.contains("body=Kira"), "{text}");
    assert_eq!(
        i64::try_from(text.len()).expect("a length"),
        length,
        "a rewound cursor reads the whole selection again"
    );

    let absent = cstring("x-absent");
    // SAFETY: the string outlives the call.
    let missing = unsafe { kira_network_response_select_header(operation, absent.as_ptr()) };
    assert_eq!(missing, END_OF_SELECTION);
    assert_eq!(
        kira_network_response_read_byte(operation),
        END_OF_SELECTION,
        "a header the response does not carry left the body selected"
    );

    kira_network_close(operation);
    kira_network_close(server);
}

/// What the surface refuses: a discarded request, a URL it cannot send to,
/// a configuration value with no meaning, and a peer that never answers
/// within the deadline.
#[test]
fn the_request_surface_refuses_what_it_cannot_carry() {
    let method = cstring("GET");
    let unusable = cstring("ftp://127.0.0.1/file");
    // SAFETY: both strings outlive the call.
    let refused = unsafe { kira_network_request_new(method.as_ptr(), unusable.as_ptr()) };
    assert_eq!(refused, NetworkError::InvalidUri.code());

    let url = cstring("http://127.0.0.1:9/discarded");
    // SAFETY: both strings outlive the call.
    let request = unsafe { kira_network_request_new(method.as_ptr(), url.as_ptr()) };
    assert!(request > 0);
    assert_eq!(
        kira_network_request_version(request, 9),
        NetworkError::InvalidConfig.code()
    );
    assert_eq!(
        kira_network_request_timeout_ms(request, -1),
        NetworkError::InvalidConfig.code()
    );
    kira_network_request_discard(request);
    assert_eq!(
        kira_network_request_send(request),
        NetworkError::UnknownHandle.code()
    );

    // A listener that accepts and never answers, so the deadline is the
    // only thing that can end the request.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("a silent listener");
    let port = silent.local_addr().expect("its address").port();
    let url = cstring(&format!("http://127.0.0.1:{port}/never"));
    // SAFETY: both strings outlive the call.
    let request = unsafe { kira_network_request_new(method.as_ptr(), url.as_ptr()) };
    assert_eq!(kira_network_request_timeout_ms(request, 250), 0);

    let operation = kira_network_request_send(request);

    assert_eq!(wait_open(operation), Err(NetworkError::Timeout.code()));
    kira_network_close(operation);
}

#[test]
fn cancellation_removes_the_operation_handle() {
    let handle = kira_network_io_roundtrip();
    assert!(handle > 0);

    kira_network_cancel(handle);

    assert_eq!(kira_network_poll(handle), -101);
    kira_network_close(handle);
}
