# Source Value and Operation Audit

## Audit scope

This is a read-only audit of the working tree. No files were modified.

The repository package version is `1.8.3` in `Cargo.toml:6`. The authoritative `.kira` path is:

`std::fs::read_to_string` → `kira_lexer::lex` → `kira_parser` AST → semantic/type/ownership analysis → HIR → MID scope/drop processing → `kira-ir` → VM or LLVM/native. Hybrid uses both paths; Web uses LLVM/emscripten.

Evidence:

- Lexer entry point: `crates/kira-parser/src/lib.rs:67-84`
- AST expression set: `crates/kira-syntax-model/src/ast/expr.rs:8-324`
- AST statement set: `crates/kira-syntax-model/src/ast/stmt.rs:9-205`
- HIR expressions: `crates/kira-semantics-model/src/hir/exprs.rs:19-320`
- IR lowering: `crates/kira-ir/src/lower.rs:27-107`
- LLVM expression lowering: `crates/kira-llvm-backend/src/codegen/lower/expr/core.rs:11-142`
- Backend parity requirement: `sites/docs/content/docs/language-reference/execution-and-feature-status.mdx:6-43`

`crates/kira-ksl-parser` is for `.ksl` shaders and must not be used as evidence for `.kira`.

The repository is dirty from other work. Some comments and docs appear to describe in-progress changes. The findings below distinguish actual parser/semantic behavior from documentation claims.

## Decision vocabulary

- `KEEP`: existing behavior is intentional and should remain.
- `CHANGE`: existing implementation, docs, or grammar disagree and must be aligned.
- `ADD`: recommended for Kira 1.9.1.
- `DEFER`: real language problem, but requires a larger design.
- `REJECT`: intentionally not part of Kira’s design.

## Shared implementation and test notation

`B0` means the existing frontend/backend path above.

`Tlex` means lexer tests in `crates/kira-lexer/src/lib.rs`.

`Tpar` means parser tests in `crates/kira-parser/src/tests`.

`Tsem` means semantic tests in `crates/kira-semantics/src/tests`.

`Tkik` means executable Kira tests in `tests-kik/harness/app`.

`Tparity` means VM/LLVM/Hybrid parity tests in `crates/kira-cli/tests/backend_parity`.

Every existing behavior should have a `Tkik` test as required by `AGENTS.md`; several parser/semantic behaviors currently lack corresponding harness coverage.

# Lexical and source language

## Identifiers and naming rules

- Rust problem: identify bindings, types, modules, fields, and labels while preventing ambiguous or unsafe names.
- Kira today: identifiers are ASCII `[A-Za-z_][A-Za-z0-9_]*`; `_` is an ordinary identifier. The lexer implements this in `crates/kira-lexer/src/lib.rs:114-126`. Qualified names are assembled from dotted identifier tokens by `crates/kira-parser/src/item/type_refs.rs:392-472`.
- Status: `KEEP`.
- Difference: Kira deliberately chooses ASCII names. Rust’s Unicode identifier model is not required by current Kira interop or macro design.
- Exact surface: `value`, `_`, `value_2`, `Geo.Point`, `FFI.Pointer`.
- Semantics: names are interned `Symbol`s. A dot in a type/name path is not part of one identifier and cannot collide with a declared single-segment name. `_` can currently be declared and referenced.
- Implementation: `B0`; no ABI impact.
- Tests: positive `let value_2 = 1`, `Geo.Point`; negative non-ASCII `let café = 1`; verify `_` remains a legal local.

## Keywords and contextual keywords

- Rust problem: reserve syntax words while permitting contextual words in ordinary names.
- Kira today: reserved token kinds are listed in `crates/kira-syntax-model/src/token.rs:32-174,177-214`. `distinct` is reserved in the lexer. `handle` is intentionally contextual. `move`, `copy`, `borrow`, `mut`, `some`, `Any`, `async`, `extend`, `macro`, `comptime`, `init`, `requires`, `lifecycle`, `self`, and `For` remain identifier tokens and gain meaning by parser context. `async function` is recognized at `crates/kira-parser/src/item.rs:79-85,168-175`.
- Status: `CHANGE` for documentation; `KEEP` for the contextual design.
- Difference: the docs keyword list omits `distinct` at `sites/docs/content/docs/language-reference/lexical-structure.mdx:17-25`. `self` is not lexically reserved even though receiver parsing is special.
- Exact surface: `let move = 1`; `async function work() {}`; `function bump(borrow mut self) {}`; `attempt {} handle {}`; `some Widget`; `For(x in xs) {}`.
- Semantics: contextual recognition must be position-sensitive. A local called `move` is valid, while `move value` is an ownership operation. `handle` is ordinary outside an `attempt`.
- Implementation: update docs and tree-sitter keyword metadata. No ABI impact.
- Tests: `let move = 1`; `let copy = 2`; `let handle = 3`; `async function f() {}`; `distinct Id = U32`; verify docs and tree-sitter include `distinct`.

## Escaped and raw identifiers

- Rust problem: refer to a name that collides with a keyword or contains otherwise reserved spelling.
- Kira today: no `r#name`, backtick identifier, Unicode escape, or identifier escape. `r#type` lexes as unknown `#` followed by an identifier.
- Status: `REJECT` for 1.9.1.
- Difference: Kira’s contextual words already avoid many keyword collisions. Interop names are handled by FFI annotations and exported names, not source-level raw identifiers.
- Exact surface: no raw identifier syntax. Use a contextual word as an ordinary name where permitted.
- Semantics: adding this later would require spelling preservation, symbol identity, macro hygiene, Unicode normalization/security rules, and tree-sitter agreement.
- Implementation: none for 1.9.1.
- Tests: negative `let r#type = 1`; negative ``let `type` = 1``.

## Comments and documentation comments

- Rust problem: discard comments for execution while retaining documentation for tooling.
- Kira today: `//` comments and whitespace are skipped by `crates/kira-lexer/src/lib.rs:76-91`. There are no lexer comment tokens. `kira doc` rescans original source because comments are absent from the AST (`crates/kira-doc/src/lib.rs:1-5,27-42,120-140`).
- Status: `KEEP`.
- Exact surface: `// comment`, `/// API documentation`.
- Semantics: consecutive `///` lines immediately before a declaration attach to it. Annotations may intervene. Comments never affect expression evaluation.
- Implementation: `B0`; documentation extraction is a separate source-span pass.
- Compatibility: no runtime or ABI effect.
- Tests: `crates/kira-lexer/src/lib.rs:412-590`; `crates/kira-doc/src/lib.rs:162-184`; `Tkik` should contain one documented public declaration.

## Whitespace, newlines, and statement termination

- Rust problem: separate tokens and statements without making formatting affect meaning unexpectedly.
- Kira today: ASCII whitespace, including newlines, is skipped; the lexer emits no newline token (`crates/kira-lexer/src/lib.rs:76-91`). Semicolons are tokens. The parser recognizes statement boundaries structurally.
- Status: `CHANGE`.
- Difference: docs claim “a statement ends at a newline or `;`” (`lexical-structure.mdx:69-74`), but the compiler cannot distinguish a newline from a space. Arrays, structs, and enum declarations accept omitted commas; ordinary call argument parsing still requires commas in `crates/kira-parser/src/expr/calls.rs:174-190`.
- Exact surface today: `let a = 1; let b = 2`; also many forms with whitespace only, such as `enum Color { Red Green }` and `[1 2 3]`.
- Decision: define newline as trivia and describe structural termination accurately. Either make all documented optional-separator forms parser-supported or remove claims about optional call separators.
- Semantics: formatting must not change expression grouping. Braces after `if`, `while`, and `for` are statement bodies, while braces after constructions can be content or closures.
- Implementation: docs and tree-sitter alignment; if newline-sensitive termination is desired, that is a parser/lexer redesign.
- Tests: positive enum/array/struct omitted separators; negative ambiguous `f(1 2)` unless parser intentionally adds it; verify `if flag {}` is not a struct literal.

## Attributes and annotations

- Rust problem: attach compile-time metadata to declarations.
- Kira today: annotations are parsed as `@Name`, qualified `@FFI.Extern`, optional parentheses, and an optional balanced block for `@Export` (`crates/kira-parser/src/item.rs:318-413`). Supported annotations are documented at `sites/docs/content/docs/language-reference/annotations-reference.mdx:6-52`.
- Status: `KEEP`.
- Exact surface: `@Main`, `@Runtime`, `@Native`, `@Export`, `@Derive(Copy)`, `@FFI.Extern { library: l; symbol: s; abi: c; }`, `@FFI.Struct { layout: c; }`.
- Semantics: annotations are compile-time declarations, not runtime values. `@Main`, `@MainThread`, and execution annotations affect reachability/backend selection. FFI annotations define ABI/layout.
- Implementation: parser annotation collection, semantic validation, macro/comptime frontend, FFI lowering. Existing backend parity applies.
- Compatibility: FFI annotations affect ABI and must preserve explicit field/layout rules.
- Tests: `Tkik` FFI, macro, main-thread, and derive suites; negatives for annotation payloads on `@Export`, annotations on ordinary declarations, and invalid FFI fields.

## Source encodings

- Rust problem: convert source bytes consistently and report invalid encodings.
- Kira today: build input uses `std::fs::read_to_string` (`crates/kira-build/src/frontend.rs:202-205`); lexer accepts `&str` and scans bytes. There is no encoding declaration or BOM handling.
- Status: `CHANGE`.
- Difference: valid UTF-8 inside strings is preserved; non-ASCII outside strings becomes unknown bytes. Invalid UTF-8 fails before lexing. A UTF-8 BOM is currently not specially recognized.
- Exact surface: UTF-8 source without an encoding declaration.
- Decision: document UTF-8 source explicitly and diagnose BOM/non-ASCII identifier use consistently. Do not add source encodings for 1.9.1.
- Implementation: build diagnostic/documentation alignment; no ABI effect.
- Tests: invalid UTF-8 read failure; BOM source; Unicode string positive; Unicode identifier negative.

## Reserved syntax

- Rust problem: reject syntax reserved for language evolution or other constructs.
- Kira today: `switch`, `case`, and `default` are not Kira constructs; `_` and `else` are not match arms; Rust `?` is not error propagation; `annotation`, `capability`, generic declaration members, and class subtyping are refused. See `sites/docs/content/docs/for-llms.mdx:30-45`.
- Status: `REJECT`.
- Exact surface: use `match`, explicit variants, `attempt`/`try`/`handle`, traits, or construct families.
- Semantics: rejected syntax must produce typed diagnostics rather than be silently interpreted as another construct.
- Implementation: parser recovery and semantic diagnostics; no ABI impact.
- Tests: each documented refusal must have parser/semantic and `Tkik` negative coverage.

## Punctuation and delimiters

