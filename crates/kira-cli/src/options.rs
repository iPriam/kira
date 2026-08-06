//! Shared argument parsing for the verbs that compile a program.
//!
//! Hand-rolled like the rest of the CLI. Backend and device selection are both
//! structured enums, resolved once here, so no handler branches on a string.

use kira_backend_api::BackendMode;
use kira_backend_api::WasmDevice;

/// What a program is being compiled to run on.
///
/// `--device` is an override. On the host, `--backend` picks among the three
/// engines; a Web device has exactly one code generator, so naming the device
/// decides the backend, and a differing `--backend` beside it is overridden
/// aloud — never served, never silently swapped.
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
    /// The `.kira` file or package directory to compile.
    pub path: String,
    /// Which backend compiles the program, on whatever device it targets.
    pub backend: BackendMode,
    /// Whether the user explicitly supplied `--backend`.
    pub backend_explicit: bool,
    /// What the program is being compiled to run on.
    pub device: Device,
    /// Whether the user explicitly supplied `--device`.
    pub device_explicit: bool,
    /// Whether to also write the textual LLVM IR beside the other artifacts.
    pub emit_llvm_ir: bool,
    /// Whether to generate code at the aggressive optimization level.
    ///
    /// A development build already optimizes: emitting without it is faster but
    /// produces stack frames large enough to overflow on a deeply nested
    /// program, so there is no unoptimized level to fall back to. `--release`
    /// asks for the level above the default.
    pub release: bool,
    /// Whether to report where the build spent its time when it finishes.
    pub timings: bool,
    /// Whether to print the informational notes a compilation reports.
    ///
    /// Off by default: a note says what the compiler decided rather than what
    /// the program got wrong, and it says it again on every build. The count is
    /// still reported, so nothing is dropped silently.
    pub show_notes: bool,
}

/// The path a `run`/`build`/`check` uses when the invocation names none: the
/// current directory, which package discovery then resolves as a package.
pub const DEFAULT_PATH: &str = ".";

/// Why an invocation could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OptionsError {
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
        let mut device_explicit = false;
        let mut emit_llvm_ir = false;
        let mut release = false;
        let mut timings = false;
        let mut show_notes = false;

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
                    device_explicit = true;
                    index += 1;
                }
                "--emit-llvm-ir" => emit_llvm_ir = true,
                "--release" => release = true,
                "--timings" => timings = true,
                "--show-notes" => show_notes = true,
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

        let backend_explicit = backend.is_some();
        // `--device` is an override: a Web device has exactly one code
        // generator, so naming the device decides the backend, and a
        // `--backend` beside it is noted aloud rather than served or refused.
        // On the host, `--backend` picks among the three engines as ever.
        let backend = match device {
            Device::Host => backend.unwrap_or(BackendMode::VmBytecode),
            Device::Web(_) => {
                if let Some(named) = backend
                    && named != BackendMode::LlvmNative
                {
                    eprintln!(
                        "kira: `--device {}` overrides `--backend {}`: the Web \
                         device has one code generator",
                        device.label(),
                        named.label(),
                    );
                }
                BackendMode::LlvmNative
            }
        };

        Ok(CompileOptions {
            // No path means the package you are standing in, the way every
            // other build tool reads a bare invocation. Nothing is guessed: `.`
            // goes through the same package discovery an explicit path does, so
            // a directory holding no `package.kira` is refused by name there
            // rather than by a usage error here.
            path: path.unwrap_or_else(|| DEFAULT_PATH.to_owned()),
            backend,
            backend_explicit,
            device,
            device_explicit,
            emit_llvm_ir,
            release,
            timings,
            show_notes,
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
    fn a_backend_survives_every_host_invocation() {
        // On the host, `--backend` picks the engine and nothing second-guesses
        // it.
        for (flag, expected) in [
            ("vm", BackendMode::VmBytecode),
            ("llvm", BackendMode::LlvmNative),
            ("hybrid", BackendMode::Hybrid),
        ] {
            let parsed =
                CompileOptions::parse(&args(&["--device", "host", "--backend", flag, "m.kira"]))
                    .expect("parses");
            assert_eq!(parsed.backend, expected);
        }
    }

    #[test]
    fn a_web_device_overrides_every_backend() {
        // `--device` is an override: a Web device has exactly one code
        // generator, so whatever backend is named beside it, the Web build is
        // what runs — announced on stderr, not silently.
        for device in ["wasm32", "wasm64"] {
            for flag in ["vm", "llvm", "hybrid"] {
                let parsed = CompileOptions::parse(&args(&[
                    "--device",
                    device,
                    "--backend",
                    flag,
                    "m.kira",
                ]))
                .expect("a Web device serves every invocation");
                assert_eq!(
                    parsed.backend,
                    BackendMode::LlvmNative,
                    "`--device {device}` must override `--backend {flag}`",
                );
            }
        }
    }

    #[test]
    fn a_device_decides_the_backend_nobody_named() {
        let host = CompileOptions::parse(&args(&["m.kira"])).expect("parses");
        assert_eq!(host.backend, BackendMode::VmBytecode);

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
    fn a_bare_invocation_compiles_the_package_you_are_standing_in() {
        assert_eq!(
            CompileOptions::parse(&args(&[])).expect("parses").path,
            DEFAULT_PATH
        );
        // Flags alone still leave the path defaulted, so `kira build
        // --backend llvm` in a package directory means what it looks like.
        assert_eq!(
            CompileOptions::parse(&args(&["--backend", "llvm"]))
                .expect("parses")
                .path,
            DEFAULT_PATH
        );
    }

    #[test]
    fn rejects_bad_invocations_with_a_reason() {
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
    fn parses_the_timings_flag_before_or_after_the_path() {
        assert!(
            !CompileOptions::parse(&args(&["m.kira"]))
                .expect("parses")
                .timings
        );
        for order in [["--timings", "m.kira"], ["m.kira", "--timings"]] {
            let options = CompileOptions::parse(&args(&order)).expect("parses");
            assert!(options.timings);
            assert_eq!(options.path, "m.kira");
        }
    }

    #[test]
    fn notes_are_hidden_unless_the_invocation_asks_for_them() {
        assert!(
            !CompileOptions::parse(&args(&["m.kira"]))
                .expect("parses")
                .show_notes
        );
        let asked = CompileOptions::parse(&args(&["--show-notes", "m.kira"])).expect("parses");
        assert!(asked.show_notes);
        assert_eq!(asked.path, "m.kira");
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
