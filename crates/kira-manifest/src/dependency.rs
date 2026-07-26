//! Dependency specifications: registry, path, and git sources.

/// A single dependency entry in a `package.kira` manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencySpec {
    pub name: String,
    pub source: DependencySource,
}

/// Where a dependency comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySource {
    Registry(RegistrySource),
    Path(PathSource),
    Git(GitSource),
}

impl DependencySource {
    /// Returns the local-path source, or `None` for deferred source kinds.
    pub fn as_path(&self) -> Option<&PathSource> {
        match self {
            Self::Path(source) => Some(source),
            Self::Registry(_) | Self::Git(_) => None,
        }
    }

    /// Whether this dependency can be resolved from the local filesystem.
    pub fn is_path(&self) -> bool {
        self.as_path().is_some()
    }
}

/// A registry dependency pinned by version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrySource {
    pub version: String,
}

/// A local path dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSource {
    pub path: String,
}

/// A git dependency, optionally pinned by rev or tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSource {
    pub url: String,
    pub rev: Option<String>,
    pub tag: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeclarationError, load_declaration};

    const EDITOR_MANIFEST: &str = r#"Package editor {
    let version = "0.1.0"
    let kira = "0.1.0"
    let kind = PackageKind.App
    let defaults = Defaults { executionMode: Backend.Llvm, buildTarget: BuildTarget.Host }
    let assets = ["Assets", "hosami"]
    let dependencies = [
        Dependency { name: "Editor", path: "../../modules/editor" },
        Dependency { name: "KiraGraphics", path: "../../../kira-graphics" },
        Dependency { name: "Core", path: "../../modules/core" },
        Dependency { name: "Graphics", path: "../../modules/graphics" }
    ]
}
"#;

    const KIRA_GRAPHICS_MANIFEST: &str = r#"Package KiraGraphics {
    let version = "0.1.0"
    let kira = "0.1.0"
    let kind = PackageKind.Library
    let moduleRoot = "KiraGraphics"
    let defaults = Defaults { executionMode: Backend.Llvm, buildTarget: BuildTarget.Host }
    let nativeLibraries = [
        NativeLibrary {
            name: "sokol",
            linkMode: LinkMode.Static,
            headers: Headers { entrypoint: "NativeLibs/Sokol/sokol_bindings.h", includeDirs: ["NativeLibs/Sokol"], defines: ["SOKOL_NO_ENTRY"] },
            sources: ["NativeLibs/Sokol/sokol_impl.c"],
            autobind: Autobind { module: "sokol", headers: ["NativeLibs/Sokol/sokol_app.h", "NativeLibs/Sokol/sokol_gfx.h", "NativeLibs/Sokol/sokol_glue.h"], mode: AutobindMode.AllPublic },
            nativeTargets: [
                NativeTarget { triple: "aarch64-macos-none", staticLib: "NativeLibs/../generated/native/aarch64-macos/libsokol.a", defines: ["SOKOL_GLCORE"], frameworks: ["Foundation", "AppKit", "QuartzCore", "OpenGL"] },
                NativeTarget { triple: "aarch64-ios-none", staticLib: "NativeLibs/../generated/native/aarch64-ios/libsokol.a", defines: ["SOKOL_METAL"], frameworks: ["Foundation", "UIKit", "QuartzCore", "Metal", "MetalKit"] },
                NativeTarget { triple: "aarch64-ios-simulator", staticLib: "NativeLibs/../generated/native/aarch64-ios-simulator/libsokol.a", defines: ["SOKOL_METAL"], frameworks: ["Foundation", "UIKit", "QuartzCore", "Metal", "MetalKit"] },
                NativeTarget { triple: "aarch64-tvos-none", staticLib: "NativeLibs/../generated/native/aarch64-tvos/libsokol.a", defines: ["SOKOL_METAL"], frameworks: ["Foundation", "UIKit", "QuartzCore", "Metal", "MetalKit"] },
                NativeTarget { triple: "aarch64-tvos-simulator", staticLib: "NativeLibs/../generated/native/aarch64-tvos-simulator/libsokol.a", defines: ["SOKOL_METAL"], frameworks: ["Foundation", "UIKit", "QuartzCore", "Metal", "MetalKit"] },
                NativeTarget { triple: "aarch64-xros-none", staticLib: "NativeLibs/../generated/native/aarch64-xros/libsokol.a", defines: ["SOKOL_METAL"], frameworks: ["Foundation", "UIKit", "QuartzCore", "Metal", "MetalKit"] },
                NativeTarget { triple: "aarch64-xros-simulator", staticLib: "NativeLibs/../generated/native/aarch64-xros-simulator/libsokol.a", defines: ["SOKOL_METAL"], frameworks: ["Foundation", "UIKit", "QuartzCore", "Metal", "MetalKit"] },
                NativeTarget { triple: "x86_64-linux-gnu", staticLib: "NativeLibs/../generated/native/x86_64-linux-gnu/libsokol.a", defines: ["SOKOL_GLCORE"], systemLibs: ["X11", "Xi", "Xcursor", "GL", "dl", "pthread", "m"] },
                NativeTarget { triple: "x86_64-windows-msvc", staticLib: "NativeLibs/../generated/native/x86_64-windows-msvc/sokol.lib", defines: ["SOKOL_GLCORE"], systemLibs: ["opengl32"] },
                NativeTarget { triple: "wasm32-emscripten-unknown", staticLib: "NativeLibs/../generated/native/wasm32-emscripten/libsokol.a", compilerFlags: ["--use-port=emdawnwebgpu"], linkerFlags: ["--use-port=emdawnwebgpu"] }
            ],
        },
        NativeLibrary {
            name: "vulkan",
            linkMode: LinkMode.Dynamic,
            headers: Headers { entrypoint: "${VULKAN_SDK}/Include/vulkan/vulkan.h", includeDirs: ["${VULKAN_SDK}/Include"], defines: ["VK_USE_PLATFORM_WIN32_KHR"] },
            autobind: Autobind { module: "vulkan", headers: ["${VULKAN_SDK}/Include/vulkan/vulkan.h", "${VULKAN_SDK}/Include/vulkan/vulkan_core.h", "${VULKAN_SDK}/Include/vulkan/vulkan_win32.h"], mode: AutobindMode.AllPublic, profile: AutobindProfile.Vulkan },
            nativeTargets: [
                NativeTarget { triple: "x86_64-windows-msvc", dynamicLib: "" },
                NativeTarget { triple: "x86_64-linux-gnu", dynamicLib: "" },
                NativeTarget { triple: "aarch64-macos-none", dynamicLib: "" },
                NativeTarget { triple: "wasm32-emscripten-unknown", dynamicLib: "" }
            ],
        },
        NativeLibrary {
            name: "kira_metal",
            linkMode: LinkMode.Dynamic,
            nativeTargets: [
                NativeTarget { triple: "aarch64-macos-none", frameworks: ["Foundation", "QuartzCore", "Metal", "AppKit"], systemLibs: ["objc"] },
                NativeTarget { triple: "aarch64-ios-none", frameworks: ["Foundation", "QuartzCore", "Metal", "UIKit"], systemLibs: ["objc"] },
                NativeTarget { triple: "wasm32-emscripten-unknown", dynamicLib: "", linkerFlags: ["-sERROR_ON_UNDEFINED_SYMBOLS=0"] }
            ],
        }
    ]
}
"#;

    #[test]
    fn reads_editor_dependencies_and_defaults() {
        let manifest = load_declaration(EDITOR_MANIFEST).unwrap();
        let actual = manifest
            .dependencies
            .iter()
            .map(|dependency| {
                let source = dependency.source.as_path().unwrap();
                (dependency.name.as_str(), source.path.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            [
                ("Editor", "../../modules/editor"),
                ("KiraGraphics", "../../../kira-graphics"),
                ("Core", "../../modules/core"),
                ("Graphics", "../../modules/graphics"),
            ]
        );
        assert_eq!(manifest.execution_mode, "llvm");
        assert_eq!(manifest.build_target, "host");
    }

    #[test]
    fn reads_kira_graphics_native_libraries() {
        // The corpus manifest verbatim, all three libraries: the static one with
        // headers, sources, autobind, and ten target rows; the dynamic one whose
        // every row is `dynamicLib: ""`; and the one that is frameworks only.
        let manifest = load_declaration(KIRA_GRAPHICS_MANIFEST).unwrap();
        assert!(manifest.dependencies.is_empty());
        assert_eq!(manifest.execution_mode, "llvm");
        assert_eq!(manifest.build_target, "host");

        let names: Vec<&str> = manifest
            .native_libraries
            .iter()
            .map(kira_native_lib_definition::NativeLibrarySpec::name)
            .collect();
        assert_eq!(names, ["sokol", "vulkan", "kira_metal"]);

        let sokol = &manifest.native_libraries[0];
        assert_eq!(sokol.targets().len(), 10);
        assert_eq!(sokol.sources(), ["NativeLibs/Sokol/sokol_impl.c"]);
        assert_eq!(
            sokol.autobind().expect("an autobind record").headers.len(),
            3
        );
        let wasm = sokol
            .targets()
            .iter()
            .find(|row| row.triple().os() == "emscripten")
            .expect("the wasm row");
        assert_eq!(wasm.attributes().linker_flags, ["--use-port=emdawnwebgpu"]);
    }

    #[test]
    fn reads_registry_and_git_dependencies() {
        let text = r#"Package Sources {
            let dependencies = [
                Dependency { name: "Registry", version: "1.2.3", tokenEnv: "TOKEN" },
                Dependency { name: "Git", url: "https://example.test/repo.git", rev: "abc", tag: "v1", shallow: true }
            ]
        }"#;
        let manifest = load_declaration(text).unwrap();
        assert_eq!(
            manifest.dependencies[0].source,
            DependencySource::Registry(RegistrySource {
                version: "1.2.3".to_owned()
            })
        );
        assert_eq!(
            manifest.dependencies[1].source,
            DependencySource::Git(GitSource {
                url: "https://example.test/repo.git".to_owned(),
                rev: Some("abc".to_owned()),
                tag: Some("v1".to_owned()),
            })
        );
        assert!(!manifest.dependencies[0].source.is_path());
        assert!(!manifest.dependencies[1].source.is_path());
    }

    #[test]
    fn refuses_dependencies_without_a_source() {
        let missing_source = r#"Package p {
            let dependencies = [Dependency { name: "Missing", token: "x" }]
        }"#;
        let malformed_entry = r#"Package p {
            let dependencies = [Dependency { name: "Broken", path "../broken" }]
        }"#;
        for text in [missing_source, malformed_entry] {
            assert_eq!(
                load_declaration(text).unwrap_err(),
                DeclarationError::MalformedValue {
                    key: "dependencies".to_owned()
                }
            );
        }
    }

    #[test]
    fn refuses_unknown_default_cases() {
        let unknown_mode =
            "Package p {\n let defaults = Defaults { executionMode: Backend.Other }\n}";
        let unknown_target =
            "Package p {\n let defaults = Defaults { buildTarget: BuildTarget.Other }\n}";
        for text in [unknown_mode, unknown_target] {
            assert_eq!(
                load_declaration(text).unwrap_err(),
                DeclarationError::MalformedValue {
                    key: "defaults".to_owned()
                }
            );
        }
    }
}
