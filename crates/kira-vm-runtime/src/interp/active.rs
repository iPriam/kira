//! Re-entry into a VM suspended at a native seam.
//!
//! While the interpreter waits on a synchronous host call, native code may
//! call a runtime function back — a graphics callback asking Kira to build a
//! frame. The guard here publishes the suspended VM for the duration of the
//! host call, and `call_active` lends its heap to a nested dispatch so the
//! callback reads and writes the same objects the suspended frames hold.

use kira_bytecode::module::Module;
use kira_runtime_abi::HostCapabilities;

use super::Vm;
use crate::error::VmError;

struct ActiveVmContext {
    vm: *mut (),
    module: *const Module,
}

thread_local! {
    static ACTIVE_VM: std::cell::Cell<*mut ActiveVmContext> = const {
        std::cell::Cell::new(std::ptr::null_mut())
    };
}

pub(super) struct ActiveVmGuard {
    previous: *mut ActiveVmContext,
    _context: Box<ActiveVmContext>,
}

impl ActiveVmGuard {
    pub(super) fn install(vm: *mut Vm<'_>, module: &Module) -> Self {
        let mut context = Box::new(ActiveVmContext {
            vm: vm.cast(),
            module: std::ptr::from_ref(module),
        });
        let current = std::ptr::from_mut(context.as_mut());
        let previous = ACTIVE_VM.with(|active| active.replace(current));
        Self {
            previous,
            _context: context,
        }
    }
}

impl Drop for ActiveVmGuard {
    fn drop(&mut self) {
        ACTIVE_VM.with(|active| active.set(self.previous));
    }
}

/// Calls a runtime function on the VM currently suspended at a native seam.
///
/// `None` means the caller is outside a VM call, so it must create the ordinary
/// standalone callback VM instead.
pub fn call_active(
    function_id: u32,
    args: &[kira_runtime_abi::NativeArg<'_>],
    capture: &[u32],
) -> Option<Result<kira_runtime_abi::NativeReturn, VmError>> {
    ACTIVE_VM.with(|active| {
        let context = active.get();
        // SAFETY: a non-null `ACTIVE_VM` is the frame this thread is executing
        // inside, so the context outlives this reentrant call.
        (!context.is_null()).then(|| unsafe { call_on_active(context, function_id, args, capture) })
    })
}

/// Re-enters the active VM's heap with a caller-supplied host.
///
/// A native callback can arrive while the active VM's host is holding a
/// synchronization guard around the native call. Reusing that host for the
/// nested VM would deadlock when the nested body crosses the native seam
/// again. The hybrid session supplies a fresh stateless host here while the
/// active VM still supplies its heap and capture cells.
pub fn call_active_with_host(
    function_id: u32,
    args: &[kira_runtime_abi::NativeArg<'_>],
    capture: &[u32],
    host: &mut dyn HostCapabilities,
) -> Option<Result<kira_runtime_abi::NativeReturn, VmError>> {
    ACTIVE_VM.with(|active| {
        let context = active.get();
        // SAFETY: a non-null `ACTIVE_VM` is the frame this thread is executing
        // inside, so the context outlives this reentrant call.
        (!context.is_null())
            .then(|| unsafe { call_on_active_with_host(context, function_id, args, capture, host) })
    })
}

unsafe fn call_on_active(
    context: *mut ActiveVmContext,
    function_id: u32,
    args: &[kira_runtime_abi::NativeArg<'_>],
    capture: &[u32],
) -> Result<kira_runtime_abi::NativeReturn, VmError> {
    // SAFETY: the context is installed only around a synchronous host call;
    // both pointers remain live until that call returns.
    let context = unsafe { &*context };
    // SAFETY: the active VM is suspended while native code calls back, so its
    // heap can be lent to a nested interpreter without touching its frames.
    let vm = unsafe { &mut *context.vm.cast::<Vm<'_>>() };
    // SAFETY: the module belongs to the suspended VM call and remains live for
    // the same duration as the context.
    let module = unsafe { &*context.module };
    vm.call_capturing_on_shared_heap(module, function_id, args, capture)
}

unsafe fn call_on_active_with_host(
    context: *mut ActiveVmContext,
    function_id: u32,
    args: &[kira_runtime_abi::NativeArg<'_>],
    capture: &[u32],
    host: &mut dyn HostCapabilities,
) -> Result<kira_runtime_abi::NativeReturn, VmError> {
    // SAFETY: the context and module invariants are the same as
    // `call_on_active`; only the host is supplied by the callback's session.
    let context = unsafe { &*context };
    // SAFETY: the active VM is suspended while native code calls back, so its
    // heap can be lent to a nested interpreter without touching its frames.
    let vm = unsafe { &mut *context.vm.cast::<Vm<'_>>() };
    // SAFETY: the module belongs to the suspended VM call and remains live for
    // the same duration as the context.
    let module = unsafe { &*context.module };
    vm.call_capturing_on_shared_heap_with_host(module, function_id, args, capture, host)
}
