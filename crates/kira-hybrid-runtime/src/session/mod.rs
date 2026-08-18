//! A loaded hybrid program, and the two directions it runs in.
//!
//! A [`Session`] owns both halves: the [`Program`] the VM runs and the
//! [`NativeLibrary`] the machine code lives in. It is the embedder for the
//! first and the host for the second, which is what lets the two call each
//! other without either learning about the other's world.
//!
//! # Why the host holds no mutable state
//!
//! The VM takes its host as `&mut dyn HostCapabilities` and holds it for the
//! whole run. Native code called *through* that borrow can call back and needs
//! a host again — so a single long-lived `&mut` host would have to be aliased,
//! which it may not be. The way out is to make the host worth nothing: [`Host`]
//! is a handle over a shared `&Session` and carries no state of its own, so a
//! fresh one is built per nesting level and the `&mut` is only ever to the
//! handle. `write_line` goes straight to stdout, so there is nothing to
//! synchronize between levels.
//!
//! # Why the session is found through a thread-local
//!
//! The invoker the library calls back through is a bare `extern "C" fn` with no
//! user-data pointer, so it cannot close over the session. It finds it in a
//! thread-local instead, set for the run's duration by a guard and cleared when
//! the guard drops. Native code calling back from a thread the host never
//! entered therefore finds nothing — and says so and exits, rather than running
//! against a null pointer.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};

use kira_bytecode::module::Module;
use kira_dynamic_ffi::{ForeignLibrary, PROCESS_BINDING_MARKER};
use kira_hybrid_definition::HybridManifest;
use kira_libffi::{FfiClosure, LibffiRuntime, RawFfiCif};
use kira_runtime_abi::{
    BridgeValue, Execution, FileRequest, FileResponse, FileSystemError, ForeignArg,
    ForeignCallError, ForeignResult, ForeignSignature, ForeignType, ForeignTypeSpec,
    HostCapabilities, LinuxSyscall, NativeArg, NativeCallError, NativeResult, NativeReturn,
    NativeStateError, NativeStatePathStep, NativeStateStore, NativeStateToken, NativeStateTypeId,
    NativeStateValue, SyscallError, file_system, native_state_walk, native_state_walk_mut, syscall,
};
use kira_vm_runtime::{Program, debug::VmDebugObserver};

use crate::error::HybridError;
use crate::library::NativeLibrary;
use crate::marshal::{self, OwnedArg};
use crate::validate;

thread_local! {
    /// The session the invoker on this thread should call back into.
    ///
    /// Null when no session is running here, which is the case this must be
    /// able to represent: the library may call back from anywhere.
    static ACTIVE_SESSION: Cell<*const Session> = const { Cell::new(ptr::null()) };
}

/// A loaded hybrid program: its bytecode half, its native half, and the
/// manifest tying them together.
pub struct Session {
    manifest: HybridManifest,
    /// The program used for the next VM entry or native callback.
    ///
    /// A graphics app's entrypoint is suspended inside the native window loop
    /// for most of its lifetime. The native callback thunks can still enter the
    /// VM while that happens, so a live VM reload replaces this slot rather
    /// than trying to interrupt the suspended entrypoint. An `Arc` keeps the
    /// program that is already executing alive until its current callback
    /// returns.
    program: RwLock<VmProgram>,
    library: NativeLibrary,
    foreign_libraries: Vec<ForeignLibrary>,
    foreign_paths: Vec<Option<PathBuf>>,
    libffi: Option<LibffiRuntime>,
    callback_registry: Mutex<CallbackRegistry>,
    /// VM callback state for an all-runtime hybrid bundle.
    ///
    /// The VM's path-addressed store keeps field access proportional to the
    /// field instead of copying the whole callback value.
    vm_state: Option<Mutex<NativeStateStore>>,
    /// The generation of the latest program swap, and the generation observed
    /// by a callback after that swap. The runner waits for the latter before it
    /// reports a reload complete, so a committed swap is not mistaken for a
    /// frame that never used the new code.
    observed_generation: AtomicU64,
    observed_wait: Condvar,
    observed_lock: Mutex<()>,
}

/// A validated VM program plus the reload generation it belongs to.
struct VmProgram {
    generation: u64,
    program: Arc<Program>,
    /// Maps function ids baked into the loaded native callback thunks to the
    /// corresponding function in this generation's module.
    callback_ids: Arc<Vec<u32>>,
}

