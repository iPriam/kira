//! The Web half of `build` and `run`: object emission, the emscripten link,
//! and serving the result.
//!
//! The pipeline mirrors the native one exactly. The LLVM backend emits a
//! WebAssembly object in process — the same C-API codegen, a different target
//! machine, never a textual-IR round trip — and emscripten's `emcc` is the
//! linker driver over that object plus the runtime archive, the same role
//! `clang` plays for a host executable. What emscripten adds at link time is
//! the Web glue the host gets from its OS: memory setup, stdio routed to the
//! page, and the page itself.
//!
//! `build --device wasm32` writes the module and the page or loader that uses it.
//! `run --device wasm32` does the same and then serves them, because a Kira
//! program on the Web runs in a browser and a browser needs an origin.

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_backend_api::WasmDevice;
use kira_build::export_surface;
use kira_ir::IrProgram;
use kira_llvm_backend::{LlvmError, NativeExportSurface, NativeLinkInputs};

use crate::native::Artifacts;
use crate::serve::{ServeError, Server, open_browser};

/// The emscripten-target runtime archive's file name beside `kira`.
///
/// The name carries the target so it can sit beside the host archive without
/// either being mistaken for the other: linking a host runtime into a wasm
/// module fails in the linker at best and at runtime at worst.
const WASM32_RUNTIME_ARCHIVE: &str = "libkira_native_bridge-wasm32-emscripten.a";
const WASM64_RUNTIME_ARCHIVE: &str = "libkira_native_bridge-wasm64-emscripten.a";

/// Why a Web build or run could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum WebError {
    /// The artifact directory could not be prepared.
    #[error("cannot prepare the build directory: {0}")]
    Artifacts(#[source] std::io::Error),
    /// The backend could not emit the module's object.
    #[error(transparent)]
    Backend(#[from] LlvmError),
    /// The library has no package name with which to derive its export symbols.
    #[error("a Web library build needs a package name to derive its exported symbols")]
    LibraryNameMissing,
    /// The bytecode compiler could not provide the library's export table.
    #[error(transparent)]
    Bytecode(#[from] kira_bytecode::CompileError),
    /// The runtime archive for the Web is not installed beside this compiler.
    #[error(
        "the Web runtime archive `{name}` is missing; install or build the matching \
         `kira-native-bridge` target"
    )]
    RuntimeArchiveMissing {
        /// The target-specific runtime archive name.
        name: &'static str,
    },
    /// `emcc` rejected the generated foreign shim.
    #[error(
        "`emcc` could not compile the generated foreign shim; its output above names the error"
    )]
    ShimUncompilable,
    /// `emcc` is required to link the module and is not on PATH.
    #[error("`emcc` was not found on PATH; the Web link is driven by emscripten")]
    EmccUnavailable,
    /// `emcc` ran and failed.
    #[error("`emcc` could not link the module; its output above names the error")]
    LinkFailed,
    /// The development server failed.
    #[error(transparent)]
    Serve(#[from] ServeError),
}

/// Where the Web artifacts live and what they are called.
///
/// They go in their own directory under `.kira-build` because that directory is
/// what gets served: the object files and bytecode beside them are nobody's
/// business, and a server that only has web assets under its root cannot hand
/// out anything else.
pub struct WebArtifacts {
    directory: PathBuf,
    stem: String,
}

impl WebArtifacts {
    /// Where the generated foreign C shim source is written.
    fn shim_source(&self) -> PathBuf {
        self.directory.join(format!("{}_ffi_shim.c", self.stem))
    }

    /// Where the shim object `emcc` compiles lands.
    fn shim_object(&self) -> PathBuf {
        self.directory.join(format!("{}_ffi_shim.o", self.stem))
    }

    /// Resolves the Web artifact layout for `source`, creating the directory.
    pub fn for_source(source: &Path) -> Result<Self, WebError> {
        let base = Artifacts::for_source(source).map_err(WebError::Artifacts)?;
        let directory = base.web_directory();
        std::fs::create_dir_all(&directory).map_err(WebError::Artifacts)?;
        Ok(Self {
            directory,
            stem: base.stem().to_owned(),
        })
    }

    /// The directory the server is rooted at.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// The module path.
    pub fn wasm(&self) -> PathBuf {
        self.directory.join(format!("{}.wasm", self.stem))
    }

    /// The object the backend emits, which the link consumes.
    fn object(&self) -> PathBuf {
        self.directory.join(format!("{}.o", self.stem))
    }

    /// The page path.
    pub fn page(&self) -> PathBuf {
        self.directory.join(format!("{}.html", self.stem))
    }
}

