//! `kira export <family>`: generate a per-platform project for a Kira package.
//!
//! Each family writes a project tree under `<package>/exports/<platform>/`. The
//! CMake families (Windows, Linux) are pure scaffolds and complete here; the
//! Apple, Web, and Android families need a per-architecture or wasm build on top
//! of the generated tree and are wired in as that orchestration lands, reporting
//! precisely rather than emitting a project that references artifacts no build
//! produced.

use std::path::{Path, PathBuf};

use kira_export::{GeneratedProject, cmake, web};
use kira_manifest::platform_config::{
    BuildProfile, ExportFamily, RunnerId, WebSurface, web_surface_requirements,
};

use crate::pipeline::{EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::progress::{err, out};

/// The families whose generators are wired end to end today.
const AVAILABLE: &str = "windows, linux, web, apple";

/// Runs `kira export <family> [path] [--profile ...] [--surface ...]`.
pub fn export(args: &[String]) -> i32 {
    let request = match Request::parse(args) {
        Ok(request) => request,
        Err(code) => return code,
    };

    let target = match kira_project::resolve_target(Path::new(&request.path)) {
        Ok(target) => target,
        Err(error) => {
            err!("kira export: {error}");
            return EXIT_FAILURE;
        }
    };
    let Some(root) = target.root_path.clone() else {
        err!(
            "kira export: `{}` is not inside a Kira package; export needs a package to build from",
            request.path
        );
        return EXIT_USAGE;
    };
    let project_name = target
        .project_name
        .clone()
        .unwrap_or_else(|| "KiraApp".to_owned());
    let mut request = request;
    // Every family builds from one entrypoint, including an Xcode Run Script
    // rebuild; the package name names the bundle.
    request.package_name = target
        .project
        .as_ref()
        .map(|project| project.manifest.name.clone())
        .unwrap_or_else(|| project_name.clone());
    match crate::pipeline::resolve_source_path(&request.path) {
        Ok(source) => request.source = source,
        Err(code) => return code,
    }
    let exports_root = PathBuf::from(&root).join("exports");

    match request.family {
        ExportFamily::Windows => write_project(
            &exports_root.join("windows"),
            cmake::windows_project(&project_name),
            "Windows Visual Studio/CMake project",
            &["cmake"],
        ),
        ExportFamily::Linux => write_project(
            &exports_root.join("linux"),
            cmake::linux_project(&project_name),
            "Linux CMake/Ninja project",
            &["cmake", "ninja"],
        ),
        ExportFamily::Web => export_web(&request, &exports_root, &project_name),
        ExportFamily::Apple
        | ExportFamily::Macos
        | ExportFamily::Ios
        | ExportFamily::Tvos
        | ExportFamily::Visionos => {
            crate::export_apple::run(&request, &exports_root, &root, &project_name)
        }
        other => {
            err!(
                "kira export: the `{}` family is not available in this toolchain build yet; \
                 wired families are: {AVAILABLE}",
                other.label()
            );
            EXIT_FAILURE
        }
    }
}

/// The runner id an Apple platform's live sessions and bundles use.
pub(crate) fn runner_id_for(platform: kira_export::apple::ApplePlatform) -> RunnerId {
    match platform {
        kira_export::apple::ApplePlatform::Macos => RunnerId::Macos,
        kira_export::apple::ApplePlatform::Ios => RunnerId::Ios,
        kira_export::apple::ApplePlatform::Tvos => RunnerId::Tvos,
        kira_export::apple::ApplePlatform::Visionos => RunnerId::Visionos,
    }
}

/// Exports a Web app: compile the package to wasm32-emscripten and wrap the
/// result in the shell page, generated FFI glue, and manifest.
fn export_web(request: &Request, exports_root: &Path, project_name: &str) -> i32 {
    if request.surface == WebSurface::Hybrid {
        err!(
            "kira export: the hybrid web surface is modeled, but it needs a browser \
             VM/native boundary runner that this toolchain does not ship yet"
        );
        return EXIT_FAILURE;
    }
    let device = web_device();
    let source = match crate::pipeline::resolve_source_path(&request.path) {
        Ok(source) => source,
        Err(code) => return code,
    };
    let triple = crate::foreign_libs::target_for_device(&device);
    let compiled = match crate::pipeline::compile_verified_path(&source, &triple) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    let ir = match crate::pipeline::entrypoint_ir("export", compiled) {
        Ok(ir) => ir,
        Err(code) => return code,
    };
    let foreign = match crate::pipeline::foreign_inputs(&source, &ir, &device) {
        Ok(foreign) => foreign,
        Err(code) => return code,
    };
    let link = crate::pipeline::foreign_link_of(&foreign);

    let web_root = exports_root.join("web");
    let built = match crate::wasm::build_export_app(
        &ir,
        kira_backend_api::WasmDevice::Wasm32,
        link,
        &web_root,
        request.profile != BuildProfile::Debug,
    ) {
        Ok(built) => built,
        Err(error) => {
            err!("kira export: {error}");
            return EXIT_FAILURE;
        }
    };

    let requirements = web_surface_requirements(request.surface);
    if let Err(error) = web::web_project(project_name, requirements).write_to(&web_root) {
        err!("kira export: {error}");
        return EXIT_FAILURE;
    }

    out!(
        "exported Kira Wasm {} app at {} (loader {}, module {})",
        request.surface.label(),
        web_root.display(),
        built
            .loader
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
        built
            .module
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
    );
    audit_tools(&["emcc"]);
    EXIT_OK
}

/// The device a Web export compiles for.
///
/// A Web export is always a wasm32 module: emscripten's toolchain, its ports,
/// and every browser today are 32-bit wasm, so there is no surface a wider
/// module would serve.
fn web_device() -> crate::options::Device {
    crate::options::Device::Web(kira_backend_api::WasmDevice::Wasm32)
}

/// Reports each host tool a family's local build needs — a missing tool is a
/// note, not a failure, because the export is still correct and buildable once
/// the tool is installed.
fn audit_tools(required: &[&str]) {
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|tool| !command_on_path(tool))
        .collect();
    if !missing.is_empty() {
        err!(
            "note: {} not found on PATH; install {} to build this export locally",
            missing.join(" and "),
            if missing.len() == 1 { "it" } else { "them" },
        );
    }
}

