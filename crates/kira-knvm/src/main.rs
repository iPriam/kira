//! The `knvm` binary: parse argv, resolve the toolchains root, dispatch.
//!
//! Standalone tool crate at the binary layer. All logic lives in the library
//! target so it is reachable by tests; this file holds argv, stderr, and exit
//! codes.

use kira_knvm::{DirectoryReleaseSource, GitHubReleaseSource, KnvmCommand, ReleaseSource};

/// The operation ran.
const EXIT_OK: i32 = 0;
/// The operation was understood and failed.
const EXIT_FAILED: i32 = 1;
/// The invocation was not understood.
const EXIT_USAGE: i32 = 2;

/// The environment variable that points knvm at a local release directory
/// instead of GitHub — the offline install route, and the escape hatch when a
/// network is unavailable.
const RELEASE_DIR_VAR: &str = "KNVM_RELEASE_DIR";

fn main() {
    let paint = kira_knvm::Paint::auto();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let command = match kira_knvm::cli::parse(&arguments) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("knvm: {error}");
            eprintln!();
            eprint!("{}", kira_knvm::usage(paint));
            std::process::exit(EXIT_USAGE);
        }
    };
    std::process::exit(run(command, paint));
}

/// Runs a parsed command and returns the process exit code.
fn run(command: KnvmCommand, paint: kira_knvm::Paint) -> i32 {
    if matches!(command, KnvmCommand::Help) {
        print!("{}", kira_knvm::usage(paint));
        return EXIT_OK;
    }

    let toolchains_root = match kira_toolchain::toolchains_root() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("knvm: {error}");
            return EXIT_FAILED;
        }
    };

    match command {
        // Handled above; the root is resolved for every operating verb.
        KnvmCommand::Help => EXIT_OK,
        KnvmCommand::Overview => overview(paint, &toolchains_root),
        KnvmCommand::Install { spec, channel } => {
            let source = match release_source() {
                Ok(source) => source,
                Err(error) => {
                    eprintln!("knvm: {error}");
                    return EXIT_FAILED;
                }
            };
            match kira_knvm::install(&toolchains_root, source.as_ref(), &spec, channel) {
                Ok(installed) => {
                    let state = if installed.already_installed {
                        "already installed"
                    } else {
                        "installed"
                    };
                    println!(
                        "knvm: {state} {} {} at {}",
                        installed.channel.dir_name(),
                        installed.version,
                        installed.root.display()
                    );
                    println!("knvm: selected it; `kira` now dispatches to this toolchain");
                    EXIT_OK
                }
                Err(error) => {
                    eprintln!("knvm: {error}");
                    EXIT_FAILED
                }
            }
        }
        KnvmCommand::Binstall => {
            let start = match std::env::current_dir() {
                Ok(directory) => directory,
                Err(error) => {
                    eprintln!("knvm: could not read the working directory: {error}");
                    return EXIT_FAILED;
                }
            };
            match kira_knvm::binstall(&toolchains_root, &start) {
                Ok(installed) => {
                    let state = if installed.already_installed {
                        "rebuilt"
                    } else {
                        "built"
                    };
                    println!(
                        "knvm: {state} {} {} at {}",
                        installed.channel.dir_name(),
                        installed.version,
                        installed.root.display()
                    );
                    println!("knvm: selected it; `kira` now dispatches to this build");
                    EXIT_OK
                }
                Err(error) => {
                    eprintln!("knvm: {error}");
                    EXIT_FAILED
                }
            }
        }
        KnvmCommand::Sinstall => {
            let start = match std::env::current_dir() {
                Ok(directory) => directory,
                Err(error) => {
                    eprintln!("knvm: could not read the working directory: {error}");
                    return EXIT_FAILED;
                }
            };
            let kira_home = match kira_toolchain::kira_home() {
                Ok(home) => home,
                Err(error) => {
                    eprintln!("knvm: {error}");
                    return EXIT_FAILED;
                }
            };
            let Some(shell_home) = std::env::home_dir() else {
                eprintln!("knvm: no home directory, so no shell startup file to configure");
                return EXIT_FAILED;
            };
            let shell = std::env::var("SHELL").ok();
            match kira_knvm::sinstall(&kira_home, &shell_home, shell.as_deref(), &start) {
                Ok(installed) => {
                    println!(
                        "knvm: installed `knvm`, `kira`, and the `kira-language-server` \
                         alias into {}",
                        installed.bin_dir.display()
                    );
                    if installed.startup_file_updated {
                        println!(
                            "knvm: added a PATH line to {}",
                            installed.startup_file.display()
                        );
                    } else {
                        println!(
                            "knvm: {} already sources the env script",
                            installed.startup_file.display()
                        );
                    }
                    reload_shell(&installed.bin_dir, shell.as_deref());
                    EXIT_OK
                }
                Err(error) => {
                    eprintln!("knvm: {error}");
                    EXIT_FAILED
                }
            }
        }
        KnvmCommand::List => match kira_knvm::list(&toolchains_root) {
            Ok(installed) => {
                print_listing(paint, &installed);
                EXIT_OK
            }
            Err(error) => {
                eprintln!("knvm: {error}");
                EXIT_FAILED
            }
        },
        KnvmCommand::Use { version, channel } => {
            match kira_knvm::select(&toolchains_root, channel, &version) {
                Ok(selected) => {
                    if selected.was_already_current {
                        println!(
                            "knvm: {} {} was already selected",
                            selected.channel.dir_name(),
                            selected.version
                        );
                    } else {
                        println!(
                            "knvm: selected {} {} at {}",
                            selected.channel.dir_name(),
                            selected.version,
                            selected.root.display()
                        );
                    }
                    EXIT_OK
                }
                Err(error) => {
                    eprintln!("knvm: {error}");
                    EXIT_FAILED
                }
            }
        }
        KnvmCommand::Uninstall { version, channel } => {
            match kira_knvm::uninstall(&toolchains_root, channel, &version) {
                Ok(removed) => {
                    println!(
                        "knvm: removed {} {} from {}",
                        removed.channel.dir_name(),
                        removed.version,
                        removed.root.display()
                    );
                    if removed.was_current {
                        eprintln!(
                            "knvm: warning: that was the selected toolchain; nothing is \
                             selected now. Run `knvm use <version>` or `knvm install latest`"
                        );
                    }
                    EXIT_OK
                }
                Err(error) => {
                    eprintln!("knvm: {error}");
                    EXIT_FAILED
                }
            }
        }
    }
}

