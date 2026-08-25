//! `kira export apple|macos|ios|tvos|visionos`: the Xcode workspace and the
//! per-architecture artifacts behind it.
//!
//! The generators in `kira-export` produce pure text; this module produces
//! everything that is *not* text: one Kira build per architecture slice, each
//! linked into its platform's target through `OTHER_LDFLAGS`, plus the
//! embedded `.klbundle` a hybrid app plays. The result is an Xcode workspace
//! whose every scheme builds from real artifacts — a platform whose slices
//! could not be built still opens, marked unavailable with the reason, rather
//! than referencing objects no build produced.
//!
//! # The embedded layout
//!
//! A hybrid target's native half is compiled to a bare object and linked
//! *into* the application binary (with `-Wl,-export_dynamic`), and its hybrid
//! manifest records [`SELF_LIBRARY_MARKER`] instead of a dylib name — the
//! running image *is* the native half. The support archive providing
//! `kira_live_runner_entry` and the whole runtime is force-loaded beside it.
//! Exactly one Rust static library therefore lands in any app link.
//!
//! # One manifest, four apps
//!
//! The workspace shares `Resources/KiraRunner.toml` between targets, so it
//! ships naming the first healthy platform; `kind` only decides which runner
//! id a *live* session connects as, and the live flow rewrites the copy inside
//! the built `.app` before launching it. Standalone playback never reads it.

use std::path::{Path, PathBuf};

use kira_backend_api::{CrossTarget, Linkage, NativeTarget, RelocationModel};
use kira_dynamic_ffi::SELF_LIBRARY_MARKER;
use kira_export::apple::pbxproj::LdflagsBlock;
use kira_export::apple::{self, ApplePlatform, project, slices};
use kira_live::{Bundle, NamedPayload, PayloadKind};
use kira_llvm_backend::NativeBuildTarget;
use kira_manifest::platform_config::{
    BuildProfile, ExportFamily, RunnerKind, RunnerManifest, RuntimeMode,
};

use crate::pipeline::{EXIT_FAILURE, EXIT_OK};
use crate::progress::{err, out};

pub(crate) mod archives;

use archives::{SysrootCache, libffi_archive_for_slice, locate_support_archive};

/// Runs the Apple family export.
pub(crate) fn run(
    request: &crate::export::Request,
    exports_root: &Path,
    target_root: &str,
    product_stem: &str,
) -> i32 {
    // Every path written into the generated project must survive Xcode
    // building from its own directories: absolute on the way in, or broken at
    // link time.
    let target_root = std::path::absolute(target_root)
        .unwrap_or_else(|_| PathBuf::from(target_root))
        .display()
        .to_string();
    let apple_root = std::path::absolute(exports_root.join("apple"))
        .unwrap_or_else(|_| exports_root.join("apple"));
    let platforms = platforms_for(request.family);

    if let Some(sdk) = &request.xcode_rebuild {
        return rebuild_for_sdk(sdk, request, &apple_root, &target_root);
    }

    let mut context = BuildContext {
        request,
        apple_root: &apple_root,
        target_root: &target_root,
        product_stem,
        sysroots: SysrootCache::default(),
        bundle_payloads: None,
        bundle_id: bundle_id(&request.package_name),
        healthy_platform: None,
    };

    let mut specs = Vec::new();
    let mut failures = Vec::new();
    for platform in platforms {
        let spec = build_platform(platform, &mut context);
        if let Some(reason) = &spec.spec.unavailable_reason {
            failures.push((platform, reason.clone()));
        }
        specs.push(spec);
    }

    let healthy = specs
        .iter()
        .filter(|spec| spec.spec.unavailable_reason.is_none())
        .count();
    if healthy == 0 {
        for (platform, reason) in &failures {
            err!(
                "kira export: {} could not be exported: {reason}",
                platform.label()
            );
        }
        err!("kira export: no Apple platform could be built, so there is no workspace to write");
        return EXIT_FAILURE;
    }

    if let Err(error) = write_tree(&mut context, &specs) {
        err!("kira export: {error}");
        return EXIT_FAILURE;
    }

    out!(
        "exported Apple Xcode workspace at {}",
        apple_root.join("KiraApp.xcworkspace").display()
    );
    for spec in &specs {
        out!("  {} → {}", spec.platform.label(), spec.product_name());
    }
    for (platform, reason) in &failures {
        err!("note: {} target is unavailable: {reason}", platform.label());
    }
    audit_tools(["xcodebuild", "xcrun"]);
    EXIT_OK
}

