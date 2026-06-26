const std = @import("std");
const kira_toolchain = @import("packages/kira_toolchain/src/root.zig");
const llvm_metadata = @import("packages/kira_build/src/llvm_metadata.zig");
const toolchain_layout = @import("packages/kira_llvm_toolchain_layout/src/root.zig");
const llvm_probe = @import("build_support/llvm_probe.zig");
const managed_install = @import("build_support/managed_install.zig");
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
    .{ .name = "kira_ir", .path = "packages/kira_ir/src/root.zig", .imports = &.{ "kira_core", "kira_source", "kira_diagnostics", "kira_semantics_model", "kira_runtime_abi" } },
    .{ .name = "kira_hybrid_definition", .path = "packages/kira_hybrid_definition/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi" } },
    .{ .name = "kira_native_lib_definition", .path = "packages/kira_native_lib_definition/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi" } },
    .{ .name = "kira_dynamic_ffi", .path = "packages/kira_dynamic_ffi/src/root.zig", .imports = &.{} },
    .{ .name = "kira_backend_api", .path = "packages/kira_backend_api/src/root.zig", .imports = &.{ "kira_core", "kira_ir", "kira_native_lib_definition" } },
    .{ .name = "kira_bytecode", .path = "packages/kira_bytecode/src/root.zig", .imports = &.{ "kira_core", "kira_ir", "kira_runtime_abi" } },
    .{ .name = "kira_vm_runtime", .path = "packages/kira_vm_runtime/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi", "kira_bytecode", "kira_dynamic_ffi" } },
    .{ .name = "kira_native_bridge", .path = "packages/kira_native_bridge/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi", "kira_hybrid_definition", "kira_native_lib_definition", "kira_dynamic_ffi" } },
    .{ .name = "kira_hybrid_runtime", .path = "packages/kira_hybrid_runtime/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi", "kira_hybrid_definition", "kira_native_bridge", "kira_vm_runtime", "kira_bytecode" } },
    .{ .name = "kira_llvm_toolchain_layout", .path = "packages/kira_llvm_toolchain_layout/src/root.zig", .imports = &.{} },
    .{ .name = "kira_llvm_backend", .path = "packages/kira_llvm_backend/src/root.zig", .imports = &.{ "kira_core", "kira_ir", "kira_backend_api", "kira_native_lib_definition", "kira_runtime_abi", "kira_toolchain", "kira_llvm_toolchain_layout", "kira_dynamic_ffi" } },
    .{ .name = "kira_manifest", .path = "packages/kira_manifest/src/root.zig", .imports = &.{ "kira_core", "kira_native_lib_definition" } },
    .{ .name = "kira_wasm_runtime", .path = "packages/kira_wasm_runtime/src/root.zig", .imports = &.{} },
    .{ .name = "kira_project", .path = "packages/kira_project/src/root.zig", .imports = &.{ "kira_core", "kira_manifest" } },
    .{ .name = "kira_package_manager", .path = "packages/kira_package_manager/src/root.zig", .imports = &.{ "kira_manifest", "kira_diagnostics", "kira_toolchain" } },
    .{ .name = "kira_program_graph", .path = "packages/kira_program_graph/src/root.zig", .imports = &.{ "kira_source", "kira_diagnostics", "kira_syntax_model", "kira_lexer", "kira_parser", "kira_package_manager" } },
    .{ .name = "kira_build_definition", .path = "packages/kira_build_definition/src/root.zig", .imports = &.{ "kira_core", "kira_native_lib_definition" } },
    .{ .name = "kira_build", .path = "packages/kira_build/src/root.zig", .imports = &.{ "kira_core", "kira_source", "kira_diagnostics", "kira_diagnostic_messages", "kira_syntax_model", "kira_lexer", "kira_parser", "kira_semantics", "kira_ir", "kira_bytecode", "kira_vm_runtime", "kira_manifest", "kira_project", "kira_package_manager", "kira_program_graph", "kira_build_definition", "kira_backend_api", "kira_native_lib_definition", "kira_hybrid_definition", "kira_runtime_abi", "kira_llvm_backend", "kira_llvm_toolchain_layout", "kira_toolchain", "kira_dynamic_ffi", "kira_ksl_syntax_model", "kira_ksl_parser", "kira_ksl_semantics", "kira_shader_ir", "kira_shader_model", "kira_glsl_backend", "kira_wgsl_backend", "kira_hlsl_backend", "kira_msl_backend", "kira_spirv_backend" } },
    .{ .name = "kira_instruments", .path = "packages/kira_instruments/src/root.zig", .imports = &.{} },
    .{ .name = "kira_linter", .path = "packages/kira_linter/src/root.zig", .imports = &.{ "kira_core", "kira_diagnostics", "kira_parser", "kira_semantics" } },
    .{ .name = "kira_doc", .path = "packages/kira_doc/src/root.zig", .imports = &.{ "kira_core", "kira_parser", "kira_semantics" } },
    .{ .name = "kira_app_generation", .path = "packages/kira_app_generation/src/root.zig", .imports = &.{"kira_core"} },
    .{ .name = "kira_main", .path = "packages/kira_main/src/root.zig", .imports = &.{ "kira_core", "kira_runtime_abi", "kira_hybrid_definition", "kira_bytecode", "kira_vm_runtime", "kira_native_bridge", "kira_hybrid_runtime", "kira_build", "kira_build_definition", "kira_diagnostics", "kira_project" } },
    .{ .name = "kira_live", .path = "packages/kira_live/src/root.zig", .imports = &.{ "kira_build", "kira_build_definition", "kira_diagnostics", "kira_diagnostic_messages", "kira_hybrid_definition", "kira_hybrid_runtime", "kira_ir", "kira_llvm_backend", "kira_manifest", "kira_native_lib_definition", "kira_package_manager", "kira_project", "kira_wasm_runtime" } },
    .{ .name = "kira_cli", .path = "packages/kira_cli/src/main.zig", .imports = &.{ "cli", "kira_core", "kira_source", "kira_diagnostics", "kira_diagnostic_messages", "kira_syntax_model", "kira_lexer", "kira_parser", "kira_semantics", "kira_ir", "kira_bytecode", "kira_vm_runtime", "kira_build", "kira_build_definition", "kira_hybrid_runtime", "kira_runtime_abi", "kira_app_generation", "kira_live", "kira_log", "kira_toolchain", "kira_project", "kira_package_manager", "kira_manifest", "kira_ksl_syntax_model", "kira_shader_model", "kira_instruments", "kira_wasm_runtime", "kira_main" } },
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
    const llvm_headers = llvm_probe.discoverLlvmHeaders(b.allocator, repo_root, llvm_version, llvm_host_key, b.graph.environ_map.get("KIRA_LLVM_HOME"));
    var modules: std.StringArrayHashMapUnmanaged(*std.Build.Module) = .empty;
    defer modules.deinit(b.allocator);
    const cli_dep = b.dependency("cli", .{ .target = target, .optimize = optimize });
    modules.put(b.allocator, "cli", cli_dep.module("cli")) catch @panic("failed to register zig-cli module");

    // Interpreter-hot packages are compiled ReleaseFast even in Debug builds:
    // the Debug dev snapshot is what `kira run` uses for interactive UI work
    // (live apps, resize/layout frames), and a Debug interpreter is 4-11x
    // slower than an optimized one (tag/bounds checks alone cost ~7x in the
    // dispatch loop, so ReleaseSafe is not enough). Unit tests still exercise
    // these packages with full safety: the test step builds its own
    // safety-mode variants below. Pass -Dvm-debug to debug the VM runtime
    // itself with full Debug codegen.
    const vm_debug = b.option(bool, "vm-debug", "Compile the VM runtime packages with Debug codegen (default: ReleaseFast inside Debug builds for usable `kira run` performance)") orelse false;
    const runtime_hot_optimize: std.builtin.OptimizeMode = if (optimize == .Debug and !vm_debug) .ReleaseFast else optimize;

    for (packages) |pkg| {
        const pkg_optimize = if (isRuntimeHotPackage(pkg.name)) runtime_hot_optimize else optimize;
        const module = b.createModule(.{
            .root_source_file = b.path(pkg.path),
            .target = target,
            .optimize = pkg_optimize,
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

    if (llvm_headers) |probe| {
        for (probe.include_dirs) |dir| {
            modules.get("kira_llvm_backend").?.addIncludePath(.{ .cwd_relative = dir });
        }
        if (probe.library_dir) |dir| {
            if (probe.link_name) |name| {
                const llvm_backend = modules.get("kira_llvm_backend").?;
                llvm_backend.addLibraryPath(.{ .cwd_relative = dir });
                llvm_backend.linkSystemLibrary(name, .{
                    .use_pkg_config = .no,
                    .preferred_link_mode = .dynamic,
                    .search_strategy = .paths_first,
                });
                if (target.result.os.tag != .windows) {
                    llvm_backend.addRPath(.{ .cwd_relative = dir });
                }
            }
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
    const install_toolchain_step = managed_install.addManagedToolchainInstallStep(
        b,
        b.graph.host.result,
        cli,
        bootstrapper,
        kirac_version,
        channel.dirName(),
        b.path("llvm-metadata.toml"),
        b.path("templates"),
        b.path("foundation"),
        b.path("packages/kira_main/include"),
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
    _ = b.addInstallArtifact(static_file_server, .{});

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

    const fetch_libffi_options = b.addOptions();
    fetch_libffi_options.addOption([]const u8, "repo_root", repo_root);
    const fetch_libffi_module = b.createModule(.{
        .root_source_file = b.path("packages/kira_build/src/fetch_libffi_main.zig"),
        .target = b.graph.host,
        .optimize = .ReleaseSafe,
    });
    fetch_libffi_module.addImport("kira_llvm_toolchain_layout", modules.get("kira_llvm_toolchain_layout").?);
    fetch_libffi_module.addImport("kira_toolchain", modules.get("kira_toolchain").?);
    fetch_libffi_module.link_libc = true;
    const fetch_libffi_tool = b.addExecutable(.{
        .name = "fetch-libffi",
        .root_module = fetch_libffi_module,
    });
    fetch_libffi_tool.root_module.addOptions("fetch_libffi_build_options", fetch_libffi_options);
    const fetch_libffi_run = b.addRunArtifact(fetch_libffi_tool);
    if (b.args) |args| fetch_libffi_run.addArgs(args);
    const fetch_libffi_step = b.step("fetch-libffi", "Download and install the pinned LibFFI toolchain");
    fetch_libffi_step.dependOn(&fetch_libffi_run.step);

    const live_support_step = b.step("live-runner-support", "Build the generic live runner support static library");
    live_support_step.dependOn(&install_live_support.step);
    const live_desktop_step = b.step("live-desktop-runner", "Build the generic desktop live runner executable");
    live_desktop_step.dependOn(&install_live_desktop_runner.step);

    const repository_truth_module = b.createModule(.{ .root_source_file = b.path("tests/repository_truth.zig"), .target = target, .optimize = optimize });
    const repository_truth = b.addExecutable(.{ .name = "kira-repository-truth", .root_module = repository_truth_module });
    const platform_matrix_module = b.createModule(.{ .root_source_file = b.path("tests/platform_validation_matrix.zig"), .target = target, .optimize = optimize });
    const platform_matrix = b.addExecutable(.{ .name = "kira-platform-validation-matrix", .root_module = platform_matrix_module });
    const memory_validation_module = b.createModule(.{ .root_source_file = b.path("tests/memory_validation.zig"), .target = target, .optimize = optimize });
    const memory_validation = b.addExecutable(.{ .name = "kira-memory-validation", .root_module = memory_validation_module });
    const cli_matrix_cmd = b.addRunArtifact(repository_truth);
    const cli_matrix_step = b.step("cli-matrix", "Run the discovered sibling-project CLI matrix");
    cli_matrix_step.dependOn(&cli_matrix_cmd.step);

    const real_runtime_verify_cmd = b.addRunArtifact(repository_truth);
    const platform_matrix_cmd = b.addRunArtifact(platform_matrix);
    _ = b.addRunArtifact(memory_validation);
    const real_runtime_verify_step = b.step("verify-real-runtime", "Verify real runtime, Wasm, device runner, and backend policy paths");
    real_runtime_verify_step.dependOn(&real_runtime_verify_cmd.step);
    real_runtime_verify_step.dependOn(&platform_matrix_cmd.step);
    const repo_truth_step = b.step("repo-truth", "Reject Python, root Zig clutter, and fake validation markers");
    repo_truth_step.dependOn(&real_runtime_verify_cmd.step);
    const platform_matrix_step = b.step("platform-validation-matrix", "Verify platform validation matrix wiring and anti-smoke evidence");
    platform_matrix_step.dependOn(&platform_matrix_cmd.step);

    const test_step = b.step("test", "Run package unit tests and repository policy checks");
    // `zig build` is the documented workflow for refreshing the development
    // snapshot that the `kira` bootstrapper launches from ~/.kira/toolchains.
    // Keep the managed kirac install in lock-step with source changes before
    // running the default validation step.
    test_step.dependOn(&install_toolchain_step.step);
    test_step.dependOn(&real_runtime_verify_cmd.step);
    test_step.dependOn(&platform_matrix_cmd.step);
    // Unit tests for the interpreter-hot packages run against safety-mode
    // variants (full optimize-mode checks), independent of the ReleaseFast
    // modules the `kira` snapshot ships with.
    var safety_test_modules: std.StringArrayHashMapUnmanaged(*std.Build.Module) = .empty;
    defer safety_test_modules.deinit(b.allocator);
    for (test_roots) |name| {
        const root_module = if (isRuntimeHotPackage(name))
            safetyTestModule(b, name, &modules, &safety_test_modules, target, optimize)
        else
            modules.get(name).?;
        const unit_tests = b.addTest(.{
            .root_module = root_module,
        });
        const run_tests = b.addRunArtifact(unit_tests);
        test_step.dependOn(&run_tests.step);
    }

    const vm_runtime_test_step = b.step("test-vm-runtime", "Run only the kira_vm_runtime unit tests");
    const vm_runtime_test_module = safetyTestModule(b, "kira_vm_runtime", &modules, &safety_test_modules, target, optimize);
    const vm_runtime_unit_tests = b.addTest(.{
        .root_module = vm_runtime_test_module,
    });
    const run_vm_runtime_tests = b.addRunArtifact(vm_runtime_unit_tests);
    vm_runtime_test_step.dependOn(&run_vm_runtime_tests.step);

    const bootstrapper_tests = b.addTest(.{
        .root_module = bootstrapper_module,
    });
    const run_bootstrapper_tests = b.addRunArtifact(bootstrapper_tests);

    // `zig build test` - default validation: unit/policy tests plus VM run corpus.
    // `zig build test-backends` - run corpus across VM + LLVM + Hybrid.
    // `zig build test-full` - check, build, and run corpus across all backends.
    const run_corpus_module = b.createModule(.{
        .root_source_file = b.path("tests/corpus_main.zig"),
        .target = target,
        .optimize = optimize,
    });
    run_corpus_module.addImport("kira_build", modules.get("kira_build").?);
    run_corpus_module.addImport("kira_build_definition", modules.get("kira_build_definition").?);
    run_corpus_module.addImport("kira_diagnostics", modules.get("kira_diagnostics").?);
    run_corpus_module.addImport("kira_hybrid_runtime", modules.get("kira_hybrid_runtime").?);
    run_corpus_module.addImport("kira_source", modules.get("kira_source").?);
    run_corpus_module.addImport("kira_vm_runtime", modules.get("kira_vm_runtime").?);
    run_corpus_module.link_libc = true;

    const hybrid_runner_module = b.createModule(.{
        .root_source_file = b.path("tests/hybrid_runner.zig"),
        .target = target,
        .optimize = optimize,
    });
    hybrid_runner_module.addImport("kira_hybrid_runtime", modules.get("kira_hybrid_runtime").?);
    hybrid_runner_module.addImport("kira_runtime_abi", modules.get("kira_runtime_abi").?);
    hybrid_runner_module.link_libc = true;

    const corpus_runner_exec = b.addExecutable(.{
        .name = "kira-corpus-tests",
        .root_module = run_corpus_module,
    });
    const hybrid_runner_exec = b.addExecutable(.{
        .name = "kira-hybrid-runner",
        .root_module = hybrid_runner_module,
    });

    const run_vm_corpus = addCorpusRun(b, corpus_runner_exec, hybrid_runner_exec, "vm", "run", true);
    test_step.dependOn(&run_vm_corpus.step);

    const run_backend_corpus = addCorpusRun(b, corpus_runner_exec, hybrid_runner_exec, "all", "run", false);
    const backend_test_step = b.step("test-backends", "Run corpus execution across all backends (vm+llvm+hybrid)");
    backend_test_step.dependOn(&install_toolchain_step.step);
    backend_test_step.dependOn(&cli.step);
    backend_test_step.dependOn(&bootstrapper.step);
    backend_test_step.dependOn(&run_bootstrapper_tests.step);
    backend_test_step.dependOn(&real_runtime_verify_cmd.step);
    backend_test_step.dependOn(&platform_matrix_cmd.step);
    for (test_roots) |name| {
        const root_module = if (isRuntimeHotPackage(name))
            safetyTestModule(b, name, &modules, &safety_test_modules, target, optimize)
        else
            modules.get(name).?;
        const unit_tests = b.addTest(.{ .root_module = root_module });
        const run_tests = b.addRunArtifact(unit_tests);
        backend_test_step.dependOn(&run_tests.step);
    }
    backend_test_step.dependOn(&run_backend_corpus.step);

    const run_full_corpus = addCorpusRun(b, corpus_runner_exec, hybrid_runner_exec, "all", "check build run", false);
    const full_test_step = b.step("test-full", "Run complete validation: check, build, and run corpus across all backends");
    full_test_step.dependOn(&install_toolchain_step.step);
    full_test_step.dependOn(&cli.step);
    full_test_step.dependOn(&bootstrapper.step);
    full_test_step.dependOn(&run_bootstrapper_tests.step);
    full_test_step.dependOn(&real_runtime_verify_cmd.step);
    full_test_step.dependOn(&platform_matrix_cmd.step);
    for (test_roots) |name| {
        const root_module = if (isRuntimeHotPackage(name))
            safetyTestModule(b, name, &modules, &safety_test_modules, target, optimize)
        else
            modules.get(name).?;
        const unit_tests = b.addTest(.{ .root_module = root_module });
        const run_tests = b.addRunArtifact(unit_tests);
        full_test_step.dependOn(&run_tests.step);
    }
    full_test_step.dependOn(&run_full_corpus.step);

    b.default_step = test_step; // Default to simple (fast) mode
}

fn addCorpusRun(
    b: *std.Build,
    corpus_runner_exec: *std.Build.Step.Compile,
    hybrid_runner_exec: *std.Build.Step.Compile,
    backends: []const u8,
    phases: []const u8,
    stable: bool,
) *std.Build.Step.Run {
    const run = b.addRunArtifact(corpus_runner_exec);
    run.addArtifactArg(hybrid_runner_exec);
    run.setEnvironmentVariable("KIRA_CORPUS_BACKENDS", backends);
    run.setEnvironmentVariable("KIRA_CORPUS_PHASES", phases);
    if (stable) run.setEnvironmentVariable("KIRA_CORPUS_STABLE", "1");
    run.stdio = .inherit;
    return run;
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

fn hostExecutableName(host: std.Target, base_name: []const u8) []const u8 {
    return if (host.os.tag == .windows)
        std.fmt.allocPrint(std.heap.page_allocator, "{s}.exe", .{base_name}) catch @panic("out of memory")
    else
        base_name;
}

/// Builds (and memoizes) a module for `name` compiled with the requested
/// optimize mode for unit testing, recursively giving the runtime-hot
/// dependencies safety-mode variants too; everything else reuses the main
/// module map.
fn safetyTestModule(
    b: *std.Build,
    name: []const u8,
    main_modules: *std.StringArrayHashMapUnmanaged(*std.Build.Module),
    safety_modules: *std.StringArrayHashMapUnmanaged(*std.Build.Module),
    target: std.Build.ResolvedTarget,
    optimize: std.builtin.OptimizeMode,
) *std.Build.Module {
    if (safety_modules.get(name)) |existing| return existing;
    const pkg = for (packages) |candidate| {
        if (std.mem.eql(u8, candidate.name, name)) break candidate;
    } else @panic("unknown runtime-hot package");
    if (!isRuntimeHotPackage(name) and !dependsOnRuntimeHotPackage(pkg)) return main_modules.get(name).?;
    const module = b.createModule(.{
        .root_source_file = b.path(pkg.path),
        .target = target,
        .optimize = optimize,
    });
    if (std.mem.eql(u8, name, "kira_vm_runtime")) module.link_libc = true;
    safety_modules.put(b.allocator, name, module) catch @panic("failed to register safety test module");
    for (pkg.imports) |import_name| {
        module.addImport(import_name, safetyTestModule(b, import_name, main_modules, safety_modules, target, optimize));
    }
    return module;
}

/// Packages on the per-frame interpreter/bridge hot path of `kira run`.
fn isRuntimeHotPackage(name: []const u8) bool {
    const hot = [_][]const u8{
        "kira_vm_runtime",
        "kira_runtime_abi",
        "kira_bytecode",
        "kira_hybrid_runtime",
    };
    for (hot) |hot_name| {
        if (std.mem.eql(u8, name, hot_name)) return true;
    }
    return false;
}

fn dependsOnRuntimeHotPackage(pkg: Package) bool {
    for (pkg.imports) |import_name| {
        if (isRuntimeHotPackage(import_name)) return true;
    }
    return false;
}

fn channelForOptimize(optimize: std.builtin.OptimizeMode) kira_toolchain.Channel {
    return switch (optimize) {
        .Debug => .dev,
        .ReleaseSmall, .ReleaseFast, .ReleaseSafe => .release,
    };
}
