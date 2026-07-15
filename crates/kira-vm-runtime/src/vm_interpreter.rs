//! The VM dispatch loop, operating on `vm_prepare`'s prepared modules.
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/vm_interpreter.zig`,
//! where dispatch is a labeled-switch ("threaded") loop: every opcode arm
//! jumps directly to the next instruction's arm via `continue :dispatch`,
//! giving each opcode its own indirect branch (much better prediction) and
//! removing the per-step pc bounds check — the decode pass appends an
//! implicit `ret`, so execution cannot fall off the end of the code.
//!
//! # Port plan (dispatch strategy)
//!
//! 1. **Now:** a plain `loop { match opcode { .. } }`. Rust has no direct
//!    equivalent of Zig's labeled-switch threading; a match-in-loop is the
//!    correct, safe starting point and LLVM already jump-tables it.
//! 2. **When stable:** migrate the hot arms to explicit tail calls with the
//!    `become` keyword (RFC 3407 / feature `explicit_tail_calls`) to recover
//!    per-opcode indirect branches — the same predictor win the Zig
//!    labeled-switch delivers. Keep the match-in-loop as the fallback for
//!    toolchains without the feature.
//! 3. **Shape:** unsafe core + safe facade. The inner loop gets unchecked
//!    register-file/constant-pool/code indexing (bounds proven by the decode
//!    pass, as in Zig) behind small `unsafe` blocks with SAFETY comments; the
//!    public `run`/`call` surface stays entirely safe and owns all
//!    invariant-establishing validation.
//!
//! All ownership semantics (owned-slot tracking, transfers, borrows, drops)
//! port byte-for-byte from the Zig interpreter; only the dispatch machinery
//! is idiom-translated.
