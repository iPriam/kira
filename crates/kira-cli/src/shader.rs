//! `kira shader`: compile KSL and say what each target actually emitted.
//!
//! A build cannot answer that. Every shader is compiled for all five targets and
//! a target that cannot express one leaves **empty sources** plus a note rather
//! than failing the build — so a shader can build clean, hand the driver an
//! empty string, and fail at `sg_make_shader` with an empty info log. This
//! prints the emission per target, which turns that into something visible.
//!
//! ```text
//! kira shader build
//! kira shader build --target glsl_330
//! kira shader build --emit glsl_330
//! ```
//!
//! It builds every `.ksl` in the package, the way `kira build` builds every
//! `.kira` in it. Naming one file would answer a question nobody has: a shader
//! is not built on its own, and the one a build breaks on is the one you did not
//! think to name.
//!
//! The emitted source is **written** to `generated/shaders/`, under the names
//! the runtime loader reads — `{Shader}.vert.glsl` and so on. A shader loaded by
//! asset name resolves to those files, so a package that never ran this hands
//! the driver an empty string and fails with an empty info log.

use std::path::{Path, PathBuf};

use kira_build::shader::{CompiledShader, ShaderEmission, compile_files};
use kira_source::SourceMap;

use crate::dispatch::EXIT_UNAVAILABLE;

/// What the verb was asked to do.
struct Options {
    /// Only this target, when given.
    target: Option<String>,
    /// Print the emitted source for this target rather than a summary.
    emit: Option<String>,
}

/// Runs `kira shader`. Returns the process exit code.
pub fn shader(args: &[String]) -> i32 {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("kira shader: {message}");
            return EXIT_UNAVAILABLE;
        }
    };
    let root = PathBuf::from(".");
    let files = match collect(&root) {
        Ok(files) if files.is_empty() => {
            eprintln!("kira shader build: this package has no `.ksl` file");
            return EXIT_UNAVAILABLE;
        }
        Ok(files) => files,
        Err(message) => {
            eprintln!("kira shader build: {message}");
            return EXIT_UNAVAILABLE;
        }
    };

    let (emissions, diagnostics, sources) = compile_files(&files);
    let mut map = SourceMap::new();
    for (path, text) in &sources {
        // The ids come back in insertion order, which is the order
        // `compile_files` numbered its diagnostics against.
        let _ = map.insert(path.clone(), text.clone());
    }
    crate::diagnostics::emit(&diagnostics, &map);
    let failed = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == kira_diagnostics::Severity::Error);

    if let Some(target) = &options.emit {
        print_sources(&emissions, target);
        return i32::from(failed);
    }
    if failed {
        return 1;
    }
    let written = match write_all(&emissions) {
        Ok(written) => written,
        Err(message) => {
            eprintln!("kira shader build: {message}");
            return EXIT_UNAVAILABLE;
        }
    };
    print_summary(&emissions, options.target.as_deref());
    println!("Built {written} shader file(s) into {OUTPUT_DIR}");
    0
}

/// Where the runtime loader reads a shader named by asset.
static OUTPUT_DIR: &str = "generated/shaders";

/// Writes every emitted stage under the names the loader reads.
fn write_all(emissions: &[ShaderEmission]) -> Result<usize, String> {
    let directory = PathBuf::from(OUTPUT_DIR);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("`{}` could not be created: {error}", directory.display()))?;
    let mut written = 0;
    for emission in emissions {
        let asset = emission
            .path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let compiled = &emission.compiled;
        for (suffix, source) in stage_files(emission.target, compiled) {
            if source.is_empty() {
                continue;
            }
            let file = directory.join(format!("{asset}{suffix}"));
            std::fs::write(&file, source)
                .map_err(|error| format!("`{}` could not be written: {error}", file.display()))?;
            written += 1;
        }
        // Write one resource digest per shader rather than one per target. Every
        // target reflects the same resources, so the first emission is enough.
        if !compiled.resource_reflection.is_empty() {
            let file = directory.join(format!("{asset}.resources"));
            std::fs::write(&file, &compiled.resource_reflection)
                .map_err(|error| format!("`{}` could not be written: {error}", file.display()))?;
        }
    }
    Ok(written)
}

