# Survey: nominal identity and mangling (2026-09-01)

- Ids: `StructId`/`EnumId` per-program table indices; `StructTable`/`EnumTable` carry `owners:
  Vec<Option<String>>` (package name string) and key `"{owner}::{name}"` (ty/structs.rs:153-158,
  ty/enums.rs:110-115; declare_owned structs.rs:221-232, enums.rs:149-159). `DistinctTable`
  (ty/distincts.rs:56-59) has no owner/index. `NativeStateId` interned by Type. Traits have no Type.
  `TypeTable::type_name` (ty/table.rs:393-440) returns bare names → diagnostics, overload symbols,
  method keys.
- Packages: manifest name only (kira-manifest package_manifest.rs:8-16); module identity
  `"Pkg::Module"` in `ModuleSource::module` (kira-semantics lib.rs:123-135); `ImportTable`
  (imports.rs:129-134), `package_of(SourceId)` (169-175) is the owner source; declaration sites
  decl.rs:150-153, enums.rs:66-69, classes/mod.rs:157-160, constructs/collection.rs:34-37,185-186,
  traits/existential.rs:91-92, generics/aggregates.rs:104-112,221. No version/path in identity.
- LLVM symbols: `symbol_name(index, name)` = `kira_fn_{index}_{sanitized}` (codegen/symbols.rs:24-36);
  overload symbol `name$Type…` bare (analyze/signatures.rs:226-243); method names
  `Type.method` + `$idx$Class` (analyze/callable.rs:213-243); drop glue ordinary FuncId
  (ty/structs.rs:70); exports snake-case (exports.rs:54-75).
- Generic keys are strings: `mangle` (generics.rs:588-600) `"{template}<{args}>"`, args qualified
  via `identity_spelling` (606-625) but template not; generic enums declared owner-less
  (generics.rs:485,549) vs aggregates owner-keyed (generics/aggregates.rs:112,221);
  `Instantiation { template: String, arguments }` (ty/enums.rs:74-80); `admits` compares bare
  template names (ty/widening.rs:103); memos `generic_function_instances`, `traits`,
  `construct_families`, `trait_existentials`, `sig_index`, `foreign_index`, `aliases`, `distincts`,
  `pointer_targets`, `constant_index` all bare-name keyed (analyze/mod.rs).
- Runtime identity: `ErasedTypeId` = family<<32 | row (ty/erased.rs:66-108), no names/packages;
  native-state fingerprint mixes bare names (ty/table.rs:299,311,346; `native_state_type_id`
  216-262). Foundation DeriveSerde uses `target.name` for wire text and generated fn names
  (DeriveSerde.kira:178-492). Macro `Declaration` has name+path, no package (kira-macros decl.rs:
  148-199); `Registry` bare-name maps, last-wins absorb (registry.rs:144-174).
- Tests: kira-semantics tests/imports.rs:432-645 (two packages same struct name etc.),
  end_to_end/packages.rs:233 (Alpha/Beta distinct names). No cross-package same-name coverage via
  generics/Any/NativeState/Serde/@Export.
