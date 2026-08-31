//! The host-side filesystem: one implementation both engines share.
//!
//! Nothing here is reachable from the portable VM core — the capability's
//! default refuses, and a program only gets this by an embedder wrapping its
//! host in [`FileSystemHost`] or by running as native code, where
//! `kira_rt_fs_*` calls [`perform`] directly.

use std::fs;
use std::io::{Read, Seek, SeekFrom};

use super::{FileRequest, FileResponse, FileSystemError};
use crate::{
    ForeignArg, ForeignCallError, ForeignResult, HostCapabilities, LinuxSyscall, MainThreadError,
    MainThreadHandle, MainThreadRequest, MainThreadResponse, NativeArg, NativeCallError,
    NativeReturn, NativeStateError, NativeStateToken, NativeStateTypeId, NativeStateValue,
    SyscallError,
};

/// Runs one request against the process's real filesystem.
///
/// One function, called from both engines — the VM host and the native
/// `kira_rt_fs_*` runtime — because byte-identical output across backends means
/// the two must not merely be written to the same rules, they must *be* the same
/// rules. Every answer it can give is an ordinary value; see the module docs for
/// why nothing here fails.
pub fn perform(request: FileRequest<'_>) -> FileResponse {
    match request {
        FileRequest::ReadRange {
            path,
            offset,
            count,
        } => FileResponse::Bytes(read_range(path, offset, count)),
        FileRequest::WriteBytes { path, bytes } => {
            FileResponse::Flag(fs::write(path, bytes).is_ok())
        }
        FileRequest::ReadText { path } => FileResponse::Text(read_text(path)),
        FileRequest::WriteText { path, text } => {
            FileResponse::Flag(fs::write(path, text.as_bytes()).is_ok())
        }
        FileRequest::ListDirectory { path } => FileResponse::Names(list_directory(path)),
        FileRequest::IsDirectory { path } => {
            FileResponse::Flag(fs::metadata(path).is_ok_and(|meta| meta.is_dir()))
        }
        FileRequest::MakeDirectory { path } => FileResponse::Flag(make_directory(path)),
        FileRequest::RenamePath { from, to } => FileResponse::Flag(fs::rename(from, to).is_ok()),
        FileRequest::RemovePath { path } => FileResponse::Flag(remove_path(path)),
        FileRequest::FileExists { path } => {
            FileResponse::Flag(fs::metadata(path).is_ok_and(|meta| meta.is_file()))
        }
        FileRequest::PathExists { path } => {
            // `symlink_metadata` rather than `metadata`, so an entry that exists
            // but whose target does not — a broken symlink — still counts as
            // present, which is what "exists as any entry" means.
            FileResponse::Flag(fs::symlink_metadata(path).is_ok())
        }
        FileRequest::FileSize { path } => {
            FileResponse::Size(fs::metadata(path).map(|meta| meta.len()).unwrap_or(0))
        }
    }
}

/// Reads at most `count` bytes from `offset`, answering with what it got.
///
/// A true partial read: the file is opened and seeked, never read whole. A
/// non-positive count, a negative offset, an offset past the end, a missing
/// file, and a directory all produce no bytes.
fn read_range(path: &str, offset: i64, count: i64) -> Vec<u8> {
    let (Ok(offset), Ok(count)) = (u64::try_from(offset), usize::try_from(count)) else {
        return Vec::new();
    };
    if count == 0 {
        return Vec::new();
    }
    let Ok(mut file) = fs::File::open(path) else {
        return Vec::new();
    };
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.take(count as u64).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    bytes
}

/// Reads a whole file as text, stopping at the first NUL byte.
///
/// The NUL rule is not an accident of this implementation: text crosses the
/// reference implementation's seam as a C string, so a program that writes a NUL
/// and reads the file back as text sees the bytes before it. `size` on
/// `FileContents` still counts the whole file, which is how a caller tells the
/// two apart.
fn read_text(path: &str) -> String {
    let Ok(bytes) = fs::read(path) else {
        return String::new();
    };
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// The names directly inside a directory, in the platform's own order.
///
/// Deliberately unsorted: the reference walks `readdir` and hands back what it
/// finds, so sorting here would be a *difference*, not an improvement. Neither
/// implementation promises an order across hosts.
fn list_directory(path: &str) -> Vec<String> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect()
}

/// Creates one directory and answers whether one is there afterwards.
///
/// Phrasing the answer as "does it exist now" is what makes the call idempotent
/// on a directory that already exists while still refusing a path that is
/// already a file, without either case being special-cased.
fn make_directory(path: &str) -> bool {
    let _ = fs::create_dir(path);
    fs::metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Removes a file, or a directory and everything under it.
///
/// The kind is read with `symlink_metadata`, so a symlink pointing at a
/// directory is unlinked rather than followed and emptied.
fn remove_path(path: &str) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_dir() {
        fs::remove_dir_all(path).is_ok()
    } else {
        fs::remove_file(path).is_ok()
    }
}

