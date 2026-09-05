//! The diagnostic code table: every code the toolchain emits, and what it means.
//!
//! The table itself is `diagnostic-codes.tsv` beside this crate's manifest, one
//! row per code. It is data rather than Rust because three artifacts are
//! written from it: the `KiraError` enum a Kira program reads a diagnostic
//! with, the function that turns code text into one, and the diagnostics
//! appendix. `build.rs` turns the rows into the table below, so a malformed
//! row fails the build rather than a lookup.
//!
//! `kira-diagnostic-registry` is what makes the table authoritative. It fails
//! when the compiler emits a code the table does not list, when the table lists
//! one nothing emits, and when a generated artifact no longer matches.

/// One row of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisteredCode {
    /// The code as it is written in a diagnostic, such as `KSEM107`.
    pub code: &'static str,
    /// One line saying what the code refuses.
    pub summary: &'static str,
    /// The part of the toolchain that owns the code.
    pub family: CodeFamily,
}

/// The part of the toolchain a code's family belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CodeFamily {
    /// `KLEX`: the lexer.
    Lexer,
    /// `KPAR`: the parser.
    Parser,
    /// `KMAC`: macro expansion.
    Macro,
    /// `KSEM`: semantic analysis.
    Semantics,
    /// `KIR`: HIR and IR lowering.
    Lowering,
    /// `KBE`: a code-generation backend.
    Backend,
    /// `KSLP`: the KSL parser.
    ShaderParser,
    /// `KSLS`: KSL semantics and the shader build seam.
    ShaderSemantics,
    /// `KPK`: packages, projects, and manifests.
    Package,
    /// `KTC`: toolchain discovery and activation.
    Toolchain,
    /// `KCL`: the command line.
    Cli,
    /// `KLINT`: the package's own linter.
    Lint,
    /// `KIC`: an internal compiler error.
    Internal,
}

/// Every family, in the order the table and the generated artifacts use.
///
/// The order runs with the compiler: source text, the phases that read it, then
/// the tools around them.
pub const FAMILIES: [CodeFamily; 13] = [
    CodeFamily::Lexer,
    CodeFamily::Parser,
    CodeFamily::Macro,
    CodeFamily::Semantics,
    CodeFamily::Lowering,
    CodeFamily::Backend,
    CodeFamily::ShaderParser,
    CodeFamily::ShaderSemantics,
    CodeFamily::Package,
    CodeFamily::Toolchain,
    CodeFamily::Cli,
    CodeFamily::Lint,
    CodeFamily::Internal,
];

impl CodeFamily {
    /// The family's code prefix, such as `KSEM`.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            CodeFamily::Lexer => "KLEX",
            CodeFamily::Parser => "KPAR",
            CodeFamily::Macro => "KMAC",
            CodeFamily::Semantics => "KSEM",
            CodeFamily::Lowering => "KIR",
            CodeFamily::Backend => "KBE",
            CodeFamily::ShaderParser => "KSLP",
            CodeFamily::ShaderSemantics => "KSLS",
            CodeFamily::Package => "KPK",
            CodeFamily::Toolchain => "KTC",
            CodeFamily::Cli => "KCL",
            CodeFamily::Lint => "KLINT",
            CodeFamily::Internal => "KIC",
        }
    }

    /// What the family owns, as the diagnostics appendix states it.
    #[must_use]
    pub const fn owner(self) -> &'static str {
        match self {
            CodeFamily::Lexer => "The lexer.",
            CodeFamily::Parser => "The parser.",
            CodeFamily::Macro => "Macro expansion.",
            CodeFamily::Semantics => "Semantic analysis. The largest set by far.",
            CodeFamily::Lowering => "HIR and IR lowering.",
            CodeFamily::Backend => "A code-generation backend.",
            CodeFamily::ShaderParser => "The KSL parser.",
            CodeFamily::ShaderSemantics => "KSL semantics and the shader build seam.",
            CodeFamily::Package => "Package and project discovery, and manifests.",
            CodeFamily::Toolchain => "Toolchain discovery and activation.",
            CodeFamily::Cli => "The command line.",
            CodeFamily::Lint => "The package's own linter, including lints written in Kira.",
            CodeFamily::Internal => "An internal compiler error.",
        }
    }

    /// The family a code belongs to, or `None` when nothing owns its prefix.
    ///
    /// Longest prefix wins: `KSLP001` is the KSL parser, not a `KSL` family
    /// with a four-digit number.
    #[must_use]
    pub fn of(code: &str) -> Option<Self> {
        let mut best: Option<Self> = None;
        for family in FAMILIES {
            let prefix = family.prefix();
            let Some(digits) = code.strip_prefix(prefix) else {
                continue;
            };
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            if best.is_none_or(|current| prefix.len() > current.prefix().len()) {
                best = Some(family);
            }
        }
        best
    }
}

