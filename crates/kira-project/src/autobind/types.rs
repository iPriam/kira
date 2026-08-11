//! Mapping one C type onto the Kira type a binding writes, and declaring
//! whatever that mapping needs along the way.
//!
//! Three rules decide everything here.
//!
//! **Width is read, never assumed.** `long` is 32 bits on Windows and 64 on
//! every other host Kira targets, and `char` is signed on some and unsigned on
//! others. So an integer maps by the size and signedness clang reports for the
//! target being built, not by the keyword it was written with.
//!
//! **Position matters.** C decays an array to a pointer in a parameter or a
//! result — a different type with different ownership — so an inline array is
//! carried as a struct member and refused anywhere else. `CString` is the
//! mirror image: a `const char *` is text in a signature, and a pointer word in
//! a struct.
//!
//! **A refusal names the type.** Nothing maps to a plausible-looking guess. A
//! `long double`, a union by value, a bitfield: each comes back as a reason
//! that ends up in the generated file beside the declaration it stopped.

use kira_clang::{CType, CursorKind, TypeKind};

use super::emit::{array_type_name, pointer_alias_name};
use super::harvest::Harvest;
use super::model::{
    ArrayDecl, CallbackDecl, FieldDecl, KiraType, OpaqueDecl, PointerDecl, StructDecl,
};

/// Where a type appears, which decides what is legal there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// A function parameter: `CString` is text, an inline array is refused.
    Parameter,
    /// A function result: `CString` is a pointer word, an inline array is
    /// refused.
    Result,
    /// A struct member: an inline array is storage, `CString` is a pointer.
    Field,
    /// The target of another pointer. A C pointer nested inside a pointer is
    /// still a pointer word; it must keep its own named alias rather than
    /// becoming the seam-only `CString` spelling used by a function
    /// parameter/result.
    PointerTarget,
}

impl Harvest {
    /// The Kira type a parameter of C type `ty` is written as.
    pub(super) fn map_parameter(&mut self, ty: &CType<'_>) -> Result<KiraType, String> {
        self.map(ty, Position::Parameter)
    }

    /// The Kira type a result of C type `ty` is written as.
    pub(super) fn map_result(&mut self, ty: &CType<'_>) -> Result<KiraType, String> {
        self.map(ty, Position::Result)
    }

    /// The name a C type is declared under in a binding, when it has one.
    ///
    /// The written spelling wins over the canonical one: `typedef struct
    /// kira_text_face_ kira_text_face;` is `kira_text_face` everywhere a
    /// program writes it, and a binding that used the tag name would name a
    /// type the header's own callers never see.
    pub(super) fn type_name(&self, ty: &CType<'_>) -> Option<String> {
        [ty.declaration().name(), ty.canonical().declaration().name()]
            .into_iter()
            .find(|candidate| is_identifier(candidate))
    }

    /// Declares whatever `ty` names, and returns the name it is declared under.
    ///
    /// The entry point for a type reached by selection rather than by a
    /// signature; a signature reaches the same code through [`Harvest::map`].
    pub(super) fn declare_named(&mut self, ty: &CType<'_>) -> Result<KiraType, String> {
        self.map(ty, Position::Field)
    }

    /// Declares `name` as an opaque C type when that is what it is.
    ///
    /// Opaque means the unit never defines it, not that this cursor is not the
    /// definition. A header is free to write `struct S;` early and define `S`
    /// later, and clang hands out whichever declaration the use site reached;
    /// judging by that one alone files a type with fields as a handle, and a
    /// handle has no layout to pass by value.
    ///
    /// `true` when it was opaque and is now declared, so the caller stops.
    pub(super) fn declare_if_opaque(&mut self, ty: &CType<'_>, name: &str) -> bool {
        let declaration = ty.canonical().declaration();
        let opaque = matches!(
            declaration.kind(),
            CursorKind::STRUCT_DECL | CursorKind::UNION_DECL
        ) && !declaration.is_definition()
            && declaration.definition().is_none();
        if opaque && self.declared.insert(name.to_owned()) {
            self.module.opaques.push(OpaqueDecl {
                name: name.to_owned(),
            });
        }
        opaque
    }

