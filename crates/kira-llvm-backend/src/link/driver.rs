//! Everything the linker driver is handed that is not an input file.
//!
//! The flags, the symbol-forcing spellings, the response file, and the runtime
//! files staged beside the output. They are gathered here because each of them
//! is a per-platform decision, and the platform they must follow is the one
//! being *built for* — [`NativeBuildTarget`] answers that, and a host build gets
//! the same answers it always did.

use std::path::{Path, PathBuf};

use kira_native_lib_definition::NativeLinkInputs;

use super::LinkError;
use super::target::NativeBuildTarget;

/// Everything a tool said, whichever stream it said it on.
///
/// Reading only `stderr` loses the whole diagnostic under MSVC, where
/// `link.exe` writes `LNK2001`/`LNK1120` to *stdout* and leaves stderr empty —
/// so a failed Windows link reported nothing but an exit code, and the
/// stale-marker case could never recognise itself either. Unix linkers use
/// stderr, so joining the two costs nothing there and is the difference between
/// a diagnosis and a number here.
pub(super) fn tool_diagnostic(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|stream| !stream.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// How the target's driver spells "produce a shared library".
///
/// Apple's linker spells it differently from everyone else's, and the answer
/// follows the machine being built for rather than the one building.
pub(super) fn shared_library_flag(target: &NativeBuildTarget) -> &'static str {
    if target.is_macos() {
        "-dynamiclib"
    } else {
        "-shared"
    }
}

/// Makes dependencies staged beside a native live library visible to its loader
/// on the platforms that need an explicit relative search path.
///
/// A live library is loaded by this process, so this is a host question and
/// stays one.
pub(super) fn native_live_runtime_arguments() -> Vec<String> {
    if cfg!(target_os = "macos") {
        vec!["-Wl,-rpath,@loader_path".to_owned()]
    } else if cfg!(unix) {
        vec!["-Wl,-rpath,$ORIGIN".to_owned()]
    } else {
        Vec::new()
    }
}

/// Makes libraries staged BESIDE a linked program visible to its loader.
///
/// A Kira program that imports a dynamic native library — Dawn, say — is linked
/// against it by name and the library is copied next to the executable. Nothing
/// in that arrangement tells the loader to look next to the executable, so the
/// program builds, ships its own dependency, and then refuses to start with
/// `cannot open shared object file` unless the caller happens to set
/// `LD_LIBRARY_PATH`. Having to set it is the bug: the library is right there.
///
/// Asked of the TARGET rather than the host, because a cross link produces an
/// image whose loader is the target's — `$ORIGIN` is what ELF spells this and
/// `@loader_path` is what Mach-O does, and Windows resolves a DLL beside the
/// executable already, so it needs nothing.
pub(super) fn staged_library_runtime_arguments(target: &NativeBuildTarget) -> Vec<String> {
    if target.is_macos() {
        return vec!["-Wl,-rpath,@loader_path".to_owned()];
    }
    if target.is_windows() {
        return Vec::new();
    }
    vec!["-Wl,-rpath,$ORIGIN".to_owned()]
}

/// Exports Kira body names from a Windows native image so LLDB can resolve
/// them even when the platform linker keeps native debug records in a separate
/// PDB without translating every subprogram name to the exported symbol.
pub(super) fn export_debug_symbols(target: &NativeBuildTarget, symbols: &[String]) -> Vec<String> {
    if !target.is_msvc() {
        return Vec::new();
    }
    symbols
        .iter()
        .map(|symbol| format!("-Wl,/export:{symbol}"))
        .collect()
}

/// The linker flags that retain callback thunks a foreign C library reaches
/// only through a function pointer.
pub(super) fn force_callback_symbols(
    target: &NativeBuildTarget,
    callback_symbols: &[String],
) -> Vec<String> {
    force_symbols(target, callback_symbols.iter().cloned())
}

