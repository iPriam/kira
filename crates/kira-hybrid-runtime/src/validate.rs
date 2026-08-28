//! Cross-checking the two halves of a hybrid bundle against each other.
//!
//! One build writes the `.khm` and the `.kbc`, and they describe one program.
//! Nothing stops a stale one sitting beside a fresh one, though, and the two
//! disagreeing is not a condition the seam survives: the manifest is what the
//! host marshals against, and the module is what the VM runs. If they disagree
//! about a signature, every crossing marshals to the wrong shape — into machine
//! code that reads its arguments by position and cannot check.
//!
//! So the bundle is proven consistent once, at load, and never re-checked.

use kira_bytecode::module::Module;
use kira_hybrid_definition::{HybridFunction, HybridManifest};
use kira_runtime_abi::{BridgeValueTag, Execution, Ownership};

use crate::error::HybridError;

/// Names an entrypoint slot for a mismatch message.
///
/// A library's absent entrypoint reads as a phrase rather than a missing
/// number, so the two halves disagreeing about being a library says so.
fn describe_entry(entry: Option<u32>) -> String {
    match entry {
        Some(index) => format!("function {index}"),
        None => "absent (a library)".to_owned(),
    }
}

/// Proves `manifest` and `module` describe the same program.
pub fn bundle(manifest: &HybridManifest, module: &Module) -> Result<(), HybridError> {
    // The bytecode half may carry helpers of its own after the program's
    // functions — the widen rebuilds — so the manifest describes a *prefix*.
    // The count is still exact: the helpers are declared in the manifest, so a
    // stale half carrying a function neither side described is still caught.
    let described = manifest.functions.len() + manifest.internal_functions as usize;
    if described != module.functions.len() {
        return Err(HybridError::Mismatch(format!(
            "the manifest describes {described} functions ({} of them internal to the \
             bytecode half) and the bytecode half carries {}",
            manifest.internal_functions,
            module.functions.len(),
        )));
    }
    if manifest.entry != module.main {
        return Err(HybridError::Mismatch(format!(
            "the manifest's entrypoint is {} and the bytecode half's is {}",
            describe_entry(manifest.entry),
            describe_entry(module.main),
        )));
    }

    for (index, function) in manifest.functions.iter().enumerate() {
        // An id is one index into both halves; anything else makes every
        // `CallNative(id)` the VM reaches a lookup into the wrong row.
        if function.id as usize != index {
            return Err(HybridError::Mismatch(format!(
                "manifest function `{}` sits at index {index} but claims id {}",
                function.name, function.id,
            )));
        }
        let proto = &module.functions[index];
        if function.execution != proto.execution {
            return Err(HybridError::Mismatch(format!(
                "function `{}` runs on {:?} in the manifest and {:?} in the bytecode half",
                function.name, function.execution, proto.execution,
            )));
        }
        if function.params.len() as u64 != proto.param_count {
            return Err(HybridError::Mismatch(format!(
                "function `{}` takes {} parameters in the manifest and {} in the bytecode half",
                function.name,
                function.params.len(),
                proto.param_count,
            )));
        }
        ownership(function)?;
    }

    foreign(manifest, module)?;
    entry(manifest)
}

