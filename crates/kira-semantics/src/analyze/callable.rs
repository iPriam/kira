//! Which callables a program has, and what each one is named.
//!
//! Specialization and subclassing live here together because they answer one
//! question between them: given a call, which body does it reach.

use super::*;

impl<'a> Analyzer<'a> {
    /// Every function the program declares, in one stable order: a free
    /// function where it was written, and a struct's methods where the struct
    /// was.
    ///
    /// A method is an ordinary function that happens to have a receiver, so it
    /// takes a slot in the same table. Everything downstream of analysis — the
    /// IR, both compilers, the hybrid manifest — sees a flat list of functions
    /// and never learns that some of them were written inside a struct.
    pub(super) fn callables(&self) -> Vec<Callable<'a>> {
        let mut callables = Vec::new();
        for (source, item) in self.tree.items_with_source() {
            match item {
                // A bodyless `@FFI.Extern` function is never an ordinary
                // callable: it becomes a row in `HirProgram::foreign`, not a
                // `HirFunction`, so it is skipped here and handled by
                // `collect_foreign`.
                Item::Function(function) if function.foreign.is_some() => {}
                Item::Function(function) => callables.push(Callable {
                    receiver: None,
                    origin: None,
                    specialize: Vec::new(),
                    initializes: None,
                    function,
                    source,
                }),
                Item::Struct(declaration) => {
                    // The struct this declaration minted, under its own
                    // package: a bare name is not unique across packages, and
                    // this is registering what *this* declaration provides.
                    let package = self.imports.package_of(source);
                    let owner = self
                        .program
                        .types
                        .structs()
                        .lookup_owned(package, self.interner.resolve(declaration.name));
                    for method in &declaration.methods {
                        callables.push(Callable {
                            receiver: owner,
                            origin: None,
                            specialize: Vec::new(),
                            initializes: None,
                            function: method,
                            source,
                        });
                    }
                }
                Item::Class(declaration) => {
                    self.class_callables(declaration, source, &mut callables)
                }
                Item::Construct(declaration) => {
                    self.construct_callables(declaration, source, &mut callables)
                }
                // An `extend` block's modifiers are not ordinary callables:
                // each lowers to a synthesized function whose receiver is the
                // family value, built after signatures exist. See
                // `constructs::extend`.
                // A module-scope `let` is a value, not a callable. Its
                // initializer is folded during analysis and substituted at every
                // read, so nothing about it reaches this table.
                Item::Constant(_)
                | Item::Enum(_)
                | Item::TypeAlias(_)
                | Item::Import(_)
                | Item::Extend(_)
                | Item::Trait(_)
                | Item::Unsupported(_) => {}
            }
        }
        // Before specialization, so a trait default a class inherits specializes
        // on its class-typed parameters exactly as a written method does.
        self.trait_callables(&mut callables);
        self.specialize_callables(&mut callables);
        callables
    }

    /// Adds one copy of every callable per subclass its parameters admit.
    ///
    /// This is what makes `feed(a: Animal)` accept a `Dog` *and* call `Dog`'s
    /// override: the copy has that parameter typed as `Dog`, so `a.speak()`
    /// inside it is statically the subclass's method. The same trick
    /// `class_callables` plays for inheritance, applied to arguments.
    ///
    /// The cross product over several class-typed parameters is deliberate — a
    /// function of two animals has to specialize on both or the second falls
    /// back to the parent's method, which is the bug this exists to remove. It
    /// is bounded by [`Self::SPECIALIZATION_LIMIT`], past which the function is
    /// left as written rather than silently half-specialized.
    pub(super) fn specialize_callables(&self, callables: &mut Vec<Callable<'a>>) {
        let mut added: Vec<Callable<'a>> = Vec::new();
        for callable in callables.iter() {
            let choices = self.parameter_subclasses(callable);
            if choices.is_empty() {
                continue;
            }
            let mut combinations: Vec<Vec<(usize, StructId)>> = vec![Vec::new()];
            for (index, subclasses) in choices {
                let mut grown = Vec::new();
                for existing in &combinations {
                    // The declared class itself is one of the choices, which is
                    // how the copy for `feed(Animal)` stays reachable.
                    grown.push(existing.clone());
                    for subclass in &subclasses {
                        let mut next = existing.clone();
                        next.push((index, *subclass));
                        grown.push(next);
                    }
                }
                combinations = grown;
                if combinations.len() > Self::SPECIALIZATION_LIMIT {
                    break;
                }
            }
            if combinations.len() > Self::SPECIALIZATION_LIMIT {
                continue;
            }
            for specialize in combinations {
                if specialize.is_empty() {
                    continue;
                }
                added.push(Callable {
                    specialize,
                    ..callable.clone()
                });
            }
        }
        callables.extend(added);
    }

    /// How many copies of one callable specialization may produce.
    ///
    /// A function of three class-typed parameters in a program with a deep
    /// hierarchy would otherwise mint a copy per combination. Past the limit the
    /// function keeps only what was written — a call with a subclass argument
    /// still compiles, it simply reaches the parent's method, which is the
    /// behaviour to report rather than to hide.
    const SPECIALIZATION_LIMIT: usize = 64;

    /// The class a written type reference names, when it names one.
    ///
    /// Resolves without reporting: an unknown or non-class type is simply not a
    /// candidate for specialization, and the diagnostic for a name that resolves
    /// to nothing belongs to signature collection, which reports it once.
    pub(super) fn written_class(
        &self,
        written: kira_syntax_model::ast::TypeRefId,
    ) -> Option<StructId> {
        let kira_syntax_model::ast::TypeRef::Named { name, .. } = self.tree.type_ref(written)
        else {
            return None;
        };
        let id = self.visible_struct(self.interner.resolve(*name))?;
        self.classes.contains_key(&id).then_some(id)
    }

    /// Each written parameter that names a class, with the classes that inherit
    /// it.
    pub(super) fn parameter_subclasses(
        &self,
        callable: &Callable<'_>,
    ) -> Vec<(usize, Vec<StructId>)> {
        if !callable.specialize.is_empty() {
            return Vec::new();
        }
        let mut found = Vec::new();
        for (index, param) in callable.function.params.iter().enumerate() {
            let Some(declared) = self.written_class(param.ty) else {
                continue;
            };
            let subclasses: Vec<StructId> = self
                .classes
                .iter()
                .filter(|(id, info)| **id != declared && info.ancestors.contains(&declared))
                .map(|(id, _)| *id)
                .collect();
            if !subclasses.is_empty() {
                found.push((index, subclasses));
            }
        }
        found
    }

    /// The name a callable is known by.
    ///
    /// A method is qualified with its struct (`Point.sum`), which is what keeps
    /// two structs' methods of the same name apart and keeps a method from
    /// colliding with a free function — `.` cannot appear in an identifier, so
    /// no user name can collide with a qualified one.
    pub(crate) fn callable_name(&self, callable: &Callable<'_>) -> String {
        // Every `init(…)` of one declaration answers to one name, so the
        // overload set under it is that declaration's initializers and nothing
        // else. A construction site consults it beside the primary form.
        if let Some(id) = callable.initializes {
            return self.initializer_name(id);
        }
        let written = self.interner.resolve(callable.function.name);
        // A specialization is a distinct callable, so it needs a distinct name.
        // `$` is already the separator inheritance uses for the same reason and
        // cannot appear in an identifier, so `feed$1$Dog` collides with nothing
        // a user can write. The parameter index is part of it because two
        // parameters may specialize on the same class.
        let suffix: String = callable
            .specialize
            .iter()
            .map(|(index, class)| {
                format!(
                    "${index}${}",
                    self.program.types.type_name(Type::Struct(*class))
                )
            })
            .collect();
        let written = &format!("{written}{suffix}");
        let Some(id) = callable.receiver else {
            return written.to_owned();
        };
        let receiver = self.program.types.type_name(Type::Struct(id));
        // A class carries one copy of every method any ancestor declares. The
        // copy that wins bare lookup takes the plain `Class.method` name a call
        // site spells; a copy an override shadows takes a qualified name, which
        // is what `ClsSquare.scaledArea()` inside `ClsCube` resolves to. `$`
        // cannot appear in an identifier, so neither can collide with a user
        // name.
        // Bare lookup is asked with the *member key*, which carries the
        // parameters an overloaded name is told apart by; the specialization
        // suffix is not part of what the class declared.
        let key = self.member_key(
            self.interner.resolve(callable.function.name),
            &callable.function.params,
        );
        match callable.origin {
            Some(origin) if !self.is_most_derived(id, origin, &key) => {
                let origin = self.program.types.type_name(Type::Struct(origin));
                format!("{receiver}.{origin}${written}")
            }
            _ => format!("{receiver}.{written}"),
        }
    }

    /// The name every `init(…)` of `id` is registered under.
    ///
    /// `$` cannot appear in an identifier, so this collides with nothing a
    /// program can write, and the declaration's own name is in it so two
    /// declarations' initializers never share an overload set.
    pub(crate) fn initializer_name(&self, id: StructId) -> String {
        format!("{}$init", self.program.types.type_name(Type::Struct(id)))
    }

    /// What a class member is known by: its name together with what it takes.
    ///
    /// A name alone stopped being an identity when names became overloadable —
    /// `bump()` and `bump(step: Int)` are two members of one class. The written
    /// type spelling is used rather than the resolved type because members are
    /// keyed while classes are being flattened, which is before every type a
    /// parameter may name has an id.
    pub(crate) fn member_key(
        &self,
        name: &str,
        params: &[kira_syntax_model::ast::Param],
    ) -> String {
        let written: Vec<String> = params
            .iter()
            .map(|param| self.written_type_name(param.ty))
            .collect();
        format!("{name}({})", written.join(","))
    }

    /// Whether `origin`'s copy of the member `key` names is the one bare lookup
    /// on `class` finds.
    pub(crate) fn is_most_derived(&self, class: StructId, origin: StructId, key: &str) -> bool {
        matches!(
            self.classes
                .get(&class)
                .and_then(|info| info.bare_methods.get(key)),
            Some(crate::classes::Member::One(winner)) if *winner == origin
        )
    }
}