    /// The whole mapping, by what clang says the type canonically is.
    fn map(&mut self, ty: &CType<'_>, position: Position) -> Result<KiraType, String> {
        self.map_at(ty, position, None)
    }

    /// [`Harvest::map`], told which struct field it is resolving.
    ///
    /// The site is only ever used to name an unnamed function-pointer type
    /// after the field that holds it, which is the spelling the generated
    /// dialect already uses.
    fn map_at(
        &mut self,
        ty: &CType<'_>,
        position: Position,
        field: Option<&FieldSite<'_>>,
    ) -> Result<KiraType, String> {
        let canonical = ty.canonical();
        let kind = canonical.kind();
        if let Some(scalar) = scalar_for(kind, &canonical) {
            return Ok(scalar);
        }
        match kind {
            TypeKind::ENUM => self.map_enum(&canonical),
            TypeKind::POINTER => self.map_pointer(ty, &canonical, field, position),
            TypeKind::RECORD => self.map_record(ty, &canonical, position),
            TypeKind::CONSTANT_ARRAY => self.map_array(&canonical, position),
            TypeKind::INCOMPLETE_ARRAY => {
                Err("an array of unstated length, which C gives no size to carry".to_owned())
            }
            TypeKind::FUNCTION_PROTO | TypeKind::FUNCTION_NO_PROTO => {
                Err("a function type by value; C passes function pointers, so write one".to_owned())
            }
            TypeKind::LONGDOUBLE => {
                Err("a `long double`, whose width and layout differ per target".to_owned())
            }
            _ => Err(format!(
                "`{}`, a C type this seam cannot carry",
                ty.spelling()
            )),
        }
    }

    /// An enum crosses as the integer type its target represents it with.
    ///
    /// It gets no name of its own: an enum is an integer at the boundary, and a
    /// Kira alias for it would suggest the seam checks that a value is one of
    /// the enumerators, which C does not and neither does this.
    fn map_enum(&mut self, canonical: &CType<'_>) -> Result<KiraType, String> {
        let declaration = canonical.declaration();
        if declaration.kind() != CursorKind::ENUM_DECL {
            return Err("an enum with no declaration to read a width from".to_owned());
        }
        let integer = declaration.enum_integer_type();
        scalar_for(integer.kind(), &integer)
            .ok_or_else(|| "an enum with a width this seam cannot carry".to_owned())
    }

