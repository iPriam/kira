//! The desktop [`RunnerHost`]: what load, link, and start actually mean here.
//!
//! The three steps are kept genuinely separate rather than collapsed into one
//! "run it" call, because a live session's whole value is saying *which* step
//! failed. They map onto real work:
//!
//! - **load** — write the bundle into this runner's cache and decode the entry
//!   payload. Nothing has been resolved yet; this is bytes becoming structure.
//! - **link** — resolve what the payloads need. For the VM that is validating
//!   the module (every jump in range, every call bound). For a hybrid bundle it
//!   is `dlopen` plus binding each native symbol. This is where an unresolved
//!   symbol or a bad jump surfaces.
//! - **start** — actually run the entrypoint.
//!
//! A step that fails returns an error naming what failed; it never falls through
//! to the next step. A session that reports `bundle linked` here linked.

use std::path::{Path, PathBuf};

use kira_bytecode::{Module, ModuleDecodeError};
use kira_live::{Bundle, BundleError, PayloadKind, RunnerHost};
use kira_runtime_abi::HostCapabilities;
use kira_vm_runtime::{Program, VmError};

use crate::staged::Staged;

/// Why the desktop runner could not load, link, or start a bundle.
#[derive(Debug, thiserror::Error)]
pub enum DesktopRunnerError {
    /// The bundle could not be written to the runner's cache.
    #[error("could not stage the bundle at `{path}`: {source}")]
    Stage {
        /// Where it was being written.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The cache directory holds something this runner did not put there.
    ///
    /// Staging clears the cache, so it refuses a directory it cannot recognize
    /// as a previous stage rather than deleting whatever is in it.
    #[error("refusing to clear `{path}` to stage a bundle: {reason}")]
    CacheNotOurs {
        /// The directory that was going to be cleared.
        path: PathBuf,
        /// Why it was not recognized as a runner's cache.
        reason: &'static str,
    },
    /// The bundle itself was not usable.
    #[error("bundle is not usable: {0}")]
    Bundle(#[from] BundleError),
    /// The entry payload's bytecode did not decode.
    #[error("entry bytecode did not decode: {0}")]
    Bytecode(#[from] ModuleDecodeError),
    /// The VM rejected the module, or trapped running it.
    #[error("vm: {0}")]
    Vm(#[from] VmError),
    /// The hybrid session could not be loaded or run.
    #[error("hybrid: {0}")]
    Hybrid(#[from] kira_hybrid_runtime::HybridError),
    /// The bundle's entrypoint is a kind this runner does not host.
    ///
    /// Reported precisely rather than skipped: a runner that ignored a payload
    /// it did not understand would start an app that is missing half of itself
    /// and call the session ready.
    #[error(
        "the desktop runner cannot host a `{kind}` entrypoint; \
         it hosts `vm-bytecode` and `hybrid-manifest` entrypoints"
    )]
    UnsupportedEntry {
        /// The entry payload's kind.
        kind: &'static str,
    },
    /// The bundle's manifest named no entrypoint payload.
    #[error("the bundle's manifest names no entrypoint payload")]
    NoEntrypoint,
    /// A step was asked for before the one it depends on.
    ///
    /// A typed error rather than an `unwrap` on the staged state: this crate is
    /// driven by a protocol whose peer is a socket, and a library never gets to
    /// end its caller's process because a message arrived out of order.
    #[error("the desktop runner was asked to {step} before it had {required}")]
    OutOfOrder {
        /// The step that was asked for.
        step: &'static str,
        /// What had to happen first.
        required: &'static str,
    },
}

/// A host that prints what the app prints.
///
/// The app's output is the session's output: a live session's user is watching
/// this terminal, so `print` in Kira lands here.
struct StdoutHost;

impl HostCapabilities for StdoutHost {
    fn write_line(&mut self, text: &str) {
        println!("{text}");
    }
}

/// The desktop runner's host.
#[derive(Debug)]
pub struct DesktopHost {
    cache: PathBuf,
    staged: Staged,
    hotpatch_disabled: bool,
}

impl DesktopHost {
    /// A host that stages bundles under `cache`.
    ///
    /// The bundle is written to disk rather than kept in memory because a hybrid
    /// bundle's native half is a dynamic library, and `dlopen` takes a path: the
    /// OS loader is the one consumer here that cannot be handed bytes.
    ///
    /// The hot-patch kill switch is read here, once, rather than per reload: an
    /// environment variable that can change under a running session is a session
    /// that behaves two ways for one invocation.
    pub fn new(cache: PathBuf) -> DesktopHost {
        DesktopHost {
            cache,
            staged: Staged::Empty,
            hotpatch_disabled: kira_live::hotpatch_disabled_by_env(),
        }
    }

    /// A host with hot patching explicitly on or off, ignoring the environment.
    pub fn with_hotpatch_disabled(cache: PathBuf, disabled: bool) -> DesktopHost {
        DesktopHost {
            cache,
            staged: Staged::Empty,
            hotpatch_disabled: disabled,
        }
    }

    /// Where this host stages bundles.
    pub fn cache(&self) -> &Path {
        &self.cache
    }
}

impl RunnerHost for DesktopHost {
    type Error = DesktopRunnerError;

    fn load(&mut self, bundle: &Bundle) -> Result<(), DesktopRunnerError> {
        // Staged fresh each time: a leftover payload from a previous bundle must
        // never be what a later `dlopen` resolves against.
        crate::stage::stage_fresh(&self.cache, bundle)?;

        // Total in practice: a `Bundle` only exists with an in-range entry,
        // checked by both of its constructors. Handled rather than unwrapped —
        // this is a runner driven by a socket, and it does not get to panic.
        let entry = bundle
            .manifest()
            .entry_payload()
            .ok_or(DesktopRunnerError::NoEntrypoint)?;
        self.staged = match entry.kind {
            PayloadKind::VmBytecode => Staged::VmLoaded {
                module: Module::from_bytes(bundle.entry_bytes())?,
            },
            PayloadKind::HybridManifest => Staged::HybridLoaded {
                // The bundle's payload directory is the hybrid bundle's
                // directory: a KHM1 manifest names its bytecode and library as
                // file names beside itself, and staging every payload as a
                // sibling is exactly what makes that resolve.
                manifest: self.cache.join(kira_live::PAYLOAD_DIR).join(&entry.name),
            },
            kind @ (PayloadKind::NativeLibrary | PayloadKind::Asset) => {
                return Err(DesktopRunnerError::UnsupportedEntry { kind: kind.label() });
            }
        };
        Ok(())
    }

    fn link(&mut self) -> Result<(), DesktopRunnerError> {
        self.staged = match std::mem::replace(&mut self.staged, Staged::Empty) {
            Staged::VmLoaded { module } => Staged::VmLinked {
                // Validation is the VM's link step: it is where an out-of-range
                // jump or an unbound call becomes an error instead of a trap
                // halfway through a frame.
                program: Box::new(Program::load(module)?),
            },
            Staged::HybridLoaded { manifest } => Staged::HybridLinked {
                // Loading a hybrid session dlopens the native half and binds
                // every symbol the manifest names, so a missing symbol fails
                // here rather than at the first call.
                session: Box::new(kira_hybrid_runtime::Session::load(&manifest)?),
            },
            already @ (Staged::VmLinked { .. } | Staged::HybridLinked { .. }) => already,
            Staged::Empty => {
                return Err(DesktopRunnerError::OutOfOrder {
                    step: "link",
                    required: "loaded a bundle",
                });
            }
        };
        Ok(())
    }

    fn swap(&mut self, bundle: &Bundle) -> Result<(), DesktopRunnerError> {
        // A hot patch is an edit to something live. There has to be something
        // live: `swap` replaces a linked bundle, and a merely-loaded one has
        // nothing mapped that could survive.
        if !self.staged.is_linked() {
            return Err(DesktopRunnerError::OutOfOrder {
                step: "swap",
                required: "linked a bundle",
            });
        }

        // Everything below exists to keep one promise: the loaded native library
        // is still the loaded native library when this returns.
        //
        // Which means the file it was mapped from must not be touched. `load`
        // cannot be reused here — it clears the cache, and unlinking a mapped
        // dylib and writing a new one in its place gives the next `dlopen` a
        // different inode, so the loader maps a second copy and the old one is
        // whatever `dlclose` decided. Instead only the payloads whose hash
        // actually changed are rewritten. The supervisor only asks for a swap
        // when the library is byte-identical, so the library is exactly what
        // does not get rewritten, and `dlopen` on an unchanged path returns the
        // image that is already mapped.
        crate::stage::restage_changed(&self.cache, bundle)?;

        let entry = bundle
            .manifest()
            .entry_payload()
            .ok_or(DesktopRunnerError::NoEntrypoint)?;

        // The replacement is built *before* the old one is dropped, and this is
        // load-bearing rather than tidy. Dropping the old hybrid session first
        // would `dlclose` the library, and with its refcount at zero the loader
        // is free to unmap it — so the "same library" the next `dlopen` returns
        // could be a fresh mapping at a new address, with every pointer native
        // state held into the old image dangling. Opening the new handle while
        // the old is still open keeps the refcount above zero throughout, so
        // the image is never unmapped and the addresses stay put.
        let replacement = match entry.kind {
            PayloadKind::VmBytecode => Staged::VmLinked {
                program: Box::new(Program::load(Module::from_bytes(bundle.entry_bytes())?)?),
            },
            PayloadKind::HybridManifest => {
                let manifest = self.cache.join(kira_live::PAYLOAD_DIR).join(&entry.name);
                Staged::HybridLinked {
                    session: Box::new(kira_hybrid_runtime::Session::load(&manifest)?),
                }
            }
            kind @ (PayloadKind::NativeLibrary | PayloadKind::Asset) => {
                return Err(DesktopRunnerError::UnsupportedEntry { kind: kind.label() });
            }
        };

        // Only now: the old session drops here, after the new one holds the
        // library open. A failure above returned early with the old bundle still
        // linked and still running, which is what lets the runner report a
        // rejection and mean it.
        self.staged = replacement;
        Ok(())
    }

    fn hotpatch_disabled(&self) -> bool {
        self.hotpatch_disabled
    }

    fn start(&mut self) -> Result<(), DesktopRunnerError> {
        match &self.staged {
            Staged::VmLinked { program } => {
                let mut host = StdoutHost;
                program.run(&mut host)?;
                Ok(())
            }
            Staged::HybridLinked { session } => {
                session.run()?;
                Ok(())
            }
            Staged::VmLoaded { .. } | Staged::HybridLoaded { .. } => {
                Err(DesktopRunnerError::OutOfOrder {
                    step: "start",
                    required: "linked the bundle",
                })
            }
            Staged::Empty => Err(DesktopRunnerError::OutOfOrder {
                step: "start",
                required: "loaded a bundle",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_bytecode::{FuncProto, Instruction};
    use kira_live::{NamedPayload, PayloadKind};
    use kira_manifest::{BuildProfile, RunnerId};
    use kira_runtime_abi::Execution;
    use std::fs;

    /// A scratch directory that removes itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let path = std::env::temp_dir()
                .join(format!("kira-desktop-runner-{}-{tag}", std::process::id()));
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
}