- Rust problem: delimit expressions, lists, blocks, operators, and annotations.
- Kira today: two-character tokens are `->`, `==`, `!=`, `<=`, `>=`, `&&`, `||`, `<<`, `>>`, and `..`; one-character tokens include `(){}[],;:.@?=+-*/%<>&|^~!` (`crates/kira-lexer/src/lib.rs:227-293`).
- Status: `KEEP`, with grammar/docs alignment as `CHANGE`.
- Exact surface: `@`, `()`, `{}`, `[]`, `;`, `,`, `:`, `.`, `..`, `=`, `->`, `?`, arithmetic, bitwise, comparison, and logical operators.
- Difference: no `::`, `=>`, `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `..=`, or standalone `#`. `#{...}` is read from raw macro source rather than lexed as ordinary Kira.
- Semantics: delimiters are context-sensitive: `{}` can be a statement block, struct literal, closure, content block, or task body.
- Implementation: update `editors/tree-sitter/grammar.js` whenever syntax changes. Current grammar says newline is fully insignificant (`grammar.js:74-80`) and its escape rule disagrees with the compiler.
- Tests: lexer all punctuation; negative `x += 1`, `x ..= y`, `a::b`, ordinary `#`.

# Literals

## Integer literals

- Rust problem: express exact integer constants in readable source.
- Kira today: decimal digits and hexadecimal `0x`/`0X` are recognized (`crates/kira-lexer/src/lib.rs:128-164`). The AST stores `i64` (`expr.rs:10-17`). Decimal parsing uses `i64`; hexadecimal parses `u64` and reinterprets bits as `i64` (`crates/kira-parser/src/expr.rs:403-428`).
- Status: `KEEP`.
- Exact surface: `0`, `42`, `0x2a`, `0xffffffffffffffff`.
- Semantics: decimal is a signed number. Hexadecimal is a 64-bit bit pattern, so `0xffffffffffffffff` becomes `-1`. A leading `-` is a separate unary operation.
- Implementation: existing parser/HIR/IR/backend constants.
- Tests: `crates/kira-parser/src/tests/expressions.rs:26-58`; `Tkik` scalar tests; test `0xffffffffffffffff == -1`.

## Signed and unsigned inference

- Rust problem: choose a usable integer type without forcing suffixes everywhere.
- Kira today: every integer literal starts as plain `Int` spelling. Bare `Int` is a wildcard compatible with fixed integer spellings. `let x: U8 = 5` is valid without `5u8`. Type spelling controls signedness of division, remainder, ordering, and right shift, not storage width (`crates/kira-semantics-model/src/ty/scalars.rs:1-65`).
- Status: `KEEP`.
- Exact surface: `let n: U8 = 5`; `let q: U32 = a / b`; `u8Value / 2`.
- Semantics: fixed-width names remain type-distinct. Two different written widths do not implicitly widen. Arithmetic still uses a 64-bit runtime representation; `U8` arithmetic does not truncate to eight bits. `/`, `%`, and ordering use the left operand’s signedness.
- Implementation: `Type`, `resolve_binary`, and HIR operator selection (`crates/kira-semantics/src/operators.rs:33-82,133-191`).
- Tests: `Tsem` widths/widening/operators; `Tkik` `CfxScalarExtraTests*.kira`.

## Decimal, hexadecimal, octal, and binary bases

- Rust problem: write values in bases useful for humans and bit-level work.
- Kira today: decimal and hexadecimal only. `0xyz` intentionally lexes as integer `0` plus name `xyz` (`kira-lexer/src/lib.rs:130-143`).
- Status: `REJECT` octal and binary for 1.9.1.
- Difference: hexadecimal already supplies bit-pattern notation; Kira’s fixed-width type names and `Int`/`U64` conversions cover current numeric use.
- Exact surface: `42`, `0x2a`; no `0o52` or `0b101010`.
- Semantics: adding bases later must define overflow, expected-type treatment, separators, and diagnostic recovery independently.
- Implementation: none for 1.9.1.
- Tests: negative `0o10`, `0b10`; positive decimal/hex boundary tests.

## Digit separators

- Rust problem: make large literals readable without changing value.
- Kira today: `_` terminates an identifier or number; `1_000` is currently tokenized as integer `1` and identifier `_000`.
- Status: `ADD`.
- Exact proposed surface: `1_000`, `0xFF_FF`, `1_000.25`.
- Semantics: separators are ignored in numeric value. They cannot begin/end a digit run, repeat, or occur directly beside a radix prefix. Preserve existing `1.foo` and `1..2` disambiguation.
- Implementation: lexer number scanner, parser normalization, tree-sitter numeric regex, docs, parser tests, `Tkik`, and backend parity. HIR/IR/backend representations do not change.
- Compatibility: previously invalid source becomes valid; existing valid programs remain unchanged if malformed forms preserve token boundaries.
- Tests: positive decimal/hex/float separators; negative `1_`, `_1`, `0x_FF`, `1__0`; verify `1.foo` and `1..2`.

## Literal suffixes

- Rust problem: select literal type at the spelling site.
- Kira today: no integer or float suffixes. Width is selected by the expected type or explicit conversion.
- Status: `REJECT`.
- Difference: suffixes would duplicate Kira’s type names and undermine the existing wildcard rule.
- Exact surface: `U32(1)`, `let n: U32 = 1`; no `1u32`, `1_i32`, or `1.0f32`.
- Semantics: explicit conversions remain visible and checked.
- Implementation: none.
- Tests: negative `1u32`, `1_i32`, `1.0f32`.

## Integer overflow at compile time

- Rust problem: reject an integer constant that cannot be represented.
- Kira today: decimal or hexadecimal values outside 64 bits report `KPAR021` and produce an error placeholder (`expr.rs:417-428`).
- Status: `KEEP`.
- Difference: because negative sign is unary, `-9223372036854775808` currently parses an overflowing positive literal and is rejected. This is unlike Rust’s special minimum-value handling.
- Exact surface: valid `9223372036854775807`; invalid `9223372036854775808`; valid hexadecimal bit pattern up to 64 bits.
- Semantics: no runtime integer overflow diagnostic; arithmetic is 64-bit wrapping behavior as currently specified.
- Implementation: parser only for literal overflow; backend constants unchanged.
- Tests: decimal max/min, hex max, 65-bit hex, negative minimum edge.

## Float literals

- Rust problem: express finite floating-point constants.
- Kira today: only digits-dot-digits, such as `3.5`, are lexed as floats. Parsing uses `f64` and rejects non-finite results (`crates/kira-parser/src/expr.rs:431-445`).
- Status: `KEEP`.
- Exact surface: `0.0`, `3.1415`; not `.5` or `1.`.
- Semantics: literals are finite `Float` spelling; `F32` is a type spelling with the same 64-bit runtime representation.
- Implementation: parser/HIR/IR/backend already support.
- Tests: `StrxLiteralTests.kira`, scalar parser tests, negative `.5`, `1.`.

## Float exponent syntax

- Rust problem: express very small or large finite values compactly.
- Kira today: `1e5` lexes as integer `1` plus identifier `e5`; no exponent scanner exists.
- Status: `ADD`.
- Exact proposed surface: `1e5`, `1E+5`, `1.25e-3`, `1_000.0e2`.
- Semantics: require digits after exponent marker and optional sign. Reject `1e`, `1e+`, `1e-`. Continue rejecting values whose parsed result is infinite.
- Implementation: lexer/parser/tree-sitter/docs/tests only; use the existing `Float` AST/HIR/backend.
- Compatibility: previously invalid source becomes valid; no ABI impact.
- Tests: positive exponent forms and finite boundaries; negative malformed exponents, `1e309`, `1.foo`, and `1..2`.

## NaN and infinity semantics

- Rust problem: represent IEEE exceptional results and define comparisons.
- Kira today: no `NaN` or infinity literal. Runtime floating operations can produce them; `tests-kik/harness/app/CfxScalarExtraTestsPart3.kira:400-448` proves `1.0 / 0.0` yields infinity and `0.0 / 0.0` yields NaN, with NaN unequal to itself.
- Status: `KEEP` runtime semantics; `REJECT` dedicated literal syntax for 1.9.1.
- Exact surface: `1.0 / 0.0`; no `NaN` or `inf` literal.
- Semantics: preserve IEEE behavior and `NaN != NaN`. Do not silently accept identifier names unless a library function provides them.
- Implementation: existing float backend/VM operations.
- Tests: positive infinity/NaN arithmetic; comparison tests; negative `let x = NaN`.

## Boolean literals

- Rust problem: express truth values without numeric coercion.
- Kira today: `true` and `false` are lexer keywords and AST `Bool` expressions (`crates/kira-parser/src/expr.rs:367-379`).
- Status: `KEEP`.
- Exact surface: `true`, `false`.
- Semantics: `Bool` is not numeric; no truthiness. `&&`, `||`, `!`, and comparisons require/produce `Bool`.
- Implementation: existing HIR/IR/backend.
- Tests: `PrimitiveTests.kira`, logic parity; negative `if 1 {}` and `Int(true)`.

## Character literals and character type

- Rust problem: express Unicode scalar values distinctly from strings and integers.
- Kira today: no `Char` type or character literal. String APIs expose bytes; Unicode scalar conversion uses `scalarText(Int)`.
- Status: `REJECT` for 1.9.1.
- Difference: Kira’s strings are byte-oriented and existing FFI/collection APIs use bytes. Adding `Char` would require a new type, literal grammar, indexing contract, and ABI policy.
- Exact surface: `scalarText(233)`; no `'a'`.
- Semantics: `String.charAt` returns an integer byte; `scalarText` validates scalar ranges and renders a string.
- Implementation: none for 1.9.1.
- Tests: positive scalar conversion; negative `'a'`, `Char('a')`.

## String literals

- Rust problem: express owned text with escapes.
- Kira today: double-quoted strings only; seven escapes and escaped-newline continuation are decoded by `crates/kira-lexer/src/lib.rs:166-225,296-349`.
- Status: `KEEP`.
- Exact surface: `"text"`, `"\n\t\r\e\0\"\\\\"`.
- Semantics: strings are heap-owned byte strings. `count`, `charAt`, `substring`, and `indexOf` operate in bytes. Unknown escapes produce `KLEX003`; the current decoder preserves the escaped character after reporting.
- Difference needing alignment: tree-sitter’s escape rule accepts any escape (`grammar.js:1153-1155`) while the compiler diagnoses unknown escapes.
- Implementation: existing string HIR/IR/runtime; grammar/docs should match compiler.
- Tests: `Tkik` `StrxLiteralTests.kira`, `StrxStringTests*.kira`; unknown escape, embedded NUL, unterminated string.

## Raw strings

- Rust problem: write text without escape processing.
- Kira today: no raw-string delimiter.
- Status: `DEFER`.
- Decision: escaped-newline strings currently cover multiline messages. Raw delimiter design would need delimiter nesting, diagnostics, indentation rules, and tree-sitter support.
- Exact surface: none.
- Implementation: future lexer/parser/docs/tree-sitter/test work; no backend change.
- Tests if added: `r"..."`, hashes, embedded quotes, unterminated delimiters.

## Multiline strings

- Rust problem: write text spanning source lines.
- Kira today: a backslash immediately before LF or CRLF continues a string and contributes no newline or indentation (`kira-lexer/src/lib.rs:177-213`).
- Status: `KEEP`.
- Exact surface: `"first\\\n    second"`.
- Semantics: continuation is a lexical escape, not a literal newline. An unescaped newline terminates with an unterminated-string diagnostic.
- Implementation: existing lexer/parser/string runtime.
- Tests: LF, CRLF, indentation, blank continued lines in `StrxLiteralTests.kira`.

## Byte strings and byte characters

