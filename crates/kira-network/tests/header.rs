//! The public header is the C ABI's declaration of itself, so it is checked
//! against the exports rather than against a list someone maintains beside
//! them: a symbol added to the library and not to the header is a symbol every
//! C and Kira caller has to declare by hand.

use kira_network::NetworkError;

const HEADER: &str = include_str!("../include/kira_network.h");
const LIBRARY: &str = include_str!("../src/lib.rs");

/// The name of every `extern "C"` function the library exports.
fn exported_symbols() -> Vec<&'static str> {
    LIBRARY
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let rest = line
                .strip_prefix("pub extern \"C\" fn ")
                .or_else(|| line.strip_prefix("pub unsafe extern \"C\" fn "))?;
            rest.split('(').next()
        })
        .collect()
}

#[test]
fn public_header_declares_every_exported_symbol() {
    let symbols = exported_symbols();
    assert!(
        symbols.len() >= 30,
        "found only {} exported symbols, so the scan is not reading the library",
        symbols.len()
    );

    for symbol in symbols {
        assert!(
            HEADER.contains(symbol),
            "missing {symbol} from the public header"
        );
    }
}

#[test]
fn public_header_defines_a_constant_for_every_error() {
    for error in NetworkError::ALL {
        let code = format!("({})", error.code());
        assert!(
            HEADER.contains(&code),
            "missing the constant for {error} ({code}) from the public header"
        );
    }
}
