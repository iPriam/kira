//! Generating Kira bindings from a native library's C headers.
//!
//! A `NativeLibrary` declaration says how to *link* a C library and, when it
//! carries an `autobind` record, what to *call* in it. This module reads the
//! second half: it parses the declared headers with the managed toolchain's own
//! clang and writes one Kira source file of `@FFI.Extern` functions and the
//! `@FFI.*` types their signatures need.
//!
//! # Why the compiler generates them rather than a person
//!
//! A hand-written binding is a copy of a header, and a copy drifts. The width
//! of a `long`, the order of a struct's fields, whether a pointer is `const` —
//! each is part of the C contract and none of it is visible at the call site
//! that got it wrong. Worse, the copy has to exist before the program compiles,
//! so a package that ships headers and no bindings does not build at all on a
//! machine that has not run a generator by hand.
//!
//! # What this module does not do
//!
//! It does not link, and it does not compile C. `super::native_libraries`
//! resolves archives and `super::native_sources` builds them; a library's
//! headers are read here and nowhere else. It also never decides *when* to run:
//! the build drives it, once, before semantic analysis.

mod cache;
mod emit;
mod harvest;
mod model;
mod names;
mod types;

use std::path::{Path, PathBuf};

use kira_clang::Clang;
use kira_native_lib_definition::{Availability, NativeLibrarySpec, TargetTriple};

pub use model::BindingModule;

/// Where one package's declarations are anchored, and what they are being
/// generated for.
#[derive(Debug, Clone)]
pub struct AutobindContext {
    /// The package directory, under which the stamp cache lives.
    pub package_root: PathBuf,
    /// The package's Kira source root — the directory whose `.kira` files are
    /// the package. A generated binding has to land inside it to be compiled.
    pub source_root: PathBuf,
    /// The directory the declaration's relative paths are written against: the
    /// package root for an inline entry, the TOML's own directory for a file.
    pub base_dir: PathBuf,
    /// The target this build selected.
    pub target: TargetTriple,
}

/// Why a library's bindings could not be generated.
#[derive(Debug, thiserror::Error)]
pub enum AutobindError {
    /// libclang could not be loaded out of the managed toolchain.
    #[error(transparent)]
    Load(#[from] kira_clang::LoadError),
    /// A declared header does not exist.
    #[error(
        "native library `{library}`: the `autobind` header `{path}` does not exist\n\
         note: paths are resolved against `{base}`, and `${{NAME}}` is read from the environment"
    )]
    MissingHeader {
        /// The library whose declaration named it.
        library: String,
        /// The header path, after expansion.
        path: String,
        /// The directory relative paths anchor at.
        base: String,
    },
    /// clang refused the headers.
    #[error("native library `{library}`: {source}")]
    Parse {
        /// The library being bound.
        library: String,
        /// What clang said.
        #[source]
        source: kira_clang::ParseError,
    },
    /// The generated file or its stamp could not be written.
    #[error("native library `{library}`: cannot write `{path}`: {message}")]
    Io {
        /// The library being bound.
        library: String,
        /// The path that could not be written.
        path: String,
        /// The underlying I/O failure, rendered.
        message: String,
    },
}

/// What has to happen for one library's bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutobindStatus {
    /// The binding is current; nothing to do.
    Current,
    /// The binding has to be generated, which needs a C parser.
    Stale,
    /// A binding exists that this generator did not write: it is the package's
    /// own source and is left as it stands.
    Adopt,
}

/// One library's generation, resolved but not yet run.
///
/// Separated from running it so a build can decide whether it needs a C parser
/// at all: loading libclang costs more than every up-to-date package in a graph
/// put together, and most builds have nothing to generate.
#[derive(Debug, Clone)]
pub struct AutobindPlan {
    /// The library being bound.
    pub library: String,
    /// Where the generated Kira source goes.
    pub output: PathBuf,
    /// What has to happen.
    pub status: AutobindStatus,
    /// The headers to parse, in declaration order.
    headers: Vec<PathBuf>,
    /// The clang driver arguments the parse runs with.
    arguments: Vec<String>,
    /// Where the stamp goes.
    stamp_file: PathBuf,
    /// The stamp to write once the output is current.
    stamp: cache::Stamp,
}

