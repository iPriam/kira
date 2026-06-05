const std = @import("std");
const kira_toolchain = @import("packages/kira_toolchain/src/root.zig");
const llvm_metadata = @import("packages/kira_build/src/llvm_metadata.zig");
const toolchain_layout = @import("packages/kira_llvm_toolchain_layout/src/root.zig");
const test_roots = @import("build_support/test_roots.zig").test_roots;
const kirac_version = "0.1.0";
const kira_primary_executable = "kirac";
const kira_bootstrapper_name = "kira-bootstrapper";
const kira_repository = "kira-lang-com/kira";

const Package = struct {
    name: []const u8,
    path: []const u8,
    imports: []const []const u8,
};

const packages = [_]Package{
    .{ .name = "kira_core", .path = "packages/kira_core/src/root.zig", .imports = &.{} },
    .{ .name = "kira_toolchain", .path = "packages/kira_toolchain/src/root.zig", .imports = &.{} },
    .{ .name = "kira_source", .path = "packages/kira_source/src/root.zig", .imports = &.{"kira_core"} },
    .{ .name = "kira_diagnostics", .path = "packages/kira_diagnostics/src/root.zig", .imports = &.{ "kira_core", "kira_source" } },
    .{ .name = "kira_diagnostic_messages", .path = "packages/kira_diagnostic_messages/src/root.zig", .imports = &.{ "kira_diagnostics", "kira_source" } },
    .{ .name = "kira_log", .path = "packages/kira_log/src/root.zig", .imports = &.{"kira_core"} },
    .{ .name = "kira_runtime_abi", .path = "packages/kira_runtime_abi/src/root.zig", .imports = &.{"kira_core"} },
    .{ .name = "kira_syntax_model", .path = "packages/kira_syntax_model/src/root.zig", .imports = &.{ "kira_core", "kira_source" } },
    .{ .name = "kira_lexer", .path = "packages/kira_lexer/src/root.zig", .imports = &.{ "kira_core", "kira_source", "kira_syntax_model", "kira_diagnostics" } },
    .{ .name = "kira_parser", .path = "packages/kira_parser/src/root.zig", .imports = &.{ "kira_core", "kira_source", "kira_syntax_model", "kira_lexer", "kira_diagnostics" } },
    .{ .name = "kira_semantics_model", .path = "packages/kira_semantics_model/src/root.zig", .imports = &.{ "kira_core", "kira_source", "kira_syntax_model", "kira_runtime_abi" } },
    .{ .name = "kira_shader_model", .path = "packages/kira_shader_model/src/root.zig", .imports = &.{} },
    .{ .name = "kira_ksl_syntax_model", .path = "packages/kira_ksl_syntax_model/src/root.zig", .imports = &.{"kira_source"} },
    .{ .name = "kira_ksl_parser", .path = "packages/kira_ksl_parser/src/root.zig", .imports = &.{ "kira_source", "kira_diagnostics", "kira_ksl_syntax_model" } },
    .{ .name = "kira_shader_ir", .path = "packages/kira_shader_ir/src/root.zig", .imports = &.{ "kira_source", "kira_shader_model" } },
    .{ .name = "kira_ksl_semantics", .path = "packages/kira_ksl_semantics/src/root.zig", .imports = &.{ "kira_source", "kira_diagnostics", "kira_ksl_syntax_model", "kira_ksl_parser", "kira_shader_model", "kira_shader_ir" } },
    .{ .name = "kira_glsl_backend", .path = "packages/kira_glsl_backend/src/root.zig", .imports = &.{ "kira_diagnostics", "kira_shader_model", "kira_shader_ir" } },
    .{ .name = "kira_wgsl_backend", .path = "packages/kira_wgsl_backend/src/root.zig", .imports = &.{ "kira_diagnostics", "kira_shader_model", "kira_shader_ir" } },
    .{ .name = "kira_hlsl_backend", .path = "packages/kira_hlsl_backend/src/root.zig", .imports = &.{ "kira_diagnostics", "kira_shader_model", "kira_shader_ir" } },
    .{ .name = "kira_msl_backend", .path = "packages/kira_msl_backend/src/root.zig", .imports = &.{ "kira_diagnostics", "kira_shader_model", "kira_shader_ir" } },
    .{ .name = "kira_spirv_backend", .path = "packages/kira_spirv_backend/src/root.zig", .imports = &.{ "kira_diagnostics", "kira_shader_model", "kira_shader_ir" } },
    .{ .name = "kira_semantics", .path = "packages/kira_semantics/src/root.zig", .imports = &.{ "kira_core", "kira_source", "kira_syntax_model", "kira_diagnostics", "kira_semantics_model", "kira_runtime_abi", "kira_lexer", "kira_parser" } },
    .{ .name = "kira_ir", .path = "packages/kira_ir/src/root.zig", .imports = &.{ "kira_core", "kira_semantics_model", "kira_runtime_abi" } },
    .{ .name = "kira_hybrid_definition", .path = "packages/kira_hybrid_definition/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi" } },
    .{ .name = "kira_native_lib_definition", .path = "packages/kira_native_lib_definition/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi" } },
    .{ .name = "kira_backend_api", .path = "packages/kira_backend_api/src/root.zig", .imports = &.{ "kira_core", "kira_ir", "kira_native_lib_definition" } },
    .{ .name = "kira_bytecode", .path = "packages/kira_bytecode/src/root.zig", .imports = &.{ "kira_core", "kira_ir", "kira_runtime_abi" } },
    .{ .name = "kira_vm_runtime", .path = "packages/kira_vm_runtime/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi", "kira_bytecode" } },
    .{ .name = "kira_native_bridge", .path = "packages/kira_native_bridge/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi", "kira_hybrid_definition", "kira_native_lib_definition" } },
    .{ .name = "kira_hybrid_runtime", .path = "packages/kira_hybrid_runtime/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi", "kira_hybrid_definition", "kira_native_bridge", "kira_vm_runtime", "kira_bytecode" } },
    .{ .name = "kira_llvm_toolchain_layout", .path = "packages/kira_llvm_toolchain_layout/src/root.zig", .imports = &.{} },
    .{ .name = "kira_llvm_backend", .path = "packages/kira_llvm_backend/src/root.zig", .imports = &.{ "kira_core", "kira_ir", "kira_backend_api", "kira_native_lib_definition", "kira_runtime_abi", "kira_toolchain", "kira_llvm_toolchain_layout" } },
    .{ .name = "kira_manifest", .path = "packages/kira_manifest/src/root.zig", .imports = &.{ "kira_core", "kira_native_lib_definition" } },
    .{ .name = "kira_wasm_runtime", .path = "packages/kira_wasm_runtime/src/root.zig", .imports = &.{} },
    .{ .name = "kira_project", .path = "packages/kira_project/src/root.zig", .imports = &.{ "kira_core", "kira_manifest" } },
    .{ .name = "kira_package_manager", .path = "packages/kira_package_manager/src/root.zig", .imports = &.{ "kira_manifest", "kira_diagnostics", "kira_toolchain" } },
    .{ .name = "kira_program_graph", .path = "packages/kira_program_graph/src/root.zig", .imports = &.{ "kira_source", "kira_diagnostics", "kira_syntax_model", "kira_lexer", "kira_parser", "kira_package_manager" } },
    .{ .name = "kira_build_definition", .path = "packages/kira_build_definition/src/root.zig", .imports = &.{ "kira_core", "kira_native_lib_definition" } },
    .{ .name = "kira_build", .path = "packages/kira_build/src/root.zig", .imports = &.{ "kira_core", "kira_source", "kira_diagnostics", "kira_diagnostic_messages", "kira_syntax_model", "kira_lexer", "kira_parser", "kira_semantics", "kira_ir", "kira_bytecode", "kira_vm_runtime", "kira_manifest", "kira_project", "kira_package_manager", "kira_program_graph", "kira_build_definition", "kira_backend_api", "kira_native_lib_definition", "kira_hybrid_definition", "kira_runtime_abi", "kira_llvm_backend", "kira_llvm_toolchain_layout", "kira_toolchain", "kira_ksl_syntax_model", "kira_ksl_parser", "kira_ksl_semantics", "kira_shader_ir", "kira_shader_model", "kira_glsl_backend", "kira_wgsl_backend", "kira_hlsl_backend", "kira_msl_backend", "kira_spirv_backend" } },
    .{ .name = "kira_instruments", .path = "packages/kira_instruments/src/root.zig", .imports = &.{} },
    .{ .name = "kira_linter", .path = "packages/kira_linter/src/root.zig", .imports = &.{ "kira_core", "kira_diagnostics", "kira_parser", "kira_semantics" } },
    .{ .name = "kira_doc", .path = "packages/kira_doc/src/root.zig", .imports = &.{ "kira_core", "kira_parser", "kira_semantics" } },
    .{ .name = "kira_app_generation", .path = "packages/kira_app_generation/src/root.zig", .imports = &.{"kira_core"} },
    .{ .name = "kira_main", .path = "packages/kira_main/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi", "kira_hybrid_definition", "kira_bytecode", "kira_vm_runtime", "kira_native_bridge", "kira_hybrid_runtime" } },
    .{ .name = "kira_live", .path = "packages/kira_live/src/root.zig", .imports = &.{ "kira_build", "kira_build_definition", "kira_diagnostics", "kira_diagnostic_messages", "kira_hybrid_definition", "kira_hybrid_runtime", "kira_ir", "kira_llvm_backend", "kira_manifest", "kira_native_lib_definition", "kira_package_manager", "kira_project", "kira_wasm_runtime" } },
    .{ .name = "kira_cli", .path = "packages/kira_cli/src/main.zig", .imports = &.{ "cli", "kira_core", "kira_source", "kira_diagnostics", "kira_diagnostic_messages", "kira_syntax_model", "kira_lexer", "kira_parser", "kira_semantics", "kira_ir", "kira_bytecode", "kira_vm_runtime", "kira_build", "kira_build_definition", "kira_hybrid_runtime", "kira_runtime_abi", "kira_app_generation", "kira_live", "kira_log", "kira_toolchain", "kira_project", "kira_package_manager", "kira_manifest", "kira_ksl_syntax_model", "kira_shader_model", "kira_instruments", "kira_wasm_runtime" } },
};

