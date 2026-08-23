//! Tests for the hybrid manifest: what it accepts, and what it refuses by name.
//!
//! Split from `manifest.rs` to keep that file under the size the repository
//! allows; every case here is one manifest shape.

use super::*;

fn manifest() -> HybridManifest {
    HybridManifest {
        module_name: "demo".to_owned(),
        bytecode_path: ".kira-build/demo.kbc".to_owned(),
        native_library_path: ".kira-build/libdemo.dylib".to_owned(),
        entry: Some(0),
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
        foreign: Vec::new(),
        foreign_aggregates: Default::default(),
        internal_functions: 0,
    }
}

fn foreign_manifest() -> HybridManifest {
    let mut manifest = manifest();
    manifest.foreign = vec![
        HybridForeign {
            library: "ffimath".to_owned(),
            symbol: "kira_ffi_add".to_owned(),
            abi: ForeignAbi::C,
            signature: ForeignSignature::scalars(
                [ForeignType::I32, ForeignType::I32],
                ForeignType::I32,
            ),
            adapter_symbol: "kira_foreign_adapter_0".to_owned(),
        },
        HybridForeign {
            library: "ffimath".to_owned(),
            symbol: "kira_ffi_name_len".to_owned(),
            abi: ForeignAbi::C,
            signature: ForeignSignature::scalars([ForeignType::CString], ForeignType::U64),
            adapter_symbol: "kira_foreign_adapter_1".to_owned(),
        },
    ];
    manifest
}

#[test]
fn a_manifest_with_foreign_imports_round_trips() {
    let original = foreign_manifest();
    let decoded = HybridManifest::from_bytes(&original.to_bytes()).expect("decodes");
    assert_eq!(decoded, original);
    assert_eq!(decoded.foreign.len(), 2);
    assert_eq!(decoded.foreign[0].adapter_symbol, "kira_foreign_adapter_0");
}

#[test]
fn retained_foreign_parameters_round_trip_in_the_appended_tail() {
    let mut original = foreign_manifest();
    original.foreign[0].signature = original.foreign[0]
        .signature
        .clone()
        .with_retained([false, true]);
    let bytes = original.to_bytes();
    let decoded = HybridManifest::from_bytes(&bytes).expect("decodes");
    assert_eq!(decoded, original);
    assert!(decoded.foreign[0].signature.is_retained(1));
    assert!(!decoded.foreign[0].signature.is_retained(0));

    // Two rows: one count plus position for the first import, and a zero count
    // for the second, behind the section's row count.
    let retained_start = bytes.len() - 16;
    for length in retained_start + 1..bytes.len() {
        assert!(
            HybridManifest::from_bytes(&bytes[..length]).is_err(),
            "retained tail truncated to {length} bytes must be rejected"
        );
    }
}

#[test]
fn an_old_manifest_without_a_foreign_section_decodes_with_none() {
    // A manifest with no foreign imports writes no foreign bytes at all, so
    // its encoding is identical to one produced before the section existed.
    let bare = manifest();
    assert!(bare.foreign.is_empty());
    let decoded = HybridManifest::from_bytes(&bare.to_bytes()).expect("decodes");
    assert!(decoded.foreign.is_empty());
}

#[test]
fn an_unavailable_foreign_binding_round_trips_with_an_empty_locator() {
    let mut broken = foreign_manifest();
    broken.foreign[0].adapter_symbol.clear();
    assert_eq!(HybridManifest::from_bytes(&broken.to_bytes()), Ok(broken));
}

