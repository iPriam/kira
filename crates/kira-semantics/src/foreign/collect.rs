//! Collecting `@FFI` declarations and refusing the ones that cannot cross.
//!
//! The first half of the seam: what a program *declared*, checked before any of
//! it is mapped to a foreign type. A shape refused here never reaches lowering.

use super::*;

/// Parsed fields of one callable foreign declaration.
struct ForeignFields {
    library: String,
    symbol: String,
    retains: Vec<(String, Span)>,
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
            let annotation = function
                .foreign
                .as_ref()
                .map_or("@FFI.Extern", |mark| mark.kind.annotation());
            if self.sig_index.contains_key(&name) {
                self.emit(
                    function.name_span,
                    "KSEM184",
                    format!(
                        "`{name}` is already defined as a function: an `{annotation}` \
                         name shares the call namespace, so it cannot repeat one"
                    ),
                );
                continue;
            }
            if self.foreign_index.contains_key(&name) {
                self.emit(
                    function.name_span,
                    "KSEM185",
                    format!("`{annotation}` function `{name}` is already declared"),
                );
                continue;
            }
            let id = ForeignId(self.program.foreign.len() as u32);
            self.foreign_index.insert(name, id);
            self.program.foreign.push(hir_foreign);
        }
    }

    /// Validates one bodyless foreign declaration, returning its row when every
    /// check passes and `None` — with diagnostics emitted — when any fails.
    ///
    /// The checks run unconditionally so an author sees every mistake at once,
    /// not one per rebuild. The annotation check is shared because it is the same
    /// contradiction either way — a foreign symbol and a system call are both
    /// called rather than run as an entrypoint, and neither is a Kira export —
    /// and what a `@FFI.Syscall` block and signature must satisfy after that is
    /// [`crate::syscall`]'s.
    pub(super) fn validate_foreign(
        &mut self,
        function: &Function,
        name: &str,
    ) -> Option<HirForeign> {
        let mark = function.foreign.as_ref()?;
        let annotations_ok = self.check_foreign_annotations(function, mark);
        if mark.kind == ForeignKind::Syscall {
            return self.validate_syscall(function, name, mark, annotations_ok);
        }
        let fields = self.parse_foreign_fields(mark);
        if mark.kind == ForeignKind::Address && !self.check_address_signature(function) {
            return None;
        }
        let retained = fields
            .as_ref()
            .map(|fields| self.check_retained_params(function, &fields.retains))
            .unwrap_or_default();
        let signature = self.map_foreign_signature(function);
        match (annotations_ok, fields, signature) {
            (true, Some(fields), Some(mapped)) => Some(HirForeign {
                kira_name: name.to_owned(),
                library: fields.library,
                symbol: fields.symbol,
                abi: match mark.kind {
                    ForeignKind::Address => ForeignAbi::CAddress,
                    _ => ForeignAbi::C,
                },
                signature: mapped.signature.with_retained(retained),
                param_wrappers: mapped.param_wrappers,
                param_pointees: mapped.param_pointees,
                result_pointee: mapped.result_pointee,
                result_wrapper: mapped.result_wrapper,
                name_span: function.name_span,
            }),
            _ => None,
        }
    }

    /// Refuses a bodyless foreign declaration that also carries an execution or
    /// export annotation, returning whether it was clean.
    ///
    /// Neither form is a Kira entrypoint or a Kira export, and neither runs on a
    /// chosen engine — a foreign symbol runs on the host and a system call runs
    /// in the kernel — so every one of these is a contradiction rather than a
    /// refinement. The message names whichever form was written, because a reader
    /// told about `@FFI.Extern` looks for a declaration they never wrote.
    pub(super) fn check_foreign_annotations(
        &mut self,
        function: &Function,
        mark: &ForeignMark,
    ) -> bool {
        let annotation = mark.kind.annotation();
        let outside = match mark.kind {
            ForeignKind::Extern => "a foreign symbol",
            ForeignKind::Address => "the address of a foreign symbol",
            ForeignKind::Syscall => "a system call",
        };
        let mut ok = true;
        if function.is_main || function.is_main_thread_lifecycle {
            self.emit(
                mark.span,
                "KSEM177",
                format!(
                    "an `{annotation}` function cannot also be an entrypoint: {outside} is called, \
                     not run as the program's lifecycle"
                ),
            );
            ok = false;
        }
        if function.is_main_thread {
            self.emit(
                mark.span,
                "KSEM177",
                format!(
                    "an `{annotation}` function cannot also be `@MainThread`: {outside} is called, not \
                     scheduled by Kira's main-thread event loop"
                ),
            );
            ok = false;
        }
        if let Some(engine) = function.execution.annotation() {
            self.emit(
                mark.span,
                "KSEM177",
                format!(
                    "an `{annotation}` function cannot also be `@{engine}`: {outside} does not run \
                     on a Kira execution engine"
                ),
            );
            ok = false;
        }
        if let Some(export) = function.export {
            self.emit(
                export.span,
                "KSEM177",
                format!(
                    "an `{annotation}` function cannot also be `@Export`: {outside} is imported \
                     into Kira, not exported from it"
                ),
            );
            ok = false;
        }
        ok
    }

    /// Reads the `library`, `symbol`, `abi`, and `retains` fields out of an
    /// `@FFI.Extern` block, returning the library and symbol names plus the
    /// parameters `retains:` named when every field is present, unique, known,
    /// and (for `abi`) `c`.
    fn parse_foreign_fields(&mut self, mark: &ForeignMark) -> Option<ForeignFields> {
        let mut library: Option<String> = None;
        let mut symbol: Option<String> = None;
        let mut abi: Option<(String, Span)> = None;
        let mut retains: Vec<(String, Span)> = Vec::new();
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
                // Repeatable by design: each occurrence names one parameter
                // the callee keeps pointers from past the call. Which names
                // exist is the declaration's business and checked against its
                // signature, not here.
                "retains" => {
                    retains.push((value, field.value_span));
                    continue;
                }
                _ => {
                    self.emit(
                        field.key_span,
                        "KSEM178",
                        format!(
                            "unknown `@FFI.Extern` field `{key}` (expected `library`, \
                             `symbol`, `abi`, or `retains`)"
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
            // A data symbol is not called, so it has no calling convention to
            // name. `@FFI.Address` therefore carries no `abi`, and demanding one
            // would be demanding an answer to a question the form does not ask.
            None if mark.kind == ForeignKind::Address => {}
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
            (true, Some(library), Some(symbol)) => Some(ForeignFields {
                library,
                symbol,
                retains,
            }),
            _ => None,
        }
    }

    /// Resolves the parameters `retains:` named to per-position flags.
    ///
    /// A name that matches no parameter is `KSEM285` and a parameter named
    /// twice is `KSEM286`; both report and drop the entry rather than guessing
    /// a position, because a wrong lifetime here is a use-after-free or a leak
    /// at the seam, not a style problem.
    fn check_retained_params(
        &mut self,
        function: &Function,
        retains: &[(String, Span)],
    ) -> Vec<bool> {
        let mut flags = vec![false; function.params.len()];
        for (name, span) in retains {
            let position = function
                .params
                .iter()
                .position(|param| self.interner.resolve(param.name) == name);
            match position {
                Some(index) if flags[index] => {
                    self.emit(
                        *span,
                        "KSEM286",
                        format!("`retains` names parameter `{name}` twice"),
                    );
                }
                Some(index) => flags[index] = true,
                None => {
                    self.emit(
                        *span,
                        "KSEM285",
                        format!(
                            "`retains` names `{name}`, which is not a parameter of `{}`",
                            self.interner.resolve(function.name)
                        ),
                    );
                }
            }
        }
        flags
    }

    /// An `@FFI.Address` function answers the address and takes nothing.
    ///
    /// A parameter would be an argument to a symbol that is never called, and a
    /// result other than `RawPtr` would be a claim about what is AT the address
    /// rather than what the address is -- which is the `@FFI.Pointer` family's
    /// question, asked through the pointer this hands back.
    pub(super) fn check_address_signature(&mut self, function: &Function) -> bool {
        let mut ok = true;
        if let Some(param) = function.params.first() {
            self.emit(
                param.name_span,
                "KSEM186",
                "an `@FFI.Address` function takes no parameters: it answers the address of a \
                 data symbol, which is never called",
            );
            ok = false;
        }
        match function.return_type {
            Some(_) => {}
            None => {
                self.emit(
                    function.name_span,
                    "KSEM187",
                    "an `@FFI.Address` function returns `RawPtr`: the address of the symbol",
                );
                ok = false;
            }
        }
        ok
    }

    /// Reports a missing required string field and clears the ok flag, or
    /// returns the value unchanged.
    pub(super) fn require_foreign_field(
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
}
