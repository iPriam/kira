; Every brace-, paren-, and bracket-delimited body indents its contents one
; level and dedents on the token that closes it.

(block "}" @end) @indent
(struct_body "}" @end) @indent
(class_body "}" @end) @indent
(enum_body "}" @end) @indent
(trait_body "}" @end) @indent
(extend_body "}" @end) @indent
(construct_body "}" @end) @indent
(content_block "}" @end) @indent
(quote_body "}" @end) @indent
(requires_section "}" @end) @indent
(lifecycle_section "}" @end) @indent
(macro_body "}" @end) @indent
(comptime_macro_body "}" @end) @indent
(macro_section "}" @end) @indent
(attribute_block "}" @end) @indent
(match_statement "}" @end) @indent

(parameters ")" @end) @indent
(macro_parameters ")" @end) @indent
(arguments ")" @end) @indent
(function_type ")" @end) @indent

(array_type "]" @end) @indent
(array_literal "]" @end) @indent
(attribute_type_list "]" @end) @indent

(type_arguments ">" @end) @indent
