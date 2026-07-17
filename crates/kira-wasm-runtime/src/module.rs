//! The module builder: signatures, imports, globals, data, and section order.
//!
//! A module is assembled by declaring imports first (they occupy the low end of
//! the function index space), then defining functions. Indices are handed out
//! by the builder rather than computed by callers, so a function can only be
//! called through a handle the builder issued.

use crate::encode::{Bytes, HEADER, ValType, section};
use crate::func::{AddrType, Func, FuncIdx, GlobalIdx};

/// A function signature.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FuncType {
    params: Vec<ValType>,
    results: Vec<ValType>,
}

/// An imported function.
#[derive(Debug, Clone)]
struct Import {
    module: String,
    name: String,
    type_index: u32,
}

/// A defined function: its signature index and its body.
#[derive(Debug)]
struct Defined {
    type_index: u32,
    body: Bytes,
    export: Option<String>,
}

/// A mutable global initialized to a constant.
#[derive(Debug)]
struct Global {
    ty: ValType,
    init: u64,
}

/// Builds a complete WebAssembly module.
#[derive(Debug)]
pub struct Module {
    addr: AddrType,
    types: Vec<FuncType>,
    imports: Vec<Import>,
    defined: Vec<Defined>,
    globals: Vec<Global>,
    data: Vec<(u64, Vec<u8>)>,
    memory_pages: u64,
}

impl Module {
    /// Starts an empty module with `pages` 64KiB pages of memory at `addr`
    /// width.
    pub fn new(addr: AddrType, pages: u64) -> Self {
        Self {
            addr,
            types: Vec::new(),
            imports: Vec::new(),
            defined: Vec::new(),
            globals: Vec::new(),
            data: Vec::new(),
            memory_pages: pages,
        }
    }

    /// The address width this module's memory uses.
    pub fn addr(&self) -> AddrType {
        self.addr
    }

    /// Starts a function body at this module's address width.
    pub fn func(&self, params: Vec<ValType>, results: Vec<ValType>) -> Func {
        Func::new(self.addr, params, results)
    }

    /// Declares an imported function, returning its index.
    ///
    /// Every import must be declared before the first definition: imports own
    /// the low end of the function index space, and a definition's index is
    /// only stable once no more imports can appear ahead of it.
    ///
    /// Returns `None` if a function has already been defined, which is the one
    /// way that ordering can be violated.
    pub fn import(
        &mut self,
        module: &str,
        name: &str,
        params: Vec<ValType>,
        results: Vec<ValType>,
    ) -> Option<FuncIdx> {
        if !self.defined.is_empty() {
            return None;
        }
        let type_index = self.intern_type(params, results);
        let index = self.imports.len() as u32;
        self.imports.push(Import {
            module: module.to_owned(),
            name: name.to_owned(),
            type_index,
        });
        Some(FuncIdx(index))
    }

    /// Reserves an index for a function whose body comes later.
    ///
    /// Mutual recursion needs the index before the body exists — the runtime's
    /// helpers call each other, and Kira functions may recurse — so a slot is
    /// reserved first and filled by [`Module::define`].
    pub fn declare(&mut self, params: Vec<ValType>, results: Vec<ValType>) -> FuncIdx {
        let type_index = self.intern_type(params, results);
        let index = self.imports.len() + self.defined.len();
        self.defined.push(Defined {
            type_index,
            body: Bytes::new(),
            export: None,
        });
        FuncIdx(index as u32)
    }

    /// Fills in the body of a previously [`declare`](Module::declare)d function.
    ///
    /// Returns `false` for a handle this module never issued, so a mismatched
    /// index is reported rather than silently dropping a body on the floor.
    pub fn define(&mut self, index: FuncIdx, func: Func) -> bool {
        let Some(slot) = self.slot_mut(index) else {
            return false;
        };
        slot.body = func.finish();
        true
    }

    /// Exports a defined function under `name`.
    ///
    /// Returns `false` for a handle this module never issued.
    pub fn export(&mut self, index: FuncIdx, name: &str) -> bool {
        let Some(slot) = self.slot_mut(index) else {
            return false;
        };
        slot.export = Some(name.to_owned());
        true
    }

