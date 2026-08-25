//! `kira live macos|ios|tvos|visionos`: a live session in an exported Xcode app.
//!
//! The app is the runner. This flow generates the same workspace `kira export`
//! would — the real per-architecture artifacts, the embedded bundle, the
//! project — with one difference baked in before Xcode ever runs: the runner
//! manifest says `mode = "live"` and carries this session's port. The built
//! app then connects back over the live protocol, and from there the session
//! is the ordinary one: watch, rebuild, reload, relaunch.
//!
//! A relaunch re-runs `xcodebuild` first, on purpose. An exported app's native
//! half is linked into its binary, so when that half changes, restarting the
//! old binary would silently run stale code; an incremental build is what
//! makes the fresh start actually fresh.
//!
//! Devices are named explicitly (`kira live ios-device`) and refused with
//! their real prerequisite — provisioning cannot be automated from here. The
//! simulator is the default because it is the loop a developer actually sits
//! in: no signing, seconds to boot.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use kira_export::apple::{self, ApplePlatform};
use kira_manifest::platform_config::{
    BuildProfile, ExportFamily, RunnerKind, RuntimeMode, WebSurface,
};

use crate::export::Request;
use crate::live::{AppleDestination, LiveError};
use crate::progress::err;

/// How long a freshly launched simulator gets to connect.
///
/// A macOS app connects in milliseconds; a simulator boots an OS first, and a
/// cold boot has been seen to take most of this budget.
const SIMULATOR_CONNECT_GRACE: Duration = Duration::from_secs(60);