#[test]
fn an_unknown_foreign_type_byte_is_rejected() {
    // The last import's tail is `...[result tag][adapter-symbol string]`.
    // Corrupt the first import's parameter tag by finding the abi byte after
    // its symbol; simpler is to corrupt a byte and require a typed error for
    // every corruption that lands inside the foreign section.
    let mut bytes = foreign_manifest().to_bytes();
    let clean_len = manifest().to_bytes().len();
    // The parameter tag of the first import sits a few bytes past the count;
    // scan for a byte we can flip to an out-of-range foreign type and assert
    // the decode stays typed rather than panicking.
    let mut saw_typed = false;
    for index in clean_len..bytes.len() {
        let original = bytes[index];
        bytes[index] = 0x7f;
        if let Err(ManifestDecodeError::UnknownForeignType { tag: 0x7f, .. }) =
            HybridManifest::from_bytes(&bytes)
        {
            saw_typed = true;
        }
        bytes[index] = original;
    }
    assert!(
        saw_typed,
        "a foreign-type byte in the section must decode to a typed error"
    );
}

#[test]
fn a_count_larger_than_the_input_is_typed_rather_than_reserved() {
    // Every counted run in this format spends at least a byte per element,
    // so a count past the end of the stream is malformed on its face. It
    // has to be rejected before allocation. Passing a corrupted high byte to
    // `Vec::with_capacity` could request billions of elements and abort the
    // process instead of returning a typed error.
    //
    // Every count in the manifest gets the same treatment, so this walks
    // them: the function count, a function's parameter count, the foreign
    // import count, an import's parameter count, and an aggregate's member
    // count all sit somewhere in these bytes as a little-endian `u32`.
    let bytes = foreign_manifest().to_bytes();
    let mut reserved = 0;
    for index in 0..bytes.len().saturating_sub(4) {
        let mut corrupt = bytes.clone();
        // Raise the high byte only. The low bytes stay whatever they were,
        // so this lands on a plausible-looking count rather than a value no
        // encoder would ever produce.
        corrupt[index + 3] = 0x7f;
        // Any other outcome is fine — the byte landed on a tag, a length or
        // a string instead, and a typed error or a clean decode both say the
        // decoder stayed in control. What must never happen is the process
        // dying, which is a failure no assertion here can catch and the
        // reason this walks every offset rather than one known one.
        if let Err(ManifestDecodeError::CountExceedsInput { count, remaining }) =
            HybridManifest::from_bytes(&corrupt)
        {
            assert!(
                count > remaining,
                "a count error must actually exceed the input: {count} vs {remaining}"
            );
            reserved += 1;
        }
    }
    assert!(
        reserved > 0,
        "no corrupted count reached the guard; the walk covers no count field"
    );
}

#[test]
fn every_truncation_inside_the_foreign_section_is_typed() {
    let bytes = foreign_manifest().to_bytes();
    // A manifest with the same functions but no foreign section ends exactly
    // here; that boundary is the one clean shorter decode (an old manifest).
    let functions_end = manifest().to_bytes().len();
    assert!(
        HybridManifest::from_bytes(&bytes[..functions_end])
            .expect("the functions boundary is a shorter valid manifest")
            .foreign
            .is_empty()
    );
    for length in functions_end + 1..bytes.len() {
        match HybridManifest::from_bytes(&bytes[..length]) {
            Err(_) => {}
            Ok(decoded) => panic!(
                "a manifest truncated to {length} bytes decoded with {} foreign rows",
                decoded.foreign.len()
            ),
        }
    }
    assert!(HybridManifest::from_bytes(&bytes).is_ok());
}

#[test]
fn a_manifest_round_trips() {
    let original = manifest();
    let decoded = HybridManifest::from_bytes(&original.to_bytes()).expect("decodes");
    assert_eq!(decoded, original);
    assert_eq!(
        decoded.entry_function().expect("an entrypoint").name,
        "main"
    );
}

/// The internal-function count survives a round trip with no foreign
/// section to sit behind.
///
/// The trailing sections are positional rather than tagged, so writing this
/// count while the two before it are absent would have the decoder read it
/// as the foreign-import count. The encoder writes those empty sections
/// explicitly when there is a tail; this is what holds it.
#[test]
fn an_internal_function_count_round_trips_without_a_foreign_section() {
    let mut original = manifest();
    original.internal_functions = 3;
    assert!(original.foreign.is_empty());
    assert!(original.foreign_aggregates.is_empty());
    let decoded = HybridManifest::from_bytes(&original.to_bytes()).expect("decodes");
    assert_eq!(decoded, original);
    assert_eq!(decoded.internal_functions, 3);
    assert!(decoded.foreign.is_empty());
}