/// The platforms a family addresses.
fn platforms_for(family: ExportFamily) -> Vec<ApplePlatform> {
    match family {
        ExportFamily::Macos => vec![ApplePlatform::Macos],
        ExportFamily::Ios => vec![ApplePlatform::Ios],
        ExportFamily::Tvos => vec![ApplePlatform::Tvos],
        ExportFamily::Visionos => vec![ApplePlatform::Visionos],
        _ => vec![
            ApplePlatform::Macos,
            ApplePlatform::Ios,
            ApplePlatform::Tvos,
            ApplePlatform::Visionos,
        ],
    }
}

/// State shared by every platform's builds.
struct BuildContext<'a> {
    request: &'a crate::export::Request,
    apple_root: &'a Path,
    target_root: &'a str,
    /// The app's product-name stem, e.g. `KiraApp` in `KiraApp-iOS`.
    product_stem: &'a str,
    /// Resolved SDK paths, one `xcrun` call per SDK at most.
    sysroots: SysrootCache,
    /// The first successfully built bundle's payloads, embedded once: bytecode
    /// and a self-hosted hybrid manifest are arch-independent by design.
    bundle_payloads: Option<Vec<NamedPayload>>,
    /// The content-derived id of that bundle.
    bundle_id: String,
    /// The first platform whose slices all built.
    healthy_platform: Option<ApplePlatform>,
}

/// Builds one platform: every slice, then the spec describing them.
fn build_platform(
    platform: ApplePlatform,
    context: &mut BuildContext<'_>,
) -> project::PlatformSpec {
    let arch_slices = slices::slices_for(platform);

    let mut blocks = Vec::new();
    let mut failure: Option<String> = None;
    'slices: for slice in &arch_slices {
        match build_slice(slice, context) {
            Ok(ldflags) => blocks.push(LdflagsBlock {
                sdk_condition: slice.sdk_condition.map(str::to_owned),
                value: ldflags,
            }),
            Err(reason) => {
                failure = Some(format!("{}: {reason}", slice.label));
                break 'slices;
            }
        }
    }

    let archs = unique_archs(&arch_slices);
    let unavailable = failure;
    // A failed platform links nothing; Xcode compiles main.m with the
    // KIRA_TARGET_UNAVAILABLE guard and produces an app that explains itself.
    let spec_blocks = if unavailable.is_some() {
        Vec::new()
    } else {
        blocks
    };
    let rebuild_script = (unavailable.is_none()).then(|| {
        xcode_rebuild_script(std::env::current_exe().ok().as_deref(), context.target_root)
    });

    let mut spec = project::hybrid_spec(
        platform,
        context.product_stem,
        apple::DEFAULT_BUNDLE_ID,
        &archs,
    )
    .with_ldflags(spec_blocks);
    if let Some(script) = rebuild_script {
        spec = spec.with_rebuild_script(script);
    }
    if let Some(reason) = unavailable {
        spec = spec.marked_unavailable(reason);
    }
    spec
}

