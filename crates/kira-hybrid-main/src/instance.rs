//! A running hybrid library: one heap, two engines, and the host between them.
//!
//! # What `@Runtime` and `@Native` mean when there is no `@Main`
//!
//! Exactly what they have always meant: **which engine runs a function's body.**
//! That is the whole answer, and it is worth stating plainly because the split
//! was built to serve live-reload of applications, and a library has no
//! application to reload.
//!
//! What a library changes is not the split but the *entry*. An application is
//! entered at its `@Main`, on whichever engine owns it. A library is entered by
//! its consumer, one call at a time — and the consumer always enters through the
//! **bytecode half**. So:
//!
//! - `@Runtime` (the default) — the body is bytecode, run on this instance's VM.
//! - `@Native` — the body is machine code in the shared library, reached from
//!   Kira through the seam exactly as it is in an application.
//! - `@Export` — the consumer-facing surface, and it names *runtime* functions.
//!   An `@Export` that is also `@Native` is refused at build time, by name; see
//!   below.
//!
//! That combination is what makes this engine worth having rather than a parity
//! checkbox. A VM-engine library compiles every function to bytecode; a
//! native-engine library compiles every function to machine code. The hybrid
//! engine is the only one where an author can put the hot inner function in
//! machine code and keep the surface — and the handles, and the strings — on the
//! VM, which is what the `@Native` annotation was for in the first place.
//!
//! # Why an exported function may not itself be `@Native`
//!
//! Handles. A handle is a root into *this instance's* heap: `make_button`
//! returns a `Button` because the VM rooted the object it built and handed back
//! a ticket. Machine code has no access to that heap and no way to mint a root
//! in it, so a `@Native` export returning a class would have to allocate in a
//! second heap, and the consumer would be holding two different things behind
//! one `Button` newtype — with one destructor. Refused at build time rather than
//! papered over, and refused for the whole surface rather than only for the
//! exports that happen to mention a class, because "this export may be `@Native`
//! and that one may not" is a rule nobody can hold in their head.
//!
//! # Why native code may not call back into the runtime here
//!
//! [`kira_main::Instance::call`] takes `&mut self` — it owns a heap, which is
//! the whole reason a handle can outlive a call. A native function calling back
//! into a `@Runtime` function *from inside* an exported call would need a second
//! `&mut` to the same instance, which is not a borrow that can be taken. An
//! application's hybrid session has no such problem because it runs on a
//! [`Program`](kira_vm_runtime::Program), which holds no heap and calls through
//! `&self`.
//!
//! So this engine installs **no runtime invoker**, and a hybrid library whose
//! native half calls a `@Runtime` function is refused at build time, by
//! function name. The C-level backstop is already there and already loud: with
//! no invoker installed, `kira_hybrid_call_runtime` names the function and
//! aborts rather than calling a null pointer.

use std::rc::Rc;

use kira_hybrid_runtime::NativeLibrary;
use kira_main::{Handle, Instance as VmLibraryInstance, StdoutHost};
use kira_runtime_abi::{HostCapabilities, NativeArg, NativeCallError, NativeResult, NativeReturn};

use crate::error::HybridMainError;

/// A host that serves the consumer's output and the library's own seam.
///
/// The consumer supplies a [`HostCapabilities`] to say where `print` goes. The
/// hybrid engine needs one more thing from a host that the VM engine does not:
/// `call_native`, the route from a `@Runtime` function to a `@Native` one. This
/// wraps the first to provide the second, so a consumer's host stays a plain
/// host and never learns that a native half exists.
struct SeamHost<H: HostCapabilities> {
    /// Where the library's output goes: the consumer's own host.
    inner: H,
    /// The loaded native half, shared with the instance that owns it.
    ///
    /// `Rc` rather than a borrow because the instance owns both the host and the
    /// library, and a host holding `&NativeLibrary` from the same struct would
    /// be a self-reference. Shared ownership costs one refcount per load and
    /// buys a type that needs no `unsafe`.
    library: Rc<NativeLibrary>,
}

impl<H: HostCapabilities> HostCapabilities for SeamHost<H> {
    fn write_line(&mut self, text: &str) {
        self.inner.write_line(text);
    }