fn applyImports(module: *std.Build.Module, modules: *std.StringArrayHashMapUnmanaged(*std.Build.Module), names: []const []const u8) void {
    for (names) |name| {
        module.addImport(name, modules.get(name).?);
    }
}

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{
        .default_target = preferredDefaultTarget(b.graph.host.result),
    });
    const optimize = b.standardOptimizeOption(.{});
    const apple_sdk = b.option([]const u8, "apple-sdk", "Apple SDK sysroot path used when cross-compiling generated runner support") orelse "";
    const channel = channelForOptimize(optimize);
    const repo_root = b.pathFromRoot("");
    const metadata = llvm_metadata.parseFile(b.allocator, b.pathFromRoot("llvm-metadata.toml")) catch
        @panic("failed to parse llvm-metadata.toml");
    const llvm_version = metadata.llvm_version;
    const llvm_host_key = toolchain_layout.hostLlvmBundleKey(b.graph.host.result) orelse "unsupported-host";
    const llvm_probe = discoverLlvmHeaders(b.allocator, repo_root, llvm_version, llvm_host_key, b.graph.environ_map.get("KIRA_LLVM_HOME"));
    var modules: std.StringArrayHashMapUnmanaged(*std.Build.Module) = .empty;
    defer modules.deinit(b.allocator);
    const cli_dep = b.dependency("cli", .{ .target = target, .optimize = optimize });
    modules.put(b.allocator, "cli", cli_dep.module("cli")) catch @panic("failed to register zig-cli module");

    for (packages) |pkg| {
        const module = b.createModule(.{
            .root_source_file = b.path(pkg.path),
            .target = target,
            .optimize = optimize,
        });
        modules.put(b.allocator, pkg.name, module) catch @panic("failed to register module");
    }

    for (packages) |pkg| {
        applyImports(modules.get(pkg.name).?, &modules, pkg.imports);
    }

    const build_options = b.addOptions();
    build_options.addOption([]const u8, "repo_root", repo_root);
    modules.get("kira_build").?.addOptions("kira_build_build_options", build_options);

    const llvm_options = b.addOptions();
    llvm_options.addOption([]const u8, "repo_root", repo_root);
    llvm_options.addOption([]const u8, "zig_exe", b.graph.zig_exe);
    llvm_options.addOption([]const u8, "llvm_version", llvm_version);
    llvm_options.addOption([]const u8, "llvm_host_key", llvm_host_key);
    llvm_options.addOption([]const u8, "kira_channel", channel.dirName());
    llvm_options.addOption([]const u8, "kira_version", kirac_version);
    modules.get("kira_llvm_backend").?.addOptions("kira_llvm_build_options", llvm_options);
    modules.get("kira_llvm_backend").?.link_libc = true;

    const cli_options = b.addOptions();
    cli_options.addOption([]const u8, "binary_name", kira_bootstrapper_name);
    cli_options.addOption([]const u8, "version", kirac_version);
    cli_options.addOption([]const u8, "channel", channel.dirName());
    cli_options.addOption([]const u8, "primary_executable", kira_primary_executable);
    modules.get("kira_cli").?.addOptions("kira_cli_build_options", cli_options);

    const live_options = b.addOptions();
    live_options.addOption([]const u8, "repo_root", repo_root);
    live_options.addOption([]const u8, "zig_exe", b.graph.zig_exe);
    live_options.addOption([]const u8, "static_file_server_path", b.getInstallPath(.bin, "kira-static-file-server"));
    modules.get("kira_live").?.addOptions("kira_live_build_options", live_options);

    if (llvm_probe) |probe| {
        for (probe.include_dirs) |dir| {
            modules.get("kira_llvm_backend").?.addIncludePath(.{ .cwd_relative = dir });
        }
    }
    if (apple_sdk.len > 0) {
        const apple_include = std.fs.path.join(b.allocator, &.{ apple_sdk, "usr", "include" }) catch @panic("failed to build Apple SDK include path");
        modules.get("kira_native_bridge").?.addSystemIncludePath(.{ .cwd_relative = apple_include });
    }

    const cli = b.addExecutable(.{
        .name = kira_primary_executable,
        .root_module = modules.get("kira_cli").?,
    });

    const bootstrapper_options = b.addOptions();
    bootstrapper_options.addOption([]const u8, "version", kirac_version);
    bootstrapper_options.addOption([]const u8, "channel", channel.dirName());
    bootstrapper_options.addOption([]const u8, "llvm_version", llvm_version);
    bootstrapper_options.addOption([]const u8, "llvm_host_key", llvm_host_key);
    bootstrapper_options.addOption([]const u8, "release_repository", kira_repository);
    const bootstrapper_module = b.createModule(.{
        .root_source_file = b.path("packages/kira_bootstrapper/src/main.zig"),
        .target = b.graph.host,
        .optimize = optimize,
    });
    bootstrapper_module.addImport("kira_toolchain", modules.get("kira_toolchain").?);
    bootstrapper_module.addOptions("kira_bootstrapper_build_options", bootstrapper_options);
    bootstrapper_module.link_libc = true;
    const bootstrapper = b.addExecutable(.{
        .name = kira_bootstrapper_name,
        .root_module = bootstrapper_module,
    });
    const install_bootstrapper = b.addInstallArtifact(bootstrapper, .{});

    const bootstrapper_install_path = b.getInstallPath(.bin, hostExecutableName(b.graph.host.result, kira_bootstrapper_name));
    const bootstrapper_install_dir = std.fs.path.dirname(bootstrapper_install_path) orelse ".";
    const install_toolchain_step = addManagedToolchainInstallStep(
        b,
        b.graph.host.result,
        cli,
        bootstrapper,
        kirac_version,
        channel.dirName(),
        b.path("llvm-metadata.toml"),
        b.path("templates"),
        b.path("foundation"),
        bootstrapper_install_dir,
    );

    b.getInstallStep().dependOn(&install_bootstrapper.step);
    b.getInstallStep().dependOn(&install_toolchain_step.step);

    const kirac_step = b.step("kirac", "Build the standalone kirac executable");
    kirac_step.dependOn(&cli.step);

    const bootstrapper_step = b.step("kira-bootstrapper", "Build the kira-bootstrapper launcher");
    bootstrapper_step.dependOn(&bootstrapper.step);

    const install_kirac_step = b.step("install-kirac", "Install the active Kira toolchain and kira-bootstrapper");
    install_kirac_step.dependOn(&install_toolchain_step.step);

    const kira_main = b.addLibrary(.{
        .linkage = .static,
        .name = "kira_main",
        .root_module = modules.get("kira_main").?,
    });
    b.installArtifact(kira_main);
    kira_main.installHeadersDirectory(b.path("packages/kira_main/include"), "", .{});

    const live_support_module = b.createModule(.{
        .root_source_file = b.path("packages/kira_live/src/runner_support.zig"),
        .target = target,
        .optimize = optimize,
    });
    live_support_module.addImport("kira_hybrid_definition", modules.get("kira_hybrid_definition").?);
    live_support_module.addImport("kira_hybrid_runtime", modules.get("kira_hybrid_runtime").?);
    live_support_module.link_libc = true;
    const live_support_c_flags: []const []const u8 = if (apple_sdk.len > 0) &.{ "-isysroot", apple_sdk } else &.{};
    if (apple_sdk.len > 0) live_support_module.addSystemIncludePath(.{ .cwd_relative = std.fs.path.join(b.allocator, &.{ apple_sdk, "usr", "include" }) catch @panic("failed to build Apple SDK include path") });

    const live_support = b.addLibrary(.{
        .linkage = .static,
        .name = "kira_live_runner_support",
        .root_module = live_support_module,
    });
    live_support.root_module.addCSourceFile(.{
        .file = b.path("packages/kira_native_bridge/src/runtime_helpers.c"),
        .flags = live_support_c_flags,
    });
    const install_live_support = b.addInstallArtifact(live_support, .{});

    const live_desktop_module = b.createModule(.{
        .root_source_file = b.path("packages/kira_live/src/desktop_main.zig"),
        .target = target,
        .optimize = optimize,
    });
    live_desktop_module.addImport("kira_hybrid_definition", modules.get("kira_hybrid_definition").?);
    live_desktop_module.addImport("kira_hybrid_runtime", modules.get("kira_hybrid_runtime").?);
    live_desktop_module.link_libc = true;
    const live_desktop_runner = b.addExecutable(.{
        .name = "kira-live-desktop-runner",
        .root_module = live_desktop_module,
    });
    const install_live_desktop_runner = b.addInstallArtifact(live_desktop_runner, .{});
    const static_file_server_module = b.createModule(.{ .root_source_file = b.path("packages/kira_live/src/static_file_server.zig"), .target = target, .optimize = optimize });
    const static_file_server = b.addExecutable(.{ .name = "kira-static-file-server", .root_module = static_file_server_module });
    const install_static_file_server = b.addInstallArtifact(static_file_server, .{});

    const run_cmd = b.addRunArtifact(cli);
    if (b.args) |args| run_cmd.addArgs(args);
    const run_step = b.step("run", "Run the kirac CLI");
    run_step.dependOn(&run_cmd.step);

    const fetch_llvm_options = b.addOptions();
    fetch_llvm_options.addOption([]const u8, "repo_root", repo_root);
    const fetch_llvm_module = b.createModule(.{
        .root_source_file = b.path("packages/kira_build/src/fetch_llvm_main.zig"),
        .target = b.graph.host,
        .optimize = .ReleaseSafe,
    });
    fetch_llvm_module.addImport("kira_llvm_toolchain_layout", modules.get("kira_llvm_toolchain_layout").?);
    fetch_llvm_module.addImport("kira_toolchain", modules.get("kira_toolchain").?);
    fetch_llvm_module.link_libc = true;
    const fetch_llvm_tool = b.addExecutable(.{
        .name = "fetch-llvm",
        .root_module = fetch_llvm_module,
    });
    fetch_llvm_tool.root_module.addOptions("fetch_llvm_build_options", fetch_llvm_options);
    const fetch_llvm_run = b.addRunArtifact(fetch_llvm_tool);
    if (b.args) |args| fetch_llvm_run.addArgs(args);
    const fetch_llvm_step = b.step("fetch-llvm", "Download and install the pinned LLVM toolchain");
    fetch_llvm_step.dependOn(&fetch_llvm_run.step);

    const live_support_step = b.step("live-runner-support", "Build the generic live runner support static library");
    live_support_step.dependOn(&install_live_support.step);
    const live_desktop_step = b.step("live-desktop-runner", "Build the generic desktop live runner executable");
    live_desktop_step.dependOn(&install_live_desktop_runner.step);

    const repository_truth_module = b.createModule(.{ .root_source_file = b.path("tests/repository_truth.zig"), .target = target, .optimize = optimize });
    const repository_truth = b.addExecutable(.{ .name = "kira-repository-truth", .root_module = repository_truth_module });
    const platform_matrix_module = b.createModule(.{ .root_source_file = b.path("tests/platform_validation_matrix.zig"), .target = target, .optimize = optimize });
    const platform_matrix = b.addExecutable(.{ .name = "kira-platform-validation-matrix", .root_module = platform_matrix_module });
    const cli_matrix_cmd = b.addRunArtifact(repository_truth);
    const cli_matrix_step = b.step("cli-matrix", "Run the discovered sibling-project CLI matrix");
    cli_matrix_step.dependOn(&cli_matrix_cmd.step);

    const real_runtime_verify_cmd = b.addRunArtifact(repository_truth);
    const platform_matrix_cmd = b.addRunArtifact(platform_matrix);
    const real_runtime_verify_step = b.step("verify-real-runtime", "Verify real runtime, Wasm, device runner, and backend policy paths");
    real_runtime_verify_step.dependOn(&real_runtime_verify_cmd.step);
    real_runtime_verify_step.dependOn(&platform_matrix_cmd.step);
    const repo_truth_step = b.step("repo-truth", "Reject Python, root Zig clutter, and fake validation markers");
    repo_truth_step.dependOn(&real_runtime_verify_cmd.step);
    const platform_matrix_step = b.step("platform-validation-matrix", "Verify platform validation matrix wiring and anti-smoke evidence");
    platform_matrix_step.dependOn(&platform_matrix_cmd.step);

    const test_step = b.step("test", "Run package tests");
    test_step.dependOn(&real_runtime_verify_cmd.step);
    test_step.dependOn(&platform_matrix_cmd.step);
    for (test_roots) |name| {
        const unit_tests = b.addTest(.{
            .root_module = modules.get(name).?,
        });
        const run_tests = b.addRunArtifact(unit_tests);
        test_step.dependOn(&run_tests.step);
    }

    const bootstrapper_tests = b.addTest(.{
        .root_module = bootstrapper_module,
    });
    const run_bootstrapper_tests = b.addRunArtifact(bootstrapper_tests);
    test_step.dependOn(&run_bootstrapper_tests.step);

    const corpus_module = b.createModule(.{
        .root_source_file = b.path("tests/corpus_main.zig"),
        .target = target,
        .optimize = optimize,
    });
    corpus_module.addImport("kira_build", modules.get("kira_build").?);
    corpus_module.addImport("kira_build_definition", modules.get("kira_build_definition").?);
    corpus_module.addImport("kira_diagnostics", modules.get("kira_diagnostics").?);
    corpus_module.addImport("kira_hybrid_runtime", modules.get("kira_hybrid_runtime").?);
    corpus_module.addImport("kira_vm_runtime", modules.get("kira_vm_runtime").?);
    corpus_module.link_libc = true;

    const hybrid_runner_module = b.createModule(.{
        .root_source_file = b.path("tests/hybrid_runner.zig"),
        .target = target,
        .optimize = optimize,
    });
    hybrid_runner_module.addImport("kira_hybrid_runtime", modules.get("kira_hybrid_runtime").?);
    hybrid_runner_module.addImport("kira_runtime_abi", modules.get("kira_runtime_abi").?);
    hybrid_runner_module.link_libc = true;

    const corpus_runner = b.addExecutable(.{
        .name = "kira-corpus-tests",
        .root_module = corpus_module,
    });
    const hybrid_runner = b.addExecutable(.{
        .name = "kira-hybrid-runner",
        .root_module = hybrid_runner_module,
    });
    // `zig build test -Dstable-tests` runs the corpus serially and retries transient
    // native (llvm) build/link failures, trading speed for determinism on loaded or
    // contended machines. Equivalent to setting KIRA_CORPUS_STABLE=1.
    const stable_tests = b.option(bool, "stable-tests", "Run the corpus serially and retry transient llvm build/link failures") orelse false;
    const run_corpus = b.addRunArtifact(corpus_runner);
    run_corpus.addArtifactArg(hybrid_runner);
    if (stable_tests) run_corpus.setEnvironmentVariable("KIRA_CORPUS_STABLE", "1");
    run_corpus.stdio = .inherit;
    test_step.dependOn(&run_corpus.step);

    b.default_step.dependOn(&cli.step);
    b.default_step.dependOn(&bootstrapper.step);
    b.default_step.dependOn(&kira_main.step);
    b.default_step.dependOn(&install_static_file_server.step);
}

