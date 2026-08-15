//! Live bundle construction and native load-time dependency closure.

use std::collections::HashSet;
use std::path::Path;

use kira_ir::IrProgram;
use kira_live::{Bundle, NamedPayload, PayloadKind};
use kira_llvm_backend::NativeLinkInputs;
use kira_manifest::{BuildProfile, RunnerId};

use super::{LiveBackend, LiveError};
use crate::hybrid;
use crate::native::{self, Artifacts};

/// Builds `program` into a live bundle for `runner`.
///
/// The bundle is what the runner gets, so this is where a backend choice stops
/// mattering: each backend becomes an entry payload and its staged dependencies
/// by the time it reaches the wire.
///
/// `foreign_link` is what a program's `@FFI.Extern` imports resolved to. A
/// program with none links nothing and the value is empty; a program with them
/// gets direct import metadata and native dependencies as VM payloads. An LLVM
/// live bundle carries one whole-program native library plus every selected
/// native runtime asset.
pub(crate) fn build_bundle(
    program: &IrProgram,
    source: &Path,
    runner: RunnerId,
    backend: LiveBackend,
    foreign_link: &NativeLinkInputs,
) -> Result<Bundle, LiveError> {
    match backend {
        LiveBackend::Vm => {
            let module = kira_bytecode::compile(program)
                .map_err(|error| LiveError::build(backend, &error))?;
            let artifacts = artifact_layout(source)?;
            let bytecode_path = artifacts.bytecode();
            let bytecode = named_payload_bytes(
                &bytecode_path,
                PayloadKind::VmBytecode,
                module.to_bytes(),
                backend,
            )?;
            let mut names = HashSet::new();
            let mut payloads = Vec::new();
            append_payload(&mut payloads, &mut names, bytecode, backend)?;
            let mut direct_dependencies = Vec::new();
            if !program.foreign_imports.is_empty() || !program.foreign_callbacks.is_empty() {
                let bindings = native::direct_foreign_bindings(program, source, foreign_link)
                    .map_err(|error| LiveError::build(backend, &error))?;
                let bindings =
                    native::stage_direct_foreign_bindings(artifacts.directory(), &bindings)
                        .map_err(|error| LiveError::build(backend, &error))?;
                direct_dependencies.extend(
                    bindings
                        .iter()
                        .filter_map(|binding| binding.library_path().map(Path::to_path_buf))
                        .filter(|path| path.is_file()),
                );
                let bindings_path = artifacts.foreign_bindings();
                native::write_foreign_binding_names(&bindings_path, &bindings)
                    .map_err(|error| LiveError::build(backend, &error))?;
                append_payload(
                    &mut payloads,
                    &mut names,
                    named_payload(&bindings_path, PayloadKind::ForeignBindings, backend)?,
                    backend,
                )?;
                let libffi = kira_libffi::stage_bundle(artifacts.directory())
                    .map_err(|error| LiveError::build(backend, &error))?;
                direct_dependencies.push(libffi);
            }
            payloads.extend(native_dependency_payloads(
                foreign_link,
                &direct_dependencies,
                backend,
                &mut names,
            )?);
            Ok(Bundle::build(runner, BuildProfile::Debug, payloads, 0)?)
        }
        LiveBackend::Llvm => {
            build_native_live_bundle(program, source, runner, backend, foreign_link)
        }
        LiveBackend::Hybrid => build_hybrid_bundle(program, source, runner, backend, foreign_link),
    }
}

/// Builds a self-contained whole-program native live bundle.
fn build_native_live_bundle(
    program: &IrProgram,
    source: &Path,
    runner: RunnerId,
    backend: LiveBackend,
    foreign_link: &NativeLinkInputs,
) -> Result<Bundle, LiveError> {
    let artifacts = native::build_live(program, source, false, false, foreign_link)
        .map_err(|error| LiveError::build(backend, &error))?;
    let library = artifacts.library.ok_or_else(|| LiveError::Build {
        backend: backend.label(),
        reason: "the native live build produced no shared library".to_owned(),
    })?;
    let library_payload = named_payload(&library, PayloadKind::NativeLibrary, backend)?;
    let mut names = HashSet::new();
    let mut payloads: Vec<NamedPayload> = Vec::new();
    append_payload(&mut payloads, &mut names, library_payload, backend)?;
    let mut extra_dependencies = native::dynamic_foreign_library_paths(foreign_link);
    if has_foreign_surface(program) {
        let directory = library.parent().ok_or_else(|| LiveError::Build {
            backend: backend.label(),
            reason: format!(
                "native live library `{}` has no parent directory",
                library.display()
            ),
        })?;
        let libffi = kira_libffi::stage_bundle(directory)
            .map_err(|error| LiveError::build(backend, &error))?;
        extra_dependencies.push(libffi);
    }
    payloads.extend(native_dependency_payloads(
        foreign_link,
        &extra_dependencies,
        backend,
        &mut names,
    )?);
    Ok(Bundle::build(runner, BuildProfile::Debug, payloads, 0)?)
}

