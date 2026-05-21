const std = @import("std");

pub const RunnerKind = enum {
    desktop_dynamic_host,
    xcode_macos,
    xcode_ios,

    pub fn parse(text: []const u8) ?RunnerKind {
        if (std.mem.eql(u8, text, "desktop") or std.mem.eql(u8, text, "desktop-dynamic-host")) return .desktop_dynamic_host;
        if (std.mem.eql(u8, text, "macos") or std.mem.eql(u8, text, "xcode-macos")) return .xcode_macos;
        if (std.mem.eql(u8, text, "ios") or std.mem.eql(u8, text, "xcode-ios")) return .xcode_ios;
        return null;
    }

    pub fn cliName(self: RunnerKind) []const u8 {
        return switch (self) {
            .desktop_dynamic_host => "desktop",
            .xcode_macos => "macos",
            .xcode_ios => "ios",
        };
    }

    pub fn manifestName(self: RunnerKind) []const u8 {
        return switch (self) {
            .desktop_dynamic_host => "desktop-dynamic-host",
            .xcode_macos => "xcode-macos",
            .xcode_ios => "xcode-ios",
        };
    }

    pub fn deterministicDirectoryName(self: RunnerKind) []const u8 {
        return self.manifestName();
    }
};

test "RunnerKind parses and prints canonical names" {
    try std.testing.expectEqual(RunnerKind.desktop_dynamic_host, RunnerKind.parse("desktop").?);
    try std.testing.expectEqual(RunnerKind.xcode_macos, RunnerKind.parse("xcode-macos").?);
    try std.testing.expectEqual(RunnerKind.xcode_ios, RunnerKind.parse("ios").?);
    try std.testing.expectEqualStrings("desktop-dynamic-host", RunnerKind.desktop_dynamic_host.manifestName());
    try std.testing.expectEqualStrings("xcode-macos", RunnerKind.xcode_macos.manifestName());
    try std.testing.expectEqualStrings("xcode-ios", RunnerKind.xcode_ios.manifestName());
}
