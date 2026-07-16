//! The hybrid module manifest: the `.khm` artifact that ties a program's
//! bytecode half to its native half.
//!
//! A hybrid build emits three things: a bytecode module, a native shared
//! library, and this manifest describing how they fit together. The manifest is
//! what the hybrid runtime loads first — it names the two payloads, says which
//! function is the entrypoint and which engine owns it, and for every function
//! records the engine, the signature, and (for native functions) the symbol to
//! resolve out of the library.
//!
//! # Why signatures live here
//!
//! The runtime marshals every value that crosses the boundary, and it must know
//! what a function expects *before* it calls it — the native side is machine
//! code with no reflection. Types are recorded as [`BridgeValueTag`]s, the same
//! tags the values themselves carry, so the manifest and the ABI can never
//! describe two different lattices.
//!
//! # Format
//!
//! `KHM1`, little-endian, length-prefixed, append-only: never renumber a tag,
//! reorder a field, or insert one mid-record. A manifest is a deserializable
//! public artifact, so decoding validates rather than trusts.

use kira_runtime_abi::{BridgeValueTag, Execution, Ownership};

/// The magic bytes that open a serialized manifest: "KHM1".
pub const MAGIC: [u8; 4] = *b"KHM1";

/// One parameter's type and how it takes its argument.
///
/// Within one engine the borrow checker settles ownership at compile time and
/// nothing has to be written down. A call that crosses engines cannot: the two
/// halves are separately compiled, so the mode travels here and the runtime
/// reads it to decide whether handing a value over is a transfer or a loan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HybridParam {
    /// The parameter's type, as the tag its values carry.
    pub ty: BridgeValueTag,
    /// How the parameter takes its argument, and so who frees it.
    pub ownership: Ownership,
}

impl HybridParam {
    /// A by-value parameter: the argument is moved in and the callee frees it.
    pub fn owned(ty: BridgeValueTag) -> HybridParam {
        HybridParam {
            ty,
            ownership: Ownership::Owned,
        }
    }
}

/// A hybrid module: its two payloads, its entrypoint, and its functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HybridManifest {
    /// The module's name, for diagnostics.
    pub module_name: String,
    /// Path to the bytecode payload the VM half runs.
    pub bytecode_path: String,
    /// Path to the shared library the native half lives in.
    pub native_library_path: String,
    /// Index of the entrypoint within [`HybridManifest::functions`].
    pub entry: u32,
    /// Every function in the program, in the program's own function order.
    ///
    /// The order matches the bytecode module's function table, so an id is one
    /// index into both halves.
    pub functions: Vec<HybridFunction>,
}

/// One function's engine, signature, and native symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HybridFunction {
    /// The function's index in the program.
    pub id: u32,
    /// The function's name, for diagnostics.
    pub name: String,
    /// Which engine owns this function's body.
    ///
    /// Always resolved: a manifest records the engine a function *runs* on, so
    /// [`Execution::Inherited`] never survives into one.
    pub execution: Execution,
    /// The parameters, in order: each type plus how it takes its argument.
    pub params: Vec<HybridParam>,
    /// The return type ([`BridgeValueTag::VOID`] when it returns nothing).
    ///
    /// A returned value is always owned — it is the callee handing a value out,
    /// which is a move by definition, so there is no mode to record. Returned
    /// borrows would need lifetime validation the language does not have yet.
    pub returns: BridgeValueTag,
    /// The symbol to resolve from the shared library, for a native function.
    ///
    /// `None` for a runtime function, which has no native symbol to bind.
    pub exported_name: Option<String>,
}

