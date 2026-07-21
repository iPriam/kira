//! Parsing the `@FFI.*` annotation family.
//!
//! Two grammars share this file because they share a head — `@FFI.<Member>`
//! followed by a `{ key: value; ... }` block:
//!
//! * `@FFI.Extern` rides a bodyless *function* and names a foreign C symbol.
//!   Its block is `identifier : identifier ;` throughout, recorded verbatim as
//!   [`ForeignField`]s; meaning (required, duplicate, the `abi` value) is the
//!   analyzer's.
//! * `@FFI.Struct`/`Pointer`/`Alias`/`Array`/`Callback` ride a *struct* and each
//!   declares a C type. Their blocks carry richer values — a type, a bracketed
//!   type list, an integer — so they parse into a typed [`FfiTypeKind`] rather
//!   than a flat field list. Every structural mistake is reported and recovered
//!   from here; which fields a form *requires* is the analyzer's.

use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{FfiTypeKind, FfiTypeMark, ForeignField, ForeignMark, TypeRefId};

use super::Annotations;
use crate::Parser;

/// The recognized `@FFI.*` members that ride a struct, mapped from their name.
#[derive(Debug, Clone, Copy)]
enum FfiForm {
    Struct,
    Pointer,
    Alias,
    Array,
    Callback,
}

impl FfiForm {
    /// The form a `@FFI.<member>` name selects, or `None` when the member is
    /// not one of the struct-attached family.
    fn from_member(member: &str) -> Option<Self> {
        match member {
            "Struct" => Some(FfiForm::Struct),
            "Pointer" => Some(FfiForm::Pointer),
            "Alias" => Some(FfiForm::Alias),
            "Array" => Some(FfiForm::Array),
            "Callback" => Some(FfiForm::Callback),
            _ => None,
        }
    }
}

/// The arguments a `@FFI.*` type block carried, collected key by key before a
/// [`FfiTypeKind`] is assembled from the ones the form uses.
#[derive(Default)]
struct FfiBlockFields {
    layout: Option<(kira_core::Symbol, Span)>,
    ownership: Option<(kira_core::Symbol, Span)>,
    abi: Option<(kira_core::Symbol, Span)>,
    target: Option<TypeRefId>,
    element: Option<TypeRefId>,
    result: Option<TypeRefId>,
    count: Option<(i64, Span)>,
    params: Option<Vec<TypeRefId>>,
}

impl FfiBlockFields {
    /// Builds the form's [`FfiTypeKind`] from the collected fields.
    fn into_kind(self, form: FfiForm) -> FfiTypeKind {
        match form {
            FfiForm::Struct => FfiTypeKind::Struct {
                layout: self.layout,
            },
            FfiForm::Pointer => FfiTypeKind::Pointer {
                target: self.target,
                ownership: self.ownership,
            },
            FfiForm::Alias => FfiTypeKind::Alias {
                target: self.target,
            },
            FfiForm::Array => FfiTypeKind::Array {
                element: self.element,
                count: self.count,
            },
            FfiForm::Callback => FfiTypeKind::Callback {
                abi: self.abi,
                params: self.params.unwrap_or_default(),
                result: self.result,
            },
        }
    }
}

