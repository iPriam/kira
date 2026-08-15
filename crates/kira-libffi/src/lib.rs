//! Kira's bundled libffi runtime and shared C-signature model.
//!
//! The graph is built from [`kira_runtime_abi::ForeignSignature`] and
//! [`kira_runtime_abi::ForeignAggregates`]. Both the VM host and native
//! runtimes use this crate, so nested structs, fixed arrays, scalar widths,
//! pointers, strings, and aggregate results cannot acquire backend-specific
//! ABI rules.

mod call;
mod closure;
mod native;
mod raw;
mod types;

pub use call::{LibffiRuntime, PreparedCall};
pub use closure::{FfiClosure, FfiClosureCallback};
pub use native::{
    FfiAggregateDescriptor, FfiMemberDescriptor, FfiSignatureDescriptor, FfiTypeDescriptor,
    KIRA_FFI_INVALID_DESCRIPTOR, KIRA_FFI_INVALID_RESULT, KIRA_FFI_MISSING_BUNDLE,
    KIRA_FFI_NULL_FUNCTION, KIRA_FFI_OK, kira_rt_ffi_call, kira_rt_ffi_call_bytes,
    kira_rt_ffi_closure,
};
pub use raw::RawFfiCif;

use std::path::{Path, PathBuf};

/// Why the bundled libffi runtime could not be loaded or prepared.
#[derive(Debug, thiserror::Error)]
pub enum LibffiError {
    /// No Kira-shipped libffi binary was found beside the executable or in the
    /// target's bundled vendor directory.
    #[error("Kira's bundled libffi runtime is missing; expected `{expected}`")]
    MissingBundle {
        /// The platform file name that was required.
        expected: PathBuf,
    },
    /// This target has no Kira-provided libffi artifact.
    #[error(
        "Kira has no bundled libffi artifact for host `{target}`; build the sibling libffi source for this host and package `{expected}`"
    )]
    UnavailableHost {
        /// The target host that lacks a prebuilt artifact.
        target: String,
        /// The file name the package must provide.
        expected: PathBuf,
    },
    /// The bundled binary could not be opened.
    #[error("cannot load bundled libffi `{path}`: {source}")]
    Load {
        /// The binary Kira tried to open.
        path: PathBuf,
        /// The loader error.
        #[source]
        source: libloading::Error,
    },
    /// A required libffi symbol was absent.
    #[error("bundled libffi does not export `{name}`: {source}")]
    Symbol {
        /// The missing symbol.
        name: String,
        /// The loader error.
        #[source]
        source: libloading::Error,
    },
    /// libffi rejected a CIF or closure description.
    #[error("libffi rejected the signature with status {status}")]
    Prepare {
        /// The open libffi status code.
        status: i32,
    },
    /// An aggregate id did not exist in the shared table.
    #[error("libffi graph names unknown aggregate {0}")]
    UnknownAggregate(u32),
    /// Storage for a native argument or result could not be allocated.
    #[error("cannot allocate aligned libffi storage of {size} bytes with alignment {alignment}")]
    Storage {
        /// The requested byte count.
        size: usize,
        /// The requested alignment.
        alignment: usize,
    },
    /// The target address was null.
    #[error("libffi call received a null function address")]
    NullFunction,
    /// The call's checked foreign value contract failed.
    #[error(transparent)]
    Call(#[from] kira_runtime_abi::ForeignCallError),
    /// The aggregate table could not be laid out.
    #[error(transparent)]
    Aggregate(#[from] kira_runtime_abi::ForeignAggregateError),
    /// The native libffi helper archive could not be located beside Kira.
    #[error("Kira's native libffi helper archive is missing; expected `{expected}`")]
    RuntimeArchive {
        /// The archive name the build expected.
        expected: PathBuf,
    },
    /// A bundled file could not be staged beside a native artifact.
    #[error("cannot stage bundled libffi at `{path}`: {source}")]
    Io {
        /// The file or directory involved in staging.
        path: PathBuf,
        /// The filesystem failure.
        #[source]
        source: std::io::Error,
    },
}

/// Returns the path of Kira's bundled libffi binary, without consulting a
/// system library search path or an environment variable.
pub fn bundled_path() -> Result<PathBuf, LibffiError> {
    raw::bundled_path()
}

/// Returns the file name used for the bundled libffi runtime on this target.
pub fn bundled_file_name() -> &'static str {
    raw::bundled_file_name()
}

