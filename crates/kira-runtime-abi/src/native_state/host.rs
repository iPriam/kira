//! Host-capability wrapper with portable callback-state storage.

use super::*;

/// A host wrapper that adds portable native callback-state storage.
#[derive(Debug)]
pub struct NativeStateHost<H> {
    inner: H,
    store: NativeStateStore,
}

impl<H> NativeStateHost<H> {
    /// Wraps `inner` with an empty callback-state store.
    pub fn new(inner: H) -> Self {
        Self {
            inner,
            store: NativeStateStore::new(),
        }
    }

    /// Borrows the wrapped host.
    pub fn inner(&self) -> &H {
        &self.inner
    }

    /// Mutably borrows the wrapped host.
    pub fn inner_mut(&mut self) -> &mut H {
        &mut self.inner
    }

    /// Returns the wrapped host.
    pub fn into_inner(self) -> H {
        self.inner
    }
}

impl<H: HostCapabilities> HostCapabilities for NativeStateHost<H> {
    fn write_line(&mut self, text: &str) {
        self.inner.write_line(text);
    }

    fn call_native(
        &mut self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeReturn, NativeCallError> {
        self.inner.call_native(function_id, args)
    }

    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        self.inner.call_foreign(foreign_id, args)
    }

    fn syscall(&mut self, call: LinuxSyscall, args: &[i64]) -> Result<i64, SyscallError> {
        self.inner.syscall(call, args)
    }

    fn foreign_callback(&mut self, callback_id: u32) -> Result<u64, ForeignCallError> {
        self.inner.foreign_callback(callback_id)
    }

    fn native_state_create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        self.store.create(ty, value)
    }

    fn native_state_recover(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        self.store.recover(token, ty)
    }

    fn native_state_replace(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        self.store.replace(token, ty, value)
    }

    fn native_state_check(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<(), NativeStateError> {
        self.store.check(token, ty)
    }

    fn native_state_read(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
    ) -> Result<NativeStateValue, NativeStateError> {
        self.store.read_at(token, ty, path).cloned()
    }

    fn native_state_write(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        *self.store.write_at(token, ty, path)? = value;
        Ok(())
    }

    fn native_state_append(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        match self.store.write_at(token, ty, path)? {
            // The elements are shared with whoever last read this array, so the
            // append buys a block of its own before it lands.
            NativeStateValue::Array(elements) => Arc::make_mut(elements).push(value),
            _ => return Err(NativeStateError::PathMismatch),
        }
        Ok(())
    }

    fn native_state_free(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        self.store.free(token)
    }

    fn file_system(&mut self, request: FileRequest<'_>) -> Result<FileResponse, FileSystemError> {
        self.inner.file_system(request)
    }
}
