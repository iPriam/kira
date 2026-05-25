const std = @import("std");
const build_def = @import("kira_build_definition");
const Parsed = @import("../command/ParsedCommand.zig");

pub fn parseBackend(text: []const u8) ?build_def.ExecutionTarget {
    if (std.mem.eql(u8, text, "vm")) return .vm;
    if (std.mem.eql(u8, text, "llvm") or std.mem.eql(u8, text, "llvm_native")) return .llvm_native;
    if (std.mem.eql(u8, text, "hybrid")) return .hybrid;
    return null;
}

pub fn parseInstrumentBackend(text: []const u8) ?Parsed.InstrumentBackend {
    if (std.mem.eql(u8, text, "runtime") or std.mem.eql(u8, text, "vm")) return .runtime;
    if (std.mem.eql(u8, text, "llvm")) return .llvm;
    if (std.mem.eql(u8, text, "hybrid")) return .hybrid;
    return null;
}

pub fn parseTrack(text: []const u8) ?Parsed.InstrumentTrack {
    if (std.mem.eql(u8, text, "memory")) return .memory;
    if (std.mem.eql(u8, text, "cpu")) return .cpu;
    return null;
}
