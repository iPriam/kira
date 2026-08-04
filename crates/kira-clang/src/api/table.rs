//! Loading `libclang` and holding its entry points.
//!
//! [`Api`] is the whole `unsafe` fence for calls that own something — the
//! index, the translation unit, the visit, and the strings libclang allocates.
//! The accessors that only read a by-value handle live in [`super::calls`].

use std::ffi::{CStr, c_char, c_int, c_longlong, c_uint, c_void};
use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};

use super::raw::{
    CxClientData, CxCursor, CxCursorVisitor, CxDiagnostic, CxFile, CxIndex, CxSourceLocation,
    CxString, CxTranslationUnit, CxType, DiagnosticSeverity,
};

/// Why libclang could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// No candidate path under the LLVM install named a loadable library.
    #[error(
        "cannot load libclang from the LLVM toolchain at `{home}`: {message}\n\
         note: header autobinding needs the `libclang` shared library that ships \
         beside `clang`; checked {checked}"
    )]
    Unloadable {
        /// The LLVM install root that was searched.
        home: String,
        /// The last loader failure, rendered.
        message: String,
        /// Every candidate path that was tried, rendered.
        checked: String,
    },
    /// The library loaded but does not export a call this crate needs.
    #[error("libclang at `{path}` exports no `{symbol}`: {message}")]
    MissingSymbol {
        /// The library that was loaded.
        path: String,
        /// The C symbol that was missing.
        symbol: &'static str,
        /// The loader failure, rendered.
        message: String,
    },
}

/// The loaded libclang entry points, in `clang-c/Index.h` order.
///
/// Held as plain function pointers rather than `libloading::Symbol` values so a
/// call site needs no lifetime: [`Api::library`] keeps the library alive for as
/// long as the pointers exist, which is the invariant every call below rests on.
pub struct Api {
    /// Keeps the loaded library mapped; every pointer below points into it.
    pub(super) library: Library,
    pub(super) create_index: unsafe extern "C" fn(c_int, c_int) -> CxIndex,
    pub(super) dispose_index: unsafe extern "C" fn(CxIndex),
    pub(super) parse_translation_unit2: unsafe extern "C" fn(
        CxIndex,
        *const c_char,
        *const *const c_char,
        c_int,
        *mut c_void,
        c_uint,
        c_uint,
        *mut CxTranslationUnit,
    ) -> c_int,
    pub(super) dispose_translation_unit: unsafe extern "C" fn(CxTranslationUnit),
    pub(super) get_translation_unit_cursor: unsafe extern "C" fn(CxTranslationUnit) -> CxCursor,
    pub(super) visit_children:
        unsafe extern "C" fn(CxCursor, CxCursorVisitor, CxClientData) -> c_uint,
    pub(super) get_cursor_spelling: unsafe extern "C" fn(CxCursor) -> CxString,
    pub(super) get_cursor_type: unsafe extern "C" fn(CxCursor) -> CxType,
    pub(super) get_cursor_result_type: unsafe extern "C" fn(CxCursor) -> CxType,
    pub(super) get_cursor_location: unsafe extern "C" fn(CxCursor) -> CxSourceLocation,
    pub(super) get_file_location:
        unsafe extern "C" fn(CxSourceLocation, *mut CxFile, *mut c_uint, *mut c_uint, *mut c_uint),
    pub(super) get_file_name: unsafe extern "C" fn(CxFile) -> CxString,
    pub(super) cursor_is_variadic: unsafe extern "C" fn(CxCursor) -> c_uint,
    pub(super) cursor_is_bit_field: unsafe extern "C" fn(CxCursor) -> c_uint,
    pub(super) cursor_storage_class: unsafe extern "C" fn(CxCursor) -> c_int,
    pub(super) is_cursor_definition: unsafe extern "C" fn(CxCursor) -> c_uint,
    pub(super) get_typedef_decl_underlying_type: unsafe extern "C" fn(CxCursor) -> CxType,
    pub(super) get_enum_decl_integer_type: unsafe extern "C" fn(CxCursor) -> CxType,
    pub(super) get_enum_constant_decl_value: unsafe extern "C" fn(CxCursor) -> c_longlong,
    pub(super) get_type_spelling: unsafe extern "C" fn(CxType) -> CxString,
    pub(super) get_type_declaration: unsafe extern "C" fn(CxType) -> CxCursor,
    pub(super) get_canonical_type: unsafe extern "C" fn(CxType) -> CxType,
    pub(super) get_pointee_type: unsafe extern "C" fn(CxType) -> CxType,
    pub(super) get_array_element_type: unsafe extern "C" fn(CxType) -> CxType,
    pub(super) get_array_size: unsafe extern "C" fn(CxType) -> c_longlong,
    pub(super) get_result_type: unsafe extern "C" fn(CxType) -> CxType,
    pub(super) get_num_arg_types: unsafe extern "C" fn(CxType) -> c_int,
    pub(super) get_arg_type: unsafe extern "C" fn(CxType, c_uint) -> CxType,
    pub(super) is_function_type_variadic: unsafe extern "C" fn(CxType) -> c_uint,
    pub(super) is_const_qualified_type: unsafe extern "C" fn(CxType) -> c_uint,
    pub(super) type_get_size_of: unsafe extern "C" fn(CxType) -> c_longlong,
    pub(super) get_num_diagnostics: unsafe extern "C" fn(CxTranslationUnit) -> c_uint,
    pub(super) get_diagnostic: unsafe extern "C" fn(CxTranslationUnit, c_uint) -> CxDiagnostic,
    pub(super) get_diagnostic_severity: unsafe extern "C" fn(CxDiagnostic) -> DiagnosticSeverity,
    pub(super) format_diagnostic: unsafe extern "C" fn(CxDiagnostic, c_uint) -> CxString,
    pub(super) dispose_diagnostic: unsafe extern "C" fn(CxDiagnostic),
    pub(super) get_c_string: unsafe extern "C" fn(CxString) -> *const c_char,
    pub(super) dispose_string: unsafe extern "C" fn(CxString),
}

