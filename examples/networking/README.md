# Async networking example

This package exercises one async Kira program against the real
`kira-network` native library. It starts and joins all of these loopback
operations:

- HTTP/1.1 over Tokio TCP
- HTTP/2 over Tokio TCP
- HTTP/3 over Quinn + h3 + Rustls
- a text WebSocket echo
- a raw asynchronous TCP read/write round trip

The native functions are deliberately nonblocking. Each start function returns
an operation handle; `networkPoll` reports completion, and `taskYield()` lets
Kira's cooperative scheduler run the other protocol tasks while Tokio drives
the sockets. The public C declarations and stable error constants are in
`crates/kira-network/include/kira_network.h`; `kira_network_cancel` provides
idempotent cancellation, while `kira_network_close` remains its compatibility
alias.

The crate also exposes a reusable Rust async API for native hosts. It includes
pooled HTTP/1.1 and HTTP/2 clients, streaming request and response bodies,
exact-path routers, deadlines, cancellation tokens, DNS, UDP, configurable
WebSocket sessions, and a multiplexed HTTP/3 client/server with explicit Rustls
certificate roots:

```sh
cargo run -p kira-network --example async_protocols
cargo run -p kira-network --example network_load
```

`async_protocols` runs HTTP/1.1, HTTP/2, HTTP/3, WebSocket, UDP/DNS, and raw
Tokio TCP I/O in one async Rust program. `network_load` sends 64 concurrent
streamed requests through the pool and verifies cancellation. The original
`all_protocols` companion remains the C ABI/Kira-operation compatibility test.

## Run on the host

Build the static library from the workspace root, then run the example from
this directory:

```sh
cargo build -p kira-network
kira run --backend vm examples/networking/main.kira
kira run --backend llvm examples/networking/main.kira
kira run --backend hybrid examples/networking/main.kira
```

All three runs print `10`: two successful operation results for each of the
four client/server pairs, one successful raw I/O operation, and one successful
cancellation probe. The Rust
crate also has a direct end-to-end test and a runnable companion:

```sh
cargo test -p kira-network --lib
cargo run -p kira-network --example all_protocols
```

For a release native library, use `cargo build --release -p kira-network` and
point the package's `NativeLibs/kira_network.toml` entries at `target/release`
instead of `target/debug`. The Kira FFI layer is host-native: browser/WASM
builds should provide a browser transport adapter at the async boundary rather
than linking Tokio sockets or Quinn into the web runtime.

HTTP/3 uses a self-signed certificate generated for the local loopback server;
the client trusts that certificate through the in-process operation registry.
