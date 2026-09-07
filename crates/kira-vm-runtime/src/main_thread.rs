//! The VM helper-thread and host-main-thread event loop.
//!
//! A normal VM entry is still available for embedders that already own their
//! thread. [`execute_with_main_thread`] is the application entry for the new
//! runtime shape: the caller thread services main-thread requests while a
//! scoped helper thread runs Kira's `@Main` entrypoint. Requests exchange
//! owned [`NativeStateValue`] trees, never VM heap handles.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread;

use kira_bytecode::Module;
use kira_runtime_abi::{
    HostCapabilities, MainThreadError, MainThreadHandle, MainThreadOp, MainThreadRequest,
    MainThreadResponse, NativeStateValue,
};

use crate::debug::VmDebugObserver;
use crate::error::VmError;
use crate::fiber::Fiber;
use crate::interp::{Program, RunOutcome};

mod hosts;

use hosts::{ForwardingHost, MainLoopHandle, ProxyHost};

/// Runs a VM program with Kira on a scoped helper thread and the caller thread
/// acting as the main-thread event loop.
///
/// The host must be movable behind the loop's synchronization boundary. A
/// scoped thread keeps the API borrow-based, so callers retain ownership of
/// their host and all host state is returned in place when this function ends.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub fn execute_with_main_thread<H>(module: &Module, host: &mut H) -> Result<RunOutcome, VmError>
where
    H: HostCapabilities + Send,
{
    module.validate()?;
    let runner = VmMainThreadRunner {
        program: Program::load(module.clone())?,
    };
    execute_with_main_thread_using(module, host, runner)
}

/// Runs a VM program with the helper thread also carrying the instruction
/// debugger.
///
/// The observer follows the helper because that is where Kira's `@Main` body
/// executes. `VmDebugObserver` is `Send` for this reason: a debugger must be
/// able to inspect the actual Kira execution thread after the runtime split.
pub fn execute_with_main_thread_debug<H>(
    module: &Module,
    host: &mut H,
    observer: &mut dyn VmDebugObserver,
) -> Result<RunOutcome, VmError>
where
    H: HostCapabilities + Send,
{
    module.validate()?;
    let runner = VmMainThreadRunner {
        program: Program::load(module.clone())?,
    };
    execute_with_main_thread_using_debug(module, host, runner, observer)
}

/// Runs a VM entry with a caller-provided main-thread function runner.
///
/// This is the bridge point for a hybrid host: the runner can dispatch a
/// target to native code or to the VM while the event-loop machinery remains
/// generic. The runner is used only on the caller thread; it does not need to
/// be shared with the helper.
pub fn execute_with_main_thread_using<H, R>(
    module: &Module,
    host: &mut H,
    runner: R,
) -> Result<RunOutcome, VmError>
where
    H: HostCapabilities + Send,
    R: MainThreadRunner,
{
    execute_with_main_thread_inner(module, host, runner, None)
}

/// Runs a VM entry with a caller-provided target runner and a helper-thread
/// instruction debugger.
pub fn execute_with_main_thread_using_debug<H, R>(
    module: &Module,
    host: &mut H,
    runner: R,
    observer: &mut dyn VmDebugObserver,
) -> Result<RunOutcome, VmError>
where
    H: HostCapabilities + Send,
    R: MainThreadRunner,
{
    execute_with_main_thread_inner(module, host, runner, Some(observer))
}