/// Why a manifest could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestDecodeError {
    /// The stream did not begin with the `KHM1` magic.
    #[error("not a Kira hybrid manifest (bad magic)")]
    BadMagic,
    /// The stream ended before a full manifest was read.
    #[error("truncated hybrid manifest")]
    Truncated,
    /// A string in the manifest was not valid UTF-8.
    #[error("invalid UTF-8 in hybrid manifest")]
    InvalidString,
    /// An execution byte named no engine this build knows.
    #[error("unknown execution engine `{0}` in hybrid manifest")]
    UnknownExecution(u8),
    /// An ownership byte named no parameter mode this build knows.
    #[error("unknown parameter ownership `{0}` in hybrid manifest")]
    UnknownOwnership(u8),
    /// The entrypoint index does not name a function in the manifest.
    #[error("hybrid manifest entrypoint {entry} names no function (of {count})")]
    EntryOutOfRange {
        /// The recorded entrypoint index.
        entry: u32,
        /// How many functions the manifest carries.
        count: u32,
    },
    /// A native function carried no symbol to bind.
    #[error("native function `{0}` in hybrid manifest carries no exported symbol")]
    NativeWithoutSymbol(String),
}

impl HybridManifest {
    /// The entrypoint function.
    pub fn entry_function(&self) -> &HybridFunction {
        // Decoding rejects an out-of-range entry, and every constructor here
        // goes through it, so this index is always live.
        &self.functions[self.entry as usize]
    }

    /// Serializes the manifest to its byte format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        write_string(&mut out, &self.module_name);
        write_string(&mut out, &self.bytecode_path);
        write_string(&mut out, &self.native_library_path);
        out.extend_from_slice(&self.entry.to_le_bytes());
        write_u32(&mut out, self.functions.len() as u32);
        for function in &self.functions {
            out.extend_from_slice(&function.id.to_le_bytes());
            out.push(function.execution.as_byte());
            write_string(&mut out, &function.name);
            write_u32(&mut out, function.params.len() as u32);
            for param in &function.params {
                out.push(param.ty.0);
                out.push(param.ownership.as_byte());
            }
            out.push(function.returns.0);
            // An absent symbol and an empty symbol are the same thing: a
            // function with nothing to bind.
            write_string(&mut out, function.exported_name.as_deref().unwrap_or(""));
        }
        out
    }

    /// Decodes a manifest, validating it rather than trusting it.
    pub fn from_bytes(bytes: &[u8]) -> Result<HybridManifest, ManifestDecodeError> {
        let mut reader = Reader { bytes, pos: 0 };
        if reader.take(4)? != MAGIC {
            return Err(ManifestDecodeError::BadMagic);
        }
        let module_name = reader.string()?;
        let bytecode_path = reader.string()?;
        let native_library_path = reader.string()?;
        let entry = reader.u32()?;
        let count = reader.u32()?;

        let mut functions = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let id = reader.u32()?;
            let byte = reader.byte()?;
            let execution =
                Execution::from_byte(byte).ok_or(ManifestDecodeError::UnknownExecution(byte))?;
            let name = reader.string()?;
            let param_count = reader.u32()?;
            let mut params = Vec::with_capacity(param_count as usize);
            for _ in 0..param_count {
                let ty = BridgeValueTag(reader.byte()?);
                let byte = reader.byte()?;
                let ownership = Ownership::from_byte(byte)
                    .ok_or(ManifestDecodeError::UnknownOwnership(byte))?;
                params.push(HybridParam { ty, ownership });
            }
            let returns = BridgeValueTag(reader.byte()?);
            let exported = reader.string()?;
            // A native function the runtime cannot bind is a broken artifact,
            // not a runtime surprise: reject it at load.
            if execution == Execution::Native && exported.is_empty() {
                return Err(ManifestDecodeError::NativeWithoutSymbol(name));
            }
            functions.push(HybridFunction {
                id,
                name,
                execution,
                params,
                returns,
                exported_name: (!exported.is_empty()).then_some(exported),
            });
        }

        if entry >= count {
            return Err(ManifestDecodeError::EntryOutOfRange { entry, count });
        }
        Ok(HybridManifest {
            module_name,
            bytecode_path,
            native_library_path,
            entry,
            functions,
        })
    }
}

