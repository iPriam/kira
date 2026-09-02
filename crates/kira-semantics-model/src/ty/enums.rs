//! Declared enum shapes and the program's enum table.
//!
//! An enum is a tagged value: one of a fixed set of named variants, each
//! optionally carrying a single payload value. The table is the one owner of
//! enum shapes — the HIR, the IR, and every backend read a variant's tag and
//! its payload type from here rather than carrying their own copy.

use super::Type;

/// Index of a declared enum within an [`EnumTable`].
///
/// Only an [`EnumTable`] mints one, so an id always names a row of the table it
/// came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnumId(u32);

impl EnumId {
    /// This id as an index, for a backend keying its own per-enum data.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// One declared enum: its name and its variants, in declaration order.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    /// The enum's name, as written.
    pub name: String,
    /// The variants, in declaration order. A variant's index is its **tag** —
    /// the discriminant compared by `==` and stored in the runtime value.
    pub variants: Vec<VariantDef>,
}

impl EnumDef {
    /// The tag (declaration index) of the variant named `name`, or `None` when
    /// there is no such variant.
    pub fn variant_index(&self, name: &str) -> Option<u32> {
        self.variants
            .iter()
            .position(|variant| variant.name == name)
            .map(|index| index as u32)
    }

    /// The variant at `tag`, or `None` when out of range.
    pub fn variant(&self, tag: u32) -> Option<&VariantDef> {
        self.variants.get(tag as usize)
    }
}

/// One variant of an [`EnumDef`].
#[derive(Debug, Clone, PartialEq)]
pub struct VariantDef {
    /// The variant's name, as written.
    pub name: String,
    /// The payload's resolved type, or `None` for a payload-less variant.
    ///
    /// A scalar fits the enum box's payload word directly. Strings and nested
    /// enums travel as owned handles; structs travel through an erased aggregate
    /// box with compiler-generated clone/free leaves. Arrays remain refused until
    /// their element callbacks can travel with the payload too.
    pub payload: Option<Type>,
}

/// What a monomorphized enum row was instantiated from.
///
/// A generic enum declares no type of its own: `Result<Int, E>` and
/// `Result<Any, E>` are two ordinary rows of the table with nothing in their
/// shapes to say they came from one template. This is that missing link, and it
/// is what lets one instantiation widen into another — see
/// [`super::TypeTable::admits`].
///
/// The template is named rather than pointed at because a template is not a
/// row: it lives only in the analyzer, and the name is what a use site writes.
#[derive(Debug, Clone, PartialEq)]
pub struct Instantiation {
    /// The generic enum's package-qualified name — `Pkg::Result`, or `Result`
    /// for the program's own template; never `Result<Int, E>`. Qualified so
    /// two packages' same-named templates never read as one.
    pub template: String,
    /// The type arguments substituted in, in declaration order.
    pub arguments: Vec<Type>,
}

/// Every enum a program declares, indexed by [`EnumId`].
///
/// The table is the one owner of enum shapes: the HIR, the IR, and every
/// backend read tags and payload types from here rather than carrying their own
/// copy.
///
/// # Why the name index is keyed by owner
///
/// One program holds every package it depends on, and two packages may each
/// declare a `Color` without either being wrong — the same rule the struct
/// table already follows. So the index is keyed by *owner and name* rather
/// than name alone: both declarations get a row, and deciding which one a file
/// means is the resolver's job, not the table's. An enum with no owner belongs
/// to the program's own files, which share one scope and so share one key
/// space.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EnumTable {
    defs: Vec<EnumDef>,
    // Kept in step with `defs` by `declare_owned`, the only way to add one.
    index: std::collections::HashMap<String, EnumId>,
    // Index-aligned with `defs`, and written by the same one place.
    owners: Vec<Option<String>>,
    // Index-aligned with `defs`: the module inside its package each row was
    // declared in, or empty for a row minted by the compiler.
    modules: Vec<String>,
    // Only the rows a generic template minted appear here, so a hand-written
    // enum is distinguishable from an instantiation by absence.
    instantiations: std::collections::HashMap<EnumId, Instantiation>,
}

/// The index key a declaration sits under.
fn owned_key(owner: Option<&str>, name: &str) -> String {
    match owner {
        Some(owner) => format!("{owner}::{name}"),
        None => name.to_owned(),
    }
}

impl EnumTable {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an enum owned by no package — the program's own.
    pub fn declare(&mut self, def: EnumDef) -> Option<EnumId> {
        self.declare_owned(None, def)
    }

    /// Rewrites every variant payload's type through `visit`.
    ///
    /// Variant names, order, and discriminants are untouched, so the wire form
    /// is exactly what it was: this exists so a payload written at a distinct
    /// type carries the representation once the frontend is done with it.
    pub fn visit_payload_types_mut(&mut self, visit: &dyn Fn(&mut Type)) {
        for def in &mut self.defs {
            for variant in &mut def.variants {
                if let Some(payload) = &mut variant.payload {
                    visit(payload);
                }
            }
        }
    }

