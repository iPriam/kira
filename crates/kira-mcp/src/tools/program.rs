//! Running one Kira program under one engine and device.
//!
//! Shared by every tool that has to *execute* Kira rather than build it, so
//! that "run this on the VM" means the same command in each of them. A tool
//! that assembled its own invocation would eventually compare two things that
//! were not configured the same way and call the difference a divergence.

use std::time::Duration;

use serde_json::{Value, json};

use crate::exec::{self, ExecError, Run};

/// One engine-and-device pair a program can run under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configuration {
    pub backend: String,
    pub device: String,
}

impl Configuration {
    pub fn new(backend: &str, device: &str) -> Configuration {
        Configuration {
            backend: backend.to_owned(),
            device: device.to_owned(),
        }
    }

    /// How this configuration is named in a result.
    pub fn label(&self) -> String {
        format!("{}/{}", self.backend, self.device)
    }

    pub fn json(&self) -> Value {
        json!({ "backend": self.backend, "device": self.device })
    }
}

/// The argument vector that runs `source` under `configuration`.
///
/// `--backend` and `--device` are always spelled out, never left to default:
/// a comparison whose two halves inherited a default from the environment
/// would be comparing that environment, not the two backends.
pub fn invocation(
    configuration: &Configuration,
    source: &str,
    program_args: &[String],
) -> Vec<String> {
    let mut args = exec::argv(&["run", "-q", "-p", "kira-cli", "--", "run"]);
    args.push("--backend".to_owned());
    args.push(configuration.backend.clone());
    args.push("--device".to_owned());
    args.push(configuration.device.clone());
    args.push(source.to_owned());
    args.extend_from_slice(program_args);
    args
}

/// Runs `source` once under `configuration`.
pub fn run(
    configuration: &Configuration,
    source: &str,
    program_args: &[String],
    env: &[(String, String)],
    timeout: Duration,
) -> Result<Run, ExecError> {
    let args = invocation(configuration, source, program_args);
    exec::run("cargo", &args, &exec::repository_root(), env, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both selectors appear, so neither half of a comparison can drift.
    #[test]
    fn an_invocation_names_its_backend_and_its_device() {
        let args = invocation(&Configuration::new("llvm", "wasm32"), "m.kira", &[]);
        let position = |flag: &str| args.iter().position(|arg| arg == flag).expect(flag);
        assert_eq!(args[position("--backend") + 1], "llvm");
        assert_eq!(args[position("--device") + 1], "wasm32");
        assert_eq!(args.last().expect("a source"), "m.kira");
    }

    /// Program arguments follow the source, where the CLI expects them.
    #[test]
    fn program_arguments_follow_the_source() {
        let args = invocation(
            &Configuration::new("vm", "host"),
            "m.kira",
            &["--seed".to_owned(), "7".to_owned()],
        );
        let tail = &args[args.len() - 3..];
        assert_eq!(tail, ["m.kira", "--seed", "7"]);
    }
}
