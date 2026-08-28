//! Byte-format round-trip, truncation and rejection tests for the module,
//! split out of `module.rs` on the file-size ladder.

use super::*;
use kira_runtime_abi::{BridgeValueTag, ForeignAbi, ForeignType, ForeignTypeSpec};

#[test]
fn module_round_trips_through_bytes() {
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        main: Some(1),
        strings: vec!["hello".to_owned(), "world".to_owned()],
        functions: vec![
            FuncProto {
                name: "helper".to_owned(),
                param_count: 1,
                local_count: 2,
                execution: Execution::Runtime,
                code: vec![Instruction::LoadLocal(0), Instruction::Return],
                releases: FrameRelease::EveryLocal,
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
                releases: FrameRelease::EveryLocal,
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
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        main: None,
        strings: Vec::new(),
        functions: vec![FuncProto {
            name: "add".to_owned(),
            param_count: 2,
            local_count: 2,
            execution: Execution::Runtime,
            code: vec![Instruction::LoadLocal(0), Instruction::Return],
            releases: FrameRelease::EveryLocal,
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
    assert_eq!(&bytes[4..12], &[0xff; 8]);
    assert_eq!(NO_ENTRYPOINT, u32::MAX);
}

#[test]
fn an_entrypoint_index_is_never_the_sentinel() {
    // The two states are distinguishable in both directions: a real index
    // decodes as `Some`, never as a library.
    let bytes = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        main: Some(0),
        ..library_module()
    }
    .to_bytes();
    assert_eq!(&bytes[4..12], &[0; 8]);
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
        releases: FrameRelease::EveryLocal,
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
    // `exporting_module` has exports but no foreign imports, so its bytes
    // end after the exports section. Each appended section is complete when
    // it carries a count of zero — four zero bytes — and there are five of
    // them: foreign imports, aggregates, callbacks, retained parameters, and
    // module constants. The release section cannot be empty the same way: its
    // entries are positional, so a complete one names every function, here
    // two, each asking for every local. Three more bytes past all of that is
    // trailing garbage the decoder must reject once every section is read,
    // rather than run half an artifact.
    let module = exporting_module();
    let mut bytes = module.to_bytes();
    for _ in 0..3 {
        bytes.extend_from_slice(&[0; 8]);
    }
    bytes.extend_from_slice(&(module.functions.len() as u64).to_le_bytes());
    for _ in 0..module.functions.len() {
        bytes.extend_from_slice(&[0xff; 8]);
    }
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0, 0, 0]);
    assert_eq!(
        Module::from_bytes(&bytes).unwrap_err(),
        ModuleDecodeError::TrailingBytes(3)
    );
}

fn legacy_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn legacy_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn legacy_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    legacy_u32(bytes, value.len() as u32);
    bytes.extend_from_slice(value);
}

/// A KBC1 fixture with every appended section and every section's legacy
/// width. It is deliberately hand-written: KBC2 is the only format this crate
/// emits, so compatibility coverage must prove that the reader still accepts
/// bytes produced by the old writer rather than round-tripping through a new
/// encoder.
fn legacy_module_with_all_sections() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&LEGACY_MAGIC);
    legacy_u32(&mut bytes, 0);
    legacy_u32(&mut bytes, 1);
    legacy_bytes(&mut bytes, b"s");
    legacy_u32(&mut bytes, 1);
    legacy_bytes(&mut bytes, b"main");
    legacy_u16(&mut bytes, 0);
    legacy_u16(&mut bytes, 2);
    bytes.push(Execution::Runtime.as_byte());
    let code = [
        0x04, 0, 0, 0, 0, // ConstStr(0)
        0x07, 1, 0, // StoreLocal(1)
        0x06, 1, 0,    // LoadLocal(1)
        0x2a, // Return
    ];
    legacy_u32(&mut bytes, code.len() as u32);
    bytes.extend_from_slice(&code);

    // Exports: one class and one zero-argument handle-returning function.
    legacy_u32(&mut bytes, 1);
    legacy_bytes(&mut bytes, b"Button");
    legacy_u32(&mut bytes, 1);
    legacy_bytes(&mut bytes, b"make_button");
    legacy_bytes(&mut bytes, b"makeButton");
    legacy_u32(&mut bytes, 0);
    legacy_u32(&mut bytes, 0);
    bytes.push(BridgeValueTag::HANDLE.0);
    legacy_u32(&mut bytes, 0);

    // Foreign imports: one aggregate parameter and an I32 result.
    legacy_u32(&mut bytes, 1);
    legacy_bytes(&mut bytes, b"lib");
    legacy_bytes(&mut bytes, b"sym");
    bytes.push(ForeignAbi::C.tag());
    legacy_u32(&mut bytes, 1);
    bytes.push(ForeignTypeSpec::AGGREGATE_TAG);
    legacy_u32(&mut bytes, 0);
    bytes.push(ForeignType::I32.tag());

    // One aggregate row containing one scalar member.
    legacy_u32(&mut bytes, 1);
    legacy_u32(&mut bytes, 1);
    bytes.push(ForeignType::I32.tag());

    // One callback row, with one I32 parameter and a void result.
    legacy_u32(&mut bytes, 1);
    legacy_u32(&mut bytes, 0);
    legacy_u32(&mut bytes, 1);
    bytes.push(ForeignType::I32.tag());
    bytes.push(ForeignType::Void.tag());

    // One planned release slot. KBC1 plans use a u16 slot width.
    legacy_u32(&mut bytes, 1);
    legacy_u32(&mut bytes, 1);
    legacy_u16(&mut bytes, 1);
    bytes
}

