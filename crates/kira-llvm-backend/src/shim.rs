//! Generating the C translation unit that carries C-layout aggregates across
//! the seam.
//!
//! # Why a C file, and not a classifier
//!
//! Passing a struct by value is the one place the C ABI cannot be derived from
//! the type alone: x86-64 System V classifies eightbytes, AArch64 AAPCS detects
//! homogeneous float aggregates and returns large ones indirectly, and wasm32
//! has its own rules. Writing that classifier here would mean shipping two
//! thirds of it asserted, because one machine can differential-test one
//! architecture.
//!
//! So Kira does not classify. For each `@FFI.Extern` whose signature names an
//! aggregate, this module emits a small C file that redeclares the aggregate
//! structurally, redeclares the real symbol with its true by-value signature,
//! and wraps the call in a shim taking every aggregate through a pointer:
//!
//! ```c
//! struct kira_ffi_agg_0 { double f0; double f1; };
//! extern double rect_sum(struct kira_ffi_agg_0);
//! double kira_ffi_shim_0(const struct kira_ffi_agg_0 *p0) {
//!     return rect_sum(*p0);
//! }
//! ```
//!
//! The managed clang compiles it, and everything Kira emits — the generated
//! adapter, the native call site, the VM host — speaks only pointers and
//! scalars, which the seam already handles exactly. `byval`/`sret` would not
//! substitute: both force the memory class, and the corpus needs the register
//! cases (`MetalCGRect` is four `F64`s, an AArch64 HFA returned in `v0`–`v3`).
//!
//! # Only an aggregate result takes an out-pointer
//!
//! A shim returning an aggregate returns `void` and writes through a leading
//! `kira_out` pointer, which is how the caller presents the result buffer and
//! why nothing crosses ownership. A shim returning a scalar returns it
//! normally: a scalar has no by-value ABI question to delegate, and returning it
//! directly means the adapter's existing result path handles a shim call and a
//! direct C call identically.

use std::collections::BTreeSet;

use kira_runtime_abi::{
    ForeignAggregateId, ForeignAggregates, ForeignArrayElement, ForeignImport, ForeignMember,
    ForeignSignature, ForeignType, ForeignTypeSpec,
};

/// The C name of the aggregate at `id` in the generated translation unit.
///
/// Structural, not the C tag name from the original header: the header's name
/// is not part of the contract, and two headers may spell the same layout
/// differently.
pub fn aggregate_name(id: ForeignAggregateId) -> String {
    format!("kira_ffi_agg_{}", id.0)
}

/// The C name of the shim wrapping the import at `index`.
pub fn shim_name(index: usize) -> String {
    format!("kira_ffi_shim_{index}")
}

/// The C spelling of a seam scalar.
///
/// Fixed-width integers go through `<stdint.h>` rather than `int`/`long long`,
/// so the emitted declaration means the same width the seam promised on every
/// target rather than the same width it happens to have here.
fn scalar_c_type(ty: ForeignType) -> &'static str {
    match ty {
        ForeignType::Void => "void",
        ForeignType::Bool => "_Bool",
        ForeignType::I8 => "int8_t",
        ForeignType::U8 => "uint8_t",
        ForeignType::I16 => "int16_t",
        ForeignType::U16 => "uint16_t",
        ForeignType::I32 => "int32_t",
        ForeignType::U32 => "uint32_t",
        ForeignType::I64 => "int64_t",
        ForeignType::U64 => "uint64_t",
        ForeignType::F32 => "float",
        ForeignType::F64 => "double",
        ForeignType::RawPtr => "void *",
        ForeignType::CString => "const char *",
    }
}

/// The C spelling of one signature position, as written by value.
fn spec_c_type(spec: ForeignTypeSpec) -> String {
    match spec {
        ForeignTypeSpec::Scalar(ty) => scalar_c_type(ty).to_owned(),
        ForeignTypeSpec::Aggregate(id) => format!("struct {}", aggregate_name(id)),
    }
}

