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

use kira_runtime_abi::{
    BridgeValueTag, Execution, ForeignAbi, ForeignAggregate, ForeignAggregateError,
    ForeignAggregateId, ForeignAggregates, ForeignArrayElement, ForeignImport, ForeignMember,
    ForeignSignature, ForeignType, ForeignTypeSpec, Ownership,
};

/// The magic bytes that open a serialized manifest: "KHM1".
pub const MAGIC: [u8; 4] = *b"KHM1";

/// The aggregate-member byte that introduces a nested aggregate; anything else
/// is a scalar's own foreign-type tag, none of which reaches `0xfe`.
const NESTED_MEMBER_TAG: u8 = 0xff;

/// The aggregate-member byte that introduces an inline fixed-size array: the
/// element's own member byte follows, then a `u32` count.
const ARRAY_MEMBER_TAG: u8 = 0xfe;

/// The entrypoint slot's value when a hybrid module has no entrypoint (a
/// library).
///
/// The same sentinel discipline as `kira_bytecode`'s `NO_ENTRYPOINT`,
/// for the same reason: the byte layout does not move, and decoding has always
/// rejected an entry index at or past the function count, so no manifest that
/// ever decoded cleanly carries this value.
pub const NO_ENTRYPOINT: u32 = u32::MAX;

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
    /// Index of the entrypoint within [`HybridManifest::functions`], or `None`
    /// for a library.
    ///
    /// Serialized as [`NO_ENTRYPOINT`] when absent.
    pub entry: Option<u32>,
    /// Every function in the program, in the program's own function order.
    ///
    /// The order matches the bytecode module's function table, so an id is one
    /// index into both halves.
    pub functions: Vec<HybridFunction>,
    /// Every `@FFI.Extern` import the program declares, in import-id order.
    ///
    /// A `CallForeign(id)` in the bytecode half indexes this table; each row
    /// carries the adapter symbol the session resolves out of the same native
    /// half the trampolines live in. Empty for a program with no foreign
    /// imports, and absent from the byte stream then — an old manifest that
    /// predates this section decodes with an empty table.
    pub foreign: Vec<HybridForeign>,
    /// The C-layout aggregates the foreign signatures name by index.
    ///
    /// Empty for a program whose externs pass only scalars, which is also what
    /// a manifest written before this section existed decodes as.
    pub foreign_aggregates: ForeignAggregates,
    /// How many functions the bytecode half carries beyond
    /// [`HybridManifest::functions`].
    ///
    /// The VM half synthesizes helpers of its own — the widen rebuilds — which
    /// are appended after the program's functions and belong to no crossing:
    /// nothing native calls one, because a crossing names a function by its
    /// manifest id and these have none. They still have to be *counted*, or the
    /// bundle check could not tell a legitimate helper from a stale bytecode
    /// half carrying a function the manifest never described.
    ///
    /// Zero for a program that widens nothing, and for a manifest written
    /// before this field existed — which is what keeps those bytes unchanged.
    pub internal_functions: u32,
}

/// One `@FFI.Extern` import: its C library and symbol, its exact-width
/// signature, and the adapter symbol that reaches it inside the native half.
///
/// The adapter is generated by the LLVM backend and lives in the same shared
/// library as the `@Native` trampolines, so the hybrid session binds it out of
/// the one dylib it already loaded — never a second `dlopen`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HybridForeign {
    /// The declared native-library name, for diagnostics.
    pub library: String,
    /// The C symbol the adapter ultimately calls, for diagnostics.
    pub symbol: String,
    /// The declaration's ABI.
    pub abi: ForeignAbi,
    /// The import's exact-width parameter and result types.
    pub signature: ForeignSignature,
    /// The exported adapter symbol the session resolves and calls.
    pub adapter_symbol: String,
}