- Rust problem: express raw byte data without Unicode/string semantics.
- Kira today: no byte literal or byte-string syntax. `[U8]` and FFI arrays provide byte storage; ordinary `String` already carries bytes.
- Status: `REJECT` for 1.9.1.
- Exact surface: `[U8]`, `String`, FFI array forms; no `b"..."` or `b'a'`.
- Semantics: keep byte ownership and C-layout behavior explicit rather than introduce a second string-like runtime type.
- Implementation: none for 1.9.1.
- Tests: positive `[U8]`/FFI array tests; negative `b"abc"`, `b'a'`.

## C strings

- Rust problem: pass NUL-terminated text across a C ABI.
- Kira today: `CString` is a seam type. It is legal in `@FFI.Extern` signatures and C-layout forms, not ordinary locals or ordinary Kira parameters (`crates/kira-semantics/src/types.rs:280-297`). A Kira `String` is accepted for a C-string parameter and materialized transiently. A `CString` foreign result is copied into an owned Kira `String`; a callback returning a C string is refused because ownership cannot be safely inferred (`crates/kira-semantics/src/foreign_callback.rs:215-231`, foreign tests around `675-690`).
- Status: `KEEP` seam; `REJECT` C-string literal syntax.
- Exact surface: `function greet(name: CString) -> I32;`, call `greet("hi")`; no dedicated literal.
- Semantics: transient parameter storage lives through the call. Retained C-string parameters require `move` and produce owned C storage (`foreign.rs:880-925`).
- Implementation: existing FFI adapters, CBlock ownership, VM/LLVM/Hybrid.
- Tests: FFI string parameter/result, retained string, ordinary-local rejection, callback-result rejection.

## Interpolated strings

- Rust problem: combine text and values ergonomically.
- Kira today: no interpolation delimiters.
- Status: `DEFER`.
- Exact current surface: `"value=" + String(x)`, `scalarText(x)`.
- Semantics: explicit conversion keeps ownership, byte, formatting, and compile-time behavior visible.
- Implementation: future lexer/parser, formatting rules, escape interactions, and backend/runtime support.
- Tests if added: escaped delimiters, nested expressions, non-printable values, ownership of interpolated strings.

# Expression forms

## Literal expressions

- Rust problem: produce primitive values.
- Kira today: integer, float, Boolean, string, array, struct, enum, closure, task, and compiler-intrinsic expressions are represented in `Expr` (`expr.rs:8-324`).
- Status: `KEEP`.
- Exact surface: `42`, `3.5`, `true`, `"x"`, `[1, 2]`, `Point { x = 1 }`, `.Red`, `{ x in return x }`, `Task { work(1) }`.
- Semantics/implementation/tests: see each form below and literal decisions above. Existing forms need `Tkik` coverage.

## Names, paths, `self`, and `Self`

- Rust problem: refer to locals, parameters, functions, modules, receiver state, and type identity.
- Kira today: names are `Expr::Name`; qualified paths are dotted names or field chains. `self` is ordinary identifier syntax except receiver parsing (`type_refs.rs:194-237`). `Self` has no special token or type resolution and is an ordinary name if declared.
- Status: `KEEP` ordinary `self`; `REJECT` special Rust `Self` semantics.
- Exact surface: `self.name`, bare `name` inside a method, `Geo.Point`, `callbacks.invoke`.
- Semantics: a method may use bare fields as implicit receiver reads. A receiver is written `borrow self` or `borrow mut self`; bare `self` is rejected with `KPAR075`.
- Implementation: parser receiver handling and semantic implicit-field resolution.
- Tests: `StrxStringTests.kira`, method tests; negative special `Self`, bare consuming `self`.

## Member access `x.foo`, fields, type/member queries, static access, and tuple members

- Rust problem: read fields/properties, call methods, select associated items, and inspect tuple members.
- Kira today:
  - `x.foo` parses as `Expr::Field`; `x.foo(...)` parses as `Expr::MethodCall` (`crates/kira-parser/src/expr.rs:247-325`).
  - Computed construct members read as properties.
  - Arrays and strings expose `.count`; task handles expose `.await`.
  - `foo.0` is rejected because a member after `.` must be an identifier (`KPAR022`).
  - There is no `x.type`, `Type::member`, associated/static member grammar, or runtime type query.
  - `Parent.member` inside a class method is a parent-qualified field/method selection, not Rust static dispatch; tests `classes.rs:208-244`.
  - Compile-time reflection exposes `target.fields`, `field.type`, `target.syntax`, and `Syntax` operations (`sites/docs/content/docs/macros/reflection.mdx:6-65`).
- Status: `KEEP` ordinary field/property/parent access; `REJECT` tuple members, runtime type queries, and Rust associated/static access.
- Difference: the reflection docs show `static function join(...)`, but ordinary `.kira` parser syntax has no `static` member declaration. That example must either be marked API-model-only or corrected.
- Exact surface: `rect.width`, `values.count`, `task.await`, `Left.v`, `Left.ping()`, `field.type` in reflection data. No `foo.0`, `x.type`, or `Type::member`.
- Semantics: field reads produce owned values; mutable field paths are places. Computed members may execute code. Parent qualification is only valid within the relevant class method.
- Implementation: existing field/property/parent semantic dispatch and backend field lowering. Reflection is a comptime frontend API.
- Compatibility: no ABI change for ordinary fields; C-layout fields remain ABI-sensitive.
- Tests: positive `foo.bar`, nested fields, `.count`, parent-qualified members; negative `foo.0`, `x.type`, static declarations, parent access outside methods.

## Method calls, free calls, callable values, and generic calls

- Rust problem: invoke methods, free functions, closures/function values, and generic instantiations.
- Kira today:
  - Free calls use `f(args)` or qualified `module.f(args)`.
  - Methods use `receiver.method(args)`.
  - Labels may use `name: value` or `name = value`; defaults fill omitted parameters.
  - Closures and named function references are first-class values. Function types are represented internally as synthesized structs with `StructOrigin::FunctionType`; closure values are dispatched through a generated finite dispatcher (`crates/kira-semantics/src/closures/mod.rs:1-32`, `function_values.rs:134-260`).
  - Local, constant, and field closure values can be called through their name/field (`crates/kira-semantics/src/closures/calls.rs:15-89`).
  - The AST call node stores a named callee symbol (`Expr::Call::callee`), so arbitrary callee expressions such as `make()(x)` are not supported.
  - Explicit generic calls are disambiguated only when `<...>` is followed by `(` or `{` (`crates/kira-parser/src/expr/calls.rs:69-125`).
- Status: `KEEP` existing forms; `DEFER` arbitrary callable-expression calls.
- Exact surface: `helper()`, `callbacks.invoke(add, 0, 5)`, `handler(0.016)`, `f<T>(x)`, `receiver.method(x)`, `receiver.callbackField(x)`.
- Semantics: argument evaluation and ownership follow parameter modes. Generic values are specialized in the frontend. Closure parameter types come from the expected function type.
- Implementation: existing parser/AST/HIR closure desugaring and dispatcher. Arbitrary callee calls would require AST/HIR/IR indirect-call representation, VM opcode, LLVM function pointer or equivalent, Hybrid/Web ABI, and ownership/default-label rules.
- Compatibility: generic specialization has no new external ABI; arbitrary callable values could change representation/ABI.
- Tests: existing closure/function-value tests; negative `make()(x)` and generic method declaration with own type parameters.

## Indexing `x[i]`

- Rust problem: read/write sequence elements and C array/pointer elements.
- Kira today: `Expr::Index` parses any base/index (`expr.rs:195-214`). Semantic analysis supports Kira arrays, user-defined `subscript` methods, and FFI pointer-backed arrays (`crates/kira-semantics/src/arrays.rs:1-293`).
- Status: `KEEP`.
- Exact surface: `values[index]`, `values[index] = next`, `grid[1][1]`, FFI `pointer[index]`.
- Semantics: index must be an integer. Array bounds trap at runtime. Index assignment is a place; base path mutability is checked. FFI pointer indexing uses target C layout.
- Implementation: HIR/IR `Index`, `ForeignElement`, VM/LLVM lowering, C-layout offset calculation.
- Compatibility: C pointer width and layout remain target-specific.
- Tests: `Collections.kira`, `ArxTests.kira`, `Tsem` arrays/FFI, backend array parity; negative non-array index, non-integer index, out-of-range runtime trap.

## Slicing

- Rust problem: borrow or copy a subrange without spelling a loop.
- Kira today: no `x[a..b]` expression and no slice type. Strings provide explicit byte slicing through `s.substring(start, end)`.
- Status: `REJECT` standalone slice syntax for 1.9.1; `KEEP` explicit string API.
- Exact surface: `text.substring(start, end)`; no `values[a..b]`.
- Semantics: `substring` is half-open byte slicing and traps for inverted/out-of-range bounds. It returns an owned string, not a borrowed view.
- Implementation: existing string HIR/IR/runtime. General slices would require lifetime/ownership, place, layout, and backend representation.
- Tests: positive substring boundaries; negative `xs[1..3]`, `text[1..2]`, inverted/out-of-range substring.

## Dereference

- Rust problem: access storage through a reference or raw pointer.
- Kira today: no unary dereference syntax. `*` is multiplication. `RawPtr` is opaque; `@FFI.Pointer` field reads are explicit semantic operations rather than `*p`.
- Status: `REJECT`.
- Exact surface: FFI `pointer.member` and `pointer[index]`; no `*pointer`.
- Semantics: no user pointer arithmetic, dereference, allocation, or free. C-layout pointer reads use target metadata and target-dependent offsets.
- Implementation: existing FFI field/element lowering; no first-class dereference.
- Tests: positive FFI pointer member/index reads; negative `*p`, `RawPtr(0).x`, pointer arithmetic.

## References and borrows

- Rust problem: lend storage without transferring ownership and enforce aliasing.
- Kira today: no `&value` or `&mut value` expression and no first-class reference type. Kira uses explicit parameter modes `borrow T`, `borrow mut T`; mutable borrows require a place and are written back (`crates/kira-semantics/src/place.rs:19-124`, `kira-ir/src/lower.rs:128-173`).
- Status: `REJECT` first-class reference syntax; `KEEP` parameter borrow modes.
- Exact surface: `function read(x: borrow T)`, `function edit(x: borrow mut T)`, `read(value)`, `edit(varValue)`, `function value(borrow self)`.
- Semantics: `borrow mut` must receive mutable storage; all path steps must be writable. Returned borrows are refused. No lifetime parameters, reference pattern, or auto-deref.
- Implementation: existing ownership/type checker, HIR writebacks, IR by-reference/by-pointer parameter lists, VM/LLVM.
- Compatibility: no reference ABI is exposed; adding one would affect all backends and FFI.
- Tests: `Ownership.kira`, `LtxLentTemporaryTests.kira`, mutation tests; negative borrow of `let`, borrow of temporary, returned borrow, overlapping mutable paths.

## Unary operators

- Rust problem: negate numbers, invert Boolean, invert integer bits, and explicitly transfer/copy ownership.
- Kira today: `-`, `!`, `~` are parsed at `crates/kira-parser/src/expr.rs:150-185`; `move`/`copy` are contextual ownership expressions.
- Status: `KEEP`, with one parser `CHANGE`.
- Exact surface: `-value`, `!flag`, `~mask`, `move value`, `copy value`.
- Semantics:
  - `-` preserves integer/float spelling; integer negation is two’s-complement behavior.
  - `!` accepts only `Bool`.
  - `~` accepts only integers.
  - `move local` marks a binding moved.
  - `move temporary` and `move field` consume no binding.
  - `copy` explicitly requests a non-consuming read.
