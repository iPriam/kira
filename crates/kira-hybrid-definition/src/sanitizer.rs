//! Native instrumentation recorded by a hybrid manifest.

/// Native instrumentation a hybrid bundle requires before its library loads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HybridSanitizer {
    /// The native half is not instrumented.
    #[default]
    None,
    /// The native half is instrumented by AddressSanitizer.
    Address,
}

impl HybridSanitizer {
    const ADDRESS_TAG: u8 = 1;

    pub(crate) fn as_byte(self) -> Option<u8> {
        match self {
            Self::None => None,
            Self::Address => Some(Self::ADDRESS_TAG),
        }
    }

    pub(crate) fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            Self::ADDRESS_TAG => Some(Self::Address),
            _ => None,
        }
    }
}