impl HybridForeign {
    /// Describes one import for the manifest, pairing it with its adapter symbol.
    pub fn from_import(import: &ForeignImport, adapter_symbol: impl Into<String>) -> HybridForeign {
        HybridForeign {
            library: import.library().to_owned(),
            symbol: import.symbol().to_owned(),
            abi: import.abi(),
            signature: import.signature().clone(),
            adapter_symbol: adapter_symbol.into(),
        }
    }
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
    /// A counted run claimed more elements than the remaining bytes can hold.
    #[error("hybrid manifest declares {count} elements with only {remaining} bytes left")]
    CountExceedsInput {
        /// The count the stream asked for.
        count: usize,
        /// How many bytes were actually left to read.
        remaining: usize,
    },
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
    /// A foreign import's ABI byte named no ABI this build knows.
    #[error("unknown foreign ABI `{tag}` for import `{import}` in hybrid manifest")]
    UnknownForeignAbi {
        /// The import whose ABI byte was unknown.
        import: String,
        /// The unrecognized ABI byte.
        tag: u8,
    },
    /// A foreign import's type byte named no foreign type this build knows.
    #[error("unknown foreign type `{tag}` for import `{import}` in hybrid manifest")]
    UnknownForeignType {
        /// The import whose type byte was unknown.
        import: String,
        /// The unrecognized foreign-type byte.
        tag: u8,
    },
    /// A foreign import carried no adapter symbol to bind.
    #[error("foreign import `{0}` in hybrid manifest carries no adapter symbol")]
    ForeignWithoutAdapter(String),
    /// An aggregate member named a type byte this build does not know.
    #[error("unknown member type `{tag}` in aggregate {index} of hybrid manifest")]
    UnknownForeignAggregateMember {
        /// The offending aggregate's table index.
        index: u32,
        /// The unrecognized member byte.
        tag: u8,
    },
    /// An aggregate's members could not form a layable-out row.
    #[error("aggregate {index} in hybrid manifest is malformed")]
    MalformedForeignAggregate {
        /// The offending aggregate's table index.
        index: u32,
        /// Why the aggregate table rejected the row.
        #[source]
        source: ForeignAggregateError,
    },
    /// A foreign signature named an aggregate index the table does not contain.
    #[error("foreign import `{import}` names aggregate {index}, absent from this manifest")]
    UnknownForeignAggregate {
        /// The import whose signature named it.
        import: String,
        /// The unresolved aggregate index.
        index: u32,
    },
}

impl HybridManifest {
    /// The entrypoint function, or `None` for a library.
    ///
    /// Decoding rejects an out-of-range entry, so a `Some` entry always names a
    /// live row; the option here is a library's genuine absence of one, not a
    /// bounds concern.
    pub fn entry_function(&self) -> Option<&HybridFunction> {
        self.functions.get(self.entry? as usize)
    }