    /// Declares a mutable global with a constant initializer.
    pub fn global(&mut self, ty: ValType, init: u64) -> GlobalIdx {
        let index = self.globals.len() as u32;
        self.globals.push(Global { ty, init });
        GlobalIdx(index)
    }

    /// Declares a mutable address-width global holding `init`.
    pub fn addr_global(&mut self, init: u64) -> GlobalIdx {
        self.global(self.addr.val(), init)
    }

    /// Replaces a global's constant initializer.
    ///
    /// The heap's bump pointer starts at the first address past the literal
    /// pool, which is only known once the last literal is interned — after the
    /// bodies that intern them are emitted. The global is declared early so
    /// those bodies can name it, and its start is set here once.
    ///
    /// Returns `false` for a handle this module never issued.
    pub fn set_global_init(&mut self, index: GlobalIdx, init: u64) -> bool {
        match self.globals.get_mut(index.0 as usize) {
            Some(global) => {
                global.init = init;
                true
            }
            None => false,
        }
    }

    /// Places `bytes` in linear memory at `offset`.
    pub fn data(&mut self, offset: u64, bytes: Vec<u8>) {
        self.data.push((offset, bytes));
    }

    /// Grows the memory the module starts with to at least `bytes`.
    ///
    /// A data segment is written at instantiation, before any code runs, so it
    /// cannot grow the memory it needs — a module whose literals run past its
    /// initial memory is refused by the engine and never reaches `main`. The
    /// allocator grows memory for the heap at runtime; the literals have to fit
    /// before there is a runtime.
    pub fn reserve(&mut self, bytes: u64) {
        let pages = bytes.div_ceil(u64::from(crate::layout::PAGE_BYTES));
        self.memory_pages = self.memory_pages.max(pages);
    }

    /// Encodes the finished module.
    pub fn finish(self) -> Vec<u8> {
        let mut out = Bytes::new();
        out.raw(&HEADER);

        let mut types = Bytes::new();
        types.u32(self.types.len() as u32);
        for ty in &self.types {
            types.byte(0x60);
            types.u32(ty.params.len() as u32);
            for param in &ty.params {
                types.byte(param.code());
            }
            types.u32(ty.results.len() as u32);
            for result in &ty.results {
                types.byte(result.code());
            }
        }
        out.section(section::TYPE, &types);

        if !self.imports.is_empty() {
            let mut imports = Bytes::new();
            imports.u32(self.imports.len() as u32);
            for import in &self.imports {
                imports.name(&import.module);
                imports.name(&import.name);
                imports.byte(0x00);
                imports.u32(import.type_index);
            }
            out.section(section::IMPORT, &imports);
        }

        let mut functions = Bytes::new();
        functions.u32(self.defined.len() as u32);
        for function in &self.defined {
            functions.u32(function.type_index);
        }
        out.section(section::FUNCTION, &functions);

        let mut memory = Bytes::new();
        memory.u32(1);
        // Limits flags: bit 0 is "has a maximum", bit 2 is "64-bit addresses".
        // A minimum with no maximum — the allocator grows memory as a program
        // needs it, so a ceiling here would be a second, quieter limit.
        memory.byte(match self.addr {
            AddrType::I32 => 0x00,
            AddrType::I64 => 0x04,
        });
        memory.u64(self.memory_pages);
        out.section(section::MEMORY, &memory);

        if !self.globals.is_empty() {
            let mut globals = Bytes::new();
            globals.u32(self.globals.len() as u32);
            for global in &self.globals {
                globals.byte(global.ty.code());
                globals.byte(0x01);
                Self::const_expr(&mut globals, global.ty, global.init);
            }
            out.section(section::GLOBAL, &globals);
        }

        let exports: Vec<(&str, u32)> = self
            .defined
            .iter()
            .enumerate()
            .filter_map(|(offset, function)| {
                let name = function.export.as_deref()?;
                Some((name, (self.imports.len() + offset) as u32))
            })
            .collect();
        let mut export_section = Bytes::new();
        // The memory is always exported: the embedder reads printed strings out
        // of it, so a module that hid it could not say anything.
        export_section.u32(exports.len() as u32 + 1);
        export_section.name("memory");
        export_section.byte(0x02);
        export_section.u32(0);
        for (name, index) in exports {
            export_section.name(name);
            export_section.byte(0x00);
            export_section.u32(index);
        }
        out.section(section::EXPORT, &export_section);

        let mut code = Bytes::new();
        code.u32(self.defined.len() as u32);
        for function in &self.defined {
            code.raw(function.body.as_slice());
        }
        out.section(section::CODE, &code);

        if !self.data.is_empty() {
            let mut data = Bytes::new();
            data.u32(self.data.len() as u32);
            for (offset, bytes) in &self.data {
                data.u32(0);
                // An active segment's offset is a constant expression at the
                // memory's address width, so Memory64 spells it `i64.const`.
                Self::const_expr(&mut data, self.addr.val(), *offset);
                data.u32(bytes.len() as u32);
                data.raw(bytes);
            }
            out.section(section::DATA, &data);
        }

        out.into_vec()
    }