/// The file suffix each of one target's stages is read back under.
fn stage_files<'a>(target: &str, compiled: &'a CompiledShader) -> Vec<(&'static str, &'a str)> {
    let (vertex, fragment, compute) = match target {
        "msl" => return vec![(".metal", compiled.combined_source.as_str())],
        "wgsl" => (".vert.wgsl", ".frag.wgsl", ".comp.wgsl"),
        "glsl_430" => (".vert.glsl", ".frag.glsl", ".comp.glsl"),
        "hlsl" => (".vert.hlsl", ".frag.hlsl", ".comp.hlsl"),
        "spirv" => (".vert.spv", ".frag.spv", ".comp.spv"),
        _ => return Vec::new(),
    };
    vec![
        (vertex, compiled.vertex_source.as_str()),
        (fragment, compiled.fragment_source.as_str()),
        (compute, compiled.compute_source.as_str()),
    ]
}

/// Parses `build [--target <name>] [--emit <name>]`.
fn parse(args: &[String]) -> Result<Options, String> {
    let mut target = None;
    let mut emit = None;
    let mut seen_build = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "build" => seen_build = true,
            "--target" => {
                target = Some(rest.next().ok_or("`--target` needs a target name")?.clone());
            }
            "--emit" => {
                emit = Some(rest.next().ok_or("`--emit` needs a target name")?.clone());
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown option `{other}`"));
            }
            other => {
                return Err(format!(
                    "unknown argument `{other}`; usage: `kira shader build`"
                ));
            }
        }
    }
    if !seen_build {
        return Err("usage: `kira shader build [--target <name>] [--emit <name>]`".to_owned());
    }
    Ok(Options { target, emit })
}

/// Every `.ksl` file at or under `path`, in a stable order.
fn collect(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(format!("`{}` does not exist", path.display()));
    }
    let mut found = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("`{}` could not be read: {error}", directory.display()))?;
        for entry in entries.flatten() {
            let entry = entry.path();
            if entry.is_dir() {
                stack.push(entry);
            } else if entry.extension().is_some_and(|kind| kind == "ksl") {
                found.push(entry);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Prints one line per shader per target, saying which stages carry source.
fn print_summary(emissions: &[ShaderEmission], only: Option<&str>) {
    let mut current = None;
    for emission in emissions {
        if only.is_some_and(|target| target != emission.target) {
            continue;
        }
        if current != Some(&emission.path) {
            println!("{}", emission.path.display());
            current = Some(&emission.path);
        }
        let compiled = &emission.compiled;
        let stages = [
            ("combined", &compiled.combined_source),
            ("vertex", &compiled.vertex_source),
            ("fragment", &compiled.fragment_source),
            ("compute", &compiled.compute_source),
        ];
        let carried: Vec<&str> = stages
            .iter()
            .filter(|(_, source)| !source.is_empty())
            .map(|(name, _)| *name)
            .collect();
        // A target that emitted nothing is the case worth seeing: the build
        // stays green and the driver is handed an empty string.
        let what = if carried.is_empty() {
            "nothing emitted".to_owned()
        } else {
            carried.join(", ")
        };
        println!("  {:<10} {what}", emission.target);
    }
}

/// Prints the source one target emitted, for reading or diffing.
fn print_sources(emissions: &[ShaderEmission], target: &str) {
    for emission in emissions.iter().filter(|one| one.target == target) {
        let compiled = &emission.compiled;
        for (stage, source) in [
            ("combined", &compiled.combined_source),
            ("vertex", &compiled.vertex_source),
            ("fragment", &compiled.fragment_source),
            ("compute", &compiled.compute_source),
        ] {
            if source.is_empty() {
                continue;
            }
            println!(
                "// {} — {target} {stage}\n{source}",
                emission.path.display()
            );
        }
    }
}
