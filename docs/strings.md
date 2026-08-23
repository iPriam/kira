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
| `s.dropLastScalar()` | `String` | `s` without its last Unicode scalar |
| `scalarText(c)` | `String` | the text of the Unicode scalar at code point `c` |

Bytes rather than characters is the choice these four make together. A program
that carves text at a delimiter it located itself hands the index it got back to
the operation that slices — so the index has to mean the same thing to both, and
a character count beside a byte index would not. `"café".count` is 5 and
`"中文".count` is 6, because that is how many bytes those strings hold.

`String(x)` renders exactly what `print` would, so a value printed and a value
converted never disagree: `String(2.0)` is `"2"`, `String(1.0 / 3.0)` is
`"0.3333333333333333"`, `String(true)` is `"true"`.

An empty needle matches at the front, so `s.indexOf("")` is `0`.

## Writing one

A literal resolves seven escapes — `\n`, `\t`, `\r`, `\e`, `\0`, `\"` and
`\\`. A backslash before anything else is an error (`KLEX003`) rather than the
character itself, so a Windows path written with single backslashes is caught
where it is written instead of arriving somewhere as text nobody meant.

A backslash before a **newline** continues the literal on the next line:

```kira
let message = "this compositor offers no wl_compositor or no \
               xdg_wm_base, and a window needs both"
```

That is one string with no newline in it. The line break and the indentation
lining the second line up under the first are layout for whoever reads the
source; neither reaches the value, so the literal above is exactly the string
written on one long line. A run of spaces in the middle of a sentence is never
what the indentation meant.

The space before the backslash is ordinary text and is kept, which is where the
one between `no` and `xdg_wm_base` comes from. A continuation written without
one joins the halves directly:

```kira
let joined = "un\
              split"          // "unsplit"
```

This is the only escape whose meaning is *produce nothing*, and it is why a
message too long for a line does not have to be concatenated out of pieces.

## The two operations that count scalars, not bytes

`dropLastScalar` and `scalarText` are the exceptions, and they exist because
backspace is not a byte operation. `"kirä".substring(0, 3)` cuts the `ä` in half
and leaves bytes that are no longer UTF-8; `"kirä".dropLastScalar()` is `"kir"`,
and `"A😀".dropLastScalar()` is `"A"` though the emoji is four bytes.

`scalarText` is the inverse, and the reason a text field needs no C helper: a key
press arrives as a code point, and `base + scalarText(code)` appends it. A code
point outside Unicode, or a surrogate half, names no scalar and renders as the
empty string rather than trapping — there is no wrong answer to give, and a trap
would make a keyboard event able to end a program.

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
