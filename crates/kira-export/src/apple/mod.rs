//! The Apple export: the merged `KiraApp.xcworkspace` and its single
//! `KiraApp.xcodeproj` with one target per Apple platform.
//!
//! This module owns the pure, build-independent parts: the per-platform metadata
//! (SDK, deployment target, device family, Info.plist), the two always-written
//! source files, the Info.plist and workspace text, and — via [`pbxproj`] — the
//! project and scheme emitters. The CLI drives the per-architecture builds, fills
//! each [`pbxproj::TargetSpec`]'s `OTHER_LDFLAGS`, and writes the tree.

pub mod pbxproj;
pub mod project;
pub mod slices;

pub use kira_manifest::platform_config::ApplePlatform;

/// The Apple Developer team used for automatic signing of device-capable targets.
///
/// It must be a team the signed-in Apple ID recognises for managed provisioning;
/// it is a scaffold default a user overrides with their own team in Xcode.
pub const DEFAULT_DEVELOPMENT_TEAM: &str = "F3U5976KWH";

/// The bundle identifier the scaffold ships; a user rebrands it in Xcode.
pub const DEFAULT_BUNDLE_ID: &str = "com.kira.live.dev";

/// The fixed build-configuration facts for one Apple platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformMeta {
    /// The product-name suffix, e.g. `iOS` in `KiraApp-iOS`.
    pub product_suffix: &'static str,
    /// The `SDKROOT`, e.g. `iphoneos`.
    pub sdkroot: &'static str,
    /// The `SUPPORTED_PLATFORMS`, e.g. `iphoneos iphonesimulator`.
    pub supported_platforms: &'static str,
    /// The deployment-target build setting name.
    pub deployment_key: &'static str,
    /// The deployment-target value.
    pub deployment_value: &'static str,
    /// `TARGETED_DEVICE_FAMILY`, when the platform sets one.
    pub device_family: Option<&'static str>,
    /// The Info.plist file name for this platform.
    pub plist_basename: &'static str,
}

/// The build-configuration facts for `platform`.
pub fn platform_meta(platform: ApplePlatform) -> PlatformMeta {
    match platform {
        ApplePlatform::Macos => PlatformMeta {
            product_suffix: "macOS",
            sdkroot: "macosx",
            supported_platforms: "macosx",
            deployment_key: "MACOSX_DEPLOYMENT_TARGET",
            deployment_value: "13.0",
            device_family: None,
            plist_basename: "macOS-Info.plist",
        },
        ApplePlatform::Ios => PlatformMeta {
            product_suffix: "iOS",
            sdkroot: "iphoneos",
            supported_platforms: "iphoneos iphonesimulator",
            deployment_key: "IPHONEOS_DEPLOYMENT_TARGET",
            deployment_value: "17.0",
            device_family: Some("1,2"),
            plist_basename: "iOS-Info.plist",
        },
        ApplePlatform::Tvos => PlatformMeta {
            product_suffix: "tvOS",
            sdkroot: "appletvos",
            supported_platforms: "appletvos appletvsimulator",
            deployment_key: "TVOS_DEPLOYMENT_TARGET",
            deployment_value: "15.0",
            device_family: Some("3"),
            plist_basename: "tvOS-Info.plist",
        },
        ApplePlatform::Visionos => PlatformMeta {
            product_suffix: "visionOS",
            sdkroot: "xros",
            supported_platforms: "xros xrsimulator",
            deployment_key: "XROS_DEPLOYMENT_TARGET",
            deployment_value: "1.0",
            device_family: Some("7"),
            plist_basename: "visionOS-Info.plist",
        },
    }
}

