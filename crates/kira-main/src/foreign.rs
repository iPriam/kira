//! A native-capable host that answers `call_foreign` through a generated sidecar.
//!
//! The VM is a portable core: it marshals a foreign call to borrowed
//! [`ForeignArg`]s and asks its host, never loading or linking anything itself.
//! This is the host that says yes. It wraps another [`HostCapabilities`] for
//! `write_line` and adds a foreign half backed by one
//! [`ForeignAdapterLibrary`] — the adapter sidecar the CLI build produced for
//! this exact program.
//!
//! Native-only by construction: it `dlopen`s the sidecar, so it is compiled out
//! for `wasm32` targets. That is the same division of labour the seam is built
//! around — the VM stays portable precisely because the host is not.

use std::path::Path;

use kira_dynamic_ffi::{ForeignAdapterError, ForeignAdapterLibrary};
use kira_runtime_abi::{
    ForeignArg, ForeignCallError, ForeignResult, ForeignSignature, HostCapabilities, NativeArg,
    NativeCallError, NativeResult, NativeStateError, NativeStateToken, NativeStateTypeId,
    NativeStateValue,
};

/// One foreign import's binding: the adapter symbol to call and the exact-width
/// signature to marshal against.
///
/// The CLI builds one of these per import, in import-id order, from the
/// program's foreign table and `kira_llvm_backend::adapter_name`. The VM's
/// `CallForeign(id)` indexes this list.
#[derive(Debug, Clone)]
pub struct ForeignBinding {
    /// The exported adapter symbol in the sidecar (`kira_foreign_adapter_<id>`).
    pub adapter_symbol: String,
    /// The import's exact-width parameter and result types.
    pub signature: ForeignSignature,
}

impl ForeignBinding {
    /// Pairs an adapter symbol with the signature it was generated for.
    pub fn new(adapter_symbol: impl Into<String>, signature: ForeignSignature) -> ForeignBinding {
        ForeignBinding {
            adapter_symbol: adapter_symbol.into(),
            signature,
        }
    }
}

/// A host that forwards `write_line` to an inner host and answers `call_foreign`
/// through a loaded adapter sidecar.
///
/// Generic over the inner host so an embedder that captures output — or streams
/// it to stdout with [`StdoutHost`](crate::StdoutHost) — keeps its own behaviour
/// and gains a foreign half.
pub struct ForeignHost<H: HostCapabilities> {
    inner: H,
    library: ForeignAdapterLibrary,
    imports: Vec<ForeignBinding>,
    detail: Option<String>,
}

impl<H: HostCapabilities> ForeignHost<H> {
    /// Loads the adapter sidecar at `sidecar` and binds it to `imports`.
    ///
    /// The load verifies the sidecar's foreign-adapter ABI marker and resolves
    /// its string helpers; a stale or incompatible sidecar is rejected here by
    /// name rather than at the first foreign call.
    pub fn load(
        sidecar: &Path,
        imports: Vec<ForeignBinding>,
        inner: H,
    ) -> Result<ForeignHost<H>, ForeignAdapterError> {
        let library = ForeignAdapterLibrary::load(sidecar)?;
        Ok(ForeignHost {
            inner,
            library,
            imports,
            detail: None,
        })
    }

    /// Hands the inner host back, dropping the loaded sidecar.
    pub fn into_inner(self) -> H {
        self.inner
    }

    /// The inner host, for an embedder that reads it between runs.
    pub fn inner(&self) -> &H {
        &self.inner
    }

    /// Takes the detail of the last adapter-load-consistency failure, if any.
    ///
    /// A missing or malformed adapter symbol surfaces to the VM as a typed
    /// foreign-call error, but its full explanation cannot ride that enum; the
    /// CLI reads it here to name the sidecar problem precisely.
    pub fn take_detail(&mut self) -> Option<String> {
        self.detail.take()
    }
}

impl<H: HostCapabilities> HostCapabilities for ForeignHost<H> {
    fn write_line(&mut self, text: &str) {
        self.inner.write_line(text);
    }

    fn call_native(
        &mut self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeResult, NativeCallError> {
        // A VM-plus-sidecar host has no `@Native` half; only the foreign seam.
        self.inner.call_native(function_id, args)
    }

    fn native_state_create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        self.inner.native_state_create(ty, value)
    }

    fn native_state_recover(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        self.inner.native_state_recover(token, ty)
    }

    fn native_state_replace(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        self.inner.native_state_replace(token, ty, value)
    }

    fn native_state_free(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        self.inner.native_state_free(token)
    }

    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        let Some(binding) = self.imports.get(foreign_id as usize) else {
            // The VM validated the id against the module's import table already,
            // so this is a build inconsistency, not a program fault.
            self.detail = Some(format!(
                "no adapter binding for foreign import {foreign_id} (the sidecar and the \
                 program disagree)"
            ));
            return Err(ForeignCallError::NoForeignHost);
        };
        match self
            .library
            .call(&binding.adapter_symbol, &binding.signature, args)
        {
            Ok(result) => Ok(result),
            // A contract or adapter-status error is the program's to see, typed.
            Err(ForeignAdapterError::Call(error)) => Err(error),
            // A missing/malformed adapter symbol is a stale-sidecar fault: record
            // the detail and surface a typed refusal rather than panicking.
            Err(other) => {
                self.detail = Some(other.to_string());
                Err(ForeignCallError::NoForeignHost)
            }
        }
    }
}