/// What generating one library's bindings produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutobindReport {
    /// The library that was bound.
    pub library: String,
    /// The file that was written.
    pub output: PathBuf,
    /// How many declarations it holds.
    pub declarations: usize,
    /// How many the headers hold that the seam cannot carry.
    pub skipped: usize,
}

/// Resolves what has to happen for `spec`, or `None` when nothing does.
///
/// `None` covers the two ordinary cases: a library that declares no `autobind`
/// record, and one with no target row for this build. A missing row is not
/// reported here — linking reports it, with the whole declaration in view — and
/// a library declared `Optional` is excluded on this target by design.
pub fn plan(
    spec: &NativeLibrarySpec,
    context: &AutobindContext,
) -> Result<Option<AutobindPlan>, AutobindError> {
    let Some(autobind) = spec.autobind() else {
        return Ok(None);
    };
    let selected = spec
        .targets()
        .iter()
        .any(|row| row.triple() == &context.target);
    if !selected {
        return Ok(None);
    }

    let headers = resolve_headers(spec, context)?;
    if headers.is_empty() {
        return Ok(None);
    }

    let module = autobind
        .module
        .clone()
        .unwrap_or_else(|| spec.name().to_owned());
    let output = output_path(autobind.output.as_deref(), &module, context);
    let stamp_file = cache::stamp_path(&context.package_root.join(".kira-build"), spec.name());
    let arguments = clang_arguments(spec, context, &headers);
    let stamp = cache::Stamp {
        key: format!(
            "{} {} {} {}",
            spec.name(),
            context.target,
            arguments.join(" "),
            match spec.availability() {
                Availability::Required => "required",
                Availability::Optional => "optional",
            }
        ),
        inputs: headers
            .iter()
            .map(|header| cache::describe_input(header))
            .collect(),
    };
    let status = match cache::freshness(&stamp_file, &output, &stamp) {
        cache::Freshness::Current => AutobindStatus::Current,
        cache::Freshness::Stale => AutobindStatus::Stale,
        cache::Freshness::Adopt => AutobindStatus::Adopt,
    };
    Ok(Some(AutobindPlan {
        library: spec.name().to_owned(),
        output,
        status,
        headers,
        arguments,
        stamp_file,
        stamp,
    }))
}

/// Records an existing binding as current without touching it.
pub fn adopt(plan: &AutobindPlan) -> Result<(), AutobindError> {
    cache::write(&plan.stamp_file, &plan.stamp).map_err(|error| AutobindError::Io {
        library: plan.library.clone(),
        path: plan.stamp_file.display().to_string(),
        message: error.to_string(),
    })
}

/// Parses the headers and writes the binding.
pub fn generate(
    plan: &AutobindPlan,
    spec: &NativeLibrarySpec,
    clang: &Clang,
) -> Result<AutobindReport, AutobindError> {
    let Some(autobind) = spec.autobind() else {
        return Err(AutobindError::Io {
            library: plan.library.clone(),
            path: plan.output.display().to_string(),
            message: "the library declares no `autobind` record".to_owned(),
        });
    };
    // One translation unit including every declared header in order, rather
    // than one unit per header. A C header set is a sequence, not a set of
    // independent files: `sokol_glue.h` refuses to compile unless
    // `sokol_gfx.h` came first, and parsing them apart binds nothing for every
    // header that depends on its predecessors.
    let umbrella = write_umbrella(plan)?;
    let unit = clang
        .parse(&umbrella, &plan.arguments)
        .map_err(|source| AutobindError::Parse {
            library: plan.library.clone(),
            source,
        })?;
    let mut module = harvest::harvest(&unit, &plan.library, autobind, &plan.headers);
    module.sort();

    let text = emit::render(&module);
    write_output(plan, &text)?;
    cache::write(&plan.stamp_file, &plan.stamp).map_err(|error| AutobindError::Io {
        library: plan.library.clone(),
        path: plan.stamp_file.display().to_string(),
        message: error.to_string(),
    })?;
    Ok(AutobindReport {
        library: plan.library.clone(),
        output: plan.output.clone(),
        declarations: module.declaration_count(),
        skipped: module.skipped.len(),
    })
}

