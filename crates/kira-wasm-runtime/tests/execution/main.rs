//! Differential tests: the VM and the generated wasm module must not disagree.
//!
//! Parity is proven, not asserted. Each case compiles one program from one
//! [`IrProgram`] to bytecode and to wasm, runs the bytecode on the VM and the
//! module on a wasm engine, and requires identical output. The VM is the
//! reference: it is the simplest of the engines and the one the others are
//! defined to mirror, so a disagreement names the side that drifted.
//!
//! Both devices are compared on every case. `wasm32` and `wasm64` run the same
//! lowering through different address widths, and a program cannot tell which
//! memory it is in — so a case that passes on one and fails on the other has
//! found a bug in the width axis, which is exactly what it is here to do.
//!
//! # Why an engine, and why this one
//!
//! Checking the emitted bytes against expected bytes would test the encoder
//! against itself: the claim is that a Kira program *runs*, and only an engine
//! can answer that. The engine is `wasmi` — an interpreter that is a
//! dependency, not a program on the machine. Everything follows from that: it
//! runs in-process, it runs on every machine that can build this workspace, and
//! there is no path where a missing tool turns into a green build that proved
//! nothing.
//!
//! It also reads the spec independently of this crate. An encoder bug that
//! happened to match a mistaken reading here would have to match wasmi's too,
//! which is the point of testing against someone else's implementation.
//!
//! What this does *not* cover is the generated page: the browser host, `fetch`,
//! and `instantiateStreaming` are the browser's, and no wasm engine exercises
//! them. That surface is thin by design — the host supplies two imports over
//! bytes the module already rendered — and it is checked by opening the page.

use kira_wasm_runtime::WasmDevice;
use wasmi::{Caller, Config, Engine, Linker, Module, Store};

/// Collects what a program printed, standing in for the CLI's stdout host.
#[derive(Default)]
struct Collector {
    lines: Vec<String>,
}

impl kira_runtime_abi::HostCapabilities for Collector {
    fn write_line(&mut self, text: &str) {
        self.lines.push(text.to_owned());
    }
}

/// Compiles `source` to IR through the same frontend the CLI drives.
fn lower(source: &str) -> kira_ir::IrProgram {
    lower_modules(source, &[])
}

/// Compiles a multi-module program to IR.
///
/// The modules are handed to the frontend directly rather than written to disk:
/// module *loading* is the CLI's job and needs a filesystem, but module
/// *resolution* is the frontend's and does not — which is exactly why the
/// module texts are an input. So an import resolves here with no temp
/// directory, in a test that also has to run under `wasm32`'s rules.
fn lower_modules(source: &str, modules: &[(&str, &str)]) -> kira_ir::IrProgram {
    let db = salsa::DatabaseImpl::new();
    let modules: Vec<kira_semantics::ModuleSource> = modules
        .iter()
        .map(|&(module, text)| kira_semantics::ModuleSource {
            module: module.to_owned(),
            path: format!("{module}.kira"),
            text: text.to_owned(),
        })
        .collect();
    let program = kira_semantics::SourceProgram::application(
        &db,
        source.to_owned(),
        "test.kira".to_owned(),
        modules,
    );
    let analyzed = kira_semantics::analyzed(&db, program);
    kira_ir::lower(&analyzed)
}

/// Runs `source` on the VM, returning its output lines.
fn run_on_vm(source: &str) -> Result<Vec<String>, String> {
    let ir = lower(source);
    let module = kira_bytecode::compile(&ir).map_err(|error| error.to_string())?;
    let mut host = Collector::default();
    match kira_vm_runtime::execute(&module, &mut host) {
        Ok(_) => Ok(host.lines),
        Err(trap) => Err(trap.to_string()),
    }
}

/// Asserts `source` reclaims every allocation it made.
///
/// The VM's heap counts allocations and frees together, so `current == 0` at
/// exit is the proof that every copy was matched by a drop. Parity says the
/// backends agree on *output*; this says the run was also balanced, which a
/// program that agreed while leaking would pass.
fn assert_heap_balanced(source: &str) {
    let ir = lower(source);
    let module = kira_bytecode::compile(&ir).expect("the program compiles");
    let mut host = Collector::default();
    let outcome = kira_vm_runtime::execute(&module, &mut host).expect("the program runs");
    assert_eq!(
        outcome.heap.current, 0,
        "every allocation must be reclaimed, {} allocated and {} freed",
        outcome.heap.allocated, outcome.heap.freed
    );
}