/// A host wrapper that grants access to the process's real filesystem.
///
/// Mirrors [`NativeStateHost`](crate::NativeStateHost): an embedder opts in by
/// wrapping its own host, and everything else it provides passes straight
/// through. A host that is never wrapped keeps the refusing default, which is
/// what lets the portable VM core stay portable.
#[derive(Debug)]
pub struct FileSystemHost<H> {
    inner: H,
}

impl<H> FileSystemHost<H> {
    /// Wraps `inner`, adding real filesystem access.
    pub fn new(inner: H) -> Self {
        Self { inner }
    }

    /// Borrows the wrapped host.
    pub fn inner(&self) -> &H {
        &self.inner
    }

    /// Mutably borrows the wrapped host.
    pub fn inner_mut(&mut self) -> &mut H {
        &mut self.inner
    }

    /// Returns the wrapped host.
    pub fn into_inner(self) -> H {
        self.inner
    }
}

impl<H: HostCapabilities> HostCapabilities for FileSystemHost<H> {
    fn write_line(&mut self, text: &str) {
        self.inner.write_line(text);
    }

    fn call_native(
        &mut self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeReturn, NativeCallError> {
        self.inner.call_native(function_id, args)
    }

    fn main_thread(
        &mut self,
        request: MainThreadRequest,
    ) -> Result<MainThreadResponse, MainThreadError> {
        self.inner.main_thread(request)
    }

    fn main_thread_join(
        &mut self,
        handle: MainThreadHandle,
    ) -> Result<NativeStateValue, MainThreadError> {
        self.inner.main_thread_join(handle)
    }

    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        self.inner.call_foreign(foreign_id, args)
    }

    fn syscall(&mut self, call: LinuxSyscall, args: &[i64]) -> Result<i64, SyscallError> {
        self.inner.syscall(call, args)
    }

    fn foreign_callback(&mut self, callback_id: u32) -> Result<u64, ForeignCallError> {
        self.inner.foreign_callback(callback_id)
    }

