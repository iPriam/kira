# Session handoff — 2026-08-06/07

Everything from one long session across three repos: `kira`, `kira-graphics`,
`ui-foundation`. **All work is uncommitted.** Nothing was pushed, no PR opened.

## Working state

| repo | changed paths | HEAD |
| --- | --- | --- |
| `kira` | 163 | `5cac2e3` |
| `kira-graphics` | 76 | `80e14cc` |
| `ui-foundation` | 30 | `d6b4b01` |

Counts measured with `git status --porcelain` after the third pass below.

Verified at the end of the session:

- `cargo test --workspace`: **708 passed, 3 failed**. All three failures are
  `emcc: program not found` in `ffi_wasm` and `web`. The user says emcc is on
  PATH and only needs a session restart, so they are environmental.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean, which is what
  CI runs. `cargo fmt --all --check`: clean.
- `kira check` clean: `kira-graphics`, all 14 of its examples, `ui-foundation`,
  `ui-foundation/Examples/liquid-glass-app`.
- `kira-graphics/examples/declared_pipelines` runs and prints its three
  declarations.

Verification rule for this repo is in `.codex/skills/verifying-work`. The full
gate — fmt, clippy at `-D warnings`, and the workspace suite — was run on
2026-08-07 and is green apart from the three `emcc` failures.

## How the user wants things done

- Invoke the installed `kira` from PATH, bare, from inside the package
  directory. Never `./target/debug/kira.exe`, never `KIRA_FOUNDATION_HOME`,
  never a path argument. `knvm binstall` installs the freshly built toolchain
  with its own bundled Foundation — Foundation edits need a `knvm binstall`
  before they take effect.
- Avoid `--backend llvm` builds; they are slow on this machine. `kira check` is
  the normal verification.
- A parallel agent may be using the shared `target/` directory. Do not delete
  `target/debug/incremental`; wait out file locks instead. Transient
  "Access is denied (os error 5)" and `LNK2001 unresolved external symbol
  anon.*.llvm.*` are that contention plus a known rustc incremental bug on
  Windows — retrying usually clears them.
- Never run the same expensive command twice in one shell line.
- Naming: no `wordLikeThis` prefixes used as a namespace. State what a thing
  does. Prefer real types with methods (`extend` splits them across files).
- `match` over enums must name every variant. **No `_` arm** — a new variant
  should be a compile error at each site that must learn about it.
- Do not add "unavailable" stubs for things that do not exist. Delete them.

## What landed

### kira-graphics: the texture path came off the C shim

The original report was "the liquid-glass background can't be found in the
path". It was not a path problem. `uiLoadRgbaTextureFile` read
`graphics.metalContext`, which is null on any non-Metal backend, so it returned
0 before opening a file. Every one of the app's six candidate paths failed
identically and the app rendered a message blaming the working directory.

The loader also reached around the `kg_*` abstraction entirely, into the Metal
backend's private Objective-C surface (`metalCreateTexture`,
`metalMsgSendReplaceRegion`, and even `fopen` bound through `library: kira_metal`).

Fixed by moving the whole texture lifecycle into Kira:

- `SokolTexture.kira` calls `sg_make_image` / `sg_make_view` / `sg_update_image`
  / `sg_query_image_desc` / `sg_destroy_*` directly through the generated
  bindings. No new C.
- `GraphicsTexture` carries `viewId`, `colorViewId`, `depthViewId`, `width`,
  `height`, `sampleCount` — exactly what the C `kg_texture_record` held.
- `sokol_impl.c` lost `kg_texture_record`, `kg_texture_records[64]`,
  `kg_find_texture`, `kg_remove_texture`, `kg_create_texture_id`,
  `kg_destroy_texture_id`. `kg_create_texture_from_file_id` became
  `kg_decode_image_file_id` — decode only, no view, no record. The decoder stayed
  in C at the time because sokol links stb_image; it is Kira now, see the note
  at the end of the third pass.
- `kg_set_bind_group_texture`, `kg_ui_draw_texture` and the render-pass
  attachments now take **view ids**; `kg_begin_render_pass` gained
  `color_width/height/sample_count` because Kira is the side that knows them.

`generated/` is gitignored and rebuilt per target from the checked-in C, so
changing `sokol_impl.c` carries no stale-archive hazard.

### kira-graphics: two backends, not four

`Vulkan` and `DirectX12` were declared but carried no renderer. Removed from the
`GraphicsBackend` enum and from the repo: both backend modules, `DirectX12.toml`,
both `NativeLibrary` manifest entries, and the `DynamicFfi` module whose only
remaining users were those two. **46,043 lines deleted**, mostly
`app/bindings/directx12.kira` (17,918) and `vulkan.kira` (26,998).

The three raw-memory helpers that survived moved to `app/Core/RuntimeMemory.kira`
as `allocateBytes` / `freeBytes` / `writeByte`.

### kira-graphics: `Graphics` is pure dispatch

Every method is a `match self.backend` naming both variants, no `_`. Backend
bodies moved to new files `MetalGraphics.kira` and `SokolGraphics.kira`, so
following a method reaches the implementing backend in one step. `Frame.kira`,
`FrameLifecycle.kira` and `GraphicsRuntime.kira` the same.

Where one backend genuinely lacks a capability the arm answers through
`BackendSupport.kira` rather than pretending.

### kira-graphics: typed vocabulary

`Formats.kira` and `PassVocabulary.kira` hold 16 enums replacing 54 constant
functions. Each carries its wire code (`textureFormatCode`, `blendPresetCode`, …)
so no backend changed.

| | before | after |
| --- | --- | --- |
| `Int` pseudo-enum fields in descriptors | 24 | 0 |
| constant-returning functions | 232 | 137 |
| enums | 1 | 17 |

The 137 that remain are the `metal*` / `sokol*` **wire tables** — the translation
targets, reached only from the `*Code()` mappers. `Constants.kira` keeps only
`uniformSlotScene` / `uniformSlotObject`, which are offsets rather than closed sets.

