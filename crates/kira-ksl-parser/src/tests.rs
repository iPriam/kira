//! What the parser must accept, taken from the shapes the shader corpus writes.

use kira_ksl_syntax_model::ast::{
    Access, BinaryOp, Expr, Item, ResourceKind, StageWord, Stmt, TypeRef,
};
use kira_source::SourceId;

use crate::{Parsed, parse};

/// Parses `text`, asserting it reported nothing.
fn clean(text: &str) -> Parsed {
    let parsed = parse(SourceId::new(0), text);
    assert!(
        parsed.is_clean(),
        "{:?}",
        parsed
            .diagnostics
            .iter()
            .map(|d| (d.code, d.message.clone()))
            .collect::<Vec<_>>()
    );
    parsed
}

/// The codes `text` reported.
fn codes(text: &str) -> Vec<&'static str> {
    parse(SourceId::new(0), text)
        .diagnostics
        .into_iter()
        .filter_map(|diagnostic| diagnostic.code)
        .collect()
}

const GRAPHICS: &str = r#"
type VertexIn {
    let position: Float2
}

type VertexOut {
    @builtin(position)
    let clip_position: Float4

    @interpolate(flat)
    let color: Float4
}

type FragmentOut {
    let color: Float4
}

shader BasicTriangle {
    vertex {
        input VertexIn
        output VertexOut

        function entry(vertexInput: VertexIn) -> VertexOut {
            let result: VertexOut
            result.clip_position = Float4(vertexInput.position, 0.0, 1.0)
            result.color = Float4(1.0, 0.95, 0.85, 1.0)
            return result
        }
    }

    fragment {
        input VertexOut
        output FragmentOut

        function entry(fragmentInput: VertexOut) -> FragmentOut {
            let result: FragmentOut
            result.color = fragmentInput.color
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

type SimParams {
    let count: UInt
}

shader SimStep {
    option workgroup_width: UInt = 64

    group Work {
        storage read page: [Float]
        storage read_write particles: [Particle]
        uniform params: SimParams
    }

    compute {
        input QIn
        threads(workgroup_width, 1, 1)

        function entry(q: QIn) {
            let i = q.gid.x
            if i < params.count {
                let ny = particles[i].px - particles[i].vy * 0.1
                if ny < 0.0 {
                    particles[i].px = 0.0
                } else if ny > 1.0 {
                    particles[i].px = 1.0
                } else {
                    particles[i].px = ny
                }
            }
            return
        }
    }
}
"#;

const IMPORTS: &str = r#"
import Common.Lighting as Lighting

type SurfaceUniform {
    let albedo_color: Float3
    let alpha: Float
}

function saturate(value: Float) -> Float {
    if value < 0.0 { return 0.0 }
    return value
}

shader LitSurface {
    group Material {
        uniform surface: SurfaceUniform
        texture albedo: Texture2d
        sampler linear: Sampler
    }

    fragment {
        function entry(input: SurfaceUniform) -> SurfaceUniform {
            let lit = Lighting.lambert(input.albedo_color, input.albedo_color)
            let sampled = sample(albedo, linear, input.albedo_color)
            return input
        }
    }
}
"#;

#[test]
fn a_graphics_shader_parses_whole() {
    let parsed = clean(GRAPHICS);
    assert_eq!(parsed.tree.items.len(), 4);
    let Some(Item::Shader(shader)) = parsed.tree.items.last() else {
        panic!("the last item is the shader");
    };
    assert_eq!(shader.stages.len(), 2);
    assert_eq!(shader.stages[0].stage, StageWord::Vertex);
    assert_eq!(shader.stages[1].stage, StageWord::Fragment);
    assert!(shader.stages[0].input.is_some());
    assert!(shader.stages[0].output.is_some());
    assert_eq!(shader.stages[0].functions.len(), 1);
}

#[test]
fn annotations_land_on_the_field_they_were_written_above() {
    let parsed = clean(GRAPHICS);
    let Some(Item::Type(declared)) = parsed.tree.items.get(1) else {
        panic!("the second item is `type VertexOut`");
    };
    let builtin = declared.fields[0].builtin.expect("a builtin");
    assert_eq!(parsed.interner.resolve(builtin), "position");
    assert!(declared.fields[0].interpolation.is_none());
    let interpolation = declared.fields[1].interpolation.expect("interpolation");
    assert_eq!(parsed.interner.resolve(interpolation), "flat");
}

#[test]
fn a_compute_shader_carries_its_options_groups_and_thread_extents() {
    let parsed = clean(COMPUTE);
    let Some(Item::Shader(shader)) = parsed.tree.items.last() else {
        panic!("the last item is the shader");
    };
    assert_eq!(shader.options.len(), 1);
    assert_eq!(shader.groups.len(), 1);
    assert_eq!(shader.groups[0].resources.len(), 3);
    assert_eq!(shader.groups[0].resources[0].kind, ResourceKind::Storage);
    assert_eq!(shader.groups[0].resources[0].access, Some(Access::Read));
    assert_eq!(
        shader.groups[0].resources[1].access,
        Some(Access::ReadWrite)
    );
    assert_eq!(shader.groups[0].resources[2].kind, ResourceKind::Uniform);
    assert!(shader.groups[0].resources[2].access.is_none());
    assert!(shader.stages[0].threads.is_some());
}

#[test]
fn a_storage_element_type_is_an_array_of_the_written_element() {
    let parsed = clean(COMPUTE);
    let Some(Item::Shader(shader)) = parsed.tree.items.last() else {
        panic!("the last item is the shader");
    };
    let ty = parsed.tree.type_ref(shader.groups[0].resources[0].ty);
    let TypeRef::Array { element, .. } = ty else {
        panic!("`[Float]` is an array, got {ty:?}");
    };
    let TypeRef::Named { path, .. } = parsed.tree.type_ref(*element) else {
        panic!("its element is a named type");
    };
    assert_eq!(parsed.interner.resolve(path[0]), "Float");
}

#[test]
fn an_else_if_chain_nests_rather_than_flattening() {
    let parsed = clean(COMPUTE);
    // `if a {} else if b {} else {}` is two `If`s: the outer one's else slot
    // holds the inner `If`, and the inner one's holds a plain `Block`. Both
    // shapes have to be present, and an `else if` flattened into one node
    // would leave only one of them.
    let elses: Vec<_> = parsed
        .tree
        .stmts
        .iter()
        .filter_map(|(_, stmt)| match stmt {
            Stmt::If {
                otherwise: Some(otherwise),
                ..
            } => Some(parsed.tree.stmt(*otherwise)),
            _ => None,
        })
        .collect();
    assert!(
        elses.iter().any(|stmt| matches!(stmt, Stmt::If { .. })),
        "an `else if` keeps its own `If`: {elses:?}"
    );
    assert!(
        elses.iter().any(|stmt| matches!(stmt, Stmt::Block(_))),
        "the final `else` is a plain block: {elses:?}"
    );
}

#[test]
fn an_import_alias_and_a_qualified_call_both_survive() {
    let parsed = clean(IMPORTS);
    let Some(Item::Import(import)) = parsed.tree.items.first() else {
        panic!("the first item is the import");
    };
    assert_eq!(import.path.len(), 2);
    assert_eq!(parsed.interner.resolve(import.path[0]), "Common");
    assert_eq!(parsed.interner.resolve(import.path[1]), "Lighting");
    assert_eq!(
        import.alias.map(|alias| parsed.interner.resolve(alias)),
        Some("Lighting")
    );
}

#[test]
fn the_words_the_corpus_uses_as_names_stay_usable_as_names() {
    // `sample`, `texture`, `input`, and `output` are all positional words; a
    // shader that binds them as values must still parse.
    clean(
        "function f() -> Float {\n    let sample = 1.0\n    let input = sample\n    let texture = \
         input\n    let output = texture\n    return output\n}\n",
    );
}

#[test]
fn precedence_groups_the_way_c_does() {
    let parsed = clean("function f() -> Float {\n    return 1.0 + 2.0 * 3.0\n}\n");
    let top = parsed
        .tree
        .exprs
        .iter()
        .find_map(|(id, expr)| match expr {
            Expr::Binary {
                op: BinaryOp::Add, ..
            } => Some(id),
            _ => None,
        })
        .expect("an addition");
    let Expr::Binary { rhs, .. } = parsed.tree.expr(top) else {
        panic!("an addition");
    };
    assert!(
        matches!(
            parsed.tree.expr(*rhs),
            Expr::Binary {
                op: BinaryOp::Mul,
                ..
            }
        ),
        "the multiplication binds tighter"
    );
}

#[test]
fn subtraction_is_left_associative() {
    let parsed = clean("function f() -> Float {\n    return 9.0 - 3.0 - 2.0\n}\n");
    let top = parsed
        .tree
        .exprs
        .iter()
        .filter_map(|(id, expr)| match expr {
            Expr::Binary {
                op: BinaryOp::Sub, ..
            } => Some(id),
            _ => None,
        })
        .next_back()
        .expect("a subtraction");
    let Expr::Binary { lhs, .. } = parsed.tree.expr(top) else {
        panic!("a subtraction");
    };
    assert!(
        matches!(
            parsed.tree.expr(*lhs),
            Expr::Binary {
                op: BinaryOp::Sub,
                ..
            }
        ),
        "`(9 - 3) - 2`, not `9 - (3 - 2)`"
    );
}

#[test]
fn a_malformed_field_costs_its_own_line_and_nothing_after_it() {
    let text = "type T {\n    let a: \n    let b: Float\n}\n\ntype U {\n    let c: Float\n}\n";
    let parsed = parse(SourceId::new(0), text);
    assert!(!parsed.diagnostics.is_empty());
    // Both `type` declarations still reached the tree.
    assert_eq!(parsed.tree.items.len(), 2);
}

#[test]
fn a_bad_access_mode_is_named_rather_than_guessed() {
    assert!(
        codes("shader S {\n    group G {\n        storage sideways x: [Float]\n    }\n}\n")
            .contains(&"KSLP004")
    );
}

#[test]
fn an_unknown_annotation_is_reported_by_name() {
    assert!(codes("type T {\n    @nonsense(x)\n    let a: Float\n}\n").contains(&"KSLP005"));
}

#[test]
fn threads_needs_exactly_three_extents() {
    assert!(
        codes("shader S {\n    compute {\n        threads(8, 1)\n    }\n}\n").contains(&"KSLP007")
    );
}

#[test]
fn junk_at_the_top_level_is_reported_once_and_the_parse_continues() {
    let text = "$$$\n\ntype T {\n    let a: Float\n}\n";
    let parsed = parse(SourceId::new(0), text);
    assert!(
        parsed.diagnostics.iter().any(|d| d.code == Some("KSLP002")),
        "{:?}",
        parsed.diagnostics
    );
    assert_eq!(parsed.tree.items.len(), 1);
}

#[test]
fn a_file_of_only_junk_terminates() {
    // The progress guard is what makes this finish rather than spin.
    let parsed = parse(SourceId::new(0), "} } ) ] , . @ @ @");
    assert!(!parsed.diagnostics.is_empty());
    assert!(parsed.tree.items.is_empty());
}

#[test]
fn an_empty_file_parses_to_an_empty_tree_with_nothing_reported() {
    let parsed = clean("");
    assert!(parsed.tree.items.is_empty());
}
