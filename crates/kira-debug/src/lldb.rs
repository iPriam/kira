//! LLDB process control and CPU-instruction inspection.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::DebugInfo;
use crate::engine::{self, Engine};

/// A prepared LLDB launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LldbLaunch {
    /// The executable LLDB should debug.
    pub target: PathBuf,
    /// Symbols to break on before the process starts.
    pub breakpoints: Vec<String>,
    /// Conditional symbol breakpoints, expressed in LLDB's expression syntax.
    pub conditional_breakpoints: Vec<(String, String)>,
    /// Commands LLDB attaches to numbered breakpoints and runs on every stop.
    ///
    /// This is what lets a VM probe publish decoded Kira state each time an
    /// interactive user types `continue`, not only at the launch stop.
    pub breakpoint_commands: Vec<(u32, String)>,
    /// Whether to include native disassembly commands.
    pub disassemble: bool,
    /// Whether LLDB should run non-interactively and return its transcript.
    pub batch: bool,
    /// Whether to request a thread backtrace after the first stop.
    ///
    /// Some LLDB builds abort while unwinding a frame from a dynamically
    /// loaded library. Hybrid launches can disable this one post-stop query
    /// and still inspect source, registers, and CPU instructions.
    pub thread_backtrace: bool,
    /// Arguments passed to the debugged program when LLDB executes it.
    pub arguments: Vec<String>,
}

impl LldbLaunch {
    /// Creates a launch from compiler debug identities.
    #[must_use]
    pub fn from_info(target: impl Into<PathBuf>, info: &DebugInfo) -> Self {
        let breakpoints = info
            .functions
            .iter()
            .filter_map(|function| function.symbol.clone())
            .collect();
        Self {
            target: target.into(),
            breakpoints,
            conditional_breakpoints: Vec::new(),
            breakpoint_commands: Vec::new(),
            disassemble: true,
            batch: false,
            thread_backtrace: true,
            arguments: Vec::new(),
        }
    }

    /// Adds one symbol breakpoint if it is not already present.
    pub fn add_breakpoint(&mut self, symbol: impl Into<String>) {
        let symbol = symbol.into();
        if !self.breakpoints.contains(&symbol) {
            self.breakpoints.push(symbol);
        }
    }

    /// Adds one symbol breakpoint with an LLDB condition.
    pub fn add_conditional_breakpoint(
        &mut self,
        symbol: impl Into<String>,
        condition: impl Into<String>,
    ) {
        let breakpoint = (symbol.into(), condition.into());
        if !self.conditional_breakpoints.contains(&breakpoint) {
            self.conditional_breakpoints.push(breakpoint);
        }
    }

    /// Adds a one-line command to a numeric LLDB breakpoint command list.
    pub fn add_breakpoint_command(&mut self, breakpoint: u32, command: impl Into<String>) {
        let command = (breakpoint, command.into());
        if !self.breakpoint_commands.contains(&command) {
            self.breakpoint_commands.push(command);
        }
    }

    /// Returns the exact LLDB `-o` commands this launch will issue.
    #[must_use]
    pub fn commands(&self) -> Vec<String> {
        let mut commands = vec![
            "settings set target.inline-breakpoint-strategy always".to_owned(),
            "settings set target.process.stop-on-sharedlibrary-events false".to_owned(),
        ];
        for symbol in &self.breakpoints {
            commands.push(format!("breakpoint set --name {symbol}"));
        }
        for (symbol, condition) in &self.conditional_breakpoints {
            // `--skip-prologue false` is what makes the condition mean anything.
            //
            // The condition reads the ARGUMENT REGISTERS, and those hold the
            // arguments only at the function's first instruction. LLDB's default
            // is to skip the prologue, and a prologue spills its arguments to
            // the stack and reuses the registers — so an optimized build, whose
            // prologue is nothing, matched, and a debug build silently never
            // did: the breakpoint resolved a hundred bytes in, at an inlined
            // call site, with `$rdi` long since something else. The program ran
            // to completion and every command after `run` then failed with
            // "requires a process which is currently stopped".
            commands.push(format!(
                "breakpoint set --name {symbol} --skip-prologue false --condition {}",
                quote_command_argument(condition)
            ));
        }
        for (breakpoint, command) in &self.breakpoint_commands {
            commands.push(format!(
                "breakpoint command add -o {} {breakpoint}",
                quote_command_argument(command)
            ));
        }
        if !self.arguments.is_empty() {
            let arguments = self
                .arguments
                .iter()
                .map(|argument| quote_command_argument(argument))
                .collect::<Vec<_>>()
                .join(" ");
            commands.push(format!("settings set -- target.run-args {arguments}"));
        }
        commands.push("run".to_owned());
        // These commands run after `run` returns at the first breakpoint, so
        // the transcript describes the actual stopped CPU state rather than
        // only a symbol's static body. Keep the function-level listing below
        // too: it remains useful when a breakpoint is resolved lazily or when
        // LLDB resumes past the stop in an interactive session.
        commands.push("frame info".to_owned());
        commands.push("source list --count 8".to_owned());
        commands.push("image lookup --address $pc".to_owned());
        commands.push("frame variable".to_owned());
        if self.thread_backtrace {
            commands.push("thread backtrace".to_owned());
        }
        commands.push("register read".to_owned());
        if self.disassemble {
            commands.push("disassemble --frame --count 32".to_owned());
            for symbol in &self.breakpoints {
                commands.push(format!("disassemble --name {symbol} --count 32"));
            }
        }
        commands
    }

