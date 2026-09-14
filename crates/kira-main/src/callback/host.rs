use super::*;

/// A [`HostCapabilities`] over a shared session.
///
/// Carries nothing itself, which is what lets a nested run build another one
/// while the outer one is still borrowed by the interpreter.
pub(super) struct SessionHost<'a> {
    pub(super) session: &'a ForeignSession,
}

impl HostCapabilities for SessionHost<'_> {
    fn write_line(&mut self, text: &str) {
        println!("{text}");
    }

    /// Reaches the process's own filesystem, exactly as [`crate::StdoutHost`]
    /// does.
    ///
    /// This host serves a VM run that also has a foreign half, which is the
    /// only thing that distinguishes it — and nothing about having one narrows
    /// what the program may read. Leaving it to the default refused every file
    /// operation in exactly the programs most likely to open one.
    fn file_system(&mut self, request: FileRequest<'_>) -> Result<FileResponse, FileSystemError> {
        Ok(file_system::perform(request))
    }

    /// Enters this process's kernel, exactly as [`crate::StdoutHost`] reaches
    /// this process's filesystem.
    ///
    /// The same grant, on the same reasoning: this host stands in the process a
    /// `--backend vm` run happens in, so the descriptors the program writes to
    /// are this process's. Which calls it may serve is not this host's decision
    /// — [`syscall::call`] applies the policy the call itself carries, and the
    /// CLI refuses a non-servable one by name before the program starts.
    fn syscall(&mut self, call: LinuxSyscall, args: &[i64]) -> Result<i64, SyscallError> {
        // SAFETY: the words came from a `@FFI.Syscall` call site the frontend
        // validated to register-width scalars, and a pointer among them is one
        // this program produced — the obligation this session already carries
        // for every pointer it hands a C library through libffi.
        unsafe { syscall::perform(call, args) }
    }

    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        let _active = ActiveSession::bind(self.session);
        // Served here rather than on the session, because entering the kernel is
        // the *host's* capability: the session owns libraries and closures, and
        // a system call needs neither.
        if let Some((call, signature)) = self.session.syscall_binding(foreign_id) {
            return syscall::call(self, call, &signature, args);
        }
        self.session.call_foreign(foreign_id, args)
    }

    fn foreign_callback(&mut self, callback_id: u32) -> Result<u64, ForeignCallError> {
        self.session.callback_address(callback_id)
    }

    fn native_state_create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .create(ty, value)
    }

    fn native_state_recover(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .recover(token, ty)
    }

    fn native_state_replace(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateValue, NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .replace(token, ty, value)
    }

    // The path-addressed operations, forwarded to the same store. Without these
    // the trait's defaults answer by recovering — a deep copy of the whole state
    // per field read and two per write — which is the difference between a UI
    // frame costing its own work and costing its glyph cache on every access.
    fn native_state_check(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<(), NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .check(token, ty)
    }

    fn native_state_read(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
    ) -> Result<NativeStateValue, NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .read_at(token, ty, path)
            .cloned()
    }

    fn native_state_write(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<NativeStateValue, NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .replace_at(token, ty, path, value)
    }

    fn native_state_append(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .append_at(token, ty, path, value)
    }

    fn native_state_retain(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .retain(token)
    }

    fn native_state_release(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .release(token)
            .map(|_| ())
    }

    fn native_state_release_dropping(
        &mut self,
        token: NativeStateToken,
    ) -> Result<Option<NativeStateValue>, NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .release_dropping(token)
    }
}
