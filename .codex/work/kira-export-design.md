# Exporting a Kira library to Rust: the decision

Decided 2026-07-18. This is the **Kira → Rust** direction: UI Foundation stays
authored in Kira, and a Rust program calls `uifoundation::make_button("ok")`.
The opposite direction — consuming a Rust crate from Kira — is
`.codex/work/extern-c-design.md`; § 12 states how the two relate.

## 1. The decision

**Build one author surface and one consumer product, backed by two engines: a
new `@Export` marker on a `kind = "library"` package, compiled by `kirac build`
into (a) a `.kbc` with an appended KBC1 exports section for the VM engine and
(b) a self-contained native library with stable C-ABI trampolines for the LLVM
engine — both fronted by a machine-generated Rust wrapper crate with the same
safe API.** That is agent 1's Shape B and Shape A composed behind one surface;
the engine is the backend axis, the wrapper API is what parity is measured on.

- **The VM engine is the default** (`--backend vm`): the wrapper crate embeds
  the bytecode via `include_bytes!` and runs it on a new persistent
  `Instance` in `kira-vm-runtime`. No unsafe, no linker, no LLVM; provable on
  the CI machine; compiles for `wasm32-unknown-unknown`, which is this
  feature's honest wasm yes.
- **The native engine** (`--backend llvm`) is the literal "extern C in Kira":
  a staticlib/cdylib of compiled Kira code exporting one stable trampoline per
  export in the uniform signature the hybrid seam already load-tests
  (`unsafe extern "C" fn(*const BridgeValue, u32, *mut BridgeValue)`,
  `kira-hybrid-runtime/src/library.rs:34`), guarded by a per-library ABI
  marker symbol. Never per-signature typed C symbols — Shape D is rejected for
  the reasons the trampoline design already records (`library.rs:25-30`,
  `kira-native-bridge/src/runtime.rs:1-13`): it re-opens ABI drift and
  per-signature marshalling to buy nothing the generated wrapper doesn't hide.
- **`kira-main` is the embedding crate and grows in two steps.** v1 fills it
  with the *Rust* embedding surface (load a library, call an export, hold a
  handle) that the generated VM-engine wrapper depends on — fulfilling its
  charter ("stable entry points embedders call to load and run Kira programs")
  without freezing a C API prematurely. The language-agnostic C facade (agent
  1's Shape C: `kira_program_load` / `kira_program_call` for Swift/C/Zig
  consumers) is v2 growth of the same crate, deferred because every C
  signature is append-only forever and the one consumer this feature names is
  Rust. `kira-main` moves above `kira-vm-runtime` in the layer DAG; its
  "Layer 10" doc line and the where-to-change skill's layer-1 listing are both
  reconciled in the same change.
- **Handles ship in v1.** This is the load-bearing call: without cross-call
  state a UI library cannot remember a widget between calls, and Kira has no
  globals. `@Export` on a class makes it handle-eligible; instances cross as
  opaque owned handles (appended `BridgeValueTag::HANDLE = 8`), backed by a
  persistent `Instance` heap on the VM engine and native object pointers on
  the LLVM engine. A scalar-and-string-only v1 would prove the seam and not
  serve the motivating library.
- **The wasm C-ABI artifact is refused by name in v1** (§ 4), with a stated
  reason and a recorded yes-path. **Hybrid is mandatory** — see § 4 and § 1a.

## 1a. Blocking prerequisite: Kira cannot build a library at all

**No package can be built as a library today, on any backend.** This is not a
hybrid gap and not an export-feature gap — it is a hole in the toolchain that
every part of this design sits on top of, and it must be filled before any
export work begins. Two facts establish it:

- **`PackageKind::Library` is inert.** It is declared in
  `kira-manifest/src/project_manifest.rs:9-12` and is even the default in
  `package_manifest.rs:24`, but **nothing outside `kira-manifest` reads it** —
  a repo-wide search for `PackageKind::Library` returns no consumer in any
  backend, the build layer, or the CLI. The manifest can *say* "library" and
  nothing downstream changes behavior.
