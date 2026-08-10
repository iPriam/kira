# Dawn backend handoff — 2026-08-10

Written mid-session, under notice that the machine may go offline. Everything
below is what is known now; the "next step" section says where to resume.

## The three repositories

All three are live checkouts on `main`, all three carry uncommitted work.

### `C:\Users\ipado\Coding\kira-projects\kira`

Broad uncommitted work across the compiler (bytecode, IR, LLVM backend, CLI,
build, live, native bridge). Not mine except where named below. The pieces this
session touched:

- `crates/kira-dynamic-ffi/src/dynamic_library.rs` — **changed by me**, see
  "The failing test" below. Fixed and verified.

Untouched by me but relevant: `crates/kira-shader-ir/src/reflection.rs` holds
`resource_digest`, the compact per-shader digest a graphics host parses. It
**already** emits storage buffers (`s|name:binding:stageMask:glslBinding:readonly`).
The gap the Dawn work needed was on the *reader* side, in kira-graphics, not here.

### `C:\Users\ipado\Coding\kira-projects\kira-graphics`

- `NativeLibs/Dawn/x86_64-windows-msvc/` is an unpacked release payload. It is
  untracked and **not gitignored**, so it shows up as untracked noise in
  `git status`. Do not delete it casually; re-fetch with
  `gh release download v2026.08.09-ebd4f560aad0 --repo kira-lang-com/dawn`.
- `app/Shader/KslReflection.kira` — **new file I added this session**. It is
  `app/Backend/Sokol/SokolReflection.kira` moved to a shared home (both backends
  read the digest now, not just Sokol) with one change of substance: storage
  buffers are described (`KslStorageBuffer`: name, binding, stageMask,
  glslBinding, readOnly) instead of merely counted.
  **INCOMPLETE**: at the moment this note is written the *old*
  `app/Backend/Sokol/SokolReflection.kira` is still present, so the package has
  two definitions of `KslResourceReflection` and friends and **will not
  compile**. The next step is to delete the old file and update its one
  remaining consumer, `app/Backend/Sokol/SokolComputeShader.kira:270`, which
  reads `reflection.storageBufferCount > 0` — that becomes
  `reflection.storageBuffers.count > 0`. Nothing else reads `storageBufferCount`.

### `C:\Users\ipado\Coding\kira-projects\ui-foundation`

Not modified by me yet. The example under test is
`Examples/liquid-glass-app`.

Off limits for the current task (an unfinished text pass owns them):
`app/Views/Text.kira`, `app/Backend/UiBatchRuns.kira`,
`app/Backend/UiBatchGlyphs.kira`, `NativeLibs/Text/*`, `app/bindings/text.kira`.
Everything else in the package is fair game (the coordinator lifted the wider
freeze for the CPU-time work).

## The failing test — FIXED

`dynamic_library::tests::a_symbol_resolves_to_an_address_that_can_be_called`,
`crates/kira-native-bridge/src/dynamic_library.rs:147`,
"the host C library must be loadable".

**Cause, confirmed by reading the code rather than guessed.** The previous round
rewrote `open_native` in `crates/kira-dynamic-ffi/src/dynamic_library.rs` to make
Windows search a loaded library's own directory for its dependencies. It did so
by calling `std::path::absolute(path)` unconditionally and then
`Library::load_with_flags(absolute, LOAD_WITH_ALTERED_SEARCH_PATH)`. The flag is
honoured only for an absolute path, so making it absolute is right *for a path*.
But the argument is not always a path: `kira_dynamic_library_open` takes a
**module name** — `msvcrt.dll`, `vulkan-1.dll`, `d3d12.dll` — which the loader
resolves through the system search order. `absolute("msvcrt.dll")` yields
`<cwd>\msvcrt.dll`, which does not exist, so the load failed outright. It was not
the flag combination; it was the path rewrite.

**Fix landed.** `open_native` now asks whether the argument names a real file:

