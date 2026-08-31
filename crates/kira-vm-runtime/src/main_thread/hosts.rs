//! Host plumbing for the VM main-thread event loop.
//!
//! The helper thread and main-thread targets both need host capabilities, but
//! only one side may hold the caller's host at a time. [`ProxyHost`] serves
//! the helper by forwarding capability calls through the shared lock and
//! turning main-thread operations into event-loop messages, while
//! [`ForwardingHost`] serves a target already running on the loop and
//! re-enters it synchronously through [`MainLoopHandle`].

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};

use kira_runtime_abi::{
    CheckRequest, CompilerError, FileRequest, FileResponse, FileSystemError, ForeignArg,
    ForeignCallError, ForeignResult, HostCapabilities, LinuxSyscall, MainThreadError,
    MainThreadHandle, MainThreadRequest, MainThreadResponse, NativeArg, NativeCallError,
    NativeReturn, NativeStateError, NativeStatePathStep, NativeStateToken, NativeStateTypeId,
    NativeStateValue, SyscallError,
};

use super::{Job, MainLoop, MainThreadRunner};

/// The helper-side host: ordinary capabilities are shared with the caller and
/// main-thread operations become event-loop messages.
pub(super) struct ProxyHost<'a, H> {
    /// The caller's host behind the loop's synchronization boundary.
    pub(super) shared: Arc<Mutex<&'a mut H>>,
    /// The channel that delivers main-thread operations to the event loop.
    pub(super) jobs: Sender<Job>,
}

/// The main-side host used while a main-thread target runs.
pub(super) struct ForwardingHost<'a, H> {
    /// The caller's host behind the loop's synchronization boundary.
    pub(super) shared: Arc<Mutex<&'a mut H>>,
    /// The reentrant view of the event loop servicing the current target.
    pub(super) main_thread: MainLoopHandle,
}

/// A synchronous, reentrant view of the event loop for a target already
/// running on its owner thread.
///
/// The pointer is valid only while `MainLoop::execute_request` is on the stack.
/// Keeping the handle as a pair of function pointers avoids making the host's
/// capability implementation generic over the loop's runner type.
#[derive(Clone, Copy)]
pub(super) struct MainLoopHandle {
    data: *const (),
    request: unsafe fn(*const (), MainThreadRequest) -> Result<MainThreadResponse, MainThreadError>,
    join: unsafe fn(*const (), MainThreadHandle) -> Result<NativeStateValue, MainThreadError>,
}

