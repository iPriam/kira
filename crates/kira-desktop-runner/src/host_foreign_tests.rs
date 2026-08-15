use super::*;
use kira_runtime_abi::{ForeignAbi, ForeignImport, ForeignSignature, ForeignType};

fn foreign_module() -> Module {
    let mut module = printing_module();
    module.foreign_imports = vec![ForeignImport::new(
        "fixture",
        "ffi_noop",
        ForeignAbi::C,
        ForeignSignature::scalars([], ForeignType::Void),
    )];
    module
}

fn vm_foreign_bundle(module: &Module) -> Bundle {
    Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![
            NamedPayload {
                name: "app.kbc".to_owned(),
                kind: PayloadKind::VmBytecode,
                bytes: module.to_bytes(),
            },
            NamedPayload {
                name: "app.ffi-bindings".to_owned(),
                kind: PayloadKind::ForeignBindings,
                bytes: b"not-a-library\n".to_vec(),
            },
            NamedPayload {
                name: "not-a-library".to_owned(),
                kind: PayloadKind::NativeDependency,
                bytes: b"not a native library".to_vec(),
            },
        ],
        0,
    )
    .expect("a valid VM foreign bundle")
}

#[test]
fn a_vm_foreign_bundle_loads_bytecode_entry_and_binding_dependency() {
    let dir = TempDir::new("vm-foreign");
    let mut host = DesktopHost::new(dir.0.clone());
    let bundle = vm_foreign_bundle(&foreign_module());

    assert_eq!(
        bundle.manifest().entry_payload().map(|entry| entry.kind),
        Some(PayloadKind::VmBytecode)
    );
    assert_eq!(
        bundle.manifest().payloads[1].kind,
        PayloadKind::ForeignBindings
    );

    host.load(&bundle).expect("the VM entry and binding stage");
    match &host.staged {
        Staged::VmLoaded { bindings, .. } => {
            assert_eq!(
                bindings,
                &Some(vec![Some(
                    dir.0.join(kira_live::PAYLOAD_DIR).join("not-a-library")
                )])
            );
        }
        staged => panic!("a VM+FFI bundle entered {staged:?}"),
    }

    let error = host
        .link()
        .expect_err("the invalid binding must fail through the direct loader");
    assert!(
        matches!(error, DesktopRunnerError::ForeignSession(_)),
        "got {error:?}"
    );
}

/// An import without a VM live binding payload fails before the foreign loader
/// can turn it into an unavailable or process-relative binding.
#[test]
fn a_vm_import_without_binding_metadata_is_rejected() {
    let dir = TempDir::new("vm-foreign-missing");
    let mut host = DesktopHost::new(dir.0.clone());
    let bundle = Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![NamedPayload {
            name: "app.kbc".to_owned(),
            kind: PayloadKind::VmBytecode,
            bytes: foreign_module().to_bytes(),
        }],
        0,
    )
    .expect("a valid VM foreign bundle");

    host.load(&bundle).expect("the bytecode entry stages");
    assert!(matches!(
        host.link(),
        Err(DesktopRunnerError::MissingForeignBindings)
    ));
}

/// A live binding line cannot escape the flat runner payload directory.
#[test]
fn a_vm_binding_manifest_rejects_path_traversal() {
    let dir = TempDir::new("vm-foreign-traversal");
    let mut host = DesktopHost::new(dir.0.clone());
    let bundle = Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![
            NamedPayload {
                name: "app.kbc".to_owned(),
                kind: PayloadKind::VmBytecode,
                bytes: foreign_module().to_bytes(),
            },
            NamedPayload {
                name: "app.ffi-bindings".to_owned(),
                kind: PayloadKind::ForeignBindings,
                bytes: b"..\\outside.dll\n".to_vec(),
            },
        ],
        0,
    )
    .expect("a valid VM foreign bundle");

    let error = host
        .load(&bundle)
        .expect_err("a binding path must stay inside the payload directory");
    assert!(
        matches!(
            error,
            DesktopRunnerError::InvalidForeignBindings { line: 1, .. }
        ),
        "got {error:?}"
    );
}

/// Foreign binding metadata is a dependency and cannot become the runner entry.
#[test]
fn a_foreign_bindings_payload_is_not_an_entrypoint() {
    let dir = TempDir::new("foreign-bindings-entry");
    let mut host = DesktopHost::new(dir.0.clone());
    let bundle = Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![NamedPayload {
            name: "app.ffi-bindings".to_owned(),
            kind: PayloadKind::ForeignBindings,
            bytes: b"\n".to_vec(),
        }],
        0,
    )
    .expect("the bundle format permits the malformed entry for runner testing");

    let error = host
        .load(&bundle)
        .expect_err("foreign binding metadata cannot be an entrypoint");
    assert!(matches!(
        error,
        DesktopRunnerError::UnsupportedEntry {
            kind: "foreign-bindings"
        }
    ));
}
