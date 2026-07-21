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
    if manifest.functions.len() != module.functions.len() {
        return Err(HybridError::Mismatch(format!(
            "the manifest carries {} functions and the bytecode half carries {}",
            manifest.functions.len(),
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
        if function.params.len() != usize::from(proto.param_count) {
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

/// Rejects a parameter mode no code path implements.
///
/// v0's IR carries no per-parameter mode — there is no borrow syntax yet — so
/// the codegen frees every `String` parameter at return, unconditionally.
/// Honouring a borrowed `String` parameter would mean the callee *not* freeing
/// it, which nothing emits: accepting one here would be a double free at the
/// first crossing. Reject it instead of implementing half of it.
///
/// Ownership is immaterial for `Int`/`Float`/`Bool`, which are `Copy` and have
/// nothing to free, so those pass in any mode.
fn ownership(function: &HybridFunction) -> Result<(), HybridError> {
    for (index, param) in function.params.iter().enumerate() {
        if param.ty == BridgeValueTag::STRING && param.ownership != Ownership::Owned {
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
        }
    }

    fn module() -> Module {
        Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            functions: vec![
                FuncProto {
                    name: "main".to_owned(),
                    param_count: 0,
                    local_count: 0,
                    execution: Execution::Runtime,
                    code: vec![Instruction::ReturnVoid],
                },
                FuncProto {
                    name: "hot".to_owned(),
                    param_count: 1,
                    local_count: 1,
                    execution: Execution::Native,
                    code: Vec::new(),
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

    /// The mode nothing implements must be refused at load, where it is a
    /// readable error, rather than at the first crossing, where it is a double
    /// free.
    #[test]
    fn a_borrowed_string_parameter_is_rejected() {
        let mut manifest = manifest();
        manifest.functions[1].params[0].ownership = Ownership::Borrow;
        let error = bundle(&manifest, &module()).expect_err("borrowed strings are not implemented");
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
}