/// Validates a replacement bytecode module for an all-runtime live session and
/// returns the old native-to-runtime function-id map.
///
/// The native library is deliberately not replaced during a VM hot patch. Its
/// generated callback thunks still carry the old function ids, while the new
/// bytecode is allowed to add, remove, or reorder runtime-only functions. The
/// crossing surface must remain identical; the implementation table itself
/// does not have to remain positional.
pub fn hot_reload(
    manifest: &HybridManifest,
    previous: &Module,
    next: &Module,
) -> Result<Vec<u32>, HybridError> {
    if manifest
        .functions
        .iter()
        .any(|function| function.execution != Execution::Runtime)
    {
        return Err(HybridError::Mismatch(
            "a VM hot reload was requested for a mixed execution manifest".to_owned(),
        ));
    }
    if previous.foreign_imports != next.foreign_imports
        || previous.foreign_aggregates != next.foreign_aggregates
    {
        return Err(HybridError::Mismatch(
            "the native library's foreign crossing surface changed; relaunch is required"
                .to_owned(),
        ));
    }
    callback_surface(previous, next)?;
    foreign(manifest, next)?;
    if next
        .functions
        .iter()
        .any(|function| function.execution != Execution::Runtime)
    {
        return Err(HybridError::Mismatch(
            "the replacement module contains a native function body".to_owned(),
        ));
    }

    let mut remap = vec![u32::MAX; manifest.functions.len()];
    let mut used = std::collections::HashSet::new();
    for function in &manifest.functions {
        let candidates: Vec<u32> = next
            .functions
            .iter()
            .enumerate()
            .filter(|(_, prototype)| {
                prototype.name == function.name
                    && prototype.param_count == function.params.len() as u64
            })
            .map(|(index, _)| index as u32)
            .collect();
        let Some(&replacement) = candidates.first() else {
            return Err(HybridError::Mismatch(format!(
                "the replacement module has no runtime function `{}` with {} parameter(s)",
                function.name,
                function.params.len()
            )));
        };
        if candidates.len() != 1 || !used.insert(replacement) {
            return Err(HybridError::Mismatch(format!(
                "the replacement module has an ambiguous runtime identity for `{}`",
                function.name
            )));
        }
        remap[function.id as usize] = replacement;
    }

    if let Some(entry) = manifest.entry {
        let replacement = remap
            .get(entry as usize)
            .copied()
            .filter(|&id| id != u32::MAX);
        if next.main != replacement {
            return Err(HybridError::Mismatch(
                "the replacement module's entrypoint no longer matches the live app".to_owned(),
            ));
        }
    } else if next.main.is_some() {
        return Err(HybridError::Mismatch(
            "the replacement module gained an entrypoint".to_owned(),
        ));
    }

    Ok(remap)
}

/// Proves that callback slots still mean the same C-to-Kira calls.
///
/// A callback row's function id belongs to the bytecode function table, so it
/// may change when runtime-only functions are reordered. The callback slot and
/// its C signature cannot change: the native thunk is already compiled for
/// that position. Compare the target's stable source identity instead of the
/// positional id, then [`hot_reload`] can use its id map when the old thunk
/// enters the new module.
fn callback_surface(previous: &Module, next: &Module) -> Result<(), HybridError> {
    if previous.foreign_callbacks.len() != next.foreign_callbacks.len() {
        return Err(HybridError::Mismatch(
            "the native library's foreign callback surface changed; relaunch is required"
                .to_owned(),
        ));
    }
    for (index, (old, new)) in previous
        .foreign_callbacks
        .iter()
        .zip(next.foreign_callbacks.iter())
        .enumerate()
    {
        if old.signature() != new.signature() {
            return Err(HybridError::Mismatch(format!(
                "foreign callback {index}'s C signature changed; relaunch is required"
            )));
        }
        let old_target = previous
            .functions
            .get(old.function() as usize)
            .map(|function| (&function.name, function.param_count));
        let new_target = next
            .functions
            .get(new.function() as usize)
            .map(|function| (&function.name, function.param_count));
        if old_target != new_target {
            return Err(HybridError::Mismatch(format!(
                "foreign callback {index}'s Kira target changed; relaunch is required"
            )));
        }
    }
    Ok(())
}

/// Proves the manifest's foreign table matches the bytecode half's.
///
/// A `CallForeign(id)` in the bytecode indexes both the module's own import
/// table and the manifest's; the host binds the adapter and marshals against the
/// manifest row, so a drift between the two would call an adapter with the wrong
/// argument shape. One build writes both, and this catches a stale pairing at
/// load rather than at the first foreign call.
fn foreign(manifest: &HybridManifest, module: &Module) -> Result<(), HybridError> {
    if manifest.foreign.len() != module.foreign_imports.len() {
        return Err(HybridError::Mismatch(format!(
            "the manifest carries {} foreign imports and the bytecode half carries {}",
            manifest.foreign.len(),
            module.foreign_imports.len(),
        )));
    }
    for (index, row) in manifest.foreign.iter().enumerate() {
        let import = &module.foreign_imports[index];
        if row.symbol != import.symbol()
            || row.library != import.library()
            || row.abi != import.abi()
            || &row.signature != import.signature()
        {
            return Err(HybridError::Mismatch(format!(
                "foreign import {index} (`{}`) has a different library, symbol, or signature in \
                 the manifest than in the bytecode half",
                row.symbol,
            )));
        }
    }
    Ok(())
}

