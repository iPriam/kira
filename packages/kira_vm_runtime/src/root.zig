pub const Vm = @import("vm.zig").Vm;
pub const Hooks = @import("vm.zig").Hooks;
pub const OpCode = @import("opcodes.zig").OpCode;
pub const printValue = @import("builtins.zig").printValue;
pub const loadModuleFromFile = @import("module_loader.zig").loadModuleFromFile;
pub const FfiDispatcher = @import("vm_ffi.zig").Dispatcher;

test {
    _ = @import("vm_ffi.zig");
}