impl Session {
    /// Loads the bundle `manifest_path` describes: both payloads, bound and
    /// proven consistent.
    ///
    /// Payload paths are resolved relative to the manifest's own directory
    /// unless they are absolute, so a built bundle can be moved as a unit.
    pub fn load(manifest_path: &Path) -> Result<Session, HybridError> {
        let bytes = read(manifest_path)?;
        let manifest =
            HybridManifest::from_bytes(&bytes).map_err(|source| HybridError::Manifest {
                path: manifest_path.to_path_buf(),
                source,
            })?;

        let base = manifest_path.parent().unwrap_or(Path::new("."));
        let bytecode_path = resolve(base, &manifest.bytecode_path);
        let library_path = resolve(base, &manifest.native_library_path);

        let module_bytes = read(&bytecode_path)?;
        let module = Module::from_bytes(&module_bytes).map_err(|source| HybridError::Bytecode {
            path: bytecode_path,
            source,
        })?;

        // Prove the halves agree before either is trusted: the manifest is what
        // every crossing marshals against.
        validate::bundle(&manifest, &module)?;

        let program = Arc::new(Program::load(module).map_err(HybridError::Program)?);
        let callback_ids = Arc::new(
            (0..manifest.functions.len())
                .map(|index| index as u32)
                .collect(),
        );
        let vm_state = manifest
            .functions
            .iter()
            .all(|function| function.execution == Execution::Runtime)
            .then(|| Mutex::new(NativeStateStore::new()));
        let callbacks = program.module().foreign_callbacks.len();
        let library = NativeLibrary::load(&library_path, &manifest.functions, callbacks)?;
        let mut foreign_paths = Vec::with_capacity(manifest.foreign.len());
        let mut foreign_libraries = Vec::new();
        for import in &manifest.foreign {
            if import.adapter_symbol.is_empty() {
                foreign_paths.push(None);
                continue;
            }
            let process = import.adapter_symbol == PROCESS_BINDING_MARKER;
            let path = if process {
                PathBuf::from(PROCESS_BINDING_MARKER)
            } else {
                resolve_foreign_path(base, &import.adapter_symbol)
            };
            if !foreign_libraries
                .iter()
                .any(|library: &ForeignLibrary| library.path() == path)
            {
                let runtime_path = base.join(kira_libffi::bundled_file_name());
                let library = if process {
                    ForeignLibrary::load_process_with_runtime_path(
                        manifest.foreign_aggregates.clone(),
                        runtime_path,
                    )?
                } else {
                    ForeignLibrary::load_with_runtime_path(
                        &path,
                        manifest.foreign_aggregates.clone(),
                        runtime_path,
                    )?
                };
                foreign_libraries.push(library);
            }
            foreign_paths.push(Some(path));
        }
        let runtime_path = base.join(kira_libffi::bundled_file_name());
        let libffi = (callbacks != 0)
            .then(|| LibffiRuntime::load_from(&runtime_path))
            .transpose()?;
        let callback_closures = (0..callbacks).map(|_| None).collect();

        Ok(Session {
            manifest,
            program: RwLock::new(VmProgram {
                generation: 0,
                program,
                callback_ids,
            }),
            library,
            foreign_libraries,
            foreign_paths,
            libffi,
            callback_registry: Mutex::new(CallbackRegistry {
                closures: callback_closures,
                contexts: Vec::new(),
            }),
            vm_state,
            observed_generation: AtomicU64::new(0),
            observed_wait: Condvar::new(),
            observed_lock: Mutex::new(()),
        })
    }

    /// The manifest this session was loaded from.
    pub fn manifest(&self) -> &HybridManifest {
        &self.manifest
    }

    /// Whether every Kira function in this session is running on the VM.
    ///
    /// An all-runtime session can replace its VM program at callback boundaries.
    /// A mixed session cannot make that promise because native code may still
    /// hold a stack into the old native image.
    pub fn is_vm_only(&self) -> bool {
        self.manifest
            .functions
            .iter()
            .all(|function| function.execution == Execution::Runtime)
    }

