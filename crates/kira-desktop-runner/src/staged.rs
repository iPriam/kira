//! What the runner currently holds, and how far it has got with it.
//!
//! A state machine rather than a pile of `Option`s, so that "linked" is a state
//! the type system knows about: `start` cannot run a bundle that only loaded,
//! because there is no variant for it to match. The states are what the three
//! host steps produce, and the distinction between loaded and linked is the one
//! a live session reports on.

use std::fmt;
use std::path::PathBuf;

use kira_bytecode::Module;
use kira_vm_runtime::Program;

/// What the runner has staged.
pub enum Staged {
    /// Nothing loaded yet.
    Empty,
    /// A VM bytecode entry, decoded but not yet validated.
    VmLoaded {
        /// The decoded entry module.
        module: Module,
    },
    /// A VM bytecode entry, validated and ready to run.
    VmLinked {
        /// The validated program.
        program: Box<Program>,
    },
    /// A hybrid entry, staged on disk but not yet loaded.
    HybridLoaded {
        /// The staged manifest's path.
        manifest: PathBuf,
    },
    /// A hybrid entry whose native half is loaded and bound.
    ///
    /// The session owns the `dlopen`ed library. Dropping this unloads it, which
    /// is why a hot patch builds its replacement before assigning over it.
    HybridLinked {
        /// The live hybrid session.
        session: Box<kira_hybrid_runtime::Session>,
    },
}

impl Staged {
    /// The state's name, for diagnostics.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::VmLoaded { .. } => "vm-loaded",
            Self::VmLinked { .. } => "vm-linked",
            Self::HybridLoaded { .. } => "hybrid-loaded",
            Self::HybridLinked { .. } => "hybrid-linked",
        }
    }

    /// Whether something is linked and could be running.
    ///
    /// This is what a hot patch requires: a swap replaces something live, and a
    /// merely-loaded bundle has nothing mapped that a swap could preserve.
    pub fn is_linked(&self) -> bool {
        matches!(self, Self::VmLinked { .. } | Self::HybridLinked { .. })
    }
}

/// Written by hand because neither a validated `Program` nor a live `Session` is
/// `Debug`, and neither would be legible dumped anyway: the state's name is the
/// part worth printing.
impl fmt::Debug for Staged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_have_labels() {
        assert_eq!(Staged::Empty.label(), "empty");
        assert_eq!(
            Staged::HybridLoaded {
                manifest: PathBuf::from("app.khm")
            }
            .label(),
            "hybrid-loaded"
        );
    }

    /// Only a linked state is one a swap can replace. If this ever said
    /// otherwise, a hot patch could "preserve" a process that had nothing
    /// running in it.
    #[test]
    fn only_linked_states_are_linked() {
        assert!(!Staged::Empty.is_linked());
        assert!(
            !Staged::HybridLoaded {
                manifest: PathBuf::from("app.khm")
            }
            .is_linked()
        );
    }

    #[test]
    fn a_state_renders_as_its_name() {
        assert_eq!(format!("{:?}", Staged::Empty), "empty");
    }
}
