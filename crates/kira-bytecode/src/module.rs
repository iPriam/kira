//! The compiled module: functions, their code, and the string constant pool.
//!
//! A [`Module`] is what the compiler produces and the VM runs. It also has a
//! self-describing byte format ([`Module::to_bytes`] / [`Module::from_bytes`])
//! behind the `KBC1` magic, so a module is a real serializable artifact and not
//! just an in-memory structure. The format is append-only.

use crate::exports::{ExportTable, ExportType, ModuleExport};
use crate::module_foreign::{
    read_foreign, read_foreign_aggregates, read_foreign_callbacks, write_foreign,
    write_foreign_aggregates, write_foreign_callbacks,
};
use crate::module_release::{read_releases, write_releases};
use crate::op::{DecodeError, Instruction, decode, encode};
use kira_runtime_abi::{
    BridgeValueTag, Execution, ForeignAggregateError, ForeignAggregates, ForeignCallback,
    ForeignImport,
};

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
    /// The foreign (`@FFI.Extern`) imports a `CallForeign` id indexes.
    ///
    /// Each row carries the C symbol's library, ABI, and exact-width signature.
    /// Empty for a module that declares no extern, which is also what a module
    /// written before the foreign section existed decodes as. When this is
    /// non-empty but [`Module::exports`] is empty, the empty exports section is
    /// still written first, so the appended foreign bytes are never misread as
    /// an exports section.
    pub foreign_imports: Vec<ForeignImport>,
    /// The C-layout aggregates the foreign signatures name by index.
    ///
    /// Empty for a module whose externs pass only scalars, which is also what a
    /// module written before the aggregate section existed decodes as.
    pub foreign_aggregates: ForeignAggregates,
    /// The Kira functions reachable from C as function pointers.
    ///
    /// Empty for a module that passes no Kira function to C, which is also what
    /// a module written before this section existed decodes as. A
    /// `ForeignCallback(id)` instruction indexes it, and the host resolves the
    /// entry thunk the backend generated for the same id.
    pub foreign_callbacks: Vec<ForeignCallback>,
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
    /// Which local slots the VM releases when this function returns.
    ///
    /// Written by the compiler from `kira_ir::mid`'s plan — the same stage the
    /// native backend reads — so one answer covers both engines.
    pub releases: FrameRelease,
}

impl FuncProto {
    /// Whether this function's body lives in the native half.
    pub fn is_native(&self) -> bool {
        self.execution == Execution::Native
    }
}

