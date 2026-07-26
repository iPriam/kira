//! Portable file-system requests, responses, and host errors.
//!
//! The VM core owns no filesystem: it describes what a program asked for and
//! hands the request to the embedder through
//! [`HostCapabilities::file_system`](crate::HostCapabilities::file_system),
//! exactly as it hands finished lines to `write_line`. That is what keeps
//! `kira-vm-runtime` buildable for `wasm32-unknown-unknown`, where there is no
//! filesystem to reach.
//!
//! # Failure is a value, not a trap
//!
//! Every operation here answers a question about the outside world, and the
//! outside world is allowed to say no: a missing file, a directory that is
//! really a file, a path with no write permission. Those are ordinary answers —
//! an empty byte array, `false`, a zero size — because a Kira program that asks
//! whether a file exists must be able to hear "no" without dying.
//!
//! [`FileSystemError`] is reserved for the one condition that is *not* an
//! answer: the host provides no filesystem at all.

pub mod host;

use thiserror::Error;

pub use host::{FileSystemHost, perform};

/// Which file-system operation one request performs.
///
/// The discriminants are a wire contract: they travel in the operand byte of
/// the `FileSystem` bytecode instruction, so they are **append-only** — a new
/// operation takes the next free number and no existing one ever moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FileSystemOp {
    /// Read at most `count` bytes of a file starting at `offset`.
    ReadRange = 0,
    /// Write a byte array over a file's whole contents.
    WriteBytes = 1,
    /// Read a whole file as text.
    ReadText = 2,
    /// Write text over a file's whole contents.
    WriteText = 3,
    /// List the entry names directly inside a directory.
    ListDirectory = 4,
    /// Whether a path exists and is a directory.
    IsDirectory = 5,
    /// Create one directory, without creating its parents.
    MakeDirectory = 6,
    /// Rename a path.
    RenamePath = 7,
    /// Remove a file or an empty directory.
    RemovePath = 8,
    /// Whether a path exists and is a regular file.
    FileExists = 9,
    /// Whether a path exists as any kind of entry.
    PathExists = 10,
    /// A file's size in bytes.
    FileSize = 11,
}

impl FileSystemOp {
    /// Every operation, in wire order.
    ///
    /// The one place the set is written down: decoding indexes this rather than
    /// repeating a match, so a new operation cannot be added to the enum and
    /// forgotten by the decoder.
    pub const ALL: [FileSystemOp; 12] = [
        FileSystemOp::ReadRange,
        FileSystemOp::WriteBytes,
        FileSystemOp::ReadText,
        FileSystemOp::WriteText,
        FileSystemOp::ListDirectory,
        FileSystemOp::IsDirectory,
        FileSystemOp::MakeDirectory,
        FileSystemOp::RenamePath,
        FileSystemOp::RemovePath,
        FileSystemOp::FileExists,
        FileSystemOp::PathExists,
        FileSystemOp::FileSize,
    ];