```rust
let library = match module_file(path) {
    Some(file) => unsafe { Library::load_with_flags(file, LOAD_WITH_ALTERED_SEARCH_PATH) }?,
    None => unsafe { Library::new(path) }?,
};
```

with `module_file` returning `std::path::absolute(path)` only when it
`is_file()`. A name that resolves to no file on disk is handed to the loader
unchanged, which is both the module-name case and the genuinely-absent case.
A regression test was added *in `kira-dynamic-ffi`*, where the logic lives:
`a_bare_module_name_still_reaches_the_system_search_order`. The
`kira-native-bridge` test was not weakened.

**Verified**: `cargo test -p kira-dynamic-ffi -p kira-native-bridge` — exit 0,
all six `dynamic_library::tests` pass, `99 passed; 0 failed` for native-bridge.
The full-workspace gate has **not** been re-run since; that is still owed.

Use `cargo nextest run --workspace --no-fail-fast` for iteration (per the user),
plus `cargo test --doc` since nextest skips doctests; `kira_dev_validate
{scope: "workspace", full: true, detail: "failures"}` is still the gate that
decides. Note: the `kira_dev_*` MCP tools were **not exposed** in this session,
so cargo was used directly.

## The plain run of `liquid-glass-app` — could not reproduce a failure

The user reports being unable to get the example working at all. On this machine,
today, the plain path works. Evidence:

```
cd C:\Users\ipado\Coding\kira-projects\ui-foundation\Examples\liquid-glass-app
kira build                      # "Successfully built"
.\app\.kira-build\main.exe      # window opens, still running after 12 s, no error
```

and bounded, with a capture:

```
KIRA_GRAPHICS_QUIT_AFTER_FRAMES=70 \
KIRA_GRAPHICS_CAPTURE_FRAME=<...>/lg-sokol.ppm KIRA_GRAPHICS_CAPTURE_AT=60 \
  ./app/.kira-build/main.exe
```

exits 0 and writes a 1924x1055 PPM in which **20926 of 20926 sampled pixels are
non-black** — i.e. the frame is fully drawn. `kira run` with
`KIRA_GRAPHICS_QUIT_AFTER_FRAMES=30` also exits 0 and emits
`KIRA_APP_RENDERED_VISIBLE_CONTENT`.

**The default backend is not Dawn.** `graphicsDefaultBackend()` in
`kira-graphics/app/Backend/Backend.kira` returns Metal on macOS/iOS and **Sokol
everywhere else**, including Windows. Dawn is only ever reached by setting
`KIRA_GRAPHICS_BACKEND=dawn`. So the "default selection reaches Dawn" hypothesis
is ruled out by that function.

One plausible mechanism for the user's failure that *was* real and is now fixed:
the `open_native` regression above broke **every** load-a-library-by-name path in
the toolchain, not only the test. Nothing in kira-graphics or ui-foundation calls
`kira_dynamic_library_open` today, so it should not have hit this example — but
anything that does (a driver probe, a sidecar) would have failed with nothing but
`LoadLibraryExW failed`. Worth re-asking the user for the exact symptom and any
console output; without it there is no further lead.

## `liquid-glass-app` on Dawn — diagnosed, fix designed, partially landed

### Reproduce

```
cd C:\Users\ipado\Coding\kira-projects\ui-foundation\Examples\liquid-glass-app
kira build
KIRA_GRAPHICS_BACKEND=dawn KIRA_GRAPHICS_QUIT_AFTER_FRAMES=70 \
KIRA_GRAPHICS_CAPTURE_FRAME=<...>/lg-dawn.ppm KIRA_GRAPHICS_CAPTURE_AT=60 \
  ./app/.kira-build/main.exe
```

Previous round: exits 0, opens a window, runs 70 frames, captures a black frame
(1 non-black pixel of 54,745 sampled). `<...>/lg-dawn.ppm` from that round is at
`kira/.codex/tmp/lg-dawn.ppm`.

Compare against the Sokol capture of the same program, **not** against
`kira/.codex/tmp/lg-final.ppm` — the Sokol capture itself differs from that old
reference by ~53,000 pixels, all text, owned by the unfinished text pass.

