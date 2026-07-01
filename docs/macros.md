# Macros

Kira has two macro forms. `macro` is **declarative**: it binds expression fragments and
substitutes them into a fixed template, with no compile-time execution. `comptime macro` is
**procedural**: it is a real compile-time function that receives syntax, runs arbitrary Kira code
against it (loops, conditionals, string building, calls into the compiler reflection API), and
returns the syntax to splice in.

Both forms are a pure **frontend AST → AST transform** that runs after parsing and before
semantic analysis / HIR lowering. Expansion output flows through the normal
`kira_source -> kira_lexer -> kira_parser -> [macro expansion] -> kira_semantics -> kira_ir ->
backends` pipeline like any hand-written code, so **VM, LLVM/native, hybrid, and WASM parity is
structural**: there is no per-backend macro work. A macro can never produce code that runs on one
backend and not another, because by the time a backend sees it, it is ordinary Kira AST.

`comptime macro` *bodies* execute on the same compile-time evaluator as `comptime function`. The
reflection types (`Syntax`, `Declaration`, `Field`, `TypeRef`, `Identifier`, `Diagnostics`) are
compile-time runtime surface and are validated by their own tests; they never reach a backend.

This page documents the macro language surface. Forms below are exercised by the corpus under
`tests/pass/check/macro_*`, `tests/pass/run/macro_*`, and
`tests/fail/semantics/macro_*` / `tests/fail/parse/macro_*`.

## Invocation summary

| Form | Declared with | Invoked as | Backed by |
| --- | --- | --- | --- |
| Declarative | `macro Name(p: expr) { expand { … } }` | `Name!(arg)` | fixed template |
| Procedural, function | `comptime macro Name { kind { function } … }` | `Name!(arg)` | compile-time code |
| Procedural, attribute | `comptime macro Name { kind { attribute } … }` | `@Name` above a declaration | compile-time code |
| Procedural, derive | `comptime macro Name { kind { derive } … }` | `@Derive(Name, …)` above a declaration | compile-time code |

A trailing `!` at the call site marks every value-position macro, declarative or procedural-function
— the user always sees that arguments are *unevaluated syntax*, not values. Attribute and derive
macros attach to a declaration with `@`. `@Derive` takes a comma-separated list and runs each derive
over the same declaration.

## Fragment evaluation and ownership (the load-bearing rule)

A `macro` parameter is a **fragment**: a piece of syntax captured at the call site. Each fragment is
declared with a *kind*. v1 has two:

- `expr` — a single expression, captured **call-by-value**.
- `place` — an assignable lvalue path (a variable, field, or index target).

### `expr` is evaluated exactly once

This is the rule that makes macros compose with Kira's affine ownership instead of fighting it. An
`expr` fragment is evaluated **exactly once** at the call site into a hygienic temporary; that
temporary is then substituted at every occurrence of the parameter in `expand`. Concretely:

```kira
macro square(value: expr) {
    expand {
        value * value
    }
}

let n = square!(buildThing())
```

expands as if written:

```kira
let n = {
    let _value$0 = buildThing()   // evaluated once
    _value$0 * _value$0
}
```

`buildThing()` runs **once**, never twice — there is no C-style double-evaluation footgun, even
though `value` appears twice in the template.

Ownership is **unchanged** by macros. If the fragment's value is a non-`Copy` (owned) type and the
template *consumes* it in more than one position, that is an ordinary affine move error
(`KSEM` move diagnostics) — exactly the error the same code would produce written by hand. Macros do
not relax, hide, or duplicate ownership; they only guarantee single evaluation. The mental model is:
**`expr` macros are referentially transparent with respect to evaluation count, and transparent with
respect to ownership.**

### `place` is an assignable lvalue

Some macros need to read *and write* their arguments — `swap!` is the canonical case. Those
parameters are declared `place`. A `place` fragment is substituted as an lvalue path and is read or
written exactly where the template reads or writes it, with normal Kira semantics — the same as
hand-writing the path.

```kira
macro swap(a: place, b: place) {
    expand {
        let temporary = a
        a = b
        b = temporary
    }
}

swap!(left, right)
```

expands to:

```kira
let temporary$0 = left
left = right
right = temporary$0
```

