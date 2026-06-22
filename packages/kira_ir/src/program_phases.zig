//! Explicit compiler-phase types for the Kira pipeline.
//!
//! The pipeline distinguishes three phases by *type* so a backend can never consume a
//! program that is merely typechecked or only half-lowered:
//!
//!   ExecutableProgram  -- executable lowering completed (constructs/content lowered,
//!                         instructions are low-level IR). Produced by IR lowering.
//!   VerifiedProgram    -- every executable obligation has been discharged for the target
//!                         backend. The ONLY constructor is `verify`; VM/LLVM/hybrid
//!                         emission accepts solely this type.
//!
//! `verify` is the obligation layer: it proves the executable requirements hold before
//! emission, so `build`/`run` fail early with a precise diagnostic instead of compiling
//! something that detonates at runtime. Obligations are checked here and grow over time;
//! adding a check makes a whole class of latent runtime failures impossible to emit.

const std = @import("std");
const ir = @import("ir.zig");

/// A program that has finished executable lowering. Constructed by the IR lowering stage;
/// it is the sole input accepted by `verify`.
pub const ExecutableProgram = struct {
    program: ir.Program,

    pub fn programPtr(self: *const ExecutableProgram) *const ir.Program {
        return &self.program;
    }
};

/// What the target backend requires of an executable program. Passed to `verify` so the
/// obligation set can be tightened per backend (e.g. native layouts for LLVM/hybrid)
/// without the verifier reaching up into backend packages.
pub const BackendCapabilities = struct {
    /// Native backends (LLVM, hybrid) require every referenced aggregate to have a known
    /// native layout, i.e. its struct/enum declaration must be present in the program.
    requires_native_layout: bool = false,
};

pub const VerifyFailureKind = enum {
    invalid_entry_point,
    unresolved_call_target,
    unknown_struct_type,
    unknown_enum_type,

    pub fn summary(self: VerifyFailureKind) []const u8 {
        return switch (self) {
            .invalid_entry_point => "program entry point is out of range",
            .unresolved_call_target => "call targets a function that was never lowered",
            .unknown_struct_type => "struct type has no declaration (layout unknown)",
            .unknown_enum_type => "enum type has no declaration (layout unknown)",
        };
    }
};

/// A precise description of the first unmet obligation, mapped to a compile diagnostic by
/// the build layer.
pub const VerifyFailure = struct {
    kind: VerifyFailureKind,
    /// Function the obligation was checked in (empty for program-level failures).
    function_name: []const u8 = "",
    /// The offending symbol/type name.
    detail: []const u8 = "",
};

/// A program proven to satisfy every executable obligation. Backends accept only this type,
/// so a `CheckedProgram`/`ExecutableProgram` cannot be emitted by mistake. The only way to
/// obtain one is `verify`.
pub const VerifiedProgram = struct {
    program: ir.Program,

    pub fn programPtr(self: *const VerifiedProgram) *const ir.Program {
        return &self.program;
    }

    /// Explicit, loud escape hatch: wrap a program as verified WITHOUT running the
    /// obligation checks. Production code must obtain a `VerifiedProgram` from `verify`;
    /// this exists only for trusted inputs (hand-authored test IR) and is intentionally
    /// grep-able so audits can confirm no real build path bypasses verification.
    pub fn assumeVerified(program: ir.Program) VerifiedProgram {
        return .{ .program = program };
    }
};

pub const VerifyResult = union(enum) {
    verified: VerifiedProgram,
    failure: VerifyFailure,
};

