/**
 * @file Tree-sitter grammar for Kira (v0 subset)
 * @license MIT
 *
 * This grammar tracks the kira-rusty compiler's v0 surface exactly:
 *   - kira-lexer/src/**            tokens, literals, comments, identifiers
 *   - kira-parser/src/lib.rs       items, functions, attributes, types, blocks
 *   - kira-parser/src/stmt.rs      statements
 *   - kira-parser/src/expr.rs      expressions and the precedence ladder
 *
 * Deliberate omissions. `struct enum class import match for in switch case
 * default break continue` are lexed as keywords but have no parse rules in v0:
 * the compiler turns them into Unsupported/Error nodes (KSEM900/KSEM901). They
 * get no rules here either, so they produce ERROR nodes and the editor agrees
 * with the compiler instead of colouring a construct that will not compile.
 * For the same reason `.` `[` `]` `?` are lexed by the compiler but are
 * unreachable from the expression parser, so there is no member access, no
 * indexing, and no optional chaining here.
 *
 * Read those omissions as "not yet", not "never". Kira has structs, arrays,
 * imports and the rest today — kira-zig implements them, and it is the
 * behaviour oracle kira-rusty is catching up to. This grammar tracks what
 * *kira-rusty* parses, so each construct comes back here in the same change
 * that teaches kira-parser to accept it. Do not restore a rule from an older
 * grammar to get ahead of the compiler: a construct that highlights but does
 * not compile is a promise the toolchain does not keep, which is the whole
 * failure this file exists to avoid.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

/**
 * A comma-separated list of `rule` that permits a trailing comma, matching the
 * compiler's parse loops for both parameter lists and call arguments: the
 * `!self.at(RParen)` re-test after `eat(Comma)` lets `,)` fall through to
 * `expect(RParen)`, while a missing comma between elements is KPAR001.
 *
 * @param {RuleOrLiteral} rule
 * @returns {ChoiceRule}
 */
function commaSepTrailing(rule) {
  return optional(seq(rule, repeat(seq(',', rule)), optional(',')));
}

