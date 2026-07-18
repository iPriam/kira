//! The compiled module: functions, their code, and the string constant pool.
//!
//! A [`Module`] is what the compiler produces and the VM runs. It also has a
//! self-describing byte format ([`Module::to_bytes`] / [`Module::from_bytes`])
//! behind the `KBC1` magic, so a module is a real serializable artifact and not
//! just an in-memory structure. The format is append-only.

use crate::op::{DecodeError, Instruction, decode, encode};
use kira_runtime_abi::Execution;

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
        out
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
        Ok(Module {
            functions,
            main,
            strings,
        })
    }
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
}
