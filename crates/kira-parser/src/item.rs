//! Top-level item parsing: functions, structs, and the parse-don't-crash path
//! for constructs the v0 subset does not analyze yet.
//!
//! Recovery boundary: an item that cannot be parsed consumes its balanced
//! `{...}` body if it has one and becomes an [`Item::Unsupported`] node, so one
//! malformed declaration never derails the rest of the file.
//!
//! Two grammars that surround an item live in submodules of this one, so this
//! file stays about items: [`foreign`] parses the `@FFI.Extern { ... }` block,
//! and [`type_refs`] parses written types and the signature pieces every
//! declaration shares.

mod foreign;
mod type_refs;

use kira_core::Symbol;
use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_runtime_abi::Execution;
use kira_source::{FileSpan, Span};
use kira_syntax_model::TokenKind;
use kira_syntax_model::ast::{
    Block, ExportMark, FfiTypeMark, ForeignMark, Function, ImportDecl, Item, UnsupportedItem,
};

use crate::Parser;

/// What a run of `@Name` annotations on one declaration said.
pub(crate) struct Annotations {
    /// Whether `@Main` was written.
    pub(crate) is_main: bool,
    /// The engine `@Runtime` / `@Native` selected, or
    /// [`Execution::Inherited`] when neither was written.
    pub(crate) execution: Execution,
    /// The `@Export` marker, when one was written.
    pub(crate) export: Option<ExportMark>,
    /// The `@FFI.Extern { ... }` marker, when one was written.
    pub(crate) foreign: Option<ForeignMark>,
    /// The `@FFI.Struct`/`Pointer`/`Alias`/`Array`/`Callback` marker, when one
    /// was written. These ride a struct declaration, not a function.
    pub(crate) ffi_type: Option<FfiTypeMark>,
    /// The span of a `@Derive(Copy)` written on the declaration.
    ///
    /// `Copy` is the one derive the compiler owns: macro expansion leaves it in
    /// place precisely so the type checker can see it, because it generates
    /// nothing and asserts something only the field types can settle.
    pub(crate) derives_copy: Option<Span>,
}

impl Default for Annotations {
    fn default() -> Self {
        Self {
            is_main: false,
            execution: Execution::Inherited,
            export: None,
            foreign: None,
            ffi_type: None,
            derives_copy: None,
        }
    }
}

