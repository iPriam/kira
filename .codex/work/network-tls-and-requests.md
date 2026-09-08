# Networking: TLS and the request surface (2026-09-07)

Before this, a Kira program could start a loopback protocol demonstration and read one number
back. It could not call a service: no start function took a URL, a header or a body, and both
TCP clients refused an `https` URI outright (`api/client.rs` returned `Unsupported` for any
scheme but `http`). HTTP/3 was the only encrypted path and it verified against a certificate the
loopback server generated, so nothing in the crate could reach a public root.

## What now exists

- `api/tls.rs` owns every rustls configuration this crate builds over TCP: the public roots are
  the compiled-in Mozilla bundle, plus whatever DER anchors a caller adds. TLS 1.3 and 1.2,
  `ring` throughout, because quinn already pins that provider.
- `HttpClient` speaks `https` on both paths. The pooled path wraps its connector in
  hyper-rustls; the streaming path connects through `tls::ClientStream`, a plain-or-TLS stream
  the HTTP/1.1 and HTTP/2 handshakes share. An HTTP/2 caller requires the peer to have selected
  `h2` rather than proceeding on an unannounced connection.
- `HttpClientConfig.root_certificates` are extra anchors, never a replacement for the public set.
- `request.rs` is the payload-carrying C surface: a request is assembled against its own handle
  (method, URL, headers, body as text or bytes, version, deadline, extra trust) and `send`
  consumes it into an ordinary operation handle. Clients are cached by version and trust, so a
  second request through the same configuration reuses the pool rather than handshaking again.
- The response is read from that handle: one selection at a time (the body, or one header),
  through a byte reader and a Unicode-scalar reader. Scalars because Kira's `scalarText` takes a
  code point — a byte reader alone would make every caller wanting a `String` write its own UTF-8
  decoder.
- `kira_network_https_server` serves TLS until it is cancelled and publishes its generated
  certificate, which is what lets `kira_network_request_trust_loopback` prove the whole path on a
  machine with no internet and no CA.

## Why the response is read rather than returned

`CString` is a parameter-only seam type (KSEM176): a borrowed C string has no owned Kira
representation, so a C function cannot hand text back. Reading from the handle is what the seam
allows. The alternative — a `kira_rt_net_*` intrinsic family beside `env*` and `fs*` — would put
Tokio and rustls behind every native Kira program's runtime, which is a much larger claim than
an opt-in native library makes.

## Proof

`tests/c_surface.rs` drives the ABI as a C caller does. The TLS cases cover HTTP/1.1 and HTTP/2,
the parts that must arrive (method, path, header, body — the loopback server echoes a transcript
of what it received), the cursor's rewind and length, a binary body, a deadline against a peer
that never answers, and an untrusted certificate being refused. `api/mod.rs` covers the streaming
path over TLS separately, since it builds its own connection.

`examples/networking/main.kira` makes the request from Kira and rebuilds the response as a
`String`; it prints 11 on the VM, LLVM and hybrid backends.

## Repository state this landed on

`main` did not build: `VmExecutors` was used by `fiber.rs` and `interp/frames.rs` and defined
nowhere, and `backend_parity/tasks.rs` called an `assert_trap_message_parity` that did not exist.
Both are now written. `cargo fmt --check` and `cargo clippy --workspace --all-targets` were also
failing across many files; the mechanical fixes are in this tree and are unrelated to networking.

Both FFI examples were unrunnable: the shared-FFI-path gate refuses a static archive unless the
package opts in, and neither `examples/ffi` nor `examples/networking` had a `package.kira` at all.
Both now have one, and `allowThinFfiShim` is documented in `sites/docs`, where it was not.

The Kira-level example steps moved from `networking.yml` to `ci.yml`: they need the managed LLVM
and the pinned libffi, and the networking workflow provisions neither.

Two suites were failing on stale expectations left by the same commit, and one on a real
divergence the restored helper caught:

