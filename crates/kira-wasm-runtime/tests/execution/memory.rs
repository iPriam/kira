//! Parity for programs that outgrow the module's first page.

use kira_wasm_runtime::WasmDevice;

use crate::{assert_parity, run_on_wasm};

#[test]
fn a_program_whose_literals_outgrow_a_page_still_instantiates() {
    // Literals are written by a data segment at instantiation, before any code
    // runs, so they cannot grow the memory they need the way the heap can. A
    // module that reserved one page for them was refused by the engine outright
    // — the program never started, whatever it did.
    let mut source = String::from("@Main function main() {\n");
    let mut expected = Vec::new();
    for index in 0..2000 {
        // Distinct, so none of them dedup away, and 40 bytes each: past one
        // page in total and nowhere near it individually.
        let line = format!("literal number {index:04} padding padding pad");
        source.push_str(&format!("    print(\"{line}\")\n"));
        expected.push(line);
    }
    source.push_str("    return\n}");

    for device in [WasmDevice::Wasm32, WasmDevice::Wasm64] {
        let actual = run_on_wasm(&source, device).expect("the module instantiates and runs");
        assert_eq!(actual, expected, "{} lost literals", device.label());
    }
}

#[test]
fn a_concatenating_loop_outgrows_the_first_page() {
    // The allocator never frees, so this is what makes it grow memory: a module
    // that could not grow would trap partway through instead of printing.
    assert_parity(
        r#"@Main function main() {
            var text = ""
            var i = 0
            while i < 2000 {
                text = text + "0123456789012345678901234567890123456789"
                i = i + 1
            }
            print(text == "")
            print(i)
            return
        }"#,
    );
}

// ----- structs ---------------------------------------------------------
//
// A struct is a pointer into linear memory here, and this heap never frees. So
// the only thing standing between value semantics and a shared object is the
// deep copy the lowering emits — and these cases are what prove it happens
// where the VM's copy happens.