    /// A pointer is `RawPtr`, `CString`, a callback, or a named pointer alias.
    fn map_pointer(
        &mut self,
        ty: &CType<'_>,
        canonical: &CType<'_>,
        field: Option<&FieldSite<'_>>,
        position: Position,
    ) -> Result<KiraType, String> {
        let pointee = canonical.pointee();
        let pointee_kind = pointee.canonical().kind();
        if pointee_kind == TypeKind::POINTER {
            // libclang keeps every pointer layer in the type tree. Resolve the
            // inner layer first so `T **` becomes a pointer alias targeting
            // `T_ptr`, and therefore has a stable, readable Kira spelling of
            // `T_ptr_ptr`. The pointer-target position is important for
            // `char **`: the inner `char *` is a C pointer alias, not the
            // function-seam-only `CString` abstraction.
            let inner = self.map_at(&pointee, Position::PointerTarget, field)?;
            let target = inner.spelling().to_owned();
            let name = pointer_alias_name(&target);
            if self.declared.insert(name.clone()) {
                self.module.pointers.push(PointerDecl {
                    name: name.clone(),
                    target,
                });
            }
            return Ok(KiraType::Named(name));
        }
        if pointee_kind == TypeKind::VOID {
            return Ok(KiraType::RawPtr);
        }
        if matches!(pointee_kind, TypeKind::CHAR_S | TypeKind::CHAR_U)
            && pointee.is_const()
            && position != Position::PointerTarget
        {
            return Ok(KiraType::CString);
        }
        if matches!(
            pointee_kind,
            TypeKind::FUNCTION_PROTO | TypeKind::FUNCTION_NO_PROTO
        ) {
            return self.declare_callback(ty, &pointee.canonical(), field);
        }
        // The written pointee is asked first so a pointer to a typedef keeps the
        // typedef's spelling. It answers nothing when the pointer itself is
        // what was typedef'd — `typedef struct T *Handle` is a typedef type,
        // not a pointer type, until it is canonicalized — and the canonical
        // pointee is the same C type under the name its own declaration
        // carries. Without that fallback every handle-based C API binds
        // nothing: WebGPU spells all 263 of its entry points this way.
        let target = self
            .type_name(&ty.pointee())
            .or_else(|| self.type_name(&pointee))
            .or_else(|| builtin_target_name(&pointee.canonical()))
            .ok_or_else(|| {
                format!(
                    "a pointer to `{}`, which has no name to point at",
                    pointee.spelling()
                )
            })?;
        // A selected function is allowed to be the only declaration that
        // reaches a C-layout record. Keep that record in the binding even when
        // `autobind` is in `Selected` mode: callers of a typed pointer may pass
        // the record by address and, for a readable target, inspect its fields.
        // If one of the fields is outside the seam, leave the pointer as an
        // opaque handle; that is still a valid and useful C pointer binding.
        let declaration = pointee.canonical().declaration();
        if matches!(
            declaration.kind(),
            CursorKind::STRUCT_DECL | CursorKind::UNION_DECL
        ) && (declaration.is_definition() || declaration.definition().is_some())
        {
            let _ = self.map_record(&ty.pointee(), &pointee.canonical(), Position::Field);
        }
        // An opaque C type — named by the headers, never defined — gets an
        // alias so the pointer has a target that reads as the C type it is.
        if declaration.kind() == CursorKind::STRUCT_DECL
            && !declaration.is_definition()
            && declaration.definition().is_none()
            && self.declared.insert(target.clone())
        {
            self.module.opaques.push(OpaqueDecl {
                name: target.clone(),
            });
        }
        let name = pointer_alias_name(&target);
        if self.declared.insert(name.clone()) {
            self.module.pointers.push(PointerDecl {
                name: name.clone(),
                target,
            });
        }
        Ok(KiraType::Named(name))
    }

    /// A C-layout struct crosses by value once every field maps.
    fn map_record(
        &mut self,
        ty: &CType<'_>,
        canonical: &CType<'_>,
        position: Position,
    ) -> Result<KiraType, String> {
        let declaration = canonical.declaration();
        if declaration.kind() == CursorKind::UNION_DECL {
            return Err("a union by value, whose active member the seam cannot know".to_owned());
        }
        let name = self
            .type_name(ty)
            .ok_or_else(|| "an anonymous struct, which has no name to declare".to_owned())?;
        if let Some(reason) = self.refused.get(&name) {
            return Err(reason.clone());
        }
        if self.declared.contains(&name) {
            return Ok(KiraType::Named(name));
        }
        if !self.in_progress.insert(name.clone()) {
            // Reached while its own fields are being resolved: the name is
            // already going to be declared, so the field may name it.
            return Ok(KiraType::Named(name));
        }
        let resolved = self.resolve_fields(&declaration, &name, position);
        self.in_progress.remove(&name);
        let fields = match resolved {
            Ok(fields) => fields,
            Err(reason) => {
                self.refused.insert(name, reason.clone());
                return Err(reason);
            }
        };
        self.declared.insert(name.clone());
        self.module.structs.push(StructDecl {
            name: name.clone(),
            fields,
        });
        Ok(KiraType::Named(name))
    }

