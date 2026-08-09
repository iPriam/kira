//! What the emitted GLSL must say, and what it must refuse.

use kira_ksl_semantics::{Module, check};
use kira_shader_ir::{ShaderIr, lower};
use kira_shader_model::{BackendTarget, Stage};
use kira_source::SourceId;

use crate::emit;

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
    lower(checked.module, BackendTarget::Glsl430)
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
    let vertex = emit(&ir, Stage::Vertex);
    let fragment = emit(&ir, Stage::Fragment);
    assert!(vertex.starts_with("#version 430 core\n"), "{vertex}");
    assert!(fragment.starts_with("#version 430 core\n"), "{fragment}");
    assert!(vertex.contains("void main()"), "{vertex}");
}

#[test]
fn a_vertex_input_is_a_located_attribute_and_a_varying_links_by_name() {
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex);
    let fragment = emit(&ir, Stage::Fragment);
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
    let vertex = emit(&ir, Stage::Vertex);
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
    let fragment = emit(&ir, Stage::Fragment);
    assert!(
        fragment.contains("f.clip_position = gl_FragCoord;"),
        "{fragment}"
    );
    assert!(!fragment.contains("gl_Position"), "{fragment}");
}

#[test]
fn a_fragment_output_is_a_located_out_variable() {
    let ir = build(TEXTURED);
    let fragment = emit(&ir, Stage::Fragment);
    assert!(
        fragment.contains("layout(location = 0) out vec4 color;"),
        "{fragment}"
    );
    assert!(fragment.contains("color = r.color;"), "{fragment}");
}

#[test]
fn a_uniform_is_a_struct_uniform_whose_members_have_locations() {
    // Not an interface block. A GL host addresses a uniform by the dotted name
    // `glGetUniformLocation` finds, and a block has no such name — it is written
    // through a buffer object, which the hosts consuming this do not have. A
    // block therefore compiles and then reads zero at every draw.
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex);
    assert!(vertex.contains("struct Camera {"), "{vertex}");
    assert!(vertex.contains("uniform Camera camera;"), "{vertex}");
    assert!(!vertex.contains("uniform Camera_block"), "{vertex}");
    assert!(vertex.contains("camera.view_projection"), "{vertex}");
}

#[test]
fn a_texture_and_its_sampler_collapse_into_one_uniform() {
    // GLSL 330 has no standalone sampler object, so the sampler never gets a
    // declaration and the argument disappears at the call.
    let ir = build(TEXTURED);
    let fragment = emit(&ir, Stage::Fragment);
    assert!(fragment.contains("uniform sampler2D albedo;"), "{fragment}");
    assert!(!fragment.contains("uniform  linear"), "{fragment}");
    assert!(fragment.contains("texture(albedo, f.uv)"), "{fragment}");
}

#[test]
fn the_entry_rebuilds_its_input_struct_from_the_loose_variables() {
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex);
    assert!(vertex.contains("VIn v;"), "{vertex}");
    assert!(vertex.contains("v.position = position;"), "{vertex}");
}

#[test]
fn a_compute_shader_declares_its_workgroup_size() {
    // Compute arrived in GLSL 430, which is the version this backend emits.
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
    let compute = emit(&ir, Stage::Compute);
    assert!(
        compute.contains("layout(local_size_x = 64, local_size_y = 1, local_size_z = 1) in;"),
        "{compute}"
    );
    assert!(compute.contains("#version 430 core"), "{compute}");
}

/// A written texture is an image, and `store` reaches it through `imageStore`.
///
/// A `sampler2D` cannot be written at all, and `store` had no GLSL spelling: it
/// fell through to the unnamed builtin and emitted `(result, gid.xy, value)` —
/// a comma expression that compiles, evaluates its arguments, and stores
/// nothing. Every KSL compute shader reached a GL driver with its writes gone.
#[test]
fn a_written_texture_becomes_an_image_and_store_becomes_imagestore() {
    let ir = build(
        r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}
shader Write {
    group G {
        texture write result: Texture2d
    }
    compute {
        input QIn
        threads(8, 8, 1)
        function entry(q: QIn) {
            store(result, q.gid.xy, Float4(1.0, 0.0, 0.0, 1.0))
            return
        }
    }
}
"#,
    );
    let compute = emit(&ir, Stage::Compute);
    assert!(
        compute.contains("uniform writeonly image2D result;"),
        "{compute}"
    );
    assert!(compute.contains("layout(binding = "), "{compute}");
    assert!(compute.contains("rgba8)"), "{compute}");
    assert!(
        compute.contains("imageStore(result, ivec2(q.gid.xy), vec4(1.0, 0.0, 0.0, 1.0));"),
        "{compute}"
    );
    assert!(!compute.contains("sampler2D result"), "{compute}");
}

