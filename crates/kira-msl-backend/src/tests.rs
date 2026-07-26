//! What the emitted MSL must say, checked end to end from KSL source.

use kira_ksl_semantics::{Module, check};
use kira_shader_ir::{ShaderIr, lower};
use kira_shader_model::BackendTarget;
use kira_source::SourceId;

use crate::emit;

/// Parses, checks, and lowers `text` for Metal.
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
            .map(|d| (d.code, d.message.clone()))
            .collect::<Vec<_>>()
    );
    lower(checked.module, BackendTarget::Msl)
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
fn the_module_opens_with_the_metal_prelude() {
    let msl = emit(&build(TEXTURED));
    assert!(
        msl.starts_with("#include <metal_stdlib>\nusing namespace metal;\n"),
        "{msl}"
    );
}

#[test]
fn both_stages_land_in_one_module() {
    // Metal compiles one source into a `.metallib` and the pipeline names its
    // functions out of it, so the stages cannot be split.
    let msl = emit(&build(TEXTURED));
    assert!(msl.contains("vertex vertex_VOut_out vertex_main("), "{msl}");
    assert!(
        msl.contains("fragment fragment_FOut_out fragment_main("),
        "{msl}"
    );
}

#[test]
fn a_vertex_input_takes_attribute_slots_and_an_output_takes_user_locations() {
    let msl = emit(&build(TEXTURED));
    assert!(msl.contains("float3 position [[attribute(0)]];"), "{msl}");
    assert!(msl.contains("float2 uv [[attribute(1)]];"), "{msl}");
    assert!(msl.contains("float4 clip_position [[position]];"), "{msl}");
    assert!(msl.contains("float2 uv [[user(loc0)]];"), "{msl}");
}

#[test]
fn a_fragment_output_takes_a_colour_attachment() {
    let msl = emit(&build(TEXTURED));
    assert!(msl.contains("float4 color [[color(0)]];"), "{msl}");
}

#[test]
fn resources_bind_at_the_slots_lowering_assigned() {
    let msl = emit(&build(TEXTURED));
    assert!(
        msl.contains("constant Camera& camera [[buffer(1)]]"),
        "{msl}"
    );
    assert!(
        msl.contains("texture2d<float> albedo [[texture(0)]]"),
        "{msl}"
    );
    assert!(msl.contains("sampler linear [[sampler(0)]]"), "{msl}");
}

#[test]
fn mul_becomes_the_operator_because_metal_matrices_multiply_on_the_left() {
    let msl = emit(&build(TEXTURED));
    assert!(msl.contains("(camera.view_projection * float4("), "{msl}");
}

#[test]
fn sample_becomes_a_method_on_the_texture() {
    let msl = emit(&build(TEXTURED));
    assert!(msl.contains("albedo.sample(linear, f.uv)"), "{msl}");
}

#[test]
fn a_kernel_takes_its_builtins_as_loose_parameters_and_rebuilds_the_struct() {
    // There is no `[[stage_in]]` for a compute function, so the entry point
    // takes the builtins directly and the body's input value is rebuilt.
    let msl = emit(&build(COMPUTE));
    assert!(
        msl.contains("kernel void compute_main(uint3 gid [[thread_position_in_grid]]"),
        "{msl}"
    );
    assert!(msl.contains("QIn q = { gid };"), "{msl}");
}

#[test]
fn a_read_write_array_binds_as_a_device_pointer() {
    let msl = emit(&build(COMPUTE));
    assert!(
        msl.contains("device Particle* particles [[buffer(1)]]"),
        "{msl}"
    );
}

#[test]
fn an_array_length_is_read_from_the_buffer_the_host_binds() {
    // Metal has no shader-side buffer-length intrinsic, unlike the other four
    // dialects, so the count arrives as its own buffer after every other.
    let msl = emit(&build(COMPUTE));
    assert!(
        msl.contains("constant uint& particles_count [[buffer(2)]]"),
        "{msl}"
    );
    assert!(msl.contains("particles_count"), "{msl}");
}

#[test]
fn a_let_without_an_initializer_is_zeroed_rather_than_left_undefined() {
    // A stage entry point builds its output field by field; reading one it
    // never wrote must not be garbage.
    let msl = emit(&build(TEXTURED));
    assert!(msl.contains("vertex_VOut_out r = {};"), "{msl}");
}

#[test]
fn a_float_literal_keeps_its_point_so_division_does_not_truncate() {
    let msl = emit(&build(COMPUTE));
    assert!(msl.contains("1.0"), "{msl}");
    assert!(!msl.contains("+ 1)"), "an integer 1 would truncate: {msl}");
}

#[test]
fn a_file_with_no_shader_emits_nothing() {
    let ir = build("function f(x: Float) -> Float {\n    return x\n}\n");
    assert_eq!(emit(&ir), "");
}
