//! Value cloning: deep-clones a `Value` graph (arrays, closures, structs,
//! strings) into freshly-registered heap objects.
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/vm_value_clone.zig`.
//! Logic lands with the interpreter port.