Each side is moved exactly once, so this is a correct affine swap for owned values as well as
`Copy` ones. A `place` argument must be a real assignable path; passing a non-lvalue
(`swap!(1, x)`) is a diagnostic (`KMAC004`). Index/field paths used as `place` arguments evaluate
their sub-expressions where they appear in the template; prefer `expr` for everything that does not
genuinely need to be written through.

`ident` (a bare name) and `type` (a type reference) are reserved fragment kinds for a near-term
extension; v1 ships `expr` and `place` only.

## Hygiene

Any identifier introduced inside `expand` that is **not** one of the macro's own fragment
parameters is hygienic. Each expansion gets a fresh, compiler-generated name for it (shown above as
`temporary$0`, `_value$0`), so:

- Two separate `swap!` calls in the same function never share a `temporary`.
- A real variable named `temporary` at the call site is never shadowed or captured by the macro, and
  the macro's `temporary` is never visible to the caller.

Fragment parameters are the *only* names that cross the boundary, and they cross as the caller wrote
them, resolved in the caller's scope. This is non-negotiable: a macro cannot reach into the call
site and bind, shadow, or capture a name the caller did not pass in.

## `expand` is a block-expression

`expand { … }` is a block. Where the macro is invoked determines how it is used:

- In **expression position** (`let x = clamp!(…)`), the block's **trailing expression** is its
  value.
- In **statement position** (`swap!(left, right)`), the block's statements are spliced in place.

A macro whose `expand` ends in a statement (like `swap!`) is statement-position only; using it as a
value is a diagnostic (`KMAC005`). A macro whose `expand` ends in an expression works in both
positions.

```kira
macro clamp(value: expr, low: expr, high: expr) {
    expand {
        if value < low {
            low
        } else if value > high {
            high
        } else {
            value
        }
    }
}

let opacity: Float64 = clamp!(rawOpacity, 0.0, 1.0)
```

The trailing `if/else` is the block's value because `clamp!` is used in expression position.

A multi-statement expansion in statement position:

```kira
swap!(left, right)
// =>
let temporary$0 = left
left = right
right = temporary$0
```

## Procedural macros: `comptime macro`

```kira
comptime macro Name {
    kind { function }                       // or: attribute | derive
    appliesTo { struct, class, enum }       // required for attribute/derive; omitted for function

    expand(input: Syntax) -> Syntax {       // function:  (Syntax)      -> Syntax
        body                                // attribute: (Declaration) -> Syntax
    }                                       // derive:     (Declaration) -> Syntax
}
```

`kind` is required and fixed to one of `function`, `attribute`, `derive`; it determines both the
call syntax and the signature of `expand`. `appliesTo` is required for `attribute` and `derive`
(it lists the declaration kinds the macro is legal on) and omitted for `function`. `expand` is the
one member every `comptime macro` must define, and its body is ordinary Kira run at compile time on
the same evaluator as `comptime function`.

### Expansion ordering and visibility

When a declaration carries several macros (`@A @B` and/or `@Derive(C, D)`):

- Every attribute and derive macro observes the **original** declaration. No macro ever sees another
  macro's output.
- Outputs are **concatenated** with the original declaration; the result order follows source order
  of the annotations.
- Because no macro sees another's output, sibling-generated blocks can never form an ordering
  dependency on each other.

### Compiler reflection API

```kira
struct Syntax {
    function identifiers() -> [Identifier]
    static function join(items: [Syntax], separator: String) -> Syntax
}

struct Identifier {
    function asString() -> String
    // No `String -> Identifier`. See "Hygiene boundary" below — this absence is deliberate.
}

struct Declaration {
    var name: Identifier
    var fields: [Field]
    var syntax: Syntax
}

struct Field {
    var name: Identifier
    var type: TypeRef
}

struct TypeRef {
    function asSyntax() -> Syntax
}

struct Diagnostics {
    static function error(message: String, at: Syntax)
}
```

#### Hygiene boundary: no `String → Identifier`

There is **no** way to turn a `String` into an `Identifier`. A macro can only obtain an identifier
from reflection (`target.name`, `field.name`) or from a hygienic gensym introduced inside `quote`.
This is a deliberate hygiene guarantee: a macro **cannot fabricate a name from a string and use it
to capture** something at the call site. It is also why the use-site property-wrapper rewrite
(below) is compiler-owned rather than a macro — a macro literally cannot mint the `_count` / `$count`
names.

