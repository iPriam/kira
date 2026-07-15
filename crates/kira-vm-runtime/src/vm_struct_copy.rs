//! Deep struct copy: clones struct field graphs (including nested managed
//! objects) respecting ownership and the heap free-list pools.
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/vm_struct_copy.zig`.
//! Logic lands with the interpreter port.