#[test]
fn a_kbc1_module_remains_readable() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&LEGACY_MAGIC);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(b"main");
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.push(Execution::Runtime.as_byte());
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&[0x06, 0, 0, 0x2a]);

    let decoded = Module::from_bytes(&bytes).expect("KBC1 remains readable");
    assert_eq!(decoded.main, Some(0));
    assert_eq!(decoded.functions[0].param_count, 0);
    assert_eq!(decoded.functions[0].local_count, 1);
    assert_eq!(
        decoded.functions[0].code,
        vec![Instruction::LoadLocal(0), Instruction::Return]
    );
}

#[test]
fn kbc1_all_sections_decode_and_upgrade_to_a_kbc2_round_trip() {
    let legacy = legacy_module_with_all_sections();
    let decoded = Module::from_bytes(&legacy).expect("all KBC1 sections decode");
    assert_eq!(decoded.strings, ["s"]);
    assert_eq!(decoded.exports.classes, ["Button"]);
    assert_eq!(decoded.foreign_imports.len(), 1);
    assert_eq!(decoded.foreign_aggregates.len(), 1);
    assert_eq!(decoded.foreign_callbacks.len(), 1);
    assert_eq!(
        decoded.functions[0].releases,
        FrameRelease::Planned(vec![1])
    );
    decoded
        .validate()
        .expect("legacy fixture is structurally valid");

    let current = decoded.to_bytes();
    assert_eq!(&current[..4], &MAGIC);
    assert_eq!(
        Module::from_bytes(&current).expect("KBC2 round trip"),
        decoded
    );
}

#[test]
fn kbc2_round_trips_wide_function_and_release_operands_with_all_sections() {
    let mut module =
        Module::from_bytes(&legacy_module_with_all_sections()).expect("legacy fixture decodes");
    let above_u16 = u64::from(u16::MAX) + 1;
    module.functions[0].local_count = above_u16 + 1;
    module.functions[0].releases = FrameRelease::Planned(vec![above_u16]);
    let bytes = module.to_bytes();
    assert_eq!(&bytes[..4], &MAGIC);
    assert_eq!(
        Module::from_bytes(&bytes).expect("wide module decodes"),
        module
    );
}

#[test]
fn kbc2_rejects_an_entrypoint_beyond_the_u32_function_id_seam() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&(u64::from(u32::MAX) + 1).to_le_bytes());
    assert_eq!(
        Module::from_bytes(&bytes).unwrap_err(),
        ModuleDecodeError::EntrypointTooLarge {
            index: u64::from(u32::MAX) + 1,
        }
    );
}

#[test]
fn kbc2_rejects_an_indexed_section_count_beyond_its_u32_seam() {
    let bytes = u64::MAX.to_le_bytes();
    let mut reader = Reader {
        bytes: &bytes,
        offset: 0,
    };
    assert_eq!(
        reader.read_index_count(Format::Wide, "foreign callback"),
        Err(ModuleDecodeError::IndexTableTooLarge {
            table: "foreign callback",
            count: u64::MAX,
        })
    );
}

#[test]
fn kbc2_rejects_a_function_count_without_function_bytes() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&u64::MAX.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(
        Module::from_bytes(&bytes).unwrap_err(),
        ModuleDecodeError::Truncated
    );
}

#[test]
fn kbc2_rejects_an_impossible_length_without_allocating_it() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&u64::MAX.to_le_bytes());
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(&u64::MAX.to_le_bytes());

    assert_eq!(
        Module::from_bytes(&bytes).unwrap_err(),
        ModuleDecodeError::Truncated
    );
}

/// A module whose second function is a constant init and whose main reads the
/// slot it fills.
fn constant_module() -> Module {
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: vec![1],
        main: Some(0),
        strings: Vec::new(),
        functions: vec![
            FuncProto {
                name: "main".to_owned(),
                param_count: 0,
                local_count: 0,
                execution: Execution::Runtime,
                code: vec![
                    Instruction::LoadConstant(0),
                    Instruction::Pop,
                    Instruction::ReturnVoid,
                ],
                releases: FrameRelease::EveryLocal,
            },
            FuncProto {
                name: "answer$constant".to_owned(),
                param_count: 0,
                local_count: 0,
                execution: Execution::Runtime,
                code: vec![Instruction::ConstInt(42), Instruction::Return],
                releases: FrameRelease::EveryLocal,
            },
        ],
    }
}

#[test]
fn a_constants_section_round_trips_through_bytes() {
    let module = constant_module();
    let bytes = module.to_bytes();
    let decoded = Module::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, module);
    assert_eq!(decoded.constants, vec![1]);
}

#[test]
fn a_module_without_constants_writes_no_constants_section() {
    // The last eight bytes of a constant-carrying module are its one table
    // row; a module without constants ends at the section before it.
    let with = constant_module().to_bytes();
    let mut without = constant_module();
    without.constants = Vec::new();
    let without = without.to_bytes();
    assert!(with.len() > without.len());
}

#[test]
fn a_constant_row_past_the_function_table_is_rejected() {
    let mut module = constant_module();
    module.constants = vec![9];
    let bytes = module.to_bytes();
    assert_eq!(
        Module::from_bytes(&bytes).unwrap_err(),
        ModuleDecodeError::ConstantInitOutOfRange {
            slot: 0,
            init: 9,
            functions: 2,
        }
    );
}

#[test]
fn a_truncated_constants_section_is_a_typed_error() {
    let bytes = constant_module().to_bytes();
    assert_eq!(
        Module::from_bytes(&bytes[..bytes.len() - 1]).unwrap_err(),
        ModuleDecodeError::Truncated
    );
}
