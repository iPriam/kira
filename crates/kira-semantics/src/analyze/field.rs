//! Resolving a field on a type, and the diagnostics that resolution emits.
//!
//! Quiet and loud resolution sit together so the difference between them stays
//! one function apart: a speculative lookup must not emit, and a real one must.

use super::*;

impl<'a> Analyzer<'a> {
    /// Whether `base_ty` has a field named `name`, reporting nothing.
    ///
    /// For a diagnostic that needs to know, not for resolving one.
    pub(crate) fn resolve_field_quietly(&self, base_ty: Type, name: &str) -> bool {
        if self.as_function_type(base_ty).is_some() {
            return false;
        }
        matches!(base_ty, Type::Struct(id)
            if self
                .program.types.structs().get(id)
                .is_some_and(|def| def.field_index(name).is_some()))
    }

    /// Resolves `name` as a field of `base_ty`, returning its index and type.
    ///
    /// A field of a non-struct is reported once here; an `Error` base stays
    /// silent, because whatever produced it already spoke.
    pub(crate) fn resolve_field(
        &mut self,
        base_ty: Type,
        name: &str,
        span: Span,
    ) -> Option<(u32, Type)> {
        let Type::Struct(id) = base_ty else {
            if base_ty != Type::Error {
                self.emit(
                    span,
                    "KSEM090",
                    format!(
                        "type `{}` has no fields, so it has no field `{name}`",
                        self.type_name(base_ty)
                    ),
                );
            }
            return None;
        };
        // A function type is a struct only because that is how closures are
        // desugared. The oracle pins no member access on a function value, so
        // letting the ordinary field path run here would publish the
        // representation — `f.tag` — as invented surface.
        if self.as_function_type(base_ty).is_some() {
            self.emit(
                span,
                "KSEM136",
                format!(
                    "`{}` is a function; a function has no members, only a call",
                    self.type_name(base_ty)
                ),
            );
            return None;
        }
        let resolved = self
            .program
            .types
            .structs()
            .get(id)
            .and_then(|def| def.field_index(name).map(|index| (index, def)))
            .and_then(|(index, def)| def.field(index).map(|field| (index, field.ty)));
        if resolved.is_none() {
            self.emit(
                span,
                "KSEM091",
                format!("struct `{}` has no field `{name}`", self.type_name(base_ty)),
            );
        }
        resolved
    }

    /// The spelling of `ty` for a diagnostic.
    ///
    /// Owned rather than borrowed on purpose: a struct's name lives in
    /// `self.program` and an array's is built on demand, and holding a borrow
    /// across an `emit` — which needs `&mut self` — would not compile.
    pub(crate) fn type_name(&self, ty: Type) -> String {
        self.program.types.type_name(ty)
    }

    pub(crate) fn emit(&mut self, span: Span, code: &'static str, message: impl Into<String>) {
        let message = message.into();
        let file_span = FileSpan::new(self.source, span);
        let mut diagnostic = Diagnostic::single(
            Severity::Error,
            message.clone(),
            Label::primary(file_span, message),
        );
        diagnostic.code = Some(Code::known(code));
        diagnostic.phase = Some("semantics");
        self.diagnostics.push(diagnostic);
    }
}
