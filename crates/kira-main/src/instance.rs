//! A running copy of a library: its heap, its host, and calls by name.
//!
//! [`kira_vm_runtime::Instance`] is the engine — one heap that outlives a call,
//! a root table naming what the consumer still holds. This is the *embedding*
//! surface over it, and it adds the three things a Rust program needs that the
//! portable core deliberately does not provide:
//!
//! - **It owns the host.** The core takes a `&mut dyn HostCapabilities` per
//!   call, which is right for a crate that must not assume where output goes.
//!   An embedder threading that through every generated method would be
//!   ceremony, so the instance holds it. Generic, not boxed, so an embedder that
//!   captures output can read its own host back afterwards.
//! - **It calls by name.** The core calls by function id, which is the artifact's
//!   internal numbering. A consumer knows `make_button`; resolving that through
//!   the module's own export table is what makes the id an internal detail a
//!   library may renumber.
//! - **It checks arguments before the VM sees them.** Passing a string where an
//!   integer belongs is a mistake entirely outside the library, and a trap from
//!   inside says nothing about it.
//!
//! # What is checked, and what is not
//!
//! Argument checking here is **kind-level**: integer, float, boolean, string,
//! handle. It deliberately does not check *which class* a handle denotes,
//! because a [`NativeArg::Handle`] is one word and the seam cannot see more.
//! Class typing belongs to the generated newtypes one layer up, where a
//! `Button` and a `Window` are different Rust types and the mistake cannot be
//! written. Stating the split here keeps this layer from pretending to a
//! guarantee it cannot make.

use kira_runtime_abi::{BridgeValueTag, HostCapabilities, NativeArg, NativeResult};
use kira_vm_runtime::{HeapStats, Instance as VmInstance, RootId};

use crate::error::{Error, describe_kind};
use crate::host::StdoutHost;

/// The name a consumer holds for one object living inside an [`Instance`].
///
/// A newtype over the word the seam carries rather than a bare `u64`, so a
/// handle cannot be confused with an integer argument at a call site. It is
/// still an opaque ticket: not an address, not an index, and nothing to compute
/// with. The generated wrapper wraps this again, once per exported class, to add
/// the class typing this layer cannot see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Handle(u64);

impl Handle {
    /// Reads a handle back from the word a call produced.
    pub fn from_word(word: u64) -> Handle {
        Handle(word)
    }

    /// The word this handle crosses as.
    pub fn as_word(self) -> u64 {
        self.0
    }
}

/// A library loaded and running, with its own heap and its own host.
///
/// Single-threaded by construction — every call takes `&mut self` — matching the
/// `!Send` wrapper types a generated crate hands the consumer.
pub struct Instance<H: HostCapabilities = StdoutHost> {
    inner: VmInstance,
    host: H,
}

/// Reports what a consumer can observe, not the heap behind it.
///
/// Written by hand rather than derived because the engine instance holds a heap
/// and a root table with no `Debug` of their own — and printing either would be
/// printing a library's private state into a consumer's log.
impl<H: HostCapabilities> core::fmt::Debug for Instance<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Instance")
            .field(
                "exports",
                &self.inner.program().module().exports.functions.len(),
            )
            .field("live_handles", &self.live_handles())
            .finish_non_exhaustive()
    }
}

impl<H: HostCapabilities> Instance<H> {
    /// Wraps a loaded VM instance and the host it will run against.
    pub(crate) fn new(inner: VmInstance, host: H) -> Instance<H> {
        Instance { inner, host }
    }

    /// The host this instance runs against.
    pub fn host(&self) -> &H {
        &self.host
    }

    /// The host, mutably, for an embedder that drains it between calls.
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    /// How many handles the consumer still holds.
    pub fn live_handles(&self) -> usize {
        self.inner.live_roots()
    }