/// Writes the one C file that includes every declared header in order.
///
/// It lives beside the stamp, in the package's build directory: a real file
/// rather than an in-memory buffer because the include paths a header writes
/// resolve against the file including it, and because a header set that will
/// not compile is far easier to diagnose when the exact input is on disk.
fn write_umbrella(plan: &AutobindPlan) -> Result<PathBuf, AutobindError> {
    let path = plan
        .stamp_file
        .with_file_name(format!("{}.umbrella.c", plan.library));
    let mut text = String::from("/* generated by kira FFI autobinding; not a source file */\n");
    for header in &plan.headers {
        text.push_str("#include \"");
        text.push_str(&header.display().to_string());
        text.push_str("\"\n");
    }
    let failed = |message: String| AutobindError::Io {
        library: plan.library.clone(),
        path: path.display().to_string(),
        message,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| failed(error.to_string()))?;
    }
    std::fs::write(&path, text).map_err(|error| failed(error.to_string()))?;
    Ok(path)
}

/// Writes the generated source, creating its directory.
///
/// A file whose bytes are already what would be written is left alone, so a
/// regenerated-but-unchanged binding does not restamp every file that watches
/// it — which is what makes a live session survive a `kira check` beside it.
fn write_output(plan: &AutobindPlan, text: &str) -> Result<(), AutobindError> {
    if std::fs::read_to_string(&plan.output).is_ok_and(|existing| existing == text) {
        return Ok(());
    }
    let failed = |message: String| AutobindError::Io {
        library: plan.library.clone(),
        path: plan.output.display().to_string(),
        message,
    };
    if let Some(parent) = plan.output.parent() {
        std::fs::create_dir_all(parent).map_err(|error| failed(error.to_string()))?;
    }
    std::fs::write(&plan.output, text).map_err(|error| failed(error.to_string()))
}

/// Every declared header, expanded and resolved against the declaration's base.
fn resolve_headers(
    spec: &NativeLibrarySpec,
    context: &AutobindContext,
) -> Result<Vec<PathBuf>, AutobindError> {
    let Some(autobind) = spec.autobind() else {
        return Ok(Vec::new());
    };
    let declared: Vec<&String> = match autobind.headers.is_empty() {
        false => autobind.headers.iter().collect(),
        // A declaration that lists no headers of its own binds the entrypoint
        // the library is already compiled against, which is the header a
        // one-header library would otherwise write twice.
        true => spec
            .headers()
            .and_then(|headers| headers.entrypoint.as_ref())
            .into_iter()
            .collect(),
    };
    let mut resolved = Vec::new();
    for header in declared {
        let Some(expanded) = expand_environment(header) else {
            // An unset variable means the SDK this header lives in is not on
            // this machine. That is not a fault to stop a build with: the
            // library it belongs to is the one that will report it, at link
            // time, naming the target.
            return Ok(Vec::new());
        };
        let path = context.base_dir.join(expanded);
        if !path.is_file() {
            return Err(AutobindError::MissingHeader {
                library: spec.name().to_owned(),
                path: path.display().to_string(),
                base: context.base_dir.display().to_string(),
            });
        }
        // Absolute from here on. A declaration writes its headers relative to
        // its own manifest, and the file that includes them is written into the
        // build directory — so a relative path would resolve against the wrong
        // directory, and the same header would compare unequal to itself when
        // clang reports which file a declaration came from.
        resolved.push(std::fs::canonicalize(&path).unwrap_or(path));
    }
    Ok(resolved)
}

