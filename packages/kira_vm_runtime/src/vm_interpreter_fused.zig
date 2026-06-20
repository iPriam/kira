//! Scalar helpers for the decode-produced fused superinstructions in the VM
//! dispatch loop (see `Fusion` in vm_prepare.zig). The fast integer paths are
//! inlined; the general path falls back to the shared value helpers.

const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");
const value_impl = @import("vm_values.zig");
const vm_mod = @import("vm.zig");

const Vm = vm_mod.Vm;

pub inline fn compareIntegers(lhs: i64, rhs: i64, op: bytecode.CompareOp) bool {
    return switch (op) {
        .equal => lhs == rhs,
        .not_equal => lhs != rhs,
        .less => lhs < rhs,
        .less_equal => lhs <= rhs,
        .greater => lhs > rhs,
        .greater_equal => lhs >= rhs,
    };
}

pub inline fn arithIntegers(lhs: i64, rhs: i64, kind: bytecode.ArithKind) i64 {
    return switch (kind) {
        .add => lhs +% rhs,
        .subtract => lhs -% rhs,
        .multiply => lhs *% rhs,
    };
}

pub fn arithValues(vm: *Vm, lhs: runtime_abi.Value, rhs: runtime_abi.Value, kind: bytecode.ArithKind) !runtime_abi.Value {
    return switch (kind) {
        .add => try value_impl.addValues(vm, lhs, rhs),
        .subtract => try value_impl.subtractValues(vm, lhs, rhs),
        .multiply => try value_impl.multiplyValues(vm, lhs, rhs),
    };
}