- Difference: ownership lookahead currently recognizes only identifiers/literals/minus/bang/tilde/arrays, not a leading dot, bare block, or parenthesized operand (`expr.rs:94-123`).
- Decision: `CHANGE` ownership parser to accept every valid primary operand.
- Exact proposed additional surface: `move (x)`, `move .Red`, `move { in return 1 }`, `move Point { x = 1 }`.
- Implementation: parser lookahead/tree-sitter/docs; semantic ownership already handles temporaries/fields. No ABI change.
- Tests: positive and negative ownership combinations; malformed `move`, moved-twice, move from borrow, `move .Red`.

## Binary operators

- Rust problem: combine values with arithmetic, comparison, Boolean, bit, and shift operations.
- Kira today: binary operators are `||`, `&&`, `|`, `^`, `&`, equality, orderings, shifts, `+`, `-`, `*`, `/`, `%` (`crates/kira-parser/src/expr.rs:474-498`).
- Status: `KEEP`.
- Exact surface: as listed above.
- Semantics:
  - Numeric types must agree exactly or involve a bare wildcard spelling.
  - String `+` concatenates.
  - Struct `+`, `-`, `*`, `/` can lower to conventional methods (`crates/kira-semantics/src/typeck/struct_ops.rs:15-85`).
  - Concrete structs do not have ordinary equality.
  - Enum equality compares discriminant tags.
  - `Bool` supports only `&&`, `||`, `!`, equality, and ordering where currently defined.
- Implementation: operator resolver produces typed HIR operations; IR, VM, LLVM, Hybrid, and Web already lower them.
- Tests: `CfxScalarExtraTests*.kira`, operator semantics, backend arithmetic/logic/bitwise parity; negatives for mismatched types and Boolean bitwise operations.

## Comparisons

- Rust problem: produce an ordered or equality Boolean.
- Kira today: equality/inequality and four orderings. Numeric widths follow Kira’s exact/wildcard rules. Strings support equality where defined; enum equality is tag equality.
- Status: `KEEP`.
- Exact surface: `a == b`, `a != b`, `a < b`, `a <= b`, `a > b`, `a >= b`.
- Semantics: ordering signedness follows the left integer spelling. No general structural equality for structs/arrays.
- Implementation: `operators.rs:194-248`, enum equality in `typeck.rs:142-169`.
- Tests: numeric signed/unsigned ordering, enum tag comparison, NaN comparison, struct equality negative.

## Boolean operators and lazy evaluation

- Rust problem: combine conditions while avoiding evaluation of unnecessary branches.
- Kira today: `&&` and `||` are Boolean-only and short-circuit. HIR/IR/backend lowering branches rather than eagerly evaluating the right operand.
- Status: `KEEP`.
- Exact surface: `left && right`, `left || right`.
- Semantics: left operand is evaluated first. Right operand is type-checked but evaluated only when required. No truthiness and no bitwise Boolean operators.
- Implementation: typed HIR binary ops and backend branch lowering.
- Tests: `false && expression_that_traps()`, `true || expression_that_traps()`, side-effect counters, VM/LLVM/Hybrid parity.

## Arithmetic

- Rust problem: calculate numeric values with defined signedness and overflow behavior.
- Kira today: `+`, `-`, `*`, `/`, `%` for integers/floats; String `+`.
- Status: `KEEP`.
- Semantics: integer arithmetic uses 64-bit runtime values for every spelling. `/` and `%` choose signed/unsigned operation from the left operand. Floating arithmetic follows IEEE behavior. Struct arithmetic may dispatch to methods.
- Implementation: `operators.rs:159-191`, HIR/IR arithmetic ops and runtime.
- Tests: overflow/wrapping, signed/unsigned division/remainder, float infinity/NaN, string concatenation, struct operator methods.

## Bit operations

- Rust problem: manipulate integer bit patterns.
- Kira today: `&`, `|`, `^` on integer types. No Boolean bitwise form.
- Status: `KEEP`.
- Semantics: bare integer literals adapt to the other operand’s width spelling. Runtime value is 64-bit; written `U8` does not mask to eight bits.
- Implementation: `operators.rs:110-131`, backend bit operations.
- Tests: masks, signed patterns, mismatched float/string/Bool negatives, parity.

## Shifts

- Rust problem: shift bit patterns with a count.
- Kira today: `<<`, `>>`; RHS may be any integer spelling; result and signedness follow LHS; counts are modulo 64 (`operators.rs:133-157`).
- Status: `KEEP`.
- Exact surface: `mask << places`, `value >> places`.
- Semantics: signed right shift propagates sign; unsigned right shift fills zero; shift by 64 is defined as shift by 0.
- Implementation: typed HIR/IR shift variants and all backend lowerers.
- Tests: signed/unsigned right shift, counts 0/63/64/65, nonmatching RHS widths.

## Assignment

- Rust problem: update a mutable place.
- Kira today: assignment is a statement, parsed as `Stmt::Assign` (`crates/kira-parser/src/stmt.rs:41-61`).
- Status: `KEEP`.
- Exact surface: `x = value`, `object.field = value`, `array[index] = value`.
- Semantics: target must be a local, field path, or index path. Every path step must be mutable. Assignment returns no value and cannot be nested in an expression.
- Implementation: place resolution, HIR/IR write operations, VM/LLVM storage lowering.
- Tests: nested field/index assignment and immutable/temporary negatives.

## Compound assignment

- Rust problem: read, operate, and write a place concisely while evaluating the place only once.
- Kira today: no `+=`, `-=`, `*=`, `/=`, `%=` or bitwise compound assignment tokens.
- Status: `DEFER`.
- Exact current surface: `x = x + 1`.
- Decision: if added, it must evaluate the target path once, preserve mutable-place/writeback rules, and define return type/value. Do not make it silently expression-valued.
- Implementation: lexer, AST, place analysis, HIR/IR read-modify-write, all backends.
- Tests if added: fields/indexes, aliasing, temporary rejection, integer/float/struct operators.

## Casts and conversions

- Rust problem: make representation/type changes explicit.
- Kira today: no `as` cast. Conversion calls are recognized in `crates/kira-semantics/src/conversions.rs:27-105`.
- Status: `KEEP`.
- Exact surface: `Int(x)`, `I32(x)`, `U32(x)`, `Float(x)`, `F32(x)`, `RawPtr(integer)`, `rawPointerWord(pointer)`, `floatToBits(x)`, `bitsToFloat(x)`, `floatToBits32(x)`, `bitsToFloat32(x)`, `String(x)`.
- Semantics: exactly one argument; `Int`/`Float` conversions perform numeric conversion; same-kind width conversions retag the 64-bit value; pointer conversion is restricted; bit functions reinterpret IEEE bits; no `Bool` conversion or implicit truthiness. Distinct types require explicit constructor and `.raw`.
- Implementation: HIR `Convert`, IR conversion, VM/LLVM lowering.
- Compatibility: numeric conversions do not alter external ABI; pointer and bit functions are FFI-sensitive.
- Tests: `Tsem conversions`, `NuxTests.kira`, FFI pointer tests, wrong arity/source type negatives.

## Ranges

- Rust problem: represent iteration intervals.
- Kira today: `a..b` is not an expression. It is a `ForIterable::Range` parsed only in a `for` header (`crates/kira-syntax-model/src/ast/stmt.rs:42-62`, `crates/kira-parser/src/stmt.rs:164-213`).
- Status: `KEEP` for half-open `for`; `REJECT` standalone ranges.
- Exact surface: `for i in 0..count {}`.
- Semantics: lower bound is inclusive, upper bound exclusive; both bounds evaluate once; loop variable is fresh and immutable.
- Implementation: semantic desugar to while and IR loop; MID releases on loop exits.
- Tests: `LtxLentTemporaryTests.kira`, arrays/control-flow harnesses; negative `let r = 0..4`.

## Inclusive and exclusive ranges

- Rust problem: choose whether the upper endpoint participates.
- Kira today: only exclusive `..`; `..=` is not a token and parses as `DotDot` plus `Equals`, with no expression or loop support.
- Status: `KEEP` exclusive; `REJECT` inclusive syntax for 1.9.1.
- Exact surface: `0..n`; no `0..=n`.
- Semantics: use `for i in 0..n + 1` only when overflow-safe and explicitly intended.
- Implementation: none for inclusive form.
- Tests: positive empty/equal/reversed half-open ranges; negative `0..=n`.

## Array expressions

- Rust problem: construct homogeneous indexed collections.
- Kira today: `[a, b]`, `[]`, nested arrays, optional commas (`crates/kira-parser/src/expr/aggregates.rs:28-57`).
- Status: `KEEP`.
- Exact surface: `[1, 2, 3]`, `[1 2 3]`, `var xs: [Int] = []`.
- Semantics: element type inferred from first element or expected type for empty arrays. Elements require exact compatible types. Arrays are shared growable COW handles, move on binding, and mutate through places.
- Implementation: HIR/IR `ArrayNew`, `Index`, `ArrayAppend`, VM/LLVM runtime.
- Tests: `Collections.kira`, `ArxTests.kira`, array semantic/parity tests.

## Tuple expressions and tuple types

- Rust problem: compact positional product values and destructuring.
- Kira today: parentheses are grouping; function types use parameter parentheses. There is no tuple AST/type and `foo.0` is rejected.
- Status: `DEFER`.
- Difference: named structs provide products with stable field names and nominal layout.
- Exact current surface: `Point { x = 1, y = 2 }`; no `(1, 2)`.
- Semantics if added: tuple arity/type identity, `.0` access, ownership/drop of each element, layout, parameter/return ABI, pattern integration, and C/Hybrid/Web representation must all be defined.
- Implementation: parser/AST/type table/HIR/IR/VM/LLVM/FFI/tree-sitter.
- Tests if added: empty/singleton/nested tuples, tuple indexing, tuple assignment, tuple patterns, ABI parity.

## Struct construction

- Rust problem: construct named aggregate values with field defaults and explicit ownership.
- Kira today: `Name { field = value }`, `Name { field: value }`, and memberwise `Name(args)` (`expr/aggregates.rs:59-137`; `memberwise.rs:29-159`).
- Status: `KEEP`.
- Exact surface: `Point { x = 1, y = 2 }`, `Point(y: 2, x: 1)`, `Point(1, 2)`, `Point()`.
- Semantics: labels bind fields; omitted fields use defaults or produce a missing-field diagnostic. Struct values are nominal. HIR normalizes fields into declaration order. A C-layout struct zero-fills omitted fields and obeys target layout.
- Implementation: `HirExpr::StructNew`, declaration/default resolution, C-layout lowering, VM/LLVM.
- Compatibility: ordinary struct layout is compiler-owned; `@FFI.Struct` layout is ABI-sensitive.
- Tests: memberwise/default/duplicate/missing field tests, nested construction, FFI layout parity.

## Struct update and spread

