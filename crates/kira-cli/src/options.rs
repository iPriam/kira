//! Shared argument parsing for the verbs that compile a program.
//!
//! Hand-rolled like the rest of the CLI. Backend and device selection are both
//! structured enums, resolved once here, so no handler branches on a string.

use kira_backend_api::BackendMode;
use kira_wasm_runtime::WasmDevice;

/// What a program is being compiled to run on.
///
/// The device is a separate axis from the backend, and they are independent:
/// `--backend` chooses which engine compiles a program, `--device` chooses what
/// machine it runs on, and every pair means something. A device never overrides
/// a backend; it only decides which backend a command that named none gets.
///
/// Not every pair is built yet. An unbuilt one is refused by name, so a
/// `--backend` a user wrote is never quietly replaced by another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    /// This machine.
    Host,
    /// The Web: a WebAssembly module, and the page that runs it.
    Web(WasmDevice),
}

impl Device {
    /// The name this device is spelled by on the command line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Web(device) => device.label(),
        }
    }
}

/// A parsed `run`/`build`/`check` invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct CompileOptions {
    /// The `.kira` file to compile.
    pub path: String,
    /// Which backend compiles the program, on whatever device it targets.
    pub backend: BackendMode,
    /// What the program is being compiled to run on.
    pub device: Device,
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
    /// `--device` was given without a value.
    #[error("`--device` expects one of: host, wasm32, wasm64")]
    DeviceMissingValue,
    /// `--device` was given an unknown value.
    #[error("unknown device `{0}`; expected one of: host, wasm32, wasm64")]
    UnknownDevice(String),
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
        // Tracked as an option so that "the user named no backend" stays
        // distinguishable from "the user named the one that is also the
        // default" — which is what lets the device pick a default without ever
        // overriding a choice.
        let mut backend: Option<BackendMode> = None;
        let mut device = Device::Host;
        let mut emit_llvm_ir = false;

        let mut index = 0;
        while index < args.len() {
            let argument = args[index].as_str();
            match argument {
                "--backend" => {
                    let value = args
                        .get(index + 1)
                        .ok_or(OptionsError::BackendMissingValue)?;
                    backend = Some(parse_backend(value)?);
                    index += 1;
                }
                "--device" => {
                    let value = args
                        .get(index + 1)
                        .ok_or(OptionsError::DeviceMissingValue)?;
                    device = parse_device(value)?;
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
            // `--backend` is honored on every device. What a device changes is
            // only the *default*, for a command that named no backend: on this
            // machine the VM, and on the Web the device's own code generator.
            // A default is not an override — an explicit `--backend` always
            // survives to the pipeline, which either serves it or says it is
            // not built yet.
            backend: backend.unwrap_or(match device {
                Device::Host => BackendMode::VmBytecode,
                Device::Web(_) => BackendMode::LlvmNative,
            }),
            device,
            emit_llvm_ir,
        })
    }
}

/// Resolves a `--device` value.
fn parse_device(value: &str) -> Result<Device, OptionsError> {
    if value == "host" {
        return Ok(Device::Host);
    }
    WasmDevice::parse(value)
        .map(Device::Web)
        .ok_or_else(|| OptionsError::UnknownDevice(value.to_owned()))
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
    fn defaults_to_the_vm_backend_on_this_machine() {
        let options = CompileOptions::parse(&args(&["main.kira"])).expect("parses");
        assert_eq!(options.backend, BackendMode::VmBytecode);
        assert_eq!(options.device, Device::Host);
        assert_eq!(options.path, "main.kira");
        assert!(!options.emit_llvm_ir);
    }

    #[test]
    fn parses_each_device_before_or_after_the_path() {
        for (value, expected) in [
            ("host", Device::Host),
            ("wasm32", Device::Web(WasmDevice::Wasm32)),
            ("wasm64", Device::Web(WasmDevice::Wasm64)),
        ] {
            let after = CompileOptions::parse(&args(&["main.kira", "--device", value]));
            let before = CompileOptions::parse(&args(&["--device", value, "main.kira"]));
            assert_eq!(after.expect("parses").device, expected);
            assert_eq!(before.expect("parses").device, expected);
        }
    }

    #[test]
    fn a_backend_survives_every_device() {
        // `--backend` is never overridden. A device that served a backend other
        // than the one on the command line would compile one thing while the
        // user read another off their own shell history.
        for device in ["host", "wasm32", "wasm64"] {
            for (flag, expected) in [
                ("vm", BackendMode::VmBytecode),
                ("llvm", BackendMode::LlvmNative),
                ("hybrid", BackendMode::Hybrid),
            ] {
                let parsed = CompileOptions::parse(&args(&[
                    "--device",
                    device,
                    "--backend",
                    flag,
                    "m.kira",
                ]))
                .expect("a backend and a device are independent axes");
                assert_eq!(
                    parsed.backend, expected,
                    "`--backend {flag}` did not survive `--device {device}`",
                );
            }
        }
    }

    #[test]
    fn a_device_only_decides_the_backend_nobody_named() {
        // A default is not an override: it applies when `--backend` is absent,
        // and never otherwise.
        let host = CompileOptions::parse(&args(&["m.kira"])).expect("parses");
        assert_eq!(host.backend, BackendMode::VmBytecode);

        // On the Web the default is the device's own code generator, which is
        // what makes `kirac build --device wasm32` mean what it always did.
        let web = CompileOptions::parse(&args(&["--device", "wasm32", "m.kira"])).expect("parses");
        assert_eq!(web.backend, BackendMode::LlvmNative);
    }

    #[test]
    fn rejects_a_bad_device_with_a_reason() {
        assert_eq!(
            CompileOptions::parse(&args(&["--device"])),
            Err(OptionsError::DeviceMissingValue)
        );
        assert_eq!(
            CompileOptions::parse(&args(&["--device", "wasm128", "m.kira"])),
            Err(OptionsError::UnknownDevice("wasm128".to_owned()))
        );
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
