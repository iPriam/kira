//! Building an analyzer and driving one pass over a program.
//!
//! The order the passes run in is the whole content of this module: what is
//! collected before what is resolved, and what may only be checked once both
//! are done.

use super::*;

impl<'a> Analyzer<'a> {
    pub(super) fn new(
        tree: &'a SyntaxTree,
        interner: &'a Names,
        modules: &[(String, SourceId)],
        build_kind: BuildKind,
        machine: &BuildMachine,
    ) -> Self {
        let entries = crate::imports::collect_imports(tree, interner);
        let imports = crate::imports::ImportTable::build(modules, &entries);
        let mut analyzer = Self {
            source: crate::FILE_SOURCE_ID,
            build_kind,
            machine: machine.clone(),
            imports,
            tree,
            interner,
            sigs: Vec::new(),
            sig_index: HashMap::new(),
            mutating_methods: Vec::new(),
            foreign_index: HashMap::new(),
            in_foreign_signature: false,
            struct_defaults: Vec::new(),
            resolving_struct_defaults: BTreeSet::new(),
            param_defaults: Vec::new(),
            resolving_param_defaults: BTreeSet::new(),
            enum_defaults: Vec::new(),
            generic_enums: crate::generics::GenericEnumTable::new(),
            type_bindings: crate::generics::TypeBindings::new(),
            instantiation_depth: 0,
            payload_blame: None,
            aliases: AliasTable::new(),
            pointer_targets: HashMap::new(),
            classes: HashMap::new(),
            constructs: HashMap::new(),
            construct_families: BTreeMap::new(),
            construct_family_names: HashMap::new(),
            traits: crate::traits::TraitTable::new(),
            conformances: Vec::new(),
            own_methods: HashMap::new(),
            unflattenable_classes: BTreeSet::new(),
            fn_types: crate::closures::FnTypeTable::default(),
            init_content: HashMap::new(),
            synth_base: 0,
            synth: Vec::new(),
            closure_sites: Vec::new(),
            current_execution: kira_semantics_model::Execution::Inherited,
            ffi_structs: HashMap::new(),
            struct_sources: HashMap::new(),
            enum_sources: HashMap::new(),
            ffi_array_counts: HashMap::new(),
            ffi_callback_signatures: HashMap::new(),
            foreign_aggregates: crate::foreign_aggregate::ForeignAggregateBuilder::default(),
            program: HirProgram::default(),
            diagnostics: Vec::new(),
            definitions: Vec::new(),
            decl_spans: crate::definitions::DeclSpans::collect(tree, interner),
        };
        analyzer.report_unresolved_imports(&entries);
        analyzer.link_resolved_imports(&entries);
        analyzer
    }

