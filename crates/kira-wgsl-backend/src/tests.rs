//! What the emitted WGSL must say, checked end to end from KSL source.

use kira_ksl_semantics::{Module, check};
use kira_shader_ir::{ShaderIr, lower};
use kira_shader_model::{BackendTarget, Stage};
use kira_source::SourceId;

use crate::emit;

/// Parses, checks, and lowers `text` for WebGPU.
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
    lower(checked.module, BackendTarget::Wgsl)
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

const COMPUTE: &str = r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}

shader Step {
    group Work {
        storage read_write out: [UInt]
        storage read page: [Float]
    }
    compute {
        input QIn
        threads(64, 1, 1)
        function entry(q: QIn) {
            if q.gid.x < out.count {
                out[q.gid.x] = 1
            }
            return
        }
    }
}
"#;

/// A shader whose texture and whose local are both WGSL reserved words. Both
/// spellings are ordinary KSL, and both are in the UI corpus.
const RESERVED: &str = r#"
type VIn {
    let position: Float2
}

type VOut {
    @builtin(position)
    let clip_position: Float4
}

type FOut {
    let color: Float4
}

shader Reserved {
    group Material {
        texture external: Texture2d
        sampler smp: Sampler
    }

    vertex {
        input VIn
        output VOut
        function entry(v: VIn) -> VOut {
            let ref: Float4
            ref = Float4(v.position, 0.0, 1.0)
            let r: VOut
            r.clip_position = ref
            return r
        }
    }

    fragment {
        input VOut
        output FOut
        function entry(f: VOut) -> FOut {
            let r: FOut
            r.color = sample(external, smp, Float2(0.0, 0.0))
            return r
        }
    }
}
"#;

#[test]
fn each_stage_is_its_own_module() {
    // A WebGPU pipeline creates and names the vertex and fragment modules
    // separately, so they cannot share one source.
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex);
    let fragment = emit(&ir, Stage::Fragment);
    assert!(vertex.contains("@vertex fn vertex_main("), "{vertex}");
    assert!(!vertex.contains("@fragment"), "{vertex}");
    assert!(
        fragment.contains("@fragment fn fragment_main("),
        "{fragment}"
    );
    assert!(!fragment.contains("@vertex"), "{fragment}");
}

#[test]
fn a_stage_the_shader_does_not_declare_emits_nothing() {
    let ir = build(TEXTURED);
    assert_eq!(emit(&ir, Stage::Compute), "");
}

#[test]
fn resources_are_declared_at_module_scope_with_their_group_and_binding() {
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex);
    assert!(
        vertex.contains("@group(0) @binding(0) var<uniform> camera: Camera;"),
        "{vertex}"
    );
    assert!(
        vertex.contains("@group(1) @binding(0) var albedo: texture_2d<f32>;"),
        "{vertex}"
    );
    assert!(
        vertex.contains("@group(1) @binding(1) var linear: sampler;"),
        "{vertex}"
    );
}

#[test]
fn an_interface_struct_carries_locations_and_builtins() {
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex);
    assert!(
        vertex.contains("@location(0) position: vec3<f32>,"),
        "{vertex}"
    );
    assert!(
        vertex.contains("@builtin(position) clip_position: vec4<f32>,"),
        "{vertex}"
    );
}

#[test]
fn a_local_is_a_var_because_wgsls_let_cannot_be_reassigned() {
    // KSL rebinds locals constantly; a WGSL `let` is immutable, so every one
    // has to become a `var` or the corpus would not compile.
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex);
    assert!(vertex.contains("var r: VOut = VOut();"), "{vertex}");
    assert!(!vertex.contains("let r"), "{vertex}");
}

#[test]
fn every_literal_carries_its_suffix() {
    // WGSL has no implicit numeric conversion, so a bare `1` beside a `u32` is
    // an error rather than a promotion.
    let ir = build(COMPUTE);
    let compute = emit(&ir, Stage::Compute);
    assert!(compute.contains("1u"), "{compute}");
}

/// A sample names its level, because the form that does not is unreachable.
///
/// `textureSample` computes its own derivatives and WGSL permits it only from
/// uniform control flow — a sample inside an `if` is refused, and the UI corpus
/// samples inside branches on every glass surface. There is one mip level to
/// read, so naming it costs nothing and compiles everywhere.
#[test]
fn a_sample_names_its_level_so_it_may_appear_inside_a_branch() {
    let ir = build(TEXTURED);
    let fragment = emit(&ir, Stage::Fragment);
    assert!(
        fragment.contains("textureSampleLevel(albedo, linear, f.uv, 0.0)"),
        "{fragment}"
    );
    assert!(!fragment.contains("textureSample("), "{fragment}");
}

