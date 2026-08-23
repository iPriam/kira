//! The foreign-import section: the `@FFI.Extern` rows a `CallForeign` id
//! indexes, and the C-layout aggregate table those rows index in turn.
//!
//! Appended after the exports section, with the same append-only discipline:
//! an old module ends before it and decodes as zero imports, a partial section
//! is a typed truncation error, and an unknown ABI or foreign-type tag is a
//! typed error rather than a guess. See [`crate::module`] for the empty-exports
//! prelude that keeps this section unambiguous when a module has foreign
//! imports but no exports.
//!
//! The aggregate table is appended *after* the import rows, which reference it
//! by index, so a decoder reads the references before it can resolve them and
//! checks every index once the table is in hand. The other order would have
//! been readable in one pass but not appendable: a module written before
//! aggregates existed ends where the table now begins, and moving the table
//! ahead of the rows would change bytes that already exist.

use kira_runtime_abi::{
    ForeignAbi, ForeignAggregate, ForeignAggregateId, ForeignAggregates, ForeignArrayElement,
    ForeignCallback, ForeignImport, ForeignMember, ForeignSignature, ForeignType, ForeignTypeSpec,
};

use crate::module::{Format, ModuleDecodeError, Reader, write_bytes, write_count, write_u32};

/// The member byte that introduces a nested aggregate; anything else is a
/// scalar's own foreign-type tag.
const NESTED_MEMBER_TAG: u8 = 0xff;

/// The member byte that introduces an inline fixed-size array: the element's own
/// member byte (with its index when nested) follows, then an external count.
const ARRAY_MEMBER_TAG: u8 = 0xfe;

/// Writes the foreign-import section: a count, then one row per import.
///
/// Each row is the library name, the C symbol, the ABI tag byte, the parameter
/// count, one type-spec per parameter, and the result spec. A spec is one tag
/// byte, followed by an external table index when the tag names an aggregate.
pub(crate) fn write_foreign(out: &mut Vec<u8>, imports: &[ForeignImport]) {
    write_count(out, imports.len());
    for import in imports {
        write_bytes(out, import.library().as_bytes());
        write_bytes(out, import.symbol().as_bytes());
        out.push(import.abi().tag());
        let signature = import.signature();
        write_count(out, signature.parameters().len());
        for parameter in signature.parameters() {
            write_spec(out, *parameter);
        }
        write_spec(out, signature.result());
    }
}

/// Writes the retained-parameters section: one row per import, each a count of
/// retained positions followed by the positions themselves.
///
/// Appended after every other section and omitted when no import retains
/// anything, so a module without a `retains:` declaration is byte-for-byte
/// what it was before the section existed.
pub(crate) fn write_foreign_retained(out: &mut Vec<u8>, imports: &[ForeignImport]) {
    write_count(out, imports.len());
    for import in imports {
        let positions: Vec<usize> = import.signature().retained_positions().collect();
        write_count(out, positions.len());
        for position in positions {
            write_u32(out, position as u32);
        }
    }
}

/// Reads the retained-parameters section back onto `imports`, or leaves every
/// parameter borrowed when the stream ended first.
///
/// A row count that disagrees with the import count, or a position outside its
/// signature, is a typed error: both mean the module was not produced by this
/// compiler's writer.
pub(crate) fn read_foreign_retained(
    reader: &mut Reader<'_>,
    imports: &mut [ForeignImport],
    format: Format,
) -> Result<(), ModuleDecodeError> {
    if reader.is_at_end() {
        return Ok(());
    }
    let rows = reader.read_index_count(format, "retained foreign parameter")?;
    if rows != imports.len() as u64 {
        return Err(ModuleDecodeError::RetainedRowMismatch {
            rows,
            imports: imports.len(),
        });
    }
    for import in imports.iter_mut() {
        let count = reader.read_count(format)?;
        let params = import.signature().parameters().len();
        let mut retained = vec![false; params];
        for _ in 0..count {
            let position = reader.read_u32()? as usize;
            let slot = retained.get_mut(position).ok_or_else(|| {
                ModuleDecodeError::RetainedOutOfRange {
                    import: import.symbol().to_owned(),
                    position,
                    params,
                }
            })?;
            if *slot {
                return Err(ModuleDecodeError::DuplicateRetainedPosition {
                    import: import.symbol().to_owned(),
                    position,
                });
            }
            *slot = true;
        }
        import.retain_parameters(retained);
    }
    Ok(())
}

