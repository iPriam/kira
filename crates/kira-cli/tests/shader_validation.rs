//! Every shader backend's output, put through the compiler that consumes it.
//!
//! The unit tests in each backend crate check what this workspace *decided* —
//! that a matrix carries `column_major`, that a storage buffer carries its
//! stride. They cannot check that the result is a shader, because the only
//! thing that knows is the toolchain on the other side. So these run it:
//! Apple's `metal` over the MSL, `naga` over the WGSL, `glslang` over the GLSL
//! and (through its HLSL front end) the HLSL, and `spirv-val` over the SPIR-V.
//!
//! This found four real defects the day it was written — two in the new SPIR-V
//! backend, one in WGSL and one in GLSL that had shipped — which is the
//! argument for it existing.
//!
//! # The shaders are emitted through `ksl!`, not through an internal call
//!
//! A Kira program with a userland `ksl` macro writes each backend's output to a
//! file with Foundation's `writeFile`, and this runs it with the `kira` binary
//! under test. So what the validators see is what a real build produces, down
//! to the string escaping the macro put it through.
//!
//! # A missing validator fails rather than skips
//!
//! A test that quietly passes when its tool is absent is worth less than no
//! test: it reports success for a shader nobody compiled. Each one names the
//! command that installs it instead. The Metal case is compiled out entirely
//! off macOS, which is a fact about the platform rather than a skip.

use std::path::{Path, PathBuf};
use std::process::Command;

/// How to install SPIRV-Tools, on the platform the reader is on.
const SPIRV_TOOLS_HINT: &str = if cfg!(windows) {
    "download install.zip from https://storage.googleapis.com/spirv-tools/badges/build_link_windows_vs2019_release.html and put spirv-val.exe on PATH"
} else {
    "brew install spirv-tools"
};

/// How to install glslang. The binary is `glslang` in current releases;
/// `glslangValidator` is the name this test invokes and the one a release zip
/// has to be copied to.
const GLSLANG_HINT: &str = if cfg!(windows) {
    "download glslang-main-windows-x86_64-release.zip from the KhronosGroup/glslang main-tot release and copy bin/glslang.exe to glslangValidator.exe on PATH"
} else {
    "brew install glslang"
};

/// The path a compiler can write its output to and have it discarded.
///
/// `/dev/null` does not exist on Windows, where the equivalent device is `NUL`.
/// A validator told to write to a path it cannot open fails on the *output*,
/// which reads exactly like the shader being rejected.
fn null_sink() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

/// A graphics shader reaching for what every backend has to get right: a
/// uniform matrix, a texture and its sampler, a varying, and a builtin that is
/// written by one stage and read by the next.
const GRAPHICS: &str = r"
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
shader Tri {
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
";

/// A compute shader over storage buffers, an atomic, and a loop.
const COMPUTE: &str = r"
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
        storage read_write counter: [UInt]
        storage read source: [Float]
    }
    compute {
        input QIn
        threads(64, 1, 1)
        function entry(q: QIn) {
            let zero: UInt = 0
            let one: UInt = 1
            if q.gid.x < particles.count {
                let slot = atomicAdd(counter, zero, one)
                particles[q.gid.x].px = particles[q.gid.x].px + source[zero]
            }
            let i: UInt = 0
            while i < one {
                i = i + one
            }
            return
        }
    }
}
";

/// The program that emits every backend's output beside itself.
///
/// An ordinary userland `ksl` macro over `Ksl.compile`, which is the whole
/// point: the compiler knows nothing about `KslArtifact` or its fields.
const DUMP: &str = r#"
import Foundation

struct KslArtifact {
    var combinedMsl: String = ""
    var vertexWgsl: String = ""
    var fragmentWgsl: String = ""
    var computeWgsl: String = ""
    var vertexGlsl: String = ""
    var fragmentGlsl: String = ""
    var vertexHlsl: String = ""
    var fragmentHlsl: String = ""
    var computeHlsl: String = ""
    var vertexSpirv: String = ""
    var fragmentSpirv: String = ""
    var computeSpirv: String = ""
}

