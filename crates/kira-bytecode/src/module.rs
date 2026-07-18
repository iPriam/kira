//! The compiled module: functions, their code, and the string constant pool.
//!
//! A [`Module`] is what the compiler produces and the VM runs. It also has a
//! self-describing byte format ([`Module::to_bytes`] / [`Module::from_bytes`])
//! behind the `KBC1` magic, so a module is a real serializable artifact and not
//! just an in-memory structure. The format is append-only.

use crate::exports::{ExportTable, ExportType, ModuleExport};
use crate::op::{DecodeError, Instruction, decode, encode};
use kira_runtime_abi::{BridgeValueTag, Execution};

/// The magic bytes that open a serialized module: "KBC1".
pub const MAGIC: [u8; 4] = *b"KBC1";

/// The entrypoint slot's value when a module has no entrypoint (a library).
///
/// A sentinel in the existing `u32` rather than a new field, which keeps the
/// format append-only in the strictest sense — the byte layout does not move at
/// all. `u32::MAX` is safe to claim because [`crate::validate`] has always
/// rejected an entrypoint index at or past the function count, so no module
/// that ever decoded cleanly carries this value, and a decoder from before
/// libraries existed rejects a library loudly instead of calling function
/// 4294967295.
pub const NO_ENTRYPOINT: u32 = u32::MAX;

/// A compiled program: a set of functions plus a shared string pool.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    /// The functions; [`Module::main`] indexes into this list.
    pub functions: Vec<FuncProto>,
    /// Index of the entrypoint function, or `None` for a library.
    ///
    /// Serialized as [`NO_ENTRYPOINT`] when absent.
    pub main: Option<u32>,
    /// Deduplicated string constants referenced by `ConstStr`.
    pub strings: Vec<String>,
    /// The `@Export` surface this module offers a consumer.
    ///
    /// Empty for an application and for a library that exports nothing, which is
    /// also what a module written before the exports section existed decodes as.
    pub exports: ExportTable,
}

/// One compiled function: its signature shape and its code.
///
/// The name is an owned `String` (not `kira_core::Symbol`) by design: a
/// serialized module must be self-describing with no interner attached, and
/// the name exists only for diagnostics and disassembly. Revisit alongside
/// the HIR's Symbol migration when structs land.
#[derive(Debug, Clone, PartialEq)]
pub struct FuncProto {
    /// The function's name (for diagnostics and disassembly).
    pub name: String,
    /// Number of leading local slots that are parameters.
    pub param_count: u16,
    /// Total number of local slots the function uses.
    pub local_count: u16,
    /// Which engine owns this function's body.
    ///
    /// [`Execution::Runtime`] for everything a VM-only build produces. A hybrid
    /// build marks the functions whose bodies live in the native half instead:
    /// they keep their slot here so one id indexes both halves, they carry their
    /// signature so a caller knows the arity to marshal, and their `code` is
    /// empty — the body is somewhere else, and saying so is what stops a stray
    /// `Call` from quietly returning unit.
    pub execution: Execution,
    /// The function's instructions; empty for a native function.
    pub code: Vec<Instruction>,
}

impl FuncProto {
    /// Whether this function's body lives in the native half.
    pub fn is_native(&self) -> bool {
        self.execution == Execution::Native
    }
}

/// An error decoding a serialized module.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModuleDecodeError {
    /// The stream did not begin with the `KBC1` magic.
    #[error("not a Kira bytecode module (bad magic)")]
    BadMagic,
    /// The stream ended before a full module was read.
    #[error("truncated module")]
    Truncated,
    /// A string in the module was not valid UTF-8.
    #[error("invalid UTF-8 in module string")]
    InvalidString,
    /// An instruction stream inside the module failed to decode.
    #[error("invalid instruction stream: {0}")]
    Code(#[from] DecodeError),
    /// An execution byte named no engine this build knows.
    #[error("unknown execution engine `{0}` in module")]
    UnknownExecution(u8),
    /// An export's parameter or result named a type that cannot cross.
    ///
    /// Either the tag byte names nothing this build knows, or it names a type
    /// that never travels (a struct, an array, an enum), or a non-handle carried
    /// a class index. All three mean the same thing: the writer meant something
    /// this reader cannot honor, and honoring it anyway would be guessing at
    /// ownership.
    #[error("export `{export}` names type tag `{tag}`, which cannot cross the export boundary")]
    UncrossableExportType {
        /// The consumer-facing name of the offending export.
        export: String,
        /// The tag byte that named nothing crossable.
        tag: u8,
    },
    /// Bytes remained after the last section the format defines.
    ///
    /// Every section is self-delimiting, so leftovers mean the stream was
    /// written by something this build does not understand — reading the prefix
    /// and discarding the rest would be running half an artifact.
    #[error("module has {0} trailing bytes after its last section")]
    TrailingBytes(usize),
}

