//! End-to-end debugger sessions through the real `kira` binary.

use std::process::Command;

use kira_debug::{Engine, PreparedTarget};

use crate::{kira, write_isolated_source};

#[test]
fn vm_batch_debugger_stops_disassembles_and_resumes() {
    let path =
        write_isolated_source("@Main function main() { let value = 6 + 1 print(value) return }\n");
    let output = kira(&[
        "debug",
        "--backend",
        "vm",
        "--batch",
        "--break",
        "main",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("stopped: main"), "stdout was: {stdout}");
    assert!(stdout.contains("source:"), "stdout was: {stdout}");
    assert_eq!(
        stdout.matches("stopped:").count(),
        1,
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("ConstInt"), "stdout was: {stdout}");
    assert!(stdout.ends_with("7\n"), "stdout was: {stdout}");
}

#[test]
fn vm_lldb_debugger_stops_at_a_bytecode_function_and_pc() {
    if !lldb_available() {
        eprintln!("skipping VM LLDB test: LLDB is not installed");
        return;
    }
    let path = write_isolated_source(
        "function helper(value: Int) -> Int { return value + 1 }\n\
         @Main function main() { print(helper(41)) return }\n",
    );
    let directory = path.parent().unwrap().to_path_buf();
    let output = kira(&[
        "debug",
        "--backend",
        "vm",
        "--lldb",
        "--batch",
        "--no-disassemble",
        "--break",
        "main:2",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_dir_all(directory);
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("LLDB VM probe: kira_vm_debug_probe"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("frame #0"), "stdout was: {stdout}");
    assert!(
        stdout.contains("kira_vm_debug_probe"),
        "stdout was: {stdout}"
    );
    assert!(
        stdout.contains("General Purpose Registers:"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("rip ="), "stdout was: {stdout}");
    assert!(stdout.contains("kira-vm-stop"), "stdout was: {stdout}");
    assert!(
        stdout.contains("instruction-bytes:"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("locals:"), "stdout was: {stdout}");
    assert!(stdout.contains("operand-stack:"), "stdout was: {stdout}");
    assert!(stdout.contains("backtrace:"), "stdout was: {stdout}");
}

#[test]
fn vm_lldb_dap_reads_decoded_state_from_a_real_stop() {
    if !lldb_dap_available() {
        eprintln!("skipping VM LLDB DAP test: lldb-dap is not installed");
        return;
    }
    let path = write_isolated_source(
        "function helper(value: Int) -> Int { return value + 1 }\n\
         @Main function main() { print(helper(41)) return }\n",
    );
    let directory = path.parent().unwrap().to_path_buf();
    let output = kira(&[
        "debug",
        "--backend",
        "vm",
        "--lldb-dap",
        "--dap-continues",
        "2",
        "--no-disassemble",
        "--break",
        "main:0",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_dir_all(directory);
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("lldb-dap-breakpoint verified=true"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("lldb-dap-stop #1"), "stdout was: {stdout}");
    assert!(stdout.contains("lldb-dap-stop #2"), "stdout was: {stdout}");
    assert!(stdout.contains("lldb-dap-stop #3"), "stdout was: {stdout}");
    assert!(
        stdout.contains("kira-vm-stop function=main"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("locals:"), "stdout was: {stdout}");
    assert!(stdout.contains("operand-stack:"), "stdout was: {stdout}");
}

#[test]
fn llvm_lldb_dap_stops_at_a_native_function() {
    if !lldb_dap_available() {
        eprintln!("skipping LLVM LLDB DAP test: lldb-dap is not installed");
        return;
    }
    let path =
        write_isolated_source("@Main function main() { let value = 6 + 1 print(value) return }\n");
    let directory = path.parent().unwrap().to_path_buf();
    let output = kira(&[
        "debug",
        "--backend",
        "llvm",
        "--lldb-dap",
        "--no-disassemble",
        "--break",
        "main",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_dir_all(directory);
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LLDB DAP target:"), "stdout was: {stdout}");
    assert!(
        stdout.contains("lldb-dap-breakpoint verified=true"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("lldb-dap-stop #1"), "stdout was: {stdout}");
}

#[test]
fn hybrid_lldb_dap_stops_in_the_vm_half() {
    if !lldb_dap_available() {
        eprintln!("skipping Hybrid LLDB DAP test: lldb-dap is not installed");
        return;
    }
    let path = write_isolated_source(
        "@Native function fast(value: Int) -> Int { return value * 2 }\n\
         @Main function main() { print(fast(21)) return }\n",
    );
    let directory = path.parent().unwrap().to_path_buf();
    let output = kira(&[
        "debug",
        "--backend",
        "hybrid",
        "--lldb-dap",
        "--no-disassemble",
        "--break",
        "main:0",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_dir_all(directory);
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("LLDB DAP hybrid host:"),
        "stdout was: {stdout}"
    );
    assert!(
        stdout.contains("lldb-dap-breakpoint verified=true"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("lldb-dap-stop #1"), "stdout was: {stdout}");
    assert!(
        stdout.contains("kira-vm-stop function=main"),
        "stdout was: {stdout}"
    );
}

#[test]
fn hybrid_lldb_dap_stops_at_a_native_function() {
    if !lldb_dap_available() {
        eprintln!("skipping Hybrid LLDB DAP native test: lldb-dap is not installed");
        return;
    }
    let path = write_isolated_source(
        "@Native function fast(value: Int) -> Int { return value * 2 }\n\
         @Main function main() { print(fast(21)) return }\n",
    );
    let directory = path.parent().unwrap().to_path_buf();
    let output = kira(&[
        "debug",
        "--backend",
        "hybrid",
        "--lldb-dap",
        "--no-disassemble",
        "--break",
        "fast",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_dir_all(directory);
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("LLDB DAP hybrid host:"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("lldb-dap-stop #1"), "stdout was: {stdout}");
    assert!(
        stdout.contains("frame=kira_native_fn_0"),
        "stdout was: {stdout}"
    );
}

#[test]
fn hybrid_batch_debugger_stops_in_the_vm_half_and_runs_native_code() {
    let path = write_isolated_source(
        "@Native function fast(value: Int) -> Int { return value * 2 }\n\
         @Main function main() { print(fast(21)) return }\n",
    );
    let artifact_directory = path.parent().unwrap().join(".kira-build");
    let output = kira(&[
        "debug",
        "--backend",
        "hybrid",
        "--batch",
        "--no-disassemble",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(artifact_directory);
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hybrid debug bundle:"),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("stopped: main"), "stdout was: {stdout}");
    assert!(stdout.contains("source:"), "stdout was: {stdout}");
    assert!(stdout.ends_with("42\n"), "stdout was: {stdout}");
}

#[test]
fn hybrid_lldb_debugger_combines_vm_stops_with_native_runtime_inspection() {
    if !lldb_available() {
        eprintln!("skipping Hybrid LLDB test: LLDB is not installed");
        return;
    }
    let path = write_isolated_source(
        "@Native function fast(value: Int) -> Int { return value * 2 }\n\
         @Main function main() { print(fast(21)) return }\n",
    );
    let directory = path.parent().unwrap().to_path_buf();
    let output = kira(&[
        "debug",
        "--backend",
        "hybrid",
        "--lldb",
        "--batch",
        "--break",
        "fast",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_dir_all(directory);
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LLDB hybrid host:"), "stdout was: {stdout}");
    assert!(stdout.contains("stopped: main"), "stdout was: {stdout}");
    assert!(stdout.contains("kira_native_fn_0"), "stdout was: {stdout}");
    assert!(stdout.contains("frame #0"), "stdout was: {stdout}");
    assert!(stdout.contains("rip ="), "stdout was: {stdout}");
    assert!(stdout.contains("<+0>"), "stdout was: {stdout}");
}

/// `--prepare` is the contract a debugger frontend builds through: it must
/// build the program, describe it, and leave the artifacts on disk for the
/// session that has not started yet.
#[test]
fn preparing_a_vm_target_describes_a_host_that_can_be_debugged_later() {
    let path =
        write_isolated_source("@Main function main() { let value = 6 + 1 print(value) return }\n");
    let output = kira(&[
        "debug",
        "--backend",
        "vm",
        "--prepare",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let target = prepared(&output.stdout);

    assert_eq!(target.backend, "vm");
    assert!(
        target.executable.exists(),
        "the VM host must exist: {}",
        target.executable.display()
    );
    // The VM is hosted by `kira` itself, through its private host verb.
    assert_eq!(
        target.arguments.first().map(String::as_str),
        Some("__vm-debug-host")
    );
    let probe = target.probe.as_ref().expect("the VM backend has a probe");
    assert_eq!(probe.symbol, kira_debug::VM_PROBE_SYMBOL);
    assert!(
        target.function("main").is_some(),
        "functions were: {:?}",
        target.functions
    );

    // The artifacts outlive the command, because the session that debugs them
    // starts after it returns.
    assert!(!target.artifacts.is_empty(), "the module must be kept");
    for artifact in &target.artifacts {
        assert!(artifact.exists(), "missing artifact {}", artifact.display());
    }
    target.clean();
    for artifact in &target.artifacts {
        assert!(
            !artifact.exists(),
            "artifact outlived its target: {}",
            artifact.display()
        );
    }
}

#[test]
fn preparing_a_native_target_describes_the_executable_and_its_symbols() {
    let path =
        write_isolated_source("@Main function main() { let value = 6 + 1 print(value) return }\n");
    let output = kira(&[
        "debug",
        "--backend",
        "llvm",
        "--prepare",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let target = prepared(&output.stdout);

    assert_eq!(target.backend, "llvm");
    assert!(
        target.executable.exists(),
        "the native executable must exist: {}",
        target.executable.display()
    );
    assert!(
        target.probe.is_none(),
        "a native build needs no VM probe: {:?}",
        target.probe
    );
    let main = target.function("main").expect("a main function");
    assert_eq!(main.symbol.as_deref(), Some("kira_fn_0_main"));
    assert_eq!(main.execution, kira_debug::Execution::Native);
}

/// A hybrid function that stayed bytecode must not claim a native symbol: a
/// breakpoint on it would resolve to an address with no body behind it.
#[test]
fn a_prepared_hybrid_target_reports_where_each_function_actually_runs() {
    let path =
        write_isolated_source("@Main function main() { let value = 6 + 1 print(value) return }\n");
    let output = kira(&[
        "debug",
        "--backend",
        "hybrid",
        "--prepare",
        path.to_str().unwrap(),
    ]);
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let target = prepared(&output.stdout);

    assert_eq!(target.backend, "hybrid");
    assert!(target.probe.is_some(), "a hybrid build keeps the VM probe");
    assert_eq!(
        target.arguments.first().map(String::as_str),
        Some("__hybrid-debug-host")
    );
    // The whole session belongs to one debugger, so the VM half reports
    // through the probe rather than as text on the same stdout.
    assert!(
        target
            .arguments
            .iter()
            .any(|argument| argument == "--vm-probe"),
        "arguments were: {:?}",
        target.arguments
    );
    for function in &target.functions {
        match function.execution {
            kira_debug::Execution::Native => assert!(
                function.symbol.is_some(),
                "`{}` runs natively without a symbol",
                function.name
            ),
            kira_debug::Execution::Bytecode => assert!(
                function.symbol.is_none(),
                "`{}` is bytecode but claims the symbol {:?}",
                function.name,
                function.symbol
            ),
        }
    }
}

/// The description is the command's result, and a frontend reads it off stdout.
fn prepared(stdout: &[u8]) -> PreparedTarget {
    let stdout = String::from_utf8_lossy(stdout);
    let description = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no prepared target in: {stdout}"));
    serde_json::from_str(description)
        .unwrap_or_else(|error| panic!("cannot read `{description}`: {error}"))
}

fn lldb_available() -> bool {
    engine_available(Engine::CommandLine, "--version")
}

fn lldb_dap_available() -> bool {
    engine_available(Engine::DebugAdapter, "--help")
}

/// Whether an LLDB frontend can start here.
///
/// Started through the same environment `kira debug` gives it, so an
/// installation whose runtime libraries live beside the toolchain rather than
/// on `PATH` counts as present. Probing it bare would fail in the loader and
/// silently skip every LLDB test on a host that has a working LLDB.
fn engine_available(engine: Engine, argument: &str) -> bool {
    let executable = engine.executable();
    let mut command = Command::new(&executable);
    kira_debug::configure_engine(&mut command, &executable);
    command
        .arg(argument)
        .output()
        .is_ok_and(|output| output.status.success())
}