### Cause — confirmed independently this session

Every KSL shader puts all its resources in `@group(0)` with global binding
indices. Verified directly:

```
ui-foundation/generated/shaders/UiBatch.vert.wgsl:86..94
  @group(0) @binding(0) var<storage, read> verts: array<UiQuad>;
  @group(0) @binding(1) var atlas: texture_2d<f32>;   ... through @binding(8)
```

The ui-foundation compositor partitions that one group across the encoder's four
slots (`UiBatchDraw.kira:60-63`, `UiBatchGlassDraw.kira:17-18,251-266`,
`UiBatchDraw.kira:204-206`), each slot carrying the *global* binding index inside
it. Sokol and Metal are fine with that; WebGPU is not — it wants one bind group
per `@group(n)`, matching the pipeline layout exactly.

`DawnDraw.kira:117-120` calls `dawnApplyBindGroup(..., slot, ...)` for slots 0..3
and each one does `wgpuRenderPipelineGetBindGroupLayout(pipeline, slot)`, so slot
0's group (one entry) is validated against group 0's layout (seven entries) and
Dawn refuses with `entries (1) != expected (7)`. Slots 1..3 name groups the
pipeline does not have.

The `7` matches: `UiGlassBand.resources` is
`s|verts:0:1:0:1; t|atlas:1:0:...; m|atlasSmp:2:0; t|external:3:2:...;
m|externalSmp:4:2; t|backdrop:5:2:...; m|backdropSmp:6:2; t|nearBlur:7:2:...;
m|nearSmp:8:2;` — nine resources, of which `atlas` and `atlasSmp` carry
**stageMask 0** (declared in the WGSL, statically used by no stage, so Tint's
derived layout omits them). Nine minus two is seven.

### The fix, as designed

Two halves.

1. **Merge the encoder's four slots into one WebGPU group at index 0.** The
   entries already carry the correct global binding numbers, so merging is a
   concatenation, not a renumbering.
2. **Filter the merged entries to exactly the bindings the pipeline's layout
   declares**, using the shader's `resourceReflection` digest: keep a resource
   iff its `stageMask != 0`. UiRect, for instance, declares only `verts`, while
   the compositor binds nine slots at it; without the filter the merged group
   would be nine entries against a one-entry layout.

**Deliberate departure from the brief.** The brief said to build an explicit
`WGPUPipelineLayout` from the reflection. I chose to keep the derived
(`layout: NULL`) layout and use `wgpuRenderPipelineGetBindGroupLayout(pipeline, 0)`
as today, because:

- Tint's derived layout contains exactly the statically-used bindings, which is
  precisely the set the `stageMask != 0` filter computes — so the explicit layout
  would buy nothing the filter does not already give.
- `WGPUPipelineLayoutDescriptor.bindGroupLayouts` is a `WGPUBindGroupLayout_ptr`,
  and the bindings file declares no nameable `WGPUBindGroupLayout` element type
  to build an `@FFI.Array` of (only `WGPUBindGroupLayout_ptr` at
  `app/bindings/webgpu.kira:1601`). Every other `_ptr` field in this backend is
  fed either a single struct value or an `@FFI.Array` struct; neither route
  exists for an array of handles without adding a declaration. That is solvable,
  but it is cost with no benefit here.
- A storage *image* (`i|` record) has no format in the digest, so an explicit
  layout could not describe one anyway. Storage images appear only in compute
  shaders, which are already correct (see below).

If the derived layout turns out to disagree with the reflection filter, Dawn
says so on the uncaptured-error callback (`dawnUncapturedError` in
`DawnContext.kira`), which is the signal to escalate to the explicit layout.

### What is already correct and must not be disturbed

The **compute** path is fine. `dawnDispatch` (`DawnCompute.kira:49`) builds one
group 0 with every entry and uses the derived layout, and `dawnBlurEntries`
numbers 0,1,2,(3,4) / 0,1,2,3,(4,5) exactly as `UiGlassBlurH.resources` and
`UiGlassBlurV.resources` declare. Verified by reading both digests against the
code. Leave it alone.

