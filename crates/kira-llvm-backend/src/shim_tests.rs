//! What [`crate::shim`] generates, in both directions across the seam.
//!
//! Every shape here goes through the real compiler as well as through the
//! assertions. Generated C that this crate never compiles is generated C that is
//! only *asserted* to be C, and the whole design rests on it being the thing a C
//! compiler agrees with.

use super::shim::*;
use kira_runtime_abi::{
    ForeignAbi, ForeignAggregate, ForeignAggregateId, ForeignAggregates, ForeignArrayElement,
    ForeignCallback, ForeignImport, ForeignMember, ForeignSignature, ForeignType, ForeignTypeSpec,
};

fn import(symbol: &str, signature: ForeignSignature) -> ForeignImport {
    ForeignImport::new("fixture", symbol, ForeignAbi::C, signature)
}

/// The unit for a program whose only aggregates are at its imports.
///
/// Shadows [`super::shim::generate`] so the import cases below read as what they
/// are about; the callback direction passes its own rows.
fn generate(
    imports: &[ForeignImport],
    table: &ForeignAggregates,
    unavailable: &[usize],
) -> Option<String> {
    super::shim::generate(imports, &[], table, unavailable)
}

/// A table holding `struct { double; double }` at id 0.
fn point_table() -> (ForeignAggregates, ForeignAggregateId) {
    let mut table = ForeignAggregates::new();
    let id = table
        .push(ForeignAggregate::new(vec![
            ForeignMember::Scalar(ForeignType::F64),
            ForeignMember::Scalar(ForeignType::F64),
        ]))
        .expect("pushes");
    (table, id)
}

