//! Parsing for the `FailTest` construct — expected-compile-outcome tests.
//!
//! `FailTest Name { backends {..} source {..} expect {..} }` uses a dedicated
//! path (not the generic construct-form parser) because its `source` section
//! is QUOTED: raw text is captured verbatim and never handed to the enclosing
//! package's parser/semantics.
//!
//! Mirrors kira-zig `packages/kira_parser/src/parser_failtest.zig`.
//! TODO(port): FailTest parsing lands during migration.