/// Writes the aggregate table: a count, then each aggregate's members in
/// declaration order.
///
/// A member is one byte — a scalar's foreign-type tag, [`NESTED_MEMBER_TAG`]
/// followed by an external index, or [`ARRAY_MEMBER_TAG`] followed by the
/// element's own member bytes and an external count. The tags are unambiguous because no
/// scalar tag reaches `0xfe`.
pub(crate) fn write_foreign_aggregates(out: &mut Vec<u8>, aggregates: &ForeignAggregates) {
    write_count(out, aggregates.len());
    for aggregate in aggregates.iter() {
        write_count(out, aggregate.members().len());
        for member in aggregate.members() {
            match member {
                ForeignMember::Scalar(ty) => out.push(ty.tag()),
                ForeignMember::Aggregate(id) => {
                    out.push(NESTED_MEMBER_TAG);
                    write_u32(out, id.0);
                }
                ForeignMember::Array { element, count } => {
                    out.push(ARRAY_MEMBER_TAG);
                    match element {
                        ForeignArrayElement::Scalar(ty) => out.push(ty.tag()),
                        ForeignArrayElement::Aggregate(id) => {
                            out.push(NESTED_MEMBER_TAG);
                            write_u32(out, id.0);
                        }
                    }
                    write_u32(out, *count);
                }
            }
        }
    }
}

/// Reads the foreign-import section, or an empty vector when there is none.
///
/// A module written before this section existed ends after its exports, and
/// that absence decodes as zero imports. A partial section is a truncation
/// error, and an unknown ABI or foreign-type tag is a typed error — never a
/// guessed value.
pub(crate) fn read_foreign(
    reader: &mut Reader<'_>,
    format: Format,
) -> Result<Vec<ForeignImport>, ModuleDecodeError> {
    if reader.is_at_end() {
        return Ok(Vec::new());
    }
    let count = reader.read_index_count(format, "foreign import")?;
    let mut imports = Vec::new();
    for _ in 0..count {
        let library = reader.read_string(format)?;
        let symbol = reader.read_string(format)?;
        let abi_byte = reader.take(1)?[0];
        let abi =
            ForeignAbi::from_tag(abi_byte).ok_or_else(|| ModuleDecodeError::UnknownForeignAbi {
                import: symbol.clone(),
                tag: abi_byte,
            })?;
        let param_count = reader.read_count(format)?;
        let mut parameters = Vec::new();
        for _ in 0..param_count {
            parameters.push(read_spec(reader, &symbol)?);
        }
        let result = read_spec(reader, &symbol)?;
        imports.push(ForeignImport::new(
            library,
            symbol,
            abi,
            ForeignSignature::new(parameters, result),
        ));
    }
    Ok(imports)
}

/// Reads the aggregate table, or an empty table when the stream ends first, and
/// checks that every index the imports named resolves inside it.
pub(crate) fn read_foreign_aggregates(
    reader: &mut Reader<'_>,
    imports: &[ForeignImport],
    format: Format,
) -> Result<ForeignAggregates, ModuleDecodeError> {
    let mut aggregates = ForeignAggregates::new();
    if !reader.is_at_end() {
        let count = reader.read_index_count(format, "foreign aggregate")?;
        for index in 0..count {
            let member_count = reader.read_count(format)?;
            let mut members = Vec::new();
            for _ in 0..member_count {
                members.push(read_member(reader, index)?);
            }
            aggregates
                .push(ForeignAggregate::new(members))
                .map_err(|source| ModuleDecodeError::MalformedForeignAggregate { index, source })?;
        }
    }
    for import in imports {
        let signature = import.signature();
        for spec in signature
            .parameters()
            .iter()
            .copied()
            .chain(std::iter::once(signature.result()))
        {
            if let Some(id) = spec.aggregate()
                && aggregates.get(id).is_none()
            {
                return Err(ModuleDecodeError::UnknownForeignAggregate {
                    import: import.symbol().to_owned(),
                    index: id.0,
                });
            }
        }
    }
    Ok(aggregates)
}

