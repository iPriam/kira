//! The checker: resolving a parsed KSL file, and everything it imports, into
//! one [`CheckedModule`].
//!
//! Total, like the parser. A rejected construct becomes
//! [`CheckedExprKind::Invalid`] or is dropped, and checking continues — so a
//! file with one bad line still reports everything else wrong with it.
//!
//! Imported items are flattened into the same module with their alias folded
//! into the name: `Lighting.lambert` becomes `Lighting_lambert`. Backends emit
//! a flat namespace, none of the shader dialects have modules, and doing it
//! once here keeps every backend from having to invent the same mangling.

use std::collections::HashMap;

use kira_core::Interner;
use kira_ksl_syntax_model::ast::{
    ConstDecl, EnumDecl, Field, Function, Group, Item, Resource,
    ResourceKind as SyntaxResourceKind, Shader, StageDecl, StageWord, TypeDecl, TypeRef,
};
use kira_ksl_syntax_model::tree::{KslTree, TypeRefId};
use kira_shader_model::{
    AccessMode, ResourceKind, ScalarType, Stage, Type, builtin_allowed, classify_group_name,
};
use kira_source::{SourceId, Span};

use crate::builtins;
use crate::diagnostics::{self, Reporter};
use crate::model::{
    CheckedField, CheckedFunction, CheckedGroup, CheckedModule, CheckedOption, CheckedParam,
    CheckedResource, CheckedShader, CheckedStage, CheckedStruct, ConstValue,
};

mod body;
mod expr;

/// One parsed KSL file, ready to be checked.
#[derive(Debug)]
pub struct Module {
    /// The file the tree came from, for diagnostics.
    pub source: SourceId,
    /// Its parsed tree.
    pub tree: KslTree,
    /// The interner its symbols resolve through.
    pub interner: Interner,
}

/// A function's signature, for checking a call against it.
#[derive(Debug, Clone)]
pub(crate) struct Signature {
    pub(crate) params: Vec<Type>,
    pub(crate) result: Type,
}

/// The running check.
pub(crate) struct Checker<'a> {
    /// The file being read right now, which an import switches.
    tree: &'a KslTree,
    interner: &'a Interner,
    /// The prefix imported names are given, empty for the main file.
    prefix: String,
    /// Every struct's fields, by emitted name.
    pub(crate) structs: HashMap<String, Vec<CheckedField>>,
    /// Every declared function's signature, by emitted name.
    pub(crate) signatures: HashMap<String, Signature>,
    /// Every resource in scope, by name.
    pub(crate) resources: HashMap<String, ResourceBinding>,
    /// Every option in scope, by name.
    pub(crate) options: HashMap<String, (Type, ConstValue)>,
    /// Every `const` and enum variant in scope, by emitted path.
    ///
    /// An enum variant is keyed `Enum_Variant`, which is what
    /// [`Checker::qualified`] spells an imported name as, so a variant reached
    /// through an import alias and one written locally find the same entry.
    pub(crate) constants: HashMap<String, (Type, ConstValue)>,
    /// The local scopes of the body being checked, innermost last.
    pub(crate) scopes: Vec<HashMap<String, Type>>,
    /// What the body being checked must return.
    pub(crate) result: Type,
    pub(crate) module: CheckedModule,
    pub(crate) reporter: Reporter,
}

/// A resource as the body sees it: its type, and whether it can be written.
#[derive(Debug, Clone)]
pub(crate) struct ResourceBinding {
    pub(crate) ty: Type,
    pub(crate) writable: bool,
}

impl<'a> Checker<'a> {
    /// Prepares a check of `tree`.
    pub(crate) fn new(module: &'a Module, reporter: Reporter) -> Self {
        Self {
            tree: &module.tree,
            interner: &module.interner,
            prefix: String::new(),
            structs: HashMap::new(),
            signatures: HashMap::new(),
            resources: HashMap::new(),
            options: HashMap::new(),
            constants: HashMap::new(),
            scopes: Vec::new(),
            result: Type::Void,
            module: CheckedModule::default(),
            reporter,
        }
    }