- Rust problem: copy an existing aggregate while overriding selected fields.
- Kira today: no `..base` spread syntax. Construct `copy/update` is a Kira construct feature, not Rust struct-update syntax.
- Status: `REJECT` Rust spelling for 1.9.1.
- Exact current surface: construct-specific `copy`/`init` mechanisms and explicit `Name { field = value }`; no `Name { ..base }`.
- Semantics: explicit field ownership avoids hidden whole-value copies and makes C-layout ownership visible.
- Implementation: none for Rust form.
- Tests: negative `Point { x = 1, ..base }`; positive construct copy/update tests.

## Enum construction

- Rust problem: construct tagged alternatives, optionally carrying payloads.
- Kira today: leading-dot variants resolve against expected enum type (`.Red`, `.Ok(value)`), and variants inside match are unqualified. The AST is `Expr::DotMember` (`expr.rs:190-207`); semantics resolves expected type (`typeck.rs:99-145`).
- Status: `KEEP`.
- Exact surface: `let color: Color = .Red`, `let result: Result = .Ok(value)`, `match value { Ok(v) -> ... }`.
- Semantics: an enum has one optional payload per variant. Leading-dot construction requires expected enum type; no expected type is `KSEM119`. Payload bindings are owned immutable values. Payload-carrying enums are not direct C values.
- Implementation: enum tables, HIR `EnumNew`, `EnumTag`, `EnumPayload`, match desugar, VM/LLVM.
- Tests: `Enums.kira`, `EnxEnumTests*.kira`, match and enum semantic/parity tests.

## Blocks as expressions

- Rust problem: use a statement sequence as a value-producing expression.
- Kira today: ordinary `{ ... }` is a statement block. Braces also form closures, task bodies, and construction content. `Expr` has no general block expression.
- Status: `REJECT` for 1.9.1.
- Exact current surface: statement blocks; value-producing code uses explicit `return` or `?:`.
- Semantics: no implicit block value, no block-local trailing-expression return, and no block expression ownership boundary.
- Implementation: none for current design.
- Tests: negative `let x = { 1 }`; positive explicit `function f() -> Int { return 1 }`.

## `if` expressions

- Rust problem: choose one of two values through control flow.
- Kira today: `if` is a statement. The value-producing conditional is `condition ? then : otherwise` (`Expr::Conditional`).
- Status: `REJECT` Rust `if` expression; `KEEP` Kira conditional.
- Exact surface: `let x = flag ? a : b`.
- Semantics: both branches are type-checked; exactly one is evaluated. Branch types must agree under Kira’s numeric wildcard rule. `Void` branches are rejected.
- Implementation: existing conditional HIR/IR/backend branch lowering.
- Tests: `NuxTests.kira`, conditional parser/semantic/parity tests; negative `let x = if flag { 1 } else { 2 }`.

## `if-let`

- Rust problem: branch while destructuring a refutable pattern.
- Kira today: no pattern condition. Use `if` plus enum comparison or a statement `match`.
- Status: `DEFER`.
- Exact current surface: `if value == .Variant { ... }` or `match value { Variant(payload) -> ... }`.
- Semantics if added: pattern scope, ownership of payload, else path, and definite initialization require the general pattern design.
- Implementation: pattern AST/type checker/ownership/control flow/MID/backends.
- Tests if added: payload binding, failed branch, moved values, nested conditions.

## `let-else`

- Rust problem: destructure a value and diverge immediately if the pattern fails.
- Kira today: no pattern-bearing `let`; `try`/`attempt` handles Kira’s defined fallible flow.
- Status: `DEFER`.
- Exact current surface: `attempt { let value = try fallible() ... } handle { ... }`.
- Implementation: generalized pattern and divergence analysis.
- Tests if added: irrefutable/refutable declarations, `return`/`break` else paths, ownership.

## Match

- Rust problem: exhaustive tagged control flow with payload binding.
- Kira today: `match subject { Variant -> body; Variant(payload) -> body }`; only enum subjects, unqualified variant names, one optional binding (`stmt.rs:9-40`; `matches.rs:61-230`).
- Status: `KEEP`.
- Exact surface:
  `match subject { Light -> return 1; Tag(text) -> { print(text) } }`
- Semantics: subject evaluates once. Every enum variant must be covered exactly once. Missing, duplicate, and unknown variants produce `KSEM125-129`. Payload binding is immutable and owned. Match is a statement, not a value.
- Implementation: desugars to enum tag tests and payload reads; no new backend construct.
- Tests: `Enums.kira`, `EmxTests.kira`, `Tsem matches`, match parity.

## Loops, `while`, `while-let`, and `for`

- Rust problem: repeat computation over conditions, patterns, ranges, or iterators.
- Kira today:
  - `while condition {}` is a statement.
  - `for name in start..end {}` iterates a half-open range.
  - `for name in array {}` iterates arrays.
  - `For(x in xs) {}` inside content blocks is a separate builder form.
  - There are no loop labels, iterator protocol, or `while-let`.
- Status: `KEEP` current while/for; `DEFER` `while-let`; `REJECT` Rust iterator/pattern syntax for 1.9.1.
- Exact surface: `while i < n {}`, `for i in 0..n {}`, `for item in items {}`.
- Semantics: conditions/bounds/array bases follow existing evaluation rules. Loop bindings are fresh immutable names. A loop body may not move a value that the next iteration needs without reinitialization.
- Implementation: `for` desugars to `while`; MID releases on loop exits; VM/LLVM parity.
- Tests: control-flow and collection harnesses; negative non-Boolean while condition, non-array iterable, repeated move.

## Break with and without value

- Rust problem: leave a loop, optionally returning a loop expression value.
- Kira today: `break` has no value and no label (`stmt.rs:215-225`).
- Status: `KEEP` bare break; `REJECT` value/label forms for 1.9.1.
- Exact surface: `break`; no `break value` or `'label: break`.
- Semantics: only exits innermost loop. MID releases scopes up to loop boundary.
- Implementation: existing HIR/IR control flow and scope release.
- Tests: positive break/continue loop tests; negatives `break 1`, labeled break, break outside loop.

## Continue

- Rust problem: skip the rest of the current iteration.
- Kira today: bare `continue` only.
- Status: `KEEP`.
- Exact surface: `continue`.
- Semantics: for-loop rewrite advances the cursor before the next iteration; scope releases occur correctly.
- Implementation: existing MID loop handling.
- Tests: control-flow harness and continue-with-nested-scope parity; negative labeled continue.

## Return

- Rust problem: leave a function with an optional value.
- Kira today: `return` or `return expression` (`stmt.rs:114-124`).
- Status: `KEEP`.
- Exact surface: `return`, `return value`.
- Semantics: result type is checked. A bare return is valid for `Void`. An exhaustive match or complete attempt can establish definite return. There is no implicit trailing-expression return.
- Implementation: existing statement/HIR/IR/backend control flow and scope release.
- Tests: `ControlFlow.kira`, match/attempt definite-return tests; negatives missing return and value in `Void` function.

## Closures

- Rust problem: package code and captures as callable values.
- Kira today: `{ params in body }` and `{ in body }` (`crates/kira-parser/src/expr/closures.rs:23-117`). Parameters are names only; expected function type supplies types and ownership modes.
- Status: `KEEP`.
- Exact surface: `let f: (Int) -> Int = { value in return value + 1 }`; `let g: () -> Int = { in return 1 }`.
- Semantics: immutable `let` captures by value when trivially copyable; captured `var` uses a shared cell. Closure values are synthesized structs containing a tag and captures, then dispatched through finite generated functions. Function type ownership modes are part of type identity.
- Implementation: frontend closure lifting only; VM/LLVM/Hybrid/Web see ordinary structs/functions.
- Tests: `Closures.kira`, `StrxClosureTests*.kira`, closure/function-value parity.

## Move closures

- Rust problem: force closure captures to move from the surrounding environment.
- Kira today: no `move ||` or `move {}` modifier. `move` is an ownership operator on a value; capture policy is determined by `let`/`var`.
- Status: `REJECT`.
- Exact current surface: `let f = { in return value }`; use `move value` when transferring a named closure.
- Semantics: adding a closure modifier would conflict with Kira’s explicit capture-cell model and require a separate capture analysis rule.
- Implementation: none for 1.9.1.
- Tests: negative `let f = move { in return 1 }`; positive `let f = move closureValue`.

## Async blocks

- Rust problem: create a deferred computation from arbitrary statements and captures.
- Kira today: `async function` is contextual; `Task { ... }` accepts a direct named scalar call or scalar literal only (`crates/kira-semantics/src/tasks.rs:1-102`).
- Status: `REJECT` arbitrary async blocks for 1.9.1.
- Exact surface: `async function work(x: Int) -> Int { return x }`; `Task { work(1) }`.
- Semantics: task arguments evaluate at spawn; body executes later. Ordinary tasks carry integer/float result descriptors; task handles are opaque.
- Implementation: existing task HIR/IR/task spine, VM/LLVM/Hybrid/Web parity.
- Tests: `AsyncSpine*.kira`, task backend parity; negatives `Task { let x = 1 }`, `Task { closure() }`, aggregate result.

## Await

- Rust problem: suspend until an asynchronous computation completes.
- Kira today: `.await` is a property of ordinary task and main-thread task handles. `await` is not a keyword. Task methods are `.requestCancel()` and `.detach()` (`tasks.rs:251-310`).
- Status: `KEEP` Kira property; `REJECT` Rust await expression.
- Exact surface: `let result = task.await`; no `await expression`.
- Semantics: `.await()` is rejected because await is a property. Handles are opaque and cannot be indexed, added, or inspected.
- Implementation: task join HIR/IR and VM/LLVM runtime.
- Tests: task await/cancel/detach parity; negative `.await()` and `task + 1`.

## Error propagation and Rust `?`

- Rust problem: propagate a failure without manually branching.
- Kira today: `?` is the conditional operator. Error propagation uses `try` only as the entire initializer of a `let` directly inside `attempt`, with `handle` arms (`crates/kira-semantics/src/stmt/attempts.rs:24-35,188-216`).
- Status: `REJECT` Rust `?`; `KEEP` Kira `attempt`/`try`.
- Exact surface:
  `attempt { let value = try fallible() return value } handle { Error(reason) { return reason } }`
- Semantics: `try` routes the failure enum to handlers; all `try` expressions in one attempt must agree on failure enum. Statements after success continue; failure jumps to handler. Arbitrary `try` positions are rejected.
- Implementation: structured HIR/IR attempt steps, VM/LLVM control flow.
- Tests: `EmxTests.kira`, `Tsem attempts`, attempt parity; negatives `let x = try f()` outside attempt and `f(try g())`.

## Unsafe blocks

- Rust problem: explicitly delimit operations requiring unchecked memory/FFI assumptions.
- Kira today: no `unsafe` block. FFI is declared through `@FFI.*`, and raw pointers remain opaque.
- Status: `REJECT`.
- Exact current surface: `@FFI.Extern`, `@FFI.Pointer`, `RawPtr`, and explicit FFI annotations.
- Semantics: arbitrary pointer dereference, pointer arithmetic, and user unsafe code are unavailable. This keeps VM/native/Web behavior aligned.
- Implementation: none for 1.9.1.
- Tests: positive FFI seam tests; negative `unsafe {}`, `*p`, pointer arithmetic.

## Const blocks