/// Runs an Apple live session for whatever `options` asked for.
pub(crate) fn run(options: &crate::live::LiveOptions) -> i32 {
    if options.apple_destination == AppleDestination::Device {
        err!(
            "kira live: running on a physical device needs signing and provisioning \
             that no toolchain can do for you: attach the device, open the generated \
             Xcode project from `kira export apple`, and press Run once to trust it"
        );
        return crate::pipeline::EXIT_FAILURE;
    }

    let platform = match options.runner {
        kira_manifest::RunnerId::Macos => ApplePlatform::Macos,
        kira_manifest::RunnerId::Ios => ApplePlatform::Ios,
        kira_manifest::RunnerId::Tvos => ApplePlatform::Tvos,
        _ => unreachable!("the caller routes only Apple runners here"),
    };

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
    let product_stem = target
        .project_name
        .clone()
        .unwrap_or_else(|| "KiraApp".to_owned());

    // One request drives both the initial workspace generation and the
    // per-reload artifact rebuilds; it describes the package exactly as
    // `kira export <family>` would have received it.
    let mut request = Request {
        family: family_for(platform),
        path: root.clone(),
        source: String::new(),
        package_name: target
            .project
            .as_ref()
            .map(|project| project.manifest.name.clone())
            .unwrap_or_else(|| product_stem.clone()),
        profile: BuildProfile::Debug,
        surface: WebSurface::Dom,
        xcode_rebuild: None,
    };
    match crate::pipeline::resolve_source_path(&request.path) {
        Ok(source) => request.source = source,
        Err(code) => return code,
    }

    let mut launcher =
        match AppleLauncher::new(platform, &root, &product_stem, &request.source.clone()) {
            Ok(launcher) => launcher,
            Err(code) => return code,
        };

    // The workspace exists before the session starts: the server binds inside
    // the supervisor, and the launcher patches the freshly generated manifest
    // with its port at launch time — the only moment both the file and the
    // address exist together.
    let exit = crate::export_apple::run(
        &request,
        &PathBuf::from(&root).join("exports"),
        &root,
        &product_stem,
    );
    if exit != crate::pipeline::EXIT_OK {
        err!("kira live: generating the Xcode workspace failed");
        return crate::pipeline::EXIT_FAILURE;
    }

    // The rebuild closure compiles for the platform's *first* slice (the host
    // architecture on macOS, the device triple elsewhere): the served bundle's
    // payloads are the hybrid manifest and bytecode, which are machine-
    // independent by design because every app carries its own native half.
    let slice = &apple::slices::slices_for(platform)[0];
    let triple = kira_native_lib_definition::TargetTriple::new(slice.arch, slice.os, slice.abi);
    let mut frontend = kira_build::FrontendSession::new();
    let watched = PathBuf::from(&options.path);
    let source_for_watch = request.source.clone();
    let runner_id = crate::export::runner_id_for(platform);
    let work = PathBuf::from(&root)
        .join("exports")
        .join("apple")
        .join("build")
        .join("live");
    let mut rebuild = move || -> Result<Option<crate::supervisor::LiveBuild>, LiveError> {
        let Ok(compiled) = crate::pipeline::runnable_path_compiled_with_frontend(
            &source_for_watch,
            &triple,
            &mut frontend,
        ) else {
            return Ok(None);
        };
        let watch_set = crate::live::watch_set(&watched, &compiled.sources);
        let Ok(ir) = crate::pipeline::entrypoint_ir("live", compiled) else {
            return Ok(None);
        };
        let foreign = crate::pipeline::foreign_inputs(
            &source_for_watch,
            &ir,
            &crate::options::Device::Cross(kira_backend_api::CrossTarget::new(
                triple.clone(),
                kira_backend_api::RelocationModel::Pic,
                kira_backend_api::Linkage::Dynamic,
            )),
        )
        .map_err(|_| LiveError::NothingToRun)?;
        let link = crate::pipeline::foreign_link_of(&foreign).clone();

        // The live app carries its native half in its own binary; the served
        // bundle is the manifest-plus-bytecode pair that binary knows how to
        // play. The object emitted here is what an Xcode relaunch relinks.
        let work_dir = work.clone();
        std::fs::create_dir_all(&work_dir).map_err(LiveError::Locate)?;
        let object_path = work_dir.join("kira_app.o");
        let sysroot =
            crate::export_apple::archives::sdk_sysroot(slice.apple_sdk).map_err(|reason| {
                LiveError::Build {
                    backend: "sdk",
                    reason,
                }
            })?;
        let options = kira_llvm_backend::NativeBuildOptions {
            module_name: "kira_app".to_owned(),
            object_path: object_path.clone(),
            executable_path: None,
            shared_library_path: None,
            archive_path: None,
            exports: Default::default(),
            ir_path: None,
            runtime_archive: object_path.clone(),
            optimize: false,
            unavailable_imports: link.unavailable_imports().to_vec(),
            foreign_link: link.clone(),
            target: kira_llvm_backend::NativeBuildTarget::new(
                kira_backend_api::NativeTarget::Cross(kira_backend_api::CrossTarget::new(
                    triple.clone(),
                    kira_backend_api::RelocationModel::Pic,
                    kira_backend_api::Linkage::Dynamic,
                )),
                Some(sysroot),
            ),
        };
        let (_object, trampolines) = kira_llvm_backend::build_hybrid_object(&ir, &options)
            .map_err(|error| LiveError::Build {
                backend: "hybrid",
                reason: error.to_string(),
            })?;
        let payloads =
            crate::export_apple::hybrid_embedded_payloads(&ir, "kira_app", &trampolines, &link)
                .map_err(|reason| LiveError::Build {
                    backend: "hybrid",
                    reason,
                })?;
        let bundle = kira_live::Bundle::build(runner_id, BuildProfile::Debug, payloads, 0)
            .map_err(|error| LiveError::Build {
                backend: "hybrid",
                reason: error.to_string(),
            })?;
        Ok(Some(crate::supervisor::LiveBuild { bundle, watch_set }))
    };

    match crate::supervisor::run(options, &mut rebuild, &mut launcher, true) {
        Ok(()) => crate::pipeline::EXIT_OK,
        Err(error) => {
            err!("kira live: {error}");
            crate::pipeline::EXIT_FAILURE
        }
    }
}

fn family_for(platform: ApplePlatform) -> ExportFamily {
    match platform {
        ApplePlatform::Macos => ExportFamily::Macos,
        ApplePlatform::Ios => ExportFamily::Ios,
        ApplePlatform::Tvos => ExportFamily::Tvos,
        ApplePlatform::Visionos => ExportFamily::Visionos,
    }
}

/// Builds and launches the exported app, and launches it again on relaunch.
struct AppleLauncher {
    platform: ApplePlatform,
    /// The export tree this session generated.
    apple_root: PathBuf,
    /// The scheme (and product) name, e.g. `webdemo-macOS`.
    scheme: String,
    /// The package's entry source, for Run-Script-free artifact refreshes.
    #[allow(dead_code)]
    source: String,
    /// The booted simulator device name, for simulator platforms.
    sim_device: Option<String>,
    /// The launched macOS child, tracked so shutdown can wait for it.
    child: Option<Child>,
}

impl AppleLauncher {
    fn new(
        platform: ApplePlatform,
        package_root: &str,
        product_stem: &str,
        source: &str,
    ) -> Result<Self, i32> {
        let meta = apple::platform_meta(platform);
        let scheme = format!("{product_stem}-{}", meta.product_suffix);
        let sim_device = (platform != ApplePlatform::Macos)
            .then(|| find_available_simulator(simulator_section(platform)))
            .flatten();
        Ok(Self {
            apple_root: PathBuf::from(package_root).join("exports").join("apple"),
            scheme,
            source: source.to_owned(),
            sim_device,
            child: None,
            platform,
        })
    }

