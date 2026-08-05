use super::*;
use kira_bytecode::{FrameRelease, FuncProto, Instruction};
use kira_live::{NamedPayload, PayloadKind};
use kira_manifest::{BuildProfile, RunnerId};
use kira_runtime_abi::Execution;
use std::fs;

/// A scratch directory that removes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let path =
            std::env::temp_dir().join(format!("kira-desktop-runner-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A module that prints one string and returns.
fn printing_module() -> Module {
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        main: Some(0),
        strings: vec!["from the bundle".to_owned()],
        functions: vec![FuncProto {
            name: "main".to_owned(),
            param_count: 0,
            local_count: 0,
            execution: Execution::Runtime,
            code: vec![
                Instruction::ConstStr(0),
                Instruction::Print,
                Instruction::ReturnVoid,
            ],
            releases: FrameRelease::EveryLocal,
        }],
    }
}

fn vm_bundle(module: &Module) -> Bundle {
    Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![NamedPayload {
            name: "app.kbc".to_owned(),
            kind: PayloadKind::VmBytecode,
            bytes: module.to_bytes(),
        }],
        0,
    )
    .expect("a valid bundle")
}

#[test]
fn loads_links_and_starts_a_vm_bundle() {
    let dir = TempDir::new("vm-happy");
    let mut host = DesktopHost::new(dir.0.clone());
    let bundle = vm_bundle(&printing_module());

    host.load(&bundle).expect("load");
    host.link().expect("link");
    host.start().expect("start");
}

/// Loading stages the bundle where a hybrid manifest's siblings would
/// resolve, which is the whole reason the payload directory is flat.
#[test]
fn loading_stages_the_bundle_on_disk() {
    let dir = TempDir::new("stage");
    let mut host = DesktopHost::new(dir.0.clone());
    host.load(&vm_bundle(&printing_module())).expect("load");

    assert!(dir.0.join(kira_live::MANIFEST_FILE).is_file());
    assert!(dir.0.join(kira_live::PAYLOAD_DIR).join("app.kbc").is_file());
}

/// A stale payload from a previous bundle must not survive into the next
/// one, or a later dlopen could resolve against code no build produced.
#[test]
fn loading_clears_a_previous_bundle() {
    let dir = TempDir::new("restage");
    let mut host = DesktopHost::new(dir.0.clone());
    host.load(&vm_bundle(&printing_module())).expect("load");

    let stale = dir.0.join(kira_live::PAYLOAD_DIR).join("stale.dylib");
    fs::write(&stale, b"stale").expect("write stale");
    host.load(&vm_bundle(&printing_module())).expect("reload");

    assert!(!stale.exists(), "a stale payload survived a reload");
}

/// The runner must never delete a directory it did not stage. `--cache` is
/// user input, and staging clears the cache: without this, pointing it at a
/// real directory erases it.
#[test]
fn staging_refuses_to_clear_a_directory_it_did_not_stage() {
    let dir = TempDir::new("not-ours");
    fs::create_dir_all(&dir.0).expect("create");
    let precious = dir.0.join("precious.txt");
    fs::write(&precious, b"work that exists nowhere else").expect("write");

    let mut host = DesktopHost::new(dir.0.clone());
    let error = host
        .load(&vm_bundle(&printing_module()))
        .expect_err("staging into somebody's directory must be refused");

    assert!(
        matches!(error, DesktopRunnerError::CacheNotOurs { .. }),
        "got {error:?}"
    );
    assert!(
        precious.is_file(),
        "the runner deleted a file it did not own"
    );
    assert_eq!(
        fs::read(&precious).expect("read back"),
        b"work that exists nowhere else"
    );
}

/// A swap replaces the running code and the process keeps going.
#[test]
fn swapping_replaces_the_linked_program() {
    let dir = TempDir::new("swap");
    let mut host = DesktopHost::new(dir.0.clone());
    host.load(&vm_bundle(&printing_module())).expect("load");
    host.link().expect("link");
    host.start().expect("start");

    host.swap(&vm_bundle(&printing_module()))
        .expect("a linked host takes a swap");
    host.start().expect("the swapped-in code runs");
}

