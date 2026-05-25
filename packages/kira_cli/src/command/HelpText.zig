const std = @import("std");
const support = @import("../support.zig");
const CommandKind = @import("CommandKind.zig").CommandKind;

pub fn print(writer: anytype, command: ?CommandKind) !void {
    if (command) |kind| {
        return printCommand(writer, kind);
    }
    try writer.print(
        \\{s} <command> [args]
        \\
        \\Commands:
        \\  check        Analyze a project, example, or source file.
        \\  build        Build a project, example, library, or source file.
        \\  run          Build and execute a runnable target.
        \\  live         Start a bounded live runner session for an app/example target.
        \\  shader       Check, inspect, or build KSL shaders.
        \\  instruments  Run a target under process instrumentation.
        \\  sync         Resolve and sync package dependencies.
        \\  add          Add a package dependency.
        \\  remove       Remove a package dependency.
        \\  update       Update registry package dependencies.
        \\  package      Pack or inspect Kira package archives.
        \\  new          Scaffold an app or library package.
        \\  fetch-llvm   Install or describe the managed LLVM toolchain.
        \\  tokens       Print frontend tokens.
        \\  ast          Print frontend AST.
        \\  help         Show command help.
        \\  version      Print the CLI version.
        \\
        \\Project roots:
        \\  Library roots are checkable and buildable, but `run` and `live` reject them with KCL020/KCL021.
        \\  Examples and apps are the runnable/live-capable surfaces.
        \\
        \\Use `{s} help <command>` or `{s} <command> --help` for command details.
        \\
    , .{ support.binaryName(), support.binaryName(), support.binaryName() });
}

fn printCommand(writer: anytype, kind: CommandKind) !void {
    switch (kind) {
        .check => try writer.writeAll(
            \\usage: kira check [--backend vm|llvm|hybrid] [--offline] [--locked] [--timings] [<project-dir|manifest|source>]
            \\Analyze a target. Libraries are valid check targets.
            \\
        ),
        .build => try writer.writeAll(
            \\usage: kira build [--backend vm|llvm|hybrid] [--offline] [--locked] [--timings] [<project-dir|manifest|source>]
            \\Build a target. Libraries are validated as package roots; apps/examples emit backend artifacts.
            \\
        ),
        .run => try writer.writeAll(
            \\usage: kira run [--backend vm|llvm|hybrid] [--offline] [--locked] [--trace-execution] [--timings] [--quit-after <duration>] [<project-dir|manifest|source>]
            \\Run an app, example, or source file. `--quit-after` accepts values like 5s, 5000ms, or 5.
            \\
        ),
        .live => try writer.writeAll(
            \\usage: kira live [desktop|macos|ios] <target> [--quit-after <duration>] [--run-for <duration>] [--kill-after] [--device auto]
            \\       kira live <target> --quit-after <duration>
            \\       kira live runners list|build|clean <target>
            \\Start live mode for an app/example. `--quit-after` is the non-disruptive smoke-test form.
            \\
        ),
        .fetch_llvm => try writer.writeAll(
            \\usage: kira fetch-llvm [--ci-metadata --json | --archive <path>]
            \\Install the pinned LLVM bundle or print CI metadata.
            \\
        ),
        .new => try writer.writeAll(
            \\usage: kira new [--lib] <Name> <destination>
            \\Create an application package by default, or a library package with `--lib`.
            \\
        ),
        .shader => try writer.writeAll(
            \\usage: kira shader check <file.ksl>
            \\       kira shader ast <file.ksl>
            \\       kira shader build [<file.ksl>|Shaders] [--out-dir <dir>]
            \\
        ),
        .instruments => try writer.writeAll(
            \\usage: kira instruments run <target> --backend runtime|llvm|hybrid --track memory|cpu --duration <time> --sample-rate <rate> [--fail-on-growth <bytes>] [--json-out <path>]
            \\
        ),
        .sync => try writer.writeAll("usage: kira sync [--offline] [--locked] [<project-dir|manifest>]\n"),
        .add => try writer.writeAll("usage: kira add <Package>\n       kira add --git <url> (--rev <commit>|--tag <tag>) <Package>\n"),
        .remove => try writer.writeAll("usage: kira remove <Package>\n"),
        .update => try writer.writeAll("usage: kira update [<project-dir|manifest>]\n"),
        .package => try writer.writeAll("usage: kira package pack [<project-dir|manifest>]\n       kira package inspect <archive-path|project-dir>\n"),
        .tokens => try writer.writeAll("usage: kira tokens [<project-dir|manifest|source>]\n"),
        .ast => try writer.writeAll("usage: kira ast [<project-dir|manifest|source>]\n"),
        .version => try writer.writeAll("usage: kira version\n"),
        .help => try writer.writeAll("usage: kira help [command]\n"),
        .instrument_artifact => try writer.writeAll("usage: kira __instrument-artifact --backend runtime|llvm|hybrid --artifact <path> [--cwd <dir>]\n"),
        .run_hybrid_artifact => try writer.writeAll("usage: kira __run-hybrid-artifact --manifest <path> [--cwd <dir>]\n"),
    }
}
