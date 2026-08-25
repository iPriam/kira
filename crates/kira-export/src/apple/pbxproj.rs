//! The `project.pbxproj` and `.xcscheme` emitters.
//!
//! Both are pure string builders driven by [`TargetSpec`] — one spec per Apple
//! platform the export addresses. The project object model is written in the
//! flat `objectVersion = 56` form Xcode reads: a project, one group tree, shared
//! source/resource file references, and per-target native targets with their
//! build phases and `XCBuildConfiguration`s. Nothing here touches the filesystem
//! or a toolchain, so it is tested against its own output.

use std::fmt::Write as _;
use std::path::Path;

/// One `OTHER_LDFLAGS` entry: unconditional, or gated on an SDK condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LdflagsBlock {
    /// `None` for an unconditional `OTHER_LDFLAGS`; otherwise the `sdk=<cond>`
    /// the flags apply to, e.g. `iphonesimulator*`.
    pub sdk_condition: Option<String>,
    /// The parenthesised flag list body, e.g. `"/path/to/app.a"`.
    pub value: String,
}

/// Everything one Apple target needs from the project generator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSpec {
    /// The product (and target) name, e.g. `KiraApp-iOS`.
    pub product_name: String,
    /// The bundle identifier written into the build configuration.
    pub bundle_id: String,
    /// The Info.plist path relative to the export root, e.g.
    /// `Resources/iOS-Info.plist`.
    pub info_plist_rel: String,
    /// The `SDKROOT`, e.g. `iphoneos`.
    pub sdkroot: String,
    /// The `SUPPORTED_PLATFORMS`, e.g. `iphoneos iphonesimulator`.
    pub supported_platforms: String,
    /// The deployment-target build setting name, e.g. `IPHONEOS_DEPLOYMENT_TARGET`.
    pub deployment_key: String,
    /// The deployment-target value, e.g. `17.0`.
    pub deployment_value: String,
    /// `TARGETED_DEVICE_FAMILY`, when the platform sets one.
    pub device_family: Option<String>,
    /// `ARCHS`/`VALID_ARCHS`, e.g. `arm64`.
    pub archs: String,
    /// The `OTHER_LDFLAGS` blocks, one per architecture slice.
    pub ldflags_blocks: Vec<LdflagsBlock>,
    /// The Apple Developer team used for automatic signing.
    pub development_team: String,
    /// When set, the platform could not be built: the target opens but is guarded
    /// with `KIRA_TARGET_UNAVAILABLE` and links nothing.
    pub unavailable_reason: Option<String>,
    /// A shell script run as the target's first build phase (regenerates the
    /// active SDK's Kira artifacts before compile and link).
    pub rebuild_script: Option<String>,
    /// A native (llvm) target compiles only the link stub; its `main` arrives
    /// through `OTHER_LDFLAGS`, and it copies no runner manifest or bundles.
    pub native_entry: bool,
}

/// The stable object identifiers a single target contributes.
///
/// `prefix` is the bare `T{index}` every build-file id is built from; the target
/// object itself is `{prefix}TGT`. Build files and the phases that list them
/// share the bare prefix so the two always agree.
struct Ids {
    prefix: String,
    target: String,
    product: String,
    sources_phase: String,
    frameworks_phase: String,
    resources_phase: String,
    config_list: String,
    config_debug: String,
    config_release: String,
    plist_ref: String,
}

impl Ids {
    fn for_index(index: usize) -> Ids {
        let prefix = format!("T{index}");
        Ids {
            prefix: prefix.clone(),
            target: format!("{prefix}TGT"),
            product: format!("{prefix}PRD"),
            sources_phase: format!("{prefix}SRC"),
            frameworks_phase: format!("{prefix}FRW"),
            resources_phase: format!("{prefix}RES"),
            config_list: format!("{prefix}CLST"),
            config_debug: format!("{prefix}CDBG"),
            config_release: format!("{prefix}CREL"),
            plist_ref: format!("{prefix}PLR"),
        }
    }
}

