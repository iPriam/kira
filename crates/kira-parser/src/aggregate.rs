//! Aggregate declaration parsing: `struct`, `enum`, and `class`.
//!
//! These three share a shape — a name, a braced body, and members separated by
//! nothing more than newlines or `;` — so they share a file. A class adds an
//! `extends` list and `override` members on top of the struct grammar.

use kira_core::Symbol;
use kira_runtime_abi::Execution;
use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{
    ClassDecl, ClassMethod, EnumDecl, FieldDecl, OverrideFieldDecl, ParentRef, StructDecl,
    VariantDecl,
};

use crate::Parser;

impl Parser<'_> {
    // ----- structs ------------------------------------------------------

    /// Parses `struct Name { <member>* }`.
    ///
    /// Members are `let`/`var` bindings. Newlines and `;` are both
    /// insignificant, so the member keyword is what starts each member and a
    /// member with no keyword is reported rather than silently skipped.
    pub(crate) fn parse_struct(&mut self) -> Option<StructDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Struct);
        let (name, name_span) = if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            (self.intern_span(span), span)
        } else {
            self.error(self.current().span, "KPAR007", "expected a struct name");
            (Symbol::ERROR, self.current().span)
        };
        // Consumes the name just interned above.
        if self.at(TokenKind::Identifier) {
            self.bump();
        }
        let type_params = if self.at_type_params() {
            self.parse_type_params()
        } else {
            Vec::new()
        };
        let traits = self.parse_trait_list();
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        if !self.expect(TokenKind::LBrace) {
            let span = Span::from_bounds(start.start, self.previous_end());
            return Some(StructDecl {
                name,
                name_span,
                type_params,
                traits,
                fields,
                methods,
                ffi: None,
                derives_copy: None,
                span,
            });
        }
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            while self.eat(TokenKind::Semicolon) {}
            if self.at(TokenKind::RBrace) || self.at_eof() {
                break;
            }
            match self.current_kind() {
                TokenKind::Let => {
                    if let Some(field) = self.parse_field(false) {
                        fields.push(field);
                    }
                }
                TokenKind::Var => {
                    if let Some(field) = self.parse_field(true) {
                        fields.push(field);
                    }
                }
                TokenKind::Function => {
                    if let Some(mut method) = self.parse_function(false, Execution::Inherited, None)
                    {
                        self.refuse_generic_member(&mut method);
                        methods.push(method);
                    }
                }
                kind => {
                    let span = self.current().span;
                    self.error(
                        span,
                        "KPAR009",
                        format!(
                            "expected `let`, `var`, or `function` to start a struct member, \
                             found {}",
                            kind.describe()
                        ),
                    );
                    self.bump();
                }
            }
            while self.eat(TokenKind::Semicolon) {}
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(StructDecl {
            name,
            name_span,
            type_params,
            traits,
            fields,
            methods,
            ffi: None,
            derives_copy: None,
            span,
        })
    }

    /// Parses one `let`/`var` struct member, with the keyword at the cursor.
    ///
    /// A block-bodied member (`let Green: Color { … }`) is deliberately *not*
    /// accepted here. A computed member is a construct-backed surface — see
    /// [`Parser::parse_construct_let`] — and a plain `struct`/`class` has no
    /// such form: the oracle rejects one with this same `KPAR009`, and reading
    /// it back as `Color.Green` would need static members, which the language
    /// does not have.
    fn parse_field(&mut self, mutable: bool) -> Option<FieldDecl> {
        let start = self.current().span;
        self.bump(); // `let` / `var`
        if !self.at(TokenKind::Identifier) {
            self.error(self.current().span, "KPAR010", "expected a member name");
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        // A member's type is required: a struct's shape is its contract, and
        // there is no initializer to infer it from when the default is absent.
        self.expect(TokenKind::Colon);
        let ty = self.parse_type_ref();
        let default = self.eat(TokenKind::Equals).then(|| self.parse_expr());
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(FieldDecl {
            name,
            name_span,
            mutable,
            ty,
            default,
            span,
        })
    }

    // ----- enums ---------------------------------------------------------

    /// Parses `enum Name { <variant>* }`.
    ///
    /// Variants are separated by newlines, spaces, or `;` — never commas, which
    /// the enum grammar does not use — so the variant name is what starts each
    /// one and a non-name where a variant is expected is reported rather than
    /// silently skipped.
    pub(crate) fn parse_enum(&mut self) -> Option<EnumDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Enum);
        let (name, name_span) = if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            (self.intern_span(span), span)
        } else {
            self.error(self.current().span, "KPAR030", "expected an enum name");
            (Symbol::ERROR, self.current().span)
        };
        // Consumes the name just interned above.
        if self.at(TokenKind::Identifier) {
            self.bump();
        }
        // Enum parameters are recorded like the other declaration forms; the
        // semantic pass decides when their concrete rows are needed.
        let type_params = if self.at_type_params() {
            self.parse_type_params()
        } else {
            Vec::new()
        };
        let traits = self.parse_trait_list();
        let mut variants = Vec::new();
        if !self.expect(TokenKind::LBrace) {
            let span = Span::from_bounds(start.start, self.previous_end());
            return Some(EnumDecl {
                name,
                name_span,
                traits,
                type_params,
                variants,
                derives_copy: None,
                span,
            });
        }
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            while self.eat(TokenKind::Semicolon) {}
            if self.at(TokenKind::RBrace) || self.at_eof() {
                break;
            }
            if self.at(TokenKind::Identifier) {
                if let Some(variant) = self.parse_variant() {
                    variants.push(variant);
                }
            } else {
                let span = self.current().span;
                self.error(
                    span,
                    "KPAR031",
                    format!(
                        "expected an enum variant name, found {}",
                        self.current_kind().describe()
                    ),
                );
                self.bump();
            }
            while self.eat(TokenKind::Semicolon) {}
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(EnumDecl {
            name,
            name_span,
            traits,
            type_params,
            variants,
            derives_copy: None,
            span,
        })
    }

    /// Parses one enum variant, with the name at the cursor.
    ///
    /// Three shapes: `Name` (payload-less), `Name(Type)` (a payload), and
    /// `Name: Type = default` (a payload with a default supplied when the
    /// variant is built with none). The `= default` only follows the `:` form.
    fn parse_variant(&mut self) -> Option<VariantDecl> {
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        let (payload, default) = if self.at(TokenKind::LParen) {
            self.bump(); // `(`
            let ty = self.parse_type_ref();
            self.expect(TokenKind::RParen);
            (Some(ty), None)
        } else if self.eat(TokenKind::Colon) {
            let ty = self.parse_type_ref();
            let default = self.eat(TokenKind::Equals).then(|| self.parse_expr());
            (Some(ty), default)
        } else {
            (None, None)
        };
        let span = Span::from_bounds(name_span.start, self.previous_end());
        Some(VariantDecl {
            name,
            name_span,
            payload,
            default,
            span,
        })
    }

    // ----- classes ------------------------------------------------------

    /// Parses `class Name [extends Parent, ...] { <member>* }`.
    ///
    /// The body is the struct body plus two members a struct cannot have:
    /// `override let name = value`, which rebinds an inherited field's default,
    /// and `override function`, which replaces an inherited method. An
    /// `override var` is rejected here rather than in semantics — the mutability
    /// of the inherited slot is not the override's to restate.
    pub(crate) fn parse_class(&mut self) -> Option<ClassDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Class);
        let (name, name_span) = self.parse_declaration_name("KPAR033", "expected a class name");
        let type_params = if self.at_type_params() {
            self.parse_type_params()
        } else {
            Vec::new()
        };
        // `: traits` first, `extends parents` second: the colon is always
        // conformance and `extends` is always a parent, so the two clauses
        // never have to be told apart by what they name.
        let traits = self.parse_trait_list();
        let parents = self.parse_extends_list();
        let mut fields = Vec::new();
        let mut overrides = Vec::new();
        let mut methods = Vec::new();
        if !self.expect(TokenKind::LBrace) {
            let span = Span::from_bounds(start.start, self.previous_end());
            return Some(ClassDecl {
                name,
                name_span,
                type_params,
                traits,
                parents,
                fields,
                overrides,
                methods,
                export: None,
                span,
            });
        }
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            while self.eat(TokenKind::Semicolon) {}
            if self.at(TokenKind::RBrace) || self.at_eof() {
                break;
            }
            match self.current_kind() {
                TokenKind::Let => {
                    if let Some(field) = self.parse_field(false) {
                        fields.push(field);
                    }
                }
                TokenKind::Var => {
                    if let Some(field) = self.parse_field(true) {
                        fields.push(field);
                    }
                }
                TokenKind::Function => {
                    if let Some(mut function) =
                        self.parse_function(false, Execution::Inherited, None)
                    {
                        self.refuse_generic_member(&mut function);
                        methods.push(ClassMethod {
                            is_override: false,
                            function,
                        });
                    }
                }
                // An annotated member. The annotations are recorded on the
                // method rather than rejected here, so `@Export` on a method is
                // refused in semantics by name — with the reason — instead of
                // as a syntax error about a missing `let`.
                TokenKind::At => {
                    let annotations = self.parse_annotations();
                    if self.at(TokenKind::Function) {
                        if let Some(mut function) = self.parse_function_annotated(&annotations) {
                            self.refuse_generic_member(&mut function);
                            methods.push(ClassMethod {
                                is_override: false,
                                function,
                            });
                        }
                    } else {
                        let span = self.current().span;
                        self.error(
                            span,
                            "KPAR042",
                            "expected `function` after an annotation in a class body",
                        );
                    }
                }
                TokenKind::Override => self.parse_override_member(&mut overrides, &mut methods),
                kind => {
                    let span = self.current().span;
                    self.error(
                        span,
                        "KPAR009",
                        format!(
                            "expected `let`, `var`, `function`, or `override` to start a class \
                             member, found {}",
                            kind.describe()
                        ),
                    );
                    self.bump();
                }
            }
            while self.eat(TokenKind::Semicolon) {}
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(ClassDecl {
            name,
            name_span,
            type_params,
            traits,
            parents,
            fields,
            overrides,
            methods,
            export: None,
            span,
        })
    }

    /// Parses the comma-separated `extends A, B` list, or nothing.
    ///
    /// A trailing comma is tolerated the way it is everywhere else in this
    /// grammar. Duplicate and cyclic parents are semantics' business, not the
    /// parser's — every name written is recorded so those diagnostics have a
    /// span to point at.
    fn parse_extends_list(&mut self) -> Vec<ParentRef> {
        let mut parents = Vec::new();
        if !self.eat(TokenKind::Extends) {
            return parents;
        }
        loop {
            if !self.at(TokenKind::Identifier) {
                self.error(
                    self.current().span,
                    "KPAR034",
                    "expected a parent type name",
                );
                break;
            }
            let span = self.current().span;
            let name = self.intern_span(span);
            self.bump();
            let type_args = if self.at(TokenKind::Lt) {
                self.parse_call_type_args()
            } else {
                Vec::new()
            };
            parents.push(ParentRef {
                name,
                span,
                type_args,
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.at(TokenKind::LBrace) {
                break;
            }
        }
        parents
    }

    /// Parses one `override` member, with `override` at the cursor.
    fn parse_override_member(
        &mut self,
        overrides: &mut Vec<OverrideFieldDecl>,
        methods: &mut Vec<ClassMethod>,
    ) {
        let start = self.current().span;
        self.bump(); // `override`
        match self.current_kind() {
            TokenKind::Function => {
                if let Some(mut function) = self.parse_function(false, Execution::Inherited, None) {
                    self.refuse_generic_member(&mut function);
                    methods.push(ClassMethod {
                        is_override: true,
                        function,
                    });
                }
            }
            TokenKind::Let | TokenKind::Var => {
                let is_var = self.at(TokenKind::Var);
                let keyword_span = self.current().span;
                self.bump();
                if is_var {
                    self.error(
                        keyword_span,
                        "KPAR035",
                        "`override` a field with `let`: the inherited field already decided \
                         whether it is mutable",
                    );
                }
                if !self.at(TokenKind::Identifier) {
                    self.error(self.current().span, "KPAR010", "expected a member name");
                    return;
                }
                let name_span = self.current().span;
                let name = self.intern_span(name_span);
                self.bump();
                // A type is optional here: an override rebinds an inherited
                // slot, which already has one. Restating it is legal and
                // changes nothing; whether it *agrees* is a question about
                // resolved types, so semantics answers it.
                let ty = self.eat(TokenKind::Colon).then(|| self.parse_type_ref());
                if !self.expect(TokenKind::Equals) {
                    return;
                }
                let default = self.parse_expr();
                let span = Span::from_bounds(start.start, self.previous_end());
                overrides.push(OverrideFieldDecl {
                    name,
                    name_span,
                    ty,
                    default,
                    span,
                });
            }
            kind => {
                let span = self.current().span;
                self.error(
                    span,
                    "KPAR037",
                    format!(
                        "expected `function` or `let` after `override`, found {}",
                        kind.describe()
                    ),
                );
                self.bump();
            }
        }
    }

    /// Reads the name token of an aggregate declaration, or reports its absence.
    fn parse_declaration_name(
        &mut self,
        code: &'static str,
        message: &'static str,
    ) -> (Symbol, Span) {
        if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            let name = self.intern_span(span);
            self.bump();
            (name, span)
        } else {
            self.error(self.current().span, code, message);
            (Symbol::ERROR, self.current().span)
        }
    }
}
