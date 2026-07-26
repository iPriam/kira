# Macros

Kira has two macro forms, and both are pure frontend source-to-source
transforms that run after lexing and before semantic analysis. `macro` is
**declarative**: it binds expression fragments and substitutes them into a fixed
template, with no compile-time execution. `comptime macro` is **procedural**: a
real compile-time function that receives syntax, runs arbitrary Kira against it,
and returns the syntax to splice in.

Because expansion output flows through the normal
`kira-source → kira-lexer → kira-macros → kira-parser → kira-semantics →
kira-ir → backends` pipeline like any hand-written code, **VM, LLVM/native,
hybrid, and WASM parity is structural**. A macro cannot produce code that runs
on one backend and not another: by the time a backend sees it, it is ordinary
Kira. There is no per-backend macro work.

## Invocation summary

| Form | Declared with | Invoked as |
| --- | --- | --- |
| Declarative | `macro Name(p: expr) { expand { … } }` | `Name!(arg)` |
| Procedural, function | `comptime macro Name { kind { function } … }` | `Name!(arg)` |
| Procedural, attribute | `comptime macro Name { kind { attribute } … }` | `@Name` above a declaration |
| Procedural, derive | `comptime macro Name { kind { derive } … }` | `@Derive(Name, …)` above a declaration |
| Procedural, field-triggered | `kind { attribute } trigger { field } replace { true }` | `@Name` on a *field* of a declaration |
| Procedural, wrapper | `comptime macro Name { kind { wrapper } … }` | `@Name` declares a template; the template's name on a field summons it |

A trailing `!` marks every value-position macro, so a reader always sees that
the arguments are unevaluated syntax rather than values. Attribute and derive
macros attach to a declaration with `@`, and `@Derive` takes a comma-separated
list, running each derive over the same declaration.

## Fragment evaluation and ownership

A `macro` parameter is a **fragment**: a piece of syntax captured at the call
site, declared with a kind. v1 has `expr` (a single expression, captured
call-by-value) and `place` (an assignable lvalue path). `ident` and `type` are
reserved for a near-term extension.

### `expr` is evaluated exactly once

This is the rule that makes macros compose with Kira's affine ownership instead
of fighting it.

```kira
macro square(value: expr) {
    expand {
        value * value
    }
}

let n = square!(buildThing())
```

expands as if written

```kira
let __kmac_value_0 = buildThing()   // hoisted ahead of the statement, run once
let n = (__kmac_value_0 * __kmac_value_0)
```

so `buildThing()` runs once, never twice, even though `value` appears twice in
the template. An argument that cannot do anything when read again — a literal, a
name, a field path, an index — is substituted directly rather than hoisted,
because repeating it is already a single evaluation.

Ownership is **unchanged** by macros. If the fragment's value is a non-`Copy`
type and the template consumes it in more than one position, that is an ordinary
affine move error — exactly the error the same code would produce written by
hand. Macros do not relax, hide, or duplicate ownership; they only guarantee
single evaluation.

### `place` is an assignable lvalue

Some macros need to read *and* write their argument. `swap!` is the canonical
case:

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

expands to

```kira
let __kmac_temporary_0 = left
left = right
right = __kmac_temporary_0
```

Each side moves exactly once, so this is a correct affine swap for owned values
as well as `Copy` ones. Passing a non-lvalue (`swap!(1, x)`) is `KMAC004`.

## Hygiene

Any identifier introduced inside `expand` that is not one of the macro's own
fragment parameters is hygienic: each expansion gets a fresh compiler-generated
name for it. Two separate `swap!` calls never share a `temporary`, a real
variable named `temporary` at the call site is never captured, and the macro's
`temporary` is never visible to the caller. Fragment parameters are the only
names that cross the boundary, and they cross as the caller wrote them, resolved
in the caller's scope.