/// Writes `project` under `directory`, reports it, and audits the host tools it
/// needs — a missing tool is a note, not a failure, because the project is still
/// correct and buildable once the tool is installed.
fn write_project(
    directory: &Path,
    project: GeneratedProject,
    description: &str,
    required_tools: &[&str],
) -> i32 {
    if let Err(error) = project.write_to(directory) {
        err!("kira export: {error}");
        return EXIT_FAILURE;
    }
    out!("exported {description} at {}", directory.display());
    audit_tools(required_tools);
    EXIT_OK
}

/// Whether `name` resolves to an executable on `PATH`.
pub(crate) fn command_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| {
        let candidate = directory.join(name);
        candidate.is_file() || candidate.with_extension("exe").is_file()
    })
}

/// A parsed `kira export` invocation.
pub(crate) struct Request {
    pub(crate) family: ExportFamily,
    pub(crate) path: String,
    /// The entry source the package resolves to, filled after discovery.
    pub(crate) source: String,
    pub(crate) package_name: String,
    pub(crate) profile: BuildProfile,
    pub(crate) surface: WebSurface,
    /// An Xcode Run Script rebuild request: which SDK to regenerate.
    pub(crate) xcode_rebuild: Option<String>,
}

impl Request {
    /// Parses the family, an optional package path, and the options the
    /// families accept.
    fn parse(args: &[String]) -> Result<Request, i32> {
        let mut family: Option<ExportFamily> = None;
        let mut path: Option<String> = None;
        let mut profile = BuildProfile::Debug;
        let mut surface = WebSurface::Dom;
        let mut xcode_rebuild = None;
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            match arg.as_str() {
                "--profile" => {
                    let value = Self::value_of("--profile", args, &mut index)?;
                    match BuildProfile::parse(&value) {
                        Some(parsed) => profile = parsed,
                        None => {
                            err!(
                                "kira export: `{value}` is not a profile (debug, profiler, release)"
                            );
                            return Err(EXIT_USAGE);
                        }
                    }
                }
                "--surface" => {
                    let value = Self::value_of("--surface", args, &mut index)?;
                    match WebSurface::parse(&value) {
                        Some(parsed) => surface = parsed,
                        None => {
                            err!(
                                "kira export: `{value}` is not a web surface (dom, webgpu, hybrid)"
                            );
                            return Err(EXIT_USAGE);
                        }
                    }
                }
                "--xcode-rebuild" => {
                    let value = Self::value_of("--xcode-rebuild", args, &mut index)?;
                    xcode_rebuild = Some(value);
                }
                flag if flag.starts_with('-') => {
                    err!("kira export: unknown option `{flag}`");
                    return Err(EXIT_USAGE);
                }
                positional if family.is_none() => {
                    let Some(parsed) = ExportFamily::parse(positional) else {
                        err!(
                            "kira export: `{positional}` is not an export family \
                             (apple, macos, ios, tvos, visionos, windows, android, web, linux)"
                        );
                        return Err(EXIT_USAGE);
                    };
                    family = Some(parsed);
                }
                positional if path.is_none() => path = Some(positional.to_owned()),
                extra => {
                    err!("kira export: unexpected argument `{extra}`");
                    return Err(EXIT_USAGE);
                }
            }
            index += 1;
        }
        let Some(family) = family else {
            err!("kira export: name a family (apple, windows, linux, web, ...)");
            return Err(EXIT_USAGE);
        };
        Ok(Request {
            family,
            path: path.unwrap_or_else(|| ".".to_owned()),
            source: String::new(),
            package_name: String::new(),
            profile,
            surface,
            xcode_rebuild,
        })
    }

    /// Reads the value following an option flag, advancing `index` past it.
    fn value_of(flag: &str, args: &[String], index: &mut usize) -> Result<String, i32> {
        let Some(value) = args.get(*index + 1) else {
            err!("kira export: `{flag}` needs a value");
            return Err(EXIT_USAGE);
        };
        *index += 1;
        Ok(value.clone())
    }
}
