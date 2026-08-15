//! A native-capable host that answers `call_foreign` through libffi.
//!
//! The VM is a portable core: it marshals a foreign call to borrowed
//! [`ForeignArg`]s and asks its host, never loading or linking anything itself.
//! This is the host that says yes. It wraps another [`HostCapabilities`] for
//! `write_line` and adds a foreign half backed by the [`ForeignLibrary`]
//! handles opened from the package's resolved native libraries.
//!
//! Native-only by construction: it `dlopen`s the declared libraries, so it is
//! compiled out for `wasm32` targets. That is the same division of labour the
//! seam is built around — the VM stays portable precisely because the host is
//! not.

use std::path::{Path, PathBuf};

use kira_dynamic_ffi::{ForeignLibrary, ForeignLibraryError};
use kira_runtime_abi::{
    ForeignAggregates, ForeignArg, ForeignCallError, ForeignResult, ForeignSignature,
    HostCapabilities, NativeArg, NativeCallError, NativeReturn, NativeStateError, NativeStateToken,
    NativeStateTypeId, NativeStateValue,
};

/// The native target of one direct foreign import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignBindingTarget {
    /// No native library was selected for this target.
    Unavailable,
    /// A symbol supplied by the host process image.
    Process { symbol: String },
    /// A symbol supplied by a separately loaded native library.
    Library { path: PathBuf, symbol: String },
}

/// One foreign import's binding and the exact-width signature to marshal
/// against.
///
/// The CLI builds one of these per import, in import-id order, from the
/// program's foreign table and the resolved native libraries. The VM's
/// `CallForeign(id)` indexes this list.
#[derive(Debug, Clone)]
pub struct ForeignBinding {
    /// Where the direct call resolves.
    pub target: ForeignBindingTarget,
    /// The import's exact-width parameter and result types.
    pub signature: ForeignSignature,
}

impl ForeignBinding {
    /// Creates a binding to a real native library symbol.
    pub fn dynamic(
        library_path: impl Into<PathBuf>,
        symbol: impl Into<String>,
        signature: ForeignSignature,
    ) -> ForeignBinding {
        ForeignBinding {
            target: ForeignBindingTarget::Library {
                path: library_path.into(),
                symbol: symbol.into(),
            },
            signature,
        }
    }

    /// Creates a binding to a symbol exported by the host process image.
    pub fn process(symbol: impl Into<String>, signature: ForeignSignature) -> ForeignBinding {
        ForeignBinding {
            target: ForeignBindingTarget::Process {
                symbol: symbol.into(),
            },
            signature,
        }
    }

    /// Creates a binding for a library excluded on this target.
    pub fn unavailable(signature: ForeignSignature) -> ForeignBinding {
        ForeignBinding {
            target: ForeignBindingTarget::Unavailable,
            signature,
        }
    }

    /// Returns the path of a separately loaded library target.
    pub fn library_path(&self) -> Option<&Path> {
        match &self.target {
            ForeignBindingTarget::Library { path, .. } => Some(path),
            ForeignBindingTarget::Unavailable | ForeignBindingTarget::Process { .. } => None,
        }
    }

    /// Returns the real symbol of a direct process or library target.
    pub fn direct_symbol(&self) -> Option<&str> {
        match &self.target {
            ForeignBindingTarget::Process { symbol }
            | ForeignBindingTarget::Library { symbol, .. } => Some(symbol),
            ForeignBindingTarget::Unavailable => None,
        }
    }
}

/// A host that forwards `write_line` to an inner host and answers `call_foreign`
/// through the native libraries its bindings name.
///
/// Generic over the inner host so an embedder that captures output — or streams
/// it to stdout with [`StdoutHost`](crate::StdoutHost) — keeps its own behaviour
/// and gains a foreign half.
pub struct ForeignHost<H: HostCapabilities> {
    inner: H,
    libraries: Vec<ForeignLibrary>,
    imports: Vec<ForeignBinding>,
    detail: Option<String>,
}

impl<H: HostCapabilities> ForeignHost<H> {
    /// Opens every real native library named by `imports` and routes calls
    /// through Kira's bundled libffi engine.
    ///
    /// `aggregates` is the program's C-layout aggregate table, which the
    /// bindings' signatures index; it sizes the buffer an aggregate result is
    /// written into.
    pub fn load_dynamic(
        imports: Vec<ForeignBinding>,
        aggregates: ForeignAggregates,
        inner: H,
    ) -> Result<ForeignHost<H>, ForeignLibraryError> {
        Self::load_dynamic_inner(imports, aggregates, inner, None)
    }