Procedural macros are deliberately **not** hygienic: they emit source, and their
generated names bind in the caller's scope by design — that is what lets a
derive macro generate `eq_Point` and a caller call it.

## `expand` is a block

Where the macro is invoked decides how the block is used. In expression position
(`let x = clamp!(…)`) the block must be a single expression, which becomes the
value. In statement position (`swap!(left, right)`) the block's statements are
spliced in place. Using a statement-only macro as a value is `KMAC005`.

Kira `if` is a statement, so a template ending in `if/else` is statement-only
until block expressions exist. Nothing about macros changes when they do.

A macro may invoke another macro, in its template or in its arguments: expansion
is a fixpoint, running innermost-first until nothing is left to expand.
Recursion is bounded, and exceeding the bound is `KMAC010` rather than a hang.

## Procedural macros: `comptime macro`

```kira
comptime macro Name {
    kind { function }                       // or: attribute | derive | wrapper
    appliesTo { struct, class, enum, form } // required except for `function`
    trigger { field }                       // optional: auto-apply from a field
    replace { true }                        // optional: output REPLACES the declaration

    expand(input: Syntax) -> Syntax {       // function:  (Syntax)      -> Syntax
        body                                // attribute: (Declaration) -> Syntax
    }                                       // derive:    (Declaration) -> Syntax
}                                           // wrapper:   (Declaration, Declaration) -> Syntax
```

`kind` is required and fixed to one of the four words; it determines both the
call syntax and the signature of `expand`. `appliesTo` lists the declaration
kinds the macro is legal on and is required for everything but `function`;
`form` admits construct-backed declarations (`MyPanel Counter(…) { … }`).
`expand` is the one member every `comptime macro` must define, and its body is
ordinary Kira run at compile time.

Two opt-in members extend attribute macros. `trigger { field }` auto-applies the
macro to a whole declaration whenever one of its *fields* carries an annotation
matching the macro's name — the property-wrapper shape, where the macro sees the
full declaration and the field annotation is only the trigger. A field-triggered
macro must also be `replace { true }` (`KMAC029`), since its purpose is
rewriting the declaration that carries the field. `replace { true }` makes the
macro's output take the annotated declaration's place instead of being appended
alongside it, and at most one replace-mode macro may apply to a declaration
(`KMAC028`) — a second replacer would have no original left to observe.

### Expansion ordering

Every attribute and derive macro observes the **original** declaration; no macro
ever sees another macro's output. Outputs are concatenated with the original in
the source order of the annotations. Because no macro sees another's output,
sibling-generated blocks can never form an ordering dependency on each other.

Generated declarations are appended to the end of the file that produced them,
and everything a macro consumes — the macro declarations themselves, the
annotations they answer to — is blanked rather than deleted. Every byte the user
wrote that survives expansion keeps the offset it started at, so a diagnostic
about untouched code still points at its own line.

### Compiler reflection API

```kira
struct Syntax {
    function identifiers() -> [Identifier]
    static function join(items: [Syntax], separator: String) -> Syntax
    // Declaration-shaped Syntax only (a value derived from `target.syntax`):
    function dropField(name: Identifier) -> Syntax
    function rewriteProperty(name: Identifier, read: Syntax, writeCallee: Syntax) -> Syntax
    function replaceIdentifier(from: String, to: String) -> Syntax
}

struct Identifier { function asString() -> String }
struct TypeRef    { function asSyntax() -> Syntax }

struct Declaration {
    var name: Identifier
    var fields: [Field]
    var syntax: Syntax        // the declaration's exact source text
}

struct Field {
    var name: Identifier
    var type: TypeRef
    var initializer: Syntax   // source of the initial-value expression ("" when absent)
    var syntax: Syntax        // the whole field declaration, annotations included
    function hasAnnotation(name: String) -> Bool
}

struct Diagnostics { static function error(message: String, at: Syntax) }
```

An enum's variants surface through `target.fields`: `field.name` is the variant
name and `field.type` its payload type, or empty. One derive macro walks a
struct and an enum with the same loop.