/// A uniform's unsigned members are declared signed, and only a uniform's.
///
/// A GL host writes an integral uniform through `glUniform*iv`, which a `uint`
/// uniform refuses as a type mismatch — so `uint` there is a uniform nothing
/// can ever write. Every other unsigned declaration keeps its type: the
/// narrowing is the loading call's, not the language's.
#[test]
fn an_unsigned_uniform_member_is_declared_signed() {
    let ir = build(
        r#"
type Extent {
    let width: UInt
    let size: UInt2
    let scale: Float
}
type Counters {
    let hits: UInt
}
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}
shader Tally {
    group G {
        uniform extent: Extent
        storage read_write counters: [Counters]
    }
    compute {
        input QIn
        threads(8, 8, 1)
        function entry(q: QIn) {
            if q.gid.x >= extent.width {
                return
            }
            counters[q.gid.x].hits = extent.size.x
            return
        }
    }
}
"#,
    );
    let compute = emit(&ir, Stage::Compute);
    assert!(
        compute.contains("struct Extent {\n    int width;"),
        "{compute}"
    );
    assert!(compute.contains("ivec2 size;"), "{compute}");
    // The float member is untouched, and a struct that is not a uniform keeps
    // every unsigned member it declared.
    assert!(compute.contains("float scale;"), "{compute}");
    assert!(
        compute.contains("struct Counters {\n    uint hits;"),
        "{compute}"
    );
}

/// A storage binding becomes a `std430` block holding an unsized array.
///
/// The corpus builds its GPU simulation steps on storage buffers, and the
/// unsized trailing array is what makes the length a run-time property — which
/// is the whole reason a storage buffer is not a uniform block.
#[test]
fn a_storage_buffer_becomes_a_std430_block() {
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
    let vertex = emit(&ir, Stage::Vertex);
    assert!(vertex.contains("buffer page_block {"), "{vertex}");
    assert!(vertex.contains("float page[];"), "{vertex}");
    assert!(vertex.contains("readonly"), "{vertex}");
}

#[test]
fn a_helper_taking_a_sampler_drops_it_from_the_signature_and_the_call() {
    // GLSL has no standalone sampler object, so there is no type to spell the
    // parameter with: it emitted as `sampler2D src,  smp` — a parameter with an
    // empty type — and the module did not compile. The texture already carries
    // the sampling state, which is why `sample` drops the argument too.
    let ir = build(
        r#"
type VOut {
    @builtin(position)
    let clip_position: Float4
    let uv: Float2
}
type FOut {
    let color: Float4
}
function tap(src: Texture2d, smp: Sampler, uv: Float2) -> Float4 {
    return sample(src, smp, uv)
}
shader Helper {
    group G {
        texture albedo: Texture2d
        sampler linear: Sampler
    }
    fragment {
        input VOut
        output FOut
        function entry(f: VOut) -> FOut {
            let r: FOut
            r.color = tap(albedo, linear, f.uv)
            return r
        }
    }
}
"#,
    );
    let fragment = emit(&ir, Stage::Fragment);
    assert!(
        fragment.contains("vec4 tap(sampler2D src, vec2 uv) {"),
        "{fragment}"
    );
    assert!(fragment.contains("tap(albedo, f.uv)"), "{fragment}");
}

#[test]
fn a_stage_declares_only_the_resources_it_reads() {
    // `albedo` is sampled in the fragment stage and named nowhere in the
    // vertex stage; `camera` is the other way round. Declaring either in the
    // stage that does not read it is not merely redundant: a GL driver strips
    // an unused global, so the host's `glGetUniformLocation` for it answers -1
    // and the host reports a shader/binding mismatch that is not one. This is
    // the same rule the MSL backend already applies to its parameter list.
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex);
    let fragment = emit(&ir, Stage::Fragment);
    assert!(!vertex.contains("sampler2D albedo"), "{vertex}");
    assert!(fragment.contains("sampler2D albedo"), "{fragment}");
    assert!(vertex.contains("uniform Camera camera;"), "{vertex}");
    assert!(!fragment.contains("uniform Camera camera;"), "{fragment}");
}

#[test]
fn a_stage_the_shader_does_not_declare_is_empty_rather_than_an_error() {
    let ir = build(TEXTURED);
    assert_eq!(emit(&ir, Stage::Compute), "");
}

#[test]
fn a_fragment_stage_measures_its_position_from_the_upper_left() {
    // KSL is one language across five targets, and `@builtin(position)` in a
    // fragment stage has to name the same pixel in all of them. Metal, D3D and
    // WebGPU measure from the upper left; GL measures from the lower left
    // unless the builtin is redeclared, which is what this emits. A shader
    // reading its own device position to address a full-screen texture — the
    // UI compositor's interior cache does exactly that — is off by a vertical
    // flip on one backend without it.
    let ir = build(TEXTURED);
    let fragment = emit(&ir, Stage::Fragment);
    let vertex = emit(&ir, Stage::Vertex);
    assert!(
        fragment.contains("layout(origin_upper_left) in vec4 gl_FragCoord;"),
        "{fragment}"
    );
    // `gl_Position` in a vertex stage is clip space, which has no origin to
    // agree about.
    assert!(!vertex.contains("origin_upper_left"), "{vertex}");
}