- Rust problem: evaluate a block at compile time in expression position.
- Kira today: module-scope `let` constants, `comptime function`, `comptime macro`, `quote`, and compiler reflection provide compile-time behavior. No `const {}` expression.
- Status: `REJECT`.
- Exact surface: `let Name = value` at module scope; `comptime macro`; `quote { ... }`.
- Semantics: constants are resolved/initialized through module constant machinery; macros operate on syntax and are expanded before backend lowering.
- Implementation: existing comptime/macro frontend and constant HIR/IR.
- Tests: `TlxConstantTests.kira`, `ComptimeFunctionTests.kira`, `MxxMacroTests.kira`; negative `const {}`.

## Parentheses and grouping

- Rust problem: override precedence and group a value.
- Kira today: `(expression)` is grouping (`crates/kira-parser/src/expr/calls.rs:249-254`). Empty `()` is not a unit expression. Function types use `()` as a parameter list, for example `() -> Int`.
- Status: `KEEP` grouping; `REJECT` tuple/unit interpretation.
- Exact surface: `(a + b)`, `((x))`, `() -> Int`; no `()`, `(x, y)`.
- Semantics: grouping preserves inner type and ownership. Parentheses can make an ownership operand unambiguous.
- Implementation: parser only for grouping; function type parser handles type parentheses.
- Tests: precedence/grouping parser tests; negative empty expression and tuple forms.

# Expression semantics

## Precedence

- Rust problem: determine which operation binds first.
- Kira today: C-style ladder: `||`, `&&`, `|`, `^`, `&`, equality, ordering, shifts, `+/-`, `*//%`, unary, postfix. Conditional `?:` is lowest (`crates/kira-parser/src/expr.rs:1-17,474-498`).
- Status: `KEEP`.
- Semantics: bitwise operators intentionally bind looser than equality. `a & b == c` means `a & (b == c)` under the documented ladder.
- Implementation: precedence-climbing parser.
- Tests: `crates/kira-parser/src/tests/precedence.rs:69-178`; backend arithmetic/logic parity.

## Associativity

- Rust problem: group repeated operations.
- Kira today: all binary operators are left-associative. Conditional is right-associative because both branches parse as full expressions (`expr.rs:49-70,78-92`).
- Status: `KEEP`.
- Tests: `a - b - c`, `a / b / c`, nested `a ? b : c ? d : e`.

## Evaluation order

- Rust problem: make side effects and ownership deterministic.
- Kira today: operands and ordinary call arguments are analyzed and retained in written order. Match subjects, range bounds, task arguments, and array bases have explicit single-evaluation rules. Struct HIR fields are normalized to declaration order after written initializers are analyzed (`memberwise.rs:61-159`, `calls/literal.rs:57-165`).
- Status: `KEEP`, but document and pin construct-field runtime order.
- Decision: ordinary expressions evaluate left-to-right. Struct/construct field initializers should be specified as written-order evaluation while storage remains declaration order; current implementation needs a test to prove whether lowering currently follows declaration order.
- Implementation: existing HIR/IR vectors; if written-order preservation differs, introduce sequencing temporaries in semantic lowering.
- Tests: side-effecting labeled struct fields, call arguments, binary operands, task spawn arguments, match subject, range bounds.

## Short-circuiting and partial evaluation

- Rust problem: evaluate only reachable branches while still checking the whole program.
- Kira today: `&&`, `||`, and `?:` evaluate only selected runtime branches; type analysis visits both sides/branches. Match analyzes all arms but executes one. `Task` analyzes its body but defers execution.
- Status: `KEEP`.
- Semantics: compile-time checking is not runtime evaluation. A dead branch can still contain a type error; a dead runtime branch does not execute side effects or traps.
- Implementation: HIR/IR branch nodes and backend control flow.
- Tests: dead-branch traps, dead-branch type errors, match arm side effects, task defer timing.

## Place expressions and values

- Rust problem: distinguish writable storage from computed values.
- Kira today: `HirPlace` is a local plus field/index path (`crates/kira-semantics/src/place.rs:1-17,99-276`).
- Status: `KEEP`.
- Exact surface: writable `x`, `object.field`, `array[index]`; non-place `make()`, literal, arithmetic result.
- Semantics: reads produce owned values. Assignment, append, mutating methods, and `borrow mut` require places. Every path step must be mutable. Array indices are conservatively considered overlapping.
- Implementation: place resolver, HIR writebacks, IR places, VM/LLVM storage.
- Tests: nested mutation, immutable roots, sibling fields, dynamic-index overlap, temporary mutation rejection.

## Lvalues and rvalues equivalent

- Rust problem: define which expressions can appear on the left of assignment or borrow.
- Kira today: no `lvalue`/`rvalue` terminology in syntax; the place/value distinction is explicit in semantic analysis.
- Status: `KEEP`.
- Semantics: field/index expressions can be either read values or resolved places depending on context. A computed member or function result is never a place.
- Implementation/tests: same as place semantics.

## Temporary creation

- Rust problem: define ownership of literals, calls, constructors, and intermediate results.
- Kira today: literals/constructors/calls produce owned values; `move` of a temporary consumes no named binding. Temporary mutation such as `make().append(1)` is rejected (`Tsem arrays`).
- Status: `KEEP`.
- Semantics: a temporary can be read, passed according to parameter mode, or dropped; it cannot be used as mutable storage.
- Implementation: HIR owned expression values, IR release/copy machinery.
- Tests: `make().append(1)` negative; temporary field read positive; temporary passed to `borrow mut` negative.

## Temporary lifetime

- Rust problem: keep temporaries alive long enough for their uses and no longer.
- Kira today: temporary ownership is handled by expression/call lowering and lexical scope releases. Borrowed C strings live through a foreign call; C-layout materializations can outlive a call when a retaining seam requires them.
- Status: `KEEP`.
- Semantics: no user-visible temporary references or returned borrows. A temporary cannot escape through an unsupported borrow. Foreign retention is explicit with `retains`.
- Implementation: CBlock/runtime storage, IR scope releases, backend cleanup.
- Tests: temporary string/array/struct calls, retained C-string, temporary FFI aggregate and callback cases.

## Drop scopes

- Rust problem: release owned values exactly once on all control-flow exits.
- Kira today: MID scope analysis inserts releases for lexical scopes and handles `return`, `break`, `continue`, and task helper functions (`crates/kira-ir/src/mid/scope.rs:220-390`).
- Status: `KEEP`.
- Semantics: moved owners are not dropped twice; moved-out locals are dead; no partial field moves; `Drop` bodies run according to ownership.
- Implementation: ownership analysis plus MID scope release, VM/LLVM value release.
- Tests: `DrxDropTests.kira`, `EmxDropTests.kira`, release parity, early returns/break/continue.

## Statement and expression distinction

- Rust problem: permit effectful statements and value-producing expressions in predictable positions.
- Kira today: statements are `let`, assignment, return, expression statement, `if`, `while`, `for`, `match`, `attempt`, break, continue. Expressions have no general statement-block or statement-valued forms.
- Status: `KEEP`.
- Semantics: an expression statement discards its result according to ownership rules. Assignment is not an expression.
- Implementation: separate AST/HIR statement/expression enums.
- Tests: expression statements, assignment nesting negative, `Void` call statement.

## Trailing-expression returns

- Rust problem: make the final expression of a block its result.
- Kira today: unsupported. Functions and closures return only through explicit `return`.
- Status: `REJECT`.
- Exact surface: `function f() -> Int { return 1 }`; no `function f() -> Int { 1 }`.
- Semantics: explicit return makes ownership and cleanup visible.
- Tests: negative trailing expression in function/closure; positive explicit return.

## Semicolon behavior

- Rust problem: distinguish expression statements from their values.
- Kira today: semicolon is a separator; it does not turn an expression into a unit value. Newlines are skipped and cannot independently be observed.
- Status: `CHANGE` documentation.
- Exact surface: `print(1); return`; semicolon is optional where parser structural recovery permits.
- Semantics: no implicit `()`.
- Tests: semicolon between statements, omitted semicolon, malformed adjacent expressions.

## Unreachable and diverging expressions

- Rust problem: model control flow that never returns.
- Kira today: `return`, `break`, and `continue` are statements. There is no `Type::Never`, no `!` type, and no general diverging expression.
- Status: `REJECT` `Never` type for 1.9.1.
- Semantics: definite-return analysis understands explicit control flow and exhaustive match/attempt, not a first-class bottom type. Duplicate match variants are diagnosed as unreachable/duplicate where known.
- Implementation: existing statement control-flow analysis.
- Tests: return in branches, exhaustive match return, unreachable duplicate variant, negative `let x: !`.

## Implicit unit values

- Rust problem: give no-value statements/functions a first-class unit result.
- Kira today: `Void` represents no value. Bare `return` and `Void` calls are valid only in effect positions. There is no `()` expression.
- Status: `KEEP` `Void`; `REJECT` unit literal.
- Exact surface: `function log() -> Void { return }`, `log()`; no `return ()`.
- Semantics: `Void` cannot be used in arithmetic, arrays, or conditional value branches.
- Tests: Void function tests; negative `return ()`, `let x = ()`, `true ? log() : log()`.

# Patterns

## Literal patterns

- Rust problem: match a value against a constant.
- Kira today: no literal pattern syntax. Use `if value == constant` or enum match.
- Status: `DEFER`.
- Exact current surface: `if n == 3 {}`.
- Implementation if added: pattern AST, type checking, exhaustiveness, ownership.
- Tests if added: integer/string/Boolean literals, overlapping literals, NaN behavior.

## Identifier bindings

- Rust problem: bind a matched value or payload to a name.
- Kira today: match binds only one enum payload name, such as `Tag(text)`. `let`, `for`, closure parameters, and function parameters bind names only.
- Status: `KEEP` current enum payload binding; `DEFER` general binding patterns.
- Semantics: match bindings are immutable owned values scoped to one arm.
- Tests: `Enums.kira`, `matches.rs`; negative binding outside payload.

## Mutability bindings

- Rust problem: choose whether a binding can be written.
- Kira today: `let` is immutable and `var` mutable. Match and `for` bindings are always immutable.
- Status: `KEEP`.
- Exact surface: `let x = 1`, `var x = 1`, `Tag(value)`.
- Semantics: mutability is checked at places; matching does not create a writable alias to the enum.
- Tests: assignment to `let`, assignment to `for` binding, assignment to match binding.

## Wildcard `_`

- Rust problem: ignore a value or match all remaining cases.
- Kira today: `_` is an ordinary identifier. A match arm `_ -> ...` is treated as an unknown variant and rejected; there is no wildcard or `else` arm (`for-llms.mdx:36-38`).
- Status: `REJECT` wildcard match for 1.9.1.
- Difference: exhaustive explicit variants are a deliberate Kira safety/readability rule.
- Exact current surface: omit an enum payload binding (`Tag -> ...`) when payload is unused. Use explicit comparisons for fallback logic.
- Implementation: none.
- Tests: negative wildcard/else arms; positive exhaustive explicit arms and ignored payload.

## Rest pattern `..`

- Rust problem: ignore remaining fields/elements in a destructuring pattern.
- Kira today: `..` is a for-range delimiter only.
- Status: `REJECT` pattern rest.
- Exact surface: explicit struct fields or array indexing; no `Point { x, .. }`.
- Semantics: no destructuring syntax exists whose remainder needs naming.
- Tests: negative struct/tuple rest patterns; positive explicit field access.