/// Reads one exported function pointer out of a loaded library.
///
/// Each entry point names its symbol exactly once; spelling it twice is how a
/// table drifts from the header it mirrors.
///
/// # Safety
///
/// `T` must be the exact signature `symbol` is declared with in
/// `clang-c/Index.h`. A mismatch is undefined behavior at the first call, not
/// at load time, which is why the whole table is bound in one place against
/// one copy of the header.
unsafe fn entry<T: Copy>(
    library: &Library,
    path: &Path,
    symbol: &'static str,
) -> Result<T, LoadError> {
    // SAFETY: `T` is the declared signature by this function's own contract,
    // and the pointer is only ever called with the library still loaded —
    // `Api` owns it for exactly as long.
    let found: Symbol<'_, T> =
        unsafe { library.get(symbol.as_bytes()) }.map_err(|error| LoadError::MissingSymbol {
            path: path.display().to_string(),
            symbol,
            message: error.to_string(),
        })?;
    Ok(*found)
}

impl Api {
    /// Loads libclang from the LLVM installation rooted at `home`.
    ///
    /// Every layout the managed bundles ship is tried in turn, so the caller
    /// names an install root rather than a file. All of them failing reports
    /// each path that was checked.
    pub fn load(home: &Path) -> Result<Self, LoadError> {
        let candidates = libclang_candidates(home);
        let mut last = String::from("no candidate path exists");
        for candidate in &candidates {
            // SAFETY: loading a shared library runs its initializers. This one
            // is libclang out of Kira's own managed toolchain, selected by
            // layout rather than by a caller-supplied path.
            match unsafe { Library::new(candidate) } {
                Ok(library) => return Self::bind(library, candidate),
                Err(error) => last = error.to_string(),
            }
        }
        Err(LoadError::Unloadable {
            home: home.display().to_string(),
            message: last,
            checked: candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        })
    }

