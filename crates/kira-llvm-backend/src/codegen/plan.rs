//! What a module is being built as, and which part of the program it carries.
//!
//! The lowering in [`super`] is one path taken five ways — a process executable,
//! a whole-program live library, a library, a hybrid half, or a foreign-adapter
//! and, for a program, taken several times over in parallel across the
//! same functions. These are the values that say which of those is happening;
//! nothing here emits anything.

use kira_backend_api::{NativeTarget, WasmDevice};
use kira_runtime_abi::{Execution, ForeignPointerWidth};

use crate::exports::NativeExportSurface;

/// What this module is being built as.
///
/// The modes differ only in which functions have bodies here and how the
/// program is entered, so they share one lowering with an engine plan rather
/// than duplicating it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModuleKind {
    /// A whole program: every function is native and a C `main` starts it.
    Executable,
    /// A whole native program in a shared library: every function is native and
    /// the runner starts it through the fixed live-entry symbol.
    NativeLiveLibrary,
    /// The native half of a hybrid program: only `@Native` functions have
    /// bodies, each also gets a trampoline the host can call, and there is no
    /// `main` — the host is the program.
    HybridLibrary,
    /// A whole Kira library: every function is native, exactly as in an
    /// [`ModuleKind::Executable`], and no C `main` is emitted because a library
    /// is entered by its consumer rather than started by the operating system.
    ///
    /// What it is entered *through* is its `@Export` surface: one stable
    /// trampoline per export, one synthesized destructor per exported class, and
    /// the per-library ABI marker. See [`super::library`].
    Library,
}

/// The target whose data layout the module must carry while it is lowered.
///
/// Not merely a label: the lowering asks LLVM for a type's ABI size to compute
/// an array's stride and a struct field's offset, and the answer comes from the
/// module's data layout. A module lowered against this machine's layout and then
/// emitted for another one is a program whose offsets were computed twice, by
/// two different machines, and agree only by luck.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum CodegenTarget {
    /// A real machine: this one, or another named by `--target`.
    Native(NativeTarget),
    /// A WebAssembly target selected by the command line.
    Wasm(WasmDevice),
}

impl CodegenTarget {
    /// The compiling host, which is what every module that is not explicitly
    /// aimed elsewhere is lowered for.
    pub(crate) fn host() -> Self {
        Self::Native(NativeTarget::Host)
    }
}

/// Which of a program's function bodies one module carries.
///
/// A whole-program native build lowers every function into one LLVM module, and
/// LLVM's code generator is per-function: no pass this backend runs looks across
/// two of them. So the bodies can be dealt out among several modules, emitted on
/// several threads, and linked together — and each function's machine code is
/// the same code the single module would have produced.
///
/// What is *not* dealt out is everything a program has exactly one of: the C
/// `main` and the callback thunks. Those are emitted by the first unit, and
/// every other unit reaches them as declarations the linker resolves — which is
/// also how each unit reaches a Kira function another unit defines.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CodegenUnit {
    index: usize,
    count: usize,
}

impl CodegenUnit {
    /// The one unit of an unsplit build: it carries everything.
    pub(crate) const WHOLE: Self = Self { index: 0, count: 1 };

    /// Unit `index` of `count`.
    pub(crate) fn new(index: usize, count: usize) -> Self {
        Self {
            index,
            count: count.max(1),
        }
    }

    /// Whether this unit emits the body of function `index`.
    ///
    /// Round-robin rather than contiguous blocks: consecutive ids are a
    /// declaration's own methods, so their sizes travel together, and dealing
    /// them out one at a time spreads a heavy family across every unit instead
    /// of piling it onto one.
    pub(super) fn owns(self, function: usize) -> bool {
        function % self.count == self.index
    }

    /// Whether this unit carries the definitions a program has one of.
    pub(super) fn is_first(self) -> bool {
        self.index == 0
    }
}

/// Everything about *how* a program is lowered that is not the program itself.
///
/// One value rather than a parameter list, because the `Module::build_*` entry
/// points differ from each other in exactly these fields and in nothing else,
/// which is the shape a struct says and a parameter list does not.
pub(super) struct Plan<'a> {
    /// What the module is being built as.
    pub(super) kind: ModuleKind,
    /// Which engine owns each function, in `IrProgram::functions` order.
    pub(super) engines: Vec<Execution>,
    /// Whether each function can be reached by this native module.
    pub(super) reachable: Vec<bool>,
    /// What this library exports, empty for anything that is not one.
    pub(super) exports: &'a NativeExportSurface,
    /// The pointer width of the target this module is emitted for.
    pub(super) pointer_width: ForeignPointerWidth,
    /// The target machine that owns the module's LLVM data layout.
    pub(super) target: CodegenTarget,
    /// Imports whose library is absent on this target.
    pub(super) unavailable: &'a [usize],
    /// Which of the program's function bodies this module carries.
    pub(super) unit: CodegenUnit,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole split rests on: every function is emitted by
    /// exactly one unit. One missed body is an undefined symbol at the link;
    /// one emitted twice is a duplicate.
    #[test]
    fn every_function_belongs_to_exactly_one_unit() {
        for count in 1..=8 {
            let units: Vec<CodegenUnit> = (0..count).map(|i| CodegenUnit::new(i, count)).collect();
            for function in 0..200 {
                let owners = units.iter().filter(|unit| unit.owns(function)).count();
                assert_eq!(owners, 1, "function {function} across {count} units");
            }
        }
    }

    #[test]
    fn the_whole_build_carries_every_function_and_the_one_of_a_kind_definitions() {
        assert!(CodegenUnit::WHOLE.is_first());
        for function in 0..50 {
            assert!(CodegenUnit::WHOLE.owns(function));
        }
    }

    /// Only one unit emits the C `main`, the adapters, and the callback thunks.
    #[test]
    fn exactly_one_unit_carries_the_one_of_a_kind_definitions() {
        let first = (0..6)
            .filter(|&i| CodegenUnit::new(i, 6).is_first())
            .count();
        assert_eq!(first, 1);
    }

    /// A count of zero would divide by zero in `owns`; it is clamped instead,
    /// so a caller that computed one from an empty program still builds.
    #[test]
    fn a_count_of_zero_is_one_unit() {
        let unit = CodegenUnit::new(0, 0);
        assert!(unit.is_first());
        assert!(unit.owns(7));
    }
}
