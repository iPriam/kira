//! The export surface a library module offers its consumer.
//!
//! A `@Export` function is the one thing in a Kira library a Rust consumer can
//! name, and [`ExportTable`] is how a compiled module says so: the consumer name,
//! the function it resolves to, and the shape of the call. It is a *serialized*
//! table rather than something regenerated from source, because the consumer's
//! generated wrapper is built once and then checked against the artifact it was
//! built from — data is the only guard available on that side, so the data has to
//! be in the module.
//!
//! # Why the section is appended rather than woven in
//!
//! KBC1 is append-only. This section sits after the function table, at the end of
//! the stream, so a module written before exports existed decodes as exactly what
//! it is: a module with no exports. Nothing about the existing bytes moves.
//!
//! # What a handle is doing in a bytecode module
//!
//! An exported class's instances cross to the consumer as opaque handles
//! ([`BridgeValueTag::HANDLE`]), and a handle is worth nothing without knowing
//! *which* class it denotes — the consumer mints one Rust newtype per exported
//! class, and a handle typed only as "a word" would let a `Button` be passed
//! where a `Window` was wanted. So the table carries a class list, and a handle
//! type indexes it.

use kira_ir::IrProgram;
use kira_runtime_abi::BridgeValueTag;
use kira_semantics_model::Type;

use crate::compile::CompileError;

/// The `@Export` surface of one compiled module.
///
/// Empty for an application, and for a library that exports nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExportTable {
    /// Every class marked `@Export`, by name, in declaration order.
    ///
    /// A handle type indexes this list. The names are the consumer's newtype
    /// names, and are the only reason a handle is more than an untyped word.
    pub classes: Vec<String>,
    /// Every exported function, in declaration order.
    pub functions: Vec<ModuleExport>,
}

impl ExportTable {
    /// Whether this module exports nothing at all.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty() && self.functions.is_empty()
    }
}

/// One function a library offers its consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleExport {
    /// The name the consumer calls it by (`make_button`).
    ///
    /// Derived by snake_casing the Kira name in the frontend; two exports may
    /// never map onto one of these, which [`crate::Module::validate`] rechecks
    /// because a module is a public artifact and not necessarily one this
    /// compiler wrote.
    pub name: String,
    /// The name the Kira author wrote (`makeButton`), for diagnostics.
    pub kira_name: String,
    /// Index of the exported function in [`crate::Module::functions`].
    pub function: u32,
    /// The parameter types, in order; one per parameter the function declares.
    pub params: Vec<ExportType>,
    /// The result type ([`ExportType::Void`] when the function returns nothing).
    pub result: ExportType,
}

/// A type that may cross the export boundary.
///
/// A closed, Kira-owned Rust enum rather than a raw tag byte: it is produced
/// only after the byte has been checked, exactly as `BridgeData` is. The wire
/// spelling is a [`BridgeValueTag`] plus a class index, and the tags that never
/// travel (struct, array, enum) have no variant here at all — the frontend
/// refuses them, and a decoder rejects an artifact that claims otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportType {
    /// No value.
    Void,
    /// A 64-bit signed integer. Every Kira integer width crosses as one.
    Int,
    /// A 64-bit float.
    Float,
    /// A boolean.
    Bool,
    /// A string. Lent in, owned out — see the boundary contract.
    String,
    /// An opaque handle to an instance of an exported class.
    Handle {
        /// Index into [`ExportTable::classes`].
        class: u32,
    },
}

impl ExportType {
    /// The wire tag this type is written as.
    pub fn tag(self) -> BridgeValueTag {
        match self {
            ExportType::Void => BridgeValueTag::VOID,
            ExportType::Int => BridgeValueTag::INT,
            ExportType::Float => BridgeValueTag::FLOAT,
            ExportType::Bool => BridgeValueTag::BOOL,
            ExportType::String => BridgeValueTag::STRING,
            ExportType::Handle { .. } => BridgeValueTag::HANDLE,
        }
    }

    /// The class index written alongside the tag; zero for every non-handle.
    pub fn class_index(self) -> u32 {
        match self {
            ExportType::Handle { class } => class,
            _ => 0,
        }
    }

    /// Reads a type back from its wire spelling, or `None` when the tag names
    /// nothing that may cross.
    ///
    /// A tag that names a type but never travels (struct, array, enum) is
    /// `None` here just as an unknown byte is: neither can be honored, and
    /// guessing at either is guessing at ownership.
    pub fn from_wire(tag: BridgeValueTag, class: u32) -> Option<ExportType> {
        let ty = match tag {
            BridgeValueTag::VOID => ExportType::Void,
            BridgeValueTag::INT => ExportType::Int,
            BridgeValueTag::FLOAT => ExportType::Float,
            BridgeValueTag::BOOL => ExportType::Bool,
            BridgeValueTag::STRING => ExportType::String,
            BridgeValueTag::HANDLE => return Some(ExportType::Handle { class }),
            _ => return None,
        };
        // A class index on a non-handle is a reserved field carrying data, which
        // means the writer meant something this reader does not understand.
        (class == 0).then_some(ty)
    }
}

