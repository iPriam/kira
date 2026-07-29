//! The native half of Kira's filesystem: `kira_rt_fs_*`.
//!
//! Each symbol here is the native mirror of one `FileSystem` VM instruction, and
//! both go through the same [`kira_runtime_abi::file_system::perform`] — the two
//! engines share an implementation rather than sharing a specification, which is
//! what makes byte-identical output something the code enforces instead of
//! something a test hopes for.
//!
//! # Ownership
//!
//! Affine, like the rest of this runtime: **every helper consumes the handles it
//! is given**. A string argument is freed before the call returns, and so is a
//! byte array. That mirrors the VM, which drops the operands it popped, and it
//! mirrors [`crate::runtime::kira_rt_print_str`], the other helper that takes a
//! value rather than borrowing one.
//!
//! Every symbol is `extern "C"` with a `kira_rt_` prefix and a fixed signature.
//! These names are a wire contract with the backend's lowering and are
//! append-only: never rename one or change a signature in place.

use kira_runtime_abi::{FileRequest, FileResponse};

use crate::array::{KArray, kira_rt_array_free, kira_rt_array_len, kira_rt_array_new};
use crate::runtime::KStr;
use crate::values::{handle_of, int_array, release, string_array, text_of};

/// The C truth value the backend reads back as a Kira `Bool`.
fn flag(response: FileResponse) -> u8 {
    match response {
        FileResponse::Flag(value) => u8::from(value),
        // Unreachable: `perform` answers each request with its own shape. Written
        // as a value rather than a panic, because a runtime never gets to end its
        // caller's process.
        _ => 0,
    }
}

/// Reads at most `count` bytes of a file from `offset` into a fresh `[U8]`.
///
/// # Safety
/// `path` must be null or a live string handle, which this frees; `esize` must
/// be the ABI size of a Kira integer element.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_read_range(
    path: KStr,
    offset: i64,
    count: i64,
    esize: i64,
) -> KArray {
    // SAFETY: forwarded contract.
    let text = unsafe { text_of(path) };
    // SAFETY: same.
    unsafe { release(path) };
    let FileResponse::Bytes(bytes) =
        kira_runtime_abi::file_system::perform(FileRequest::ReadRange {
            path: &text,
            offset,
            count,
        })
    else {
        return kira_rt_array_new(0, esize.max(0) as usize);
    };
    let values: Vec<i64> = bytes.into_iter().map(i64::from).collect();
    // SAFETY: forwarded `esize` contract.
    unsafe { int_array(&values, esize) }
}

/// Writes a `[U8]` over a file's whole contents.
///
/// # Safety
/// `path` must be null or a live string handle and `bytes` null or a live array
/// built with `esize`; both are freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_write_bytes(path: KStr, bytes: KArray, esize: i64) -> u8 {
    // SAFETY: forwarded contract.
    let text = unsafe { text_of(path) };
    // SAFETY: same.
    unsafe { release(path) };
    let stride = esize.max(0) as usize;
    // SAFETY: the caller vouches for the array and its element size.
    let len = unsafe { kira_rt_array_len(bytes) }.max(0) as usize;
    let mut buffer = Vec::with_capacity(len);
    if !bytes.is_null() {
        // SAFETY: a live array's item block holds `len` slots of `stride` bytes.
        let items = unsafe { (*bytes).items };
        for index in 0..len {
            if items.is_null() {
                break;
            }
            // SAFETY: `index < len`, and a Kira integer element is an `i64`.
            let value = unsafe { items.add(index * stride).cast::<i64>().read() };
            buffer.push(value as u8);
        }
    }
    // SAFETY: the elements are inert integers, so no element leaf is needed.
    unsafe { kira_rt_array_free(bytes, stride, None) };
    flag(kira_runtime_abi::file_system::perform(
        FileRequest::WriteBytes {
            path: &text,
            bytes: &buffer,
        },
    ))
}

/// Reads a whole file as text, stopping at the first NUL byte.
///
/// # Safety
/// `path` must be null or a live string handle, which this frees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_read_text(path: KStr) -> KStr {
    // SAFETY: forwarded contract.
    let text = unsafe { text_of(path) };
    // SAFETY: same.
    unsafe { release(path) };
    match kira_runtime_abi::file_system::perform(FileRequest::ReadText { path: &text }) {
        FileResponse::Text(contents) => handle_of(&contents),
        _ => handle_of(""),
    }
}

