//! Parity for the `Ksl` shader namespace and the userland macro over it.
//!
//! `ksl!` is no compiler builtin — the engine declares an ordinary `comptime
//! macro` and the compiler supplies only `Ksl.compile(path, target)`. So this
//! is the whole path a renderer depends on, end to end: a real `.ksl` file, the
//! KSL pipeline behind the seam, a Kira artifact type the compiler has never
//! heard of, and the shader source arriving inside the running program.
//!
//! Expansion is a frontend pass, so the three backends see a program with no
//! macro left in it — but *what* they see is a whole Metal module inlined as a
//! string literal, and a literal that survived escaping on one backend and not
//! another would show up here rather than as a black window.

use crate::assert_parity_with_files;

/// A graphics shader every target can express.
const TRIANGLE: &str = r"
type Camera {
    let view_projection: Float4x4
}
type VOut {
    @builtin(position)
    let clip_position: Float4
}
type FOut {
    let color: Float4
}
shader Tri {
    group Frame {
        uniform camera: Camera
    }
    vertex {
        output VOut
        function entry() -> VOut {
            let r: VOut
            r.clip_position = mul(camera.view_projection, Float4(0.0, 0.0, 0.0, 1.0))
            return r
        }
    }
    fragment {
        input VOut
        output FOut
        function entry(f: VOut) -> FOut {
            let r: FOut
            r.color = Float4(1.0, 1.0, 1.0, 1.0)
            return r
        }
    }
}
";

/// The artifact type and the macro that builds it, both ordinary Kira.
///
/// Held here rather than in each case so the cases differ only in the shader
/// they name — which is the variable under test.
const ENGINE: &str = r#"
struct KslArtifact {
    var shaderName: String = ""
    var vertexEntry: String = ""
    var fragmentEntry: String = ""
    var computeEntry: String = ""
    var combinedMsl: String = ""
    var vertexWgsl: String = ""
    var fragmentWgsl: String = ""
    var vertexGlsl: String = ""
    var fragmentGlsl: String = ""
    var vertexHlsl: String = ""
    var fragmentHlsl: String = ""
    var computeHlsl: String = ""
    var vertexSpirv: String = ""
    var computeSpirv: String = ""
    var resourceReflection: String = ""
}

// Declared here rather than imported, like `KslArtifact` above: the point of
// this test is that the whole surface is userland, so the target enum is the
// program's too.
enum ShaderBackend { Msl Wgsl Glsl Hlsl Spirv }

comptime macro ksl {
    kind { function }
    expand(input: Syntax) -> Syntax {
        let msl = Ksl.compile(input, ShaderBackend.Msl)
        let wgsl = Ksl.compile(input, ShaderBackend.Wgsl)
        let glsl = Ksl.compile(input, ShaderBackend.Glsl)
        let hlsl = Ksl.compile(input, ShaderBackend.Hlsl)
        let spirv = Ksl.compile(input, ShaderBackend.Spirv)
        return quote {
            KslArtifact(
                shaderName: #{msl.shaderName},
                vertexEntry: #{msl.vertexEntry},
                fragmentEntry: #{msl.fragmentEntry},
                computeEntry: #{msl.computeEntry},
                combinedMsl: #{msl.combinedSource},
                vertexWgsl: #{wgsl.vertexSource},
                fragmentWgsl: #{wgsl.fragmentSource},
                vertexGlsl: #{glsl.vertexSource},
                fragmentGlsl: #{glsl.fragmentSource},
                vertexHlsl: #{hlsl.vertexSource},
                fragmentHlsl: #{hlsl.fragmentSource},
                computeHlsl: #{hlsl.computeSource},
                vertexSpirv: #{spirv.vertexSource},
                computeSpirv: #{spirv.computeSource},
                resourceReflection: #{msl.resourceReflection}
            )
        }
    }
}
"#;

/// Every backend runs a program carrying a compiled shader, and agrees on it.
///
/// The lengths matter as much as the names: a Metal module is thousands of
/// bytes of source that crossed into the program as one string literal, and
/// printing how much of it arrived is what distinguishes an artifact that was
/// really inlined from one whose fields defaulted to empty.
#[test]
fn a_userland_ksl_macro_inlines_every_backends_shader() {
    let program = format!(
        r##"{ENGINE}
@Main
function main() {{
    let art = ksl!("Shaders/Tri.ksl")
    print(art.shaderName)
    print(art.vertexEntry)
    print(art.fragmentEntry)
    print(art.resourceReflection)
    // The whole Metal module arrived, newlines and quotes intact.
    print(art.combinedMsl.indexOf("#include <metal_stdlib>") >= 0)
    print(art.combinedMsl.indexOf("vertex_main") >= 0)
    print(art.vertexWgsl.indexOf("@vertex") >= 0)
    print(art.fragmentWgsl.indexOf("@fragment") >= 0)
    print(art.vertexGlsl.indexOf("#version 430 core") >= 0)
    print(art.vertexHlsl.indexOf("column_major float4x4") >= 0)
    print(art.fragmentHlsl.indexOf("SV_Target0") >= 0)
    // SPIR-V is binary, so it crossed as hexadecimal — the magic word first.
    print(art.vertexSpirv.substring(0, 8))
    return
}}
"##
    );
    let output = assert_parity_with_files(&program, &[("Shaders/Tri.ksl", TRIANGLE)]);
    assert_eq!(
        output,
        "Tri\n\
         vertex_main\n\
         fragment_main\n\
         u|camera:0:64:1:1:view_projection@0#64:f;\n\
         true\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\n\
         07230203\n",
    );
}

/// A target that cannot express the shader leaves its fields empty, and the
/// others still carry it.
///
/// GLSL 330 has no compute stage. The build succeeds with a note rather than
/// failing, because Metal and WebGPU still have the shader — so a renderer
/// reads the same fields either way and asks whether one is empty, instead of
/// branching on a shape that varies per target.
#[test]
fn a_target_that_cannot_express_a_shader_leaves_empty_fields_not_absent_ones() {
    let program = format!(
        r#"{ENGINE}
@Main
function main() {{
    let art = ksl!("Shaders/Step.ksl")
    print(art.computeEntry)
    print(art.combinedMsl.indexOf("kernel") >= 0)
    // D3D and Vulkan both have a compute stage where GLSL 330 does not, so
    // these two carry it.
    print(art.computeHlsl.indexOf("[numthreads(64, 1, 1)]") >= 0)
    print(art.computeSpirv.substring(0, 8))
    // Never compiled, so empty — and readable rather than missing.
    print(art.vertexGlsl.count)
    print(art.vertexEntry.count)
    return
}}
"#
    );
    let output = assert_parity_with_files(
        &program,
        &[(
            "Shaders/Step.ksl",
            r"
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
        threads(64, 1, 1)
        function entry(q: QIn) {
            out[q.gid.x] = 1
            return
        }
    }
}
",
        )],
    );
    assert_eq!(output, "compute_main\ntrue\ntrue\n07230203\n0\n0\n");
}