fn execute_with_main_thread_inner<H, R>(
    module: &Module,
    host: &mut H,
    runner: R,
    mut observer: Option<&mut dyn VmDebugObserver>,
) -> Result<RunOutcome, VmError>
where
    H: HostCapabilities + Send,
    R: MainThreadRunner,
{
    module.validate()?;
    let helper_program = Program::load(module.clone())?;
    let shared = Arc::new(Mutex::new(host));
    let (jobs_tx, jobs_rx) = channel();

    thread::scope(|scope| {
        let helper_shared = Arc::clone(&shared);
        let helper_jobs = jobs_tx.clone();
        let helper = scope.spawn(move || {
            let mut proxy = ProxyHost {
                shared: helper_shared,
                jobs: helper_jobs.clone(),
            };
            let outcome = match observer.take() {
                Some(observer) => helper_program.run_with_debug(&mut proxy, observer),
                None => helper_program.run(&mut proxy),
            };
            let _ = helper_jobs.send(Job::Finished(outcome));
        });
        drop(jobs_tx);

        let loop_state = MainLoop {
            runner,
            shared,
            jobs: jobs_rx,
            tasks: RefCell::new(HashMap::new()),
            posts: RefCell::new(VecDeque::new()),
            next_handle: Cell::new(1),
            helper_outcome: RefCell::new(None),
            deferred_error: RefCell::new(None),
            external_lifecycles: Cell::new(false),
            module: module.clone(),
            fibers: RefCell::new(Vec::new()),
        };
        loop_state.run();
        let helper_joined = helper.join();
        let helper_outcome = loop_state.helper_outcome.take();
        match (helper_outcome, helper_joined) {
            (Some(outcome), Ok(())) => loop_state.finish(outcome),
            (Some(outcome), Err(_)) => {
                Err(VmError::MainThread(MainThreadError::Function(format!(
                    "the Kira helper thread panicked after returning {:?}",
                    outcome.as_ref().err()
                ))))
            }
            (None, Ok(())) => Err(VmError::MainThread(MainThreadError::NoHost)),
            (None, Err(_)) => Err(VmError::MainThread(MainThreadError::Function(
                "the Kira helper thread panicked before reporting its result".to_owned(),
            ))),
        }
    })
}

/// Executes one resolved main-thread target on the caller's event-loop thread.
pub trait MainThreadRunner {
    /// Runs `function` with copied arguments and returns `None` for `Void`.
    fn call(
        &self,
        host: &mut dyn HostCapabilities,
        function: u32,
        args: &[NativeStateValue],
    ) -> Result<Option<NativeStateValue>, MainThreadError>;

    /// Starts a lifecycle owned outside the VM, returning whether it handled it.
    fn start_lifecycle(&self, function: u32) -> Result<bool, MainThreadError> {
        let _ = function;
        Ok(false)
    }

    /// Advances externally owned lifecycles and reports whether any remain.
    fn pump_lifecycles(&self, budget: u64) -> Result<bool, MainThreadError> {
        let _ = budget;
        Ok(false)
    }

    /// Releases externally owned lifecycle state after the host loop ends.
    fn reset_lifecycles(&self) {}
}

/// The default runner for a pure VM entry.
struct VmMainThreadRunner {
    program: Program,
}

impl MainThreadRunner for VmMainThreadRunner {
    fn call(
        &self,
        host: &mut dyn HostCapabilities,
        function: u32,
        args: &[NativeStateValue],
    ) -> Result<Option<NativeStateValue>, MainThreadError> {
        self.program
            .call_state(host, function, args)
            .map_err(|error| MainThreadError::Function(error.to_string()))
    }
}

/// One message from the helper VM to the caller's event loop.
enum Job {
    /// A new invocation, queued task, or post.
    Request {
        /// The owned operation.
        request: MainThreadRequest,
        /// The synchronous acknowledgement channel.
        reply: Sender<Result<MainThreadResponse, MainThreadError>>,
    },
    /// A join of a previously spawned main-thread task.
    Join {
        /// The task being joined.
        handle: MainThreadHandle,
        /// The result channel.
        reply: Sender<Result<NativeStateValue, MainThreadError>>,
    },
    /// The helper entrypoint ended.
    Finished(Result<RunOutcome, VmError>),
}

/// A queued main-thread task.
enum MainTask {
    /// The target has not run yet.
    Queued(MainThreadRequest),
    /// The target ran and its result is waiting for a join.
    Finished(Result<Option<NativeStateValue>, MainThreadError>),
}

/// State owned by the caller's main-thread event loop.
struct MainLoop<'a, H, R> {
    runner: R,
    shared: Arc<Mutex<&'a mut H>>,
    jobs: Receiver<Job>,
    tasks: RefCell<HashMap<MainThreadHandle, MainTask>>,
    posts: RefCell<VecDeque<MainThreadRequest>>,
    next_handle: Cell<u64>,
    helper_outcome: RefCell<Option<Result<RunOutcome, VmError>>>,
    deferred_error: RefCell<Option<MainThreadError>>,
    /// Whether the runner owns at least one unfinished native lifecycle.
    external_lifecycles: Cell<bool>,
    /// The module the lifecycles run, kept so a slice can be resumed without
    /// borrowing the caller's copy for the loop's whole life.
    module: Module,
    /// Every `@MainThreadLifecycle` function this thread carries, each with
    /// its own suspended execution.
    ///
    /// Several may be declared: a graphics loop and a UI loop share the main
    /// thread with dispatched `@MainThread` tasks, so each gets a slice in
    /// turn and the thread is never held by one of them.
    fibers: RefCell<Vec<Fiber>>,
}