/// Linker flags that pull the host-facing runtime symbols into a shared library.
///
/// A linker pulls only *referenced* members out of an archive, and nothing in
/// generated code references these: the host calls
/// `kira_hybrid_install_runtime_invoker` itself, and the string helpers are
/// reached only by a program that happens to use strings. Without this, a
/// hybrid library built from a program with no strings in its native half
/// carries no `kira_rt_str_new` — and the host's `dlsym` fails on a library
/// that is otherwise perfectly good.
///
/// Each symbol is requested as an undefined one, which makes the linker pull in
/// exactly the member defining it. That is deliberately narrower than
/// force-loading the whole archive (`-force_load` / `--whole-archive`), which
/// would drag every unreferenced member of a Rust `staticlib` — the entire
/// standard library — into every hybrid program.
///
/// `kira_runtime_abi::HYBRID_HOST_SYMBOLS` is the same list the host resolves
/// from, so what is forced in and what is looked up cannot drift.
pub(super) fn force_host_symbols(target: &NativeBuildTarget) -> Vec<String> {
    force_symbols(
        target,
        kira_runtime_abi::HYBRID_HOST_SYMBOLS
            .iter()
            .map(|s| (*s).to_owned()),
    )
}

/// Linker flags that bind a hybrid library's own calls to its own definitions.
///
/// The host executable carries `kira-native-bridge` too — `kira-cli` depends on
/// it so cargo builds the staticlib — so both images define
/// `kira_hybrid_call_runtime` and each has its own invoker slot. On ELF the
/// executable is searched first, so the library's *internal* call to it binds to
/// the host's copy, while the host installs the invoker into the library's,
/// because it resolves that symbol through `dlsym` on the library handle. The
/// slot written is never the slot read, and the first `@Native` function to call
/// a `@Runtime` one aborts on an invoker that was installed all along.
///
/// `kira-hybrid-runtime` states the rule this restores: every runtime symbol
/// must resolve out of the loaded library, never out of this process's own copy.
///
/// Mach-O's two-level namespace and PE's import tables already bind a library's
/// own calls to its own definitions, so this is an ELF-only correction.
pub(super) fn bind_own_symbols_locally(target: &NativeBuildTarget) -> Vec<String> {
    if target.is_macos() || target.is_msvc() {
        Vec::new()
    } else {
        vec!["-Wl,-Bsymbolic".to_owned()]
    }
}

/// Spells "pull this symbol's definition in, and let the host find it by name"
/// for the target platform's linker.
///
/// One definition rather than one per caller: the hybrid library and the VM's
/// adapter sidecar have the same requirement — a shared library whose symbols
/// are resolved by `dlsym`/`GetProcAddress` and referenced from nowhere inside
/// it — and when only one of them knew the PE/COFF spelling, the sidecar linked
/// clean on Windows and then failed to produce its ABI marker.
///
/// PE/COFF needs a second flag that the other two formats do not. Pulling an
/// archive member in is the whole job on Mach-O and ELF, where a definition is
/// exported by default; a DLL exports *nothing* it was not explicitly told to.
/// So the member is forced in with `/INCLUDE:` and then named again in
/// `/EXPORT:`, or the library links clean, holds the code, and resolves nothing
/// by name — which is exactly what the host reported: `app.dll` "does not
/// export `kira_rt_str_new`".
pub(super) fn force_symbols(
    target: &NativeBuildTarget,
    names: impl IntoIterator<Item = String>,
) -> Vec<String> {
    names
        .into_iter()
        .flat_map(|symbol| {
            if target.is_macos() {
                // Mach-O prefixes C symbols with an underscore; ELF does not.
                vec![format!("-Wl,-u,_{symbol}")]
            } else if target.is_msvc() {
                // No leading underscore: that is the 32-bit x86 convention, and
                // the 64-bit Windows ABI drops it.
                vec![
                    format!("-Wl,/INCLUDE:{symbol}"),
                    format!("-Wl,/EXPORT:{symbol}"),
                ]
            } else {
                vec![format!("-Wl,--undefined={symbol}")]
            }
        })
        .collect()
}

/// The system libraries the Rust `staticlib` runtime needs on the machine being
/// built for, spelled as driver arguments.
///
/// The names come from [`crate::platform`], which is the one place they are
/// written down — the generated `build.rs` a consumer links through reads the
/// same list, so a library added for one is added for both. The list is chosen
/// by the target's operating system rather than this one's, which is the whole
/// reason that module is keyed by `target_os` instead of by host.
pub(super) fn platform_link_arguments(target: &NativeBuildTarget) -> Vec<String> {
    let list = crate::platform::link_list_for(target.target_os());
    let mut arguments: Vec<String> = list
        .libraries
        .iter()
        // `gcc_s` is the *shared* unwinder, and a static link has no way to use
        // one: the whole point of the link is that nothing is resolved at
        // startup. clang supplies the static unwinder itself under `-static`,
        // so naming this one asks the linker for a file that a freestanding
        // target has no reason to ship — and the failure names the library
        // rather than the linkage that made it unusable.
        .filter(|library| !(**library == "gcc_s" && target.is_statically_linked()))
        .map(|library| format!("-l{library}"))
        .collect();
    for framework in list.frameworks {
        arguments.push("-framework".to_owned());
        arguments.push((*framework).to_owned());
    }
    arguments
}