/// Writes the callback table: a count, then a `u32` function index and a
/// signature per row.
///
/// Appended after the aggregate table, which a callback signature may index
/// exactly as an import's does.
pub(crate) fn write_foreign_callbacks(out: &mut Vec<u8>, callbacks: &[ForeignCallback]) {
    write_count(out, callbacks.len());
    for callback in callbacks {
        write_u32(out, callback.function());
        let signature = callback.signature();
        write_count(out, signature.parameters().len());
        for parameter in signature.parameters() {
            write_spec(out, *parameter);
        }
        write_spec(out, signature.result());
    }
}

/// Reads the callback table, or an empty one when the stream ends first.
///
/// The aggregate table is read before this, so a signature's aggregate
/// references are resolved here against it: an index the table does not
/// contain is refused at load with the same error an import gets, rather than
/// surfacing later in engine-specific vocabulary at the first call.
pub(crate) fn read_foreign_callbacks(
    reader: &mut Reader<'_>,
    format: Format,
    aggregates: &ForeignAggregates,
) -> Result<Vec<ForeignCallback>, ModuleDecodeError> {
    if reader.is_at_end() {
        return Ok(Vec::new());
    }
    let count = reader.read_index_count(format, "foreign callback")?;
    let mut callbacks = Vec::new();
    for index in 0..count {
        let function = reader.read_u32()?;
        // A callback row names no symbol, so a malformed type tag is reported
        // against the row's own index rather than an import's name.
        let named = format!("callback {index}");
        let param_count = reader.read_count(format)?;
        let mut parameters = Vec::new();
        for _ in 0..param_count {
            parameters.push(read_spec(reader, &named)?);
        }
        let result = read_spec(reader, &named)?;
        let signature = ForeignSignature::new(parameters, result);
        for spec in signature
            .parameters()
            .iter()
            .copied()
            .chain(std::iter::once(signature.result()))
        {
            if let Some(id) = spec.aggregate()
                && aggregates.get(id).is_none()
            {
                return Err(ModuleDecodeError::UnknownCallbackAggregate {
                    callback: index,
                    index: id.0,
                });
            }
        }
        callbacks.push(ForeignCallback::new(function, signature));
    }
    Ok(callbacks)
}

/// Writes one signature position.
fn write_spec(out: &mut Vec<u8>, spec: ForeignTypeSpec) {
    out.push(spec.tag());
    if let Some(id) = spec.aggregate() {
        write_u32(out, id.0);
    }
}

/// Reads one signature position, naming `import` in the error on an unknown tag
/// so the diagnosis points at the offending row.
fn read_spec(reader: &mut Reader<'_>, import: &str) -> Result<ForeignTypeSpec, ModuleDecodeError> {
    let tag = reader.take(1)?[0];
    if tag == ForeignTypeSpec::AGGREGATE_TAG {
        return Ok(ForeignTypeSpec::Aggregate(ForeignAggregateId(
            reader.read_u32()?,
        )));
    }
    ForeignType::from_tag(tag)
        .map(ForeignTypeSpec::Scalar)
        .ok_or_else(|| ModuleDecodeError::UnknownForeignType {
            import: import.to_owned(),
            tag,
        })
}