/// Discharge the executable obligations for `executable` against `caps`. On success the
/// program is wrapped as a `VerifiedProgram`; otherwise the first unmet obligation is
/// returned for the caller to render as a diagnostic. Never mutates the program.
pub fn verify(
    allocator: std.mem.Allocator,
    executable: ExecutableProgram,
    caps: BackendCapabilities,
) !VerifyResult {
    const program = executable.program;

    // Obligation: the entry point names a real function.
    if (program.functions.len == 0 or program.entry_index >= program.functions.len) {
        return .{ .failure = .{ .kind = .invalid_entry_point } };
    }

    var function_ids = std.AutoHashMapUnmanaged(u32, void){};
    defer function_ids.deinit(allocator);
    for (program.functions) |function_decl| {
        try function_ids.put(allocator, function_decl.id, {});
    }

    var struct_names = std.StringHashMapUnmanaged(void){};
    defer struct_names.deinit(allocator);
    for (program.types) |type_decl| {
        try struct_names.put(allocator, type_decl.name, {});
    }

    var enum_names = std.StringHashMapUnmanaged(void){};
    defer enum_names.deinit(allocator);
    for (program.enums) |enum_decl| {
        try enum_names.put(allocator, enum_decl.name, {});
    }

    // NOTE: type methods are intentionally NOT required to be standalone entries in
    // `program.functions` — modifier/extend methods (e.g. `Text.font`) resolve through
    // virtual/value dispatch rather than as directly-lowered functions. A method-resolution
    // obligation belongs in a dispatch-aware check (future work), not here.

    // Per-function obligations: call targets known, aggregate layouts known.
    for (program.functions) |function_decl| {
        if (function_decl.is_extern) continue;
        for (function_decl.instructions) |instruction| {
            switch (instruction) {
                .call => |value| {
                    if (!function_ids.contains(value.callee)) {
                        return .{ .failure = .{
                            .kind = .unresolved_call_target,
                            .function_name = function_decl.name,
                            .detail = "",
                        } };
                    }
                },
                .alloc_struct => |value| {
                    if (caps.requires_native_layout and !struct_names.contains(value.type_name)) {
                        return .{ .failure = .{
                            .kind = .unknown_struct_type,
                            .function_name = function_decl.name,
                            .detail = value.type_name,
                        } };
                    }
                },
                .field_ptr => |value| {
                    if (caps.requires_native_layout and !struct_names.contains(value.base_type_name)) {
                        return .{ .failure = .{
                            .kind = .unknown_struct_type,
                            .function_name = function_decl.name,
                            .detail = value.base_type_name,
                        } };
                    }
                },
                .alloc_enum => |value| {
                    if (caps.requires_native_layout and !enum_names.contains(value.enum_type_name)) {
                        return .{ .failure = .{
                            .kind = .unknown_enum_type,
                            .function_name = function_decl.name,
                            .detail = value.enum_type_name,
                        } };
                    }
                },
                else => {},
            }
        }
    }

    return .{ .verified = .{ .program = program } };
}

test "verify accepts a minimal well-formed program" {
    var program = ir.Program{
        .functions = @constCast(&[_]ir.Function{.{
            .id = 0,
            .name = "main",
            .execution = .inherited,
            .register_count = 1,
            .local_count = 0,
            .local_types = &.{},
            .instructions = @constCast(&[_]ir.Instruction{.{ .ret = .{} }}),
        }}),
        .entry_index = 0,
    };
    const result = try verify(std.testing.allocator, .{ .program = program }, .{});
    try std.testing.expect(result == .verified);
    _ = &program;
}

test "verify rejects an out-of-range entry point" {
    const program = ir.Program{
        .functions = @constCast(&[_]ir.Function{.{
            .id = 0,
            .name = "main",
            .execution = .inherited,
            .register_count = 1,
            .local_count = 0,
            .local_types = &.{},
            .instructions = @constCast(&[_]ir.Instruction{.{ .ret = .{} }}),
        }}),
        .entry_index = 7,
    };
    const result = try verify(std.testing.allocator, .{ .program = program }, .{});
    try std.testing.expect(result == .failure);
    try std.testing.expectEqual(VerifyFailureKind.invalid_entry_point, result.failure.kind);
}

test "verify rejects a call to a missing function" {
    const program = ir.Program{
        .functions = @constCast(&[_]ir.Function{.{
            .id = 0,
            .name = "main",
            .execution = .inherited,
            .register_count = 1,
            .local_count = 0,
            .local_types = &.{},
            .instructions = @constCast(&[_]ir.Instruction{
                .{ .call = .{ .callee = 99, .args = &.{} } },
                .{ .ret = .{} },
            }),
        }}),
        .entry_index = 0,
    };
    const result = try verify(std.testing.allocator, .{ .program = program }, .{});
    try std.testing.expect(result == .failure);
    try std.testing.expectEqual(VerifyFailureKind.unresolved_call_target, result.failure.kind);
}
