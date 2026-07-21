//! Structured target triples matched exactly as `arch-os-abi`.

use std::fmt;

/// A build target named by its three components, compared structurally.
///
/// A triple is exactly three non-empty components — architecture, operating
/// system, and ABI — so `aarch64-macos-none` and `wasm32-emscripten-unknown`
/// are triples but `x86_64` and a four-part string are not. Matching is always
/// on the structured components, never a substring of the rendered text, so a
/// host-only library selected for a wasm target is a clean structural miss.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TargetTriple {
    arch: String,
    os: String,
    abi: String,
}

impl TargetTriple {
    /// Builds a triple directly from its three components.
    pub fn new(arch: impl Into<String>, os: impl Into<String>, abi: impl Into<String>) -> Self {
        Self {
            arch: arch.into(),
            os: os.into(),
            abi: abi.into(),
        }
    }

    /// Parses `arch-os-abi` into a structured triple.
    ///
    /// Requires exactly three `-`-separated, non-empty components; anything else
    /// is a [`TripleError::Malformed`] rather than a lenient best guess.
    pub fn parse(text: &str) -> Result<Self, TripleError> {
        let mut components = text.split('-');
        match (
            components.next(),
            components.next(),
            components.next(),
            components.next(),
        ) {
            (Some(arch), Some(os), Some(abi), None)
                if !arch.is_empty() && !os.is_empty() && !abi.is_empty() =>
            {
                Ok(Self::new(arch, os, abi))
            }
            _ => Err(TripleError::Malformed {
                text: text.to_owned(),
            }),
        }
    }

    /// The architecture component.
    pub fn arch(&self) -> &str {
        &self.arch
    }

    /// The operating-system component.
    pub fn os(&self) -> &str {
        &self.os
    }

    /// The ABI component.
    pub fn abi(&self) -> &str {
        &self.abi
    }
}

impl fmt::Display for TargetTriple {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}-{}-{}", self.arch, self.os, self.abi)
    }
}

/// Why a string could not be read as a [`TargetTriple`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TripleError {
    /// The text was not exactly three non-empty `arch-os-abi` components.
    #[error(
        "`{text}` is not a target triple: expected exactly three non-empty `arch-os-abi` components"
    )]
    Malformed {
        /// The text that could not be parsed.
        text: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_host_and_a_wasm_triple() {
        let host = TargetTriple::parse("aarch64-macos-none").expect("a valid host triple");
        assert_eq!(host.arch(), "aarch64");
        assert_eq!(host.os(), "macos");
        assert_eq!(host.abi(), "none");
        assert_eq!(host.to_string(), "aarch64-macos-none");

        let wasm = TargetTriple::parse("wasm32-emscripten-unknown").expect("a valid wasm triple");
        assert_eq!(wasm.arch(), "wasm32");
        assert_eq!(wasm.to_string(), "wasm32-emscripten-unknown");
        assert_ne!(host, wasm);
    }

    #[test]
    fn rejects_malformed_triples() {
        for text in ["", "x86_64", "a-b", "a-b-c-d", "a--c", "-b-c", "a-b-"] {
            assert_eq!(
                TargetTriple::parse(text),
                Err(TripleError::Malformed {
                    text: text.to_owned()
                }),
                "expected `{text}` to be rejected",
            );
        }
    }

    #[test]
    fn matching_is_structural_not_stringly() {
        let host = TargetTriple::new("aarch64", "macos", "none");
        let same = TargetTriple::parse("aarch64-macos-none").expect("valid");
        assert_eq!(host, same);
        assert_ne!(host, TargetTriple::new("aarch64", "macos", "simulator"));
    }
}