The controlled escape hatch reserved for a later version is
`Identifier.derived(base: Identifier, prefix: String, suffix: String)`, which can only *extend* an
identifier the macro already legitimately holds — never conjure one from thin air.

### `quote` and `#{ … }` splicing

`quote { … }` is a compiler intrinsic, not a function: the literal Kira syntax inside the braces
becomes a `Syntax` value instead of running. Inside `quote`, `#{ value }` splices a value in. **What
it splices to is chosen by the static type of the value, not by where it sits** — so there is no
case-by-case ambiguity:

| Static type of `value` | Splices as |
| --- | --- |
| `Syntax` | the syntax, as-is |
| `Identifier` | a bare name (usable as a binding, type name, or member access) |
| `String` | a quoted string literal |
| `Int` / `Bool` | its literal |
| `[T]` of any of the above | each element in sequence, nothing between them |

Array splicing inserts elements with nothing between them, which is correct for statement lists and
declaration bodies. Where a comma-separated list is needed (a parameter list), build it explicitly
with `Syntax.join(items, separator: ", ")` and splice the single joined `Syntax`.

The same source expression can splice two different ways by type. `target.name` is an `Identifier`
and splices bare as `Player`; `target.name.asString()` is a `String` and splices as `"Player"`.

### Function-like procedural macro

The case a declarative `macro` genuinely cannot reach — the output size depends on the input:

```kira
comptime macro bitflags {
    kind { function }

    expand(input: Syntax) -> Syntax {
        let names: [Identifier] = input.identifiers()
        var constants: [Syntax] = []
        var value: Int = 1

        for name in names {
            constants.append(quote {
                static let #{name}: Int = #{value}
            })
            value = value * 2
        }

        return quote {
            struct Flags {
                #{constants}
            }
        }
    }
}

bitflags!(Read, Write, Execute)
```

expands to:

```kira
struct Flags {
    static let Read: Int = 1
    static let Write: Int = 2
    static let Execute: Int = 4
}
```

### Attribute macro

Attached to one declaration; sees only that declaration; returns syntax added alongside it.

```kira
comptime macro Loggable {
    kind { attribute }
    appliesTo { struct, class }

    expand(target: Declaration) -> Syntax {
        var lines: [Syntax] = []
        for field in target.fields {
            lines.append(quote {
                output = output + #{field.name.asString()} + ": " + self.#{field.name}.toString() + " "
            })
        }
        return quote {
            extend #{target.name} {
                function log() {
                    var output: String = #{target.name.asString()} + " { "
                    #{lines}
                    Console.print(output + "}")
                }
            }
        }
    }
}
```

### Derive macro

Same shape as attribute, invoked through `@Derive(...)`, list-friendly.

```kira
comptime macro MemberwiseInit {
    kind { derive }
    appliesTo { struct }

    expand(target: Declaration) -> Syntax {
        var parameters: [Syntax] = []
        var assignments: [Syntax] = []
        for field in target.fields {
            parameters.append(quote { #{field.name}: #{field.type.asSyntax()} })
            assignments.append(quote { self.#{field.name} = #{field.name} })
        }
        let parameterList: Syntax = Syntax.join(parameters, separator: ", ")
        return quote {
            extend #{target.name} {
                init(#{parameterList}) {
                    #{assignments}
                }
            }
        }
    }
}

@Derive(Debug, MemberwiseInit)
struct Vec2 {
    var x: Float64
    var y: Float64
}
```

`@Derive(Debug, MemberwiseInit)` runs both and produces both `extend` blocks.

## Case study: property wrappers (where the macro line falls)

`PropertyWrapper` is an ordinary attribute macro. It validates that the annotated struct has a
`wrappedValue` member, records whether it also has `projectedValue`, and generates conformance
query functions for the type:

```kira
comptime macro PropertyWrapper {
    kind { attribute }
    appliesTo { struct }

    expand(target: Declaration) -> Syntax {
        var hasWrappedValue: Bool = false
        var hasProjectedValue: Bool = false
        for field in target.fields {
            if field.name.asString() == "wrappedValue" { hasWrappedValue = true }
            if field.name.asString() == "projectedValue" { hasProjectedValue = true }
        }
        if hasWrappedValue == false {
            Diagnostics.error("PropertyWrapper requires a wrappedValue field", at: target.syntax)
            return quote { }
        }
        return quote {
            function is_#{target.name}_propertyWrapper() -> Bool { return true }
            function has_#{target.name}_projectedValue() -> Bool { return #{hasProjectedValue} }
        }
    }
}

@PropertyWrapper
struct State {
    var wrappedValue: Int
    var projectedValue: Bool
}
```

