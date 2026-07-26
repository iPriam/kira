//! What checking must accept, and what it must refuse.

use kira_shader_model::{AccessMode, ResourceKind, ScalarType, Stage, Type, VectorType};
use kira_source::SourceId;

use crate::model::{CheckedExprKind, ConstValue};
use crate::{Checked, Module, check};

/// Parses and checks `text` on its own.
fn check_text(text: &str) -> Checked {
    let parsed = kira_ksl_parser::parse(SourceId::new(0), text);
    assert!(parsed.is_clean(), "{:?}", parsed.diagnostics);
    check(
        &Module {
            source: SourceId::new(0),
            tree: parsed.tree,
            interner: parsed.interner,
        },
        &[],
    )
}

/// Checks `text`, asserting it reported nothing.
fn clean(text: &str) -> Checked {
    let checked = check_text(text);
    assert!(
        checked.is_clean(),
        "{:?}",
        checked
            .diagnostics
            .iter()
            .map(|d| (d.code, d.message.clone()))
            .collect::<Vec<_>>()
    );
    checked
}

/// The codes checking `text` reported.
fn codes(text: &str) -> Vec<&'static str> {
    check_text(text)
        .diagnostics
        .into_iter()
        .filter_map(|d| d.code)
        .collect()
}

const GRAPHICS: &str = r#"
type VertexIn {
    let position: Float2
}

type VertexOut {
    @builtin(position)
    let clip_position: Float4
    let color: Float4
}

type FragmentOut {
    let color: Float4
}

type Camera {
    let view_projection: Float4x4
}

shader Tri {
    group Frame {
        uniform camera: Camera
    }

    vertex {
        input VertexIn
        output VertexOut

        function entry(v: VertexIn) -> VertexOut {
            let result: VertexOut
            result.clip_position = mul(camera.view_projection, Float4(v.position, 0.0, 1.0))
            result.color = Float4(1.0, 0.95, 0.85, 1.0)
            return result
        }
    }

    fragment {
        input VertexOut
        output FragmentOut

        function entry(f: VertexOut) -> FragmentOut {
            let result: FragmentOut
            result.color = f.color
            return result
        }
    }
}
"#;

const COMPUTE: &str = r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}

type Particle {
    let px: Float
    let vy: Float
}

type Params {
    let count: UInt
}

shader SimStep {
    option workgroup_width: UInt = 64

    group Work {
        storage read page: [Float]
        storage read_write particles: [Particle]
        uniform params: Params
    }

    compute {
        input QIn
        threads(workgroup_width, 1, 1)

        function entry(q: QIn) {
            let i = q.gid.x
            if i < params.count {
                let sixteen: UInt = 16
                let ny = particles[i].px - particles[i].vy * 0.1
                let ix = UInt((particles[i].px + 2.0) / 0.25)
                let d = page[(ix * sixteen + ix) * sixteen + ix]
                if d < 0.0 {
                    particles[i].px = 0.0
                } else {
                    particles[i].px = ny
                }
            }
            return
        }
    }
}
"#;

#[test]
fn a_graphics_shader_checks_whole() {
    let checked = clean(GRAPHICS);
    let shader = checked.module.shader.expect("a shader");
    assert_eq!(shader.stages.len(), 2);
    assert_eq!(shader.stages[0].stage, Stage::Vertex);
    assert_eq!(shader.stages[1].stage, Stage::Fragment);
    assert_eq!(shader.groups[0].resources[0].kind, ResourceKind::Uniform);
}

#[test]
fn a_compute_shader_folds_an_option_into_its_thread_extents() {
    let checked = clean(COMPUTE);
    let shader = checked.module.shader.expect("a shader");
    assert_eq!(shader.stages[0].threads, Some([64, 1, 1]));
    assert_eq!(shader.options[0].value, ConstValue::Uint(64));
    assert_eq!(
        shader.groups[0].resources[1].access,
        Some(AccessMode::ReadWrite)
    );
}

#[test]
fn a_swizzle_narrows_a_vector_to_its_component_type() {
    let checked = clean(COMPUTE);
    let swizzle = checked
        .module
        .exprs
        .iter()
        .find(|(_, expr)| matches!(expr.kind, CheckedExprKind::Swizzle { .. }))
        .expect("`q.gid.x`");
    assert_eq!(swizzle.1.ty, Type::Scalar(ScalarType::Uint));
}

