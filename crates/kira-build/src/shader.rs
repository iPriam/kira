//! Compiling the shaders a program's macro call sites name.
//!
//! This is the implementation of `kira-macros`'s `ShaderCompiler` seam, and it
//! lives here because here is where the whole KSL pipeline and a filesystem are
//! both in reach: parsing is layer 1, checking layer 2, lowering layer 3, and
//! the emitters layer 4, all below this crate.
//!
//! Shaders are compiled **before** analysis rather than during it. Macro
//! expansion runs inside salsa queries, which must be pure, and compiling a
//! shader reads files — so the paths are scanned out of the source first, each
//! one compiled, and the results handed in as an input the queries only read.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_ksl_semantics::{Module, check};
use kira_macros::{CompiledShader, PrecompiledShaders};
use kira_shader_ir::{ShaderIr, lower};
use kira_shader_model::{BackendTarget, Stage};
use kira_source::{FileSpan, SourceId, Span};

/// The targets every shader is compiled for.
///
/// All of them, every time: a shader that compiled for Metal but silently not
/// for WebGPU would fail on the platform nobody built on.
const TARGETS: [BackendTarget; 3] = [
    BackendTarget::Msl,
    BackendTarget::Wgsl,
    BackendTarget::Glsl330,
];

/// Every KSL file one compilation read, in the order it was given an id.
pub(crate) type ShaderSources = Vec<(String, String)>;

/// Compiles every shader `files` names, relative to `root`.
///
/// Returns the table, everything the compilation reported, and the text of
/// every `.ksl` file read. A shader that fails to compile contributes
/// diagnostics and no entry, so the macro call site refuses too rather than
/// expanding to something wrong.
///
/// `base` is the first source id the shaders may use. They are numbered from
/// there so a diagnostic about a shader renders against the shader's own text
/// rather than against whatever Kira file happens to share its id — the caller
/// registers the returned sources at those same ids.
pub(crate) fn precompile(
    root: &Path,
    files: &[(SourceId, &str)],
    base: u32,
) -> (PrecompiledShaders, Vec<Diagnostic>, ShaderSources) {
    let mut entries = Vec::new();
    let mut diagnostics = Vec::new();
    let mut sources: ShaderSources = Vec::new();
    for path in kira_macros::shader_paths(files) {
        kira_diagnostics::progress!("compiling shader {path}");
        let resolved = root.join(&path);
        let Some(ir) = compile_one(&resolved, base, &mut diagnostics, &mut sources) else {
            continue;
        };
        for target in TARGETS {
            let ir = if ir.reflection.as_ref().is_some_and(|r| r.backend == target) {
                ir.clone()
            } else {
                lower(ir.module.clone(), target)
            };
            entries.push((path.clone(), target.label().to_owned(), emit(&ir, target)));
        }
    }
    (PrecompiledShaders::new(entries), diagnostics, sources)
}

/// Parses, resolves imports for, and checks one `.ksl` file.
fn compile_one(
    path: &Path,
    base: u32,
    diagnostics: &mut Vec<Diagnostic>,
    sources: &mut ShaderSources,
) -> Option<ShaderIr> {
    let module = load(path, base, diagnostics, sources)?;
    let mut imports = Vec::new();
    let mut seen = BTreeSet::new();
    let directory = path.parent().unwrap_or(Path::new("."));
    for (alias, segments) in import_paths(&module) {
        if !seen.insert(alias.clone()) {
            continue;
        }
        let Some(resolved) = resolve_import(directory, &segments) else {
            continue;
        };
        if let Some(loaded) = load(&resolved, base, diagnostics, sources) {
            imports.push((alias, loaded));
        }
    }
    let checked = check(&module, &imports);
    let failed = !checked.diagnostics.is_empty();
    diagnostics.extend(checked.diagnostics);
    if failed {
        return None;
    }
    Some(lower(checked.module, BackendTarget::Msl))
}