impl Module {
    /// Serializes the module to its byte format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&self.main.unwrap_or(NO_ENTRYPOINT).to_le_bytes());
        write_u32(&mut out, self.strings.len() as u32);
        for string in &self.strings {
            write_bytes(&mut out, string.as_bytes());
        }
        write_u32(&mut out, self.functions.len() as u32);
        for function in &self.functions {
            write_bytes(&mut out, function.name.as_bytes());
            out.extend_from_slice(&function.param_count.to_le_bytes());
            out.extend_from_slice(&function.local_count.to_le_bytes());
            out.push(function.execution.as_byte());
            let code = encode(&function.code);
            write_bytes(&mut out, &code);
        }
        self.write_exports(&mut out);
        out
    }

    /// Writes the appended exports section.
    ///
    /// Omitted entirely when there is nothing to export, so an application's
    /// bytes are byte-for-byte what they were before exports existed.
    fn write_exports(&self, out: &mut Vec<u8>) {
        if self.exports.is_empty() {
            return;
        }
        write_u32(out, self.exports.classes.len() as u32);
        for class in &self.exports.classes {
            write_bytes(out, class.as_bytes());
        }
        write_u32(out, self.exports.functions.len() as u32);
        for export in &self.exports.functions {
            write_bytes(out, export.name.as_bytes());
            write_bytes(out, export.kira_name.as_bytes());
            write_u32(out, export.function);
            write_u32(out, export.params.len() as u32);
            for param in &export.params {
                write_export_type(out, *param);
            }
            write_export_type(out, export.result);
        }
    }

    /// Deserializes a module from its byte format.
    pub fn from_bytes(bytes: &[u8]) -> Result<Module, ModuleDecodeError> {
        let mut reader = Reader { bytes, offset: 0 };
        if reader.take(4)? != MAGIC {
            return Err(ModuleDecodeError::BadMagic);
        }
        let main = match reader.read_u32()? {
            NO_ENTRYPOINT => None,
            index => Some(index),
        };
        let string_count = reader.read_u32()?;
        let mut strings = Vec::with_capacity(string_count as usize);
        for _ in 0..string_count {
            strings.push(reader.read_string()?);
        }
        let function_count = reader.read_u32()?;
        let mut functions = Vec::with_capacity(function_count as usize);
        for _ in 0..function_count {
            let name = reader.read_string()?;
            let param_count = reader.read_u16()?;
            let local_count = reader.read_u16()?;
            let byte = reader.take(1)?[0];
            let execution =
                Execution::from_byte(byte).ok_or(ModuleDecodeError::UnknownExecution(byte))?;
            let code_bytes = reader.read_len_prefixed()?;
            let code = decode(code_bytes)?;
            functions.push(FuncProto {
                name,
                param_count,
                local_count,
                execution,
                code,
            });
        }
        let exports = read_exports(&mut reader)?;
        if reader.offset != bytes.len() {
            return Err(ModuleDecodeError::TrailingBytes(
                bytes.len() - reader.offset,
            ));
        }
        Ok(Module {
            functions,
            main,
            strings,
            exports,
        })
    }
}

/// Reads the appended exports section, or an empty table when there is none.
///
/// A module written before this section existed ends after its functions, and
/// that absence is the whole compatibility story: no exports, which is exactly
/// what such a module has. A *partial* section is a different thing entirely and
/// is a truncation error, never an empty table.
fn read_exports(reader: &mut Reader<'_>) -> Result<ExportTable, ModuleDecodeError> {
    if reader.is_at_end() {
        return Ok(ExportTable::default());
    }
    let class_count = reader.read_u32()?;
    let mut classes = Vec::new();
    for _ in 0..class_count {
        classes.push(reader.read_string()?);
    }
    let export_count = reader.read_u32()?;
    let mut functions = Vec::new();
    for _ in 0..export_count {
        let name = reader.read_string()?;
        let kira_name = reader.read_string()?;
        let function = reader.read_u32()?;
        let param_count = reader.read_u32()?;
        let mut params = Vec::new();
        for _ in 0..param_count {
            params.push(reader.read_export_type(&name)?);
        }
        let result = reader.read_export_type(&name)?;
        functions.push(ModuleExport {
            name,
            kira_name,
            function,
            params,
            result,
        });
    }
    Ok(ExportTable { classes, functions })
}

