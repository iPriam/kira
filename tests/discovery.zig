const std = @import("std");

pub const Phase = enum {
    check,
    build,
    run,
};

pub const Backend = enum {
    vm,
    llvm,
    hybrid,
};

pub const Stage = enum {
    lexer,
    parser,
    graph,
    semantics,
    ir,
    backend_prepare,
};

pub const ExpectedResult = enum {
    pass,
    fail,
    blocked,
};

pub const PhaseExpectation = struct {
    result: ExpectedResult,
    stdout: ?[]const u8 = null,
    diagnostic_code: ?[]const u8 = null,
    diagnostic_title: ?[]const u8 = null,
    stage: ?Stage = null,
};

pub const Expectation = struct {
    backends: []const Backend = &.{},
    check: PhaseExpectation,
    build: PhaseExpectation,
    run: PhaseExpectation,
};

pub const Case = struct {
    name: []const u8,
    source_path: []const u8,
    expectation: Expectation,
};

pub fn discoverCases(allocator: std.mem.Allocator) ![]Case {
    const repo_root = try findRepoRoot(allocator) orelse return error.FileNotFound;
    defer allocator.free(repo_root);
    var original_cwd = try std.Io.Dir.cwd().openDir(std.Options.debug_io, ".", .{});
    defer {
        std.process.setCurrentDir(std.Options.debug_io, original_cwd) catch {};
        original_cwd.close(std.Options.debug_io);
    }
    var repo_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, repo_root, .{});
    defer repo_dir.close(std.Options.debug_io);
    try std.process.setCurrentDir(std.Options.debug_io, repo_dir);

    var cases = std.array_list.Managed(Case).init(allocator);
    try scanRoot(allocator, "tests/pass/run", &cases);
    try scanRoot(allocator, "tests/pass/check", &cases);
    try scanRoot(allocator, "tests/fail", &cases);
    sortCases(cases.items);
    return cases.toOwnedSlice();
}

fn scanRoot(
    allocator: std.mem.Allocator,
    root_rel: []const u8,
    cases: *std.array_list.Managed(Case),
) !void {
    if (!dirExists(root_rel)) return;
    try scanDir(allocator, root_rel, "", cases);
}

fn scanDir(
    allocator: std.mem.Allocator,
    root_rel: []const u8,
    current_rel: []const u8,
    cases: *std.array_list.Managed(Case),
) !void {
    const dir_path = if (current_rel.len == 0)
        root_rel
    else
        try std.fs.path.join(allocator, &.{ root_rel, current_rel });

    var dir = try std.Io.Dir.cwd().openDir(std.Options.debug_io, dir_path, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);

    var walker = dir.iterate();
    while (try walker.next(std.Options.debug_io)) |entry| {
        switch (entry.kind) {
            .directory => {
                const next_rel = if (current_rel.len == 0)
                    try allocator.dupe(u8, entry.name)
                else
                    try std.fs.path.join(allocator, &.{ current_rel, entry.name });
                try scanDir(allocator, root_rel, next_rel, cases);
            },
            .file => {
                if (!std.mem.eql(u8, entry.name, "main.kira")) continue;
                const case_rel = if (current_rel.len == 0)
                    entry.name
                else
                    current_rel;
                const expect_path = try std.fs.path.join(allocator, &.{ root_rel, case_rel, "expect.toml" });
                if (!fileExists(expect_path)) continue;
                const source_path = try std.fs.path.join(allocator, &.{ root_rel, case_rel, "main.kira" });
                const expectation = try loadExpectation(allocator, expect_path);
                try cases.append(.{
                    .name = try std.fs.path.join(allocator, &.{ root_rel, case_rel }),
                    .source_path = source_path,
                    .expectation = expectation,
                });
            },
            else => {},
        }
    }
}

fn loadExpectation(
    allocator: std.mem.Allocator,
    path: []const u8,
) !Expectation {
    const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(1 << 20));
    return parseExpectation(allocator, text);
}