    /// The built `.app`, per the derived-data layout xcodebuild uses.
    fn built_app(&self) -> PathBuf {
        let configuration = match self.platform {
            ApplePlatform::Macos => "Debug".to_owned(),
            other => format!("Debug-{}", sdk_for(other)),
        };
        self.apple_root
            .join("DerivedData")
            .join("Build")
            .join("Products")
            .join(configuration)
            .join(format!("{}.app", self.scheme))
    }

    /// Patches the generated runner manifest into live mode for this session.
    ///
    /// Done at every start, after any Xcode-side regeneration could have
    /// overwritten it and before the build copies it into the app.
    fn patch_manifest_for_live(&self, bound: std::net::SocketAddr) -> Result<(), LiveError> {
        let path = self.apple_root.join("Resources").join("KiraRunner.toml");
        let text = std::fs::read_to_string(&path).map_err(LiveError::Locate)?;
        let mut manifest =
            kira_manifest::platform_config::RunnerManifest::parse(&text).map_err(|error| {
                LiveError::Build {
                    backend: "manifest",
                    reason: error.to_string(),
                }
            })?;
        manifest.mode = RuntimeMode::Live;
        manifest.kind = kind_for(self.platform);
        manifest.server_host = "127.0.0.1".to_owned();
        manifest.server_port = bound.port();
        std::fs::write(&path, manifest.render()).map_err(LiveError::Locate)
    }
}

impl crate::supervisor::LaunchedRunner for AppleLauncher {
    fn connect_grace(&self) -> Duration {
        SIMULATOR_CONNECT_GRACE
    }

    fn start(&mut self, bound: std::net::SocketAddr) -> Result<(), LiveError> {
        self.patch_manifest_for_live(bound)?;

        let status = Command::new("xcodebuild")
            .arg("-project")
            .arg(self.apple_root.join("KiraApp.xcodeproj"))
            .args(["-scheme", &self.scheme])
            .args(["-configuration", "Debug"])
            .args([
                "-derivedDataPath",
                &self.apple_root.join("DerivedData").display().to_string(),
            ])
            .args(["-sdk", sdk_for(self.platform)])
            .args([
                "-destination",
                &destination_for(self.platform, self.sim_device.as_deref()),
            ])
            .arg("build")
            .arg("CODE_SIGNING_ALLOWED=NO")
            .status()
            .map_err(LiveError::Locate)?;
        if !status.success() {
            return Err(LiveError::Build {
                backend: "xcodebuild",
                reason: format!("building `{}` failed", self.scheme),
            });
        }

        let app = self.built_app();
        match self.platform {
            ApplePlatform::Macos => {
                let executable = app.join("Contents").join("MacOS").join(&self.scheme);
                let child =
                    Command::new(executable)
                        .spawn()
                        .map_err(|source| LiveError::Spawn {
                            runner: "macos",
                            path: app,
                            source,
                        })?;
                self.child = Some(child);
            }
            other => {
                let Some(device) = &self.sim_device else {
                    return Err(LiveError::Build {
                        backend: "simulator",
                        reason: format!("no available {} simulator was found", other.label()),
                    });
                };
                // Booting an already-booted simulator is fine; everything after
                // it is not, and those failures are real.
                run_simctl(&["boot", device])?;
                run_simctl(&["bootstatus", device, "-b"])?;
                run_simctl(&["install", device, &app.display().to_string()])?;
                run_simctl(&["launch", "--terminate-running-process", device, BUNDLE_ID])?;
            }
        }
        Ok(())
    }

    fn stop(&mut self, grace: Duration) -> Result<(), LiveError> {
        if let Some(mut child) = self.child.take() {
            const POLL: Duration = Duration::from_millis(5);
            let deadline = std::time::Instant::now() + grace;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if std::time::Instant::now() >= deadline => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    Ok(None) => std::thread::sleep(POLL),
                    Err(source) => return Err(LiveError::Shutdown { source }),
                }
            }
        } else if self.platform != ApplePlatform::Macos
            && let Some(device) = &self.sim_device
        {
            let _ = run_simctl(&["terminate", device, BUNDLE_ID]);
        }
        Ok(())
    }
}

/// The bundle id every generated Kira app is signed under.
const BUNDLE_ID: &str = "com.kira.live.dev";

