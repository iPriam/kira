; The outline panel and symbol jump (cmd-shift-o).
;
; The attributes above a declaration are shown as context — `@Main` on a
; function, `@Required` on a construct field — because that is what tells you
; which declaration is the one you are looking for.

; ----- functions -----------------------------------------------------------

(function_definition
  (attribute)* @context
  "async"? @context
  "function" @context
  name: (identifier) @name) @item

; ----- structs and classes --------------------------------------------------

(struct_declaration
  (attribute)* @context
  "struct" @context
  name: (identifier) @name) @item

(class_declaration
  (attribute)* @context
  "class" @context
  name: (identifier) @name) @item

(field_declaration
  ["let" "var"] @context
  name: (identifier) @name) @item

(override_member
  "override" @context
  ["let" "var"] @context
  name: (identifier) @name) @item

; ----- enums -----------------------------------------------------------------

(enum_declaration
  (attribute)* @context
  "enum" @context
  name: (identifier) @name) @item

(enum_variant
  name: (identifier) @name) @item

; ----- traits and extend blocks ---------------------------------------------

(trait_declaration
  "trait" @context
  name: (identifier) @name) @item

(extend_declaration
  "extend" @context
  name: [(type_identifier) (qualified_type_identifier)] @name) @item

; ----- constructs and their members ------------------------------------------

(construct_declaration
  (attribute)* @context
  "construct" @context
  name: (identifier) @name) @item

(construct_field
  (attribute)* @context
  ["let" "var"] @context
  name: (identifier) @name) @item

(construct_computed_member
  (attribute)* @context
  ["let" "var"] @context
  name: (identifier) @name) @item

(construct_member_shorthand
  name: (identifier) @name) @item

(construct_initializer "init" @name) @item

(requires_section "requires" @name) @item

(lifecycle_section "lifecycle" @name) @item

(lifecycle_hook
  (attribute)* @context
  name: (identifier) @name) @item

; ----- type aliases, packages, and macros ------------------------------------

(type_alias_declaration
  "type" @context
  name: (identifier) @name) @item

(package_declaration
  "Package" @context
  name: (identifier) @name) @item

(macro_declaration
  "macro" @context
  name: (identifier) @name) @item

(comptime_macro_declaration
  "comptime" @context
  "macro" @context
  name: (identifier) @name) @item
