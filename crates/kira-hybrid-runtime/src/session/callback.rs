//! The callback registry: closures a foreign library calls back through, and
//! the pinned contexts that keep them addressable.
//!
//! A context's *address* is what libffi hands the C entry, so nothing here may
//! move once registered — the constraint that makes this its own module rather
//! than a section of the session's.

use super::*;

/// Owns callback closures and their pinned user-data in one lock domain.
///
/// A context is pinned because its address is what libffi hands the callback
/// entry: it must stay valid until the registry drops, whatever the vector
/// does as it grows.
pub(super) struct CallbackRegistry {
    pub(super) closures: Vec<Option<FfiClosure>>,
    pub(super) contexts: Vec<Pin<Box<CallbackContext>>>,
}

/// The immutable data a libffi closure needs to enter one Kira function.
pub(super) struct CallbackContext {
    pub(super) function_id: u32,
    pub(super) signature: ForeignSignature,
}

/// The C-to-VM entry used by a bundled libffi closure.
pub(super) unsafe extern "C" fn ffi_callback_entry(
    _cif: *mut RawFfiCif,
    result: *mut c_void,
    arguments: *mut *mut c_void,
    user_data: *mut c_void,
) {
    if user_data.is_null() {
        fatal("a libffi callback has no Kira function context");
    }
    // SAFETY: the context is retained until the closure is dropped, and libffi
    // only calls this while that closure is live.
    let context = unsafe { &*(user_data.cast::<CallbackContext>()) };
    let session_pointer = ACTIVE_SESSION.get();
    if session_pointer.is_null() {
        fatal(&format!(
            "a C callback entered Kira function {} without a running program",
            context.function_id
        ));
    }
    // SAFETY: `ActiveSession` installs this pointer for the complete VM run and
    // clears it only after the C call and all nested callbacks return.
    let session = unsafe { &*session_pointer };
    let count = context.signature.parameters().len();
    if count != 0 && arguments.is_null() {
        fatal("libffi supplied no argument array for a non-empty callback");
    }
    let pointers: &[*mut c_void] = if count == 0 {
        &[]
    } else {
        // SAFETY: libffi supplies one pointer for every prepared CIF parameter.
        unsafe { std::slice::from_raw_parts(arguments.cast_const(), count) }
    };
    let mut strings: Vec<Option<String>> = (0..count).map(|_| None).collect();
    for (index, (spec, pointer)) in context
        .signature
        .parameters()
        .iter()
        .copied()
        .zip(pointers.iter().copied())
        .enumerate()
    {
        if pointer.is_null() {
            fatal(&format!("libffi supplied a null argument slot {index}"));
        }
        if let ForeignTypeSpec::Scalar(ForeignType::CString) = spec {
            // SAFETY: `pointer` addresses the C pointer word libffi decoded for
            // this parameter. The pointed-to bytes live through this callback.
            let address = unsafe { ptr::read_unaligned(pointer.cast::<*const c_char>()) };
            let text = if address.is_null() {
                String::new()
            } else {
                // SAFETY: a CString callback parameter is NUL-terminated by its
                // C caller and remains live through this synchronous callback.
                unsafe { String::from_utf8_lossy(CStr::from_ptr(address).to_bytes()) }.into_owned()
            };
            strings[index] = Some(text);
        }
    }
    let native_arguments: Vec<NativeArg<'_>> = context
        .signature
        .parameters()
        .iter()
        .copied()
        .zip(pointers.iter().copied())
        .enumerate()
        .map(|(index, (spec, pointer))| callback_argument(spec, pointer, &strings[index]))
        .collect();
    let mut host = Host { session };
    let (program, _, callback_ids) = session.current_program();
    // The id in the context is the one the module had when this closure was
    // prepared, and a VM reload renumbers functions. `replace_vm_program`
    // publishes `callback_ids` for exactly this remapping, keyed by the original
    // id — so a closure prepared before a reload must be resolved through it,
    // the same way `invoke_runtime` resolves the calls that arrive by symbol.
    // Calling `context.function_id` directly would run whatever function now
    // holds that slot, which after a reload is a different one.
    let Some(&current_function_id) = callback_ids.get(context.function_id as usize) else {
        fatal(&format!(
            "a C callback named runtime function {}, but the live module has no identity for it",
            context.function_id
        ));
    };
    match program.call(&mut host, current_function_id, &native_arguments) {
        Ok(value) => write_callback_result(context.signature.result(), value, result),
        Err(trap) => fatal(&format!("runtime trap in C callback: {trap}")),
    }
}

