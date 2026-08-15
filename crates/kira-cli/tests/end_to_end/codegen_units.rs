//! A native build large enough to be split across codegen units, with behavior
//! matching the equivalent VM program.
//!
//! The split is invisible by design — the bodies are dealt out, the linker puts
//! them back — so the only proof worth having is behavioural: the same program
//! on the VM and on LLVM, printing the same thing. A chain of calls is what
//! makes it a real test rather than a large one: every function calls the next,
//! so a unit that failed to declare what another unit defines does not link, and
//! a body left out of every unit takes the answer with it.

use std::path::{Path, PathBuf};

use crate::{kira, write_isolated_source};

/// How many chained functions the program declares.
///
/// Above the backend's threshold for splitting at all (a unit per 96 functions)
/// with enough left over for several units on an ordinary machine.
const CHAIN: usize = 400;

/// A program of `count` functions, each calling the next and adding one.
///
/// `main` prints the chain's answer — which is `count` only if every one of them
/// ran — and then a string built at run time, so each unit also carries the
/// internal string leaves the lowering emits on demand.
fn chained_program(count: usize) -> String {
    let mut source =
        String::from("@Main function main() { print(step0()) print(joined()) return }\n");
    for index in 0..count {
        let body = if index + 1 == count {
            "return 1".to_owned()
        } else {
            format!("return 1 + step{}()", index + 1)
        };
        source.push_str(&format!("function step{index}() -> Int {{ {body} }}\n"));
    }
    source.push_str(
        "function joined() -> String { var text = \"a\" text = text + \"b\" return text }\n",
    );
    source
}

/// Where `kira` writes a bare source file's build artifacts.
fn build_directory(source: &Path) -> PathBuf {
    source
        .parent()
        .expect("the source has a directory")
        .join(".kira-build")
}

#[test]
fn a_program_split_across_codegen_units_runs_as_one_module() {
    let path = write_isolated_source(&chained_program(CHAIN));
    let source = path.to_str().expect("a UTF-8 temp path");

    let vm = kira(&["run", "--backend", "vm", source]);
    let native = kira(&["run", "--backend", "llvm", source]);

    let vm_stdout = String::from_utf8_lossy(&vm.stdout).into_owned();
    let native_stdout = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(
        vm.status.success(),
        "the VM run failed: {}",
        String::from_utf8_lossy(&vm.stderr)
    );
    assert!(
        native.status.success(),
        "the native run failed: {}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert_eq!(vm_stdout, format!("{CHAIN}\nab\n"));
    assert_eq!(native_stdout, vm_stdout, "the two backends disagreed");

    // The split itself, where the machine has the cores for one: the second
    // unit's object exists beside the first. A single-core host emits one unit
    // and the parity above is all there is to check.
    let stem = path
        .file_stem()
        .expect("a temp file stem")
        .to_string_lossy()
        .into_owned();
    let second = build_directory(&path).join(format!("{stem}.1.o"));
    if std::thread::available_parallelism().is_ok_and(|cores| cores.get() > 1) {
        assert!(
            second.is_file(),
            "{CHAIN} functions should have been split across units; no {}",
            second.display()
        );
    }

    let _ = std::fs::remove_dir_all(build_directory(&path));
    let _ = std::fs::remove_file(&path);
}