## Range patterns

- Rust problem: match a numeric interval.
- Kira today: no range patterns; `..` only appears in for headers.
- Status: `REJECT`.
- Exact surface: `if n >= low && n < high {}` or explicit match variants.
- Implementation/tests: no pattern machinery; add negative range-pattern tests.

## Reference patterns

- Rust problem: destructure through a reference without moving the referent.
- Kira today: no reference type, `&pattern`, `ref`, or `ref mut`.
- Status: `REJECT`.
- Exact surface: `borrow` parameters and explicit field reads.
- Tests: negative `&x`, `ref x`, `ref mut x`.

## Struct destructuring

- Rust problem: bind named fields from an aggregate.
- Kira today: no struct pattern. Use `value.field`.
- Status: `DEFER`.
- Semantics if added: field-by-field ownership, partial move rules, mutability, defaults, and C-layout aggregates must be specified.
- Tests if added: field omission, rename, nested fields, partial moves, mutable bindings.

## Tuple destructuring

- Rust problem: bind positional product members.
- Kira today: no tuple type or tuple pattern.
- Status: `DEFER` with tuple values.
- Tests if added: arity/type mismatch, nested tuples, `.0` and pattern ownership.

## Enum payload destructuring

- Rust problem: branch on variant and bind payload.
- Kira today: supported in the narrow form `Variant(binding)` with exactly one binding.
- Status: `KEEP`.
- Semantics: payload is extracted once into an immutable owned binding; nested payloads require nested matches.
- Implementation: enum tag/payload HIR and match desugar.
- Tests: `EmxTests.kira`, nested enum payload and drop tests.

## Nested patterns

- Rust problem: destructure multiple layers in one match.
- Kira today: nested `match` statements and field reads.
- Status: `DEFER`.
- Exact current surface: nested `match`.
- Implementation/tests if added: generalized pattern tree and recursive ownership analysis.

## OR patterns

- Rust problem: share one body among alternatives.
- Kira today: no `A | B` arm pattern.
- Status: `DEFER`.
- Exact current surface: separate arms or Boolean conditions.
- Tests if added: alternatives with equal binding sets, mismatched bindings, exhaustiveness.

## Binding with `@`

- Rust problem: bind the whole value while constraining it with a subpattern.
- Kira today: no `@` pattern; `@` is annotation punctuation.
- Status: `DEFER`.
- Exact current surface: bind payload, then inspect it in the arm.
- Tests if added: whole-value plus nested binding ownership.

## Match guards

- Rust problem: add a condition to an otherwise matching arm.
- Kira today: no guard syntax.
- Status: `DEFER`.
- Exact current surface: explicit `if` before/inside a match arm or separate match logic.
- Semantics: current match exhaustiveness remains purely variant-based.
- Tests if added: guard side effects, fall-through, exhaustiveness.

## `ref` and `ref mut` equivalents

- Rust problem: select binding by reference rather than by value.
- Kira today: no equivalent pattern. `borrow` and `borrow mut` exist only in parameter/type positions.
- Status: `REJECT` pattern spelling.
- Exact current surface: `function f(value: borrow T)`, `function f(value: borrow mut T)`.
- Tests: negative pattern forms; positive borrow parameter tests.

## Pattern binding modes

- Rust problem: infer move, copy, or borrow behavior from pattern context.
- Kira today: binding ownership is explicit. Match payload bindings are immutable owned copies; parameter ownership is written in the signature.
- Status: `KEEP` explicit ownership; `REJECT` implicit pattern modes.
- Implementation/tests: existing ownership checker and match tests.

## Pattern ergonomics and auto-deref

- Rust problem: make reference patterns ergonomic.
- Kira today: no reference values and no auto-deref.
- Status: `REJECT`.
- Exact surface: explicit `borrow` parameters and fields.
- Tests: negative auto-deref/reference pattern cases.

## Exhaustiveness

- Rust problem: ensure all possible variants are handled.
- Kira today: enum `match` and attempt handlers are checked for unknown, duplicate, and missing variants. `matches.rs` lowers exhaustive match to an if chain.
- Status: `KEEP`.
- Semantics: no wildcard fallback. An exhaustive match whose arms all return counts for definite return.
- Implementation: semantic variant table, HIR desugar, control-flow analysis.
- Tests: missing/duplicate/unknown arms, exhaustive definite-return, nested matches.

## Unreachable patterns

- Rust problem: diagnose patterns shadowed by earlier patterns.
- Kira today: only duplicate known enum variants are detectable and rejected. There are no guards/ranges/literals that create richer reachability.
- Status: `KEEP` duplicate diagnostics; `DEFER` full reachability.
- Tests: duplicate variant; future overlapping literal/range/guard cases.

## Irrefutable and refutable distinction

- Rust problem: restrict patterns by whether failure is possible.
- Kira today: no general patterns. Names in `let`, `for`, parameters, and closures are irrefutable.
- Status: `KEEP` name-only positions; `DEFER` distinction for generalized patterns.
- Tests: positive all current binding positions; negative pattern syntax.

## Patterns in parameters

- Rust problem: destructure arguments at function entry.
- Kira today: parameters require a name followed by `:` and a type. Receiver has explicit `self`.
- Status: `REJECT` pattern parameters for 1.9.1.
- Exact current surface: `function f(point: Point)`, `function f(borrow self)`.
- Tests: negative `function f((x, y): ...)`, struct/enum parameter patterns.

## Patterns in `let`

- Rust problem: destructure and bind in one declaration.
- Kira today: `let`/`var` accept exactly one identifier (`stmt.rs:63-112`).
- Status: `REJECT` current pattern syntax; `DEFER` generalized binding.
- Tests: negative tuple/struct/enum patterns; positive ordinary declarations.

## Patterns in `for`

- Rust problem: destructure each iteration item.
- Kira today: `for` accepts one identifier only; loop bindings are immutable.
- Status: `REJECT` current patterns; `DEFER` generalized patterns.
- Tests: negative `for (x, y) in pairs`; positive array/range loops.

## Patterns in closures

- Rust problem: destructure closure arguments.
- Kira today: closure parameters are names only (`expr/closures.rs:86-117`).
- Status: `REJECT` current patterns; `DEFER` generalized patterns.
- Tests: negative closure destructuring; positive typed closure values.

# Core value types

## `Bool`

- Status: `KEEP`.
- Surface: `true`, `false`, `Bool` parameters/results.
- Semantics: strict Boolean; no truthiness; logical operators short-circuit.
- Implementation/tests: scalar HIR/IR/backend and logic parity.

## Integer forms

- Status: `KEEP` Kira’s current set.
- Surface: `Int`, `I8`, `I16`, `I32`, `U8`, `U16`, `U32`, `U64`; `Int32` aliases `I32`.
- Difference: no `I64`, `I128`, `U128`. `Int` is the signed 64-bit spelling.
- Semantics: all use one 64-bit two’s-complement runtime representation. Width names affect type identity and signedness-sensitive operations, not physical width.
- Implementation: `IntSpelling`, type checker, typed operators, backend scalar lowering.
- Tests: widths/widening/operator parity.

## Floating forms

- Status: `KEEP`.
- Surface: `Float`, `F32`; no `F64`.
- Semantics: both use one 64-bit IEEE representation; spelling affects type identity only. Literals must be finite; runtime operations may produce NaN/infinity.
- Implementation/tests: existing float HIR/IR/backend and scalar tests.

## `Char`

- Status: `REJECT`.
- Surface: no character type/literal; use `Int` plus `scalarText`.
- Semantics: avoids imposing Unicode scalar indexing on byte-oriented strings and avoids an FFI type decision.
- Tests: negative `Char`, `'x'`.

## `String`

- Status: `KEEP`.
- Surface: `"text"`, `String(value)`, `.count`, `.charAt(i)`, `.substring(a, b)`, `.indexOf(needle)`.
- Semantics: owned heap bytes, byte count/indexing, explicit scalar conversion. String copies are independent.
- Implementation: string HIR/IR/runtime and backend parity.
- Tests: `StrxStringTests*.kira`, string parity and ownership tests.

## `Void` and unit

- Status: `KEEP` `Void`; `REJECT` Rust unit literal.
- Surface: `function f() -> Void { return }`.
- Semantics: no value; cannot be stored or used as an operand. `()` appears only in function type syntax such as `() -> Int`.
- Tests: Void returns/calls; negative unit expressions.

## Never

- Status: `REJECT` `!` type.
- Surface: no never type. Explicit return/break/continue carry control-flow meaning.
- Semantics: no bottom-type subtyping or implicit coercion.
- Tests: negative `let x: !`, never-returning expression annotations.

## Tuples

- Status: `DEFER`.
- Surface: named structs instead.
- Semantics/implementation: requires tuple identity, positional members, destructuring, ownership/drop, ABI, and FFI rules.

## Arrays

- Status: `KEEP`.
- Surface: `[T]`, `[a, b]`, `xs[i]`, `xs.count`, `xs.append(v)`.
- Semantics: shared growable COW handle; array binding moves to avoid aliasing; explicit copy detaches on mutation.
- Implementation/tests: array HIR/IR/runtime and full parity.

## Slices

- Status: `REJECT` current slice type; `DEFER` future borrowed views.
- Surface: explicit `.substring` for strings and loops/indexing for arrays.
- Semantics: no lifetime-bearing view or pointer-length pair in ordinary Kira.
- Implementation if added: type, ownership, layout, HIR/IR/backend/FFI.

## Structs

- Status: `KEEP`.
- Surface: `struct Point { var x: Int; var y: Int }`, `Point { x = 1, y = 2 }`.
- Semantics: nominal named fields, defaults, deep copy, mutable paths, methods.
- Implementation/tests: struct tables, HIR/IR, VM/LLVM/Hybrid/Web, struct harnesses.

## Enums

- Status: `KEEP`.
- Surface: `enum Color { Red Green }`, `.Red`, `.Payload(value)`.
- Semantics: nominal tagged value, one optional payload per variant, exhaustive statement match.
- Implementation/tests: enum HIR/IR, tag/payload operations, match parity.

## Functions

- Status: `KEEP`.
- Surface: `function add(a: Int, b: Int) -> Int { return a + b }`; function type `(Int) -> Int`.
- Semantics: top-level named functions can be values when a function type is inferred/expected. Function type ownership modes are part of identity.
- Implementation: named-function value desugaring to function-type representation structs and dispatchers.
- Tests: closure/function-value tests and FFI callbacks.

## Closures

- Status: `KEEP`.
- Surface: `{ value in return value + 1 }`.
- Semantics: synthesized struct tag plus captures; immutable values captured by value where trivially copyable; `var` captures shared cells.
- Implementation: frontend only; no closure-specific backend opcodes.
- Tests: closure capture, nested closures, mutable cells, backend parity.

## References

- Status: `REJECT` first-class reference type.
- Surface: `borrow T`, `borrow mut T` parameter modes.
- Semantics: no reference values, lifetimes, dereference, reference patterns, or returned borrows.
- Implementation/tests: ownership/place/writeback machinery already covers Kira’s intended borrow problem.

## Raw pointers