/// Reads and parses one KSL file.
///
/// A file that cannot be read is reported rather than skipped: a silently
/// missing shader would reach the call site as "no output was compiled", which
/// says nothing about the path being wrong.
fn load(
    path: &Path,
    base: u32,
    diagnostics: &mut Vec<Diagnostic>,
    sources: &mut ShaderSources,
) -> Option<Module> {
    let display = path.display().to_string();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            diagnostics.push(unreadable(&display, &error.to_string()));
            return None;
        }
    };
    // One id per file, reused when the same file is read twice — an import
    // shared by two shaders is one source, not two.
    let source = match sources.iter().position(|(known, _)| *known == display) {
        Some(at) => SourceId::new(base + u32::try_from(at).unwrap_or(0)),
        None => {
            sources.push((display, text.clone()));
            SourceId::new(base + u32::try_from(sources.len() - 1).unwrap_or(0))
        }
    };
    let parsed = kira_ksl_parser::parse(source, &text);
    let failed = !parsed.diagnostics.is_empty();
    diagnostics.extend(parsed.diagnostics);
    if failed {
        return None;
    }
    Some(Module {
        source,
        tree: parsed.tree,
        interner: parsed.interner,
    })
}

/// The diagnostic a shader file that could not be read reports.
fn unreadable(path: &str, reason: &str) -> Diagnostic {
    let message = format!("`{path}` could not be read: {reason}");
    let mut diagnostic = Diagnostic::single(
        Severity::Error,
        message.clone(),
        Label::primary(FileSpan::new(SourceId::new(0), Span::new(0, 0)), message),
    );
    diagnostic.code = Some("KSLS011");
    diagnostic.phase = Some("ksl");
    diagnostic
}

/// Every import a module writes, as an alias and its path segments.
fn import_paths(module: &Module) -> Vec<(String, Vec<String>)> {
    module
        .tree
        .items
        .iter()
        .filter_map(|item| {
            let kira_ksl_syntax_model::ast::Item::Import(import) = item else {
                return None;
            };
            let segments: Vec<String> = import
                .path
                .iter()
                .map(|&symbol| module.interner.resolve(symbol).to_owned())
                .collect();
            let alias = import.alias.map_or_else(
                || segments.last().cloned().unwrap_or_default(),
                |symbol| module.interner.resolve(symbol).to_owned(),
            );
            Some((alias, segments))
        })
        .collect()
}

/// Finds `A.B` under `directory`, matching each segment case-insensitively.
///
/// The corpus writes `import Common.Lighting` against a directory named
/// `Common` in one repository and `common` in another, so matching exactly
/// would resolve on one machine and not the next.
fn resolve_import(directory: &Path, segments: &[String]) -> Option<PathBuf> {
    let mut current = directory.to_path_buf();
    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        let mut found = None;
        for entry in std::fs::read_dir(&current).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".ksl").unwrap_or(&name);
            if stem.eq_ignore_ascii_case(segment) {
                found = Some(entry.path());
                break;
            }
        }
        current = found?;
        if last {
            return Some(current);
        }
    }
    None
}

