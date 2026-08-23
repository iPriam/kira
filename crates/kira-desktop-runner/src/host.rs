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
use std::sync::Arc;

use kira_bytecode::{Module, ModuleDecodeError};
use kira_live::{Bundle, BundleError, PayloadKind, RunnerHost};
use kira_main::{ForeignBinding, ForeignSession};
use kira_runtime_abi::{HostCapabilities, NativeStateHost};
use kira_vm_runtime::{Program, VmError};

use crate::VmHotPatch;
use crate::native::NativeProgram;
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
    /// A VM direct foreign session could not be loaded.
    #[error("foreign session: {0}")]
    ForeignSession(#[from] kira_dynamic_ffi::ForeignLibraryError),
    /// A whole-program native live library could not be loaded or completed.
    #[error("native program: {0}")]
    Native(#[from] crate::native::NativeProgramError),
    /// The VM bytecode declares a foreign boundary but the bundle has no
    /// direct binding metadata payload for it.
    #[error("vm bytecode requires direct foreign binding metadata")]
    MissingForeignBindings,
    /// A VM direct binding manifest is malformed or names an invalid payload.
    #[error("foreign binding manifest `{path}` is invalid at line {line}: {reason}")]
    InvalidForeignBindings {
        /// The staged binding manifest.
        path: PathBuf,
        /// The one-based manifest line that failed validation.
        line: usize,
        /// Why the line cannot be consumed as a binding.
        reason: String,
    },
    /// The bundle contains more than one direct binding metadata payload.
    #[error("bundle contains more than one foreign binding manifest: `{path}`")]
    DuplicateForeignBindings {
        /// The later manifest payload that made the bundle ambiguous.
        path: PathBuf,
    },
    /// The bundle's entrypoint is a kind this runner does not host.
    ///
    /// Reported precisely rather than skipped: a runner that ignored a payload
    /// it did not understand would start an app that is missing half of itself
    /// and call the session ready.
    #[error(
        "the desktop runner cannot host a `{kind}` entrypoint; \
         it hosts `vm-bytecode`, `native-library`, and `hybrid-manifest` entrypoints"
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
    hotpatch: VmHotPatch,
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
            hotpatch: VmHotPatch::new(cache.clone()),
            cache,
            staged: Staged::Empty,
            hotpatch_disabled: kira_live::hotpatch_disabled_by_env(),
        }
    }

    /// A host with hot patching explicitly on or off, ignoring the environment.
    pub fn with_hotpatch_disabled(cache: PathBuf, disabled: bool) -> DesktopHost {
        DesktopHost {
            hotpatch: VmHotPatch::new(cache.clone()),
            cache,
            staged: Staged::Empty,
            hotpatch_disabled: disabled,
        }
    }

    /// Where this host stages bundles.
    pub fn cache(&self) -> &Path {
        &self.cache
    }

    /// Whether this host refuses hot patching outright.
    pub fn hotpatch_disabled(&self) -> bool {
        self.hotpatch_disabled
    }

    /// The VM reload control for the app thread.
    pub fn hotpatch(&self) -> VmHotPatch {
        self.hotpatch.clone()
    }

    /// The thread-safe VM hot-patch status for the protocol relay.
    pub fn hotpatch_status(&self) -> crate::hotpatch::VmHotPatchStatus {
        self.hotpatch.status()
    }

    /// Why the entrypoint could not start, or `Ok(())` if it can.
    ///
    /// The same answer [`RunnerHost::start`] would give without running
    /// anything, which is what a runner needs when the app it is about to start
    /// will outlive the call: whether the app started is reported the moment it
    /// does, so it has to be knowable the moment before.
    pub fn startable(&self) -> Result<(), DesktopRunnerError> {
        if self.staged.is_linked() {
            return Ok(());
        }
        Err(DesktopRunnerError::OutOfOrder {
            step: "start",
            required: required_before_start(&self.staged),
        })
    }
}

/// What has to have happened before the entrypoint can start.
fn required_before_start(staged: &Staged) -> &'static str {
    match staged {
        Staged::Empty => "loaded a bundle",
        _ => "linked the bundle",
    }
}

impl RunnerHost for DesktopHost {
    type Error = DesktopRunnerError;

