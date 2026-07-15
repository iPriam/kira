//! Semantic analyzer: type checking, ownership, and import resolution over the AST.
//!
//! Layer 2 of the Kira package graph.
//! Ported from kira-zig `packages/kira_semantics` (~19k LOC). This crate is a
//! module-tree scaffold: one Rust module per major Zig source file, each with
//! a doc header describing what it will own. Test-only Zig files
//! (`analyzer_*_tests.zig`, `analyzer_test_support.zig`) become Rust
//! `#[cfg(test)]` modules when the logic lands and are not mirrored here.

pub mod analyzer;
pub mod construct_form_surface;
pub mod function_types;
pub mod imported_globals;
pub mod lower_construct_content;
pub mod lower_construct_content_validation;
pub mod lower_construct_default_accessors;
pub mod lower_construct_extensions;
pub mod lower_construct_field_requirements;
pub mod lower_construct_functions;
pub mod lower_construct_members;
pub mod lower_construct_node_bridge;
pub mod lower_construct_requirements;
pub mod lower_construct_tests;
pub mod lower_exprs;
pub mod lower_exprs_assignment;
pub mod lower_exprs_async;
pub mod lower_exprs_builder;
pub mod lower_exprs_call_dispatch;
pub mod lower_exprs_call_resolution;
pub mod lower_exprs_callbacks;
pub mod lower_exprs_calls;
pub mod lower_exprs_comptime;
pub mod lower_exprs_construct_any;
pub mod lower_exprs_core;
pub mod lower_exprs_enum_variants;
pub mod lower_exprs_function_types;
pub mod lower_exprs_implicit_members;
pub mod lower_exprs_members;
pub mod lower_exprs_names;
pub mod lower_exprs_native_state;
pub mod lower_exprs_ownership;
pub mod lower_exprs_receivers;
pub mod lower_exprs_scope_flow;
pub mod lower_exprs_type_resolvers;
pub mod lower_exprs_types;
pub mod lower_program;
pub mod lower_program_construct_decl;
pub mod lower_program_construct_validation;
pub mod lower_program_enums;
pub mod lower_program_ffi_boundary;
pub mod lower_program_field_defaults;
pub mod lower_program_forms;
pub mod lower_program_functions;
pub mod lower_program_imports;
pub mod lower_program_symbol_indexes;
pub mod lower_program_type_headers;
pub mod lower_program_type_members;
pub mod lower_program_types;
pub mod lower_shared;
pub mod lower_shared_annotations;
pub mod lower_shared_captures;
pub mod lower_shared_construct_queries;
pub mod lower_shared_decls;
pub mod lower_shared_ffi_annotations;
pub mod lower_shared_symbols;
pub mod lower_shared_type_relations;
pub mod lower_shared_type_text;
pub mod lower_stmts_attempt;
pub mod lower_stmts_match;
pub mod lower_to_hir;
pub mod lower_type_constant_accessors;
pub mod lower_widget_content;
