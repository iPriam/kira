//! `kira live windows|linux`: the scaffold audit for runners with no loop yet.
//!
//! A live session needs a runner that can host an app and connect back. On a
//! machine that cannot run the target — or where the runner is still a CMake
//! scaffold — there is no honest session to start, and pretending otherwise
//! would be worse than refusing: this module generates exactly what `kira
//! export` would, audits the tools a local build would need, and says
//! precisely which piece of the loop does not exist here yet.

use std::path::PathBuf;

use crate::progress::{err, out};

/// Audits one platform's scaffold and reports what its live loop is missing.
pub(crate) fn run(options: &crate::live::LiveOptions) -> i32 {
    let platform = options.runner.label();
    let target = match kira_project::resolve_target(std::path::Path::new(&options.path)) {
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
    let exports_root = PathBuf::from(&root).join("exports");

    let (directory, project, required_tools) = match options.runner {
        kira_manifest::RunnerId::Windows => (
            exports_root.join("windows"),
            kira_export::cmake::windows_project(&project_name),
            vec!["cmake"],
        ),
        kira_manifest::RunnerId::Linux => (
            exports_root.join("linux"),
            kira_export::cmake::linux_project(&project_name),
            vec!["cmake", "ninja"],
        ),
        other => {
            err!(
                "kira live: `{}` has no live runner in this build",
                other.label()
            );
            return crate::pipeline::EXIT_FAILURE;
        }
    };

    if let Err(error) = project.write_to(&directory) {
        err!("kira live: {error}");
        return crate::pipeline::EXIT_FAILURE;
    }
    out!(
        "exported {} CMake scaffold at {}",
        platform,
        directory.display()
    );

    let missing: Vec<&str> = required_tools
        .iter()
        .copied()
        .filter(|tool| !crate::export::command_on_path(tool))
        .collect();
    match (options.runner, missing.as_slice()) {
        (kira_manifest::RunnerId::Windows, _) => err!(
            "kira live: Windows runners need Visual Studio tools on a Windows host; \
             this host can only generate the export scaffold"
        ),
        (_, []) => err!(
            "kira live: the {platform} CMake/Ninja scaffold exists, but cross-host \
             live launch is not available on this host — build and run the \
             scaffold to exercise it locally"
        ),
        (_, missing) => err!(
            "kira live: {} not found on PATH; install {} before building the scaffold",
            missing.join(" and "),
            if missing.len() == 1 { "it" } else { "them" },
        ),
    }
    crate::pipeline::EXIT_FAILURE
}
