//! Renders the code table as the three artifacts written from it.

use kira_diagnostic_messages::registry::{self, CodeFamily, FAMILIES, RegisteredCode};

/// The line every generated artifact carries, so a reader who opens one knows
/// where to make the change instead.
const PROVENANCE: [&str; 2] = [
    "Generated from `crates/kira-diagnostic-messages/diagnostic-codes.tsv`.",
    "Run `cargo run -p kira-diagnostic-registry -- write` to refresh it.",
];

/// The families that have at least one code, in table order.
fn populated() -> impl Iterator<Item = (CodeFamily, Vec<&'static RegisteredCode>)> {
    FAMILIES.into_iter().filter_map(|family| {
        let codes: Vec<_> = registry::family(family).collect();
        (!codes.is_empty()).then_some((family, codes))
    })
}

/// `foundation/app/Kira/Diagnostics.kira`: the enum a program reads a
/// diagnostic's code as.
#[must_use]
pub fn kira_enum() -> String {
    let mut out = String::new();
    for line in PROVENANCE {
        out.push_str("// ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(
        "//\n\
         // `KiraError` names every code the toolchain emits. A test asserting\n\
         // `.KSEM061` compares a value, and goes on comparing it after the message is\n\
         // reworded, which is the whole reason a code exists.\n\
         \n\
         enum KiraError {\n",
    );
    for (family, codes) in populated() {
        out.push_str(&format!("    // {}\n", family.owner()));
        for entry in codes {
            out.push_str(&format!("    {}\n", entry.code));
        }
        out.push('\n');
    }
    out.push_str(
        "    // Not a diagnostic code: what `kiraErrorFromCode` answers for a code this\n\
         \x20   // enum does not list, so reading a diagnostic is total.\n\
         \x20   Unrecognized\n\
         }\n",
    );
    out
}

/// `foundation/app/Kira/DiagnosticCodes.kira`: code text to `KiraError`.
#[must_use]
pub fn kira_from_code() -> String {
    let mut out = String::new();
    for line in PROVENANCE {
        out.push_str("// ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!(
        "//\n\
         // One `if` per code rather than a `match`: `match` selects on an enum, and the\n\
         // question here is the other way round. Splitting the chain by code prefix was\n\
         // measured and made no difference, so this costs what its {} entries cost and\n\
         // no more.\n\
         \n\
         // The code `text` names, or `.Unrecognized` when `KiraError` does not list it.\n\
         function kiraErrorFromCode(text: borrow String) -> KiraError {{\n",
        registry::all().len()
    ));
    for entry in registry::all() {
        out.push_str(&format!(
            "    if text == \"{code}\" {{ return .{code} }}\n",
            code = entry.code
        ));
    }
    out.push_str("    return .Unrecognized\n}\n");
    out
}

/// `sites/docs/content/docs/appendix/diagnostics/codes.mdx`: the appendix
/// listing every code.
#[must_use]
pub fn docs_index() -> String {
    let mut out = String::from(
        "---\n\
         title: Diagnostic Codes\n\
         description: Every code the Kira toolchain emits, by family, with what each one refuses.\n\
         ---\n\
         \n",
    );
    out.push_str(&format!(
        "{{/* {} {} */}}\n\n",
        PROVENANCE[0], PROVENANCE[1]
    ));
    out.push_str(&format!(
        "The toolchain emits {} codes. A code is the stable part of a diagnostic: a message may be reworded, `KSEM107` will always be a use after move.\n",
        registry::all().len()
    ));
    for (family, codes) in populated() {
        out.push_str(&format!("\n## {}\n\n", family.prefix()));
        out.push_str(&format!("{}\n\n", family.owner()));
        out.push_str("| Code | Means |\n| --- | --- |\n");
        for entry in codes {
            out.push_str(&format!("| `{}` | {} |\n", entry.code, entry.summary));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{docs_index, kira_enum, kira_from_code};
    use kira_diagnostic_messages::registry;

    #[test]
    fn the_enum_lists_every_code_and_the_total_answer() {
        let rendered = kira_enum();
        for entry in registry::all() {
            assert!(
                rendered.contains(&format!("\n    {}\n", entry.code)),
                "{} is missing",
                entry.code
            );
        }
        assert!(rendered.contains("\n    Unrecognized\n}\n"));
        assert!(rendered.trim_start().starts_with("// Generated from"));
    }

    #[test]
    fn every_code_has_an_arm_that_returns_its_own_variant() {
        let rendered = kira_from_code();
        for entry in registry::all() {
            assert!(
                rendered.contains(&format!(
                    "    if text == \"{code}\" {{ return .{code} }}\n",
                    code = entry.code
                )),
                "{} is missing",
                entry.code
            );
        }
        assert!(rendered.ends_with("    return .Unrecognized\n}\n"));
    }

    #[test]
    fn the_appendix_rows_are_one_table_cell_each() {
        let rendered = docs_index();
        for entry in registry::all() {
            assert!(
                rendered.contains(&format!("| `{}` | {} |", entry.code, entry.summary)),
                "{} is missing",
                entry.code
            );
            assert!(
                !entry.summary.contains('|'),
                "{} would split its row",
                entry.code
            );
        }
        assert!(rendered.starts_with("---\ntitle: Diagnostic Codes\n"));
    }
}