/// A swap needs something live to replace. A merely-loaded bundle has
/// nothing mapped, so there is nothing a swap could preserve — and calling it
/// one would make the tier distinction meaningless.
#[test]
fn swapping_before_linking_is_an_error() {
    let dir = TempDir::new("swap-order");
    let mut host = DesktopHost::new(dir.0.clone());
    host.load(&vm_bundle(&printing_module())).expect("load");

    let error = host
        .swap(&vm_bundle(&printing_module()))
        .expect_err("a swap before link must fail");
    assert!(
        matches!(
            error,
            DesktopRunnerError::OutOfOrder {
                step: "swap",
                required: "linked a bundle"
            }
        ),
        "got {error:?}"
    );
}

#[test]
fn swapping_with_nothing_loaded_is_an_error() {
    let dir = TempDir::new("swap-empty");
    let mut host = DesktopHost::new(dir.0.clone());
    let error = host
        .swap(&vm_bundle(&printing_module()))
        .expect_err("a swap with nothing running must fail");
    assert!(
        matches!(error, DesktopRunnerError::OutOfOrder { step: "swap", .. }),
        "got {error:?}"
    );
}

/// The heart of the hot patch: a payload that did not change is not
/// rewritten. A rewritten file is a new inode, and the loader would map a
/// second copy of a dylib rather than hand back the image already mapped —
/// which is the difference between a hot patch and a slow relaunch.
#[test]
fn swapping_does_not_rewrite_an_unchanged_payload() {
    let dir = TempDir::new("swap-untouched");
    let mut host = DesktopHost::new(dir.0.clone());

    let before = Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![
            NamedPayload {
                name: "app.kbc".to_owned(),
                kind: PayloadKind::VmBytecode,
                bytes: printing_module().to_bytes(),
            },
            NamedPayload {
                name: "libapp.dylib".to_owned(),
                kind: PayloadKind::NativeLibrary,
                bytes: b"native code that does not change".to_vec(),
            },
        ],
        0,
    )
    .expect("a valid bundle");
    host.load(&before).expect("load");
    host.link().expect("link");

    let library = dir.0.join(kira_live::PAYLOAD_DIR).join("libapp.dylib");
    let untouched_since = fs::metadata(&library)
        .expect("the library is staged")
        .modified()
        .expect("a modification time");

    // A different bytecode half, the same native half.
    let mut changed = printing_module();
    changed.strings = vec!["different".to_owned()];
    let after = Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![
            NamedPayload {
                name: "app.kbc".to_owned(),
                kind: PayloadKind::VmBytecode,
                bytes: changed.to_bytes(),
            },
            NamedPayload {
                name: "libapp.dylib".to_owned(),
                kind: PayloadKind::NativeLibrary,
                bytes: b"native code that does not change".to_vec(),
            },
        ],
        0,
    )
    .expect("a valid bundle");
    host.swap(&after).expect("swap");

    assert!(library.is_file(), "the library was deleted by a swap");
    assert_eq!(
        fs::metadata(&library)
            .expect("the library survives")
            .modified()
            .expect("a modification time"),
        untouched_since,
        "an unchanged payload was rewritten, so its inode changed and a \
             loaded library would have been re-mapped"
    );
}

/// A swap that fails leaves the old bundle linked and running. The runner
/// reports a rejection and the app it names is still there.
#[test]
fn a_failed_swap_leaves_the_old_program_running() {
    let dir = TempDir::new("swap-fail");
    let mut host = DesktopHost::new(dir.0.clone());
    host.load(&vm_bundle(&printing_module())).expect("load");
    host.link().expect("link");

    let broken = Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![NamedPayload {
            name: "app.kbc".to_owned(),
            kind: PayloadKind::VmBytecode,
            bytes: b"not bytecode at all".to_vec(),
        }],
        0,
    )
    .expect("a valid bundle");

    host.swap(&broken).expect_err("garbage must not swap in");
    // The old program is still linked, so the app the session thinks is
    // running actually is.
    host.start()
        .expect("the previous program still runs after a failed swap");
}