pub fn parseExpectation(
    allocator: std.mem.Allocator,
    text: []const u8,
) !Expectation {
    var backends = std.array_list.Managed(Backend).init(allocator);
    var check = PhaseBuilder{};
    var build = PhaseBuilder{};
    var run = PhaseBuilder{};
    var section: Section = .top;

    var lines = std.mem.splitScalar(u8, text, '\n');
    while (lines.next()) |raw_line| {
        const line = std.mem.trim(u8, raw_line, " \t\r");
        if (line.len == 0 or line[0] == '#') continue;

        if (line[0] == '[') {
            if (line[line.len - 1] != ']') return error.InvalidExpectation;
            section = try parseSection(line[1 .. line.len - 1]);
            continue;
        }

        const equals = std.mem.indexOfScalar(u8, line, '=') orelse return error.InvalidExpectation;
        const key = std.mem.trim(u8, line[0..equals], " \t");
        const value = std.mem.trim(u8, line[equals + 1 ..], " \t");

        switch (section) {
            .top => {
                if (!std.mem.eql(u8, key, "backends")) return error.InvalidExpectation;
                if (backends.items.len != 0) return error.InvalidExpectation;
                const parsed = try parseBackends(allocator, value);
                try backends.appendSlice(parsed);
            },
            .check => try check.assign(allocator, key, value),
            .build => try build.assign(allocator, key, value),
            .run => try run.assign(allocator, key, value),
        }
    }

    const resolved_backends = if (backends.items.len == 0)
        try defaultBackends(allocator)
    else
        try backends.toOwnedSlice();
    try validateBackendPolicy(resolved_backends);

    const check_expectation = try check.finish();
    const build_expectation = try build.finish();
    const run_expectation = try run.finish();
    try validatePhaseOrder(check_expectation, build_expectation, run_expectation);

    return .{
        .backends = resolved_backends,
        .check = check_expectation,
        .build = build_expectation,
        .run = run_expectation,
    };
}

const Section = enum {
    top,
    check,
    build,
    run,
};

const PhaseBuilder = struct {
    result: ?ExpectedResult = null,
    stdout: ?[]const u8 = null,
    diagnostic_code: ?[]const u8 = null,
    diagnostic_title: ?[]const u8 = null,
    stage: ?Stage = null,

    fn assign(self: *PhaseBuilder, allocator: std.mem.Allocator, key: []const u8, value: []const u8) !void {
        if (std.mem.eql(u8, key, "result")) {
            if (self.result != null) return error.InvalidExpectation;
            const parsed_opt = try parseTomlString(allocator, value);
            const parsed = parsed_opt orelse return error.InvalidExpectation;
            self.result = try parseExpectedResult(parsed);
            return;
        }
        if (std.mem.eql(u8, key, "stdout")) {
            if (self.stdout != null) return error.InvalidExpectation;
            const parsed_opt = try parseTomlString(allocator, value);
            self.stdout = parsed_opt orelse return error.InvalidExpectation;
            return;
        }
        if (std.mem.eql(u8, key, "diagnostic_code")) {
            if (self.diagnostic_code != null) return error.InvalidExpectation;
            const parsed_opt = try parseTomlString(allocator, value);
            self.diagnostic_code = parsed_opt orelse return error.InvalidExpectation;
            return;
        }
        if (std.mem.eql(u8, key, "diagnostic_title")) {
            if (self.diagnostic_title != null) return error.InvalidExpectation;
            const parsed_opt = try parseTomlString(allocator, value);
            self.diagnostic_title = parsed_opt orelse return error.InvalidExpectation;
            return;
        }
        if (std.mem.eql(u8, key, "stage")) {
            if (self.stage != null) return error.InvalidExpectation;
            const parsed_opt = try parseTomlString(allocator, value);
            const parsed = parsed_opt orelse return error.InvalidExpectation;
            self.stage = try parseStage(parsed);
            return;
        }
        return error.InvalidExpectation;
    }

    fn finish(self: PhaseBuilder) !PhaseExpectation {
        const result = self.result orelse return error.InvalidExpectation;
        switch (result) {
            .pass => {
                if (self.diagnostic_code != null or self.diagnostic_title != null or self.stage != null) {
                    return error.InvalidExpectation;
                }
            },
            .fail => {
                if (self.diagnostic_code == null or self.diagnostic_title == null) {
                    return error.InvalidExpectation;
                }
            },
            .blocked => {
                if (self.stdout != null or self.diagnostic_code != null or self.diagnostic_title != null or self.stage != null) {
                    return error.InvalidExpectation;
                }
            },
        }
        return .{
            .result = result,
            .stdout = self.stdout,
            .diagnostic_code = self.diagnostic_code,
            .diagnostic_title = self.diagnostic_title,
            .stage = self.stage,
        };
    }
};

