# Kira day-to-day friction log

Running list of papercuts hit while working in Kira — syntax surprises, lowering
gaps, CLI/TUI annoyances, measurement traps. Each entry: what happened, why it
hurt, and a concrete fix idea. Newest at the top.

---

## Performance / native boundary

### `@Native` marshaling of a struct-array is catastrophically deep (the native-engine trap)
- **What:** Passing a VM `LayoutTree` (≈470 `LayoutNode` structs in an array) to a no-op
  `@Native` function costs **~45ms/frame** — the hybrid bridge deep-copies the
  struct-array into native layout (and back) field by field. A flat `[Float]` array of
  the *same byte size* marshals in **~2ms** round-trip (POD memcpy). `nativeRecover`
  (VM→VM) is cheap by comparison — it materializes handles, not a layout conversion.
  These are very different costs and easy to conflate.
- **Hurt:** This is THE blocker for running a hot subsystem (the layout engine) natively
  to escape VM interpretation. The obvious move — make the engine `@Native`, pass it the
  tree — is ~8× slower than the VM relayout it would replace. The only viable native path
  is a flat-`[Float]` (or raw-buffer) representation, i.e. rewrite the engine to index a
  flat buffer instead of struct fields — a large change. Measured the whole 120fps
  effort against this and it caps a native engine at ~80-90fps anyway (2ms marshal +
  ~0.5ms native compute vs 5.5ms VM), so the real lever is incremental/axis-split layout,
  not native execution.
