#[test]
fn public_header_contains_the_complete_c_abi() {
    let header = include_str!("../include/kira_network.h");

    for symbol in [
        "kira_network_http1_server",
        "kira_network_http1_client",
        "kira_network_http2_server",
        "kira_network_http2_client",
        "kira_network_http3_server",
        "kira_network_http3_client",
        "kira_network_websocket_server",
        "kira_network_websocket_client",
        "kira_network_io_roundtrip",
        "kira_network_server_port",
        "kira_network_poll",
        "kira_network_result",
        "kira_network_cancel",
        "kira_network_close",
    ] {
        assert!(
            header.contains(symbol),
            "missing {symbol} from public header"
        );
    }

    for code in [
        "KIRA_NETWORK_ERROR_RUNTIME_INIT (-100)",
        "KIRA_NETWORK_ERROR_UNKNOWN_HANDLE (-101)",
        "KIRA_NETWORK_ERROR_TIMEOUT (-110)",
        "KIRA_NETWORK_ERROR_CANCELED (-111)",
        "KIRA_NETWORK_ERROR_INVALID_CONFIG (-116)",
    ] {
        assert!(header.contains(code), "missing {code} from public header");
    }
}