/// Shared file-reference identifiers, one per source or resource every target
/// references by the same id.
const FR_MAIN_M: &str = "FRMAINM";
const FR_RUNNER_TOML: &str = "FRRTOML";
const FR_BUNDLES: &str = "FRBUNDLES";
const FR_NATIVE_STUB: &str = "FRNSTUB";

/// Renders the whole `project.pbxproj` for `targets`.
pub fn render(targets: &[TargetSpec]) -> String {
    let ids: Vec<Ids> = (0..targets.len()).map(Ids::for_index).collect();

    // The runner manifest and bytecode bundles exist only on the hybrid path. A
    // native (llvm) export never writes them, so referencing them would show red
    // in Xcode; emit those references only when a hybrid target is present.
    let has_hybrid = targets.iter().any(|spec| !spec.native_entry);

    let mut out = String::new();
    out.push_str("// !$*UTF8*$!\n{\n");
    out.push_str("archiveVersion = 1;\nclasses = {};\nobjectVersion = 56;\nobjects = {\n");

    // Project object.
    out.push_str("PROJ = {isa = PBXProject; buildConfigurationList = PROJCLST; compatibilityVersion = \"Xcode 14.0\"; developmentRegion = en; hasScannedForEncodings = 0; knownRegions = (en, Base, ); mainGroup = GRPMAIN; productRefGroup = GRPPROD; projectDirPath = \"\"; projectRoot = \"\"; targets = (");
    for entry in &ids {
        let _ = write!(out, "{}, ", entry.target);
    }
    out.push_str("); };\n");

    // Groups.
    out.push_str("GRPMAIN = {isa = PBXGroup; children = (GRPSRC, GRPRES, GRPPROD, ); sourceTree = \"<group>\"; };\n");
    let _ = writeln!(
        out,
        "GRPSRC = {{isa = PBXGroup; path = Sources; sourceTree = \"<group>\"; children = ({FR_MAIN_M}, {FR_NATIVE_STUB}, ); }};"
    );
    out.push_str(
        "GRPRES = {isa = PBXGroup; path = Resources; sourceTree = \"<group>\"; children = (",
    );
    for entry in &ids {
        let _ = write!(out, "{}, ", entry.plist_ref);
    }
    if has_hybrid {
        let _ = write!(out, "{FR_RUNNER_TOML}, {FR_BUNDLES}, ");
    }
    out.push_str("); };\n");
    out.push_str(
        "GRPPROD = {isa = PBXGroup; name = Products; sourceTree = \"<group>\"; children = (",
    );
    for entry in &ids {
        let _ = write!(out, "{}, ", entry.product);
    }
    out.push_str("); };\n");

    // Shared file references.
    let _ = writeln!(
        out,
        "{FR_MAIN_M} = {{isa = PBXFileReference; lastKnownFileType = sourcecode.c.objc; path = main.m; sourceTree = \"<group>\"; }};"
    );
    let _ = writeln!(
        out,
        "{FR_NATIVE_STUB} = {{isa = PBXFileReference; lastKnownFileType = sourcecode.c.c; path = native_link_stub.c; sourceTree = \"<group>\"; }};"
    );
    if has_hybrid {
        let _ = writeln!(
            out,
            "{FR_RUNNER_TOML} = {{isa = PBXFileReference; lastKnownFileType = text; path = KiraRunner.toml; sourceTree = \"<group>\"; }};"
        );
        let _ = writeln!(
            out,
            "{FR_BUNDLES} = {{isa = PBXFileReference; lastKnownFileType = folder; path = Bundles; sourceTree = \"<group>\"; }};"
        );
    }

    // Per-target product refs, plist refs, and build files.
    for (spec, entry) in targets.iter().zip(&ids) {
        let _ = writeln!(
            out,
            "{} = {{isa = PBXFileReference; explicitFileType = wrapper.application; path = \"{}.app\"; includeInIndex = 0; sourceTree = BUILT_PRODUCTS_DIR; }};",
            entry.product, spec.product_name
        );
        let plist_basename = Path::new(&spec.info_plist_rel)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&spec.info_plist_rel);
        let _ = writeln!(
            out,
            "{} = {{isa = PBXFileReference; lastKnownFileType = text.plist.xml; path = {plist_basename}; sourceTree = \"<group>\"; }};",
            entry.plist_ref
        );
        if spec.native_entry {
            let _ = writeln!(
                out,
                "{}BFsrc = {{isa = PBXBuildFile; fileRef = {FR_NATIVE_STUB}; }};",
                entry.prefix
            );
        } else {
            let _ = writeln!(
                out,
                "{}BFsrc = {{isa = PBXBuildFile; fileRef = {FR_MAIN_M}; }};",
                entry.prefix
            );
            let _ = writeln!(
                out,
                "{}BFtoml = {{isa = PBXBuildFile; fileRef = {FR_RUNNER_TOML}; }};",
                entry.prefix
            );
            let _ = writeln!(
                out,
                "{}BFbnd = {{isa = PBXBuildFile; fileRef = {FR_BUNDLES}; }};",
                entry.prefix
            );
        }
    }

    // Targets and their build phases. A rebuild Run Script, when present, runs
    // first so the active SDK's artifacts are regenerated before compile and link.
    for (spec, entry) in targets.iter().zip(&ids) {
        if spec.rebuild_script.is_some() {
            let _ = writeln!(
                out,
                "{target} = {{isa = PBXNativeTarget; buildConfigurationList = {list}; buildPhases = ({target}SCRIPT, {src}, {frw}, {res}, ); buildRules = (); dependencies = (); name = \"{name}\"; productName = \"{name}\"; productReference = {product}; productType = \"com.apple.product-type.application\"; }};",
                target = entry.target,
                list = entry.config_list,
                src = entry.sources_phase,
                frw = entry.frameworks_phase,
                res = entry.resources_phase,
                name = spec.product_name,
                product = entry.product,
            );
        } else {
            let _ = writeln!(
                out,
                "{target} = {{isa = PBXNativeTarget; buildConfigurationList = {list}; buildPhases = ({src}, {frw}, {res}, ); buildRules = (); dependencies = (); name = \"{name}\"; productName = \"{name}\"; productReference = {product}; productType = \"com.apple.product-type.application\"; }};",
                target = entry.target,
                list = entry.config_list,
                src = entry.sources_phase,
                frw = entry.frameworks_phase,
                res = entry.resources_phase,
                name = spec.product_name,
                product = entry.product,
            );
        }
        if let Some(script) = &spec.rebuild_script {
            let _ = writeln!(
                out,
                "{target}SCRIPT = {{isa = PBXShellScriptBuildPhase; buildActionMask = 2147483647; name = \"Rebuild Kira ({name})\"; files = (); inputPaths = (); outputPaths = (); alwaysOutOfDate = 1; runOnlyForDeploymentPostprocessing = 0; shellPath = /bin/sh; shellScript = \"{script}\"; }};",
                target = entry.target,
                name = spec.product_name,
                script = escape_pbx_string(script),
            );
        }
        if spec.native_entry {
            let _ = writeln!(
                out,
                "{src} = {{isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = ({prefix}BFsrc, ); runOnlyForDeploymentPostprocessing = 0; }};",
                src = entry.sources_phase,
                prefix = entry.prefix,
            );
            let _ = writeln!(
                out,
                "{frw} = {{isa = PBXFrameworksBuildPhase; buildActionMask = 2147483647; files = (); runOnlyForDeploymentPostprocessing = 0; }};",
                frw = entry.frameworks_phase,
            );
            let _ = writeln!(
                out,
                "{res} = {{isa = PBXResourcesBuildPhase; buildActionMask = 2147483647; files = (); runOnlyForDeploymentPostprocessing = 0; }};",
                res = entry.resources_phase,
            );
        } else {
            let _ = writeln!(
                out,
                "{src} = {{isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = ({prefix}BFsrc, ); runOnlyForDeploymentPostprocessing = 0; }};",
                src = entry.sources_phase,
                prefix = entry.prefix,
            );
            let _ = writeln!(
                out,
                "{frw} = {{isa = PBXFrameworksBuildPhase; buildActionMask = 2147483647; files = (); runOnlyForDeploymentPostprocessing = 0; }};",
                frw = entry.frameworks_phase,
            );
            let _ = writeln!(
                out,
                "{res} = {{isa = PBXResourcesBuildPhase; buildActionMask = 2147483647; files = ({prefix}BFtoml, {prefix}BFbnd, ); runOnlyForDeploymentPostprocessing = 0; }};",
                res = entry.resources_phase,
                prefix = entry.prefix,
            );
        }
    }

    // Project-level configuration list and configurations.
    out.push_str("PROJCLST = {isa = XCConfigurationList; buildConfigurations = (PROJCDBG, PROJCREL, ); defaultConfigurationIsVisible = 0; defaultConfigurationName = Release; };\n");
    out.push_str("PROJCDBG = {isa = XCBuildConfiguration; buildSettings = { ENABLE_USER_SCRIPT_SANDBOXING = NO; }; name = Debug; };\n");
    out.push_str("PROJCREL = {isa = XCBuildConfiguration; buildSettings = { ENABLE_USER_SCRIPT_SANDBOXING = NO; }; name = Release; };\n");

    // Per-target configuration lists and build configurations.
    for (spec, entry) in targets.iter().zip(&ids) {
        let _ = writeln!(
            out,
            "{list} = {{isa = XCConfigurationList; buildConfigurations = ({debug}, {release}, ); defaultConfigurationIsVisible = 0; defaultConfigurationName = Release; }};",
            list = entry.config_list,
            debug = entry.config_debug,
            release = entry.config_release,
        );
        write_build_config(&mut out, &entry.config_debug, "Debug", spec);
        write_build_config(&mut out, &entry.config_release, "Release", spec);
    }

    out.push_str("};\nrootObject = PROJ;\n}\n");
    out
}