    /// Replaces the VM half while preserving the loaded native library
    /// and callback-state store.
    ///
    /// The manifest is intentionally validated again. The loaded native half
    /// remains in place, so a changed function signature, callback table, or
    /// foreign boundary must be rejected and handled by the supervisor's
    /// relaunch path rather than being allowed to cross an old ABI.
    pub fn replace_vm_program(&self, module: Module) -> Result<u64, HybridError> {
        if !self.is_vm_only() {
            return Err(HybridError::Mismatch(
                "a mixed hybrid session cannot replace its VM half while running".to_owned(),
            ));
        }
        let callback_ids = {
            let slot = self.program.read().unwrap_or_else(|held| held.into_inner());
            validate::hot_reload(&self.manifest, slot.program.module(), &module)
                .map_err(|error| HybridError::Mismatch(error.to_string()))?
        };
        let program = Arc::new(Program::load(module).map_err(HybridError::Program)?);
        let mut slot = self
            .program
            .write()
            .unwrap_or_else(|held| held.into_inner());
        let generation = slot.generation.saturating_add(1);
        slot.generation = generation;
        slot.program = program;
        slot.callback_ids = Arc::new(callback_ids);
        // The retained UI must rebuild once after this swap. The marker lives
        // in the loaded adapter library, not in the runner, so the callback
        // consumes the same state even though the swap request came from the
        // protocol thread.
        self.library.mark_live_reload();
        Ok(generation)
    }

    /// Waits until a callback has entered the newly swapped program.
    pub fn wait_for_vm_reload(&self, generation: u64, timeout: std::time::Duration) -> bool {
        if self.observed_generation.load(Ordering::Acquire) >= generation {
            return true;
        }
        let guard = self
            .observed_lock
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        let (guard, _) = self
            .observed_wait
            .wait_timeout_while(guard, timeout, |_| {
                self.observed_generation.load(Ordering::Acquire) < generation
            })
            .unwrap_or_else(|held| held.into_inner());
        drop(guard);
        self.observed_generation.load(Ordering::Acquire) >= generation
    }

    /// Takes the program currently selected for a callback or entrypoint.
    fn current_program(&self) -> (Arc<Program>, u64, Arc<Vec<u32>>) {
        let slot = self.program.read().unwrap_or_else(|held| held.into_inner());
        (
            Arc::clone(&slot.program),
            slot.generation,
            Arc::clone(&slot.callback_ids),
        )
    }

    /// Records that a callback ran through `generation`.
    fn observe_generation(&self, generation: u64) {
        let observed = self.observed_generation.load(Ordering::Acquire);
        if observed < generation {
            self.observed_generation
                .store(generation, Ordering::Release);
            self.observed_wait.notify_all();
        }
    }
}

mod active;
mod callback;
mod host;
mod run;

use active::ActiveSession;
use callback::{CallbackContext, CallbackRegistry};
use host::Host;

/// Reports a condition the seam cannot return from, and exits.
///
/// Spelled the way `kira_rt_trap_div_zero` spells its own: this is reached from
/// inside native code, and which side of the boundary a trap happened on is not
/// something a user should have to notice.
fn fatal(message: &str) -> ! {
    eprintln!("kira: {message}");
    std::process::exit(1);
}

/// Reads a file, naming it on failure.
fn read(path: &Path) -> Result<Vec<u8>, HybridError> {
    std::fs::read(path).map_err(|source| HybridError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Resolves a manifest-recorded payload path against the manifest's directory.
fn resolve(base: &Path, recorded: &str) -> PathBuf {
    let path = Path::new(recorded);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// Resolves a foreign-library path recorded by the direct Libffi manifest.
fn resolve_foreign_path(base: &Path, recorded: &str) -> PathBuf {
    resolve(base, recorded)
}

/// Asks the native half for its heap balance when a run ends, however it ends.
///
/// A guard rather than a call at the end of `run`, because a run that traps
/// leaves through `?` and a leak is exactly as interesting on that path — more
/// so, since an early return is where a release is most often skipped.
struct HeapReportAtExit<'a> {
    library: &'a NativeLibrary,
}

impl<'a> HeapReportAtExit<'a> {
    fn new(library: &'a NativeLibrary) -> Self {
        HeapReportAtExit { library }
    }
}

impl Drop for HeapReportAtExit<'_> {
    fn drop(&mut self) {
        self.library.report_heap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_payload_resolves_against_the_manifest_directory() {
        assert_eq!(
            resolve(Path::new("/tmp/build"), "demo.kbc"),
            PathBuf::from("/tmp/build/demo.kbc"),
        );
    }

    #[test]
    fn an_absolute_payload_is_left_alone() {
        assert_eq!(
            resolve(Path::new("/tmp/build"), "/elsewhere/demo.kbc"),
            PathBuf::from("/elsewhere/demo.kbc"),
        );
    }

    /// No session is installed outside a run, which is what stops a stray
    /// callback from running against a null pointer.
    #[test]
    fn no_session_is_active_on_a_fresh_thread() {
        assert!(ACTIVE_SESSION.get().is_null());
    }
}