/// Builds the export table a compiled module carries, from the lowered program.
///
/// # Which classes end up in the class list
///
/// The ones a signature actually mentions, in first-mention order — not every
/// class the author marked `@Export`. A class no exported signature names cannot
/// be obtained or passed by a consumer (v1 exports functions, never methods), so
/// a row for it would name a type the consumer can never hold. Deriving the list
/// from use keeps the table and the signatures that index it consistent by
/// construction: there is no way to write a handle type pointing at a class the
/// table does not have.
pub(crate) fn build_export_table(program: &IrProgram) -> Result<ExportTable, CompileError> {
    let mut classes: Vec<String> = Vec::new();
    let mut functions = Vec::with_capacity(program.exports.len());
    for export in &program.exports {
        let mut params = Vec::with_capacity(export.params.len());
        for ty in &export.params {
            params.push(export_type(
                program,
                *ty,
                &export.exported_name,
                &mut classes,
            )?);
        }
        let result = export_type(program, export.result, &export.exported_name, &mut classes)?;
        functions.push(ModuleExport {
            name: export.exported_name.clone(),
            kira_name: export.kira_name.clone(),
            function: export.function,
            params,
            result,
        });
    }
    Ok(ExportTable { classes, functions })
}

/// Maps one resolved type onto its crossing form, interning a class as needed.
///
/// Every struct type here is a class: which structs are handle-eligible is the
/// frontend's decision, made against the `@Export` markers, and this list only
/// contains signatures that already passed it.
///
/// A type that cannot cross is an error rather than a substitution: the frontend
/// refuses every one of them ahead of this, so reaching here means the compiler
/// and the analyzer disagree — which is worth saying loudly, not papering over
/// with some other tag.
fn export_type(
    program: &IrProgram,
    ty: Type,
    export: &str,
    classes: &mut Vec<String>,
) -> Result<ExportType, CompileError> {
    Ok(match ty {
        Type::Void => ExportType::Void,
        // Every Kira integer width crosses as one 64-bit word; the consumer's
        // wrapper narrows on its side, as the seam already does.
        Type::Int(_) => ExportType::Int,
        Type::Float(_) => ExportType::Float,
        Type::Bool => ExportType::Bool,
        Type::String => ExportType::String,
        Type::Struct(id) => {
            let name = program.types.type_name(Type::Struct(id));
            let class = match classes.iter().position(|known| known == &name) {
                Some(index) => index,
                None => {
                    classes.push(name);
                    classes.len() - 1
                }
            };
            ExportType::Handle {
                class: class as u32,
            }
        }
        // `RawPtr` and `CString` are the C-FFI seam types. `CString` is
        // seam-only and never reaches an export; a `RawPtr` export is not part
        // of the export surface this milestone pins, so both are refused here
        // rather than given an export wire spelling that would have to be
        // supported forever.
        // `Any` joins them for a different reason — the analyzer's `KSEM186`
        // already refused it, because a consumer's wrapper cannot name an
        // erased type — but the outcome here is the same: no wire spelling is
        // invented for a type that never legally reaches this point.
        // A capture cell joins them because it is shared mutable storage this
        // runtime counts holds on; the analyzer already refused it (`KSEM186`),
        // and inventing a wire spelling for one would be inventing a way for a
        // consumer to hold a share nobody releases.
        Type::Array(_)
        | Type::Enum(_)
        | Type::RawPtr
        | Type::CString
        | Type::Any
        | Type::NativeState(_)
        | Type::Task(_)
        | Type::Cell(_)
        | Type::Error => {
            return Err(CompileError::UncrossableExport {
                export: export.to_owned(),
                ty: program.types.type_name(ty),
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_crossing_type_round_trips_through_its_wire_spelling() {
        for ty in [
            ExportType::Void,
            ExportType::Int,
            ExportType::Float,
            ExportType::Bool,
            ExportType::String,
            ExportType::Handle { class: 0 },
            ExportType::Handle { class: 7 },
        ] {
            assert_eq!(
                ExportType::from_wire(ty.tag(), ty.class_index()),
                Some(ty),
                "round trip failed for {ty:?}"
            );
        }
    }

    #[test]
    fn a_type_that_never_travels_is_rejected_rather_than_read() {
        for tag in [
            BridgeValueTag::STRUCT,
            BridgeValueTag::ARRAY,
            BridgeValueTag::ENUM,
            BridgeValueTag(200),
        ] {
            assert_eq!(ExportType::from_wire(tag, 0), None, "{tag:?} must not read");
        }
    }

    #[test]
    fn a_class_index_on_a_non_handle_is_rejected() {
        // The field is reserved for handles. A writer that filled it meant
        // something this reader cannot honor, so it says so.
        assert_eq!(ExportType::from_wire(BridgeValueTag::INT, 1), None);
        assert_eq!(ExportType::from_wire(BridgeValueTag::VOID, 3), None);
    }

    #[test]
    fn an_empty_table_is_what_a_module_without_exports_has() {
        assert!(ExportTable::default().is_empty());
        assert!(
            !ExportTable {
                classes: vec!["Button".to_owned()],
                functions: Vec::new(),
            }
            .is_empty()
        );
    }
}