fn preferredDefaultTarget(host: std.Target) std.Target.Query {
    return switch (host.os.tag) {
        .windows => .{
            .cpu_arch = host.cpu.arch,
            .os_tag = .windows,
            .abi = .msvc,
        },
        else => .{},
    };
}

const LlvmHeaderProbe = struct {
    include_dirs: []const []const u8,
};

fn discoverLlvmHeaders(
    allocator: std.mem.Allocator,
    repo_root: []const u8,
    llvm_version: []const u8,
    llvm_host_key: []const u8,
    env_home: ?[]const u8,
) ?LlvmHeaderProbe {
    if (env_home) |path| {
        if (headerProbeForHome(allocator, path)) |probe| return probe;
    }

    if (llvm_version.len > 0 and !std.mem.eql(u8, llvm_host_key, "unsupported-host")) {
        const shared_home = kira_toolchain.managedLlvmHome(allocator, llvm_version, llvm_host_key) catch return null;
        if (headerProbeForHome(allocator, shared_home)) |probe| return probe;
    }

    if (llvm_version.len > 0 and !std.mem.eql(u8, llvm_host_key, "unsupported-host")) {
        const managed_home = toolchain_layout.managedLlvmHome(allocator, repo_root, llvm_version, llvm_host_key) catch return null;
        if (headerProbeForHome(allocator, managed_home)) |probe| return probe;
    }

    const legacy_current = toolchain_layout.legacyLlvmCurrentHome(allocator, repo_root) catch return null;
    if (headerProbeForHome(allocator, legacy_current)) |probe| return probe;

    if (llvm_version.len > 0 and !std.mem.eql(u8, llvm_host_key, "unsupported-host")) {
        const legacy_versioned = toolchain_layout.legacyLlvmVersionedHome(allocator, repo_root, llvm_version, llvm_host_key) catch return null;
        if (headerProbeForHome(allocator, legacy_versioned)) |probe| return probe;
    }

    return null;
}