/// Reads one aggregate member, naming the containing aggregate on an unknown
/// scalar tag.
fn read_member(reader: &mut Reader<'_>, index: u64) -> Result<ForeignMember, ModuleDecodeError> {
    let tag = reader.take(1)?[0];
    if tag == NESTED_MEMBER_TAG {
        return Ok(ForeignMember::Aggregate(ForeignAggregateId(
            reader.read_u32()?,
        )));
    }
    if tag == ARRAY_MEMBER_TAG {
        let element = read_array_element(reader, index)?;
        return Ok(ForeignMember::Array {
            element,
            count: reader.read_u32()?,
        });
    }
    ForeignType::from_tag(tag)
        .map(ForeignMember::Scalar)
        .ok_or(ModuleDecodeError::UnknownForeignAggregateMember { index, tag })
}

/// Reads an inline array's element: a scalar tag, or a nested aggregate index.
///
/// An element is never itself an array — a C array of arrays is written as an
/// array of the aggregate wrapping the inner one — so an [`ARRAY_MEMBER_TAG`]
/// here is as unknown as any other tag the writer never emits.
fn read_array_element(
    reader: &mut Reader<'_>,
    index: u64,
) -> Result<ForeignArrayElement, ModuleDecodeError> {
    let tag = reader.take(1)?[0];
    if tag == NESTED_MEMBER_TAG {
        return Ok(ForeignArrayElement::Aggregate(ForeignAggregateId(
            reader.read_u32()?,
        )));
    }
    ForeignType::from_tag(tag)
        .map(ForeignArrayElement::Scalar)
        .ok_or(ModuleDecodeError::UnknownForeignAggregateMember { index, tag })
}

#[cfg(test)]
mod tests {
    use kira_runtime_abi::Execution;

    use crate::module::{MAGIC, Module};
    use crate::op::Instruction;

    use super::*;

    /// A module with two foreign imports and no exports: the case that exercises
    /// the empty-exports prelude.
    fn foreign_module() -> Module {
        Module {
            functions: vec![crate::module::FuncProto {
                name: "main".to_owned(),
                param_count: 0,
                local_count: 0,
                execution: Execution::Runtime,
                code: vec![Instruction::CallForeign(0), Instruction::ReturnVoid],
                releases: crate::module::FrameRelease::EveryLocal,
            }],
            main: Some(0),
            strings: Vec::new(),
            exports: Default::default(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            foreign_imports: vec![
                ForeignImport::new(
                    "ffimath",
                    "kira_ffi_add",
                    ForeignAbi::C,
                    ForeignSignature::scalars(
                        [ForeignType::I32, ForeignType::I32],
                        ForeignType::I32,
                    ),
                ),
                ForeignImport::new(
                    "ffimath",
                    "kira_ffi_name_len",
                    ForeignAbi::C,
                    ForeignSignature::scalars([ForeignType::CString], ForeignType::U64),
                ),
            ],
        }
    }

    #[test]
    fn a_module_with_foreign_imports_round_trips() {
        let module = foreign_module();
        let decoded = Module::from_bytes(&module.to_bytes()).expect("decodes");
        assert_eq!(decoded, module);
        assert_eq!(decoded.foreign_imports.len(), 2);
        assert_eq!(decoded.foreign_imports[0].symbol(), "kira_ffi_add");
    }

    #[test]
    fn retained_parameters_round_trip_and_absence_means_borrowed() {
        let mut module = foreign_module();
        // Without a `retains:` mark, the bytes carry no retained section and
        // every parameter decodes borrowed.
        let borrowed = Module::from_bytes(&module.to_bytes()).expect("decodes");
        assert!(!borrowed.foreign_imports[0].signature().any_retained());

        module.foreign_imports[0].retain_parameters([false, true]);
        let decoded = Module::from_bytes(&module.to_bytes()).expect("decodes");
        assert_eq!(decoded, module);
        let signature = decoded.foreign_imports[0].signature();
        assert!(!signature.is_retained(0));
        assert!(signature.is_retained(1));
        assert!(!decoded.foreign_imports[1].signature().any_retained());
    }

    #[test]
    fn a_truncated_retained_section_is_a_typed_error() {
        let mut module = foreign_module();
        module.foreign_imports[0].retain_parameters([false, true]);
        let complete = module.to_bytes();
        let mut section = Vec::new();
        write_foreign_retained(&mut section, &module.foreign_imports);
        // Every cut *inside* the retained section must be a typed refusal,
        // never a module that silently decodes with different lifetimes. The
        // cut at the section's own boundary is the append-only bargain — it
        // reads as a module written before the section existed — and is
        // exercised by the absence test above, not here.
        let start = complete.len() - section.len();
        for cut in start + 1..complete.len() {
            assert!(
                Module::from_bytes(&complete[..cut]).is_err(),
                "prefix of {cut}/{} bytes decoded",
                complete.len()
            );
        }
    }

    #[test]
    fn a_retained_position_outside_the_signature_is_a_typed_error() {
        let mut module = foreign_module();
        // The *last* import retains, so the section's final four bytes are the
        // retained position (little-endian u32); pointing it past the
        // signature must be refused, not clamped.
        module.foreign_imports[1].retain_parameters([true]);
        let mut bytes = module.to_bytes();
        let at = bytes.len() - 4;
        bytes[at..].copy_from_slice(&9u32.to_le_bytes());
        assert!(matches!(
            Module::from_bytes(&bytes).unwrap_err(),
            ModuleDecodeError::RetainedOutOfRange { position: 9, .. }
        ));
    }

    #[test]
    fn a_duplicate_retained_position_is_a_typed_error() {
        let mut module = foreign_module();
        module.foreign_imports[1].retain_parameters([true]);
        let mut bytes = module.to_bytes();
        let count = bytes.len() - 12;
        bytes[count..count + 8].copy_from_slice(&2u64.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            Module::from_bytes(&bytes).unwrap_err(),
            ModuleDecodeError::DuplicateRetainedPosition { position: 0, .. }
        ));
    }

