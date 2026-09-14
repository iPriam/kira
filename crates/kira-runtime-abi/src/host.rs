use super::*;

/// The effects an embedder grants a running Kira program.
///
/// The VM owns the runtime value representation and all formatting; the host
/// only receives already-rendered lines. This keeps the VM compilable for
/// `wasm32-unknown-unknown`, where the concrete host is supplied by the
/// browser embedder rather than by the standard library.
///
/// The same rule is what makes hybrid possible without breaking the portable
/// core: the VM never dlopens anything or touches a C ABI. When it reaches a
/// call into the native half it asks the embedder, in safe Rust, through
/// [`HostCapabilities::call_native`] — and the embedder, which is native-only
/// by construction, does the marshalling.
pub trait HostCapabilities {
    /// Emits one line of program output (the effect behind the `print` builtin).
    ///
    /// The text is already fully formatted and carries no trailing newline;
    /// the host owns line termination for its destination.
    fn write_line(&mut self, text: &str);

    /// Runs the native function `function_id`, returning what it produced.
    ///
    /// The default refuses: most hosts (the VM-only CLI, the wasm embedder,
    /// tests) have no native half, and a program that reaches this on such a
    /// host is a build error surfacing late, not something to paper over.
    fn call_native(
        &mut self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeReturn, NativeCallError> {
        let _ = (function_id, args);
        Err(NativeCallError::NoNativeHalf)
    }

    /// Carries out one channel primitive on a table the host owns.
    ///
    /// `None` — the default — means the host has no channel table and the
    /// engine should use its own, which is every host but a hybrid session's.
    ///
    /// A hybrid session has one, because the two halves of a hybrid program
    /// have to share a channel table. A channel end is an ordinary value: a
    /// `@Runtime` function can create one and hand it to a `@Native` function,
    /// or the other way round, and a handle is an index — so two tables mean
    /// the receiving half looks the end up in a table that never had it and
    /// traps on a program that is correct.
    fn channel_op(
        &mut self,
        prim: ChannelPrim,
        a: i64,
        b: i64,
        c: i64,
    ) -> Option<Result<i64, ChannelTrap>> {
        let _ = (prim, a, b, c);
        None
    }

    /// Services one request on the host's main-thread event loop.
    ///
    /// The default refuses because a portable VM host may not have a process
    /// main thread at all. An application runner installs the capability and
    /// owns the loop; the VM only supplies the copied request.
    fn main_thread(
        &mut self,
        request: MainThreadRequest,
    ) -> Result<MainThreadResponse, MainThreadError> {
        let _ = request;
        Err(MainThreadError::NoHost)
    }

    /// Joins a task previously returned by [`Self::main_thread`] with
    /// [`MainThreadOp::Spawn`].
    fn main_thread_join(
        &mut self,
        handle: MainThreadHandle,
    ) -> Result<NativeStateValue, MainThreadError> {
        let _ = handle;
        Err(MainThreadError::NoHost)
    }

    /// Runs the generated adapter for `foreign_id`.
    ///
    /// The default refuses so the portable VM never acquires a dynamic-loading
    /// dependency and embedders opt into foreign access explicitly.
    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        let _ = (foreign_id, args);
        Err(ForeignCallError::NoForeignHost)
    }

    /// The address C calls to enter the Kira function callback `callback_id`
    /// names.
    ///
    /// The VM has no native code of its own, so the pointer a `@FFI.Callback`
    /// value carries is one the host produced — the entry thunk in the generated
    /// sidecar. The default refuses for the same reason [`Self::call_foreign`]
    /// does: a portable VM acquires no dynamic-loading dependency, and an
    /// embedder opts into foreign access explicitly.
    fn foreign_callback(&mut self, callback_id: u32) -> Result<u64, ForeignCallError> {
        let _ = callback_id;
        Err(ForeignCallError::NoForeignHost)
    }

    /// Makes one Linux system call on the running program's behalf, answering
    /// the word the kernel's result register holds.
    ///
    /// The answer is the kernel's own encoding — a value, or a small negative
    /// `-errno` — undecoded, because the Kira program decodes it. A host that
    /// decoded it here would make the interpreted call answer something the
    /// call the backend emitted does not, and the two engines printing the same
    /// bytes is the whole reason a host serves this at all.
    ///
    /// The default refuses, exactly as [`Self::call_native`] refuses for want of
    /// a native half: most hosts stand nowhere near a Linux kernel — a browser
    /// tab, a test, a `wasm32` embedder — and a program that reaches this on one
    /// of them must hear so rather than receive a number it will read as a
    /// count. A host that does stand in a Linux process grants the capability by
    /// answering with [`syscall::perform`], which is where the registers are.
    ///
    /// Which calls a host may serve at all is not the host's question:
    /// [`LinuxSyscall::servable_by_an_interpreter`] answers it, because it turns
    /// on what the call does rather than on who is asked.
    fn syscall(&mut self, call: LinuxSyscall, args: &[i64]) -> Result<i64, SyscallError> {
        let _ = (call, args);
        Err(SyscallError::NoKernelHost)
    }