fn parseSection(text: []const u8) !Section {
    if (std.mem.eql(u8, text, "phases.check")) return .check;
    if (std.mem.eql(u8, text, "phases.build")) return .build;
    if (std.mem.eql(u8, text, "phases.run")) return .run;
    return error.InvalidExpectation;
}

fn parseExpectedResult(text: []const u8) !ExpectedResult {
    if (std.mem.eql(u8, text, "pass")) return .pass;
    if (std.mem.eql(u8, text, "fail")) return .fail;
    if (std.mem.eql(u8, text, "blocked")) return .blocked;
    return error.InvalidExpectation;
}

fn parseStage(text: []const u8) !Stage {
    if (std.mem.eql(u8, text, "lexer")) return .lexer;
    if (std.mem.eql(u8, text, "parser")) return .parser;
    if (std.mem.eql(u8, text, "graph")) return .graph;
    if (std.mem.eql(u8, text, "semantics")) return .semantics;
    if (std.mem.eql(u8, text, "ir")) return .ir;
    if (std.mem.eql(u8, text, "backend_prepare")) return .backend_prepare;
    return error.InvalidExpectation;
}

fn parseBackends(allocator: std.mem.Allocator, value: []const u8) ![]const Backend {
    if (value.len < 2 or value[0] != '[' or value[value.len - 1] != ']') return error.InvalidExpectation;
    var list = std.array_list.Managed(Backend).init(allocator);
    var parts = std.mem.splitScalar(u8, value[1 .. value.len - 1], ',');
    while (parts.next()) |part| {
        const trimmed = std.mem.trim(u8, part, " \t\r");
        if (trimmed.len == 0) continue;
        const parsed_opt = try parseTomlString(allocator, trimmed);
        const parsed_text = parsed_opt orelse return error.InvalidExpectation;
        const parsed = try parseBackend(parsed_text);
        try list.append(parsed);
    }
    return list.toOwnedSlice();
}

fn parseBackend(text: []const u8) !Backend {
    if (std.mem.eql(u8, text, "vm")) return .vm;
    if (std.mem.eql(u8, text, "llvm")) return .llvm;
    if (std.mem.eql(u8, text, "hybrid")) return .hybrid;
    return error.InvalidExpectation;
}

fn defaultBackends(allocator: std.mem.Allocator) ![]const Backend {
    // Unclassified Kira language cases inherit the shared language matrix:
    // hybrid is mandatory, and VM joins only for VM-representable behavior.
    return allocator.dupe(Backend, &.{ .hybrid, .vm });
}

fn validateBackendPolicy(backends: []const Backend) !void {
    // Most matrices must include hybrid, with LLVM opted in by native/backend
    // integration cases. The single exception is a pure VM matrix: direct FFI
    // executed through LibFFI is genuine VM-only truth — hybrid and native
    // reject it with KSEM093 because they require an @Native boundary — so
    // `["vm"]` alone is a legitimate, non-redundant expectation.
    var saw_hybrid = false;
    var saw_vm = false;
    var saw_llvm = false;
    for (backends) |backend| {
        switch (backend) {
            .hybrid => {
                if (saw_hybrid) return error.InvalidExpectation;
                saw_hybrid = true;
            },
            .vm => {
                if (saw_vm) return error.InvalidExpectation;
                saw_vm = true;
            },
            .llvm => {
                if (saw_llvm) return error.InvalidExpectation;
                saw_llvm = true;
            },
        }
    }
    if (saw_vm and !saw_hybrid and !saw_llvm) return;
    if (!saw_hybrid) return error.InvalidExpectation;
}

