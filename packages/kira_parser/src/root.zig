pub const parse = @import("parser.zig").parse;
pub const parseSource = @import("parser.zig").parseSource;

test {
    _ = @import("parser.zig");
}