    /// Launches the real LLDB executable selected by `KIRA_LLDB` or `PATH`.
    pub fn launch(&self) -> Result<LldbOutput, LldbError> {
        let executable = Engine::CommandLine.executable();
        let mut command = Command::new(&executable);
        engine::configure(&mut command, &executable);
        command.arg("--no-lldbinit");
        if self.batch {
            command.arg("--batch");
        }
        command.arg(&self.target);
        for instruction in self.commands() {
            command.arg("-o").arg(instruction);
        }
        if self.batch {
            let output = command
                .output()
                .map_err(|source| LldbError::Spawn { executable, source })?;
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            if !output.status.success() {
                return Err(LldbError::Failed {
                    code: output.status.code(),
                    stdout,
                    stderr,
                });
            }
            Ok(LldbOutput { stdout, stderr })
        } else {
            command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            let status = command
                .status()
                .map_err(|source| LldbError::Spawn { executable, source })?;
            if !status.success() {
                return Err(LldbError::Failed {
                    code: status.code(),
                    stdout: String::new(),
                    stderr: "LLDB exited without a successful debug session".to_owned(),
                });
            }
            Ok(LldbOutput {
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }
}

/// Quotes one program argument for LLDB's command interpreter.
fn quote_command_argument(argument: &str) -> String {
    let escaped = argument.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// LLDB's captured batch transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LldbOutput {
    /// Standard output, including disassembly and stop reasons.
    pub stdout: String,
    /// Standard error from LLDB.
    pub stderr: String,
}

/// Why an LLDB launch failed.
#[derive(Debug, thiserror::Error)]
pub enum LldbError {
    /// LLDB could not be started.
    #[error("cannot start LLDB `{executable}`: {source}")]
    Spawn {
        /// The configured executable.
        executable: PathBuf,
        /// The process error.
        #[source]
        source: std::io::Error,
    },
    /// LLDB started but did not complete successfully.
    #[error("LLDB failed with exit code {code:?}: {stderr}")]
    Failed {
        /// The process exit code, when it had one.
        code: Option<i32>,
        /// Captured standard output.
        stdout: String,
        /// Captured standard error.
        stderr: String,
    },
}

/// Escapes a target for a command transcript without changing the path passed
/// to LLDB as an argument.
#[must_use]
pub fn target_label(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Backend, DebugFunction, DebugInfo};

    fn info() -> DebugInfo {
        DebugInfo {
            module_name: "demo".to_owned(),
            backend: Backend::Llvm,
            source: None,
            functions: vec![DebugFunction {
                id: 0,
                name: "main".to_owned(),
                backend: Backend::Llvm,
                symbol: Some("kira_fn_0_main".to_owned()),
                line: 1,
            }],
            optimized: true,
        }
    }

    #[test]
    fn commands_include_breakpoints_and_cpu_disassembly() {
        let launch = LldbLaunch::from_info("demo.exe", &info());
        let commands = launch.commands();
        assert!(
            commands
                .iter()
                .any(|command| command.contains("breakpoint set"))
        );
        assert!(
            commands
                .iter()
                .any(|command| command.contains("disassemble"))
        );
        assert!(commands.iter().any(|command| command == "thread backtrace"));
        assert!(commands.iter().any(|command| command == "register read"));
        assert!(commands.iter().any(|command| command == "frame info"));
        assert!(
            commands
                .iter()
                .any(|command| command == "source list --count 8")
        );
        assert!(
            commands
                .iter()
                .any(|command| command == "image lookup --address $pc")
        );
        assert!(commands.iter().any(|command| command == "frame variable"));
        assert_eq!(target_label(Path::new("demo.exe")), "demo.exe");
    }

    #[test]
    fn commands_forward_program_arguments_with_lldb_quoting() {
        let mut launch = LldbLaunch::from_info("demo.exe", &info());
        launch.arguments = vec!["first".to_owned(), "second value".to_owned()];
        assert!(
            launch
                .commands()
                .iter()
                .any(|command| { command.contains("target.run-args \"first\" \"second value\"") })
        );
    }

    #[test]
    fn commands_can_skip_unwinding_for_shared_library_stops() {
        let mut launch = LldbLaunch::from_info("demo.exe", &info());
        launch.thread_backtrace = false;
        assert!(
            launch
                .commands()
                .iter()
                .all(|command| command != "thread backtrace")
        );
        assert!(
            launch
                .commands()
                .iter()
                .any(|command| command == "register read")
        );
    }

    #[test]
    fn commands_include_conditional_breakpoints() {
        let mut launch = LldbLaunch::from_info("demo.exe", &info());
        launch.add_conditional_breakpoint("kira_vm_debug_probe", "function_id == 3 && pc == 2");
        // The prologue flag is part of the command, not decoration: the
        // condition reads argument registers, which hold the arguments only at
        // the function's first instruction.
        let expected = concat!(
            "breakpoint set --name kira_vm_debug_probe --skip-prologue false ",
            "--condition \"function_id == 3 && pc == 2\""
        );
        assert!(launch.commands().iter().any(|command| command == expected));
    }

    #[test]
    fn commands_attach_a_debugger_dump_to_each_stop() {
        let mut launch = LldbLaunch::from_info("demo.exe", &info());
        launch.add_breakpoint_command(1, "expr -- (void)kira_vm_debug_dump()");
        assert!(launch.commands().iter().any(|command| {
            command == "breakpoint command add -o \"expr -- (void)kira_vm_debug_dump()\" 1"
        }));
    }
}