#[test]
fn a_literal_takes_the_type_its_position_expects() {
    // `let sixteen: UInt = 16` must store a `UInt`, not an `Int`, or every
    // arithmetic use of it downstream would mismatch.
    let checked = clean(COMPUTE);
    assert!(
        checked
            .module
            .exprs
            .iter()
            .any(|(_, expr)| matches!(expr.kind, CheckedExprKind::Const(ConstValue::Uint(16)))),
        "the literal kept its `Int` type"
    );
}

#[test]
fn a_vector_scales_by_a_scalar_in_either_order() {
    clean(
        r#"
type Out { let color: Float4 }
shader S {
    fragment {
        output Out
        function entry() -> Out {
            let result: Out
            let v = Float3(1.0, 1.0, 1.0)
            let scaled = v * 0.5
            let other = 2.0 * v
            result.color = Float4(scaled + other, 1.0)
            return result
        }
    }
}
"#,
    );
}

#[test]
fn mul_takes_the_matrixs_row_count_as_its_result_width() {
    let checked = clean(GRAPHICS);
    let call = checked
        .module
        .exprs
        .iter()
        .find(|(_, expr)| {
            matches!(
                expr.kind,
                CheckedExprKind::Builtin {
                    which: crate::model::BuiltinFn::Mul,
                    ..
                }
            )
        })
        .expect("the `mul`");
    assert_eq!(
        call.1.ty,
        Type::Vector(VectorType {
            scalar: ScalarType::Float,
            width: 4
        })
    );
}

#[test]
fn an_unknown_type_is_reported_by_name() {
    assert!(codes("type T {\n    let a: Nonsense\n}\n").contains(&"KSLS001"));
}

#[test]
fn an_unbound_name_is_reported() {
    assert!(codes("function f() -> Float {\n    return missing\n}\n").contains(&"KSLS002"));
}

#[test]
fn a_member_that_does_not_exist_is_reported() {
    assert!(
        codes("type T { let a: Float }\nfunction f(t: T) -> Float {\n    return t.b\n}\n")
            .contains(&"KSLS006")
    );
}

#[test]
fn a_component_past_the_end_of_a_vector_is_reported() {
    assert!(codes("function f(v: Float2) -> Float {\n    return v.z\n}\n").contains(&"KSLS006"));
}

#[test]
fn a_builtin_illegal_for_its_stage_is_reported() {
    // `thread_id` is a compute input; a vertex stage cannot carry it.
    assert!(
        codes(
            r#"
type In {
    @builtin(thread_id)
    let id: UInt3
}
type Out { let color: Float4 }
shader S {
    vertex {
        input In
        output Out
        function entry(i: In) -> Out {
            let r: Out
            return r
        }
    }
}
"#
        )
        .contains(&"KSLS007")
    );
}

#[test]
fn writing_through_a_uniform_is_refused() {
    assert!(
        codes(
            r#"
type P { let count: UInt }
shader S {
    group G {
        uniform params: P
    }
    compute {
        threads(1, 1, 1)
        function entry() {
            params.count = 1
            return
        }
    }
}
"#
        )
        .contains(&"KSLS008")
    );
}

#[test]
fn writing_through_read_write_storage_is_allowed() {
    clean(
        r#"
shader S {
    group G {
        storage read_write out: [UInt]
    }
    compute {
        threads(1, 1, 1)
        function entry() {
            out[0] = 1
            return
        }
    }
}
"#,
    );
}

#[test]
fn writing_through_read_only_storage_is_refused() {
    assert!(
        codes(
            r#"
shader S {
    group G {
        storage read page: [UInt]
    }
    compute {
        threads(1, 1, 1)
        function entry() {
            page[0] = 1
            return
        }
    }
}
"#
        )
        .contains(&"KSLS008")
    );
}

#[test]
fn a_uniform_of_a_non_struct_type_is_refused() {
    assert!(
        codes("shader S {\n    group G {\n        uniform x: Float\n    }\n}\n")
            .contains(&"KSLS012")
    );
}

#[test]
fn a_compute_stage_without_threads_is_refused() {
    assert!(
        codes(
            "shader S {\n    compute {\n        function entry() {\n            return\n        }\n    }\n}\n"
        )
        .contains(&"KSLS009")
    );
}

#[test]
fn a_stage_without_an_entry_is_refused() {
    assert!(codes("shader S {\n    vertex {\n    }\n}\n").contains(&"KSLS009"));
}