include!(concat!(env!("OUT_DIR"), "/diagnostic_codes.rs"));

/// Every registered code, ordered by family and then by number.
#[must_use]
pub fn all() -> &'static [RegisteredCode] {
    &CODES
}

/// The registered codes of one family, in number order.
pub fn family(family: CodeFamily) -> impl Iterator<Item = &'static RegisteredCode> {
    all().iter().filter(move |entry| entry.family == family)
}

/// The row for `code`, or `None` when the table does not list it.
#[must_use]
pub fn lookup(code: &str) -> Option<&'static RegisteredCode> {
    all().iter().find(|entry| entry.code == code)
}

/// Whether the table lists `code`.
#[must_use]
pub fn contains(code: &str) -> bool {
    lookup(code).is_some()
}

#[cfg(test)]
mod tests {
    use super::{CodeFamily, FAMILIES, RegisteredCode, all, contains, family, lookup};

    /// The rank a row sorts under: its family's position, then its number.
    fn rank(entry: &RegisteredCode) -> (usize, u32) {
        let position = FAMILIES
            .iter()
            .position(|candidate| *candidate == entry.family)
            .expect("every family is listed");
        let number = entry.code[entry.family.prefix().len()..]
            .parse()
            .expect("the build script admits only a numeric suffix");
        (position, number)
    }

    #[test]
    fn the_table_carries_every_family() {
        assert!(all().len() > 400, "{}", all().len());
        for candidate in FAMILIES {
            assert!(
                family(candidate).next().is_some(),
                "{} has no codes",
                candidate.prefix()
            );
        }
    }

    #[test]
    fn a_code_is_listed_once_and_in_order() {
        let mut previous = (0usize, 0u32);
        for entry in all() {
            let current = rank(entry);
            assert!(current > previous, "{} is out of order", entry.code);
            previous = current;
        }
    }

    #[test]
    fn a_row_carries_the_family_its_prefix_names() {
        for entry in all() {
            assert_eq!(
                CodeFamily::of(entry.code),
                Some(entry.family),
                "{}",
                entry.code
            );
            assert!(!entry.summary.is_empty(), "{}", entry.code);
        }
    }

    #[test]
    fn the_longest_matching_prefix_wins() {
        assert_eq!(CodeFamily::of("KSLP001"), Some(CodeFamily::ShaderParser));
        assert_eq!(CodeFamily::of("KSLS001"), Some(CodeFamily::ShaderSemantics));
        assert_eq!(CodeFamily::of("KIC001"), Some(CodeFamily::Internal));
        assert_eq!(CodeFamily::of("KZZZ001"), None);
        assert_eq!(CodeFamily::of("KSEM"), None);
    }

    #[test]
    fn a_known_code_resolves_to_its_row() {
        let moved = lookup("KSEM107").expect("KSEM107 is registered");
        assert_eq!(moved.family, CodeFamily::Semantics);
        assert!(moved.summary.contains("moved"), "{}", moved.summary);
        assert!(contains("KSEM367"));
        assert!(!contains("KSEM999"));
    }

    #[test]
    fn a_family_selects_only_its_own() {
        assert_eq!(family(CodeFamily::Lexer).count(), 6);
        assert!(family(CodeFamily::Lexer).all(|entry| entry.code.starts_with("KLEX")));
    }
}
