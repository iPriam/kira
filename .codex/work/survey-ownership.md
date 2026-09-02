# Survey: ownership HIR, Copyable, Drop, release (2026-09-01)

- Syntax: `OwnershipMode {Owned,BorrowRead,BorrowMut,Move,Copy}` (kira-syntax-model ownership.rs:22),
  `OwnershipOp {Move,Copy}` (85), `Expr::Ownership` (ast/expr.rs:230).
- Typeck erases: `analyze_copy_expr` (kira-semantics ownership.rs:324) is a no-op;
  `analyze_move_expr` (339) returns `read_local` + `mark_moved`; HIR has no Copy/Move/Borrow node
  (hir/exprs.rs: only `Local` :99); `HirLocal.ownership` (hir.rs:290) only survives for params;
  policy doc ownership.rs:20-39. LLVM re-derives from type+position (lower/expr/storage.rs:16,53),
  live flags (lower/mod.rs:173).
- Drop: traits/drop.rs; `StructDef.drop_glue` (ty/structs.rs:70); `user_drop`/`runs_user_drop`
  (ty/table.rs:499,514); `moves_on_bind` (591). Copyability: copyable.rs `not_copyable_reason` (92),
  `is_leaf_copyable` (250) treats Cell copyable; `is_trivially_copyable` (ty/mod.rs:374) true for
  NativeState/Task/MainThreadTask/Cell; nothing checks `copy` expressions/params against
  copyability (ownership.rs:529-552 uses is_trivially_copyable). Test pinning permissive copy:
  tests/mod.rs:629 `copying_a_non_trivial_value_is_allowed`.
- Analysis: `LocalOwnership` whole-local (ownership.rs:64); `BranchMoves` may-have-moved union
  (123-182) used by if (stmt.rs:311) and match (stmt/matches.rs:133,151); divergence = Return only
  (stmt.rs:77); loops `LoopMoves` (198, 272 KSEM270); attempts.rs has no move handling; no partial
  moves/init (place.rs:160-176 whole-local only).
- Release: `IrStmt::ReleaseLocals` (kira-ir ir.rs:496-513, none on Return); `scope_releases`
  (mid/scope.rs:40..), `ReleasePlan` slot order (mid.rs:71,181); VM function.rs:77; LLVM
  lower/stmt.rs:64 → release_local_if_live :197 with runtime i1 flags.
- Hidden locals: `declare_hidden_as` (analyze/scope.rs:443) pushes `LocalOwnership::owned()`
  regardless of mode (bug); borrow-declaring sites in traits/existential.rs:493,495,
  constructs/dispatch*.rs, closures/function_values.rs:357.
- Tests: tests-kik Ownership/Owx/Owy/StxOwnership/Drx/Emx/Rlx/Ltx; `copy` operator absent from
  harness (only examples/ownership/ownership.kira:32,73).
