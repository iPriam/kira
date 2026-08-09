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
    Check,
    Lint,
    Test,
    Build,
    Ffi,
    Instruments,
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
/// had to exist before the thing it installs — `knvm install-llvm` does it,
/// and `knvm` links no LLVM at all.
pub const ALL: [Command; 21] = [
    Command::Run,
    Command::Debug,
    Command::Tokens,
    Command::Ast,
    Command::Check,
    Command::Lint,
    Command::Test,
    Command::Build,
    Command::Ffi,
    Command::Instruments,
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
            Self::Check => "check",
            Self::Lint => "lint",
            Self::Test => "test",
            Self::Build => "build",
            Self::Ffi => "ffi",
            Self::Instruments => "instruments",
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
            Self::Run | Self::Build => {
                " [file|dir] [--backend vm|llvm|hybrid] [--device] [--timings] [--show-notes]"
            }
            Self::Check => " [file|dir] [--timings] [--show-notes]",
            Self::Shader => " build [--target <name>] [--emit <name>]",
            Self::Lint | Self::Sync => " [file|dir]",
            Self::Live => " [runner] <file> [--backend vm|hybrid] [--no-watch]",
            Self::Help => " [all]",
            _ => "",
        }
    }

    /// One line for the usage screen: what the verb does.
    pub fn description(self) -> &'static str {
        match self {
            Self::Run => "compile and run a program on the VM",
            Self::Debug => "run a program under the debugger",
            Self::Tokens => "print a file's tokens",
            Self::Ast => "print a file's syntax tree",
            Self::Check => "analyze a program without running it",
            Self::Lint => "report what a package's `linter.kira` asks about",
            Self::Test => "build and run a program's tests",
            Self::Build => "compile to a native binary via LLVM",
            Self::Ffi => "inspect and bind native libraries",
            Self::Instruments => "profile a running program",
            Self::Shader => "build every KSL shader and report what each target emitted",
            Self::New => "scaffold a new project",
            Self::Sync => "write `kira.lock` from the package manifests",
            Self::Add => "add a dependency",
            Self::Remove => "remove a dependency",
            Self::Update => "update dependencies",
            Self::Package => "package a library for distribution",
            Self::MigrateManifest => "upgrade a manifest to the current format",
            Self::Live => "run with live reload",
            Self::Export => "export a library for embedding",
            Self::Help => "print this message",
            Self::Version => "print the version",
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
}
