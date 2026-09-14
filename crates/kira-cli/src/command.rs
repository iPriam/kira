//! The `kira` command verbs and their parsing.
//!
//! Hand-rolled on purpose — the CLI takes no argument-parsing dependency.

/// Every verb `kira` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Run,
    Debug,
    Tokens,
    Ast,
    Doc,
    Check,
    Lint,
    Test,
    Build,
    Ffi,
    Profile,
    Shader,
    New,
    Sync,
    Add,
    Remove,
    Update,
    Package,
    MigrateManifest,
    Live,
    Export,
    Help,
    Version,
}

/// All verbs, in the order they appear in help output.
///
/// Provisioning the managed LLVM is deliberately not among them. `kira` links
/// the LLVM backend, so a `kira` that could fetch LLVM would be a binary that
/// had to exist before the thing it installs — `knvm llvm install` does it,
/// and `knvm` links no LLVM at all.
pub const ALL: [Command; 22] = [
    Command::Run,
    Command::Debug,
    Command::Tokens,
    Command::Ast,
    Command::Doc,
    Command::Check,
    Command::Lint,
    Command::Test,
    Command::Build,
    Command::Ffi,
    Command::Profile,
    Command::Shader,
    Command::New,
    Command::Sync,
    Command::Add,
    Command::Remove,
    Command::Update,
    Command::Package,
    Command::MigrateManifest,
    Command::Live,
    Command::Export,
    Command::Help,
];

impl Command {
    /// Parses a verb string into a [`Command`], if it names one.
    ///
    /// The version report is a flag, not a verb: `--version` (or `-V`), the
    /// spelling every tool teaches a caller's fingers.
    pub fn parse(command: &str) -> Option<Self> {
        if command == "--version" || command == "-V" {
            return Some(Self::Version);
        }
        ALL.iter().copied().find(|kind| kind.label() == command)
    }