    /// Binds every entry point out of an already-loaded library.
    fn bind(library: Library, path: &Path) -> Result<Self, LoadError> {
        // SAFETY: each signature below is the one `clang-c/Index.h` declares
        // for the symbol named beside it — transcribed once in the field list
        // above and paired with its name here. This is the only call to
        // `entry`, so the whole table is bound against one copy of the header.
        Ok(unsafe {
            Self {
                create_index: entry(&library, path, "clang_createIndex")?,
                dispose_index: entry(&library, path, "clang_disposeIndex")?,
                parse_translation_unit2: entry(&library, path, "clang_parseTranslationUnit2")?,
                dispose_translation_unit: entry(&library, path, "clang_disposeTranslationUnit")?,
                get_translation_unit_cursor: entry(
                    &library,
                    path,
                    "clang_getTranslationUnitCursor",
                )?,
                visit_children: entry(&library, path, "clang_visitChildren")?,
                get_cursor_spelling: entry(&library, path, "clang_getCursorSpelling")?,
                get_cursor_type: entry(&library, path, "clang_getCursorType")?,
                get_cursor_result_type: entry(&library, path, "clang_getCursorResultType")?,
                get_cursor_location: entry(&library, path, "clang_getCursorLocation")?,
                get_file_location: entry(&library, path, "clang_getFileLocation")?,
                get_file_name: entry(&library, path, "clang_getFileName")?,
                cursor_is_variadic: entry(&library, path, "clang_Cursor_isVariadic")?,
                cursor_is_bit_field: entry(&library, path, "clang_Cursor_isBitField")?,
                cursor_storage_class: entry(&library, path, "clang_Cursor_getStorageClass")?,
                is_cursor_definition: entry(&library, path, "clang_isCursorDefinition")?,
                get_typedef_decl_underlying_type: entry(
                    &library,
                    path,
                    "clang_getTypedefDeclUnderlyingType",
                )?,
                get_enum_decl_integer_type: entry(&library, path, "clang_getEnumDeclIntegerType")?,
                get_enum_constant_decl_value: entry(
                    &library,
                    path,
                    "clang_getEnumConstantDeclValue",
                )?,
                get_type_spelling: entry(&library, path, "clang_getTypeSpelling")?,
                get_type_declaration: entry(&library, path, "clang_getTypeDeclaration")?,
                get_canonical_type: entry(&library, path, "clang_getCanonicalType")?,
                get_pointee_type: entry(&library, path, "clang_getPointeeType")?,
                get_array_element_type: entry(&library, path, "clang_getArrayElementType")?,
                get_array_size: entry(&library, path, "clang_getArraySize")?,
                get_result_type: entry(&library, path, "clang_getResultType")?,
                get_num_arg_types: entry(&library, path, "clang_getNumArgTypes")?,
                get_arg_type: entry(&library, path, "clang_getArgType")?,
                is_function_type_variadic: entry(&library, path, "clang_isFunctionTypeVariadic")?,
                is_const_qualified_type: entry(&library, path, "clang_isConstQualifiedType")?,
                type_get_size_of: entry(&library, path, "clang_Type_getSizeOf")?,
                get_num_diagnostics: entry(&library, path, "clang_getNumDiagnostics")?,
                get_diagnostic: entry(&library, path, "clang_getDiagnostic")?,
                get_diagnostic_severity: entry(&library, path, "clang_getDiagnosticSeverity")?,
                format_diagnostic: entry(&library, path, "clang_formatDiagnostic")?,
                dispose_diagnostic: entry(&library, path, "clang_disposeDiagnostic")?,
                get_c_string: entry(&library, path, "clang_getCString")?,
                dispose_string: entry(&library, path, "clang_disposeString")?,
                library,
            }
        })
    }

    /// The loaded library, kept alive for as long as the entry points are.
    pub(crate) fn library(&self) -> &Library {
        &self.library
    }

    /// Reads a libclang-owned string and releases it.
    ///
    /// Every string call goes through here, so no path can forget the disposal.
    pub(crate) fn take_string(&self, string: CxString) -> String {
        // SAFETY: `string` was produced by a libclang call on this same `Api`,
        // so both the read and the disposal are on the library that owns it,
        // and the borrow of the C string ends before the disposal.
        unsafe {
            let pointer = (self.get_c_string)(string);
            let text = match pointer.is_null() {
                true => String::new(),
                false => CStr::from_ptr(pointer).to_string_lossy().into_owned(),
            };
            (self.dispose_string)(string);
            text
        }
    }

    /// Creates a parsing index that excludes declarations from precompiled
    /// headers and prints nothing of its own.
    pub(crate) fn create_index(&self) -> CxIndex {
        // SAFETY: a plain libclang constructor with no pointer arguments.
        unsafe { (self.create_index)(1, 0) }
    }