    pub(super) fn run(mut self) -> Analysis {
        // Aliases are registered first because any of the three collections
        // below may name one; they resolve lazily on first use, so registering
        // them here does not require the struct or enum table to exist yet.
        self.collect_type_aliases();
        // Traits are registered from syntax alone, before any type table, because
        // one type namespace means a struct, class, enum, or family declared
        // below has to be able to lose its name to a trait.
        self.collect_traits();
        // Enum *names* are declared before structs, so a struct field may name
        // one; a struct is declared before signatures, so a parameter may name
        // either. Enum *payloads* wait until every struct exists, because a
        // payload may name one — the two declaration kinds each need the other.
        let enum_headers = self.declare_enum_headers();
        // A family type must exist before ordinary structs resolve fields that
        // name it; concrete variants are filled after backed structs exist.
        self.collect_construct_family_headers();
        // A parent's surface joins its children before anything reads a family:
        // a backed declaration is checked against the merged surface, and a
        // child's declarations become variants of the parent's type.
        self.inherit_construct_families();
        // `extend Family { … }` modifiers join the family's method surface once
        // the family exists and before its method signatures are resolved, so a
        // modifier's parameter and result types are resolved with the rest.
        self.collect_extend_blocks();
        // Class *names* join the same table before struct fields resolve, for
        // the reason enum names do: a struct field may name a class and a class
        // field may name a struct, so neither kind can wait for the other to
        // finish. Flattening still waits until every struct exists, because a
        // class may extend one.
        let class_headers = self.declare_class_headers();
        self.collect_structs();
        self.collect_classes(&class_headers);
        // Every by-value edge in the table exists only now that both kinds are
        // filled, and a cycle spelled through a class is one neither pass could
        // see on its own.
        self.break_remaining_value_cycles();
        // Construct-backed declarations become struct-shaped types too, and a
        // param or member may name any struct, enum, or class, so they are
        // declared once every one of those exists and before signatures — a
        // backed declaration's methods take signature slots.
        self.collect_constructs();
        // Every type a payload could name now has an id, so the variants the
        // header pass left empty are filled here.
        self.resolve_enum_payloads(&enum_headers);
        // Conformance names a type, so it is resolved once every struct-shaped
        // one has an id — and before callables are enumerated, because a
        // default a conforming type did not write becomes one of its methods.
        self.collect_conformances();
        // `@Derive(Copy)` asks a question about a whole reachable shape, so it
        // is answered once every struct, class, enum, and construct-backed type
        // exists and every payload is resolved.
        self.check_copy_derives();
        let callables = self.callables();
        // Every synthesized function sits after every declared one, so the
        // declared count is the offset a reserved id is measured from. Fixed
        // here, before any signature can reserve one.
        self.synth_base = callables.len() as u32;
        self.collect_signatures(&callables);
        // Which init parameters take content is a question about the written
        // `some X`, and the ids the answer is filed under exist only now.
        self.record_init_content(&callables);
        // Each `extend` modifier lowers to one synthesized function; its id is
        // reserved here, once `synth_base` is fixed, so an uncalled modifier is
        // still checked and lowered. The bodies are filled after ordinary ones.
        self.reserve_extend_bodies();
        // Which methods mutate their receiver is decided once here, before any
        // body is analyzed: a body analyzes `self` as mutable exactly when its
        // method is marked mutating, and a call site writes the receiver back
        // exactly when its callee is. The fixpoint reads the signatures the step
        // above built, so it runs after them.
        self.collect_mutating_methods(&callables);
        self.check_construct_method_signatures();
        // A claimed conformance is checked against resolved shapes, so it waits
        // for the signatures every implementation and every requirement has.
        self.check_trait_conformance();
        // `@Main` is a property of the program, not of any one file, and the
        // "no `@Main`" diagnostic has no span to point at — so it is attributed
        // to the entry file rather than to whichever module happened to declare
        // the last signature.
        self.source = crate::FILE_SOURCE_ID;
        self.check_main();
        // Exports are checked once signatures exist, because every refusal is
        // about a *resolved* parameter or result type, and once classes are
        // flattened, because handle-eligibility is a property of a struct row.
        self.check_exports(&callables);
        // Foreign callables are collected once signatures exist — a foreign name
        // may not collide with a user function's, and the collision check reads
        // the signature index — and before any body, so a call in a body
        // resolves to `Callee::Foreign`.
        self.collect_foreign();
        // An unannotated construct field needs the complete callable/type
        // surface to analyze its initializer. Resolve it before the ordinary
        // default pass so construction sites see the inferred field type.
        self.resolve_construct_field_types();
        // Inference can reveal a by-value edge that was `Error` while the
        // construct header was collected. Re-run the value-cycle break after
        // those edges become concrete, before any instance default is lowered.
        self.break_remaining_value_cycles();
        // A field default belongs to its declaration. Resolve every one now, with
        // signatures and foreign callables available but before a construction
        // site can supply some unrelated file scope, and reuse that HIR at every
        // site that omits the field.
        self.resolve_struct_defaults();
        // A parameter default belongs to its declaration too, and follows the
        // same rule: resolve every one now, in its declaring file, and reuse the
        // HIR at every call that omits the argument.
        self.resolve_param_defaults();
        // A construct-family method carries no signature row, so its parameter
        // defaults resolve in their own pass — same rule, same moment.
        self.resolve_construct_method_defaults();
        // Bodies are analyzed in the same order the signatures were collected,
        // which is what makes a `FuncId` index both.
        for (index, callable) in callables.iter().enumerate() {
            let hir_function = self.analyze_function(FuncId(index as u32), callable);
            self.program.functions.push(hir_function);
        }
        // Dynamic construct dispatchers, family value-member dispatchers, and
        // `extend` modifier bodies share one synthesized-function id space, and
        // building one of them can reserve another: a modifier that reads a
        // family member reserves that member's dispatcher *while* the modifier
        // is being built, and a dispatcher arm can do the same. So the fill
        // passes run until a round reserves nothing new — which is what makes
        // "a reserved id is always filled" true rather than a hope. A hole left
        // here reached the backends as a call to a zero-argument `Void`
        // placeholder and was rejected by the LLVM verifier.
        loop {
            let reserved = self.reserved_synth();
            self.build_extend_methods();
            self.build_construct_dispatchers();
            if self.reserved_synth() == reserved {
                break;
            }
        }
        // Every enum exists now, including the ones a body minted by writing a
        // generic instantiation, and the desugar below is the first pass to ask
        // for a value of a type nobody wrote.
        self.check_enum_terminates();
        self.finalize_closures();
        // After lifting, not before: a closure's representation struct is only
        // final once every literal of its type has been found, and a callback
        // state's identity is a fingerprint of the shape it boxes.
        self.finalize_native_state_type_ids();
        Analysis {
            program: self.program,
            diagnostics: self.diagnostics,
            definitions: self.definitions,
        }
    }
}