/// Instructions one lifecycle may run before the thread moves on.
///
/// The slice is what turns several long-lived loops into fair sharing: the
/// more lifecycles a program declares, the less of the thread each gets, and
/// none of them can starve the dispatched `@MainThread` work between passes.
const LIFECYCLE_SLICE: u64 = 4096;

impl<H: HostCapabilities + Send, R: MainThreadRunner> MainLoop<'_, H, R> {
    /// Services requests until the helper reports that its entrypoint ended.
    fn run(&self) {
        let mut helper_done = false;
        loop {
            // A lifecycle is a long-lived event loop, so it is pumped once per
            // pass rather than waited on: blocking in one would freeze its
            // siblings, the queued main-thread work, and the tasks any of them
            // dispatch.
            self.pump_lifecycles();
            if self.service_one_deferred() {
                continue;
            }
            // A lifecycle outlives the entrypoint: it ends when its own body
            // returns, so the thread stays until both are done.
            let idle = self.lifecycles_finished();
            if helper_done && idle {
                break;
            }
            // With a lifecycle to pump, waiting on the helper would stop every
            // loop until the application thread happened to speak. Only a
            // program with no lifecycle can afford to block here.
            let event = if idle {
                match self.jobs.recv() {
                    Ok(event) => event,
                    Err(_) => break,
                }
            } else {
                match self.jobs.try_recv() {
                    Ok(event) => event,
                    Err(TryRecvError::Empty) => continue,
                    // The helper is gone, so no further job can arrive. A
                    // lifecycle outlives it, so keep pumping rather than
                    // tearing the thread down under a running loop.
                    Err(TryRecvError::Disconnected) => continue,
                }
            };
            if self.service(event) {
                helper_done = true;
            }
        }
        self.drain_deferred();
        self.runner.reset_lifecycles();
    }

    /// Gives every unfinished lifecycle one slice, in declaration order.
    fn pump_lifecycles(&self) {
        let mut fibers = self.fibers.borrow_mut();
        for fiber in fibers.iter_mut() {
            if fiber.finished() {
                continue;
            }
            let mut host = ForwardingHost {
                shared: Arc::clone(&self.shared),
                main_thread: MainLoopHandle::new(self),
            };
            if let Err(error) = fiber.step(&mut host, &self.module, LIFECYCLE_SLICE) {
                self.deferred_error
                    .borrow_mut()
                    .get_or_insert(MainThreadError::Function(error.to_string()));
            }
        }
        drop(fibers);
        match self.runner.pump_lifecycles(LIFECYCLE_SLICE) {
            Ok(active) => self.external_lifecycles.set(active),
            Err(error) => {
                self.external_lifecycles.set(false);
                self.deferred_error.borrow_mut().get_or_insert(error);
            }
        }
    }

    /// Whether every declared lifecycle has ended.
    fn lifecycles_finished(&self) -> bool {
        self.fibers.borrow().iter().all(Fiber::finished) && !self.external_lifecycles.get()
    }

    /// Executes one queued post or spawned task before waiting for another
    /// helper request. This is the event-loop turn that lets the helper keep
    /// running while main-thread work is serviced independently.
    fn service_one_deferred(&self) -> bool {
        if let Some(request) = self.posts.borrow_mut().pop_front() {
            if let Err(error) = self.execute_request(&request) {
                self.deferred_error.borrow_mut().get_or_insert(error);
            }
            return true;
        }
        let Some(handle) = self
            .tasks
            .borrow()
            .iter()
            .filter_map(|(handle, task)| matches!(task, MainTask::Queued(_)).then_some(*handle))
            .min_by_key(|handle| handle.word())
        else {
            return false;
        };
        let Some(MainTask::Queued(request)) = self.tasks.borrow_mut().remove(&handle) else {
            return false;
        };
        let result = self.execute_request(&request);
        if let Err(error) = &result {
            self.deferred_error
                .borrow_mut()
                .get_or_insert(error.clone());
        }
        self.tasks
            .borrow_mut()
            .insert(handle, MainTask::Finished(result));
        true
    }

    /// Handles one message, returning `true` when the helper's result ended the
    /// loop.
    fn service(&self, event: Job) -> bool {
        match event {
            Job::Request { request, reply } => {
                self.request(request, reply);
                false
            }
            Job::Join { handle, reply } => {
                self.join(handle, reply);
                false
            }
            Job::Finished(outcome) => {
                *self.helper_outcome.borrow_mut() = Some(outcome);
                true
            }
        }
    }

    /// Registers or runs one main-thread request.
    fn request(
        &self,
        request: MainThreadRequest,
        reply: Sender<Result<MainThreadResponse, MainThreadError>>,
    ) {
        let result = self.request_response(request);
        let _ = reply.send(result);
    }

    /// Handles a request from code already running on the main loop.
    ///
    /// Main-thread targets may call another main-thread operation directly. It
    /// is still the same synchronous event loop, so servicing that request
    /// inline preserves the thread affinity without sending a message to the
    /// thread that is already executing it.
    fn request_response(
        &self,
        request: MainThreadRequest,
    ) -> Result<MainThreadResponse, MainThreadError> {
        Ok(match request.operation {
            MainThreadOp::Invoke => self.execute_request(&request).map(|value| match value {
                Some(value) => MainThreadResponse::Value(value),
                None => MainThreadResponse::Posted,
            })?,
            MainThreadOp::Spawn => {
                let handle = self.allocate_handle();
                self.tasks
                    .borrow_mut()
                    .insert(handle, MainTask::Queued(request));
                MainThreadResponse::Spawned(handle)
            }
            MainThreadOp::Post => {
                self.posts.borrow_mut().push_back(request);
                MainThreadResponse::Posted
            }
            MainThreadOp::LifecycleStart => {
                if self.runner.start_lifecycle(request.function)? {
                    self.external_lifecycles.set(true);
                    return Ok(MainThreadResponse::Posted);
                }
                if !self
                    .module
                    .main_thread_lifecycles()
                    .contains(&request.function)
                {
                    return Err(MainThreadError::UnknownFunction(request.function));
                }
                self.fibers.borrow_mut().push(Fiber::new(request.function));
                MainThreadResponse::Posted
            }
        })
    }

    /// Joins and, if necessary, runs one queued main-thread task.
    fn join(
        &self,
        handle: MainThreadHandle,
        reply: Sender<Result<NativeStateValue, MainThreadError>>,
    ) {
        let result = match self.tasks.borrow_mut().remove(&handle) {
            Some(MainTask::Queued(request)) => self.execute_request(&request),
            Some(MainTask::Finished(result)) => result,
            None => Err(MainThreadError::UnknownHandle(handle.word())),
        };
        let result = result.map(|value| {
            // `Void` has no state-tree form. Main-thread task joins use the
            // ordinary task convention and produce integer zero for it.
            value.unwrap_or(NativeStateValue::Int(0))
        });
        let _ = reply.send(result);
    }

    /// Runs all posts and tasks left when the helper entrypoint has returned.
    fn drain_deferred(&self) {
        while self.service_one_deferred() {}
    }

    /// Runs one target function on the caller thread.
    fn execute_request(
        &self,
        request: &MainThreadRequest,
    ) -> Result<Option<NativeStateValue>, MainThreadError> {
        let mut host = ForwardingHost {
            shared: Arc::clone(&self.shared),
            main_thread: MainLoopHandle::new(self),
        };
        self.runner.call(&mut host, request.function, &request.args)
    }

    /// Allocates a non-zero handle without reusing a live row.
    fn allocate_handle(&self) -> MainThreadHandle {
        let next = self.next_handle.get();
        let handle = MainThreadHandle::from_word(next);
        self.next_handle.set(next.saturating_add(1).max(1));
        handle
    }

    /// Combines the helper outcome with any deferred main-thread failure.
    fn finish(self, outcome: Result<RunOutcome, VmError>) -> Result<RunOutcome, VmError> {
        if let Some(error) = self.deferred_error.into_inner() {
            return Err(VmError::MainThread(error));
        }
        outcome
    }
}

#[cfg(test)]
#[path = "main_thread_tests.rs"]
mod tests;