#[test]
fn a_function_that_can_fall_out_without_returning_is_refused() {
    assert!(
        codes("function f(x: Float) -> Float {\n    if x > 0.0 {\n        return x\n    }\n}\n")
            .contains(&"KSLS015")
    );
}

#[test]
fn a_return_in_both_branches_satisfies_the_result() {
    clean(
        "function f(x: Float) -> Float {\n    if x > 0.0 {\n        return x\n    } else {\n        return 0.0\n    }\n}\n",
    );
}

#[test]
fn a_wrong_component_count_in_a_constructor_is_reported() {
    assert!(
        codes("function f() -> Float4 {\n    return Float4(1.0, 2.0)\n}\n").contains(&"KSLS005")
    );
}

#[test]
fn a_vector_constructor_counts_its_arguments_components() {
    // `Float4(v.position, 0.0, 1.0)` is 2 + 1 + 1 = 4.
    clean("function f(v: Float2) -> Float4 {\n    return Float4(v, 0.0, 1.0)\n}\n");
}

#[test]
fn comparing_two_different_types_is_reported() {
    assert!(
        codes("function f(a: Float, b: UInt) -> Bool {\n    return a < b\n}\n")
            .contains(&"KSLS014")
    );
}

#[test]
fn one_bad_line_does_not_hide_the_next() {
    let reported = codes(
        "function f() -> Float {\n    let a = missing_one\n    let b = missing_two\n    return 0.0\n}\n",
    );
    assert_eq!(
        reported.iter().filter(|code| **code == "KSLS002").count(),
        2,
        "{reported:?}"
    );
}

#[test]
fn an_array_reports_how_many_elements_it_holds() {
    // `particles.count` is the one member an array has, and the binding decides
    // it at draw time — so it is never folded to a constant.
    let checked = clean(
        r#"
type P { let x: Float }
shader S {
    group G {
        storage read particles: [P]
    }
    compute {
        threads(1, 1, 1)
        function entry() {
            let n = particles.count
            return
        }
    }
}
"#,
    );
    assert!(
        checked.module.exprs.iter().any(|(_, expr)| matches!(
            expr.kind,
            CheckedExprKind::ArrayLength { .. }
        ) && expr.ty == Type::Scalar(ScalarType::Uint)),
        "`particles.count` is a `UInt` array length"
    );
}

#[test]
fn an_arrays_only_member_is_its_count() {
    assert!(
        codes(
            r#"
shader S {
    group G {
        storage read page: [Float]
    }
    compute {
        threads(1, 1, 1)
        function entry() {
            let n = page.length
            return
        }
    }
}
"#
        )
        .contains(&"KSLS006")
    );
}

#[test]
fn an_unqualified_call_inside_an_imported_module_reaches_its_own_sibling() {
    // `lambert` calls `saturate`, both declared in the imported file. The call
    // has to reach `Lighting_saturate`, not look for a bare `saturate` that the
    // importing file never declared.
    let library = kira_ksl_parser::parse(
        SourceId::new(1),
        "function saturate(v: Float) -> Float {\n    return v\n}\n\
         function lambert(v: Float) -> Float {\n    return saturate(v)\n}\n",
    );
    assert!(library.is_clean(), "{:?}", library.diagnostics);
    let main = kira_ksl_parser::parse(
        SourceId::new(0),
        "import Common.Lighting as Lighting\n\
         function f(x: Float) -> Float {\n    return Lighting.lambert(x)\n}\n",
    );
    assert!(main.is_clean(), "{:?}", main.diagnostics);
    let checked = check(
        &Module {
            source: SourceId::new(0),
            tree: main.tree,
            interner: main.interner,
        },
        &[(
            "Lighting".to_owned(),
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
            .map(|d| (d.code, d.message.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        checked
            .module
            .functions
            .iter()
            .any(|f| f.name == "Lighting_saturate"),
        "the imported function kept its alias prefix"
    );
}

#[test]
fn an_import_with_no_module_supplied_is_reported() {
    let main = kira_ksl_parser::parse(
        SourceId::new(0),
        "import Common.Missing as Missing\nfunction f() -> Float {\n    return 0.0\n}\n",
    );
    let checked = check(
        &Module {
            source: SourceId::new(0),
            tree: main.tree,
            interner: main.interner,
        },
        &[],
    );
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.code == Some("KSLS011")),
        "{:?}",
        checked.diagnostics
    );
}
