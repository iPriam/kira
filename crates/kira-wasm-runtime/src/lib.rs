//! WASM module assembly for the Web target.
//!
//! Layer 4 of the Kira package graph.
//!
//! # Shape
//!
//! The backend consumes the same verified [`IrProgram`] the VM's bytecode
//! compiler and the LLVM backend do, and writes a WebAssembly module byte by
//! byte — no external assembler, no linker, no toolchain to install. A Kira
//! program targeting the Web needs `kirac` and nothing else, which is why the
//! encoder lives here rather than behind `emcc` or `wasm-ld`.
//!
//! # Parity
//!
//! A Kira program must say the same thing on every engine, so the module
//! carries its own runtime instead of borrowing the host's:
//!
//! - integers render through a generated decimal loop, and floats through a
//!   generated Dragon4 — *not* through JavaScript's `Number.toString`, which
//!   answers a different question and disagrees with Rust's `f64` `Display` on
//!   values as ordinary as `1e21`,
//! - integer arithmetic wraps like the VM's, and `Int` minimum over `-1` is
//!   branched around because `i64.div_s` would trap where the VM wraps,
//! - division by zero raises Kira's trap, carrying the VM's own message.
//!
//! The host supplies two imports — `kira.print` and `kira.trap` — and both take
//! bytes the module already rendered. Nothing a Kira program can observe is
//! decided by the embedder.
//!
//! # Address width
//!
//! [`WasmDevice`] picks the memory: `wasm32` for the baseline 32-bit memory and
//! `wasm64` for the Memory64 proposal's 64-bit one. It is one axis through the
//! whole backend — every pointer is emitted at the module's width — so both
//! devices run the same lowering and differ only in the shape of an address.

use std::path::{Path, PathBuf};

use kira_ir::{IrFunction, IrProgram};
use kira_semantics_model::Type;

pub mod encode;
pub mod error;
pub mod func;
pub mod layout;
pub mod literals;
pub mod lower;
pub mod module;
pub mod rt;
pub mod web;

pub use error::WasmError;
pub use func::AddrType;

use encode::ValType;
use literals::Literals;
use lower::Lowering;
use module::Module;
use rt::Runtime;

/// Which wasm memory a build targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WasmDevice {
    /// `wasm32`: the baseline 32-bit memory, which every engine has.
    Wasm32,
    /// `wasm64`: the Memory64 proposal's 64-bit memory.
    ///
    /// Needs an engine with Memory64 enabled. A Kira program does not change
    /// shape between the two — `Int` is 64-bit either way — so this widens what
    /// is addressable, not what is computable.
    Wasm64,
}

impl WasmDevice {
    /// The device's spelling on the command line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Wasm32 => "wasm32",
            Self::Wasm64 => "wasm64",
        }
    }

    /// Resolves a `--device` value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "wasm32" => Some(Self::Wasm32),
            "wasm64" => Some(Self::Wasm64),
            _ => None,
        }
    }

    /// The address width this device's memory uses.
    pub fn addr(self) -> AddrType {
        match self {
            Self::Wasm32 => AddrType::I32,
            Self::Wasm64 => AddrType::I64,
        }
    }
}

/// What a wasm build should produce, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct WasmBuildOptions {
    /// The module name, used in the generated page's title.
    pub module_name: String,
    /// Which memory to target.
    pub device: WasmDevice,
    /// Where the `.wasm` module is written.
    pub wasm_path: PathBuf,
    /// Where the page that runs it is written, when one is wanted.
    pub page_path: Option<PathBuf>,
}

/// The artifacts a wasm build produced.
#[derive(Debug, Clone, PartialEq)]
pub struct WasmArtifacts {
    /// The emitted module.
    pub wasm: PathBuf,
    /// The generated page, when one was requested.
    pub page: Option<PathBuf>,
}

/// Compiles `program` to a WebAssembly module.
///
/// Pure: IR in, bytes out. That is what lets a test assemble a module and run
/// it without a filesystem anywhere in the way.
pub fn compile(program: &IrProgram, device: WasmDevice) -> Result<Vec<u8>, WasmError> {
    let mut module = Module::new(device.addr(), u64::from(layout::INITIAL_PAGES));
    let mut literals = Literals::new();

    let runtime = Runtime::declare(&mut module).ok_or(WasmError::Wiring)?;

    // Every Kira function gets an index before any body is emitted, so a call
    // can name a function that has not been lowered yet — which is what direct
    // and mutual recursion both need.
    let mut handles = Vec::with_capacity(program.functions.len());
    for function in &program.functions {
        let (params, results) = signature(function, device)?;
        handles.push(module.declare(params, results));
    }

    if !runtime.define(&mut module, &mut literals) {
        return Err(WasmError::Wiring);
    }

    for (index, function) in program.functions.iter().enumerate() {
        let handle = *handles.get(index).ok_or(WasmError::Wiring)?;
        let (params, results) = signature(function, device)?;
        let mut func = func::Func::new(device.addr(), params, results);
        Lowering::new(program, &runtime, &mut literals, &handles).function(&mut func, function)?;
        if !module.define(handle, func) {
            return Err(WasmError::Wiring);
        }
    }

    let main = *handles
        .get(program.main as usize)
        .ok_or(WasmError::UnknownFunction(program.main))?;
    if !module.export(main, web::MAIN_EXPORT) {
        return Err(WasmError::Wiring);
    }

    // Every literal is interned by now, so the heap starts where they end — and
    // the memory has to be big enough to hold them at instantiation, which a
    // program with more than a page of strings would otherwise not be.
    module.data(u64::from(layout::LITERALS), literals.data().to_vec());
    module.reserve(literals.heap_base());
    if !module.set_global_init(runtime.heap, literals.heap_base()) {
        return Err(WasmError::Wiring);
    }

    Ok(module.finish())
}