`dropField`, `rewriteProperty`, and `replaceIdentifier` are span edits over the
declaration's original source, so untouched source survives byte-for-byte,
comments included. `rewriteProperty` walks every member body with full
lexical-scope tracking: a read is rewritten only where the property is not
shadowed by a local binding, a parameter, a `for` binding, or a `match` binding.
Assigning *through* a wrapped property (`name.x = v`, `name[i] = v`) is
`KMAC027` — the proxy has no place to write through, so read the value, change
the copy, and assign it back.

There is deliberately **no** `String → Identifier`. A macro can only obtain an
identifier from reflection or from a quote, so it cannot fabricate a name from a
string and use it to capture something at the call site.

### `quote` and `#{ … }` splicing

`quote { … }` is a compiler intrinsic, not a function: the literal Kira inside
the braces becomes a `Syntax` value instead of running. Inside a quote,
`#{ value }` splices a value in, and **what it splices to is chosen by the
static type of the value, not by where it sits**.

| Static type | Splices as |
| --- | --- |
| `Syntax` | the syntax, as-is |
| `Identifier` | a bare name |
| `TypeRef` | the written type |
| `String` | a quoted string literal |
| `Int` / `Bool` | its literal |
| `[T]` of any of the above | each element, one per line |

Anything else is `KMAC009`. The same source expression splices two ways by type:
`target.name` is an `Identifier` and splices bare as `Player`, while
`target.name.asString()` is a `String` and splices as `"Player"`.

Splices glue by source adjacency, so `mxp_#{name}` with no space before `#{`
renders as the single identifier `mxp_Foo` while `a + b` keeps its spaces. Array
splicing puts one element per line, which is right for statement lists and
declaration bodies; a comma-separated list is built explicitly with
`Syntax.join(items, separator: ", ")` and spliced as one `Syntax`.

### Function-like procedural macro

The case a declarative `macro` cannot reach — the output size depends on the
input:

```kira
comptime macro bitflags {
    kind { function }

    expand(input: Syntax) -> Syntax {
        let names: [Identifier] = input.identifiers()
        var constants: [Syntax] = []
        var value: Int = 1
        for name in names {
            constants.append(quote { function #{name}() -> Int { return #{value} } })
            value = value * 2
        }
        return quote { #{constants} }
    }
}

bitflags!(Read, Write, Execute)
```

A `function` macro works in all three positions. At file scope its expansion
must be declarations; in statement position it must parse as statements
(`KMAC016`); in expression position it must be a single expression (`KMAC017`).

### Attribute and derive macros

Both are attached to one declaration, see only that declaration, and return
syntax added alongside it. They differ only in how they are written: `@Name` for
an attribute, `@Derive(Name, …)` for a derive, which is list-friendly.

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
                init(#{parameterList}) { #{assignments} }
            }
        }
    }
}
```

`@Derive(X)` where `X` is not a derive-kind macro is `KMAC011`, and applying a
macro to a declaration kind outside its `appliesTo` is `KMAC007`.

## Property wrappers

`kind { wrapper }` is the full property-wrapper protocol: one macro defines it,
and every wrapper type is an ordinary annotated struct.

```kira
comptime macro PropertyWrapper {
    kind { wrapper }
    appliesTo { form }

    expand(target: Declaration, wrapper: Declaration) -> Syntax { … }
}

@PropertyWrapper
struct State {
    var wrappedValue: Wrapped     // `Wrapped` is the macro's placeholder for the field's type
    var key: String = ""

    function get() -> Wrapped { …storage read… }
    function set(value: Wrapped) { …storage write… }
}

