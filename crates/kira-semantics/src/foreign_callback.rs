//! Passing a Kira function to C: what an `@FFI.Callback`-typed position accepts,
//! and what it records.
//!
//! A `@FFI.Callback` declares a C function pointer's signature. Naming a Kira
//! function where one is expected records a **callback entry** — the function
//! and the exact-width C signature it will be entered with — and the value
//! becomes the address of the entry thunk the backend generates for that entry.
//! Which is why the check here is not "are these types compatible" but "is this
//! function callable through *this* C signature": the thunk is generated from
//! the callback's declaration, and the function it enters has to be the one that
//! declaration describes.
//!
//! # Why the function is named rather than called
//!
//! Kira has no function type, so `Hooks { add: adder }` is not an expression
//! that evaluates to a function and is then converted. The name is recognized in
//! the one position that gives it a meaning — a slot whose type is a callback —
//! and anywhere else it is still an undefined name. That keeps a function name
//! from silently becoming a value in positions the language does not have one.

use kira_runtime_abi::{ForeignCallback, ForeignSignature, ForeignType, ForeignTypeSpec};
use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_source::Span;

use crate::analyze::Analyzer;
use crate::ffi_types::FfiStructKind;

/// Which side of a callback signature a type sits on.
///
/// The two differ on exactly one type: a `String` parameter carries a
/// `const char*` the thunk copies in, and a `String` result has nowhere to hand
/// C storage from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallbackPosition {
    /// A parameter of the callback.
    Param,
    /// The callback's result.
    Result,
}