fn headerProbeForHome(allocator: std.mem.Allocator, home: []const u8) ?LlvmHeaderProbe {
    const install_include = std.fs.path.join(allocator, &.{ home, "include" }) catch return null;
    if (isValidLlvmIncludeDir(install_include)) {
        return .{ .include_dirs = allocator.dupe([]const u8, &.{install_include}) catch return null };
    }

    const source_include = std.fs.path.join(allocator, &.{ home, "llvm-project", "llvm", "include" }) catch return null;
    if (!isDir(source_include)) return null;

    const build_variants = [_][]const u8{
        "build/include",
        "build-msvc/include",
        "build-release/include",
        "build-debug/include",
    };
    for (build_variants) |suffix| {
        const build_include = std.fs.path.join(allocator, &.{ home, suffix }) catch continue;
        if (isValidLlvmSplitIncludeDirs(source_include, build_include)) {
            return .{
                .include_dirs = allocator.dupe([]const u8, &.{ source_include, build_include }) catch return null,
            };
        }
    }
    return null;
}

fn isValidLlvmIncludeDir(include_dir: []const u8) bool {
    const core_header = std.fs.path.join(std.heap.page_allocator, &.{ include_dir, "llvm-c", "Core.h" }) catch return false;
    defer std.heap.page_allocator.free(core_header);
    const config_header = std.fs.path.join(std.heap.page_allocator, &.{ include_dir, "llvm", "Config", "llvm-config.h" }) catch return false;
    defer std.heap.page_allocator.free(config_header);
    return isFile(core_header) and isFile(config_header);
}