comptime macro ksl {
    kind { function }
    expand(input: Syntax) -> Syntax {
        let msl = Ksl.compile(input, "msl")
        let wgsl = Ksl.compile(input, "wgsl")
        let glsl = Ksl.compile(input, "glsl_330")
        let hlsl = Ksl.compile(input, "hlsl")
        let spirv = Ksl.compile(input, "spirv")
        return quote {
            KslArtifact(
                combinedMsl: #{msl.combinedSource},
                vertexWgsl: #{wgsl.vertexSource},
                fragmentWgsl: #{wgsl.fragmentSource},
                computeWgsl: #{wgsl.computeSource},
                vertexGlsl: #{glsl.vertexSource},
                fragmentGlsl: #{glsl.fragmentSource},
                vertexHlsl: #{hlsl.vertexSource},
                fragmentHlsl: #{hlsl.fragmentSource},
                computeHlsl: #{hlsl.computeSource},
                vertexSpirv: #{spirv.vertexSource},
                fragmentSpirv: #{spirv.fragmentSource},
                computeSpirv: #{spirv.computeSource}
            )
        }
    }
}

function dump(name: String, art: KslArtifact) {
    let a = writeFile(name + ".metal", art.combinedMsl)
    let b = writeFile(name + ".vert.wgsl", art.vertexWgsl)
    let c = writeFile(name + ".frag.wgsl", art.fragmentWgsl)
    let d = writeFile(name + ".comp.wgsl", art.computeWgsl)
    let e = writeFile(name + ".vert.glsl", art.vertexGlsl)
    let f = writeFile(name + ".frag.glsl", art.fragmentGlsl)
    let g = writeFile(name + ".vert.hlsl", art.vertexHlsl)
    let h = writeFile(name + ".frag.hlsl", art.fragmentHlsl)
    let i = writeFile(name + ".comp.hlsl", art.computeHlsl)
    let j = writeFile(name + ".vert.spvhex", art.vertexSpirv)
    let k = writeFile(name + ".frag.spvhex", art.fragmentSpirv)
    let l = writeFile(name + ".comp.spvhex", art.computeSpirv)
    return
}

@Main
function main() {
    dump("Tri", ksl!("Shaders/Tri.ksl"))
    dump("Step", ksl!("Shaders/Step.ksl"))
    return
}
"#;

