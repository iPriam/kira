//! Runs every Kira networking operation through the crate's nonblocking C ABI.

use std::time::{Duration, Instant};

use kira_network::{
    kira_network_close, kira_network_http1_client, kira_network_http1_server,
    kira_network_http2_client, kira_network_http2_server, kira_network_http3_client,
    kira_network_http3_server, kira_network_io_roundtrip, kira_network_poll, kira_network_result,
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