    /// Calls one export by its consumer-facing name.
    ///
    /// Ownership follows the boundary contract the core states: **arguments
    /// borrow** (a `&str` is copied in; a handle's object is copied in and the
    /// handle keeps naming the original), and **the result owns** (a returned
    /// string is an owned `String`; a returned object is a [`Handle`] the caller
    /// must [`release`](Instance::release)).
    pub fn call(&mut self, name: &str, args: &[NativeArg<'_>]) -> Result<NativeResult, Error> {
        let export = self
            .inner
            .program()
            .module()
            .exports
            .functions
            .iter()
            .find(|export| export.name == name)
            .ok_or_else(|| Error::UnknownExport {
                name: name.to_owned(),
            })?;
        let function = export.function;

        if args.len() != export.params.len() {
            return Err(Error::ArgumentCount {
                export: name.to_owned(),
                expected: export.params.len(),
                found: args.len(),
            });
        }
        for (position, (declared, passed)) in export.params.iter().zip(args).enumerate() {
            if declared.tag() != arg_tag(passed) {
                return Err(Error::ArgumentType {
                    export: name.to_owned(),
                    position,
                    expected: describe_kind(*declared),
                    found: describe_arg(passed),
                });
            }
        }

        Ok(self.inner.call(&mut self.host, function, args)?)
    }

    /// Releases a handle, freeing the object it named.
    ///
    /// A handle that is not live is [`VmError::DanglingRoot`], which covers both
    /// releasing twice and presenting another instance's handle. It is a typed
    /// error and never a hit on whatever object came later — root ids are never
    /// reused, which is the property that makes that guarantee real.
    ///
    /// [`VmError::DanglingRoot`]: kira_vm_runtime::VmError::DanglingRoot
    pub fn release(&mut self, handle: Handle) -> Result<(), Error> {
        Ok(self.inner.release(RootId::from_word(handle.as_word()))?)
    }

    /// Releases every handle still live.
    pub fn release_all(&mut self) {
        self.inner.release_all();
    }

    /// Releases everything and reports the heap's final accounting.
    ///
    /// `current` is 0 for an instance whose every allocation was reclaimed.
    /// Consuming `self` is what makes the number mean something: nothing can
    /// allocate after it.
    pub fn finish(self) -> HeapStats {
        self.inner.finish()
    }

    /// Releases everything and hands the host back.
    ///
    /// The counterpart to [`finish`](Instance::finish) for an embedder whose
    /// host accumulated something worth reading — a capture buffer, a log.
    pub fn into_host(self) -> H {
        let Instance { inner, host } = self;
        // Balance the heap on the way out for the same reason `finish` does:
        // an instance dropped with live roots has simply leaked them, and there
        // is no later moment at which that becomes visible.
        let _ = inner.finish();
        host
    }
}

/// The bridge tag an argument crosses as.
///
/// A raw pointer carries [`BridgeValueTag::RAW_PTR`], which no export parameter
/// type ever declares — a raw pointer is a foreign-seam value, not an export one
/// — so passing one to an export call mismatches every declared parameter and is
/// reported as an argument-type error rather than misread as some other kind.
fn arg_tag(arg: &NativeArg<'_>) -> BridgeValueTag {
    match arg {
        NativeArg::Void => BridgeValueTag::VOID,
        NativeArg::Int(_) => BridgeValueTag::INT,
        NativeArg::Float(_) => BridgeValueTag::FLOAT,
        NativeArg::Bool(_) => BridgeValueTag::BOOL,
        NativeArg::Str(_) => BridgeValueTag::STRING,
        NativeArg::Handle(_) => BridgeValueTag::HANDLE,
        NativeArg::RawPtr(_) => BridgeValueTag::RAW_PTR,
    }
}

/// A one-word name for what an argument carries, for a message.
fn describe_arg(arg: &NativeArg<'_>) -> &'static str {
    match arg {
        NativeArg::Void => "nothing",
        NativeArg::Int(_) => "an integer",
        NativeArg::Float(_) => "a float",
        NativeArg::Bool(_) => "a boolean",
        NativeArg::Str(_) => "a string",
        NativeArg::Handle(_) => "a handle",
        NativeArg::RawPtr(_) => "a raw pointer",
    }
}

#[cfg(test)]
#[path = "instance_tests.rs"]
mod tests;