/// Compiles both shaders through `ksl!` and returns the directory holding every
/// backend's output.
///
/// Built once per test rather than once per process: the tests run in parallel
/// and a shared directory would have them reading each other's half-written
/// files.
fn emitted(tag: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("kira-shader-validation-{tag}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(directory.join("Shaders")).expect("a scratch directory");
    std::fs::write(directory.join("Shaders/Tri.ksl"), GRAPHICS).expect("the graphics shader");
    std::fs::write(directory.join("Shaders/Step.ksl"), COMPUTE).expect("the compute shader");
    std::fs::write(directory.join("dump.kira"), DUMP).expect("the dump program");

    let foundation = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../foundation");
    let run = Command::new(env!("CARGO_BIN_EXE_kira"))
        .env("KIRA_FOUNDATION_HOME", foundation)
        .current_dir(&directory)
        .args(["run", "dump.kira"])
        .output()
        .expect("run kira");
    assert!(
        run.status.success(),
        "the shaders did not compile:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    directory
}

/// Resolves a validator, or fails naming the command that installs it.
///
/// Never `None`: a validator that is not here has not decided the shader is
/// fine, and a test that passed on that basis would be reporting a shader
/// nobody compiled.
fn validator(name: &str, install: &str) -> String {
    // Asked of the tool itself rather than of a shell. `sh -c 'command -v'`
    // needs a POSIX shell to exist and to see the same PATH this process does,
    // and on Windows that is neither guaranteed nor the same lookup the
    // `Command::new` below performs — so a tool that is present could report
    // absent, and one that resolves here could fail to spawn there. Running it
    // is the only question that matters: what fails is being unable to start
    // it, and every one of these answers `--version`.
    let found = Command::new(name)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    assert!(
        found,
        "`{name}` is not on PATH, so this shader was validated by nothing. Install it with:\n    \
         {install}"
    );
    name.to_owned()
}

/// Runs one validator over one file and requires it to be happy.
///
/// Run from the file's own directory, because a validator asked to compile
/// rather than only parse writes its output beside wherever it was started —
/// and the working directory of a test is the crate it lives in.
fn accepts(tool: &str, args: &[&str], file: &Path) {
    let mut command = Command::new(tool);
    if let Some(parent) = file.parent() {
        command.current_dir(parent);
    }
    command.args(args).arg(file);
    let run = command.output().unwrap_or_else(|error| {
        panic!("could not run {tool}: {error}");
    });
    assert!(
        run.status.success(),
        "{tool} rejected {}:\n{}\n{}",
        file.display(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

/// Decodes a hexadecimal SPIR-V module into the binary a validator reads.
///
/// The artifact carries SPIR-V as eight characters per word because a macro
/// splices strings; this is the other half of that, and doing it here is also
/// the only check that the encoding round-trips at all.
fn decode_spirv(hex: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(hex).expect("the hexadecimal module");
    if text.trim().is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for word in text.trim().as_bytes().chunks(8) {
        let word = std::str::from_utf8(word).expect("hexadecimal is ascii");
        let value = u32::from_str_radix(word, 16)
            .unwrap_or_else(|_| panic!("`{word}` is not a hexadecimal word"));
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    let binary = hex.with_extension("spv");
    std::fs::write(&binary, bytes).expect("the decoded module");
    Some(binary)
}

#[test]
fn every_spirv_module_passes_the_khronos_validator() {
    let tool = validator("spirv-val", SPIRV_TOOLS_HINT);
    let directory = emitted("spirv");
    let mut checked = 0;
    for name in [
        "Tri.vert.spvhex",
        "Tri.frag.spvhex",
        "Step.comp.spvhex",
        "Tri.comp.spvhex",
        "Step.vert.spvhex",
        "Step.frag.spvhex",
    ] {
        let Some(binary) = decode_spirv(&directory.join(name)) else {
            continue;
        };
        accepts(&tool, &["--target-env", "vulkan1.1"], &binary);
        checked += 1;
    }
    assert_eq!(checked, 3, "the vertex, fragment, and compute modules");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn every_wgsl_module_passes_naga() {
    let tool = validator("naga", "cargo install naga-cli");
    let directory = emitted("wgsl");
    for name in ["Tri.vert.wgsl", "Tri.frag.wgsl", "Step.comp.wgsl"] {
        accepts(&tool, &[], &directory.join(name));
    }
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn every_glsl_module_compiles_under_glslang() {
    let tool = validator("glslangValidator", GLSLANG_HINT);
    let directory = emitted("glsl");
    accepts(&tool, &["-S", "vert"], &directory.join("Tri.vert.glsl"));
    accepts(&tool, &["-S", "frag"], &directory.join("Tri.frag.glsl"));
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn every_hlsl_module_compiles_under_glslangs_hlsl_front_end() {
    let tool = validator("glslangValidator", GLSLANG_HINT);
    let directory = emitted("hlsl");
    for (stage, entry, file) in [
        ("vert", "vertex_main", "Tri.vert.hlsl"),
        ("frag", "fragment_main", "Tri.frag.hlsl"),
        ("comp", "compute_main", "Step.comp.hlsl"),
    ] {
        // `-V` is what makes this compile the HLSL rather than only parse it,
        // and `-o` keeps the module it then writes out of the tree.
        accepts(
            &tool,
            &["-D", "-S", stage, "-e", entry, "-V", "-o", null_sink()],
            &directory.join(file),
        );
    }
    let _ = std::fs::remove_dir_all(&directory);
}

/// Metal exists on macOS and nowhere else, so this is compiled out rather than
/// skipped: there is no Metal shader to be wrong about on another platform.
#[cfg(target_os = "macos")]
#[test]
fn every_metal_module_compiles_under_apples_compiler() {
    // `metal` is a component of Xcode rather than part of it, and an Xcode with
    // the component missing reports exactly that — so the assertion names the
    // command that fetches it.
    let probe = Command::new("xcrun")
        .args(["-sdk", "macosx", "metal", "--version"])
        .output()
        .expect("run xcrun");
    assert!(
        probe.status.success(),
        "Apple's Metal compiler is not installed, so this shader was validated by nothing. \
         Install it with:\n    xcodebuild -downloadComponent MetalToolchain\n{}",
        String::from_utf8_lossy(&probe.stderr)
    );

    let directory = emitted("metal");
    for name in ["Tri.metal", "Step.metal"] {
        let file = directory.join(name);
        let run = Command::new("xcrun")
            .args(["-sdk", "macosx", "metal", "-std=metal3.0", "-c"])
            .arg(&file)
            .args(["-o", null_sink()])
            .output()
            .expect("run metal");
        assert!(
            run.status.success(),
            "metal rejected {}:\n{}",
            file.display(),
            String::from_utf8_lossy(&run.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&directory);
}
