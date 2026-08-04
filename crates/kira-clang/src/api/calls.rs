//! Every cursor and type accessor, as safe calls on plain by-value handles.
//!
//! libclang's accessors take and return the handles in [`super::raw`] and touch
//! no memory the caller owns, so each is a one-line `unsafe` block whose only
//! invariant — the handle came from this same library — is established by the
//! wrappers in [`crate::cursor`], which never mint a handle themselves.

use super::raw::{CxCursor, CxFile, CxType};
use super::table::Api;
use std::ffi::c_uint;

impl Api {
    /// The name a cursor declares.
    pub(crate) fn cursor_spelling(&self, cursor: CxCursor) -> String {
        // SAFETY: `cursor` came from this library, and the string is disposed
        // by `take_string`.
        self.take_string(unsafe { (self.get_cursor_spelling)(cursor) })
    }

    /// The type a cursor declares.
    pub(crate) fn cursor_type(&self, cursor: CxCursor) -> CxType {
        // SAFETY: `cursor` came from this library.
        unsafe { (self.get_cursor_type)(cursor) }
    }

    /// The result type of a function cursor.
    pub(crate) fn cursor_result_type(&self, cursor: CxCursor) -> CxType {
        // SAFETY: `cursor` came from this library.
        unsafe { (self.get_cursor_result_type)(cursor) }
    }

    /// The file a cursor was written in, or the empty string for a built-in.
    pub(crate) fn cursor_file(&self, cursor: CxCursor) -> String {
        // SAFETY: `cursor` came from this library; the out-parameters are
        // stack locals of the right types, and `file` is only read after the
        // call has written it.
        unsafe {
            let location = (self.get_cursor_location)(cursor);
            let mut file: CxFile = std::ptr::null_mut();
            let mut line: c_uint = 0;
            let mut column: c_uint = 0;
            let mut offset: c_uint = 0;
            (self.get_file_location)(
                location,
                &raw mut file,
                &raw mut line,
                &raw mut column,
                &raw mut offset,
            );
            match file.is_null() {
                true => String::new(),
                false => self.take_string((self.get_file_name)(file)),
            }
        }
    }

    /// Whether a function cursor takes a trailing `...`.
    pub(crate) fn cursor_is_variadic(&self, cursor: CxCursor) -> bool {
        // SAFETY: `cursor` came from this library.
        unsafe { (self.cursor_is_variadic)(cursor) != 0 }
    }

    /// Whether a field cursor declares a bitfield.
    pub(crate) fn cursor_is_bit_field(&self, cursor: CxCursor) -> bool {
        // SAFETY: `cursor` came from this library.
        unsafe { (self.cursor_is_bit_field)(cursor) != 0 }
    }

    /// Whether a cursor was declared `static` (`CX_SC_Static` is 3).
    pub(crate) fn cursor_is_static(&self, cursor: CxCursor) -> bool {
        // SAFETY: `cursor` came from this library.
        unsafe { (self.cursor_storage_class)(cursor) == 3 }
    }

    /// Whether a cursor is a definition rather than only a declaration.
    pub(crate) fn cursor_is_definition(&self, cursor: CxCursor) -> bool {
        // SAFETY: `cursor` came from this library.
        unsafe { (self.is_cursor_definition)(cursor) != 0 }
    }

    /// What a `typedef` cursor is a typedef of.
    pub(crate) fn typedef_underlying_type(&self, cursor: CxCursor) -> CxType {
        // SAFETY: `cursor` came from this library.
        unsafe { (self.get_typedef_decl_underlying_type)(cursor) }
    }

    /// The integer type an `enum` cursor is represented as.
    pub(crate) fn enum_integer_type(&self, cursor: CxCursor) -> CxType {
        // SAFETY: `cursor` came from this library.
        unsafe { (self.get_enum_decl_integer_type)(cursor) }
    }

    /// The value an enumerator declares.
    pub(crate) fn enum_constant_value(&self, cursor: CxCursor) -> i64 {
        // SAFETY: `cursor` came from this library.
        unsafe { (self.get_enum_constant_decl_value)(cursor) }
    }

    /// How a type is written in C.
    pub(crate) fn type_spelling(&self, ty: CxType) -> String {
        // SAFETY: `ty` came from this library.
        self.take_string(unsafe { (self.get_type_spelling)(ty) })
    }

    /// The cursor declaring a record, enum, or typedef type.
    pub(crate) fn type_declaration(&self, ty: CxType) -> CxCursor {
        // SAFETY: `ty` came from this library.
        unsafe { (self.get_type_declaration)(ty) }
    }

    /// The type with every typedef and elaboration stripped.
    pub(crate) fn canonical_type(&self, ty: CxType) -> CxType {
        // SAFETY: `ty` came from this library.
        unsafe { (self.get_canonical_type)(ty) }
    }

    /// What a pointer type points at.
    pub(crate) fn pointee_type(&self, ty: CxType) -> CxType {
        // SAFETY: `ty` came from this library.
        unsafe { (self.get_pointee_type)(ty) }
    }

    /// What an array type holds.
    pub(crate) fn array_element_type(&self, ty: CxType) -> CxType {
        // SAFETY: `ty` came from this library.
        unsafe { (self.get_array_element_type)(ty) }
    }

    /// How many elements a constant-sized array type holds.
    pub(crate) fn array_size(&self, ty: CxType) -> i64 {
        // SAFETY: `ty` came from this library.
        unsafe { (self.get_array_size)(ty) }
    }

    /// What a function type returns.
    pub(crate) fn result_type(&self, ty: CxType) -> CxType {
        // SAFETY: `ty` came from this library.
        unsafe { (self.get_result_type)(ty) }
    }

    /// How many parameters a function type declares.
    pub(crate) fn num_arg_types(&self, ty: CxType) -> i32 {
        // SAFETY: `ty` came from this library.
        unsafe { (self.get_num_arg_types)(ty) }
    }

    /// One parameter type of a function type.
    pub(crate) fn arg_type(&self, ty: CxType, index: u32) -> CxType {
        // SAFETY: `ty` came from this library; an out-of-range index is a
        // documented `CXType_Invalid` rather than an unsound read.
        unsafe { (self.get_arg_type)(ty, index) }
    }

    /// Whether a function type takes a trailing `...`.
    pub(crate) fn type_is_variadic(&self, ty: CxType) -> bool {
        // SAFETY: `ty` came from this library.
        unsafe { (self.is_function_type_variadic)(ty) != 0 }
    }

    /// Whether a type is `const`-qualified.
    pub(crate) fn type_is_const(&self, ty: CxType) -> bool {
        // SAFETY: `ty` came from this library.
        unsafe { (self.is_const_qualified_type)(ty) != 0 }
    }

    /// A type's size in bytes, or a negative libclang error code.
    pub(crate) fn type_size(&self, ty: CxType) -> i64 {
        // SAFETY: `ty` came from this library.
        unsafe { (self.type_get_size_of)(ty) }
    }
}