    /// Boxes a backend-neutral Kira value in stable callback-state storage.
    fn native_state_create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        let _ = (ty, value);
        Err(NativeStateError::NoStateHost)
    }

    /// Boxes a value that runs a user `Drop` body, recording the body's
    /// function index so the destroying release can hand it back to be run.
    ///
    /// The default drops the body and boxes as usual: a host that owns
    /// value-tree storage overrides this and [`Self::native_state_release_dropping`]
    /// together, which is what lets a boxed `Drop` value's body run where the
    /// value dies rather than being abandoned.
    fn native_state_create_dropping(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
        glue: Option<u32>,
    ) -> Result<NativeStateToken, NativeStateError> {
        let _ = glue;
        self.native_state_create(ty, value)
    }

    /// Recovers an owned copy of callback state after validating its type.
    fn native_state_recover(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        let _ = (token, ty);
        Err(NativeStateError::NoStateHost)
    }

    /// Replaces callback state after validating its token and type.
    fn native_state_replace(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateValue, NativeStateError> {
        let _ = (token, ty, value);
        Err(NativeStateError::NoStateHost)
    }

    /// Checks that a token names live state of this type, reading nothing.
    ///
    /// `nativeRecover` needs the type check and nothing else — it hands back a
    /// handle, not a copy. The default answers by recovering, which deep-copies
    /// the whole state and discards it: a host that owns its storage should
    /// override this, or every recovery pays for the state's entire contents.
    fn native_state_check(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<(), NativeStateError> {
        self.native_state_recover(token, ty).map(|_| ())
    }

    /// Reads one value out of callback state, addressed by path.
    ///
    /// The default recovers the whole state and walks it, which is what
    /// [`Self::native_state_recover`] costs. A host that owns its storage
    /// should override this: reading one integer field is otherwise a deep copy
    /// of everything the state holds, and a UI batch holding a glyph cache pays
    /// that on every field access of every frame.
    fn native_state_read(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
    ) -> Result<NativeStateValue, NativeStateError> {
        let root = self.native_state_recover(token, ty)?;
        native_state_walk(&root, path).cloned()
    }

    /// Writes one value into callback state, addressed by path.
    ///
    /// The default recovers, walks, writes, and replaces — two deep copies of
    /// the whole state per field write. Overriding it is the difference between
    /// a field assignment costing the state's size and costing its depth.
    fn native_state_write(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<NativeStateValue, NativeStateError> {
        let mut root = self.native_state_recover(token, ty)?;
        let old = std::mem::replace(native_state_walk_mut(&mut root, path)?, value);
        let replaced_root = self.native_state_replace(token, ty, root)?;
        drop(replaced_root);
        Ok(old)
    }

    /// Appends one element to an array inside callback state, addressed by path.
    fn native_state_append(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        let mut root = self.native_state_recover(token, ty)?;
        match native_state_walk_mut(&mut root, path)? {
            // The elements are shared with whoever last read this array, so the
            // append buys a block of its own before it lands.
            NativeStateValue::Array(elements) => std::sync::Arc::make_mut(elements).push(value),
            _ => return Err(NativeStateError::PathMismatch),
        }
        let replaced_root = self.native_state_replace(token, ty, root)?;
        drop(replaced_root);
        Ok(())
    }

    /// Adds one owner to live callback state.
    fn native_state_retain(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        let _ = token;
        Err(NativeStateError::NoStateHost)
    }

    /// Removes one owner from live callback state, destroying it with the last.
    fn native_state_release(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        let _ = token;
        Err(NativeStateError::NoStateHost)
    }

    /// Removes one owner and returns the owned tree when final destruction must
    /// run under a Kira engine. The tree carries every nested user `Drop` body
    /// and NativeState ownership obligation. The default forwards to
    /// [`Self::native_state_release`] for hosts whose states need no engine.
    fn native_state_release_dropping(
        &mut self,
        token: NativeStateToken,
    ) -> Result<Option<NativeStateValue>, NativeStateError> {
        self.native_state_release(token).map(|()| None)
    }

    /// Performs one file-system operation on the embedder's behalf.
    ///
    /// The default refuses, for the same reason [`Self::call_foreign`] does: the
    /// VM core reaches nothing outside itself, so an embedder — a browser tab, a
    /// test, a sandbox — grants filesystem access explicitly by wrapping its
    /// host in [`FileSystemHost`] or implementing this itself.
    ///
    /// A *failed* operation is not an error here: a missing file answers
    /// [`FileResponse::Flag(false)`](FileResponse::Flag) or an empty result. The
    /// error is only for a host with no filesystem at all.
    fn file_system(&mut self, request: FileRequest<'_>) -> Result<FileResponse, FileSystemError> {
        let _ = request;
        Err(FileSystemError::NoFileSystemHost)
    }

    /// Checks a package set the program built in memory, answering with its
    /// diagnostics.
    ///
    /// The default answers through the compiler the embedder installed with
    /// [`compiler::install`], and refuses when it installed none. That is the
    /// same arrangement [`Self::file_system`] has with
    /// [`file_system::perform`] and it is what the VM's position in the
    /// layering forces: the VM sits *below* the compiler and can never hold
    /// one, so a build that contains a frontend has to hand it in. Every other
    /// host — a browser tab, a test, a sandbox — refuses by name instead of
    /// answering with an empty diagnostic list that would read as success.
    ///
    /// A package that does not compile is not an error here: its problems are
    /// the answer. The error is for a host with no compiler at all, and for a
    /// request that could not be read.
    fn compiler(&mut self, request: &CheckRequest) -> Result<Vec<CheckDiagnostic>, CompilerError> {
        compiler::perform(request)
    }

    /// Checks, builds, or runs a package that is already on a disk.
    ///
    /// A separate slot from [`Self::compiler`] rather than another operation of
    /// it, because a host can honestly have one and not the other: a browser
    /// tab embeds the frontend and can answer a question about source it was
    /// handed, and has no directory to build and no process to start. Each
    /// refuses on its own.
    ///
    /// A package that does not compile is not an error here either — its
    /// problems are the answer, and so is the exit code of a program that ran
    /// and failed. The error is for a host with no toolchain at all.
    fn toolchain(
        &mut self,
        verb: ToolVerb,
        request: &ToolRequest,
    ) -> Result<ToolAnswer, ToolchainError> {
        toolchain::perform(verb, request)
    }
}

/// A [`HostCapabilities`] implementation that records every line in memory.
///
/// Useful for tests and for embedders that want to capture output rather than
/// stream it. Ships in the portable core because it needs nothing but `alloc`.
#[derive(Debug, Default)]
pub struct CapturingHost {
    lines: Vec<String>,
}

impl CapturingHost {
    /// Creates a host with no captured output.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns every line captured so far, in emission order.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Renders all captured lines back into a single newline-terminated string.
    pub fn into_output(self) -> String {
        let mut out = String::new();
        for line in self.lines {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

impl HostCapabilities for CapturingHost {
    fn write_line(&mut self, text: &str) {
        self.lines.push(text.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capturing_host_records_lines_in_order() {
        let mut host = CapturingHost::new();
        host.write_line("first");
        host.write_line("second");
        assert_eq!(host.lines(), ["first".to_owned(), "second".to_owned()]);
        assert_eq!(host.into_output(), "first\nsecond\n");
    }

    /// A host that was never given a compiler says so, rather than answering
    /// "no diagnostics" — which a caller would read as "it compiled".
    #[test]
    fn host_capabilities_refuses_the_compiler_by_default() {
        let mut host = CapturingHost::new();
        assert_eq!(
            host.compiler(&CheckRequest::default()),
            Err(CompilerError::NoCompilerHost)
        );
    }

    #[test]
    fn host_capabilities_refuses_foreign_calls_by_default() {
        let mut host = CapturingHost::new();
        assert_eq!(
            host.call_foreign(0, &[ForeignArg::I32(7)]),
            Err(ForeignCallError::NoForeignHost)
        );
    }

    /// A host that was never given a kernel says so, for every call including
    /// the ones an interpreter is allowed to serve. Answering a number instead
    /// would be worse than refusing: a Kira program reads a non-negative answer
    /// as a byte count.
    #[test]
    fn host_capabilities_refuses_system_calls_by_default() {
        let mut host = CapturingHost::new();
        for call in LINUX_SYSCALLS {
            assert_eq!(
                host.syscall(call, &[1, 0, 0]),
                Err(SyscallError::NoKernelHost),
                "{}",
                call.label()
            );
        }
    }
}
