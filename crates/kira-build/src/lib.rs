//! The build system: orchestrates frontend, backends, packaging, and managed LLVM fetch.
//!
//! Layer 7 of the Kira package graph.
//! Ported from kira-zig `packages/kira_build` (~14k LOC there); this module
//! tree mirrors that file split so the port can land file by file. The
//! `ffi_autobind_*` family is the KG/kira-graphics critical path: graphics
//! depends on FFI restoration.

// #![warn(missing_docs)] // enable once the port lands real code

pub mod archive_extract;
pub mod build_system;
pub mod cache;
pub mod cache_files;
pub mod fetch_libffi;
pub mod fetch_libffi_main;
pub mod fetch_llvm;
pub mod fetch_llvm_main;
pub mod ffi_autobind;
pub mod ffi_autobind_cache;
pub mod ffi_autobind_clang;
pub mod ffi_autobind_dynamic_runtime;
pub mod ffi_autobind_fs;
pub mod ffi_autobind_json;
pub mod ffi_autobind_kira_types;
pub mod ffi_autobind_macros;
pub mod ffi_autobind_names;
pub mod ffi_autobind_profiles;
pub mod ffi_autobind_sdk;
pub mod ffi_autobind_sdk_clang_ast;
pub mod ffi_autobind_sdk_model;
pub mod ffi_autobind_tests;
pub mod ffi_autobind_type_text;
pub mod ffi_support;
pub mod github_release_fetch;
pub mod libffi_metadata;
pub mod llvm_metadata;
pub mod llvm_tooldir;
pub mod macro_eval;
pub mod macro_expand;
pub mod macro_instantiate;
pub mod macro_procedural;
pub mod native_artifact_build;
pub mod native_artifact_compile;
pub mod native_build_paths;
pub mod native_lib_resolver;
pub mod pipeline;
pub mod pipeline_frontend;
pub mod pipeline_tests;
pub mod pipeline_timing;
pub mod shader;
pub mod syntax_rewrite;
pub mod synth_test_driver;
pub mod synth_test_driver_eq;
pub mod synth_test_driver_tests;
pub mod wasm_emscripten_closure_width_tests;
pub mod wasm_emscripten_test_support;
pub mod wasm_emscripten_tests;
pub mod wasm_emscripten_width_tests;
