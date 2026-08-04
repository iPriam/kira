//! What the emitted HLSL must say, checked end to end from KSL source.

use kira_ksl_semantics::{Module, check};
use kira_shader_ir::{ShaderIr, lower};
use kira_shader_model::{BackendTarget, Stage};
use kira_source::SourceId;

use crate::{HlslError, emit};

/// Parses, checks, and lowers `text` for D3D.
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
    lower(checked.module, BackendTarget::Hlsl)
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
    @interpolate(flat)
    let id: UInt
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
        sampler albedo_sampler: Sampler
    }

    vertex {
        input VIn
        output VOut
        function entry(v: VIn) -> VOut {
            let r: VOut
            r.clip_position = mul(camera.view_projection, Float4(v.position, 1.0))
            r.uv = v.uv
            r.id = 0
            return r
        }
    }

    fragment {
        input VOut
        output FOut
        function entry(f: VOut) -> FOut {
            let r: FOut
            r.color = sample(albedo, albedo_sampler, f.uv)
            return r
        }
    }
}
"#;

#[test]
fn a_uniform_is_a_constant_buffer_holding_the_struct_the_body_reads() {
    let hlsl = emit(&build(TEXTURED), Stage::Vertex).expect("vertex");
    assert!(
        hlsl.contains("cbuffer camera_buffer : register(b0) {"),
        "{hlsl}"
    );
    // The struct itself sits inside, so the body's path is the one KSL wrote.
    assert!(hlsl.contains("Camera camera;"), "{hlsl}");
    assert!(hlsl.contains("mul(camera.view_projection,"), "{hlsl}");
}

#[test]
fn a_matrix_is_stored_by_columns_because_the_host_packs_it_that_way() {
    // HLSL's default is rows. A matrix declared without this reads the host's
    // bytes transposed, which is a wrong image rather than a compile error —
    // the whole reason the qualifier is emitted.
    let hlsl = emit(&build(TEXTURED), Stage::Vertex).expect("vertex");
    assert!(
        hlsl.contains("column_major float4x4 view_projection;"),
        "{hlsl}"
    );
}

#[test]
fn a_matrix_spelling_transposes_because_hlsl_writes_rows_first() {
    // Every matrix KSL spells today is square, where the two orders agree. This
    // pins the rule anyway: the spelling is rows-first, so the day a
    // non-square one lands it is not silently emitted as its own transpose.
    use kira_shader_model::{MatrixType, Type};
    assert_eq!(
        crate::type_name(&Type::Matrix(MatrixType {
            columns: 4,
            rows: 3
        })),
        "float3x4"
    );
    assert_eq!(
        crate::type_name(&Type::Matrix(MatrixType {
            columns: 4,
            rows: 4
        })),
        "float4x4"
    );
}

#[test]
fn each_stage_gets_its_own_copy_of_an_interface_struct() {
    // One KSL struct is a vertex output and a fragment input, and the semantics
    // differ: the same declaration cannot carry both.
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex).expect("vertex");
    let fragment = emit(&ir, Stage::Fragment).expect("fragment");
    assert!(vertex.contains("struct vs_VOut_out {"), "{vertex}");
    assert!(fragment.contains("struct ps_VOut_in {"), "{fragment}");
    // And the body says this stage's name for it, or the entry would return a
    // struct carrying no semantics at all.
    assert!(vertex.contains("vs_VOut_out vertex_main("), "{vertex}");
    assert!(
        vertex.contains("vs_VOut_out r = (vs_VOut_out)0;"),
        "{vertex}"
    );
}

#[test]
fn an_interface_member_carries_a_semantic_by_what_it_is() {
    let ir = build(TEXTURED);
    let vertex = emit(&ir, Stage::Vertex).expect("vertex");
    let fragment = emit(&ir, Stage::Fragment).expect("fragment");
    assert!(
        vertex.contains("float4 clip_position : SV_Position;"),
        "{vertex}"
    );
    assert!(vertex.contains("float2 uv : TEXCOORD0;"), "{vertex}");
    // A varying that cannot be interpolated says so, or D3D interpolates it.
    assert!(
        vertex.contains("nointerpolation uint id : TEXCOORD1;"),
        "{vertex}"
    );
    // A fragment output is a colour attachment, not a varying.
    assert!(
        fragment.contains("float4 color : SV_Target0;"),
        "{fragment}"
    );
}

#[test]
fn a_texture_and_its_sampler_take_registers_of_their_own_kinds() {
    let hlsl = emit(&build(TEXTURED), Stage::Fragment).expect("fragment");
    assert!(
        hlsl.contains("Texture2D<float4> albedo : register(t0);"),
        "{hlsl}"
    );
    assert!(
        hlsl.contains("SamplerState albedo_sampler : register(s0);"),
        "{hlsl}"
    );
    assert!(
        hlsl.contains("albedo.Sample(albedo_sampler, f.uv)"),
        "{hlsl}"
    );
}

const COMPUTE: &str = r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}

type Particle {
    let px: Float
    let vy: Float
}

