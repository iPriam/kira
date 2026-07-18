//! What can go wrong embedding a Kira library, and how each one is named.
//!
//! Every failure here is typed and names the thing that disagreed. That is not
//! politeness: an embedder holds an artifact it did not compile, and "the call
//! failed" gives it nowhere to look. A wrapper generated against one build of a
//! library and handed another must be able to say *which export* moved.

use kira_bytecode::exports::{ExportTable, ExportType};
use kira_bytecode::module::ModuleDecodeError;
use kira_bytecode::validate::ModuleValidateError;
use kira_vm_runtime::VmError;

/// A failure loading, checking, or calling an embedded Kira library.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    /// The bytes did not decode as a Kira module.
    #[error("this artifact is not a readable Kira module: {0}")]
    Decode(#[from] ModuleDecodeError),
    /// The module decoded but is not structurally well formed.
    #[error("this library module is not well formed: {0}")]
    Invalid(#[from] ModuleValidateError),
    /// The library does not offer the surface the wrapper was generated from.
    #[error("this library does not match the wrapper built for it: {0}")]
    Contract(#[from] ContractError),
    /// The caller named an export this library does not have.
    #[error("this library exports nothing named `{name}`")]
    UnknownExport {
        /// The consumer-facing name that resolved to nothing.
        name: String,
    },
    /// The caller passed a different number of arguments than the export takes.
    ///
    /// Distinct from the VM's own arity check, and reported first: the VM counts
    /// a function's parameter slots, this counts an export's declared
    /// parameters, and the caller was reading the latter.
    #[error("export `{export}` takes {expected} arguments, but the caller passed {found}")]
    ArgumentCount {
        /// The export that was called.
        export: String,
        /// How many parameters it declares.
        expected: usize,
        /// How many the caller passed.
        found: usize,
    },
    /// An argument's type is not the one the export's signature declares.
    ///
    /// Checked here rather than left to trap, because a trap says only that
    /// something went wrong inside — and the mistake is entirely outside.
    #[error(
        "export `{export}` takes {expected} at position {position}, but the caller passed {found}"
    )]
    ArgumentType {
        /// The export that was called.
        export: String,
        /// Which argument disagreed, counting from zero.
        position: usize,
        /// The kind the signature declares.
        expected: &'static str,
        /// The kind the caller actually passed.
        found: &'static str,
    },
    /// The call ran and the VM refused it or the program trapped.
    #[error("{0}")]
    Vm(#[from] VmError),
}

/// The specific way a library and the wrapper built for it disagree.
///
/// Split out from [`Error`] because these are all one situation — a stale or
/// mismatched build — and a consumer that wants to say "regenerate the wrapper"
/// should be able to match one arm rather than seven.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    /// The library exports a different number of classes than the wrapper knows.
    #[error("it has {found} exported classes, but the wrapper was built for {expected}")]
    ClassCount {
        /// How many classes the wrapper knows.
        expected: usize,
        /// How many the library actually has.
        found: usize,
    },
    /// An exported class sits at a different position than the wrapper expects.
    ///
    /// Position matters because a handle's type is an index into this list: two
    /// classes swapping places would let a `Window` be passed where a `Button`
    /// was wanted, with every name still spelled correctly.
    #[error("its exported class {position} is `{found}`, but the wrapper knows it as `{expected}`")]
    ClassName {
        /// The index into the class list.
        position: usize,
        /// The name the wrapper knows.
        expected: String,
        /// The name the library actually has.
        found: String,
    },
    /// The library does not export something the wrapper calls.
    #[error("it does not export `{name}`")]
    MissingExport {
        /// The consumer-facing name the wrapper was built to call.
        name: String,
    },
    /// An export takes a different number of parameters than the wrapper passes.
    #[error("its `{export}` takes {found} parameters, but the wrapper passes {expected}")]
    Arity {
        /// The export that disagreed.
        export: String,
        /// How many the wrapper passes.
        expected: usize,
        /// How many the library declares.
        found: usize,
    },
    /// An export's parameter has a different type than the wrapper passes.
    #[error(
        "its `{export}` takes {found} at position {position}, but the wrapper passes {expected}"
    )]
    ParamType {
        /// The export that disagreed.
        export: String,
        /// Which parameter, counting from zero.
        position: usize,
        /// The type the wrapper passes.
        expected: String,
        /// The type the library declares.
        found: String,
    },
    /// An export returns a different type than the wrapper expects.
    #[error("its `{export}` returns {found}, but the wrapper expects {expected}")]
    ResultType {
        /// The export that disagreed.
        export: String,
        /// The type the wrapper expects.
        expected: String,
        /// The type the library returns.
        found: String,
    },
    /// The library's bytes are not the bytes the wrapper was generated from.
    ///
    /// Reported last, because it is true of every mismatch above and says the
    /// least about which one: a structural disagreement names the export that
    /// moved, and this one only names that *something* did.
    #[error("its bytes hash to {found:#018x}, but the wrapper was generated from {expected:#018x}")]
    ContentHash {
        /// The hash the wrapper recorded at generation time.
        expected: u64,
        /// The hash of the bytes actually loaded.
        found: u64,
    },
}

/// A one-word name for the kind a value crosses the boundary as.
///
/// Deliberately *not* the class of a handle: this describes what a
/// [`NativeArg`](kira_runtime_abi::NativeArg) carries, and a handle argument is
/// one word whose class the seam cannot see. Class typing is the generated
/// newtypes' job, one layer up.
pub(crate) fn describe_kind(ty: ExportType) -> &'static str {
    match ty {
        ExportType::Void => "nothing",
        ExportType::Int => "an integer",
        ExportType::Float => "a float",
        ExportType::Bool => "a boolean",
        ExportType::String => "a string",
        ExportType::Handle { .. } => "a handle",
    }
}

/// A name for a crossing type that resolves a handle's class through `classes`.
///
/// Used only where the class list is known to agree — the contract check
/// compares classes before signatures for exactly this reason, so by the time a
/// handle's class index is compared, the index means the same thing on both
/// sides and naming it is honest.
pub(crate) fn describe_type(ty: ExportType, classes: &[String]) -> String {
    match ty {
        ExportType::Handle { class } => match classes.get(class as usize) {
            Some(name) => format!("a handle to `{name}`"),
            // Unreachable through `Library`, whose module decoded with every
            // handle index in range. Named rather than unwrapped, because a
            // library never gets to end its caller's process over a case it
            // believes impossible.
            None => format!("a handle to unknown class {class}"),
        },
        other => describe_kind(other).to_owned(),
    }
}

/// The names of the classes a table exports, for [`describe_type`].
pub(crate) fn class_names(table: &ExportTable) -> &[String] {
    &table.classes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_is_described_by_the_class_it_denotes() {
        let classes = vec!["Button".to_owned(), "Window".to_owned()];
        assert_eq!(
            describe_type(ExportType::Handle { class: 1 }, &classes),
            "a handle to `Window`"
        );
        assert_eq!(describe_type(ExportType::Int, &classes), "an integer");
    }

    #[test]
    fn a_handle_out_of_range_is_named_rather_than_panicked_on() {
        assert_eq!(
            describe_type(ExportType::Handle { class: 9 }, &[]),
            "a handle to unknown class 9"
        );
    }

    #[test]
    fn a_contract_failure_says_which_export_moved() {
        let error = Error::Contract(ContractError::Arity {
            export: "make_button".to_owned(),
            expected: 1,
            found: 2,
        });
        assert_eq!(
            error.to_string(),
            "this library does not match the wrapper built for it: its `make_button` takes 2 \
             parameters, but the wrapper passes 1"
        );
    }
}