/// Writes text over a file's whole contents.
///
/// # Safety
/// Both handles must be null or live; both are freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_write_text(path: KStr, contents: KStr) -> u8 {
    // SAFETY: forwarded contract.
    let (path_text, body) = unsafe { (text_of(path), text_of(contents)) };
    // SAFETY: same.
    unsafe {
        release(path);
        release(contents);
    }
    flag(kira_runtime_abi::file_system::perform(
        FileRequest::WriteText {
            path: &path_text,
            text: &body,
        },
    ))
}

/// Lists a directory's entry names into a fresh `[String]`.
///
/// # Safety
/// `path` must be null or a live string handle, which this frees; `esize` must
/// be the ABI size of a string-handle element.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_list_directory(path: KStr, esize: i64) -> KArray {
    // SAFETY: forwarded contract.
    let text = unsafe { text_of(path) };
    // SAFETY: same.
    unsafe { release(path) };
    let FileResponse::Names(names) =
        kira_runtime_abi::file_system::perform(FileRequest::ListDirectory { path: &text })
    else {
        return kira_rt_array_new(0, esize.max(0) as usize);
    };
    // SAFETY: forwarded `esize` contract.
    unsafe { string_array(&names, esize) }
}

/// Runs one path-in, flag-out operation, freeing the handle.
///
/// # Safety
/// `path` must be null or a live string handle, which this frees.
unsafe fn one_path(path: KStr, build: impl FnOnce(&str) -> FileRequest<'_>) -> u8 {
    // SAFETY: forwarded contract.
    let text = unsafe { text_of(path) };
    // SAFETY: same.
    unsafe { release(path) };
    flag(kira_runtime_abi::file_system::perform(build(&text)))
}

/// Whether a path exists and is a directory.
///
/// # Safety
/// `path` must be null or a live string handle, which this frees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_is_directory(path: KStr) -> u8 {
    // SAFETY: forwarded contract.
    unsafe { one_path(path, |path| FileRequest::IsDirectory { path }) }
}

/// Creates one directory, without creating its parents.
///
/// # Safety
/// `path` must be null or a live string handle, which this frees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_make_directory(path: KStr) -> u8 {
    // SAFETY: forwarded contract.
    unsafe { one_path(path, |path| FileRequest::MakeDirectory { path }) }
}

/// Removes a file, or a directory and everything under it.
///
/// # Safety
/// `path` must be null or a live string handle, which this frees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_remove_path(path: KStr) -> u8 {
    // SAFETY: forwarded contract.
    unsafe { one_path(path, |path| FileRequest::RemovePath { path }) }
}

/// Whether a path exists and is a regular file.
///
/// # Safety
/// `path` must be null or a live string handle, which this frees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_file_exists(path: KStr) -> u8 {
    // SAFETY: forwarded contract.
    unsafe { one_path(path, |path| FileRequest::FileExists { path }) }
}

/// Whether a path exists as any kind of entry.
///
/// # Safety
/// `path` must be null or a live string handle, which this frees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_path_exists(path: KStr) -> u8 {
    // SAFETY: forwarded contract.
    unsafe { one_path(path, |path| FileRequest::PathExists { path }) }
}

/// Renames a path, overwriting an existing target.
///
/// # Safety
/// Both handles must be null or live; both are freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_rename_path(from: KStr, to: KStr) -> u8 {
    // SAFETY: forwarded contract.
    let (source, target) = unsafe { (text_of(from), text_of(to)) };
    // SAFETY: same.
    unsafe {
        release(from);
        release(to);
    }
    flag(kira_runtime_abi::file_system::perform(
        FileRequest::RenamePath {
            from: &source,
            to: &target,
        },
    ))
}

/// A file's size in bytes, zero when there is no file.
///
/// # Safety
/// `path` must be null or a live string handle, which this frees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_fs_file_size(path: KStr) -> i64 {
    // SAFETY: forwarded contract.
    let text = unsafe { text_of(path) };
    // SAFETY: same.
    unsafe { release(path) };
    match kira_runtime_abi::file_system::perform(FileRequest::FileSize { path: &text }) {
        // The same 64 bits the VM pushes: `U64` and `Int` share one runtime
        // representation, so nothing is narrowed on the way through.
        FileResponse::Size(size) => size as i64,
        _ => 0,
    }
}