/// Builds one architecture slice: compile, lower the native half, assemble the
/// bundle, and render the link flags.
fn build_slice(
    slice: &slices::ArchSlice,
    context: &mut BuildContext<'_>,
) -> Result<String, String> {
    let triple = kira_native_lib_definition::TargetTriple::new(slice.arch, slice.os, slice.abi);
    let source = context.request.source.clone();

    // Per-slice compilation keeps autobind honest: bindings are generated per
    // target inside the frontend, so a device slice and its simulator see
    // their own rows even though they share an entrypoint.
    let compiled = crate::pipeline::compile_verified_path(&source, &triple)
        .map_err(|_| "the program did not compile".to_owned())?;
    let ir = crate::pipeline::entrypoint_ir("export", compiled)
        .map_err(|_| "the package has no runnable program to export".to_owned())?;
    let device = crate::options::Device::Cross(CrossTarget::new(
        triple.clone(),
        RelocationModel::Pic,
        Linkage::Dynamic,
    ));
    let foreign = crate::pipeline::foreign_inputs(&source, &ir, &device)
        .map_err(|_| "foreign libraries could not be resolved".to_owned())?;
    let link = crate::pipeline::foreign_link_of(&foreign).clone();

    let work_root = context.apple_root.join("build").join(slice.label);
    std::fs::create_dir_all(&work_root).map_err(|error| error.to_string())?;
    let sysroot = context.sysroots.path(slice.apple_sdk)?;
    let support = locate_support_archive(&slice.normalized_triple())?;
    let libffi = libffi_archive_for_slice(slice, &work_root)?;
    let stem = "kira_app";
    let object_path = work_root.join(format!("{stem}.o"));

    let options = kira_llvm_backend::NativeBuildOptions {
        module_name: stem.to_owned(),
        object_path: object_path.clone(),
        executable_path: None,
        shared_library_path: None,
        archive_path: None,
        exports: Default::default(),
        ir_path: None,
        // Nothing reads this for an emit-only build: the helpers the object
        // references are satisfied by the force-loaded support archive at app
        // link time, not by a runtime archive linked here.
        runtime_archive: support.clone(),
        optimize: context.request.profile != BuildProfile::Debug,
        unavailable_imports: link.unavailable_imports().to_vec(),
        foreign_link: link.clone(),
        target: NativeBuildTarget::new(
            NativeTarget::Cross(CrossTarget::new(
                triple.clone(),
                RelocationModel::Pic,
                Linkage::Dynamic,
            )),
            Some(sysroot),
        ),
    };

    let (_object, trampolines) =
        kira_llvm_backend::build_hybrid_object(&ir, &options).map_err(|error| error.to_string())?;
    let payloads = hybrid_embedded_payloads(&ir, stem, &trampolines, &link)?;
    assemble_bundle_once(context, payloads)?;

    Ok(ldflags_value(&support, &libffi, &object_path, &link))
}

/// The payloads of the bundle an embedded app plays: its hybrid manifest
/// naming the process itself as the native half, the bytecode half, and any
/// file-backed foreign libraries.
///
/// Shared by the export and the live flow — one program, one bundle shape —
/// so an exported app and a live session can never disagree about what an
/// embedded bundle contains.
pub(crate) fn hybrid_embedded_payloads(
    ir: &kira_ir::IrProgram,
    module_name: &str,
    trampolines: &[(u32, String)],
    link: &kira_native_lib_definition::NativeLinkInputs,
) -> Result<Vec<NamedPayload>, String> {
    let module = kira_bytecode::compile_hybrid(ir).map_err(|error| error.to_string())?;
    let internal = kira_build::hybrid_internal_function_count(ir, &module)
        .map_err(|error| error.to_string())?;
    let khm = kira_build::hybrid_manifest_with_foreign_paths(
        ir,
        module_name,
        "app.kbc",
        SELF_LIBRARY_MARKER,
        trampolines,
        internal,
        &embedded_foreign_paths(ir, link),
    )
    .map_err(|error| error.to_string())?
    .to_bytes();
    let mut payloads = vec![
        NamedPayload {
            name: "app.khm".to_owned(),
            kind: PayloadKind::HybridManifest,
            bytes: khm,
        },
        NamedPayload {
            name: "app.kbc".to_owned(),
            kind: PayloadKind::VmBytecode,
            bytes: module.to_bytes(),
        },
    ];
    // File-backed foreign libraries ride along as dependencies, so the host
    // stages them before anything opens the manifest that names them.
    let mut seen: Vec<String> = Vec::new();
    for path in crate::native::dynamic_foreign_library_paths(link) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if seen.iter().any(|known| known == name) {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        seen.push(name.to_owned());
        payloads.push(NamedPayload {
            name: name.to_owned(),
            kind: PayloadKind::NativeDependency,
            bytes,
        });
    }
    Ok(payloads)
}