/// Escapes a string for a double-quoted pbxproj value.
fn escape_pbx_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

/// Writes one `XCBuildConfiguration` for a target.
fn write_build_config(out: &mut String, id: &str, name: &str, spec: &TargetSpec) {
    let _ = write!(
        out,
        "{id} = {{isa = XCBuildConfiguration; buildSettings = {{ "
    );
    let _ = write!(
        out,
        "PRODUCT_NAME = \"{}\"; PRODUCT_BUNDLE_IDENTIFIER = \"{}\"; ",
        spec.product_name, spec.bundle_id
    );
    let _ = write!(
        out,
        "SDKROOT = {}; SUPPORTED_PLATFORMS = \"{}\"; {} = {}; ",
        spec.sdkroot, spec.supported_platforms, spec.deployment_key, spec.deployment_value
    );
    if let Some(family) = &spec.device_family {
        let _ = write!(out, "TARGETED_DEVICE_FAMILY = \"{family}\"; ");
    }
    let _ = write!(
        out,
        "ARCHS = {archs}; VALID_ARCHS = {archs}; ONLY_ACTIVE_ARCH = YES; ",
        archs = spec.archs
    );
    let _ = write!(
        out,
        "WRAPPER_EXTENSION = app; PRODUCT_BUNDLE_PACKAGE_TYPE = APPL; GENERATE_INFOPLIST_FILE = NO; INFOPLIST_FILE = \"{}\"; ",
        spec.info_plist_rel
    );
    out.push_str(
        "CLANG_ENABLE_MODULES = YES; DEAD_CODE_STRIPPING = NO; ALWAYS_SEARCH_USER_PATHS = NO; ",
    );

    if spec.unavailable_reason.is_some() {
        out.push_str("GCC_PREPROCESSOR_DEFINITIONS = (\"KIRA_TARGET_UNAVAILABLE=1\", ); CODE_SIGNING_ALLOWED = NO; OTHER_LDFLAGS = (); ");
    } else {
        let _ = write!(
            out,
            "CODE_SIGN_STYLE = Automatic; DEVELOPMENT_TEAM = {}; CODE_SIGNING_ALLOWED = YES; ",
            spec.development_team
        );
        out.push_str("\"CODE_SIGNING_REQUIRED[sdk=*simulator*]\" = NO; \"CODE_SIGN_IDENTITY[sdk=iphoneos*]\" = \"Apple Development\"; \"CODE_SIGN_IDENTITY[sdk=appletvos*]\" = \"Apple Development\"; \"CODE_SIGN_IDENTITY[sdk=xros*]\" = \"Apple Development\"; \"CODE_SIGN_IDENTITY[sdk=macosx*]\" = \"-\"; ");
        for block in &spec.ldflags_blocks {
            match &block.sdk_condition {
                Some(condition) => {
                    let _ = write!(
                        out,
                        "\"OTHER_LDFLAGS[sdk={condition}]\" = ({}); ",
                        block.value
                    );
                }
                None => {
                    let _ = write!(out, "OTHER_LDFLAGS = ({}); ", block.value);
                }
            }
        }
    }
    let _ = writeln!(out, "}}; name = {name}; }};");
}