    /// Opens direct foreign bindings with libffi staged beside a live bundle.
    pub fn load_dynamic_with_runtime_path(
        imports: Vec<ForeignBinding>,
        aggregates: ForeignAggregates,
        runtime_path: impl AsRef<Path>,
        inner: H,
    ) -> Result<ForeignHost<H>, ForeignLibraryError> {
        Self::load_dynamic_inner(
            imports,
            aggregates,
            inner,
            Some(runtime_path.as_ref().to_path_buf()),
        )
    }

    fn load_dynamic_inner(
        imports: Vec<ForeignBinding>,
        aggregates: ForeignAggregates,
        inner: H,
        runtime_path: Option<PathBuf>,
    ) -> Result<ForeignHost<H>, ForeignLibraryError> {
        let mut libraries = Vec::new();
        for binding in &imports {
            match &binding.target {
                ForeignBindingTarget::Library { path, .. } => {
                    if libraries.iter().any(|library: &ForeignLibrary| {
                        !library.is_process() && library.path() == path
                    }) {
                        continue;
                    }
                    let library = match runtime_path.as_deref() {
                        Some(runtime_path) => ForeignLibrary::load_with_runtime_path(
                            path,
                            aggregates.clone(),
                            runtime_path,
                        ),
                        None => ForeignLibrary::load(path, aggregates.clone()),
                    }?;
                    libraries.push(library);
                }
                ForeignBindingTarget::Process { .. } => {
                    if libraries.iter().any(ForeignLibrary::is_process) {
                        continue;
                    }
                    let library = match runtime_path.as_deref() {
                        Some(runtime_path) => ForeignLibrary::load_process_with_runtime_path(
                            aggregates.clone(),
                            runtime_path,
                        ),
                        None => ForeignLibrary::load_process(aggregates.clone()),
                    }?;
                    libraries.push(library);
                }
                ForeignBindingTarget::Unavailable => {}
            }
        }
        Ok(ForeignHost {
            inner,
            libraries,
            imports,
            detail: None,
        })
    }

    /// Hands the inner host back, dropping the loaded libraries.
    pub fn into_inner(self) -> H {
        self.inner
    }

    /// The inner host, for an embedder that reads it between runs.
    pub fn inner(&self) -> &H {
        &self.inner
    }

    /// Takes the detail of the last binding-consistency failure, if any.
    ///
    /// A missing library or symbol surfaces to the VM as a typed foreign-call
    /// error, but its full explanation cannot ride that enum; the CLI reads it
    /// here to name the binding problem precisely.
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
    ) -> Result<NativeReturn, NativeCallError> {
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
                "no foreign binding for import {foreign_id} (the native library table and the \
                 program disagree)"
            ));
            return Err(ForeignCallError::NoForeignHost);
        };
        let result = match &binding.target {
            ForeignBindingTarget::Library { path, symbol } => {
                let Some(library) = self
                    .libraries
                    .iter()
                    .find(|library| !library.is_process() && library.path() == path)
                else {
                    self.detail = Some(format!(
                        "native library `{}` was not loaded",
                        path.display()
                    ));
                    return Err(ForeignCallError::NoForeignHost);
                };
                // SAFETY: the binding came from the exact foreign declaration
                // and the library owns the exported function address.
                unsafe { library.call(symbol, &binding.signature, args) }.map_err(|error| {
                    match error {
                        ForeignLibraryError::Call(call) => call,
                        _other => ForeignCallError::NoForeignHost,
                    }
                })
            }
            ForeignBindingTarget::Process { symbol } => {
                let Some(library) = self.libraries.iter().find(|library| library.is_process())
                else {
                    self.detail = Some("the host process image was not opened".to_owned());
                    return Err(ForeignCallError::NoForeignHost);
                };
                // SAFETY: the process binding came from the exact foreign
                // declaration and uses its declared LibFFI signature.
                unsafe { library.call(symbol, &binding.signature, args) }.map_err(|error| {
                    match error {
                        ForeignLibraryError::Call(call) => call,
                        _other => ForeignCallError::NoForeignHost,
                    }
                })
            }
            ForeignBindingTarget::Unavailable => {
                self.detail = Some("the foreign binding is unavailable on this target".to_owned());
                return Err(ForeignCallError::NoForeignHost);
            }
        };
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                self.detail = Some(error.to_string());
                Err(error)
            }
        }
    }
}
