//! The parser's cursor, its recovery rule, and the top-level declarations.
//!
//! Error-resilient by construction: every parse function returns something,
//! reports what was wrong, and resynchronizes at a brace or a declaration
//! keyword. One malformed field never costs a file the rest of its
//! diagnostics, which is what makes the same parser usable by an editor.

use kira_core::{Interner, Symbol};
use kira_ksl_syntax_model::ast::{
    Access, ConstDecl, EnumDecl, EnumVariant, Field, Function, Group, Import, Item, OptionDecl,
    Param, Resource, ResourceKind, Shader, StageDecl, StageWord, TypeDecl, TypeRef,
};
use kira_ksl_syntax_model::token::{Token, TokenKind};
use kira_ksl_syntax_model::tree::{KslTree, TypeRefId};
use kira_source::Span;

use crate::diagnostics::{self, Reporter};

mod expr;
mod stmt;

/// The words that open a top-level declaration, used to resynchronize.
const DECLARATION_STARTS: [TokenKind; 4] = [
    TokenKind::Import,
    TokenKind::Type,
    TokenKind::Function,
    TokenKind::Shader,
];

/// The running parse: the tokens, the tree being built, and what went wrong.
pub(crate) struct Parser<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    at: usize,
    pub(crate) tree: KslTree,
    pub(crate) interner: Interner,
    pub(crate) reporter: Reporter,
}

impl<'a> Parser<'a> {
    /// Prepares a parse of `text`, whose tokens are `tokens`.
    pub(crate) fn new(text: &'a str, tokens: Vec<Token>, reporter: Reporter) -> Self {
        Self {
            text,
            tokens,
            at: 0,
            tree: KslTree::new(),
            interner: Interner::new(),
            reporter,
        }
    }

    // -- cursor ----------------------------------------------------------

    /// The kind `ahead` tokens past the cursor.
    pub(crate) fn peek(&self, ahead: usize) -> TokenKind {
        self.tokens
            .get(self.at + ahead)
            .map_or(TokenKind::Eof, |token| token.kind)
    }

    /// The kind at the cursor.
    pub(crate) fn current(&self) -> TokenKind {
        self.peek(0)
    }

    /// The span at the cursor.
    pub(crate) fn span(&self) -> Span {
        self.tokens
            .get(self.at)
            .map_or_else(|| Span::new(0, 0), |token| token.span)
    }

