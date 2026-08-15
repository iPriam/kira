//! The small libffi C surface Kira uses.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};

use crate::LibffiError;

pub(crate) const FFI_TYPE_STRUCT: u16 = 13;

#[repr(C)]
pub(crate) struct RawFfiType {
    pub(crate) size: usize,
    pub(crate) alignment: u16,
    pub(crate) kind: u16,
    pub(crate) elements: *mut *mut RawFfiType,
}

#[repr(C)]
pub struct RawFfiCif {
    pub(crate) abi: u32,
    pub(crate) nargs: u32,
    pub(crate) arg_types: *mut *mut RawFfiType,
    pub(crate) result_type: *mut RawFfiType,
    pub(crate) bytes: u32,
    pub(crate) flags: u32,
}

pub(crate) type RawFunction = unsafe extern "C" fn();
pub(crate) type ClosureCallback = unsafe extern "C" fn(
    cif: *mut RawFfiCif,
    result: *mut c_void,
    arguments: *mut *mut c_void,
    user_data: *mut c_void,
);

type PrepCif = unsafe extern "C" fn(
    cif: *mut RawFfiCif,
    abi: u32,
    nargs: u32,
    result_type: *mut RawFfiType,
    argument_types: *mut *mut RawFfiType,
) -> i32;
type Call = unsafe extern "C" fn(
    cif: *mut RawFfiCif,
    function: RawFunction,
    result: *mut c_void,
    arguments: *mut *mut c_void,
);
type ClosureAlloc = unsafe extern "C" fn(size: usize, code: *mut *mut c_void) -> *mut c_void;
type ClosureFree = unsafe extern "C" fn(closure: *mut c_void);
type PrepClosure = unsafe extern "C" fn(
    closure: *mut c_void,
    cif: *mut RawFfiCif,
    callback: ClosureCallback,
    user_data: *mut c_void,
    code: *mut c_void,
) -> i32;
type ClosureSize = unsafe extern "C" fn() -> usize;
type DefaultAbi = unsafe extern "C" fn() -> u32;

/// The loaded libffi entry points and standard scalar type descriptors.
pub(crate) struct RawLibffi {
    _library: Library,
    pub(crate) prep_cif: PrepCif,
    pub(crate) call: Call,
    pub(crate) closure_alloc: ClosureAlloc,
    pub(crate) closure_free: ClosureFree,
    pub(crate) prep_closure_loc: PrepClosure,
    pub(crate) closure_size: ClosureSize,
    pub(crate) default_abi: DefaultAbi,
    pub(crate) type_void: *mut RawFfiType,
    pub(crate) type_uint8: *mut RawFfiType,
    pub(crate) type_sint8: *mut RawFfiType,
    pub(crate) type_uint16: *mut RawFfiType,
    pub(crate) type_sint16: *mut RawFfiType,
    pub(crate) type_uint32: *mut RawFfiType,
    pub(crate) type_sint32: *mut RawFfiType,
    pub(crate) type_uint64: *mut RawFfiType,
    pub(crate) type_sint64: *mut RawFfiType,
    pub(crate) type_float: *mut RawFfiType,
    pub(crate) type_double: *mut RawFfiType,
    pub(crate) type_pointer: *mut RawFfiType,
}

// SAFETY: `RawLibffi` owns the loaded library handle and only stores function
// pointers plus libffi's process-global scalar type descriptors. The function
// table and descriptors are immutable after loading; every CIF, aggregate
// graph, and closure allocation that libffi mutates is owned by the caller and
// never shared through this value. Keeping `_library` in the same object keeps
// all copied pointers valid until the last `Arc` is dropped.
unsafe impl Send for RawLibffi {}

// SAFETY: the same invariant as `Send` applies to concurrent readers. Libffi's
// call, CIF preparation, and closure preparation APIs receive their mutable
// state through caller-owned pointers, while this table is read-only.
unsafe impl Sync for RawLibffi {}

impl RawLibffi {
    pub(crate) fn load() -> Result<Self, LibffiError> {
        Self::load_from(&bundled_path()?)
    }

    pub(crate) fn load_from(path: &Path) -> Result<Self, LibffiError> {
        let path = path.to_path_buf();
        // SAFETY: loading a library is the boundary this type owns; all symbols
        // are checked immediately and the handle stays alive with the pointers.
        let library = unsafe { Library::new(&path) }.map_err(|source| LibffiError::Load {
            path: path.clone(),
            source,
        })?;
        Ok(Self {
            prep_cif: symbol(&library, b"ffi_prep_cif\0")?,
            call: symbol(&library, b"ffi_call\0")?,
            closure_alloc: symbol(&library, b"ffi_closure_alloc\0")?,
            closure_free: symbol(&library, b"ffi_closure_free\0")?,
            prep_closure_loc: symbol(&library, b"ffi_prep_closure_loc\0")?,
            closure_size: symbol(&library, b"ffi_get_closure_size\0")?,
            default_abi: symbol(&library, b"ffi_get_default_abi\0")?,
            type_void: symbol(&library, b"ffi_type_void\0")?,
            type_uint8: symbol(&library, b"ffi_type_uint8\0")?,
            type_sint8: symbol(&library, b"ffi_type_sint8\0")?,
            type_uint16: symbol(&library, b"ffi_type_uint16\0")?,
            type_sint16: symbol(&library, b"ffi_type_sint16\0")?,
            type_uint32: symbol(&library, b"ffi_type_uint32\0")?,
            type_sint32: symbol(&library, b"ffi_type_sint32\0")?,
            type_uint64: symbol(&library, b"ffi_type_uint64\0")?,
            type_sint64: symbol(&library, b"ffi_type_sint64\0")?,
            type_float: symbol(&library, b"ffi_type_float\0")?,
            type_double: symbol(&library, b"ffi_type_double\0")?,
            type_pointer: symbol(&library, b"ffi_type_pointer\0")?,
            _library: library,
        })
    }
}

fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, LibffiError> {
    // SAFETY: the symbol name is NUL-terminated and the returned function/data
    // pointer is kept valid by `RawLibffi::_library`.
    let value: Symbol<'_, T> =
        unsafe { library.get(name) }.map_err(|source| LibffiError::Symbol {
            name: String::from_utf8_lossy(&name[..name.len() - 1]).into_owned(),
            source,
        })?;
    Ok(*value)
}

pub(crate) fn bundled_path() -> Result<PathBuf, LibffiError> {
    let filename = bundled_file_name();
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.join(filename));
        if let Some(profile) = directory.parent() {
            candidates.push(profile.join(filename));
        }
    }
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("vendor")
            .join(vendor_target())
            .join(filename),
    );
    if let Some(candidate) = candidates.into_iter().find(|candidate| candidate.is_file()) {
        return Ok(candidate);
    }
    if vendor_target() == "unsupported" {
        return Err(LibffiError::UnavailableHost {
            target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            expected: PathBuf::from(filename),
        });
    }
    Err(LibffiError::MissingBundle {
        expected: PathBuf::from(filename),
    })
}

pub(crate) fn bundled_file_name() -> &'static str {
    kira_toolchain::bundled_libffi_name()
}

fn vendor_target() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "windows-x86_64"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macos-aarch64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "macos-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux-aarch64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64"
    } else {
        "unsupported"
    }
}
