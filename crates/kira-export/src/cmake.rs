//! The Windows and Linux exports: a CMake project driven by Ninja presets.
//!
//! Both platforms share one scaffold — a `CMakeLists.txt`, a `CMakePresets.json`
//! with debug and release Ninja presets, and a host `src/main.c`. They differ
//! only in the project suffix, so the caller's tool-availability check (Windows
//! wants `cmake`; Linux wants `cmake` and `ninja`) is the only platform-specific
//! part and lives above this pure emitter.

use crate::{ExportedFile, GeneratedProject, safe_identifier};

/// The CMake presets file: one Ninja preset per configuration.
///
/// Identical for both platforms, so it is a constant rather than a per-platform
/// render. Ninja is the generator on both because it is the one CMake generator
/// present on a Linux host and installable on Windows without Visual Studio's own
/// project system.
const CMAKE_PRESETS: &str = r#"{"version":6,"configurePresets":[{"name":"debug","generator":"Ninja","binaryDir":"build/debug","cacheVariables":{"CMAKE_BUILD_TYPE":"Debug"}},{"name":"release","generator":"Ninja","binaryDir":"build/release","cacheVariables":{"CMAKE_BUILD_TYPE":"Release"}}]}
"#;

/// The host entry the scaffold links, kept minimal on purpose.
const MAIN_C: &str =
    "#include <stdio.h>\nint main(void) { puts(\"Kira platform export host\"); return 0; }\n";

/// Builds the CMake scaffold for `project_name` on `platform` (`windows` or
/// `linux`), the string that names the project and appears in its own directory.
fn scaffold(project_name: &str, platform: &str) -> GeneratedProject {
    let cmake_lists = format!(
        "cmake_minimum_required(VERSION 3.25)\nproject({identifier}_kira_{platform} C)\nadd_executable(KiraApp src/main.c)\n",
        identifier = safe_identifier(project_name),
    );
    GeneratedProject {
        files: vec![
            ExportedFile::new("CMakeLists.txt", cmake_lists),
            ExportedFile::new("CMakePresets.json", CMAKE_PRESETS),
            ExportedFile::new("src/main.c", MAIN_C),
        ],
    }
}

/// The Windows Visual Studio/CMake export scaffold for `project_name`.
pub fn windows_project(project_name: &str) -> GeneratedProject {
    scaffold(project_name, "windows")
}

/// The Linux CMake/Ninja export scaffold for `project_name`.
pub fn linux_project(project_name: &str) -> GeneratedProject {
    scaffold(project_name, "linux")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn file<'a>(project: &'a GeneratedProject, name: &str) -> &'a str {
        project
            .files
            .iter()
            .find(|file| file.path == Path::new(name))
            .map(|file| file.contents.as_str())
            .unwrap_or_else(|| panic!("no `{name}` in the scaffold"))
    }

    #[test]
    fn windows_and_linux_share_a_scaffold_but_name_their_platform() {
        let windows = windows_project("Harmony Browser");
        let linux = linux_project("Harmony Browser");
        assert!(
            file(&windows, "CMakeLists.txt").contains("project(harmony_browser_kira_windows C)")
        );
        assert!(file(&linux, "CMakeLists.txt").contains("project(harmony_browser_kira_linux C)"));
        // The presets and host entry are identical across the two platforms.
        assert_eq!(
            file(&windows, "CMakePresets.json"),
            file(&linux, "CMakePresets.json")
        );
        assert_eq!(file(&windows, "src/main.c"), file(&linux, "src/main.c"));
    }

    #[test]
    fn the_presets_define_debug_and_release_ninja_configurations() {
        let project = linux_project("Demo");
        let presets = file(&project, "CMakePresets.json");
        assert!(presets.contains("\"generator\":\"Ninja\""));
        assert!(presets.contains("\"name\":\"debug\""));
        assert!(presets.contains("\"name\":\"release\""));
    }
}
