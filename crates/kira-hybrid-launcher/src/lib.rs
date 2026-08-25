//! What a standalone hybrid program invocation means, decided before anything
//! runs.
//!
//! `kira build --backend hybrid` stages this crate's binary as the program's
//! executable beside its bundle: `<stem>`, `<stem>.khm`, `<stem>.kbc`, and the
//! native shared library share one directory, and the four of them are the whole
//! deployment. The staged copy is entered like any program; what it must decide
//! first is *which bundle to run*, because the manifest is how everything else —
//! the bytecode payload, the native half, the entrypoint's engine — is found.
//!
//! Two ways to say it, in this order:
//!
//! 1. **The first argument that names a `.khm` file.** An explicit answer for a
//!    directory that holds more than one bundle.
//! 2. **The manifest beside the executable.** The layout every Kira build
//!    writes: one program, one bundle directory. Exactly one `.khm` beside the
//!    executable is the answer no matter what either file is called — renaming
//!    the executable to ship it does not break the pairing, because the
//!    *directory* is the deployment unit, not the name. Zero or several
//!    manifests are refused rather than guessed at, and the refusal names what
//!    was seen and how to point at the right one outright.
//!
//! Every argument after the manifest goes to the program. Arguments before a
//! `.khm`-named argument cannot exist — the first argument decides — so there is
//! no flag grammar here to disagree with a program's own flags.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// One resolved invocation: the bundle to load, and the arguments the program
/// sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The hybrid manifest naming the two payloads and the entrypoint.
    pub manifest: PathBuf,
    /// Everything the program itself was given, manifest path excluded.
    pub program_arguments: Vec<String>,
}

/// Why an invocation could not be resolved into an [`Invocation`].
#[derive(Debug, thiserror::Error)]
pub enum InvocationError {
    /// `argv[0]` named nothing this can derive a search from.
    #[error("cannot locate this executable, so there is nowhere to look for a manifest")]
    NoExecutableName,
    /// No `.khm` sits beside the executable.
    #[error(
        "no hybrid manifest was found beside this executable\n\
         note: a standalone hybrid program is its bundle directory — the \
         executable, the `.khm`, the `.kbc`, and the native shared library\n\
         note: pass the manifest's path as the first argument to run one \
         somewhere else"
    )]
    NoManifestNearby {
        /// Where the search happened.
        directory: PathBuf,
    },
    /// Several `.khm` files sit beside the executable.
    #[error(
        "several hybrid manifests sit beside this executable\n\
         note: {manifests}\n\
         note: pass the one you mean as the first argument",
        manifests = manifests
            .iter()
            .map(|path| format!("`{}`", path.display()))
            .collect::<Vec<_>>()
            .join(", ")
    )]
    AmbiguousManifest {
        /// Every candidate, in directory order.
        manifests: Vec<PathBuf>,
    },
    /// A program argument is not valid UTF-8.
    ///
    /// Kira strings are UTF-8 end to end, so an argument that is not UTF-8
    /// cannot reach the program intact. Mangling it silently would hand the
    /// program something its caller never wrote.
    #[error("program argument {index} is not valid UTF-8")]
    ProgramArgumentNotUtf8 {
        /// Which argument, counting from the first after the manifest.
        index: usize,
    },
}

/// Resolves `arguments` against the running executable at `executable`.
///
/// `arguments` excludes `argv[0]`; `executable` is the path of this process's
/// own image. Splitting the resolution out from reading either is what makes it
/// testable without spawning processes.
pub fn resolve(arguments: &[OsString], executable: &Path) -> Result<Invocation, InvocationError> {
    let (manifest, program_arguments) = match arguments.first() {
        Some(first) if manifest_named(first) => (PathBuf::from(first), &arguments[1..]),
        _ => (manifest_beside(executable)?, arguments),
    };

    let mut program_arguments_text = Vec::with_capacity(program_arguments.len());
    for (index, argument) in program_arguments.iter().enumerate() {
        let text = argument
            .to_str()
            .ok_or(InvocationError::ProgramArgumentNotUtf8 { index: index + 1 })?;
        program_arguments_text.push(text.to_owned());
    }

    Ok(Invocation {
        manifest,
        program_arguments: program_arguments_text,
    })
}

/// Whether an argument names a hybrid manifest rather than a program argument.
fn manifest_named(argument: &OsString) -> bool {
    Path::new(argument)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("khm"))
}