/// What the module told the host, as the host saw it.
#[derive(Default)]
struct Host {
    lines: Vec<String>,
    /// The message from a `trap` call, if the program raised one.
    ///
    /// A Kira trap arrives as a call and *then* an `unreachable`, so the engine
    /// error that follows is the stop, not the reason. The reason is this.
    trap: Option<String>,
}

/// Reads a Kira string's bytes out of the module's exported memory.
fn read_text(caller: &Caller<'_, Host>, pointer: u64, length: u32) -> String {
    let memory = caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .expect("the module exports its memory");
    let data = memory.data(caller);
    let start = pointer as usize;
    let end = start + length as usize;
    String::from_utf8_lossy(&data[start..end]).into_owned()
}

/// Runs a compiled module, returning its output lines or why it stopped.
///
/// The two imports are the same two the browser supplies, over the same bytes:
/// this host writes them into a `Vec` where the page writes them into the DOM.
fn execute(bytes: &[u8], device: WasmDevice) -> Result<Vec<String>, String> {
    let mut config = Config::default();
    // Off by default in wasmi, and a `wasm64` module is rejected outright
    // without it.
    config.wasm_memory64(true);
    let engine = Engine::new(&config);

    let module = Module::new(&engine, bytes).map_err(|error| format!("invalid module: {error}"))?;
    let mut store = Store::new(&engine, Host::default());
    let mut linker = Linker::new(&engine);

    // A pointer is an `i32` under Memory32 and an `i64` under Memory64, so the
    // host's own signatures move with the device — exactly as the browser's do.
    match device {
        WasmDevice::Wasm32 => {
            linker
                .func_wrap(
                    "kira",
                    "print",
                    |caller: Caller<'_, Host>, pointer: i32, length: i32| {
                        let text = read_text(&caller, pointer as u32 as u64, length as u32);
                        let mut caller = caller;
                        caller.data_mut().lines.push(text);
                    },
                )
                .map_err(|error| error.to_string())?;
            linker
                .func_wrap(
                    "kira",
                    "trap",
                    |caller: Caller<'_, Host>, pointer: i32, length: i32| {
                        let text = read_text(&caller, pointer as u32 as u64, length as u32);
                        let mut caller = caller;
                        caller.data_mut().trap = Some(text);
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        WasmDevice::Wasm64 => {
            linker
                .func_wrap(
                    "kira",
                    "print",
                    |caller: Caller<'_, Host>, pointer: i64, length: i32| {
                        let text = read_text(&caller, pointer as u64, length as u32);
                        let mut caller = caller;
                        caller.data_mut().lines.push(text);
                    },
                )
                .map_err(|error| error.to_string())?;
            linker
                .func_wrap(
                    "kira",
                    "trap",
                    |caller: Caller<'_, Host>, pointer: i64, length: i32| {
                        let text = read_text(&caller, pointer as u64, length as u32);
                        let mut caller = caller;
                        caller.data_mut().trap = Some(text);
                    },
                )
                .map_err(|error| error.to_string())?;
        }
    }

    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .map_err(|error| format!("cannot instantiate: {error}"))?;
    let main = instance
        .get_typed_func::<(), ()>(&store, "kira_main")
        .map_err(|error| format!("no entrypoint: {error}"))?;

    match main.call(&mut store, ()) {
        Ok(()) => Ok(store.data().lines.clone()),
        Err(error) => Err(match &store.data().trap {
            // A Kira trap: report what Kira said, the way the CLI does.
            Some(reason) => format!("kirac: runtime trap: {reason}"),
            // Anything else is the engine refusing the module, which is a bug
            // in this backend and must not be mistaken for a program's trap.
            None => format!("the module faulted with no Kira trap: {error}"),
        }),
    }
}

/// Runs `source` on the VM as a multi-module program.
fn run_modules_on_vm(source: &str, modules: &[(&str, &str)]) -> Result<Vec<String>, String> {
    let ir = lower_modules(source, modules);
    let module = kira_bytecode::compile(&ir).map_err(|error| error.to_string())?;
    let mut host = Collector::default();
    match kira_vm_runtime::execute(&module, &mut host) {
        Ok(_) => Ok(host.lines),
        Err(trap) => Err(trap.to_string()),
    }
}