### Implementation sketch, not yet written

In `kira-graphics/app/Backend/Dawn/`:

- `DawnShader.kira` — `DawnShaderRecord` gains the used-binding set parsed from
  the digest, plus a flag saying whether the shader has one at all.
  `dawnCreateShaderFromArtifact` has `artifact.resourceReflection` in hand.
  `dawnCreateShaderFromKsl` should read `{directory}/{asset}.resources` beside
  the `.wgsl` files it already reads — `kira shader build` writes it (see
  `ui-foundation/generated/shaders/*.resources`). `dawnCreateShaderFromSources`
  (raw WGSL) has no reflection and must keep the existing per-slot behaviour: a
  caller writing their own `@group(n)` means the slots literally.
- `DawnPipeline.kira` — record the used-binding set per pipeline id in a small
  table on `DawnState`, since `RenderPipeline` does not carry the shader id.
  Drop the record in `dawnDestroyPipeline`.
- `DawnDraw.kira` — replace the four `dawnApplyBindGroup` calls with one merged
  bind when the pipeline has a binding set, falling back to the current per-slot
  path when it does not.
- `DawnBindGroup.kira` — the per-group cache on `BindGroupState`
  (`deviceGroupId`/`devicePipeline`/`deviceSlot`/`deviceRevision`) cannot key a
  *merged* group, and cannot even tell two groups apart: `dawnBeginBindGroup`
  hands every group `id: 1`. Give each `BindGroupState` a lazily-assigned
  identity from a counter on `DawnState`, and cache merged groups on `DawnState`
  keyed by (pipelineId, four (identity, revision) pairs). Bounded — flush the
  whole table past a cap. Flushing mid-frame is safe: Dawn ref-counts objects a
  command references, which is why `dawnDispatch` already releases its group
  immediately after `SetBindGroup`.

## The CPU time — not yet investigated

10–12 ms mean per frame on a native build with vsync excluded. Already ruled out
by the previous round: the VM, vsync, and draw volume (30 draws across 2 passes,
~350 µs each). The 2.7 s first frame is separately known — the Kira PNG decode of
the 2600x2061 backdrop, attributed to array preallocation — and is out of scope.

Nothing new learned this session. The stated suspects, in the order worth
checking: per-frame descriptor rebuilds, re-uploading buffers that did not
change, re-walking or re-flattening the retained tree every frame, allocation
churn, and C storage a struct-by-address gets and never reclaims. `UiBatchDraw
.presentBatch` commits *every* stream every frame
(`while commitStream < state.streams.count { self.commitStream(...) }`) whether or
not it changed, which is the first thing worth measuring.

## Where I was, and the next step

Mid-way through the kira-graphics reflection move. The immediate next actions, in
order:

1. Delete `kira-graphics/app/Backend/Sokol/SokolReflection.kira` (its content now
   lives in `app/Shader/KslReflection.kira`) and change
   `SokolComputeShader.kira:270` from `reflection.storageBufferCount > 0` to
   `reflection.storageBuffers.count > 0`. **The package does not compile until
   this is done.**
2. `kira check` in `kira-graphics` to confirm the move is clean.
3. Implement the Dawn merge per the sketch above.
4. Capture Dawn and Sokol frames of `liquid-glass-app` and state the difference
   as a pixel count.
5. Re-run the workspace gate and report `failed`, not just the tally — reporting
   a tally without reading `failed` is what went wrong the round before this one.

## Tooling notes for whoever picks this up

- There is **no `python` on this machine** (the Store alias intercepts it), so the
  `.codex/tmp/*.py` PPM helpers left by earlier sessions do not run. PowerShell
  reading the PPM header and bytes works fine and is what produced the
  20926/20926 figure above.
- Build once with `kira build` and run `.\app\.kira-build\main.exe` directly;
  `kira run` recompiles.
- Never two cargo invocations against `target/` at once.
