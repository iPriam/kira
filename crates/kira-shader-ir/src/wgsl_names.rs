//! The name an identifier takes once it reaches WGSL.
//!
//! The counterpart of [`crate::glsl_safe_name`], and here for the same reason:
//! the emitted name is a decision this crate owns, so the reflection and the
//! backend cannot disagree about it.
//!
//! WGSL reserves an unusually large vocabulary — its specification sets aside
//! every keyword any plausible future version might want, which includes `ref`,
//! `external`, `type`, `set`, `mod`, `match` and `resource`. KSL allows all of
//! them, and the UI corpus already uses two: a texture called `external` and a
//! local called `ref`. Both compiled everywhere else and were refused by Dawn
//! with `'external' is a reserved keyword`, which — because a shader module is
//! still handed back for source WebGPU rejected — surfaced as a pipeline that
//! could not be built rather than as a shader that could not be compiled.

/// A name WGSL will accept, given a name KSL allowed.
///
/// Reserved names are prefixed rather than rejected: what the author called a
/// thing is their business, and the emitted name is nobody's but the backend's.
/// The prefix is `ksl_`, the same one GLSL uses, so a reader who has seen one
/// recognises the other.
#[must_use]
pub fn wgsl_safe_name(name: &str) -> String {
    if KEYWORDS.contains(&name) || RESERVED.contains(&name) {
        return format!("ksl_{name}");
    }
    name.to_owned()
}

/// WGSL's live keywords — the words that mean something today.
const KEYWORDS: &[&str] = &[
    "alias",
    "break",
    "case",
    "const",
    "const_assert",
    "continue",
    "continuing",
    "default",
    "diagnostic",
    "discard",
    "else",
    "enable",
    "false",
    "fn",
    "for",
    "if",
    "let",
    "loop",
    "override",
    "requires",
    "return",
    "struct",
    "switch",
    "true",
    "var",
    "while",
];

/// WGSL's reserved words — set aside by the specification, meaningless today and
/// refused all the same.
const RESERVED: &[&str] = &[
    "NULL",
    "Self",
    "abstract",
    "active",
    "alignas",
    "alignof",
    "as",
    "asm",
    "asm_fragment",
    "async",
    "attribute",
    "auto",
    "await",
    "become",
    "binding_array",
    "cast",
    "catch",
    "class",
    "co_await",
    "co_return",
    "co_yield",
    "coherent",
    "column_major",
    "common",
    "compile",
    "compile_fragment",
    "concept",
    "const_cast",
    "consteval",
    "constexpr",
    "constinit",
    "crate",
    "debugger",
    "decltype",
    "delete",
    "demote",
    "demote_to_helper",
    "do",
    "dynamic_cast",
    "enum",
    "explicit",
    "export",
    "extends",
    "extern",
    "external",
    "fallthrough",
    "filter",
    "final",
    "finally",
    "friend",
    "from",
    "fxgroup",
    "get",
    "goto",
    "groupshared",
    "highp",
    "impl",
    "implements",
    "import",
    "inline",
    "instanceof",
    "interface",
    "layout",
    "lowp",
    "macro",
    "macro_rules",
    "match",
    "mediump",
    "meta",
    "mod",
    "module",
    "move",
    "mut",
    "mutable",
    "namespace",
    "new",
    "nil",
    "noexcept",
    "noinline",
    "nointerpolation",
    "non_coherent",
    "noncoherent",
    "noperspective",
    "null",
    "nullptr",
    "of",
    "operator",
    "package",
    "packoffset",
    "partition",
    "pass",
    "patch",
    "pixelfragment",
    "precise",
    "precision",
    "premerge",
    "priv",
    "protected",
    "pub",
    "public",
    "readonly",
    "ref",
    "regardless",
    "register",
    "reinterpret_cast",
    "require",
    "resource",
    "restrict",
    "self",
    "set",
    "shared",
    "sizeof",
    "smooth",
    "snorm",
    "static",
    "static_assert",
    "static_cast",
    "std",
    "subroutine",
    "super",
    "target",
    "template",
    "this",
    "thread_local",
    "throw",
    "trait",
    "try",
    "type",
    "typedef",
    "typeid",
    "typename",
    "typeof",
    "union",
    "unless",
    "unorm",
    "unsafe",
    "unsized",
    "use",
    "using",
    "varying",
    "virtual",
    "volatile",
    "wgsl",
    "where",
    "with",
    "writeonly",
    "yield",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserved_word_is_prefixed_and_an_ordinary_name_is_not() {
        // The two the UI corpus actually uses.
        assert_eq!(wgsl_safe_name("external"), "ksl_external");
        assert_eq!(wgsl_safe_name("ref"), "ksl_ref");
        // A live keyword, not merely reserved.
        assert_eq!(wgsl_safe_name("var"), "ksl_var");
        assert_eq!(wgsl_safe_name("atlas"), "atlas");
        assert_eq!(wgsl_safe_name("backdrop"), "backdrop");
    }

    /// The two backends prefix the same way, so a name refused by both reads
    /// identically in either output.
    #[test]
    fn the_prefix_matches_the_glsl_one() {
        assert_eq!(
            wgsl_safe_name("external"),
            crate::glsl_safe_name("external")
        );
    }
}
