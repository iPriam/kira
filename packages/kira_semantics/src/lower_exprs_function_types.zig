const std = @import("std");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const function_types = @import("function_types.zig");

pub fn functionTypeFromResolvedSignature(
    allocator: std.mem.Allocator,
    params: []const model.ResolvedType,
    return_type: model.ResolvedType,
) !model.ResolvedType {
    const param_ownership = try allocator.alloc(model.OwnershipMode, params.len);
    @memset(param_ownership, .owned);
    return .{
        .kind = .callback,
        .name = try function_types.signatureText(allocator, params, param_ownership, return_type),
    };
}

pub fn functionTypeFromHeader(allocator: std.mem.Allocator, header: shared.FunctionHeader) !model.ResolvedType {
    return .{
        .kind = .callback,
        .name = try function_types.signatureText(allocator, header.params, header.param_ownership, header.return_type),
    };
}
