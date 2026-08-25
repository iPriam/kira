//! `kira live web`: build the Web export and serve it.
//!
//! A Kira program on the Web runs in a browser, and a browser needs an origin,
//! so the live loop for `web` is: compile and link exactly as the export does,
//! then serve the result from a directory that holds nothing but web assets
//! and open it. The page reloads when the module changes — which, for a
//! program whose whole surface is a page, is the browser's own reload tier.

use std::path::{Path, PathBuf};

use kira_backend_api::WasmDevice;
use kira_export::web;
use kira_manifest::platform_config::{WebSurface, web_surface_requirements};

use crate::progress::{err, out};

/// Builds the Web export and serves it until interrupted.
pub(crate) fn run(options: &crate::live::LiveOptions) -> i32 {
    if options.backend != crate::live::LiveBackend::Vm {
        err!(
            "kira live: the web runner builds with the wasm pipeline; \
             `--backend` does not apply"
        );
        return crate::pipeline::EXIT_USAGE;
    }
    if options.quit_after.is_some() {
        err!(
            "kira live: the web runner serves until you stop it; \
             `--quit-after` bounds a program, not a server"
        );
        return crate::pipeline::EXIT_USAGE;
    }

    let target = match kira_project::resolve_target(Path::new(&options.path)) {
        Ok(target) => target,
        Err(error) => {
            err!("kira live: {error}");
            return crate::pipeline::EXIT_FAILURE;
        }
    };
    let Some(root) = target.root_path.clone() else {
        err!("kira live: `{}` is not inside a Kira package", options.path);
        return crate::pipeline::EXIT_USAGE;
    };
    let project_name = target
        .project_name
        .clone()
        .unwrap_or_else(|| "KiraApp".to_owned());

    let entry = match crate::pipeline::resolve_source_path(&options.path) {
        Ok(entry) => entry,
        Err(code) => return code,
    };
    let device = crate::options::Device::Web(WasmDevice::Wasm32);
    let triple = crate::foreign_libs::target_for_device(&device);

    // The served tree and the build tree are siblings under exports/, so the
    // server's root holds nothing but what a browser should fetch.
    let web_root = PathBuf::from(&root).join("exports").join("web");
    if let Err(error) = std::fs::create_dir_all(&web_root) {
        err!("kira live: {error}");
        return crate::pipeline::EXIT_FAILURE;
    }

    let compiled = match crate::pipeline::compile_verified_path(&entry, &triple) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    let ir = match crate::pipeline::entrypoint_ir("live", compiled) {
        Ok(ir) => ir,
        Err(code) => return code,
    };
    let foreign = match crate::pipeline::foreign_inputs(&entry, &ir, &device) {
        Ok(foreign) => foreign,
        Err(code) => return code,
    };
    let link = crate::pipeline::foreign_link_of(&foreign).clone();

    if let Err(error) =
        crate::wasm::build_export_app(&ir, WasmDevice::Wasm32, &link, &web_root, false)
    {
        err!("kira live: {error}");
        return crate::pipeline::EXIT_FAILURE;
    }
    if let Err(error) = web::web_project(&project_name, web_surface_requirements(WebSurface::Dom))
        .write_to(&web_root)
    {
        err!("kira live: {error}");
        return crate::pipeline::EXIT_FAILURE;
    }

    let server = match crate::serve::Server::bind(web_root.clone()) {
        Ok(server) => server,
        Err(error) => {
            err!("kira live: {error}");
            return crate::pipeline::EXIT_FAILURE;
        }
    };
    let url = format!("{}{}", server.url(), web::PAGE_FILE);
    out!("serving {} on {url}", web_root.display());
    if !crate::serve::open_browser(&url) {
        out!("could not open a browser; open {url} to run it");
    }
    out!("kira live: serving until you stop it with Ctrl-C");

    if let Err(error) = server.serve_forever() {
        err!("kira live: {error}");
        return crate::pipeline::EXIT_FAILURE;
    }
    crate::pipeline::EXIT_OK
}
