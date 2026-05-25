const std = @import("std");
const RunnerKind = @import("runner_kind.zig").RunnerKind;

pub const LivePlatform = enum {
    desktop,
    ios_simulator,
    ios_device,

    pub fn parse(text: []const u8) ?LivePlatform {
        if (std.mem.eql(u8, text, "desktop")) return .desktop;
        if (std.mem.eql(u8, text, "ios") or
            std.mem.eql(u8, text, "ios-simulator") or
            std.mem.eql(u8, text, "simulator"))
        {
            return .ios_simulator;
        }
        if (std.mem.eql(u8, text, "ios-device") or
            std.mem.eql(u8, text, "device"))
        {
            return .ios_device;
        }
        return null;
    }

    pub fn cliName(self: LivePlatform) []const u8 {
        return switch (self) {
            .desktop => "desktop",
            .ios_simulator => "ios-simulator",
            .ios_device => "ios-device",
        };
    }

    pub fn runnerKind(self: LivePlatform) ?RunnerKind {
        return switch (self) {
            .desktop => .desktop_dynamic_host,
            .ios_simulator, .ios_device => null,
        };
    }
};

test "LivePlatform parses user-facing aliases" {
    try std.testing.expectEqual(LivePlatform.desktop, LivePlatform.parse("desktop").?);
    try std.testing.expectEqual(LivePlatform.ios_simulator, LivePlatform.parse("ios").?);
    try std.testing.expectEqual(LivePlatform.ios_simulator, LivePlatform.parse("ios-simulator").?);
    try std.testing.expectEqual(LivePlatform.ios_device, LivePlatform.parse("ios-device").?);
}
