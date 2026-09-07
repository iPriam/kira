//! The signature table: every function's resolved shape, collected before any
//! body is checked so calls type-check regardless of declaration order.
//!
//! Split out of [`super`] on the file-size ladder. One job lives here: turning
//! declarations into [`FuncSig`] rows and answering questions about them —
//! lookup by name, parameter shapes, and the declaration site a call links to.

use kira_semantics_model::hir::{
    CallableSignature, ParamSignature, ReceiverSignature, ThreadAffinity,
};
use kira_semantics_model::hir::{FuncId, HirExpr, HirExprId};
use kira_semantics_model::ty::StructId;
use kira_semantics_model::{OwnershipMode, Type};
use kira_source::{FileSpan, SourceId, Span};

use super::{Analyzer, Callable, FieldDefault};

/// The signature of a user function, resolved before bodies are checked so
/// calls can be type-checked regardless of declaration order.
pub(crate) struct FuncSig {
    /// The name a call site writes, which several declarations may share.
    pub(crate) name: String,
    /// The name this declaration alone answers to in the compiled program.
    ///
    /// Equal to [`name`](Self::name) for a name declared once. An overloaded
    /// name gives every one of its declarations a mangled symbol, because a
    /// backend has one symbol per function and two overloads are two functions.
    pub(crate) symbol: String,
    pub(crate) params: Vec<Type>,
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
    pub(crate) is_main_thread_lifecycle: bool,
    pub(crate) is_main_thread: bool,
    /// The complete contract: receiver, per-parameter ownership and labels,
    /// defaults, result, `async`, and thread affinity. What every comparison
    /// of two callables reads, so none compares types alone.
    pub(crate) signature: CallableSignature,
}

