//! End-to-end debugger sessions through the real `kira` binary.

use std::ffi::OsString;
#[cfg(windows)]
use std::path::PathBuf;
use std::process::Command;

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

fn lldb_available() -> bool {
    let executable = std::env::var_os("KIRA_LLDB").unwrap_or_else(|| OsString::from("lldb"));
    let mut command = Command::new(executable);
    #[cfg(windows)]
    configure_windows_lldb_path(&mut command);
    command
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn lldb_dap_available() -> bool {
    let executable =
        std::env::var_os("KIRA_LLDB_DAP").unwrap_or_else(|| OsString::from("lldb-dap"));
    let mut command = Command::new(executable);
    #[cfg(windows)]
    configure_windows_lldb_path(&mut command);
    command
        .arg("--help")
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(windows)]
fn configure_windows_lldb_path(command: &mut Command) {
    let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let python = PathBuf::from(local_app_data)
        .join("Programs")
        .join("Python")
        .join("Python39");
    if !python.join("python39.dll").is_file() {
        return;
    }
    let mut paths = vec![python];
    if let Some(current) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current));
    }
    if let Ok(path) = std::env::join_paths(paths) {
        command.env("PATH", path);
    }
}
