const source_pkg = @import("kira_source");
const ResolvedType = @import("types.zig").ResolvedType;
const OwnershipMode = @import("types.zig").OwnershipMode;

pub const LocalSymbol = struct {
    id: u32,
    name: []const u8,
    ty: ResolvedType,
    ownership: OwnershipMode = .owned,
    is_param: bool = false,
    is_capture: bool = false,
    span: source_pkg.Span,
};