/// Reads the native files the runner must stage beside a whole-program library.
///
/// A runtime-file declaration may name one file or a directory. The link step
/// applies the same rule, so the bundle expands directories here rather than
/// staging an unreadable directory payload.
fn native_dependency_payloads(
    foreign_link: &NativeLinkInputs,
    extra_paths: &[std::path::PathBuf],
    backend: LiveBackend,
    names: &mut HashSet<String>,
) -> Result<Vec<NamedPayload>, LiveError> {
    let mut seen_paths = HashSet::new();
    let mut payloads: Vec<NamedPayload> = Vec::new();
    let mut declared_files = foreign_link.runtime_files().to_vec();
    declared_files.extend(extra_paths.iter().cloned());
    for declared in declared_files {
        let files = if declared.is_dir() {
            let mut files = std::fs::read_dir(&declared)
                .map_err(|source| LiveError::Io {
                    path: declared.clone(),
                    source,
                })?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| LiveError::Io {
                    path: declared.clone(),
                    source,
                })?;
            files.retain(|path| path.is_file());
            files.sort();
            files
        } else {
            vec![declared.clone()]
        };
        for file in files {
            if !seen_paths.insert(file.clone()) {
                continue;
            }
            let payload = named_payload(&file, PayloadKind::NativeDependency, backend)?;
            if names.contains(&payload.name)
                && payloads.iter().any(|existing| {
                    existing.name == payload.name && existing.bytes == payload.bytes
                })
            {
                // A dynamic import may have been copied beside the artifact
                // while the same DLL is also declared by runtimeFiles. Equal
                // bytes under one flat name are one dependency, not an
                // ambiguous payload.
                continue;
            }
            append_payload(&mut payloads, names, payload, backend)?;
        }
    }
    Ok(payloads)
}

/// Builds a hybrid bundle: the manifest, its bytecode, and its native library.
///
/// The manifest is the entrypoint, and the other two are the halves it names.
/// A `KHM1` manifest names them as plain file names beside itself, and a
/// bundle's payloads are staged flat in one directory — so the manifest resolves
/// inside the runner's cache exactly as it did in the build directory.
fn build_hybrid_bundle(
    program: &IrProgram,
    source: &Path,
    runner: RunnerId,
    backend: LiveBackend,
    foreign_link: &NativeLinkInputs,
) -> Result<Bundle, LiveError> {
    let bundle = hybrid::build(program, source, false, foreign_link)
        .map_err(|error| LiveError::build(backend, &error))?;
    let artifacts = Artifacts::for_source(source).map_err(|error| LiveError::Io {
        path: source.to_owned(),
        source: error,
    })?;

    let manifest_path = bundle.manifest;
    let bytecode_path = artifacts.bytecode();
    let library_path = artifacts.shared_library();
    let mut extra_dependencies = bundle.foreign_dependencies;
    if has_foreign_surface(program) {
        let libffi = kira_libffi::stage_bundle(artifacts.directory())
            .map_err(|error| LiveError::build(backend, &error))?;
        extra_dependencies.push(libffi);
    }

    let mut names = HashSet::new();
    let mut payloads = Vec::new();
    for payload in [
        named_payload(&manifest_path, PayloadKind::HybridManifest, backend)?,
        named_payload(&bytecode_path, PayloadKind::VmBytecode, backend)?,
        named_payload(&library_path, PayloadKind::NativeLibrary, backend)?,
    ] {
        append_payload(&mut payloads, &mut names, payload, backend)?;
    }
    payloads.extend(native_dependency_payloads(
        foreign_link,
        &extra_dependencies,
        backend,
        &mut names,
    )?);
    // The manifest is payload 0, and it is the entrypoint: it is the only payload
    // that knows how the other two fit together.
    Ok(Bundle::build(runner, BuildProfile::Debug, payloads, 0)?)
}

/// Whether the runtime must carry the Libffi engine for this program.
fn has_foreign_surface(program: &IrProgram) -> bool {
    !program.foreign_imports.is_empty() || !program.foreign_callbacks.is_empty()
}

/// Reads `path` into a payload named by its file name.
fn named_payload(
    path: &Path,
    kind: PayloadKind,
    backend: LiveBackend,
) -> Result<NamedPayload, LiveError> {
    let bytes = std::fs::read(path).map_err(|source| LiveError::Io {
        path: path.to_owned(),
        source,
    })?;
    named_payload_bytes(path, kind, bytes, backend)
}

/// Names an in-memory artifact using the same path metadata as a disk artifact.
fn named_payload_bytes(
    path: &Path,
    kind: PayloadKind,
    bytes: Vec<u8>,
    backend: LiveBackend,
) -> Result<NamedPayload, LiveError> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| LiveError::Build {
            backend: backend.label(),
            reason: format!("built artifact `{}` has no file name", path.display()),
        })?;
    Ok(NamedPayload { name, kind, bytes })
}