    fn call_native(
        &mut self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeReturn, NativeCallError> {
        let trampoline = self
            .library
            .trampoline(function_id)
            .ok_or(NativeCallError::UnboundFunction(function_id))?;

        // The callee frees every string among these; this side must not. The
        // same contract the application-side session marshals under, read out
        // of the same module, because it is the same generated code being
        // called.
        // Building an aggregate's node tree allocates in the native half and
        // can fail, so a bad argument is reported here rather than reaching a
        // trampoline with a half-built list.
        let mut lowered = kira_hybrid_runtime::marshal::lower_args(&self.library, args)
            .map_err(|_| NativeCallError::MalformedResult(function_id))?;
        // SAFETY: the trampoline came from this library, and the VM calls with
        // the module's own arity — which bundle validation proved equals the
        // manifest's, which is the signature the trampoline was emitted for.
        let out = unsafe { self.library.call(trampoline, &mut lowered) };
        // SAFETY: `out` is what the trampoline just wrote, and its string handle
        // (if any) is unfreed.
        let result = unsafe { kira_hybrid_runtime::marshal::lift_result(&self.library, out) }
            .map_err(|_| NativeCallError::MalformedResult(function_id))?;
        // SAFETY: `lowered` is the array that call wrote through, and no
        // written-through slot has been lifted yet.
        let writebacks = unsafe {
            kira_hybrid_runtime::marshal::lift_writebacks(&self.library, function_id, &lowered)
        }
        .map_err(|_| NativeCallError::MalformedResult(function_id))?;
        Ok(NativeReturn { result, writebacks })
    }
}

/// A hybrid library loaded and running: its heap, its host, and both engines.
///
/// The same surface [`kira_main::Instance`] offers, which is the point — a
/// generated wrapper's methods are written once and compile against either. What
/// differs is underneath: a call may run bytecode, and that bytecode may cross
/// into machine code and back, and the consumer sees none of it.
///
/// Single-threaded by construction, matching the `!Send` wrapper types a
/// generated crate hands the consumer.
pub struct HybridInstance<H: HostCapabilities = StdoutHost> {
    inner: VmLibraryInstance<SeamHost<H>>,
}

/// Reports what a consumer can observe, and nothing about either half's
/// internals — for the reason [`kira_main::Instance`]'s own `Debug` is written
/// by hand.
impl<H: HostCapabilities> core::fmt::Debug for HybridInstance<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HybridInstance")
            .field("live_handles", &self.live_handles())
            .field("native_half", &self.inner.host().library.path())
            .finish_non_exhaustive()
    }
}

impl<H: HostCapabilities> HybridInstance<H> {
    /// Builds an instance over a loaded bytecode half and a loaded native half.
    pub(crate) fn new(
        library: &kira_main::Library,
        host: H,
        native: Rc<NativeLibrary>,
    ) -> Result<HybridInstance<H>, HybridMainError> {
        let inner = library.instantiate_with(SeamHost {
            inner: host,
            library: native,
        })?;
        Ok(HybridInstance { inner })
    }

    /// The host this instance runs against.
    pub fn host(&self) -> &H {
        &self.inner.host().inner
    }

    /// The host, mutably, for an embedder that drains it between calls.
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.inner.host_mut().inner
    }

    /// How many handles the consumer still holds.
    pub fn live_handles(&self) -> usize {
        self.inner.live_handles()
    }

    /// Calls one export by its consumer-facing name.
    ///
    /// The ownership contract is the bytecode half's, unchanged: **arguments
    /// borrow**, **the result owns**. A call that crosses into the native half
    /// and back is invisible here — that crossing has its own contract, and it
    /// is settled entirely between the VM and the seam.
    pub fn call(
        &mut self,
        name: &str,
        args: &[NativeArg<'_>],
    ) -> Result<NativeResult, HybridMainError> {
        Ok(self.inner.call(name, args)?)
    }

    /// Releases a handle, freeing the object it named.
    pub fn release(&mut self, handle: Handle) -> Result<(), HybridMainError> {
        Ok(self.inner.release(handle)?)
    }

    /// Releases every handle still live.
    pub fn release_all(&mut self) {
        self.inner.release_all();
    }

    /// Releases everything and hands the consumer's host back.
    pub fn into_host(self) -> H {
        self.inner.into_host().inner
    }
}
