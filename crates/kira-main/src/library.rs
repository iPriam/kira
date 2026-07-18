//! A loaded Kira library, and the check that it is the one you were promised.
//!
//! # Why a wrapper checks its own library
//!
//! The native engine guards a stale build with a symbol: the generated crate
//! calls `kira_lib_<lib>_abi_1`, and an archive from another contract fails the
//! link by name (see [`crate::abi`]). The VM engine has no link step to fail —
//! the `.kbc` is `include_bytes!`d data — so **data is the only guard
//! available**, and [`Library::verify`] is it.
//!
//! What it compares is the surface a consumer can actually observe: the
//! exported class list, and each export's consumer-facing name, parameter types
//! and result type. Function ids and Kira-side spellings are deliberately not
//! compared, because they are internals a library may renumber freely without
//! any wrapper being wrong.
//!
//! # Why the order of the checks is part of the design
//!
//! Classes first, then signatures, then the content hash. Classes lead because a
//! handle's type is an *index* into the class list, so comparing a signature
//! before the list agrees would compare two indices that mean different things.
//! The hash trails because it is true of every mismatch above it and says the
//! least about which one: a structural check names the export that moved, the
//! hash only says something did.

use kira_bytecode::exports::{ExportTable, ExportType, ModuleExport};
use kira_bytecode::module::Module;
use kira_runtime_abi::HostCapabilities;
use kira_vm_runtime::{Instance as VmInstance, Program};

use crate::error::{ContractError, Error, class_names, describe_type};
use crate::host::StdoutHost;
use crate::instance::Instance;

/// The export surface a generated wrapper was built against.
///
/// Borrowed rather than owned because the generator writes one as a `const`:
/// this is seam vocabulary a wrapper spells out in its own source, not a model
/// type anything stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportContract<'a> {
    /// Every exported class, in the order handle types index them.
    pub classes: &'a [&'a str],
    /// Every export the wrapper offers a method for.
    ///
    /// A library exporting *more* than this is not an error: the wrapper calls
    /// by name and simply has no method for the extra one. It is the hash that
    /// notices, and it says so as a stale build rather than as a mismatch.
    pub functions: &'a [ExpectedExport<'a>],
    /// [`content_hash`] of the bytes the wrapper was generated from.
    pub content_hash: u64,
}

/// One export a generated wrapper calls, as the wrapper understands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedExport<'a> {
    /// The consumer-facing name (`make_button`).
    pub name: &'a str,
    /// The parameter types, in order.
    pub params: &'a [ExportType],
    /// The result type.
    pub result: ExportType,
}

/// Hashes the bytes a library artifact is made of.
///
/// FNV-1a, 64-bit. A **change detector, not a security primitive**: it answers
/// "are these the bytes the wrapper was generated from", where the adversary is
/// a stale build directory rather than a person. Written out here rather than
/// pulled in as a dependency because that is the whole requirement, and because
/// every consumer of a generated wrapper would inherit the dependency.
pub fn content_hash(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// A Kira library, decoded and proven well formed, ready to be instantiated.
///
/// Holds no heap and runs nothing: a library is the *program*, and an
/// [`Instance`] is a running copy of it with a heap that outlives one call. One
/// library may be instantiated any number of times, and the instances share
/// nothing.
#[derive(Debug, Clone)]
pub struct Library {
    module: Module,
    content_hash: u64,
}

impl Library {
    /// Decodes and validates a `.kbc` artifact.
    ///
    /// Validation happens here rather than at first call so that a broken
    /// artifact is refused by the embedder's `load()`, where there is still
    /// something useful to say about it.
    pub fn from_bytes(bytes: &[u8]) -> Result<Library, Error> {
        let module = Module::from_bytes(bytes)?;
        module.validate()?;
        Ok(Library {
            module,
            content_hash: content_hash(bytes),
        })
    }

    /// The `@Export` surface this library offers.
    pub fn exports(&self) -> &ExportTable {
        &self.module.exports
    }

    /// The hash of the bytes this library was decoded from.
    pub fn content_hash(&self) -> u64 {
        self.content_hash
    }

    /// The compiled module, for an embedder that needs to look deeper.
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Looks up one export by its consumer-facing name.
    pub fn export(&self, name: &str) -> Option<&ModuleExport> {
        self.module
            .exports
            .functions
            .iter()
            .find(|export| export.name == name)
    }

    /// Checks that this library is the one `contract` was generated from.
    ///
    /// Returns the **first** disagreement, in the order the module docs give,
    /// because that is the one the reader has to fix and a list of consequences
    /// after it is noise.
    pub fn verify(&self, contract: &ExportContract<'_>) -> Result<(), Error> {
        let table = &self.module.exports;
        self.verify_classes(contract, table)?;
        for expected in contract.functions {
            self.verify_export(expected, table)?;
        }
        if contract.content_hash != self.content_hash {
            return Err(ContractError::ContentHash {
                expected: contract.content_hash,
                found: self.content_hash,
            }
            .into());
        }
        Ok(())
    }

    /// Compares the exported class list, which every handle type indexes.
    fn verify_classes(
        &self,
        contract: &ExportContract<'_>,
        table: &ExportTable,
    ) -> Result<(), Error> {
        if contract.classes.len() != table.classes.len() {
            return Err(ContractError::ClassCount {
                expected: contract.classes.len(),
                found: table.classes.len(),
            }
            .into());
        }
        for (position, (expected, found)) in contract.classes.iter().zip(&table.classes).enumerate()
        {
            if expected != found {
                return Err(ContractError::ClassName {
                    position,
                    expected: (*expected).to_owned(),
                    found: found.clone(),
                }
                .into());
            }
        }
        Ok(())
    }

    /// Compares one export's name, arity, parameter types, and result type.
    fn verify_export(
        &self,
        expected: &ExpectedExport<'_>,
        table: &ExportTable,
    ) -> Result<(), Error> {
        let Some(found) = self.export(expected.name) else {
            return Err(ContractError::MissingExport {
                name: expected.name.to_owned(),
            }
            .into());
        };
        if expected.params.len() != found.params.len() {
            return Err(ContractError::Arity {
                export: expected.name.to_owned(),
                expected: expected.params.len(),
                found: found.params.len(),
            }
            .into());
        }
        let classes = class_names(table);
        for (position, (want, have)) in expected.params.iter().zip(&found.params).enumerate() {
            if want != have {
                return Err(ContractError::ParamType {
                    export: expected.name.to_owned(),
                    position,
                    expected: describe_type(*want, classes),
                    found: describe_type(*have, classes),
                }
                .into());
            }
        }
        if expected.result != found.result {
            return Err(ContractError::ResultType {
                export: expected.name.to_owned(),
                expected: describe_type(expected.result, classes),
                found: describe_type(found.result, classes),
            }
            .into());
        }
        Ok(())
    }

    /// Instantiates the library with the default [`StdoutHost`].
    pub fn instantiate(&self) -> Result<Instance<StdoutHost>, Error> {
        self.instantiate_with(StdoutHost)
    }

    /// Instantiates the library with a host the embedder supplies.
    ///
    /// The instance owns the host for its whole life, so a host that accumulates
    /// (a capture buffer, a log) is readable afterwards through
    /// [`Instance::host`] and recovered by [`Instance::into_host`].
    pub fn instantiate_with<H: HostCapabilities>(&self, host: H) -> Result<Instance<H>, Error> {
        let program = Program::load(self.module.clone())?;
        Ok(Instance::new(VmInstance::new(program), host))
    }
}

#[cfg(test)]
#[path = "library_tests.rs"]
mod tests;
