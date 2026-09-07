//! The shape of the value `try value as Type` answers with.
//!
//! Analysis mints the rows and both backends build them, so the variant order
//! is stated once here rather than rediscovered in three places. It is the
//! order every `Result`-shaped enum in the language has: success first.

/// `Ok(target)`.
pub const OK_TAG: u32 = 0;
/// `Error(TypeCastError)`.
pub const ERROR_TAG: u32 = 1;
/// `TypeCastError.Mismatch(Type)`, carrying the descriptor the value held.
pub const MISMATCH_TAG: u32 = 0;

/// The name the compiler's cast-failure enum is declared under.
///
/// Not spellable in source and not Foundation's: a cast is a language
/// operation, so a program that imports nothing still writes one, and a failure
/// it cannot name is a failure it cannot handle.
pub const TYPE_CAST_ERROR: &str = "TypeCastError";

/// The module the minted rows are attributed to.
pub const OWNING_MODULE: &str = "Kira";

/// The template identity the per-target result rows record.
pub const RESULT_TEMPLATE: &str = "Kira::CastResult";