/// Copies the bundled libffi binary beside a finished native artifact.
pub fn stage_bundle(destination: &Path) -> Result<PathBuf, LibffiError> {
    let source = bundled_path()?;
    std::fs::create_dir_all(destination).map_err(|source| LibffiError::Io {
        path: destination.to_path_buf(),
        source,
    })?;
    let file_name = source
        .file_name()
        .ok_or_else(|| LibffiError::MissingBundle {
            expected: source.clone(),
        })?;
    let target = destination.join(file_name);
    if source != target {
        std::fs::copy(&source, &target).map_err(|source| LibffiError::Io {
            path: target.clone(),
            source,
        })?;
    }
    Ok(target)
}

/// Locates the static Rust helper archive linked into native Kira artifacts.
pub fn runtime_archive() -> Result<PathBuf, LibffiError> {
    let expected = PathBuf::from(runtime_archive_name());
    let executable = std::env::current_exe().map_err(|_| LibffiError::RuntimeArchive {
        expected: expected.clone(),
    })?;
    let directory = executable
        .parent()
        .ok_or_else(|| LibffiError::RuntimeArchive {
            expected: expected.clone(),
        })?;
    find_runtime_archive(directory, &expected).ok_or(LibffiError::RuntimeArchive { expected })
}

fn runtime_archive_name() -> &'static str {
    if cfg!(target_env = "msvc") {
        "kira_libffi.lib"
    } else {
        "libkira_libffi.a"
    }
}