/// The unified `main.m` every Apple target compiles on the hybrid path.
///
/// Identical for macOS, iOS, tvOS, and visionOS: it reads the bundled
/// `KiraRunner.toml` and hands its path to `kira_live_runner_entry`, whose `mode`
/// field selects standalone playback versus live reload. Under
/// `KIRA_TARGET_UNAVAILABLE` it logs and returns rather than linking a backend
/// that was not built.
pub fn unified_main_source() -> &'static str {
    "#import <Foundation/Foundation.h>\n\
\n\
// Unified Kira Runner Entry: identical for macOS, iOS, iPadOS, tvOS and visionOS.\n\
// The KiraRunner.toml `mode` field selects standalone playback vs. live reload.\n\
extern int kira_live_runner_entry(const char *manifest_path);\n\
\n\
int main(int argc, char **argv) {\n\
    (void)argc;\n\
    (void)argv;\n\
#if defined(KIRA_TARGET_UNAVAILABLE)\n\
    @autoreleasepool {\n\
        NSLog(@\"Kira: this platform target has no native backend build yet.\");\n\
    }\n\
    return 0;\n\
#else\n\
    @autoreleasepool {\n\
        NSString *path = [[NSBundle mainBundle] pathForResource:@\"KiraRunner\" ofType:@\"toml\"];\n\
        return kira_live_runner_entry([path UTF8String]);\n\
    }\n\
#endif\n\
}\n"
}

/// The trivial translation unit a native (llvm) target compiles.
///
/// A native target's `main` arrives through `OTHER_LDFLAGS`
/// (`kira_native_app.o`); Xcode still needs one compiled source in the target or
/// it skips the link step and emits an `.app` with no executable, so this exists
/// only to make the linker run.
pub fn native_link_stub_source() -> &'static str {
    "/* Forces Xcode to link native (llvm) targets; main lives in kira_native_app.o. */\n\
typedef int kira_native_link_stub_t;\n"
}

/// The `contents.xcworkspacedata` wrapping the single generated project.
pub fn workspace_contents() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<Workspace version=\"1.0\">\n\
  <FileRef location=\"group:KiraApp.xcodeproj\"></FileRef>\n\
</Workspace>\n"
}

/// The per-platform `Info.plist` for `platform`, naming the app `product_name`.
///
/// iOS opts into ProMotion (`CADisableMinimumFrameDurationOnPhone`) because an
/// iPhone otherwise caps `CADisplayLink` at 60 Hz; macOS declares
/// `NSHighResolutionCapable`; the phone/TV/vision platforms require `iPhoneOS`
/// and declare a launch scene.
pub fn info_plist(platform: ApplePlatform, product_name: &str) -> String {
    let requires_ios = match platform {
        ApplePlatform::Macos => "",
        _ => "<key>LSRequiresIPhoneOS</key><true/>",
    };
    let launch = match platform {
        ApplePlatform::Macos => "<key>NSHighResolutionCapable</key><true/>",
        _ => {
            "<key>UILaunchScreen</key><dict/><key>UIApplicationSupportsMultipleScenes</key><false/>"
        }
    };
    let orientations = match platform {
        ApplePlatform::Ios => {
            "<key>UISupportedInterfaceOrientations</key><array><string>UIInterfaceOrientationPortrait</string><string>UIInterfaceOrientationLandscapeLeft</string><string>UIInterfaceOrientationLandscapeRight</string></array>"
        }
        _ => "",
    };
    let promotion = match platform {
        ApplePlatform::Ios => "<key>CADisableMinimumFrameDurationOnPhone</key><true/>",
        _ => "",
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\"><dict><key>CFBundleName</key><string>{product_name}</string><key>CFBundleDisplayName</key><string>{product_name}</string><key>CFBundleIdentifier</key><string>$(PRODUCT_BUNDLE_IDENTIFIER)</string><key>CFBundleExecutable</key><string>$(EXECUTABLE_NAME)</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleVersion</key><string>1</string><key>CFBundleShortVersionString</key><string>0.1.0</string><key>LSSupportsGameMode</key><false/>{requires_ios}{launch}{orientations}{promotion}</dict></plist>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ios_opts_into_promotion_and_macos_does_not() {
        let key = "CADisableMinimumFrameDurationOnPhone";
        assert!(info_plist(ApplePlatform::Ios, "Demo").contains(key));
        assert!(!info_plist(ApplePlatform::Macos, "Demo").contains(key));
    }

    #[test]
    fn macos_names_no_device_family_but_ios_does() {
        assert_eq!(platform_meta(ApplePlatform::Macos).device_family, None);
        assert_eq!(platform_meta(ApplePlatform::Ios).device_family, Some("1,2"));
    }

    #[test]
    fn the_unified_main_reads_the_runner_manifest() {
        assert!(unified_main_source().contains("kira_live_runner_entry"));
        assert!(unified_main_source().contains("pathForResource:@\"KiraRunner\""));
    }
}
