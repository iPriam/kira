; v0 has no `[` or `]`: there are no arrays and no indexing, so the grammar
; emits no such tokens. A pair for them here is not a harmless extra — an
; invalid node type fails the whole query, which fails the *entire language
; load*, taking syntax highlighting and the language server down with it.
("{" @open "}" @close)
("(" @open ")" @close)
(("\"" @open "\"" @close) (#set! rainbow.exclude))