The conformance is surfaced as free functions whose names are glued to the annotated type via a
mid-identifier splice (`is_#{target.name}_propertyWrapper` → `is_State_propertyWrapper`), because
Kira `extend` applies only to *constructs* — not plain structs — and Kira has no `static` members,
so the `extend T: Conformance { static let ... }` shape is not expressible here. The behaviour is
identical in spirit: the macro sees one declaration, validates it, and emits a Bool-returning
conformance surface derived from its fields. This exact macro is exercised end-to-end across
vm / llvm / hybrid in `tests/pass/run/macro_property_wrapper` (with the missing-`wrappedValue`
diagnostic, `KMAC021`, pinned by `tests/fail/semantics/macro_property_wrapper_missing`).

That is *everything* `PropertyWrapper` does: it validates one declaration and tags it. An attribute
macro only ever sees the single declaration it is attached to, so it has no way to reach into
`Widget Counter` and rewrite a `@State var count: Int = 0` somewhere else in the program — that is a
different declaration, one that does not even exist until `State` has already been validated.

So the lowering of `@State var count: Int = 0` is **not a macro**. It is a single fixed rule, owned
by the compiler, that runs whenever a property declaration is annotated with a validated
property-wrapper type (one whose `@PropertyWrapper` check passed). Given `@State var count: Int = 0`,
the compiler generates a backing
field (`_count`), a computed property under the original name proxying `wrappedValue`, and — only if
`hasProjectedValue` was recorded `true` — a `$`-prefixed computed property proxying
`projectedValue`:

```kira
Widget Counter() {
    var _count: State = State(wrappedValue: 0)
    var count: Int {
        get { return _count.wrappedValue as Int }
        set(newValue) { _count.wrappedValue = newValue }
    }
    var $count: Binding {
        get { return _count.projectedValue as Binding }
    }
    body { /* references `count` by that exact name */ }
}
```

`_count`, `count`, `$count` are **not** hygienic gensyms — they are derived deterministically from
the literal property name, because the whole point is that `count` stays referenceable by that exact
name everywhere else in the widget. This is the only place in the system where a name is
intentionally derived rather than passed through as a fragment or hygienically renamed, and it is
compiler-owned precisely because (a) it depends on a fact recorded by a *different* declaration and
(b) it applies uniformly to every wrapper type. The hygiene boundary (no `String -> Identifier`)
makes this division of labor enforced rather than incidental: a macro could not implement the
use-site rewrite even if it wanted to.

## Diagnostics

| Code | Condition |
| --- | --- |
| `KMAC001` | unknown macro at a `!` call site |
| `KMAC002` | wrong fragment count at a `!` call site |
| `KMAC003` | `expr`/`place` fragment kind mismatch (e.g. non-expression for `expr`) |
| `KMAC004` | `place` fragment given a non-assignable argument |
| `KMAC005` | statement-only macro used in expression position |
| `KMAC006` | `comptime macro` missing `kind`, or `kind` not one of `function`/`attribute`/`derive` |
| `KMAC007` | attribute/derive macro applied to a declaration kind not in `appliesTo` |
| `KMAC008` | `appliesTo` present on a `function` macro, or absent on attribute/derive |
| `KMAC009` | `#{ … }` splice of a type with no splice rule |
| `KMAC010` | macro recursion/expansion-depth limit exceeded |
| `KMAC011` | `@Derive(X)` where `X` is not a `derive`-kind macro |
| `KMAC012` | `comptime macro` `expand` signature does not match its `kind` |
| `KMAC016` | a function macro used in statement position whose expansion does not parse as statements |
| `KMAC017` | a function macro used in expression position whose expansion is not a single expression |

## Implementation status

**Declarative `macro` — implemented and parity-verified.** Lexing (`macro` keyword), parsing
(`macro Name(p: expr|place) { expand { ... } }` and `name!(args)` calls), and the AST→AST expansion
pass (`packages/kira_build/src/macro_expand.zig` + `macro_instantiate.zig`) are complete. The pass
runs at every frontend entry (`compileFileToIr`, `checkFileFrontend`, `checkPackageRoot`) before
semantics. Covered by `tests/pass/run/macro_declarative` (vm + llvm + hybrid, all green) and the
negative cases `tests/fail/semantics/macro_{unknown,arg_count,place_not_lvalue,stmt_only_as_value}`.

