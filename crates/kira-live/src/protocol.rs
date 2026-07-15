//! The live-reload wire protocol: message kinds and frame model.
//!
//! Ported from kira-zig `kira_live/src/protocol.zig`. Frame read/write lands
//! with the port; these are the protocol types.

/// Message kinds exchanged between the supervisor and a runner. Wire values
/// are stable u32 discriminants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum LiveMessageKind {
    Hello = 1,
    RuntimeInfo = 2,
    BundleGraph = 3,
    ReplaceBundle = 4,
    ReplaceModule = 5,
    RestartRequired = 6,
    NativeRebuildStarted = 7,
    NativeRebuildFinished = 8,
    Diagnostics = 9,
    LogLine = 10,
    Heartbeat = 11,
    Shutdown = 12,
    ShutdownAck = 13,
    ReloadFailed = 14,
}

/// A single length-prefixed protocol frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: LiveMessageKind,
    pub payload: Vec<u8>,
}

/// Payload of a `ReplaceBundle` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceBundlePayload {
    pub bundle_id: String,
    pub files: Vec<FilePayload>,
}

/// One file transferred in a bundle replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePayload {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}