fn isValidLlvmSplitIncludeDirs(source_include: []const u8, build_include: []const u8) bool {
    const core_header = std.fs.path.join(std.heap.page_allocator, &.{ source_include, "llvm-c", "Core.h" }) catch return false;
    defer std.heap.page_allocator.free(core_header);
    const config_header = std.fs.path.join(std.heap.page_allocator, &.{ build_include, "llvm", "Config", "llvm-config.h" }) catch return false;
    defer std.heap.page_allocator.free(config_header);
    return isFile(core_header) and isFile(config_header);
}

fn isDir(path: []const u8) bool {
    var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch std.Io.Dir.cwd().openDir(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}

fn isFile(path: []const u8) bool {
    var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

fn hostExecutableName(host: std.Target, base_name: []const u8) []const u8 {
    return if (host.os.tag == .windows)
        std.fmt.allocPrint(std.heap.page_allocator, "{s}.exe", .{base_name}) catch @panic("out of memory")
    else
        base_name;
}

fn channelForOptimize(optimize: std.builtin.OptimizeMode) kira_toolchain.Channel {
    return switch (optimize) {
        .Debug => .dev,
        .ReleaseSmall, .ReleaseFast, .ReleaseSafe => .release,
    };
}

fn addManagedToolchainInstallStep(
    b: *std.Build,
    host: std.Target,
    cli: *std.Build.Step.Compile,
    bootstrapper: *std.Build.Step.Compile,
    version: []const u8,
    channel: []const u8,
    metadata_file: std.Build.LazyPath,
    templates_dir: std.Build.LazyPath,
    foundation_dir: std.Build.LazyPath,
    bootstrapper_install_dir: []const u8,
) *std.Build.Step.Run {
    const step = switch (host.os.tag) {
        .windows => b.addSystemCommand(&.{
            "powershell.exe",
            "-NoProfile",
            "-Command",
            b.fmt(
                "& {{ param([string]$cliSource, [string]$bootstrapperSource, [string]$version, [string]$channel, [string]$metadataSource, [string]$templatesSource, [string]$foundationSource, [string]$bootstrapperBinDir); " ++
                    "$ErrorActionPreference = 'Stop'; " ++
                    "$kiraHome = Join-Path $HOME '.kira'; " ++
                    "$toolchainRoot = Join-Path $kiraHome ('toolchains\\' + $channel + '\\' + $version); " ++
                    "if (Test-Path $toolchainRoot) {{ Remove-Item $toolchainRoot -Recurse -Force; }}; " ++
                    "$binDir = Join-Path $toolchainRoot 'bin'; " ++
                    "New-Item -ItemType Directory -Force -Path $binDir | Out-Null; " ++
                    "$kiracDest = Join-Path $binDir 'kirac.exe'; " ++
                    "Copy-Item $cliSource $kiracDest -Force; " ++
                    "(Get-Item $kiracDest).LastWriteTime = Get-Date; " ++
                    "$pdbSource = [System.IO.Path]::ChangeExtension($cliSource, 'pdb'); " ++
                    "if (Test-Path $pdbSource) {{ $kiracPdbDest = Join-Path $binDir 'kirac.pdb'; Copy-Item $pdbSource $kiracPdbDest -Force; (Get-Item $kiracPdbDest).LastWriteTime = Get-Date; }}; " ++
                    "New-Item -ItemType Directory -Force -Path $bootstrapperBinDir | Out-Null; " ++
                    "$bootstrapperDest = Join-Path $bootstrapperBinDir 'kira-bootstrapper.exe'; " ++
                    "$kiraDest = Join-Path $bootstrapperBinDir 'kira.exe'; " ++
                    "if ([System.IO.Path]::GetFullPath($bootstrapperSource) -ne [System.IO.Path]::GetFullPath($bootstrapperDest)) {{ Copy-Item $bootstrapperSource $bootstrapperDest -Force }}; " ++
                    "Copy-Item $bootstrapperSource $kiraDest -Force; " ++
                    "(Get-Item $bootstrapperDest).LastWriteTime = Get-Date; " ++
                    "(Get-Item $kiraDest).LastWriteTime = Get-Date; " ++
                    "$bootstrapperPdbSource = [System.IO.Path]::ChangeExtension($bootstrapperSource, 'pdb'); " ++
                    "if (Test-Path $bootstrapperPdbSource) {{ $bootstrapperPdbDest = Join-Path $bootstrapperBinDir 'kira-bootstrapper.pdb'; $kiraPdbDest = Join-Path $bootstrapperBinDir 'kira.pdb'; if ([System.IO.Path]::GetFullPath($bootstrapperPdbSource) -ne [System.IO.Path]::GetFullPath($bootstrapperPdbDest)) {{ Copy-Item $bootstrapperPdbSource $bootstrapperPdbDest -Force }}; Copy-Item $bootstrapperPdbSource $kiraPdbDest -Force; (Get-Item $bootstrapperPdbDest).LastWriteTime = Get-Date; (Get-Item $kiraPdbDest).LastWriteTime = Get-Date; }}; " ++
                    "Copy-Item $metadataSource (Join-Path $toolchainRoot 'llvm-metadata.toml') -Force; " ++
                    "$templatesDest = Join-Path $toolchainRoot 'templates'; " ++
                    "if (Test-Path $templatesDest) {{ Remove-Item $templatesDest -Recurse -Force; }}; " ++
                    "Copy-Item $templatesSource $templatesDest -Recurse -Force; " ++
                    "$foundationDest = Join-Path $toolchainRoot 'foundation'; " ++
                    "if (Test-Path $foundationDest) {{ Remove-Item $foundationDest -Recurse -Force; }}; " ++
                    "Copy-Item $foundationSource $foundationDest -Recurse -Force; " ++
                    "$currentDir = Join-Path $kiraHome 'toolchains'; " ++
                    "New-Item -ItemType Directory -Force -Path $currentDir | Out-Null; " ++
                    "$currentPath = Join-Path $currentDir 'current.toml'; " ++
                    "Set-Content -Path $currentPath -Value ('channel = \"' + $channel + '\"'); " ++
                    "Add-Content -Path $currentPath -Value ('version = \"' + $version + '\"'); " ++
                    "Add-Content -Path $currentPath -Value 'primary = \"kirac\"'; " ++
                    "$normalize = {{ param([string]$value) if ([string]::IsNullOrWhiteSpace($value)) {{ return '' }} return $value.Trim().TrimEnd([char[]]@(92, 47)).ToLowerInvariant() }}; " ++
                    "$target = & $normalize $bootstrapperBinDir; " ++
                    "$userPath = [Environment]::GetEnvironmentVariable('Path', 'User'); " ++
                    "$entries = @(); " ++
                    "$entries += $env:Path -split ';' | Where-Object {{ -not [string]::IsNullOrWhiteSpace($_) }}; " ++
                    "if (-not [string]::IsNullOrWhiteSpace($userPath)) {{ $entries += $userPath -split ';' | Where-Object {{ -not [string]::IsNullOrWhiteSpace($_) }} }}; " ++
                    "$exists = $false; " ++
                    "foreach ($entry in $entries) {{ if ((& $normalize $entry) -eq $target) {{ $exists = $true; break }} }}; " ++
                    "if (-not $exists) {{ " ++
                    "  $newPath = if ([string]::IsNullOrWhiteSpace($userPath)) {{ $bootstrapperBinDir }} else {{ $userPath.TrimEnd(';') + ';' + $bootstrapperBinDir }}; " ++
                    "  [Environment]::SetEnvironmentVariable('Path', $newPath, 'User'); " ++
                    "}}; " ++
                    "Write-Host ('Installed Kira toolchain to ' + $toolchainRoot); " ++
                    "Write-Host ('Activated ' + $channel + '/' + $version); " ++
                    "if ($exists) {{ Write-Host 'kira-bootstrapper is already in your PATH. Restart your shell if needed.' }} else {{ Write-Host 'Added kira-bootstrapper to your PATH. Restart your shell to use it.' }} }}",
                .{},
            ),
        }),
        else => b.addSystemCommand(&.{
            "sh",
            "-c",
            b.fmt(
                "set -eu; " ++
                    "cli_source=\"$0\"; bootstrapper_source=\"$1\"; version=\"$2\"; channel=\"$3\"; metadata_source=\"$4\"; templates_source=\"$5\"; foundation_source=\"$6\"; bootstrapper_bin_dir=\"$7\"; " ++
                    "kira_home=\"$HOME/.kira\"; toolchain_root=\"$kira_home/toolchains/$channel/$version\"; bin_dir=\"$toolchain_root/bin\"; " ++
                    "rm -rf \"$toolchain_root\"; " ++
                    "mkdir -p \"$bin_dir\"; " ++
                    "cp \"$cli_source\" \"$bin_dir/kirac\"; chmod +x \"$bin_dir/kirac\"; touch \"$bin_dir/kirac\"; " ++
                    "mkdir -p \"$bootstrapper_bin_dir\"; " ++
                    "if [ \"$bootstrapper_source\" != \"$bootstrapper_bin_dir/kira-bootstrapper\" ]; then cp \"$bootstrapper_source\" \"$bootstrapper_bin_dir/kira-bootstrapper\"; fi; chmod +x \"$bootstrapper_bin_dir/kira-bootstrapper\"; touch \"$bootstrapper_bin_dir/kira-bootstrapper\"; " ++
                    "cp \"$bootstrapper_source\" \"$bootstrapper_bin_dir/kira\"; chmod +x \"$bootstrapper_bin_dir/kira\"; touch \"$bootstrapper_bin_dir/kira\"; " ++
                    "cp \"$metadata_source\" \"$toolchain_root/llvm-metadata.toml\"; " ++
                    "rm -rf \"$toolchain_root/templates\"; cp -R \"$templates_source\" \"$toolchain_root/templates\"; " ++
                    "rm -rf \"$toolchain_root/foundation\"; cp -R \"$foundation_source\" \"$toolchain_root/foundation\"; " ++
                    "mkdir -p \"$kira_home/toolchains\"; " ++
                    "cat > \"$kira_home/toolchains/current.toml\" <<EOF\nchannel = \"$channel\"\nversion = \"$version\"\nprimary = \"kirac\"\nEOF\n" ++
                    "path_added=0; " ++
                    "case \":$PATH:\" in *\":$bootstrapper_bin_dir:\"*) path_exists=1 ;; *) path_exists=0 ;; esac; " ++
                    "if [ \"$path_exists\" -eq 0 ]; then " ++
                    "  shell_name=$(basename \"${{SHELL:-}}\"); " ++
                    "  case \"$shell_name\" in zsh) rc_file=\"$HOME/.zshrc\" ;; bash) rc_file=\"$HOME/.bashrc\" ;; *) if [ \"$(uname -s)\" = Darwin ]; then rc_file=\"$HOME/.zshrc\"; else rc_file=\"$HOME/.profile\"; fi ;; esac; " ++
                    "  line=\"export PATH=\\\"$bootstrapper_bin_dir:\\$PATH\\\"\"; touch \"$rc_file\"; " ++
                    "  if ! grep -Fqx \"$line\" \"$rc_file\"; then printf '\\n%s\\n' \"$line\" >> \"$rc_file\"; path_added=1; fi; " ++
                    "fi; " ++
                    "printf '%s\\n' \"Installed Kira toolchain to $toolchain_root\"; " ++
                    "printf '%s\\n' \"Activated $channel/$version\"; " ++
                    "if [ \"$path_exists\" -eq 1 ] || [ \"$path_added\" -eq 0 -a \"$path_exists\" -eq 0 ]; then printf '%s\\n' 'kira-bootstrapper is already in your PATH. Restart your shell if needed.'; else printf '%s\\n' 'Added kira-bootstrapper to your PATH. Restart your shell to use it.'; fi",
                .{},
            ),
        }),
    };
    // This installer mutates the user's managed Kira toolchain tree, so it
    // must always execute when requested even if the version string and input
    // files have not changed.
    step.has_side_effects = true;
    step.addArtifactArg(cli);
    step.addArtifactArg(bootstrapper);
    step.addArg(version);
    step.addArg(channel);
    step.addFileArg(metadata_file);
    step.addFileArg(templates_dir);
    step.addFileArg(foundation_dir);
    step.addArg(bootstrapper_install_dir);
    return step;
}
