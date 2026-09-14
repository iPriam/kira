use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use kira_runtime_abi::NativeStateToken;

/// Refcount changes that value ownership created while no host was available.
///
/// Heap operations can copy and destroy `NativeState` handles, but only the
/// interpreter owns the host that counts those references. Nested callback-state
/// nodes can also die from `Arc` copy-on-write outside a heap borrow, so the queue
/// is shared and records both sources in one place.
#[derive(Debug, Default)]
pub(super) struct NativeStateEvents {
    pending: AtomicUsize,
    retains: Mutex<Vec<NativeStateToken>>,
    releases: Mutex<Vec<NativeStateToken>>,
}

impl NativeStateEvents {
    pub(super) fn retain(&self, token: NativeStateToken) {
        self.retains
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .push(token);
        self.pending.fetch_add(1, Ordering::Release);
    }

    pub(super) fn release(&self, token: NativeStateToken) {
        self.releases
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .push(token);
        self.pending.fetch_add(1, Ordering::Release);
    }

    pub(super) fn has_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire) != 0
    }

    pub(super) fn take(&self) -> (Vec<NativeStateToken>, Vec<NativeStateToken>) {
        if self.pending.swap(0, Ordering::AcqRel) == 0 {
            return (Vec::new(), Vec::new());
        }
        let retains =
            std::mem::take(&mut *self.retains.lock().unwrap_or_else(|held| held.into_inner()));
        let releases = std::mem::take(
            &mut *self
                .releases
                .lock()
                .unwrap_or_else(|held| held.into_inner()),
        );
        (retains, releases)
    }
}