/// Stores the embedded bundle's payloads on the first slice that gets here.
///
/// Later slices must agree byte-for-byte: one `Bundles/` directory is shared
/// by every target, so a slice whose hybrid manifest or foreign paths name
/// its own architecture's artifacts cannot reuse the first slice's copy —
/// shipping it would point another slice's app at the wrong native half.
/// The export refuses instead of guessing which slice is right.
fn assemble_bundle_once(
    context: &mut BuildContext<'_>,
    payloads: Vec<NamedPayload>,
) -> Result<(), String> {
    match &context.bundle_payloads {
        Some(stored) => {
            if payload_digest(stored) != payload_digest(&payloads) {
                return Err(
                    "slices compiled different embedded bundles (their manifests or \
                     foreign dependencies are architecture-specific), so they cannot \
                     share one `Resources/Bundles`; export each platform separately \
                     or make the native-library rows identical across device and \
                     simulator triples"
                        .to_owned(),
                );
            }
            Ok(())
        }
        None => {
            // Hashing and validation happen when the bundle is assembled for
            // embedding; until then the payloads are simply the bundle's
            // future contents.
            context.bundle_payloads = Some(payloads);
            Ok(())
        }
    }
}

/// One digest over every payload's name and bytes, in bundle order.
fn payload_digest(payloads: &[NamedPayload]) -> String {
    use std::fmt::Write as _;
    let mut digest = String::new();
    for payload in payloads {
        let _ = write!(
            digest,
            "{}:{};",
            payload.name,
            kira_live::ContentHash::of(&payload.bytes)
        );
    }
    digest
}