/// Runs a multi-module program as a wasm module on `device`.
fn run_modules_on_wasm(
    source: &str,
    modules: &[(&str, &str)],
    device: WasmDevice,
) -> Result<Vec<String>, String> {
    let ir = lower_modules(source, modules);
    let bytes = kira_wasm_runtime::compile(&ir, device).map_err(|error| error.to_string())?;
    execute(&bytes, device)
}

/// Asserts the VM and both wasm devices agree on a multi-module program.
///
/// Imports vanish in the frontend: the IR a wasm module is built from carries
/// no trace of how many files the program was written in. That is the claim —
/// so the way to check it is to run a program whose answer comes out of an
/// imported module and require the same three engines to agree on it.
fn assert_module_parity(source: &str, modules: &[(&str, &str)]) {
    let expected = run_modules_on_vm(source, modules);
    for device in [WasmDevice::Wasm32, WasmDevice::Wasm64] {
        let actual = run_modules_on_wasm(source, modules, device);
        match (&expected, &actual) {
            (Ok(vm), Ok(wasm)) => assert_eq!(
                vm,
                wasm,
                "the vm and {} disagree on output for:\n{source}",
                device.label()
            ),
            (Err(vm), Err(wasm)) => assert!(
                wasm.contains(vm.as_str()),
                "the vm trapped with `{vm}` but {} reported:\n{wasm}\nfor:\n{source}",
                device.label()
            ),
            (vm, wasm) => panic!(
                "the vm produced {vm:?} but {} produced {wasm:?} for:\n{source}",
                device.label()
            ),
        }
    }
}

/// Runs `source` as a wasm module on `device`, returning its output lines.
fn run_on_wasm(source: &str, device: WasmDevice) -> Result<Vec<String>, String> {
    let ir = lower(source);
    let bytes = kira_wasm_runtime::compile(&ir, device).map_err(|error| error.to_string())?;
    execute(&bytes, device)
}

/// Asserts the VM and both wasm devices agree on `source`.
///
/// The three engines are independent, so they run at once: each case is a
/// module built and interpreted twice over, and waiting for one width before
/// starting the other is the case paying twice for nothing. The harness
/// parallelises across cases; this is what parallelises inside one.
fn assert_parity(source: &str) {
    let (expected, actuals) = std::thread::scope(|scope| {
        let wasm: Vec<_> = [WasmDevice::Wasm32, WasmDevice::Wasm64]
            .map(|device| (device, scope.spawn(move || run_on_wasm(source, device))))
            .into_iter()
            .collect();
        // The VM is the reference and the cheapest of the three, so it runs
        // here rather than paying for a thread of its own.
        let expected = run_on_vm(source);
        let actuals: Vec<_> = wasm
            .into_iter()
            .map(|(device, worker)| (device, worker.join().expect("the wasm engine ran")))
            .collect();
        (expected, actuals)
    });

    for (device, actual) in actuals {
        let actual = &actual;
        match (&expected, &actual) {
            (Ok(vm), Ok(wasm)) => assert_eq!(
                vm,
                wasm,
                "the vm and {} disagree on output for:\n{source}",
                device.label()
            ),
            (Err(vm), Err(wasm)) => assert!(
                wasm.contains(vm.as_str()),
                "the vm trapped with `{vm}` but {} reported:\n{wasm}\nfor:\n{source}",
                device.label()
            ),
            (Ok(vm), Err(wasm)) => panic!(
                "the vm printed {vm:?} but {} failed:\n{wasm}\nfor:\n{source}",
                device.label()
            ),
            (Err(vm), Ok(wasm)) => panic!(
                "the vm trapped with `{vm}` but {} printed {wasm:?} for:\n{source}",
                device.label()
            ),
        }
    }
}

mod aliases;
mod arrays;
mod attempts;
mod bitwise;
mod calls;
mod classes;
mod closures;
mod control_flow;
mod enums;
mod floats;
mod generics;
mod imports;
mod libraries;
mod matches;
mod memory;
mod ownership;
mod scalars;
mod structs;
mod widths;
