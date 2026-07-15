//! Decode pass: turns a loaded bytecode module into a `PreparedModule`
//! (validated code, resolved constants, implicit trailing `ret`) the
//! dispatch loop indexes without bounds checks.
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/vm_prepare.zig`.
//! The safety of the future unchecked interpreter core is established HERE.
//! Logic lands with the interpreter port.
