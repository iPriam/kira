//! The signature table: every function's resolved shape, collected before any
//! body is checked so calls type-check regardless of declaration order.
//!
//! Split out of [`super`] on the file-size ladder. One job lives here: turning
//! declarations into [`FuncSig`] rows and answering questions about them —
//! lookup by name, parameter shapes, and the declaration site a call links to.

use kira_core::Symbol;
use kira_semantics_model::hir::{FuncId, HirExpr, HirExprId};
use kira_semantics_model::{OwnershipMode, Type};
use kira_source::{FileSpan, SourceId, Span};

use super::{Analyzer, Callable, FieldDefault};

/// The signature of a user function, resolved before bodies are checked so
/// calls can be type-checked regardless of declaration order.
pub(crate) struct FuncSig {
    pub(crate) name: String,
    pub(crate) params: Vec<Type>,
    /// The written name of each parameter, positionally aligned with `params`,
    /// so a labeled call can bind an argument to the parameter it names.
    ///
    /// A method's receiver occupies slot 0 and has no written name, so its
    /// entry is `None` — a receiver slot can never be named by a label.
    pub(crate) param_names: Vec<Option<Symbol>>,
    /// How each parameter takes its argument, positionally aligned with
    /// `params`.
    ///
    /// A method's receiver occupies slot 0 of both, and takes
    /// [`OwnershipMode::BorrowRead`]: calling `p.sum()` does not consume `p`.
    pub(crate) param_ownership: Vec<OwnershipMode>,
    pub(crate) return_type: Type,
    pub(crate) name_span: Span,
    /// The file the declaration was written in, so a call site in any file
    /// can link to it.
    pub(crate) source: SourceId,
    pub(crate) is_main: bool,
}