fn write_export_type(out: &mut Vec<u8>, ty: ExportType) {
    out.push(ty.tag().0);
    write_u32(out, ty.class_index());
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ModuleDecodeError> {
        let end = self.offset + n;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(ModuleDecodeError::Truncated)?;
        self.offset = end;
        Ok(slice)
    }

    fn is_at_end(&self) -> bool {
        self.offset >= self.bytes.len()
    }

    /// Reads one export type: a tag byte plus its class index.
    ///
    /// `export` names the export in the error, because "some type somewhere is
    /// wrong" is not a diagnosis a consumer can act on.
    fn read_export_type(&mut self, export: &str) -> Result<ExportType, ModuleDecodeError> {
        let tag = BridgeValueTag(self.take(1)?[0]);
        let class = self.read_u32()?;
        ExportType::from_wire(tag, class).ok_or_else(|| ModuleDecodeError::UncrossableExportType {
            export: export.to_owned(),
            tag: tag.0,
        })
    }

    fn read_u16(&mut self) -> Result<u16, ModuleDecodeError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, ModuleDecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_len_prefixed(&mut self) -> Result<&'a [u8], ModuleDecodeError> {
        let len = self.read_u32()? as usize;
        self.take(len)
    }

    fn read_string(&mut self) -> Result<String, ModuleDecodeError> {
        let bytes = self.read_len_prefixed()?;
        core::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| ModuleDecodeError::InvalidString)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_round_trips_through_bytes() {
        let module = Module {
            exports: Default::default(),
            main: Some(1),
            strings: vec!["hello".to_owned(), "world".to_owned()],
            functions: vec![
                FuncProto {
                    name: "helper".to_owned(),
                    param_count: 1,
                    local_count: 2,
                    execution: Execution::Runtime,
                    code: vec![Instruction::LoadLocal(0), Instruction::Return],
                },
                FuncProto {
                    name: "main".to_owned(),
                    param_count: 0,
                    local_count: 0,
                    execution: Execution::Runtime,
                    code: vec![
                        Instruction::ConstStr(0),
                        Instruction::Print,
                        Instruction::ReturnVoid,
                    ],
                },
            ],
        };
        let bytes = module.to_bytes();
        assert_eq!(Module::from_bytes(&bytes).unwrap(), module);
    }

    #[test]
    fn bad_magic_is_rejected() {
        assert_eq!(
            Module::from_bytes(b"XXXX").unwrap_err(),
            ModuleDecodeError::BadMagic
        );
    }

    /// A library module: no entrypoint, and the functions a consumer calls.
    fn library_module() -> Module {
        Module {
            exports: Default::default(),
            main: None,
            strings: Vec::new(),
            functions: vec![FuncProto {
                name: "add".to_owned(),
                param_count: 2,
                local_count: 2,
                execution: Execution::Runtime,
                code: vec![Instruction::LoadLocal(0), Instruction::Return],
            }],
        }
    }

    #[test]
    fn a_library_module_round_trips_with_no_entrypoint() {
        let module = library_module();
        let bytes = module.to_bytes();
        let decoded = Module::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, module);
        assert_eq!(decoded.main, None);
    }

    #[test]
    fn the_no_entrypoint_sentinel_is_pinned_in_the_bytes() {
        // The wire value is part of the format, so it is spelled out here
        // rather than only round-tripped: a change to it is a format change and
        // must fail this test.
        let bytes = library_module().to_bytes();
        assert_eq!(&bytes[0..4], &MAGIC);
        assert_eq!(&bytes[4..8], &[0xff, 0xff, 0xff, 0xff]);
        assert_eq!(NO_ENTRYPOINT, u32::MAX);
    }

    #[test]
    fn an_entrypoint_index_is_never_the_sentinel() {
        // The two states are distinguishable in both directions: a real index
        // decodes as `Some`, never as a library.
        let bytes = Module {
            exports: Default::default(),
            main: Some(0),
            ..library_module()
        }
        .to_bytes();
        assert_eq!(&bytes[4..8], &[0, 0, 0, 0]);
        assert_eq!(Module::from_bytes(&bytes).unwrap().main, Some(0));
    }

    #[test]
    fn a_truncated_library_module_is_a_typed_error() {
        let bytes = library_module().to_bytes();
        for cut in 0..bytes.len() {
            // Every prefix is rejected, never panicked on.
            let _ = Module::from_bytes(&bytes[..cut]);
        }
        assert_eq!(
            Module::from_bytes(&bytes[..6]).unwrap_err(),
            ModuleDecodeError::Truncated
        );
    }

    /// A library with the whole export surface: a handle in and out, a string,
    /// a scalar, and a void result.
    fn exporting_module() -> Module {
        let mut module = library_module();
        module.functions.push(FuncProto {
            name: "makeButton".to_owned(),
            param_count: 1,
            local_count: 1,
            execution: Execution::Runtime,
            code: vec![Instruction::LoadLocal(0), Instruction::Return],
        });
        module.exports = ExportTable {
            classes: vec!["Button".to_owned()],
            functions: vec![
                ModuleExport {
                    name: "add".to_owned(),
                    kira_name: "add".to_owned(),
                    function: 0,
                    params: vec![ExportType::Int, ExportType::Float],
                    result: ExportType::Bool,
                },
                ModuleExport {
                    name: "make_button".to_owned(),
                    kira_name: "makeButton".to_owned(),
                    function: 1,
                    params: vec![ExportType::String],
                    result: ExportType::Handle { class: 0 },
                },
            ],
        };
        module
    }

    #[test]
    fn an_exports_section_round_trips_through_bytes() {
        let module = exporting_module();
        let decoded = Module::from_bytes(&module.to_bytes()).unwrap();
        assert_eq!(decoded, module);
        assert_eq!(decoded.exports.classes, ["Button"]);
        assert_eq!(
            decoded.exports.functions[1].result,
            ExportType::Handle { class: 0 }
        );
    }

    /// The compatibility claim, tested rather than asserted: the section is
    /// appended, so a module with no exports is byte-for-byte what it was before
    /// exports existed, and a decoder reading one gets an empty table.
    #[test]
    fn a_module_without_exports_writes_no_section_at_all() {
        let bytes = library_module().to_bytes();
        let mut with_section = library_module();
        with_section.exports = ExportTable::default();
        assert_eq!(with_section.to_bytes(), bytes);
        assert!(Module::from_bytes(&bytes).unwrap().exports.is_empty());
    }

    /// Truncation, byte by byte, across the whole section: every prefix is a
    /// typed error and none is a panic — so a *partial* section never decodes as
    /// "no exports", which would silently hand a consumer a library missing the
    /// function it came for.
    ///
    /// One cut is deliberately exempt: the byte where the function table ends.
    /// A stream cut exactly there **is** an old module, byte for byte, and no
    /// decoder can distinguish the two — that indistinguishability is what makes
    /// the section append-only rather than a format break. Every cut past it is
    /// a section that started and did not finish, and is rejected.
    #[test]
    fn every_truncation_inside_an_exports_section_is_a_typed_error() {
        let with_exports = exporting_module();
        let mut without = exporting_module();
        without.exports = ExportTable::default();
        // Where the section begins: everything before it is the module a build
        // without exports would have written.
        let section_start = without.to_bytes().len();

        let bytes = with_exports.to_bytes();
        let complete = bytes.len();
        for cut in section_start + 1..complete {
            match Module::from_bytes(&bytes[..cut]) {
                Err(_) => {}
                Ok(module) => panic!("prefix of {cut}/{complete} bytes decoded as {module:?}"),
            }
        }
        assert!(
            Module::from_bytes(&bytes[..section_start])
                .unwrap()
                .exports
                .is_empty(),
            "a stream ending where the section starts is exactly an old module"
        );
        assert_eq!(Module::from_bytes(&bytes).unwrap(), with_exports);
    }

    #[test]
    fn an_export_type_tag_that_cannot_cross_is_rejected() {
        let mut bytes = exporting_module().to_bytes();
        // The last five bytes are the final export's result type: tag + class.
        let tag = bytes.len() - 5;
        for byte in [
            BridgeValueTag::STRUCT.0,
            BridgeValueTag::ARRAY.0,
            BridgeValueTag::ENUM.0,
            200,
        ] {
            bytes[tag] = byte;
            assert_eq!(
                Module::from_bytes(&bytes).unwrap_err(),
                ModuleDecodeError::UncrossableExportType {
                    export: "make_button".to_owned(),
                    tag: byte,
                }
            );
        }
    }

    #[test]
    fn a_class_index_on_a_non_handle_type_is_rejected() {
        let mut bytes = exporting_module().to_bytes();
        let tag = bytes.len() - 5;
        // A string result with a class index: reserved bytes carrying data.
        bytes[tag] = BridgeValueTag::STRING.0;
        bytes[tag + 1] = 1;
        assert_eq!(
            Module::from_bytes(&bytes).unwrap_err(),
            ModuleDecodeError::UncrossableExportType {
                export: "make_button".to_owned(),
                tag: BridgeValueTag::STRING.0,
            }
        );
    }

    #[test]
    fn bytes_after_the_last_section_are_rejected() {
        let mut bytes = exporting_module().to_bytes();
        bytes.extend_from_slice(&[0, 0, 0]);
        assert_eq!(
            Module::from_bytes(&bytes).unwrap_err(),
            ModuleDecodeError::TrailingBytes(3)
        );
    }
}
