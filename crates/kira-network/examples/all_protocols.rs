//! Runs every Kira networking operation through the crate's nonblocking C ABI.

use std::ffi::CString;
use std::time::{Duration, Instant};

use kira_network::{
    END_OF_SELECTION, kira_network_close, kira_network_http1_client, kira_network_http1_server,
    kira_network_http2_client, kira_network_http2_server, kira_network_http3_client,
    kira_network_http3_server, kira_network_https_server, kira_network_io_roundtrip,
    kira_network_poll, kira_network_request_body_text, kira_network_request_header,
    kira_network_request_new, kira_network_request_send, kira_network_request_trust_loopback,
    kira_network_response_read_scalar, kira_network_response_select_body, kira_network_result,
    kira_network_server_port, kira_network_websocket_client, kira_network_websocket_server,
};

const TIMEOUT: Duration = Duration::from_secs(10);

fn wait_for(label: &str, handle: i64) -> bool {
    if handle <= 0 {
        println!("{label}: start failed with code {handle}");
        return false;
    }
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let state = kira_network_poll(handle);
        if state == 0 {
            if Instant::now() >= deadline {
                println!("{label}: timed out");
                kira_network_close(handle);
                return false;
            }
            std::thread::yield_now();
            continue;
        }
        let result = kira_network_result(handle);
        kira_network_close(handle);
        if state == 1 && result > 0 {
            return true;
        }
        println!("{label}: operation failed with code {result}");
        return false;
    }
}

fn run_pair(
    label: &str,
    server: extern "C" fn() -> i64,
    client: extern "C" fn(u16) -> i64,
) -> bool {
    let server_handle = server();
    if server_handle <= 0 {
        println!("{label} server: start failed with code {server_handle}");
        return false;
    }
    let port = kira_network_server_port(server_handle);
    if !(1..=i64::from(u16::MAX)).contains(&port) {
        println!("{label}: invalid server port {port}");
        kira_network_close(server_handle);
        return false;
    }
    let client_handle = client(port as u16);
    if client_handle <= 0 {
        println!("{label} client: start failed with code {client_handle}");
        kira_network_close(server_handle);
        return false;
    }
    let client_ok = wait_for(&format!("{label} client"), client_handle);
    if !client_ok {
        kira_network_close(server_handle);
        return false;
    }
    wait_for(&format!("{label} server"), server_handle)
}

/// One HTTPS request assembled through the C ABI and read back through it.
///
/// This is the shape a program calling a service writes: a method, a URL,
/// a header and a body go out as NUL-terminated strings, and the response
/// comes back from the same handle a scalar at a time.
fn run_request(label: &str) -> bool {
    let server = kira_network_https_server();
    if server <= 0 {
        println!("{label}: the HTTPS server failed to start with code {server}");
        return false;
    }
    let port = kira_network_server_port(server);
    if !(1..=i64::from(u16::MAX)).contains(&port) {
        println!("{label}: invalid server port {port}");
        kira_network_close(server);
        return false;
    }
    let method = CString::new("POST").expect("a method with no NUL");
    let url = CString::new(format!("https://127.0.0.1:{port}/echo")).expect("a URL with no NUL");
    // SAFETY: every pointer below addresses a `CString` that outlives the call
    // it is passed to.
    let request = unsafe { kira_network_request_new(method.as_ptr(), url.as_ptr()) };
    if request <= 0 {
        println!("{label}: the request failed to open with code {request}");
        kira_network_close(server);
        return false;
    }
    let name = CString::new("x-kira-test").expect("a header name with no NUL");
    let value = CString::new("carried").expect("a header value with no NUL");
    let body = CString::new("{\"hello\":\"kira\"}").expect("a body with no NUL");
    // SAFETY: as above.
    let configured = kira_network_request_trust_loopback(request, port as u16)
        .min(unsafe { kira_network_request_header(request, name.as_ptr(), value.as_ptr()) })
        .min(unsafe { kira_network_request_body_text(request, body.as_ptr()) });
    if configured < 0 {
        println!("{label}: the request could not be assembled: code {configured}");
        kira_network_close(server);
        return false;
    }
    let operation = kira_network_request_send(request);
    let sent = wait_open(label, operation);
    let text = read_body(operation);
    kira_network_close(operation);
    kira_network_close(server);
    if sent != 200 {
        println!("{label}: the server answered {sent}");
        return false;
    }
    if !text.contains("body={\"hello\":\"kira\"}") {
        println!("{label}: the body did not arrive: {text}");
        return false;
    }
    true
}

/// Waits for an operation, leaving its handle open to be read.
fn wait_open(label: &str, handle: i64) -> i64 {
    if handle <= 0 {
        println!("{label}: start failed with code {handle}");
        return handle;
    }
    let deadline = Instant::now() + TIMEOUT;
    while kira_network_poll(handle) == 0 {
        if Instant::now() >= deadline {
            println!("{label}: timed out");
            return -1;
        }
        std::thread::yield_now();
    }
    kira_network_result(handle)
}

/// Reads the selected response body back as text, a scalar at a time.
fn read_body(handle: i64) -> String {
    if kira_network_response_select_body(handle) <= 0 {
        return String::new();
    }
    let mut text = String::new();
    loop {
        let scalar = kira_network_response_read_scalar(handle);
        if scalar < 0 {
            if scalar != END_OF_SELECTION {
                println!("reading the response failed with code {scalar}");
            }
            return text;
        }
        match u32::try_from(scalar).ok().and_then(char::from_u32) {
            Some(scalar) => text.push(scalar),
            None => return text,
        }
    }
}

fn main() {
    let mut passed = 0;
    let checks = [
        (
            "HTTP/1.1",
            run_pair(
                "HTTP/1.1",
                kira_network_http1_server,
                kira_network_http1_client,
            ),
        ),
        (
            "HTTP/2",
            run_pair(
                "HTTP/2",
                kira_network_http2_server,
                kira_network_http2_client,
            ),
        ),
        (
            "HTTP/3",
            run_pair(
                "HTTP/3",
                kira_network_http3_server,
                kira_network_http3_client,
            ),
        ),
        (
            "WebSocket",
            run_pair(
                "WebSocket",
                kira_network_websocket_server,
                kira_network_websocket_client,
            ),
        ),
        (
            "async I/O",
            wait_for("async I/O", kira_network_io_roundtrip()),
        ),
        ("HTTPS request", run_request("HTTPS request")),
    ];
    for (name, result) in checks {
        if result {
            passed += 1;
            println!("{name}: ok");
        }
    }
    if passed == checks.len() {
        println!("all {} async networking checks passed", checks.len());
    } else {
        println!("{passed}/{} async networking checks passed", checks.len());
        std::process::exit(1);
    }
}