/// Emits the `struct` definitions for every aggregate in `table`, in table
/// order.
///
/// Table order is definition order by construction: a member's id is always
/// lower than its container's, so a nested struct is complete before the struct
/// embedding it names it — which C requires for a by-value member.
fn write_aggregates(out: &mut String, table: &ForeignAggregates) {
    for (index, aggregate) in table.iter().enumerate() {
        let name = aggregate_name(ForeignAggregateId(index as u32));
        out.push_str(&format!("struct {name} {{"));
        if aggregate.members().is_empty() {
            // A C struct may not be empty. Kira gives an empty aggregate size 1,
            // which is what a compiler's own empty-struct extension produces, so
            // one `char` reproduces the layout the seam computed.
            out.push_str(" char kira_empty;");
        }
        for (field, member) in aggregate.members().iter().enumerate() {
            let (ty, extent) = match member {
                ForeignMember::Scalar(ty) => (scalar_c_type(*ty).to_owned(), String::new()),
                ForeignMember::Aggregate(id) => {
                    (format!("struct {}", aggregate_name(*id)), String::new())
                }
                // An inline array is written as a C array member, so the C
                // compiler sizes and aligns it — and classifies the containing
                // struct with it — exactly as the header does.
                ForeignMember::Array { element, count } => {
                    let ty = match element {
                        ForeignArrayElement::Scalar(ty) => scalar_c_type(*ty).to_owned(),
                        ForeignArrayElement::Aggregate(id) => {
                            format!("struct {}", aggregate_name(*id))
                        }
                    };
                    (ty, format!("[{count}]"))
                }
            };
            // `void *f0` and `int32_t f0` differ in where the space goes; the
            // pointer spellings already carry their own trailing space.
            let separator = if ty.ends_with('*') { "" } else { " " };
            out.push_str(&format!(" {ty}{separator}f{field}{extent};"));
        }
        out.push_str(" };\n");
    }
}

