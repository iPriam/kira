//! Locating the LLDB executables and the libraries they load at startup.
//!
//! A Swift toolchain's `lldb.exe` links the Swift runtime and delay-loads
//! CPython, and neither lives beside it: the runtime is installed under
//! `Runtimes/<version>/usr/bin` next to `Toolchains`, and Python is installed
//! per user. Started with only its own directory reachable, the process fails
//! in the loader with `0xC0000135` before it prints anything a caller could
//! act on, so the search happens here and the result is put on the child's
//! `PATH`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The environment variable naming the LLDB command-line executable.
pub const LLDB_VARIABLE: &str = "KIRA_LLDB";
/// The environment variable naming the LLDB Debug Adapter Protocol executable.
pub const LLDB_DAP_VARIABLE: &str = "KIRA_LLDB_DAP";

/// Which LLDB frontend a launch needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// The command-line frontend, driven with `-o` commands.
    CommandLine,
    /// The Debug Adapter Protocol frontend, driven with JSON requests.
    DebugAdapter,
}

impl Engine {
    /// The environment variable that overrides this frontend's executable.
    #[must_use]
    pub const fn variable(self) -> &'static str {
        match self {
            Self::CommandLine => LLDB_VARIABLE,
            Self::DebugAdapter => LLDB_DAP_VARIABLE,
        }
    }

    /// The executable name looked up on `PATH` when nothing overrides it.
    #[must_use]
    pub const fn default_executable(self) -> &'static str {
        match self {
            Self::CommandLine => "lldb",
            Self::DebugAdapter => "lldb-dap",
        }
    }

    /// The executable this frontend will run.
    ///
    /// The override variable wins; otherwise the bare name is used when `PATH`
    /// resolves it, and on macOS `xcrun` is asked as the fallback — Xcode
    /// ships `lldb-dap` inside the developer directory without linking it
    /// into `/usr/bin`, so a stock machine has one that no `PATH` lookup
    /// finds.
    #[must_use]
    pub fn executable(self) -> PathBuf {
        if let Some(overridden) = std::env::var_os(self.variable()) {
            return PathBuf::from(overridden);
        }
        let name = self.default_executable();
        if on_path(name) {
            return PathBuf::from(name);
        }
        developer_tool(name).unwrap_or_else(|| PathBuf::from(name))
    }
}

/// Whether `name` resolves through `PATH`.
fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| directory.join(name).is_file())
}

/// The developer-directory copy of `name`, as `xcrun` reports it.
///
/// `None` off macOS, when `xcrun` is absent, or when it does not know the
/// tool — each of which leaves the bare name to fail with its own message.
fn developer_tool(name: &str) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = Command::new("xcrun").args(["-f", name]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = PathBuf::from(path.trim());
    path.is_file().then_some(path)
}

/// Prepares `command` so the LLDB executable can load its own libraries.
pub fn configure(command: &mut Command, executable: &Path) {
    let extra = support_directories(executable);
    if extra.is_empty() {
        return;
    }
    let mut paths = extra;
    if let Some(current) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        command.env("PATH", joined);
    }
}

/// The directories that must precede `PATH` for `executable` to start.
#[must_use]
pub fn support_directories(executable: &Path) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    for directory in swift_runtime_directories(executable) {
        push_unique(&mut directories, directory);
    }
    if let Some(home) = python_home() {
        push_unique(&mut directories, home);
    }
    directories
}

fn push_unique(directories: &mut Vec<PathBuf>, directory: PathBuf) {
    if !directories.contains(&directory) {
        directories.push(directory);
    }
}

/// The Swift runtime `bin` directories belonging to `executable`'s install.
///
/// A Swift installation places `Toolchains` and `Runtimes` side by side, and
/// their version directories do not match: a toolchain may be `6.1.2+Asserts`
/// while its runtime is `6.1.2`. Every installed runtime is therefore offered,
/// newest name last so the loader prefers it.
fn swift_runtime_directories(executable: &Path) -> Vec<PathBuf> {
    let Some(root) = installation_root(executable) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(root.join("Runtimes")) else {
        return Vec::new();
    };
    let mut directories = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("usr").join("bin"))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    directories.sort();
    directories
}

/// The directory holding `Toolchains`, found by walking up from `executable`.
fn installation_root(executable: &Path) -> Option<PathBuf> {
    let resolved = resolve_on_path(executable)?;
    resolved
        .ancestors()
        .find(|ancestor| ancestor.file_name() == Some("Toolchains".as_ref()))
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// The full path of `executable`, searching `PATH` when it carries no directory.
///
/// A bare `lldb` says nothing about which installation will answer it, and the
/// runtime directories are derived from where the file actually is.
fn resolve_on_path(executable: &Path) -> Option<PathBuf> {
    if executable.components().count() > 1 {
        return Some(executable.to_path_buf());
    }
    let name = executable.file_name()?;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|directory| {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        for extension in executable_extensions() {
            let mut named = OsString::from(name);
            named.push(extension);
            let candidate = directory.join(named);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    })
}

#[cfg(windows)]
fn executable_extensions() -> &'static [&'static str] {
    &[".exe"]
}

#[cfg(not(windows))]
fn executable_extensions() -> &'static [&'static str] {
    &[]
}

/// The Python installation LLDB's scripting support delay-loads.
///
/// `KIRA_PYTHON_HOME` and `PYTHONHOME` are explicit overrides; the standard
/// per-user location is the fallback for the version Swift's LLDB links.
#[cfg(windows)]
fn python_home() -> Option<PathBuf> {
    const LIBRARY: &str = "python39.dll";
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("KIRA_PYTHON_HOME") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("PYTHONHOME") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local_app_data)
                .join("Programs")
                .join("Python")
                .join("Python39"),
        );
    }
    candidates
        .into_iter()
        .find(|path| path.join(LIBRARY).is_file())
}

#[cfg(not(windows))]
fn python_home() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_frontend_names_its_own_override_and_executable() {
        assert_eq!(Engine::CommandLine.variable(), LLDB_VARIABLE);
        assert_eq!(Engine::DebugAdapter.variable(), LLDB_DAP_VARIABLE);
        assert_eq!(Engine::CommandLine.default_executable(), "lldb");
        assert_eq!(Engine::DebugAdapter.default_executable(), "lldb-dap");
    }

    /// The runtime directories are found from the toolchain layout alone, so a
    /// checkout with no Swift installation still proves the derivation.
    #[test]
    fn runtime_directories_are_read_from_the_installation_beside_toolchains() {
        let root = std::env::temp_dir().join(format!("kira-debug-engine-{}", std::process::id()));
        let toolchain = root.join("Toolchains").join("6.1.2+Asserts").join("usr");
        let runtime = root.join("Runtimes").join("6.1.2").join("usr").join("bin");
        std::fs::create_dir_all(toolchain.join("bin")).expect("toolchain layout");
        std::fs::create_dir_all(&runtime).expect("runtime layout");
        let executable = toolchain.join("bin").join("lldb");
        std::fs::write(&executable, b"").expect("executable");

        assert_eq!(swift_runtime_directories(&executable), vec![runtime]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_executable_outside_a_swift_installation_needs_no_runtime_directory() {
        assert!(swift_runtime_directories(Path::new("/usr/bin/lldb")).is_empty());
    }
}
