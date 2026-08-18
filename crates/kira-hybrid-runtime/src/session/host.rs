//! The host a hybrid program's bytecode half runs against.
//!
//! Wraps the consumer's capabilities and adds the one thing a hybrid program
//! needs that a VM-only one does not: `call_native`, the route from a
//! `@Runtime` function into a `@Native` one.

use super::*;

/// A [`HostCapabilities`] over a shared session.
///
/// Stateless by design; see this module's docs for why that is what makes
/// nesting work.
pub(super) struct Host<'a> {
    pub(super) session: &'a Session,
}

impl HostCapabilities for Host<'_> {
    fn write_line(&mut self, text: &str) {
        // Straight to stdout, exactly as the VM-only host does. Both halves
        // write through Rust's `LineWriter`, which flushes on newline, so
        // output from the two engines interleaves correctly on fd 1 with no
        // extra flushing.
        println!("{text}");
    }

    fn call_native(
        &mut self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeReturn, NativeCallError> {
        self.session.call_native(function_id, args)
    }

    /// Enters this process's kernel, exactly as [`Self::file_system`] reaches
    /// this process's files.
    ///
    /// The two halves of a hybrid program run in one process, so the descriptor
    /// a `@Runtime` function writes to is the one the native half writes to —
    /// and the native half reaches it by an instruction the backend emitted,
    /// which is the same kernel this enters.
    fn syscall(&mut self, call: LinuxSyscall, args: &[i64]) -> Result<i64, SyscallError> {
        // SAFETY: the words came from a `@FFI.Syscall` call site the frontend
        // validated to register-width scalars, and a pointer among them is one
        // this program produced — the obligation this session already carries
        // for every pointer its bytecode half hands a C library through libffi.
        unsafe { syscall::perform(call, args) }
    }

    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        // The bytecode half reaches a `@FFI.Syscall` only when a `@Runtime`
        // function called one directly; `packages/linux` marks its wrappers
        // `@Native`, so the usual route is the emitted instruction. Served here
        // rather than on the session because entering the kernel is the host's
        // capability, and refused for the calls no interpreted half can serve —
        // under `kira test` this process is the runner, and an `exit_group` from
        // the bytecode half would end the suite mid-report.
        if let Some((call, signature)) = self.session.syscall_binding(foreign_id) {
            return syscall::call(self, call, &signature, args);
        }
        self.session.call_foreign(foreign_id, args)
    }

    fn foreign_callback(&mut self, callback_id: u32) -> Result<u64, ForeignCallError> {
        self.session.callback_address(callback_id)
    }

    /// Straight to the process's filesystem, exactly as the VM-only host does.
    ///
    /// The two halves of a hybrid program run in one process, so a `@Runtime`
    /// function and a `@Native` one reach the same files — and through the same
    /// implementation, since `kira_rt_fs_*` calls this very function.
    fn file_system(&mut self, request: FileRequest<'_>) -> Result<FileResponse, FileSystemError> {
        Ok(file_system::perform(request))
    }

    fn native_state_create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .create(ty, value),
            None => self.session.library.native_state_create(ty, value),
        }
    }

    fn native_state_recover(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .recover(token, ty),
            None => self.session.library.native_state_recover(token, ty),
        }
    }

    fn native_state_replace(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .replace(token, ty, value),
            None => self.session.library.native_state_replace(token, ty, value),
        }
    }

    fn native_state_free(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .free(token),
            None => self.session.library.native_state_free(token),
        }
    }

    fn native_state_check(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<(), NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .check(token, ty),
            None => self
                .session
                .library
                .native_state_recover(token, ty)
                .map(|_| ()),
        }
    }

    fn native_state_read(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
    ) -> Result<NativeStateValue, NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .read_at(token, ty, path)
                .cloned(),
            None => {
                let root = self.session.library.native_state_recover(token, ty)?;
                native_state_walk(&root, path).cloned()
            }
        }
    }

    fn native_state_write(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        match &self.session.vm_state {
            Some(state) => {
                *state
                    .lock()
                    .unwrap_or_else(|held| held.into_inner())
                    .write_at(token, ty, path)? = value;
                Ok(())
            }
            None => {
                let mut root = self.session.library.native_state_recover(token, ty)?;
                *native_state_walk_mut(&mut root, path)? = value;
                self.session.library.native_state_replace(token, ty, root)
            }
        }
    }

    fn native_state_append(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        match &self.session.vm_state {
            Some(state) => match state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .write_at(token, ty, path)?
            {
                NativeStateValue::Array(elements) => Arc::make_mut(elements).push(value),
                _ => return Err(NativeStateError::PathMismatch),
            },
            None => {
                let mut root = self.session.library.native_state_recover(token, ty)?;
                match native_state_walk_mut(&mut root, path)? {
                    NativeStateValue::Array(elements) => Arc::make_mut(elements).push(value),
                    _ => return Err(NativeStateError::PathMismatch),
                }
                self.session.library.native_state_replace(token, ty, root)?;
            }
        }
        Ok(())
    }
}