/// The stack reserve for a generated executable on Windows.
///
/// Kira lowers aggregates and construct dispatch explicitly. A large UI can
/// therefore have a deeper, wider call graph than the 1 MiB PE default. Put
/// the reserve on the executable itself so a launcher does not need a Kira
/// runtime-specific stack setting. Shared libraries and sidecars do not get
/// this flag: their host owns the thread stack.
///
/// The size is [`kira_toolchain::WINDOWS_STACK_RESERVE`], which the toolchain's
/// own binaries also reserve.
pub(super) fn executable_stack_arguments(target: &NativeBuildTarget) -> Vec<String> {
    if target.is_windows() {
        vec![format!(
            "-Wl,/STACK:{}",
            kira_toolchain::WINDOWS_STACK_RESERVE
        )]
    } else {
        Vec::new()
    }
}

/// Flags that make the target's linker write the same bytes for the same
/// inputs.
///
/// Kira compares native library bytes when selecting its live-reload tier. On
/// MSVC, `/Brepro` makes PE output reproducible and `/INCREMENTAL:NO` keeps
/// linker and PDB layouts stable for debugger addresses. Without these flags,
/// timestamp or incremental-link metadata could make unchanged inputs look
/// different.
pub(super) fn reproducible_link_arguments(target: &NativeBuildTarget) -> Vec<String> {
    if target.is_msvc() {
        vec!["-Wl,/Brepro".to_owned(), "-Wl,/INCREMENTAL:NO".to_owned()]
    } else {
        Vec::new()
    }
}

/// The longest command line this platform will accept, with room to spare.
///
/// Windows caps a process's whole command line at 32,767 characters, and the
/// driver's own path counts against it. A package binding a large C API is not
/// an exotic case: kira-graphics declares roughly eight hundred foreign imports
/// across Vulkan and Direct3D, each of which is forced into the link by name —
/// and on PE by *two* names, since a symbol must be kept and then exported. That
/// is well past the cap, and the failure is `os error 206`, which names the
/// length and nothing about linking.
const MAX_COMMAND_LINE: usize = 30_000;

/// Writes `arguments` to a response file when they are too long to pass directly.
///
/// Returns `None` when the command line fits, which is the common case and
/// leaves the invocation exactly as it was.
///
/// The file sits beside the artifact being linked, so a failed link leaves the
/// exact argument list behind to read.
pub(super) fn response_file_for(
    arguments: &[std::ffi::OsString],
    output: &Path,
) -> Result<Option<PathBuf>, LinkError> {
    let length: usize = arguments.iter().map(|a| a.len() + 1).sum();
    if length <= MAX_COMMAND_LINE {
        return Ok(None);
    }

    let path = output.with_extension("rsp");
    let body: String = arguments
        .iter()
        .map(|argument| quote_response_argument(&argument.to_string_lossy()))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, body).map_err(|source| LinkError::ArchiveUnwritable {
        path: path.clone(),
        source,
    })?;
    Ok(Some(path))
}

