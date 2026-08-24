; Kira v0 highlighting.
;
; Every capture below binds to a node the grammar actually produces. The
; language is small on purpose — four scalar types, no structs, no imports — so
; anything absent here is absent from the language, not forgotten.
;
; Order matters: the first pattern to match a node wins, so specific captures
; come before the catch-all `(identifier) @variable` at the bottom.

; Keywords — the whole set. There is no `for`, no `match`, no `import`: those
; lex as keywords but the compiler answers KSEM900/KSEM901, and the grammar
; leaves them as errors rather than colouring a promise the language does not
; keep.
[
  "function"
  "let"
  "var"
  "return"
  "if"
  "else"
  "while"
] @keyword

; Attributes. `@Main`, `@Runtime`, and `@Native` are the three the compiler
; acts on; any other name parses and is ignored, so it is coloured as the
; ordinary attribute it is rather than flagged here.
(attribute
  "@" @punctuation.special
  name: (identifier) @attribute)

; Types. The grammar accepts any identifier in type position — `let x: Foo` is
; a *semantic* error (KSEM050), not a parse error — so the five real types are
; distinguished here rather than in the grammar.
(type_identifier) @type

((type_identifier) @type.builtin
  (#match? @type.builtin "^(Int|Float|Bool|String|Void)$"))

; Functions
(function_definition
  name: (identifier) @function.definition)

(call_expression
  function: (identifier) @function)

; `print` is the only builtin, and it is an ordinary identifier to the parser.
((call_expression
  function: (identifier) @function.builtin)
  (#eq? @function.builtin "print"))

(parameter
  name: (identifier) @variable.parameter)

; Literals
(integer_literal) @number

(float_literal) @number

(string_literal) @string

(escape_sequence) @string.escape

(boolean_literal) @boolean

(comment) @comment

; Operators
[
  "="
  "->"
  "+"
  "-"
  "*"
  "/"
  "%"
  "=="
  "!="
  "<"
  "<="
  ">"
  ">="
  "&&"
  "||"
  "!"
] @operator

; Punctuation
[
  "("
  ")"
  "{"
  "}"
] @punctuation.bracket

[
  ","
  ";"
  ":"
] @punctuation.delimiter

; The catch-all, last: anything not matched above is a plain name.
(identifier) @variable