    /// Releases a parsing index.
    ///
    /// # Safety
    ///
    /// `index` must have come from [`Api::create_index`] on this `Api` and must
    /// not be used again.
    pub(crate) unsafe fn dispose_index(&self, index: CxIndex) {
        // SAFETY: guaranteed by this function's own contract.
        unsafe { (self.dispose_index)(index) }
    }

    /// Parses one translation unit.
    ///
    /// # Safety
    ///
    /// `index` must be live, `filename` a NUL-terminated path, and `arguments`
    /// an array of `argument_count` NUL-terminated pointers that outlive the
    /// call.
    pub(crate) unsafe fn parse(
        &self,
        index: CxIndex,
        filename: *const c_char,
        arguments: *const *const c_char,
        argument_count: c_int,
        options: c_uint,
        unit: *mut CxTranslationUnit,
    ) -> c_int {
        // SAFETY: guaranteed by this function's own contract; the unsaved-file
        // array is empty, which is what a null pointer with a zero count means.
        unsafe {
            (self.parse_translation_unit2)(
                index,
                filename,
                arguments,
                argument_count,
                std::ptr::null_mut(),
                0,
                options,
                unit,
            )
        }
    }

    /// Releases a translation unit.
    ///
    /// # Safety
    ///
    /// `unit` must have come from [`Api::parse`] on this `Api` and must not be
    /// used again.
    pub(crate) unsafe fn dispose_translation_unit(&self, unit: CxTranslationUnit) {
        // SAFETY: guaranteed by this function's own contract.
        unsafe { (self.dispose_translation_unit)(unit) }
    }

    /// The root cursor of a parsed unit.
    ///
    /// # Safety
    ///
    /// `unit` must be live.
    pub(crate) unsafe fn translation_unit_cursor(&self, unit: CxTranslationUnit) -> CxCursor {
        // SAFETY: guaranteed by this function's own contract.
        unsafe { (self.get_translation_unit_cursor)(unit) }
    }

    /// Visits a cursor's immediate children.
    ///
    /// # Safety
    ///
    /// `visitor` must be sound for every child cursor it is handed, and `data`
    /// must be what that visitor expects.
    pub(crate) unsafe fn visit_children(
        &self,
        cursor: CxCursor,
        visitor: CxCursorVisitor,
        data: CxClientData,
    ) {
        // SAFETY: guaranteed by this function's own contract.
        unsafe {
            (self.visit_children)(cursor, visitor, data);
        }
    }

    /// The diagnostics a parse produced, most severe first not guaranteed.
    ///
    /// # Safety
    ///
    /// `unit` must be live.
    pub(crate) unsafe fn diagnostics(&self, unit: CxTranslationUnit) -> Vec<(i32, String)> {
        // SAFETY: guaranteed by this function's own contract; each diagnostic
        // handle is disposed before the next is fetched.
        unsafe {
            let count = (self.get_num_diagnostics)(unit);
            let mut found = Vec::new();
            for index in 0..count {
                let diagnostic = (self.get_diagnostic)(unit, index);
                let severity = (self.get_diagnostic_severity)(diagnostic).0;
                let text = self.take_string((self.format_diagnostic)(diagnostic, 1));
                (self.dispose_diagnostic)(diagnostic);
                found.push((severity, text));
            }
            found
        }
    }
}

/// Every place a managed LLVM bundle may keep `libclang`, in search order.
///
/// The bundles put it in `lib/` on Unix and `bin/` on Windows, and a Linux
/// build may ship only a versioned soname, so the unversioned name is tried
/// first and the versioned ones after it.
fn libclang_candidates(home: &Path) -> Vec<PathBuf> {
    let mut names: Vec<(&str, &str)> = Vec::new();
    if cfg!(target_os = "windows") {
        names.push(("bin", "libclang.dll"));
        names.push(("bin", "clang.dll"));
    } else if cfg!(target_os = "macos") {
        names.push(("lib", "libclang.dylib"));
    } else {
        names.push(("lib", "libclang.so"));
        names.push(("lib", "libclang.so.22"));
        names.push(("lib", "libclang-22.so"));
    }
    names
        .into_iter()
        .map(|(directory, file)| home.join(directory).join(file))
        .filter(|path| path.exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_home_with_no_library_offers_no_candidates() {
        assert!(libclang_candidates(Path::new("/definitely/not/llvm")).is_empty());
    }
}