    /// Adds an enum `owner` declares, returning its id, or `None` when that
    /// owner already declares the name.
    ///
    /// Rejecting the duplicate here rather than overwriting keeps the name
    /// index and the rows in step: every id resolves, and every name resolves
    /// to the first declaration *of its owner*.
    pub fn declare_owned(&mut self, owner: Option<&str>, def: EnumDef) -> Option<EnumId> {
        let key = owned_key(owner, &def.name);
        if self.index.contains_key(&key) {
            return None;
        }
        let id = EnumId(u32::try_from(self.defs.len()).ok()?);
        self.index.insert(key, id);
        self.defs.push(def);
        self.owners.push(owner.map(str::to_owned));
        self.modules.push(String::new());
        Some(id)
    }

    /// Records the module `id` was declared in.
    pub fn set_module(&mut self, id: EnumId, module: &str) {
        if let Some(slot) = self.modules.get_mut(id.0 as usize) {
            *slot = module.to_owned();
        }
    }

    /// The module `id` was declared in, or empty for a compiler-minted row.
    pub fn module_of(&self, id: EnumId) -> &str {
        self.modules
            .get(id.0 as usize)
            .map_or("", String::as_str)
    }

    /// The enum `name` declares in the program's own files.
    pub fn lookup(&self, name: &str) -> Option<EnumId> {
        self.lookup_owned(None, name)
    }

    /// The enum `owner` declares under `name`, or `None` when it declares
    /// none.
    pub fn lookup_owned(&self, owner: Option<&str>, name: &str) -> Option<EnumId> {
        self.index.get(&owned_key(owner, name)).copied()
    }

    /// The package that declared the enum at `id`, or `None` for one of the
    /// program's own files.
    ///
    /// `None` for an id this table never minted, matching how every other
    /// out-of-range read here answers rather than erroring.
    pub fn owner_of(&self, id: EnumId) -> Option<&str> {
        self.owners.get(id.0 as usize).and_then(Option::as_deref)
    }

    /// The definition behind an id.
    pub fn get(&self, id: EnumId) -> Option<&EnumDef> {
        self.defs.get(id.0 as usize)
    }

    /// Replaces the variants of an enum declared as an empty header.
    ///
    /// Construct-family enums are cyclic with their backed structs: the family
    /// type must exist before those structs resolve fields, while each variant's
    /// payload is one of those structs. Declaring the empty header first and
    /// filling it once all struct ids exist breaks that registration cycle
    /// without exposing an incomplete shape downstream.
    pub fn set_variants(&mut self, id: EnumId, variants: Vec<VariantDef>) -> bool {
        match self.defs.get_mut(id.0 as usize) {
            Some(def) => {
                def.variants = variants;
                true
            }
            None => false,
        }
    }

