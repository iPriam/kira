//! Host-capability calls made by bytecode instructions.

use kira_bytecode::module::Module;
use kira_runtime_abi::{ForeignArg, ForeignPointerWidth, ForeignResult, NativeArg, NativeResult};

use super::NativeCallScratch;
use super::Vm;
use super::frames::{Frame, Writeback};
use crate::error::VmError;
use crate::value::{AggregateMismatch, Value};

/// The fixed inputs for one native seam call.
struct NativeCallRequest<'a> {
    /// The bytecode module whose native function is entered.
    module: &'a Module,
    /// The function index being entered.
    id: u32,
    /// Caller places that receive values written through by the native side.
    writebacks: &'a [Writeback],
    /// Active VM frames receiving writeback values.
    frames: &'a mut [Frame],
    /// First argument position on the operand stack.
    first: usize,
    /// Number of arguments on the operand stack.
    count: usize,
}

impl Vm<'_> {
    /// Rebuilds every deferred state read sitting at or above `first` on the
    /// operand stack.
    ///
    /// A seam call is where a value stops being the VM's: the other side reads
    /// it as an object, so the deferral cannot cross.
    pub(super) fn own_arguments(&mut self, first: usize) {
        for index in first..self.stack.len() {
            if matches!(self.stack[index], Value::NativeSnapshot(_)) {
                self.stack[index] = self.heap.own(self.stack[index]);
            }
        }
    }

    /// Calls into the native half through the embedder.
    ///
    /// `writebacks` is empty for an ordinary crossing. When it is not, the
    /// callee was declared to write through those parameters, and their final
    /// values come back with the result rather than being moved out of a callee
    /// frame — there is no callee frame, and the two engines share no heap, so
    /// what crossed was a copy and what returns is another one. A recovered
    /// native-state view follows the same copy protocol, with the returned
    /// tree replacing the host-owned state when the view was passed whole.
    pub(super) fn call_native(
        &mut self,
        module: &Module,
        id: u32,
        writebacks: &[Writeback],
        frames: &mut [Frame],
    ) -> Result<(), VmError> {
        let proto = module
            .functions
            .get(usize::try_from(id).map_err(|_| VmError::UnknownFunction(u64::from(id)))?)
            .ok_or(VmError::UnknownFunction(u64::from(id)))?;
        let count = usize::try_from(proto.param_count)
            .map_err(|_| VmError::LocalSlotOutOfRange(proto.param_count))?;
        let first = self
            .stack
            .len()
            .checked_sub(count)
            .ok_or(VmError::StackUnderflow)?;
        let mut scratch = std::mem::take(&mut self.native_scratch);
        let result = self.call_native_with_scratch(
            NativeCallRequest {
                module,
                id,
                writebacks,
                frames,
                first,
                count,
            },
            &mut scratch,
        );
        // `call_native_with_scratch` can refuse before it reaches the host: a
        // native-state view may not recover, an aggregate may not have a seam
        // tree, or a returned value may fail a writeback. The arguments are
        // still owned by the operand stack in all of those cases. Remove them
        // here, outside the fallible helper, so every path releases exactly the
        // values that crossed (or were about to cross) the seam.
        for value in self.stack.drain(first..first + count) {
            self.heap.drop_value(value);
        }
        scratch.clear();
        self.native_scratch = scratch;
        result
    }

    /// Executes one native crossing with its temporary vectors detached from
    /// the VM. Keeping those buffers outside the VM during the call lets the
    /// argument trees borrow freely while every success and error path returns
    /// their capacity through [`Self::call_native`].
    fn call_native_with_scratch(
        &mut self,
        call: NativeCallRequest<'_>,
        scratch: &mut NativeCallScratch,
    ) -> Result<(), VmError> {
        let NativeCallRequest {
            module,
            id,
            writebacks,
            frames,
            first,
            count,
        } = call;
        let active_vm = self as *mut _;
        let arguments = &mut scratch.arguments;
        let trees = &mut scratch.trees;
        let native_views = &mut scratch.native_views;
        // A deferred state read becomes objects before it reaches the seam.
        // A recovered view is materialized below from the host-owned state, so
        // neither deferred reads nor native-state handles cross this call.
        self.own_arguments(first);
        // Copied off the stack so the borrow ends here: building an aggregate's
        // tree needs the heap mutably, and the borrowed arguments below cannot
        // be holding it at the same time. A `Value` is `Copy`, so this costs a
        // memcpy of the argument words and nothing else — the stack still owns
        // every one of them, and the drop loop below is still what releases
        // them.
        arguments.clear();
        arguments.extend_from_slice(&self.stack[first..first + count]);

        // Every aggregate becomes an owned tree first. `NativeArg::Aggregate`
        // borrows, so the trees have to outlive the argument list built from
        // them; this is where they live for the duration of the call.
        trees.clear();
        native_views.clear();
        native_views.resize(count, None);
        for (index, value) in arguments.iter().enumerate() {
            let tree = match *value {
                Value::Struct(_) | Value::Array(_) => Some(
                    self.heap
                        .seam_tree(*value)
                        .ok_or(VmError::StructAtSeam { function: id })?,
                ),
                // Only the payload-carrying ones: a payload-less enum crosses
                // as its tag, with no tree and no allocation.
                Value::Enum(enum_id) if self.heap.enum_seam_tag(enum_id).is_none() => Some(
                    self.heap
                        .seam_tree(*value)
                        .ok_or(VmError::EnumAtSeam { function: id })?,
                ),
                Value::Erased(_) => Some(
                    self.heap
                        .seam_tree(*value)
                        .ok_or(VmError::HandleAtSeam { function: id })?,
                ),
                // A recovered view is a borrow into host-owned callback state,
                // not a VM heap handle. Snapshot it into the same backend-
                // neutral tree used by an ordinary aggregate. If the callee
                // writes through the parameter, the source is used below to
                // replace the state with the returned tree.
                Value::NativeView { token, type_id } => {
                    native_views[index] = Some((token, type_id));
                    Some(
                        self.host
                            .native_state_recover(token, type_id)
                            .map_err(VmError::NativeState)?,
                    )
                }
                _ => None,
            };
            trees.push(tree);
        }

        let mut lowered = Vec::with_capacity(count);
        for (index, value) in arguments.iter().enumerate() {
            lowered.push(match *value {
                Value::Int(value) => NativeArg::Int(value),
                Value::Float(value) => NativeArg::Float(value),
                Value::Bool(value) => NativeArg::Bool(value),
                Value::Str(id) => NativeArg::Str(self.heap.get(id)),
                Value::Void => NativeArg::Void,
                // A struct, an array, and a payload-carrying enum all cross as
                // the tree built above. The tree is this side's copy; the
                // original stays on the stack and the drop loop releases it.
                Value::Struct(_) | Value::Array(_) => match &trees[index] {
                    Some(tree) => NativeArg::Aggregate(tree),
                    None => return Err(VmError::StructAtSeam { function: id }),
                },
                // A payload-less enum crosses as its variant tag alone; one
                // carrying something takes the tree, and the two are told apart
                // by the value rather than by the signature.
                Value::Enum(enum_id) => match self.heap.enum_seam_tag(enum_id) {
                    Some(tag) => NativeArg::Enum(tag),
                    None => match &trees[index] {
                        Some(tree) => NativeArg::Aggregate(tree),
                        None => return Err(VmError::EnumAtSeam { function: id }),
                    },
                },
                Value::RawPtr(value) => NativeArg::RawPtr(value),
                // A C block crosses as its payload address. The block stays on
                // the stack until the drop loop after the call, so the callee
                // reads live storage for exactly the duration of the call —
                // the borrowed half of the `c_storage` contract.
                Value::CBlock(block) => NativeArg::RawPtr(self.heap.cblock_address(block)),
                // A cell is refused with the handles: it is shared mutable
                // storage this heap counts holds on, and no ordinary seam
                // signature names one. An erased value has an explicit state
                // tree form, so it is handled by the aggregate arm below.
                // A deferred read is refused with them, and is unreachable:
                // `own_arguments` above rebuilt every one on this stack, so a
                // state read arrives as the struct, array or enum it holds.
                Value::NativeState(_) | Value::NativeSnapshot(_) | Value::Cell(_) => {
                    return Err(VmError::HandleAtSeam { function: id });
                }
                Value::Erased(_) => match &trees[index] {
                    Some(tree) => NativeArg::Aggregate(tree),
                    None => return Err(VmError::HandleAtSeam { function: id }),
                },
                Value::NativeView { .. } => match &trees[index] {
                    Some(tree) => NativeArg::Aggregate(tree),
                    None => return Err(VmError::HandleAtSeam { function: id }),
                },
            });
        }
        let returned = {
            let _active = super::ActiveVmGuard::install(active_vm, module);
            self.host
                .call_native(id, &lowered)
                .map_err(VmError::NativeCall)
        };

        let returned = returned?;
        // The writebacks land before the result is pushed, so a failure among
        // them leaves nothing half-pushed on the operand stack. Each is stored
        // into the caller's place exactly as a returning frame's would be —
        // same walk, same drop of what was there.
        for writeback in writebacks {
            let value = returned
                .writebacks
                .iter()
                .find(|(param, _)| u64::from(*param) == writeback.param)
                .map(|(_, value)| value.clone())
                .ok_or(VmError::MissingSeamWriteback {
                    function: id,
                    param: writeback.param,
                })?;
            // A whole recovered view is still backed by the host's state
            // store. Replacing the caller's VM local with an ordinary struct
            // would silently sever that view, so write the native result back
            // into the object it names. A non-empty path remains a normal VM
            // writeback; the place machinery already writes through a view.
            if let Some(source) = native_views
                .get(
                    usize::try_from(writeback.param)
                        .map_err(|_| VmError::LocalSlotOutOfRange(writeback.param))?,
                )
                .and_then(|source| *source)
                && writeback.steps.is_empty()
            {
                let NativeResult::Aggregate(value) = value else {
                    return Err(VmError::HandleAtSeam { function: id });
                };
                self.host
                    .native_state_replace(source.0, source.1, value)
                    .map_err(VmError::NativeState)?;
                continue;
            }
            let value = self
                .heap
                .absorb(value)
                .ok_or(VmError::HandleAtSeam { function: id })?;
            self.write_back(frames, writeback, value)?;
        }

        let result = self
            .heap
            .absorb(returned.result)
            .ok_or(VmError::HandleAtSeam { function: id })?;
        self.stack.push(result);
        Ok(())
    }

    /// Calls a foreign C function through the embedder's `call_foreign`.
    /// Pushes the address C enters the Kira function callback `id` names.
    ///
    /// The VM produces no native code, so the address is the host's to give: it
    /// is the entry thunk the backend generated for this row. A host without a
    /// foreign half refuses, exactly as it refuses a foreign call.
    pub(super) fn foreign_callback(&mut self, module: &Module, id: u32) -> Result<(), VmError> {
        if module.foreign_callbacks.get(id as usize).is_none() {
            return Err(VmError::UnknownForeignCallback(id));
        }
        let address = self
            .host
            .foreign_callback(id)
            .map_err(VmError::ForeignCall)?;
        self.stack.push(Value::RawPtr(address));
        Ok(())
    }

    pub(super) fn call_foreign(&mut self, module: &Module, id: u32) -> Result<(), VmError> {
        let active_vm = self as *mut _;
        let import = module
            .foreign_imports
            .get(id as usize)
            .ok_or(VmError::UnknownForeign(id))?;
        let params = import.signature().parameters();
        let count = params.len();
        let first = self
            .stack
            .len()
            .checked_sub(count)
            .ok_or(VmError::StackUnderflow)?;
        // Same reason as the native seam: an aggregate crossing into C is laid
        // out from real objects, so the deferral ends before the lowering does.
        self.own_arguments(first);

        // An aggregate argument's C-layout bytes are built here and held for the
        // whole call: `ForeignArg::Aggregate` borrows them, so the buffers must
        // outlive `lowered` and cannot live inside the loop.
        let width = ForeignPointerWidth::HOST;
        let mut aggregate_bytes: Vec<(usize, Vec<u8>)> = Vec::new();
        let mut mismatch = None;
        let mut too_long: Option<(u32, usize)> = None;
        for (offset, &expected) in params.iter().enumerate() {
            let Some(id) = expected.aggregate() else {
                continue;
            };
            let value = self.stack[first + offset];
            match self
                .heap
                .aggregate_bytes(&module.foreign_aggregates, id, value, width)
            {
                Ok(bytes) => aggregate_bytes.push((offset, bytes)),
                // An array that does not fit is the program's own mistake, so it
                // is reported as itself rather than folded into the shape
                // mismatch that means the compiler built the table wrong.
                Err(AggregateMismatch::ArrayTooLong { count, len }) => {
                    too_long = Some((count, len));
                    break;
                }
                Err(AggregateMismatch::Shape) => {
                    mismatch = Some(expected);
                    break;
                }
            }
        }

        let mut lowered = Vec::with_capacity(count);
        if mismatch.is_none() && too_long.is_none() {
            for (offset, &expected) in params.iter().enumerate() {
                let value = self.stack[first + offset];
                let argument =
                    match expected.aggregate() {
                        Some(id) => aggregate_bytes.iter().find(|(at, _)| *at == offset).map(
                            |(_, bytes)| ForeignArg::Aggregate {
                                id,
                                bytes: bytes.as_slice(),
                            },
                        ),
                        None => self.heap.foreign_arg(expected, value),
                    };
                match argument {
                    Some(argument) => lowered.push(argument),
                    None => {
                        mismatch = Some(expected);
                        break;
                    }
                }
            }
        }
        let outcome = match (too_long, mismatch) {
            (Some((count, len)), _) => Err(VmError::ForeignArrayTooLong { count, len }),
            (None, Some(expected)) => Err(VmError::ForeignArgMismatch {
                foreign: id,
                expected,
            }),
            (None, None) => {
                let _active = super::ActiveVmGuard::install(active_vm, module);
                self.host
                    .call_foreign(id, &lowered)
                    .map_err(VmError::ForeignCall)
            }
        };
        drop(lowered);
        // A `retains:` parameter's argument transfers to the retained registry
        // instead of dropping: the callee kept pointers into its C storage, so
        // the value must outlive every schedule this side could guess. Only a
        // call that actually ran retains — a refused call showed C nothing.
        let succeeded = outcome.is_ok();
        for (index, value) in self.stack.drain(first..first + count).enumerate() {
            if succeeded && import.signature().is_retained(index) {
                self.heap.retain_for_foreign(value);
            } else {
                self.heap.drop_value(value);
            }
        }

        let outcome = outcome?;
        let spec = outcome.spec();
        // An aggregate result is rebuilt into a Kira struct here rather than in
        // `absorb_foreign`: the member tree lives on the module, which the heap
        // has no access to.
        let result = match outcome {
            ForeignResult::Aggregate {
                id: aggregate,
                ref bytes,
            } => self
                .heap
                .absorb_aggregate(&module.foreign_aggregates, aggregate, bytes, width),
            other => self.heap.absorb_foreign(other),
        };
        let result = result.ok_or(VmError::ForeignArgMismatch {
            foreign: id,
            expected: spec,
        })?;
        self.stack.push(result);
        Ok(())
    }
}
