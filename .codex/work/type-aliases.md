# Type aliases

`type Name = Target` landed as a **frontend-only** feature: the syntax tree
gains one item, the analyzer gains one lazy resolver, and nothing from the HIR
down changed at all. No IR node, no opcode, no `kira_rt_*` helper, no wasm
lowering arm, no ABI bump. The workspace build after the semantics change
compiled clean with no exhaustive-match breakage below layer 2, which is the
evidence that the boundary held.

That is not a desugar in `kira-semantics/src/stmt.rs` — there is no statement
to rewrite. An alias is resolved at the one place a written type becomes a
resolved one, `Analyzer::resolve_named_type` in `crates/kira-semantics/src/types.rs`,
so every type position gets aliases at once: parameters, return types, `let`
annotations, struct fields, enum payloads, and array elements.

## Why resolution is lazy

Aliases are registered before enums and structs are collected, and resolved on
first use. Eager resolution would have needed a topological order over a graph
that also contains structs and enums; lazy resolution needs only a three-state
guard per alias, and it is what lets `type Matrix = [Buffer]` precede
`type Buffer = [Count]`.

The guard is the cycle check. An alias reached while it is already resolving is
the chain closing on itself, reported as `KSEM157` — the oracle's code for it —
and yielding `Type::Error`.

Only a *successful* resolution is memoized. An alias whose target does not
resolve stays unresolved, so each use site reports against its own span through
its own `NameContext` instead of inheriting whichever site touched the alias
first. That is what makes `type Count = Nonexistent` report twice for two uses
rather than once at an arbitrary one.

## Collisions are refused, not shadowed

`KSEM130` rejects an alias name that a builtin, a struct, an enum, or an earlier
alias already holds. The oracle registers alias names in its top-level name
table and gets this from the generic duplicate-name check; there is no such
table here, so the check lives in `collect_type_aliases` and scans the
declarations as written (the nominal tables do not exist yet at that point).

Refusing rather than shadowing matters most for the builtin case. Builtins are
tried before aliases in `resolve_named_type`, so a tolerated `type Int = Float`
would keep type-checking as `Int` — a wrong answer where an error belongs.

## Oracle grounding

`tests-kik/harness/app/aliases/Aliases.kira` is the only corpus site, and it
pins the four forms ported here: alias to a scalar, to an array, to an array of
an alias, and to another alias. `packages/kira_semantics/src/lower_shared.zig`
supplies the resolver shape (`TypeAliasHeader`, the
`unresolved`/`resolving`/`resolved` state machine) and `KSEM157`.

The corpus writes `type Byte = U8`. `U8` does not exist in this port yet —
scalar widening is unported — so the example and tests alias `Int` instead. The
alias mechanism is what is being tested; nothing about it depends on which
scalar sits at the end of the chain.

## Left unported

Imported aliases. The oracle resolves an alias declared in another file through
`imported_globals.findAlias`, which needs imports; imports are unported here, so
an alias is file-scoped by the only scope that exists.
