//! Putting `<kira-home>/bin` on the user's PATH, the way each host means it.
//!
//! Two hosts, two mechanisms, one promise: after `knvm sinstall`, the next
//! shell the user opens finds `kira`.
//!
//! On unix that is an `env` script under the kira home plus one line appended
//! to the startup file the user's shell actually reads — rustup's shape.
//!
//! On Windows a POSIX startup file configures nothing, and there is no file to
//! append to at all: the durable place is the user's own `Path` under
//! `HKCU\Environment`. `setx` writes that badly — it truncates the stored value
//! at 1024 characters and folds the machine's `Path` into the user's, so a
//! developer machine with a long PATH silently loses entries. So this reads the
//! raw value, prepends the bin directory when it is missing, writes it back as
//! `REG_EXPAND_SZ`, and broadcasts `WM_SETTINGCHANGE` so shells started
//! afterwards see it without a sign-out.
//!
//! Those Win32 calls are made through `powershell.exe` rather than linked in.
//! `[Environment]::SetEnvironmentVariable(name, value, 'User')` performs
//! exactly that registry write and exactly that broadcast, which buys the
//! mechanism for neither a new dependency nor an `unsafe` block in a tool
//! crate. It stores the value as `REG_SZ`, so a second statement restores the
//! expandable type that entries like `%USERPROFILE%\bin` need to keep working.
//! The new value travels to that process in an environment variable rather than
//! inside the script text, so no `Path` entry can be read as syntax.

use std::path::{Path, PathBuf};

use crate::binstall::BinstallError;
#[cfg(not(windows))]
use crate::install::InstallError;

/// How this host was pointed at the installed tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathConfigured {
    /// A shell startup file sources the kira home's `env` script (unix).
    StartupFile {
        /// The `env` script that prepends the bin directory to `PATH`.
        env_script: PathBuf,
        /// The startup file the user's shell actually reads.
        startup_file: PathBuf,
        /// Whether this run appended the source line, or found it there.
        updated: bool,
    },
    /// The user's persistent `Path` in `HKCU\Environment` names the bin
    /// directory (Windows).
    UserEnvironment {
        /// Whether this run added the entry, or found it there.
        updated: bool,
    },
}

impl PathConfigured {
    /// Whether this run changed anything, as against finding it configured.
    pub fn updated(&self) -> bool {
        match self {
            Self::StartupFile { updated, .. } | Self::UserEnvironment { updated } => *updated,
        }
    }
}

/// Points the host's PATH at `bin_dir`, idempotently.
///
/// `shell_home` and `shell` decide *which* startup file on unix, and mean
/// nothing on Windows, where the user's environment is not a file.
#[cfg(not(windows))]
pub(crate) fn configure(
    kira_home: &Path,
    bin_dir: &Path,
    shell_home: &Path,
    shell: Option<&str>,
) -> Result<PathConfigured, BinstallError> {
    let env_script = kira_home.join("env");
    let env_contents = format!(
        "# Added by `knvm sinstall`: puts the kira tools on PATH.\n\
         export PATH=\"{}:$PATH\"\n",
        bin_dir.display()
    );
    std::fs::write(&env_script, env_contents)
        .map_err(|error| InstallError::io("write", &env_script, error))?;

    let startup_file = startup_file_for(shell_home, shell);
    let updated = ensure_source_line(&startup_file, &env_script)?;
    Ok(PathConfigured::StartupFile {
        env_script,
        startup_file,
        updated,
    })
}

/// Points the host's PATH at `bin_dir`, idempotently.
///
/// See the module docs for why this edits `HKCU\Environment` through
/// PowerShell instead of appending to a file no Windows shell reads.
#[cfg(windows)]
pub(crate) fn configure(
    _kira_home: &Path,
    bin_dir: &Path,
    _shell_home: &Path,
    _shell: Option<&str>,
) -> Result<PathConfigured, BinstallError> {
    let bin_dir = bin_dir.display().to_string();
    let current = read_user_path()?;
    let Some(wanted) = user_path_with(current.as_deref(), &bin_dir) else {
        return Ok(PathConfigured::UserEnvironment { updated: false });
    };
    write_user_path(&wanted)?;
    Ok(PathConfigured::UserEnvironment { updated: true })
}

/// The user `Path` this install needs, or `None` when it already lists
/// `bin_dir`.
///
/// The bin directory goes first, for the reason the unix `env` script prepends
/// it: the tools knvm just installed must win over an older copy. Entries are
/// compared the way Windows treats paths — case-insensitively, ignoring a
/// trailing separator and the quotes some installers leave behind — so a second
/// run recognizes its own work rather than stacking a duplicate.
pub fn user_path_with(current: Option<&str>, bin_dir: &str) -> Option<String> {
    let current = current.unwrap_or_default();
    if current
        .split(';')
        .any(|entry| normalized(entry) == normalized(bin_dir))
    {
        return None;
    }
    if current.is_empty() {
        return Some(bin_dir.to_string());
    }
    Some(format!("{bin_dir};{current}"))
}

/// One `Path` entry, in the form two entries can be compared in.
fn normalized(entry: &str) -> String {
    entry
        .trim()
        .trim_matches('"')
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
}