/// Rejects a manifest mode outside the hybrid crossing contract.
///
/// The build writes a read-only borrow as an owned crossing copy: the native
/// half receives a fresh handle and releases it at return. A stale or manually
/// edited manifest that says `Borrow` for a `String` asks for a non-owning path
/// the trampoline does not emit, so accepting it would be a double free at the
/// first crossing.
///
/// `BorrowMut` is a different case and is implemented: the release plan already
/// skips a written-through parameter, because within one engine it is a pointer
/// into the caller's storage and never the callee's to free. Across the seam it
/// is a copy whose final value goes back in the slot it arrived in, and the
/// side that reads it there frees it — exactly once, like any other value that
/// crosses.
///
/// Ownership is immaterial for `Int`/`Float`/`Bool`, which are `Copy` and have
/// nothing to free, so those pass in any mode.
fn ownership(function: &HybridFunction) -> Result<(), HybridError> {
    for (index, param) in function.params.iter().enumerate() {
        if param.ty == BridgeValueTag::STRING && param.ownership == Ownership::Borrow {
            return Err(HybridError::UnsupportedOwnership {
                function: function.name.clone(),
                index,
                ownership: param.ownership,
            });
        }
    }
    Ok(())
}

/// Proves the entrypoint is one this host can start.
///
/// A library has none to check. That is not a defect in the bundle: the two
/// halves already agreed about it above, and `run` is what refuses to start it.
fn entry(manifest: &HybridManifest) -> Result<(), HybridError> {
    let Some(entry) = manifest.entry_function() else {
        return Ok(());
    };
    if !entry.params.is_empty() {
        return Err(HybridError::Mismatch(format!(
            "the entrypoint `{}` takes {} parameters; an entrypoint takes none",
            entry.name,
            entry.params.len(),
        )));
    }
    // `Inherited` is a source-level "whatever my caller runs on". An entrypoint
    // has no caller, and a manifest records where a function *runs*, so one
    // reaching here means the build never resolved it.
    if entry.execution == Execution::Inherited {
        return Err(HybridError::Mismatch(format!(
            "the entrypoint `{}` records no engine; the build left it unresolved",
            entry.name,
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_bytecode::module::FuncProto;
    use kira_bytecode::op::Instruction;
    use kira_hybrid_definition::HybridParam;

    fn manifest() -> HybridManifest {
        HybridManifest {
            module_name: "demo".to_owned(),
            bytecode_path: "demo.kbc".to_owned(),
            native_library_path: "libdemo.dylib".to_owned(),
            entry: Some(0),
            functions: vec![
                HybridFunction {
                    id: 0,
                    name: "main".to_owned(),
                    execution: Execution::Runtime,
                    params: Vec::new(),
                    returns: BridgeValueTag::VOID,
                    exported_name: None,
                },
                HybridFunction {
                    id: 1,
                    name: "hot".to_owned(),
                    execution: Execution::Native,
                    params: vec![HybridParam::owned(BridgeValueTag::STRING)],
                    returns: BridgeValueTag::INT,
                    exported_name: Some("kira_native_fn_1".to_owned()),
                },
            ],
            foreign: Vec::new(),
            foreign_aggregates: Default::default(),
            internal_functions: 0,
        }
    }

    fn module() -> Module {
        Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            constants: Vec::new(),
            functions: vec![
                FuncProto {
                    name: "main".to_owned(),
                    param_count: 0,
                    local_count: 0,
                    execution: Execution::Runtime,
                    code: vec![Instruction::ReturnVoid],
                    releases: kira_bytecode::FrameRelease::EveryLocal,
                },
                FuncProto {
                    name: "hot".to_owned(),
                    param_count: 1,
                    local_count: 1,
                    execution: Execution::Native,
                    code: Vec::new(),
                    releases: kira_bytecode::FrameRelease::EveryLocal,
                },
            ],
            main: Some(0),
            strings: Vec::new(),
        }
    }

    #[test]
    fn a_matching_bundle_passes() {
        bundle(&manifest(), &module()).expect("the two halves agree");
    }

    #[test]
    fn a_disagreeing_arity_is_rejected() {
        let mut module = module();
        module.functions[1].param_count = 2;
        let error = bundle(&manifest(), &module).expect_err("arities disagree");
        assert!(matches!(error, HybridError::Mismatch(_)), "{error:?}");
    }

    #[test]
    fn a_disagreeing_engine_is_rejected() {
        let mut module = module();
        module.functions[1].execution = Execution::Runtime;
        let error = bundle(&manifest(), &module).expect_err("engines disagree");
        assert!(matches!(error, HybridError::Mismatch(_)), "{error:?}");
    }

    #[test]
    fn a_stale_half_with_fewer_functions_is_rejected() {
        let mut module = module();
        module.functions.pop();
        let error = bundle(&manifest(), &module).expect_err("counts disagree");
        assert!(matches!(error, HybridError::Mismatch(_)), "{error:?}");
    }

    /// A stale manifest must be refused at load, where it is a readable error,
    /// rather than at the first crossing, where it could double free.
    #[test]
    fn a_borrowed_string_parameter_is_rejected() {
        let mut manifest = manifest();
        manifest.functions[1].params[0].ownership = Ownership::Borrow;
        let error = bundle(&manifest, &module()).expect_err("a stale ownership mode");
        assert!(
            matches!(
                error,
                HybridError::UnsupportedOwnership {
                    index: 0,
                    ownership: Ownership::Borrow,
                    ..
                }
            ),
            "{error:?}",
        );
    }

    /// The written-through mode *is* implemented, and refusing it would refuse
    /// every app whose native half writes through a parameter.
    #[test]
    fn a_written_through_string_parameter_is_accepted() {
        let mut manifest = manifest();
        manifest.functions[1].params[0].ownership = Ownership::BorrowMut;
        bundle(&manifest, &module()).expect("a borrow mut crosses as a copy each way");
    }

    /// A borrow of a `Copy` type frees nothing either way, so it is not the
    /// case the rejection above is guarding against.
    #[test]
    fn a_borrowed_int_parameter_is_accepted() {
        let mut manifest = manifest();
        manifest.functions[1].params[0] = HybridParam {
            ty: BridgeValueTag::INT,
            ownership: Ownership::Borrow,
        };
        bundle(&manifest, &module()).expect("an int borrow frees nothing");
    }

    #[test]
    fn an_entrypoint_taking_parameters_is_rejected() {
        let mut manifest = manifest();
        manifest.functions[0].params = vec![HybridParam::owned(BridgeValueTag::INT)];
        let mut module = module();
        module.functions[0].param_count = 1;
        module.functions[0].local_count = 1;
        let error = bundle(&manifest, &module).expect_err("an entrypoint takes no parameters");
        assert!(matches!(error, HybridError::Mismatch(_)), "{error:?}");
    }

    fn runtime_manifest() -> HybridManifest {
        let mut manifest = manifest();
        for function in &mut manifest.functions {
            function.execution = Execution::Runtime;
            function.exported_name = None;
        }
        manifest
    }

    fn runtime_module(order: &[&str], callback_function: Option<u32>) -> Module {
        let callbacks = callback_function
            .map(|function| {
                vec![kira_runtime_abi::ForeignCallback::new(
                    function,
                    kira_runtime_abi::ForeignSignature::scalars(
                        [kira_runtime_abi::ForeignType::I32],
                        kira_runtime_abi::ForeignType::I32,
                    ),
                )]
            })
            .unwrap_or_default();
        Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: callbacks,
            constants: Vec::new(),
            functions: order
                .iter()
                .map(|name| FuncProto {
                    name: (*name).to_owned(),
                    param_count: if *name == "hot" { 1 } else { 0 },
                    local_count: if *name == "hot" { 1 } else { 0 },
                    execution: Execution::Runtime,
                    code: vec![Instruction::ReturnVoid],
                    releases: kira_bytecode::FrameRelease::EveryLocal,
                })
                .collect(),
            main: Some(
                order
                    .iter()
                    .position(|name| *name == "main")
                    .expect("the runtime fixture has an entrypoint") as u32,
            ),
            strings: Vec::new(),
        }
    }

    #[test]
    fn a_hot_reload_maps_old_runtime_ids_after_reordering() {
        let manifest = runtime_manifest();
        let previous = runtime_module(&["main", "hot"], None);
        let next = runtime_module(&["hot", "main"], None);
        assert_eq!(
            hot_reload(&manifest, &previous, &next).expect("the runtime identities still match"),
            vec![1, 0]
        );
    }

    #[test]
    fn a_hot_reload_maps_a_callback_target_after_reordering() {
        let manifest = runtime_manifest();
        let previous = runtime_module(&["main", "hot"], Some(1));
        let next = runtime_module(&["hot", "main"], Some(0));
        assert_eq!(
            hot_reload(&manifest, &previous, &next).expect("the callback still means `hot`"),
            vec![1, 0]
        );
    }

    #[test]
    fn a_hot_reload_rejects_a_changed_callback_target() {
        let manifest = runtime_manifest();
        let previous = runtime_module(&["main", "hot"], Some(1));
        let next = runtime_module(&["hot", "main"], Some(1));
        let error = hot_reload(&manifest, &previous, &next)
            .expect_err("the old callback would enter `hot`, not `main`");
        assert!(
            error
                .to_string()
                .contains("callback 0's Kira target changed"),
            "{error}"
        );
    }
}