    /// The source text the token at the cursor covers.
    pub(crate) fn slice(&self) -> &'a str {
        let span = self.span();
        self.text
            .get(span.start as usize..span.end() as usize)
            .unwrap_or("")
    }

    /// Steps past the token at the cursor and answers its span.
    pub(crate) fn advance(&mut self) -> Span {
        let span = self.span();
        if self.at < self.tokens.len() {
            self.at += 1;
        }
        span
    }

    /// Steps past the token at the cursor when it is `kind`.
    pub(crate) fn eat(&mut self, kind: TokenKind) -> bool {
        if self.current() == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Requires `kind` at the cursor, reporting when it is missing.
    ///
    /// Never consumes the wrong token: a missing `)` must not swallow the `}`
    /// that would have ended the enclosing block.
    pub(crate) fn expect(&mut self, kind: TokenKind) -> Option<Span> {
        if self.current() == kind {
            return Some(self.advance());
        }
        self.reporter.error(
            self.span(),
            diagnostics::UNEXPECTED,
            format!(
                "expected {}, found {}",
                kind.spelling(),
                self.current().spelling()
            ),
        );
        None
    }

    /// Whether the cursor is at the end of the file.
    pub(crate) fn at_end(&self) -> bool {
        self.current() == TokenKind::Eof
    }

    /// Where the cursor is, for a loop that must prove it made progress.
    pub(crate) fn at_index(&self) -> usize {
        self.at
    }

    /// Interns the text at the cursor and steps past it.
    ///
    /// The interner is bounded, so a file with more distinct names than it can
    /// hold reports rather than aborting; the placeholder keeps the parse
    /// going so the rest of the file is still checked.
    pub(crate) fn intern_current(&mut self) -> Symbol {
        let text = self.slice().to_owned();
        let span = self.advance();
        self.intern(&text, span)
    }

    /// Interns `text`, reporting a full interner at `span`.
    pub(crate) fn intern(&mut self, text: &str, span: Span) -> Symbol {
        match self.interner.intern(text) {
            Ok(symbol) => symbol,
            Err(_) => {
                self.reporter.error(
                    span,
                    diagnostics::TOO_MANY_NAMES,
                    "this file has more distinct names than the compiler can intern",
                );
                Symbol::from_u32(0)
            }
        }
    }

    /// Requires an identifier, answering its symbol and span.
    pub(crate) fn expect_name(&mut self) -> Option<(Symbol, Span)> {
        if self.current() != TokenKind::Identifier {
            self.reporter.error(
                self.span(),
                diagnostics::UNEXPECTED,
                format!("expected a name, found {}", self.current().spelling()),
            );
            return None;
        }
        let span = self.span();
        Some((self.intern_current(), span))
    }

    /// The span from `start` through the token before the cursor.
    pub(crate) fn since(&self, start: Span) -> Span {
        let end = self
            .at
            .checked_sub(1)
            .and_then(|index| self.tokens.get(index))
            .map_or(start.end(), |token| token.span.end());
        Span::new(start.start, end.saturating_sub(start.start))
    }

    // -- recovery --------------------------------------------------------

    /// Steps forward to the next declaration keyword or the end of the file.
    fn recover_to_declaration(&mut self) {
        while !self.at_end() && !DECLARATION_STARTS.contains(&self.current()) {
            self.advance();
        }
    }

    // -- items -----------------------------------------------------------

    /// Parses every top-level item until the end of the file.
    pub(crate) fn items(&mut self) {
        while !self.at_end() {
            let before = self.at;
            match self.item() {
                Some(item) => self.tree.items.push(item),
                None => {
                    if self.at == before {
                        self.reporter.error(
                            self.span(),
                            diagnostics::NOT_A_DECLARATION,
                            format!(
                                "expected `import`, `type`, `function`, or `shader`, found {}",
                                self.current().spelling()
                            ),
                        );
                        self.advance();
                    }
                    self.recover_to_declaration();
                }
            }
        }
    }

    /// Parses one top-level item.
    fn item(&mut self) -> Option<Item> {
        match self.current() {
            TokenKind::Import => self.import().map(Item::Import),
            TokenKind::Type => self.type_decl().map(Item::Type),
            TokenKind::Const => self.const_decl().map(Item::Const),
            TokenKind::Enum => self.enum_decl().map(Item::Enum),
            TokenKind::Function => self.function().map(Item::Function),
            TokenKind::Shader => self.shader().map(Item::Shader),
            _ => None,
        }
    }

    /// `import A.B as C`
    fn import(&mut self) -> Option<Import> {
        let start = self.advance();
        let mut path = vec![self.expect_name()?.0];
        while self.eat(TokenKind::Dot) {
            path.push(self.expect_name()?.0);
        }
        let alias = if self.eat(TokenKind::As) {
            Some(self.expect_name()?.0)
        } else {
            None
        };
        Some(Import {
            path,
            alias,
            span: self.since(start),
        })
    }

    /// `type Name { … }`
    fn type_decl(&mut self) -> Option<TypeDecl> {
        let start = self.advance();
        let (name, _) = self.expect_name()?;
        self.expect(TokenKind::LBrace)?;
        let mut fields = Vec::new();
        while !self.at_end() && self.current() != TokenKind::RBrace {
            let before = self.at;
            match self.field() {
                Some(field) => fields.push(field),
                None => {
                    if self.at == before {
                        self.advance();
                    }
                }
            }
        }
        self.expect(TokenKind::RBrace);
        Some(TypeDecl {
            name,
            fields,
            span: self.since(start),
        })
    }

    /// `const name: Type = <literal>`
    fn const_decl(&mut self) -> Option<ConstDecl> {
        let start = self.advance();
        let (name, _) = self.expect_name()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.type_ref()?;
        self.expect(TokenKind::Equals)?;
        let value = self.expr()?;
        Some(ConstDecl {
            name,
            ty,
            value,
            span: self.since(start),
        })
    }

    /// `enum Name { A = 0, B = 1 }`
    ///
    /// Every variant writes its number. A shader's enum names an encoding that
    /// arrived from outside, so there is nothing for declaration order to be
    /// right about.
    fn enum_decl(&mut self) -> Option<EnumDecl> {
        let start = self.advance();
        let (name, _) = self.expect_name()?;
        self.expect(TokenKind::LBrace)?;
        let mut variants = Vec::new();
        while !self.at_end() && self.current() != TokenKind::RBrace {
            let before = self.at;
            match self.enum_variant() {
                Some(variant) => variants.push(variant),
                None => {
                    if self.at == before {
                        self.advance();
                    }
                }
            }
            self.eat(TokenKind::Comma);
        }
        self.expect(TokenKind::RBrace);
        Some(EnumDecl {
            name,
            variants,
            span: self.since(start),
        })
    }

    /// One `A = 0` inside an `enum` body.
    fn enum_variant(&mut self) -> Option<EnumVariant> {
        let start = self.span();
        let (name, _) = self.expect_name()?;
        self.expect(TokenKind::Equals)?;
        let value = self.expr()?;
        Some(EnumVariant {
            name,
            value,
            span: self.since(start),
        })
    }

    /// One annotated `let name: Type` inside a `type` block.
    fn field(&mut self) -> Option<Field> {
        let start = self.span();
        let mut builtin = None;
        let mut interpolation = None;
        while self.current() == TokenKind::At {
            let (which, argument) = self.annotation()?;
            let word = self.interner.resolve(which).to_owned();
            match word.as_str() {
                "builtin" => builtin = Some(argument),
                "interpolate" => interpolation = Some(argument),
                other => self.reporter.error(
                    start,
                    diagnostics::BAD_ANNOTATION,
                    format!("`@{other}` is not an annotation KSL defines"),
                ),
            }
        }
        self.expect(TokenKind::Let)?;
        let (name, _) = self.expect_name()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.type_ref()?;
        Some(Field {
            name,
            ty,
            builtin,
            interpolation,
            span: self.since(start),
        })
    }

    /// `@name(argument)`, answering both words.
    fn annotation(&mut self) -> Option<(Symbol, Symbol)> {
        self.advance();
        let (which, _) = self.expect_name()?;
        self.expect(TokenKind::LParen)?;
        let (argument, _) = self.expect_name()?;
        self.expect(TokenKind::RParen)?;
        Some((which, argument))
    }

    /// A written type: a dotted path, or `[T]`.
    pub(crate) fn type_ref(&mut self) -> Option<TypeRefId> {
        let start = self.span();
        if self.eat(TokenKind::LBracket) {
            let element = self.type_ref()?;
            self.expect(TokenKind::RBracket)?;
            return Some(self.tree.types.alloc(TypeRef::Array {
                element,
                span: self.since(start),
            }));
        }
        let mut path = vec![self.expect_name()?.0];
        while self.current() == TokenKind::Dot && self.peek(1) == TokenKind::Identifier {
            self.advance();
            path.push(self.expect_name()?.0);
        }
        Some(self.tree.types.alloc(TypeRef::Named {
            path,
            span: self.since(start),
        }))
    }

    /// `function name(params) -> Type { … }`
    pub(crate) fn function(&mut self) -> Option<Function> {
        let start = self.advance();
        let (name, _) = self.expect_name()?;
        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();
        while !self.at_end() && self.current() != TokenKind::RParen {
            let param_start = self.span();
            let Some((param_name, _)) = self.expect_name() else {
                break;
            };
            if self.expect(TokenKind::Colon).is_none() {
                break;
            }
            let Some(ty) = self.type_ref() else {
                break;
            };
            params.push(Param {
                name: param_name,
                ty,
                span: self.since(param_start),
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen)?;
        let result = if self.eat(TokenKind::Arrow) {
            Some(self.type_ref()?)
        } else {
            None
        };
        let body = self.block()?;
        Some(Function {
            name,
            params,
            result,
            body,
            span: self.since(start),
        })
    }

    /// `shader Name { … }`
    fn shader(&mut self) -> Option<Shader> {
        let start = self.advance();
        let (name, _) = self.expect_name()?;
        self.expect(TokenKind::LBrace)?;
        let mut options = Vec::new();
        let mut groups = Vec::new();
        let mut stages = Vec::new();
        while !self.at_end() && self.current() != TokenKind::RBrace {
            let before = self.at;
            match self.current() {
                TokenKind::Option => {
                    if let Some(option) = self.option_decl() {
                        options.push(option);
                    }
                }
                TokenKind::Group => {
                    if let Some(group) = self.group() {
                        groups.push(group);
                    }
                }
                TokenKind::Identifier => match stage_word(self.slice()) {
                    Some(stage) => {
                        if let Some(declared) = self.stage(stage) {
                            stages.push(declared);
                        }
                    }
                    None => {
                        self.reporter.error(
                            self.span(),
                            diagnostics::NOT_A_DECLARATION,
                            format!(
                                "`{}` is not something a shader can contain: expected `option`, \
                                 `group`, `vertex`, `fragment`, or `compute`",
                                self.slice()
                            ),
                        );
                        self.advance();
                    }
                },
                _ => {
                    self.reporter.error(
                        self.span(),
                        diagnostics::NOT_A_DECLARATION,
                        format!(
                            "expected `option`, `group`, or a stage, found {}",
                            self.current().spelling()
                        ),
                    );
                    self.advance();
                }
            }
            if self.at == before {
                self.advance();
            }
        }
        self.expect(TokenKind::RBrace);
        Some(Shader {
            name,
            options,
            groups,
            stages,
            span: self.since(start),
        })
    }

    /// `option name: Type = value`
    fn option_decl(&mut self) -> Option<OptionDecl> {
        let start = self.advance();
        let (name, _) = self.expect_name()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.type_ref()?;
        self.expect(TokenKind::Equals)?;
        let value = self.expr()?;
        Some(OptionDecl {
            name,
            ty,
            value,
            span: self.since(start),
        })
    }

    /// `group Name { … }`
    fn group(&mut self) -> Option<Group> {
        let start = self.advance();
        let (name, _) = self.expect_name()?;
        self.expect(TokenKind::LBrace)?;
        let mut resources = Vec::new();
        while !self.at_end() && self.current() != TokenKind::RBrace {
            let before = self.at;
            match self.resource() {
                Some(resource) => resources.push(resource),
                None => {
                    if self.at == before {
                        self.advance();
                    }
                }
            }
        }
        self.expect(TokenKind::RBrace);
        Some(Group {
            name,
            resources,
            span: self.since(start),
        })
    }

    /// One resource declaration inside a group.
    fn resource(&mut self) -> Option<Resource> {
        let start = self.span();
        let mut binding = None;
        while self.current() == TokenKind::At {
            let (which, at) = self.annotation_name()?;
            let word = self.interner.resolve(which).to_owned();
            if word == "binding" {
                binding = self.binding_index();
            } else {
                self.reporter.error(
                    at,
                    diagnostics::BAD_ANNOTATION,
                    format!("`@{word}` is not an annotation a resource takes: expected `@binding`"),
                );
                return None;
            }
        }
        if self.current() != TokenKind::Identifier {
            self.reporter.error(
                start,
                diagnostics::BAD_RESOURCE,
                format!(
                    "expected `uniform`, `storage`, `texture`, or `sampler`, found {}",
                    self.current().spelling()
                ),
            );
            return None;
        }
        let kind = match self.slice() {
            "uniform" => ResourceKind::Uniform,
            "storage" => ResourceKind::Storage,
            "texture" => ResourceKind::Texture,
            "sampler" => ResourceKind::Sampler,
            other => {
                self.reporter.error(
                    start,
                    diagnostics::BAD_RESOURCE,
                    format!(
                        "`{other}` does not declare a resource: expected `uniform`, `storage`, \
                         `texture`, or `sampler`"
                    ),
                );
                return None;
            }
        };
        self.advance();
        // Storage always writes its access; a texture may. A texture with
        // none is the ordinary sampled kind, which is what every shader
        // written before storage textures existed says.
        // Storage always writes its access, and reports when it is missing; a
        // texture's is optional, so it is read only when one is actually there.
        // A texture with none is the ordinary sampled kind, which is what every
        // shader written before storage textures existed says.
        let writes_access = kind == ResourceKind::Storage
            || (kind == ResourceKind::Texture && self.at_access_word());
        let access = if writes_access {
            Some(self.access()?)
        } else {
            None
        };
        let (name, _) = self.expect_name()?;
        self.expect(TokenKind::Colon)?;
        let ty = self.type_ref()?;
        Some(Resource {
            kind,
            access,
            name,
            binding,
            ty,
            span: self.since(start),
        })
    }

    /// `@name`, leaving the cursor on the `(`. The argument is read by whoever
    /// knows what shape it takes — `@builtin(position)` a word, `@binding(3)` a
    /// number — so this answers only the annotation's name and where it began.
    fn annotation_name(&mut self) -> Option<(Symbol, Span)> {
        let at = self.span();
        self.advance();
        let (which, _) = self.expect_name()?;
        Some((which, at))
    }

    /// The `(3)` of `@binding(3)`.
    fn binding_index(&mut self) -> Option<u32> {
        self.expect(TokenKind::LParen)?;
        let span = self.span();
        if self.current() != TokenKind::IntLiteral {
            self.reporter.error(
                span,
                diagnostics::BAD_ANNOTATION,
                format!(
                    "`@binding` takes a slot number, found {}",
                    self.current().spelling()
                ),
            );
            return None;
        }
        let written = self.slice().to_owned();
        let Ok(index) = written.parse::<u32>() else {
            self.reporter.error(
                span,
                diagnostics::BAD_ANNOTATION,
                format!("`{written}` is not a slot number"),
            );
            return None;
        };
        self.advance();
        self.expect(TokenKind::RParen)?;
        Some(index)
    }

    /// Whether the cursor sits on an access word rather than on a name.
    ///
    /// A texture's access is optional, and `texture write out: …` and
    /// `texture write: …` are told apart by the word alone — no resource is
    /// named `read`, `read_write` or `write`, because those are exactly the
    /// words this rejects as names.
    fn at_access_word(&self) -> bool {
        self.current() == TokenKind::Identifier
            && matches!(self.slice(), "read" | "read_write" | "write")
    }

    /// The access mode written after `storage`, or after `texture`.
    fn access(&mut self) -> Option<Access> {
        let span = self.span();
        if self.current() != TokenKind::Identifier {
            self.reporter.error(
                span,
                diagnostics::BAD_ACCESS,
                format!(
                    "expected `read` or `read_write` after `storage`, found {}",
                    self.current().spelling()
                ),
            );
            return None;
        }
        let mode = match self.slice() {
            "read" => Access::Read,
            "read_write" => Access::ReadWrite,
            "write" => Access::Write,
            other => {
                self.reporter.error(
                    span,
                    diagnostics::BAD_ACCESS,
                    format!(
                        "`{other}` is not an access mode: expected `read`, `read_write`, or \
                         `write`"
                    ),
                );
                return None;
            }
        };
        self.advance();
        Some(mode)
    }

    /// One stage body: `vertex { … }` and its two siblings.
    fn stage(&mut self, stage: StageWord) -> Option<StageDecl> {
        let start = self.advance();
        self.expect(TokenKind::LBrace)?;
        let mut input = None;
        let mut output = None;
        let mut threads = None;
        let mut functions = Vec::new();
        while !self.at_end() && self.current() != TokenKind::RBrace {
            let before = self.at;
            match self.current() {
                TokenKind::Function => {
                    if let Some(function) = self.function() {
                        functions.push(function);
                    }
                }
                TokenKind::Identifier => match self.slice() {
                    "input" => {
                        self.advance();
                        input = self.expect_name().map(|(name, _)| name);
                    }
                    "output" => {
                        self.advance();
                        output = self.expect_name().map(|(name, _)| name);
                    }
                    "threads" => threads = self.threads(),
                    other => {
                        self.reporter.error(
                            self.span(),
                            diagnostics::BAD_STAGE_ITEM,
                            format!(
                                "`{other}` is not something a stage can contain: expected \
                                 `input`, `output`, `threads`, or `function`"
                            ),
                        );
                        self.advance();
                    }
                },
                _ => {
                    self.reporter.error(
                        self.span(),
                        diagnostics::BAD_STAGE_ITEM,
                        format!(
                            "expected `input`, `output`, `threads`, or `function`, found {}",
                            self.current().spelling()
                        ),
                    );
                    self.advance();
                }
            }
            if self.at == before {
                self.advance();
            }
        }
        self.expect(TokenKind::RBrace);
        Some(StageDecl {
            stage,
            input,
            output,
            threads,
            functions,
            span: self.since(start),
        })
    }

    /// `threads(x, y, z)`, which needs all three extents.
    fn threads(&mut self) -> Option<[kira_ksl_syntax_model::tree::ExprId; 3]> {
        let start = self.advance();
        self.expect(TokenKind::LParen)?;
        let mut extents = Vec::new();
        while !self.at_end() && self.current() != TokenKind::RParen {
            extents.push(self.expr()?);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen)?;
        match <[_; 3]>::try_from(extents) {
            Ok(three) => Some(three),
            Err(given) => {
                self.reporter.error(
                    self.since(start),
                    diagnostics::BAD_THREADS,
                    format!(
                        "`threads` takes three extents, but {} were given",
                        given.len()
                    ),
                );
                None
            }
        }
    }
}

/// The stage `word` names, when it names one.
fn stage_word(word: &str) -> Option<StageWord> {
    Some(match word {
        "vertex" => StageWord::Vertex,
        "fragment" => StageWord::Fragment,
        "compute" => StageWord::Compute,
        _ => return None,
    })
}