/// Adds a payload while keeping the flat staging namespace unambiguous.
fn append_payload(
    payloads: &mut Vec<NamedPayload>,
    names: &mut HashSet<String>,
    payload: NamedPayload,
    backend: LiveBackend,
) -> Result<(), LiveError> {
    if !names.insert(payload.name.clone()) {
        return Err(LiveError::Build {
            backend: backend.label(),
            reason: format!(
                "live payload `{}` collides with another staged payload",
                payload.name
            ),
        });
    }
    payloads.push(payload);
    Ok(())
}

/// Resolves the source's artifact layout without keeping its build lock while a
/// backend writes another artifact in the same directory.
fn artifact_layout(source: &Path) -> Result<Artifacts, LiveError> {
    Artifacts::for_source(source).map_err(|source_error| LiveError::Io {
        path: source.to_owned(),
        source: source_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_native_lib_definition::{
        LinkMode, NativeLinkAttributes, ResolvedTargetRow, TargetTriple,
    };
    use std::fs;

    #[test]
    fn native_dependency_payloads_follow_runtime_metadata() {
        let root = std::env::temp_dir().join(format!(
            "kira-live-native-dependencies-{}",
            std::process::id()
        ));
        let runtime = root.join("runtime");
        let import_library = root.join("import.lib");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&runtime).expect("runtime directory");
        fs::write(runtime.join("z.dll"), b"z contents").expect("z runtime");
        fs::write(runtime.join("a.dll"), b"a contents").expect("a runtime");
        fs::write(&import_library, b"link-only input").expect("import library");

        let row = ResolvedTargetRow::new(
            TargetTriple::new("x86_64", "windows", "msvc"),
            Some(import_library),
            NativeLinkAttributes::default(),
        )
        .with_runtime_files(vec![runtime.clone()]);
        let mut foreign_link = NativeLinkInputs::default();
        foreign_link.push_row(&row);

        let mut names = HashSet::new();
        let payloads = native_dependency_payloads(&foreign_link, &[], LiveBackend::Vm, &mut names)
            .expect("declared runtime files become payloads");

        assert_eq!(
            payloads
                .iter()
                .map(|payload| (payload.name.clone(), payload.kind))
                .collect::<Vec<_>>(),
            vec![
                ("a.dll".to_owned(), PayloadKind::NativeDependency),
                ("z.dll".to_owned(), PayloadKind::NativeDependency),
            ]
        );
        assert_eq!(payloads[0].bytes, b"a contents");
        assert_eq!(payloads[1].bytes, b"z contents");
        assert!(
            payloads.iter().all(|payload| payload.name != "import.lib"),
            "link-only archives are not load-time dependencies"
        );

        let mut bundle_payloads = vec![NamedPayload {
            name: "main.kbc".to_owned(),
            kind: PayloadKind::VmBytecode,
            bytes: b"KBC1".to_vec(),
        }];
        bundle_payloads.extend(payloads);
        let bundle = Bundle::build(RunnerId::Desktop, BuildProfile::Debug, bundle_payloads, 0)
            .expect("dependency payloads form a verified bundle");
        assert_eq!(
            bundle
                .manifest()
                .payloads
                .iter()
                .map(|payload| payload.name.as_str())
                .collect::<Vec<_>>(),
            vec!["main.kbc", "a.dll", "z.dll"]
        );
        assert_eq!(
            bundle.manifest().payloads[1].hash,
            kira_live::ContentHash::of(b"a contents")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_live_carries_file_backed_dynamic_foreign_libraries() {
        let root =
            std::env::temp_dir().join(format!("kira-live-dynamic-foreign-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("dynamic foreign directory");
        let dynamic = root.join("libfixture.so");
        fs::write(&dynamic, b"dynamic foreign library").expect("dynamic foreign library");

        let row = ResolvedTargetRow::new(
            TargetTriple::new("x86_64", "linux", "gnu"),
            Some(dynamic.clone()),
            NativeLinkAttributes::default(),
        )
        .with_link_mode(LinkMode::Dynamic);
        let mut foreign_link = NativeLinkInputs::default();
        foreign_link.push_library("dynamic", dynamic.clone(), &row);
        foreign_link.push_library_path("system", "system-driver".into());

        assert_eq!(
            native::dynamic_foreign_library_paths(&foreign_link),
            vec![dynamic]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn foreign_bindings_are_not_the_vm_live_entry_payload() {
        let bundle = Bundle::build(
            RunnerId::Desktop,
            BuildProfile::Debug,
            vec![
                NamedPayload {
                    name: "app.kbc".to_owned(),
                    kind: PayloadKind::VmBytecode,
                    bytes: b"KBC1".to_vec(),
                },
                NamedPayload {
                    name: "app.ffi-bindings".to_owned(),
                    kind: PayloadKind::ForeignBindings,
                    bytes: b"fixture.so\n".to_vec(),
                },
            ],
            0,
        )
        .expect("foreign bindings are a valid dependency payload");

        assert_eq!(
            bundle
                .manifest()
                .entry_payload()
                .map(|payload| payload.kind),
            Some(PayloadKind::VmBytecode)
        );
        assert_eq!(
            bundle.manifest().payloads[1].kind,
            PayloadKind::ForeignBindings
        );
    }
}