    fn load(&mut self, bundle: &Bundle) -> Result<(), DesktopRunnerError> {
        self.hotpatch.clear();
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
                bindings: foreign_bindings_from_bundle(&self.cache, bundle)?,
            },
            PayloadKind::HybridManifest => Staged::HybridLoaded {
                // The bundle's payload directory is the hybrid bundle's
                // directory: a KHM1 manifest names its bytecode and library as
                // file names beside itself, and staging every payload as a
                // sibling is exactly what makes that resolve.
                manifest: self.cache.join(kira_live::PAYLOAD_DIR).join(&entry.name),
            },
            PayloadKind::NativeLibrary => Staged::NativeLoaded {
                library: self.cache.join(kira_live::PAYLOAD_DIR).join(&entry.name),
            },
            kind @ (PayloadKind::Asset
            | PayloadKind::ForeignAdapter
            | PayloadKind::NativeDependency
            | PayloadKind::ForeignBindings) => {
                return Err(DesktopRunnerError::UnsupportedEntry { kind: kind.label() });
            }
        };
        Ok(())
    }

    fn link(&mut self) -> Result<(), DesktopRunnerError> {
        self.staged = match std::mem::replace(&mut self.staged, Staged::Empty) {
            Staged::VmLoaded { module, bindings } => link_vm(module, bindings)?,
            Staged::HybridLoaded { manifest } => Staged::HybridLinked {
                // Loading a hybrid session dlopens the native half and binds
                // every symbol the manifest names, so a missing symbol fails
                // here rather than at the first call.
                session: {
                    let session = Arc::new(kira_hybrid_runtime::Session::load(&manifest)?);
                    self.hotpatch.activate(Arc::clone(&session));
                    session
                },
            },
            Staged::NativeLoaded { library } => Staged::NativeLinked {
                program: NativeProgram::load(&library)?,
            },
            already @ (Staged::VmLinked { .. }
            | Staged::VmForeignLinked { .. }
            | Staged::NativeLinked { .. }
            | Staged::HybridLinked { .. }) => already,
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
        if self.hotpatch.has_active_vm() {
            self.hotpatch.swap(bundle)?;
            return Ok(());
        }
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
            PayloadKind::VmBytecode => link_vm(
                Module::from_bytes(bundle.entry_bytes())?,
                foreign_bindings_from_bundle(&self.cache, bundle)?,
            )?,
            PayloadKind::HybridManifest => {
                let manifest = self.cache.join(kira_live::PAYLOAD_DIR).join(&entry.name);
                Staged::HybridLinked {
                    session: Arc::new(kira_hybrid_runtime::Session::load(&manifest)?),
                }
            }
            PayloadKind::NativeLibrary => {
                let library = self.cache.join(kira_live::PAYLOAD_DIR).join(&entry.name);
                Staged::NativeLinked {
                    program: NativeProgram::load(&library)?,
                }
            }
            kind @ (PayloadKind::Asset
            | PayloadKind::ForeignAdapter
            | PayloadKind::NativeDependency
            | PayloadKind::ForeignBindings) => {
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

    fn hot_patch_refusal(&self) -> Option<String> {
        self.hotpatch_disabled
            .then(kira_live::hotpatch_kill_switch_reason)
    }

    fn start(&mut self) -> Result<(), DesktopRunnerError> {
        match &self.staged {
            Staged::VmLinked { program } => {
                // The same stack `kira run` puts under a VM program: callback
                // state is portable storage the host provides, not something the
                // VM carries, so a runner that hands over a bare `StdoutHost`
                // traps the moment an app boxes state for a native callback —
                // which every UI app does, on its first frame.
                let mut host = NativeStateHost::new(StdoutHost);
                program.run(&mut host)?;
                Ok(())
            }
            Staged::VmForeignLinked { session } => {
                session.run()?;
                Ok(())
            }
            Staged::HybridLinked { session } => {
                session.run()?;
                Ok(())
            }
            Staged::NativeLinked { program } => {
                program.run()?;
                Ok(())
            }
            not_linked => Err(DesktopRunnerError::OutOfOrder {
                step: "start",
                required: required_before_start(not_linked),
            }),
        }
    }
}

/// Parses the VM live binding payload into loader paths and process markers the
/// foreign session can open.
///
/// Live manifests carry names rather than build-machine paths. A name matching
/// a `NativeDependency` resolves inside the runner cache; any other plain name
/// is left for the platform loader to resolve, which preserves system-library
/// bindings without making them fake bundle payloads.
fn foreign_bindings_from_bundle(
    cache: &Path,
    bundle: &Bundle,
) -> Result<Option<Vec<Option<PathBuf>>>, DesktopRunnerError> {
    let payload_directory = cache.join(kira_live::PAYLOAD_DIR);
    let mut manifest_path = None;
    for payload in &bundle.manifest().payloads {
        if payload.kind != PayloadKind::ForeignBindings {
            continue;
        }
        let path = payload_directory.join(&payload.name);
        if manifest_path.replace(path.clone()).is_some() {
            return Err(DesktopRunnerError::DuplicateForeignBindings { path });
        }
    }
    let Some(path) = manifest_path else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&path).map_err(|source| DesktopRunnerError::Stage {
        path: path.clone(),
        source,
    })?;
    let mut bindings = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        if line.is_empty() {
            bindings.push(None);
            continue;
        }
        if line == kira_dynamic_ffi::PROCESS_BINDING_MARKER {
            bindings.push(Some(PathBuf::from(line)));
            continue;
        }
        if !is_plain_loader_name(line) {
            return Err(DesktopRunnerError::InvalidForeignBindings {
                path: path.clone(),
                line: line_number,
                reason: "binding names must be plain file or loader names".to_owned(),
            });
        }
        let binding = match bundle.manifest().payload(line) {
            Some(payload) if payload.kind == PayloadKind::NativeDependency => {
                let staged = payload_directory.join(line);
                if !staged.is_file() {
                    return Err(DesktopRunnerError::InvalidForeignBindings {
                        path: path.clone(),
                        line: line_number,
                        reason: format!(
                            "native dependency payload `{line}` was not staged as a file"
                        ),
                    });
                }
                Some(staged)
            }
            Some(payload) => {
                return Err(DesktopRunnerError::InvalidForeignBindings {
                    path: path.clone(),
                    line: line_number,
                    reason: format!(
                        "binding name `{line}` refers to a `{}` payload, not a native dependency",
                        payload.kind.label()
                    ),
                });
            }
            None => Some(PathBuf::from(line)),
        };
        bindings.push(binding);
    }
    Ok(Some(bindings))
}

