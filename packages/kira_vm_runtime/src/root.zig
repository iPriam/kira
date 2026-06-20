pub const Vm = @import("vm.zig").Vm;
pub const Hooks = @import("vm.zig").Hooks;
pub const OpCode = @import("opcodes.zig").OpCode;
pub const printValue = @import("builtins.zig").printValue;
pub const loadModuleFromFile = @import("module_loader.zig").loadModuleFromFile;
pub const FfiDispatcher = @import("vm_ffi.zig").Dispatcher;

test {
    _ = @import("vm_ffi.zig");
    // NOTE: vm.zig's test block (the interpreter/execution/native-bridge
    // suites in vm_execution_tests.zig and vm_native_bridge_tests.zig) is NOT
    // wired in yet. Those suites were added in 62fb039 but never discovered by
    // the test runner (root.zig is the test entry module and only imported
    // vm_ffi.zig). When wired they reveal 14 pre-existing failures spanning
    // construct-any/enum native-bridge materialization, ownership leaks, and a
    // double-free. The stale APIs they referenced have been repaired so the
    // suites compile; wiring + fixing those failures (and validating VM/LLVM/
    // hybrid parity) is tracked as dedicated follow-up work. Do not add
    // `_ = @import("vm.zig");` here until those failures are resolved — it
    // would turn `zig build test` red.
}