    /// Serializes the manifest to its byte format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        write_string(&mut out, &self.module_name);
        write_string(&mut out, &self.bytecode_path);
        write_string(&mut out, &self.native_library_path);
        out.extend_from_slice(&self.entry.unwrap_or(NO_ENTRYPOINT).to_le_bytes());
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
        // The trailing sections are positional, not tagged, so a later one can
        // only be written if every earlier one is — otherwise the decoder reads
        // this section's count as the foreign count. A program that widens
        // nothing writes no tail at all and keeps its bytes unchanged.
        let tail = self.internal_functions != 0;
        // The foreign-import section, written when there is something in it (or
        // when the tail below forces it): a program with no `@FFI.Extern`
        // imports writes nothing here, so its bytes are identical to a manifest
        // that predates this section. That is what lets an old manifest decode
        // as an empty table.
        if !self.foreign.is_empty() || tail {
            write_u32(&mut out, self.foreign.len() as u32);
            for import in &self.foreign {
                write_string(&mut out, &import.library);
                write_string(&mut out, &import.symbol);
                out.push(import.abi.tag());
                write_u32(&mut out, import.signature.parameters().len() as u32);
                for parameter in import.signature.parameters() {
                    write_spec(&mut out, *parameter);
                }
                write_spec(&mut out, import.signature.result());
                write_string(&mut out, &import.adapter_symbol);
            }
        }
        // The aggregate table follows the imports that index it, and is omitted
        // when empty for the same reason the imports are: a scalar-only program
        // writes bytes identical to a manifest predating aggregates.
        if !self.foreign_aggregates.is_empty() || tail {
            write_u32(&mut out, self.foreign_aggregates.len() as u32);
            for aggregate in self.foreign_aggregates.iter() {
                write_u32(&mut out, aggregate.members().len() as u32);
                for member in aggregate.members() {
                    match member {
                        ForeignMember::Scalar(ty) => out.push(ty.tag()),
                        ForeignMember::Aggregate(id) => {
                            out.push(NESTED_MEMBER_TAG);
                            write_u32(&mut out, id.0);
                        }
                        ForeignMember::Array { element, count } => {
                            out.push(ARRAY_MEMBER_TAG);
                            match element {
                                ForeignArrayElement::Scalar(ty) => out.push(ty.tag()),
                                ForeignArrayElement::Aggregate(id) => {
                                    out.push(NESTED_MEMBER_TAG);
                                    write_u32(&mut out, id.0);
                                }
                            }
                            write_u32(&mut out, *count);
                        }
                    }
                }
            }
        }
        if tail {
            write_u32(&mut out, self.internal_functions);
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
        let count = reader.count()?;

        let mut functions = Vec::with_capacity(count);
        for _ in 0..count {
            let id = reader.u32()?;
            let byte = reader.byte()?;
            let execution =
                Execution::from_byte(byte).ok_or(ManifestDecodeError::UnknownExecution(byte))?;
            let name = reader.string()?;
            let param_count = reader.count()?;
            let mut params = Vec::with_capacity(param_count);
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

        let foreign = read_foreign(&mut reader)?;
        let foreign_aggregates = read_foreign_aggregates(&mut reader, &foreign)?;
        // Absent means zero: a manifest written before this field existed ends
        // here, and so does one for a program that widens nothing.
        let internal_functions = reader.u32().unwrap_or_default();

        // A library carries no entrypoint, so there is no index to bound. The
        // count is reported as it was written — it was read as a `u32` and the
        // reader only ever narrows it, so this conversion cannot lose one.
        let reported = u32::try_from(count).unwrap_or(u32::MAX);
        let entry = match entry {
            NO_ENTRYPOINT => None,
            index if index as usize >= count => {
                return Err(ManifestDecodeError::EntryOutOfRange {
                    entry: index,
                    count: reported,
                });
            }
            index => Some(index),
        };
        Ok(HybridManifest {
            module_name,
            bytecode_path,
            native_library_path,
            entry,
            functions,
            foreign,
            foreign_aggregates,
            internal_functions,
        })
    }
}

/// Reads the foreign-import section, or an empty table when there is none.
///
/// A manifest written before this section existed ends after its functions, so
/// that absence decodes as zero imports. A partial section is a truncation
/// error, and an unknown ABI or foreign-type byte is a typed error — never a
/// guessed value.
fn read_foreign(reader: &mut Reader<'_>) -> Result<Vec<HybridForeign>, ManifestDecodeError> {
    if reader.is_at_end() {
        return Ok(Vec::new());
    }
    let count = reader.count()?;
    let mut foreign = Vec::with_capacity(count);
    for _ in 0..count {
        let library = reader.string()?;
        let symbol = reader.string()?;
        let abi_byte = reader.byte()?;
        let abi = ForeignAbi::from_tag(abi_byte).ok_or_else(|| {
            ManifestDecodeError::UnknownForeignAbi {
                import: symbol.clone(),
                tag: abi_byte,
            }
        })?;
        let param_count = reader.count()?;
        let mut parameters = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            parameters.push(read_spec(reader, &symbol)?);
        }
        let result = read_spec(reader, &symbol)?;
        let adapter_symbol = reader.string()?;
        if adapter_symbol.is_empty() {
            return Err(ManifestDecodeError::ForeignWithoutAdapter(symbol));
        }
        foreign.push(HybridForeign {
            library,
            symbol,
            abi,
            signature: ForeignSignature::new(parameters, result),
            adapter_symbol,
        });
    }
    Ok(foreign)
}

/// Reads the aggregate table, or an empty table when the stream ends first, and
/// checks that every index the imports named resolves inside it.
fn read_foreign_aggregates(
    reader: &mut Reader<'_>,
    imports: &[HybridForeign],
) -> Result<ForeignAggregates, ManifestDecodeError> {
    let mut aggregates = ForeignAggregates::new();
    if !reader.is_at_end() {
        let count = reader.u32()?;
        for index in 0..count {
            let member_count = reader.count()?;
            let mut members = Vec::with_capacity(member_count);
            for _ in 0..member_count {
                members.push(read_member(reader, index)?);
            }
            aggregates
                .push(ForeignAggregate::new(members))
                .map_err(|source| ManifestDecodeError::MalformedForeignAggregate {
                    index,
                    source,
                })?;
        }
    }
    for import in imports {
        for spec in import
            .signature
            .parameters()
            .iter()
            .copied()
            .chain(std::iter::once(import.signature.result()))
        {
            if let Some(id) = spec.aggregate()
                && aggregates.get(id).is_none()
            {
                return Err(ManifestDecodeError::UnknownForeignAggregate {
                    import: import.symbol.clone(),
                    index: id.0,
                });
            }
        }
    }
    Ok(aggregates)
}