fn kind_for(platform: ApplePlatform) -> RunnerKind {
    match platform {
        ApplePlatform::Macos => RunnerKind::XcodeMacos,
        ApplePlatform::Ios => RunnerKind::XcodeIos,
        ApplePlatform::Tvos => RunnerKind::XcodeTvos,
        ApplePlatform::Visionos => RunnerKind::XcodeVisionos,
    }
}

fn sdk_for(platform: ApplePlatform) -> &'static str {
    match platform {
        ApplePlatform::Macos => "macosx",
        ApplePlatform::Ios => "iphonesimulator",
        ApplePlatform::Tvos => "appletvsimulator",
        ApplePlatform::Visionos => "xrsimulator",
    }
}

fn destination_for(platform: ApplePlatform, device: Option<&str>) -> String {
    match platform {
        ApplePlatform::Macos => "generic/platform=macOS".to_owned(),
        ApplePlatform::Ios => match device {
            Some(name) => format!("platform=iOS Simulator,name={name}"),
            None => "generic/platform=iOS Simulator".to_owned(),
        },
        ApplePlatform::Tvos => match device {
            Some(name) => format!("platform=tvOS Simulator,name={name}"),
            None => "generic/platform=tvOS Simulator".to_owned(),
        },
        ApplePlatform::Visionos => match device {
            Some(name) => format!("platform=visionOS Simulator,name={name}"),
            None => "generic/platform=visionOS Simulator".to_owned(),
        },
    }
}

/// The `simctl list devices` section header for a platform's simulators.
fn simulator_section(platform: ApplePlatform) -> &'static str {
    match platform {
        ApplePlatform::Macos => "-- iOS --",
        ApplePlatform::Ios => "-- iOS --",
        ApplePlatform::Tvos => "-- tvOS --",
        ApplePlatform::Visionos => "-- visionOS --",
    }
}

/// The first available simulator of a section, by name.
///
/// Parsed from `simctl list devices available` rather than hardcoded: device
/// names change with every Xcode release, and a runner that names yesterday's
/// phone is broken on today's.
pub(crate) fn find_available_simulator(section: &'static str) -> Option<String> {
    let output = Command::new("xcrun")
        .args(["simctl", "list", "devices", "available"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == section && line.starts_with("--") {
            inside = true;
            continue;
        }
        if inside && line.starts_with("--") {
            return None;
        }
        if inside && let Some(name) = parse_device_name(line) {
            return Some(name);
        }
    }
    None
}

/// The device name from one `simctl list devices` row.
fn parse_device_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.contains('(') || !trimmed.ends_with(')') {
        return None;
    }
    let open = trimmed.find('(')?;
    let name = trimmed[..open].trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Runs one `simctl` verb, reporting failure precisely.
fn run_simctl(arguments: &[&str]) -> Result<(), LiveError> {
    let status = Command::new("xcrun")
        .arg("simctl")
        .args(arguments)
        .status()
        .map_err(LiveError::Locate)?;
    if status.success() {
        return Ok(());
    }
    Err(LiveError::Build {
        backend: "simctl",
        reason: format!("`simctl {}` failed with {status}", arguments[0]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_rows_yield_their_names_and_noise_does_not() {
        assert_eq!(
            parse_device_name(
                "    iPhone 17 Pro Max (477CADAE-828E-4E4F-9308-FF7708E19BBE) (Shutdown)"
            ),
            Some("iPhone 17 Pro Max".to_owned())
        );
        assert_eq!(parse_device_name(""), None);
        assert_eq!(parse_device_name("-- iOS 27.0 --"), None);
    }

    #[test]
    fn destinations_are_derived_from_the_sdk_and_a_device_when_one_exists() {
        assert_eq!(sdk_for(ApplePlatform::Macos), "macosx");
        assert_eq!(
            destination_for(ApplePlatform::Ios, Some("iPhone 17e")),
            "platform=iOS Simulator,name=iPhone 17e"
        );
        assert_eq!(
            destination_for(ApplePlatform::Ios, None),
            "generic/platform=iOS Simulator"
        );
    }

    #[test]
    fn schemes_carry_the_platform_suffix() {
        let launcher = AppleLauncher::new(
            ApplePlatform::Macos,
            "/tmp/pkg",
            "KiraApp",
            "/tmp/pkg/app/main.kira",
        )
        .expect("macOS needs no simulator");
        assert_eq!(launcher.scheme, "KiraApp-macOS");
        assert_eq!(
            launcher.built_app(),
            PathBuf::from(
                "/tmp/pkg/exports/apple/DerivedData/Build/Products/Debug/KiraApp-macOS.app"
            )
        );
    }
}