/// A reserved word is prefixed on its way out, at every position it can appear.
#[test]
fn a_reserved_word_is_renamed_wherever_it_is_written() {
    let ir = build(RESERVED);
    let vertex = emit(&ir, Stage::Vertex);
    assert!(vertex.contains("var ksl_ref: vec4<f32>"), "{vertex}");
    let fragment = emit(&ir, Stage::Fragment);
    assert!(fragment.contains("var ksl_external:"), "{fragment}");
    assert!(
        fragment.contains("textureSampleLevel(ksl_external, smp,"),
        "{fragment}"
    );
}

#[test]
fn an_array_length_asks_the_binding_rather_than_the_host() {
    // Unlike Metal, WGSL can ask, so nothing extra is bound.
    let ir = build(COMPUTE);
    let compute = emit(&ir, Stage::Compute);
    assert!(compute.contains("arrayLength(&out)"), "{compute}");
    assert!(!compute.contains("out_count"), "{compute}");
}

#[test]
fn a_storage_binding_carries_its_access_mode() {
    let ir = build(COMPUTE);
    let compute = emit(&ir, Stage::Compute);
    assert!(
        compute.contains("var<storage, read_write> out: array<u32>;"),
        "{compute}"
    );
    assert!(
        compute.contains("var<storage, read> page: array<f32>;"),
        "{compute}"
    );
}

#[test]
fn a_compute_entry_declares_its_workgroup_size_and_takes_builtins_directly() {
    let ir = build(COMPUTE);
    let compute = emit(&ir, Stage::Compute);
    assert!(
        compute.contains("@compute @workgroup_size(64, 1, 1) fn compute_main("),
        "{compute}"
    );
    assert!(
        compute.contains("@builtin(global_invocation_id) gid: vec3<u32>"),
        "{compute}"
    );
    assert!(compute.contains("var q: QIn = QIn(gid);"), "{compute}");
}

#[test]
fn a_buffer_an_atomic_names_is_declared_atomic_and_read_through_atomic_ops() {
    // WGSL has no atomic operation on an ordinary integer: `atomicAdd` over an
    // `array<u32>` is rejected outright, which is what naga reported about this
    // backend's output before the element type followed the use.
    let ir = build(
        r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}
shader S {
    group Work {
        storage read_write counter: [UInt]
        storage read_write other: [UInt]
    }
    compute {
        input QIn
        threads(1, 1, 1)
        function entry(q: QIn) {
            let zero: UInt = 0
            let one: UInt = 1
            let slot = atomicAdd(counter, zero, one)
            counter[slot] = q.gid.x
            other[zero] = one
            return
        }
    }
}
"#,
    );
    let compute = emit(&ir, Stage::Compute);
    assert!(
        compute.contains("var<storage, read_write> counter: array<atomic<u32>>;"),
        "{compute}"
    );
    // An ordinary access to that same buffer has to follow the element type.
    assert!(
        compute.contains("atomicStore(&counter[slot], q.gid.x);"),
        "{compute}"
    );
    // And a buffer no atomic names stays a plain array, read and written
    // plainly — the spelling follows the use, not the shader.
    assert!(
        compute.contains("var<storage, read_write> other: array<u32>;"),
        "{compute}"
    );
    assert!(compute.contains("other[zero] = one;"), "{compute}");
}
const STORAGE_TEXTURE: &str = r#"
type Q {
    @builtin(thread_id)
    let gid: UInt3
}

shader Writer {
    group Work {
        texture source: Texture2d
        sampler linear: Sampler
        texture write result: Texture2d
    }

    compute {
        input Q
        threads(8, 8, 1)

        function entry(q: Q) {
            let colour = sample(source, linear, Float2(0.5, 0.5))
            store(result, q.gid.xy, colour)
            return
        }
    }
}
"#;

/// A `texture write` is a storage binding, not a sampled one.
///
/// Emitting it as sampled compiles just as happily and binds a read-only view a
/// write cannot use, so what this pins is the *declaration* rather than the
/// fact that the shader came out at all.
#[test]
fn a_write_texture_is_declared_as_storage_and_written_through() {
    let ir = build(STORAGE_TEXTURE);
    let compute = emit(&ir, Stage::Compute);
    assert!(
        compute.contains("var result: texture_storage_2d<rgba8unorm, write>;"),
        "{compute}"
    );
    assert!(compute.contains("textureStore(result,"), "{compute}");
    // The sampled binding beside it keeps its own kind.
    assert!(
        compute.contains("var source: texture_2d<f32>;"),
        "{compute}"
    );
}