impl Parser<'_> {
    /// Parses a qualified annotation — the cursor is on its first identifier and
    /// a `. identifier` follows. `@FFI.Extern` and the five struct-attached
    /// `@FFI.*` forms are known; anything else is [`KPAR053`].
    pub(crate) fn parse_qualified_annotation(&mut self, annotations: &mut Annotations) {
        let root_span = self.current().span;
        let root = self.text_of(root_span).to_owned();
        let member_span = self.peek(2).span;
        let member = self.text_of(member_span).to_owned();
        let name_span = Span::from_bounds(root_span.start, member_span.end());
        self.bump(); // root identifier
        self.bump(); // `.`
        self.bump(); // member identifier

        if root == "FFI" && member == "Extern" {
            let (fields, block_span) =
                self.parse_ffi_block_span(name_span, |parser| parser.parse_foreign_block());
            annotations.foreign = Some(ForeignMark {
                span: name_span,
                block_span,
                fields,
            });
            return;
        }

        if root == "FFI"
            && let Some(form) = FfiForm::from_member(&member)
        {
            let (collected, block_span) =
                self.parse_ffi_block_span(name_span, |parser| parser.parse_ffi_block());
            annotations.ffi_type = Some(FfiTypeMark {
                kind: collected.into_kind(form),
                name_span,
                block_span,
            });
            return;
        }

        self.error(
            name_span,
            "KPAR053",
            format!(
                "unknown qualified annotation `@{root}.{member}`; the `@FFI.*` family is \
                 `Extern`, `Struct`, `Pointer`, `Alias`, `Array`, and `Callback`"
            ),
        );
        // Skip a `{ ... }` payload so recovery lands on the declaration.
        if self.at(TokenKind::LBrace) {
            self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
        }
    }

    /// Runs `parse` over the block and returns its result plus the block's span.
    ///
    /// When no `{` was there to consume, nothing advances, so the block has no
    /// span of its own; the annotation name is what a diagnostic points at
    /// instead.
    fn parse_ffi_block_span<T>(
        &mut self,
        name_span: Span,
        parse: impl FnOnce(&mut Self) -> T,
    ) -> (T, Span) {
        let block_start = self.current().span.start;
        let value = parse(self);
        let block_end = self.previous_end();
        let block_span = if block_end >= block_start {
            Span::from_bounds(block_start, block_end)
        } else {
            name_span
        };
        (value, block_span)
    }

    /// Parses the `{ key: value; ... }` block of an `@FFI.Extern` annotation.
    ///
    /// Each field is `identifier : identifier ;`. Every structural mistake — a
    /// missing brace, a non-identifier key, a missing colon, a non-identifier
    /// value, a missing terminator — is reported with its own code, and recovery
    /// advances to the next field so one bad field does not swallow the rest.
    /// Field *meaning* (required, duplicate, unknown, the `abi` value) is the
    /// analyzer's, not the parser's.
    fn parse_foreign_block(&mut self) -> Vec<ForeignField> {
        let mut fields = Vec::new();
        if !self.at(TokenKind::LBrace) {
            self.error(
                self.current().span,
                "KPAR048",
                "expected `{` to open the `@FFI.Extern` block",
            );
            return fields;
        }
        self.bump(); // `{`
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            if let Some(field) = self.parse_foreign_field() {
                fields.push(field);
            }
            // Force progress: a field that consumed nothing would spin.
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        fields
    }

    /// Parses one `identifier : identifier ;` field, or reports why it could
    /// not and returns `None`.
    fn parse_foreign_field(&mut self) -> Option<ForeignField> {
        if !self.at(TokenKind::Identifier) {
            self.error(
                self.current().span,
                "KPAR049",
                "expected a field name in the `@FFI.Extern` block",
            );
            return None;
        }
        let key_span = self.current().span;
        let key = self.intern_span(key_span);
        self.bump();
        if !self.at(TokenKind::Colon) {
            self.error(
                self.current().span,
                "KPAR050",
                "expected `:` after an `@FFI.Extern` field name",
            );
            return None;
        }
        self.bump(); // `:`
        if !self.at(TokenKind::Identifier) {
            self.error(
                self.current().span,
                "KPAR051",
                "expected a field value in the `@FFI.Extern` block",
            );
            return None;
        }
        let value_span = self.current().span;
        let value = self.intern_span(value_span);
        self.bump();
        if !self.at(TokenKind::Semicolon) {
            self.error(
                self.current().span,
                "KPAR052",
                "expected `;` after an `@FFI.Extern` field",
            );
            return Some(ForeignField {
                key,
                key_span,
                value,
                value_span,
            });
        }
        self.bump(); // `;`
        Some(ForeignField {
            key,
            key_span,
            value,
            value_span,
        })
    }

    /// Parses the `{ key: value; ... }` block of a struct-attached `@FFI.*`
    /// annotation, collecting each recognized key's value.
    ///
    /// The value grammar depends on the key: `target`/`element`/`result` name a
    /// type, `params` a bracketed type list, `count` an integer, and
    /// `layout`/`abi`/`ownership` a bare identifier. An unrecognized key's value
    /// is swallowed so an unknown field never derails the block; the analyzer
    /// decides which keys a form requires.
    fn parse_ffi_block(&mut self) -> FfiBlockFields {
        let mut fields = FfiBlockFields::default();
        if !self.at(TokenKind::LBrace) {
            self.error(
                self.current().span,
                "KPAR048",
                "expected `{` to open the `@FFI.*` block",
            );
            return fields;
        }
        self.bump(); // `{`
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            self.parse_ffi_field(&mut fields);
            // Force progress: a field that consumed nothing would spin.
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        fields
    }

    /// Parses one `key : value ;` field of a struct-attached `@FFI.*` block into
    /// `out`, dispatching the value grammar on the key.
    fn parse_ffi_field(&mut self, out: &mut FfiBlockFields) {
        if !self.at(TokenKind::Identifier) {
            self.error(
                self.current().span,
                "KPAR049",
                "expected a field name in the `@FFI.*` block",
            );
            self.recover_ffi_field();
            return;
        }
        let key_span = self.current().span;
        let key = self.text_of(key_span).to_owned();
        self.bump();
        if !self.at(TokenKind::Colon) {
            self.error(
                self.current().span,
                "KPAR050",
                "expected `:` after an `@FFI.*` field name",
            );
            self.recover_ffi_field();
            return;
        }
        self.bump(); // `:`
        match key.as_str() {
            "layout" => out.layout = self.parse_ffi_symbol_value(),
            "ownership" => out.ownership = self.parse_ffi_symbol_value(),
            "abi" => out.abi = self.parse_ffi_symbol_value(),
            "target" => out.target = Some(self.parse_ffi_type_value()),
            "element" => out.element = Some(self.parse_ffi_type_value()),
            "result" => out.result = Some(self.parse_ffi_type_value()),
            "count" => out.count = self.parse_ffi_int_value(),
            "params" => out.params = Some(self.parse_ffi_type_list()),
            // An unknown key is recorded by no form; swallow its value up to the
            // terminator so the rest of the block still parses.
            _ => self.skip_to_ffi_field_end(),
        }
        if !self.eat(TokenKind::Semicolon) {
            self.error(
                self.current().span,
                "KPAR052",
                "expected `;` after an `@FFI.*` field",
            );
        }
    }

    /// Reads a bare-identifier field value (`c`, `borrowed`).
    fn parse_ffi_symbol_value(&mut self) -> Option<(kira_core::Symbol, Span)> {
        if !self.at(TokenKind::Identifier) {
            self.error(
                self.current().span,
                "KPAR051",
                "expected a field value in the `@FFI.*` block",
            );
            return None;
        }
        let span = self.current().span;
        let symbol = self.intern_span(span);
        self.bump();
        Some((symbol, span))
    }

    /// Reads a type field value (`target`, `element`, `result`, a `params`
    /// element), tolerating a leading C `union` tag on the type name.
    fn parse_ffi_type_value(&mut self) -> TypeRefId {
        // A generated `target: union Name` carries the C aggregate tag; it names
        // no Kira type, so it is skipped when a real type name follows it.
        if self.at_word("union") && self.peek(1).kind == TokenKind::Identifier {
            self.bump();
        }
        self.parse_type_ref()
    }

    /// Reads an integer field value (`count`).
    fn parse_ffi_int_value(&mut self) -> Option<(i64, Span)> {
        if !self.at(TokenKind::IntLiteral) {
            self.error(
                self.current().span,
                "KPAR051",
                "expected an integer field value in the `@FFI.*` block",
            );
            return None;
        }
        let span = self.current().span;
        let text = self.text_of(span);
        let value = match text.parse::<i64>() {
            Ok(value) => value,
            Err(_) => {
                self.error(
                    span,
                    "KPAR021",
                    "integer literal does not fit in a 64-bit integer",
                );
                0
            }
        };
        self.bump();
        Some((value, span))
    }

    /// Reads a `[T, T, ...]` type list (`params`), possibly empty.
    fn parse_ffi_type_list(&mut self) -> Vec<TypeRefId> {
        let mut types = Vec::new();
        if !self.eat(TokenKind::LBracket) {
            self.error(
                self.current().span,
                "KPAR051",
                "expected `[` to open the `params` type list",
            );
            return types;
        }
        while !self.at(TokenKind::RBracket) && !self.at_eof() && !self.at(TokenKind::Semicolon) {
            let before = self.pos;
            types.push(self.parse_ffi_type_value());
            if self.pos == before {
                self.bump();
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBracket);
        types
    }

    /// Consumes the rest of a malformed `@FFI.*` field up to — but not
    /// including — its terminating `;` or the block's `}`.
    fn recover_ffi_field(&mut self) {
        while !self.at(TokenKind::Semicolon) && !self.at(TokenKind::RBrace) && !self.at_eof() {
            self.bump();
        }
    }

    /// Swallows an unknown key's value up to its terminator, the same way
    /// [`Parser::recover_ffi_field`] does.
    fn skip_to_ffi_field_end(&mut self) {
        self.recover_ffi_field();
    }
}