fn callback_argument<'a>(
    spec: ForeignTypeSpec,
    pointer: *mut c_void,
    string: &'a Option<String>,
) -> NativeArg<'a> {
    match spec {
        ForeignTypeSpec::Aggregate(_) => {
            // C passes an aggregate by value. The callback contract presents its
            // address to Kira as the corresponding pointer word.
            NativeArg::RawPtr(pointer as usize as u64)
        }
        ForeignTypeSpec::Scalar(ty) => match ty {
            ForeignType::Void => NativeArg::Void,
            ForeignType::I8 => NativeArg::Int(i64::from(read_unaligned::<i8>(pointer))),
            ForeignType::I16 => NativeArg::Int(i64::from(read_unaligned::<i16>(pointer))),
            ForeignType::I32 => NativeArg::Int(i64::from(read_unaligned::<i32>(pointer))),
            ForeignType::I64 => NativeArg::Int(read_unaligned::<i64>(pointer)),
            ForeignType::U8 => NativeArg::Int(i64::from(read_unaligned::<u8>(pointer))),
            ForeignType::U16 => NativeArg::Int(i64::from(read_unaligned::<u16>(pointer))),
            ForeignType::U32 => NativeArg::Int(i64::from(read_unaligned::<u32>(pointer))),
            ForeignType::U64 => NativeArg::Int(read_unaligned::<u64>(pointer) as i64),
            ForeignType::Bool => NativeArg::Bool(read_unaligned::<u8>(pointer) != 0),
            ForeignType::F32 => NativeArg::Float(f64::from(read_unaligned::<f32>(pointer))),
            ForeignType::F64 => NativeArg::Float(read_unaligned::<f64>(pointer)),
            ForeignType::RawPtr => NativeArg::RawPtr(read_unaligned::<usize>(pointer) as u64),
            ForeignType::CString => match string.as_deref() {
                Some(value) => NativeArg::Str(value),
                None => fatal("a CString callback argument was not decoded"),
            },
        },
    }
}

fn write_callback_result(spec: ForeignTypeSpec, result: NativeResult, output: *mut c_void) {
    if spec == ForeignTypeSpec::Scalar(ForeignType::Void) {
        if !matches!(result, NativeResult::Void) {
            fatal("a void C callback returned a value");
        }
        return;
    }
    if output.is_null() {
        fatal("libffi supplied no result storage for a non-void callback");
    }
    match (spec, result) {
        (ForeignTypeSpec::Scalar(ForeignType::I8), NativeResult::Int(value)) => {
            write_unaligned(output, value as i8)
        }
        (ForeignTypeSpec::Scalar(ForeignType::I16), NativeResult::Int(value)) => {
            write_unaligned(output, value as i16)
        }
        (ForeignTypeSpec::Scalar(ForeignType::I32), NativeResult::Int(value)) => {
            write_unaligned(output, value as i32)
        }
        (ForeignTypeSpec::Scalar(ForeignType::I64), NativeResult::Int(value)) => {
            write_unaligned(output, value)
        }
        (ForeignTypeSpec::Scalar(ForeignType::U8), NativeResult::Int(value)) => {
            write_unaligned(output, value as u8)
        }
        (ForeignTypeSpec::Scalar(ForeignType::U16), NativeResult::Int(value)) => {
            write_unaligned(output, value as u16)
        }
        (ForeignTypeSpec::Scalar(ForeignType::U32), NativeResult::Int(value)) => {
            write_unaligned(output, value as u32)
        }
        (ForeignTypeSpec::Scalar(ForeignType::U64), NativeResult::Int(value)) => {
            write_unaligned(output, value as u64)
        }
        (ForeignTypeSpec::Scalar(ForeignType::Bool), NativeResult::Bool(value)) => {
            write_unaligned(output, u8::from(value))
        }
        (ForeignTypeSpec::Scalar(ForeignType::F32), NativeResult::Float(value)) => {
            write_unaligned(output, value as f32)
        }
        (ForeignTypeSpec::Scalar(ForeignType::F64), NativeResult::Float(value)) => {
            write_unaligned(output, value)
        }
        (ForeignTypeSpec::Scalar(ForeignType::RawPtr), NativeResult::RawPtr(value)) => {
            if (value as usize) as u64 != value {
                fatal("a callback returned a pointer wider than the target");
            }
            write_unaligned(output, value as usize)
        }
        _ => fatal("a Kira callback returned a value with the wrong C type"),
    }
}

fn read_unaligned<T: Copy>(pointer: *mut c_void) -> T {
    // SAFETY: libffi supplies initialized storage for the type described by the
    // callback CIF; unaligned access handles all valid C layouts.
    unsafe { ptr::read_unaligned(pointer.cast::<T>()) }
}

fn write_unaligned<T: Copy>(pointer: *mut c_void, value: T) {
    // SAFETY: libffi supplies writable storage sized for the callback result CIF.
    unsafe { ptr::write_unaligned(pointer.cast::<T>(), value) };
}