shader Step {
    group Work {
        storage read_write particles: [Particle]
    }
    compute {
        input QIn
        threads(64, 1, 1)
        function entry(q: QIn) {
            if q.gid.x < particles.count {
                particles[q.gid.x].px = particles[q.gid.x].px + 1.0
            }
            return
        }
    }
}
"#;

#[test]
fn a_compute_entry_declares_its_thread_group_and_takes_builtins_directly() {
    let hlsl = emit(&build(COMPUTE), Stage::Compute).expect("compute");
    assert!(hlsl.contains("[numthreads(64, 1, 1)]"), "{hlsl}");
    assert!(
        hlsl.contains("void compute_main(uint3 gid : SV_DispatchThreadID)"),
        "{hlsl}"
    );
    // HLSL has no interface struct for a kernel, so the body's value is rebuilt
    // from the loose builtins.
    assert!(hlsl.contains("QIn q = (QIn)0;"), "{hlsl}");
    assert!(hlsl.contains("q.gid = gid;"), "{hlsl}");
}

#[test]
fn a_read_write_buffer_is_a_uav_and_a_read_only_one_is_not() {
    let hlsl = emit(&build(COMPUTE), Stage::Compute).expect("compute");
    assert!(
        hlsl.contains("RWStructuredBuffer<Particle> particles : register(u0);"),
        "{hlsl}"
    );
    let read_only = build(
        r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}
shader S {
    group Work {
        storage read page: [Float]
    }
    compute {
        input QIn
        threads(1, 1, 1)
        function entry(q: QIn) {
            let v = page[q.gid.x]
            return
        }
    }
}
"#,
    );
    let hlsl = emit(&read_only, Stage::Compute).expect("compute");
    assert!(
        hlsl.contains("StructuredBuffer<float> page : register(t0);"),
        "{hlsl}"
    );
}

#[test]
fn a_buffers_length_is_read_into_a_temporary_before_the_statement_wanting_it() {
    // `GetDimensions` answers through `out` parameters, so there is no
    // expression form of it to write inline.
    let hlsl = emit(&build(COMPUTE), Stage::Compute).expect("compute");
    assert!(
        hlsl.contains("particles.GetDimensions(kira_temporary_0, kira_temporary_1);"),
        "{hlsl}"
    );
    assert!(hlsl.contains("if ((q.gid.x < kira_temporary_0))"), "{hlsl}");
}

#[test]
fn an_atomic_answers_through_a_temporary_because_hlsl_has_no_expression_for_it() {
    // The corpus writes `let slot = atomicAdd(…)` and reads the value that was
    // there, which `InterlockedAdd` gives up only through an `out` parameter.
    let ir = build(
        r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}
shader S {
    group Work {
        storage read_write counter: [UInt]
    }
    compute {
        input QIn
        threads(1, 1, 1)
        function entry(q: QIn) {
            let zero: UInt = 0
            let one: UInt = 1
            let slot = atomicAdd(counter, zero, one)
            counter[slot] = q.gid.x
            return
        }
    }
}
"#,
    );
    let hlsl = emit(&ir, Stage::Compute).expect("compute");
    assert!(
        hlsl.contains("InterlockedAdd(counter[zero], one, kira_temporary_0);"),
        "{hlsl}"
    );
    assert!(hlsl.contains("uint slot = kira_temporary_0;"), "{hlsl}");
}

#[test]
fn a_statement_only_call_in_a_loop_condition_is_refused_by_name() {
    // Hoisting it would evaluate it once and let every later iteration read the
    // stale temporary — a wrong answer rather than a compile error, so this is
    // refused instead of emitted.
    let ir = build(
        r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}
shader S {
    group Work {
        storage read_write counter: [UInt]
    }
    compute {
        input QIn
        threads(1, 1, 1)
        function entry(q: QIn) {
            let i: UInt = 0
            while i < counter.count {
                i = i + 1
            }
            return
        }
    }
}
"#,
    );
    let error = emit(&ir, Stage::Compute).expect_err("a loop condition");
    assert_eq!(
        error,
        HlslError::StatementOnlyInLoopCondition {
            shader: "S".to_owned(),
            call: "length".to_owned(),
        }
    );
}

#[test]
fn a_stage_the_shader_does_not_declare_emits_nothing() {
    let hlsl = emit(&build(COMPUTE), Stage::Vertex).expect("no vertex stage");
    assert!(hlsl.is_empty(), "{hlsl}");
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

/// A `texture write` is unordered access, in the `u` register space.
///
/// Emitting it as `t` compiles and binds a read-only view a write cannot use,
/// so the register letter is the assertion that matters here.
#[test]
fn a_write_texture_is_declared_read_write_in_the_u_space() {
    let ir = build(STORAGE_TEXTURE);
    let compute = emit(&ir, Stage::Compute).expect("compute");
    assert!(
        compute.contains("RWTexture2D<float4> result : register(u"),
        "{compute}"
    );
    assert!(compute.contains("result["), "{compute}");
    // The sampled binding beside it stays in `t`.
    assert!(
        compute.contains("Texture2D<float4> source : register(t"),
        "{compute}"
    );
}