/// The clang driver arguments one library's headers are parsed with.
fn clang_arguments(
    spec: &NativeLibrarySpec,
    context: &AutobindContext,
    headers: &[PathBuf],
) -> Vec<String> {
    let mut arguments = vec!["-x".to_owned(), "c".to_owned()];
    if let Some(triple) = clang_triple(&context.target) {
        arguments.push(format!("--target={triple}"));
    }
    // libclang carries no default sysroot on Apple hosts any more than the
    // managed driver does (see `native_sources`): without `-isysroot` naming
    // the active SDK, `<math.h>` and every other C library header is simply
    // not found and the harvest binds nothing.
    if matches!(context.target.os(), "macos" | "ios" | "tvos" | "xros")
        && let Some(sdk) = crate::native_sources::apple_sdk_root(&context.target)
    {
        arguments.push("-isysroot".to_owned());
        arguments.push(sdk);
    }
    // Each header's own directory, so a header that includes its neighbour by
    // bare name resolves the way it does when the library is compiled.
    let mut include_dirs: Vec<PathBuf> = headers
        .iter()
        .filter_map(|header| header.parent().map(Path::to_path_buf))
        .collect();
    if let Some(declared) = spec.headers() {
        for directory in &declared.include_dirs {
            if let Some(expanded) = expand_environment(directory) {
                include_dirs.push(context.base_dir.join(expanded));
            }
        }
    }
    include_dirs.dedup();
    for directory in include_dirs {
        // Through the same normalizer the source build uses: libclang's header
        // search cannot read a Windows verbatim path any more than clang's can,
        // and these directories are canonicalized above.
        arguments.push(format!(
            "-I{}",
            crate::native_sources::compiler_path(&directory)
        ));
    }
    if let Some(declared) = spec.headers() {
        for define in &declared.defines {
            arguments.push(format!("-D{define}"));
        }
    }
    if let Some(row) = spec
        .targets()
        .iter()
        .find(|row| row.triple() == &context.target)
    {
        for define in row.defines() {
            arguments.push(format!("-D{define}"));
        }
    }
    arguments
}

/// The LLVM triple a Kira target triple is parsed as, when it is not the host.
///
/// A host build takes clang's own defaults, which already carry this machine's
/// sysroot and its `long` width. A cross build states the target, so the
/// generated widths are the ones the program will run with rather than the ones
/// the machine generating it happens to have.
pub(crate) fn clang_triple(target: &TargetTriple) -> Option<String> {
    if target == &host_target() {
        return None;
    }
    let triple = match (target.arch(), target.os(), target.abi()) {
        (arch, "macos", _) => format!("{arch}-apple-darwin"),
        (arch, os @ ("ios" | "tvos" | "xros"), "sim" | "simulator") => {
            format!("{arch}-apple-{os}-simulator")
        }
        (arch, os @ ("ios" | "tvos" | "xros"), _) => format!("{arch}-apple-{os}"),
        (arch, "linux", abi) => format!("{arch}-unknown-linux-{abi}"),
        (arch, "windows", abi) => format!("{arch}-pc-windows-{abi}"),
        (arch, "emscripten", _) => format!("{arch}-unknown-emscripten"),
        (arch, os, abi) => format!("{arch}-{os}-{abi}"),
    };
    Some(triple)
}

/// This machine's target triple in the spelling a manifest writes.
pub fn host_target() -> TargetTriple {
    let (os, abi) = match std::env::consts::OS {
        "macos" => ("macos", "none"),
        "linux" => ("linux", "gnu"),
        "windows" => ("windows", "msvc"),
        other => (other, "none"),
    };
    TargetTriple::new(std::env::consts::ARCH, os, abi)
}

/// Replaces every `${NAME}` with its environment value, or `None` when one is
/// unset.
///
/// The corpus writes SDK-rooted headers this way — `${VULKAN_SDK}/Include/...`
/// — because the SDK is installed per machine and no manifest can name its
/// path. An unset variable is "this SDK is not here", not a malformed path.
fn expand_environment(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}')?;
        let value = std::env::var(&after[..end]).ok()?;
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Where a library's generated binding is written.
///
/// Under the package's Kira source root, because a file outside it is not part
/// of the package and would never be compiled. A declaration's own `output` is
/// honored when it lands inside that root and ignored when it does not — a
/// generated file nothing loads is worse than no file at all, and the default
/// is the location the language already documents for bindings.
fn output_path(declared: Option<&str>, module: &str, context: &AutobindContext) -> PathBuf {
    let default = context
        .source_root
        .join("bindings")
        .join(format!("{module}.kira"));
    let Some(declared) = declared else {
        return default;
    };
    let Some(expanded) = expand_environment(declared) else {
        return default;
    };
    let candidate = normalize(&context.base_dir.join(expanded));
    match candidate.starts_with(normalize(&context.source_root)) {
        true => candidate,
        false => default,
    }
}

/// Resolves `.` and `..` lexically, without touching the disk.
///
/// A declared output is written relative to the manifest that declared it and
/// routinely climbs out of it (`../bindings/x.kira`), so the question "is this
/// inside the source root" cannot be answered by comparing the text. It is
/// asked before the file exists, so it cannot be answered by canonicalizing
/// either.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests;
