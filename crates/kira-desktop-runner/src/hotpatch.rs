//! The shared VM hot-patch control used by the protocol and app threads.
//!
//! A desktop app spends its lifetime inside the native window loop, so the app
//! thread cannot answer a protocol-thread `swap` request. The VM backend has a
//! safe boundary anyway: native callback thunks enter the VM one callback at a
//! time. This control lets the protocol thread stage the new bytecode and swap
//! the VM program while the existing native window remains alive.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kira_bytecode::Module;
use kira_hybrid_runtime::Session;
use kira_live::{Bundle, PayloadKind};

use crate::host::DesktopRunnerError;

/// A reload controller shared by the runner's protocol and app threads.
#[derive(Clone)]
pub struct VmHotPatch {
    cache: PathBuf,
    active: Arc<Mutex<Option<Arc<Session>>>>,
}

impl std::fmt::Debug for VmHotPatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VmHotPatch")
            .field("cache", &self.cache)
            .field("active", &self.has_active_vm())
            .finish()
    }
}

impl VmHotPatch {
    /// Creates an inactive controller for a runner cache.
    pub fn new(cache: PathBuf) -> VmHotPatch {
        VmHotPatch {
            cache,
            active: Arc::new(Mutex::new(None)),
        }
    }

    /// Publishes the VM-only session currently owning the native window.
    pub fn activate(&self, session: Arc<Session>) {
        let mut active = self.active.lock().unwrap_or_else(|held| held.into_inner());
        *active = session.is_vm_only().then_some(session);
    }

    /// Clears the active session.
    pub fn clear(&self) {
        let mut active = self.active.lock().unwrap_or_else(|held| held.into_inner());
        *active = None;
    }

    /// Whether a VM-only session can accept a bytecode swap in place.
    pub fn has_active_vm(&self) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .is_some()
    }

    /// Stages the changed payloads and replaces the active VM program.
    ///
    /// `None` means this controller does not own the current session, so the
    /// caller should use the ordinary app-thread work queue instead.
    pub fn swap(&self, bundle: &Bundle) -> Result<Option<u64>, DesktopRunnerError> {
        let session = self
            .active
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone();
        let Some(session) = session else {
            return Ok(None);
        };

        crate::stage::restage_changed(&self.cache, bundle)?;
        let bytecode_index = bundle
            .manifest()
            .payloads
            .iter()
            .position(|payload| payload.kind == PayloadKind::VmBytecode)
            .ok_or(DesktopRunnerError::NoEntrypoint)?;
        let bytecode = bundle
            .payload_bytes(bytecode_index)
            .ok_or(DesktopRunnerError::NoEntrypoint)?;
        let module = Module::from_bytes(bytecode)?;
        Ok(Some(session.replace_vm_program(module)?))
    }

    /// Waits for a frame callback to enter the replacement program.
    pub fn wait_for_observation(&self, generation: u64) -> bool {
        let session = self
            .active
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone();
        session
            .is_some_and(|session| session.wait_for_vm_reload(generation, Duration::from_secs(5)))
    }
}