/// Accepts only the relocatable names written by a live bundle.
fn is_plain_loader_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !Path::new(name).is_absolute()
}

/// Links a VM module with the ordinary VM foreign-session contract.
fn link_vm(
    module: Module,
    bindings: Option<Vec<Option<PathBuf>>>,
) -> Result<Staged, DesktopRunnerError> {
    let program = Program::load(module)?;
    if program.module().foreign_imports.is_empty() && program.module().foreign_callbacks.is_empty()
    {
        return Ok(Staged::VmLinked {
            program: Arc::new(program),
        });
    }

    let paths = bindings.unwrap_or_default();
    if paths.len() != program.module().foreign_imports.len() {
        return Err(DesktopRunnerError::MissingForeignBindings);
    }
    let imports = program
        .module()
        .foreign_imports
        .iter()
        .zip(paths)
        .map(|(entry, path)| {
            // An address import binds its symbol exactly as a call does; what
            // differs is that nothing is invoked after the lookup. The manifest
            // carries that in the ABI, and it has to survive being rebuilt here
            // or the session calls an object's first bytes.
            let binding = path.map_or_else(
                || ForeignBinding::unavailable(entry.signature().clone()),
                |path| {
                    if path == Path::new(kira_dynamic_ffi::PROCESS_BINDING_MARKER) {
                        ForeignBinding::process(entry.symbol(), entry.signature().clone())
                    } else {
                        ForeignBinding::dynamic(path, entry.symbol(), entry.signature().clone())
                    }
                },
            );
            if entry.abi().answers_an_address() {
                binding.answering_address()
            } else {
                binding
            }
        })
        .collect();
    let callbacks = program
        .module()
        .foreign_callbacks
        .iter()
        .map(|callback| callback.signature().clone())
        .collect();
    let aggregates = program.module().foreign_aggregates.clone();
    let session = ForeignSession::load_dynamic(program, imports, callbacks, aggregates)?;
    Ok(Staged::VmForeignLinked {
        session: Arc::new(session),
    })
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