    /// The verb's canonical spelling on the command line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Debug => "debug",
            Self::Tokens => "tokens",
            Self::Ast => "ast",
            Self::Doc => "doc",
            Self::Check => "check",
            Self::Lint => "lint",
            Self::Test => "test",
            Self::Build => "build",
            Self::Ffi => "ffi",
            Self::Profile => "profile",
            Self::Shader => "shader",
            Self::New => "new",
            Self::Sync => "sync",
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Update => "update",
            Self::Package => "package",
            Self::MigrateManifest => "migrate-manifest",
            Self::Live => "live",
            Self::Export => "export",
            Self::Help => "help",
            Self::Version => "version",
        }
    }

    /// The verb's argument shape for the usage screen, `""` when it takes none.
    ///
    /// Kept honest against the real parsers: `run`/`build` go through
    /// `CompileOptions::parse`, `live` through `LiveOptions::parse`, `check`
    /// takes one optional path, `help` one optional word. A verb with no
    /// handler yet has no argument shape to advertise.
    ///
    /// The path is optional on `run`, `build`, and `check` because omitting it
    /// means the package directory you are standing in.
    pub fn arguments(self) -> &'static str {
        match self {
            Self::Run => {
                " [file|dir] [--backend vm|llvm|hybrid] [--device] [--sanitize address] [--release] [--emit-llvm-ir] [--quit-after 5s] [--timings] [--show-notes] [-- <args...>]"
            }
            Self::Build => {
                " [file|dir] [--backend vm|llvm|hybrid] [--device] [--target arch-os-abi] [--sysroot <dir>] [--relocation-model pic|static] [--linkage dynamic|static] [--sanitize address] [--release] [--emit-llvm-ir] [--timings] [--show-notes] [-- <args...>]"
            }
            Self::Debug => {
                " [file|dir] [--backend vm|llvm|hybrid] [--sanitize address] [--break name[:pc]] [--batch] [--lldb|--lldb-dap] [--dap-continues n] [--prepare] [-- <args...>]"
            }
            Self::Check => {
                " [file|dir] [--device host|wasm32|wasm64] [--target arch-os-abi] [--timings] [--show-notes]"
            }
            Self::Shader => " build [--target <name>] [--emit <name>]",
            Self::Lint => {
                " [file|dir] [--fix] [--strict|--pedantic|--restriction] [--groups=<names>] [--lint-level=<level>] [--allow=<CODE>] [--warn=<CODE>] [--deny=<CODE>]"
            }
            Self::Sync | Self::Update => " [file|dir]",
            Self::Ffi => " [file|dir] [--device host|wasm32|wasm64] [--target arch-os-abi]",
            Self::Profile => " record|report|annotate|script|stat|diff [...]",
            Self::Package => " [file|dir] [--backend vm|llvm|hybrid] [--emit-llvm-ir]",
            Self::Export => {
                " <apple|macos|ios|tvos|visionos|windows|android|web|linux> [dir] [--profile debug|profiler|release] [--surface dom|webgpu|hybrid]"
            }
            Self::Add => " <name> (--path <dir>|--version <version>|--git <url>) [dir]",
            Self::Remove => " <name> [dir]",
            Self::MigrateManifest => " [dir]",
            Self::Live => " [runner] <file> [--backend vm|llvm|hybrid] [--watch|--no-watch]",
            Self::Tokens | Self::Ast => " <file>",
            Self::Doc => " [file|dir]",
            Self::New => " [--app|--library] <dir>",
            Self::Help => " [all]",
            _ => "",
        }
    }

    /// One line for the usage screen: what the verb does.
    pub fn description(self) -> &'static str {
        match self {
            Self::Run => "compile and run a program on the VM",
            Self::Debug => "run a program under the debugger",
            Self::Tokens => "print a file's lexical tokens",
            Self::Ast => "print a file's syntax tree",
            Self::Doc => "render documented declarations as Markdown",
            Self::Check => "analyze a program without running it",
            Self::Lint => "report what a package's `linter.kira` asks about",
            Self::Test => "build and run a program's tests",
            Self::Build => "compile to an application or library artifact",
            Self::Ffi => "inspect and bind native libraries",
            Self::Profile => "record and read a sampled profile of a run",
            Self::Shader => "build every KSL shader and report what each target emitted",
            Self::New => "scaffold a new project",
            Self::Sync => "write `kira.lock` from the package manifests",
            Self::Add => "add a dependency",
            Self::Remove => "remove a dependency",
            Self::Update => "update dependencies",
            Self::Package => "build a library package for distribution",
            Self::MigrateManifest => "upgrade a manifest to the current format",
            Self::Live => "run with live reload",
            Self::Export => "generate a per-platform project (Xcode, CMake, web)",
            Self::Help => "print this message",
            Self::Version => "print the version",
        }
    }

    /// The flags-and-behavior block `kira <verb> --help` prints under the usage
    /// line, or `""` when the usage line already says everything.
    ///
    /// One entry per flag the verb's parser actually reads, so `--help` and the
    /// parser cannot drift: a flag with no line here is a flag a caller cannot
    /// discover, and a line here for a flag the parser rejects is a lie the
    /// screen tells. Kept beside `arguments`, which advertises the same flags in
    /// the short form the usage screen shows.
    pub fn help_text(self) -> &'static str {
        match self {
            Self::Run => {
                "Compiles a program and runs it in this process.\n\n  \
                 --backend vm|llvm|hybrid      pick the engine (default: vm, or the package's)\n  \
                 --device host|wasm32|wasm64   what to run on; a wasm device serves to a browser\n  \
                 --sanitize address            instrument native memory access (llvm/hybrid only)\n  \
                 --release                     optimize harder than the default build\n  \
                 --emit-llvm-ir                also write textual LLVM IR beside the artifacts\n  \
                 --quit-after <dur>            end the process this long after it starts (5s, 500ms, 2m)\n  \
                 --timings                     report where the build spent its time\n  \
                 --show-notes                  print the compiler's informational notes\n  \
                 -- <args...>                  pass everything after to the program"
            }
            Self::Build => {
                "Compiles to an artifact under `.kira-build/` without running it.\n\n  \
                 --backend vm|llvm|hybrid          pick the engine\n  \
                 --device host|wasm32|wasm64       what to build for\n  \
                 --target <arch-os-abi>            cross-compile for another machine (aarch64-linux-gnu)\n  \
                 --sysroot <dir>                   where a cross build's headers and libraries come from\n  \
                 --relocation-model pic|static     how a cross image is addressed\n  \
                 --linkage dynamic|static          how a cross image is linked\n  \
                 --sanitize address                instrument native memory access (llvm/hybrid only)\n  \
                 --release                         optimize harder than the default build\n  \
                 --emit-llvm-ir                    also write textual LLVM IR\n  \
                 --timings                         report where the build spent its time\n  \
                 --show-notes                      print the compiler's informational notes"
            }
            Self::Check => {
                "Runs the whole frontend and reports diagnostics without emitting anything.\n\n  \
                 --device host|wasm32|wasm64   which machine to check for\n  \
                 --target <arch-os-abi>        check for another machine (autobind and native rows are per target)\n  \
                 --timings                     report where the check spent its time\n  \
                 --show-notes                  print the compiler's informational notes"
            }
            Self::Test => {
                "Runs the package's tests through the same pipeline as `run`, entering at the\n\
                 generated `kiraTestMain` instead of `@Main`.\n\n  \
                 --backend vm|llvm|hybrid   pick the engine\n  \
                 --sanitize address         instrument native memory access (llvm/hybrid only)\n  \
                 -- <args...>               pass everything after to the test program"
            }
            Self::Debug => {
                "Runs a program under the debugger on any backend.\n\n  \
                 --backend vm|llvm|hybrid   pick the engine\n  \
                 --sanitize address         instrument native memory access (llvm/hybrid only)\n  \
                 --break <name[:pc]>        stop at a function by name, optionally at one pc within it\n  \
                 --batch                    run to completion without stopping for input\n  \
                 --lldb                     drive the session through LLDB\n  \
                 --lldb-dap                 drive it through LLDB's DAP server\n  \
                 --dap-continues <n>        auto-continue n times under --lldb-dap\n  \
                 --prepare                  stage the session without launching\n  \
                 -- <args...>               pass everything after to the program"
            }
            Self::Lint => {
                "Runs the package's `linter.kira` and reports what it finds. With no group\n\
                 flag, only the lints the package enabled run; a group asks for more than the\n\
                 package configured, so a stricter run can be seen without editing the file.\n\n\
                 Groups (add to what the package configured):\n  \
                 --strict            every group there is — the full survey\n  \
                 --pedantic          the pedantic group\n  \
                 --restriction       the restriction group\n  \
                 --groups=<names>    a comma-separated list of the above\n\n\
                 Severity (over what the package or a group set):\n  \
                 --lint-level=<level>   move every lint at once: allow|off, warn|warning, deny|error\n  \
                 --allow=<CODE>         silence one lint by code (e.g. KLINT001)\n  \
                 --warn=<CODE>          report one lint without failing the run\n  \
                 --deny=<CODE>          fail the run on one lint\n\n  \
                 --fix               write back every machine-applicable suggestion"
            }
            _ => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_label_roundtrip() {
        for kind in ALL {
            assert_eq!(Some(kind), Command::parse(kind.label()));
        }
        assert_eq!(None, Command::parse("frobnicate"));
    }

    #[test]
    fn version_is_a_flag_not_a_verb() {
        assert_eq!(Some(Command::Version), Command::parse("--version"));
        assert_eq!(Some(Command::Version), Command::parse("-V"));
        assert_eq!(None, Command::parse("version"));
    }

    /// The example the CLI is measured against: `kira lint --help` must name the
    /// group styles and severity levels, and the usage line must advertise the
    /// flags that reach them, so neither drifts from `pipeline::lint`.
    #[test]
    fn lint_help_names_its_group_styles_and_levels() {
        let help = Command::Lint.help_text();
        for flag in ["--strict", "--pedantic", "--restriction", "--groups="] {
            assert!(help.contains(flag), "lint --help omits {flag}");
        }
        for level in ["--lint-level=", "--allow=", "--warn=", "--deny=", "--fix"] {
            assert!(help.contains(level), "lint --help omits {level}");
        }
        let usage = Command::Lint.arguments();
        for flag in [
            "--fix",
            "--strict",
            "--pedantic",
            "--restriction",
            "--lint-level=",
        ] {
            assert!(usage.contains(flag), "lint usage omits {flag}");
        }
    }

    /// Every verb that reads `--sanitize` spells it out under `--help`, and no
    /// verb that cannot honor it advertises one it would reject.
    #[test]
    fn the_sanitizer_is_surfaced_by_exactly_the_verbs_that_take_it() {
        for verb in [Command::Run, Command::Build, Command::Debug, Command::Test] {
            assert!(
                verb.help_text().contains("--sanitize address"),
                "{} --help omits the sanitizer",
                verb.label(),
            );
        }
        assert!(!Command::Check.help_text().contains("--sanitize"));
    }

    /// A verb with no flag block still answers `--help`: the renderer falls back
    /// to the usage line, so `""` here means "the usage line says it all", never
    /// a verb that cannot be asked.
    #[test]
    fn flag_heavy_verbs_carry_a_help_block() {
        for verb in [
            Command::Run,
            Command::Build,
            Command::Check,
            Command::Test,
            Command::Debug,
            Command::Lint,
        ] {
            assert!(
                !verb.help_text().is_empty(),
                "{} has flags but no help block",
                verb.label(),
            );
        }
    }
}
