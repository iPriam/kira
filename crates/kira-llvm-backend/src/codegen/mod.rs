//! Lowering a verified [`IrProgram`] into an LLVM module, and emitting it as a
//! native object.
//!
//! This module owns the LLVM objects (context, module, builder) and the
//! program-wide scaffold: one declaration per Kira function, and the plan for
//! which engine owns which body. The pieces around it each own one job:
//!
//! - [`types`] — the LLVM types Kira's values map onto, and the `kira_rt_*`
//!   declarations,
//! - [`lower`] — statement and expression lowering, which must agree with the
//!   interpreter instruction for instruction,
//! - [`entry`] — how the module is entered: a C `main`, or host trampolines,
//! - [`bridge`] — packing and unpacking values that cross between engines,
//! - [`symbols`] — the native symbol each function is emitted under,
//! - [`target`] — the target machine, host or cross, and object emission,
//! - [`ffi`] — glue for the LLVM C API's strings.
//!
//! Every LLVM object here is a raw pointer from the C API, so the whole module
//! is one `unsafe` fence: [`Module`] owns its context and disposes of it on
//! drop, and no LLVM reference escapes that lifetime.

mod boxing;
mod bridge;
mod callback;
mod constants;
mod debug;
mod elements;
mod entry;
mod ffi;
mod foreign_scalar;
mod glue;
mod library;
mod lower;
mod module;
mod native_ffi;
mod native_state;
mod native_state_enums;
mod native_state_values;
#[cfg(test)]
mod native_state_values_tests;
mod plan;
mod program;
mod symbols;
mod syscall;
mod target;
mod types;
mod values;

use kira_backend_api::NativeTarget;
use kira_ir::IrProgram;
use kira_runtime_abi::{Execution, ForeignPointerWidth};
use kira_semantics_model::Type;
use llvm_sys::prelude::*;
use llvm_sys::target::LLVMTargetDataRef;
use std::collections::{BTreeSet, HashMap};

use self::elements::Leaf;
use self::ffi::c_string;
use self::native_state::StateLeaf;

use self::types::{Runtime, Types};
use crate::exports::NativeExportSurface;

use self::plan::{CodegenTarget, ModuleKind, Plan};

pub(crate) use self::plan::CodegenUnit;
pub(crate) use self::symbols::trampoline_name;
pub(crate) use self::target::check_supported;
pub(crate) use self::types::Callable;

/// The direct LLVM selector is substantially faster for a large executable
/// module. Smaller programs keep the normal codegen level, which is important
/// because it colours stack slots and is what keeps ordinary native programs'
/// frames compact.
const FAST_CODEGEN_REACHABLE_FUNCTIONS: usize = 1_000;

fn needs_fast_codegen(program: &IrProgram, plan: &Plan<'_>) -> bool {
    // Never for a cross build. This is a build-time escape hatch, bought by
    // emitting worse code, and it is worth that on the machine a developer is
    // waiting at. A binary being produced for another machine is one nobody is
    // waiting to run here, and the frames the direct selector leaves behind are
    // the ones that overflowed an 8 MB stack.
    if !matches!(plan.target, CodegenTarget::Native(NativeTarget::Host)) {
        return false;
    }
    if !cfg!(target_os = "windows") || plan.kind != ModuleKind::Executable {
        return false;
    }
    let native_reachable = program
        .functions
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            plan.engines.get(*index) == Some(&Execution::Native)
                && plan.reachable.get(*index).copied().unwrap_or(false)
        })
        .count();
    native_reachable >= FAST_CODEGEN_REACHABLE_FUNCTIONS
}

/// An LLVM module holding a lowered Kira program.
///
/// Owns its LLVM context; dropping it disposes of every LLVM object built from
/// it, which is why no reference into the module outlives this value.
pub(crate) struct Module {
    context: LLVMContextRef,
    module: LLVMModuleRef,
    builder: LLVMBuilderRef,
    fast_codegen: bool,
    /// The machine this module was lowered against, kept so the emission uses
    /// the same one.
    ///
    /// Carried rather than passed in again at emission time: the data layout the
    /// offsets were computed with and the target machine the object comes out of
    /// have to be the same target, and a second argument is a second chance for
    /// them not to be.
    target: CodegenTarget,
}

