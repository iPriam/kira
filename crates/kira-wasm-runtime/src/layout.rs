//! The linear-memory map every generated module shares.
//!
//! Memory is carved once, at build time, into three regions:
//!
//! ```text
//!   0 .. 512      unused: address 0 stays a null that no live string can hold
//! 512 .. 3072     five fixed big-integer registers, for float formatting
//! 3072 .. 4096    the digit buffer the float formatter generates into
//! 4096 .. N       string literals, written by the data section
//!    N .. end     the string heap, handed out by the bump allocator
//! ```
//!
//! # Strings
//!
//! A string is one object: a 4-byte little-endian length, then that many UTF-8
//! bytes. A `String` value is the address of the length word, so `Str` is an
//! `i32` like every other pointer and the embedder can read a printed string
//! out of the exported memory with two loads.
//!
//! # Why the heap never frees
//!
//! The VM gives strings value semantics with affine drops, and its heap
//! accounting is how it proves it. A generated module allocates and never
//! frees: nothing observable depends on the difference — a string's *contents*
//! are what a program can see, and copying on every read is exactly what the VM
//! does — and a module is torn down whole when its program ends. The allocator
//! grows memory on demand, so a concatenating loop runs until the host refuses
//! to grow rather than until a fixed arena runs out.

/// How many 32-bit limbs a big-integer register holds.
///
/// The formatter's widest intermediate is bounded by the widest `f64`: a
/// denormal scales its numerator by 10^323 on top of a 54-bit significand, and
/// a maximal finite value carries a 1024-bit numerator against a denominator
/// scaled by 10^309. Both land under 1200 bits; 64 limbs is 2048, which leaves
/// the bound unarguable rather than tight.
pub const BIGNUM_LIMBS: u32 = 64;

/// The byte stride between big-integer registers: a length word, the limbs,
/// and slack to keep each register's base a round number.
pub const BIGNUM_STRIDE: u32 = 512;

/// The scaled numerator.
pub const BIGNUM_R: u32 = 512;
/// The scaled denominator.
pub const BIGNUM_S: u32 = BIGNUM_R + BIGNUM_STRIDE;
/// The distance to the next representable value above.
pub const BIGNUM_MP: u32 = BIGNUM_S + BIGNUM_STRIDE;
/// The distance to the next representable value below.
pub const BIGNUM_MM: u32 = BIGNUM_MP + BIGNUM_STRIDE;
/// Scratch, for comparisons that must not clobber an operand.
pub const BIGNUM_T: u32 = BIGNUM_MM + BIGNUM_STRIDE;

/// Where the float formatter writes its generated decimal digits.
///
/// Shortest-round-trip never needs more than 17 digits for an `f64`; the region
/// is a whole kilobyte so the sign and rounding carry have nowhere to land but
/// inside it.
pub const DIGITS: u32 = BIGNUM_T + BIGNUM_STRIDE;

/// The first address past the digit buffer.
///
/// The integer formatter fills the buffer backwards from here, because decimal
/// digits fall out least-significant first; the float formatter fills it
/// forwards from [`DIGITS`], because Dragon4 yields them most-significant
/// first. Neither needs more than 32 bytes, so they cannot meet.
pub const DIGITS_END: u32 = DIGITS + 1024;

/// Where the data section starts writing string literals.
pub const LITERALS: u32 = 4096;

/// How many 64KiB pages a module starts with.
///
/// One page covers the fixed regions and a small program's literals; the
/// allocator grows past this as a program needs, so the number is a floor and
/// not a budget.
pub const INITIAL_PAGES: u32 = 1;

/// Bytes per page of linear memory.
pub const PAGE_BYTES: u32 = 65_536;

/// The alignment every allocation is rounded up to.
pub const ALIGN: u32 = 8;

/// The map is fixed at build time, so its invariants are checked at build time:
/// a region that overlapped its neighbour would corrupt a float mid-format, and
/// finding that out from a failing test is later than finding it out from a
/// failing compile.
const _: () = {
    // A register is a length word plus its limbs, and each must end before the
    // next begins.
    let register_bytes = 4 + BIGNUM_LIMBS * 4;
    assert!(BIGNUM_R + register_bytes <= BIGNUM_S);
    assert!(BIGNUM_S + register_bytes <= BIGNUM_MP);
    assert!(BIGNUM_MP + register_bytes <= BIGNUM_MM);
    assert!(BIGNUM_MM + register_bytes <= BIGNUM_T);
    assert!(BIGNUM_T + register_bytes <= DIGITS);

    assert!(DIGITS < DIGITS_END);
    assert!(DIGITS_END <= LITERALS);
    // The fixed regions must leave the literals inside the memory a module
    // starts with, or its data segment would not fit at instantiation.
    assert!(LITERALS < INITIAL_PAGES * PAGE_BYTES);

    // Address zero stays unused, so no live string can hold it.
    assert!(BIGNUM_R > 0);
};