/// Renders the shared scheme for the target at `target_index`.
///
/// The scheme wires Build, Test, Launch, Profile, Analyze, and Archive actions to
/// the target's built `.app`, so `xcodebuild -scheme` and Xcode's own picker both
/// resolve it and Product > Profile has a runnable to launch.
pub fn scheme_xml(product_name: &str, target_index: usize) -> String {
    let reference = format!(
        "<BuildableReference BuildableIdentifier=\"primary\" BlueprintIdentifier=\"T{target_index}TGT\" BuildableName=\"{product_name}.app\" BlueprintName=\"{product_name}\" ReferencedContainer=\"container:KiraApp.xcodeproj\"></BuildableReference>"
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Scheme LastUpgradeVersion="1600" version="1.7">
  <BuildAction parallelizeBuildables="YES" buildImplicitDependencies="YES">
    <BuildActionEntries><BuildActionEntry buildForTesting="YES" buildForRunning="YES" buildForProfiling="YES" buildForArchiving="YES" buildForAnalyzing="YES">{reference}</BuildActionEntry></BuildActionEntries>
  </BuildAction>
  <TestAction buildConfiguration="Debug" selectedDebuggerIdentifier="Xcode.DebuggerFoundation.Debugger.LLDB" selectedLauncherIdentifier="Xcode.DebuggerFoundation.Launcher.LLDB" shouldUseLaunchSchemeArgsEnv="YES"></TestAction>
  <LaunchAction buildConfiguration="Debug" selectedDebuggerIdentifier="Xcode.DebuggerFoundation.Debugger.LLDB" selectedLauncherIdentifier="Xcode.DebuggerFoundation.Launcher.LLDB" launchStyle="0" useCustomWorkingDirectory="NO" ignoresPersistentStateOnLaunch="NO" debugDocumentVersioning="YES" debugServiceExtension="internal" allowLocationSimulation="YES"><BuildableProductRunnable runnableDebuggingMode="0">{reference}</BuildableProductRunnable></LaunchAction>
  <ProfileAction buildConfiguration="Release" shouldUseLaunchSchemeArgsEnv="YES" savedToolIdentifier="" useCustomWorkingDirectory="NO" debugDocumentVersioning="YES"><BuildableProductRunnable runnableDebuggingMode="0">{reference}</BuildableProductRunnable></ProfileAction>
  <AnalyzeAction buildConfiguration="Debug"></AnalyzeAction>
  <ArchiveAction buildConfiguration="Release" revealArchiveInOrganizer="YES"></ArchiveAction>
</Scheme>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hybrid_spec(product: &str, sdkroot: &str, plist: &str) -> TargetSpec {
        TargetSpec {
            product_name: product.to_owned(),
            bundle_id: "com.kira.live.dev".to_owned(),
            info_plist_rel: plist.to_owned(),
            sdkroot: sdkroot.to_owned(),
            supported_platforms: "iphoneos iphonesimulator".to_owned(),
            deployment_key: "IPHONEOS_DEPLOYMENT_TARGET".to_owned(),
            deployment_value: "17.0".to_owned(),
            device_family: Some("1,2".to_owned()),
            archs: "arm64".to_owned(),
            ldflags_blocks: Vec::new(),
            development_team: "F3U5976KWH".to_owned(),
            unavailable_reason: None,
            rebuild_script: None,
            native_entry: false,
        }
    }

    #[test]
    fn one_target_per_spec_with_shared_sources_and_sdk_scoped_ldflags() {
        let mut macos = hybrid_spec("KiraApp-macOS", "macosx", "Resources/macOS-Info.plist");
        macos.ldflags_blocks = vec![LdflagsBlock {
            sdk_condition: None,
            value: "\"/tmp/x.a\"".to_owned(),
        }];
        let mut ios = hybrid_spec("KiraApp-iOS", "iphoneos", "Resources/iOS-Info.plist");
        ios.ldflags_blocks = vec![
            LdflagsBlock {
                sdk_condition: Some("iphoneos*".to_owned()),
                value: "\"/tmp/dev.a\"".to_owned(),
            },
            LdflagsBlock {
                sdk_condition: Some("iphonesimulator*".to_owned()),
                value: "\"/tmp/sim.a\"".to_owned(),
            },
        ];
        let project = render(&[macos, ios]);
        assert!(project.contains("path = main.m"));
        assert!(project.contains("lastKnownFileType = folder; path = Bundles"));
        assert!(project.contains("T0TGT"));
        assert!(project.contains("T1TGT"));
        assert!(project.contains("\"OTHER_LDFLAGS[sdk=iphonesimulator*]\" = (\"/tmp/sim.a\")"));
        assert!(!project.contains("extern int kira_live_runner_entry"));
    }

    #[test]
    fn a_native_entry_target_compiles_the_stub_and_copies_no_runner_resources() {
        let mut spec = hybrid_spec("KiraApp-iOS", "iphoneos", "Resources/iOS-Info.plist");
        spec.native_entry = true;
        spec.ldflags_blocks = vec![LdflagsBlock {
            sdk_condition: Some("iphonesimulator*".to_owned()),
            value: "\"/tmp/kira_native_app.o\"".to_owned(),
        }];
        let project = render(&[spec]);
        assert!(project.contains("path = native_link_stub.c"));
        assert!(project.contains("T0BFsrc = {isa = PBXBuildFile; fileRef = FRNSTUB"));
        assert!(project.contains("files = (T0BFsrc, ); runOnlyForDeploymentPostprocessing = 0; }"));
        assert!(!project.contains("T0BFtoml"));
        assert!(!project.contains("T0BFbnd"));
        assert!(!project.contains("path = KiraRunner.toml"));
        assert!(!project.contains("path = Bundles"));
        assert!(project.contains("path = iOS-Info.plist"));
    }

    #[test]
    fn an_unavailable_target_opens_but_links_nothing() {
        let mut spec = hybrid_spec("KiraApp-tvOS", "appletvos", "Resources/tvOS-Info.plist");
        spec.unavailable_reason = Some("arch tvos-device failed".to_owned());
        let project = render(&[spec]);
        assert!(project.contains("KIRA_TARGET_UNAVAILABLE=1"));
        assert!(project.contains("CODE_SIGNING_ALLOWED = NO"));
    }

    #[test]
    fn the_scheme_wires_a_profile_action_to_the_built_app() {
        let scheme = scheme_xml("KiraApp-iOS", 1);
        let profile_open = scheme.find("<ProfileAction").expect("a profile action");
        let profile_close = scheme
            .find("</ProfileAction>")
            .expect("a profile action close");
        let profile = &scheme[profile_open..profile_close];
        assert!(profile.contains("BuildableProductRunnable"));
        assert!(profile.contains("BuildableName=\"KiraApp-iOS.app\""));
        assert!(scheme.contains("buildForProfiling=\"YES\""));
    }
}