- Status: `KEEP` opaque FFI value.
- Surface: `RawPtr(0)`, `RawPtr` parameters/results, `rawPointerWord(pointer)`, FFI pointer annotations.
- Semantics: pointer word may be stored, copied, returned, and passed back to C. It cannot be dereferenced, arithmetically manipulated, or freed by ordinary Kira. `ForeignPtr` retains target C-layout metadata.
- Implementation: HIR/IR raw pointer, FFI adapters, target-specific pointer width, VM/LLVM/Hybrid/Web.
- Tests: FFI pointer round trip, member/index access, raw pointer conversion, negative dereference/arithmetic.

# Kira-native value and operation forms

## `Any`

- Problem solved: erase a value’s concrete type at an explicit dynamic boundary.
- Kira today: `Any` accepts every type; it is opaque in the reverse direction. There is no `is`, `as`, or downcast.
- Status: `KEEP`.
- Surface: `let value: Any = concrete`; `Any Family`.
- Semantics: crossing into `Any` inserts an erased representation; it can be stored, copied, passed, returned, and dropped, but not inspected.
- Implementation: `IntoAny`/erased type HIR/IR and backend runtime.
- Tests: `Any` widening and generic parity; negative downcast syntax.

## Distinct types

- Problem solved: nominal IDs/newtypes without runtime wrapper cost.
- Kira today: `distinct TabId = U32`; construct with `TabId(U32(1))`; read `.raw` (`crates/kira-semantics-model/src/ty/mod.rs:69-78`, distinct tests).
- Status: `KEEP`.
- Semantics: distinct type is assignable only to itself; equality is allowed between equal distinct types; arithmetic and comparison with representation are rejected. IR erases the distinct wrapper before backend lowering.
- Implementation: parser/type table/semantic checks and `kira-ir` distinct erasure; no ABI cost.
- Tests: `crates/kira-semantics/src/tests/distincts.rs`, `Tkik` distinct tests.

## Traits and trait existentials

- Problem solved: static behavior reuse and dynamic existential values.
- Kira today: trait names in type positions hold conforming values; dispatch/defaults/supertraits are resolved in the frontend.
- Status: `KEEP`.
- Surface: `let item: Scored = Key(n: 3)`.
- Semantics: compiler-known marker traits are not value types. No class subtyping; use traits or construct families.
- Implementation/tests: trait semantic lowering and parity.

## Construct families

- Problem solved: declarative requirements/content slots and family-based dynamic values.
- Kira today: `some Family` and `Any Family` are existential construct values; family names are not ordinary types. Trailing content blocks and builders use `For(...)` and content `if`.
- Status: `KEEP`.
- Surface: `let widget: some Widget = Button()`, `Any Widget`, `Row { Text("a") }`.
- Semantics: child slots, defaults, `init`, `copy/update`, and family conformance are frontend features. No Rust trait/object syntax should be copied.
- Implementation/tests: construct parser, semantic family tables, HIR normal construction, all backends.

## Tasks and main-thread handles

- Problem solved: deferred work and host-thread scheduling.
- Kira today: `Task { namedCall }`, `task.await`, `.requestCancel()`, `.detach()`, and `MainThread.invoke/spawn/post { direct @MainThread call }`.
- Status: `KEEP`.
- Semantics: opaque handles, explicit supported operations, Send checks, target-specific Web refusal for main-thread APIs.
- Implementation/tests: task HIR/IR/runtime and main-thread backend parity.

## Compile-time syntax values

- Problem solved: hygienic source transformation and reflection.
- Kira today: `quote { ... }`, `#{ value }`, `Syntax`, `Identifier`, `TypeRef`, `Declaration`, `Field`, and `Diagnostics` in comptime macros (`sites/docs/content/docs/macros/reflection.mdx:6-67`).
- Status: `KEEP`.
- Semantics: `String` does not convert to `Identifier`; reflected identifiers preserve syntax identity. Splice behavior depends on static type.
- Implementation/tests: macro frontend; no backend work after expansion. `MxxMacroTests.kira`.

# Decision index

## Keep decisions

- ASCII identifiers and dotted qualified names.
- Contextual keyword strategy.
- `//` and `///` comments with source-rescanned documentation.
- Existing punctuation and delimiter set.
- `@Name` and `@FFI.*` annotation syntax.
- Decimal and hexadecimal integer literals.
- Plain literal typing against fixed integer/float spellings.
- Compile-time literal overflow diagnostics.
- Finite float literals and runtime IEEE NaN/infinity behavior.
- Boolean literals and strict Boolean operations.
- Double-quoted strings and escaped-newline continuation.
- Existing literal, name, field, method, free-call, generic-call, closure, task, enum, struct, array, and conditional expressions.
- `foo.bar`, ordinary field/property access, parent-qualified class members.
- `foo.await` only as a task/main-thread-handle property.
- `x[i]`, including mutable index places and supported FFI pointer indexing.
- `-`, `!`, `~`, `move`, and `copy` semantics.
- Binary precedence, associativity, arithmetic, comparison, Boolean, bitwise, and shift rules.
- Short-circuiting and branch laziness.
- Explicit assignment statements.
- Explicit conversion calls and FFI pointer/bit intrinsics.
- Half-open ranges only in `for`.
- Arrays, named structs, enums, closures, functions, `Void`, `Any`, `Distinct`, traits, construct families, tasks, and FFI seam values.
- Place/value distinction, temporary restrictions, ownership, drop scopes, match exhaustiveness, and backend parity.

## Change decisions

- Correct docs to list `distinct` as reserved.
- Correct newline/semicolon documentation: lexer discards newline and parser recognizes structure.
- Resolve claims about optional call separators.
- Align tree-sitter escape acceptance with compiler diagnostics.
- State UTF-8 source handling and BOM/non-ASCII behavior.
- Fix `var name: Type` contradiction: docs and harness use uninitialized `var`, but parser currently emits `KPAR011`.
- Prefer implementing definite assignment for uninitialized `var`:
  `var value: Int`, later `value = 1`.
- Extend `move`/`copy` parser lookahead to every valid primary operand.
- Pin/document field-initializer evaluation order.
- Correct or qualify reflection docs that show unsupported `static function` syntax.

## Add decisions

- Integer digit separators: `1_000`, `0xFF_FF`.
- Float exponents: `1e5`, `1.25e-3`, `1E+5`.

Both additions are lexical sugar and do not require HIR, IR, runtime, or ABI changes.

## Explicit reject decisions

- Unicode and escaped/raw identifiers for 1.9.1.
- Octal and binary integer literals.
- Integer/float suffixes.
- Dedicated NaN/infinity literals.
- Character type and character literals.
- Byte-string and byte-character literals.
- C-string literal syntax.
- Runtime `x.type` reflection and Rust `Type::member`/associated static syntax.
- Tuple members `.0` and Rust tuple syntax for 1.9.1.
- Standalone range values and inclusive `..=`.
- First-class `&`, `&mut`, `*reference`, reference patterns, and auto-deref.
- Rust wildcard/`else` match arms.
- Move-closure modifier syntax.
- Arbitrary async blocks.
- Rust `?` propagation syntax.
- Unsafe blocks.
- Const blocks.
- Never type.
- Unit literal `()`.
- Trailing-expression returns.
- Value `break`, loop labels, and Rust expression-valued control-flow forms.

Each rejection has an existing Kira alternative or would require a language/ownership/ABI design that is not justified for 1.9.1.

## Defer decisions

- Raw strings and triple-quoted strings.
- String interpolation.
- Arbitrary callable-expression calls such as `make()(x)`.
- Compound assignment.
- Tuple values and tuple patterns.
- Slice values and borrowed views.
- General `if-let`, `let-else`, and `while-let`.
- Literal, range, struct, tuple, nested, OR, `@`, guard, reference, and generalized binding patterns.
- Full pattern reachability beyond duplicate enum variants.
- General refutable/irrefutable pattern analysis.
- Generalized destructuring in parameters, `let`, `for`, and closures.

# Unresolved cross-agent questions

1. Is uninitialized `var` intended for Kira 1.9.1? The docs and `tests-kik` contain it, but `crates/kira-parser/src/stmt.rs:91-100` rejects it.
2. Should 1.9.1 implement definite assignment or remove/correct the existing documented syntax?
3. Is the current working-tree closure/function-value implementation the release baseline? Some older comments still say Kira has no function type, while `closures/function_values.rs` implements function values.
4. Should field initializer side effects be guaranteed in written order or declaration order after HIR normalization?
5. Should call arguments without commas be accepted when separated by newlines, or should docs explicitly require commas?
6. Should the compiler reject UTF-8 BOM or accept and strip it?
7. Should unknown escapes remain parser-recoverable after `KLEX003`, or should the compiler make them hard errors consistently with tree-sitter?
8. Is `static function` in reflection documentation intended as Kira source or only an abstract API model?
9. Are `CString` foreign results and callback results intentionally asymmetric? Current tests say foreign results become owned `String`, while callback C-string results are refused.
10. Which proposed additions are mandatory for 1.9.1 versus audit-only recommendations?

# Implementation dependency graph

| Prerequisite | Dependent work |
| --- | --- |
| Lexer number scanner | Integer separators and float exponents |
| Lexer syntax contract | Tree-sitter grammar, lexical docs, parser negative tests |
| `var` AST optional initializer | Definite assignment analysis |
| Definite assignment analysis | HIR uninitialized slots, ownership/drop initialization state |
| HIR uninitialized slots | MID scope release, VM storage, LLVM allocas/live flags, Hybrid parity |
| Generalized pattern AST | `if-let`, `let-else`, `while-let`, destructuring, guards, OR patterns |
| Pattern ownership model | Exhaustiveness, refutability, partial moves, all pattern positions |
| Tuple/slice/reference type model | Type checker, ownership, layout, HIR/IR, VM/LLVM/FFI/Web |
| Arbitrary callable AST | Dynamic call HIR/IR, VM dispatch, LLVM/Web call representation, ABI |
| Value-producing blocks/loops | Control-flow HIR, temporary ownership, definite return, backend joins |
| FFI layout changes | Runtime ABI, C adapters, generated bindings, target-specific parity |

The 1.9.1 low-risk lane is independent:

1. Align docs and tree-sitter with actual syntax.
2. Add integer separators and float exponents.
3. Resolve uninitialized `var`.
4. Fix ownership operand parsing.
5. Add missing `Tkik` coverage.
6. Run VM/LLVM/Hybrid/Web validation.

# Kira 1.9.1 landing requirements

Kira 1.9.1 should land with:

- A single authoritative lexical contract.
- Integer separators and float exponents, if accepted by the release decision.
- A resolved and tested `var` definite-assignment policy.
- Correct `move`/`copy` parsing for every valid operand.
- Correct documentation for `distinct`, newline handling, commas, UTF-8, and unsupported syntax.
- Tree-sitter grammar synchronized with compiler behavior.
- `tests-kik` coverage for every supported syntax/behavior, including tiny forms:
  `foo.bar`, `foo.0`, `foo.await`, `_`, `Self`, `self`, `!`, `..`, `..=`, and `x[i]`.
- VM, LLVM/native, Hybrid, and Web parity evidence for every supported changed form.
- Explicit diagnostics and negative tests for rejected Rust forms.
- No tuple/reference/slice/general-pattern/unsafe/async-block ABI expansion in 1.9.1 unless the corresponding dependency graph is implemented completely.
