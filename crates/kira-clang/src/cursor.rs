//! Safe cursors and types over a parsed translation unit.
//!
//! A [`Cursor`] and a [`CType`] are plain by-value libclang handles paired with
//! the [`Api`] they came from. Both are `Copy`, both borrow the translation
//! unit that owns them, and neither can be constructed from outside this crate
//! — which is the invariant every `unsafe` accessor on `Api` rests on.

use crate::api::{Api, ChildVisitResult, CursorKind, CxClientData, CxCursor, CxType, TypeKind};

/// One declaration in a parsed translation unit.
#[derive(Clone, Copy)]
pub struct Cursor<'a> {
    raw: CxCursor,
    api: &'a Api,
}

impl std::fmt::Debug for Cursor<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Cursor")
            .field("kind", &self.kind().0)
            .field("name", &self.name())
            .finish()
    }
}

impl<'a> Cursor<'a> {
    /// Pairs a raw cursor with the library that produced it.
    pub(crate) fn new(raw: CxCursor, api: &'a Api) -> Self {
        Self { raw, api }
    }

    /// What this cursor declares.
    pub fn kind(&self) -> CursorKind {
        CursorKind(self.raw.kind)
    }

    /// The name this cursor declares, empty for an anonymous one.
    pub fn name(&self) -> String {
        self.api.cursor_spelling(self.raw)
    }

    /// The file this cursor was written in, empty for a compiler built-in.
    pub fn file(&self) -> String {
        self.api.cursor_file(self.raw)
    }

    /// The type this cursor declares.
    pub fn c_type(&self) -> CType<'a> {
        CType::new(self.api.cursor_type(self.raw), self.api)
    }

    /// The result type of a function cursor.
    pub fn result_type(&self) -> CType<'a> {
        CType::new(self.api.cursor_result_type(self.raw), self.api)
    }

    /// What a `typedef` cursor is a typedef of.
    pub fn typedef_underlying_type(&self) -> CType<'a> {
        CType::new(self.api.typedef_underlying_type(self.raw), self.api)
    }

    /// The integer type an `enum` cursor is represented as.
    pub fn enum_integer_type(&self) -> CType<'a> {
        CType::new(self.api.enum_integer_type(self.raw), self.api)
    }

    /// The value an enumerator declares.
    pub fn enum_constant_value(&self) -> i64 {
        self.api.enum_constant_value(self.raw)
    }

    /// Whether this function cursor takes a trailing `...`.
    pub fn is_variadic(&self) -> bool {
        self.api.cursor_is_variadic(self.raw)
    }

    /// Whether this field cursor declares a bitfield.
    pub fn is_bit_field(&self) -> bool {
        self.api.cursor_is_bit_field(self.raw)
    }

    /// Whether this cursor was declared `static`.
    pub fn is_static(&self) -> bool {
        self.api.cursor_is_static(self.raw)
    }

    /// Whether this cursor is a definition rather than only a declaration.
    pub fn is_definition(&self) -> bool {
        self.api.cursor_is_definition(self.raw)
    }

    /// The cursor that defines what this one declares, anywhere in this unit.
    ///
    /// `None` when the unit only ever declares it. A header that writes
    /// `struct S;` and defines `S` later hands out the forward declaration at
    /// the first use, so asking this cursor alone whether it is a definition
    /// answers about the spelling rather than about the type.
    pub fn definition(&self) -> Option<Cursor<'a>> {
        let raw = self.api.cursor_definition(self.raw);
        if CursorKind(raw.kind) == CursorKind::INVALID_FILE {
            return None;
        }
        Some(Cursor::new(raw, self.api))
    }

    /// This cursor's immediate children, in declaration order.
    pub fn children(&self) -> Vec<Cursor<'a>> {
        let mut collected: Vec<CxCursor> = Vec::new();
        // SAFETY: `visit_child` reads its client data as the `Vec<CxCursor>`
        // that is passed here and nothing else, and the pointer stays valid for
        // the whole call because `collected` outlives it.
        unsafe {
            self.api.visit_children(
                self.raw,
                visit_child,
                (&raw mut collected).cast::<std::ffi::c_void>(),
            );
        }
        collected
            .into_iter()
            .map(|raw| Cursor::new(raw, self.api))
            .collect()
    }
}

/// Appends one visited child to the `Vec<CxCursor>` behind `data`.
///
/// # Safety
///
/// `data` must be a live `*mut Vec<CxCursor>`, which is what
/// [`Cursor::children`] — the only caller — passes.
unsafe extern "C" fn visit_child(
    cursor: CxCursor,
    _parent: CxCursor,
    data: CxClientData,
) -> ChildVisitResult {
    // SAFETY: guaranteed by this function's own contract; the reference lives
    // only for the push, and libclang calls the visitor on one thread.
    unsafe {
        if let Some(collected) = data.cast::<Vec<CxCursor>>().as_mut() {
            collected.push(cursor);
        }
    }
    ChildVisitResult::CONTINUE
}

/// One C type in a parsed translation unit.
#[derive(Clone, Copy)]
pub struct CType<'a> {
    raw: CxType,
    api: &'a Api,
}

impl std::fmt::Debug for CType<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CType")
            .field("kind", &self.kind().0)
            .field("spelling", &self.spelling())
            .finish()
    }
}

impl<'a> CType<'a> {
    /// Pairs a raw type with the library that produced it.
    pub(crate) fn new(raw: CxType, api: &'a Api) -> Self {
        Self { raw, api }
    }

    /// What this type is.
    pub fn kind(&self) -> TypeKind {
        TypeKind(self.raw.kind)
    }

    /// How this type is written in C.
    pub fn spelling(&self) -> String {
        self.api.type_spelling(self.raw)
    }

    /// The cursor declaring this record, enum, or typedef type.
    pub fn declaration(&self) -> Cursor<'a> {
        Cursor::new(self.api.type_declaration(self.raw), self.api)
    }

    /// This type with every typedef and elaboration stripped.
    pub fn canonical(&self) -> CType<'a> {
        CType::new(self.api.canonical_type(self.raw), self.api)
    }

    /// What this pointer type points at.
    pub fn pointee(&self) -> CType<'a> {
        CType::new(self.api.pointee_type(self.raw), self.api)
    }

    /// What this array type holds.
    pub fn array_element(&self) -> CType<'a> {
        CType::new(self.api.array_element_type(self.raw), self.api)
    }

    /// How many elements this constant-sized array type holds.
    pub fn array_size(&self) -> i64 {
        self.api.array_size(self.raw)
    }

    /// What this function type returns.
    pub fn result(&self) -> CType<'a> {
        CType::new(self.api.result_type(self.raw), self.api)
    }

    /// This function type's parameter types, in order.
    ///
    /// Empty for a type that is not a function prototype, which is what
    /// libclang's negative argument count means.
    pub fn arguments(&self) -> Vec<CType<'a>> {
        let count = self.api.num_arg_types(self.raw);
        let count = u32::try_from(count).unwrap_or(0);
        (0..count)
            .map(|index| CType::new(self.api.arg_type(self.raw, index), self.api))
            .collect()
    }

    /// Whether this function type takes a trailing `...`.
    pub fn is_variadic(&self) -> bool {
        self.api.type_is_variadic(self.raw)
    }

    /// Whether this type is `const`-qualified.
    pub fn is_const(&self) -> bool {
        self.api.type_is_const(self.raw)
    }

    /// This type's size in bytes, or `None` when it has none — an incomplete
    /// type, or one libclang reports an error code for.
    pub fn size(&self) -> Option<u64> {
        u64::try_from(self.api.type_size(self.raw)).ok()
    }
}
