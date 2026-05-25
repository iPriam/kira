pub const CompilerPhase = enum {
    cli_argument_parsing,
    target_selection,
    project_discovery,
    parser,
    graph,
    semantics,
    lowering,
    backend_prepare,
    toolchain_activation,
    runtime_execution,
    crash_boundary,
};