    /// Every field of a struct, or the first reason one of them stops it.
    fn resolve_fields(
        &mut self,
        declaration: &kira_clang::Cursor<'_>,
        name: &str,
        position: Position,
    ) -> Result<Vec<FieldDecl>, String> {
        // The cursor a use site reaches may be a forward declaration of a type
        // the unit defines further down, so the fields are read from whichever
        // cursor defines it. Only a type nothing defines has no layout.
        let defining = match declaration.definition() {
            Some(defining) => defining,
            None if declaration.is_definition() => *declaration,
            None => {
                return Err(format!(
                    "`{name}`, which the headers declare and never define, so it has no layout"
                ));
            }
        };
        let mut fields = Vec::new();
        for field in defining.children() {
            if field.kind() != CursorKind::FIELD_DECL {
                continue;
            }
            if field.is_bit_field() {
                return Err(format!(
                    "`{name}`, whose field `{}` is a bitfield, which has no Kira spelling",
                    field.name()
                ));
            }
            let written = field.name();
            let taken: Vec<String> = fields
                .iter()
                .map(|kept: &FieldDecl| kept.name.clone())
                .collect();
            let Some(field_name) = super::names::field_name(&written, &taken) else {
                return Err(format!(
                    "`{name}`, which has an anonymous member the seam cannot name"
                ));
            };
            let site = FieldSite {
                record: name,
                field: &field_name,
            };
            let field_type = self
                .map_at(&field.c_type(), Position::Field, Some(&site))
                .map_err(|reason| format!("`{name}`, whose field `{written}` is {reason}"))?;
            fields.push(FieldDecl {
                name: field_name,
                field_type,
            });
        }
        // A struct reached in a signature position must be able to cross by
        // value, and one reached as a field only has to have a layout. Both are
        // the same set today; the position is kept so a future restriction has
        // somewhere to live.
        let _ = position;
        Ok(fields)
    }

    /// An inline C array is storage inside a struct, and nothing else.
    fn map_array(&mut self, canonical: &CType<'_>, position: Position) -> Result<KiraType, String> {
        if position != Position::Field {
            return Err(
                "an inline array, which C decays to a pointer here; write `RawPtr` when that \
                 is what the symbol takes"
                    .to_owned(),
            );
        }
        let count = u64::try_from(canonical.array_size())
            .map_err(|_| "an array whose length C does not state".to_owned())?;
        let element = self.map(&canonical.array_element(), Position::Field)?;
        let name = array_type_name(&element, count);
        if self.declared.insert(name.clone()) {
            self.module.arrays.push(ArrayDecl {
                name: name.clone(),
                element,
                count,
            });
        }
        Ok(KiraType::Named(name))
    }

    /// A C function pointer becomes an `@FFI.Callback` typedef.
    ///
    /// Named after the typedef it was written as when it has one, because that
    /// is the name a program hands a Kira function to; an unnamed one is named
    /// after the field that holds it, and a signature reached from nowhere in
    /// particular after its own shape.
    ///
    /// A signature the *callback* seam cannot carry is still declared. The
    /// language draws the line at the use: declaring such a callback is clean,
    /// and handing a Kira function to one is what KSEM245 refuses. Refusing it
    /// here instead would take the whole struct that holds it with it, and with
    /// `sapp_desc` that is every entry point sokol has.
    fn declare_callback(
        &mut self,
        written: &CType<'_>,
        prototype: &CType<'_>,
        field: Option<&FieldSite<'_>>,
    ) -> Result<KiraType, String> {
        let result = match prototype.result().kind() {
            TypeKind::VOID => KiraType::Void,
            _ => self.map(&prototype.result(), Position::Result)?,
        };
        let mut params = Vec::new();
        for argument in prototype.arguments() {
            params.push(self.map(&argument, Position::Parameter)?);
        }
        if prototype.is_variadic() {
            return Err("a variadic function pointer, which has no fixed signature".to_owned());
        }
        let name = self.type_name(written).unwrap_or_else(|| match field {
            Some(site) => format!("{}_{}_callback", site.record, site.field),
            None => synthetic_callback_name(&params, &result),
        });
        if self.declared.insert(name.clone()) {
            self.module.callbacks.push(CallbackDecl {
                name: name.clone(),
                params,
                result,
            });
        }
        Ok(KiraType::Named(name))
    }
}

/// Where a type is being resolved, when it is a struct field.
///
/// Carried only so an unnamed function-pointer field can be named after the
/// field that holds it.
pub(super) struct FieldSite<'a> {
    /// The struct being resolved.
    pub(super) record: &'a str,
    /// The field being resolved.
    pub(super) field: &'a str,
}

