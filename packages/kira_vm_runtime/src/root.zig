pub const Vm = @import("vm.zig").Vm;
pub const Hooks = @import("vm.zig").Hooks;
pub const OpCode = @import("opcodes.zig").OpCode;
pub const printValue = @import("builtins.zig").printValue;
pub const loadModuleFromFile = @import("module_loader.zig").loadModuleFromFile;
pub const FfiDispatcher = @import("vm_ffi.zig").Dispatcher;

test {
    _ = @import("vm_ffi.zig");
    _ = @import("vm.zig");
    // Keep vm.zig imported so the interpreter/execution/native-bridge suites
    // run under the normal package test entrypoint. These tests previously
    // existed but were dormant because root.zig only imported vm_ffi.zig.
}
