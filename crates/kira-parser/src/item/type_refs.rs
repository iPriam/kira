//! Type references and the signature pieces every declaration shares.
//!
//! A written type — a name, an array, or a function type — and the parameter
//! list, ownership prefix, and return clause that surround one. Split out of
//! [`super`] so the item grammar there stays about items rather than the types
//! they mention.

use kira_source::Span;
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{
    ConstantDecl, DistinctDecl, Param, ReceiverDecl, TypeAliasDecl, TypeRef, TypeRefId,
};
use kira_syntax_model::ownership::OwnershipMode;

use crate::Parser;

/// What follows a written type, which decides whether the compat `Any Family`
/// spelling can be told apart from the `Any` top type.
///
/// A two-word type is only unambiguous when something other than a name must
/// come next. Carried as a type rather than a `bool` so the two positions are
/// named at every call site instead of a bare `true` nobody can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeEnd {
    /// A delimiter must follow: `)`, `]`, `,`, `=`, `{`, `->`. A trailing
    /// identifier can only be part of the type, so `Any Widget` is unambiguous.
    Enclosed,
    /// The type may be the statement's last token, so a following identifier
    /// may be the next statement. `Any Family` is accepted only when what
    /// follows the family name is the binding's `=`.
    StatementFinal,
}

impl Parser<'_> {
    /// Parses `let Name: T = value` at module scope.
    ///
    /// The type annotation is optional for the reason a local's is: the
    /// initializer usually says the type already. There is no `var` counterpart
    /// — a module-scope binding is one value shared by a whole program, and a
    /// mutable one would be shared mutable state with an observable
    /// initialization order.
    pub(crate) fn parse_constant(&mut self) -> Option<ConstantDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Let);
        if !self.at(TokenKind::Identifier) {
            self.error(self.current().span, "KPAR080", "expected a constant name");
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        let declared_type = if self.eat(TokenKind::Colon) {
            Some(self.parse_type_ref())
        } else {
            None
        };
        // `=` is required: a constant with no value names nothing.
        if !self.eat(TokenKind::Equals) {
            self.error(
                self.current().span,
                "KPAR081",
                "a module-scope `let` needs an `=` initializer",
            );
            return None;
        }
        let value = self.parse_expr();
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(ConstantDecl {
            name,
            name_span,
            declared_type,
            value,
            span,
        })
    }

    /// Parses `type Name = Target`.
    ///
    /// The target is an ordinary type reference, so `type ByteMatrix =
    /// [[Byte]]` needs no grammar of its own. A missing name yields no node at
    /// all: an alias with nothing to bind would register a name nobody wrote,
    /// and the `type` keyword is already consumed, so recovery still advances.
    pub(crate) fn parse_type_alias(&mut self) -> Option<TypeAliasDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Type);
        if !self.at(TokenKind::Identifier) {
            self.error(self.current().span, "KPAR032", "expected a type alias name");
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        // `=` is required, not optional: `type Name` on its own aliases nothing.
        self.expect(TokenKind::Equals);
        let target = self.parse_type_ref();
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(TypeAliasDecl {
            name,
            name_span,
            target,
            span,
        })
    }

    /// Parses `distinct Name = Representation`.
    ///
    /// The same shape as `type Name = Target` and the opposite meaning, so it
    /// is parsed beside it: what the two produce is one written type reference
    /// bound to one name, and only analysis decides whether that binding is a
    /// second spelling or a second type. A missing name yields no node, exactly
    /// as an alias's does — a declaration with nothing to bind would register a
    /// name nobody wrote.
    pub(crate) fn parse_distinct(&mut self) -> Option<DistinctDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Distinct);
        if !self.at(TokenKind::Identifier) {
            self.error(
                self.current().span,
                "KPAR084",
                "expected a name after `distinct`",
            );
            // Consume the `= Representation` the declaration was going to bind,
            // so one missing name is one diagnostic rather than a stray-token
            // cascade over the type nobody can now name.
            if self.eat(TokenKind::Equals) {
                self.parse_type_ref();
            }
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        // `=` is required for the reason an alias's is: `distinct Name` alone
        // declares a type with no representation, which is a type nothing can
        // build a value of.
        if !self.eat(TokenKind::Equals) {
            self.error(
                self.current().span,
                "KPAR085",
                "a `distinct` declaration needs `= Representation`, the type it is at run time",
            );
            return None;
        }
        let representation = self.parse_type_ref();
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(DistinctDecl {
            name,
            name_span,
            representation,
            span,
        })
    }

    // ----- shared signature pieces ---------------------------------------

    pub(crate) fn parse_params(&mut self) -> Vec<Param> {
        let (receiver, params) = self.parse_signature_params();
        if let Some(receiver) = receiver {
            self.error(
                receiver.span,
                "KPAR074",
                "only a method declares a `self` receiver, and this declaration runs on no \
                 value; drop the receiver",
            );
        }
        params
    }

    /// Parses `( [self,] <param>* )`, reporting the receiver separately.
    ///
    /// A leading `self` — bare, `borrow self`, or `borrow mut self` — is the
    /// receiver rather than a parameter: it names no type, because its type is
    /// whatever the declaration is a method of. Every other position calls
    /// [`Parser::parse_params`], which refuses one.
    pub(crate) fn parse_signature_params(&mut self) -> (Option<ReceiverDecl>, Vec<Param>) {
        let mut params = Vec::new();
        if !self.expect(TokenKind::LParen) {
            return (None, params);
        }
        let receiver = self.parse_receiver();
        if receiver.is_some() && !self.at(TokenKind::RParen) {
            self.expect(TokenKind::Comma);
        }
        while !self.at(TokenKind::RParen) && !self.at_eof() {
            let before = self.pos;
            if let Some(param) = self.parse_param() {
                params.push(param);
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RParen);
        (receiver, params)
    }

    /// Consumes a written `self` receiver at the head of a parameter list.
    ///
    /// `self` is an ordinary identifier everywhere else, so the receiver is
    /// committed to only when the name is followed by `,` or `)` — a parameter
    /// *named* `self` writes `self: T` and still parses as one.
    ///
    /// Only the two borrow modes reach a receiver. A consuming receiver would
    /// be a method that destroys the value it was called on, which is a
    /// different call rule at every site; it is refused by name rather than
    /// read as a borrow.
    fn parse_receiver(&mut self) -> Option<ReceiverDecl> {
        let start = self.current().span;
        let (mutable, offset) = if self.at_word("borrow")
            && self.peek_is_word(1, "mut")
            && self.peek_is_word(2, "self")
        {
            (true, 3)
        } else if self.at_word("borrow") && self.peek_is_word(1, "self") {
            (false, 2)
        } else if self.at_word("self") {
            (false, 1)
        } else {
            return None;
        };
        if !matches!(self.peek(offset).kind, TokenKind::Comma | TokenKind::RParen) {
            return None;
        }
        // A bare `self` is refused rather than read as a borrow: it reads like
        // the consuming receiver it is not.
        let bare = offset == 1;
        for _ in 0..offset {
            self.bump();
        }
        let span = Span::from_bounds(start.start, self.previous_end());
        if bare {
            self.error(
                span,
                "KPAR075",
                "write `borrow self` or `borrow mut self`: a receiver borrows, and a bare \
                 `self` would read as a method that consumes the value it was called on",
            );
        }
        Some(ReceiverDecl { mutable, span })
    }

    fn parse_param(&mut self) -> Option<Param> {
        if !self.at(TokenKind::Identifier) {
            self.error(self.current().span, "KPAR005", "expected a parameter name");
            return None;
        }
        let name_span = self.current().span;
        let name = self.intern_span(name_span);
        self.bump();
        self.expect(TokenKind::Colon);
        let (ownership, ownership_span) = self.parse_ownership_prefix();
        let ty = self.parse_type_ref();
        // A trailing `= expr` gives the parameter a default, the same grammar a
        // struct field or enum-variant payload already uses. Semantics resolves
        // it once in the declaring file and fills it at a call that omits it.
        let default = self.eat(TokenKind::Equals).then(|| self.parse_expr());
        let span = Span::from_bounds(name_span.start, self.previous_end());
        Some(Param {
            name,
            name_span,
            ownership,
            ownership_span,
            ty,
            default,
            span,
        })
    }

    /// Parses the ownership prefix of a written type, if one is written.
    ///
    /// Accepts `borrow`, `borrow mut`, `move`, and `copy`. All four are
    /// contextual identifiers, so each is committed to only when a type name
    /// follows it — that is what keeps `f(borrow: Int)` (a parameter *named*
    /// `borrow`) and `f(x: move)` (a type named `move`, were one declared)
    /// parsing as they always did. A bare type yields
    /// [`OwnershipMode::Owned`], which is the default rather than a fallback.
    ///
    /// Shared by every position that admits a prefix — a declared parameter, a
    /// function type's parameter, a `let` or `var` annotation — because the
    /// lookahead that makes the keyword contextual has to be identical in all
    /// of them or one position would commit where another backs off.
    pub(crate) fn parse_ownership_prefix(&mut self) -> (OwnershipMode, Option<Span>) {
        if self.at_word("borrow") && self.peek_is_word(1, "mut") && self.peek_starts_type(2) {
            let start = self.current().span;
            self.bump(); // `borrow`
            let end = self.current().span;
            self.bump(); // `mut`
            return (
                OwnershipMode::BorrowMut,
                Some(Span::from_bounds(start.start, end.end())),
            );
        }
        if self.at_word("borrow") && self.peek_starts_type(1) {
            let span = self.current().span;
            self.bump();
            return (OwnershipMode::BorrowRead, Some(span));
        }
        for (word, mode) in [("move", OwnershipMode::Move), ("copy", OwnershipMode::Copy)] {
            if self.at_word(word) && self.peek_starts_type(1) {
                let span = self.current().span;
                self.bump();
                return (mode, Some(span));
            }
        }
        (OwnershipMode::Owned, None)
    }

    pub(crate) fn parse_return_type(&mut self) -> Option<TypeRefId> {
        // Kira accepts both `-> Type` and `): Type`.
        if self.eat(TokenKind::Arrow) || self.eat(TokenKind::Colon) {
            Some(self.parse_type_ref())
        } else {
            None
        }
    }

    /// Whether the token `n` ahead can begin a written type.
    ///
    /// A type starts with a name (`Int`, `Point`), with `[` (`[Int]`), or with
    /// `(` (`(Int) -> Void`). This is what every contextual-keyword lookahead
    /// asks, and asking it in one place is what keeps `borrow [Int]` from
    /// silently parsing as a parameter whose type is named `borrow`.
    pub(crate) fn peek_starts_type(&self, n: usize) -> bool {
        matches!(
            self.peek(n).kind,
            TokenKind::Identifier | TokenKind::LBracket | TokenKind::LParen
        )
    }

    /// Whether the cursor sits on the `some Family` existential spelling.
    ///
    /// `some` is contextual, so it is committed to only when a name follows —
    /// which is what keeps a type *named* `some`, or a parameter named `some`,
    /// parsing as it always did. One `[some X]` spelling falls out for free:
    /// the array branch recurses into this same function.
    pub(crate) fn at_some_construct(&self) -> bool {
        self.at_word("some") && self.peek(1).kind == TokenKind::Identifier
    }

    /// Whether the cursor sits on `Any Family`, the spelling of the family
    /// existential.
    ///
    /// `Any` is also the top type, so in a position where the type may end the
    /// statement a following name could instead begin the next one. A binding
    /// always carries an `=`, so requiring one after the family name is what
    /// tells the two apart — `let x: Any` followed by `render()` still reads as
    /// two statements, and `let x: Any Widget = w` reads as one.
    fn at_any_construct(&self, end: TypeEnd) -> bool {
        if !(self.at_word("Any") && self.peek(1).kind == TokenKind::Identifier) {
            return false;
        }
        if end == TypeEnd::Enclosed {
            return true;
        }
        let mut ahead = 2;
        while self.peek(ahead).kind == TokenKind::Dot
            && self.peek(ahead + 1).kind == TokenKind::Identifier
        {
            ahead += 2;
        }
        self.peek(ahead).kind == TokenKind::Equals
    }

    /// Whether the cursor sits on `[some Family]`, the list existential.
    ///
    /// Asked only by the construct grammar, which needs to know a field is a
    /// child slot *before* the type is parsed. [`Parser::parse_type_ref`] needs
    /// no such lookahead: its array branch recurses.
    pub(crate) fn at_bracketed_some_construct(&self) -> bool {
        self.at(TokenKind::LBracket)
            && self.peek(1).kind == TokenKind::Identifier
            && self.text_of(self.peek(1).span) == "some"
            && self.peek(2).kind == TokenKind::Identifier
    }

    /// Consumes a possibly module-qualified name starting at `start`, returning
    /// its text and the span covering every segment.
    ///
    /// Shared by the nominal and existential branches so `some Support.Widget`
    /// qualifies exactly the way `Support.Widget` does — one grammar, not two
    /// that can drift apart.
    fn parse_qualified_name(&mut self, start: Span) -> (String, Span) {
        let mut text = self.text_of(start).to_owned();
        self.bump();
        while self.at(TokenKind::Dot) && self.peek(1).kind == TokenKind::Identifier {
            self.bump(); // `.`
            let segment = self.current().span;
            text.push('.');
            text.push_str(self.text_of(segment));
            self.bump();
        }
        (text, Span::from_bounds(start.start, self.previous_end()))
    }

    /// Parses a written type: a name, `some Family`, `[` element `]`, or a
    /// function type, nested to any depth.
    ///
    /// A name may be **module-qualified** (`Support.Point`). The qualifier is
    /// kept in the interned name — a dot cannot appear in an identifier, so a
    /// qualified spelling can never collide with a declared one — and semantics
    /// is what strips it against the file's imports.
    ///
    /// `some Family` is the Construct 2.0 existential: "a value of some concrete
    /// declaration backing `Family`". It resolves to the same type bare `Family`
    /// does, so it is parsed as its own node purely to earn the check that the
    /// name really is a family.
    ///
    /// A leading `(` always starts a function type: no other written type is
    /// parenthesized, so there is nothing to disambiguate against. That is also
    /// why a function result type is spelled with `:` rather than `->` on a
    /// declaration — `function f(): (Int) -> Int` — and both spellings are
    /// accepted for every other result type.
    pub(crate) fn parse_type_ref(&mut self) -> TypeRefId {
        self.parse_type_ref_ending(TypeEnd::Enclosed)
    }

    /// Parses a written type that may be the final token of its statement.
    ///
    /// Only the local-binding annotation is in this position, and it is the one
    /// place the compat `Any Family` spelling is refused: there, and only there,
    /// a bare `Any` can be followed by an identifier that starts the *next
    /// statement* rather than naming a family.
    pub(crate) fn parse_type_ref_statement_final(&mut self) -> TypeRefId {
        self.parse_type_ref_ending(TypeEnd::StatementFinal)
    }

    fn parse_type_ref_ending(&mut self, end: TypeEnd) -> TypeRefId {
        let allowed = self.enter_nesting();
        if !allowed {
            self.recover_refused_nesting();
            let span = self.current().span;
            return self.tree.add_type(TypeRef::Error { span });
        }
        let ty = self.parse_type_ref_inner(end);
        self.exit_nesting();
        ty
    }

    fn parse_type_ref_inner(&mut self, end: TypeEnd) -> TypeRefId {
        if self.at(TokenKind::LParen) {
            return self.parse_function_type();
        }
        if self.at(TokenKind::LBracket) {
            let start = self.current().span;
            self.bump(); // `[`
            // An element type is enclosed by the brackets whatever encloses the
            // array, so `var xs: [Any Widget]` keeps the compat spelling.
            let element = self.parse_type_ref();
            self.expect(TokenKind::RBracket);
            let span = Span::from_bounds(start.start, self.previous_end());
            return self.tree.add_type(TypeRef::Array { element, span });
        }
        if self.at(TokenKind::Identifier) {
            let start = self.current().span;
            if self.at_some_construct() || self.at_any_construct(end) {
                self.bump(); // `some` / `Any`
                let family_start = self.current().span;
                let (text, family_span) = self.parse_qualified_name(family_start);
                let family = self.intern_text(&text, family_span);
                let span = Span::from_bounds(start.start, self.previous_end());
                return self.tree.add_type(TypeRef::SomeConstruct {
                    family,
                    family_span,
                    span,
                });
            }

            let (text, span) = self.parse_qualified_name(start);
            let name = self.intern_text(&text, span);
            // `Name<...>` is a generic instantiation. A type position has no
            // comparison operator, so a `<` here is never ambiguous.
            if self.at_type_params() {
                return self.parse_generic_args(name, span, start);
            }
            return self.tree.add_type(TypeRef::Named { name, span });
        }
        let span = self.current().span;
        self.error(span, "KPAR006", "expected a type name");
        self.tree.add_type(TypeRef::Error { span })
    }

    /// Parses `(A, B) -> R`, with the cursor on `(`.
    ///
    /// The result is mandatory: a function type with no `->` names nothing, so
    /// a missing arrow is reported and the whole type recovers to
    /// [`TypeRef::Error`] rather than silently becoming `() -> Void`.
    ///
    /// A parameter may carry an ownership prefix — `(borrow GraphicsEvent) ->
    /// Void` — exactly as a declared parameter may. The mode is carried on the
    /// type rather than dropped: it is invisible at run time but decisive at the
    /// ownership check, so an indirect call reads it to decide whether an
    /// argument needs `move`.
    fn parse_function_type(&mut self) -> TypeRefId {
        let start = self.current().span;
        self.bump(); // `(`
        let mut params = Vec::new();
        let mut param_ownership = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at_eof() {
            let before = self.pos;
            let (mode, _) = self.parse_ownership_prefix();
            param_ownership.push(mode);
            params.push(self.parse_type_ref());
            self.eat(TokenKind::Comma);
            // A parameter that consumed nothing would spin; force progress.
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RParen);
        if !self.eat(TokenKind::Arrow) {
            let span = Span::from_bounds(start.start, self.previous_end());
            self.error(span, "KPAR038", "expected `->` in a function type");
            return self.tree.add_type(TypeRef::Error { span });
        }
        let result = self.parse_type_ref();
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.add_type(TypeRef::Function {
            params,
            param_ownership,
            result,
            span,
        })
    }
}