/// The wasm signature of a Kira function.
fn signature(
    function: &IrFunction,
    device: WasmDevice,
) -> Result<(Vec<ValType>, Vec<ValType>), WasmError> {
    let mut params = Vec::with_capacity(function.param_count as usize);
    for slot in 0..function.param_count {
        let ty = function
            .locals
            .get(slot as usize)
            .copied()
            .ok_or_else(|| WasmError::VoidLocal(function.name.clone()))?;
        params.push(
            value_type(ty, device)?.ok_or_else(|| WasmError::VoidLocal(function.name.clone()))?,
        );
    }
    let results = match value_type(function.return_type, device)? {
        Some(value) => vec![value],
        None => Vec::new(),
    };
    Ok((params, results))
}

/// The wasm value type a Kira type occupies on `device`, or `None` for `Void`.
fn value_type(ty: Type, device: WasmDevice) -> Result<Option<ValType>, WasmError> {
    Ok(match ty {
        // A `String` is an address, so it is as wide as the memory is.
        Type::String => Some(device.addr().val()),
        other => Lowering::val_type(other)?,
    })
}

/// Compiles `program` and writes the artifacts `options` asks for.
pub fn build(program: &IrProgram, options: &WasmBuildOptions) -> Result<WasmArtifacts, WasmError> {
    let bytes = compile(program, options.device)?;
    write(&options.wasm_path, &bytes)?;

    let page = match &options.page_path {
        Some(path) => {
            let wasm_file = options
                .wasm_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("module.wasm");
            write(path, web::page(&options.module_name, wasm_file).as_bytes())?;
            Some(path.clone())
        }
        None => None,
    };

    Ok(WasmArtifacts {
        wasm: options.wasm_path.clone(),
        page,
    })
}

/// Writes `bytes` to `path`, reporting the path that failed.
fn write(path: &Path, bytes: &[u8]) -> Result<(), WasmError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| WasmError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, bytes).map_err(|source| WasmError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_ir::{IrCallee, IrExpr, IrStmt};
    use kira_runtime_abi::Execution;
    use la_arena::Arena;

    /// Hand-builds the IR for `@Main function main() { print(<expr>) return }`.
    fn program_printing(expr: IrExpr) -> IrProgram {
        let mut exprs = Arena::new();
        let value = exprs.alloc(expr);
        let call = exprs.alloc(IrExpr::Call {
            callee: IrCallee::Print,
            args: vec![value],
            result: Type::Void,
        });
        IrProgram {
            functions: vec![IrFunction {
                name: "main".to_owned(),
                param_count: 0,
                locals: Vec::new(),
                return_type: Type::Void,
                execution: Execution::Inherited,
                body: vec![IrStmt::Eval { expr: call }, IrStmt::Return { value: None }],
            }],
            structs: Default::default(),
            main: 0,
            exprs,
        }
    }

    #[test]
    fn a_compiled_module_starts_with_the_wasm_header() {
        for device in [WasmDevice::Wasm32, WasmDevice::Wasm64] {
            let bytes = compile(&program_printing(IrExpr::Int(1)), device).expect("compiles");
            assert_eq!(&bytes[..8], &encode::HEADER, "{}", device.label());
        }
    }

    #[test]
    fn the_memory_is_flagged_64_bit_only_for_wasm64() {
        // The limits flag is what makes a module Memory64; a build that got it
        // wrong would still be a valid module, just the other one.
        let narrow =
            compile(&program_printing(IrExpr::Int(1)), WasmDevice::Wasm32).expect("compiles");
        let wide =
            compile(&program_printing(IrExpr::Int(1)), WasmDevice::Wasm64).expect("compiles");
        assert_ne!(narrow, wide);
    }

    #[test]
    fn a_device_round_trips_its_label() {
        for device in [WasmDevice::Wasm32, WasmDevice::Wasm64] {
            assert_eq!(WasmDevice::parse(device.label()), Some(device));
        }
        assert_eq!(WasmDevice::parse("wasm128"), None);
    }

    #[test]
    fn a_string_is_an_address_at_the_memorys_width() {
        assert_eq!(
            value_type(Type::String, WasmDevice::Wasm32).expect("a type"),
            Some(ValType::I32)
        );
        assert_eq!(
            value_type(Type::String, WasmDevice::Wasm64).expect("a type"),
            Some(ValType::I64)
        );
        // `Int` is 64-bit on both: the device widens addresses, not arithmetic.
        for device in [WasmDevice::Wasm32, WasmDevice::Wasm64] {
            assert_eq!(
                value_type(Type::Int, device).expect("a type"),
                Some(ValType::I64)
            );
            assert_eq!(value_type(Type::Void, device).expect("a type"), None);
        }
    }

    #[test]
    fn an_ill_typed_program_is_refused_rather_than_lowered() {
        assert!(matches!(
            value_type(Type::Error, WasmDevice::Wasm32),
            Err(WasmError::ErrorType)
        ));
    }
}
