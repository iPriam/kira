//! Shared argument parsing for the verbs that compile a program.
//!
//! Hand-rolled like the rest of the CLI. Backend and device selection are both
//! structured enums, resolved once here, so no handler branches on a string.

use kira_backend_api::BackendMode;
use kira_wasm_runtime::WasmDevice;

/// What a program is being compiled to run on.
///
/// The device is a separate axis from the backend: `--backend` chooses which
/// engine compiles a program for *this* machine, and `--device` chooses whether
/// this machine is the target at all. They are not independent — a wasm device
/// is served by the wasm backend and nothing else — which is why naming both is
/// refused rather than silently resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    /// This machine, compiled by the backend `--backend` selects.
    Host,
    /// The Web, compiled to a WebAssembly module.
    Web(WasmDevice),
}

/// A parsed `run`/`build`/`check` invocation.
#[derive(Debug, Clone, PartialEq)]
pub struct CompileOptions {
    /// The `.kira` file to compile.
    pub path: String,
    /// Which backend to compile for, when the device is the host.
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
    /// Both a wasm device and a backend were named.
    #[error(
        "`--device {device}` compiles to WebAssembly, so `--backend` cannot \
         also be given; drop `--backend` to build for the Web, or drop \
         `--device` to build for this machine"
    )]
    BackendWithWebDevice {
        /// The device that was asked for.
        device: &'static str,
    },
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
        // Tracked as an option so that naming a backend *and* a wasm device is
        // a reported conflict rather than one of them quietly winning.
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

        if let (Device::Web(web), Some(_)) = (device, backend) {
            return Err(OptionsError::BackendWithWebDevice {
                device: web.label(),
            });
        }

        Ok(CompileOptions {
            path: path.ok_or(OptionsError::MissingPath)?,
            backend: backend.unwrap_or(BackendMode::VmBytecode),
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
    fn a_backend_and_a_web_device_together_are_refused() {
        // Resolving this silently would compile for one target while the user
        // read the other off their own command line.
        assert_eq!(
            CompileOptions::parse(&args(&[
                "--device",
                "wasm32",
                "--backend",
                "llvm",
                "m.kira"
            ])),
            Err(OptionsError::BackendWithWebDevice { device: "wasm32" })
        );
        // The host device is what `--backend` is for, so it is no conflict.
        assert!(
            CompileOptions::parse(&args(&["--device", "host", "--backend", "llvm", "m.kira"]))
                .is_ok()
        );
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
