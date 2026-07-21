//! The `@FFI.Extern` seam: turning a bodyless foreign declaration into a
//! validated [`HirForeign`] row, and type-checking a call to one.
//!
//! # Why the checks live here
//!
//! Whether a type can cross the C seam, whether an ABI is supported, and
//! whether the annotation block is well-formed all have the same answer on
//! every backend — the seam is a property of the declaration, not of the engine
//! that binds the symbol. Putting the checks above the backend split, beside
//! [`crate::exports`], is what keeps three engines from each growing their own
//! opinion of what a foreign call is.
//!
//! A refused extern is never recorded: [`HirProgram::foreign`] only ever holds
//! signatures the frontend accepted, so a backend binds against a contract it
//! can trust. A call resolves to [`Callee::Foreign`] by name, exactly as a user
//! call resolves to [`Callee::User`], and the argument coercion `String ->
//! CString` is the one implicit conversion the seam allows.

use kira_runtime_abi::{ForeignAbi, ForeignSignature, ForeignType};
use kira_semantics_model::hir::{Callee, ForeignId, HirExpr, HirExprId, HirForeign};
use kira_semantics_model::{FloatSpelling, IntSpelling, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{ForeignMark, Function, Item, Param, TypeRef};

use crate::analyze::{Analyzer, FnCtx};
use crate::ffi_types::FfiStructKind;

/// Whether a `@FFI.*` struct kind is one whose runtime behavior is not yet
/// executable — an array or a callback, but not a C-layout struct.
fn is_deferred_ffi(kind: FfiStructKind) -> bool {
    matches!(kind, FfiStructKind::Array | FfiStructKind::Callback)
}

/// Whether a foreign type sits in a parameter or the result position.
///
/// The two positions differ on exactly two types: `Void` is a legal result but
/// not a legal parameter, and `CString` is a legal parameter but not a legal
/// result. Everything else maps the same way in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// A parameter of the foreign function.
    Param,
    /// The result of the foreign function.
    Result,
}