impl Parser<'_> {
    pub(crate) fn parse_item(&mut self) {
        match self.current_kind() {
            TokenKind::At => self.parse_annotated_item(),
            TokenKind::Function => {
                if let Some(function) = self.parse_function(false, Execution::Inherited, false) {
                    self.tree.push_item(self.source, Item::Function(function));
                }
            }
            TokenKind::Struct => {
                if let Some(declaration) = self.parse_struct() {
                    self.tree.push_item(self.source, Item::Struct(declaration));
                }
            }
            TokenKind::Enum => {
                if let Some(declaration) = self.parse_enum() {
                    self.tree.push_item(self.source, Item::Enum(declaration));
                }
            }
            TokenKind::Type => {
                if let Some(declaration) = self.parse_type_alias() {
                    self.tree
                        .push_item(self.source, Item::TypeAlias(declaration));
                }
            }
            TokenKind::Import => {
                if let Some(declaration) = self.parse_import() {
                    self.tree.push_item(self.source, Item::Import(declaration));
                }
            }
            TokenKind::Class => {
                if let Some(declaration) = self.parse_class() {
                    self.tree.push_item(self.source, Item::Class(declaration));
                }
            }
            TokenKind::Construct => {
                if let Some(declaration) = self.parse_construct_family() {
                    self.tree
                        .push_item(self.source, Item::Construct(declaration));
                }
            }
            // `extend Family { ... }` leads with the contextual keyword
            // `extend`, whose second token is an identifier and third a brace —
            // the same shape a construct-backed declaration wears, so it is
            // matched first.
            TokenKind::Identifier if self.at_extend_block() => {
                if let Some(declaration) = self.parse_extend() {
                    self.tree.push_item(self.source, Item::Extend(declaration));
                }
            }
            // `Family Name(params) { ... }` — a construct-backed declaration —
            // is the one identifier-led top-level form. Anything else that
            // starts with an identifier is still parse-don't-crash.
            TokenKind::Identifier if self.at_construct_backed() => {
                if let Some(declaration) = self.parse_construct_backed() {
                    self.tree
                        .push_item(self.source, Item::Construct(declaration));
                }
            }
            TokenKind::Identifier => self.parse_unsupported_item(),
            _ => {
                // Stray token at top level: skip it with a diagnostic.
                let span = self.current().span;
                self.error(
                    span,
                    "KPAR002",
                    format!("unexpected {} at top level", self.current_kind().describe()),
                );
                self.bump();
            }
        }
    }

    fn parse_annotated_item(&mut self) {
        let start = self.current().span;
        let annotations = self.parse_annotations();
        if self.at(TokenKind::Function) {
            if let Some(function) = self.parse_function_annotated(&annotations) {
                self.tree.push_item(self.source, Item::Function(function));
            }
        } else if self.at(TokenKind::Class) {
            // `@Export class` is the handle-eligibility marker, so a class is
            // the one non-function item annotations reach.
            if annotations.is_main || annotations.execution != Execution::Inherited {
                self.error(
                    start,
                    "KPAR041",
                    "only `@Export` may annotate a class; `@Main`, `@Runtime`, and \
                     `@Native` select how a *function* runs",
                );
            }
            if let Some(mut declaration) = self.parse_class() {
                declaration.export = annotations.export;
                declaration.span = Span::from_bounds(start.start, self.previous_end());
                self.tree.push_item(self.source, Item::Class(declaration));
            }
        } else if self.at(TokenKind::Struct) {
            // `@Main`, `@Runtime`, and `@Native` select how a *function* runs,
            // so they say nothing about a struct either. Same rule as the class
            // arm above, so the same code: an execution or entrypoint marker on
            // a non-function item is refused rather than silently dropped.
            if annotations.is_main || annotations.execution != Execution::Inherited {
                self.error(
                    start,
                    "KPAR041",
                    "`@Main`, `@Runtime`, and `@Native` select how a *function* runs, \
                     so none of them may annotate a struct",
                );
            }
            // Only a class mints handles, so `@Export struct` is refused by
            // name and pointed at the fix. The struct is still parsed and
            // registered: dropping it would turn one refusal into an
            // unresolved-type cascade at every use.
            if annotations.export.is_some() {
                self.error(
                    start,
                    "KPAR043",
                    "a struct cannot be `@Export`: an export boundary carries one \
                     tag and one word, so only a class crosses — as an opaque \
                     handle. Declare this a `class` to export it.",
                );
            }
            // `@FFI.Extern` rides a function, never a struct: a struct declares
            // a type, not a foreign callable. The mark is dropped and refused so
            // the struct still parses as an ordinary type.
            if let Some(foreign) = &annotations.foreign {
                self.error(
                    foreign.span,
                    "KPAR056",
                    "`@FFI.Extern` annotates a foreign *function*, not a struct; a \
                     C-layout type is `@FFI.Struct`",
                );
            }
            if let Some(mut declaration) = self.parse_struct() {
                declaration.ffi = annotations.ffi_type;
                declaration.derives_copy = annotations.derives_copy;
                declaration.span = Span::from_bounds(start.start, self.previous_end());
                self.tree.push_item(self.source, Item::Struct(declaration));
            }
        } else if self.at(TokenKind::Enum) {
            // An enum reaches an annotation for exactly one reason —
            // `@Derive(Copy)` — so it is parsed as an enum rather than falling
            // to parse-don't-crash, which would have taken its variants with it.
            if let Some(mut declaration) = self.parse_enum() {
                declaration.derives_copy = annotations.derives_copy;
                declaration.span = Span::from_bounds(start.start, self.previous_end());
                self.tree.push_item(self.source, Item::Enum(declaration));
            }
        } else {
            // Annotated non-function construct: parse-don't-crash.
            self.parse_unsupported_item_from(start);
        }
    }

    /// Consumes a run of `@Name` annotations and reports what they said.
    ///
    /// Shared by the top level and a class body, so a method's annotations are
    /// recorded rather than hitting the "expected a class member" arm — which
    /// is what lets `@Export` on a method be refused by name in semantics
    /// instead of as a syntax error about the wrong thing.
    pub(crate) fn parse_annotations(&mut self) -> Annotations {
        let mut annotations = Annotations::default();
        while self.at(TokenKind::At) {
            self.bump();
            if !self.at(TokenKind::Identifier) {
                self.error(
                    self.current().span,
                    "KPAR003",
                    "expected an annotation name after `@`",
                );
                break;
            }
            // A qualified annotation — `@FFI.Extern { ... }` — is the one whose
            // name is `identifier . identifier`. It is recognized before the
            // bare-name arm so `FFI` is never read as a plain annotation.
            if self.peek(1).kind == TokenKind::Dot && self.peek(2).kind == TokenKind::Identifier {
                self.parse_qualified_annotation(&mut annotations);
                continue;
            }
            let name_span = self.current().span;
            let is_derive = self.text_of(name_span) == "Derive";
            let is_export = match self.text_of(name_span) {
                "Main" => {
                    annotations.is_main = true;
                    false
                }
                "Export" => true,
                name => {
                    if let Some(selected) = Execution::from_annotation(name) {
                        // Two engines on one function is a contradiction,
                        // not a refinement: the second would silently win.
                        if annotations.execution != Execution::Inherited
                            && annotations.execution != selected
                        {
                            self.error(
                                name_span,
                                "KPAR005",
                                "a function selects one execution engine; \
                                 `@Runtime` and `@Native` cannot both apply",
                            );
                        }
                        annotations.execution = selected;
                    }
                    false
                }
            };
            self.bump();
            // An optional `(...)` argument list, and — for `@Export` only —
            // the pinned `{ name: value; }` annotation block. Both are skipped
            // balanced and their span recorded: `@Export` takes neither, and
            // the refusal points at what was written.
            let mut payload_span = None;
            if self.at(TokenKind::LParen) {
                let open = self.current().span;
                self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
                payload_span = Some(Span::from_bounds(open.start, self.previous_end()));
            } else if is_export && self.at(TokenKind::LBrace) {
                let open = self.current().span;
                self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                payload_span = Some(Span::from_bounds(open.start, self.previous_end()));
            }
            // `@Derive(Copy)` is the compiler's own derive: it generates
            // nothing, so it survives macro expansion and is read here. Every
            // other name in the list belonged to a macro and is gone by now.
            if is_derive
                && let Some(payload) = payload_span
                && self.text_of(payload).contains("Copy")
            {
                annotations.derives_copy = Some(Span::from_bounds(name_span.start, payload.end()));
            }
            if is_export {
                // A repeated `@Export` keeps the first mark; the payload of
                // whichever spelling carried one is what gets refused.
                let existing = annotations.export.and_then(|mark| mark.payload_span);
                annotations.export = Some(ExportMark {
                    span: annotations.export.map_or(name_span, |mark| mark.span),
                    payload_span: payload_span.or(existing),
                });
            }
        }
        annotations
    }

    /// Parses a function carrying the annotations already consumed for it.
    pub(crate) fn parse_function_annotated(
        &mut self,
        annotations: &Annotations,
    ) -> Option<Function> {
        // A struct-shaped `@FFI.*` form declares a *type*, so it never rides a
        // function. It is refused and dropped so the function still parses.
        if let Some(mark) = &annotations.ffi_type {
            self.error(
                mark.name_span,
                "KPAR056",
                format!(
                    "`@FFI.{}` declares a C type and annotates a `struct`, not a \
                     function; a foreign function is `@FFI.Extern`",
                    mark.kind.label()
                ),
            );
        }
        let mut function = self.parse_function(
            annotations.is_main,
            annotations.execution,
            annotations.foreign.is_some(),
        )?;
        function.export = annotations.export;
        function.foreign = annotations.foreign.clone();
        Some(function)
    }

    /// Parses a function declaration.
    ///
    /// `is_foreign` says whether an `@FFI.Extern` marker preceded it: a foreign
    /// function is **bodyless** (it ends with `;` and its stored body is an
    /// empty block spanned at that `;`), and an ordinary function requires a
    /// `{ ... }` body. Threading the flag here is what keeps the two apart at
    /// the one place a body would be read.
    pub(crate) fn parse_function(
        &mut self,
        is_main: bool,
        execution: Execution,
        is_foreign: bool,
    ) -> Option<Function> {
        let start = self.current().span;
        self.expect(TokenKind::Function);
        let (name, name_span) = if self.at(TokenKind::Identifier) {
            let span = self.current().span;
            (self.intern_span(span), span)
        } else {
            self.error(self.current().span, "KPAR004", "expected a function name");
            (Symbol::ERROR, self.current().span)
        };
        if self.at(TokenKind::Identifier) {
            self.bump();
        }
        self.refuse_type_params("function");
        let params = self.parse_params();
        let return_type = self.parse_return_type();
        let body = self.parse_function_body(is_foreign);
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(Function {
            name,
            name_span,
            is_main,
            // Set by `parse_function_annotated` when annotations preceded the
            // declaration; a bare `function` carries none.
            export: None,
            foreign: None,
            execution,
            params,
            return_type,
            body,
            span,
        })
    }

    /// Parses the body of a function, or the `;` of a bodyless extern.
    ///
    /// A foreign function has no body: it ends with `;`, so a `{` there is a
    /// mistake, and the stored body is an empty block spanned at the `;`. An
    /// ordinary function requires a body, so a `;` there is the mistake.
    fn parse_function_body(&mut self, is_foreign: bool) -> Block {
        if is_foreign {
            if self.at(TokenKind::LBrace) {
                self.error(
                    self.current().span,
                    "KPAR054",
                    "an `@FFI.Extern` function has no body; end its declaration with `;`",
                );
                return self.parse_block();
            }
            let semi = self.current().span;
            self.expect(TokenKind::Semicolon);
            return Block {
                stmts: Vec::new(),
                span: semi,
            };
        }
        if self.at(TokenKind::Semicolon) {
            let semi = self.current().span;
            self.error(
                semi,
                "KPAR055",
                "expected a function body; only an `@FFI.Extern` function is bodyless",
            );
            self.bump();
            return Block {
                stmts: Vec::new(),
                span: semi,
            };
        }
        self.parse_block()
    }

    /// Parses `import Module[.Sub…] [as Alias]`.
    ///
    /// Recovery: a malformed path yields no item at all rather than a partial
    /// one, because an import with no module names nothing a later phase could
    /// resolve — the parser has already said what was wrong, and inventing a
    /// module would produce a second, misleading "unresolved import".
    fn parse_import(&mut self) -> Option<ImportDecl> {
        let start = self.current().span;
        self.expect(TokenKind::Import);
        let mut path = Vec::new();
        let path_start = self.current().span;
        loop {
            if !self.at(TokenKind::Identifier) {
                self.error(
                    self.current().span,
                    "KPAR016",
                    "expected a module name after `import`",
                );
                return None;
            }
            let span = self.current().span;
            path.push(self.intern_span(span));
            self.bump();
            if !self.eat(TokenKind::Dot) {
                break;
            }
        }
        let path_span = Span::from_bounds(path_start.start, self.previous_end());
        // `as` is a keyword, so the alias clause needs no contextual lookahead.
        let (alias, alias_span) = if self.eat(TokenKind::As) {
            if self.at(TokenKind::Identifier) {
                let span = self.current().span;
                let symbol = self.intern_span(span);
                self.bump();
                (Some(symbol), Some(span))
            } else {
                self.error(self.current().span, "KPAR017", "expected a name after `as`");
                (None, None)
            }
        } else {
            (None, None)
        };
        let span = Span::from_bounds(start.start, self.previous_end());
        Some(ImportDecl {
            path,
            path_span,
            alias,
            alias_span,
            span,
        })
    }

    pub(crate) fn parse_block(&mut self) -> Block {
        let start = self.current().span;
        if !self.expect(TokenKind::LBrace) {
            return Block {
                stmts: Vec::new(),
                span: start,
            };
        }
        self.parse_block_body(start)
    }

    /// Parses statements up to and including the closing `}`, with the opening
    /// `{` (whose span is `start`) already consumed.
    ///
    /// Split out because a closure's body is the same statement list behind the
    /// same brace, only reached after its parameters and `in` were consumed —
    /// so it cannot call [`Parser::parse_block`], which would demand a second
    /// `{`.
    pub(crate) fn parse_block_body(&mut self, start: Span) -> Block {
        let mut stmts = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at_eof() {
            let before = self.pos;
            while self.eat(TokenKind::Semicolon) {}
            if self.at(TokenKind::RBrace) || self.at_eof() {
                break;
            }
            if let Some(stmt) = self.parse_stmt() {
                stmts.push(stmt);
            }
            while self.eat(TokenKind::Semicolon) {}
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace);
        let span = Span::from_bounds(start.start, self.previous_end());
        Block { stmts, span }
    }

    // ----- unsupported constructs (parse-don't-crash) -------------------

    fn parse_unsupported_item(&mut self) {
        let start = self.current().span;
        self.parse_unsupported_item_from(start);
    }

    fn parse_unsupported_item_from(&mut self, start: Span) {
        let keyword = unsupported_keyword(self.current_kind(), self.text_of(self.current().span));
        // Walk forward: if a `{...}` body appears before the next top-level
        // starter, consume it balanced; otherwise stop at the next starter.
        while !self.at_eof() {
            match self.current_kind() {
                TokenKind::LBrace => {
                    self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                    break;
                }
                kind if is_item_start(kind) && self.current().span != start => break,
                _ => {
                    self.bump();
                }
            }
        }
        let span = Span::from_bounds(start.start, self.previous_end());
        self.tree.push_item(
            self.source,
            Item::Unsupported(UnsupportedItem { keyword, span }),
        );
        let file_span = FileSpan::new(self.source, span);
        let mut diagnostic = Diagnostic::single(
            Severity::Error,
            format!("`{keyword}` is not supported yet"),
            Label::primary(file_span, "not yet supported in this compiler"),
        );
        diagnostic.code = Some("KSEM900");
        diagnostic.phase = Some("parser");
        diagnostic.help = Some(
            "the v0 subset supports functions, structs, let/var, if/while, and arithmetic"
                .to_owned(),
        );
        self.diagnostics.push(diagnostic);
    }
}

/// Whether `kind` can begin a top-level item, used to bound error recovery.
fn is_item_start(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::At
            | TokenKind::Function
            | TokenKind::Struct
            | TokenKind::Enum
            | TokenKind::Type
            | TokenKind::Class
            | TokenKind::Construct
            | TokenKind::Import
    )
}

/// A stable label for an unsupported construct, for diagnostics.
fn unsupported_keyword(kind: TokenKind, text: &str) -> &'static str {
    match kind {
        TokenKind::Enum => "enum",
        TokenKind::Class => "class",
        TokenKind::Import => "import",
        TokenKind::Identifier => match text {
            "Package" => "Package",
            "Test" => "Test",
            "construct" => "construct",
            _ => "declaration",
        },
        _ => "declaration",
    }
}