/// What a Web build produced.
pub struct BuiltWeb {
    /// The linked module.
    pub wasm: PathBuf,
    /// The page that runs it.
    pub page: PathBuf,
}

/// Builds a program for `device`: emit the object, link the module and page.
///
/// `foreign_link` are the resolved `wasm32-emscripten` link inputs that satisfy
/// the program's `@FFI.Extern` imports. The archives precede the runtime
/// archive on the `emcc` line so an adapter's reference to a C symbol is
/// satisfied by the archive that defines it, and the flags the Web target rows
/// declared (`--use-port=…`, `-sERROR_ON_UNDEFINED_SYMBOLS=0`) follow it —
/// emscripten needs those to link a port at all. A program whose target row is
/// absent was already reported by the caller, before this runs.
pub fn build(
    ir: &IrProgram,
    source: &Path,
    device: WasmDevice,
    foreign_link: &NativeLinkInputs,
) -> Result<BuiltWeb, WebError> {
    let artifacts = WebArtifacts::for_source(source)?;

    kira_llvm_backend::build_wasm_object(ir, &artifacts.stem, &artifacts.object(), device)?;

    // A program passing a struct by value needs its C shim compiled for the Web
    // target too, and by emcc rather than the host clang: the shim is what
    // applies the by-value ABI the selected emscripten target defines.
    let shim = build_wasm_shim(ir, foreign_link, &artifacts)?;

    let runtime = wasm_runtime_archive(device).ok_or(WebError::RuntimeArchiveMissing {
        name: runtime_archive_name(device),
    })?;
    link_web(
        &artifacts,
        device,
        foreign_link,
        shim.as_deref(),
        &runtime,
        &[],
        true,
    )?;

    Ok(BuiltWeb {
        wasm: artifacts.wasm(),
        page: artifacts.page(),
    })
}

/// Builds an exported library for a Web device.
///
/// The native library surface is reused as the wasm surface: every export and
/// class destructor has one `BridgeValue` trampoline, and emscripten retains
/// those symbols in the no-entry module for its JavaScript loader.
pub fn build_library(
    ir: &IrProgram,
    source: &Path,
    device: WasmDevice,
    foreign_link: &NativeLinkInputs,
    library: Option<&str>,
) -> Result<BuiltWeb, WebError> {
    let library = library.ok_or(WebError::LibraryNameMissing)?;
    let module = kira_bytecode::compile(ir)?;
    let surface = export_surface(library, &module.exports);
    let artifacts = WebArtifacts::for_source(source)?;

    kira_llvm_backend::build_wasm_library(
        ir,
        &artifacts.stem,
        &artifacts.object(),
        device,
        &surface,
    )?;
    let shim = build_wasm_shim(ir, foreign_link, &artifacts)?;
    let runtime = wasm_runtime_archive(device).ok_or(WebError::RuntimeArchiveMissing {
        name: runtime_archive_name(device),
    })?;
    let symbols = export_symbols(&surface);
    link_web(
        &artifacts,
        device,
        foreign_link,
        shim.as_deref(),
        &runtime,
        &symbols,
        false,
    )?;

    Ok(BuiltWeb {
        wasm: artifacts.wasm(),
        page: artifacts.page(),
    })
}

/// Links one Web object as either an executable page or a no-entry library.
fn link_web(
    artifacts: &WebArtifacts,
    device: WasmDevice,
    foreign_link: &NativeLinkInputs,
    shim: Option<&Path>,
    runtime: &Path,
    exports: &[String],
    executable: bool,
) -> Result<(), WebError> {
    let mut command = Command::new("emcc");
    command.arg(artifacts.object());
    if let Some(shim) = shim {
        command.arg(shim);
    }
    for archive in foreign_link.archives() {
        command.arg(archive);
    }
    command.arg(runtime);
    for argument in foreign_link.driver_arguments() {
        command.arg(argument);
    }
    if executable {
        // `main` returning ends the program, exactly as it does on the host;
        // without this emscripten keeps the runtime alive for a page that may
        // want callbacks later, which a Kira program has not asked for.
        command.arg("-sEXIT_RUNTIME=1");
    } else {
        command.arg("--no-entry");
        if !exports.is_empty() {
            let names = exports
                .iter()
                .map(|symbol| format!("_{symbol}"))
                .collect::<Vec<_>>()
                .join(",");
            command.arg(format!("-sEXPORTED_FUNCTIONS={names}"));
        }
    }
    if device == WasmDevice::Wasm64 {
        command.arg("-sMEMORY64=1");
    }
    let status = command
        .arg("-o")
        .arg(artifacts.page())
        .status()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => WebError::EmccUnavailable,
            _ => WebError::LinkFailed,
        })?;
    if !status.success() {
        return Err(WebError::LinkFailed);
    }
    Ok(())
}

