//! AddressSanitizer runtime selection, link arguments, and deployment.

use std::path::Path;

use super::driver::stage_runtime_file;
use super::{LinkError, NativeBuildTarget};

/// Link-line additions for requested native instrumentation.
pub(super) fn link_arguments(
    llvm: &kira_toolchain::LlvmInstallation,
    target: &NativeBuildTarget,
    sanitize: crate::Sanitize,
    shared: bool,
) -> Result<Vec<String>, LinkError> {
    if sanitize == crate::Sanitize::None {
        return Ok(Vec::new());
    }
    let cross = target.target().cross();
    let os = target.target_os();
    let abi = cross.map(|cross| cross.triple().abi()).unwrap_or("");
    let asan_os = target_os(target)?;
    let runtime = if shared && matches!(asan_os, kira_toolchain::AsanTargetOs::LinuxGnu { .. }) {
        llvm.asan_preload_runtime(asan_os)
    } else {
        llvm.asan_runtime(asan_os)
    }
    .map_err(|error| LinkError::SanitizerRuntimeMissing {
        detail: error.to_string(),
    })?;
    let spelled = runtime.to_string_lossy().into_owned();
    Ok(match os {
        "macos" | "ios" | "tvos" | "visionos" | "xros" => {
            let directory = runtime
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default();
            vec![spelled, format!("-Wl,-rpath,{directory}")]
        }
        "linux" if abi != "android" && !shared => vec![
            "-Wl,--whole-archive".to_owned(),
            spelled,
            "-Wl,--no-whole-archive".to_owned(),
            "-Wl,--export-dynamic".to_owned(),
            "-lpthread".to_owned(),
            "-lrt".to_owned(),
            "-lm".to_owned(),
            "-ldl".to_owned(),
        ],
        "linux" if abi != "android" => {
            let directory = runtime
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default();
            vec![spelled, format!("-Wl,-rpath,{directory}")]
        }
        _ => vec![spelled],
    })
}

/// Stages a dynamic sanitizer runtime beside the image that loads it.
pub(super) fn stage_runtime(
    llvm: &kira_toolchain::LlvmInstallation,
    target: &NativeBuildTarget,
    sanitize: crate::Sanitize,
    shared: bool,
    output: &Path,
) -> Result<(), LinkError> {
    if sanitize == crate::Sanitize::None {
        return Ok(());
    }
    let asan_os = target_os(target)?;
    let dynamic = !matches!(asan_os, kira_toolchain::AsanTargetOs::LinuxGnu { .. }) || shared;
    if !dynamic {
        return Ok(());
    }
    let runtime =
        llvm.asan_preload_runtime(asan_os)
            .map_err(|error| LinkError::SanitizerRuntimeMissing {
                detail: error.to_string(),
            })?;
    stage_runtime_file(&runtime, output)
}

fn target_os(target: &NativeBuildTarget) -> Result<kira_toolchain::AsanTargetOs, LinkError> {
    use kira_toolchain::{ApplePlatform, AsanTargetOs};

    let cross = target.target().cross();
    let os = target.target_os();
    let abi = cross.map(|cross| cross.triple().abi()).unwrap_or("");
    let simulator = abi == "sim";
    Ok(match os {
        "macos" => AsanTargetOs::Apple {
            platform: ApplePlatform::Macos,
        },
        "ios" => AsanTargetOs::Apple {
            platform: match simulator {
                true => ApplePlatform::IosSimulator,
                false => ApplePlatform::Ios,
            },
        },
        "tvos" => AsanTargetOs::Apple {
            platform: match simulator {
                true => ApplePlatform::TvosSimulator,
                false => ApplePlatform::Tvos,
            },
        },
        "visionos" | "xros" => AsanTargetOs::Apple {
            platform: match simulator {
                true => ApplePlatform::VisionosSimulator,
                false => ApplePlatform::Visionos,
            },
        },
        "linux" if abi == "android" => AsanTargetOs::Android {
            triple: triple(cross, "linux-android"),
        },
        "android" => AsanTargetOs::Android {
            triple: triple(cross, "linux-android"),
        },
        "linux" => AsanTargetOs::LinuxGnu {
            triple: triple(cross, "unknown-linux-gnu"),
        },
        "windows" => AsanTargetOs::WindowsMsvc {
            arch: cross
                .map(|cross| cross.triple().arch().to_owned())
                .unwrap_or_else(|| std::env::consts::ARCH.to_owned()),
        },
        other => {
            return Err(LinkError::SanitizerUnsupportedTarget {
                os: other.to_owned(),
            });
        }
    })
}

