//! Shared argument parsing for the verbs that compile a program.
//!
//! Hand-rolled like the rest of the CLI. Backend selection is a structured
//! [`BackendMode`], resolved once here, so no handler branches on a backend
//! string.

use kira_backend_api::BackendMode;

/// A parsed `run`/`build`/`check` invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct CompileOptions {
    /// The `.kira` file to compile.
    pub path: String,
    /// Which backend to compile for.
    pub backend: BackendMode,
    /// Whether to also write the textual LLVM IR beside the other artifacts.
    pub emit_llvm_ir: bool,
}

/// Why an invocation could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OptionsError {
    /// No path was given.
    #[error("expected a path to a .kira file")]
    MissingPath,
    /// `--backend` was given without a value.
    #[error("`--backend` expects one of: vm, llvm, hybrid")]
    BackendMissingValue,
    /// `--backend` was given an unknown value.
    #[error("unknown backend `{0}`; expected one of: vm, llvm, hybrid")]
    UnknownBackend(String),
    /// An unrecognized flag.
    #[error("unknown option `{0}`")]
    UnknownFlag(String),
    /// More than one path was given.
    #[error("expected a single path, but got both `{first}` and `{second}`")]
    ExtraPath {
        /// The first path seen.
        first: String,
        /// The second, unexpected path.
        second: String,
    },
}

impl CompileOptions {
    /// Parses `args` (everything after the verb).
    pub fn parse(args: &[String]) -> Result<Self, OptionsError> {
        let mut path: Option<String> = None;
        let mut backend = BackendMode::VmBytecode;
        let mut emit_llvm_ir = false;

        let mut index = 0;
        while index < args.len() {
            let argument = args[index].as_str();
            match argument {
                "--backend" => {
                    let value = args
                        .get(index + 1)
                        .ok_or(OptionsError::BackendMissingValue)?;
                    backend = parse_backend(value)?;
                    index += 1;
                }
                "--emit-llvm-ir" => emit_llvm_ir = true,
                other if other.starts_with('-') => {
                    return Err(OptionsError::UnknownFlag(other.to_owned()));
                }
                other => match &path {
                    // A second path is a mistake worth naming rather than
                    // silently ignoring one of them.
                    Some(first) => {
                        return Err(OptionsError::ExtraPath {
                            first: first.clone(),
                            second: other.to_owned(),
                        });
                    }
                    None => path = Some(other.to_owned()),
                },
            }
            index += 1;
        }

        Ok(CompileOptions {
            path: path.ok_or(OptionsError::MissingPath)?,
            backend,
            emit_llvm_ir,
        })
    }
}

/// Resolves a `--backend` value.
fn parse_backend(value: &str) -> Result<BackendMode, OptionsError> {
    Ok(match value {
        "vm" => BackendMode::VmBytecode,
        "llvm" => BackendMode::LlvmNative,
        "hybrid" => BackendMode::Hybrid,
        other => return Err(OptionsError::UnknownBackend(other.to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn defaults_to_the_vm_backend() {
        let options = CompileOptions::parse(&args(&["main.kira"])).expect("parses");
        assert_eq!(options.backend, BackendMode::VmBytecode);
        assert_eq!(options.path, "main.kira");
        assert!(!options.emit_llvm_ir);
    }

    #[test]
    fn parses_each_backend_before_or_after_the_path() {
        for (value, expected) in [
            ("vm", BackendMode::VmBytecode),
            ("llvm", BackendMode::LlvmNative),
            ("hybrid", BackendMode::Hybrid),
        ] {
            let after = CompileOptions::parse(&args(&["main.kira", "--backend", value]));
            let before = CompileOptions::parse(&args(&["--backend", value, "main.kira"]));
            assert_eq!(after.expect("parses").backend, expected);
            assert_eq!(before.expect("parses").backend, expected);
        }
    }

    #[test]
    fn rejects_bad_invocations_with_a_reason() {
        assert_eq!(
            CompileOptions::parse(&args(&[])),
            Err(OptionsError::MissingPath)
        );
        assert_eq!(
            CompileOptions::parse(&args(&["--backend"])),
            Err(OptionsError::BackendMissingValue)
        );
        assert_eq!(
            CompileOptions::parse(&args(&["--backend", "cranelift", "main.kira"])),
            Err(OptionsError::UnknownBackend("cranelift".to_owned()))
        );
        assert_eq!(
            CompileOptions::parse(&args(&["--turbo", "main.kira"])),
            Err(OptionsError::UnknownFlag("--turbo".to_owned()))
        );
        assert_eq!(
            CompileOptions::parse(&args(&["a.kira", "b.kira"])),
            Err(OptionsError::ExtraPath {
                first: "a.kira".to_owned(),
                second: "b.kira".to_owned(),
            })
        );
    }

    #[test]
    fn parses_the_ir_dump_flag() {
        let options =
            CompileOptions::parse(&args(&["--backend", "llvm", "--emit-llvm-ir", "m.kira"]))
                .expect("parses");
        assert!(options.emit_llvm_ir);
        assert_eq!(options.backend, BackendMode::LlvmNative);
    }
}