impl Analyzer<'_> {
    /// The callback value for `name` when `expected` is a `@FFI.Callback` type
    /// and `name` is a top-level function, or `None` when this is not that
    /// position.
    ///
    /// `None` means "not a callback here", never "a callback that failed": a
    /// function whose signature does not fit is refused with its own diagnostic
    /// and still yields a value, so the surrounding literal reports nothing
    /// further.
    pub(crate) fn foreign_callback_value(
        &mut self,
        name: &str,
        expected: Option<Type>,
        span: Span,
    ) -> Option<HirExprId> {
        let Some(Type::Struct(callback)) = expected else {
            return None;
        };
        if self.ffi_struct_kind(callback) != Some(FfiStructKind::Callback) {
            return None;
        }
        let (function, params, result) = self.lookup_function(name)?;
        let params = params.to_vec();
        // A binding file declares hundreds of callbacks it never fills, many of
        // them naming C types nothing has defined yet. So a signature the seam
        // cannot carry is not reported where it is *declared* — it is reported
        // here, where a Kira function is actually being handed to C.
        let Some(signature) = self.ffi_callback_signatures.get(&callback).cloned() else {
            let callback_name = self.type_name(Type::Struct(callback));
            self.emit(
                span,
                "KSEM245",
                format!(
                    "`{callback_name}` declares a signature that cannot cross the C seam, so                      `{name}` cannot be passed as one: a callback carries fixed-width scalars,                      `Bool`, and `RawPtr`, and returns one of those or nothing"
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        };

        Some(
            match self.callback_signature_fits(&signature, &params, result) {
                Ok(()) => {
                    let id = self.record_foreign_callback(function.0, signature);
                    let pointer = self
                        .program
                        .exprs
                        .alloc(HirExpr::ForeignCallbackPtr { callback: id });
                    self.program.exprs.alloc(HirExpr::StructNew {
                        struct_id: callback,
                        fields: vec![pointer],
                    })
                }
                Err(reason) => {
                    let callback_name = self.type_name(Type::Struct(callback));
                    self.emit(
                        span,
                        "KSEM246",
                        format!("`{name}` cannot be called as `{callback_name}`: {reason}"),
                    );
                    self.program.exprs.alloc(HirExpr::Error)
                }
            },
        )
    }

    /// Whether a Kira function of these parameter and result types can be
    /// entered through `signature`, or why not.
    fn callback_signature_fits(
        &self,
        signature: &ForeignSignature,
        params: &[Type],
        result: Type,
    ) -> Result<(), String> {
        let declared = signature.parameters();
        if declared.len() != params.len() {
            return Err(format!(
                "the callback takes {} argument(s) and the function takes {}",
                declared.len(),
                params.len()
            ));
        }
        for (index, (spec, param)) in declared.iter().zip(params.iter()).enumerate() {
            let ForeignTypeSpec::Scalar(expected) = spec else {
                return Err(format!(
                    "argument {} crosses as an aggregate, which a callback does not carry yet",
                    index + 1
                ));
            };
            match self.callback_scalar_of(*param, CallbackPosition::Param) {
                Some(actual) if actual == *expected => {}
                _ => {
                    return Err(format!(
                        "argument {} is `{expected:?}` at the seam and the function declares `{}`",
                        index + 1,
                        self.type_name(*param),
                    ));
                }
            }
        }
        let ForeignTypeSpec::Scalar(expected_result) = signature.result() else {
            return Err(
                "the callback returns an aggregate, which a callback does not carry yet".to_owned(),
            );
        };
        if expected_result == ForeignType::Void {
            return if result == Type::Void {
                Ok(())
            } else {
                Err(format!(
                    "the callback returns nothing and the function returns `{}`",
                    self.type_name(result)
                ))
            };
        }
        match self.callback_scalar_of(result, CallbackPosition::Result) {
            Some(actual) if actual == expected_result => Ok(()),
            _ => Err(format!(
                "the callback returns `{expected_result:?}` at the seam and the function returns \
                 `{}`",
                self.type_name(result)
            )),
        }
    }

    /// The seam scalar a callback position carries `ty` as.
    ///
    /// The same exact-width rule the `@FFI.Extern` seam applies: a bare `Int` or
    /// `Float` has no C width, so it is not a callback parameter either.
    ///
    /// A **parameter** may additionally be a `String`, which carries a
    /// `const char*` C sees: the thunk copies the bytes on the way in, so the
    /// Kira function receives owned text and C keeps its storage. A *result*
    /// may not, and that asymmetry is the ownership question answering itself —
    /// C would have to be handed a pointer somebody frees, and a Kira `String`
    /// belongs to Kira.
    fn callback_scalar_of(&self, ty: Type, position: CallbackPosition) -> Option<ForeignType> {
        match (ty, position) {
            // Two spellings of one C position: `CString` is how the annotation
            // names it, `String` is how a Kira function taking it does.
            (Type::CString | Type::String, CallbackPosition::Param) => Some(ForeignType::CString),
            _ => crate::foreign::scalar_foreign_type(ty),
        }
    }

    /// The id of the callback entry for `function` and `signature`, adding it on
    /// first use.
    ///
    /// Naming the same function twice for the same signature is one entry and
    /// one generated thunk; the same function behind two different callback
    /// types is two, because the C signatures differ.
    fn record_foreign_callback(&mut self, function: u32, signature: ForeignSignature) -> u32 {
        let existing = self
            .program
            .foreign_callbacks
            .iter()
            .position(|entry| entry.function() == function && entry.signature() == &signature);
        match existing {
            Some(index) => index as u32,
            None => {
                let id = self.program.foreign_callbacks.len() as u32;
                self.program
                    .foreign_callbacks
                    .push(ForeignCallback::new(function, signature));
                id
            }
        }
    }

    /// The C signature an `@FFI.Callback` annotation declares, resolved once
    /// when the type is declared — quietly.
    ///
    /// `None` when any position cannot cross, and **nothing is reported**: a
    /// generated binding declares every callback its headers name, including
    /// ones whose types nothing has defined yet, and none of that is a problem
    /// until a Kira function is handed to one. The refusal belongs at that site,
    /// which is where [`Analyzer::foreign_callback_value`] puts it. Any
    /// diagnostic the resolution itself produced is rolled back for the same
    /// reason.
    pub(crate) fn resolve_callback_signature(
        &mut self,
        params: &[kira_syntax_model::ast::TypeRefId],
        result: Option<kira_syntax_model::ast::TypeRefId>,
    ) -> Option<ForeignSignature> {
        let quiet = self.diagnostics.len();
        let signature = self.resolve_callback_signature_inner(params, result);
        if signature.is_none() {
            self.diagnostics.truncate(quiet);
        }
        signature
    }

    /// The resolution itself; see [`Analyzer::resolve_callback_signature`] for
    /// why its diagnostics are conditional.
    fn resolve_callback_signature_inner(
        &mut self,
        params: &[kira_syntax_model::ast::TypeRefId],
        result: Option<kira_syntax_model::ast::TypeRefId>,
    ) -> Option<ForeignSignature> {
        // A callback annotation *is* a C signature, so its types resolve as one:
        // `CString` names a `const char*` here exactly as it does in an
        // `@FFI.Extern`, rather than becoming `Error` and taking the whole
        // signature with it.
        let outer_foreign = self.in_foreign_signature;
        self.in_foreign_signature = true;
        let resolved: Vec<Type> = params.iter().map(|&p| self.resolve_type_ref(p)).collect();
        let written_result = result.map(|written| self.resolve_type_ref(written));
        self.in_foreign_signature = outer_foreign;

        let mut specs = Vec::with_capacity(params.len());
        for ty in resolved {
            specs.push(ForeignTypeSpec::Scalar(
                self.callback_scalar_of(ty, CallbackPosition::Param)?,
            ));
        }
        let result = match written_result {
            None => ForeignType::Void,
            Some(Type::Void) => ForeignType::Void,
            Some(ty) => self.callback_scalar_of(ty, CallbackPosition::Result)?,
        };
        Some(ForeignSignature::new(
            specs,
            ForeignTypeSpec::Scalar(result),
        ))
    }
}