/// Emits every source and name one target contributes.
fn emit(ir: &ShaderIr, target: BackendTarget) -> CompiledShader {
    let entry = |stage: Stage| {
        ir.reflection
            .as_ref()
            .and_then(|reflection| {
                reflection
                    .stages
                    .iter()
                    .find(|candidate| candidate.stage == stage)
            })
            .map(|reflected| reflected.entry_name.clone())
            .unwrap_or_default()
    };
    let mut compiled = CompiledShader {
        shader_name: ir
            .reflection
            .as_ref()
            .map(|reflection| reflection.shader_name.clone())
            .unwrap_or_default(),
        vertex_entry: entry(Stage::Vertex),
        fragment_entry: entry(Stage::Fragment),
        compute_entry: entry(Stage::Compute),
        uniform_reflection: ir.uniform_digest(),
        ..CompiledShader::default()
    };
    match target {
        BackendTarget::Msl => compiled.combined_source = kira_msl_backend::emit(ir),
        BackendTarget::Wgsl => {
            compiled.vertex_source = kira_wgsl_backend::emit(ir, Stage::Vertex);
            compiled.fragment_source = kira_wgsl_backend::emit(ir, Stage::Fragment);
            compiled.compute_source = kira_wgsl_backend::emit(ir, Stage::Compute);
        }
        // GLSL 330 cannot express every shader — compute and storage arrived in
        // 430 — and it says so rather than emitting something that will not
        // link. A shader it refuses still compiles for the other targets, so
        // the refusal leaves empty sources rather than failing the build.
        BackendTarget::Glsl330 => {
            compiled.vertex_source = kira_glsl_backend::emit(ir, Stage::Vertex).unwrap_or_default();
            compiled.fragment_source =
                kira_glsl_backend::emit(ir, Stage::Fragment).unwrap_or_default();
        }
        BackendTarget::Hlsl | BackendTarget::Spirv => {}
    }
    compiled
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own, removed when the test finishes.
    ///
    /// The workspace pins its external crates, so a scratch directory is built
    /// from the process id and a per-test tag rather than by adding a
    /// dependency for it.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("kira-shader-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a scratch directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_shader_compiles_for_every_target_a_call_site_can_ask_for() {
        let directory = Scratch::new("every-target");
        let shaders = directory.path().join("Shaders");
        std::fs::create_dir_all(&shaders).expect("the shader directory");
        std::fs::write(
            shaders.join("Tri.ksl"),
            r#"
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
"#,
        )
        .expect("the shader");

        let program =
            "@Main function main() {\n    let art = ksl!(\"Shaders/Tri.ksl\")\n    return\n}\n";
        let (table, diagnostics, sources) =
            precompile(directory.path(), &[(SourceId::new(0), program)], 1);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(sources.len(), 1, "one shader was read");

        use kira_macros::ShaderCompiler;
        let msl = table.compile("Shaders/Tri.ksl", "msl").expect("msl");
        assert!(msl.combined_source.contains("#include <metal_stdlib>"));
        assert_eq!(msl.vertex_entry, "vertex_main");
        // The artifact carries the compact digest the graphics host parses,
        // not the whole KSLR1 reflection — the two are different contracts with
        // different consumers, and handing the host the wrong one leaves every
        // shader running with its uniforms unbound.
        assert_eq!(
            msl.uniform_reflection,
            "camera:0:64:1:1:view_projection@0#64;"
        );

        let wgsl = table.compile("Shaders/Tri.ksl", "wgsl").expect("wgsl");
        assert!(wgsl.vertex_source.contains("@vertex"));
        assert!(wgsl.fragment_source.contains("@fragment"));

        let glsl = table.compile("Shaders/Tri.ksl", "glsl_330").expect("glsl");
        assert!(glsl.vertex_source.contains("#version 330 core"));
    }

    #[test]
    fn a_shader_that_does_not_check_reports_and_contributes_no_entry() {
        let directory = Scratch::new("rejected");
        std::fs::write(
            directory.path().join("Bad.ksl"),
            "type T {\n    let a: Nope\n}\n",
        )
        .expect("the shader");
        let program = "@Main function main() {\n    let art = ksl!(\"Bad.ksl\")\n    return\n}\n";
        let (table, diagnostics, sources) =
            precompile(directory.path(), &[(SourceId::new(0), program)], 4);
        let reported = diagnostics
            .iter()
            .find(|d| d.code == Some("KSLS001"))
            .expect("the rejection");
        assert!(
            diagnostics.iter().any(|d| d.code == Some("KSLS001")),
            "{diagnostics:?}"
        );
        // The span has to name the shader, not whatever Kira file shares id 0.
        assert_eq!(reported.labels[0].span.source, SourceId::new(4));
        assert_eq!(sources.len(), 1);
        assert!(table.is_empty(), "a rejected shader must not be usable");
    }

    #[test]
    fn a_path_no_call_site_names_is_never_compiled() {
        let directory = Scratch::new("no-shaders");
        let program = "@Main function main() {\n    return\n}\n";
        let (table, diagnostics, sources) =
            precompile(directory.path(), &[(SourceId::new(0), program)], 1);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(table.is_empty());
        assert!(sources.is_empty());
    }
}
