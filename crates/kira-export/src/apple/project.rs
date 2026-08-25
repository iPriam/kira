//! The Apple export as a whole: every file the generated tree is made of.
//!
//! The per-platform facts ([`platform_meta`]), the two source files, the
//! plists, and — via [`pbxproj`] — the project and schemes are pure text. This
//! module is where they compose into one [`GeneratedProject`]: given one
//! [`TargetSpec`] per platform plus the rendered `KiraRunner.toml`, it names
//! every file under the export root, so the CLI's job is only to build the
//! artifacts those files point at and write the tree out.

use std::path::Path;

use crate::apple::{self, ApplePlatform, pbxproj, unified_main_source};
use crate::{ExportedFile, GeneratedProject};

/// One platform's spec paired with the platform it was derived from.
///
/// The plist emitter needs the platform (orientation keys, launch screen) and
/// the project renderer needs the resolved build settings; carrying them
/// together means neither generator re-derives the other's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformSpec {
    /// The platform this target addresses.
    pub platform: ApplePlatform,
    /// The resolved build settings for the target.
    pub spec: pbxproj::TargetSpec,
}

impl PlatformSpec {
    /// The target's product name, e.g. `KiraApp-iOS`.
    pub fn product_name(&self) -> &str {
        &self.spec.product_name
    }
}

/// Builds a hybrid target's spec: the platform facts plus what the caller
/// cross-built for each slice.
///
/// The returned spec is healthy and unlinked — [`Self::with_ldflags`],
/// [`Self::with_rebuild_script`], and [`Self::marked_unavailable`] fill in
/// what the export built (or failed to build) for each slice.
pub fn hybrid_spec(
    platform: ApplePlatform,
    product_stem: &str,
    bundle_id: &str,
    archs: &str,
) -> PlatformSpec {
    let meta = apple::platform_meta(platform);
    let product_name = format!("{product_stem}-{}", meta.product_suffix);
    PlatformSpec {
        platform,
        spec: pbxproj::TargetSpec {
            product_name,
            bundle_id: bundle_id.to_owned(),
            info_plist_rel: format!("Resources/{}", meta.plist_basename),
            sdkroot: meta.sdkroot.to_owned(),
            supported_platforms: meta.supported_platforms.to_owned(),
            deployment_key: meta.deployment_key.to_owned(),
            deployment_value: meta.deployment_value.to_owned(),
            device_family: meta.device_family.map(str::to_owned),
            archs: archs.to_owned(),
            ldflags_blocks: Vec::new(),
            development_team: apple::DEFAULT_DEVELOPMENT_TEAM.to_owned(),
            unavailable_reason: None,
            rebuild_script: None,
            native_entry: false,
        },
    }
}

impl PlatformSpec {
    /// Attaches one `OTHER_LDFLAGS` block per architecture slice.
    pub fn with_ldflags(mut self, blocks: Vec<pbxproj::LdflagsBlock>) -> Self {
        self.spec.ldflags_blocks = blocks;
        self
    }

    /// Attaches the Run Script that rebuilds this SDK's Kira artifacts.
    pub fn with_rebuild_script(mut self, script: String) -> Self {
        self.spec.rebuild_script = Some(script);
        self
    }

    /// Marks the target openable-but-unbuilt, naming why.
    pub fn marked_unavailable(mut self, reason: String) -> Self {
        self.spec.unavailable_reason = Some(reason);
        self
    }
}