    /// The wire byte this operation travels as.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Reads a wire byte, or `None` when it names no operation.
    ///
    /// A decoder never guesses: an unknown byte is rejected by its caller
    /// rather than folded into a neighbouring operation.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.get(usize::from(byte)).copied()
    }

    /// How many operands this operation pops, in source order.
    pub const fn arity(self) -> usize {
        match self {
            FileSystemOp::ReadRange => 3,
            FileSystemOp::WriteBytes | FileSystemOp::WriteText | FileSystemOp::RenamePath => 2,
            FileSystemOp::ReadText
            | FileSystemOp::ListDirectory
            | FileSystemOp::IsDirectory
            | FileSystemOp::MakeDirectory
            | FileSystemOp::RemovePath
            | FileSystemOp::FileExists
            | FileSystemOp::PathExists
            | FileSystemOp::FileSize => 1,
        }
    }

    /// The Kira intrinsic name that compiles to this operation.
    pub const fn intrinsic_name(self) -> &'static str {
        match self {
            FileSystemOp::ReadRange => "fsReadRange",
            FileSystemOp::WriteBytes => "fsWriteBytes",
            FileSystemOp::ReadText => "fsReadText",
            FileSystemOp::WriteText => "fsWriteText",
            FileSystemOp::ListDirectory => "fsListDirectory",
            FileSystemOp::IsDirectory => "fsIsDirectory",
            FileSystemOp::MakeDirectory => "fsMakeDirectory",
            FileSystemOp::RenamePath => "fsRenamePath",
            FileSystemOp::RemovePath => "fsRemovePath",
            FileSystemOp::FileExists => "fsFileExists",
            FileSystemOp::PathExists => "fsPathExists",
            FileSystemOp::FileSize => "fsFileSize",
        }
    }

    /// The `kira_rt_*` symbol native code calls to perform this operation.
    ///
    /// Derived from the operation rather than written twice, so the backend's
    /// declaration and the runtime's definition cannot drift apart.
    pub const fn runtime_symbol(self) -> &'static str {
        match self {
            FileSystemOp::ReadRange => "kira_rt_fs_read_range",
            FileSystemOp::WriteBytes => "kira_rt_fs_write_bytes",
            FileSystemOp::ReadText => "kira_rt_fs_read_text",
            FileSystemOp::WriteText => "kira_rt_fs_write_text",
            FileSystemOp::ListDirectory => "kira_rt_fs_list_directory",
            FileSystemOp::IsDirectory => "kira_rt_fs_is_directory",
            FileSystemOp::MakeDirectory => "kira_rt_fs_make_directory",
            FileSystemOp::RenamePath => "kira_rt_fs_rename_path",
            FileSystemOp::RemovePath => "kira_rt_fs_remove_path",
            FileSystemOp::FileExists => "kira_rt_fs_file_exists",
            FileSystemOp::PathExists => "kira_rt_fs_path_exists",
            FileSystemOp::FileSize => "kira_rt_fs_file_size",
        }
    }

    /// Resolves a Kira intrinsic name to its operation, or `None`.
    pub fn from_intrinsic_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.intrinsic_name() == name)
    }
}

/// One file-system operation, with its arguments already evaluated.
///
/// Borrowing is deliberate: a request is seam vocabulary that lives only for
/// the duration of the host call, so nothing here is copied out of the engine's
/// own storage on the way in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRequest<'a> {
    /// Read at most `count` bytes from `offset`; a short read at end of file is
    /// an ordinary answer.
    ReadRange {
        /// The file to read.
        path: &'a str,
        /// Where in the file to start.
        offset: i64,
        /// How many bytes to ask for.
        count: i64,
    },
    /// Replace a file's contents with `bytes`.
    WriteBytes {
        /// The file to write.
        path: &'a str,
        /// The bytes to write, exactly.
        bytes: &'a [u8],
    },
    /// Read a whole file as text.
    ReadText {
        /// The file to read.
        path: &'a str,
    },
    /// Replace a file's contents with `text`.
    WriteText {
        /// The file to write.
        path: &'a str,
        /// The text to write.
        text: &'a str,
    },
    /// List the entry names directly inside a directory.
    ListDirectory {
        /// The directory to list.
        path: &'a str,
    },
    /// Whether `path` exists and is a directory.
    IsDirectory {
        /// The path to test.
        path: &'a str,
    },
    /// Create one directory, without creating its parents.
    MakeDirectory {
        /// The directory to create.
        path: &'a str,
    },
    /// Rename `from` to `to`.
    RenamePath {
        /// The path as it is now.
        from: &'a str,
        /// The path it should have.
        to: &'a str,
    },
    /// Remove a file or an empty directory.
    RemovePath {
        /// The path to remove.
        path: &'a str,
    },
    /// Whether `path` exists and is a regular file.
    FileExists {
        /// The path to test.
        path: &'a str,
    },
    /// Whether `path` exists as any kind of entry.
    PathExists {
        /// The path to test.
        path: &'a str,
    },
    /// A file's size in bytes.
    FileSize {
        /// The file to measure.
        path: &'a str,
    },
}

