//! The `kirac` command verbs and their parsing.
//!
//! Hand-rolled on purpose — the CLI takes no argument-parsing dependency.

/// Every verb `kirac` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Run,
    Debug,
    FetchLlvm,
    Tokens,
    Ast,
    Check,
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
pub const ALL: [Command; 22] = [
    Command::Run,
    Command::Debug,
    Command::FetchLlvm,
    Command::Tokens,
    Command::Ast,
    Command::Check,
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
    Command::Version,
];

impl Command {
    /// Parses a verb string into a [`Command`], if it names one.
    pub fn parse(command: &str) -> Option<Self> {
        ALL.iter().copied().find(|kind| kind.label() == command)
    }

    /// The verb's canonical spelling on the command line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Debug => "debug",
            Self::FetchLlvm => "fetch-llvm",
            Self::Tokens => "tokens",
            Self::Ast => "ast",
            Self::Check => "check",
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
}
