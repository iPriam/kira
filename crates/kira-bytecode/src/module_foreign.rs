//! The KBC1 foreign-import section: the `@FFI.Extern` rows a `CallForeign` id
//! indexes.
//!
//! Appended after the exports section, with the same append-only discipline:
//! an old module ends before it and decodes as zero imports, a partial section
//! is a typed truncation error, and an unknown ABI or foreign-type tag is a
//! typed error rather than a guess. See [`crate::module`] for the empty-exports
//! prelude that keeps this section unambiguous when a module has foreign
//! imports but no exports.

use kira_runtime_abi::{ForeignAbi, ForeignImport, ForeignSignature, ForeignType};

use crate::module::{ModuleDecodeError, Reader, write_bytes, write_u32};

/// Writes the foreign-import section: a count, then one row per import.
///
/// Each row is the library name, the C symbol, the ABI tag byte, the parameter
/// count, one foreign-type tag byte per parameter, and the result tag byte.
pub(crate) fn write_foreign(out: &mut Vec<u8>, imports: &[ForeignImport]) {
    write_u32(out, imports.len() as u32);
    for import in imports {
        write_bytes(out, import.library().as_bytes());
        write_bytes(out, import.symbol().as_bytes());
        out.push(import.abi().tag());
        let signature = import.signature();
        write_u32(out, signature.parameters().len() as u32);
        for parameter in signature.parameters() {
            out.push(parameter.tag());
        }
        out.push(signature.result().tag());
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
) -> Result<Vec<ForeignImport>, ModuleDecodeError> {
    if reader.is_at_end() {
        return Ok(Vec::new());
    }
    let count = reader.read_u32()?;
    let mut imports = Vec::new();
    for _ in 0..count {
        let library = reader.read_string()?;
        let symbol = reader.read_string()?;
        let abi_byte = reader.take(1)?[0];
        let abi =
            ForeignAbi::from_tag(abi_byte).ok_or_else(|| ModuleDecodeError::UnknownForeignAbi {
                import: symbol.clone(),
                tag: abi_byte,
            })?;
        let param_count = reader.read_u32()?;
        let mut parameters = Vec::new();
        for _ in 0..param_count {
            parameters.push(read_foreign_type(reader, &symbol)?);
        }
        let result = read_foreign_type(reader, &symbol)?;
        imports.push(ForeignImport::new(
            library,
            symbol,
            abi,
            ForeignSignature::new(parameters, result),
        ));
    }
    Ok(imports)
}

/// Reads one foreign-type tag byte, naming `import` in the error on an unknown
/// tag so the diagnosis points at the offending row.
fn read_foreign_type(
    reader: &mut Reader<'_>,
    import: &str,
) -> Result<ForeignType, ModuleDecodeError> {
    let tag = reader.take(1)?[0];
    ForeignType::from_tag(tag).ok_or_else(|| ModuleDecodeError::UnknownForeignType {
        import: import.to_owned(),
        tag,
    })
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
            }],
            main: Some(0),
            strings: Vec::new(),
            exports: Default::default(),
            foreign_imports: vec![
                ForeignImport::new(
                    "ffimath",
                    "kira_ffi_add",
                    ForeignAbi::C,
                    ForeignSignature::new([ForeignType::I32, ForeignType::I32], ForeignType::I32),
                ),
                ForeignImport::new(
                    "ffimath",
                    "kira_ffi_name_len",
                    ForeignAbi::C,
                    ForeignSignature::new([ForeignType::CString], ForeignType::U64),
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
        let foreign_start = functions_end + 8;

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
            ForeignSignature::new([ForeignType::I8], ForeignType::I32),
        )];
        module
    }

    #[test]
    fn an_unknown_abi_tag_is_a_typed_error() {
        let mut bytes = single_import_module().to_bytes();
        let abi_index = bytes.len() - 7;
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
    fn a_foreign_module_still_opens_with_the_magic() {
        assert_eq!(&foreign_module().to_bytes()[0..4], &MAGIC);
    }
}