fn triple(cross: Option<&kira_backend_api::CrossTarget>, suffix: &str) -> String {
    match cross {
        Some(cross) => cross.triple().to_string(),
        None => format!("{}-{suffix}", std::env::consts::ARCH),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kira_backend_api::{CrossTarget, Linkage, NativeTarget, RelocationModel};
    use kira_native_lib_definition::TargetTriple;

    use super::*;

    struct FakeLlvm {
        root: PathBuf,
        installation: kira_toolchain::LlvmInstallation,
    }

    impl FakeLlvm {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "kira-asan-link-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("lib/clang/23/lib")).expect("runtime tree");
            let installation = kira_toolchain::LlvmInstallation {
                home: root.clone(),
                bin_dir: root.join("bin"),
                lib_dir: root.join("lib"),
                llvm_config: None,
                source: kira_toolchain::DiscoverySource::EnvironmentOverride,
            };
            Self { root, installation }
        }

        fn runtime(&self, relative: &str) -> PathBuf {
            let path = self.root.join("lib/clang/23/lib").join(relative);
            std::fs::create_dir_all(path.parent().expect("runtime parent"))
                .expect("runtime parent directory");
            std::fs::write(&path, b"runtime").expect("runtime file");
            path
        }
    }

    impl Drop for FakeLlvm {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn cross(text: &str) -> NativeBuildTarget {
        NativeBuildTarget::new(
            NativeTarget::Cross(CrossTarget::new(
                TargetTriple::parse(text).expect("a valid triple"),
                RelocationModel::Pic,
                Linkage::Dynamic,
            )),
            None,
        )
    }

    #[test]
    fn every_apple_platform_selects_its_managed_asan_slice() {
        let llvm = FakeLlvm::new();
        for (triple, infix) in [
            ("aarch64-macos-none", "osx"),
            ("aarch64-ios-none", "ios"),
            ("aarch64-ios-sim", "iossim"),
            ("aarch64-tvos-none", "tvos"),
            ("aarch64-tvos-sim", "tvossim"),
            ("aarch64-xros-none", "xros"),
            ("aarch64-xros-sim", "xrossim"),
        ] {
            let runtime = llvm.runtime(&format!("darwin/libclang_rt.asan_{infix}_dynamic.dylib"));
            let arguments = link_arguments(
                &llvm.installation,
                &cross(triple),
                crate::Sanitize::Address,
                false,
            )
            .expect("platform runtime exists");
            assert_eq!(arguments[0], runtime.to_string_lossy());
            assert_eq!(
                arguments[1],
                format!(
                    "-Wl,-rpath,{}",
                    runtime.parent().expect("runtime directory").display()
                )
            );
        }
    }

    #[test]
    fn non_apple_targets_select_their_platform_link_contract() {
        let llvm = FakeLlvm::new();
        let linux = llvm.runtime("aarch64-linux-gnu/libclang_rt.asan.a");
        assert_eq!(
            link_arguments(
                &llvm.installation,
                &cross("aarch64-linux-gnu"),
                crate::Sanitize::Address,
                false,
            )
            .expect("Linux runtime exists"),
            [
                "-Wl,--whole-archive".to_owned(),
                linux.to_string_lossy().into_owned(),
                "-Wl,--no-whole-archive".to_owned(),
                "-Wl,--export-dynamic".to_owned(),
                "-lpthread".to_owned(),
                "-lrt".to_owned(),
                "-lm".to_owned(),
                "-ldl".to_owned(),
            ]
        );

        let linux_dynamic = llvm.runtime("aarch64-linux-gnu/libclang_rt.asan.so");
        let shared = link_arguments(
            &llvm.installation,
            &cross("aarch64-linux-gnu"),
            crate::Sanitize::Address,
            true,
        )
        .expect("Linux dynamic runtime exists");
        assert_eq!(shared[0], linux_dynamic.to_string_lossy());
        assert!(shared[1].starts_with("-Wl,-rpath,"));

        let windows = llvm.runtime("windows/clang_rt.asan_dynamic-x86_64.lib");
        assert_eq!(
            link_arguments(
                &llvm.installation,
                &cross("x86_64-windows-msvc"),
                crate::Sanitize::Address,
                false,
            )
            .expect("Windows runtime exists"),
            [windows.to_string_lossy().into_owned()]
        );

        let android = llvm.runtime("aarch64-linux-android/libclang_rt.asan.so");
        assert_eq!(
            link_arguments(
                &llvm.installation,
                &cross("aarch64-linux-android"),
                crate::Sanitize::Address,
                false,
            )
            .expect("Android runtime exists"),
            [android.to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn an_unknown_sanitizer_target_is_refused_by_operating_system() {
        let llvm = FakeLlvm::new();
        let error = link_arguments(
            &llvm.installation,
            &cross("aarch64-freebsd-none"),
            crate::Sanitize::Address,
            false,
        )
        .expect_err("FreeBSD has no managed runtime contract");
        assert!(
            matches!(error, LinkError::SanitizerUnsupportedTarget { ref os } if os == "freebsd"),
            "{error:?}"
        );
    }
}