impl<'a> Analyzer<'a> {
    pub(crate) fn collect_signatures(&mut self, callables: &[Callable<'a>]) {
        let mut main_seen = false;
        for callable in callables {
            let function = callable.function;
            // A signature's types are written in the file the function was, so
            // they resolve against that file's imports.
            self.source = callable.source;
            let name = self.callable_name(*callable);
            // A method's receiver is parameter 0, so its signature carries the
            // struct type ahead of what was written.
            let mut params: Vec<Type> = callable.receiver.map(Type::Struct).into_iter().collect();
            params.extend(
                function
                    .params
                    .iter()
                    .map(|param| self.resolve_type_ref(param.ty)),
            );
            // A receiver slot carries no written name, so a label can never
            // bind to it; the written parameters follow it in order.
            let mut param_names: Vec<Option<Symbol>> =
                callable.receiver.map(|_| None).into_iter().collect();
            param_names.extend(function.params.iter().map(|param| Some(param.name)));
            // A method's receiver borrows: `p.sum()` reads `p` and leaves it
            // usable. The oracle says the same — an unannotated receiver is
            // `borrow_read` — so a method call never demands `move`.
            let mut param_ownership: Vec<OwnershipMode> = callable
                .receiver
                .map(|_| OwnershipMode::BorrowRead)
                .into_iter()
                .collect();
            for param in &function.params {
                param_ownership.push(self.check_param_ownership(param));
            }
            // Defaults align with `params`, receiver included. A receiver slot
            // never has one; each written parameter carries its default as
            // unresolved syntax bound to this file, resolved once below.
            let mut param_defaults: Vec<Option<FieldDefault>> =
                callable.receiver.map(|_| None).into_iter().collect();
            param_defaults.extend(function.params.iter().map(|param| {
                param
                    .default
                    .map(|syntax| FieldDefault::new(syntax, callable.source))
            }));
            let return_type = match &function.return_type {
                Some(type_ref) => self.resolve_type_ref(*type_ref),
                None => Type::Void,
            };
            let id = FuncId(self.sigs.len() as u32);
            if self.sig_index.contains_key(&name) {
                self.emit(
                    function.name_span,
                    "KSEM003",
                    match callable.receiver {
                        Some(_) => format!("`{name}` is already defined"),
                        None => format!("function `{name}` is already defined"),
                    },
                );
            } else {
                self.sig_index.insert(name.clone(), id);
            }
            let is_main = function.is_main;
            if is_main && main_seen {
                self.emit(
                    function.name_span,
                    "KSEM010",
                    "a program may declare only one `@Main` function",
                );
            }
            main_seen = main_seen || is_main;
            self.sigs.push(FuncSig {
                name,
                params,
                param_names,
                param_ownership,
                return_type,
                name_span: function.name_span,
                source: callable.source,
                is_main,
            });
            // Pushed in lockstep with `sigs` so a `FuncId` indexes both. A
            // synthesized function reserves a later id with no row here, so the
            // default lookup returns `None` for it — never a panic.
            self.param_defaults.push(param_defaults);
        }
    }

    /// The mode a parameter declares, reporting the one mode this port does
    /// not implement.
    ///
    /// `borrow mut` is the single ownership mode that is **observable at run
    /// time**: a callee writing through it must change the caller's binding.
    /// Every other mode reduces to the deep copy the runtime already does, so
    /// it lands as a pure static check. Accepting `borrow mut` without the
    /// by-reference calling convention would not be an incomplete feature —
    /// it would silently compute wrong answers, because the callee would
    /// mutate a copy and the caller would never see the write.
    ///
    /// So it is refused with a typed error until the backends carry it,
    /// following the oracle's own precedent for a reserved-but-unimplemented
    /// mode (`copy` of a non-trivial value is `KSEM116` there for exactly this
    /// reason). `KSEM112` is the free code in the ownership band.
    pub(crate) fn check_param_ownership(
        &mut self,
        param: &kira_syntax_model::ast::Param,
    ) -> OwnershipMode {
        if param.ownership == OwnershipMode::BorrowMut {
            let span = param.ownership_span.unwrap_or(param.span);
            self.emit(
                span,
                "KSEM112",
                "Kira parsed `borrow mut`, but a mutable borrow is not implemented yet: \
                 the callee would write to a copy the caller never sees. Take the value \
                 with `move` and return the updated one, or use `borrow` to read it.",
            );
        }
        // The mode is returned unchanged even when refused. Rewriting it to
        // something implementable would make the body and the call sites check
        // against a signature nobody wrote — a `borrow mut` body writing to
        // its parameter would collect a spurious "cannot assign" on top of the
        // real problem. The program is already rejected; every other
        // diagnostic it collects should still be about what it said.
        param.ownership
    }

    /// Looks up a signature by name (for call resolution).
    pub(crate) fn lookup_function(&self, name: &str) -> Option<(FuncId, &[Type], Type)> {
        let id = *self.sig_index.get(name)?;
        let sig = &self.sigs[id.0 as usize];
        Some((id, &sig.params, sig.return_type))
    }

    /// Links a call site to the declaration of the function it resolved to.
    ///
    /// A synthesized function — a lifted closure, a dispatcher — has no
    /// written declaration, and its reserved id has no signature row yet;
    /// bounds-checking against the collected signatures is what keeps those
    /// from linking.
    pub(crate) fn link_function(&mut self, id: FuncId, reference: Span) {
        if let Some(sig) = self.sigs.get(id.0 as usize) {
            let definition = FileSpan::new(sig.source, sig.name_span);
            self.link(reference, definition);
        }
    }

    /// The ownership mode each parameter of `id` declares, receiver included.
    pub(crate) fn param_ownership(&self, id: FuncId) -> Vec<OwnershipMode> {
        self.sigs[id.0 as usize].param_ownership.clone()
    }

    /// The written name of each parameter of `id`, receiver included (`None`).
    ///
    /// A labeled call binds arguments to these names.
    pub(crate) fn param_names(&self, id: FuncId) -> Vec<Option<Symbol>> {
        self.sigs[id.0 as usize].param_names.clone()
    }

    /// The resolved type of each parameter of `id`, receiver included.
    ///
    /// Reading these beats re-resolving the written [`TypeRef`]: resolution
    /// reports an unknown name every time it runs, so a second pass over an
    /// already-resolved signature would report the same unknown type twice.
    ///
    /// [`TypeRef`]: kira_syntax_model::ast::TypeRef
    pub(crate) fn param_types(&self, id: FuncId) -> Vec<Type> {
        self.sigs[id.0 as usize].params.clone()
    }

    /// The resolved return type of `id`, `Void` when none was written.
    ///
    /// Read rather than re-resolved, for the reason [`Self::param_types`]
    /// gives.
    pub(crate) fn signature_return_type(&self, id: FuncId) -> Type {
        self.sigs[id.0 as usize].return_type
    }

    /// The default recorded for parameter `slot` of `id`, if one was written.
    pub(crate) fn param_default(&self, id: FuncId, slot: usize) -> Option<FieldDefault> {
        self.param_defaults
            .get(id.0 as usize)
            .and_then(|defaults| defaults.get(slot))
            .copied()
            .flatten()
    }

    /// Resolves every declared parameter default once, each in its declaring
    /// file, after signatures exist so a default may call any function.
    pub(crate) fn resolve_param_defaults(&mut self) {
        for id in 0..self.sigs.len() as u32 {
            let count = self.param_defaults.get(id as usize).map_or(0, Vec::len);
            for slot in 0..count {
                self.resolve_param_default(FuncId(id), slot);
            }
        }
    }

    /// Returns one resolved parameter default, resolving it in declaration
    /// scope on first use — a call that omits the argument may reach it before
    /// the outer pass does.
    ///
    /// The expression is analyzed in an empty local scope against the
    /// parameter's declared type, exactly as a field default is, so it can see
    /// neither the caller's locals nor the callee's other parameters. The
    /// resolved HIR is cached and shared by every call that omits the argument.
    pub(crate) fn resolve_param_default(&mut self, id: FuncId, slot: usize) -> Option<HirExprId> {
        let default = self.param_default(id, slot)?;
        if let Some(resolved) = default.resolved {
            return Some(resolved);
        }

        let key = (id.0, slot as u32);
        if !self.resolving_param_defaults.insert(key) {
            let previous_source = self.source;
            self.source = default.source;
            self.emit(
                self.tree.expr(default.syntax).span(),
                "KSEM240",
                "parameter defaults fill each other through the call graph and have no finite value",
            );
            self.source = previous_source;
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }

        let expected = self.param_types(id).get(slot).copied();
        let previous_source = self.source;
        self.source = default.source;
        let resolved = self.analyze_default(default.syntax, expected);
        self.source = previous_source;
        self.resolving_param_defaults.remove(&key);
        if let Some(target) = self
            .param_defaults
            .get_mut(id.0 as usize)
            .and_then(|defaults| defaults.get_mut(slot))
            .and_then(Option::as_mut)
        {
            target.resolved = Some(resolved);
        }
        Some(resolved)
    }
}
