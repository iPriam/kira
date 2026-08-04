//! What `const` and `enum` must fold to, and what they must refuse.

use kira_source::SourceId;

use super::{check_text, clean, codes};
use crate::model::{CheckedExprKind, ConstValue};
use crate::{Module, check};

/// A shader that reads `subject` from its fragment stage, so a constant
/// written above it has somewhere to be used.
fn reading(declarations: &str, subject: &str) -> String {
    format!(
        "{declarations}\n\
         type In {{\n    @builtin(position)\n    let clip_position: Float4\n}}\n\
         type Out {{\n    let color: Float4\n}}\n\
         shader S {{\n\
         \x20   fragment {{\n\
         \x20       input In\n\
         \x20       output Out\n\
         \x20       function entry(f: In) -> Out {{\n\
         \x20           let result: Out\n\
         \x20           result.color = Float4({subject}, 0.0, 0.0, 1.0)\n\
         \x20           return result\n\
         \x20       }}\n\
         \x20   }}\n\
         }}\n"
    )
}

/// Whether the checked form of `text` holds `value` as a folded constant.
fn folds_to(text: &str, value: ConstValue) -> bool {
    clean(text)
        .module
        .exprs
        .iter()
        .any(|(_, expr)| expr.kind == CheckedExprKind::Const(value))
}

#[test]
fn a_const_reads_as_the_value_it_folded_to() {
    // Nothing named `INK_LOW` may survive into the checked module: no shader
    // dialect agrees on how a module-scope constant is spelled, so the read
    // has to have become the number before any backend sees it.
    assert!(folds_to(
        &reading("const INK_LOW: Float = 0.25", "INK_LOW"),
        ConstValue::Float(0.25)
    ));
}

#[test]
fn an_enum_variant_reads_as_its_number() {
    assert!(folds_to(
        &reading(
            "enum Ink {\n    Low = 1,\n    High = 2\n}",
            "Float(Ink.High)"
        ),
        ConstValue::Int(2)
    ));
}

#[test]
fn an_enum_variant_takes_the_type_its_position_expects() {
    // Variants are written as whole numbers but get compared against `UInt`
    // fields all the time, so the fold has to retype the way a literal does.
    assert!(folds_to(
        &reading(
            "enum Ink {\n    Low = 1\n}\nconst WIDTH: UInt = Ink.Low",
            "Float(WIDTH)"
        ),
        ConstValue::Uint(1)
    ));
}

#[test]
fn a_const_can_be_built_from_a_const_written_above_it() {
    assert!(folds_to(
        &reading("const BASE: Float = 0.5\nconst SAME: Float = BASE", "SAME"),
        ConstValue::Float(0.5)
    ));
}

#[test]
fn a_const_sets_a_thread_count() {
    clean(
        "const WIDTH: UInt = 64\n\
         type QIn {\n    @builtin(thread_id)\n    let gid: UInt3\n}\n\
         shader Sim {\n\
         \x20   compute {\n\
         \x20       input QIn\n\
         \x20       threads(WIDTH, 1, 1)\n\
         \x20       function entry(q: QIn) {\n\
         \x20           let i = q.gid.x\n\
         \x20       }\n\
         \x20   }\n\
         }\n",
    );
}

#[test]
fn a_const_that_is_not_constant_is_reported() {
    let reported = codes(&reading("const NOISE: Float = clip_position.x", "NOISE"));
    assert!(
        reported.iter().any(|code| code == "KSLS017"),
        "{reported:?}"
    );
}

#[test]
fn a_name_declared_twice_is_reported() {
    let reported = codes(&reading("const N: Float = 1.0\nconst N: Float = 2.0", "N"));
    assert!(
        reported.iter().any(|code| code == "KSLS003"),
        "{reported:?}"
    );
}

#[test]
fn an_unknown_variant_of_a_known_enum_is_reported() {
    // `Ink` names no value, so the read falls through to an ordinary member
    // access and is reported as an unbound name rather than silently folding.
    let reported = codes(&reading("enum Ink {\n    Low = 1\n}", "Float(Ink.Nope)"));
    assert!(!reported.is_empty(), "an unknown variant reported nothing");
}

#[test]
fn a_const_in_an_imported_module_is_reached_through_its_alias() {
    let library = kira_ksl_parser::parse(
        SourceId::new(1),
        "const GAMMA: Float = 2.2\nenum Ink {\n    Low = 7\n}\n",
    );
    assert!(library.is_clean(), "{:?}", library.diagnostics);
    let main = kira_ksl_parser::parse(
        SourceId::new(0),
        "import Common.Ramp as Ramp\n\
         function f() -> Float {\n    return Ramp.GAMMA + Float(Ramp.Ink.Low)\n}\n\
         type In {\n    @builtin(position)\n    let clip_position: Float4\n}\n\
         type Out {\n    let color: Float4\n}\n\
         shader S {\n\
         \x20   fragment {\n\
         \x20       input In\n\
         \x20       output Out\n\
         \x20       function entry(v: In) -> Out {\n\
         \x20           let result: Out\n\
         \x20           result.color = Float4(f(), 0.0, 0.0, 1.0)\n\
         \x20           return result\n\
         \x20       }\n\
         \x20   }\n\
         }\n",
    );
    assert!(main.is_clean(), "{:?}", main.diagnostics);
    let checked = check(
        &Module {
            source: SourceId::new(0),
            tree: main.tree,
            interner: main.interner,
        },
        &[(
            "Ramp".to_owned(),
            Module {
                source: SourceId::new(1),
                tree: library.tree,
                interner: library.interner,
            },
        )],
    );
    assert!(
        checked.is_clean(),
        "{:?}",
        checked
            .diagnostics
            .iter()
            .map(|d| (d.code.clone(), d.message.clone()))
            .collect::<Vec<_>>()
    );
    for value in [ConstValue::Float(2.2), ConstValue::Int(7)] {
        assert!(
            checked
                .module
                .exprs
                .iter()
                .any(|(_, expr)| expr.kind == CheckedExprKind::Const(value)),
            "{value:?} did not fold"
        );
    }
}

#[test]
fn a_const_emits_nothing_of_its_own() {
    // The whole point: a `const` is a source-level convenience, so the checked
    // module a backend walks holds no trace of it.
    let checked = check_text(&reading("const INK_LOW: Float = 0.25", "INK_LOW"));
    assert!(
        checked.module.functions.iter().all(|f| f.name != "INK_LOW"),
        "a const reached the module as a function"
    );
    assert!(
        checked.module.structs.iter().all(|s| s.name != "INK_LOW"),
        "a const reached the module as a struct"
    );
}