module.exports = grammar({
  name: 'kira',

  // The lexer skips ASCII whitespace and `//` comments and emits no newline
  // token, so the language is fully newline-insensitive.
  // kira-lexer/src/lib.rs:76-91
  extras: ($) => [/\s/, $.comment],

  // Keywords are carved out of `identifier`, exactly as the compiler's lexer
  // classifies a lexed identifier through `keyword_from_text`.
  // kira-lexer/src/lib.rs:113-124
  word: ($) => $.identifier,

  // `keyword_from_text` is unconditional: the compiler's lexer never lets one
  // of these 21 words fall back to being an identifier, whatever the parser
  // wanted there. The global reserved set reproduces that, which is what makes
  // the reserved-but-unimplemented words genuinely fail.
  //
  // Without it tree-sitter would happily lex a bare `break` as an identifier
  // and parse it as an expression statement -- no ERROR, while the compiler
  // emits KSEM901. That is the false promise this grammar exists to avoid, so
  // the twelve unimplemented words are reserved here and given no rule.
  // kira-syntax-model/src/token.rs:150-177
  reserved: {
    global: ($) => [
      // The nine v0 keywords, reserved so they can never be misread as an
      // identifier in a state that happens to want one.
      'function',
      'let',
      'var',
      'return',
      'if',
      'else',
      'while',
      'true',
      'false',
      // The twelve reserved-but-unimplemented words, as one token.
      $.reserved_keyword,
    ],
  },

  rules: {
    // Only functions exist at the top level. A bare identifier, `struct`,
    // `enum`, `class`, or `import` here is an Unsupported item (KSEM900), and
    // an attribute not followed by `function` is one too.
    // kira-parser/src/lib.rs:68-82, :176-200, :243-250
    source_file: ($) => repeat($.function_definition),

    // ----- items ---------------------------------------------------------

    // Attributes repeat freely, including `@Main @Native` and even the
    // contradictory `@Runtime @Native` (KPAR005 is a diagnostic; the tree
    // stands). kira-parser/src/lib.rs:202-251
    function_definition: ($) =>
      seq(
        repeat($.attribute),
        'function',
        field('name', $.identifier),
        field('parameters', $.parameters),
        optional(field('return_type', $.return_type)),
        field('body', $.block),
      ),

    // The name is any identifier: Main/Runtime/Native are recognised and
    // unknown names are consumed silently, so that distinction belongs in
    // highlights.scm, not here. `@import` errors: the name must be an
    // Identifier token, not a keyword (KPAR003).
    // kira-parser/src/lib.rs:202-242
    attribute: ($) =>
      seq('@', field('name', $.identifier), optional($.attribute_arguments)),

    // The parser skips a balanced `(...)` without interpreting it, so the
    // contents are arbitrary balanced token soup.
    // kira-parser/src/lib.rs:231-233, skip_balanced :414-432
    //
    // `skip_balanced` counts LParen/RParen **tokens**, not bytes, and the
    // lexer has already run — so a string literal is one token and the parens
    // inside it are invisible to the depth counter. `@Deprecated("use bar(
    // instead")` is valid v0 for exactly that reason. Matching bytes here
    // instead would see that `(` as real, never find its partner, and ERROR
    // across the rest of the file — turning a valid program into an
    // unhighlighted mess.
    //
    // So strings are recognised as themselves, and the catch-all below cannot
    // match a quote. That also keeps an *unterminated* string inside arguments
    // an error, as the lexer says it is (KLEX002), rather than swallowing it
    // as byte soup and rendering a clean function the compiler rejects.
    attribute_arguments: ($) =>
      seq(
        '(',
        repeat(
          choice($.attribute_arguments, $.string_literal, $._attribute_token),
        ),
        ')',
      ),

    _attribute_token: (_$) => token(prec(-1, /[^()"]+/)),

    // The comma between parameters is required; a trailing comma is legal.
    // kira-parser/src/lib.rs:282-301
    parameters: ($) => seq('(', commaSepTrailing($.parameter), ')'),

    // The type is mandatory. kira-parser/src/lib.rs:303-320
    parameter: ($) =>
      seq(field('name', $.identifier), ':', field('type', $._type)),

    // Kira accepts both `-> Type` and `): Type`, from a single
    // `eat(Arrow) || eat(Colon)`, so exactly one of the two may appear.
    // kira-parser/src/lib.rs:322-329
    return_type: ($) => seq(choice('->', ':'), $._type),

    // Any identifier. `Int Float Bool String Void` live in
    // kira-semantics-model/src/ty.rs:26-35, so an unknown type name is the
    // semantic error KSEM050, not a parse error. The alias gives type
    // positions their own node for highlighting.
    // kira-parser/src/lib.rs:331-345
    _type: ($) => alias($.identifier, $.type_identifier),

    // ----- statements ----------------------------------------------------

    // A block eats arbitrary runs of semicolons before and after every
    // statement, so `;` is never part of a statement node and `{ ;;; }` is
    // legal. kira-parser/src/lib.rs:347-370
    block: ($) => seq('{', repeat(choice($._statement, ';')), '}'),

    // The complete statement dispatch: no nested functions, no bare blocks, no
    // for/switch/match/break/continue. kira-parser/src/stmt.rs:17-33
    _statement: ($) =>
      choice(
        $.variable_declaration,
        $.assignment_statement,
        $.return_statement,
        $.if_statement,
        $.while_statement,
        $.expression_statement,
      ),

    // The `: Type` annotation is optional; the `= initializer` is required
    // (KPAR011). kira-parser/src/stmt.rs:35-72
    variable_declaration: ($) =>
      seq(
        choice('let', 'var'),
        field('name', $.identifier),
        optional(seq(':', field('type', $._type))),
        '=',
        field('value', $._expression),
      ),

    // The target is a bare identifier, and assignment is a statement, never an
    // expression: `Stmt::Assign` carries a Symbol, not an ExprId.
    // kira-parser/src/stmt.rs:28-30, :74-87
    assignment_statement: ($) =>
      seq(field('left', $.identifier), '=', field('right', $._expression)),

    // A bare `return` is legal, and the value is taken greedily across
    // newlines: `return` NEWLINE `42` returns 42, so `prec.right` is required.
    // kira-parser/src/stmt.rs:89-99, :174-187
    return_statement: ($) =>
      prec.right(seq('return', optional(field('value', $._expression)))),

    // `else if` is an else holding a nested if, and the nearest-if binding
    // falls out of the recursion. No parens are required around the condition,
    // and no expression form starts with `{`, so the condition never swallows
    // the block. kira-parser/src/stmt.rs:101-128
    if_statement: ($) =>
      prec.right(
        seq(
          'if',
          field('condition', $._expression),
          field('consequence', $.block),
          optional(
            seq('else', field('alternative', choice($.if_statement, $.block))),
          ),
        ),
      ),

    while_statement: ($) =>
      seq('while', field('condition', $._expression), field('body', $.block)),

    // The negative precedence resolves the statement-boundary conflict toward
    // continuing the expression, which is what the compiler does: `let a = 5`
    // NEWLINE `-2` is one statement, not two.
    // kira-parser/src/stmt.rs:31, :139-144; test at lib.rs:614-633
    expression_statement: ($) => prec(-1, $._expression),

    // ----- expressions ---------------------------------------------------

    // The complete expression list: no member access, no index, no method
    // call, no lambda, no struct literal, no array literal.
    // kira-parser/src/expr.rs:58-97
    _expression: ($) =>
      choice(
        $.binary_expression,
        $.unary_expression,
        $.call_expression,
        $.parenthesized_expression,
        $.identifier,
        $.integer_literal,
        $.float_literal,
        $.string_literal,
        $.boolean_literal,
      ),

    // Binding powers verbatim from kira-parser/src/expr.rs:170-188. Every
    // binary operator is left-associative, comparisons included, so `a < b < c`
    // parses as `(a < b) < c` (expr.rs:25-39).
    binary_expression: ($) => {
      /** @type {[number, RuleOrLiteral][]} */
      const table = [
        [1, '||'],
        [2, '&&'],
        [3, choice('==', '!=', '<', '<=', '>', '>=')],
        [4, choice('+', '-')],
        [5, choice('*', '/', '%')],
      ];

      return choice(
        ...table.map(([precedence, operator]) =>
          prec.left(
            precedence,
            seq(
              field('left', $._expression),
              field('operator', operator),
              field('right', $._expression),
            ),
          ),
        ),
      );
    },

    // Prefix `-`/`!` bind tighter than `*` and stack freely (`- -x`, `!!x`).
    // kira-parser/src/expr.rs:41-56
    unary_expression: ($) =>
      prec.right(
        6,
        seq(field('operator', choice('-', '!')), field('operand', $._expression)),
      ),

    // The callee is a bare identifier, never an arbitrary expression:
    // `Expr::Call` carries a Symbol, so `(f)(x)` is not a call. `print` is an
    // ordinary identifier reaching this rule like any other name.
    //
    // The precedence sits above the unary ladder to settle `name` vs `name(`:
    // the compiler commits to a call the moment a `(` follows a name, without
    // consulting newlines (`parse_name_or_call`'s `if self.at(LParen)`), so the
    // shift into `arguments` must beat reducing the name to a bare expression.
    // kira-parser/src/expr.rs:123-141
    call_expression: ($) =>
      prec(
        7,
        seq(field('function', $.identifier), field('arguments', $.arguments)),
      ),

    // Trailing comma legal, empty `()` legal. kira-parser/src/expr.rs:143-158
    arguments: ($) => seq('(', commaSepTrailing($._expression), ')'),

    // Exactly one expression: no tuples, no unit `()`, and no postfix call on
    // the result. kira-parser/src/expr.rs:160-165
    parenthesized_expression: ($) => seq('(', $._expression, ')'),

    // ----- tokens --------------------------------------------------------

    // Words the compiler's lexer turns into keywords but its parser has no
    // rule for: they become Unsupported/Error nodes with KSEM900 at item level
    // (kira-parser/src/lib.rs:374-410) and KSEM901 at statement level
    // (kira-parser/src/stmt.rs:148-170).
    //
    // This rule deliberately has no users. It exists only so these words can
    // enter the global reserved set, which stops them from being lexed as
    // identifiers -- otherwise a bare `break` would parse as a clean
    // expression statement and the editor would show no error where the
    // compiler refuses to build. `token(choice(...))` makes it the single
    // terminal the reserved set requires.
    //
    // MUST stay above `identifier`: tree-sitter breaks an equal-length lexical
    // match by declaration order, so with `identifier` first this token is
    // never produced, keyword extraction silently drops it, and the reserved
    // set goes inert with no error from `generate`.
    // kira-syntax-model/src/token.rs:164-175
    reserved_keyword: (_$) =>
      token(
        choice(
          'import',
          'struct',
          'enum',
          'class',
          'match',
          'for',
          'in',
          'switch',
          'case',
          'default',
          'break',
          'continue',
        ),
      ),

    // ASCII only; a bare `_` is a legal identifier. A non-ASCII byte is
    // TokenKind::Unknown + KLEX001, so `café` must ERROR: do not reach for
    // tree-sitter's usual Unicode-friendly identifier regex.
    // kira-lexer/src/lib.rs:102, :114-126, :229-237
    identifier: (_$) => /[A-Za-z_][A-Za-z0-9_]*/,

    // Dedicated True/False tokens, not identifiers.
    // kira-syntax-model/src/token.rs:163-164
    boolean_literal: (_$) => choice('true', 'false'),

    // Decimal only: no hex/octal/binary, no `_` separators, no exponent, no
    // sign (a leading `-` is a unary_expression). kira-lexer/src/lib.rs:128-148
    integer_literal: (_$) => /[0-9]+/,

    // Digits DOT digits, both sides required: a `.` is a decimal point only
    // when followed by a digit. `1.` is IntLiteral + a stray Dot, `.5` is a
    // stray Dot + IntLiteral, and `1e5` is IntLiteral + the identifier `e5`.
    // kira-lexer/src/lib.rs:133-147
    float_literal: (_$) => token(seq(/[0-9]+/, '.', /[0-9]+/)),

    // An unescaped newline ends the literal (unterminated, KLEX002); an
    // escaped newline continues it onto the next line.
    // kira-lexer/src/lib.rs:151-178
    string_literal: ($) =>
      seq(
        '"',
        repeat(choice($._string_content, $.escape_sequence)),
        token.immediate('"'),
      ),

    // Newline excluded: only an escape may carry the literal across one. Every
    // child after the opening quote is `token.immediate`, so the `/\s/` extra
    // cannot silently skip a newline back in.
    _string_content: (_$) => token.immediate(prec(1, /[^"\\\n]+/)),

    // Backslash + ANY single character, newline included. Only `\n \t \r \0 \"
    // \\` decode specially; every other escape decodes to the character itself
    // (`\q` is `q`), so the set is deliberately open.
    // kira-lexer/src/lib.rs:162-167, decode_string_literal :246-270
    escape_sequence: (_$) => token.immediate(/\\[\s\S]/),

    // Line comments only. `/* x */` lexes as Slash, Star, identifier, Star,
    // Slash and must ERROR. kira-lexer/src/lib.rs:80-86
    comment: (_$) => token(seq('//', /[^\n]*/)),
  },
});
