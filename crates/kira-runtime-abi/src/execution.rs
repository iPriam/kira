//! Where a function executes, and how a value crosses the runtime/native
//! boundary.
//!
//! `@Runtime` and `@Native` are *execution-boundary* annotations, not
//! restrictions on what code may do: runtime code and native code call each
//! other in both directions, and values pass both ways. What the annotation
//! fixes is which engine owns a function's body — the VM or machine code — and
//! therefore which side of a call has to marshal.
//!
//! This type is defined once, here, because it is vocabulary shared by
//! everything from the parser (which records what was written) to the hybrid
//! runtime (which decides how to dispatch).

/// Where a function's body executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Execution {
    /// No annotation: the function follows the program's default engine.
    ///
    /// This is not a third engine — it is the absence of a choice, resolved
    /// per build (the VM for `kira run`, native for `kira build --backend
    /// llvm`).
    #[default]
    Inherited,
    /// `@Runtime`: the body compiles to bytecode and runs on the VM.
    Runtime,
    /// `@Native`: the body compiles to machine code.
    Native,
}

impl Execution {
    /// The annotation name that selects this engine, if one does.
    pub fn annotation(self) -> Option<&'static str> {
        match self {
            Execution::Inherited => None,
            Execution::Runtime => Some("Runtime"),
            Execution::Native => Some("Native"),
        }
    }

    /// The engine an annotation name selects, if it names one.
    pub fn from_annotation(name: &str) -> Option<Execution> {
        match name {
            "Runtime" => Some(Execution::Runtime),
            "Native" => Some(Execution::Native),
            _ => None,
        }
    }

    /// Resolves `Inherited` against the engine a build defaults to.
    pub fn resolve(self, default: Execution) -> Execution {
        match self {
            Execution::Inherited => default,
            explicit => explicit,
        }
    }

    /// The wire encoding of this engine.
    ///
    /// Hybrid manifests carry this byte, so the values are append-only: never
    /// renumber one.
    pub fn as_byte(self) -> u8 {
        match self {
            Execution::Inherited => 0,
            Execution::Runtime => 1,
            Execution::Native => 2,
        }
    }

    /// Decodes a wire byte, or `None` when it names no engine.
    ///
    /// A manifest is a deserializable public artifact, so an unknown byte is a
    /// rejection rather than a panic.
    pub fn from_byte(byte: u8) -> Option<Execution> {
        Some(match byte {
            0 => Execution::Inherited,
            1 => Execution::Runtime,
            2 => Execution::Native,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_round_trip() {
        for execution in [Execution::Runtime, Execution::Native] {
            let name = execution
                .annotation()
                .expect("an explicit engine is written");
            assert_eq!(Execution::from_annotation(name), Some(execution));
        }
        assert_eq!(Execution::Inherited.annotation(), None);
        assert_eq!(Execution::from_annotation("Main"), None);
    }

    #[test]
    fn wire_bytes_round_trip_and_reject_unknown() {
        for execution in [Execution::Inherited, Execution::Runtime, Execution::Native] {
            assert_eq!(Execution::from_byte(execution.as_byte()), Some(execution));
        }
        assert_eq!(Execution::from_byte(3), None);
    }

    #[test]
    fn inherited_resolves_to_the_builds_default_and_explicit_does_not() {
        assert_eq!(
            Execution::Inherited.resolve(Execution::Native),
            Execution::Native
        );
        assert_eq!(
            Execution::Runtime.resolve(Execution::Native),
            Execution::Runtime
        );
        assert_eq!(
            Execution::Native.resolve(Execution::Runtime),
            Execution::Native
        );
    }
}
