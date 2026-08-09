//! What lowering decides, checked end to end from KSL source.

use kira_ksl_semantics::{Module, check};
use kira_shader_model::{BackendTarget, Builtin, ResourceKind, Stage};
use kira_source::SourceId;

use crate::{ShaderIr, decode, lower};

/// Parses, checks, and lowers `text` for `target`.
fn build(text: &str, target: BackendTarget) -> ShaderIr {
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
    lower(checked.module, target)
}

const TEXTURED: &str = r#"
type Camera {
    let view_projection: Float4x4
}

type Surface {
    let albedo: Float3
    let alpha: Float
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
    option use_tint: Bool = true

    group Frame {
        uniform camera: Camera
    }

    group Material {
        uniform surface: Surface
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
fn metal_numbers_buffers_from_one_and_textures_from_zero() {
    // Metal's vertex buffer 0 carries the attribute stream, so a resource
    // buffer can never take it.
    let ir = build(TEXTURED, BackendTarget::Msl);
    let reflection = ir.reflection.expect("a shader");
    let binding = |name: &str| {
        reflection
            .resources
            .iter()
            .find(|resource| resource.resource_name == name)
            .and_then(|resource| {
                resource
                    .backend_bindings
                    .iter()
                    .find(|binding| binding.target == BackendTarget::Msl)
            })
            .map(|binding| binding.binding_index)
            .expect(name)
    };
    assert_eq!(binding("camera"), 1);
    assert_eq!(binding("surface"), 2);
    assert_eq!(binding("albedo"), 0);
    assert_eq!(binding("linear"), 0);
}

#[test]
fn wgsl_takes_the_group_a_shader_wrote_as_its_set() {
    let ir = build(TEXTURED, BackendTarget::Wgsl);
    let reflection = ir.reflection.expect("a shader");
    let surface = reflection
        .resources
        .iter()
        .find(|resource| resource.resource_name == "surface")
        .expect("surface");
    let binding = surface
        .backend_bindings
        .iter()
        .find(|binding| binding.target == BackendTarget::Wgsl)
        .expect("wgsl");
    assert_eq!(binding.group_index, 1, "`Material` is the second group");
    assert_eq!(binding.binding_index, 0, "and its first resource");
}

#[test]
fn glsl_carries_the_name_a_host_looks_the_binding_up_by() {
    let ir = build(TEXTURED, BackendTarget::Glsl430);
    let reflection = ir.reflection.expect("a shader");
    let albedo = reflection
        .resources
        .iter()
        .find(|resource| resource.resource_name == "albedo")
        .expect("albedo");
    let binding = albedo
        .backend_bindings
        .iter()
        .find(|binding| binding.target == BackendTarget::Glsl430)
        .expect("glsl");
    assert_eq!(binding.glsl_name.as_deref(), Some("albedo"));
}

#[test]
fn a_builtin_field_takes_no_location_and_the_rest_are_numbered_in_order() {
    let ir = build(TEXTURED, BackendTarget::Msl);
    let reflection = ir.reflection.expect("a shader");
    let vertex = &reflection.stages[0];
    assert_eq!(vertex.stage, Stage::Vertex);
    assert_eq!(vertex.inputs[0].location, Some(0));
    assert_eq!(vertex.inputs[1].location, Some(1));
    let clip = &vertex.outputs[0];
    assert_eq!(clip.builtin, Some(Builtin::Position));
    assert_eq!(clip.location, None, "a builtin consumes no location slot");
    assert_eq!(vertex.outputs[1].location, Some(0));
}

#[test]
fn a_uniform_layout_is_measured_and_reflected() {
    let ir = build(TEXTURED, BackendTarget::Msl);
    let reflection = ir.reflection.expect("a shader");
    let surface = reflection
        .types
        .iter()
        .find(|declared| declared.name == "Surface")
        .expect("Surface");
    let layout = surface.uniform_layout.as_ref().expect("a layout");
    assert_eq!(layout.fields[0].offset, 0);
    assert_eq!(layout.fields[1].offset, 16, "the Float3 is padded to 16");
    assert_eq!(layout.size, 32);
}

#[test]
fn the_reflection_text_round_trips_through_its_own_decoder() {
    let ir = build(TEXTURED, BackendTarget::Msl);
    let decoded = decode(&ir.reflection_text()).expect("decodes");
    assert_eq!(Some(&decoded), ir.reflection.as_ref());
}

#[test]
fn a_compute_shader_reflects_its_workgroup_size() {
    let ir = build(
        r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}

shader Step {
    group Work {
        storage read_write out: [UInt]
    }
    compute {
        input QIn
        threads(64, 2, 1)
        function entry(q: QIn) {
            out[q.gid.x] = 1
            return
        }
    }
}
"#,
        BackendTarget::Msl,
    );
    let reflection = ir.reflection.clone().expect("a shader");
    assert_eq!(
        reflection.shader_kind,
        kira_shader_model::ShaderKind::Compute
    );
    assert_eq!(reflection.stages[0].threads, Some([64, 2, 1]));
    assert_eq!(reflection.resources[0].resource_kind, ResourceKind::Storage);
    // Round-tripping matters most here: `threads` is the one record that
    // extends the stage above it rather than standing alone.
    let decoded = decode(&ir.reflection_text()).expect("decodes");
    assert_eq!(decoded.stages[0].threads, Some([64, 2, 1]));
}

#[test]
fn the_resource_digest_matches_the_shape_the_graphics_host_parses() {
    // `u|name:binding:size:stageMask:memberCount:member,member:kinds;` with each
    // member `name@offset#size`. The host parses this; the shape is its
    // contract, not ours, so it is pinned literally.
    let ir = build(TEXTURED, BackendTarget::Msl);
    let digest = ir.resource_digest();
    // Stage mask 1 is vertex alone: `camera` is read there and nowhere else.
    assert!(
        digest.contains("u|camera:0:64:1:1:view_projection@0#64:f;"),
        "{digest}"
    );
    // `Surface` is `Float3` then `Float`: the vector sits at 0 and the scalar at
    // 16 because `std140` pads it, but the member's own size stays 12 so the
    // host maps it onto `FLOAT3` rather than `FLOAT4`.
    // `surface` is declared and never read, so no stage claims it — mask 0.
    assert!(
        digest.contains("u|surface:0:32:0:2:albedo@0#12,alpha@16#4:ff;"),
        "{digest}"
    );
    // The texture carries the slot of the sampler its body samples it with, and
    // the name the two collapse into in GLSL.
    assert!(digest.contains("t|albedo:1:2:2:albedo;"), "{digest}");
    assert!(digest.contains("m|linear:2:2;"), "{digest}");
}

#[test]
fn a_written_texture_reaches_the_digest_as_a_storage_image() {
    let ir = build(
        r#"
type Extent {
    let width: UInt
    let height: UInt
}

type QIn {
    @builtin(thread_id)
    let gid: UInt3
}

shader Blit {
    group Work {
        uniform extent: Extent
        sampler smp: Sampler
        texture src: Texture2d
        texture write dst: Texture2d
    }
    compute {
        input QIn
        threads(16, 16, 1)
        function entry(q: QIn) {
            let uv = Float2(Float(q.gid.x) / Float(extent.width), Float(q.gid.y) / Float(extent.height))
            store(dst, q.gid.xy, sample(src, smp, uv))
            return
        }
    }
}
"#,
        BackendTarget::Glsl430,
    );
    let digest = ir.resource_digest();
    // Unsigned members are `u`: a size of 4 alone would leave the host loading
    // them through `glUniform1fv`, which a `uint` uniform refuses.
    assert!(
        digest.contains("u|extent:0:16:4:2:width@0#4,height@4#4:uu;"),
        "{digest}"
    );
    // The sampled texture stays a `t` record with its sampler and GLSL name;
    // the written one is an `i` record carrying its image unit and that the
    // shader only writes it.
    assert!(digest.contains("t|src:2:4:1:src;"), "{digest}");
    assert!(digest.contains("i|dst:3:4:1:1;"), "{digest}");
}

#[test]
fn the_resource_digest_carries_the_wgsl_binding_not_metals() {
    // The host matches this against the slot an application binds, which is the
    // WGSL binding — Metal's buffer index is a different number entirely.
    let ir = build(TEXTURED, BackendTarget::Msl);
    let digest = ir.resource_digest();
    let surface = digest
        .split(';')
        .find(|block| block.starts_with("u|surface:"))
        .expect("the surface block");
    let binding: u32 = surface
        .trim_start_matches("u|")
        .split(':')
        .nth(1)
        .and_then(|field| field.parse().ok())
        .expect("a binding");
    assert_eq!(binding, 0, "`Material`'s first resource is binding 0");
}

#[test]
fn a_shader_with_no_resources_has_an_empty_digest() {
    let ir = build(
        r#"
type VOut {
    @builtin(position)
    let clip_position: Float4
}
shader S {
    vertex {
        output VOut
        function entry() -> VOut {
            let r: VOut
            r.clip_position = Float4(0.0, 0.0, 0.0, 1.0)
            return r
        }
    }
}
"#,
        BackendTarget::Msl,
    );
    assert_eq!(ir.resource_digest(), "");
}

#[test]
fn a_resources_stage_visibility_is_measured_rather_than_assumed() {
    // A host binds a uniform block to every stage the reflection lists, and a
    // stage has only so many block slots — so a resource one stage reads must
    // not be reported against the other.
    let ir = build(TEXTURED, BackendTarget::Msl);
    let reflection = ir.reflection.expect("a shader");
    let stages = |name: &str| {
        reflection
            .resources
            .iter()
            .find(|resource| resource.resource_name == name)
            .map(|resource| resource.visibility.clone())
            .unwrap_or_default()
    };
    assert_eq!(stages("camera"), vec![Stage::Vertex]);
    assert_eq!(stages("albedo"), vec![Stage::Fragment]);
    assert!(stages("surface").is_empty(), "declared but never read");
}
