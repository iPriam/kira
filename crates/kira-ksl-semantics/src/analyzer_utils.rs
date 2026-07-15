//! Pure helper functions shared by the analyzer: qualified-name keys, builtin
//! lookup by name, annotation-name extraction, stage-kind conversion, group
//! class ranking, structural type equality/naming, const-value formatting,
//! std140/std430-style layout computation (`reflectedLayout`, `alignForward`),
//! and resource visibility derivation.
//!
//! Ported from kira-zig `packages/kira_ksl_semantics/src/analyzer_utils.zig`.
//! Logic lands with the migration; this module is a placement scaffold.