/// Converts a native export surface to the symbols emscripten must retain.
fn export_symbols(surface: &NativeExportSurface) -> Vec<String> {
    surface
        .abi_marker
        .iter()
        .cloned()
        .chain(surface.functions.iter().map(|export| export.symbol.clone()))
        .chain(surface.classes.iter().map(|class| class.symbol.clone()))
        .collect()
}

/// Compiles the program's foreign C shim for wasm with `emcc`, if it needs one.
///
/// `None` for a program that neither passes a struct by value nor hands C a
/// callback entered with one, so no build gains an `emcc -c` step it does not
/// need. The
/// generated source is the same text the host build compiles; only the compiler
/// differs, which is the point: each target's own C compiler decides that
/// target's by-value ABI.
fn build_wasm_shim(
    ir: &IrProgram,
    foreign_link: &NativeLinkInputs,
    artifacts: &WebArtifacts,
) -> Result<Option<PathBuf>, WebError> {
    let imports: Vec<_> = ir
        .foreign_imports
        .iter()
        .map(|entry| entry.import.clone())
        .collect();
    let Some(text) = kira_llvm_backend::shim::generate(
        &imports,
        &ir.foreign_callbacks,
        &ir.foreign_aggregates,
        foreign_link.unavailable_imports(),
    ) else {
        return Ok(None);
    };

    let source = artifacts.shim_source();
    let object = artifacts.shim_object();
    std::fs::write(&source, text).map_err(WebError::Artifacts)?;
    let status = Command::new("emcc")
        .arg("-c")
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .status()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => WebError::EmccUnavailable,
            _ => WebError::ShimUncompilable,
        })?;
    if !status.success() {
        return Err(WebError::ShimUncompilable);
    }
    Ok(Some(object))
}

/// Builds a program for `device`, then serves it and opens a browser at it.
///
/// Blocks until interrupted: the page is the program's output, so returning
/// would tear down the server the moment the browser asked for the module.
pub fn run(
    ir: &IrProgram,
    source: &Path,
    device: WasmDevice,
    foreign_link: &NativeLinkInputs,
) -> Result<(), WebError> {
    let artifacts = WebArtifacts::for_source(source)?;
    let built = build(ir, source, device, foreign_link)?;

    let server = Server::bind(artifacts.directory().to_path_buf())?;
    let page = built
        .page
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let url = format!("{}{page}", server.url());

    println!("Serving {} on {url}", built.wasm.display());
    if !open_browser(&url) {
        println!("Could not open a browser; open {url} to run it.");
    }
    println!("Press Ctrl-C to stop.");

    server.serve_forever()?;
    Ok(())
}

/// Locates the emscripten-target runtime archive.
///
/// Installed toolchains ship it beside `kira` (knvm's installers put it
/// there); a `kira` running out of a cargo target tree finds the archive
/// where the matching `cargo build -p kira-native-bridge --target
/// <device>-unknown-emscripten` left it, two directories over.
///
/// The bare `libkira_native_bridge.a` name is looked up **only** inside the
/// per-target development tree, where it genuinely is the target's archive.
/// Beside the executable that name is the *host* archive — knvm stages it
/// there for native builds — and linking host objects into a wasm module
/// fails in the linker at best and at runtime at worst.
fn wasm_runtime_archive(device: WasmDevice) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    let installed_name = runtime_archive_name(device);
    let development_name = "libkira_native_bridge.a";

    let installed = directory.join(installed_name);
    if installed.is_file() {
        return Some(installed);
    }

    // target/<profile>/kira -> target/<device>-unknown-emscripten/<profile>/
    let profile = directory.file_name()?.to_owned();
    let target = format!("{}-unknown-emscripten", device.label());
    let dev_directory = directory.parent()?.join(target).join(profile);
    [installed_name, development_name]
        .into_iter()
        .map(|name| dev_directory.join(name))
        .find(|path| path.is_file())
}

/// The installed runtime archive name for a Web memory width.
fn runtime_archive_name(device: WasmDevice) -> &'static str {
    match device {
        WasmDevice::Wasm32 => WASM32_RUNTIME_ARCHIVE,
        WasmDevice::Wasm64 => WASM64_RUNTIME_ARCHIVE,
    }
}