- **Fix idea:** A zero-copy / shared-representation marshal for POD struct-arrays across
  the native boundary (pass a pointer + element layout, don't deep-copy), or a documented
  `[Float]`/raw-buffer pattern for hot data that must cross into `@Native`. At minimum,
  document that struct-array marshal is O(fields × elements) deep so nobody designs a
  per-frame native call around it.

### Adding one VM superinstruction means editing ~6 exhaustive switches
- **What:** Adding `fused_array_field_load` (a decode-time-only fused instruction) required
  touching: the `Instruction` union, the `OpCode` enum, serialization *write*,
  serialization *read*, `countRegisterReads`, the read-register predicate, the interpreter
  dispatch, and the debug-print dump. Each exhaustive `switch` fails the build until the
  new tag is added — found them one compile error at a time.
- **Hurt:** Mechanical but slow; ~5 build-edit cycles just to satisfy switches, none of
  which is the actual logic. Easy to miss one and ship a latent crash if a switch had an
  `else`.
- **Fix idea:** Group the VM-internal fused variants behind a comptime helper / a single
  `is_fused` predicate, or a tagged sub-union so the serializer/analysis can reject them
  with one arm instead of N. Or a test that enumerates `OpCode` and asserts each pass
  handles it.

## Lowering / language

### A `nativeRecover`'d state cannot be passed to a `borrow`/`borrow mut` parameter
- **What:** `var s = nativeRecover<UiBatchState>(ptr)` then `helper(s)` where
  `helper(state: borrow mut UiBatchState)` fails with
  `error[KSEM031]: type mismatch — expected UiBatchState here, but the value resolves
  to UiBatchState`. The recovered value is a distinct *native-state-view* kind; it is
  only usable through direct field access (`s.field`), not as a normal `UiBatchState`
  value/borrow. (A *local* `var s = UiBatchState{}` passes to the same param fine —
  e.g. `uiBatchStateLoadFont(state)` in UiBatch.kira.)
- **Hurt:** Blocks the obvious "recover once, thread the state through per-element
  helpers" refactor. Forces either (a) recovering inside every helper — which is the
  perf problem below — or (b) fully inlining all helpers into the caller (huge
  functions, Core-Law-#5 pressure). Cost ~an hour and a dead-end refactor.
- **Fix idea:** Allow a recovered native-state view to coerce to `borrow mut T` /
  `borrow T` of the underlying struct (it already *is* a mutable reference to it), or
  add an explicit `&state` / reborrow form. The confusing "expected T, got T" message
  should at least name the *kind* difference (native-state-view vs value).

### `nativeRecover` per call is the dominant cost in element-wise native loops
- **What:** Each `nativeRecover<T>(handle)` materializes the state struct. The UI
  compositor calls `uiBatchEmitQuad`/`uiBatchEnsureGlyph`/`uiBatchGlyphSlot` once per
  glyph, each recovering independently → ~3-7 recoveries per character, ~3000+/frame
  for a text-heavy UI. Measured ~4ms of a 22ms resize frame (the single biggest render
  cost) is this, NOT the per-float buffer writes or the glyph lookup.
- **Hurt:** You can't amortize it (see the borrow-param limitation above), so the only
  fixes are intrusive: inline everything into one function, or move the hot data out of
  the recovered struct into raw memory (which the code already had to do once for the
  27k-float vertex array — see the comment in `UiBatchState.contentsPtr`).
- **Fix idea:** Make `nativeRecover` return a cheap reference/view (no per-call
  materialization), or special-case "recover once, mutate in place, write back once"
  so a hot loop doesn't re-marshal the whole struct each iteration.

### No `U64()` / width-specific numeric cast constructor
- **What:** `Int(x)`, `Float(x)`, `U32(x)` exist, but `U64(x)` →
  `error[KSEM010]: unknown call target — could not find a function named 'U64'`.
  (Int→U64 *does* auto-convert when passed to a `U64` parameter, so the cast is usually
  unneeded — but its absence is surprising when you reach for it.)
- **Fix idea:** Provide the full set of width casts, or document that argument-position
  implicit widening covers it.

### Per-float FFI is the only way to fill a GPU buffer (no bulk write)
- **What:** Filling the shared vertex buffer goes through `kira_dynamic_write_f32_at`
  one float at a time (92 FFI calls per quad). For text (one quad per glyph) that is
  ~92k native-boundary crossings/frame. Had to add a `kira_ui_write_quad` C helper that
  writes a whole quad (4×23 floats) in one call.
- **Fix idea:** A generic bulk writer (`kira_dynamic_write_f32_span(ptr, offset, [f32])`
  or memcpy-from-Kira-slice) in the runtime ABI so app code doesn't hand-roll one per
  vertex shape.

### `move state.field` of a closure-bearing struct field silently breaks lowering
- **What:** Adding a *second* `FoundationView` field to a `nativeState` struct (one
  already existed as `root`) made the whole program fail with `KIR001`. `FoundationView`
  carries function-typed fields (closures) + a `FoundationInteraction`. A single such
  field lowers; two does not.
- **Hurt:** Cost ~an hour of bisection. The construct that breaks (the field) is far
  from where the error surfaces.
- **Fix idea:** Either lower N closure-bearing aggregate fields in `nativeState`, or —
  at minimum — emit a diagnostic that names the *field/type* that exceeded what the
  native-state round-trip supports, not a generic KIR001 on the entry file.

### KIR001 has no span — points only at the entry file
- **What:** `error[KIR001]: feature is not executable in the current backend pipeline`
  prints `--> .../app/main.kira` with no line/column, even though the offending
  construct is deep in an imported library function.
- **Hurt:** Can't locate the construct from the message; have to bisect by hand
  (delete/comment code until it compiles).
- **Fix idea:** Thread the HIR node span of the unlowered construct through
  `error.UnsupportedExecutableFeature` so the diagnostic points at the actual
  expression/function, with a short note on *which* construct (e.g. "move of struct
  field `x`", "borrow mut of field `y` as call argument").

### Borrow-elision is defeated by reads across calls (the big one for recursive code)
- **What:** The VM's bind-elision (and any extension to `let x = arr[i]` / `let x = s.field`)
  is barrier-span based: it can only alias a binding when *every* read happens before the
  next barrier, and a `call_runtime` (any function call) is an opaque barrier. Recursive
  tree code reads an aggregate, recurses into children, then reads the aggregate again —
  so the binding can never be aliased across the recursion, even when the callee provably
  can't touch the binding's source (it isn't passed to the callee).
- **Hurt:** This is the entire layout engine. `measure()` does `let desc = node.descriptor`,
  recurses into child measures, then reads `desc.width`/`desc.padding`. The descriptor copy
  (the dominant per-node cost, ~66k structs/frame on resize) is fundamentally un-elidable
  this way. Killed a planned VM-elision generalization that would otherwise "help a lot."
- **Fix idea:** A non-conservative call-barrier: a call is *not* a barrier for a binding
  whose source local is not reachable by the callee (not passed as an arg, not a global).
  Much harder than the syntactic span analysis, but it's what recursive aggregate code needs.
- **Also:** even with the descriptor aliased, reading `desc.width` (a `SizeMode`) still
  copies the enum, and `desc.padding` (an `EdgeInsets`) still copies the struct — aliasing
  the *parent* aggregate doesn't make *field* reads free. So for value-heavy hot loops the
  real lever is interning immutable values (shared pointer instead of copy), not elision.

### No borrow binding for locals (`let x = borrow expr`)
- **What:** There's `borrow`/`borrow mut` for *parameters*, but no way to bind a local
  as a read-only alias of a place. `let node = result.nodes[index]` always deep-copies.
- **Hurt:** Hot loops (layout engine) that only *read* an array element / struct field
  pay a full aggregate copy per access. This is the single biggest cost in the
  UI-Foundation resize path (~66k struct copies/frame).
- **Fix idea:** A `let x = borrow place` binding (read-only alias, dropped at scope
  end, source must outlive it), or — better — make the VM's existing loop-element
  bind-elision also cover `let x = array[i]` / `let x = struct.field` when provably
  read-only. (Working on the latter.)

### Array-element access copies the whole element every time
- **What:** `result.nodes[i].fieldA` then `result.nodes[i].fieldB` copies the whole
  `nodes[i]` aggregate twice. Reading 5 scalar fields off one element = 5 element copies,
  which is *worse* than `let node = result.nodes[i]` (one copy).
- **Hurt:** The "obvious" refactor (avoid the `let`, read fields directly) pessimizes.
- **Fix idea:** Project to the accessed field without materializing the parent
  aggregate when the result is a scalar / is only read.

## CLI / measurement

### `print` output is lost when stdout is redirected to a file
- **What:** `kira run ... > out.txt` drops `print` output (block-buffered, not flushed
  on the offscreen/early-return path). `kira_live_emit_log_line` markers survive
  (they `fflush`), `print` does not.
- **Hurt:** Benchmarks that `print` a result (e.g. ns/frame sentinel) show nothing;
  looks like a hang/failure.
- **Workaround:** Run under a pseudo-tty: `script -q FILE env ... kira run ...`.
- **Fix idea:** Flush stdout on runtime shutdown / before `_exit` on every exit path.

### Offscreen bench needs two env vars and it's not obvious
- **What:** The Metal frame-time bench only runs when BOTH `KIRA_METAL_OFFSCREEN=1` and
  `KIRA_METAL_BENCH=1` are set (the bench lives inside the offscreen branch). Setting
  only `KIRA_METAL_BENCH` runs the on-screen loop forever.
- **Fix idea:** Let `KIRA_METAL_BENCH` imply offscreen, or document the pair in one place.

## Toolchain (Zig side, for contributors)

### `std.ArrayList` is unmanaged in the pinned Zig (0.16) — `.init(allocator)` fails
- **What:** `std.ArrayList(T).init(alloc)` → `has no member named 'init'`. Must use
  `std.ArrayListUnmanaged(T) = .empty` + pass the allocator to every `append`/`deinit`.
  (`std.StringHashMap(...).init(alloc)` is still the managed form — inconsistent.)
- **Fix idea:** Nothing actionable in-repo; just a note for anyone adding Zig code.