`GraphicsFrame` carries `RenderTargetKind`, `LoadAction`, `StoreAction`,
`IndexFormat`. Integers appear only at the actual FFI call (`SokolFrame`'s
`kgBeginRenderPass`, `SokolPipeline`'s descriptor read, the `metal*` mappers).

### kira-graphics: `Result` instead of zero handles

`GraphicsFailure` names four reasons: `BackendLacksCapability`, `DeviceRefused`,
`AssetUnreadable`, `ShaderRejected`. All 14 creation methods answer with a
`Result`; **failure-by-zero in the public surface went 29 → 0**.

Six `*Result` lifters turn a backend's zero id into a reason; six `*OrReport`
helpers hand back the same zero a caller already coped with but put the reason on
the log. **99 call sites** wrapped across ui-foundation (49) and the
kira-graphics examples (50).

`api_preflight_fake` was deliberately **reverted** — it defines its own fake
`Graphics`, and the wrapping script had applied the real helpers to it.

The payoff is visible in the app: the six-path ladder became a retry that only
fires for `AssetUnreadable`, because a device that refused an upload will refuse
it again.

### kira-graphics: the `Pipeline` construct family

A pipeline is a declaration now:

```kira
Pipeline Glow {
    shader { return ShaderArtifact(label: "UiGlow", vertexEntry: "UiGlow__vertex__main") }
    vertices { return VertexStream(stride: 16, attributes: [...]) }
    blending { return Blending(enabled: true, preset: .Additive) }
}
```

- `PipelineFamily.kira` — `@Required shader`/`vertices`; everything else optional
  **with a default body**, which is what keeps a declaration short.
- `PipelineRegistry.kira` — a `kind { collector }` macro, the same mechanism
  Foundation's `TestRunner` uses. **Guarded on `rows.count == 0`**: a collector
  runs for every program linking the package, and emitting a function naming
  `PipelinePlan` into a program that never imports it is a compile error in a
  program whose only crime is not using the feature. This broke
  liquid-glass-app before the guard.
- `Graphics.buildDeclaredPipeline` via `extend` is the bridge.
- `examples/declared_pipelines/` demonstrates it.

### ui-foundation: naming

All **153** `ui*`-prefixed names gone. `UiBatch` is a real class —
`class UiBatch { var storage: RawPtr }` with **27 methods across 8 files via
`extend UiBatch { … }`**. My earlier objection that a class could not span files
was wrong; the user corrected it.

`@Native` does not survive into an `extend` block (`Execution::Inherited`), which
matches `RenderEncoder` and `Graphics` — hot-path classes that carry none.

The rename surfaced real duplication: ui-foundation re-declared `kira_runtime`
memory externs that KiraGraphics already exports, and one pair
(`uiBatchNullPtr`/`uiBatchPtrIsNull`) were self-recursive wrappers over the pair
directly above them. Deleted rather than renamed around.

### ui-foundation: the glass cache key

`glassCacheKeyMatch(state, k0: Float, … k27: Float)` — 28 positional floats, a
hand-written elementwise loop over a flat `[Float]`, and a
`glassCacheKeyFloats() -> Int { return 28 }` constant that had to stay in sync
with both — became:

```kira
@Derive(Equatable, Clone)
struct GlassSurfaceKey { var centreX: Float = 0.0  /* 28 named fields */ }
```

Adding a field now compares automatically. Before, one argument out of order was
a silent cache hit on the wrong surface.

## Compiler and Foundation changes (`kira` repo)

### `@Derive(Equatable, Clone)` now takes an enum

A struct holding an enum field could not derive — it recursed looking for
`eq_BlendPreset` and failed. Both macros are `appliesTo { struct enum }` now.
The enum arm of `Equatable` is `a == b` (structural and total, payloads
included); `Clone`'s rebuilds by variant, classifying a payload exactly as a
struct field does so it never emits `clone_Int`.

### `match` and `attempt`/`try`/`handle` in `expand` bodies

`kira-macros/src/eval.rs` refused both. Added:

- `Value::EnumCase { enum_name, variant, payload }`, constructed from
  `Expr::DotMember`, spliced back as `.Variant` / `.Variant(payload)`.
- `Stmt::Match` — selects by variant name, binds the payload, and **errors when
  no arm matches** rather than falling through.
- `Stmt::Attempt` — runs the body until a `try` unwraps a non-`Ok` case, then
  routes to the handler naming it. `Result`-shaped is structural, as elsewhere.
- Equality on two cases ignores the enum name, because a bare `.Variant` written
  in a body never learned which enum it belongs to.

### `Declaration.kind` is an enum case, not a string

`target.kind == "enum"` became `target.kind == .Enum`, over a closed
`DeclarationForm` set the compiler owns (`DeclarationKind::variant()`). Both Kira
consumers updated (`Derive.kira`, `LintRunner.kira`).

Note: `Statement.kind` is still a string. `body.rs:34` explains why and now
points at a rule that no longer holds for `Declaration` — worth revisiting.

### `I64` and `F64` deleted

`Int` **is** the 64-bit signed integer and `Float` the 64-bit float, so a second
spelling was one type wearing two names. `FloatSpelling::F64` and
`IntSpelling::I64` are gone; `from_name` no longer answers for either and a test
pins that. Their `int_code`/`float_code` slots are left as gaps.

**Both now cross the C seam.** This was the real breakage and bigger than it
first looked: `foreign.rs` refused bare `Int` *and* bare `Float`, so deleting the
fixed widths would have left no way to name a 64-bit scalar in FFI at all. `Int`
crosses as `int64_t`, `Float` as `double`; narrower C types still name their
width. Same in `scalar_foreign_type` for C-layout struct members.

Migrated: 24 `.kira`/`.ksl` files, 12 Rust test files, 4 Kira fixtures; autobind
emits `Int`/`Float` for 8-byte C scalars; `docs/language.md` and `docs/ffi.md`
rewritten (the FFI doc claimed bare `Int`/`Float` were refused).

`ForeignType::I64`/`F64` and `ForeignArg::I64`/`F64` **stay** — those are the C
ABI vocabulary, where 64-bit widths are real.

Six semantics tests needed real fixes rather than renames: a blanket `I64`→`Int`
had quietly turned width-mismatch tests into wildcard tests that could never
fail. They use `U32` now. `U64` stays — there is no bare unsigned type to
collapse it into. Ask before adding a `UInt`.

### `extend` span bug fixed

`classes/mod.rs` had `source: if *owner == id { source } else { origin_source }`.
For a method added by an `extend` block the owner **is** the class, so it took
the class declaration's file while the spans were offsets into the extend file —
diagnostics landed on unrelated comments at impossible columns (column 1000 in a
file with shorter lines). Now always `origin_source`, which is what
`method_ast`'s own doc comment already said the rule was.

### `Asset` construct family

`foundation/app/Asset.kira` — a family plus an `AssetRegistry` collector, guarded
to emit nothing for programs that declare none.

```kira
Asset GlassBackdrop {
    path { return "assets/background-cubes-cool.rgba" }
    byteCount { return 1600 * 900 * 4 }
}
```

**It does not fix the path ladder**, and I initially claimed it would. The ladder
is a *working-directory* problem; naming the asset does not tell the program
where it was launched from.

## What landed after the first handoff (2026-08-07)

### Macro-added lifecycles, and the rule that a construct cannot fight one

`Syntax.addMember` lets a macro give a family a `lifecycle { … }` section it did
not write, so a runtime contract arrives with the annotation instead of an
`extend` block:

```kira
@Driven
construct Task { @Required function label() -> String }
```

`@Driven` returns `target.syntax.addMember(quote { lifecycle { onStart() {…} } })`.
The edit is a span splice like `dropField`, so everything already written
survives byte-for-byte, comments included.

A declaration that writes a hook the macro also adds is `KMAC025`, refused in
`addMember` — the only place both halves are in hand, so the message can say the
other one came from a macro and point at the annotation rather than at injected
source the reader never wrote. Semantics already caught the duplicate as
`KSEM202`, but it pointed at the wrong line and could not name the cause.

**No hook name is baked into the compiler.** It knows `lifecycle` (a section
keyword) and `@Comptime` (an annotation); `onStart`/`onAppear`/`onSpawn` are
ordinary methods flagged `lifecycle: true`. Same property as `Test` and `Lint`.
The cost: a misspelled hook parses clean and silently never runs.

### Construct family inheritance — the runtime-holds-instances model

`construct Child extends Parent` now executes. A family is a synthesized enum
whose variants are its backed declarations; `extends` merges the parent's surface
into the child and registers each child declaration as a variant of **every**
ancestor, each with its own tag. That is what makes this work:

```kira
class WorkQueue {
    var pending: [Any WorkDispatch] = []
    function dispatch(work: Any WorkDispatch) { self.pending.append(work) }
    function drain() { for work in self.pending { work.onStart() } }
}
```

The queue holds instances and names no declaration. This replaces the
"runtime discovery by annotation" open item — a runtime does not enumerate
constructs, it accepts them.

- `crates/kira-semantics/src/constructs/inherit.rs` (new) — parent resolution,
  cycle refusal (`KSEM205`), surface merge, variant registration.
- `ConstructInfo.family` became `families: Vec<(EnumId, u32)>`; coercion picks
  the tag by which family the position asked for.
- Conformance is the **declaring** family's job. A variant reached through
  `extends` is skipped, or it would be checked against the parent's un-narrowed
  signature and fail a conformance it actually satisfies.
- Each dispatcher arm is typed with what the concrete method returns and then
  `coerce_into`s the declared result — a narrowed `String` really is boxed when
  read through `Any Parent`. The same fix went into the `@Required let` read
  dispatcher in `value_members.rs`.

**Variance, per the user's rule "only lower, never break":** a result and a
`@Required let` member may narrow; a **parameter may not**, because everything
holding an `Any Parent` passes whatever the parent's signature accepts. Both are
`KSEM206`.

### `Any Family` is the only spelling for a family type

- Bare `Widget` as a type is now `KSEM207`. A family is not one of its own
  values, and the bare name reads like a concrete type.
- `Any Family` now parses in a **binding annotation** — it did not before. The
  refusal existed because `let x: Any` followed by a call is ambiguous; a binding
  always carries an `=`, and requiring one after the family name settles it.
- `some Family` is untouched: it is the child-slot spelling with its own grammar,
  and nothing in the user's instruction implied removing it.
- Downstream cost was **zero** — neither `kira-graphics` nor `ui-foundation` used
  the bare spelling. Only this repo's own test sources did.

### Housekeeping

- The abandoned `comptime construct` model is deleted: `comptime_constructs` in
  `registry.rs`, `is_comptime_family`, and `run_lifecycle` in `comptime_fn.rs`.
  It encoded the family-level design the user corrected away from twice.
- `docs/language.md` gained a **Construct families** section — constructs were
  undocumented before. Its examples were run verbatim before being written down.
- The evaluator environment (`functions`, `shaders`, `platform`) is now one
  `eval::Comptime` value instead of three parameters threaded through 12
  signatures. That is what cleared the two `too many arguments` clippy errors,
  which were mine and would have failed CI's `-D warnings`.

## Open work, with what I learned about each

### `comptime function` — DONE

Built on `kira-macros/src/eval.rs`, as planned. Working end to end:

```kira
comptime function sumTo(limit: Int) -> Int {
    var total = 0
    var i = 1
    while i <= limit { total = total + i  i = i + 1 }
    return total
}
print(sumTo(100))   // the backend sees print(5050)
```

- `registry.rs` — `ComptimeFunction`, scanned alongside macros. **`function` is a
  keyword token, not a contextual identifier the way `macro` is**, so it is
  matched by kind rather than by `is_word`; that cost one debug cycle.
- `invoke.rs` — `find_named` locates calls by name, since there is no `!` to key
  on. `precedes_a_name` skips `.name(`, `function name(`, `struct`/`class`, so a
  method or declaration sharing the name is untouched.
- `eval.rs` — `run_value` returns the unspliced `Value` (arguments need values,
  not text); `call_comptime` lets one comptime function call another, guarded by
  `CALL_DEPTH_LIMIT = 32`.
- `comptime_fn.rs` — evaluates each argument, runs the body, splices the result.
- The declaration is blanked like a macro's, so nothing reaches a backend.
- An unfoldable argument is `KMAC020` and the call is refused, never left
  standing. Recursion is `KMAC010`.

Six tests in `kira-macros` (110 → 116). Documented in `docs/macros.md`.

**Known wart**: when a comptime call fails to expand, the call text stays and the
declaration is already blanked, so a cascading `KSEM061: call to undefined
function` follows the real diagnostic. Macros behave the same way, but the
message is misleading here — the function does exist, as a comptime function.
Worth a better diagnostic.

**Not done: `comptime construct`.** The natural follow-on, and what would let the
`Asset` family check its own paths during compilation.

**Do not port from kira-zig.** I checked and had to correct myself twice:

- `comptime construct` there is **parsed and never read** —
  `ConstructDecl.is_comptime` is set at `parser_decls_complex.zig:86` and no
  consumer in semantics reads it. There is nothing to port.
- `comptime function` there is real but narrow: `lower_exprs_comptime.zig`, 243
  lines, folds only a body that is exactly one `return`, over
  `integer | float | boolean | string`. No locals, loops or branches.

This repo already has the far stronger evaluator — `kira-macros/src/eval.rs`
runs `let`/`if`/`while`/`for`/`break`/`continue`, arrays, strings, reflection,
`quote`, and now `match` and `attempt`. The gap is **surface**, not capability:
the interpreter can only be reached by writing a macro and returning `quote`,
which is why `hostPlatformName` is a macro when it wants to be a function.

So build `comptime function` **on `eval.rs`**. Sketch:

1. Parser — `comptime` is a contextual identifier; `comptime function` currently
   falls into `item.rs`'s `parse_unsupported_item` (KSEM900). Add `is_comptime`
   to `Function`.
2. Decide where the fold happens. The existing function-kind macro path already
   rewrites `Name!(args)` in source text during expansion; a comptime function is
   nearly the same thing without the `!`, but finding `Name(args)` by text is
   riskier (shadowing, method calls). Folding in semantics and replacing the call
   with a literal is the alternative — check what `kira-macros` exposes publicly,
   since `eval` is `pub(crate)`.
3. The declaration should not reach a backend.

`comptime construct` is the natural follow-on and is what would let the `Asset`
family check its own paths during compilation.

### `package.kira` as real Kira — one blocker, well understood

The user wants this and agreed to it. `kira-manifest` hand-parses `package.kira`
in 1,105 lines (`declaration_loader.rs` 740 + `declaration_native_libs.rs` 365).

The reasons in `declaration_loader.rs:5` are **stale**: a top-level
`Package Name { … }` *is* an item the grammar has (a construct-backed
declaration) and `.Library` *is* an expression. The layering claim is backwards —
`kira-manifest` is layer 5 and `kira-parser` layer 1, so the dependency is
downward, and it is a parser rather than the compiler.

**The single real blocker**: a construct member requires a type annotation.
Manifests write `let version = "0.1.0"`; the grammar demands
`let version: String = "0.1.0"`. Everything else in a real `package.kira` parses
(`PackageKind.App`, nested `NativeLibrary` arrays, trailing commas).

Worth fixing regardless — every `Lint` declaration pays the same ceremony.
`Stmt::Let` already models it as `ty: Option<TypeRefId>`; construct members never
got it. Two routes:

- *Syntactic inference* from the literal's shape. Covers manifests. But makes
  `let x = …` weaker inside a construct than everywhere else, and
  `let kind = .Library` would still need the annotation.
- *Deferred resolution* (recommended). `ConstructField.ty` becomes
  `Option<TypeRefId>`; an un-annotated member takes its type from the default.
  `collection.rs:227` already stores defaults as unresolved syntax
  (`FieldDefault::new(syntax, self.source)`) for a later pass, so the hook
  exists. The risk is ordering: `FieldDef.ty` is read by construction sites that
  may be analyzed before that pass.

### Where the kira-graphics migration actually stands

Measured 2026-08-07. **73 distinct `kg_*` symbols** are still bound from Kira,
and `NativeLibs/Sokol/sokol_impl.c` is **4,873 lines** with 934 `kg_` mentions.
Grouped by what it would take to remove each:

| group | symbols | blocked on | verdict |
| --- | --- | --- | --- |
| `kg_event_*` | 20 | field access through an `@FFI.Pointer` | language feature, below |
| `kg_ui_*` | 7 | the D3D11/HLSL swap (**E**) | **deletes**, does not port |
| `kg_destroy_*` | 7 | nothing | portable now |
| `kg_set_*` | 7 | nothing | portable now |
| `kg_math_*`, `kg_string_*` | 7 | nothing | portable now; pure computation in C |
| `kg_make_*`, `kg_create_*`, `kg_begin_*` | 10 | nothing | portable now |
| rest | ~15 | mixed | case by case |

So roughly: **20 symbols are language-blocked, 7 disappear with E, and the
remaining ~46 are ordinary porting work with no blocker.** The texture path
(done, first handoff) is the worked example of what that porting looks like.

The Kira side is `12,196` lines under `app/`, of which `app/bindings/sokol.kira`
is `2,343` — generated bindings, exempt from the file-size rule.

- **D** — ui-foundation's real pipelines onto `Pipeline` declarations.
  `app/Backend/UiBatchPipelines.kira` is **498 lines**, `createBatch` starting at
  line 15. Unblocked; the `Pipeline` construct family and its collector are
  already in place and `examples/declared_pipelines` proves them.
- **E** — D3D11 + HLSL. Still the big one, still unstarted.
  `KiraGraphicsFoundationBackend.kira` branches on `gpuTelemetry`, which is
  `!uiBatchPtrIsNull(graphics.metalContext)` — "am I Metal" wearing a telemetry
  name. Metal gets the retained batched path; sokol falls back to the legacy
  immediate-mode C renderer, which is the bulk of the remaining shim.

  Why: `package.kira` builds sokol for `x86_64-windows-msvc` with `SOKOL_GLCORE`.
  The retained path needs storage buffers and compute; GLSL 330 has neither —
  that is what the `KSLS016` build notes say. The vendored sokol has full
  `SOKOL_D3D11`, and `BackendTarget::Hlsl` already exists with
  `kira-hlsl-backend` behind it. Switch the Windows target and the whole
  `kg_ui_*` group becomes dead code.

Nothing in today's work touched kira-graphics. Both it and `ui-foundation` still
`kira check` clean against the freshly installed compiler, including after the
`Any Family` spelling change.

### Full `kg_*` removal is blocked on a language feature

72 `kg_*` symbols are bound from Kira. The `kg_event_*` group (20) exists because
**Kira cannot read fields through an `@FFI.Pointer`** — it erases to `RawPtr`
(`error[KSEM090]: type RawPtr has no fields`), and a callback signature is
restricted to scalars, `Bool` and `RawPtr` (`KSEM245`), so it cannot receive an
aggregate either. sokol's event callback hands over `const sapp_event*`.

The fix is field access on an `@FFI.Pointer` to a C-layout struct, lowering to a
load at the field's offset. `foreign.rs:308` already does the mirror of this for
outgoing calls, and `pointer_targets` maps alias → target. It needs a
target-carrying pointer type; `Type::RawPtr` appears **79 places**, most of which
would become `RawPtr | ForeignPtr(_)`.

### GLSL is at 430, and `kira shader build` exists — UI examples run on Windows

`examples/liquid_glass` renders (`backend.initialized / frame.submitted /
first_frame`). Four bugs stacked behind one blank GL info log:

1. **GLSL was 330.** Compute and storage arrived in 430, so the one target that
   could not express every shader left **empty sources** — an empty string
   reaching `sg_make_shader` is why the log was blank. Now `#version 430 core`,
   storage as `layout(std430, binding = N) readonly buffer X_block { T X[]; };`,
   compute emitting `local_size`. `GlslError` is deleted: nothing is refused, so
   `emit` returns `String` rather than `Result`. sokol already defaults to GL 4.3
   on Windows, so no context change was needed.
2. **Nothing wrote the shader files.** `createShaderFromKsl` reads
   `generated/shaders/{Shader}.vert.glsl`; no part of the toolchain produced
   them. `kira shader build` now writes all of them (5 targets x stages).
3. **`input` is a GLSL reserved word** (since 1.30) though KSL allows it, so
   `VertexOut input;` was a syntax error. `emit::safe_name` prefixes reserved
   names (`ksl_input`).
4. **`return` inside an `if` was emitted verbatim** while `main` is `void`. The
   copy-out is now a mode the statement emitter reads, so it applies at any
   depth rather than only at the body's top level.

`kira shader build` takes no path — it builds every `.ksl` in the package, prints
what each target emitted, and `nothing emitted` is the line that would have
caught (1) at once. `--target` filters, `--emit` dumps source.

### Kira enums are usable in a macro body

A `comptime macro` body may name any enum the **program** declares:

```kira
enum ShaderBackend { Msl Wgsl Glsl Hlsl Spirv }
let target = ShaderBackend.Glsl
```

The registry scans `enum Name { … }` alongside the macro declarations, and the
evaluator resolves `Name.Case` before evaluating the base — there is no value
called `ShaderBackend`, and asking for one reported the name as unbound rather
than the case as misspelled. A case the enum lacks is refused and the refusal
lists the ones it has:

```
error[KMAC020]: … does not support `ShaderBackend.Gsl`, because `ShaderBackend`
has no case `Gsl` — it has `.Msl`, `.Wgsl`, `.Glsl`, `.Hlsl`, `.Spirv`
```

**This is why:** `Ksl.compile(input, "glsl_330")` took a *string*. The GLSL 330
to 430 rename left it pointing at nothing, and the failure surfaced as
`KMAC022: no glsl_330 output was compiled for it` at every `ksl!` call site in
ui-foundation rather than at the one line naming the target. It now reads
`Ksl.compile(input, ShaderBackend.Glsl)`.

The precompiled shader table is keyed by the **case name**, not the versioned
label, so a version bump cannot invalidate a call site again.

### Floating-point and Unicode-scalar primitives

`sqrt`, `sin`, `cos`, `tan`, `floor`, `ceil`, `abs` — one `MathOp` opcode with an
operand byte, the way `StringOp` works. LLVM emits `llvm.sqrt.f64` and friends
(`tan` calls libm, which has no intrinsic); the VM calls Rust's own. Both engines
route through `MathOp::apply`, so a future constant-fold cannot disagree with the
VM. A program may still define its own `sqrt`: the primitive answers only when
nothing else does.

`scalarText(code)` and `s.dropLastScalar()` — the two operations that count
Unicode scalars rather than bytes, which is what a text field's backspace and
key-press handling need. Everything else about `String` stays byte-indexed.

Both deleted their C: `kg_math_*` (4), `kg_codepoint_utf8`,
`kg_string_append_codepoint`, `kg_string_drop_last_scalar`, `kg_string_concat`
(now `a + b`), and 49 lines of `sqrtApprox`/`sinApprox`/`cosApprox`/`wrapRadians`
from `foundation/app/Foundation.kira`.

### Reading a number out of text

`s.isInt()` and `s.toInt()`, following the shape the language already uses for a
range: `charAt` traps out of bounds and `.count` is how you avoid it, so `toInt`
traps on text holding no number and `isInt` is how you avoid that. There is no
sentinel to spare — every `Int` is a valid answer — so a conversion cannot report
failure in its result.

This replaced `environmentInt`'s hand-rolled digit loop in
`foundation/app/Env.kira` (42 lines, plus an `environmentDigit` helper). It also
**changed that function's behaviour**: `16x` used to read as 16, and now reads as
the fallback. A partial read turns a typo in an environment variable into a value
the caller cannot tell from a deliberate setting.

### kik is where a language feature's tests belong

`tests-kik/harness` is the in-Kira stress suite — over a thousand `Test`
declarations run on vm, llvm and hybrid, plus a checksum run that catches a
backend divergence a passing case would miss. `app/PrimitiveTests.kira` covers
the maths, the Unicode-scalar operations and the number reading: **1085 passed,
0 failed**.

Note `tan(pi/4)` asserts `0.9999999999999999`, not 1. Pi/4 is not exactly
representable, and rounding the assertion up would make it pass against an
implementation that is wrong by more.

The suite also caught that the `Any Family` spelling change had missed
`tests-kik` entirely — four bare uses, since the earlier sweep only looked at
`crates`, `foundation` and `examples`.

### A Kira array crosses the C seam

An array of seam scalars passed where a `RawPtr` is declared becomes a pointer
to a C buffer the seam writes:

```kira
@FFI.Extern { library: sokol; symbol: kg_make_float_buffer; abi: c; }
function kgMakeFloatBuffer(label: CString, usage: Int, stride: Int, values: RawPtr, count: Int): U32;

kgMakeFloatBuffer(label, usage, stride, values, values.count)
```

Two widths are in play and they are not the same: Kira holds a `[F32]` as
`double`s and C reads four bytes each, so handing over the array's own storage
would give C wrong **numbers** rather than a wrong pointer — a rendering bug
rather than a crash. `HirExpr::ArrayElements` writes them out, reusing the
storage `CLayoutAddress` already uses (never reclaimed, because a C API given a
buffer may keep it).

Deliberately a coercion at a call argument rather than a spelling: the parameter
declares `RawPtr`, which is what C receives. There is no way to name the address
of an array *inside a struct literal*, which is why `sg_buffer_desc` still gets
built in C.

**This deleted the six `kg_*_buffer_upload` symbols and their C record table.**
They existed only to stream values into a C-side buffer one at a time, and the
refusal message even said so: "pass the elements through a `RawPtr` and a length
instead". Two thin `sg_make_buffer` wrappers replaced them.

`BufferDescriptor.data` is now `[F32]` and `IndexBufferDescriptor.data` is
`[U32]`, down from `[Float]`/`[Int]`. Vertex data is 32-bit on every backend
here; the old types claimed a precision that was narrowed away in C, silently.
That rippled through 10 examples and ui-foundation's `quadIndices`.

### The kg_* migration: 73 -> 40

- **kg_event_\* (20) — DONE.** Deleted from Kira and C once `@FFI.Pointer` field
  reads landed. `Input.kira` reads `eventPtr.window_width`,
  `eventPtr.touches[0].pos_x` directly.
- **kg_string_concat — DONE.** Now `a + b`.
- **kg_ui_\* (7)** — deletes with the D3D11 swap, does not port.
- **kg_math_\* (4), kg_string_\* (4), kg_maybe_request_quit_after_frame — DONE.**
  The first two became primitives; the third became five lines of Kira once
  `environmentInt` could read a number.
- **The six `kg_*_buffer_upload` symbols — DONE**, replaced by two, once an
  array could cross the seam.
- **The rest (~38) is one coupled group, not 38 independent ports.** Measured
  2026-08-07 by starting on `kg_destroy_*` and backing out. **Done in the third
  pass — see "The resource tables came off C" below.**

  `kg_destroy_buffer_id` looks like a one-liner — null-check, `sg_destroy_buffer`
  — but it ends with `kg_update_lifetime_peaks()`, and so do **47** other sites.
  That function reads `kg_active_uniform_count()` and
  `kg_active_bind_group_count()` off C-owned tables and folds `sg_query_stats()`
  into a running peak. So `create`, `set`, `destroy`, `finalize` and the
  lifetime/telemetry reporting all share C-side state and have to move together
  or not at all.

  The design question to settle first is where the resource tables and the peak
  state live once they are Kira's — that is the decision, and the porting is
  mechanical after it. This is the next session's unit of work, and it is a
  large one; do not start it piecemeal.

**After adding a `kira_rt_*` symbol, `cargo build -p kira-native-bridge`.** The
static archive is otherwise stale and the link fails with an unresolved external
— `verifying-work` says this and it is easy to miss, because the compiler builds
fine.

**Deleting a C function: match braces, do not regex.** The `kg_event_*` pass used
`\{[^}]*\}` and stopped at the first nested block's closing brace, leaving
orphaned `return` statements that only surfaced when the shim was *built* —
`kira check` does not compile C. Every later deletion walks brace depth.

### Every KSL shader fails to compile under GL on Windows

**Fixed** — kept below for the diagnosis, since the empty-info-log symptom will
recur if the emission path regresses.

Found 2026-08-07 while verifying the `@FFI.Pointer` work. **Not** the documented
GLSL-330 storage-buffer gap — `examples/ksl_triangle` is a plain triangle with no
storage buffers, no compute, and no textures, and it fails the same way
`liquid_glass` does:

```
sg[7]  error: GL_SHADER_COMPILATION_FAILED: shader compilation failed (gl)
sg[7]  info:                                    <- the GL log is EMPTY
sg[267] error: VALIDATE_PIPELINEDESC_SHADER: sg_pipeline_desc.shader missing or invalid
sg[461] panic: VALIDATION_FAILED
```

Both the vertex and the fragment shader fail, and the driver's info log is
**empty**, which is what a driver returns for source it never saw — so the first
thing to check is whether the GLSL reaching `sg_make_shader` is empty or
malformed rather than whether the GLSL is wrong.

`clear_color` runs fine (`backend.initialized / frame.submitted / first_frame`),
so the backend, the window and the frame loop are all good; it is the shader path
alone. Nothing in the FFI work touched a shader — `git status` in kira-graphics
shows no `.ksl` modified — so this predates it.

This is what actually blocks running a UI example on Windows today, ahead of E.

### Two bugs found and not fixed — both re-verified 2026-08-07, both still live

- **`fontCandidates()`** in `UiBatch.kira` lists four paths, all
  `/System/Library/Fonts/…` — Apple only. It does not bite today because
  `loadBatchFont` is only reached from `createBatch`, which only runs on the
  Metal path. **It becomes live the moment E lands** and the retained path runs
  on Windows: the retained text path would have no font at all.
- **`kira check` and the LSP pass `PrecompiledShaders::default()`** —
  `kira-check/src/lib.rs:173,187` and `kira-lsp/src/analysis.rs:65,79`. So
  `Ksl.compile` in a macro body fails `KMAC022` in the editor and under
  `kira check`, but succeeds under `kira build`, which wires a real
  `PrecompiledShaders`. A real divergence.

### Other findings worth keeping

- `@Derive` cannot handle array, generic or optional fields; and `Serializable`
  refuses a `Float` field. Both are documented v1 limits in `Derive.kira`.
- A collector emits into **every** program linking the package. Guard on an empty
  result or it breaks programs that do not use the feature. Worth documenting in
  `docs/macros.md`.
- `Declaration.kind` is readable from a macro body and was **undocumented** in
  `docs/macros.md`'s reflection API listing. Still is.
- Optional construct members can carry a **default body**. No existing family
  uses it (`Lint` uses defaulted `let` fields instead), and it is what keeps a
  declaration short.
- A construct-backed declaration is **not a value**: `Composite.plan()` works,
  but the *name* is not a value, so `Composite` cannot be passed anywhere. An
  **instance** can: `Composite()` coerces to `Any Pipeline`, and since 2026-08-07
  to `Any` of any family the declaring one extends. Its `let` fields are still
  not readable via qualification, so a family's surface has to be functions.
- `metalMemAlloc`/`metalMemFree` bind `kira_runtime`, not `kira_metal`, despite
  living in the Metal backend — so they work on Windows.
- sokol has **no sub-region image upload**, only whole-image `sg_update_image`.
  That is why the portable API has no `updateTextureRegion`; the Metal-only one
  survives with an honest diagnostic on the sokol arm.

## Third pass, 2026-08-07 — the resource tables came off C

This is the "one coupled group" the second pass identified and deliberately did
not start piecemeal. It is done. `kira` was not touched; only `kira-graphics`
and `ui-foundation` changed.

| | before | after |
| --- | --- | --- |
| `kg_*` symbols bound from Kira | 41 | 30 |
| of those, the `kg_ui_*` group | 7 | 7 |
| `NativeLibs/Sokol/sokol_impl.c` | 4,626 lines | 3,888 lines |

Count with `grep -rhoE 'symbol: kg_[a-z0-9_]+' app/ examples/ --include=*.kira | sort -u | wc -l`.
Do **not** count bare `kg_[a-z0-9_]+` occurrences — that also matches comments
naming deleted symbols and reports 42.

### Where the state went

The four C tables — `kg_shader_records`, `kg_pipeline_records`,
`kg_uniform_records`, `kg_bind_group_records`, all fixed 64-slot arrays — are
gone. Nothing replaced them with a Kira-side lookup keyed by id. The state moved
onto the Kira values that already travel through the program:

- `GraphicsShader` and `RenderPipeline` carry the reflection the shader and
  pipeline tables held: `hasPositionAttribute`, `requiredUniformMask`,
  `uniformBlocks: [UniformBlockDescriptor]`. Creation returns them by value
  (`KgShaderInfo` / `KgPipelineInfo`, in `app/Backend/Sokol/SokolShaderInfo.kira`);
  a pipeline copies its shader's fields at creation.
- `GraphicsUniform` and `BindGroup` became **classes** whose live payload sits
  behind a `nativeState`-boxed `storage: RawPtr`, the same shape `RenderEncoder`
  and `UiBatch` already use. That is what lets `updateGraphicsUniform` reach
  every bind group that captured the same uniform while callers still hold both
  through a `let` binding, which is what ui-foundation was already doing
  everywhere. Neither was ever a real Sokol resource, so their
  create/update/destroy is now pure Kira with no C call at all.
- The lifetime peaks moved off 47 fold-on-every-call sites to one sample per
  frame, reading `sg_query_stats()` through the generated bindings, held on
  `GraphicsAppRuntimeState.lifetimePeaks` (`app/Backend/Sokol/SokolLifetime.kira`,
  `app/App/NativeStateBridge.kira`). `kg_report_lifetime` is a stateless
  formatter now.
- The draw path is Kira: `app/Backend/Sokol/SokolDraw.kira` calls
  `sg_apply_pipeline` / `sg_apply_bindings` / `sg_draw_ex` directly.

### 13 symbols deleted, 2 added

Deleted: `kg_apply_pipeline_bindings_and_draw`, `kg_create_bind_group_id`,
`kg_create_uniform_id`, `kg_destroy_bind_group_id`, `kg_destroy_uniform_id`,
`kg_finish_uniform_update`, `kg_log_submit_state`, `kg_sample_lifetime_frame`,
`kg_set_bind_group_sampler`, `kg_set_bind_group_texture`,
`kg_set_bind_group_uniform`, `kg_set_draw_base_element`, `kg_set_uniform_float`.

Also deleted as vestigial: `kg_buffer_upload_record` and its three helpers (dead
since the array-across-the-seam work), `kg_ensure_triangle_vertex_buffer`,
`kg_ensure_ui_demo_vertex_buffer` and their statics, `kg_make_pipeline`,
`kg_apply_pipeline_and_draw`.

Added, both narrow and stateless:

- `kg_prepare_draw` — reads sokol's private pass-validity flag and flushes the
  legacy `kg_ui_*` batch. It exists because the `kg_ui_*` group still does.
- `kg_apply_uniform_floats` — builds the `sg_range`. **There is still no way to
  name the address of a Kira array inside a struct literal**, which is the same
  reason `kg_make_float_buffer` survives. Both disappear the day that lands.

Signatures that changed: `kg_make_shader` / `kg_make_ksl_shader` /
`kg_make_shader_ksl_inline` return `KgShaderInfo`; `kg_make_pipeline_detailed`
takes the shader's reflection as parameters and returns `KgPipelineInfo`;
`kg_destroy_pipeline_id` takes both pipeline ids directly.

### Decisions worth knowing before touching this again

- **`kg_begin_render_pass` and `kg_end_pass_and_commit` stayed in C** (restructured
  to be stateless otherwise). They share mutable state — `kg_ui_logical_scale`,
  `kg_ui_batch_count` — with the `kg_ui_*` group, so porting them would change
  `kg_ui_*` behaviour even though none of its own symbols were touched. They
  come off with **E**.
- **Uniform and bind-group ids are the constant `1` on the sokol path**, no longer
  incrementing. Nothing keys on them; only nonzero-means-valid matters, and the
  existing `*Result` lifters already check exactly that.
- **`KG_EXPOSED_UNIFORM_BLOCKS` is 4**, distinct from the internal parsing capacity
  `KG_MAX_UNIFORM_BLOCKS` (8, unchanged). Every real shader in both repos declares
  at most 2. Raising the exposed cap is a one-line change if one ever needs 5.
- Uniform data narrowed `[Float]` to `[F32]` in `UniformDescriptor`, `Light3D`,
  the Metal path and `basic_3d_cube` — the same precision-claimed-then-silently-
  narrowed debt the buffer descriptors already had fixed.
- `kg_decode_image_file_id` stays in C. sokol links stb_image. **No longer
  true**: Foundation decodes PNG and JPEG in Kira (`foundation/app/Image/`,
  `foundation/app/Compression/`), `createTextureFromFile` is an ordinary method
  naming no backend, and the symbol, the include and the vendored
  `stb_image.h` are gone.

### ui-foundation

`app/Backend/UiBatchDraw.kira`, `UiBatchQuads.kira`, `UiBatchGlassDraw.kira`,
`UiBatchGlassTargets.kira`: about 18 direct `BindGroup { handle: …, id: X }`
literals no longer type-check now that `BindGroup` is a class with a mandatory
`storage`. They go through `metalGraphicsBindGroup(id)` (Metal, already existed)
or the new backend-agnostic `bindGroupFromId(id)` in kira-graphics's
`BindGroup.kira`.

### Test scripts that never worked

`tests/run_lifetime_stress.ps1` and `tests/run_backend_memory_compare.ps1` both
asserted a lifetime-report string — `"…textures=0 uniforms=0 bindGroups=0"` —
that **never matched any real output**, before this change or after.
`run_lifetime_stress.ps1` additionally called `kira shader build` with a path
argument and `--out-dir`, and a `kira shader check` subcommand; none of the three
exist in the current CLI. It also called `kira run <path>`, which does not
resolve shader-relative paths against the target directory. All fixed; the script
runs end to end now.

### Verification

Run by me (orchestrator), all exit 0:

| repo | command | result |
| --- | --- | --- |
| `kira` | `git status --porcelain \| wc -l` | 163, unchanged — the compiler was not touched |
| `kira-graphics` | `kira check` | `ok: .` |
| `kira-graphics` | `kira lint` | `ok: . — 78 report(s) from 3 lint(s)` |
| `kira-graphics` | `kira check` in all 14 `examples/*` | every one `ok: .` |
| `ui-foundation` | `kira check .` | `ok: .` |
| `ui-foundation` | `kira lint` | `ok: . — 146 report(s) from 3 lint(s)` |

Every lint report is `KLINT001` "leading dot", all pre-existing and none in a
file this pass wrote.

Reported by the implementer and **not independently re-run** — treat as claimed,
not proven:

- The C shim builds and runs: `basic_3d_cube`, `lifetime_stress` (200
  create/destroy iterations, `lifetime stress app passed`, no leaks),
  `declared_pipelines`, `ksl_triangle`, `ui_demo`, `liquid_glass`,
  `basic_triangle`, `frame_api_triangle`, `clear_color` each submitted a real
  frame with no validation errors.
- `pwsh -File tests/run_lifetime_stress.ps1` → `Kira Graphics lifetime stress
  checks passed.`
- `Examples/liquid-glass-app` → `kira check` `ok: .`.

Re-running the ones that draw is the first thing to do next session, since
`kira check` does not compile the C shim and this pass deleted a lot of it.

### Still open after this pass

- **The 7 `kg_ui_*` symbols**, unchanged and untouched by design. They delete with
  **E** (D3D11 + HLSL), which remains the big unstarted item. `kg_prepare_draw`,
  `kg_begin_render_pass` and `kg_end_pass_and_commit` go with them.
- **`kg_apply_uniform_floats` and `kg_make_float_buffer`** both exist only because
  a Kira array's address cannot be named inside a struct literal. That language
  gap is now the single reason for two C functions. **Closed in the fourth pass.**
- **`kg_shader_source`** (`kgShaderSource`) is declared and called from nothing.
  It predates this pass and was left alone. Delete it. **Done in the fourth pass.**
- **`Examples/glass-match` and `Examples/liquid-glass-kitchen`** (ui-foundation)
  fail `kira check` on a `Result<Int, GraphicsFailure>` assignment left by the
  *texture-loading* migration, not by this one. Both were already failing before
  this pass. They are the two examples the `Result` sweep missed. **Fixed in the
  fourth pass.**

## Fourth pass, 2026-08-07 — the last struct-literal blocker closed

The three items "Still open after this pass" left unblocked are done. The
`kg_ui_*` group, `kg_prepare_draw`, `kg_begin_render_pass`,
`kg_end_pass_and_commit` and **E** were deliberately not touched.

| | before | after |
| --- | --- | --- |
| `kg_*` symbols bound from Kira | 30 | 26 |
| `NativeLibs/Sokol/sokol_impl.c` | 3,888 lines | 3,825 lines |

### An array's address is nameable inside a struct literal

The language change, in `kira`. A C-layout struct's `RawPtr` member now accepts
an array of seam scalars, exactly as an extern's `RawPtr` argument already did —
`sg_range { ptr: values, size: … }` writes the elements out at C's widths and the
member holds their address.

- `Analyzer::array_elements_address` in `foreign.rs` is the one place that builds
  `HirExpr::ArrayElements` now; `analyze_struct_literal` in `typeck/calls.rs`
  reaches it beside the `String -> CString` member coercion, which is the rule it
  reads like. Restricted to `@FFI.Struct { layout: c }`: that is where a member
  *is* C storage, and an ordinary Kira struct's `RawPtr` field is an opaque
  handle, not a place to put a buffer.
- No backend changed. `ArrayElements` was already an ordinary expression on the
  VM, LLVM and hybrid; only the positions that may produce one grew.
- `every_backend_agrees_on_an_array_named_in_a_c_layout_member` pins it on all
  three. Its last line has C **keep** the pointer and read it after the call
  returns, which is what fails if the storage dies with the descriptor.
- Documented in `docs/ffi.md` under **Arrays as C buffers**, which also documents
  the argument position — that had landed undocumented.

### What it deleted

- `kg_apply_uniform_floats` → `SokolDraw.kira` calls `sg_apply_uniforms` with a
  `sg_range` it builds.
- `kg_make_float_buffer` and `kg_make_index_buffer` → `SokolBuffer.kira` calls
  `sg_make_buffer` with an `sg_buffer_desc` it builds. `kg_buffer_usage` went
  with them; the flags are `sokolBufferUsage(BufferUsage)` now. The lifetime
  stress loop, the only C-side caller, got its own `kg_stress_make_buffer`.
- `kg_shader_source` and `kgShaderSource`, called from nothing. The internal
  `kg_shader_source_owned` stays — most of the shader path reads it.

`BufferDescriptor.stride` and `IndexBufferDescriptor.usage` went too. Both were
read by nobody: C did `(void)stride` and forced `index_buffer` on regardless of
the usage it was handed, and Metal never looked at either. `bufferUsageCode` was
their last user and is gone.

### The two examples the `Result` sweep missed

`Examples/glass-match` and `Examples/liquid-glass-kitchen` now `kira check`
clean. Both were assigning a `Result<Int, GraphicsFailure>` to an `Int` and
comparing it to `0`, and both wrap the same way `liquid-glass-app` already did:
an `attempt`/`handle` ladder where **only `AssetUnreadable` falls through to the
next path**, with the other three reasons reported. glass-match lost an absolute
`/Users/priamc/.claude/jobs/…` path in the process; `KIRA_GLASS_BG` is what
points a run at a file elsewhere. The kitchen's helpers live in a new
`app/backdrop.kira` rather than growing `mainPart2.kira` past the size rule.

### Verification

- `kira`: `cargo fmt --all --check` clean, `cargo clippy --workspace
  --all-targets -- -D warnings` clean, `cargo test --workspace --no-fail-fast`
  green apart from the same three `emcc: program not found` failures.
  `tests-kik/harness`: 1085 passed, 0 failed.
- `kira-graphics`: `kira check` `ok: .` at the root and in all 14 examples;
  `kira lint` 76 reports (was 78 — two `BufferUsage.…` leading-dot ones went with
  the deleted code and the surviving default). **Built and drew**:
  `basic_3d_cube`, `basic_triangle`, `ksl_triangle`, `ui_demo`, `liquid_glass`,
  `clear_color`, `frame_api_triangle`, `declared_pipelines`, `lifetime_stress`
  (200 iterations, no leaks) and `pwsh -File tests/run_lifetime_stress.ps1`.
  That is what proves the shim edits, since `kira check` compiles no C.
- `ui-foundation`: `kira check` `ok: .` at the root and in all 20 examples;
  `kira lint` 145 reports. `liquid-glass-app`, `glass-match` and
  `liquid-glass-kitchen` each reached
  `KIRA_APP_RENDERED_VISIBLE_CONTENT` on a native build.

### Still open

Unchanged from the third pass: the 7 `kg_ui_*` symbols and the three pass
functions that share state with them, all of which come off with **E**.

## Fifth pass, 2026-08-08: Liquid Glass on sokol GL

The session's question was why the app looks different on Windows/sokol than on
macOS/Metal. Four defects, all in what feeds the shaders. No `.ksl` under
`ui-foundation/Shaders/` changed, and `sokol_impl.c` only shrank.

### Compute on sokol

`supportsCompute()` answered `Sokol -> false`, so the retained path fell to the
`UiBlur.ksl` render-pass ladder while Metal ran the `UiGlassBlurH`/`V` compute
pair. sokol supports compute (`sg_dispatch`, `SG_SHADERSTAGE_COMPUTE` mapping to
`GL_COMPUTE_SHADER` at `sokol_gfx.h:9275`); the gap was Kira's.

Built: `KslArtifact.computeGlsl`/`computeWgsl`/`computeHlsl` filled by `ksl!`;
`SokolReflection.kira`, `SokolComputeShader.kira`, `SokolCompute.kira` (shader
desc from reflection, `sg_pipeline{compute:true}`, `sg_begin_pass{compute:true}`,
`sg_dispatch`); `TextureUsage.RenderTargetSampledStorage`;
`GraphicsTexture.storageViewId`.

Two compiler fixes it required:

- The GLSL backend declared a written texture as `uniform sampler2D` and lowered
  `store(...)` through the unnamed-builtin fallback, emitting
  `(nearOut, q.gid.xy, value);` — a comma expression that compiles and stores
  nothing. Every KSL compute shader reached a GL driver with its writes gone.
  Now `layout(binding = N, rgba8) uniform writeonly image2D` and `imageStore`.
- A `uint` uniform member is undrivable: `glUniform*iv` against it is a type
  mismatch that aborts at sokol's `glGetError` assertion. Uniform-struct members
  emit signed; non-uniform structs keep `uint`.

### Render-target row order

Under GL a colour target stores row 0 at the bottom and an uploaded texture
stores it at the top. `UiGlassFull.ksl` samples both through one UV, so every
path falling back to the scene capture drew the backdrop mirrored. Visible as
the backdrop flipping during a Light/Dark transition, which drives
`backgroundTransition` off the endpoint that `UiBatchDraw.kira:322` requires for
the direct-backdrop source.

Two conventions were cancelling, which is why a naive direct-vs-capture
comparison looked clean:

1. the row order above;
2. `@builtin(position)` in a fragment stage. `UiGlassCopy.ksl` addresses the
   interior cache by device position and GL's `gl_FragCoord.y` counts from the
   bottom. Fixing only (1) broke the cached surfaces (23% of pixels, max delta
   23, measured).

Fixed with `Shaders/FlipRenderTargetRows.ksl` plus `SokolRowOrder.kira` swapping
a colour target's rows at pass boundaries, gated on
`sg_query_features().origin_top_left`; and the GLSL backend emitting
`layout(origin_upper_left) in vec4 gl_FragCoord;` for fragment stages.

Capture path forced, glass interiors against the direct path, mean absolute
error per channel: Translucent chip `87.8` same-row / `18.5` mirrored before,
`0.46` same-row after; sidebar `6.1` / `33.6` before, `0.03` after. Ordinary
path unmoved: 0 differing pixels of 2,002,520.

### Image decoding in Kira

`createTextureFromFile` answered `.Error(.BackendLacksCapability)` on Metal
because only sokol linked stb_image. Decoding is not a backend concern, so it
left the backend surface: decode, then `createTextureFromPixels`, which both
backends implement.

New in Foundation: `Compression/Inflate.kira` (RFC 1951 and 1950),
`Compression/HuffmanCode.kira`, `Image/Png.kira` (every colour type and bit
depth, palette, both transparency forms, Adam7), `Image/Jpeg.kira` (baseline
`SOF0`/`SOF1`, restart intervals, integer IDCT and YCbCr). 25 `Test`
declarations in `tests-kik/harness`.

Checked against a standalone stb_image oracle: the real 2600x2061 asset 0
differing bytes, 25 PNGs and 15 of 16 JPEGs bit-exact. The exception is h2v1
chroma's rightmost column, where stb applies its 3:1 weights to the wrong
neighbour (`stb_image.h:3440`); nothing in either repo uses h2v1.

`kg_decode_image_file_id`, `STB_IMAGE_IMPLEMENTATION` and
`vendor/stb_image.h` deleted. `sokol_impl.c` 4,309 to 4,273.

Decoding the 5.4-megapixel asset takes about 4.4 s under `--backend llvm`
against 0.15 s for stb. It is a startup load. The gap is 21.4 M `append` calls
for the output, so closing it is array preallocation in the language, not a
decoder change.

### Diagnosis notes

Judge by pixels. Six mechanisms were proposed for the border defect and
eliminated by measurement: display scale, blend-factor mapping, patch overlap,
colour space, Y-flipped sampling, and target bucket quantization. Reading the
code supported several of them.

Read all three channels. A seam that looked like a constant grey in R was
running to `B-R = 75`.

`KIRA_GRAPHICS_QUIT_AFTER_FRAMES`, `KIRA_GRAPHICS_CAPTURE_FRAME` and
`KIRA_GRAPHICS_CAPTURE_AT` are the instrument. A `.ppm` diffed channel by
channel settles what a screenshot cannot. `.codex/tmp/macos-reference/metal.ppm`
is a Metal capture at 1x for comparison.

Backing scale is not a defect. At 1.25x a 1920-point window lands on 1924 device
pixels, so every edge sits on a fractional boundary and covers two pixels. At 1x
the sokol edge profile matches Metal exactly.

### Rules the user set

Every backend implements every member. `.Error(.BackendLacksCapability)` is a
runtime failure that compiles, and a defaulted family member silently absorbs a
new backend. A `construct RenderBackend` must have every member `@Required` and
no default bodies, so a third backend is a wall of `KSEM234`.

No new C in `kira-graphics` for any reason, including debug instrumentation.

`ui-foundation/Shaders/*.ksl` is the reference. Both backends run byte-identical
KSL, so a divergence is in what feeds them.

Do not run the Rust workspace suite when no Rust changed.

### Still open

- **The scissored capture's skirt.** The scene capture writes only the regions
  the glass surfaces need, but the border samples outside the silhouette. Metal
  escapes it only by taking the direct-backdrop branch; a Metal frame that fails
  those conditions should show it too.
- **`blurParamsV` is an identity copy.** `UiBatchGlassTargets.kira:444-445` pass
  a direction vector in the `(radius, padding)` slots, but `UiBlur.ksl` is
  isotropic and reads slot 2 as the radius, so the vertical pass runs at radius
  0. Affects the reference ladder only.
- **The 7 `kg_ui_*` symbols** and the three pass functions sharing their state,
  which come off with **E**.
- `GraphicsFailure.kira` boilerplate, the `Graphics*` prefix rename,
  `Frame.kira`'s 40-field record, and collapsing `handle`+`id`.

`sampledTextureFromViewId` is **not** dead, contrary to a review listing it so:
`UiBatchQuads.kira:185,203` binds both backdrops through it. Verify a dead-code
claim against `ui-foundation` as well as `kira-graphics`.

## Sixth pass, 2026-08-08: the `RenderBackend` construct family

The 43 `match self.backend` / `if self.backend == .Metal` sites are gone.
`construct RenderBackend` (`app/Public/RenderBackend.kira`) has 46 members, every
one `@Required` with no body; `MetalRenderBackend.kira` and
`SokolRenderBackend.kira` answer all of them. `renderBackendFor(backend,
context)` is the only place a `GraphicsBackend` still decides anything —
`GraphicsBackend` is the *selection* vocabulary (what an app asks for, what
`validateGraphicsBackend` checks before a context exists) and `Any RenderBackend`
is the *dispatch* vocabulary.

What the family forced out into the open:

- **`GraphicsFailure.BackendLacksCapability` is deleted.** Nothing could produce
  it once every operation is a `@Required` member: a backend that has not
  implemented one is `KSEM234` at build time. 18 `handle` arms across three repos
  went with it.
- **`SokolComputeShader.kira`'s "no compute source for this backend"** is
  `.Error(.ShaderRejected)`, which is what it always was.
- **`Graphics.enableDepth` is deleted**, and `BackendSupport.kira` with it. It
  had no caller anywhere, and its sokol arm was a `reportUnavailable` for
  something sokol does automatically (`sokolEnsureOffscreenDepth` and the
  swapchain's own buffer).
- **`Graphics.createShaderFromKsl` on Metal read the sokol compiler** and handed
  back a `sg_shader` id the Metal context cannot use. It reads
  `generated/shaders/{asset}.metal` — the combined library `kira shader build`
  already writes — and answers `.AssetUnreadable` when it is not there.
- **`frame.backend == GraphicsBackend.Metal` in ui-foundation's two runners** was
  the same defect outside kira-graphics: it chose where the live viewport comes
  from. `RenderBackend.viewportWidth`/`viewportHeight` (Float points) answer it,
  and `foundationMetalViewportWidth`/`Height` and `rootViewport*` are deleted.
  Sokol's Int `sokolLogicalWidth`/`Height` went too — a point is not a whole
  number of pixels at a fractional scale.

Also deleted as dead: `graphicsBackendName`, `graphicsBackendNameFor`,
`defaultGraphicsBackend`, `graphicsPlatformName`, `unsupportedBackendWithLoader`.

**What the compiler will and will not take here**, learned by probe:

- A family member with **no result type cannot be called through `Any Family`**
  (`KSEM241`). Every procedure member writes `-> Void`.
- A member **with parameters** must be written out in full in a declaration —
  `function touch(x: T) -> Void { … }`, not the `touch { … }` shorthand, which
  binds no parameters.
- A **`borrow mut` parameter** passes semantics and then fails the VM bytecode
  compiler: `mutating-method call in 'Any F.m$dispatch' is malformed (missing
  writeback or non-user callee)`. Classes pass as plain `borrow` and mutate
  through the handle, which is what `GraphicsFrame` does.

Verified: `kira check` clean in `kira-graphics`, its 14 examples, `ui-foundation`
and its 20 examples; `kira lint` 76 -> 74 and 145 -> 140;
`tests/run_lifetime_stress.ps1` passed; nine kira-graphics examples and seven
ui-foundation examples ran and drew. `liquid-glass-app` at frame 60 is **0
differing pixels of 1,980,020** against `.codex/tmp/lg-final.ppm`, excluding the
live telemetry badge at x 66..265, y 777..1025.

## Scratch

`.codex/tmp/` holds this session's captures: `macos-reference/metal.ppm` (Metal,
1920x1080, 1x) and its screenshots, `lg-1x.ppm` (sokol at 1x), `lg-now.ppm`
(1.25x), `lg-compute-verify.ppm` (after compute landed) and `lg-final.ppm`.

Python is confined to `.codex/tmp/` per the repo rule; the interpreter is at
`/c/Python312/python.exe` (bare `python`/`python3` hit the Windows Store shim).
Windows Python cannot see Git Bash's `/tmp`. PowerShell with `System.Drawing`
converts a `.ppm` to `.png` without leaving the shell.

A run without `KIRA_GRAPHICS_QUIT_AFTER_FRAMES` leaves `main.exe` holding a link
lock on the example's output, and the next build fails until it is stopped.
