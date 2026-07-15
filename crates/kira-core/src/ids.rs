//! Opaque `u32` id newtypes shared across the compiler, one per id kind.

macro_rules! define_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(u32);

        impl $name {
            /// Wraps a raw index as a typed id.
            pub fn new(value: u32) -> Self {
                Self(value)
            }

            /// Returns the raw index behind this id.
            pub fn value(self) -> u32 {
                self.0
            }
        }
    };
}

define_id!(
    /// Identifies one module within a program graph.
    ModuleId
);
define_id!(
    /// Identifies one resolved symbol within a module.
    SymbolId
);
define_id!(
    /// Identifies one native library known to the FFI layer.
    LibraryId
);
define_id!(
    /// Identifies one native bridge registration.
    BridgeId
);
