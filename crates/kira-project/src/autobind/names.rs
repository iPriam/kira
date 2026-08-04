//! C names that cannot be written in Kira, and the ones a header leaves out.
//!
//! C and Kira share an identifier grammar but not a keyword set, so a C
//! function called `default` or a field called `type` has no Kira spelling. A
//! generator that renamed it would produce a binding whose function name is not
//! the C symbol, which is the one thing every call site here depends on — so
//! the declaration is refused by name instead, and the reason says why.

use kira_syntax_model::TokenKind;

/// Why `name` cannot be written in a binding, or `None` when it can.
///
/// The keyword set comes from the lexer rather than from a list copied here: a
/// keyword added to the language would otherwise start generating bindings that
/// do not parse.
pub(super) fn unbindable_name(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("it has no name to declare".to_owned());
    }
    if TokenKind::keyword_from_text(name).is_some() {
        return Some(format!(
            "`{name}` is a Kira keyword, so no declaration can carry the C symbol's own name"
        ));
    }
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        return Some(format!("`{name}` does not start with a letter or `_`"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii())
    {
        return Some(format!("`{name}` is not spellable as a Kira identifier"));
    }
    None
}

/// The name a parameter is written under, given what C called it.
///
/// C prototypes may name no parameters at all, and a parameter named for a Kira
/// keyword cannot keep that name — neither is a reason to refuse the function,
/// because a parameter name is local to the declaration and binds nothing. Only
/// the *symbol* has to survive unchanged.
pub(super) fn parameter_name(written: &str, index: usize) -> String {
    match written.is_empty() {
        true => format!("arg{index}"),
        false => local_name(written),
    }
}

/// The name a struct field is written under, given what C called it.
///
/// A field name is local in the same way a parameter name is: `sg_image_desc`
/// has a field C calls `type`, which Kira reads as a keyword, and refusing the
/// whole struct over it would cost every function that takes one. The renamed
/// spelling keeps the C name visible so a reader can match the two.
pub(super) fn field_name(written: &str, taken: &[String]) -> Option<String> {
    if written.is_empty() {
        return None;
    }
    let mut candidate = local_name(written);
    while taken.iter().any(|already| already == &candidate) {
        candidate.push('_');
    }
    Some(candidate)
}

/// A local name that is spellable in Kira, renaming only when it has to be.
///
/// The `_value` suffix is the spelling the generated dialect already uses for
/// this, so a program written against an earlier binding still compiles.
fn local_name(written: &str) -> String {
    match unbindable_name(written) {
        None => written.to_owned(),
        Some(_) => format!("{written}_value"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keyword_is_refused_as_a_declared_name_and_renamed_as_a_local_one() {
        assert!(unbindable_name("type").is_some());
        assert!(unbindable_name("default").is_some());
        assert_eq!(parameter_name("type", 2), "type_value");
        assert_eq!(parameter_name("", 0), "arg0");
        assert_eq!(parameter_name("font_path", 0), "font_path");
    }

    #[test]
    fn a_renamed_field_never_collides_with_one_the_struct_already_has() {
        let taken = vec!["type_value".to_owned()];
        assert_eq!(field_name("type", &taken).as_deref(), Some("type_value_"));
        assert_eq!(field_name("width", &taken).as_deref(), Some("width"));
        assert_eq!(field_name("", &taken), None);
    }

    #[test]
    fn an_ordinary_c_name_is_spellable() {
        assert_eq!(unbindable_name("kira_text_draw_run"), None);
        assert_eq!(unbindable_name("_opaque_pthread_t"), None);
    }
}
