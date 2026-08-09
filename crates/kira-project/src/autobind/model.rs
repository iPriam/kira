//! What a header's C surface becomes: the declarations a generated binding
//! file holds, before any of it is text.
//!
//! Harvesting fills this in and emission renders it, so the two can be tested
//! apart — one against a header, the other against a value. Names are `String`
//! rather than `kira_core::Symbol` for the same reason the native-library model
//! beside it spells them that way: they arrive from a C header, are written out
//! once, and never enter a compiler tree.

/// A Kira type as a generated binding writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KiraType {
    /// `Void` — a result only.
    Void,
    /// `Bool`, C's `_Bool`.
    Bool,
    /// One of the fixed-width integer spellings, written as-is.
    Int(&'static str),
    /// `F32`, C's `float`.
    F32,
    /// `F64`, C's `double`.
    F64,
    /// `CString`, a `const char *`.
    CString,
    /// `RawPtr`, a `void *`.
    RawPtr,
    /// A type this binding declares: a struct, an array typedef, a callback
    /// typedef, or a pointer alias.
    Named(String),
}

impl KiraType {
    /// How this type is written in a binding.
    pub fn spelling(&self) -> &str {
        match self {
            Self::Void => "Void",
            Self::Bool => "Bool",
            Self::Int(spelling) => spelling,
            Self::F32 => "F32",
            // `Float` is the 64-bit float; there is no `F64` spelling.
            Self::F64 => "Float",
            Self::CString => "CString",
            Self::RawPtr => "RawPtr",
            Self::Named(name) => name,
        }
    }

    /// Whether this type is a scalar the callback seam carries.
    ///
    /// A callback's parameters and result are limited to fixed-width scalars,
    /// `Bool`, and `RawPtr` — an aggregate across a function pointer is not
    /// part of the seam, so a signature naming one is not emitted as a callback
    /// at all rather than emitted and refused at every use.
    pub fn is_callback_scalar(&self) -> bool {
        matches!(
            self,
            Self::Void | Self::Bool | Self::Int(_) | Self::F32 | Self::F64 | Self::RawPtr
        )
    }
}

/// An opaque C type: named by the headers, never defined by them.
///
/// Emitted as `@FFI.Alias { target: <name>; } struct <name> {}` so a pointer to
/// it has a target to name, which is what makes `<name>_ptr` read as the C type
/// it points at rather than as an anonymous word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpaqueDecl {
    /// The C type's name, used for the alias and its target.
    pub name: String,
}

/// A `@FFI.Pointer` alias: one C pointer type, by what it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerDecl {
    /// The alias name, always `<target>_ptr`.
    pub name: String,
    /// What C says it points at, as written.
    pub target: String,
}

/// An `@FFI.Array` typedef: storage a C struct reserves inline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayDecl {
    /// The typedef name, always `<element>_array_<count>`.
    pub name: String,
    /// What one element is.
    pub element: KiraType,
    /// How many elements C reserves.
    pub count: u64,
}

/// An `@FFI.Callback` typedef: one C function-pointer signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackDecl {
    /// The typedef name.
    pub name: String,
    /// The parameter types, in order.
    pub params: Vec<KiraType>,
    /// What the function pointer returns.
    pub result: KiraType,
}

/// One field of a C-layout struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    /// The field name, as C writes it.
    pub name: String,
    /// The field's type.
    pub field_type: KiraType,
}

/// An `@FFI.Struct { layout: c }`: a C struct, field for field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDecl {
    /// The struct's name, as C writes it.
    pub name: String,
    /// Its fields, in declaration order.
    pub fields: Vec<FieldDecl>,
}

/// One parameter of a bound C function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamDecl {
    /// The parameter name, as C writes it or as generated when C leaves it out.
    pub name: String,
    /// The parameter's type.
    pub param_type: KiraType,
}

/// An `@FFI.Extern` function: one C symbol, callable from Kira.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDecl {
    /// The C symbol, which is also the Kira function's name.
    pub symbol: String,
    /// Its parameters, in order.
    pub params: Vec<ParamDecl>,
    /// What it returns.
    pub result: KiraType,
}

/// One declaration the headers hold that this seam cannot carry.
///
/// Recorded rather than dropped: a generated binding lists what it left out and
/// why, so a missing function reads as a decision with a reason instead of as
/// an unexplained gap between the header and the binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedDecl {
    /// What C called it.
    pub name: String,
    /// Why it could not be bound, in one line.
    pub reason: String,
}

/// Everything one native library's headers contribute to one binding file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingModule {
    /// The native library every `@FFI.Extern` here names.
    pub library: String,
    /// Opaque C types referenced by pointer.
    pub opaques: Vec<OpaqueDecl>,
    /// Inline array typedefs.
    pub arrays: Vec<ArrayDecl>,
    /// C-layout structs.
    pub structs: Vec<StructDecl>,
    /// Function-pointer typedefs.
    pub callbacks: Vec<CallbackDecl>,
    /// Pointer aliases.
    pub pointers: Vec<PointerDecl>,
    /// Bound functions.
    pub functions: Vec<FunctionDecl>,
    /// Declarations the seam cannot carry, with the reason for each.
    pub skipped: Vec<SkippedDecl>,
}

impl BindingModule {
    /// Sorts every group by name, so one header always produces one file.
    ///
    /// Declaration order in a C header is not stable across a refactor that
    /// changes nothing a binding cares about, and an unstable generated file
    /// invalidates its own cache and shows up as noise in a diff. Sorting is
    /// safe because Kira resolves struct names in a pass of their own: a field
    /// may name a type declared further down.
    pub fn sort(&mut self) {
        self.opaques.sort_by(|a, b| a.name.cmp(&b.name));
        self.arrays.sort_by(|a, b| a.name.cmp(&b.name));
        self.structs.sort_by(|a, b| a.name.cmp(&b.name));
        self.callbacks.sort_by(|a, b| a.name.cmp(&b.name));
        self.pointers.sort_by(|a, b| a.name.cmp(&b.name));
        self.functions.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        self.skipped.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// How many declarations this binding carries, functions and types alike.
    pub fn declaration_count(&self) -> usize {
        self.opaques.len()
            + self.arrays.len()
            + self.structs.len()
            + self.callbacks.len()
            + self.pointers.len()
            + self.functions.len()
    }
}