/// The parameter list of the real C symbol, by value.
fn declared_parameters(signature: &ForeignSignature) -> String {
    if signature.parameters().is_empty() {
        return "void".to_owned();
    }
    signature
        .parameters()
        .iter()
        .map(|spec| spec_c_type(*spec))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Emits one import's shim: the real symbol's declaration and the wrapper.
///
/// `declared` records the symbols already given an `extern` declaration, so a
/// symbol imported at more than one signature is declared once. `objc_msgSend`
/// is the case that forces this — Objective-C dispatches every message through
/// that one symbol, and Apple's own documentation says to cast it to the
/// prototype of the message being sent. Two `extern` declarations of it with
/// different parameter lists is a C error, so the declaration is emitted once
/// and each call goes through a cast to its own signature.
fn write_shim(
    out: &mut String,
    index: usize,
    import: &ForeignImport,
    declared: &mut BTreeSet<String>,
) {
    let signature = import.signature();
    let symbol = import.symbol();
    let result = spec_c_type(signature.result());

    if declared.insert(symbol.to_owned()) {
        out.push_str(&format!(
            "extern {result} {symbol}({});\n",
            declared_parameters(signature)
        ));
    }
    // The cast is what makes a second signature for the same symbol legal, and
    // it is a no-op when the symbol has only one.
    let separator_for_cast = if result.ends_with('*') { "" } else { " " };
    out.push_str(&format!(
        "typedef {result}{separator_for_cast}(*{})({});\n",
        callee_type(index),
        declared_parameters(signature)
    ));

    // The out-pointer first when the result is an aggregate, then one pointer
    // per aggregate parameter and the scalars as themselves.
    let mut parameters = Vec::with_capacity(signature.parameters().len() + 1);
    let out_type = signature
        .result()
        .aggregate()
        .map(|id| format!("struct {}", aggregate_name(id)));
    if let Some(ty) = &out_type {
        parameters.push(format!("{ty} *kira_out"));
    }
    for (position, spec) in signature.parameters().iter().enumerate() {
        let ty = spec_c_type(*spec);
        let separator = if ty.ends_with('*') { "" } else { " " };
        match spec {
            ForeignTypeSpec::Aggregate(_) => {
                parameters.push(format!("const {ty} *p{position}"));
            }
            ForeignTypeSpec::Scalar(_) => parameters.push(format!("{ty}{separator}p{position}")),
        }
    }
    let parameters = if parameters.is_empty() {
        "void".to_owned()
    } else {
        parameters.join(", ")
    };

    let arguments = signature
        .parameters()
        .iter()
        .enumerate()
        .map(|(position, spec)| match spec {
            // Dereferenced at the call, so the C compiler sees a by-value
            // argument and applies the ABI it alone knows.
            ForeignTypeSpec::Aggregate(_) => format!("*p{position}"),
            ForeignTypeSpec::Scalar(_) => format!("p{position}"),
        })
        .collect::<Vec<_>>()
        .join(", ");

    let call = format!(
        "(({})(void (*)(void)){symbol})({arguments})",
        callee_type(index)
    );
    let (returns, body) = match (&out_type, signature.result()) {
        (Some(_), _) => ("void".to_owned(), format!("    *kira_out = {call};\n")),
        (None, ForeignTypeSpec::Scalar(ForeignType::Void)) => {
            ("void".to_owned(), format!("    {call};\n"))
        }
        (None, scalar) => (spec_c_type(scalar), format!("    return {call};\n")),
    };
    let separator = if returns.ends_with('*') { "" } else { " " };
    out.push_str(&format!(
        "{returns}{separator}{}({parameters}) {{\n{body}}}\n",
        shim_name(index)
    ));
}

/// The typedef name one import's callee type is written as.
fn callee_type(index: usize) -> String {
    format!("kira_ffi_callee_{index}")
}

/// Generates the whole shim translation unit for a program's foreign imports.
///
/// Only imports whose signature names an aggregate get a shim: a scalar-only
/// import reaches its C symbol directly, exactly as before. Returns `None` when
/// no import needs one, which is every program that does not pass a struct by
/// value — those never invoke clang at all.
pub fn generate(imports: &[ForeignImport], table: &ForeignAggregates) -> Option<String> {
    let needed: Vec<(usize, &ForeignImport)> = imports
        .iter()
        .enumerate()
        .filter(|(_, import)| import.signature().has_aggregate())
        .collect();
    if needed.is_empty() {
        return None;
    }

    let mut out = String::new();
    out.push_str("/* Generated by kira: C-layout aggregates at the foreign seam.\n");
    out.push_str("   The C compiler applies the by-value ABI; Kira never classifies. */\n");
    out.push_str("#include <stdint.h>\n\n");
    write_aggregates(&mut out, table);
    out.push('\n');
    let mut declared = BTreeSet::new();
    for (index, import) in needed {
        write_shim(&mut out, index, import, &mut declared);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_runtime_abi::{ForeignAbi, ForeignAggregate};

    fn import(symbol: &str, signature: ForeignSignature) -> ForeignImport {
        ForeignImport::new("fixture", symbol, ForeignAbi::C, signature)
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

    #[test]
    fn a_scalar_only_program_generates_nothing() {
        let imports = [import(
            "plain",
            ForeignSignature::scalars([ForeignType::I32], ForeignType::I32),
        )];
        assert_eq!(generate(&imports, &ForeignAggregates::new()), None);
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
        let text = generate(&imports, &table).expect("a shim");
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
        let text = generate(&imports, &table).expect("a shim");
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
        let text = generate(&imports, &table).expect("a shim");
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
        let text = generate(&imports, &table).expect("a shim");
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
        let text = generate(&imports, &table).expect("a shim");
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
        let text = generate(&imports, &table).expect("a shim");
        // The shim is named for the import's index, so the scalar-only import at
        // 0 is skipped and the aggregate one keeps its own index.
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
        let text = generate(&imports, &table).expect("a shim");
        assert!(
            text.contains("struct kira_ffi_agg_0 { char kira_empty; };"),
            "{text}"
        );
    }

    /// Compiles `text` as C with the managed clang, returning the driver's
    /// diagnostics when it refuses.
    ///
    /// Generated C that this crate never compiles is generated C that is only
    /// asserted to be C. Every shape below goes through the real compiler.
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
        let text = generate(&imports, &table).expect("a shim");
        if let Err(stderr) = compiles(&text) {
            panic!("the generated unit is not valid C:\n{stderr}\n--- unit ---\n{text}");
        }
    }

    #[test]
    fn kiras_computed_layout_is_the_layout_clang_gives_the_generated_struct() {
        // The whole design rests on the generated C struct having the layout
        // Kira computed for it: Kira sizes the marshalling buffer, clang lays
        // out the struct the real symbol receives. A `_Static_assert` per shape
        // makes a disagreement a compile error rather than a wrong answer at
        // run time.
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
        // Inline arrays of both element shapes: the size of one is the extent
        // times the element, and the alignment stays the element's own.
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

        // The host is 64-bit; this test does not speak for wasm32, whose
        // pointer width the parity suite covers on its own target.
        let width = kira_runtime_abi::ForeignPointerWidth::Bits64;
        let layouts = table.layouts(width).expect("layouts");
        let mut text = generate(&imports, &table).expect("a shim");
        for (index, layout) in layouts.iter().enumerate() {
            let name = aggregate_name(ForeignAggregateId(index as u32));
            text.push_str(&format!(
                "_Static_assert(sizeof(struct {name}) == {}, \"size of {name}\");\n\
                 _Static_assert(_Alignof(struct {name}) == {}, \"align of {name}\");\n",
                layout.size, layout.align,
            ));
        }
        // And every scalar leaf at the offset Kira will write it to, checked
        // against clang's own `offsetof`. Only flat aggregates take part: their
        // leaves are exactly their top-level fields in order, so leaf `n` is
        // `fn` and can be named. A nested aggregate's leaves have paths this
        // table does not carry, and inventing one to name would test the
        // invention rather than the layout.
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
        let text = generate(&imports, &table).expect("a shim");
        // Inline storage, so the extent rides on the member name — a pointer
        // member would be a different type with different ownership.
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
        let text = generate(&imports, &table).expect("a shim");
        // A pointer type already carries its trailing space, so the out-slot is
        // `void **kira_out` rather than `void * *kira_out`.
        assert!(
            text.contains("void *kira_ffi_shim_0(void *p0, const struct kira_ffi_agg_0 *p1)"),
            "{text}"
        );
        assert!(!text.contains("* *"), "no split pointer stars: {text}");
    }
}
