//! What one Kira type becomes at the seam, and what it may not become.
//!
//! Its own module because it is one decision made recursively — a struct's
//! answer is its fields' answers — and reading it beside the callers that
//! consume the result obscures that.

use super::*;

impl<'a> Analyzer<'a> {
    /// Maps a resolved [`Type`] to the [`ForeignType`] it crosses the seam as,
    /// reporting the refusal (with the supported replacement) when it has none.
    ///
    /// A resolved `Error` is silent: whatever produced it already spoke.
    pub(super) fn foreign_type_of(
        &mut self,
        ty: Type,
        span: Span,
        position: Position,
    ) -> Option<ForeignType> {
        match ty {
            Type::Error => None,
            // `Int` crosses as `int64_t` and `Float` as `double`, because that
            // is what they are: one spelling per 64-bit type, so a signature
            // naming one leaves no width unsaid. A narrower C type still names
            // its own — `I32`, `U8`, `F32`.
            Type::Int(spelling) => Some(int_foreign_type(spelling)),
            Type::Float(FloatSpelling::Plain) => Some(ForeignType::F64),
            Type::Float(FloatSpelling::F32) => Some(ForeignType::F32),
            Type::Bool => Some(ForeignType::Bool),
            // A distinct type crosses as the scalar it is. `foreign_seam_of`
            // already unwraps one before it reaches here; this arm answers the
            // direct callers so the mapping is total wherever it is asked.
            Type::Distinct(_) => {
                let representation = self.program.types.representation(ty);
                self.foreign_type_of(representation, span, position)
            }
            Type::Void => match position {
                Position::Result => Some(ForeignType::Void),
                Position::Param => {
                    self.emit(
                        span,
                        "KSEM182",
                        "`Void` cannot be a foreign parameter: it names no value to pass",
                    );
                    None
                }
            },
            Type::String => {
                self.emit(
                    span,
                    "KSEM182",
                    "`String` cannot cross the C seam directly: use `CString` for a \
                     borrowed C-string parameter",
                );
                None
            }
            // A C block reads at the seam as the pointer word its payload sits
            // at, which is what a materialized image or flattened array *is*
            // to the callee.
            Type::RawPtr | Type::ForeignPtr(_) | Type::CBlock => Some(ForeignType::RawPtr),
            // A task handle names a row in the running program's own task table,
            // so it means nothing outside it and never crosses the C seam.
            Type::Task(_) | Type::MainThreadTask(_) => {
                self.emit(
                    span,
                    "KSEM182",
                    "a task handle cannot cross the C seam: it names a row in this \
                     program's task table and means nothing outside it",
                );
                None
            }
            // A `CString` crosses in both directions, and in neither does Kira
            // hold C storage. Inbound the seam builds a transient C copy of the
            // caller's `String` for the one call; outbound it copies the bytes
            // the callee returns while its pointer is still good, so nothing is
            // ever freed on Kira's side and the question of who owns a returned
            // C string never arises. Kira sees a `String` either way.
            Type::CString => Some(ForeignType::CString),
            // A standalone `@FFI.Array` has no scalar foreign signature: C
            // decays it to a pointer at a parameter or result boundary. Its
            // diagnostic names the form and its valid representation.
            Type::Struct(id) if self.ffi_struct_kind(id).is_some_and(is_deferred_ffi) => {
                // The same guard as the match arm, spelled out so no `expect`
                // is needed: a deferred FFI struct always carries its kind.
                if let Some(kind) = self.ffi_struct_kind(id) {
                    self.emit_ffi_not_executable(kind, id, span);
                }
                None
            }
            // `Any` is refused on its own terms rather than as an aggregate: it
            // is one word at the seam, so the size is not what stops it. What
            // stops it is that C would have to read the value back out, and
            // nothing can — the type is opaque in that direction on both sides.
            Type::Any => {
                self.emit(
                    span,
                    "KSEM182",
                    "`Any` cannot cross the C seam: an erased value has no type for C \
                     to read it back as. Write the concrete type the value has.",
                );
                None
            }
            // A capture cell is refused for a reason of its own, not the
            // single-word one: it *is* one word, and that is exactly the
            // problem. It is shared mutable storage whose share count this
            // runtime owns, and C has no way to release a hold on one. It is
            // not surface, so this arm guards the desugar rather than anything
            // an author can write.
            Type::Cell(_) => {
                self.emit(
                    span,
                    "KSEM182",
                    "a captured `var` cannot cross the C seam: its storage is shared and \
                     counted by this runtime, and C has no way to release a hold on it",
                );
                None
            }
            // An array of seam scalars is a pointer to the elements, which the
            // seam writes out in C's widths at the call. A *result* has no such
            // reading: a C function answers a pointer, and nothing in that
            // answer says how many elements are behind it.
            Type::Array(id) if position == Position::Param => {
                match self
                    .program
                    .types
                    .arrays()
                    .element(id)
                    .and_then(scalar_foreign_type)
                {
                    Some(_) => Some(ForeignType::RawPtr),
                    None => {
                        self.emit(
                            span,
                            "KSEM182",
                            format!(
                                "`{}` cannot cross the C seam: its elements are not a type C has a width for",
                                self.type_name(ty)
                            ),
                        );
                        None
                    }
                }
            }
            Type::Array(_) => {
                self.emit(
                    span,
                    "KSEM182",
                    "an array cannot be a foreign result: a C function answers a pointer, and nothing in that answer says how many elements are behind it",
                );
                None
            }
            // A payload-less enum is an integer with named values, which is what
            // a C enum is. Its case's number is what crosses.
            Type::Enum(id) if self.enum_crosses_as_a_number(id) => Some(ForeignType::I32),
            Type::Struct(_) | Type::Enum(_) | Type::NativeState(_) => {
                self.emit(
                    span,
                    "KSEM182",
                    format!(
                        "`{}` cannot cross the C seam: an aggregate has no single-word \
                         C representation",
                        self.type_name(ty)
                    ),
                );
                None
            }
        }
    }
}