impl<'a> Analyzer<'a> {
    pub(crate) fn collect_signatures(&mut self, callables: &[Callable<'a>]) {
        let mut main_seen = false;
        for callable in callables {
            let function = callable.function;
            let outer_bindings =
                std::mem::replace(&mut self.type_bindings, callable.type_bindings.clone());
            // A signature's types are written in the file the function was, so
            // they resolve against that file's imports.
            self.source = callable.source;
            let name = self.callable_name(callable);
            // A receiver names the value a method runs on, so a declaration
            // that runs on none has nothing for it to name. Reported here
            // rather than at the parse, because whether a declaration is a
            // method is decided by where it was written, not by how it reads.
            if let Some(receiver) = function.receiver
                && callable.receiver.is_none()
            {
                self.emit(
                    receiver.span,
                    "KSEM299",
                    format!(
                        "`{name}` is not a method, so it has no receiver to borrow: a `self` \
                         parameter belongs to a declaration written inside a type"
                    ),
                );
            }
            // A method's receiver is parameter 0, so its signature carries the
            // struct type ahead of what was written.
            let mut params: Vec<Type> = callable.receiver.into_iter().collect();
            // A specialized copy takes the subclass where the declaration wrote
            // the parent. This is the whole of the substitution: everything
            // downstream — the body, `self`-less method resolution inside it,
            // lowering — reads the signature rather than the syntax.
            let written: Vec<Type> = function
                .params
                .iter()
                .enumerate()
                .map(|(index, param)| {
                    match callable.specialize.iter().find(|(slot, _)| *slot == index) {
                        Some((_, class)) => Type::Struct(*class),
                        None => self.resolve_type_ref(param.ty),
                    }
                })
                .collect();
            params.extend(written);
            // A receiver slot carries no written name, so a label can never
            // bind to it; the written parameters follow it in order.
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
            // A `copy` parameter promises a second holder of every argument, so
            // its type has to be Copyable.
            for (param, &ty) in function
                .params
                .iter()
                .zip(params.iter().skip(usize::from(callable.receiver.is_some())))
            {
                if param.ownership == OwnershipMode::Copy
                    && let Some(reason) = self.copy_refusal(ty)
                {
                    self.emit(
                        function.name_span,
                        "KSEM356",
                        format!(
                            "parameter `{}` cannot be `copy {}`: {reason}",
                            self.interner.resolve(param.name),
                            self.type_name(ty)
                        ),
                    );
                }
            }
            let mut param_defaults: Vec<Option<FieldDefault>> =
                callable.receiver.map(|_| None).into_iter().collect();
            param_defaults.extend(function.params.iter().map(|param| {
                param
                    .default
                    .map(|syntax| FieldDefault::new(syntax, callable.source))
            }));
            // An `init(…)` writes no result type because it has only one: the
            // declaration it initializes. Filling it here is what makes its
            // body's `return Name(…)` an ordinary checked return.
            let return_type = match (callable.initializes, &function.return_type) {
                (Some(id), _) => Type::Struct(id),
                (None, Some(type_ref)) => self.resolve_type_ref(*type_ref),
                (None, None) => Type::Void,
            };
            let id = FuncId(self.sigs.len() as u32);
            // Sharing a name is overloading; sharing a name *and* what it takes
            // is redeclaring. Only the second is an error, because only the
            // second leaves a call with no way to say which one it meant.
            let twin = self.sig_index.get(&name).and_then(|ids| {
                ids.iter()
                    .find(|&&other| self.sigs[other.0 as usize].params == params)
            });
            if twin.is_some() {
                self.emit(
                    function.name_span,
                    "KSEM003",
                    match callable.receiver {
                        Some(_) => format!("`{name}` is already defined with these parameters"),
                        None => {
                            format!("function `{name}` is already defined with these parameters")
                        }
                    },
                );
            } else {
                self.sig_index.entry(name.clone()).or_default().push(id);
            }
            let is_main = function.is_main;
            if function.is_main && function.is_main_thread_lifecycle {
                self.emit(
                    function.name_span,
                    "KSEM339",
                    "the entrypoint runs on the application thread, so it cannot also be \
                     `@MainThreadLifecycle`: declare the main-thread loop as its own function",
                );
            }
            if function.is_main && function.is_main_thread {
                self.emit(
                    function.name_span,
                    "KSEM337",
                    "an entrypoint cannot also be `@MainThread`",
                );
            }
            if function.is_main_thread_lifecycle && !params.is_empty() {
                self.emit(
                    function.name_span,
                    "KSEM341",
                    "a `@MainThreadLifecycle` function must take no parameters: calling it \
                     starts an independent preserved stack and transfers no arguments",
                );
            }
            if function.is_main_thread_lifecycle && return_type != Type::Void {
                self.emit(
                    function.name_span,
                    "KSEM344",
                    "a `@MainThreadLifecycle` function must return `Void`: its call starts the \
                     lifecycle and does not wait for its eventual result",
                );
            }
            if (function.is_main_thread || function.is_main_thread_lifecycle)
                && (self.machine.platform() == "emscripten"
                    || self.machine.architecture().starts_with("wasm"))
            {
                self.emit(
                    function.name_span,
                    "KSEM338",
                    "a main-thread annotation requires an operating-system main thread and is \
                     unavailable on WebAssembly",
                );
            }
            if is_main && main_seen {
                self.emit(
                    function.name_span,
                    "KSEM010",
                    "a program may declare only one `@Main` entrypoint",
                );
            }
            main_seen = main_seen || is_main;
            let signature = CallableSignature {
                receiver: callable.receiver.map(|ty| ReceiverSignature {
                    ty,
                    mutable: function.receiver.is_some_and(|receiver| receiver.mutable),
                }),
                params: function
                    .params
                    .iter()
                    .zip(params.iter().skip(usize::from(callable.receiver.is_some())))
                    .zip(
                        param_ownership
                            .iter()
                            .skip(usize::from(callable.receiver.is_some())),
                    )
                    .map(|((param, &ty), &ownership)| ParamSignature {
                        label: self.interner.resolve(param.name).to_owned(),
                        ty,
                        ownership,
                        has_default: param.default.is_some(),
                    })
                    .collect(),
                result: return_type,
                is_async: function.is_async,
                affinity: if function.is_main_thread || function.is_main_thread_lifecycle {
                    ThreadAffinity::MainThread
                } else {
                    ThreadAffinity::Any
                },
                execution: function.execution,
            };
            self.sigs.push(FuncSig {
                // Filled by `name_overloads` once every declaration is in, so a
                // symbol depends on the overload set rather than on which
                // declaration happened to be read first.
                symbol: self.qualified_symbol(callable.source, &name),
                name,
                params,
                param_ownership,
                return_type,
                name_span: function.name_span,
                source: callable.source,
                is_main,
                is_main_thread_lifecycle: function.is_main_thread_lifecycle,
                is_main_thread: function.is_main_thread,
                signature,
            });
            // Pushed in lockstep with `sigs` so a `FuncId` indexes both. A
            // synthesized function reserves a later id with no row here, so the
            // default lookup returns `None` for it — never a panic.
            self.param_defaults.push(param_defaults);
            self.type_bindings = outer_bindings;
        }
        self.name_overloads();
    }

    /// Gives every declaration of an overloaded name a symbol of its own.
    ///
    /// A backend has one symbol per function, so two declarations that share a
    /// written name need two. The mangling names the parameter types rather
    /// than a counter so the symbol is the same however the files were ordered,
    /// and so a backtrace still says which overload it is standing in.
    ///
    /// A name declared once keeps its plain symbol. That is what leaves every
    /// program that overloads nothing byte-identical to what it compiled to
    /// before overloading existed.
    pub(super) fn name_overloads(&mut self) {
        let overloaded: Vec<FuncId> = self
            .sig_index
            .values()
            .filter(|ids| ids.len() > 1)
            .flatten()
            .copied()
            .collect();
        for id in overloaded {
            let params = self.sigs[id.0 as usize].params.clone();
            let mut symbol = self.qualified_symbol(
                self.sigs[id.0 as usize].source,
                &self.sigs[id.0 as usize].name,
            );
            for param in params {
                symbol.push('$');
                symbol.push_str(&self.program.types.identity_key(param).replace(' ', "_"));
            }
            self.sigs[id.0 as usize].symbol = symbol;
        }
    }

    /// The symbol a function declared in `source` gets: its name, qualified
    /// by the declaring package so two packages' same-named functions never
    /// share one.
    pub(crate) fn qualified_symbol(&self, source: SourceId, name: &str) -> String {
        match self.imports.package_of(source) {
            Some(package) => format!("{package}::{name}"),
            None => name.to_owned(),
        }
    }

    /// Every declaration answering to `name` that this file may see.
    ///
    /// The visibility rule is [`Self::lookup_function`]'s, applied per
    /// candidate: a bare name reaches only the packages this file imports, and
    /// a qualified `Owner.method` is reached through a value whose type was
    /// already gated.
    pub(crate) fn visible_overloads(&self, name: &str) -> Vec<FuncId> {
        let Some(ids) = self.sig_index.get(name) else {
            return Vec::new();
        };
        if name.contains('.') {
            return ids.clone();
        }
        ids.iter()
            .copied()
            .filter(|id| {
                self.imports
                    .sees(self.source, self.sigs[id.0 as usize].source)
            })
            .collect()
    }

    /// The symbol one declaration answers to in the compiled program.
    pub(crate) fn function_symbol(&self, id: FuncId) -> String {
        self.sigs[id.0 as usize].symbol.clone()
    }

    /// The mode a parameter declares.
    ///
    /// `borrow mut` is the one ownership mode that is **observable at run
    /// time**: a callee writing through it changes the caller's binding, where
    /// every other mode reduces to the deep copy the runtime already does. It
    /// is carried by the by-reference calling convention — the parameter joins
    /// [`kira_ir::IrFunction::by_reference_params`] and every call site records
    /// a writeback for it — rather than being refused here.
    pub(crate) fn check_param_ownership(
        &mut self,
        param: &kira_syntax_model::ast::Param,
    ) -> OwnershipMode {
        param.ownership
    }

    /// Looks up a signature by name (for call resolution).
    ///
    /// A **bare** name is gated the way a type name is: a function declared in
    /// another package is nameable only from a file that imports that package
    /// (see [`crate::imports::ImportTable::sees`]).
    ///
    /// A **qualified** `Owner.method` is not. A method is reached through a
    /// value, and naming the value's type was already gated; gating the method
    /// again would mean an imported type whose methods cannot be called, and
    /// would break the compiler's own walks over a family's implementations —
    /// which resolve a method while standing in the *family's* file, not the
    /// caller's.
    /// An overloaded name has no single answer here, so this reports its
    /// **first** declaration. Every caller either only asks whether the name
    /// exists, or resolves the overload itself first and looks the chosen
    /// [`FuncId`] up directly.
    pub(crate) fn lookup_function(&self, name: &str) -> Option<(FuncId, &[Type], Type)> {
        let ids = self.sig_index.get(name)?;
        let id = *ids.iter().find(|id| {
            name.contains('.')
                || self
                    .imports
                    .sees(self.source, self.sigs[id.0 as usize].source)
        })?;
        let sig = &self.sigs[id.0 as usize];
        Some((id, &sig.params, sig.return_type))
    }

    /// Looks up the method `method` as it is declared on receiver struct
    /// `receiver`.
    ///
    /// The display name two packages may share — each may declare a class of
    /// one name — lands both declarations under one index key, so picking by
    /// name alone can read the other package's mutating flag and silently drop
    /// or invent a receiver writeback. Matching the declared receiver instead
    /// picks the body this value's type actually means.
    pub(crate) fn lookup_method_for_receiver(
        &self,
        receiver: StructId,
        method: &str,
    ) -> Option<(FuncId, &[Type], Type)> {
        let qualified = format!(
            "{}.{method}",
            self.member_owner_name(Type::Struct(receiver))
        );
        let ids = self.sig_index.get(&qualified)?;
        let wanted = Type::Struct(receiver);
        let id = *ids
            .iter()
            .find(|id| self.sigs[id.0 as usize].params.first() == Some(&wanted))?;
        let sig = &self.sigs[id.0 as usize];
        Some((id, &sig.params, sig.return_type))
    }

    /// The shape of one declaration, by id.
    pub(crate) fn signature_of(&self, id: FuncId) -> (&[Type], Type) {
        let sig = &self.sigs[id.0 as usize];
        (&sig.params, sig.return_type)
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

    /// The stable source identity of a declared function, for a function value
    /// that must survive a live VM rebuild.
    pub(crate) fn function_identity(&self, id: FuncId) -> Option<(SourceId, Span, &str)> {
        self.sigs
            .get(id.0 as usize)
            .map(|sig| (sig.source, sig.name_span, sig.name.as_str()))
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
