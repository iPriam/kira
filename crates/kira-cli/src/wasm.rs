//! The Web half of `build` and `run`: artifact layout, then serving the result.
//!
//! `build --device wasm32` writes a module and the page that runs it.
//! `run --device wasm32` does the same and then serves them, because a Kira
//! program on the Web runs in a browser and a browser needs an origin.

use std::path::{Path, PathBuf};

use kira_ir::IrProgram;
use kira_wasm_runtime::{WasmArtifacts, WasmBuildOptions, WasmDevice, WasmError};

use crate::native::Artifacts;
use crate::serve::{ServeError, Server, open_browser};

/// Why a Web build or run could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum WebError {
    /// The artifact directory could not be prepared.
    #[error("cannot prepare the build directory: {0}")]
    Artifacts(#[source] std::io::Error),
    /// The wasm backend failed.
    #[error(transparent)]
    Backend(#[from] WasmError),
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

    /// The page path.
    pub fn page(&self) -> PathBuf {
        self.directory.join("index.html")
    }
}

/// Builds a program for `device`, writing the module and the page that runs it.
pub fn build(ir: &IrProgram, source: &Path, device: WasmDevice) -> Result<WasmArtifacts, WebError> {
    let artifacts = WebArtifacts::for_source(source)?;
    let options = WasmBuildOptions {
        module_name: artifacts.stem.clone(),
        device,
        wasm_path: artifacts.wasm(),
        page_path: Some(artifacts.page()),
    };
    Ok(kira_wasm_runtime::build(ir, &options)?)
}

/// Builds a program for `device`, then serves it and opens a browser at it.
///
/// Blocks until interrupted: the page is the program's output, so returning
/// would tear down the server the moment the browser asked for the module.
pub fn run(ir: &IrProgram, source: &Path, device: WasmDevice) -> Result<(), WebError> {
    let artifacts = WebArtifacts::for_source(source)?;
    let built = build(ir, source, device)?;

    let server = Server::bind(artifacts.directory().to_path_buf())?;
    let url = server.url();

    println!("Serving {} on {url}", built.wasm.display());
    if !open_browser(&url) {
        println!("Could not open a browser; open {url} to run it.");
    }
    println!("Press Ctrl-C to stop.");

    server.serve_forever()?;
    Ok(())
}