/// Every file of the export tree, in emission order.
///
/// `runner_manifest_toml` is the rendered `KiraRunner.toml`; a native-only
/// export (every target `native_entry`) embeds no bundles and so writes no
/// runner manifest — passing `None` omits both it and the Bundles folder
/// reference, exactly as [`pbxproj::render`] omits their project references.
pub fn project_files(
    platforms: &[PlatformSpec],
    runner_manifest_toml: Option<&str>,
) -> GeneratedProject {
    let specs: Vec<pbxproj::TargetSpec> =
        platforms.iter().map(|entry| entry.spec.clone()).collect();
    let has_hybrid = specs.iter().any(|spec| !spec.native_entry);
    let mut files = vec![
        ExportedFile::new("Sources/main.m", unified_main_source()),
        ExportedFile::new(
            "Sources/native_link_stub.c",
            apple::native_link_stub_source(),
        ),
    ];

    for entry in platforms {
        let meta = apple::platform_meta(entry.platform);
        files.push(ExportedFile::new(
            Path::new("Resources").join(meta.plist_basename),
            apple::info_plist(entry.platform, &entry.spec.product_name),
        ));
    }
    if has_hybrid {
        let manifest = runner_manifest_toml.expect("a hybrid export carries a runner manifest");
        files.push(ExportedFile::new("Resources/KiraRunner.toml", manifest));
    }

    files.push(ExportedFile::new(
        "KiraApp.xcodeproj/project.pbxproj",
        pbxproj::render(&specs),
    ));

    let schemes_dir = Path::new("KiraApp.xcodeproj").join("xcshareddata/xcschemes");
    for (index, entry) in platforms.iter().enumerate() {
        files.push(ExportedFile::new(
            schemes_dir.join(format!("{}.xcscheme", entry.spec.product_name)),
            pbxproj::scheme_xml(&entry.spec.product_name, index),
        ));
    }

    files.push(ExportedFile::new(
        "KiraApp.xcworkspace/contents.xcworkspacedata",
        apple::workspace_contents(),
    ));
    GeneratedProject { files }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_manifest::ApplePlatform;

    fn macos_spec() -> PlatformSpec {
        hybrid_spec(
            ApplePlatform::Macos,
            "KiraApp",
            "com.kira.live.dev",
            "arm64",
        )
        .with_ldflags(vec![pbxproj::LdflagsBlock {
            sdk_condition: None,
            value: "\"-Wl,-force_load,/tmp/libkira_app_runner.a\"".to_owned(),
        }])
    }

    #[test]
    fn a_hybrid_project_carries_every_file_the_tree_needs() {
        let project = project_files(&[macos_spec()], Some("[runtime]\nmode = \"live\"\n"));
        let paths: Vec<&Path> = project
            .files
            .iter()
            .map(|file| file.path.as_path())
            .collect();
        assert!(paths.contains(&Path::new("Sources/main.m")));
        assert!(paths.contains(&Path::new("Resources/KiraRunner.toml")));
        assert!(paths.contains(&Path::new("Resources/macOS-Info.plist")));
        assert!(paths.contains(&Path::new("KiraApp.xcodeproj/project.pbxproj")));
        assert!(paths.contains(&Path::new(
            "KiraApp.xcodeproj/xcshareddata/xcschemes/KiraApp-macOS.xcscheme"
        )));
        assert!(paths.contains(&Path::new("KiraApp.xcworkspace/contents.xcworkspacedata")));

        let pbx = &project
            .files
            .iter()
            .find(|file| file.path == Path::new("KiraApp.xcodeproj/project.pbxproj"))
            .expect("the project file")
            .contents;
        assert!(pbx.contains("-Wl,-force_load,/tmp/libkira_app_runner.a"));
        assert!(pbx.contains("path = KiraRunner.toml"));
    }

    #[test]
    fn an_unavailable_target_still_gets_its_plist_and_scheme() {
        let mut spec = macos_spec();
        spec.spec.unavailable_reason = Some("arch macos failed".to_owned());
        // A failed platform keeps its hybrid shape: Xcode opens a target that
        // explains itself, and the tree still carries what the healthy
        // siblings' targets reference.
        let project = project_files(&[spec], Some("[runtime]\nmode = \"standalone\"\n"));
        let paths: Vec<&Path> = project
            .files
            .iter()
            .map(|file| file.path.as_path())
            .collect();
        assert!(paths.contains(&Path::new("Resources/macOS-Info.plist")));
        assert!(paths.contains(&Path::new("Resources/KiraRunner.toml")));
    }

    #[test]
    fn a_native_only_export_writes_no_runner_manifest() {
        let mut spec = macos_spec();
        spec.spec.native_entry = true;
        let project = project_files(&[spec], None);
        assert!(
            !project
                .files
                .iter()
                .any(|file| file.path == Path::new("Resources/KiraRunner.toml"))
        );
    }
}