fn validatePhaseOrder(check: PhaseExpectation, build: PhaseExpectation, run: PhaseExpectation) !void {
    if (check.result == .blocked) return error.InvalidExpectation;
    if (build.result == .blocked and check.result == .pass) return error.InvalidExpectation;
    if (run.result == .blocked and check.result == .pass and build.result == .pass) return error.InvalidExpectation;
    if (run.result == .pass and run.stdout == null) return error.InvalidExpectation;
}

fn parseTomlString(allocator: std.mem.Allocator, value: []const u8) !?[]const u8 {
    if (value.len == 0 or value[0] != '"') return null;
    if (value[value.len - 1] != '"') return error.InvalidExpectation;

    var buffer = std.array_list.Managed(u8).init(allocator);
    var index: usize = 1;
    while (index < value.len - 1) {
        const ch = value[index];
        if (ch == '\\') {
            index += 1;
            if (index >= value.len - 1) return error.InvalidExpectation;
            switch (value[index]) {
                'n' => try buffer.append('\n'),
                'r' => try buffer.append('\r'),
                't' => try buffer.append('\t'),
                '"' => try buffer.append('"'),
                '\\' => try buffer.append('\\'),
                else => return error.InvalidExpectation,
            }
        } else {
            try buffer.append(ch);
        }
        index += 1;
    }
    return @as(?[]const u8, try buffer.toOwnedSlice());
}

test "phase expectation schema defaults to shared language backends" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const expectation = try parseExpectation(arena.allocator(),
        \\[phases.check]
        \\result = "pass"
        \\
        \\[phases.build]
        \\result = "pass"
        \\
        \\[phases.run]
        \\result = "pass"
        \\stdout = "ok\n"
    );

    try std.testing.expectEqual(@as(usize, 2), expectation.backends.len);
    try std.testing.expectEqual(Backend.hybrid, expectation.backends[0]);
    try std.testing.expectEqual(Backend.vm, expectation.backends[1]);
}

test "phase expectation schema rejects explicit matrices without hybrid" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    try std.testing.expectError(error.InvalidExpectation, parseExpectation(arena.allocator(),
        \\backends = ["vm"]
        \\
        \\[phases.check]
        \\result = "pass"
        \\
        \\[phases.build]
        \\result = "pass"
        \\
        \\[phases.run]
        \\result = "pass"
        \\stdout = "ok\n"
    ));
}

test "phase expectation schema rejects unreachable blocked phases" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    try std.testing.expectError(error.InvalidExpectation, parseExpectation(arena.allocator(),
        \\backends = ["hybrid", "vm"]
        \\
        \\[phases.check]
        \\result = "pass"
        \\
        \\[phases.build]
        \\result = "blocked"
        \\
        \\[phases.run]
        \\result = "blocked"
    ));
}

fn sortCases(items: []Case) void {
    var index: usize = 1;
    while (index < items.len) : (index += 1) {
        var cursor = index;
        while (cursor > 0 and std.mem.order(u8, items[cursor - 1].name, items[cursor].name) == .gt) : (cursor -= 1) {
            const tmp = items[cursor - 1];
            items[cursor - 1] = items[cursor];
            items[cursor] = tmp;
        }
    }
}

fn dirExists(path: []const u8) bool {
    var dir = std.Io.Dir.cwd().openDir(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}

fn findRepoRoot(allocator: std.mem.Allocator) !?[]u8 {
    const exe_path = try std.process.executablePathAlloc(std.Options.debug_io, allocator);
    defer allocator.free(exe_path);
    var current = try allocator.dupe(u8, std.fs.path.dirname(exe_path) orelse ".");
    errdefer allocator.free(current);

    while (true) {
        const build_path = try std.fs.path.join(allocator, &.{ current, "build.zig" });
        defer allocator.free(build_path);
        if (fileExists(build_path)) return current;

        const parent = std.fs.path.dirname(current) orelse break;
        if (std.mem.eql(u8, parent, current)) break;
        const copy = try allocator.dupe(u8, parent);
        allocator.free(current);
        current = copy;
    }

    allocator.free(current);
    return null;
}

fn fileExists(path: []const u8) bool {
    var file = if (std.fs.path.isAbsolute(path))
        std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch return false
    else
        std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}
