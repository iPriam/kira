/**
 * @file Tree-sitter grammar for Kira
 * @license MIT
 *
 * This grammar tracks the Kira compiler's surface:
 *   - kira-lexer/src/**              tokens, literals, comments, identifiers
 *   - kira-parser/src/item.rs        items, annotations, functions, imports
 *   - kira-parser/src/aggregate.rs   struct, class, enum
 *   - kira-parser/src/traits.rs      trait declarations, conformance lists
 *   - kira-parser/src/construct.rs   construct families and backed declarations
 *   - kira-parser/src/item/foreign.rs the `@FFI.*` annotation family
 *   - kira-parser/src/stmt.rs        statements, match, attempt/handle
 *   - kira-parser/src/expr.rs        expressions, closures, content blocks
 *   - kira-macros/src/decl.rs        macro, comptime macro, comptime function
 *
 * Contextual keywords — `borrow`, `mut`, `move`, `copy`, `some`, `Any`,
 * `async`, `extend`, `handle`, `init`, `requires`, `lifecycle`, `macro`,
 * `comptime`, `quote`, `Package`, `For` — are ordinary identifiers to the
 * lexer. They are written as literals here, which tree-sitter's keyword
 * extraction resolves per parse state, so a binding named `handle` still parses
 * everywhere the `attempt … handle` clause is not expected.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

/** Precedence ladder, loosest to tightest. Verbatim from kira-parser/src/expr.rs. */
const PREC = {
  conditional: 1,
  or: 2,
  and: 3,
  bitOr: 4,
  bitXor: 5,
  bitAnd: 6,
  equality: 7,
  comparison: 8,
  shift: 9,
  additive: 10,
  multiplicative: 11,
  unary: 12,
  postfix: 13,
};

/**
 * A comma-separated list of `rule` that permits a trailing comma, matching the
 * parser's loops for parameter lists, call arguments, and clause lists.
 *
 * @param {RuleOrLiteral} rule
 * @returns {ChoiceRule}
 */
function commaSepTrailing(rule) {
  return optional(seq(rule, repeat(seq(',', rule)), optional(',')));
}

/**
 * A comma-separated list of one or more `rule`, trailing comma permitted.
 *
 * @param {RuleOrLiteral} rule
 * @returns {SeqRule}
 */
function commaSep1Trailing(rule) {
  return seq(rule, repeat(seq(',', rule)), optional(','));
}

