# Enums

Enums landed on all four backends: payload-less variants, single-payload
variants (`Int`/`Float`/`Bool`/`String`), payload defaults, leading-dot
construction resolved against the expected type, and `==`/`!=` tag comparison.

## The one design decision that saved a backend node

**Enum equality is a desugar, not an operator.** The oracle compares
discriminant tags — even for payload-carrying enums (a tag-only comparison of a
payload enum "would be silent-wrong" only for the *derived* `Equatable`
comparator, not the built-in `==`; the reference's IR lowering inserts
`enum_tag` on both operands and compares integers). So `e == .V` is lowered in
the analyzer to `EqInt(tag(e), tag(.V))`, and the only new runtime primitive is
tag extraction. No backend learns enum equality exists — it *is* integer
equality by the time one sees it.

A payload-less variant literal folds straight to its tag constant, so the common
`c == .Red` never allocates a throwaway enum: it becomes `EqInt(EnumTag(c),
Int(red))`. Only the variable side reads a tag at run time.

## Representation

An enum value is a **heap handle** (like a string or an array), never an inline
value — which is what makes it move on binding and share the seam-refusal story
with structs. Two new nodes carry it end to end: `EnumNew { enum_id, tag,
payload }` and `EnumTag { value }`.

- **VM** (`value.rs`): `Object::Enum { tag: u32, payload: Option<Value> }`.
  `copy_value` deep-clones the payload; `free_enum` drops it. The leak counter
  proves balance. Opcodes `NEW_ENUM { tag, has_payload }` and `ENUM_TAG`
  (append-only, 0x35/0x36).
- **LLVM/native**: a boxed pointer to `KiraEnum { tag: i64, owns_str: i64,
  payload: u64 }`. The payload is one type-erased word plus a flag saying
  whether it is an owned `KStr` to clone/free — so one generic `enum_clone`/
  `enum_free` pair serves every variant, reusing the proven string helpers. New
  `kira_rt_enum_{new,tag,clone,free}` (append-only, no ABI bump).
- **wasm**: a 16-byte box — `i64` tag at offset 0 (wide on both memories, so a
  tag read is one `i64.load`), payload at offset 8. The heap never frees and an
  enum is immutable, so a read shares the handle; construction takes a scratch
  local like a struct build, so `construction_depth`/`expr_depth` count it.

## Scope boundary (what is refused, and why)

- **Non-scalar payloads** (`struct`/`enum`/`array`) are refused at the
  declaration (`KSEM118`). The box carries one word, and an aggregate has no
  form in it yet. Nested-enum and construct payloads (`One(EmxInner)`,
  `Filled(some EmxShape)`) still wait on that, not on `match` — the original
  argument for deferring them was that a payload is unobservable without
  `match`, and `match` has since landed. See
  [match.md](match.md); the payload types it reads are exactly the ones
  `KSEM118` admits.
- **`print(enum)`** (`KSEM081`) and **enum at the native seam**
  (`EnumAtSeam` / `BridgeValueTag::ENUM`) are refused like a struct.

## Ordering note

Enums are collected before structs (a struct field may name an enum:
`struct Box { let c: Color }`). The reverse — an enum payload naming a struct —
is refused anyway, so the single-pass order costs nothing. `Result<Value,
Failure>` (the corpus's only generic enum) is deferred to the generics feature.
