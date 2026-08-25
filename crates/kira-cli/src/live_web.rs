//! `kira live web`: build the Web export, serve it, and rebuild on change.
//!
//! A Kira program on the Web runs in a browser, and a browser needs an origin,
//! so the live loop for `web` is: compile and link exactly as the export does,
//! then serve the result from a directory that holds nothing but web assets
//! and open it. While watching, every save recompiles and replaces the served
//! `main.js`/`main.wasm`; the page picks the new module up on its next
//! reload — Kira does not inject a livereload socket into the page.

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

    // Compiles and links the current source into the served tree.
    //
    // One closure for the first build and every watched rebuild, so a
    // rebuild can never drift from what an unwatched session serves. A
    // failed rebuild keeps the last artifacts being served — killing a
    // working page over a half-typed line would make watching worse than
    // not watching.
    let rebuild = || -> Result<(), String> {
        let compiled = crate::pipeline::compile_verified_path(&entry, &triple).map_err(|_| {
            "the program does not compile; its diagnostics printed above".to_owned()
        })?;
        let ir = crate::pipeline::entrypoint_ir("live", compiled).map_err(|_| {
            "the program has no entrypoint to run, so there is nothing to serve"
                .to_owned()
        })?;
        let foreign = crate::pipeline::foreign_inputs(&entry, &ir, &device)
            .map_err(|_| "foreign import resolution failed".to_owned())?;
        let link = crate::pipeline::foreign_link_of(&foreign).clone();
        crate::wasm::build_export_app(&ir, WasmDevice::Wasm32, &link, &web_root, false)
            .map_err(|error| error.to_string())?;
        web::web_project(&project_name, web_surface_requirements(WebSurface::Dom))
            .write_to(&web_root)
            .map_err(|error| error.to_string())
    };

    if let Err(error) = rebuild() {
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

    // The server owns one thread; this thread watches and rebuilds, so a
    // save replaces main.js/main.wasm under the served root and the next
    // browser reload picks them up. The browser reloads itself only when
    // the page says so — Kira does not inject one — so say that plainly.
    if !options.watch {
        out!("kira live: serving until you stop it with Ctrl-C");
        if let Err(error) = server.serve_forever() {
            err!("kira live: {error}");
            return crate::pipeline::EXIT_FAILURE;
        }
        return crate::pipeline::EXIT_OK;
    }

    let serve_thread = std::thread::Builder::new()
        .name("kira-live-web-serve".to_owned())
        .spawn(move || {
            if let Err(error) = server.serve_forever() {
                err!("kira live web: server stopped: {error}");
            }
        });
    // The handle is deliberately dropped without joining: this loop ends only
    // on Ctrl-C (the whole process dies with its threads) or on watcher
    // failure (main returns, taking the server with it).
    let Ok(_serve_thread) = serve_thread else {
        err!("kira live: could not start the web server thread");
        return crate::pipeline::EXIT_FAILURE;
    };

    eprintln!("kira: watching for changes; refresh the browser tab to load them");
    let mut watcher = match kira_live::SourceWatcher::new(
        kira_live::WatchSet::new().root(PathBuf::from(&root)).root(PathBuf::from(&entry)),
    ) {
        Ok(watcher) => watcher,
        Err(error) => {
            err!("kira live: could not watch the package: {error}");
            return crate::pipeline::EXIT_FAILURE;
        }
    };
    out!("watching for changes");
    loop {
        let changes = watcher.wait_for(std::time::Duration::from_millis(500));
        match changes {
            Ok(changes) if changes.is_empty() => continue,
            Ok(_) => {}
            Err(error) => {
                err!("kira live: watcher failed: {error}");
                return crate::pipeline::EXIT_FAILURE;
            }
        }
        match rebuild() {
            Ok(()) => out!("rebuilt the web app; refresh the browser tab"),
            Err(reason) => err!("kira live web: rebuild failed, still serving the last good app: {reason}"),
        }
    }
}