/// The `[paths]` foreign rows an embedded manifest records.
///
/// Image-resident rows bind out of the running app itself — the adapters were
/// linked into it — and file-backed rows record the staged file name exactly
/// as the desktop live bundle does.
fn embedded_foreign_paths(
    ir: &kira_ir::IrProgram,
    link: &kira_native_lib_definition::NativeLinkInputs,
) -> Vec<Option<String>> {
    use kira_dynamic_ffi::{HOST_RUNTIME_LIBRARY, PROCESS_BINDING_MARKER};
    let image_names: std::collections::HashSet<&str> =
        link.image_libraries().iter().map(String::as_str).collect();
    ir.foreign_imports
        .iter()
        .map(|entry| {
            if entry.import.as_syscall().is_some() {
                return None;
            }
            if entry.import.library() == HOST_RUNTIME_LIBRARY
                || image_names.contains(entry.import.library())
            {
                return Some(PROCESS_BINDING_MARKER.to_owned());
            }
            link.library_paths()
                .iter()
                .find(|(name, _)| name == entry.import.library())
                .and_then(|(_, path)| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect()
}

/// The `OTHER_LDFLAGS` body linking one slice into its app.
///
/// The support archive is force-loaded because Rust archives resolve members
/// lazily and the entry symbol sits behind several layers of cross-references;
/// everything after it is the program's own object and whatever C surfaces it
/// calls into.
fn ldflags_value(
    support: &Path,
    libffi: &Path,
    object: &Path,
    link: &kira_native_lib_definition::NativeLinkInputs,
) -> String {
    let mut parts = vec![
        format!("\"-Wl,-force_load,{}\"", support.display()),
        format!("\"{}\"", libffi.display()),
        format!("\"{}\"", object.display()),
    ];
    for archive in link.archives() {
        parts.push(format!("\"{}\"", archive.display()));
    }
    for framework in link.frameworks() {
        parts.push("\"-framework\"".to_owned());
        parts.push(format!("\"{framework}\""));
    }
    for system_lib in link.system_libs() {
        parts.push(format!("\"-l{system_lib}\""));
    }
    parts.push("\"-Wl,-export_dynamic\"".to_owned());
    parts.join(", ")
}

/// `ARCHS`/`VALID_ARCHS` for a platform's slices, deduplicated in order.
fn unique_archs(slc: &[slices::ArchSlice]) -> String {
    let mut seen: Vec<&'static str> = Vec::new();
    for slice in slc {
        let arch = slice.xcode_arch();
        if !seen.contains(&arch) {
            seen.push(arch);
        }
    }
    seen.join(" ")
}

/// The content-derived directory name of the app's bundle.
fn bundle_id(package_name: &str) -> String {
    format!("com.kira.{}", kira_export::safe_identifier(package_name))
}

/// The runner kind a platform's manifest ships under before a live launch
/// rewrites the app's copy.
fn manifest_kind(platform: ApplePlatform) -> RunnerKind {
    match platform {
        ApplePlatform::Macos => RunnerKind::XcodeMacos,
        ApplePlatform::Ios => RunnerKind::XcodeIos,
        ApplePlatform::Tvos => RunnerKind::XcodeTvos,
        ApplePlatform::Visionos => RunnerKind::XcodeVisionos,
    }
}

/// The `KiraRunner.toml` text the tree ships.
fn runner_manifest_text(context: &BuildContext<'_>, khm_hash: &str) -> String {
    RunnerManifest {
        kind: manifest_kind(context.healthy_platform.unwrap_or(ApplePlatform::Macos)),
        name: context.product_stem.to_owned(),
        bundle_id: apple::DEFAULT_BUNDLE_ID.to_owned(),
        version: "0.1.0".to_owned(),
        mode: RuntimeMode::Standalone,
        target_path: context.target_root.to_owned(),
        package_name: context.request.package_name.clone(),
        // Namespaced by this app's bundle id: two exported apps must never
        // fight over one staging directory.
        local_cache_path: format!("app-support/KiraExport/{}", context.bundle_id),
        main_bundle_id: context.bundle_id.clone(),
        embedded_bundles_path: Some("Bundles".to_owned()),
        server_host: "127.0.0.1".to_owned(),
        server_port: 0,
        native_contract_hash: khm_hash.to_owned(),
    }
    .render()
}

/// Writes the embedded bundle under Resources and reports its manifest hash.
fn embed_bundle(context: &mut BuildContext<'_>) -> Option<String> {
    let payloads = context.bundle_payloads.take()?;
    let bundles_dir = context.apple_root.join("Resources").join("Bundles");
    let _ = std::fs::remove_dir_all(&bundles_dir);
    let bundle = match Bundle::build(
        context
            .healthy_platform
            .map(crate::export::runner_id_for)
            .unwrap_or(kira_manifest::RunnerId::Macos),
        BuildProfile::Debug,
        payloads,
        0,
    ) {
        Ok(bundle) => bundle,
        Err(error) => {
            err!("kira export: {error}");
            return None;
        }
    };
    // The hash of the hybrid manifest pins the native contract the app was
    // built against; a runner validates it before playing.
    let khm_hash = bundle
        .manifest()
        .payloads
        .iter()
        .find(|payload| payload.name == "app.khm")
        .map(|payload| payload.hash.to_string())
        .unwrap_or_default();
    if let Err(error) = bundle.write(&bundles_dir.join(format!("{}.klbundle", context.bundle_id))) {
        err!("kira export: {error}");
        return None;
    }
    Some(khm_hash)
}

/// Writes the generated tree and embeds the bundle beneath it.
///
/// The error is the caller's to turn into a failed exit: a workspace that
/// could not be written must never be reported as exported.
fn write_tree(
    context: &mut BuildContext<'_>,
    specs: &[project::PlatformSpec],
) -> Result<(), kira_export::ExportError> {
    context.healthy_platform = specs.first().map(|spec| spec.platform);
    let khm_hash = embed_bundle(context);
    // Every spec this module produces is hybrid-shaped, so the generator
    // requires the manifest text even when no bundle could be assembled.
    let text = runner_manifest_text(context, khm_hash.as_deref().unwrap_or(""));
    project::project_files(specs, Some(&text)).write_to(context.apple_root)
}

/// Rebuilds the artifacts one Xcode SDK's Run Script asks for.
///
/// Invoked with `$PLATFORM_NAME`, it regenerates exactly what that SDK's
/// target links — same deterministic paths — and refreshes the embedded
/// bundle, so editing Kira source and pressing ⌘B in Xcode rebuilds the app's
/// insides without leaving Xcode.
fn rebuild_for_sdk(
    sdk: &str,
    request: &crate::export::Request,
    apple_root: &Path,
    target_root: &str,
) -> i32 {
    let platform = match sdk {
        "macosx" => ApplePlatform::Macos,
        "iphoneos" | "iphonesimulator" => ApplePlatform::Ios,
        "appletvos" | "appletvsimulator" => ApplePlatform::Tvos,
        "xros" | "xrsimulator" => ApplePlatform::Visionos,
        other => {
            err!("kira export: `{other}` is not an Apple SDK this export knows");
            return EXIT_FAILURE;
        }
    };
    let slice = slices::slices_for(platform)
        .into_iter()
        .find(|slice| slice.apple_sdk == sdk)
        .unwrap_or_else(|| slices::slices_for(platform)[0].clone());

    let mut context = BuildContext {
        request,
        apple_root,
        target_root,
        product_stem: &request.package_name,
        sysroots: SysrootCache::default(),
        bundle_payloads: None,
        bundle_id: bundle_id(&request.package_name),
        healthy_platform: None,
    };
    match build_slice(&slice, &mut context) {
        Ok(_) => {
            embed_bundle(&mut context);
            out!("rebuilt Kira {} artifacts for {sdk}", slice.label);
            EXIT_OK
        }
        Err(reason) => {
            err!("kira export: rebuilding {sdk}: {reason}");
            EXIT_FAILURE
        }
    }
}

/// The shell script a standalone target runs before compiling: rebuild this
/// SDK's Kira artifacts through the very CLI that generated the project.
pub(crate) fn xcode_rebuild_script(kira_executable: Option<&Path>, target_root: &str) -> String {
    let executable = kira_executable
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "kira".to_owned());
    format!(
        "set -e\nif [ \"${{PLATFORM_NAME:-}}\" != \"\" ]; then \"{executable}\" export apple \"{target_root}\" --xcode-rebuild \"${{PLATFORM_NAME}}\" >/dev/null; fi\n"
    )
}

/// Reports the host tools an Apple export's local build needs.
fn audit_tools(required: [&str; 2]) {
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|tool| !crate::export::command_on_path(tool))
        .collect();
    if !missing.is_empty() {
        err!(
            "note: {} not found on PATH; install {} to build this export locally",
            missing.join(" and "),
            if missing.len() == 1 { "it" } else { "them" },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_select_their_platforms() {
        assert_eq!(platforms_for(ExportFamily::Apple).len(), 4);
        assert_eq!(
            platforms_for(ExportFamily::Macos),
            vec![ApplePlatform::Macos]
        );
    }

    #[test]
    fn ldflags_force_load_the_support_archive_and_export_the_surface() {
        let value = ldflags_value(
            Path::new("/tool/target/aarch64-apple-darwin/debug/libkira_app_runner.a"),
            Path::new("/pkg/exports/apple/build/macos/libffi/libkira_libffi.a"),
            Path::new("/pkg/exports/apple/build/macos/kira_app.o"),
            &Default::default(),
        );
        assert!(value.starts_with("\"-Wl,-force_load,/tool/"));
        assert!(value.contains("\"/pkg/exports/apple/build/macos/libffi/libkira_libffi.a\""));
        assert!(value.contains("\"/pkg/exports/apple/build/macos/kira_app.o\""));
        assert!(value.ends_with("\"-Wl,-export_dynamic\""));
    }

    #[test]
    fn the_rebuild_script_names_this_cli_and_the_project_root() {
        let script = xcode_rebuild_script(Some(Path::new("/toolchain/bin/kira")), "/projects/demo");
        assert!(script.contains("\"/toolchain/bin/kira\" export apple \"/projects/demo\""));
        assert!(script.contains("--xcode-rebuild"));
        assert!(script.starts_with("set -e\n"));
    }

    #[test]
    fn bundle_ids_derive_from_the_package_name() {
        assert_eq!(bundle_id("Harmony Browser"), "com.kira.harmony_browser");
    }

    #[test]
    fn arch_lists_are_deduplicated_in_xcode_spelling() {
        let ios = slices::slices_for(ApplePlatform::Ios);
        assert_eq!(unique_archs(&ios), "arm64");
    }

    #[test]
    fn every_platform_maps_to_a_runner_kind_and_an_id() {
        for platform in [
            ApplePlatform::Macos,
            ApplePlatform::Ios,
            ApplePlatform::Tvos,
            ApplePlatform::Visionos,
        ] {
            let kind = manifest_kind(platform);
            assert_ne!(kind, RunnerKind::Desktop);
            assert_eq!(kind.runner_id(), crate::export::runner_id_for(platform));
        }
    }
}