/// A program that widens nothing writes the bytes it always did.
///
/// What keeps the field append-only: the tail is omitted when the count is
/// zero, so a manifest predating it decodes identically and one written now
/// is byte-for-byte what the old encoder produced.
#[test]
fn a_zero_internal_count_writes_no_tail() {
    let mut original = manifest();
    assert_eq!(original.internal_functions, 0);
    let without = original.to_bytes();
    original.internal_functions = 1;
    let with = original.to_bytes();
    assert!(with.len() > without.len());
    assert_eq!(&with[..without.len()], &without[..]);
    assert_eq!(
        HybridManifest::from_bytes(&without)
            .expect("decodes")
            .internal_functions,
        0
    );
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
    aggregates
        .push(ForeignAggregate::new(vec![
            ForeignMember::Array {
                element: ForeignArrayElement::Scalar(ForeignType::I32),
                count: 4,
            },
            ForeignMember::Array {
                element: ForeignArrayElement::Aggregate(pair),
                count: 2,
            },
        ]))
        .expect("pushes");

    // The table is written after the imports that index it, so a manifest
    // carrying one always carries the import that named it.
    let mut original = foreign_manifest();
    original.foreign[0].signature = ForeignSignature::new(
        vec![ForeignTypeSpec::Aggregate(ForeignAggregateId(1))],
        ForeignTypeSpec::Scalar(ForeignType::Void),
    );
    original.foreign_aggregates = aggregates;
    let decoded = HybridManifest::from_bytes(&original.to_bytes()).expect("decodes");
    assert_eq!(decoded.foreign_aggregates, original.foreign_aggregates);
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
    broken.entry = None;
    broken.functions[1].exported_name = None;
    assert_eq!(
        HybridManifest::from_bytes(&broken.to_bytes()),
        Err(ManifestDecodeError::NativeWithoutSymbol("hot".to_owned()))
    );
}

#[test]
fn an_unreachable_native_application_function_may_omit_its_symbol() {
    let mut application = manifest();
    application.functions[1].exported_name = None;
    let decoded =
        HybridManifest::from_bytes(&application.to_bytes()).expect("the application is valid");
    assert_eq!(decoded.functions[1].exported_name, None);
}

#[test]
fn a_library_manifest_round_trips_with_no_entrypoint() {
    let mut library = manifest();
    library.entry = None;
    let decoded = HybridManifest::from_bytes(&library.to_bytes()).expect("a valid manifest");
    assert_eq!(decoded, library);
    assert_eq!(decoded.entry, None);
    assert!(decoded.entry_function().is_none());
}

#[test]
fn the_no_entrypoint_sentinel_is_pinned_in_the_bytes() {
    // The wire value is part of the format; a change to it must fail here
    // rather than only somewhere that happens to round-trip.
    let mut library = manifest();
    library.entry = None;
    let bytes = library.to_bytes();
    let at = bytes
        .windows(4)
        .position(|window| window == [0xff, 0xff, 0xff, 0xff])
        .expect("the sentinel appears in the encoding");
    assert_eq!(NO_ENTRYPOINT, u32::MAX);
    // It sits where the entry field is: right after the three strings.
    assert!(at > 4, "the sentinel follows the magic and the paths");
}

#[test]
fn an_entrypoint_naming_no_function_is_rejected() {
    let mut broken = manifest();
    broken.entry = Some(7);
    assert_eq!(
        HybridManifest::from_bytes(&broken.to_bytes()),
        Err(ManifestDecodeError::EntryOutOfRange { entry: 7, count: 2 })
    );
}