/// The bundle's manifest beside `executable`: the only `.khm` in its directory.
///
/// A manifest named after the executable wins when there are several, because
/// that pairing is the one a build wrote and a rename did not undo.
fn manifest_beside(executable: &Path) -> Result<PathBuf, InvocationError> {
    let directory = executable
        .parent()
        .ok_or(InvocationError::NoExecutableName)?;
    let stem = executable
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned());
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(directory)
        .map_err(|_| InvocationError::NoManifestNearby {
            directory: directory.to_path_buf(),
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("khm"))
        })
        .collect();
    candidates.sort();
    match candidates.len() {
        0 => Err(InvocationError::NoManifestNearby {
            directory: directory.to_path_buf(),
        }),
        1 => Ok(candidates.remove(0)),
        _ => {
            let preferred = stem.map(|stem| directory.join(format!("{stem}.khm")));
            if let Some(preferred) = preferred
                && candidates.contains(&preferred)
            {
                return Ok(preferred);
            }
            Err(InvocationError::AmbiguousManifest {
                manifests: candidates,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    /// A directory the test controls, holding a fake executable image.
    struct Directory(PathBuf);

    impl Directory {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("kira-hybrid-launcher-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch dir");
            Directory(path)
        }

        fn executable(&self) -> PathBuf {
            self.0.join(if cfg!(target_os = "windows") {
                "demo.exe"
            } else {
                "demo"
            })
        }

        fn touch_manifest(&self, name: &str) {
            std::fs::write(self.0.join(name), b"").expect("write manifest");
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_bare_invocation_uses_the_only_manifest_beside_the_executable() {
        let directory = Directory::new("default");
        directory.touch_manifest("demo.khm");
        let resolved = resolve(&args(&["--flag"]), &directory.executable()).expect("resolves");
        assert_eq!(resolved.manifest, directory.0.join("demo.khm"));
        assert_eq!(resolved.program_arguments, vec!["--flag".to_owned()]);
    }

    #[test]
    fn renaming_the_executable_does_not_break_the_pairing() {
        // The directory is the deployment unit: one manifest beside the
        // executable is the answer whatever either file is called.
        let directory = Directory::new("renamed");
        directory.touch_manifest("demo.khm");
        let renamed = directory.0.join(if cfg!(target_os = "windows") {
            "shipped.exe"
        } else {
            "shipped"
        });
        let resolved = resolve(&[], &renamed).expect("resolves");
        assert_eq!(resolved.manifest, directory.0.join("demo.khm"));
    }

    #[test]
    fn no_manifest_beside_the_executable_is_a_typed_refusal() {
        let directory = Directory::new("empty");
        let error = resolve(&[], &directory.executable()).expect_err("nothing to run");
        assert!(error.to_string().contains("no hybrid manifest"), "{error}");
    }

    #[test]
    fn several_manifests_are_refused_unless_one_is_named_after_the_executable() {
        let directory = Directory::new("ambiguous");
        directory.touch_manifest("beta.khm");
        directory.touch_manifest("alpha.khm");
        let error = resolve(&[], &directory.executable())
            .expect_err("two manifests and neither is the executable's");
        assert!(
            error.to_string().contains("several hybrid manifests"),
            "{error}"
        );

        // The build's own pairing wins when it is present among several.
        directory.touch_manifest("demo.khm");
        let resolved = resolve(&[], &directory.executable()).expect("resolves");
        assert_eq!(resolved.manifest, directory.0.join("demo.khm"));
    }

    #[test]
    fn a_first_argument_that_names_a_manifest_wins_over_the_directory() {
        let directory = Directory::new("explicit");
        directory.touch_manifest("demo.khm");
        let resolved = resolve(
            &args(&["/shared/other.khm", "--flag"]),
            &directory.executable(),
        )
        .expect("resolves");
        assert_eq!(resolved.manifest, PathBuf::from("/shared/other.khm"));
        // Everything after the manifest belongs to the program.
        assert_eq!(resolved.program_arguments, vec!["--flag".to_owned()]);
    }

    #[test]
    fn only_a_manifest_spelling_is_taken_as_one() {
        let directory = Directory::new("spelling");
        directory.touch_manifest("demo.khm");
        // A program argument that merely mentions a `.khm` is not a manifest.
        let resolved =
            resolve(&args(&["--manifest", "x.khm"]), &directory.executable()).expect("resolves");
        assert_eq!(resolved.manifest, directory.0.join("demo.khm"));
        assert_eq!(
            resolved.program_arguments,
            vec!["--manifest".to_owned(), "x.khm".to_owned()]
        );
    }

    #[test]
    fn the_manifest_check_is_case_insensitive_on_the_extension() {
        let directory = Directory::new("casing");
        directory.touch_manifest("BUNDLE.KHM");
        let resolved = resolve(&args(&["BUNDLE.KHM"]), &directory.executable()).expect("resolves");
        assert_eq!(resolved.manifest, PathBuf::from("BUNDLE.KHM"));
    }

    #[cfg(unix)]
    #[test]
    fn an_argument_that_cannot_be_utf8_is_refused_not_mangled() {
        use std::os::unix::ffi::OsStringExt;
        let directory = Directory::new("utf8");
        directory.touch_manifest("demo.khm");
        let invalid = OsString::from_vec(vec![0xff]);
        let error = resolve(&[invalid], &directory.executable())
            .expect_err("an invalid argument is refused");
        assert!(error.to_string().contains("not valid UTF-8"), "{error}");
    }
}