#[test]
fn starting_before_linking_is_an_error() {
    let dir = TempDir::new("order-start");
    let mut host = DesktopHost::new(dir.0.clone());
    host.load(&vm_bundle(&printing_module())).expect("load");

    let error = host.start().expect_err("start before link must fail");
    assert!(
        matches!(
            error,
            DesktopRunnerError::OutOfOrder {
                step: "start",
                required: "linked the bundle"
            }
        ),
        "got {error:?}"
    );
}

#[test]
fn linking_before_loading_is_an_error() {
    let dir = TempDir::new("order-link");
    let mut host = DesktopHost::new(dir.0.clone());

    let error = host.link().expect_err("link before load must fail");
    assert!(
        matches!(
            error,
            DesktopRunnerError::OutOfOrder {
                step: "link",
                required: "loaded a bundle"
            }
        ),
        "got {error:?}"
    );
}

/// An entrypoint this runner cannot host is named, not skipped.
#[test]
fn an_asset_entrypoint_is_refused() {
    let dir = TempDir::new("bad-entry");
    let mut host = DesktopHost::new(dir.0.clone());
    let bundle = Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![NamedPayload {
            name: "logo.png".to_owned(),
            kind: PayloadKind::Asset,
            bytes: b"\x89PNG".to_vec(),
        }],
        0,
    )
    .expect("a valid bundle");

    let error = host
        .load(&bundle)
        .expect_err("an asset cannot be an entrypoint");
    assert!(
        matches!(
            error,
            DesktopRunnerError::UnsupportedEntry { kind: "asset" }
        ),
        "got {error:?}"
    );
}

/// A bundle whose bytecode is not bytecode fails at load, with a decode
/// error rather than a panic.
#[test]
fn a_bundle_with_undecodable_bytecode_fails_to_load() {
    let dir = TempDir::new("bad-bytecode");
    let mut host = DesktopHost::new(dir.0.clone());
    let bundle = Bundle::build(
        RunnerId::Desktop,
        BuildProfile::Debug,
        vec![NamedPayload {
            name: "app.kbc".to_owned(),
            kind: PayloadKind::VmBytecode,
            bytes: b"not bytecode".to_vec(),
        }],
        0,
    )
    .expect("a valid bundle");

    let error = host.load(&bundle).expect_err("garbage must not load");
    assert!(
        matches!(error, DesktopRunnerError::Bytecode(_)),
        "got {error:?}"
    );
}

/// A module the VM rejects fails at link, not at start: linking is where
/// validation happens, and the session must be able to say so.
#[test]
fn an_invalid_module_fails_at_link() {
    let dir = TempDir::new("bad-link");
    let mut host = DesktopHost::new(dir.0.clone());
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        main: Some(0),
        strings: Vec::new(),
        functions: vec![FuncProto {
            name: "main".to_owned(),
            param_count: 0,
            local_count: 0,
            execution: Execution::Runtime,
            // A call to a function that does not exist: the VM's validator
            // is what catches this, and it runs at link.
            code: vec![Instruction::Call(99), Instruction::ReturnVoid],
            releases: FrameRelease::EveryLocal,
        }],
    };

    host.load(&vm_bundle(&module)).expect("load");
    let error = host.link().expect_err("an invalid module must not link");
    assert!(matches!(error, DesktopRunnerError::Vm(_)), "got {error:?}");
}

/// Linking twice is not an error: it is idempotent, so a retried message
/// cannot tear down a live session's linked state.
#[test]
fn linking_twice_is_idempotent() {
    let dir = TempDir::new("relink");
    let mut host = DesktopHost::new(dir.0.clone());
    host.load(&vm_bundle(&printing_module())).expect("load");
    host.link().expect("link");
    host.link().expect("link again");
    host.start().expect("start");
}
