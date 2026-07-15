//! C ABI facade over the Kira runtime and developer backend, built as staticlib/cdylib for embedders.
//!
//! Layer 10 of the Kira package graph.
//! Ported from kira-zig `packages/kira_main`; this module tree mirrors that
//! file split so the port can land file by file.

// #![warn(missing_docs)] // enable once the port lands real code

pub mod api;
pub mod developer;
pub mod developer_failtest;
pub mod developer_leak;
pub mod developer_parity;
pub mod developer_progress_report;
pub mod developer_test_decode;
pub mod developer_test_runtime;
pub mod developer_tests_config;
pub mod facade;
pub mod runtime_wrappers;