/// Writes one signature position: its tag byte, plus a table index when the tag
/// names an aggregate.
fn write_spec(out: &mut Vec<u8>, spec: ForeignTypeSpec) {
    out.push(spec.tag());
    if let Some(id) = spec.aggregate() {
        write_u32(out, id.0);
    }
}

/// Reads one signature position, naming `import` on an unknown tag.
fn read_spec(
    reader: &mut Reader<'_>,
    import: &str,
) -> Result<ForeignTypeSpec, ManifestDecodeError> {
    let tag = reader.byte()?;
    if tag == ForeignTypeSpec::AGGREGATE_TAG {
        return Ok(ForeignTypeSpec::Aggregate(ForeignAggregateId(
            reader.u32()?,
        )));
    }
    ForeignType::from_tag(tag)
        .map(ForeignTypeSpec::Scalar)
        .ok_or_else(|| ManifestDecodeError::UnknownForeignType {
            import: import.to_owned(),
            tag,
        })
}

/// Reads one aggregate member, naming the containing aggregate on an unknown
/// scalar tag.
fn read_member(reader: &mut Reader<'_>, index: u32) -> Result<ForeignMember, ManifestDecodeError> {
    let tag = reader.byte()?;
    if tag == NESTED_MEMBER_TAG {
        return Ok(ForeignMember::Aggregate(ForeignAggregateId(reader.u32()?)));
    }
    if tag == ARRAY_MEMBER_TAG {
        let element = read_array_element(reader, index)?;
        return Ok(ForeignMember::Array {
            element,
            count: reader.u32()?,
        });
    }
    ForeignType::from_tag(tag)
        .map(ForeignMember::Scalar)
        .ok_or(ManifestDecodeError::UnknownForeignAggregateMember { index, tag })
}

/// Reads an inline array's element: a scalar tag, or a nested aggregate index.
///
/// An element is never itself an array — a C array of arrays is written as an
/// array of the aggregate wrapping the inner one — so an [`ARRAY_MEMBER_TAG`]
/// here is as unknown as any tag the writer never emits.
fn read_array_element(
    reader: &mut Reader<'_>,
    index: u32,
) -> Result<ForeignArrayElement, ManifestDecodeError> {
    let tag = reader.byte()?;
    if tag == NESTED_MEMBER_TAG {
        return Ok(ForeignArrayElement::Aggregate(ForeignAggregateId(
            reader.u32()?,
        )));
    }
    ForeignType::from_tag(tag)
        .map(ForeignArrayElement::Scalar)
        .ok_or(ManifestDecodeError::UnknownForeignAggregateMember { index, tag })
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

    fn is_at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn u32(&mut self) -> Result<u32, ManifestDecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Reads a count that is about to size an allocation, and rejects one the
    /// input could not possibly satisfy.
    ///
    /// Every element of every counted run in this format costs at least one
    /// byte, so a count larger than the bytes remaining is malformed however
    /// the rest of the stream reads. Checking it here is what keeps a
    /// `Vec::with_capacity` off a number the artifact chose: one corrupted byte
    /// in the high end of a count is two billion elements, and reserving for
    /// them aborts the process on a host that will not overcommit — a decoder
    /// killing its caller instead of returning the typed error every other
    /// malformed byte gets.
    fn count(&mut self) -> Result<usize, ManifestDecodeError> {
        let count = self.u32()? as usize;
        let remaining = self.bytes.len().saturating_sub(self.pos);
        if count > remaining {
            return Err(ManifestDecodeError::CountExceedsInput { count, remaining });
        }
        Ok(count)
    }

    fn string(&mut self) -> Result<String, ManifestDecodeError> {
        let length = self.u32()? as usize;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| ManifestDecodeError::InvalidString)
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