- `kik_harness` pinned `1475` tests and a lifecycle total of `20000`. The suite is now 1478 on
  both engines, and the lifecycle harness prints `20008` on all three — 20000 iterations plus the
  channel's 7 and the task's 1, which is what its own comment says it is proving.
- Trap wording differed by engine. The VM prefixed `task trap:`/`channel trap:` before sentences
  that already name their subject, and hybrid printed no `runtime trap:` at all where the VM and
  native both do. All three now print `kira: runtime trap: <sentence>`, and the task case asserts
  the sentence rather than just the refusal.

Not runnable on this machine, unchanged by this work: the 7 wasm/web cases (no `emcc`, no Node)
and the 4 shader-validation cases (no `glslangValidator`, `spirv-val`, `naga`). CI provisions all
of them.

## JSON (same session)

A response body was a `String` and nothing in the toolchain read one. Foundation now has
`Json/`: `Value.kira` (the tree and its total accessors), `Parse.kira` (the reader),
`Number.kira` (decimal to `Float`, split out because the arithmetic is where a number parser is
right or wrong) and `Write.kira` (compact and indented).

Decisions worth keeping:

- `Integer` and `Number` are separate variants because Kira has two number types and JSON has
  one. The split follows how the literal was written; `jsonInt` and `jsonFloat` each accept both,
  so a service that starts writing `3.0` for `3` breaks nothing.
- Every accessor is total. JSON arrives from elsewhere, and a reader that trapped on a surprise
  would turn a remote schema change into a crash.
- Refusals carry a byte offset. A parser that answered only "malformed" leaves a caller holding
  700 KB and nothing to look at.
- Nesting is bounded (`jsonMaxDepth`), because the reader recurses and the input is untrusted.
- Fifteen significant digits at an ordinary scale convert exactly: digits accumulate in an `Int`
  (one correctly-rounded conversion) rather than digit-by-digit in a `Float` (one rounding per
  digit). `9223372036854775808` reads as the double it names rather than one 800 short.

`JsnJsonTests.kira` adds 32 cases: 1478 -> 1510, identical on the VM and native. The networking
example gained a second request against `/json`, so JSON is proved against a body a server
actually sent rather than only against literals; it prints 12 on all three backends.

## The hybrid engine kept a rebuilt archive

Found while running the example: the hybrid backend reuses its native half when the crossing
surface is unchanged, and `native_surface_key` was made of the *paths* of the linked archives.
The archive is linked into that half, so rebuilding one — the workflow both FFI examples
document — left the program answering with the C it was first built against, silently, while the
VM and native engines answered with the new. The key now carries each linked file's size and
modification time. `a_rebuilt_archive_reaches_a_hybrid_program_that_already_ran` fails without
that and passes with it.

Size and mtime rather than a content hash because `libkira_network.a` is 289 MB in a debug
build, this runs on every hybrid build, and a rebuild sets the mtime by writing the file.

## Reading a byte copied the string

The JSON parser scans text byte by byte, which made a VM-side cost visible that had been there
all along: `Heap::copy_value` deep-copied a `String`, and `load_local` copies every heap-backed
value it pushes — so `data.charAt(i)` copied the whole string to read one byte of it. Scanning a
document therefore cost its length *squared*.

Measured, on the same 200,000 reads: 0.27s against a 64-byte string, 3.36s against a 708 KB one.
Parsing the 708 KB response OpenRouter actually returns took 18s of user time on the VM against
1.8s on native.

`Object::Str` now holds an `Rc<str>` and a copy is a reference count, which is what
`Object::Struct`, `Object::Array` and `Object::Enum` were already changed to for the same reason
— the enum's own comment records that `copy_value` had been 10% of an editor frame. Strings are
the simplest case of the four: a Kira `String` is never written through, `+` builds a fresh one,
so there is no first-writer path to buy storage of its own.

After: 0.27s for those 200,000 reads whichever string they read, and the 708 KB parse takes 0.90s
on the VM — 20x. Every Kira program that touches text gets this, not only JSON.