/// Quotes one argument for a clang response file.
///
/// clang tokenizes a response file the GNU way even on Windows, where a
/// backslash escapes the character after it. A Windows path written verbatim
/// therefore arrives with its separators eaten — `C:\Users\x` reaches the
/// linker as `C:Usersx`, and the verbatim `\\?\C:\...` prefix canonicalization
/// produces becomes `?C:...`. So every backslash is doubled, and every argument
/// is quoted besides, which covers the spaces `C:\Program Files` guarantees.
fn quote_response_argument(argument: &str) -> String {
    let plain = strip_verbatim_prefix(argument);
    let escaped = plain.replace('\\', r"\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Drops Windows' `\\?\` verbatim prefix from a path.
///
/// Canonicalizing a path on Windows returns one, and `link.exe` does not accept
/// it for an input library: the archive is handed over, opened by nobody, and
/// every symbol it defines comes back undefined — 1,062 of them for a package
/// binding sokol, with no diagnostic naming the file. Everything downstream
/// takes the ordinary form, so the prefix is dropped rather than carried.
fn strip_verbatim_prefix(path: &str) -> &str {
    path.strip_prefix(r"\\?\").unwrap_or(path)
}

/// Puts every declared runtime file beside the link output.
///
/// The loader searches the directory the program was loaded from first, so a
/// shared library that sits there is found without a `PATH` entry, an install
/// step, or a `@rpath`. A directory is taken as its files, one level deep:
/// `NativeLibs/Dawn/<triple>/bin` is how a release payload ships, and naming its
/// three DLLs one at a time would go stale the moment the payload gains a
/// fourth.
///
/// Copied on every link rather than only when missing: the declared file is the
/// truth, and a stale copy beside a freshly linked program is the failure this
/// exists to prevent.
pub(super) fn stage_runtime_files(
    foreign_link: &NativeLinkInputs,
    output: &Path,
) -> Result<(), LinkError> {
    if output.parent().is_none() {
        return Ok(());
    }
    for declared in foreign_link.runtime_files() {
        let sources = if declared.is_dir() {
            let entries = std::fs::read_dir(declared).map_err(|source| {
                LinkError::RuntimeFileUnplaceable {
                    source_path: declared.clone(),
                    output: output.to_path_buf(),
                    source,
                }
            })?;
            let mut files = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|source| LinkError::RuntimeFileUnplaceable {
                    source_path: declared.clone(),
                    output: output.to_path_buf(),
                    source,
                })?;
                if entry.path().is_file() {
                    files.push(entry.path());
                }
            }
            files
        } else {
            vec![declared.clone()]
        };
        for file in sources {
            stage_runtime_file(&file, output)?;
        }
    }
    Ok(())
}

pub(super) fn stage_runtime_file(file: &Path, output: &Path) -> Result<(), LinkError> {
    let Some(directory) = output.parent() else {
        return Ok(());
    };
    let Some(name) = file.file_name() else {
        return Ok(());
    };
    let destination = directory.join(name);
    // A program still running from this directory holds its own copy open, and
    // replacing a file the loader has mapped is refused. Identical bytes need
    // no replacement.
    if destination.is_file() && same_file_contents(file, &destination) {
        return Ok(());
    }
    std::fs::create_dir_all(directory)
        .and_then(|()| std::fs::copy(file, &destination))
        .map(|_| ())
        .map_err(|source| LinkError::RuntimeFileUnplaceable {
            source_path: file.to_path_buf(),
            output: output.to_path_buf(),
            source,
        })
}

/// Whether two files hold the same bytes, for deciding a copy can be skipped.
///
/// A read failure answers `false`, so an unreadable destination is replaced
/// rather than trusted.
fn same_file_contents(left: &Path, right: &Path) -> bool {
    match (std::fs::metadata(left), std::fs::metadata(right)) {
        (Ok(left_meta), Ok(right_meta)) if left_meta.len() == right_meta.len() => {
            matches!(
                (std::fs::read(left), std::fs::read(right)),
                (Ok(a), Ok(b)) if a == b
            )
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_backend_api::{CrossTarget, Linkage, NativeTarget, RelocationModel};
    use kira_native_lib_definition::TargetTriple;

    fn cross(text: &str) -> NativeBuildTarget {
        NativeBuildTarget::new(
            NativeTarget::Cross(CrossTarget::new(
                TargetTriple::parse(text).expect("a valid triple"),
                RelocationModel::Pic,
                Linkage::Dynamic,
            )),
            None,
        )
    }

    /// Every host-facing symbol is forced in, in the spelling this host's
    /// linker takes — and on Windows it is also exported, which is the step
    /// the other two formats get for free.
    #[test]
    fn every_host_symbol_is_forced_in_this_platforms_spelling() {
        let host = NativeBuildTarget::host();
        let flags = force_host_symbols(&host);
        for symbol in kira_runtime_abi::HYBRID_HOST_SYMBOLS {
            let forced = flags.iter().any(|flag| flag.contains(symbol));
            assert!(forced, "`{symbol}` is never forced into the link");
            if host.is_msvc() {
                assert!(
                    flags.contains(&format!("-Wl,/EXPORT:{symbol}")),
                    "`{symbol}` is pulled in but never exported, so no host can \
                     resolve it out of the DLL"
                );
            }
        }
    }

    /// The symbol-forcing spelling follows the object format being produced.
    /// Emitting the host's spelling into a cross link is how an ELF build ends
    /// up carrying `link.exe` flags that clang passes straight through.
    #[test]
    fn the_symbol_spelling_follows_the_target_object_format() {
        let one = ["kira_rt_str_new".to_owned()];
        assert_eq!(
            force_symbols(&cross("aarch64-linux-gnu"), one.iter().cloned()),
            ["-Wl,--undefined=kira_rt_str_new"]
        );
        assert_eq!(
            force_symbols(&cross("aarch64-macos-none"), one.iter().cloned()),
            ["-Wl,-u,_kira_rt_str_new"]
        );
        assert_eq!(
            force_symbols(&cross("x86_64-windows-msvc"), one.iter().cloned()),
            [
                "-Wl,/INCLUDE:kira_rt_str_new",
                "-Wl,/EXPORT:kira_rt_str_new"
            ]
        );
    }

    /// The platform library list is the target's. An aarch64 Linux program
    /// linked with the Windows import libraries this host needs is a link that
    /// fails naming a dozen `.lib` files that were never going to be there.
    #[test]
    fn the_platform_libraries_are_the_targets_own() {
        let arguments = platform_link_arguments(&cross("aarch64-linux-gnu"));
        assert!(arguments.contains(&"-lpthread".to_owned()), "{arguments:?}");
        assert!(arguments.contains(&"-ldl".to_owned()), "{arguments:?}");
        assert!(arguments.contains(&"-lgcc_s".to_owned()), "{arguments:?}");
        assert!(!arguments.iter().any(|argument| argument == "-lkernel32"));
    }

    /// A static link drops the shared unwinder and keeps everything else.
    ///
    /// `-lgcc_s` names a shared object, so asking for it in a link that resolves
    /// nothing at startup fails on a missing file — and the message names
    /// `libgcc_s`, not the linkage that made it impossible. clang adds the
    /// static unwinder itself once `-static` is on the line.
    #[test]
    fn a_static_link_drops_the_shared_unwinder() {
        let target = NativeBuildTarget::new(
            NativeTarget::Cross(CrossTarget::new(
                TargetTriple::parse("aarch64-linux-gnu").expect("a valid triple"),
                RelocationModel::Static,
                Linkage::Static,
            )),
            None,
        );
        let arguments = platform_link_arguments(&target);
        assert!(!arguments.iter().any(|argument| argument == "-lgcc_s"));
        assert!(arguments.contains(&"-lpthread".to_owned()), "{arguments:?}");
        assert!(arguments.contains(&"-lutil".to_owned()), "{arguments:?}");
    }

    /// A Windows path survives the response file's own escaping.
    ///
    /// This is the whole reason arguments are quoted rather than written
    /// verbatim: clang eats a lone backslash, so `C:\Users\x` would reach the
    /// linker as `C:Usersx` and fail on a file that is plainly there.
    #[test]
    fn a_response_argument_keeps_its_backslashes() {
        assert_eq!(
            quote_response_argument(r"C:\Users\x\main.o"),
            r#""C:\\Users\\x\\main.o""#
        );
        assert_eq!(
            quote_response_argument(r"C:\Program Files\lib.a"),
            r#""C:\\Program Files\\lib.a""#
        );
        assert_eq!(
            quote_response_argument(r"\\?\C:\a\sokol.lib"),
            r#""C:\\a\\sokol.lib""#,
            "the verbatim prefix is dropped: link.exe opens no archive named with one"
        );
        assert_eq!(
            quote_response_argument("-Wl,/EXPORT:sym"),
            "\"-Wl,/EXPORT:sym\""
        );
    }

    /// A short link is left exactly as it was — no file, no indirection.
    #[test]
    fn a_short_command_line_needs_no_response_file() {
        let arguments = vec![std::ffi::OsString::from("main.o")];
        let response = response_file_for(&arguments, Path::new("out"))
            .expect("a short line is never an error");
        assert!(response.is_none());
    }

    #[test]
    fn every_host_link_argument_is_a_library_or_framework_flag() {
        // Guards against a stray path or empty string sneaking into the link
        // line, which the driver would silently treat as an input file.
        let frameworks = crate::platform::host_link_list().frameworks;
        for argument in platform_link_arguments(&NativeBuildTarget::host()) {
            assert!(
                argument.starts_with('-') || frameworks.contains(&argument.as_str()),
                "unexpected link argument `{argument}`",
            );
        }
    }
}
