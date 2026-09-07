use super::super::TypeRefId;
use kira_core::Symbol;
use kira_source::Span;

/// One `key: value;` field inside an `@FFI.Extern { ... }` or
/// `@FFI.Syscall { ... }` block.
///
/// Both the key (`library`, `symbol`, `abi`, `name`) and the value are written
/// as bare identifiers, so both are interned symbols. What each key means — and
/// which values a key accepts — is the analyzer's to decide; the parser only
/// records the `identifier : identifier ;` shape and the spans, so a later
/// refusal can point at the exact token the author wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignField {
    /// The field's key (`library`, `symbol`, `abi`, `name`).
    pub key: Symbol,
    /// Span of the key token, for diagnostics.
    pub key_span: Span,
    /// The field's value, written as a bare identifier (`kira_ffi_add`, `c`,
    /// `write`).
    pub value: Symbol,
    /// Span of the value token, for diagnostics.
    pub value_span: Span,
}

/// Where a bodyless function's implementation comes from.
///
/// Both forms declare the same thing — a function Kira calls but does not
/// contain — and differ only in what has to be named to reach it. That is why
/// one mark carries both: the signature rules, the arity check, the call
/// resolution, and the refusal of a body are one question with one answer, and
/// splitting them would be two places for the same rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignKind {
    /// `@FFI.Extern` — a C symbol in a named native library.
    Extern,
    /// `@FFI.Address` — the ADDRESS of a data symbol a native library exports,
    /// answered by a nullary function.
    ///
    /// C libraries export data as well as functions: interface tables, sentinel
    /// objects, version constants, `stdin`. A bodyless *function* cannot reach
    /// one, and a shim written to hand it back is glue that exists only because
    /// the boundary could not say it.
    ///
    /// It is a function rather than a binding because Kira has no globals, and
    /// inventing them for this would be a language-shaped hole opened by one C
    /// convention. A nullary call reads the same and costs the same: the address
    /// is a link-time constant either way.
    ///
    /// The answer is the symbol's ADDRESS, never its value. Address-of is the
    /// one reading that works for every symbol alike -- an opaque struct has no
    /// value this side can hold, a mutable global read once would be a stale
    /// copy, and a width read from the declaration would have to agree with C's.
    /// Reading through the address is what `@FFI.Pointer` is already for.
    Address,
    /// `@FFI.Syscall` — a Linux system call, named the way `man 2` names it.
    ///
    /// It carries no `library`, `symbol`, or `abi`, because the kernel is not a
    /// library: there is nothing to load, nothing to look a name up in, and one
    /// calling convention. What it carries instead is the call's name, and the
    /// compiler owns the number that name resolves to — a number written in Kira
    /// source could not be right on two architectures at once.
    Syscall,
}

impl ForeignKind {
    /// The annotation as an author wrote it, for a diagnostic that has to name
    /// the form it is refusing.
    pub const fn annotation(self) -> &'static str {
        match self {
            Self::Extern => "@FFI.Extern",
            Self::Address => "@FFI.Address",
            Self::Syscall => "@FFI.Syscall",
        }
    }
}

/// The parsed `@FFI.Extern { ... }` or `@FFI.Syscall { ... }` annotation on a
/// bodyless function.
///
/// New Kira design: the oracle has no seamless C-FFI. The mark records which
/// form was written, the annotation name's span, the block's span, and the
/// `key: value;` fields as written — nothing is validated here. The analyzer
/// reads the fields, checks the signature, and either mints a foreign callable
/// or refuses the whole declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignMark {
    /// Which of the two bodyless forms was written.
    pub kind: ForeignKind,
    /// Span of the qualified annotation name (`FFI.Extern`, `FFI.Syscall`).
    pub span: Span,
    /// Span covering the whole `{ ... }` block.
    pub block_span: Span,
    /// The fields the block wrote, in source order.
    pub fields: Vec<ForeignField>,
}

/// A `@FFI.*` annotation on a *struct* declaration — every member of the family
/// except `@FFI.Extern`, which rides a function instead.
///
/// The five struct-attached forms each declare a *type* whose real shape the
/// annotation carries: `@FFI.Struct` a C-layout struct, `@FFI.Pointer` a native
/// pointer alias, `@FFI.Alias` a plain typedef, `@FFI.Array` an inline
/// fixed-size C array, and `@FFI.Callback` a function-pointer typedef. The
/// parser records the shape; the analyzer resolves the referenced types and
/// decides what each becomes.
#[derive(Debug, Clone, PartialEq)]
pub struct FfiTypeMark {
    /// Which of the five struct-attached `@FFI.*` forms this is, with its
    /// parsed arguments.
    pub kind: FfiTypeKind,
    /// Span of the qualified annotation name (`FFI.Struct`, `FFI.Pointer`, …).
    pub name_span: Span,
    /// Span covering the whole `{ ... }` block.
    pub block_span: Span,
}

/// The five struct-attached `@FFI.*` forms, each with the arguments its block
/// carried. A required argument the block omitted is recorded as `None`/empty,
/// so the analyzer reports the omission against the block rather than the parser
/// bailing.
#[derive(Debug, Clone, PartialEq)]
pub enum FfiTypeKind {
    /// `@FFI.Struct { layout: c; }` — a struct laid out by C rules. The
    /// declaration's own body carries the fields; this only records `layout`.
    Struct {
        /// The `layout` value as written (`c`), and its span.
        layout: Option<(Symbol, Span)>,
    },
    /// `@FFI.Pointer { target: Target; ownership: o; }` — a native pointer alias.
    Pointer {
        /// The written pointee type, when present.
        target: Option<TypeRefId>,
        /// The `ownership` value as written (`borrowed`), and its span.
        ownership: Option<(Symbol, Span)>,
    },
    /// `@FFI.Alias { target: Target; }` — a plain typedef of one type to another.
    Alias {
        /// The written aliased type, when present.
        target: Option<TypeRefId>,
    },
    /// `@FFI.Array { element: E; count: N; }` — an inline fixed-size C array.
    Array {
        /// The written element type, when present.
        element: Option<TypeRefId>,
        /// The written element count and its span, when present.
        count: Option<(i64, Span)>,
    },
    /// `@FFI.Callback { abi: c; params: [ParamType, …]; result: ResultType; }` — a
    /// function-pointer typedef.
    Callback {
        /// The `abi` value as written (`c`), and its span.
        abi: Option<(Symbol, Span)>,
        /// The written parameter types, in order; empty for `params: []`.
        params: Vec<TypeRefId>,
        /// The written result type, when present.
        result: Option<TypeRefId>,
    },
}

impl FfiTypeKind {
    /// A short label naming the form, for diagnostics (`Struct`, `Pointer`, …).
    pub fn label(&self) -> &'static str {
        match self {
            FfiTypeKind::Struct { .. } => "Struct",
            FfiTypeKind::Pointer { .. } => "Pointer",
            FfiTypeKind::Alias { .. } => "Alias",
            FfiTypeKind::Array { .. } => "Array",
            FfiTypeKind::Callback { .. } => "Callback",
        }
    }
}
