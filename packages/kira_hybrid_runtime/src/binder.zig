const native_bridge = @import("kira_native_bridge");
const hybrid = @import("kira_hybrid_definition");

pub fn bindHybridSymbols(bridge: *native_bridge.NativeBridge, library_path: []const u8, descriptors: []const hybrid.BridgeDescriptor) !void {
    try bridge.bind(library_path, descriptors);
}

pub fn bindHybridSymbolsInSelf(bridge: *native_bridge.NativeBridge, descriptors: []const hybrid.BridgeDescriptor) !void {
    try bridge.bindCurrentProcess(descriptors);
}