    #[test]
    fn zero_imports_writes_no_section_and_decodes_empty() {
        let mut module = foreign_module();
        module.foreign_imports.clear();
        let bytes = module.to_bytes();
        // With neither exports nor foreign imports, nothing is appended after
        // the function table, so the bytes match a pre-foreign module exactly.
        let mut pre_foreign = module.clone();
        pre_foreign.foreign_imports = Vec::new();
        assert_eq!(pre_foreign.to_bytes(), bytes);
        assert!(
            Module::from_bytes(&bytes)
                .expect("decodes")
                .foreign_imports
                .is_empty()
        );
    }

    #[test]
    fn imports_without_exports_still_write_the_empty_export_prelude() {
        // The foreign module has no exports; the empty exports framing must be
        // present so the foreign bytes are not misread as an exports section.
        let module = foreign_module();
        assert!(module.exports.is_empty());
        let decoded = Module::from_bytes(&module.to_bytes()).expect("decodes");
        assert!(decoded.exports.is_empty());
        assert_eq!(decoded.foreign_imports, module.foreign_imports);
    }

    #[test]
    fn every_truncation_inside_the_foreign_section_is_a_typed_error() {
        // `foreign_module` has no exports, so its bytes are:
        //   functions | empty-exports prelude (8 bytes) | foreign section
        // A build with neither exports nor foreign imports writes only the
        // functions, so `functions_end` marks where the empty-exports prelude
        // begins and `foreign_start` (8 bytes later) marks where the foreign
        // section begins.
        let mut bare = foreign_module();
        bare.foreign_imports = Vec::new();
        let functions_end = bare.to_bytes().len();
        let foreign_start = functions_end + 16;

        let bytes = foreign_module().to_bytes();
        let complete = bytes.len();
        for cut in functions_end + 1..complete {
            // Two cuts are exempt, exactly as the exports section exempts the
            // point where its own section could be absent: a stream ending at
            // `functions_end` is an old module (no sections), and one ending at
            // `foreign_start` is a module whose foreign section is absent. Both
            // are indistinguishable from a shorter valid module, and neither can
            // be told from a truncation by any decoder. Every other cut lands
            // mid-section and must be a typed error.
            if cut == foreign_start {
                continue;
            }
            match Module::from_bytes(&bytes[..cut]) {
                Err(_) => {}
                Ok(module) => panic!("prefix of {cut}/{complete} bytes decoded as {module:?}"),
            }
        }
        for exempt in [functions_end, foreign_start] {
            assert!(
                Module::from_bytes(&bytes[..exempt])
                    .expect("a stream ending at a section boundary is a shorter valid module")
                    .foreign_imports
                    .is_empty()
            );
        }
        assert_eq!(
            Module::from_bytes(&bytes).expect("decodes"),
            foreign_module()
        );
    }

