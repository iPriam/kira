; Kira syntax highlighting, tracking the current grammar (grammar.js).
;
; Order matters: the first pattern to match a node wins, so specific captures
; come before the catch-all `(identifier) @variable` at the bottom.

; ----- keywords -------------------------------------------------------------

[
  "import"
  "as"
  "function"
  "async"
  "struct"
  "class"
  "extends"
  "let"
  "var"
  "override"
  "enum"
  "type"
  "trait"
  "extend"
  "construct"
  "init"
  "requires"
  "lifecycle"
  "Package"
  "macro"
  "expand"
  "comptime"
  "quote"
  "return"
  "if"
  "else"
  "while"
  "for"
  "For"
  "in"
  "match"
  "attempt"
  "handle"
  "try"
  "some"
  "Any"
  "borrow"
  "mut"
  "move"
  "copy"
] @keyword

; `break` and `continue` collapse to a single-token node each — there is no
; anonymous "break"/"continue" text left to match as a literal.
(break_statement) @keyword
(continue_statement) @keyword

; `self`, however it is spelled: the receiver's anonymous token in
; `self_parameter`, and an ordinary identifier read everywhere else.
("self") @variable.builtin

((identifier) @variable.builtin
  (#eq? @variable.builtin "self"))

; ----- annotations -----------------------------------------------------------

; `@Main`, `@Derive(Copy)`, `@FFI.Extern { … }`.
(attribute
  "@" @punctuation.special
  name: (identifier) @attribute)

(attribute
  "@" @punctuation.special
  name: (qualified_attribute_name) @attribute)

(attribute_field
  key: (identifier) @property)

; ----- types -------------------------------------------------------------

(type_identifier) @type

((type_identifier) @type.builtin
  (#match? @type.builtin "^(Int|Float|Bool|String|Void|Any|RawPtr|CString|I8|I16|I32|Int32|U8|U16|U32|U64|F32)$"))

(struct_declaration name: (identifier) @type)
(class_declaration name: (identifier) @type)
(enum_declaration name: (identifier) @type)
(trait_declaration name: (identifier) @type)
(construct_declaration name: (identifier) @type)
(type_alias_declaration name: (identifier) @type)

(package_declaration name: (identifier) @namespace)
(import_declaration module: (module_path (identifier) @namespace))
(import_declaration alias: (identifier) @namespace)

; ----- functions and macros -----------------------------------------------

(function_definition
  name: (identifier) @function.definition)

(call_expression
  function: (identifier) @function)

(method_call_expression
  method: (identifier) @function.method)

(macro_declaration name: (identifier) @function.macro)
(comptime_macro_declaration name: (identifier) @function.macro)

(macro_invocation
  name: (identifier) @function.macro)

(parameter
  name: (identifier) @variable.parameter)

(self_parameter) @variable.builtin

(labeled_argument
  label: (identifier) @property)

; ----- fields, variants, and constructions --------------------------------

(field_declaration name: (identifier) @property)
(construct_field name: (identifier) @property)
(construct_computed_member name: (identifier) @property)
(construct_member_shorthand name: (identifier) @property)
(field_expression field: (identifier) @property)
(named_fill name: (identifier) @property)

(enum_variant name: (identifier) @constructor)
(variant_pattern variant: (identifier) @constructor)
(dot_member_expression name: (identifier) @constructor)

; ----- literals ------------------------------------------------------------

(integer_literal) @number

(float_literal) @number

(string_literal) @string

(escape_sequence) @string.escape

(boolean_literal) @boolean

(comment) @comment

; ----- operators and punctuation -------------------------------------------

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
  "&"
  "|"
  "^"
  "~"
  "<<"
  ">>"
  "?"
  ".."
] @operator

[
  "("
  ")"
  "{"
  "}"
  "["
  "]"
  "#{"
] @punctuation.bracket

[
  ","
  ";"
  ":"
  "."
] @punctuation.delimiter

; The catch-all, last: anything not matched above is a plain name.
(identifier) @variable