module.exports = grammar({
  name: 'kira',

  // The lexer skips ASCII whitespace and `//` comments and emits no newline
  // token, so the language is fully newline-insensitive.
  extras: ($) => [/\s/, $.comment],

  // Keywords are carved out of `identifier`, exactly as the lexer classifies a
  // lexed identifier through `keyword_from_text`.
  word: ($) => $.identifier,

  conflicts: ($) => [
    // A brace item may be a field initializer or an assignment statement.
    [$.assignment_statement, $.field_initializer],
    // A name after a construction's block may open a named child fill or start
    // the next statement.
    [$.construction_expression],
    [$._named_fills],
    [$._operand_construction],
    // A brace after a name may close a construction; a name alone is a read.
    [$._expression, $._construction_target],
    [$._expression, $._callable],
    // `{ a` opens a closure parameter list or an expression that is a content
    // item; the `in` is what settles it.
    [$.closure_parameters, $._value_name],
    [$._callable, $._ownership_operand],
    // `Any` alone is the top type; `Any Family` is the existential.
    [$.existential_type, $._type_name],
  ],

  rules: {
    source_file: ($) => repeat($._item),

    // ----- items ---------------------------------------------------------

    _item: ($) =>
      choice(
        $.import_declaration,
        $.function_definition,
        $.struct_declaration,
        $.class_declaration,
        $.enum_declaration,
        $.trait_declaration,
        $.construct_declaration,
        $.family_conformance_declaration,
        $.extend_declaration,
        $.type_alias_declaration,
        $.package_declaration,
        $.macro_declaration,
        $.comptime_macro_declaration,
        $.comptime_function_declaration,
        $.macro_invocation,
      ),

    // `import Module[.Sub…] [as Alias]`. A module name is a name, not a path.
    import_declaration: ($) =>
      seq(
        'import',
        field('module', $.module_path),
        optional(seq('as', field('alias', $.identifier))),
      ),

    module_path: ($) => seq($.identifier, repeat(seq('.', $.identifier))),

    // ----- annotations ---------------------------------------------------

    // `@Main`, `@Derive(Copy)`, `@Export { … }`, `@FFI.Extern { … }`. A
    // qualified name is the `@FFI.*` family; its block is `key: value;` fields.
    attribute: ($) =>
      seq(
        '@',
        field('name', choice($.identifier, $.qualified_attribute_name)),
        optional(choice($.attribute_arguments, $.attribute_block)),
      ),

    qualified_attribute_name: ($) =>
      seq($.identifier, repeat1(seq('.', $.identifier))),

    // The parser skips a balanced `(...)` without interpreting it, so the
    // contents are arbitrary balanced token soup. Strings are recognized as
    // themselves so a `(` inside one cannot unbalance the group.
    attribute_arguments: ($) =>
      seq(
        '(',
        repeat(
          choice($.attribute_arguments, $.string_literal, $._attribute_token),
        ),
        ')',
      ),

    _attribute_token: (_$) => token(prec(-1, /[^()"]+/)),

    // `{ library: ffimath; symbol: ffi_add; abi: c; retains: desc; }` — the
    // `@FFI.*` and `@Export` block. A `retains:` field repeats.
    attribute_block: ($) => seq('{', repeat($.attribute_field), '}'),

    // The value grammar follows the key, exactly as the parser dispatches it:
    // `target`/`element`/`result` name a type, `params` a bracketed type list,
    // and every other key a bare word, an integer, or a string.
    attribute_field: ($) =>
      seq(
        choice(
          seq(
            field(
              'key',
              alias(choice('target', 'element', 'result'), $.identifier),
            ),
            ':',
            field('value', $._type),
          ),
          seq(
            field('key', alias('params', $.identifier)),
            ':',
            field('value', $.attribute_type_list),
          ),
          seq(
            field('key', $.identifier),
            ':',
            field(
              'value',
              choice($.identifier, $.integer_literal, $.string_literal),
            ),
          ),
        ),
        optional(';'),
      ),

    attribute_type_list: ($) => seq('[', commaSepTrailing($._type), ']'),

    // ----- functions -----------------------------------------------------

    // A bodyless function is a trait requirement, an `@Required` construct
    // member, or an `@FFI.*` declaration — which ends with `;`. Every other
    // function carries a block.
    function_definition: ($) =>
      prec.right(
        seq(
          repeat($.attribute),
          optional('async'),
          'function',
          field('name', $.identifier),
          field('parameters', $.parameters),
          optional(field('return_type', $.return_type)),
          optional(choice(field('body', $.block), ';')),
        ),
      ),

    parameters: ($) =>
      seq(
        '(',
        optional(
          seq(
            optional(seq($.self_parameter, optional(','))),
            commaSepTrailing($.parameter),
          ),
        ),
        ')',
      ),

    // A method's receiver: `borrow self` or `borrow mut self`. A bare `self` is
    // KPAR075, so it parses here and is refused in the compiler.
    self_parameter: (_$) =>
      seq(optional(seq('borrow', optional('mut'))), 'self'),

    parameter: ($) =>
      seq(
        field('name', $.identifier),
        ':',
        optional(field('ownership', $.ownership_modifier)),
        field('type', $._type),
        optional(seq('=', field('default', $._expression))),
      ),

    ownership_modifier: (_$) =>
      choice(seq('borrow', optional('mut')), 'move', 'copy'),

    // Kira accepts both `-> Type` and `): Type`; a function type result takes
    // the `:` spelling, because `->` would be the function type's own arrow.
    return_type: ($) => seq(choice('->', ':'), $._type),

    // ----- types ---------------------------------------------------------

    _type: ($) =>
      choice(
        $.array_type,
        $.function_type,
        $.existential_type,
        $.generic_type,
        $._type_path,
      ),

    // `some Widget` / `Any Widget`: a value of some declaration backing the
    // family. Bare `Any` is the top type and stays an ordinary type name.
    existential_type: ($) =>
      seq(choice('some', 'Any'), field('family', $._type_path)),

    // `Result<Int, AppError>`. A plain type is its own path node, so a type
    // position that names one carries no wrapper.
    generic_type: ($) =>
      seq(field('name', $._type_path), field('arguments', $.type_arguments)),

    _type_path: ($) =>
      choice($._type_name, $.qualified_type_identifier),

    // The alias gives a type position its own node for highlighting. `Any` is
    // both the top type and the head of the family existential, so it reaches a
    // type name here as well as `existential_type` there.
    _type_name: ($) =>
      choice(
        alias($.identifier, $.type_identifier),
        alias('Any', $.type_identifier),
      ),

    qualified_type_identifier: ($) =>
      seq($._type_name, repeat1(seq('.', $._type_name))),

    type_arguments: ($) => seq('<', commaSep1Trailing($._type), '>'),

    array_type: ($) => seq('[', $._type, ']'),

    function_type: ($) =>
      seq(
        '(',
        commaSepTrailing(
          seq(optional(field('ownership', $.ownership_modifier)), $._type),
        ),
        ')',
        '->',
        field('result', $._type),
      ),

    // ----- aggregates ----------------------------------------------------

    // `struct Name[: Trait, …] { <member>* }`. `:` is always conformance.
    struct_declaration: ($) =>
      seq(
        repeat($.attribute),
        'struct',
        field('name', $.identifier),
        optional(field('conforms', $.conformance_list)),
        field('body', $.struct_body),
      ),

    struct_body: ($) => seq('{', repeat(choice($._aggregate_member, ';')), '}'),

    // `class Name[: Trait, …] [extends Parent, …] { <member>* }`. Traits first,
    // parents second.
    class_declaration: ($) =>
      seq(
        repeat($.attribute),
        'class',
        field('name', $.identifier),
        optional(field('conforms', $.conformance_list)),
        optional(field('extends', $.extends_list)),
        field('body', $.class_body),
      ),

    class_body: ($) =>
      seq(
        '{',
        repeat(choice($._aggregate_member, $.override_member, ';')),
        '}',
      ),

    conformance_list: ($) => seq(':', commaSep1Trailing($._type_path)),

    extends_list: ($) => seq('extends', commaSep1Trailing($._type_path)),

    _aggregate_member: ($) => choice($.field_declaration, $.function_definition),

    field_declaration: ($) =>
      seq(
        choice('let', 'var'),
        field('name', $.identifier),
        ':',
        field('type', $._type),
        optional(seq('=', field('default', $._expression))),
      ),

    // `override let rate = 5` rebinds an inherited default; `override function`
    // replaces an inherited method.
    override_member: ($) =>
      seq(
        'override',
        choice(
          $.function_definition,
          seq(
            choice('let', 'var'),
            field('name', $.identifier),
            optional(seq(':', field('type', $._type))),
            '=',
            field('default', $._expression),
          ),
        ),
      ),

    // `enum Name[<A, B>] { <variant>* }`. Variants are separated by nothing.
    // A parameter may carry trait bounds (`Value: Scored + Send`); the comma
    // separates parameters, so the traits of one parameter's bound join with
    // `+`.
    enum_declaration: ($) =>
      seq(
        repeat($.attribute),
        'enum',
        field('name', $.identifier),
        optional(field('type_parameters', $.type_parameters)),
        field('body', $.enum_body),
      ),

    type_parameters: ($) => seq('<', commaSep1Trailing($.type_parameter), '>'),

    type_parameter: ($) =>
      seq(
        field('name', $.identifier),
        optional(seq(':', field('bounds', $.bound_traits))),
      ),

    bound_traits: ($) =>
      seq(
        alias($.identifier, $.type_identifier),
        repeat(seq('+', alias($.identifier, $.type_identifier))),
      ),

    enum_body: ($) => seq('{', repeat(choice($.enum_variant, ';')), '}'),

    // `Empty`, `Text(String)`, and `InvalidFormat: String = "…"`.
    enum_variant: ($) =>
      seq(
        field('name', $.identifier),
        optional(
          choice(
            seq('(', field('payload', $._type), ')'),
            seq(
              ':',
              field('payload', $._type),
              optional(seq('=', field('default', $._expression))),
            ),
          ),
        ),
      ),

    // `type Name = Target`.
    type_alias_declaration: ($) =>
      seq('type', field('name', $.identifier), '=', field('target', $._type)),

    // ----- traits --------------------------------------------------------

    // `trait Name { … }`. A member with no body is a requirement; one with a
    // body is a default.
    trait_declaration: ($) =>
      seq(
        'trait',
        field('name', $.identifier),
        optional(field('conforms', $.conformance_list)),
        field('body', $.trait_body),
      ),

    trait_body: ($) => seq('{', repeat(choice($.function_definition, ';')), '}'),

    // `extend Family { … }` is the fluent modifier block; `extend T: Trait { … }`
    // is the impl block.
    extend_declaration: ($) =>
      seq(
        'extend',
        field('name', $._type_path),
        optional(field('conforms', $.conformance_list)),
        field('body', $.extend_body),
      ),

    extend_body: ($) =>
      seq('{', repeat(choice($.function_definition, ';')), '}'),

    // ----- constructs ----------------------------------------------------

    // A parameter list tells the two forms apart: `construct Name(params)
    // extends Family { … }` is a backed declaration, and `construct Family { … }`
    // is the family template.
    construct_declaration: ($) =>
      seq(
        repeat($.attribute),
        'construct',
        field('name', $.identifier),
        optional(field('parameters', $.parameters)),
        optional(field('conforms', $.conformance_list)),
        optional(field('extends', $.extends_list)),
        field('body', $.construct_body),
      ),

    // `Family Name { … }` — the bare head of a zero-parameter declaration
    // backed by `Family`: the family named first, the declaration second, no
    // parameter list and no clauses. Same body, same members.
    family_conformance_declaration: ($) =>
      seq(
        repeat($.attribute),
        field('family', $.identifier),
        field('name', $.identifier),
        field('body', $.construct_body),
      ),

    construct_body: ($) =>
      seq('{', repeat(choice($._construct_member, ';')), '}'),

    _construct_member: ($) =>
      choice(
        $.construct_field,
        $.construct_computed_member,
        $.function_definition,
        $.construct_initializer,
        $.requires_section,
        $.lifecycle_section,
        $.construct_member_shorthand,
      ),

    // `let n: Int = 0`, `@Required let title: String`, `@Content let body: Any`.
    construct_field: ($) =>
      seq(
        repeat($.attribute),
        choice('let', 'var'),
        field('name', $.identifier),
        optional(seq(':', field('type', $._type))),
        optional(seq('=', field('default', $._expression))),
      ),

    // `let node: Any { … }` — a zero-argument method read as a property.
    construct_computed_member: ($) =>
      seq(
        repeat($.attribute),
        choice('let', 'var'),
        field('name', $.identifier),
        ':',
        field('type', $._type),
        field('body', $.block),
      ),

    // `render { return content }` — the shorthand for a computed member whose
    // result type the backing family decides.
    construct_member_shorthand: ($) =>
      seq(field('name', $.identifier), field('body', $.block)),

    construct_initializer: ($) =>
      seq('init', field('parameters', $.parameters), field('body', $.block)),

    // `requires { function f(…) -> T … }` — the section spelling of
    // `@Required function`.
    requires_section: ($) =>
      seq('requires', '{', repeat(choice($.function_definition, ';')), '}'),

    // `lifecycle { onAppear() { … } }` — the points a runtime drives.
    lifecycle_section: ($) =>
      seq('lifecycle', '{', repeat(choice($.lifecycle_hook, ';')), '}'),

    lifecycle_hook: ($) =>
      seq(
        repeat($.attribute),
        field('name', $.identifier),
        field('parameters', $.parameters),
        optional(field('return_type', $.return_type)),
        field('body', $.block),
      ),

    // ----- packages ------------------------------------------------------

    // `Package Name { let version = "0.1.0" … }` in a `package.kira`.
    package_declaration: ($) =>
      seq('Package', field('name', $.identifier), field('body', $.block)),

    // ----- macros --------------------------------------------------------

    // `macro square(value: expr) { expand { … } }` — declarative.
    macro_declaration: ($) =>
      seq(
        'macro',
        field('name', $.identifier),
        field('parameters', $.macro_parameters),
        field('body', $.macro_body),
      ),

    macro_parameters: ($) => seq('(', commaSepTrailing($.macro_parameter), ')'),

    macro_parameter: ($) =>
      seq(field('name', $.identifier), ':', field('kind', $.identifier)),

    macro_body: ($) => seq('{', repeat($.expand_block), '}'),

    expand_block: ($) => seq('expand', field('body', $.block)),

    // `comptime macro Name { kind { derive } … expand(t: Declaration) -> Syntax { … } }`
    comptime_macro_declaration: ($) =>
      seq(
        'comptime',
        'macro',
        field('name', $.identifier),
        field('body', $.comptime_macro_body),
      ),

    comptime_macro_body: ($) =>
      seq('{', repeat(choice($.macro_section, $.expand_function, ';')), '}'),

    // `kind { derive }`, `appliesTo { struct, enum }`, `replace { true }`. The
    // separator is a comma or nothing at all.
    macro_section: ($) =>
      seq(
        field('name', $.identifier),
        '{',
        repeat(seq($._macro_section_value, optional(','))),
        '}',
      ),

    // `appliesTo` names declaration kinds, three of which are keywords.
    _macro_section_value: ($) =>
      choice(
        $.identifier,
        $.boolean_literal,
        alias(choice('struct', 'class', 'enum'), $.identifier),
      ),

    expand_function: ($) =>
      seq(
        'expand',
        field('parameters', $.parameters),
        optional(field('return_type', $.return_type)),
        field('body', $.block),
      ),

    // `comptime function name(p: Int) -> Int { … }` — invoked with no `!`.
    comptime_function_declaration: ($) =>
      seq('comptime', field('function', $.function_definition)),

    // `quote { … }` carries the syntax a procedural macro emits. A `#{ … }`
    // splice may glue mid-identifier (`mxp_#{name}`), so the body is balanced
    // token soup with its splices recognized rather than Kira source.
    quote_block: ($) => seq('quote', $.quote_body),

    quote_body: ($) =>
      seq(
        '{',
        repeat(
          choice(
            $.splice,
            $.quote_body,
            $.string_literal,
            $.comment,
            $._quote_token,
          ),
        ),
        '}',
      ),

    splice: ($) => seq('#{', $._expression, '}'),

    _quote_token: (_$) => token(prec(-1, /[^{}"#]+|#/)),

    // `Name!(args)` — every value-position macro carries the `!`.
    macro_invocation: ($) =>
      seq(
        field('name', $.identifier),
        token.immediate('!'),
        field('arguments', $.arguments),
      ),

    // ----- statements ----------------------------------------------------

    // A block eats arbitrary runs of semicolons, so `;` is never part of a
    // statement node and `{ ;;; }` is legal.
    //
    // The precedence is what makes a `{` where a body is expected open a block
    // rather than the content of a construction the condition would then have
    // been — the rule an `if`/`while`/`for`/`match` subject follows.
    block: ($) =>
      prec.dynamic(
        1,
        prec(1, seq('{', repeat(choice($._statement, ';')), '}')),
      ),

    _statement: ($) =>
      choice(
        $.variable_declaration,
        $.assignment_statement,
        $.return_statement,
        $.if_statement,
        $.while_statement,
        $.for_statement,
        $.break_statement,
        $.continue_statement,
        $.match_statement,
        $.attempt_statement,
        $.expression_statement,
      ),

    variable_declaration: ($) =>
      seq(
        choice('let', 'var'),
        field('name', $.identifier),
        optional(
          seq(
            ':',
            optional(field('ownership', $.ownership_modifier)),
            field('type', $._type),
          ),
        ),
        '=',
        field('value', $._expression),
      ),

    // The target is written with expression syntax (`p`, `p.x`, `xs[i]`);
    // whether it names a place is a question for semantics.
    assignment_statement: ($) =>
      seq(
        field('left', $._assignment_target),
        '=',
        field('right', $._expression),
      ),

    _assignment_target: ($) =>
      choice($._value_name, $.field_expression, $.index_expression),

    // The value is taken greedily, so `return` NEWLINE `42` returns 42.
    return_statement: ($) =>
      prec.right(seq('return', optional(field('value', $._expression)))),

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

    // `for i in 0..5` walks a half-open range; `for x in xs` walks an array.
    for_statement: ($) =>
      seq(
        'for',
        field('name', $.identifier),
        'in',
        field('iterable', choice($.range_expression, $._expression)),
        field('body', $.block),
      ),

    range_expression: ($) =>
      seq(field('start', $._expression), '..', field('end', $._expression)),

    break_statement: (_$) => 'break',

    continue_statement: (_$) => 'continue',

    // `match subject { Variant[(binding)] -> arm … }`. The precedence keeps the
    // brace the body's, exactly as it does for a block.
    match_statement: ($) =>
      prec.dynamic(
        1,
        prec(
          1,
          seq(
            'match',
            field('subject', $._expression),
            '{',
            repeat($.match_arm),
            '}',
          ),
        ),
      ),

    match_arm: ($) =>
      seq(
        field('pattern', $.variant_pattern),
        '->',
        field('body', choice($.block, seq($._statement, optional(';')))),
      ),

    variant_pattern: ($) =>
      seq(
        field('variant', $.identifier),
        optional(seq('(', field('binding', $.identifier), ')')),
      ),

    // `attempt { … } handle { Variant[(binding)] { … } … }` — a handler arm
    // takes no arrow, and its body is always a block.
    attempt_statement: ($) =>
      seq(
        'attempt',
        field('body', $.block),
        'handle',
        '{',
        repeat($.handler_arm),
        '}',
      ),

    handler_arm: ($) =>
      seq(field('pattern', $.variant_pattern), field('body', $.block)),

    expression_statement: ($) => prec(-1, $._expression),

    // ----- expressions ---------------------------------------------------

    _expression: ($) =>
      choice(
        $.conditional_expression,
        $.binary_expression,
        $.unary_expression,
        $.ownership_expression,
        $.try_expression,
        $.call_expression,
        $.method_call_expression,
        $.construction_expression,
        $.trailing_closure_expression,
        $.macro_invocation,
        $.quote_block,
        $.field_expression,
        $.index_expression,
        $.closure_expression,
        $.parenthesized_expression,
        $.array_literal,
        $.dot_member_expression,
        $._value_name,
        $.integer_literal,
        $.float_literal,
        $.string_literal,
        $.boolean_literal,
      ),

    // `move` and `copy` are contextual: each is an operator only when an
    // operand follows, and an ordinary name everywhere else.
    _value_name: ($) =>
      choice(
        $.identifier,
        alias('move', $.identifier),
        alias('copy', $.identifier),
      ),

    // `c ? a : b` — the one expression that is control flow, and the one
    // right-associative form.
    conditional_expression: ($) =>
      prec.right(
        PREC.conditional,
        seq(
          field('condition', $._expression),
          '?',
          field('consequence', $._expression),
          ':',
          field('alternative', $._expression),
        ),
      ),

    // The ladder is C's, rung for rung: the bitwise operators bind looser than
    // equality, and the shifts tighter than the orderings but looser than `+`.
    binary_expression: ($) => {
      /** @type {[number, RuleOrLiteral][]} */
      const table = [
        [PREC.or, '||'],
        [PREC.and, '&&'],
        [PREC.bitOr, '|'],
        [PREC.bitXor, '^'],
        [PREC.bitAnd, '&'],
        [PREC.equality, choice('==', '!=')],
        [PREC.comparison, choice('<', '<=', '>', '>=')],
        [PREC.shift, choice('<<', '>>')],
        [PREC.additive, choice('+', '-')],
        [PREC.multiplicative, choice('*', '/', '%')],
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

    unary_expression: ($) =>
      prec.right(
        PREC.unary,
        seq(
          field('operator', choice('-', '!', '~')),
          field('operand', $._expression),
        ),
      ),

    // `move x` / `copy x` — an operator only when what follows starts an
    // operand: a name, a literal, an array literal, or a prefix operator. A
    // `move` before anything else is a read of a binding named `move`.
    ownership_expression: ($) =>
      prec.right(
        PREC.unary,
        seq(
          field('operator', choice('move', 'copy')),
          field('operand', $._ownership_operand),
        ),
      ),

    _ownership_operand: ($) =>
      choice(
        $.unary_expression,
        $.call_expression,
        alias($._operand_construction, $.construction_expression),
        $.macro_invocation,
        $.array_literal,
        $._value_name,
        $.integer_literal,
        $.float_literal,
        $.string_literal,
        $.boolean_literal,
      ),

    // `try f(n)` binds like a prefix operator, so it takes the whole call.
    try_expression: ($) =>
      prec.right(PREC.unary, seq('try', field('value', $._expression))),

    // The callee is a name or a field path, never an arbitrary expression:
    // `Expr::Call` carries a Symbol, so `(f)(x)` is not a call.
    call_expression: ($) =>
      prec(
        PREC.postfix,
        seq(field('function', $._callable), field('arguments', $.arguments)),
      ),

    // The callee is a bare name; `a.b(x)` is a `method_call_expression`.
    _callable: ($) => $._value_name,

    // One rung above the rest of the postfix chain, which is what settles
    // `p.sum()` as a method call rather than a field read that is then called.
    method_call_expression: ($) =>
      prec(
        PREC.postfix + 1,
        seq(
          field('receiver', $._expression),
          '.',
          field('method', $.identifier),
          field('arguments', $.arguments),
        ),
      ),

    arguments: ($) =>
      seq(
        '(',
        commaSepTrailing(choice($.labeled_argument, $._expression)),
        ')',
      ),

    // A leading identifier is a label only when a binder follows it, so `f(x)`
    // keeps `x` as an ordinary expression. `=` is canonical and `:` stays
    // valid.
    labeled_argument: ($) =>
      seq(
        field('label', $.identifier),
        choice(':', '='),
        field('value', $._expression),
      ),

    // `xs.count` is a property read and takes the field path.
    field_expression: ($) =>
      prec(
        PREC.postfix,
        seq(field('receiver', $._expression), '.', field('field', $.identifier)),
      ),

    index_expression: ($) =>
      prec(
        PREC.postfix,
        seq(
          field('receiver', $._expression),
          '[',
          field('index', $._expression),
          ']',
        ),
      ),

    // `.Red`, `.Ok(12)` — a member of the type expected at this position. The
    // negative precedence settles `copy.field`: a `.` after a contextual
    // `move`/`copy` continues the name it follows rather than opening the
    // operand of an ownership operator, which is the compiler's rule that an
    // operator needs an operand-starting token after it.
    dot_member_expression: ($) =>
      prec.right(
        -1,
        seq(
          '.',
          field('name', $.identifier),
          optional(field('arguments', $.arguments)),
        ),
      ),

    // A construction's trailing brace: children, `let` overrides, named fills,
    // a struct literal's fields, or a zero-parameter closure body. The reading
    // depends on the callee's signature, which analysis holds.
    construction_expression: ($) =>
      prec(
        PREC.postfix,
        seq(
          field('constructor', $._construction_target),
          field('body', $.content_block),
          optional($._named_fills),
        ),
      ),

    _named_fills: ($) =>
      seq(field('fill', $.named_fill), optional($._named_fills)),

    // `move FfiRange { ptr: p }` — the same construction, reached from the
    // narrower set of forms that may start an operand.
    _operand_construction: ($) =>
      prec(
        PREC.postfix,
        seq(
          field('constructor', choice($._value_name, $.call_expression)),
          field('body', $.content_block),
          optional($._named_fills),
        ),
      ),

    // `each { value in … }` — a trailing closure is the call's last argument,
    // and a bare name or field access is promoted to a call to take one.
    trailing_closure_expression: ($) =>
      prec(
        PREC.postfix,
        seq(
          field('function', $._construction_target),
          field('closure', $.closure_expression),
        ),
      ),

    // A construction is a name, a construction call, or a modifier call: a
    // literal or an operator result takes no trailing block.
    _construction_target: ($) =>
      choice(
        $._value_name,
        $.field_expression,
        $.call_expression,
        $.method_call_expression,
      ),

    // `NavigationSplitView { … } detail: { … }` — a fill binds to the
    // construction it follows.
    named_fill: ($) =>
      seq(
        field('name', $.identifier),
        ':',
        field('value', choice($.content_block, $._expression)),
      ),

    content_block: ($) =>
      seq('{', repeat(choice($._brace_item, ',', ';')), '}'),

    _brace_item: ($) => choice($.field_initializer, $.content_for, $._statement),

    // `x = 1` / `x: 1` inside a construction: a struct literal's field, or a
    // construction override. The `let field = value` spelling of an override is
    // a `variable_declaration`, which it is written exactly like.
    field_initializer: ($) =>
      prec.dynamic(
        1,
        seq(
          field('name', $._assignment_target),
          choice('=', ':'),
          field('value', choice($.content_block, $._expression)),
        ),
      ),

    // `For(x in xs) { … }` — the content-block builder, recognized only here.
    content_for: ($) =>
      seq(
        'For',
        '(',
        field('name', $.identifier),
        'in',
        field('iterable', $._expression),
        ')',
        field('body', $.content_block),
      ),

    // `{ params in body }`; `{ in … }` takes no parameters.
    closure_expression: ($) =>
      seq(
        '{',
        optional(field('parameters', $.closure_parameters)),
        'in',
        repeat(choice($._statement, ';')),
        '}',
      ),

    closure_parameters: ($) => commaSep1Trailing($.identifier),

    array_literal: ($) =>
      seq('[', repeat(seq($._expression, optional(','))), ']'),

    parenthesized_expression: ($) => seq('(', $._expression, ')'),

    // ----- tokens --------------------------------------------------------

    // ASCII only; a bare `_` is a legal identifier. A non-ASCII byte is
    // TokenKind::Unknown + KLEX001.
    identifier: (_$) => /[A-Za-z_][A-Za-z0-9_]*/,

    boolean_literal: (_$) => choice('true', 'false'),

    // Decimal or hexadecimal: a hex literal is a bit pattern read as 64
    // unsigned bits. No `_` separators, no exponent, no sign.
    integer_literal: (_$) => token(choice(/0[xX][0-9a-fA-F]+/, /[0-9]+/)),

    // Digits DOT digits, both sides required: `1.` is an integer plus a stray
    // dot, and `1e5` is an integer plus the identifier `e5`.
    float_literal: (_$) => token(seq(/[0-9]+/, '.', /[0-9]+/)),

    // An unescaped newline ends the literal (unterminated, KLEX002); an
    // escaped newline continues it onto the next line.
    string_literal: ($) =>
      seq(
        '"',
        repeat(choice($._string_content, $.escape_sequence)),
        token.immediate('"'),
      ),

    _string_content: (_$) => token.immediate(prec(1, /[^"\\\n]+/)),

    // Backslash + ANY single character: only `\n \t \r \0 \" \\` decode
    // specially, and every other escape decodes to the character itself.
    escape_sequence: (_$) => token.immediate(/\\[\s\S]/),

    // Line comments only. `/* x */` lexes as Slash, Star, … and must ERROR.
    comment: (_$) => token(seq('//', /[^\n]*/)),
  },
});