/// Lowers a program into an owned [`Module`].
pub(crate) struct Codegen<'a> {
    program: &'a IrProgram,
    /// Imports whose library is absent on this target; their adapters return a
    /// status instead of calling a symbol that would not link.
    unavailable: Vec<usize>,
    /// The pointer width of the target this module is emitted for.
    ///
    /// Baked into every C-layout aggregate offset the lowering computes, so it
    /// has to be the *target*'s width, not this machine's: a wasm32 module
    /// built on a 64-bit host lays a pointer member out in four bytes.
    pointer_width: ForeignPointerWidth,
    /// What this module is emitted for.
    ///
    /// A foreign call reaches its C differently on each: a host build calls
    /// through the bundled libffi graph, and a wasm module calls the symbol
    /// directly, because its C is linked into the module and there is no
    /// loader, no second image, and no libffi to reach.
    target: CodegenTarget,
    context: LLVMContextRef,
    module: LLVMModuleRef,
    builder: LLVMBuilderRef,
    types: Types,
    runtime: Runtime,
    /// What this module is being built as.
    kind: ModuleKind,
    /// Which of the program's function bodies this module carries.
    unit: CodegenUnit,
    /// Which program functions can be reached by this module.
    reachable: Vec<bool>,
    /// What this library exports, empty for anything that is not one.
    exports: NativeExportSurface,
    /// Which engine owns each function, in [`IrProgram::functions`] order.
    ///
    /// Resolved: no `Inherited` survives here, because a backend has to know
    /// where every function actually runs.
    engines: Vec<Execution>,
    /// Functions that can execute on a lifecycle fiber's preserved stack.
    lifecycle_functions: Vec<bool>,
    /// The functions that are some type's user `Drop` body.
    ///
    /// This module carries one whatever engine owns it. A release happens
    /// wherever the value dies, and in a hybrid program native code releases
    /// values whose body the bytecode half also holds — so the body is compiled
    /// into both halves rather than reached across the bridge from a release
    /// leaf that has no frame to marshal from.
    drop_glue: BTreeSet<u32>,
    /// One entry per IR function, in [`IrProgram::functions`] order.
    ///
    /// Only functions this module defines have a real entry; a function that
    /// lives in the other half is reached through the bridge instead.
    functions: Vec<Option<Callable>>,
    /// One global per module constant, in [`IrProgram::constants`] order.
    ///
    /// Empty for the hybrid native half, whose constants live on the VM and
    /// are read across the bridge. See [`constants`].
    constant_globals: Vec<LLVMValueRef>,
    /// One compact shared-graph descriptor per foreign import.
    foreign_ffi_descriptors: Vec<LLVMValueRef>,
    /// One compact shared-graph descriptor per foreign callback.
    callback_ffi_descriptors: Vec<LLVMValueRef>,
    /// The LLVM type of each declared struct, indexed by `StructId`.
    ///
    /// A struct lowers to a real LLVM struct with real field layout, not to a
    /// boxed or tagged value. Fields sit where the target's ABI puts them,
    /// which is what a `@FFI.Struct` will need — a box would be the wrong
    /// foundation to put underneath it later.
    struct_types: Vec<LLVMTypeRef>,
    /// Names every emitted string literal global uniquely.
    string_counter: u32,
    /// The host target's data layout, borrowed from the module.
    ///
    /// An array sizes its element by asking LLVM for the type's ABI size on
    /// this target (see [`Codegen::abi_size`]), so the stride matches what a
    /// struct field's offset would be — a guess computed here could disagree.
    /// Borrowed from the module, so it is valid as long as the module is and is
    /// never disposed here.
    target_data: LLVMTargetDataRef,
    /// The clone and free leaves an array's runtime helpers call, one per
    /// `(element type, leaf)`. Memoized so two arrays of the same element share
    /// one leaf. See [`elements`].
    element_leaves: HashMap<(Type, Leaf), LLVMValueRef>,
    /// The slots a copy or drop site hands its value to the leaf in, one pair
    /// per `(function, type)`. See [`glue`].
    scratch_slots: glue::ScratchSlots,
    /// Encode/decode leaves used by generic callback-state array conversion.
    native_state_leaves: HashMap<(Type, StateLeaf), LLVMValueRef>,
    /// Encode/decode helpers for payload-carrying enum callback state.
    native_state_enum_leaves: HashMap<(kira_semantics_model::EnumId, StateLeaf), Callable>,
    /// Optional debug builder for an explicitly requested debug build.
    debug: Option<debug::DebugBuilder>,
}
