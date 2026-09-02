//! Analyzing a call across the seam: argument checking, lowering, and the
//! addresses a foreign callee is handed.
//!
//! Split from the declaration half because this is the only part that runs per
//! *call site* rather than per declaration.

use kira_semantics_model::hir::FieldOrder;
use super::*;

/// Borrowed signature context for lowering one foreign call's arguments.
struct ForeignArgShape<'a> {
    params: &'a [ForeignTypeSpec],
    wrappers: &'a [Option<StructId>],
    pointees: &'a [Option<kira_semantics_model::hir::ForeignPointee>],
    distincts: &'a [Option<Type>],
    retained: &'a [bool],
    name: &'a str,
    span: Span,
}

impl<'a> Analyzer<'a> {
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
        // overlap the borrow of `self.program.foreign`. The wrappers ride
        // alongside: a single-scalar-field handle struct crosses as its field's
        // scalar, so the Kira side reads that field out of an argument and
        // rebuilds the struct around the result.
        let (
            params,
            param_wrappers,
            param_pointees,
            param_distincts,
            result,
            result_pointee,
            result_wrapper,
            result_distinct,
            name,
        ) = {
            let foreign = &self.program.foreign[id.0 as usize];
            (
                foreign.signature.parameters().to_vec(),
                foreign.param_wrappers.clone(),
                foreign.param_pointees.clone(),
                foreign.param_distincts.clone(),
                foreign.signature.result(),
                foreign.result_pointee,
                foreign.result_wrapper,
                foreign.result_distinct,
                foreign.kira_name.clone(),
            )
        };
        let retained: Vec<bool> = (0..params.len())
            .map(|index| {
                self.program.foreign[id.0 as usize]
                    .signature
                    .is_retained(index)
            })
            .collect();
        // Each argument is analyzed against the Kira type its position expects,
        // so a bare function name lands where a `@FFI.Callback` parameter can
        // recognize it. Every other position ignores the expectation, exactly as
        // it does at an ordinary call.
        let arg_hirs: Vec<HirExprId> = args
            .iter()
            .enumerate()
            .map(|(index, &arg)| {
                let expected = param_wrappers
                    .get(index)
                    .copied()
                    .flatten()
                    .map(Type::Struct);
                let value = self.analyze_expr_expecting(ctx, arg, expected);
                // A `retains:` parameter consumes: the callee keeps pointers
                // into the argument's C storage past the call, so the caller
                // must give the value up — `move` written at the call site is
                // what makes the transfer visible, and the use-after-move
                // checker is what makes it safe.
                if retained.get(index).copied().unwrap_or(false) {
                    self.require_retained_move(ctx, arg, value, &name);
                }
                value
            })
            .collect();
        let seam_args = if arg_hirs.len() != params.len() {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{name}` takes {} argument(s), found {}",
                    params.len(),
                    arg_hirs.len()
                ),
            );
            arg_hirs
        } else {
            self.check_and_lower_foreign_args(
                &arg_hirs,
                ForeignArgShape {
                    params: &params,
                    wrappers: &param_wrappers,
                    pointees: &param_pointees,
                    distincts: &param_distincts,
                    retained: &retained,
                    name: &name,
                    span,
                },
            )
        };
        // An aggregate result *is* the struct on the Kira side — the seam carries
        // its C-layout bytes and hands back the whole value — so the call's own
        // type is the wrapper and nothing is rebuilt around it. A handle result
        // is the opposite: it crosses as its field's scalar, and the struct the
        // author wrote has to be put back together.
        let aggregate_result = result.aggregate().is_some();
        let call_type = match (aggregate_result, result_wrapper) {
            (true, Some(struct_id)) => Type::Struct(struct_id),
            // A `distinct` result is handed back as the type the declaration
            // wrote, not as the scalar it crossed as. Nothing is rebuilt around
            // the call — the value already *is* the representation — so this is
            // one type on the node and no instruction anywhere.
            _ => match (result_distinct, result_pointee) {
                (Some(declared), _) => declared,
                (None, Some(target)) => self.program.types.foreign_ptr_to(target),
                (None, None) => kira_type_for_spec(result),
            },
        };
        let call = self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::Foreign(id),
            args: seam_args,
            ty: call_type,
            writebacks: Vec::new(),
        });
        match result_wrapper {
            Some(struct_id) if !aggregate_result => self.program.exprs.alloc(HirExpr::StructNew {
                struct_id,
                fields: vec![call],
                order: FieldOrder::Declared,
            }),
            _ => call,
        }
    }

    /// Type-checks each foreign argument against its parameter and returns the
    /// values to hand the seam: an ordinary argument unchanged, and a
    /// single-scalar-field handle argument replaced by a read of its one field.
    fn check_and_lower_foreign_args(
        &mut self,
        arg_hirs: &[HirExprId],
        shape: ForeignArgShape<'_>,
    ) -> Vec<HirExprId> {
        let ForeignArgShape {
            params,
            wrappers: param_wrappers,
            pointees: param_pointees,
            distincts: param_distincts,
            retained,
            name,
            span,
        } = shape;
        let mut seam_args = Vec::with_capacity(arg_hirs.len());
        for (index, &arg) in arg_hirs.iter().enumerate() {
            let actual = self.program.expr(arg).type_of();
            // A `distinct` parameter is checked on Kira's terms and lowered on
            // C's: the argument must *be* that type — the representation
            // underneath does not reach it, which is the whole point — and what
            // crosses is the scalar, with no wrapper and no conversion.
            if let Some(declared) = param_distincts.get(index).copied().flatten() {
                if actual != Type::Error && !actual.assignable_to(declared) {
                    self.emit(
                        span,
                        "KSEM183",
                        format!(
                            "argument {} of `{name}` expects `{}`, found `{}`",
                            index + 1,
                            self.type_name(declared),
                            self.type_name(actual)
                        ),
                    );
                }
                let representation = self.program.types.representation(declared);
                seam_args.push(self.program.exprs.alloc(HirExpr::Distinct {
                    value: arg,
                    ty: representation,
                }));
                continue;
            }
            match param_wrappers[index] {
                Some(struct_id) => {
                    if actual != Type::Error && actual != Type::Struct(struct_id) {
                        self.emit(
                            span,
                            "KSEM183",
                            format!(
                                "argument {} of `{name}` expects `{}`, found `{}`",
                                index + 1,
                                self.type_name(Type::Struct(struct_id)),
                                self.type_name(actual)
                            ),
                        );
                    }
                    if params[index].aggregate().is_some() {
                        // A by-value aggregate crosses as the whole struct: the
                        // seam marshals its fields into C-layout bytes, so there
                        // is no single field to project.
                        seam_args.push(arg);
                    } else {
                        // The sole field's scalar is what crosses the seam.
                        seam_args.push(self.program.exprs.alloc(HirExpr::Field {
                            base: arg,
                            index: 0,
                            ty: kira_type_for_spec(params[index]),
                        }));
                    }
                }
                None => {
                    // A pointer parameter also accepts the struct it points at,
                    // and an `@FFI.Array` of that struct: the seam writes the
                    // C-layout image and passes its address, which is what
                    // `sapp_run(move desc)` means for one and what a
                    // `T const *items` argument means for several.
                    if let Some(pointee) = param_pointees[index]
                        && let Some(image) =
                            self.clayout_image_address(arg, pointee.struct_id, span)
                    {
                        seam_args.push(image);
                        continue;
                    }
                    // A payload-less enum crosses as its case's number, which
                    // is what a C enum reads as. The named type stays on the
                    // Kira side of the call rather than being mapped to an
                    // integer by hand at every site.
                    if let Type::Enum(_) = actual
                        && matches!(
                            params[index].scalar(),
                            Some(
                                ForeignType::I8
                                    | ForeignType::I16
                                    | ForeignType::I32
                                    | ForeignType::I64
                                    | ForeignType::U8
                                    | ForeignType::U16
                                    | ForeignType::U32
                                    | ForeignType::U64
                            )
                        )
                    {
                        seam_args.push(self.program.exprs.alloc(HirExpr::EnumTag { value: arg }));
                        continue;
                    }
                    // A pointer parameter also accepts an array of seam
                    // scalars: the seam writes the elements out as C's widths
                    // and passes the address of what it wrote. This is what the
                    // `RawPtr`-and-a-length shape a C API asks for looks like
                    // from Kira, without the caller building the buffer by hand.
                    if params[index].scalar() == Some(ForeignType::RawPtr)
                        && let Some(elements) = self.array_elements_address(arg)
                    {
                        seam_args.push(elements);
                        continue;
                    }
                    // A retained C string is owned storage rather than the
                    // ordinary call-scoped conversion. The foreign call moves
                    // this block into the retained registry after C receives
                    // its payload address.
                    if retained.get(index).copied().unwrap_or(false)
                        && params[index].scalar() == Some(ForeignType::CString)
                        && actual == Type::String
                    {
                        seam_args.push(self.program.exprs.alloc(HirExpr::CStringNew { text: arg }));
                        continue;
                    }
                    let param = params[index];
                    if actual != Type::Error && !foreign_arg_matches(actual, param) {
                        let expected = match param.scalar() {
                            Some(ForeignType::CString) => "String".to_owned(),
                            _ => self.type_name(kira_type_for_spec(param)),
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
                    seam_args.push(arg);
                }
            }
        }
        seam_args
    }

    /// The address of `value`'s elements written out at C's widths, when `value`
    /// is an array of seam scalars filling a pointer position.
    ///
    /// A pointer word is a pointer word wherever it is written, so this reads
    /// the same at an extern's `RawPtr` argument and at a C-layout struct's
    /// `RawPtr` member — the two places a `RawPtr`-and-a-length C API is
    /// reached from. `sg_range { ptr: values, size: … }` is the second one, and
    /// without it a descriptor holding a data pointer can only be built in C.
    ///
    /// Answers `None` when the value is not an array, or its elements are not
    /// scalars the seam can write, which leaves the position's ordinary type
    /// check to report it.
    pub(crate) fn array_elements_address(&mut self, value: HirExprId) -> Option<HirExprId> {
        let Type::Array(id) = self.program.expr(value).type_of() else {
            return None;
        };
        let element = self.program.types.arrays().element(id)?;
        let element = scalar_foreign_type(element)?;
        Some(
            self.program
                .exprs
                .alloc(HirExpr::ArrayElements { value, element }),
        )
    }

    /// The address of a C-layout image a pointer position accepts in place of
    /// the pointer itself.
    ///
    /// Two values fill a `T *`. The struct `T`, whose image the seam writes and
    /// whose address it passes — that is what `sapp_run(move desc)` means. And
    /// an `@FFI.Array` of `T`, which is the same image with an extent: a C array
    /// is its elements laid out end to end, so one row describes both and the
    /// address of the row is the address of element zero.
    ///
    /// The second is what makes a descriptor-shaped graphics API reachable.
    /// Almost none of them take one item; they take `T const *items` beside an
    /// `itemCount` — vertex attributes, bind group entries, colour targets —
    /// and without a way to name several items' storage from Kira, that
    /// descriptor can only be built in a C helper.
    ///
    /// A shorter Kira array than the extent zero-fills the rest, which is the
    /// rule an `@FFI.Array` member already follows: the count the descriptor
    /// carries beside the pointer is what says how many C reads.
    ///
    /// A third value fills it: a struct that **begins** with `T`. The address of
    /// a struct is the address of its first member, which is the whole of how an
    /// extensible C API extends — `WGPUChainedStruct *nextInChain` pointed at a
    /// `WGPUSurfaceSourceWindowsHWND` whose first member is that chain, and
    /// Vulkan's `pNext` the same. Without it a caller can only reach an
    /// extension by redeclaring the base descriptor once per extension it wants.
    pub(crate) fn clayout_image_address(
        &mut self,
        value: HirExprId,
        pointee: StructId,
        span: Span,
    ) -> Option<HirExprId> {
        let Type::Struct(id) = self.program.expr(value).type_of() else {
            return None;
        };
        if id != pointee
            && self.ffi_array_element(id) != Some(Type::Struct(pointee))
            && !self.clayout_leads_with(id, pointee)
        {
            return None;
        }
        let aggregate = self.aggregate_seam_of(id, span)?;
        Some(
            self.program
                .exprs
                .alloc(HirExpr::CLayoutAddress { value, aggregate }),
        )
    }

    /// Whether C-layout `id` begins with a `pointee`, so the address of one is
    /// the address of the other.
    ///
    /// This is C's own rule, not a relaxation of it: a struct and its first
    /// member share an address, which is what every `pNext`/`nextInChain` cast
    /// in an extensible header relies on. Only the *first* member counts —
    /// anything later sits at a nonzero offset and is a different address.
    pub(super) fn clayout_leads_with(&self, id: StructId, pointee: StructId) -> bool {
        self.ffi_struct_kind(id) == Some(FfiStructKind::CLayout)
            && self
                .program
                .types
                .structs()
                .get(id)
                .and_then(|def| def.fields.first())
                .is_some_and(|field| field.ty == Type::Struct(pointee))
    }

    /// The value a C-layout struct's pointer member is filled with, when the
    /// literal wrote storage this side owns rather than a pointer C handed out.
    ///
    /// A member typed as an `@FFI.Pointer` is a pointer word exactly as a
    /// `RawPtr` member is, so both accept the same two fills: an array of seam
    /// scalars, and — where the pointer names a C-layout target — that struct
    /// or an `@FFI.Array` of it.
    pub(crate) fn foreign_pointer_fill(
        &mut self,
        value: HirExprId,
        member: Type,
        span: Span,
    ) -> Option<HirExprId> {
        if let Type::ForeignPtr(pointer) = member
            && let Some(target) = self.program.types.foreign_ptr_target(pointer)
            && let Some(image) = self.clayout_image_address(value, target, span)
        {
            return Some(image);
        }
        self.array_elements_address(value)
    }
}
