# Byte conversions and the Number type — release work

## Done and verified (VM + native parity)

- `String.bytes() -> [U8]`: `StringOp::Bytes` (wire byte 11). Native
  `kira_rt_string_bytes` builds an i64-slot integer array through `int_array`;
  VM `perform_string_op` builds `Value::Array` of `Value::Int`; comptime
  reflection covers it. `RUNTIME_ABI_VERSION` 17 -> 18, marker renamed to
  `kira_rt_abi_version_18`. Verified `"hi".bytes()` -> `2,104,105` on both.
- `foundation/app/Bytes.kira`: every scalar <-> `[U8]`, LE and BE, both
  directions. Ints via shift/mask, floats via `floatToBits`/`floatToBits32`,
  signed via masking (never `U32(-1)`, which traps — Kira int->unsigned is
  checked). `Int` decode OR-s bytes into an `Int` rather than `Int(u64)` (which
  traps on a top-bit-set value). Verified incl. negatives on VM + native.

## Kira facts learned

- No `F64` spelling: the 64-bit float is `Float`; `F32` is the only narrow one.
- `U32(-1)`/`U64(-1)` trap (checked conversion), so bit patterns come from
  masking, not unsigned casts. `<<` into bit 63 does not trap.
- `U64` literals above i64::MAX cannot be written (parser reads i64 first).
- No top-level `let` constant; String has no byte accessor (hence `bytes()`).

## Remaining in the byte track

- `[U8] -> String` (`String(bytes)`): needs a new `StringFromBytes` node through
  HirExpr + IrExpr + HIR->IR lower + VM opcode/interp + bytecode + LLVM
  declare/lower + native `kira_rt_string_from_bytes(array, esize)` + comptime.
  Native reads each element as i64 (see `kira_rt_fs_write_bytes`).
- tests-kik coverage for `bytes()` and the Foundation module.
- `sites/docs` for the byte surface.

## Number type — DONE, exact decimal, i64 mantissa (verified vm/llvm/hybrid)

Shipped and verified: `0.1 + 0.2 == 0.3` exact on all three backends, arithmetic,
comparison, `Number(Int|String|Float)` construction, `String|Int|Float(n)` out,
half-to-even division, overflow/divide-by-zero traps.

- Representation: `i64` mantissa + `u32` scale, `MAX_SCALE = 18`. `i128` only
  inside multiply/divide intermediates. Core in `kira-runtime-abi/src/number.rs`
  (`Decimal`). Multiply has an i64 checked fast path; divide uses an f64 hint
  verified by an i128 multiply-back with an exact-divide fallback (differential
  test proves equivalence), and reduces the result scale to fit rather than
  overflowing (`100/4 = 25`).
- Native value representation: **inline `i128`** (mantissa low 64, scale high
  64), not a heap handle — trivially copyable, no allocation, no clone/free. This
  matched `Number` to `Float` speed on native (fib(90) benchmark: boxed 47 ms ->
  pooled 37 ms -> inline 8 ms == Float's 8 ms), while staying exact where Float
  drifts past 2^53. `owns_heap(Number) = false`, `is_trivially_copyable = true`.
  The VM keeps `Value::Number(Decimal)` inline; both go through the one `Decimal`.
- `NumberOp` (16 ops) in `kira-runtime-abi/src/number_op.rs`; one `NUMBER_OP`
  bytecode opcode (0x93) with an operand byte, like `StringOp`.
- HIR/IR `NumberOperation`; VM `Value::Number(Decimal)` inline +
  `interp/numbers.rs`; native `kira_rt_number_*` a **share-counted** box
  (`native-bridge/src/number.rs`) — clone bumps the count and returns the same
  pointer (immutable, safe to share); LLVM `number_ops` table + retain/release
  through the count. `RUNTIME_ABI_VERSION` is 18.
- Semantics: `Number(x)` (conversions.rs), decimal operators (typeck.rs
  `analyze_number_binary`), `String/Int/Float(n)` out. No new syntax, so
  tree-sitter is untouched.
- `tests-kik` `Number` section (parity-checked); docs
  `language-guide/numbers-and-decimals.mdx`.

### Hybrid bridge — DONE
`Number` crosses the VM<->native hybrid boundary as a native-value node (its two
words). Verified: `@Native` functions taking and returning `Number`, both
directions and chained, identical on vm/llvm/hybrid. Wired through
`NativeStateValue::Number` (tag 14), `kira_rt_native_value_number` /
`_read_number` (added to `HYBRID_HOST_SYMBOLS`), the hybrid library's
encode/decode + `state_value_number`/`state_value_read_number` callbacks, the VM
seam (`host.rs`/`into_native_state`), and `bridge.rs` (NODE). `Number` is native-
state-*ineligible* (callback state) still, which is a separate feature; an
`@Native` returning an aggregate (struct) has the same seam limit `Number` does
not. Unary `-` on `Number` is wired (`NumberOp::Negate`).

Still open (niche, not a backend): `Number` as `@Export` type and as `@Native`
callback state (`nativeRecover`) — both refused with typed diagnostics.

## Number type — decimal, i128 mantissa + scale (original plan)

Representation: handle-based, like `String`/`Array` — the 64-bit value slot holds
a handle to a heap `Decimal { mantissa: i128, scale: u8 }`. Inline 128-bit is
rejected: it would widen the universal value slot everywhere.

Surface to build, green at each step, VM first then native/hybrid/wasm:

1. `Type::Number` (new family beside Int/Float) + `NumberSpelling` if needed;
   `Type::from_name("Number")`, assignability, `is_numeric` interplay.
2. Lexer/parser: a decimal literal that is a Number, not a Float. Likely a
   suffix or a context rule; a bare `0.1` is a Float literal today, so Number
   literals need a distinct spelling (decide: suffix `0.1d`, or `Number(0.1)`
   from a String/Float, or typed context). Tree-sitter grammar in lockstep.
3. Runtime rep: heap `Decimal` in VM (`Value::Number`), native handle
   (`kira_rt_number_*`). ABI bump again.
4. Arithmetic: add/sub align scales exactly; mul adds scales; div needs a target
   scale + rounding mode (banker's vs half-up — decide and pin). Compare.
   Overflow on i128 traps like other checked arithmetic.
5. Print: shortest exact decimal. Conversions Number <-> Int/Float/String/[U8].
6. tests-kik: `0.1 + 0.2 == 0.3`, rounding, overflow, round-trips. docs.

Div rounding and overflow are where a decimal ships subtly wrong; they need the
most care and the most tests.