/// The Kira scalar a builtin C type crosses as, by clang's kind and its width.
///
/// Width comes from clang rather than from the keyword: `long` is 32 bits under
/// MSVC and 64 elsewhere, so a table keyed on the spelling would be wrong on
/// one of the two targets Kira builds for on every commit.
fn scalar_for(kind: TypeKind, ty: &CType<'_>) -> Option<KiraType> {
    let signed = |width: Option<u64>| match width {
        Some(1) => Some(KiraType::Int("I8")),
        Some(2) => Some(KiraType::Int("I16")),
        Some(4) => Some(KiraType::Int("I32")),
        // Bare `Int` is the 64-bit signed integer; there is no `I64` spelling.
        Some(8) => Some(KiraType::Int("Int")),
        _ => None,
    };
    let unsigned = |width: Option<u64>| match width {
        Some(1) => Some(KiraType::Int("U8")),
        Some(2) => Some(KiraType::Int("U16")),
        Some(4) => Some(KiraType::Int("U32")),
        Some(8) => Some(KiraType::Int("U64")),
        _ => None,
    };
    match kind {
        TypeKind::VOID => Some(KiraType::Void),
        TypeKind::BOOL => Some(KiraType::Bool),
        TypeKind::FLOAT => Some(KiraType::F32),
        TypeKind::DOUBLE => Some(KiraType::F64),
        TypeKind::CHAR_S
        | TypeKind::SCHAR
        | TypeKind::SHORT
        | TypeKind::INT
        | TypeKind::LONG
        | TypeKind::LONGLONG => signed(ty.size()),
        TypeKind::CHAR_U
        | TypeKind::UCHAR
        | TypeKind::USHORT
        | TypeKind::UINT
        | TypeKind::ULONG
        | TypeKind::ULONGLONG => unsigned(ty.size()),
        _ => None,
    }
}

/// The name a pointer to a builtin C type points at.
///
/// A `char *` out-parameter has no typedef to name, and `char_ptr` is what the
/// dialect already spells it — the alias resolves to `RawPtr` either way, so the
/// name is documentation that a reader can match against the header.
fn builtin_target_name(canonical: &CType<'_>) -> Option<String> {
    sanitized_target_name(&canonical.spelling())
}

/// The single-word C type name in `spelling`, when it is one.
///
/// A pointer target is written into the generated file as an identifier, so a
/// multi-word spelling (`unsigned char`) has no name to write and the pointer
/// is refused rather than emitted with a name that would not parse.
fn sanitized_target_name(spelling: &str) -> Option<String> {
    let cleaned = spelling.trim_start_matches("const ").trim();
    is_identifier(cleaned).then(|| cleaned.to_owned())
}

/// Whether a clang declaration name can be written as a Kira identifier.
///
/// Anonymous records are reported by libclang with spellings such as
/// `struct (unnamed at header.h:12:3)`. They have a layout, but no stable name
/// a generated Kira declaration can carry; treating them as unnamed lets the
/// containing pointer remain an opaque handle instead of emitting invalid Kira.
fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with(|c: char| c.is_ascii_digit())
        && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The name an unnamed function-pointer signature is declared under.
fn synthetic_callback_name(params: &[KiraType], result: &KiraType) -> String {
    let mut name = String::from("callback");
    for param in params {
        name.push('_');
        name.push_str(param.spelling());
    }
    name.push_str("_to_");
    name.push_str(result.spelling());
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_synthetic_callback_name_is_derived_from_its_signature() {
        assert_eq!(
            synthetic_callback_name(&[KiraType::Int("I32"), KiraType::RawPtr], &KiraType::Void),
            "callback_I32_RawPtr_to_Void"
        );
    }

    #[test]
    fn a_pointer_target_is_a_single_word_c_name_or_nothing() {
        assert_eq!(
            sanitized_target_name("const uint8_t"),
            Some("uint8_t".to_owned())
        );
        assert_eq!(sanitized_target_name("char"), Some("char".to_owned()));
        assert_eq!(sanitized_target_name("unsigned char"), None);
        assert_eq!(sanitized_target_name("struct Foo"), None);
        assert_eq!(sanitized_target_name(""), None);
    }
}
