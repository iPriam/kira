//! What the emitted GLSL must say, and what it must refuse.

use kira_ksl_semantics::{Module, check};
use kira_shader_ir::{ShaderIr, lower};
use kira_shader_model::{BackendTarget, Stage};
use kira_source::SourceId;

use crate::{GlslError, emit};

/// Parses, checks, and lowers `text` for OpenGL.
fn build(text: &str) -> ShaderIr {
    let parsed = kira_ksl_parser::parse(SourceId::new(0), text);
    assert!(parsed.is_clean(), "{:?}", parsed.diagnostics);
    let checked = check(
        &Module {
            source: SourceId::new(0),
            tree: parsed.tree,
            interner: parsed.interner,
        },
        &[],
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
    lower(checked.module, BackendTarget::Glsl330)
}

const TEXTURED: &str = r#"
type Camera {
    let view_projection: Float4x4
}

type VIn {
    let position: Float3
    let uv: Float2
}

type VOut {
    @builtin(position)
    let clip_position: Float4
    let uv: Float2
}

type FOut {
    let color: Float4
}

shader Textured {
    group Frame {
        uniform camera: Camera
    }

    group Material {
        texture albedo: Texture2d
        sampler linear: Sampler
    }

    vertex {
        input VIn
        output VOut
        function entry(v: VIn) -> VOut {
            let r: VOut
            r.clip_position = mul(camera.view_projection, Float4(v.position, 1.0))
            r.uv = v.uv
            return r
        }
    }

    fragment {
        input VOut
        output FOut
        function entry(f: VOut) -> FOut {
            let r: FOut
            r.color = sample(albedo, linear, f.uv)
            return r
        }
    }
}
"#;

#[test]
fn each_stage_opens_with_the_version_and_is_its_own_module() {
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex).expect("emits");
    let fragment = emit(&ir, Stage::Fragment).expect("emits");
    assert!(vertex.starts_with("#version 330 core\n"), "{vertex}");
    assert!(fragment.starts_with("#version 330 core\n"), "{fragment}");
    assert!(vertex.contains("void main()"), "{vertex}");
}

#[test]
fn a_vertex_input_is_a_located_attribute_and_a_varying_links_by_name() {
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex).expect("emits");
    let fragment = emit(&ir, Stage::Fragment).expect("emits");
    assert!(
        vertex.contains("layout(location = 0) in vec3 position;"),
        "{vertex}"
    );
    // The vertex writes `v_uv` and the fragment reads `v_uv`: GLSL 330 links
    // varyings by name, so the two spellings have to match exactly.
    assert!(vertex.contains("out vec2 v_uv;"), "{vertex}");
    assert!(fragment.contains("in vec2 v_uv;"), "{fragment}");
}

#[test]
fn a_position_builtin_becomes_gl_position_rather_than_a_varying() {
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex).expect("emits");
    assert!(
        vertex.contains("gl_Position = r.clip_position;"),
        "{vertex}"
    );
    assert!(!vertex.contains("v_clip_position"), "{vertex}");
}

#[test]
fn the_same_position_builtin_is_gl_fragcoord_where_a_fragment_stage_reads_it() {
    // `gl_Position` is not declared in a fragment shader at all, so a stage
    // that named it there did not compile — which is what glslang reported
    // about this backend's output before the stage was taken into account.
    let ir = build(TEXTURED);
    let fragment = emit(&ir, Stage::Fragment).expect("emits");
    assert!(
        fragment.contains("f.clip_position = gl_FragCoord;"),
        "{fragment}"
    );
    assert!(!fragment.contains("gl_Position"), "{fragment}");
}

#[test]
fn a_fragment_output_is_a_located_out_variable() {
    let ir = build(TEXTURED);
    let fragment = emit(&ir, Stage::Fragment).expect("emits");
    assert!(
        fragment.contains("layout(location = 0) out vec4 color;"),
        "{fragment}"
    );
    assert!(fragment.contains("color = r.color;"), "{fragment}");
}

#[test]
fn a_uniform_becomes_a_std140_block_the_body_still_reads_through() {
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex).expect("emits");
    assert!(
        vertex.contains("layout(std140) uniform Camera_block {"),
        "{vertex}"
    );
    assert!(vertex.contains("} camera;"), "{vertex}");
    assert!(vertex.contains("camera.view_projection"), "{vertex}");
}

#[test]
fn a_texture_and_its_sampler_collapse_into_one_uniform() {
    // GLSL 330 has no standalone sampler object, so the sampler never gets a
    // declaration and the argument disappears at the call.
    let ir = build(TEXTURED);
    let fragment = emit(&ir, Stage::Fragment).expect("emits");
    assert!(fragment.contains("uniform sampler2D albedo;"), "{fragment}");
    assert!(!fragment.contains("uniform  linear"), "{fragment}");
    assert!(fragment.contains("texture(albedo, f.uv)"), "{fragment}");
}

#[test]
fn the_entry_rebuilds_its_input_struct_from_the_loose_variables() {
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex).expect("emits");
    assert!(vertex.contains("VIn v;"), "{vertex}");
    assert!(vertex.contains("v.position = position;"), "{vertex}");
}

#[test]
fn a_compute_shader_is_refused_by_name_rather_than_emitted() {
    // Compute arrived in GLSL 430. Emitting something that would not link, or
    // silently dropping the stage, would both be worse than saying so.
    let ir = build(
        r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}
shader Step {
    compute {
        input QIn
        threads(64, 1, 1)
        function entry(q: QIn) {
            return
        }
    }
}
"#,
    );
    assert!(matches!(
        emit(&ir, Stage::Compute),
        Err(GlslError::ComputeStage { .. })
    ));
}

#[test]
fn a_storage_buffer_is_refused_by_name() {
    // The corpus builds its GPU simulation steps on storage buffers; dropping
    // them would leave a shader that compiles and computes nothing.
    let ir = build(
        r#"
type VOut {
    @builtin(position)
    let clip_position: Float4
}
shader S {
    group G {
        storage read page: [Float]
    }
    vertex {
        output VOut
        function entry() -> VOut {
            let r: VOut
            r.clip_position = Float4(page[0], 0.0, 0.0, 1.0)
            return r
        }
    }
}
"#,
    );
    let refused = emit(&ir, Stage::Vertex).expect_err("refuses");
    assert!(matches!(refused, GlslError::StorageBuffer { .. }));
    assert!(refused.to_string().contains("page"), "{refused}");
}

#[test]
fn a_stage_the_shader_does_not_declare_is_empty_rather_than_an_error() {
    let ir = build(TEXTURED);
    assert_eq!(emit(&ir, Stage::Compute).expect("no such stage"), "");
}
