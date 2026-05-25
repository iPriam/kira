pub const CommandKind = enum {
    run,
    fetch_llvm,
    tokens,
    ast,
    check,
    build,
    instruments,
    instrument_artifact,
    run_hybrid_artifact,
    live_runner,
    shader,
    new,
    sync,
    add,
    remove,
    update,
    package,
    live,
    help,
    version,

    pub fn label(self: CommandKind) []const u8 {
        return switch (self) {
            .run => "run",
            .fetch_llvm => "fetch-llvm",
            .tokens => "tokens",
            .ast => "ast",
            .check => "check",
            .build => "build",
            .instruments => "instruments",
            .instrument_artifact => "__instrument-artifact",
            .run_hybrid_artifact => "__run-hybrid-artifact",
            .live_runner => "__live-runner",
            .shader => "shader",
            .new => "new",
            .sync => "sync",
            .add => "add",
            .remove => "remove",
            .update => "update",
            .package => "package",
            .live => "live",
            .help => "help",
            .version => "version",
        };
    }
};

pub fn parse(command: []const u8) ?CommandKind {
    inline for (@typeInfo(CommandKind).@"enum".fields) |field| {
        const kind: CommandKind = @enumFromInt(field.value);
        if (kind == .fetch_llvm) {
            if (std.mem.eql(u8, command, "fetch-llvm")) return kind;
        } else if (kind == .instrument_artifact) {
            if (std.mem.eql(u8, command, "__instrument-artifact")) return kind;
        } else if (kind == .run_hybrid_artifact) {
            if (std.mem.eql(u8, command, "__run-hybrid-artifact")) return kind;
        } else if (kind == .live_runner) {
            if (std.mem.eql(u8, command, "__live-runner")) return kind;
        } else if (std.mem.eql(u8, command, field.name)) {
            return kind;
        }
    }
    return null;
}

const std = @import("std");