    /// Points the checker at `module`, whose items get `prefix`.
    pub(crate) fn switch_to(&mut self, module: &'a Module, prefix: &str) {
        self.tree = &module.tree;
        self.interner = &module.interner;
        self.prefix = prefix.to_owned();
        self.reporter.switch_to(module.source);
    }

    /// The text `symbol` was interned from.
    pub(crate) fn name(&self, symbol: kira_core::Symbol) -> String {
        self.interner.resolve(symbol).to_owned()
    }

    /// The tree being read right now.
    pub(crate) fn tree(&self) -> &KslTree {
        self.tree
    }

    /// `name` with the current file's prefix folded in.
    pub(crate) fn qualified(&self, name: &str) -> String {
        if self.prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{}_{name}", self.prefix)
        }
    }

    /// Declares every type and function the current file holds.
    ///
    /// Types and signatures land before any body is checked, so declaration
    /// order never matters — a function may call one written below it.
    pub(crate) fn declare(&mut self) {
        let items = self.tree.items.clone();
        for item in &items {
            match item {
                Item::Type(declared) => self.declare_struct(declared),
                Item::Function(declared) => self.declare_function(declared),
                Item::Const(declared) => self.declare_const(declared),
                Item::Enum(declared) => self.declare_enum(declared),
                Item::Import(_) | Item::Shader(_) => {}
            }
        }
    }

    /// Reports every `import` no module was supplied for.
    pub(crate) fn report_unresolved_imports(&mut self, imports: &[(String, Module)]) {
        let items = self.tree.items.clone();
        for item in &items {
            let Item::Import(import) = item else {
                continue;
            };
            let written: Vec<String> = import.path.iter().map(|&s| self.name(s)).collect();
            let alias = import.alias.map_or_else(
                || written.last().cloned().unwrap_or_default(),
                |a| self.name(a),
            );
            if !imports.iter().any(|(supplied, _)| *supplied == alias) {
                self.reporter.error(
                    import.span,
                    diagnostics::UNRESOLVED_IMPORT,
                    format!("`{}` could not be loaded", written.join(".")),
                );
            }
        }
    }

    /// The module and everything reported about it.
    pub(crate) fn finish(self) -> (CheckedModule, Vec<kira_diagnostics::Diagnostic>) {
        (self.module, self.reporter.into_diagnostics())
    }

    /// Checks every function and shader the current file holds.
    pub(crate) fn check_items(&mut self) {
        let items = self.tree.items.clone();
        for item in &items {
            match item {
                Item::Function(declared) => {
                    let checked = self.function(declared);
                    self.module.functions.push(checked);
                }
                Item::Shader(declared) => self.shader(declared),
                Item::Import(_) | Item::Type(_) | Item::Const(_) | Item::Enum(_) => {}
            }
        }
    }

    // -- declarations ----------------------------------------------------

    /// Records one struct's fields.
    fn declare_struct(&mut self, declared: &TypeDecl) {
        let name = self.qualified(&self.name(declared.name));
        if self.structs.contains_key(&name) {
            self.reporter.error(
                declared.span,
                diagnostics::DUPLICATE,
                format!("`{name}` is declared more than once"),
            );
            return;
        }
        let fields: Vec<CheckedField> = declared
            .fields
            .iter()
            .map(|field| self.field(field))
            .collect();
        self.structs.insert(name.clone(), fields.clone());
        self.module.structs.push(CheckedStruct { name, fields });
    }

    /// Folds one `const` to its value.
    ///
    /// A `const` never reaches a backend: every read of it becomes the value
    /// it folded to, exactly as an `option` read does. That keeps a shared
    /// number in one place in the source without asking four dialects to agree
    /// on how a module-scope constant is spelled.
    fn declare_const(&mut self, declared: &ConstDecl) {
        let name = self.qualified(&self.name(declared.name));
        let ty = self.resolve(declared.ty);
        let Some(value) = self.constant(declared.value, &ty) else {
            self.reporter.error(
                declared.span,
                diagnostics::BAD_CONSTANT,
                format!("`{name}` needs a constant value of its own type"),
            );
            return;
        };
        self.bind_constant(name, ty, value, declared.span);
    }

    /// Folds every variant of one `enum` to its number.
    fn declare_enum(&mut self, declared: &EnumDecl) {
        let enum_name = self.name(declared.name);
        let ty = Type::Scalar(ScalarType::Int);
        for variant in &declared.variants {
            let name = self.qualified(&format!("{enum_name}_{}", self.name(variant.name)));
            let Some(value) = self.constant(variant.value, &ty) else {
                self.reporter.error(
                    variant.span,
                    diagnostics::BAD_CONSTANT,
                    format!("`{name}` needs a whole number of its own"),
                );
                continue;
            };
            self.bind_constant(name, ty.clone(), value, variant.span);
        }
    }

    /// Binds one folded constant, reporting a name already taken.
    fn bind_constant(&mut self, name: String, ty: Type, value: ConstValue, span: Span) {
        if self.constants.contains_key(&name) {
            self.reporter.error(
                span,
                diagnostics::DUPLICATE,
                format!("`{name}` is declared more than once"),
            );
            return;
        }
        self.constants.insert(name, (ty, value));
    }

    /// Resolves one field of a struct.
    fn field(&mut self, field: &Field) -> CheckedField {
        let ty = self.resolve(field.ty);
        let builtin = field.builtin.and_then(|symbol| {
            let word = self.name(symbol);
            match builtins::builtin_value(&word) {
                Some(builtin) => Some(builtin),
                None => {
                    self.reporter.error(
                        field.span,
                        diagnostics::BAD_BUILTIN,
                        format!("`{word}` is not a builtin KSL provides"),
                    );
                    None
                }
            }
        });
        let interpolation = field.interpolation.and_then(|symbol| {
            let word = self.name(symbol);
            match builtins::interpolation(&word) {
                Some(interpolation) => Some(interpolation),
                None => {
                    self.reporter.error(
                        field.span,
                        diagnostics::BAD_BUILTIN,
                        format!(
                            "`{word}` is not an interpolation qualifier: expected `perspective`, \
                             `linear`, or `flat`"
                        ),
                    );
                    None
                }
            }
        });
        CheckedField {
            name: self.name(field.name),
            ty,
            builtin,
            interpolation,
        }
    }

    /// Records one function's signature.
    fn declare_function(&mut self, declared: &Function) {
        let name = self.qualified(&self.name(declared.name));
        let signature = self.signature(declared);
        if self.signatures.insert(name.clone(), signature).is_some() {
            self.reporter.error(
                declared.span,
                diagnostics::DUPLICATE,
                format!("`{name}` is declared more than once"),
            );
        }
    }

    /// The signature `declared` writes.
    fn signature(&mut self, declared: &Function) -> Signature {
        Signature {
            params: declared
                .params
                .iter()
                .map(|param| self.resolve(param.ty))
                .collect(),
            result: declared
                .result
                .map_or(Type::Void, |result| self.resolve(result)),
        }
    }

    // -- types -----------------------------------------------------------

    /// Resolves a written type, reporting one that names nothing.
    pub(crate) fn resolve(&mut self, id: TypeRefId) -> Type {
        match self.tree.type_ref(id).clone() {
            TypeRef::Array { element, .. } => Type::RuntimeArray(Box::new(self.resolve(element))),
            TypeRef::Named { path, span } => {
                let written: Vec<String> = path.iter().map(|&symbol| self.name(symbol)).collect();
                let joined = written.join("_");
                if let Some(builtin) = builtins::builtin_type(&joined) {
                    return builtin;
                }
                // A dotted path is an imported type, whose name was flattened
                // with the same separator when it was declared.
                if self.structs.contains_key(&joined) {
                    return Type::StructRef(joined);
                }
                let local = self.qualified(&joined);
                if self.structs.contains_key(&local) {
                    return Type::StructRef(local);
                }
                self.reporter.error(
                    span,
                    diagnostics::UNKNOWN_TYPE,
                    format!("`{}` names no type", written.join(".")),
                );
                Type::Void
            }
        }
    }

    // -- shader ----------------------------------------------------------

    /// Checks one shader declaration.
    fn shader(&mut self, declared: &Shader) {
        if self.module.shader.is_some() {
            self.reporter.error(
                declared.span,
                diagnostics::SHADER_COUNT,
                "a KSL file declares at most one shader",
            );
            return;
        }
        // Each shader's option and resource names bind for *its own* bodies
        // only. The maps are shared across every module this run checks, so an
        // imported shader checked earlier would otherwise leave its bindings
        // live while later bodies are checked — and a typo'd name in main
        // would resolve silently to another shader's group.
        self.resources.clear();
        self.options.clear();
        let name = self.name(declared.name);
        let options = self.options(declared);
        let groups = self.groups(declared);
        let stages: Vec<CheckedStage> = declared
            .stages
            .iter()
            .map(|stage| self.stage(stage))
            .collect();
        if stages.is_empty() {
            self.reporter.error(
                declared.span,
                diagnostics::BAD_STAGE,
                format!("`{name}` declares no stage, so there is nothing to compile"),
            );
        }
        self.module.shader = Some(CheckedShader {
            name,
            options,
            groups,
            stages,
        });
    }

    /// Checks every option, binding each for the bodies that read it.
    fn options(&mut self, declared: &Shader) -> Vec<CheckedOption> {
        let mut checked = Vec::new();
        for option in &declared.options {
            let name = self.name(option.name);
            let ty = self.resolve(option.ty);
            let Some(value) = self.constant(option.value, &ty) else {
                self.reporter.error(
                    option.span,
                    diagnostics::BAD_OPTION,
                    format!("`{name}` needs a constant default of its own type"),
                );
                continue;
            };
            self.options.insert(name.clone(), (ty.clone(), value));
            checked.push(CheckedOption { name, ty, value });
        }
        checked
    }

    /// Checks every group, binding its resources for the bodies that use them.
    fn groups(&mut self, declared: &Shader) -> Vec<CheckedGroup> {
        let mut checked = Vec::new();
        for group in &declared.groups {
            let name = self.name(group.name);
            let class = classify_group_name(&name);
            let mut resources = Vec::new();
            for resource in &group.resources {
                if let Some(checked) = self.resource(resource) {
                    resources.push(checked);
                }
            }
            self.reject_slot_collisions(group, &name, &resources);
            checked.push(CheckedGroup {
                name,
                class,
                resources,
            });
        }
        checked
    }

    /// Rejects two resources in one group that would take the same slot.
    ///
    /// A slot is either written as `@binding(n)` or taken from the position of
    /// the declaration, and the two mix in one group — the background
    /// compositor writes its photo slots to match the groups the host already
    /// fills while leaving its uniform positional. What must not happen is two
    /// names on one slot: WGSL and SPIR-V address a resource as (set, binding),
    /// so a collision there is one resource shadowing another, and the shader
    /// would still compile on Metal, whose per-kind spaces would keep them
    /// apart. That is a shader that works on one backend and silently reads the
    /// wrong texture on the next.
    fn reject_slot_collisions(&mut self, group: &Group, name: &str, resources: &[CheckedResource]) {
        let mut taken: HashMap<u32, String> = HashMap::new();
        for (position, resource) in resources.iter().enumerate() {
            let slot = resource
                .binding
                .unwrap_or_else(|| u32::try_from(position).unwrap_or(u32::MAX));
            let span = group
                .resources
                .get(position)
                .map_or(group.span, |declared| declared.span);
            if let Some(first) = taken.get(&slot) {
                self.reporter.error(
                    span,
                    diagnostics::BAD_BINDING,
                    format!(
                        "`{}` and `{first}` both bind slot {slot} of group `{name}`",
                        resource.name
                    ),
                );
            } else {
                taken.insert(slot, resource.name.clone());
            }
        }
    }

    /// Checks one resource, rejecting a type its kind cannot hold.
    fn resource(&mut self, declared: &Resource) -> Option<CheckedResource> {
        let name = self.name(declared.name);
        let ty = self.resolve(declared.ty);
        let kind = match declared.kind {
            SyntaxResourceKind::Uniform => ResourceKind::Uniform,
            SyntaxResourceKind::Storage => ResourceKind::Storage,
            SyntaxResourceKind::Texture => ResourceKind::Texture,
            SyntaxResourceKind::Sampler => ResourceKind::Sampler,
        };
        let legal = match kind {
            ResourceKind::Uniform => matches!(ty, Type::StructRef(_)),
            ResourceKind::Storage => matches!(ty, Type::RuntimeArray(_)),
            ResourceKind::Texture => matches!(ty, Type::Texture(_)),
            ResourceKind::Sampler => matches!(ty, Type::Sampler(_)),
        };
        if !legal {
            self.reporter.error(
                declared.span,
                diagnostics::BAD_RESOURCE,
                match kind {
                    ResourceKind::Uniform => {
                        format!("`{name}` is a uniform, so its type must be a struct")
                    }
                    ResourceKind::Storage => {
                        format!("`{name}` is storage, so its type must be an array")
                    }
                    ResourceKind::Texture => {
                        format!("`{name}` is a texture, so its type must be a texture type")
                    }
                    ResourceKind::Sampler => {
                        format!("`{name}` is a sampler, so its type must be a sampler type")
                    }
                },
            );
            return None;
        }
        let access = declared.access.map(|access| match access {
            kira_ksl_syntax_model::ast::Access::Read => AccessMode::Read,
            kira_ksl_syntax_model::ast::Access::ReadWrite => AccessMode::ReadWrite,
            kira_ksl_syntax_model::ast::Access::Write => AccessMode::Write,
        });
        self.resources.insert(
            name.clone(),
            ResourceBinding {
                ty: ty.clone(),
                writable: access == Some(AccessMode::ReadWrite),
            },
        );
        Some(CheckedResource {
            name,
            kind,
            access,
            binding: declared.binding,
            ty,
        })
    }

    /// Checks one stage, its interfaces, and its functions.
    fn stage(&mut self, declared: &StageDecl) -> CheckedStage {
        let stage = match declared.stage {
            StageWord::Vertex => Stage::Vertex,
            StageWord::Fragment => Stage::Fragment,
            StageWord::Compute => Stage::Compute,
        };
        let input = declared.input.map(|symbol| self.name(symbol));
        let output = declared.output.map(|symbol| self.name(symbol));
        self.check_interface(input.as_deref(), stage, true, declared.span);
        self.check_interface(output.as_deref(), stage, false, declared.span);

        let threads = self.threads(declared, stage);

        // Every function in the stage is declared before any is checked, so a
        // helper written below the entry point is still callable from it.
        for function in &declared.functions {
            let name = self.name(function.name);
            let signature = self.signature(function);
            self.signatures.insert(name, signature);
        }
        let mut entry = None;
        let mut helpers = Vec::new();
        for function in &declared.functions {
            let checked = self.function(function);
            // The written name decides, not the checked one: inside an
            // imported module every function is registered under its
            // `Alias_entry` spelling, and comparing that against `"entry"`
            // would make every stage of an imported module look like it had
            // none.
            if self.name(function.name) == "entry" {
                if entry.is_some() {
                    self.reporter.error(
                        function.span,
                        diagnostics::BAD_STAGE,
                        "a stage has one `entry`, but this one declares two",
                    );
                }
                entry = Some(checked);
            } else {
                helpers.push(checked);
            }
        }
        let entry = entry.unwrap_or_else(|| {
            self.reporter.error(
                declared.span,
                diagnostics::BAD_STAGE,
                format!(
                    "the `{}` stage declares no `entry` function",
                    declared.stage.spelling()
                ),
            );
            CheckedFunction {
                name: "entry".to_owned(),
                params: Vec::new(),
                result: Type::Void,
                body: Vec::new(),
            }
        });
        CheckedStage {
            stage,
            input,
            output,
            threads,
            entry,
            helpers,
        }
    }

    /// Checks that a stage's interface struct exists and its builtins are legal.
    fn check_interface(&mut self, name: Option<&str>, stage: Stage, is_input: bool, span: Span) {
        let Some(name) = name else {
            return;
        };
        // The interface is written bare in this module's shader block, but the
        // struct table keys imported declarations under their qualified
        // spelling — so the lookup follows `resolve`'s fallback instead of
        // demanding the raw name.
        let key = if self.structs.contains_key(name) {
            name.to_owned()
        } else {
            let local = self.qualified(name);
            if self.structs.contains_key(&local) {
                local
            } else {
                self.reporter.error(
                    span,
                    diagnostics::UNKNOWN_TYPE,
                    format!("`{name}` names no type, so this stage has no interface"),
                );
                return;
            }
        };
        let Some(fields) = self.structs.get(&key).cloned() else {
            self.reporter.error(
                span,
                diagnostics::UNKNOWN_TYPE,
                format!("`{name}` names no type, so this stage has no interface"),
            );
            return;
        };
        let direction = if is_input {
            kira_shader_model::InterfaceDirection::Input
        } else {
            kira_shader_model::InterfaceDirection::Output
        };
        for field in &fields {
            let Some(builtin) = field.builtin else {
                continue;
            };
            if !builtin_allowed(builtin, stage, direction) {
                self.reporter.error(
                    span,
                    diagnostics::BAD_BUILTIN,
                    format!(
                        "`{}` carries a builtin that a {} {} cannot have",
                        field.name,
                        match stage {
                            Stage::Vertex => "vertex",
                            Stage::Fragment => "fragment",
                            Stage::Compute => "compute",
                        },
                        if is_input { "input" } else { "output" }
                    ),
                );
            }
        }
    }

    /// Folds a stage's thread extents, requiring them on compute and refusing
    /// them anywhere else.
    fn threads(&mut self, declared: &StageDecl, stage: Stage) -> Option<[u32; 3]> {
        match (stage, declared.threads) {
            (Stage::Compute, None) => {
                self.reporter.error(
                    declared.span,
                    diagnostics::BAD_STAGE,
                    "a compute stage needs `threads(x, y, z)`, which decides its workgroup size",
                );
                None
            }
            (Stage::Vertex | Stage::Fragment, Some(_)) => {
                self.reporter.error(
                    declared.span,
                    diagnostics::BAD_STAGE,
                    "only a compute stage has `threads`",
                );
                None
            }
            (_, None) => None,
            (Stage::Compute, Some(written)) => {
                let mut extents = [0u32; 3];
                for (slot, id) in extents.iter_mut().zip(written) {
                    let Some(value) = self
                        .constant(id, &Type::Scalar(ScalarType::Uint))
                        .and_then(ConstValue::as_extent)
                    else {
                        self.reporter.error(
                            self.tree.expr(id).span(),
                            diagnostics::BAD_STAGE,
                            "a thread extent must be a whole positive number known at compile time",
                        );
                        return None;
                    };
                    *slot = value;
                }
                Some(extents)
            }
        }
    }

    /// Checks one function body against its signature.
    fn function(&mut self, declared: &Function) -> CheckedFunction {
        let name = self.qualified(&self.name(declared.name));
        let signature = self.signature(declared);
        let params: Vec<CheckedParam> = declared
            .params
            .iter()
            .zip(&signature.params)
            .map(|(param, ty)| CheckedParam {
                name: self.name(param.name),
                ty: ty.clone(),
            })
            .collect();

        self.scopes.push(
            params
                .iter()
                .map(|param| (param.name.clone(), param.ty.clone()))
                .collect(),
        );
        self.result = signature.result.clone();
        let body = self.block(&declared.body);
        self.scopes.pop();

        if signature.result != Type::Void && !self.returns_on_every_path(&body) {
            self.reporter.error(
                declared.span,
                diagnostics::MISSING_RETURN,
                format!("`{name}` can finish without returning a value"),
            );
        }
        CheckedFunction {
            name,
            params,
            result: signature.result,
            body,
        }
    }
}
