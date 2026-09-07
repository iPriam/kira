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
    HostCapabilities, LinuxSyscall, MainThreadError, MainThreadHandle, MainThreadRequest,
    MainThreadResponse, NativeArg, NativeCallError, NativeReturn, NativeStateError,
    NativeStateToken, NativeStateTypeId, NativeStateValue, SyscallError, syscall,
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
    /// The Linux kernel, entered by instruction rather than by symbol.
    ///
    /// A kind of its own because a `@FFI.Syscall` has no library to open and no
    /// name to look up — the number is the compiler's and the entry is one
    /// instruction. Recording it as [`Self::Unavailable`] instead, which is
    /// what a library lookup for the empty string produced, made a call the
    /// host can serve perfectly well report that "the declaring library
    /// resolved to no artifact", sending a reader to look for a
    /// `nativeLibraries` row that cannot exist.
    Syscall { call: LinuxSyscall },
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
    /// True when the import names DATA and the answer is the symbol's address.
    ///
    /// The symbol is still bound and still looked up; nothing is invoked. A
    /// binding that lost this would call an object's first bytes.
    pub answers_address: bool,
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
            answers_address: false,
        }
    }

    /// Creates a binding to a symbol exported by the host process image.
    pub fn process(symbol: impl Into<String>, signature: ForeignSignature) -> ForeignBinding {
        ForeignBinding {
            target: ForeignBindingTarget::Process {
                symbol: symbol.into(),
            },
            signature,
            answers_address: false,
        }
    }

    /// Creates a binding for a library excluded on this target.
    pub fn unavailable(signature: ForeignSignature) -> ForeignBinding {
        ForeignBinding {
            target: ForeignBindingTarget::Unavailable,
            signature,
            answers_address: false,
        }
    }

    /// Creates a binding that enters the kernel instead of a library.
    pub fn syscall(call: LinuxSyscall, signature: ForeignSignature) -> ForeignBinding {
        ForeignBinding {
            target: ForeignBindingTarget::Syscall { call },
            signature,
            answers_address: false,
        }
    }

    /// Marks this binding as naming a data symbol whose address is the answer.
    #[must_use]
    pub fn answering_address(mut self) -> ForeignBinding {
        self.answers_address = true;
        self
    }

    /// Returns the system call this binding enters, or `None` when it binds a
    /// symbol.
    pub fn syscall_target(&self) -> Option<LinuxSyscall> {
        match &self.target {
            ForeignBindingTarget::Syscall { call } => Some(*call),
            ForeignBindingTarget::Unavailable
            | ForeignBindingTarget::Process { .. }
            | ForeignBindingTarget::Library { .. } => None,
        }
    }

    /// Returns the path of a separately loaded library target.
    pub fn library_path(&self) -> Option<&Path> {
        match &self.target {
            ForeignBindingTarget::Library { path, .. } => Some(path),
            ForeignBindingTarget::Unavailable
            | ForeignBindingTarget::Process { .. }
            | ForeignBindingTarget::Syscall { .. } => None,
        }
    }

    /// Returns the real symbol of a direct process or library target.
    pub fn direct_symbol(&self) -> Option<&str> {
        match &self.target {
            ForeignBindingTarget::Process { symbol }
            | ForeignBindingTarget::Library { symbol, .. } => Some(symbol),
            ForeignBindingTarget::Unavailable | ForeignBindingTarget::Syscall { .. } => None,
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
        Self::load_dynamic_inner(imports, aggregates, inner)
    }

    fn load_dynamic_inner(
        imports: Vec<ForeignBinding>,
        aggregates: ForeignAggregates,
        inner: H,
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
                    let library = ForeignLibrary::load(path, aggregates.clone())?;
                    libraries.push(library);
                }
                ForeignBindingTarget::Process { .. } => {
                    if libraries.iter().any(ForeignLibrary::is_process) {
                        continue;
                    }
                    let library = ForeignLibrary::load_process(aggregates.clone())?;
                    libraries.push(library);
                }
                // Neither of these opens anything: one has no artifact to open
                // and the other is an instruction, not a library.
                ForeignBindingTarget::Unavailable | ForeignBindingTarget::Syscall { .. } => {}
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

    fn main_thread(
        &mut self,
        request: MainThreadRequest,
    ) -> Result<MainThreadResponse, MainThreadError> {
        self.inner.main_thread(request)
    }

    fn main_thread_join(
        &mut self,
        handle: MainThreadHandle,
    ) -> Result<NativeStateValue, MainThreadError> {
        self.inner.main_thread_join(handle)
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

    fn native_state_retain(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        self.inner.native_state_retain(token)
    }

    fn native_state_release(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        self.inner.native_state_release(token)
    }

    /// Enters this process's kernel, which is the same one the emitted call
    /// would have entered.
    ///
    /// The grant `write_line` already makes, in the direction the kernel reads:
    /// a host standing in this process is a host whose descriptors the program
    /// is writing to. Which calls it may serve is not this host's decision —
    /// [`syscall::call`] applies the policy that belongs to the call itself.
    fn syscall(&mut self, call: LinuxSyscall, args: &[i64]) -> Result<i64, SyscallError> {
        // SAFETY: the words came from a `@FFI.Syscall` call site the frontend
        // validated to register-width scalars, and a pointer among them is one
        // this program produced — the obligation the `@FFI.Extern` seam beside
        // it already carries for every pointer it hands a C library.
        unsafe { syscall::perform(call, args) }
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
        // A data symbol is bound and then nothing is invoked. Resolving it is
        // the same lookup a call would do; what is skipped is the call.
        if binding.answers_address {
            let symbol = match &binding.target {
                ForeignBindingTarget::Library { symbol, .. }
                | ForeignBindingTarget::Process { symbol } => symbol.clone(),
                ForeignBindingTarget::Unavailable | ForeignBindingTarget::Syscall { .. } => {
                    self.detail =
                        Some("an address import resolves no symbol on this target".to_owned());
                    return Err(ForeignCallError::NoForeignHost);
                }
            };
            let wanted_process = matches!(binding.target, ForeignBindingTarget::Process { .. });
            let Some(library) = self
                .libraries
                .iter()
                .find(|library| library.is_process() == wanted_process)
            else {
                self.detail = Some(format!("no loaded image to resolve `{symbol}` in"));
                return Err(ForeignCallError::NoForeignHost);
            };
            return match library.symbol_address(&symbol) {
                Ok(address) => Ok(ForeignResult::RawPtr(address as u64)),
                Err(error) => {
                    self.detail = Some(format!("`{symbol}` did not resolve: {error}"));
                    Err(ForeignCallError::NoForeignHost)
                }
            };
        }
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
            // Nothing is opened, looked up, or marshalled into C storage: the
            // arguments are register words and the entry is one instruction.
            ForeignBindingTarget::Syscall { call } => {
                let call = *call;
                let signature = binding.signature.clone();
                return syscall::call(self, call, &signature, args);
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