impl<'a> Analyzer<'a> {
    /// Walks every `@FFI.Extern` declaration, validates it, and records the
    /// ones that pass in [`HirProgram::foreign`].
    ///
    /// Runs after signatures are collected — a foreign name may not collide
    /// with a user function's, and the collision check reads the signature
    /// index — and before any body, so a call in a body resolves to
    /// [`Callee::Foreign`].
    pub(crate) fn collect_foreign(&mut self) {
        // Collect the declarations first so the mutable-emitting loop does not
        // borrow the tree at the same time. The references are `'a`, tied to the
        // tree rather than to `self`, so they outlive each `&mut self` call.
        let foreigns: Vec<(SourceId, &'a Function)> = self
            .tree
            .items_with_source()
            .filter_map(|(source, item)| match item {
                Item::Function(function) if function.foreign.is_some() => Some((source, function)),
                _ => None,
            })
            .collect();
        for (source, function) in foreigns {
            self.source = source;
            let name = self.interner.resolve(function.name).to_owned();
            let Some(hir_foreign) = self.validate_foreign(function, &name) else {
                continue;
            };
            // A foreign name shares the call namespace with user functions, so a
            // clash would make one call name resolve to two callees. Both
            // clashes are refused here rather than recorded.
            if self.sig_index.contains_key(&name) {
                self.emit(
                    function.name_span,
                    "KSEM184",
                    format!(
                        "`{name}` is already defined as a function: an `@FFI.Extern` \
                         name shares the call namespace, so it cannot repeat one"
                    ),
                );
                continue;
            }
            if self.foreign_index.contains_key(&name) {
                self.emit(
                    function.name_span,
                    "KSEM185",
                    format!("`@FFI.Extern` function `{name}` is already declared"),
                );
                continue;
            }
            let id = ForeignId(self.program.foreign.len() as u32);
            self.foreign_index.insert(name, id);
            self.program.foreign.push(hir_foreign);
        }
    }

    /// Validates one `@FFI.Extern` declaration, returning its row when every
    /// check passes and `None` — with diagnostics emitted — when any fails.
    ///
    /// The three checks run unconditionally so an author sees every mistake at
    /// once, not one per rebuild.
    fn validate_foreign(&mut self, function: &Function, name: &str) -> Option<HirForeign> {
        let mark = function.foreign.as_ref()?;
        let annotations_ok = self.check_foreign_annotations(function, mark);
        let fields = self.parse_foreign_fields(mark);
        let signature = self.map_foreign_signature(function);
        match (annotations_ok, fields, signature) {
            (true, Some((library, symbol)), Some(signature)) => Some(HirForeign {
                kira_name: name.to_owned(),
                library,
                symbol,
                abi: ForeignAbi::C,
                signature,
                name_span: function.name_span,
            }),
            _ => None,
        }
    }

    /// Refuses an `@FFI.Extern` that also carries an execution or export
    /// annotation, returning whether it was clean.
    ///
    /// A foreign symbol is neither a Kira entrypoint nor a Kira export, and it
    /// runs on the host rather than on a chosen engine, so every one of these is
    /// a contradiction rather than a refinement.
    fn check_foreign_annotations(&mut self, function: &Function, mark: &ForeignMark) -> bool {
        let mut ok = true;
        if function.is_main {
            self.emit(
                mark.span,
                "KSEM177",
                "an `@FFI.Extern` function cannot also be `@Main`: a foreign symbol \
                 is called, not run as the program's entrypoint",
            );
            ok = false;
        }
        if let Some(annotation) = function.execution.annotation() {
            self.emit(
                mark.span,
                "KSEM177",
                format!(
                    "an `@FFI.Extern` function cannot also be `@{annotation}`: a foreign \
                     symbol runs on the host, not on a Kira execution engine"
                ),
            );
            ok = false;
        }
        if let Some(export) = function.export {
            self.emit(
                export.span,
                "KSEM177",
                "an `@FFI.Extern` function cannot also be `@Export`: a foreign symbol \
                 is imported into Kira, not exported from it",
            );
            ok = false;
        }
        ok
    }

    /// Reads the `library`, `symbol`, and `abi` fields out of an `@FFI.Extern`
    /// block, returning the library and symbol names when every field is
    /// present, unique, known, and (for `abi`) `c`.
    fn parse_foreign_fields(&mut self, mark: &ForeignMark) -> Option<(String, String)> {
        let mut library: Option<String> = None;
        let mut symbol: Option<String> = None;
        let mut abi: Option<(String, Span)> = None;
        let mut ok = true;
        for field in &mark.fields {
            let key = self.interner.resolve(field.key).to_owned();
            let value = self.interner.resolve(field.value).to_owned();
            let slot = match key.as_str() {
                "library" => &mut library,
                "symbol" => &mut symbol,
                "abi" => {
                    if abi.is_some() {
                        self.emit(
                            field.key_span,
                            "KSEM179",
                            "`@FFI.Extern` field `abi` is set twice",
                        );
                        ok = false;
                    } else {
                        abi = Some((value, field.value_span));
                    }
                    continue;
                }
                _ => {
                    self.emit(
                        field.key_span,
                        "KSEM178",
                        format!(
                            "unknown `@FFI.Extern` field `{key}` (expected `library`, \
                             `symbol`, or `abi`)"
                        ),
                    );
                    ok = false;
                    continue;
                }
            };
            if slot.is_some() {
                self.emit(
                    field.key_span,
                    "KSEM179",
                    format!("`@FFI.Extern` field `{key}` is set twice"),
                );
                ok = false;
                continue;
            }
            *slot = Some(value);
        }
        let library = self.require_foreign_field(library, "library", mark.block_span, &mut ok);
        let symbol = self.require_foreign_field(symbol, "symbol", mark.block_span, &mut ok);
        match abi {
            Some((value, span)) if value != "c" => {
                self.emit(
                    span,
                    "KSEM181",
                    format!("`@FFI.Extern` supports only the C ABI (`abi: c`), not `{value}`"),
                );
                ok = false;
            }
            None => {
                self.emit(
                    mark.block_span,
                    "KSEM180",
                    "`@FFI.Extern` block is missing its required `abi` field",
                );
                ok = false;
            }
            Some(_) => {}
        }
        match (ok, library, symbol) {
            (true, Some(library), Some(symbol)) => Some((library, symbol)),
            _ => None,
        }
    }

    /// Reports a missing required string field and clears the ok flag, or
    /// returns the value unchanged.
    fn require_foreign_field(
        &mut self,
        value: Option<String>,
        field: &str,
        block_span: Span,
        ok: &mut bool,
    ) -> Option<String> {
        if value.is_none() {
            self.emit(
                block_span,
                "KSEM180",
                format!("`@FFI.Extern` block is missing its required `{field}` field"),
            );
            *ok = false;
        }
        value
    }

    /// Maps a foreign declaration's written signature to a [`ForeignSignature`],
    /// or `None` when any parameter or the result cannot cross the seam.
    fn map_foreign_signature(&mut self, function: &Function) -> Option<ForeignSignature> {
        let mut params = Vec::with_capacity(function.params.len());
        let mut ok = true;
        for param in &function.params {
            match self.map_foreign_param(param) {
                Some(foreign_type) => params.push(foreign_type),
                None => ok = false,
            }
        }
        let result = self.map_foreign_result(function);
        match (ok, result) {
            (true, Some(result)) => Some(ForeignSignature::new(params, result)),
            _ => None,
        }
    }

    /// Maps one written parameter to a [`ForeignType`], reporting why it cannot
    /// cross when it cannot.
    fn map_foreign_param(&mut self, param: &Param) -> Option<ForeignType> {
        let span = self.tree.type_ref(param.ty).span();
        if let Some(()) = self.refuse_written_shape(param.ty, span) {
            return None;
        }
        let ty = self.resolve_foreign_type(param.ty);
        self.foreign_type_of(ty, span, Position::Param)
    }

    /// Maps the written result to a [`ForeignType`]; an absent result is
    /// [`ForeignType::Void`].
    fn map_foreign_result(&mut self, function: &Function) -> Option<ForeignType> {
        let Some(type_ref) = function.return_type else {
            return Some(ForeignType::Void);
        };
        let span = self.tree.type_ref(type_ref).span();
        if let Some(()) = self.refuse_written_shape(type_ref, span) {
            return None;
        }
        let ty = self.resolve_foreign_type(type_ref);
        self.foreign_type_of(ty, span, Position::Result)
    }

    /// Resolves a type inside a foreign signature, where `CString` is permitted
    /// to resolve without the seam-only refusal that guards every other
    /// position.
    fn resolve_foreign_type(&mut self, type_ref: kira_syntax_model::ast::TypeRefId) -> Type {
        self.in_foreign_signature = true;
        let ty = self.resolve_type_ref(type_ref);
        self.in_foreign_signature = false;
        ty
    }

    /// Refuses a written type whose *shape* the seam does not support —
    /// a function pointer, a generic, or an array — with a message precise to
    /// the shape. Returns `Some(())` when it refused.
    ///
    /// These are caught from the written [`TypeRef`] rather than the resolved
    /// [`Type`] because the shape names the fix: a callback and a generic have
    /// no resolved-type spelling to blame, and an array's message is clearer
    /// before it is interned into an anonymous row.
    fn refuse_written_shape(
        &mut self,
        type_ref: kira_syntax_model::ast::TypeRefId,
        span: Span,
    ) -> Option<()> {
        match self.tree.type_ref(type_ref) {
            TypeRef::Function { .. } => {
                self.emit(
                    span,
                    "KSEM182",
                    "a function pointer cannot cross the C seam: `@FFI.Extern` supports \
                     no callback parameter or result",
                );
                Some(())
            }
            TypeRef::Generic { .. } => {
                self.emit(
                    span,
                    "KSEM182",
                    "a generic type cannot cross the C seam: an `@FFI.Extern` signature \
                     names only fixed-width scalars, `Bool`, `RawPtr`, and `CString`",
                );
                Some(())
            }
            TypeRef::Array { .. } => {
                self.emit(
                    span,
                    "KSEM182",
                    "an array cannot cross the C seam: pass the elements through a \
                     `RawPtr` and a length instead",
                );
                Some(())
            }
            _ => None,
        }
    }

    /// Maps a resolved [`Type`] to the [`ForeignType`] it crosses the seam as,
    /// reporting the refusal (with the supported replacement) when it has none.
    ///
    /// A resolved `Error` is silent: whatever produced it already spoke.
    fn foreign_type_of(&mut self, ty: Type, span: Span, position: Position) -> Option<ForeignType> {
        match ty {
            Type::Error => None,
            Type::Int(IntSpelling::Plain) => {
                self.emit(
                    span,
                    "KSEM182",
                    "bare `Int` cannot cross the C seam: use a fixed-width integer like \
                     `I32` or `U64` so the C width is unambiguous",
                );
                None
            }
            Type::Int(spelling) => Some(int_foreign_type(spelling)),
            Type::Float(FloatSpelling::Plain) => {
                self.emit(
                    span,
                    "KSEM182",
                    "bare `Float` cannot cross the C seam: use `F32` or `F64` so the C \
                     width is unambiguous",
                );
                None
            }
            Type::Float(FloatSpelling::F32) => Some(ForeignType::F32),
            Type::Float(FloatSpelling::F64) => Some(ForeignType::F64),
            Type::Bool => Some(ForeignType::Bool),
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
            Type::RawPtr => Some(ForeignType::RawPtr),
            Type::CString => match position {
                Position::Param => Some(ForeignType::CString),
                Position::Result => {
                    self.emit(
                        span,
                        "KSEM182",
                        "a `CString` result is not supported: who frees a returned C \
                         string is unspecified. Return a `RawPtr` and a length instead",
                    );
                    None
                }
            },
            // A `@FFI.Callback`/`@FFI.Array` type at the seam is a declared but
            // not-yet-executable form, not a generic aggregate; its refusal
            // names the form so the fix is clear.
            Type::Struct(id) if self.ffi_struct_kind(id).is_some_and(is_deferred_ffi) => {
                let kind = self.ffi_struct_kind(id).expect("checked by the guard");
                self.emit_ffi_not_executable(kind, id, span);
                None
            }
            Type::Struct(_) | Type::Array(_) | Type::Enum(_) => {
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

    /// Whether `name` is a recorded foreign callable, for call resolution.
    pub(crate) fn foreign_named(&self, name: &str) -> Option<ForeignId> {
        self.foreign_index.get(name).copied()
    }

    /// Type-checks a call to a foreign function.
    ///
    /// Each argument is analyzed as an ordinary Kira value — a foreign call
    /// borrows, so nothing needs `move` — and checked against the parameter's
    /// [`ForeignType`]. The one implicit conversion is `String -> CString`: a
    /// `String` is accepted exactly where the parameter is `CString`, and
    /// nowhere else, and the caller keeps its `String`. The call's Kira result
    /// type is the foreign result mapped back.
    pub(crate) fn analyze_foreign_call(
        &mut self,
        ctx: &mut FnCtx,
        id: ForeignId,
        args: &[kira_syntax_model::ast::ExprId],
        span: Span,
    ) -> HirExprId {
        // Snapshot the signature so the argument loop's `&mut self` does not
        // overlap the borrow of `self.program.foreign`.
        let (params, result, name) = {
            let foreign = &self.program.foreign[id.0 as usize];
            (
                foreign.signature.parameters().to_vec(),
                foreign.signature.result(),
                foreign.kira_name.clone(),
            )
        };
        let arg_hirs: Vec<HirExprId> = args
            .iter()
            .map(|&arg| self.analyze_expr(ctx, arg))
            .collect();
        if arg_hirs.len() != params.len() {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{name}` takes {} argument(s), found {}",
                    params.len(),
                    arg_hirs.len()
                ),
            );
        } else {
            for (index, (&arg, &param)) in arg_hirs.iter().zip(params.iter()).enumerate() {
                let actual = self.program.expr(arg).type_of();
                if actual == Type::Error || foreign_arg_matches(actual, param) {
                    continue;
                }
                let expected = match param {
                    ForeignType::CString => "String".to_owned(),
                    other => self.type_name(kira_type_for_foreign(other)),
                };
                self.emit(
                    span,
                    "KSEM183",
                    format!(
                        "argument {} of `{name}` expects `{expected}`, found `{}`",
                        index + 1,
                        self.type_name(actual)
                    ),
                );
            }
        }
        let ty = kira_type_for_foreign(result);
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::Foreign(id),
            args: arg_hirs,
            ty,
            writeback: None,
        })
    }
}

/// The fixed-width [`ForeignType`] a signed/unsigned integer spelling crosses
/// as. The plain spelling is refused before it reaches here.
fn int_foreign_type(spelling: IntSpelling) -> ForeignType {
    match spelling {
        IntSpelling::Plain => ForeignType::I64,
        IntSpelling::I8 => ForeignType::I8,
        IntSpelling::I16 => ForeignType::I16,
        IntSpelling::I32 => ForeignType::I32,
        IntSpelling::I64 => ForeignType::I64,
        IntSpelling::U8 => ForeignType::U8,
        IntSpelling::U16 => ForeignType::U16,
        IntSpelling::U32 => ForeignType::U32,
        IntSpelling::U64 => ForeignType::U64,
    }
}

/// The Kira [`Type`] a foreign type maps back to — a call's result type, and
/// the type a non-`CString` argument must be assignable to.
///
/// `CString` maps to `String` here only for completeness: an argument to a
/// `CString` parameter is checked by the explicit `String -> CString` rule, not
/// through this map, and a `CString` never appears as a result.
fn kira_type_for_foreign(foreign_type: ForeignType) -> Type {
    match foreign_type {
        ForeignType::Void => Type::Void,
        ForeignType::I8 => Type::Int(IntSpelling::I8),
        ForeignType::I16 => Type::Int(IntSpelling::I16),
        ForeignType::I32 => Type::Int(IntSpelling::I32),
        ForeignType::I64 => Type::Int(IntSpelling::I64),
        ForeignType::U8 => Type::Int(IntSpelling::U8),
        ForeignType::U16 => Type::Int(IntSpelling::U16),
        ForeignType::U32 => Type::Int(IntSpelling::U32),
        ForeignType::U64 => Type::Int(IntSpelling::U64),
        ForeignType::Bool => Type::Bool,
        ForeignType::F32 => Type::Float(FloatSpelling::F32),
        ForeignType::F64 => Type::Float(FloatSpelling::F64),
        ForeignType::RawPtr => Type::RawPtr,
        ForeignType::CString => Type::String,
    }
}

/// Whether a Kira argument type is accepted for a foreign parameter type.
///
/// A `CString` parameter accepts a Kira `String` and nothing else — the single
/// explicit coercion. Every other parameter accepts a value assignable to the
/// Kira type it maps back to, so an integer literal (`Int`) reaches any fixed
/// width exactly as it does elsewhere.
fn foreign_arg_matches(actual: Type, param: ForeignType) -> bool {
    match param {
        ForeignType::CString => actual == Type::String,
        other => actual.assignable_to(kira_type_for_foreign(other)),
    }
}