/// What a returning frame releases.
///
/// Two states rather than a bare slot list, because "no plan" and "an empty
/// plan" mean opposite things: a module written before the release section
/// existed carries no plan and must keep being run the way it always was, while
/// a compiled function that genuinely releases nothing carries an empty
/// [`FrameRelease::Planned`]. Collapsing the two would make every pre-section
/// module leak every string it holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FrameRelease {
    /// Every local slot, in slot order.
    ///
    /// What a module with no release section asks for, and what a module built
    /// by hand gets by default. Releasing the whole frame over-releases
    /// nothing — a slot the function does not own holds either a scalar or an
    /// opaque token, and the VM's release of both is a no-op — so it is the
    /// safe reading of "this module did not say".
    #[default]
    EveryLocal,
    /// Exactly these slots, ascending and distinct.
    Planned(Vec<u16>),
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
    /// A foreign import named an ABI byte this build does not know.
    #[error("foreign import `{import}` names ABI tag `{tag}`, which this build does not know")]
    UnknownForeignAbi {
        /// The C symbol of the offending import.
        import: String,
        /// The unrecognized ABI tag byte.
        tag: u8,
    },
    /// A foreign import's parameter or result named a type byte this build does
    /// not know.
    #[error(
        "foreign import `{import}` names foreign type tag `{tag}`, which this build does not know"
    )]
    UnknownForeignType {
        /// The C symbol of the offending import.
        import: String,
        /// The unrecognized foreign-type tag byte.
        tag: u8,
    },
    /// An aggregate member named a type byte this build does not know.
    #[error("aggregate {index} names member type tag `{tag}`, which this build does not know")]
    UnknownForeignAggregateMember {
        /// The offending aggregate's table index.
        index: u32,
        /// The unrecognized member tag byte.
        tag: u8,
    },
    /// An aggregate's members could not form a layable-out row.
    #[error("aggregate {index} in this module is malformed")]
    MalformedForeignAggregate {
        /// The offending aggregate's table index.
        index: u32,
        /// Why the aggregate table rejected the row.
        #[source]
        source: ForeignAggregateError,
    },
    /// A foreign signature named an aggregate index the table does not contain.
    #[error("foreign import `{import}` names aggregate {index}, which this module does not define")]
    UnknownForeignAggregate {
        /// The C symbol of the offending import.
        import: String,
        /// The unresolved aggregate index.
        index: u32,
    },
    /// The release section named a different number of functions than the
    /// module has.
    ///
    /// Its entries are positional, so a disagreeing count means the writer and
    /// this reader do not agree on what a position names — and releasing by a
    /// misaligned plan frees one function's slots on another's frame.
    #[error("release section names {entries} functions; the module has {functions}")]
    ReleaseCountMismatch {
        /// How many functions the module actually has.
        functions: u32,
        /// How many the section claimed.
        entries: u32,
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
        // The foreign section follows the exports section. When there are
        // foreign imports but no exports, the empty exports framing is written
        // anyway (`force`), so `read_exports` consumes it as an empty section
        // rather than mistaking the foreign bytes for class/export counts.
        //
        // Each later section forces the ones before it to be written, empty or
        // not, for the same reason: a section is only unambiguous when every
        // section it follows is present to be consumed first.
        let has_releases = self
            .functions
            .iter()
            .any(|function| function.releases != FrameRelease::EveryLocal);
        let has_callbacks = !self.foreign_callbacks.is_empty() || has_releases;
        let has_aggregates = !self.foreign_aggregates.is_empty() || has_callbacks;
        let has_foreign = !self.foreign_imports.is_empty() || has_aggregates;
        self.write_exports(&mut out, has_foreign);
        if has_foreign {
            write_foreign(&mut out, &self.foreign_imports);
        }
        // The aggregate table follows the foreign section and is omitted when
        // empty, so a scalar-only program's bytes are unchanged from before
        // aggregates existed. An aggregate can only be reached through a
        // foreign signature, so a non-empty table always sits behind one.
        if has_aggregates {
            write_foreign_aggregates(&mut out, &self.foreign_aggregates);
        }
        if has_callbacks {
            write_foreign_callbacks(&mut out, &self.foreign_callbacks);
        }
        // And the release plans last, on the same terms.
        if has_releases {
            write_releases(&mut out, &self.functions);
        }
        out
    }

    /// Writes the appended exports section.
    ///
    /// Omitted entirely when there is nothing to export *and* no later section
    /// forces it, so an application's bytes are byte-for-byte what they were
    /// before exports existed. `force` writes the empty framing so an appended
    /// section after it decodes unambiguously.
    fn write_exports(&self, out: &mut Vec<u8>, force: bool) {
        if self.exports.is_empty() && !force {
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
                releases: FrameRelease::EveryLocal,
            });
        }
        let exports = read_exports(&mut reader)?;
        let foreign_imports = read_foreign(&mut reader)?;
        let foreign_aggregates = read_foreign_aggregates(&mut reader, &foreign_imports)?;
        let foreign_callbacks = read_foreign_callbacks(&mut reader)?;
        read_releases(&mut reader, &mut functions)?;
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
            foreign_imports,
            foreign_aggregates,
            foreign_callbacks,
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

pub(crate) fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

/// A cursor reading a serialized module, shared with the foreign section codec.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], ModuleDecodeError> {
        let end = self.offset + n;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(ModuleDecodeError::Truncated)?;
        self.offset = end;
        Ok(slice)
    }

    pub(crate) fn is_at_end(&self) -> bool {
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

    pub(crate) fn read_u16(&mut self) -> Result<u16, ModuleDecodeError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub(crate) fn read_u32(&mut self) -> Result<u32, ModuleDecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_len_prefixed(&mut self) -> Result<&'a [u8], ModuleDecodeError> {
        let len = self.read_u32()? as usize;
        self.take(len)
    }

    pub(crate) fn read_string(&mut self) -> Result<String, ModuleDecodeError> {
        let bytes = self.read_len_prefixed()?;
        core::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| ModuleDecodeError::InvalidString)
    }
}

#[cfg(test)]
#[path = "module_tests.rs"]
mod tests;