/// Compiles `text` as C with the managed clang, returning the driver's
/// diagnostics when it refuses.
fn compiles(text: &str) -> Result<(), String> {
    let llvm = kira_toolchain::discover(None).expect("the managed LLVM is present");
    let dir = std::env::temp_dir().join(format!(
        "kira-shim-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let source = dir.join("shim.c");
    std::fs::write(&source, text).expect("the generated unit");
    let output = std::process::Command::new(llvm.clang())
        .arg("-c")
        .arg("-Werror")
        .arg(&source)
        .arg("-o")
        .arg(dir.join("shim.o"))
        .output()
        .expect("clang runs");
    let _ = std::fs::remove_dir_all(&dir);
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

#[test]
fn a_scalar_only_program_generates_nothing() {
    let imports = [import(
        "plain",
        ForeignSignature::scalars([ForeignType::I32], ForeignType::I32),
    )];
    assert_eq!(generate(&imports, &ForeignAggregates::new(), &[]), None);
}

/// An import whose library this target does not have gets no shim.
///
/// Codegen already turns such an import's adapter into a trap. The shim is
/// separate C, and one written for a missing library declares and calls the
/// symbol nothing defines — which is how a Windows build of a package carrying a
/// Metal backend failed to link on `objc_msgSend`, with the adapter beside it
/// correctly trapping.
#[test]
fn an_unavailable_import_is_left_out_of_the_shim() {
    let (table, id) = point_table();
    let imports = [
        import(
            "rect_sum",
            ForeignSignature::new(
                vec![ForeignTypeSpec::Aggregate(id)],
                ForeignTypeSpec::Scalar(ForeignType::F64),
            ),
        ),
        import(
            "objc_msgSend",
            ForeignSignature::new(
                vec![ForeignTypeSpec::Aggregate(id)],
                ForeignTypeSpec::Scalar(ForeignType::F64),
            ),
        ),
    ];

    let text = generate(&imports, &table, &[1]).expect("the available import still needs one");
    assert!(text.contains("rect_sum"), "{text}");
    assert!(
        !text.contains("objc_msgSend"),
        "a symbol this target does not have must not be declared or called: {text}"
    );

    // And when every aggregate-passing import is unavailable there is no shim at
    // all, so no clang subprocess runs for a file of nothing.
    assert_eq!(generate(&imports, &table, &[0, 1]), None);
}

#[test]
fn an_aggregate_parameter_is_passed_through_a_pointer_and_dereferenced() {
    let (table, id) = point_table();
    let imports = [import(
        "rect_sum",
        ForeignSignature::new(
            vec![ForeignTypeSpec::Aggregate(id)],
            ForeignTypeSpec::Scalar(ForeignType::F64),
        ),
    )];
    let text = generate(&imports, &table, &[]).expect("a shim");
    assert!(
        text.contains("struct kira_ffi_agg_0 { double f0; double f1; };"),
        "{text}"
    );
    // The real symbol keeps its true by-value signature, so the C compiler
    // applies the ABI to the call inside the shim.
    assert!(
        text.contains("extern double rect_sum(struct kira_ffi_agg_0);"),
        "{text}"
    );
    assert!(
        text.contains("double kira_ffi_shim_0(const struct kira_ffi_agg_0 *p0) {"),
        "{text}"
    );
    assert!(
        text.contains("return ((kira_ffi_callee_0)(void (*)(void))rect_sum)(*p0);"),
        "{text}"
    );
}

#[test]
fn an_aggregate_result_is_written_through_the_out_pointer() {
    let (table, id) = point_table();
    let imports = [import(
        "make_point",
        ForeignSignature::new(
            vec![ForeignTypeSpec::Scalar(ForeignType::F64)],
            ForeignTypeSpec::Aggregate(id),
        ),
    )];
    let text = generate(&imports, &table, &[]).expect("a shim");
    assert!(
        text.contains("extern struct kira_ffi_agg_0 make_point(double);"),
        "{text}"
    );
    assert!(
        text.contains("void kira_ffi_shim_0(struct kira_ffi_agg_0 *kira_out, double p0) {"),
        "{text}"
    );
    assert!(
        text.contains("*kira_out = ((kira_ffi_callee_0)(void (*)(void))make_point)(p0);"),
        "{text}"
    );
}

#[test]
fn one_symbol_imported_at_two_signatures_is_declared_once() {
    // `objc_msgSend` is why this matters: Objective-C sends every message
    // through that one symbol, and two `extern` declarations of it with
    // different parameter lists do not compile. Each call casts instead.
    let (table, id) = point_table();
    let imports = [
        import(
            "objc_msgSend",
            ForeignSignature::new(
                vec![ForeignTypeSpec::Scalar(ForeignType::I64)],
                ForeignTypeSpec::Aggregate(id),
            ),
        ),
        import(
            "objc_msgSend",
            ForeignSignature::new(
                vec![
                    ForeignTypeSpec::Scalar(ForeignType::I64),
                    ForeignTypeSpec::Scalar(ForeignType::F64),
                ],
                ForeignTypeSpec::Aggregate(id),
            ),
        ),
    ];
    let text = generate(&imports, &table, &[]).expect("a shim");
    assert_eq!(
        text.matches("extern struct kira_ffi_agg_0 objc_msgSend(")
            .count(),
        1,
        "declared more than once: {text}"
    );
    assert!(
        text.contains("((kira_ffi_callee_0)(void (*)(void))objc_msgSend)(p0)"),
        "{text}"
    );
    assert!(
        text.contains("((kira_ffi_callee_1)(void (*)(void))objc_msgSend)(p0, p1)"),
        "{text}"
    );
}

#[test]
fn a_void_result_has_no_out_pointer() {
    let (table, id) = point_table();
    let imports = [import(
        "consume",
        ForeignSignature::new(
            vec![ForeignTypeSpec::Aggregate(id)],
            ForeignTypeSpec::Scalar(ForeignType::Void),
        ),
    )];
    let text = generate(&imports, &table, &[]).expect("a shim");
    assert!(
        text.contains("void kira_ffi_shim_0(const struct kira_ffi_agg_0 *p0) {"),
        "{text}"
    );
    assert!(
        text.contains("((kira_ffi_callee_0)(void (*)(void))consume)(*p0);"),
        "{text}"
    );
    assert!(!text.contains("kira_out"), "{text}");
}

#[test]
fn a_nested_aggregate_is_defined_before_the_struct_embedding_it() {
    let mut table = ForeignAggregates::new();
    let inner = table
        .push(ForeignAggregate::new(vec![
            ForeignMember::Scalar(ForeignType::F64),
            ForeignMember::Scalar(ForeignType::F64),
        ]))
        .expect("pushes");
    let outer = table
        .push(ForeignAggregate::new(vec![
            ForeignMember::Aggregate(inner),
            ForeignMember::Scalar(ForeignType::F64),
        ]))
        .expect("pushes");
    let imports = [import(
        "frame_area",
        ForeignSignature::new(
            vec![ForeignTypeSpec::Aggregate(outer)],
            ForeignTypeSpec::Scalar(ForeignType::F64),
        ),
    )];
    let text = generate(&imports, &table, &[]).expect("a shim");
    let inner_at = text.find("struct kira_ffi_agg_0 {").expect("inner");
    let outer_at = text.find("struct kira_ffi_agg_1 {").expect("outer");
    assert!(
        inner_at < outer_at,
        "a by-value member must be complete first"
    );
    assert!(
        text.contains("struct kira_ffi_agg_1 { struct kira_ffi_agg_0 f0; double f1; };"),
        "{text}"
    );
}

#[test]
fn only_the_imports_that_need_a_shim_get_one() {
    let (table, id) = point_table();
    let imports = [
        import(
            "plain",
            ForeignSignature::scalars([ForeignType::I32], ForeignType::I32),
        ),
        import(
            "rect_sum",
            ForeignSignature::new(
                vec![ForeignTypeSpec::Aggregate(id)],
                ForeignTypeSpec::Scalar(ForeignType::F64),
            ),
        ),
    ];
    let text = generate(&imports, &table, &[]).expect("a shim");
    // The shim is named for the import's index, so the scalar-only import at 0
    // is skipped and the aggregate one keeps its own index.
    assert!(text.contains("kira_ffi_shim_1("), "{text}");
    assert!(!text.contains("kira_ffi_shim_0("), "{text}");
    assert!(!text.contains("plain"), "{text}");
}

#[test]
fn an_empty_aggregate_gets_the_one_byte_c_stand_in() {
    let mut table = ForeignAggregates::new();
    let id = table
        .push(ForeignAggregate::new(Vec::new()))
        .expect("pushes");
    let imports = [import(
        "take_empty",
        ForeignSignature::new(
            vec![ForeignTypeSpec::Aggregate(id)],
            ForeignTypeSpec::Scalar(ForeignType::Void),
        ),
    )];
    let text = generate(&imports, &table, &[]).expect("a shim");
    assert!(
        text.contains("struct kira_ffi_agg_0 { char kira_empty; };"),
        "{text}"
    );
}

#[test]
fn every_generated_shape_compiles_as_real_c() {
    let mut table = ForeignAggregates::new();
    let point = table
        .push(ForeignAggregate::new(vec![
            ForeignMember::Scalar(ForeignType::F64),
            ForeignMember::Scalar(ForeignType::F64),
        ]))
        .expect("pushes");
    let nested = table
        .push(ForeignAggregate::new(vec![
            ForeignMember::Aggregate(point),
            ForeignMember::Scalar(ForeignType::I8),
            ForeignMember::Scalar(ForeignType::RawPtr),
        ]))
        .expect("pushes");
    let empty = table
        .push(ForeignAggregate::new(Vec::new()))
        .expect("pushes");

    let imports = [
        import(
            "takes_point",
            ForeignSignature::new(
                vec![ForeignTypeSpec::Aggregate(point)],
                ForeignTypeSpec::Scalar(ForeignType::F64),
            ),
        ),
        import(
            "returns_nested",
            ForeignSignature::new(
                vec![ForeignTypeSpec::Scalar(ForeignType::I32)],
                ForeignTypeSpec::Aggregate(nested),
            ),
        ),
        import(
            "consumes_empty",
            ForeignSignature::new(
                vec![ForeignTypeSpec::Aggregate(empty)],
                ForeignTypeSpec::Scalar(ForeignType::Void),
            ),
        ),
        import(
            "mixed",
            ForeignSignature::new(
                vec![
                    ForeignTypeSpec::Scalar(ForeignType::CString),
                    ForeignTypeSpec::Aggregate(point),
                    ForeignTypeSpec::Scalar(ForeignType::Bool),
                ],
                ForeignTypeSpec::Aggregate(point),
            ),
        ),
    ];
    let text = generate(&imports, &table, &[]).expect("a shim");
    if let Err(stderr) = compiles(&text) {
        panic!("the generated unit is not valid C:\n{stderr}\n--- unit ---\n{text}");
    }
}

#[test]
fn kiras_computed_layout_is_the_layout_clang_gives_the_generated_struct() {
    // The whole design rests on the generated C struct having the layout Kira
    // computed for it: Kira sizes the marshalling buffer, clang lays out the
    // struct the real symbol receives. A `_Static_assert` per shape makes a
    // disagreement a compile error rather than a wrong answer at run time.
    let mut table = ForeignAggregates::new();
    let point = table
        .push(ForeignAggregate::new(vec![
            ForeignMember::Scalar(ForeignType::F64),
            ForeignMember::Scalar(ForeignType::F64),
        ]))
        .expect("pushes");
    // `struct { char; double; char }` — the padding case.
    let padded = table
        .push(ForeignAggregate::new(vec![
            ForeignMember::Scalar(ForeignType::I8),
            ForeignMember::Scalar(ForeignType::F64),
            ForeignMember::Scalar(ForeignType::I8),
        ]))
        .expect("pushes");
    // A nested aggregate contributing its own alignment.
    let nested = table
        .push(ForeignAggregate::new(vec![
            ForeignMember::Scalar(ForeignType::I16),
            ForeignMember::Aggregate(padded),
            ForeignMember::Scalar(ForeignType::U32),
        ]))
        .expect("pushes");
    // Inline arrays of both element shapes: the size of one is the extent times
    // the element, and the alignment stays the element's own.
    let cells = table
        .push(ForeignAggregate::new(vec![
            ForeignMember::Array {
                element: ForeignArrayElement::Scalar(ForeignType::I32),
                count: 3,
            },
            ForeignMember::Scalar(ForeignType::I8),
        ]))
        .expect("pushes");
    let slots = table
        .push(ForeignAggregate::new(vec![ForeignMember::Array {
            element: ForeignArrayElement::Aggregate(point),
            count: 2,
        }]))
        .expect("pushes");
    let imports = [import(
        "anchor",
        ForeignSignature::new(
            vec![
                ForeignTypeSpec::Aggregate(point),
                ForeignTypeSpec::Aggregate(nested),
                ForeignTypeSpec::Aggregate(cells),
                ForeignTypeSpec::Aggregate(slots),
            ],
            ForeignTypeSpec::Scalar(ForeignType::Void),
        ),
    )];

    // The host is 64-bit; this test does not speak for wasm32, whose pointer
    // width the parity suite covers on its own target.
    let width = kira_runtime_abi::ForeignPointerWidth::Bits64;
    let layouts = table.layouts(width).expect("layouts");
    let mut text = generate(&imports, &table, &[]).expect("a shim");
    for (index, layout) in layouts.iter().enumerate() {
        let name = aggregate_name(ForeignAggregateId(index as u32));
        text.push_str(&format!(
            "_Static_assert(sizeof(struct {name}) == {}, \"size of {name}\");\n\
             _Static_assert(_Alignof(struct {name}) == {}, \"align of {name}\");\n",
            layout.size, layout.align,
        ));
    }
    // And every scalar leaf at the offset Kira will write it to, checked against
    // clang's own `offsetof`. Only flat aggregates take part: their leaves are
    // exactly their top-level fields in order, so leaf `n` is `fn` and can be
    // named. A nested aggregate's leaves have paths this table does not carry,
    // and inventing one to name would test the invention rather than the layout.
    text.insert_str(0, "#include <stddef.h>\n");
    for index in 0..table.len() {
        let id = ForeignAggregateId(index as u32);
        let name = aggregate_name(id);
        let flat = table
            .get(id)
            .expect("the row")
            .members()
            .iter()
            .all(|member| matches!(member, ForeignMember::Scalar(_)));
        if !flat {
            continue;
        }
        for (field, leaf) in table
            .leaves_of(id, width)
            .expect("leaves")
            .iter()
            .enumerate()
        {
            text.push_str(&format!(
                "_Static_assert(offsetof(struct {name}, f{field}) == {}, \
                 \"offset of {name}.f{field}\");\n",
                leaf.offset,
            ));
        }
    }
    if let Err(stderr) = compiles(&text) {
        panic!("clang disagrees with Kira's computed layout:\n{stderr}");
    }
}

#[test]
fn an_inline_array_member_is_written_as_a_c_array() {
    let mut table = ForeignAggregates::new();
    let point = table
        .push(ForeignAggregate::new(vec![
            ForeignMember::Scalar(ForeignType::F64),
            ForeignMember::Scalar(ForeignType::F64),
        ]))
        .expect("pushes");
    let id = table
        .push(ForeignAggregate::new(vec![
            ForeignMember::Array {
                element: ForeignArrayElement::Scalar(ForeignType::I32),
                count: 4,
            },
            ForeignMember::Array {
                element: ForeignArrayElement::Aggregate(point),
                count: 2,
            },
        ]))
        .expect("pushes");
    let imports = [import(
        "takes_grid",
        ForeignSignature::new(
            vec![ForeignTypeSpec::Aggregate(id)],
            ForeignTypeSpec::Scalar(ForeignType::Void),
        ),
    )];
    let text = generate(&imports, &table, &[]).expect("a shim");
    // Inline storage, so the extent rides on the member name — a pointer member
    // would be a different type with different ownership.
    assert!(
        text.contains("struct kira_ffi_agg_1 { int32_t f0[4]; struct kira_ffi_agg_0 f1[2]; };"),
        "{text}"
    );
    if let Err(stderr) = compiles(&text) {
        panic!("the generated unit is not valid C:\n{stderr}\n--- unit ---\n{text}");
    }
}

#[test]
fn a_pointer_position_spells_without_a_doubled_space() {
    let (table, id) = point_table();
    let imports = [import(
        "with_ptr",
        ForeignSignature::new(
            vec![
                ForeignTypeSpec::Scalar(ForeignType::RawPtr),
                ForeignTypeSpec::Aggregate(id),
            ],
            ForeignTypeSpec::Scalar(ForeignType::RawPtr),
        ),
    )];
    let text = generate(&imports, &table, &[]).expect("a shim");
    // A pointer type already carries its trailing space, so the out-slot is
    // `void **kira_out` rather than `void * *kira_out`.
    assert!(
        text.contains("void *kira_ffi_shim_0(void *p0, const struct kira_ffi_agg_0 *p1)"),
        "{text}"
    );
    assert!(!text.contains("* *"), "no split pointer stars: {text}");
}

/// A callback whose parameter is a struct by value gets the C entry that takes
/// it by value and hands the thunk its address.
///
/// This is the direction Dawn forces: `wgpuInstanceRequestAdapter` is the only
/// route to an adapter, and it answers through a callback whose third parameter
/// is a `WGPUStringView` by value.
#[test]
fn a_callback_taking_a_struct_by_value_gets_a_c_entry() {
    let (table, id) = point_table();
    let callbacks = [ForeignCallback::new(
        3,
        ForeignSignature::new(
            vec![
                ForeignTypeSpec::Scalar(ForeignType::I32),
                ForeignTypeSpec::Aggregate(id),
            ],
            ForeignTypeSpec::Scalar(ForeignType::I64),
        ),
    )];
    let text = super::shim::generate(&[], &callbacks, &table, &[]).expect("an entry");
    assert!(
        text.contains(
            "extern int64_t kira_ffi_callback_body_0(int32_t p0, \
             const struct kira_ffi_agg_0 *p1);"
        ),
        "{text}"
    );
    assert!(
        text.contains("int64_t kira_ffi_callback_0(int32_t p0, struct kira_ffi_agg_0 p1) {"),
        "{text}"
    );
    assert!(
        text.contains("return kira_ffi_callback_body_0(p0, &p1);"),
        "{text}"
    );
    if let Err(stderr) = compiles(&text) {
        panic!("the generated unit is not valid C:\n{stderr}\n--- unit ---\n{text}");
    }
}

/// A `void` callback entry calls rather than returns, and an aggregate in the
/// first position still forwards by address.
#[test]
fn a_void_callback_entry_calls_without_returning() {
    let (table, id) = point_table();
    let callbacks = [ForeignCallback::new(
        0,
        ForeignSignature::new(
            vec![
                ForeignTypeSpec::Aggregate(id),
                ForeignTypeSpec::Scalar(ForeignType::RawPtr),
            ],
            ForeignTypeSpec::Scalar(ForeignType::Void),
        ),
    )];
    let text = super::shim::generate(&[], &callbacks, &table, &[]).expect("an entry");
    assert!(
        text.contains("void kira_ffi_callback_0(struct kira_ffi_agg_0 p0, void *p1) {"),
        "{text}"
    );
    assert!(
        text.contains("    kira_ffi_callback_body_0(&p0, p1);"),
        "{text}"
    );
    assert!(!text.contains("return kira_ffi_callback_body_0"), "{text}");
    if let Err(stderr) = compiles(&text) {
        panic!("the generated unit is not valid C:\n{stderr}\n--- unit ---\n{text}");
    }
}

/// A scalar-only callback keeps the address C holds as an LLVM function, so no
/// clang subprocess is added to a program that never needed one.
#[test]
fn a_scalar_only_callback_generates_nothing() {
    let callbacks = [ForeignCallback::new(
        0,
        ForeignSignature::scalars([ForeignType::I32, ForeignType::I32], ForeignType::I32),
    )];
    assert_eq!(
        super::shim::generate(&[], &callbacks, &ForeignAggregates::new(), &[]),
        None
    );
    assert!(!callback_needs_entry(callbacks[0].signature()));
}
