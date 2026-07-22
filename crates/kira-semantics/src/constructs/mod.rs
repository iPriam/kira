//! Executable construct declaration families and heterogeneous `Any Family` values.
//!
//! A construct-backed declaration remains a class-shaped struct. Its construction
//! inputs and stored members are fields, computed members are zero-argument
//! methods, and ordinary members are methods. A construct family additionally
//! becomes a synthesized enum whose variants carry those concrete structs:
//!
//! ```text
//! Any Widget = Text(Text) | VStack(VStack) | Button(Button) | ...
//! ```
//!
//! The enum is declared as an empty header before structs resolve and filled once
//! every backed struct id exists. That two-phase registration is what permits a
//! backed struct to hold `Any Widget` while `Any Widget` carries that backed
//! struct. Calls on the family value become synthesized tag dispatchers, so every
//! backend executes ordinary enum projection, branching, and direct calls.

use std::collections::{BTreeMap, HashSet};

use kira_core::Symbol;
use kira_semantics_model::hir::FuncId;
use kira_semantics_model::{EnumId, OwnershipMode, StructId, Type};
use kira_source::SourceId;
use kira_syntax_model::ast::Function;

mod collection;
mod construction;
mod dispatch;
mod extend;

/// Everything analysis remembers about one construct-backed declaration beyond
/// its struct shape.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConstructInfo {
    /// The number of leading struct fields that are construction params.
    pub(crate) param_count: usize,
    /// Computed members read as properties rather than fields.
    pub(crate) computed: HashSet<String>,
    /// Child slots filled from construction trailing content.
    pub(crate) slots: Vec<ContentSlot>,
    /// The heterogeneous family variant this concrete struct wraps into.
    pub(crate) family: Option<(EnumId, u32)>,
}

/// One child slot of a construct-backed declaration.
#[derive(Debug, Clone)]
pub(crate) struct ContentSlot {
    /// The slot field's index in the struct's fields.
    pub(crate) field_index: u32,
    /// The slot field's name (its channel name).
    pub(crate) name: String,
    /// Whether the slot holds an ordered list rather than exactly one child.
    pub(crate) list: bool,
    /// The element type each child must satisfy.
    pub(crate) element_ty: Type,
    /// The slot field's stored type.
    pub(crate) field_ty: Type,
}

/// One concrete variant of a construct family.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConstructVariant {
    /// The concrete construct-backed struct.
    pub(crate) struct_id: StructId,
    /// Its declaration-order tag in the synthesized family enum.
    pub(crate) tag: u32,
}

/// One method exposed by a construct family.
#[derive(Debug, Clone)]
pub(crate) struct ConstructFamilyMethod<'a> {
    /// The family declaration's method syntax, reused for inherited bodies.
    pub(crate) function: &'a Function,
    /// The file the family declaration belongs to.
    pub(crate) source: SourceId,
    /// Whether reads use property syntax rather than a call.
    pub(crate) computed: bool,
    /// Resolved written parameters, excluding the receiver.
    pub(crate) params: Vec<Type>,
    /// Written parameter names, aligned with [`Self::params`].
    pub(crate) param_names: Vec<Option<Symbol>>,
    /// Parameter ownership modes, aligned with [`Self::params`].
    pub(crate) ownership: Vec<OwnershipMode>,
    /// Resolved result type.
    pub(crate) result: Type,
    /// Whether this is a **uniform** modifier from an `extend` block: one shared
    /// body whose receiver is the family value, rather than a per-variant method
    /// every concrete declaration implements. A uniform method is never
    /// conformance-checked against the variants and is called directly, so
    /// [`Self::dispatcher`] holds its single body rather than a tag dispatcher.
    pub(crate) uniform: bool,
    /// For a per-variant method, the synthesized dynamic dispatcher (reserved on
    /// first use). For a uniform `extend` modifier, its single body (reserved up
    /// front so an uncalled modifier is still checked and lowered).
    pub(crate) dispatcher: Option<FuncId>,
}

/// One construct family's type, conformance surface, and concrete variants.
#[derive(Debug, Clone)]
pub(crate) struct ConstructFamilyInfo<'a> {
    /// The synthesized `Any Family` enum.
    pub(crate) enum_id: EnumId,
    /// Required stored or computed member names.
    pub(crate) required: Vec<String>,
    /// Methods inherited by concrete declarations and dynamically dispatched.
    pub(crate) methods: BTreeMap<String, ConstructFamilyMethod<'a>>,
    /// Concrete backed declarations in source order.
    pub(crate) variants: Vec<ConstructVariant>,
}

impl crate::analyze::Analyzer<'_> {
    /// The struct a construct-backed declaration named `name` became.
    pub(crate) fn construct_backed_named(&self, name: &str) -> Option<StructId> {
        let id = self.program.types.structs().lookup(name)?;
        self.constructs.contains_key(&id).then_some(id)
    }

    /// Whether `name` is a computed property of construct-backed `id`.
    pub(crate) fn construct_computed_member(&self, id: StructId, name: &str) -> bool {
        self.constructs
            .get(&id)
            .is_some_and(|info| info.computed.contains(name))
    }

    /// The number of leading construction-parameter fields of `id`.
    pub(crate) fn construct_param_count(&self, id: StructId) -> usize {
        self.constructs
            .get(&id)
            .map(|info| info.param_count)
            .unwrap_or_default()
    }
}