    /// Records that `id` was minted by instantiating a generic template.
    ///
    /// Returns `false` for an id this table never minted, which keeps the note
    /// and the rows from drifting apart the way a blind insert would.
    pub fn record_instantiation(&mut self, id: EnumId, instantiation: Instantiation) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        self.instantiations.insert(id, instantiation);
        true
    }

    /// What `id` was instantiated from, or `None` for a hand-written enum.
    pub fn instantiation(&self, id: EnumId) -> Option<&Instantiation> {
        self.instantiations.get(&id)
    }

    /// The generic enum `id` is an instantiation of, or `None` when it is not
    /// one.
    pub fn template_of(&self, id: EnumId) -> Option<&str> {
        self.instantiations
            .get(&id)
            .map(|from| from.template.as_str())
    }

    /// Every declared enum, in declaration order.
    pub fn defs(&self) -> &[EnumDef] {
        &self.defs
    }

    /// Every declared enum id, in declaration order.
    pub fn ids(&self) -> impl Iterator<Item = EnumId> + '_ {
        (0..self.defs.len()).map(|index| EnumId(index as u32))
    }

    /// How many enums the program declares.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Whether the program declares no enums.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Whether every variant is payload-less, so the value is its tag alone.
    ///
    /// This is the enum a `match` compiles to a tag compare and nothing else:
    /// `GraphicsBackend`, `Color`, a state machine's states. It owns nothing,
    /// so it needs no clone, no free, and no layout beyond one integer — which
    /// is what lets it cross the `@Native`/`@Runtime` boundary where an enum
    /// carrying a payload cannot.
    ///
    /// An enum with no variants at all answers `true`: it has no variant that
    /// carries anything. No value of it can exist to cross, so the answer is
    /// unreachable rather than wrong.
    pub fn is_fieldless(&self, id: EnumId) -> bool {
        self.get(id)
            .is_some_and(|def| def.variants.iter().all(|variant| variant.payload.is_none()))
    }

    /// Whether any variant carries a payload represented by owned heap storage.
    ///
    /// Strings and nested enums are handles, and a struct payload gets its own
    /// erased aggregate allocation even when all of that struct's fields are
    /// scalar. A payload-less enum or scalar-only payload owns nothing beyond
    /// the enum box itself.
    pub fn owns_heap_payload(&self, id: EnumId) -> bool {
        self.get(id).is_some_and(|def| {
            def.variants.iter().any(|variant| {
                matches!(
                    variant.payload,
                    Some(
                        Type::String
                            | Type::Array(_)
                            | Type::Enum(_)
                            | Type::Struct(_)
                            | Type::Any
                            | Type::Cell(_)
                    )
                )
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_with_color() -> (EnumTable, EnumId) {
        let mut table = EnumTable::new();
        let id = table
            .declare(EnumDef {
                name: "Color".to_owned(),
                variants: vec![
                    VariantDef {
                        name: "Red".to_owned(),
                        payload: None,
                    },
                    VariantDef {
                        name: "Labelled".to_owned(),
                        payload: Some(Type::String),
                    },
                ],
            })
            .expect("a fresh name declares");
        (table, id)
    }

    /// A payload on any one variant is what makes the whole enum carry one.
    ///
    /// The distinction the hybrid seam turns on: a tag-only enum is an integer
    /// and crosses, and one variant with a payload makes the value a tag plus
    /// something owned, which does not fit one word.
    #[test]
    fn an_enum_is_fieldless_only_when_no_variant_carries_a_payload() {
        let (mixed, mixed_id) = table_with_color();
        assert!(
            !mixed.is_fieldless(mixed_id),
            "one payload-carrying variant is enough"
        );

        let mut table = EnumTable::new();
        let id = table
            .declare(EnumDef {
                name: "Backend".to_owned(),
                variants: vec![
                    VariantDef {
                        name: "Vm".to_owned(),
                        payload: None,
                    },
                    VariantDef {
                        name: "Native".to_owned(),
                        payload: None,
                    },
                ],
            })
            .expect("a fresh name declares");
        assert!(table.is_fieldless(id));
        assert!(
            !table.owns_heap_payload(id),
            "a fieldless enum owns nothing either"
        );
    }

    #[test]
    fn a_variant_tag_is_its_declaration_index() {
        let (table, id) = table_with_color();
        let def = table.get(id).expect("the id resolves");
        assert_eq!(def.variant_index("Red"), Some(0));
        assert_eq!(def.variant_index("Labelled"), Some(1));
        assert_eq!(def.variant_index("Green"), None);
        assert!(def.variant(0).expect("Red").payload.is_none());
        assert_eq!(
            def.variant(1).expect("Labelled").payload,
            Some(Type::String)
        );
    }

    #[test]
    fn a_duplicate_name_is_rejected_rather_than_overwriting() {
        let (mut table, id) = table_with_color();
        let again = table.declare(EnumDef {
            name: "Color".to_owned(),
            variants: Vec::new(),
        });
        assert_eq!(again, None);
        assert_eq!(table.lookup("Color"), Some(id));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn only_an_instantiated_row_names_a_template() {
        let (mut table, color) = table_with_color();
        assert_eq!(table.template_of(color), None);
        assert!(table.record_instantiation(
            color,
            Instantiation {
                template: "Result".to_owned(),
                arguments: vec![Type::INT],
            }
        ));
        assert_eq!(table.template_of(color), Some("Result"));
        assert_eq!(
            table
                .instantiation(color)
                .map(|from| from.arguments.clone()),
            Some(vec![Type::INT])
        );
    }

    #[test]
    fn a_note_for_a_row_this_table_never_minted_is_refused() {
        let mut table = EnumTable::new();
        let mut elsewhere = EnumTable::new();
        let other = elsewhere
            .declare(EnumDef {
                name: "Elsewhere".to_owned(),
                variants: Vec::new(),
            })
            .expect("a fresh table accepts the first declaration");
        assert!(!table.record_instantiation(
            other,
            Instantiation {
                template: "Result".to_owned(),
                arguments: Vec::new(),
            }
        ));
        assert_eq!(table.instantiation(other), None);
    }

    #[test]
    fn a_string_payload_owns_heap_but_a_scalar_one_does_not() {
        let (table, id) = table_with_color();
        assert!(table.owns_heap_payload(id));

        let mut scalars = EnumTable::new();
        let axis = scalars
            .declare(EnumDef {
                name: "Axis".to_owned(),
                variants: vec![
                    VariantDef {
                        name: "Horizontal".to_owned(),
                        payload: None,
                    },
                    VariantDef {
                        name: "At".to_owned(),
                        payload: Some(Type::INT),
                    },
                ],
            })
            .expect("declares");
        assert!(!scalars.owns_heap_payload(axis));
    }
}
