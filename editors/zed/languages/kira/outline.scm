; The outline panel and symbol jump (cmd-shift-o).
;
; This is the only source of symbol navigation for Kira: the language server
; implements no `documentSymbol`, so without this query the outline is empty.
; Functions are the only top-level construct v0 has, so this is the whole
; outline.
;
; The attribute is context rather than a separate item — `@Main` tells you
; which function is the entrypoint, and that is worth seeing in the list.
(function_definition
  (attribute)? @context
  "function" @context
  name: (identifier) @name) @item