MyPanel Counter() {
    @State var count: Int = 0     // works because State IS a @PropertyWrapper
    let body: Int = 1
}
```

Annotating a struct with the macro's name registers it as a wrapper **template**
and runs the macro's validation invocation, `expand(template, template)` —
`target.name == wrapper.name` discriminates the path. The macro validates the
protocol and may emit conformance declarations; the template itself is then
removed from the program, because it may carry placeholder types and is never
compiled as-is. Templates are registered in a pre-scan, so declaration order
between files and packages never matters.

A field annotated with a registered template's name summons the macro over the
enclosing declaration — `expand(target = the form, wrapper = the template)` —
and the output replaces the form. Inside that path the macro monomorphizes the
template per wrapped field with `Syntax.replaceIdentifier`, renaming the wrapper
type and substituting the field's declared type for `Wrapped`, so any field type
works without generics. It then emits per-property accessors and rewrites the
form with `dropField` and `rewriteProperty`. Storage policy lives entirely in
the wrapper struct's own `get`/`set`; the compiler knows nothing about it.

The generated names are derived by splice gluing rather than by hygienic gensym,
because the rewritten uses must be able to name them.

## The builtin `ksl!` shader macro

`ksl!("Shaders/Name.ksl")` needs no declaration: it compiles a KSL file at
compile time and expands to the artifact literal holding every backend's shader
source. Its macro surface is implemented — where it may be written, and the
validation of its single string-literal argument (`KMAC023`, `KMAC024`) — but
the KSL pipeline it delegates to is not implemented in this compiler, so a
well-formed call is refused with `KMAC022` naming what is missing rather than
expanding to a fabricated artifact. A shader that silently compiled to nothing
would take a render path down at runtime instead of at build time.

## Diagnostics

| Code | Condition |
| --- | --- |
| `KMAC001` | unknown macro at a `!` call site |
| `KMAC002` | wrong fragment count at a `!` call site |
| `KMAC003` | `expr` fragment given something that is not an expression |
| `KMAC004` | `place` fragment given a non-assignable argument |
| `KMAC005` | statement-only macro used in expression position |
| `KMAC006` | `comptime macro` missing `kind`, or naming one that does not exist |
| `KMAC007` | macro applied to a declaration kind not in its `appliesTo` |
| `KMAC008` | `appliesTo` present on a `function` macro, or absent elsewhere |
| `KMAC009` | `#{ … }` splice of a value with no splice rule |
| `KMAC010` | expansion depth limit exceeded |
| `KMAC011` | `@Derive(X)` where `X` is not a `derive`-kind macro |
| `KMAC012` | `comptime macro` `expand` does not match its `kind` |
| `KMAC016` | statement-position expansion that does not parse as statements |
| `KMAC017` | expression-position expansion that is not a single expression |
| `KMAC020` | an `expand` body used a construct the evaluator does not support |
| `KMAC021` | a macro raised `Diagnostics.error` |
| `KMAC022` | `ksl!` could not compile the shader it names |
| `KMAC023` | `ksl!` passed other than one argument |
| `KMAC024` | `ksl!` passed a path that is not a string literal |
| `KMAC025` | `Syntax.dropField` on a field that does not exist |
| `KMAC026` | a declaration-only `Syntax` method on a non-declaration value |
| `KMAC027` | assignment through a wrapped property path |
| `KMAC028` | more than one replace-mode macro on one declaration |
| `KMAC029` | a `trigger { field }` macro that is not `replace { true }` |

## Where this lives

`kira-macros` (layer 1) owns the whole pass and is called from
`kira_semantics::expanded`, the one query between reading a file and parsing it.
A program that declares no macros and writes no `ksl!` is returned
byte-identical after one lexing pass per file, so nothing downstream can tell
the pass ran.

Expansion is total: a malformed macro is reported and left unexpanded, so the
file still reaches the parser and everything else wrong with it is still
reported.

Coverage is `crates/kira-macros/src` for the pass itself and
`crates/kira-cli/tests/backend_parity/macros.rs` for the claim that matters —
the same macro-using program, the same stdout, on the VM, the LLVM backend, and
the hybrid split.
