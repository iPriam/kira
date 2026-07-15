//! Live/hot-reload platform: runners, live sessions, and module swap.
//!
//! Layer 8 of the Kira package graph.
//! Ported from kira-zig `packages/kira_live`; this module tree mirrors that
//! file split so the port can land file by file.

// #![warn(missing_docs)] // enable once the port lands real code

pub mod android_live;
pub mod apple_app_sources;
pub mod apple_live;
pub mod apple_pbxproj;
pub mod apple_runner;
pub mod apple_session;
pub mod apple_workspace;
pub mod bundle_builder;
pub mod desktop_main;
pub mod ios_live;
pub mod live_args;
pub mod manifest_loader;
pub mod model;
pub mod platform;
pub mod protocol;
pub mod reload_listener;
pub mod runner_client;
pub mod runner_kind;
pub mod runner_support;
pub mod source_watcher;
pub mod static_file_server;
pub mod supervisor;
pub mod supervisor_reload;
pub mod supervisor_shared;
pub mod target;
pub mod watch_inputs;
pub mod web_bundle;
pub mod web_live;

pub use model::{BundleGraph, BundleManifest, BundleSpec, RunnerManifest, RuntimeMode};
pub use platform::{LivePlatform, RunnerId, parse_runner_id, runner_kind};
pub use protocol::{Frame, LiveMessageKind, ReplaceBundlePayload};
pub use runner_kind::RunnerKind;