/// Appends a `u32` length-prefixed byte string.
fn write_string(out: &mut Vec<u8>, text: &str) {
    write_u32(out, text.len() as u32);
    out.extend_from_slice(text.as_bytes());
}

/// Appends a little-endian `u32`.
fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// A bounds-checked cursor over a serialized manifest.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], ManifestDecodeError> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or(ManifestDecodeError::Truncated)?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or(ManifestDecodeError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8, ManifestDecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ManifestDecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn string(&mut self) -> Result<String, ManifestDecodeError> {
        let length = self.u32()? as usize;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| ManifestDecodeError::InvalidString)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> HybridManifest {
        HybridManifest {
            module_name: "demo".to_owned(),
            bytecode_path: ".kira-build/demo.kbc".to_owned(),
            native_library_path: ".kira-build/libdemo.dylib".to_owned(),
            entry: 0,
            functions: vec![
                HybridFunction {
                    id: 0,
                    name: "main".to_owned(),
                    execution: Execution::Runtime,
                    params: Vec::new(),
                    returns: BridgeValueTag::VOID,
                    exported_name: None,
                },
                HybridFunction {
                    id: 1,
                    name: "hot".to_owned(),
                    execution: Execution::Native,
                    params: vec![
                        HybridParam::owned(BridgeValueTag::INT),
                        HybridParam {
                            ty: BridgeValueTag::STRING,
                            ownership: Ownership::Borrow,
                        },
                    ],
                    returns: BridgeValueTag::INT,
                    exported_name: Some("kira_native_fn_1".to_owned()),
                },
            ],
        }
    }

    #[test]
    fn a_manifest_round_trips() {
        let original = manifest();
        let decoded = HybridManifest::from_bytes(&original.to_bytes()).expect("decodes");
        assert_eq!(decoded, original);
        assert_eq!(decoded.entry_function().name, "main");
    }

    #[test]
    fn a_foreign_stream_is_rejected_on_its_magic() {
        assert_eq!(
            HybridManifest::from_bytes(b"KBC1and then some"),
            Err(ManifestDecodeError::BadMagic)
        );
    }

    /// A manifest is a public artifact: every truncation must be a typed
    /// rejection, never a panic.
    #[test]
    fn every_truncation_is_rejected_typed() {
        let bytes = manifest().to_bytes();
        for length in 0..bytes.len() {
            match HybridManifest::from_bytes(&bytes[..length]) {
                Err(_) => {}
                Ok(_) => panic!("a manifest truncated to {length} bytes must not decode"),
            }
        }
        assert!(HybridManifest::from_bytes(&bytes).is_ok());
    }

    #[test]
    fn an_unknown_engine_byte_is_rejected() {
        let mut bytes = manifest().to_bytes();
        // The first function's execution byte follows magic, three strings,
        // the entry index, the count, and the id.
        let offset = bytes
            .windows(4)
            .position(|window| window == b"main")
            .expect("the first function's name is in the stream")
            - 5;
        bytes[offset] = 9;
        assert_eq!(
            HybridManifest::from_bytes(&bytes),
            Err(ManifestDecodeError::UnknownExecution(9))
        );
    }

    #[test]
    fn a_native_function_without_a_symbol_is_rejected() {
        let mut broken = manifest();
        broken.functions[1].exported_name = None;
        assert_eq!(
            HybridManifest::from_bytes(&broken.to_bytes()),
            Err(ManifestDecodeError::NativeWithoutSymbol("hot".to_owned()))
        );
    }

    #[test]
    fn an_entrypoint_naming_no_function_is_rejected() {
        let mut broken = manifest();
        broken.entry = 7;
        assert_eq!(
            HybridManifest::from_bytes(&broken.to_bytes()),
            Err(ManifestDecodeError::EntryOutOfRange { entry: 7, count: 2 })
        );
    }
}