    /// Writes a constant expression yielding `value` at `ty`.
    fn const_expr(out: &mut Bytes, ty: ValType, value: u64) {
        match ty {
            ValType::I32 => {
                out.byte(crate::func::op::I32_CONST);
                out.i32(value as i32);
            }
            ValType::I64 => {
                out.byte(crate::func::op::I64_CONST);
                out.i64(value as i64);
            }
            ValType::F64 => {
                out.byte(crate::func::op::F64_CONST);
                out.f64(f64::from_bits(value));
            }
        }
        out.byte(crate::func::op::END);
    }

    /// Returns the index of `params -> results`, adding it if it is new.
    fn intern_type(&mut self, params: Vec<ValType>, results: Vec<ValType>) -> u32 {
        let wanted = FuncType { params, results };
        match self.types.iter().position(|ty| *ty == wanted) {
            Some(index) => index as u32,
            None => {
                self.types.push(wanted);
                (self.types.len() - 1) as u32
            }
        }
    }

    /// Borrows the definition behind a handle, if this module issued it.
    fn slot_mut(&mut self, index: FuncIdx) -> Option<&mut Defined> {
        let offset = (index.0 as usize).checked_sub(self.imports.len())?;
        self.defined.get_mut(offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_starts_with_the_magic_and_version() {
        let module = Module::new(AddrType::I32, 1);
        let bytes = module.finish();
        assert_eq!(&bytes[..8], &HEADER);
    }

    #[test]
    fn identical_signatures_share_one_type_index() {
        let mut module = Module::new(AddrType::I32, 1);
        let first = module.declare(vec![ValType::I32], vec![ValType::I64]);
        let second = module.declare(vec![ValType::I32], vec![ValType::I64]);
        let third = module.declare(vec![ValType::I64], vec![ValType::I64]);
        assert_ne!(first, second);
        assert_eq!(module.types.len(), 2);
        assert_eq!(module.defined[0].type_index, module.defined[1].type_index);
        assert_ne!(module.defined[0].type_index, module.defined[2].type_index);
        let _ = third;
    }

    #[test]
    fn imports_take_the_low_indices_and_definitions_follow() {
        let mut module = Module::new(AddrType::I32, 1);
        let print = module
            .import(
                "kira",
                "print",
                vec![ValType::I32, ValType::I32],
                Vec::new(),
            )
            .expect("no definitions yet");
        assert_eq!(print, FuncIdx(0));
        assert_eq!(module.declare(Vec::new(), Vec::new()), FuncIdx(1));
    }

    #[test]
    fn an_import_after_a_definition_is_refused() {
        let mut module = Module::new(AddrType::I32, 1);
        module.declare(Vec::new(), Vec::new());
        // Accepting this would renumber the definition that already exists.
        assert_eq!(module.import("kira", "print", Vec::new(), Vec::new()), None);
    }

    #[test]
    fn a_body_or_export_for_an_unknown_handle_is_refused() {
        let mut module = Module::new(AddrType::I32, 1);
        let real = module.declare(Vec::new(), Vec::new());
        assert!(module.define(real, Func::new(AddrType::I32, Vec::new(), Vec::new())));
        assert!(!module.define(
            FuncIdx(99),
            Func::new(AddrType::I32, Vec::new(), Vec::new())
        ));
        assert!(!module.export(FuncIdx(99), "main"));
    }
}
