//! Installing knvm itself out of a checkout: `knvm sinstall`.
//!
//! `binstall` provisions a *toolchain*; this provisions the *tools* — `knvm`,
//! the `kira` launcher, and the launcher's `kira-language-server` alias — which
//! live outside any toolchain version because they are what selects between
//! versions. They are built with cargo from the
//! enclosing checkout and landed in `<kira-home>/bin`, and the user's shell is
//! pointed at that directory the way rustup points one at `~/.cargo/bin`: an
//! `env` script under the kira home, sourced by one line appended to the
//! shell's startup files.
//!
//! Everything here is idempotent. The binaries are replaced atomically (staged
//! beside the destination, then renamed over it, so replacing the very `knvm`
//! that is running works on unix), and the startup-file line is appended only
//! where it is not already present.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::binstall::{BinstallError, enclosing_checkout, target_dir};
use crate::install::InstallError;

/// The tools this installs, as `(built binary, installed name)`.
///
/// The launcher lands twice: once as `kira`, and once under the language
/// server's name, where it dispatches to the selected toolchain's server
/// instead of its primary (see the multi-call note in `kira-bootstrapper`).
/// PATH then always resolves the *selected* server, never a frozen copy.
const TOOLS: [(&str, &str); 3] = [
    ("knvm", "knvm"),
    ("kira", "kira"),
    ("kira", kira_toolchain::LANGUAGE_SERVER_BINARY),
];

/// What a self-install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfInstalled {
    /// Where the tools were installed.
    pub bin_dir: PathBuf,
    /// The `env` script a shell sources to put `bin_dir` on PATH.
    pub env_script: PathBuf,
    /// The startup file the shell actually reads, chosen from `shell`.
    pub startup_file: PathBuf,
    /// Whether this run added the source line, or found it already there.
    pub startup_file_updated: bool,
}

/// Builds `knvm` and `kira` from the enclosing checkout and installs them.
///
/// `kira_home` is where the tools land (`<kira_home>/bin`) and where the `env`
/// script is written. `shell_home` is the directory whose startup file is
/// edited — the user's home, passed explicitly so tests drive a throwaway one.
/// `shell` is the user's shell (`$SHELL`), which decides *which* startup file:
/// a line in a file the shell never reads configures nothing. `start` is where
/// the checkout search begins.
pub fn sinstall(
    kira_home: &Path,
    shell_home: &Path,
    shell: Option<&str>,
    start: &Path,
) -> Result<SelfInstalled, BinstallError> {
    let checkout = enclosing_checkout(start).ok_or_else(|| BinstallError::NotACheckout {
        start: start.to_path_buf(),
    })?;

    let built = Command::new("cargo")
        .args(["build", "-p", "kira-knvm", "-p", "kira-bootstrapper"])
        .current_dir(&checkout)
        .status()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => BinstallError::CargoUnavailable,
            _ => BinstallError::BuildFailed {
                checkout: checkout.clone(),
            },
        })?;
    if !built.success() {
        return Err(BinstallError::BuildFailed { checkout });
    }

    let bin_dir = kira_home.join("bin");
    std::fs::create_dir_all(&bin_dir)
        .map_err(|error| InstallError::io("create", &bin_dir, error))?;

    let debug_dir = target_dir(&checkout).join("debug");
    for (tool, installed) in TOOLS {
        let name = kira_toolchain::executable_name(installed);
        let built_binary = debug_dir.join(kira_toolchain::executable_name(tool));
        if !built_binary.is_file() {
            return Err(BinstallError::MissingBuildArtifact {
                expected: built_binary,
            });
        }
        // Stage beside the destination, then rename over it: replacing the
        // running `knvm` in place this way is safe on unix, where a plain copy
        // onto a busy binary is not.
        let staged = bin_dir.join(format!(".incoming-{name}"));
        std::fs::copy(&built_binary, &staged)
            .map_err(|error| InstallError::io("copy the tool to", &staged, error))?;
        let destination = bin_dir.join(&name);
        std::fs::rename(&staged, &destination)
            .map_err(|error| InstallError::io("move the tool into", &destination, error))?;
    }

    let env_script = kira_home.join("env");
    let env_contents = format!(
        "# Added by `knvm sinstall`: puts the kira tools on PATH.\n\
         export PATH=\"{}:$PATH\"\n",
        bin_dir.display()
    );
    std::fs::write(&env_script, env_contents)
        .map_err(|error| InstallError::io("write", &env_script, error))?;

    let startup_file = startup_file_for(shell_home, shell);
    let startup_file_updated = ensure_source_line(&startup_file, &env_script)?;

    Ok(SelfInstalled {
        bin_dir,
        env_script,
        startup_file,
        startup_file_updated,
    })
}

/// The startup file the user's shell actually reads.
///
/// Chosen from the shell, not from what happens to exist: a default macOS home
/// has no dotfiles at all, and a line appended to `.profile` configures
/// nothing for the zsh that machine runs. zsh reads `.zshenv` on every
/// invocation; bash reads `.bashrc`; anything else gets the POSIX `.profile`.
/// The file is created when missing.
fn startup_file_for(home: &Path, shell: Option<&str>) -> PathBuf {
    let name = shell
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let file = match name {
        "zsh" => ".zshenv",
        "bash" => ".bashrc",
        _ => ".profile",
    };
    home.join(file)
}

/// Appends the source line for `env_script` to `file` unless already there.
///
/// Returns whether this run appended it. The file is created when missing.
fn ensure_source_line(file: &Path, env_script: &Path) -> Result<bool, BinstallError> {
    let line = format!(". \"{}\"", env_script.display());
    if let Ok(contents) = std::fs::read_to_string(file)
        && contents.contains(line.as_str())
    {
        return Ok(false);
    }
    append_line(file, &line)?;
    Ok(true)
}

/// Appends `line` (with a trailing newline) to `file`, creating it if needed.
fn append_line(file: &Path, line: &str) -> Result<(), BinstallError> {
    use std::io::Write as _;
    let mut handle = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .map_err(|error| InstallError::io("open", file, error))?;
    writeln!(handle, "{line}").map_err(|error| InstallError::io("append to", file, error))?;
    Ok(())
}