- **`@Main` is required unconditionally, in the frontend.**
  `kira-semantics/src/analyze.rs:409-415` emits `KSEM011` ("program has no
  `@Main` function to run") for any program lacking one, with no library
  exemption and no knowledge of `PackageKind`. A library has no `@Main` by
  definition, so it is rejected during *analysis* — before a backend is
  selected at all.

That ordering is what makes this a prerequisite rather than a per-backend task:
the refusal happens above the backend split, so it cannot be fixed inside the
VM, LLVM, or hybrid paths individually. Library build mode is therefore step 0
of the implementation plan (§ 9), ahead of `@Export`, ahead of the wrapper
generator, and ahead of anything backend-specific.

What step 0 owes, at minimum: `PackageKind` must reach analysis so the `@Main`
requirement becomes conditional on building an *application*; a library with no
`@Main` must analyze clean; a library must still refuse to `kirac run`, by name
and with a reason; and the three backends must each produce a library artifact
rather than an executable — for LLVM that means suppressing the C `main` that
`codegen/entry.rs` emits unconditionally today.

This also settles a scoping question the design pass left implicit: the export
feature is **not** the thing that introduces libraries to Kira. It consumes a
capability the toolchain is missing, and building that capability is a
first-class piece of work with its own tests on all three backends.

## 2. What the Kira author writes

UI Foundation, `package.kira`. The grammar below is the one the oracle's
templates pin; those templates are test fixtures in
`kira-manifest/src/declaration_loader.rs`, and the shape this section carried
during the design pass (`Package { name: "…" kind: "library" }`) was wrong and
does not parse. `PackageKind` already existed
(`kira-manifest/src/project_manifest.rs:9-29`) and drove nothing; this design
makes it drive the library build:

```kira
Package uifoundation {
    let version = "0.1.0"
    let kind = .Library
}
```

The library (real syntax throughout — `class` as pinned by the landed parity
corpus, `@Export` bare per the pinned annotation grammar):

```kira
// A handle-eligible class: instances cross to Rust as opaque handles.
@Export
class Button {
    var title: String = ""
    var width: I64 = 120
    var clicks: I64 = 0

    function label() -> String {
        return self.title
    }
}

// Handle out: the constructor-shaped export.
@Export
function makeButton(title: String) -> Button {
    let b = Button()
    b.title = title
    return b
}

// Scalar in, scalar out.
@Export
function buttonWidth(b: Button) -> I64 {
    return b.width
}

// String out: Rust takes ownership of the result (§ 5).
@Export
function buttonLabel(b: Button) -> String {
    return b.label()
}

// The callback direction that exists here — Rust calling back INTO Kira —
// is simply another export: the Rust event loop owns the native event source
// and re-enters Kira per event. No new machinery; re-entry is a plain call.
@Export
function clickAt(b: Button, x: I64, y: I64) -> Bool {
    b.clicks = b.clicks + 1
    return x >= 0 && x < b.width
}
```

What is **refused** in v1 instead of given invented syntax (codes provisional
— the KSEM registry runs to 157 today and the import direction's
implementation will also claim a block; titles are the spec, final numbers are
assigned from the next free range at implementation):

```kira
@Export
function titles(b: Button) -> [String] { ... }
// error[KSEM160]: an array cannot cross the export boundary yet
//   (who frees the elements is undesigned — the ArrayAtSeam reason verbatim)

@Export
function styleOf(b: Button) -> Style { ... }        // Style is a struct
// error[KSEM161]: a struct cannot cross the export boundary by value;
//   mark a class `@Export` and pass a handle instead
// (enums: KSEM162, same shape — a tagged value does not fit one tag + one word)

@Export
function onClick(b: Button, handler: (I64, I64) -> Void) { ... }
// error[KSEM163]: a function value cannot cross the export boundary;
//   Kira-calls-Rust callbacks are the native-library import direction

@Export
function place(b: Button, at: Point) { ... }        // Point is not @Export
// error[KSEM164]: `Point` is not an exported class; only `@Export` classes
//   cross as handles

@Export
function takeTitle(move s: String) { ... }
// error[KSEM165]: exported parameters may not declare `move` or `borrow mut`;
//   the boundary contract is fixed per type (§ 5)

@Export { symbol: uif_button; }
// error[KSEM166]: `@Export` takes no arguments and no block
//   (symbol names are derived; overrides are surface nobody needs yet)
```

Further semantic rules: `@Export` outside a `kind = "library"` package is
KSEM158; `@Main` inside a library package is KSEM159 (and the existing
"program has no `@Main`" requirement at `kira-semantics/src/analyze.rs:414` is
relaxed for libraries); two exports whose names collide after snake_case
mapping (`buttonLabel` / `button_label`) are a bind-time refusal, KSEM167; an
`@Native` function in a library built for the VM engine is a build-time
refusal by function name (oracle-pinned: `@Native` cannot execute on the pure
VM). v1 exports **top-level functions only** — `@Export` on a class marks
handle-eligibility and mints the Rust newtype + destructor, it does not export
methods; the author wraps methods in exported functions, as `buttonLabel`
does.

## 3. What the Rust consumer writes

The developer flow end to end:

```
$ cd uifoundation/ && kirac build                 # VM engine (default)
   -> .kira-build/lib/uifoundation.kbc
   -> .kira-build/rust/uifoundation/              # generated wrapper crate
$ kirac build --backend llvm                      # native engine, same API
   -> .kira-build/lib/libuifoundation.a  (+ .dylib)
   -> .kira-build/rust/uifoundation/              # regenerated, native internals
```

Consumer `Cargo.toml`:

```toml
[dependencies]
uifoundation = { path = "../uifoundation/.kira-build/rust/uifoundation" }
```

Consumer code — identical against either engine:

```rust
use uifoundation::{Button, Uifoundation};

fn main() -> Result<(), uifoundation::Error> {
    let ui = Uifoundation::load()?;
    let button: Button = ui.make_button("ok")?;      // handle out
    println!("{}", ui.button_label(&button)?);       // owned String out
    let hit = ui.click_at(&button, 4, 8)?;           // Rust re-enters Kira
    assert!(hit);
    drop(button);                                    // releases the Kira object
    Ok(())
}
```

Generated internals, **VM engine**: the crate holds
`Rc<RefCell<kira_main::Instance>>` (shared into each handle so `Drop` can
release), `include_bytes!`d KBC1, and per-export methods that call
`Instance::call` with `NativeArg`s — no `unsafe` anywhere. `load()` decodes
the module, verifies the exports section (name, arity, content hash) and
returns a typed error naming the first export that disagrees. Handle newtypes
carry a root id plus the shared `Rc`; `Drop` calls `Instance::release`.

Generated internals, **native engine**:

```rust
unsafe extern "C" {
    fn kira_lib_uifoundation_abi_1();               // the stale-build guard
    fn kira_lib_uifoundation_make_button(
        args: *const BridgeValue, count: u32, out: *mut BridgeValue);
    fn kira_lib_uifoundation_drop_button(
        args: *const BridgeValue, count: u32, out: *mut BridgeValue);
    fn kira_rt_str_new(data: *const u8, len: usize) -> *mut core::ffi::c_void;
    // ... str_free / str_data / str_len, per export one trampoline
}
```

with a `build.rs` that emits `cargo:rustc-link-search` +
`cargo:rustc-link-lib=static=uifoundation` plus the platform libraries
`link.rs::platform_link_arguments` already enumerates, and safe wrappers that
do exactly what `kira-hybrid-runtime/src/marshal.rs` does today: encode a
`BridgeValue` array (strings as fresh handles from the library's own
allocator), call the trampoline, lift the result (take = read + free).
`load()` calls `kira_lib_uifoundation_abi_1()` — an empty, free call whose
only job is making a stale archive fail the **link**, by name. Wrapper types
are `!Send` in v1 on both engines.

The generated crate is deterministic, keyed to a content hash of the library
build, regenerated when the hash changes, and never committed.

## 4. The four-backend matrix

**VM (`kirac build`, default) — works.** Product: `uifoundation.kbc` (KBC1 +
appended exports section) inside the generated crate, depending on
`kira-main` → `kira-vm-runtime`. The consumer *embeds the VM* — and this is
the portable core's charter, not a workaround: the embedder supplies
`HostCapabilities` (`kira-runtime-abi/src/lib.rs:159-178`), the wrapper
provides a default host (print → stdout) and accepts a custom one.
`Program::call` already proves the mechanism
(`kira-vm-runtime/src/lib.rs:114-146`); the delta is the persistent
`Instance` (§ 8). CI-provable end to end with no LLVM.

**LLVM/native (`kirac build --backend llvm`) — works.** Product:
`libuifoundation.a` and `libuifoundation.dylib`/`.so`, self-contained (the
`kira-native-bridge` runtime archive baked in, exactly as
`link_shared_library` builds the hybrid native half today,
`kira-llvm-backend/src/link.rs:78-93`). Library codegen mode: no C `main`
(today emitted unconditionally, `codegen/entry.rs`), stable name-based
trampolines instead of `kira_native_fn_<id>`, a synthesized drop trampoline
per exported class, and the per-library marker. All functions compile native
— `--backend llvm` already ignores `@Runtime`/`@Native` (only hybrid splits),
so a whole-Kira library including `@Runtime` code is fine here. The consumer
links the archive through the generated crate; **no LLVM at the consumer's
build or runtime**. Tests are LLVM-gated; per the verifying-work bar the CI
machine cannot prove this engine, which is stated, not hidden.

**Hybrid — MANDATORY in v1. This overrides the design pass's recommendation.**

The design pass proposed refusing hybrid, on the reasoning that a consumer
library gains nothing over `--backend llvm`. **That reasoning is rejected by
project decision**: hybrid is one of the three backends, and consistency across
all three is not a cost/benefit question. A feature that works on VM and LLVM
but refuses hybrid is a parity hole, which is the exact failure mode this repo
exists to prevent — the same standard every feature in the just-completed
migration was held to.

So hybrid ships. The yes-path the design pass already identified is the
starting point, not a deferred option: a wrapper crate embedding
`kira-hybrid-runtime::Session` — the bytecode half plus the dlopened native
half — presenting the *same* generated safe Rust API as the other two engines.
The objections stand as real costs to be paid rather than reasons to refuse:
`libloading` enters the consumer's dependency graph, and a dylib deployment
story (where the native half lives, how it is found at load time) must be
designed rather than assumed. Both belong in the implementation plan.

Open, and to be settled during implementation: whether a hybrid-engine library
keeps the `@Runtime`/`@Native` split *meaningful* for a consumer — that split
exists to serve live-reload of applications, and what it means for a library
with no `@Main` is a genuine question, not a settled one. Refusing hybrid is
not an acceptable answer to it.

**wasm — two consumers, one yes and one refusal.** (1) A Rust wasm
application embedding the library **works**: the VM-engine wrapper crate
compiles for `wasm32-unknown-unknown` because everything under it does
(portable-core charter, `kira-vm-runtime/src/lib.rs:5-9`); a
`cargo check --target wasm32-unknown-unknown` of the generated fixture crate
is part of the done-bar. (2) A wasm *C-ABI library artifact* for a JS host is
**refused by name**: `a library cannot be built as a wasm module yet: the
wasm backend emits one self-contained module and links no foreign code, and
the string/allocator contract across a wasm module boundary is undesigned`.
The yes-path is recorded: the module builder supports arbitrary named exports
(today it exports only `MAIN_EXPORT`).

## 5. The boundary contract: who allocates, who frees

The seam's 3-mode `Ownership` (Owned/Borrow/BorrowMut) is untouched — it
describes the Kira/Kira hybrid seam. The export boundary has a **fixed**
contract per type; no per-parameter ownership crosses. In language terms,
exported parameters behave as `copy` (scalars, handles) or `borrow` (strings);
handle results are a `move` out of Kira; `move`/`borrow mut` parameter
declarations are refused (KSEM165) — a mutable borrow across the boundary
would promise mutation of storage the other side does not manage.

| Type | Direction | Contract |
|---|---|---|
| Void, Bool, I8..I64, U8..U64, F32/F64 | both | copied by value; nothing to free |
| String | Rust → Kira | Rust lends `&str`. VM engine: copied into the call's heap (`NativeArg::Str`, the documented args-borrow rule). Native engine: wrapper allocates a fresh handle from the **library's** allocator (`kira_rt_str_new`) and does not free it — the callee frees its string arguments at return (the `marshal.rs` contract verbatim) |
| String | Kira → Rust | Rust owns the result. VM engine: `NativeResult::Str` is an owned `String` copied out before the call heap drops. Native engine: take-string discipline — read the bytes, then `kira_rt_str_free` from the same library; the Rust `String` the consumer holds is a plain Rust allocation, and the library's heap is already balanced |
| Handle (exported class, tag `HANDLE = 8`) | both | **Kira allocates, only the generated destructor frees.** VM engine: payload is a root id into the `Instance`'s rooted heap; `release` un-roots it; a dangling root id is a typed error, never UB. Native engine: payload is pointer bits to the native object; the synthesized `kira_lib_<lib>_drop_<class>` trampoline frees it. The Rust newtype enforces free-once via `Drop`, and use-after-free is unrepresentable in the safe API because methods borrow the handle and `Drop` consumes it. Null never crosses |
| struct / enum by value, arrays, function values | both | **refused** (KSEM160-163) — the existing three-tag never-travels refusal stands (`bridge.rs:40-87`); the array ownership question stays unanswered rather than guessed; function values have no crossing representation by the closure design's own charter |

Trap semantics follow the language's existing app-level split: on the VM
engine a Kira trap surfaces as a typed `uifoundation::Error`; on the native
engine a trap aborts the process, exactly as a `kira build` binary does today.
`attempt`/`try`/`handle` inside the library is the portable way to keep a trap
from reaching the boundary.

## 6. What the toolchain produces and how

`kirac build` in a `kind = "library"` package is the verb — no new verb; the
manifest field selects the mode. The pipeline: frontend (with the § 2 export
checks) → per backend:

- **VM**: `kira-bytecode` compiles the library with an appended exports
  section; artifact `.kira-build/lib/<name>.kbc`.
- **LLVM**: library codegen mode (no entry point; per-export trampolines;
  per-class drop trampolines; marker) → object → `link.rs` grows an archive
  step (combine the object with the runtime archive via the discovered
  install's `llvm-ar`) and reuses `link_shared_library` for the dylib form;
  artifacts `.kira-build/lib/lib<name>.a` and `.dylib`/`.so`.
- **Both**: `kira-build` (whose header already claims bindings generation as
  in-scope) emits the wrapper crate to `.kira-build/rust/<name>/` from the
  program it has in hand — **no new metadata artifact format is needed**; the
  KBC1 exports section is the only serialized export table, and the native
  engine's export list is known at generation time. `kira-cli` stays a thin
  driver.

**Symbols** (native engine): `kira_lib_<library>_<export>` with the export
name snake_cased (`kira_lib_uifoundation_make_button`), drop trampolines
`kira_lib_<library>_drop_<class>`, and the marker
`kira_lib_<library>_abi_1`. All trampolines share the uniform `TrampolineFn`
shape.

**How a stale build fails by name** — the `RUNTIME_ABI_VERSION` lesson
applied twice:

- Native engine: the generated crate *calls* `kira_lib_<lib>_abi_1` in
  `load()`, so an archive built under a different export-ABI contract fails
  the consumer's link naming the marker. A breaking change to the trampoline
  contract renames the marker to `_abi_2`; `kira-main` owns the constant and
  its pinning test.
- VM engine: `load()` validates the embedded module's exports section against
  the names, arities, and content hash the wrapper was generated from, and
  returns a typed error naming the first mismatch. (Symbols cannot guard
  here; data does.)

**Dependency resolution** for the VM-engine crate: the generated
`Cargo.toml` writes path dependencies on `kira-main` (and transitively the
workspace) into the toolchain checkout, resolved at generation time.
Published crates are the eventual answer; the path form is v1's, stated in
the generated crate's README.

**One library per process** (native engine): two Kira staticlibs both carry
`kira_rt_*`, so linking two into one binary fails with a duplicate-symbol
error — loud, at link, by name, which is acceptable for v1 and documented.
Per-library runtime prefixing is the recorded v2 fix; `RTLD_LOCAL` already
isolates the dylib form for hosts that dlopen.

## 7. Oracle-pinned versus new design

**The oracle has no library-export or embedding concept.** Its `kira export`
verb is application packaging (Xcode/CMake/Gradle/web scaffolds) and its
`@FFI.*` family is exclusively the *import* direction. This entire feature is
beyond the oracle, and nothing below is presented as oracle behavior.

**Pinned facts this design obeys:** the annotation grammar (bare form; block
with `:` binders — which `@Export` deliberately does not use in v1); the
compiler-semantic annotation list is closed at 13 names user code cannot
extend — adding `@Export` as a 14th builtin is an **owned divergence from the
closed-list property itself**, stated as such; `@Native` cannot execute on the
pure VM (hence the VM-engine refusal of `@Native` library functions); the
transient-string-copy discipline at seams (pinned by the oracle's
300-iteration leak test, and already this repo's `marshal.rs` law).

**New Kira design, owned here:** `@Export` itself and every § 2 refusal; the
library build mode driving `PackageKind::Library`; the KBC1 exports section;
the `kira_lib_*` symbol scheme and per-library marker; the HANDLE crossing
and exported-class handle model; the persistent `Instance`; the generated
wrapper crate and its API shape; `kira-main`'s embedding surface; every
backend refusal and yes-path in § 4.

## 8. Wire-format impact

All appends; **no `RUNTIME_ABI_VERSION` bump** — no existing `kira_rt_*`
signature, ownership rule, or representation changes, and no new `kira_rt_*`
helper is anticipated (drop trampolines are generated code, not runtime
helpers).

- **KBC1**: an appended exports section — per export: name, function id,
  param tags, result tag; plus an exported-class table (class name → the type
  its handles denote). Decoder validates every byte; round-trip and
  truncation tests extended in the same change. Old modules (no section)
  decode as libraries with no exports.
- **Opcodes**: none. The caller is Rust; no Kira bytecode names an export.
- **`BridgeValueTag`**: append `HANDLE = 8` — payload is an opaque word the
  *producing side* owns (root id on the VM engine, pointer bits on the native
  engine; the consumer never interprets it). **Shared with the import
  direction's design**, which also appends `HANDLE = 8`: one tag, defined
  once in `kira-runtime-abi`, whichever implementation lands first defines
  it; `CALLBACK = 9` belongs to the import direction alone.
- **`NativeArg`/`NativeResult`**: `Handle(u64)` arms — safe Rust enums, no
  wire implication.
- **New symbol family, new guard**: `kira_lib_<lib>_*` versioned by its own
  marker `kira_lib_<lib>_abi_1` (name-is-the-guard). Distinct from the import
  direction's `kira_x_<lib>_*` namespace by prefix, so the two features can
  never collide in one process.
- **`Execution`/`Ownership`/`KHM1`/`KLB1`**: untouched.

## 9. The implementation plan

Ordered; each step lands green on all four backends (a typed refusal is
green), in the migration's style: surface first, checked but refused, then one
engine at a time behind the same surface. Both engines ship in v1 — the
native engine is step 6, not "later".

1. **Frontend surface** — parse `@Export` (14th builtin, bare-only), library
   mode in semantics (relax `@Main`, KSEM158-167 refusals, name mapping +
   collision check). Every backend unaffected; calls refused with "library
   export is not built yet". *Cheap; layers 1-2.*
2. **Wire appends** — KBC1 exports section (+ round-trip/truncation tests),
   `HANDLE = 8` (+ layout/decode tests, coordinated with the import
   direction), `NativeArg`/`NativeResult` handle arms. *Cheap; layers 0 and 4.*
3. **Persistent `Instance`** — `kira-vm-runtime`: a heap that survives across
   calls, value rooting, `call`/`release`, typed dangling-root errors, heap
   accounting at instance drop; `wasm32-unknown-unknown` check stays green.
   *The one deep VM item; layer 4.*
4. **`kira-main` embedding surface** — `Library`/`Instance` wrappers, default
   host, exports-section verification, marker constants + pinning test; move
   the crate above `kira-vm-runtime` and fix its layer line and the
   where-to-change skill. *Medium; one crate.*
5. **VM-engine product** — `kira-build` wrapper-crate generator + `kirac`
   library build; a workspace fixture library and a consumer test crate prove
   `uifoundation::button`-shaped calls end to end **on CI, no LLVM**, plus the
   wasm32 check of the generated crate. *Medium-large; layers 7-top.*
6. **Native-engine product** — library codegen mode (no entry, stable
   symbols, drop trampolines, marker), `link.rs` archive step, generator's
   native variant with `build.rs` + extern block + marshalling. LLVM-gated
   parity tests running the same consumer test against the native crate.
   *Large; touches kira-llvm-backend + kira-build.*
7. **Refusals by name** — hybrid library build, wasm library artifact,
   `@Native`-on-VM-engine, one-library-per-process documentation. *Cheap.*
8. **Docs** — refresh README crate table (`kira-main`), toolchain docs, a
   worked example package; cross-link the two direction documents. *Cheap.*

## 10. What v1 deliberately does not do

- **No C facade for non-Rust consumers** — Shape C is v2 growth of
  `kira-main`; a C API is append-only forever and starts only when a non-Rust
  consumer exists.
- ~~No hybrid-engine library~~ — **struck by project decision. Hybrid is
  mandatory (§ 4).** All three backends carry this feature; consistency across
  them is not traded away for v1 convenience.
- **No wasm C-ABI library artifact** — refused by name; the module-export
  yes-path is recorded. The VM-engine crate on wasm32 is the supported wasm
  story.
- **No arrays, structs-by-value, or enums across the boundary** — the
  standing never-travels refusals, unchanged, for their recorded reasons.
- **No function values across the boundary** — Kira-calls-Rust callbacks
  belong to the import direction's machinery; here, "callback" means Rust
  re-entering an export.
- **No method exports** — `@Export` classes mint handles only; functions are
  the exported surface. Method export is sugar purchasable later without ABI
  change.
- **No symbol overrides, no `@Export` arguments or block** — derived names
  only.
- **No `Send`/`Sync` wrapper types** — single-thread contract, matching the
  existing session rule; a `Mutex`-guarded engine is a v2 question.
- **No two Kira native libraries in one process** — fails loud at link;
  per-library runtime prefixing is the recorded fix.
- **No prebuilt distribution / published crates** — path dependencies into
  the toolchain checkout, stated in the generated README.

## 11. Open questions

- **Final diagnostic numbers** — KSEM158-167 above are provisional; the
  import direction's implementation claims codes from the same registry, and
  whichever lands first takes the next free block.
- **Trap parity across engines** — VM engine returns typed errors where the
  native engine aborts (today's app behavior). Whether the native engine
  should grow a trap-to-error protocol at the trampoline boundary (an
  appended out-of-band trap slot) is a v2 design.
- **`Instance` re-entrancy** — v1 needs none (no Kira-calls-Rust in this
  direction), so `&mut self` suffices; if the two directions ever compose
  (a Kira library that itself imports a Rust crate), re-entry through the
  consumer's borrow needs a design.
- **Publishing** — when `kira-main`/`kira-vm-runtime` publish to a registry,
  and what the generated crate's dependency line becomes.
- **Composition of both directions in one process** — namespaces are already
  disjoint (`kira_lib_*` vs `kira_x_*`); the allocator story when a Kira
  native library and an imported Rust crate both live in a consumer binary
  needs checking when it first happens.
- **Live-reload of a consumed library** — whether `Instance` should support
  swapping the module under live roots, which is what a `kira live` story for
  Rust consumers would need.

## 12. Relation to `.codex/work/extern-c-design.md`

The two documents are the two directions of one seam. That one consumes a
Rust crate *from Kira* (`#[kira::export]`, `@FFI.*` bindings, `CALL_FOREIGN`,
`kira_x_<lib>_*`); this one exposes a Kira library *to Rust* (`@Export`,
KBC1 exports, `kira_lib_<lib>_*`). They share machinery deliberately: the
uniform `TrampolineFn` signature, `BridgeValue` and its 16-byte layout, the
`marshal.rs` who-frees discipline, the name-is-the-guard marker pattern, and
the `HANDLE = 8` tag (defined once, § 8). Neither constrains the other's
surface: symbol namespaces are disjoint, the import direction's opcode and
KBC foreign-imports section are orthogonal to this direction's exports
section, and each versions its own contract with its own marker family. The
only shared-landing rule is that `HANDLE` is appended exactly once.
