//! The generated runtime: everything a Kira program needs that is not the
//! program.
//!
//! The module imports exactly two functions — `print` and `trap` — and generates
//! the rest. That line is deliberate. A host that supplied the allocator or the
//! number formatter would be deciding what a Kira program *says*, and then the
//! Web target's output would be the embedder's answer rather than Kira's. What
//! the host is asked for is the two things a wasm module genuinely cannot do
//! itself: put bytes somewhere a user can see them, and end the process.

use crate::encode::ValType;
use crate::func::{FuncIdx, GlobalIdx};
use crate::literals::Literals;
use crate::module::Module;

pub mod bignum;
pub mod float;
pub mod float_text;
pub mod memory;
pub mod string;

use bignum::Bignum;

/// The module and field names the two imports are looked up under.
pub mod import {
    /// The import module every Kira wasm module reads its host from.
    pub const MODULE: &str = "kira";
    /// Writes one output line: `print(pointer, length)`.
    pub const PRINT: &str = "print";
    /// Reports a runtime trap: `trap(pointer, length)`.
    pub const TRAP: &str = "trap";
}

/// Handles to every generated runtime function.
#[derive(Debug, Clone, Copy)]
pub struct Runtime {
    /// The host's line printer.
    pub print: FuncIdx,
    /// The host's trap reporter.
    pub trap: FuncIdx,
    /// The bump pointer.
    pub heap: GlobalIdx,
    /// How many digits the float formatter last produced.
    pub digit_count: GlobalIdx,
    /// `alloc(size) -> address`
    pub alloc: FuncIdx,
    /// `memcpy(destination, source, length)`
    pub memcpy: FuncIdx,
    /// `str_new(length) -> address`
    pub str_new: FuncIdx,
    /// `str_concat(left, right) -> address`
    pub str_concat: FuncIdx,
    /// `str_eq(left, right) -> i32`
    pub str_eq: FuncIdx,
    /// `str_from_i64(value) -> address`
    pub str_from_i64: FuncIdx,
    /// `str_from_bool(value) -> address`
    pub str_from_bool: FuncIdx,
    /// `str_from_f64(value) -> address`
    pub str_from_f64: FuncIdx,
    /// `print_str(string)`
    pub print_str: FuncIdx,
    /// Traps: integer division or remainder by zero.
    pub trap_div_zero: FuncIdx,
    /// Traps: the host refused to grow linear memory.
    pub trap_oom: FuncIdx,
    /// Traps: a big integer outgrew its register.
    pub trap_bignum: FuncIdx,
    /// `digits(value) -> exponent`
    pub float_digits: FuncIdx,
    /// The big-integer helpers behind the float formatter.
    pub bignum: Bignum,
}

/// The VM's trap text, so a trapping program reads the same on every engine.
///
/// This is the string `kira-vm-runtime` renders for `VmError::DivideByZero`.
/// A copy is a copy, so the parity tests compare the two rather than trusting
/// this comment.
pub const DIVIDE_BY_ZERO: &str = "vm divide does not allow division by zero";
/// Reported when the host will not grow linear memory.
pub const OUT_OF_MEMORY: &str = "the host refused to grow linear memory";
/// Reported when a float's exact numerator outgrows its register.
pub const BIGNUM_OVERFLOW: &str = "float formatting exceeded its scratch registers";

impl Runtime {
    /// Declares the imports, globals, and an index for every runtime function.
    ///
    /// Imports come first because they own the low function indices; every
    /// index below is reserved before any body exists, so the helpers can call
    /// each other and a Kira function can recurse.
    pub fn declare(module: &mut Module) -> Option<Self> {
        let addr = module.addr().val();

        let print = module.import(
            import::MODULE,
            import::PRINT,
            vec![addr, ValType::I32],
            Vec::new(),
        )?;
        let trap = module.import(
            import::MODULE,
            import::TRAP,
            vec![addr, ValType::I32],
            Vec::new(),
        )?;

        // The heap starts past the last literal, which is not known until every
        // body that interns one is emitted; it is set before the module is
        // encoded.
        let heap = module.addr_global(0);
        let digit_count = module.global(ValType::I32, 0);

        Some(Self {
            print,
            trap,
            heap,
            digit_count,
            alloc: module.declare(vec![addr], vec![addr]),
            memcpy: module.declare(vec![addr, addr, ValType::I32], Vec::new()),
            str_new: module.declare(vec![ValType::I32], vec![addr]),
            str_concat: module.declare(vec![addr, addr], vec![addr]),
            str_eq: module.declare(vec![addr, addr], vec![ValType::I32]),
            str_from_i64: module.declare(vec![ValType::I64], vec![addr]),
            str_from_bool: module.declare(vec![ValType::I32], vec![addr]),
            str_from_f64: module.declare(vec![ValType::F64], vec![addr]),
            print_str: module.declare(vec![addr], Vec::new()),
            trap_div_zero: module.declare(Vec::new(), Vec::new()),
            trap_oom: module.declare(Vec::new(), Vec::new()),
            trap_bignum: module.declare(Vec::new(), Vec::new()),
            float_digits: module.declare(vec![ValType::F64], vec![ValType::I32]),
            bignum: Bignum::declare(module),
        })
    }

    /// Emits every runtime body, interning the constants they need.
    ///
    /// Returns `false` only if a handle this runtime issued is not one the
    /// module knows — a wiring mistake in this crate, never a program's fault.
    pub fn define(&self, module: &mut Module, literals: &mut Literals) -> bool {
        let yes = literals.intern("true");
        let no = literals.intern("false");
        let zero = literals.intern("0");
        let divide_by_zero = literals.intern(DIVIDE_BY_ZERO);
        let out_of_memory = literals.intern(OUT_OF_MEMORY);
        let bignum_overflow = literals.intern(BIGNUM_OVERFLOW);

        memory::define_alloc(module, self.alloc, self.heap, self.trap_oom)
            && memory::define_memcpy(module, self.memcpy)
            && string::define_str_new(module, self)
            && string::define_str_concat(module, self)
            && string::define_str_eq(module, self)
            && string::define_str_from_bool(module, self, yes, no)
            && string::define_str_from_i64(module, self, zero)
            && string::define_print_str(module, self)
            && string::define_trap(module, self, self.trap_div_zero, divide_by_zero)
            && string::define_trap(module, self, self.trap_oom, out_of_memory)
            && string::define_trap(module, self, self.trap_bignum, bignum_overflow)
            && self.bignum.define(module, self.trap_bignum)
            && float::define(module, self.float_digits, &self.bignum, self.digit_count)
            && float_text::define(module, self, literals)
    }
}
