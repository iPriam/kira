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
//! cases (`MetalCGRect` is four `Float`s, an AArch64 HFA returned in `v0`–`v3`).
//!
//! # Only an aggregate result takes an out-pointer
//!
//! A shim returning an aggregate returns `void` and writes through a leading
//! `kira_out` pointer, which is how the caller presents the result buffer and
//! why nothing crosses ownership. A shim returning a scalar returns it
//! normally: a scalar has no by-value ABI question to delegate, and returning it
//! directly means the adapter's existing result path handles a shim call and a
//! direct C call identically.
//!
//! # The other direction: a callback C enters with a struct
//!
//! The same question arrives reversed when C calls *into* Kira. A callback whose
//! parameter is a struct by value — `WGPURequestAdapterCallback` taking a
//! `WGPUStringView` is the case that forces it — is entered by C with an ABI
//! only C knows, so the address C holds cannot be an LLVM function either. It is
//! a generated entry here instead, taking the struct by value and handing the
//! thunk its address:
//!
//! ```c
//! extern int64_t kira_ffi_callback_body_0(int32_t, const struct kira_ffi_agg_0 *);
//! int64_t kira_ffi_callback_0(int32_t p0, struct kira_ffi_agg_0 p1) {
//!     return kira_ffi_callback_body_0(p0, &p1);
//! }
//! ```
//!
//! `p1` is the callee's own copy, so its address is good for exactly the call —
//! which is the same lifetime a `const char*` callback argument has, and the
//! reason the Kira side reads members through the pointer rather than storing
//! it.

use std::collections::BTreeSet;

use kira_runtime_abi::{
    ForeignAggregateId, ForeignAggregates, ForeignArrayElement, ForeignCallback, ForeignImport,
    ForeignMember, ForeignSignature, ForeignType, ForeignTypeSpec,
};

use crate::{callback_body_name, callback_name};

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

/// Whether callback `signature` needs a C entry of its own.
///
/// Only a by-value struct parameter does. The result never can be one — a
/// callback returning an aggregate is refused at the declaration site, because
/// there is nothing on the Kira side to build the C bytes out of — and a
/// scalar-only signature is one LLVM presents to C directly, which is what keeps
/// every callback program that has ever worked from gaining a clang subprocess.
pub fn callback_needs_entry(signature: &ForeignSignature) -> bool {
    signature
        .parameters()
        .iter()
        .any(|spec| spec.aggregate().is_some())
}

/// Emits one callback's C entry: the true prototype C calls, forwarding each
/// by-value struct as its address.
fn write_callback_entry(out: &mut String, index: usize, signature: &ForeignSignature) {
    let result = spec_c_type(signature.result());
    let separator = if result.ends_with('*') { "" } else { " " };

    let declared = signature
        .parameters()
        .iter()
        .enumerate()
        .map(|(position, spec)| {
            let ty = spec_c_type(*spec);
            match spec {
                ForeignTypeSpec::Aggregate(_) => format!("const {ty} *p{position}"),
                ForeignTypeSpec::Scalar(_) => {
                    let space = if ty.ends_with('*') { "" } else { " " };
                    format!("{ty}{space}p{position}")
                }
            }
        })
        .collect::<Vec<_>>();
    let entered = signature
        .parameters()
        .iter()
        .enumerate()
        .map(|(position, spec)| {
            let ty = spec_c_type(*spec);
            let space = if ty.ends_with('*') { "" } else { " " };
            // This is the C-facing prototype. Keep aggregates by value so the
            // C compiler, rather than LLVM, classifies every callback argument
            // for the selected target.
            format!("{ty}{space}p{position}")
        })
        .collect::<Vec<_>>();
    let arguments = signature
        .parameters()
        .iter()
        .enumerate()
        .map(|(position, spec)| match spec {
            // The entry's own copy, whose address is good for this call — which
            // is the whole lifetime a callback argument has.
            ForeignTypeSpec::Aggregate(_) => format!("&p{position}"),
            ForeignTypeSpec::Scalar(_) => format!("p{position}"),
        })
        .collect::<Vec<_>>()
        .join(", ");

    let list = |parts: &[String]| {
        if parts.is_empty() {
            "void".to_owned()
        } else {
            parts.join(", ")
        }
    };
    out.push_str(&format!(
        "extern {result}{separator}{}({});\n",
        callback_body_name(index),
        list(&declared)
    ));
    let call = format!("{}({arguments})", callback_body_name(index));
    let body = match signature.result() {
        ForeignTypeSpec::Scalar(ForeignType::Void) => format!("    {call};\n"),
        _ => format!("    return {call};\n"),
    };
    out.push_str(&format!(
        "{result}{separator}{}({}) {{\n{body}}}\n",
        callback_name(index),
        list(&entered)
    ));
}

/// Generates the whole shim translation unit for a program's foreign seam.
///
/// Only imports whose signature names an aggregate get a shim, and only
/// callbacks C enters with one get an entry: everything else reaches its C
/// symbol, or is reached by C, directly. Returns `None` when nothing needs
/// either — every program that does not pass a struct by value in some
/// direction, and those never invoke clang at all.
///
/// An import listed in `unavailable` is skipped. Codegen already replaces such
/// an import's adapter with a trap, because the library it names has no
/// artifact on this target — but a shim is C, compiled separately, and one
/// written for a missing library calls the very symbol nothing defines. That is
/// how a Windows build of a package with a Metal backend failed on
/// `objc_msgSend`: the adapter was correctly a trap, and the shim beside it
/// still named Apple's runtime.
pub fn generate(
    imports: &[ForeignImport],
    callbacks: &[ForeignCallback],
    table: &ForeignAggregates,
    unavailable: &[usize],
) -> Option<String> {
    let needed: Vec<(usize, &ForeignImport)> = imports
        .iter()
        .enumerate()
        .filter(|(index, import)| {
            import.signature().has_aggregate() && !unavailable.contains(index)
        })
        .collect();
    // A callback is never unavailable the way an import is: it names a Kira
    // function this build compiles, not a symbol some target's library may lack.
    let entries: Vec<(usize, &ForeignCallback)> = callbacks
        .iter()
        .enumerate()
        .filter(|(_, callback)| callback_needs_entry(callback.signature()))
        .collect();
    if needed.is_empty() && entries.is_empty() {
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
    for (index, callback) in entries {
        write_callback_entry(&mut out, index, callback.signature());
    }
    Some(out)
}
