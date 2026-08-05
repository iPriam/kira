//! The release plan, proved on real programs rather than hand-built modules.
//!
//! The unit tests below this crate prove the pieces: `kira_ir::mid` plans, the
//! compiler writes the plan into the module, and the VM releases what it names.
//! What none of them can prove is that the plan is *complete* for a program a
//! user would write — a plan that omits a live slot is not a crash, it is a
//! leak, and nothing in the output of a correct-looking run says so.
//!
//! So these compile source the whole way down and ask the VM's own accounting.
//! Its heap counts allocations and frees, and `current == 0` at exit says every
//! object the program made came back. A slot the plan forgot shows up here as a
//! non-zero balance, on the exact shapes whose ownership the plan reasons
//! about: strings in locals, `borrow mut` parameters, and values that outlive
//! several frames.
//!
//! The native side of the same claim is `backend_parity/heap_balance.rs`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use kira_runtime_abi::CapturingHost;

/// Writes `source` to its own temp directory and returns the path.
fn write_source(source: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let directory = std::env::temp_dir().join(format!("kira_release_{pid}_{unique}"));
    std::fs::create_dir_all(&directory).expect("temp dir");
    let path = directory.join("program.kira");
    std::fs::write(&path, source).expect("write temp source");
    path
}

/// Compiles `source`, runs it on the VM, and answers with its output and the
/// heap balance at exit.
fn run(source: &str) -> (Vec<String>, u64) {
    let path = write_source(source);
    let compiled = crate::frontend::compile(&path).expect("the frontend reads the program");
    assert!(
        !compiled.has_errors(),
        "the program must compile cleanly: {:?}",
        compiled.diagnostics
    );
    let module = kira_bytecode::compile(&compiled.ir).expect("compiles to bytecode");
    module.validate().expect("a compiled module is well-formed");

    let mut host = CapturingHost::new();
    let outcome = kira_vm_runtime::execute(&module, &mut host).expect("a clean run");
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
    (host.lines().to_vec(), outcome.heap.current)
}

/// Every slot class the plan reasons about, in one program: strings held in
/// locals, a string that is only ever read, one moved into a struct field, and
/// one returned past the frame that made it.
#[test]
fn a_program_holding_strings_balances_under_its_plan() {
    let (output, live) = run(r#"
struct Label {
    var text: String
}

function decorate(word: String) -> String {
    let inner = "[" + word + "]"
    return inner
}

function describe(l: borrow Label) -> String {
    let copy = l.text
    return copy
}

@Main
function main() {
    var i = 0
    while i < 3 {
        let held = decorate("row")
        let label = Label { text: held }
        print(describe(label))
        i = i + 1
    }
    return
}
"#);
    assert_eq!(output, ["[row]", "[row]", "[row]"]);
    assert_eq!(live, 0, "the plan left {live} objects unreleased");
}

/// The case the two engines answer differently. A `borrow mut` parameter is a
/// pointer into the caller's frame on native and a copy the callee owns on the
/// VM, so the plan the VM is given must keep the slot that the native plan
/// drops. Given the wrong one, every call here leaks its argument.
#[test]
fn a_mutable_string_borrow_balances_under_its_plan() {
    let (output, live) = run(r#"
struct Note {
    var body: String
}

function retitle(n: borrow mut Note, to: String) {
    n.body = to + "!"
    return
}

function grow(text: borrow mut String) {
    text = text + "+"
    return
}

@Main
function main() {
    var note = Note { body: "first" }
    retitle(note, "second")
    retitle(note, "third")
    print(note.body)

    var word = "a"
    var i = 0
    while i < 3 {
        grow(word)
        i = i + 1
    }
    print(word)
    return
}
"#);
    assert_eq!(output, ["third!", "a+++"]);
    assert_eq!(live, 0, "the plan left {live} objects unreleased");
}
