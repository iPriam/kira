# Strings

A Kira `String` is a value: it owns its bytes, a copy is independent, and `+`
builds a fresh one. On top of that it answers one property and three methods,
and every one of them works in **bytes**.

| Written | Result | Meaning |
| --- | --- | --- |
| `s.count` | `Int` | length in bytes |
| `s.charAt(i)` | `Int` | the byte at index `i` |
| `s.substring(a, b)` | `String` | the half-open byte range `a ..< b` |
| `s.indexOf(needle)` | `Int` | the first byte index of `needle`, or `-1` |
| `String(x)` | `String` | a scalar rendered as text |

Bytes rather than characters is the choice these four make together. A program
that carves text at a delimiter it located itself hands the index it got back to
the operation that slices — so the index has to mean the same thing to both, and
a character count beside a byte index would not. `"café".count` is 5 and
`"中文".count` is 6, because that is how many bytes those strings hold.

`String(x)` renders exactly what `print` would, so a value printed and a value
converted never disagree: `String(2.0)` is `"2"`, `String(1.0 / 3.0)` is
`"0.3333333333333333"`, `String(true)` is `"true"`.

An empty needle matches at the front, so `s.indexOf("")` is `0`.

## Out of range is a trap

`charAt` and `substring` **trap** rather than clamping or returning a sentinel.
An index below zero or at or past the end, a `substring` whose start is past its
end, and a range that would split a multi-byte character are all failures with
no answer — so a program that walks off the end of a string stops, identically
on the VM, the LLVM backend, and the hybrid split, instead of producing a value
only one of them agrees with.

That is not only a safety property; it is a *usable* one. Foundation's
`@Derive(Deserializable)` relies on it: every structural violation in a wire
string drives an out-of-range `charAt` or an inverted `substring`, which is what
makes malformed input a deterministic stop rather than a partial parse. See
`docs/macros.md` for the format.

## Where this lives

`kira-semantics/src/strings.rs` type-checks the surface; the four operations
lower through `HirExpr`/`IrExpr` to the VM's `StringCharAt`, `StringSubstring`,
`StringIndexOf`, and `StringOf` opcodes, and to the `kira_rt_str_char_at`,
`kira_rt_str_substring`, `kira_rt_str_index_of`, and `kira_rt_str_of_*` runtime
helpers on the native side. Both engines are held to the same answers by
`crates/kira-cli/tests/backend_parity/macros.rs` and
`crates/kira-cli/tests/backend_parity/strings.rs`, traps included.