/// The startup file the user's shell actually reads.
///
/// Chosen from the shell, not from what happens to exist: a default macOS home
/// has no dotfiles at all, and a line appended to `.profile` configures
/// nothing for the zsh that machine runs. zsh reads `.zshenv` on every
/// invocation; bash reads `.bashrc`; anything else gets the POSIX `.profile`.
/// The file is created when missing.
#[cfg(not(windows))]
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
#[cfg(not(windows))]
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
#[cfg(not(windows))]
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

/// The shell the Win32 environment calls are made through.
#[cfg(windows)]
const POWERSHELL: &str = "powershell.exe";

/// Arguments that keep that shell from reading a profile or asking questions.
#[cfg(windows)]
const POWERSHELL_FLAGS: [&str; 3] = ["-NoProfile", "-NonInteractive", "-Command"];

/// The user's persistent `Path`, unexpanded, or `None` when it is not set.
///
/// `GetEnvironmentVariable(.., 'User')` deliberately does not expand
/// `REG_EXPAND_SZ`, which is what makes a read-modify-write of this value safe:
/// writing back an expanded copy would freeze another installer's
/// `%USERPROFILE%` into a literal path.
///
/// The value comes back over a pipe, which PowerShell encodes in the console
/// codepage unless told otherwise — so a `Path` entry through `C:\Users\José`
/// would arrive as replacement characters and be written back that way. The
/// script sets UTF-8 first, without the byte-order mark that would otherwise
/// lead the first entry.
#[cfg(windows)]
fn read_user_path() -> Result<Option<String>, BinstallError> {
    const SCRIPT: &str = "\
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); \
        [Environment]::GetEnvironmentVariable('Path', 'User')";
    let output = std::process::Command::new(POWERSHELL)
        .args(POWERSHELL_FLAGS)
        .arg(SCRIPT)
        .output()
        .map_err(|error| BinstallError::UserPathUnreadable {
            detail: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(BinstallError::UserPathUnreadable {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let value = String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string();
    Ok((!value.is_empty()).then_some(value))
}

/// Writes `value` as the user's persistent `Path` and tells the desktop.
///
/// Two statements, both needed: the first is the one that broadcasts
/// `WM_SETTINGCHANGE`, and the second restores the expandable type it stores
/// the value under. `value` arrives in the environment rather than in the
/// script text, so a `Path` entry cannot be read as PowerShell syntax.
#[cfg(windows)]
fn write_user_path(value: &str) -> Result<(), BinstallError> {
    const SCRIPT: &str = "\
        [Environment]::SetEnvironmentVariable('Path', $env:KIRA_NEW_USER_PATH, 'User'); \
        Set-ItemProperty -Path 'HKCU:\\Environment' -Name 'Path' \
        -Value $env:KIRA_NEW_USER_PATH -Type ExpandString";
    let output = std::process::Command::new(POWERSHELL)
        .args(POWERSHELL_FLAGS)
        .arg(SCRIPT)
        .env("KIRA_NEW_USER_PATH", value)
        .output()
        .map_err(|error| BinstallError::UserPathUnwritable {
            detail: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(BinstallError::UserPathUnwritable {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_path_becomes_the_bin_directory_alone() {
        assert_eq!(
            user_path_with(None, r"C:\Users\dev\.kira\bin"),
            Some(r"C:\Users\dev\.kira\bin".to_string())
        );
    }

    #[test]
    fn the_bin_directory_goes_first_and_the_rest_survives() {
        assert_eq!(
            user_path_with(Some(r"C:\Windows;%USERPROFILE%\tools"), r"C:\kira\bin"),
            Some(r"C:\kira\bin;C:\Windows;%USERPROFILE%\tools".to_string())
        );
    }

    /// The bug this exists to keep dead: a second `sinstall` stacking a
    /// duplicate entry onto a `Path` that already names the bin directory.
    #[test]
    fn a_path_that_already_lists_the_bin_directory_is_left_alone() {
        assert_eq!(
            user_path_with(Some(r"C:\Windows;C:\kira\bin"), r"C:\kira\bin"),
            None
        );
    }

    /// Windows paths are case-insensitive, and the trailing separator, the
    /// quotes, and the padding other installers leave are not a difference.
    #[test]
    fn an_entry_is_recognized_however_it_was_spelled() {
        for spelling in [
            r"C:\KIRA\Bin",
            r"c:\kira\bin\",
            "\"C:\\kira\\bin\"",
            r"  C:\kira\bin  ",
        ] {
            assert_eq!(
                user_path_with(Some(&format!(r"C:\Windows;{spelling}")), r"C:\kira\bin"),
                None,
                "`{spelling}` names the same directory"
            );
        }
    }

    /// A `Path` ending in the separator Windows tolerates keeps its shape.
    #[test]
    fn a_trailing_separator_is_not_mistaken_for_an_entry() {
        assert_eq!(
            user_path_with(Some(r"C:\Windows;"), r"C:\kira\bin"),
            Some(r"C:\kira\bin;C:\Windows;".to_string())
        );
    }
}