impl MainLoopHandle {
    /// Creates a handle whose lifetime is bounded by the current request.
    pub(super) fn new<H, R>(loop_state: &MainLoop<'_, H, R>) -> Self
    where
        H: HostCapabilities + Send,
        R: MainThreadRunner,
    {
        Self {
            data: std::ptr::from_ref(loop_state).cast(),
            request: reentrant_request::<H, R>,
            join: reentrant_join::<H, R>,
        }
    }

    /// Services a request without leaving the current main-thread call.
    fn request(self, request: MainThreadRequest) -> Result<MainThreadResponse, MainThreadError> {
        // SAFETY: the handle is constructed only in `execute_request`, and the
        // target can call it only synchronously before that method returns.
        unsafe { (self.request)(self.data, request) }
    }

    /// Joins a task through the same live event loop.
    fn join(self, handle: MainThreadHandle) -> Result<NativeStateValue, MainThreadError> {
        // SAFETY: the handle is constructed only in `execute_request`, and the
        // target can call it only synchronously before that method returns.
        unsafe { (self.join)(self.data, handle) }
    }
}

/// Re-enters a concrete main loop through its erased handle.
unsafe fn reentrant_request<H, R>(
    data: *const (),
    request: MainThreadRequest,
) -> Result<MainThreadResponse, MainThreadError>
where
    H: HostCapabilities + Send,
    R: MainThreadRunner,
{
    // SAFETY: `data` came from `MainLoopHandle::new` for this exact `H` and
    // `R`, and the original `MainLoop` remains on the stack while the target
    // is executing.
    let loop_state = unsafe { &*data.cast::<MainLoop<'_, H, R>>() };
    loop_state.request_response(request)
}

/// Re-enters a concrete main loop to join a task.
unsafe fn reentrant_join<H, R>(
    data: *const (),
    handle: MainThreadHandle,
) -> Result<NativeStateValue, MainThreadError>
where
    H: HostCapabilities + Send,
    R: MainThreadRunner,
{
    // SAFETY: see `reentrant_request`.
    let loop_state = unsafe { &*data.cast::<MainLoop<'_, H, R>>() };
    let (reply, result) = channel();
    loop_state.join(handle, reply);
    result.recv().map_err(|_| MainThreadError::NoHost)?
}

/// Recovers a poisoned host mutex instead of turning a host panic into an
/// unrelated lock error after the helper has already stopped.
fn lock_host<'shared, 'host, H>(
    shared: &'shared Arc<Mutex<&'host mut H>>,
) -> MutexGuard<'shared, &'host mut H> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

macro_rules! impl_forwarding_host {
    ($host:ty, $main_thread:item, $main_thread_join:item) => {
        impl<'a, H: HostCapabilities + Send> HostCapabilities for $host {
            fn write_line(&mut self, text: &str) {
                lock_host(&self.shared).write_line(text);
            }

            fn call_native(
                &mut self,
                function_id: u32,
                args: &[NativeArg<'_>],
            ) -> Result<NativeReturn, NativeCallError> {
                lock_host(&self.shared).call_native(function_id, args)
            }

            fn call_foreign(
                &mut self,
                foreign_id: u32,
                args: &[ForeignArg<'_>],
            ) -> Result<ForeignResult, ForeignCallError> {
                lock_host(&self.shared).call_foreign(foreign_id, args)
            }

            fn foreign_callback(&mut self, callback_id: u32) -> Result<u64, ForeignCallError> {
                lock_host(&self.shared).foreign_callback(callback_id)
            }

            fn syscall(&mut self, call: LinuxSyscall, args: &[i64]) -> Result<i64, SyscallError> {
                lock_host(&self.shared).syscall(call, args)
            }

            fn native_state_create(
                &mut self,
                ty: NativeStateTypeId,
                value: NativeStateValue,
            ) -> Result<NativeStateToken, NativeStateError> {
                lock_host(&self.shared).native_state_create(ty, value)
            }

            fn native_state_recover(
                &mut self,
                token: NativeStateToken,
                ty: NativeStateTypeId,
            ) -> Result<NativeStateValue, NativeStateError> {
                lock_host(&self.shared).native_state_recover(token, ty)
            }

            fn native_state_replace(
                &mut self,
                token: NativeStateToken,
                ty: NativeStateTypeId,
                value: NativeStateValue,
            ) -> Result<(), NativeStateError> {
                lock_host(&self.shared).native_state_replace(token, ty, value)
            }

            fn native_state_check(
                &mut self,
                token: NativeStateToken,
                ty: NativeStateTypeId,
            ) -> Result<(), NativeStateError> {
                lock_host(&self.shared).native_state_check(token, ty)
            }

            fn native_state_read(
                &mut self,
                token: NativeStateToken,
                ty: NativeStateTypeId,
                path: &[NativeStatePathStep],
            ) -> Result<NativeStateValue, NativeStateError> {
                lock_host(&self.shared).native_state_read(token, ty, path)
            }

            fn native_state_write(
                &mut self,
                token: NativeStateToken,
                ty: NativeStateTypeId,
                path: &[NativeStatePathStep],
                value: NativeStateValue,
            ) -> Result<(), NativeStateError> {
                lock_host(&self.shared).native_state_write(token, ty, path, value)
            }

            fn native_state_append(
                &mut self,
                token: NativeStateToken,
                ty: NativeStateTypeId,
                path: &[NativeStatePathStep],
                value: NativeStateValue,
            ) -> Result<(), NativeStateError> {
                lock_host(&self.shared).native_state_append(token, ty, path, value)
            }

            fn native_state_free(
                &mut self,
                token: NativeStateToken,
            ) -> Result<(), NativeStateError> {
                lock_host(&self.shared).native_state_free(token)
            }

            fn file_system(
                &mut self,
                request: FileRequest<'_>,
            ) -> Result<FileResponse, FileSystemError> {
                lock_host(&self.shared).file_system(request)
            }

            fn compiler(
                &mut self,
                request: &CheckRequest,
            ) -> Result<Vec<kira_runtime_abi::CheckDiagnostic>, CompilerError> {
                lock_host(&self.shared).compiler(request)
            }

            $main_thread
            $main_thread_join
        }
    };
}

impl_forwarding_host!(
    ForwardingHost<'a, H>,
    fn main_thread(
        &mut self,
        request: MainThreadRequest,
    ) -> Result<MainThreadResponse, MainThreadError> {
        self.main_thread.request(request)
    },
    fn main_thread_join(
        &mut self,
        handle: MainThreadHandle,
    ) -> Result<NativeStateValue, MainThreadError> {
        self.main_thread.join(handle)
    }
);

impl_forwarding_host!(
    ProxyHost<'a, H>,
    fn main_thread(
        &mut self,
        request: MainThreadRequest,
    ) -> Result<MainThreadResponse, MainThreadError> {
        let (reply, result) = channel();
        self.jobs
            .send(Job::Request { request, reply })
            .map_err(|_| MainThreadError::NoHost)?;
        result.recv().map_err(|_| MainThreadError::NoHost)?
    },
    fn main_thread_join(
        &mut self,
        handle: MainThreadHandle,
    ) -> Result<NativeStateValue, MainThreadError> {
        let (reply, result) = channel();
        self.jobs
            .send(Job::Join { handle, reply })
            .map_err(|_| MainThreadError::NoHost)?;
        result.recv().map_err(|_| MainThreadError::NoHost)?
    }
);
