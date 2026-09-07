//! Main-thread target dispatch for a foreign-capable VM session.

use kira_runtime_abi::{HostCapabilities, MainThreadError, NativeStateValue};
use kira_vm_runtime::MainThreadRunner;

use super::ForeignSession;

/// Runs a main-thread target from a VM-only foreign session.
pub(super) struct ForeignMainThreadRunner<'a> {
    pub(super) session: &'a ForeignSession,
}

impl MainThreadRunner for ForeignMainThreadRunner<'_> {
    fn call(
        &self,
        host: &mut dyn HostCapabilities,
        function: u32,
        args: &[NativeStateValue],
    ) -> Result<Option<NativeStateValue>, MainThreadError> {
        self.session
            .program
            .call_state(host, function, args)
            .map_err(|error| MainThreadError::Function(error.to_string()))
    }
}