Current declarative limitations (each produces a clear diagnostic, never silent or wrong):

- A macro used in expression position must have a template whose trailing item is a real
  *expression*. The `clamp` example above ends its `expand` in an `if/else`; Kira `if` is a
  *statement*, so that template currently raises `KMAC005`. Making it work needs block/if
  expressions (a separate language feature), at which point no macro change is required.
- Invoking a macro from inside another macro's template (`KMAC015`) is not yet expanded.
- Template bodies support let / assignment / expression / return / if / for / while; other
  statement forms raise `KMAC014`.

**Procedural `comptime macro` — derive and attribute macros implemented and parity-verified.** A
focused compile-time tree-walking evaluator (`packages/kira_build/src/macro_eval.zig`) runs the
`expand` body at compile time over `Value`s, with `Syntax` modeled as Kira source text:
`quote { ... }` renders to source (filling `#{}` splices by value type) and the expansion pass
re-parses the result with `parser.parseSource` and splices the generated declarations. Implemented
reflection surface: `Declaration.{name,fields,syntax}`, `Field.{name,type}`, `Identifier.asString`,
`TypeRef.asSyntax`, `Syntax.join`, array `.append`/`.len`, and `Diagnostics.error`; plus `for`/`if`/
`while`, `let`/`var`/assignment, Int/Bool/String arithmetic and concatenation, and `quote`/`#{}`.

Invocation: `@Derive(A, B)` runs each derive macro over the original declaration; `@Name` runs an
attribute macro; both strip their annotation and append the generated declarations. Validated across
vm/llvm/hybrid in `tests-kik/harness/app/macros/MxxMacroTests.kira` (`MxxDeriveFieldCount`,
`MxxDeriveSum`, `MxxAttributeMacro`).

**Function-position** procedural macros (`name!(args)` backed by code, e.g. `bitflags`) are
implemented in **all three positions**:

- **Top level** (declaration position): a `name!(args)` item parses to a `macro_invocation`
  declaration, the arguments are rendered to source as the macro's `Syntax` `input`, and
  `expand(input)` runs and splices the generated declarations. Validated by `MxxFunctionMacro`.
- **Statement position**: the expansion is re-parsed as a statement list and spliced in place of the
  `name!(...)` call (`KMAC016` if the output does not parse as statements). Procedural macros emit
  raw source and are **not** hygienic, so generated names bind in the caller's scope by design.
  Validated by `MxxFuncMacroStmt`.
- **Expression position**: the expansion must re-parse as a single expression, which becomes the
  value (`KMAC017` otherwise). Validated by `MxxFuncMacroExpr`.

`input.identifiers()` lexes the argument text.

`quote` splices glue by **source adjacency**: `mxp_#{name}` (no space before `#{`) renders as a
single identifier `mxp_Foo`, while `a + b` keeps its spaces. Validated by `MxxSpliceGlue`.

Attribute/derive macros apply to `struct`, `class`, **and `enum`** declarations; an enum's variants
surface through `target.fields` (`field.name` is the variant name, `field.type` its payload type or
empty). `appliesTo` is enforced: applying a macro to a declaration kind not in its `appliesTo` list
reports `KMAC007`. Validated by `MxxEnumDerive`.

Procedural limitations (clear behavior, no fake success):
- Evaluator coverage is the documented reflection surface; an unsupported construct in an `expand`
  body reports `KMAC020` rather than miscompiling.

## Parity and execution notes

- Expansion is a frontend pass producing ordinary AST; **all backends are unaffected and identical
  by construction**. No `vm`/`llvm`/`hybrid`/`wasm` split exists in macro handling.
- `comptime macro` bodies run on the compile-time evaluator (the VM used for `comptime function`).
  The reflection API is compile-time-only runtime surface; it is not lowered to any backend and is
  covered by dedicated tests.
- Expansion has a depth limit (`KMAC010`) to bound recursive and mutually-recursive macros.
- Declarative `macro` performs **no** compile-time execution; it is pure template substitution with
  single-evaluation `expr` semantics and hygiene.