fn find_runtime_archive(directory: &Path, expected: &Path) -> Option<PathBuf> {
    let direct = directory.join(expected);
    if direct.is_file() {
        return Some(direct);
    }
    let stem = expected.file_stem()?.to_str()?;
    let extension = expected.extension()?.to_str()?;
    let prefix = format!("{stem}-");
    let mut directories = vec![directory.to_path_buf()];
    if directory.file_name().and_then(|name| name.to_str()) != Some("deps") {
        directories.push(directory.join("deps"));
    }
    let mut candidates = directories
        .into_iter()
        .flat_map(|directory| {
            std::fs::read_dir(directory)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
        })
        .filter(|path| {
            path.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some(extension)
                && path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.starts_with(&prefix))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    candidates.pop()
}

#[cfg(test)]
mod tests {
    use std::ffi::{c_char, c_void};

    use kira_runtime_abi::{
        ForeignAggregate, ForeignAggregates, ForeignArg, ForeignMember, ForeignResult,
        ForeignSignature, ForeignType, ForeignTypeSpec,
    };

    use super::{FfiClosure, LibffiRuntime, RawFfiCif};

    unsafe extern "C" fn add(left: i32, right: f64) -> f64 {
        f64::from(left) + right
    }

    unsafe extern "C" fn c_string_length(value: *const c_char) -> u64 {
        // SAFETY: the test caller supplies a live NUL-terminated C string.
        unsafe { std::ffi::CStr::from_ptr(value) }.count_bytes() as u64
    }

    #[repr(C)]
    struct Pair {
        left: i32,
        right: f64,
    }

    unsafe extern "C" fn pair_total(value: Pair) -> f64 {
        f64::from(value.left) + value.right
    }

    unsafe extern "C" fn scalar_callback_entry(
        _cif: *mut RawFfiCif,
        result: *mut c_void,
        arguments: *mut *mut c_void,
        _user_data: *mut c_void,
    ) {
        // SAFETY: libffi supplies one pointer to each prepared argument and a
        // result slot with the exact signature used to prepare the closure.
        unsafe {
            let left_pointer = (arguments as *const *const i32).read();
            let right_pointer = ((arguments as *const *const f64).add(1)).read();
            result
                .cast::<f64>()
                .write(f64::from(*left_pointer) + *right_pointer);
        }
    }

    unsafe extern "C" fn aggregate_callback_entry(
        _cif: *mut RawFfiCif,
        result: *mut c_void,
        arguments: *mut *mut c_void,
        _user_data: *mut c_void,
    ) {
        // SAFETY: the callback CIF names one Pair aggregate and one f64 result,
        // so the argument and result storage have those exact layouts.
        unsafe {
            let pair = &*(arguments.read().cast::<Pair>());
            result
                .cast::<f64>()
                .write(f64::from(pair.left) + pair.right);
        }
    }

    unsafe extern "C" fn invoke_scalar_callback(
        callback: unsafe extern "C" fn(i32, f64) -> f64,
    ) -> f64 {
        // SAFETY: the caller supplies a live libffi closure with this signature.
        unsafe { callback(4, 0.5) }
    }

    unsafe extern "C" fn invoke_aggregate_callback(
        callback: unsafe extern "C" fn(Pair) -> f64,
    ) -> f64 {
        // SAFETY: the caller supplies a live libffi closure with this signature.
        unsafe {
            callback(Pair {
                left: 4,
                right: 0.5,
            })
        }
    }

    #[test]
    fn bundled_libffi_calls_scalars_without_a_system_lookup() {
        let runtime = LibffiRuntime::load().expect("the checked-in host bundle loads");
        let signature =
            ForeignSignature::scalars([ForeignType::I32, ForeignType::F64], ForeignType::F64);
        // SAFETY: `add` is this test's own C-ABI function and the signature
        // describes it exactly.
        let result = unsafe {
            runtime
                .call(
                    add as *mut c_void,
                    &signature,
                    &ForeignAggregates::new(),
                    &[ForeignArg::I32(4), ForeignArg::F64(0.5)],
                )
                .expect("libffi call")
        };
        assert_eq!(result, ForeignResult::F64(4.5));
    }

    #[test]
    fn bundled_libffi_calls_a_c_string_without_a_system_lookup() {
        let runtime = LibffiRuntime::load().expect("the checked-in host bundle loads");
        let signature = ForeignSignature::scalars([ForeignType::CString], ForeignType::U64);
        // SAFETY: `c_string_length` is this test's own C-ABI function and the
        // signature describes it exactly.
        let result = unsafe {
            runtime
                .call(
                    c_string_length as *mut c_void,
                    &signature,
                    &ForeignAggregates::new(),
                    &[ForeignArg::CString("kira")],
                )
                .expect("libffi CString call")
        };
        assert_eq!(result, ForeignResult::U64(4));
    }

    #[test]
    fn the_same_graph_calls_a_by_value_aggregate_result() {
        let runtime = LibffiRuntime::load().expect("the checked-in host bundle loads");
        let mut aggregates = ForeignAggregates::new();
        let id = aggregates
            .push(ForeignAggregate::new([
                ForeignMember::Scalar(ForeignType::I32),
                ForeignMember::Scalar(ForeignType::F64),
            ]))
            .expect("aggregate");
        let signature = ForeignSignature::new([ForeignTypeSpec::Aggregate(id)], ForeignType::F64);
        let mut bytes = vec![0; 16];
        bytes[..4].copy_from_slice(&4i32.to_ne_bytes());
        bytes[8..16].copy_from_slice(&0.5f64.to_ne_bytes());
        // SAFETY: `pair_total` takes exactly the aggregate described above, and
        // `bytes` holds one of them in its C layout.
        let result = unsafe {
            runtime
                .call(
                    pair_total as *mut c_void,
                    &signature,
                    &aggregates,
                    &[ForeignArg::Aggregate { id, bytes: &bytes }],
                )
                .expect("aggregate call")
        };
        assert_eq!(result, ForeignResult::F64(4.5));
    }

    #[test]
    fn bundled_libffi_closures_enter_scalar_and_aggregate_callbacks() {
        let runtime = LibffiRuntime::load().expect("the checked-in host bundle loads");
        let scalar_signature =
            ForeignSignature::scalars([ForeignType::I32, ForeignType::F64], ForeignType::F64);
        // SAFETY: the closure carries no user data, so nothing has to outlive it.
        let scalar = unsafe {
            FfiClosure::new(
                &runtime,
                &scalar_signature,
                &ForeignAggregates::new(),
                scalar_callback_entry,
                std::ptr::null_mut(),
            )
        }
        .expect("scalar closure");
        // SAFETY: the code address libffi returned has the signature prepared
        // above, and the closure outlives the call below.
        let scalar_function: unsafe extern "C" fn(i32, f64) -> f64 =
            unsafe { std::mem::transmute(scalar.code()) };
        // SAFETY: as above, for the entry that invokes it.
        let scalar_result = unsafe { invoke_scalar_callback(scalar_function) };
        assert_eq!(scalar_result, 4.5);

        let mut aggregates = ForeignAggregates::new();
        let id = aggregates
            .push(ForeignAggregate::new([
                ForeignMember::Scalar(ForeignType::I32),
                ForeignMember::Scalar(ForeignType::F64),
            ]))
            .expect("aggregate");
        let aggregate_signature =
            ForeignSignature::new([ForeignTypeSpec::Aggregate(id)], ForeignType::F64);
        // SAFETY: the closure carries no user data, so nothing has to outlive it.
        let aggregate = unsafe {
            FfiClosure::new(
                &runtime,
                &aggregate_signature,
                &aggregates,
                aggregate_callback_entry,
                std::ptr::null_mut(),
            )
        }
        .expect("aggregate closure");
        // SAFETY: the code address libffi returned takes the aggregate prepared
        // above, and the closure outlives the call below.
        let aggregate_function: unsafe extern "C" fn(Pair) -> f64 =
            unsafe { std::mem::transmute(aggregate.code()) };
        // SAFETY: as above, for the entry that invokes it.
        let aggregate_result = unsafe { invoke_aggregate_callback(aggregate_function) };
        assert_eq!(aggregate_result, 4.5);
    }

    #[test]
    fn one_libffi_runtime_can_serve_calls_on_many_threads() {
        let runtime = LibffiRuntime::load().expect("the checked-in host bundle loads");
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let runtime = runtime.clone();
                scope.spawn(move || {
                    let signature = ForeignSignature::scalars(
                        [ForeignType::I32, ForeignType::F64],
                        ForeignType::F64,
                    );
                    for _ in 0..64 {
                        // SAFETY: `add` is this test's own C-ABI function and
                        // the signature describes it exactly.
                        let result = unsafe {
                            runtime
                                .call(
                                    add as *mut c_void,
                                    &signature,
                                    &ForeignAggregates::new(),
                                    &[ForeignArg::I32(4), ForeignArg::F64(0.5)],
                                )
                                .expect("threaded libffi call")
                        };
                        assert_eq!(result, ForeignResult::F64(4.5));
                    }
                });
            }
        });
    }

    #[test]
    fn an_owned_libffi_closure_can_move_to_its_callback_thread() {
        let runtime = LibffiRuntime::load().expect("the checked-in host bundle loads");
        let signature =
            ForeignSignature::scalars([ForeignType::I32, ForeignType::F64], ForeignType::F64);
        // SAFETY: the closure carries no user data, so nothing has to outlive it.
        let closure = unsafe {
            FfiClosure::new(
                &runtime,
                &signature,
                &ForeignAggregates::new(),
                scalar_callback_entry,
                std::ptr::null_mut(),
            )
        }
        .expect("scalar closure");
        let code = closure.code() as usize;
        let result = std::thread::spawn(move || {
            let _closure = closure;
            // SAFETY: the closure moved into this thread, so its code address
            // is live for the call, with the signature prepared above.
            let function: unsafe extern "C" fn(i32, f64) -> f64 =
                unsafe { std::mem::transmute(code as *mut c_void) };
            // SAFETY: as above, for the entry that invokes it.
            unsafe { invoke_scalar_callback(function) }
        })
        .join()
        .expect("callback thread");
        assert_eq!(result, 4.5);
    }
}
