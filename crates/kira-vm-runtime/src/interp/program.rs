//! The embedder-facing doors into the interpreter.
//!
//! [`execute`] and [`Program`] run a module whose heap belongs to one run.
//! [`crate::Instance`] is the other door, and it lives beside this one rather
//! than inside it because it owns a heap that outlives a call. What both doors
//! share — the check that a request names a real, enterable function with the
//! right number of arguments — is [`check_signature`].

use kira_bytecode::module::Module;
use kira_runtime_abi::{HostCapabilities, NativeArg, NativeResult, NativeReturn};

use crate::debug::VmDebugObserver;
use crate::error::VmError;
use crate::interp::Vm;
use crate::value::{Heap, HeapStats, Value};

/// The outcome of a completed run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunOutcome {
    /// The value `@Main` returned (`Void` for a `Void` main).
    pub result: Value,
    /// Heap accounting at exit; `current` is 0 for a clean run.
    pub heap: HeapStats,
}

/// Runs `module`'s entrypoint, sending output to `host`.
///
/// Returns the entrypoint's result and heap accounting on success, or a
/// [`VmError`] trap. The final result value is dropped before accounting, so a
/// clean run reports `current == 0`.
pub fn execute(module: &Module, host: &mut dyn HostCapabilities) -> Result<RunOutcome, VmError> {
    module.validate()?;
    run_entry(module, host)
}

/// Runs `module`'s entrypoint with an instruction-level debugger attached.
pub fn execute_with_debug(
    module: &Module,
    host: &mut dyn HostCapabilities,
    observer: &mut dyn VmDebugObserver,
) -> Result<RunOutcome, VmError> {
    module.validate()?;
    run_entry_with_debug(module, host, observer)
}

/// Runs `module`'s entrypoint on a fresh VM, assuming it is already validated.
fn run_entry(module: &Module, host: &mut dyn HostCapabilities) -> Result<RunOutcome, VmError> {
    let mut vm = Vm::new(host, Heap::new());
    let main = module.main.ok_or(VmError::NoEntrypoint)?;
    let result = vm.enter(module, main, &[])?;
    // The program's result is no longer referenced by anything; drop it so
    // heap accounting reflects a fully reclaimed program.
    vm.heap.drop_value(result);
    Ok(RunOutcome {
        result,
        heap: vm.heap.stats(),
    })
}

fn run_entry_with_debug(
    module: &Module,
    host: &mut dyn HostCapabilities,
    observer: &mut dyn VmDebugObserver,
) -> Result<RunOutcome, VmError> {
    let mut vm = Vm::new(host, Heap::new());
    let main = module.main.ok_or(VmError::NoEntrypoint)?;
    let result = vm.enter_values_with_debug(module, main, Vec::new(), observer)?;
    vm.heap.drop_value(result);
    Ok(RunOutcome {
        result,
        heap: vm.heap.stats(),
    })
}

/// An owned [`Module`] proven safe to interpret.
///
/// A `Module` is a public, deserializable artifact, so every index and operand
/// in it is validated before anything is trusted — that is what lets
/// interpretation index without the bounds checks it would otherwise need, and
/// without panicking on a malformed artifact.
///
/// Validation is a whole-module pass, so it is done once here rather than per
/// entry. That matters for a hybrid program, where the native half calls back
/// into the VM through [`Program::call`] at every crossing: re-proving the
/// module on each call would make a boundary crossing cost a scan of the
/// program.
///
/// The module is *owned* rather than borrowed: a host loads bytecode from
/// somewhere (a `.kbc` file, a network, memory), and the thing that runs it is
/// the natural owner of it.
pub struct Program {
    module: Module,
}

impl Program {
    /// Validates `module` and takes ownership of it, or reports why it cannot
    /// be run.
    pub fn load(module: Module) -> Result<Program, VmError> {
        module.validate()?;
        Ok(Program { module })
    }

    /// The module being run.
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Runs the entrypoint, sending output to `host`.
    pub fn run(&self, host: &mut dyn HostCapabilities) -> Result<RunOutcome, VmError> {
        run_entry(&self.module, host)
    }

    /// Runs the entrypoint with an instruction-level debugger attached.
    pub fn run_with_debug(
        &self,
        host: &mut dyn HostCapabilities,
        observer: &mut dyn VmDebugObserver,
    ) -> Result<RunOutcome, VmError> {
        run_entry_with_debug(&self.module, host, observer)
    }

    /// Runs one function by id with `args`, and returns what it produced.
    ///
    /// This is the mirror of [`HostCapabilities::call_native`]: that is how a
    /// running program reaches the native half, and this is how the native half
    /// reaches back. Both speak the same seam vocabulary, so an embedder hosting
    /// a hybrid program marshals one way in each direction and nothing else.
    ///
    /// Ownership follows the same rule in both directions: **args borrow** (a
    /// string arrives as a `&str` the caller still owns, and is copied into this
    /// run's heap) and **the result owns** (a returned string is handed out as
    /// an owned `String`, because handing a value out is a move).
    ///
    /// Each call runs on its own heap and operand stack. Nothing outlives the
    /// call — the result is copied out before the heap is dropped — so calls
    /// nest freely, which is exactly what a native function calling a
    /// `@Runtime` function that calls a `@Native` function needs.
    pub fn call(
        &self,
        host: &mut dyn HostCapabilities,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeResult, VmError> {
        Ok(self.call_capturing(host, function_id, args, &[])?.result)
    }

    /// [`Program::call`], also handing back the final value of each parameter
    /// slot in `capture`.
    ///
    /// What the native half calls when the `@Runtime` function it is reaching
    /// writes through a parameter. The caller is the other engine, so there is
    /// no place for the VM to write into — the values come back instead, and
    /// the caller stores them where its own signature says they belong.
    pub fn call_capturing(
        &self,
        host: &mut dyn HostCapabilities,
        function_id: u32,
        args: &[NativeArg<'_>],
        capture: &[u16],
    ) -> Result<NativeReturn, VmError> {
        check_signature(&self.module, function_id, args.len())?;

        let mut vm = Vm::new(host, Heap::new());
        let (result, captured) = vm.enter_capturing(&self.module, function_id, args, capture)?;
        let mut writebacks = Vec::with_capacity(captured.len());
        for (slot, value) in captured {
            let lifted = vm.heap.lift(value);
            vm.heap.drop_value(value);
            writebacks.push((
                slot,
                lifted.ok_or(VmError::StructAtSeam {
                    function: function_id,
                })?,
            ));
        }
        let lifted = vm.heap.lift(result);
        vm.heap.drop_value(result);
        Ok(NativeReturn {
            result: lifted.ok_or(VmError::StructAtSeam {
                function: function_id,
            })?,
            writebacks,
        })
    }
}

/// Checks that `function_id` names a function of this module that takes exactly
/// `arg_count` arguments.
///
/// The one place both embedder entry points ([`Program::call`] and
/// [`crate::Instance::call`]) agree on what a well-formed request looks like, so
/// a host driving the VM from an artifact that disagrees with this module is
/// refused the same way through either door.
pub(crate) fn check_signature(
    module: &Module,
    function_id: u32,
    arg_count: usize,
) -> Result<(), VmError> {
    let function = module
        .functions
        .get(function_id as usize)
        .ok_or(VmError::UnknownFunction(function_id))?;
    if function.is_native() {
        return Err(VmError::NativeEntry {
            function: function_id,
        });
    }
    if arg_count != usize::from(function.param_count) {
        return Err(VmError::ArityMismatch {
            function: function_id,
            expected: function.param_count,
            got: arg_count,
        });
    }
    Ok(())
}