/// Replaces this process with a fresh login shell that has the tools on PATH.
///
/// A child process cannot change its parent shell's PATH, so "reload" means
/// starting a shell that already has it: the freshly installed `knvm` and
/// `kira` work immediately, and every later shell picks the same PATH up from
/// the startup file. Skipped when stdout is not a terminal — a script driving
/// `knvm sinstall` wants its exit code, not an interactive shell — or when the
/// shell is unknown; `exec` failing is reported and the install still counts.
fn reload_shell(bin_dir: &std::path::Path, shell: Option<&str>) {
    use std::io::IsTerminal as _;
    #[cfg(unix)]
    {
        let Some(shell) = shell else { return };
        if !std::io::stdout().is_terminal() {
            return;
        }
        let path = match std::env::var("PATH") {
            Ok(current) => format!("{}:{current}", bin_dir.display()),
            Err(_) => bin_dir.display().to_string(),
        };
        println!("knvm: starting a fresh {shell} with the tools on PATH");
        use std::os::unix::process::CommandExt as _;
        let error = std::process::Command::new(shell)
            .arg("-l")
            .env("PATH", path)
            .exec();
        eprintln!("knvm: could not start {shell}: {error}; open a new terminal instead");
    }
    #[cfg(not(unix))]
    {
        let _ = (bin_dir, shell);
        println!("knvm: open a new terminal to pick the PATH up");
    }
}

/// The bare `knvm` screen: the usage text, with a first-run greeting when
/// nothing is installed yet. Exit 0 either way — asking the front door what is
/// behind it is not an error.
fn overview(paint: kira_knvm::Paint, toolchains_root: &std::path::Path) -> i32 {
    if let Ok(installed) = kira_knvm::list(toolchains_root)
        && installed.is_empty()
    {
        println!(
            "{} No Kira toolchain is installed yet; {} fetches and selects one.",
            paint.bold("Welcome!"),
            paint.cyan("knvm install latest")
        );
        println!();
    }
    print!("{}", kira_knvm::usage(paint));
    EXIT_OK
}

/// Renders the installed toolchains, grouped by channel, `*` on the selected one.
fn print_listing(paint: kira_knvm::Paint, installed: &[kira_knvm::InstalledToolchain]) {
    if installed.is_empty() {
        println!("knvm: no toolchains installed; run `knvm install latest`");
        return;
    }

    let mut channel_shown = None;
    for toolchain in installed {
        let channel = toolchain.channel.dir_name();
        if channel_shown != Some(channel) {
            println!("{}", paint.bold(&format!("{channel}:")));
            channel_shown = Some(channel);
        }
        let line = if toolchain.is_current {
            paint.green(&format!("  * {}", toolchain.version))
        } else {
            format!("    {}", toolchain.version)
        };
        let note = if toolchain.is_complete {
            String::new()
        } else {
            paint.yellow("  (broken: no bin/kirac)")
        };
        println!("{line}{note}");
    }
}

/// The source releases are fetched from: a local directory when
/// `KNVM_RELEASE_DIR` names one, GitHub otherwise.
fn release_source() -> Result<Box<dyn ReleaseSource>, kira_knvm::ReleaseSourceError> {
    if let Some(directory) = std::env::var_os(RELEASE_DIR_VAR)
        && !directory.is_empty()
    {
        return Ok(Box::new(DirectoryReleaseSource::new(
            std::path::PathBuf::from(directory),
        )?));
    }
    Ok(Box::new(GitHubReleaseSource::for_host()?))
}