impl FileRequest<'_> {
    /// The operation this request performs.
    pub const fn op(&self) -> FileSystemOp {
        match self {
            FileRequest::ReadRange { .. } => FileSystemOp::ReadRange,
            FileRequest::WriteBytes { .. } => FileSystemOp::WriteBytes,
            FileRequest::ReadText { .. } => FileSystemOp::ReadText,
            FileRequest::WriteText { .. } => FileSystemOp::WriteText,
            FileRequest::ListDirectory { .. } => FileSystemOp::ListDirectory,
            FileRequest::IsDirectory { .. } => FileSystemOp::IsDirectory,
            FileRequest::MakeDirectory { .. } => FileSystemOp::MakeDirectory,
            FileRequest::RenamePath { .. } => FileSystemOp::RenamePath,
            FileRequest::RemovePath { .. } => FileSystemOp::RemovePath,
            FileRequest::FileExists { .. } => FileSystemOp::FileExists,
            FileRequest::PathExists { .. } => FileSystemOp::PathExists,
            FileRequest::FileSize { .. } => FileSystemOp::FileSize,
        }
    }
}

/// What a file-system operation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileResponse {
    /// The bytes a read produced.
    Bytes(Vec<u8>),
    /// The text a read produced.
    Text(String),
    /// The entry names a listing produced, already ordered.
    Names(Vec<String>),
    /// A yes-or-no answer, and whether a write or a mutation succeeded.
    Flag(bool),
    /// A size in bytes.
    Size(u64),
}

/// A file-system operation the host could not even attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FileSystemError {
    /// The host provides no filesystem.
    #[error("this host does not provide a filesystem")]
    NoFileSystemHost,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_system_op_wire_bytes_are_pinned() {
        assert_eq!(FileSystemOp::ReadRange.as_byte(), 0);
        assert_eq!(FileSystemOp::WriteBytes.as_byte(), 1);
        assert_eq!(FileSystemOp::ReadText.as_byte(), 2);
        assert_eq!(FileSystemOp::WriteText.as_byte(), 3);
        assert_eq!(FileSystemOp::ListDirectory.as_byte(), 4);
        assert_eq!(FileSystemOp::IsDirectory.as_byte(), 5);
        assert_eq!(FileSystemOp::MakeDirectory.as_byte(), 6);
        assert_eq!(FileSystemOp::RenamePath.as_byte(), 7);
        assert_eq!(FileSystemOp::RemovePath.as_byte(), 8);
        assert_eq!(FileSystemOp::FileExists.as_byte(), 9);
        assert_eq!(FileSystemOp::PathExists.as_byte(), 10);
        assert_eq!(FileSystemOp::FileSize.as_byte(), 11);
    }

    #[test]
    fn every_op_round_trips_through_its_byte_and_its_name() {
        for op in FileSystemOp::ALL {
            assert_eq!(FileSystemOp::from_byte(op.as_byte()), Some(op));
            assert_eq!(
                FileSystemOp::from_intrinsic_name(op.intrinsic_name()),
                Some(op)
            );
        }
    }

    #[test]
    fn an_unknown_byte_names_no_operation() {
        assert_eq!(FileSystemOp::from_byte(12), None);
        assert_eq!(FileSystemOp::from_byte(255), None);
        assert_eq!(FileSystemOp::from_intrinsic_name("fsNotAnOperation"), None);
    }

    #[test]
    fn a_request_reports_the_operation_it_performs() {
        let request = FileRequest::ReadRange {
            path: "a.bin",
            offset: 2,
            count: 4,
        };
        assert_eq!(request.op(), FileSystemOp::ReadRange);
        assert_eq!(request.op().arity(), 3);
    }
}