    fn native_state_create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        self.inner.native_state_create(ty, value)
    }

    fn native_state_recover(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        self.inner.native_state_recover(token, ty)
    }

    fn native_state_replace(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        self.inner.native_state_replace(token, ty, value)
    }

    fn native_state_free(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        self.inner.native_state_free(token)
    }

    fn file_system(&mut self, request: FileRequest<'_>) -> Result<FileResponse, FileSystemError> {
        Ok(perform(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh empty directory to work in, named for the test that owns it.
    fn scratch(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!("kira-fs-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch directory");
        dir.to_string_lossy().into_owned()
    }

    fn flag(response: FileResponse) -> bool {
        match response {
            FileResponse::Flag(value) => value,
            other => panic!("expected a flag, got {other:?}"),
        }
    }

    fn bytes(response: FileResponse) -> Vec<u8> {
        match response {
            FileResponse::Bytes(value) => value,
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    #[test]
    fn a_range_read_is_short_at_the_end_and_empty_off_the_end() {
        let dir = scratch("range");
        let path = format!("{dir}/four.bin");
        assert!(flag(perform(FileRequest::WriteBytes {
            path: &path,
            bytes: &[104, 105, 0, 122],
        })));

        assert_eq!(
            bytes(perform(FileRequest::ReadRange {
                path: &path,
                offset: 0,
                count: 10,
            })),
            vec![104, 105, 0, 122]
        );

        for (offset, count, expected) in [
            (2, 2, vec![0u8, 122]),
            (99, 4, Vec::new()),
            (0, 0, Vec::new()),
            (-1, 4, Vec::new()),
            (0, -1, Vec::new()),
        ] {
            assert_eq!(
                bytes(perform(FileRequest::ReadRange {
                    path: &path,
                    offset,
                    count,
                })),
                expected,
                "offset {offset}, count {count}"
            );
        }

        assert_eq!(
            bytes(perform(FileRequest::ReadRange {
                path: &format!("{dir}/missing.bin"),
                offset: 0,
                count: 4,
            })),
            Vec::<u8>::new()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn text_stops_at_a_nul_while_size_counts_the_whole_file() {
        let dir = scratch("nul");
        let path = format!("{dir}/nul.bin");
        assert!(flag(perform(FileRequest::WriteBytes {
            path: &path,
            bytes: &[104, 0, 105],
        })));
        assert_eq!(
            perform(FileRequest::ReadText { path: &path }),
            FileResponse::Text("h".to_owned())
        );
        assert_eq!(
            perform(FileRequest::FileSize { path: &path }),
            FileResponse::Size(3)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_path_answers_rather_than_failing() {
        let dir = scratch("missing");
        let path = format!("{dir}/nope");
        assert_eq!(
            perform(FileRequest::ReadText { path: &path }),
            FileResponse::Text(String::new())
        );
        assert_eq!(
            perform(FileRequest::FileSize { path: &path }),
            FileResponse::Size(0)
        );
        assert!(!flag(perform(FileRequest::FileExists { path: &path })));
        assert!(!flag(perform(FileRequest::PathExists { path: &path })));
        assert!(!flag(perform(FileRequest::IsDirectory { path: &path })));
        assert!(!flag(perform(FileRequest::RemovePath { path: &path })));
        assert_eq!(
            perform(FileRequest::ListDirectory { path: &path }),
            FileResponse::Names(Vec::new())
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn making_a_directory_is_idempotent_but_never_makes_parents() {
        let dir = scratch("mkdir");
        let one = format!("{dir}/one");
        assert!(flag(perform(FileRequest::MakeDirectory { path: &one })));
        assert!(flag(perform(FileRequest::MakeDirectory { path: &one })));
        assert!(flag(perform(FileRequest::IsDirectory { path: &one })));
        assert!(!flag(perform(FileRequest::FileExists { path: &one })));
        assert!(flag(perform(FileRequest::PathExists { path: &one })));
        assert!(!flag(perform(FileRequest::MakeDirectory {
            path: &format!("{dir}/a/b"),
        })));

        let file = format!("{dir}/file.txt");
        assert!(flag(perform(FileRequest::WriteText {
            path: &file,
            text: "x",
        })));
        assert!(!flag(perform(FileRequest::MakeDirectory { path: &file })));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_a_directory_takes_everything_under_it() {
        let dir = scratch("rmdir");
        let tree = format!("{dir}/tree");
        assert!(flag(perform(FileRequest::MakeDirectory { path: &tree })));
        assert!(flag(perform(FileRequest::MakeDirectory {
            path: &format!("{tree}/inner"),
        })));
        assert!(flag(perform(FileRequest::WriteText {
            path: &format!("{tree}/inner/f.txt"),
            text: "x",
        })));
        assert!(flag(perform(FileRequest::RemovePath { path: &tree })));
        assert!(!flag(perform(FileRequest::PathExists { path: &tree })));
        assert!(!flag(perform(FileRequest::RemovePath { path: &tree })));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rename_overwrites_its_target_and_a_write_truncates() {
        let dir = scratch("rename");
        let left = format!("{dir}/left.txt");
        let right = format!("{dir}/right.txt");
        assert!(flag(perform(FileRequest::WriteText {
            path: &left,
            text: "1",
        })));
        assert!(flag(perform(FileRequest::WriteText {
            path: &right,
            text: "2",
        })));
        assert!(flag(perform(FileRequest::RenamePath {
            from: &left,
            to: &right,
        })));
        assert_eq!(
            perform(FileRequest::ReadText { path: &right }),
            FileResponse::Text("1".to_owned())
        );
        assert!(!flag(perform(FileRequest::RenamePath {
            from: &left,
            to: &right,
        })));

        assert!(flag(perform(FileRequest::WriteText {
            path: &right,
            text: "abcdef",
        })));
        assert!(flag(perform(FileRequest::WriteText {
            path: &right,
            text: "xy",
        })));
        assert_eq!(
            perform(FileRequest::FileSize { path: &right }),
            FileResponse::Size(2)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_listing_holds_every_entry_and_nothing_else() {
        let dir = scratch("list");
        for name in ["ccc", "aaa", "bbb"] {
            assert!(flag(perform(FileRequest::WriteText {
                path: &format!("{dir}/{name}"),
                text: "x",
            })));
        }
        assert!(flag(perform(FileRequest::MakeDirectory {
            path: &format!("{dir}/mmm"),
        })));
        let FileResponse::Names(mut names) = perform(FileRequest::ListDirectory { path: &dir })
        else {
            panic!("expected names");
        };
        // The order is the platform's, so the *set* is what this pins; the
        // ordering itself is proven against the reference by differential run.
        names.sort();
        assert_eq!(names, vec!["aaa", "bbb", "ccc", "mmm"]);

        assert_eq!(
            perform(FileRequest::ListDirectory {
                path: &format!("{dir}/aaa"),
            }),
            FileResponse::Names(Vec::new())
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_host_without_the_wrapper_has_no_filesystem() {
        let mut bare = crate::CapturingHost::new();
        assert_eq!(
            bare.file_system(FileRequest::PathExists { path: "." }),
            Err(FileSystemError::NoFileSystemHost)
        );
        let mut granted = FileSystemHost::new(crate::CapturingHost::new());
        assert_eq!(
            granted.file_system(FileRequest::PathExists { path: "." }),
            Ok(FileResponse::Flag(true))
        );
    }
}
