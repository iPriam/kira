# Async networking example

This package exercises one async Kira program against the real
`kira-network` native library. It starts and joins all of these loopback
operations:

- HTTP/1.1 over Tokio TCP
- HTTP/2 over Tokio TCP
- HTTP/3 over Quinn + h3 + Rustls
- a text WebSocket echo
- a raw asynchronous TCP read/write round trip
- one HTTPS request over TLS, assembled in Kira and read back in Kira
- the same request again, answered as JSON and read with Foundation's `parseJson`

The HTTPS request is the one that looks like calling a service rather than
demonstrating a protocol. Kira names the method, the URL, a header and a JSON
body; the library sends it over a verified TLS connection; and the response
comes back through the same handle, one Unicode scalar at a time, which
`scalarText` turns into an ordinary `String`. The loopback server answers with a
transcript of what arrived, so the program checks that the method, the header
and the body crossed rather than that some 200 came back.

`/json` answers that same transcript as a JSON document, and the second task
reads it with `parseJson` and takes its fields by name — which is what a program
calling a real service does with a response. Nothing in Foundation knows the
text came from a socket.

TLS is verified, not skipped. The loopback server generates its certificate at
startup and publishes it, and `kira_network_request_trust_loopback` adds that one
certificate to the public roots for one request. A request to a public service is
the same code without that call: the public roots are compiled in, so trust does
not depend on the machine's certificate store.

The native functions are deliberately nonblocking. Each start function returns
an operation handle; `networkPoll` reports completion, and `taskYield()` lets
Kira's cooperative scheduler run the other protocol tasks while Tokio drives
the sockets. The public C declarations and stable error constants are in
`crates/kira-network/include/kira_network.h`; `kira_network_cancel` provides
idempotent cancellation, while `kira_network_close` remains its compatibility
alias.

A request is assembled against its own handle — `kira_network_request_new`, then
the setters for headers, body, version, deadline and trust — and
`kira_network_request_send` turns it into an ordinary operation handle. Every
part crosses as a `CString`, which is the one direction text moves at a C
boundary: a Kira `String` is copied out NUL-terminated for the length of the
call. Nothing crosses back the same way, because a `CString` result would be
borrowed C storage with no owner, so the response is read from the handle
instead: one selection at a time (the body, or one header) through
`kira_network_response_read_scalar` or `..._read_byte`.

`package.kira` carries `allowThinFfiShim = true`. A host run opens its native
library at run time and this one is a static archive, so the package has to say
that the thin shared carrier is wanted.

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
`all_protocols` companion remains the C ABI/Kira-operation compatibility test,
and now includes the assembled HTTPS request the Kira program above makes: six
checks rather than five.

## Run on the host

Everything below runs from the workspace root, including the example itself —
one working directory throughout, because the Foundation path and the path to
the program are both written against it and mixing the two is how a command
that looks right fails.

The JSON task needs Foundation, so every run names the checkout's copy rather
than an installed one:

```sh
cargo build -p kira-network
KIRA_FOUNDATION_HOME=$PWD/foundation kira run --backend vm examples/networking/main.kira
KIRA_FOUNDATION_HOME=$PWD/foundation kira run --backend llvm examples/networking/main.kira
KIRA_FOUNDATION_HOME=$PWD/foundation kira run --backend hybrid examples/networking/main.kira
```

All three runs print `12`: two successful operation results for each of the
four client/server pairs, one successful raw I/O operation, one successful
cancellation probe, the HTTPS request, and the JSON one.

The Rust crate also has a direct end-to-end test and a runnable companion:

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