    /// A one-import module whose foreign section has a deterministic tail:
    /// `...[abi][param_count=1(4 bytes)][param tag][result tag]`. That fixes the
    /// abi byte at `len - 7`, the parameter tag at `len - 2`, and the result tag
    /// at `len - 1`, so a corruption test needs no byte-hunting.
    fn single_import_module() -> Module {
        let mut module = foreign_module();
        module.foreign_imports = vec![ForeignImport::new(
            "lib",
            "sym",
            ForeignAbi::C,
            ForeignSignature::scalars([ForeignType::I8], ForeignType::I32),
        )];
        module
    }

    #[test]
    fn an_unknown_abi_tag_is_a_typed_error() {
        let mut bytes = single_import_module().to_bytes();
        let abi_index = bytes.len() - 11;
        bytes[abi_index] = 0x7f;
        assert!(matches!(
            Module::from_bytes(&bytes),
            Err(ModuleDecodeError::UnknownForeignAbi { tag: 0x7f, .. })
        ));
    }

    #[test]
    fn an_unknown_foreign_type_tag_is_a_typed_error() {
        let mut bytes = single_import_module().to_bytes();
        let param_index = bytes.len() - 2;
        bytes[param_index] = 0x6e;
        assert!(matches!(
            Module::from_bytes(&bytes),
            Err(ModuleDecodeError::UnknownForeignType { tag: 0x6e, .. })
        ));
        let mut bytes = single_import_module().to_bytes();
        let result_index = bytes.len() - 1;
        bytes[result_index] = 0x6d;
        assert!(matches!(
            Module::from_bytes(&bytes),
            Err(ModuleDecodeError::UnknownForeignType { tag: 0x6d, .. })
        ));
    }

    #[test]
    fn an_aggregate_table_with_an_inline_array_round_trips() {
        let mut aggregates = ForeignAggregates::new();
        let pair = aggregates
            .push(ForeignAggregate::new(vec![
                ForeignMember::Scalar(ForeignType::I32),
                ForeignMember::Scalar(ForeignType::I32),
            ]))
            .expect("pushes");
        let grid = aggregates
            .push(ForeignAggregate::new(vec![
                ForeignMember::Array {
                    element: ForeignArrayElement::Scalar(ForeignType::I32),
                    count: 4,
                },
                ForeignMember::Array {
                    element: ForeignArrayElement::Aggregate(pair),
                    count: 2,
                },
                ForeignMember::Scalar(ForeignType::F64),
            ]))
            .expect("pushes");

        let mut module = foreign_module();
        module.foreign_aggregates = aggregates;
        module.foreign_imports = vec![ForeignImport::new(
            "fixture",
            "takes_grid",
            ForeignAbi::C,
            ForeignSignature::new(
                vec![ForeignTypeSpec::Aggregate(grid)],
                ForeignTypeSpec::Scalar(ForeignType::Void),
            ),
        )];

        let decoded = Module::from_bytes(&module.to_bytes()).expect("decodes");
        assert_eq!(decoded, module);
    }

    #[test]
    fn a_foreign_module_still_opens_with_the_magic() {
        assert_eq!(&foreign_module().to_bytes()[0..4], &MAGIC);
    }
}
