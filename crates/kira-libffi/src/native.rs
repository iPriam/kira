//! Native LLVM entrypoints for calls prepared by the shared libffi graph.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::slice;
use std::sync::Mutex;

use kira_runtime_abi::{
    ForeignAggregate, ForeignAggregateId, ForeignAggregates, ForeignArrayElement, ForeignMember,
    ForeignSignature, ForeignType, ForeignTypeSpec,
};

use crate::LibffiRuntime;
use crate::types::PreparedCif;

/// The raw call completed successfully.
pub const KIRA_FFI_OK: u32 = 0;
/// The bundled libffi runtime could not be loaded.
pub const KIRA_FFI_MISSING_BUNDLE: u32 = 1;
/// A native descriptor was malformed.
pub const KIRA_FFI_INVALID_DESCRIPTOR: u32 = 2;
/// A non-void call received no result storage.
pub const KIRA_FFI_INVALID_RESULT: u32 = 3;
/// The function address was null.
pub const KIRA_FFI_NULL_FUNCTION: u32 = 4;

const DESCRIPTOR_MAGIC: u32 = 0x3146_464b;
const MAX_DESCRIPTOR_BYTES: u32 = 64 * 1024 * 1024;

const MAX_DESCRIPTOR_ITEMS: u32 = 1 << 20;

/// The magic and total-length words a blob opens with.
const HEADER_BYTES: usize = 8;

/// The shortest legal blob: the header, the parameter count, the result
/// triple, and the aggregate count.
const MIN_DESCRIPTOR_BYTES: u32 = 28;

/// Calls a native symbol through a compact little-endian descriptor blob.
///
/// LLVM uses this entrypoint because a byte descriptor keeps pointers out of
/// generated object data. The blob has a versioned header followed by the
/// parameter types and the recursive aggregate table.
///
/// # Safety
/// `descriptor` must point to the complete immutable descriptor blob emitted by
/// the LLVM backend. `arguments` must be null for zero parameters or point to
/// exactly the described number of live storage pointers. `result` must be
/// writable storage for the described result, or null only for a void result.
/// `function` must be a callable address with the described C ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_ffi_call_bytes(
    function: *mut c_void,
    descriptor: *const u8,
    arguments: *mut *mut c_void,
    result: *mut c_void,
) -> u32 {
    if function.is_null() {
        return KIRA_FFI_NULL_FUNCTION;
    }
    let Some(runtime) = LibffiRuntime::shared() else {
        return KIRA_FFI_MISSING_BUNDLE;
    };
    let site = match call_site(&runtime, descriptor) {
        Ok(site) => site,
        Err(status) => return status,
    };
    // SAFETY: the site was boxed into a map this thread only ever inserts
    // into, so its address stays valid however the map grows — including from
    // a callback that re-enters this function during the call below.
    let site = unsafe { &*site };
    if !matches!(site.result, ForeignTypeSpec::Scalar(ForeignType::Void)) && result.is_null() {
        return KIRA_FFI_INVALID_RESULT;
    }
    if site.parameters != 0 && arguments.is_null() {
        return KIRA_FFI_INVALID_DESCRIPTOR;
    }
    // SAFETY: the CIF was prepared from this descriptor, and the storage
    // contract was checked above.
    match unsafe { runtime.call_prepared(function, &site.prepared, site.result, arguments, result) }
    {
        Ok(()) => KIRA_FFI_OK,
        Err(crate::LibffiError::NullFunction) => KIRA_FFI_NULL_FUNCTION,
        Err(_) => KIRA_FFI_INVALID_DESCRIPTOR,
    }
}

/// This thread's prepared call site for `descriptor`, preparing it once.
///
/// The borrow ends before the caller calls: a foreign function may enter a
/// callback that reaches Kira and another foreign call, and that call arrives
/// here while this one is still on the stack.
fn call_site(runtime: &LibffiRuntime, descriptor: *const u8) -> Result<*const CallSite, u32> {
    CALL_SITES.with(|sites| {
        if let Some(site) = sites.borrow().get(&(descriptor as usize)) {
            return Ok(std::ptr::from_ref(site.as_ref()));
        }
        // SAFETY: the caller's contract is that `descriptor` addresses the
        // complete blob emitted for this import.
        let (signature, aggregates) = unsafe { decode_bytes(descriptor) }?;
        let prepared = PreparedCif::new(&runtime.api, &signature, &aggregates)
            .map_err(|_| KIRA_FFI_INVALID_DESCRIPTOR)?;
        let site = Box::new(CallSite {
            result: signature.result(),
            parameters: signature.parameters().len(),
            prepared,
        });
        let address = std::ptr::from_ref(site.as_ref());
        sites.borrow_mut().insert(descriptor as usize, site);
        Ok(address)
    })
}

/// One import's decoded signature and its prepared CIF.
struct CallSite {
    result: ForeignTypeSpec,
    parameters: usize,
    prepared: PreparedCif,
}

thread_local! {
    /// Every call site this thread has reached, by descriptor address.
    ///
    /// A descriptor is an immutable constant emitted once per import, so its
    /// address identifies the signature. Boxed so that a site keeps its address
    /// as the map grows, and per thread so the path a frame takes thousands of
    /// times takes no lock.
    static CALL_SITES: std::cell::RefCell<BTreeMap<usize, Box<CallSite>>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

/// Returns the C-callable address of the closure for one callback entry.
///
/// The address C stores for a `@FFI.Callback` is a libffi closure rather than a
/// generated function with the declared C signature: the platform's rules for a
/// struct passed by value are libffi's to apply, and a backend that spelled the
/// signature itself would be classifying the struct a second time.
///
/// One closure per entry address, prepared once and never released: C keeps the
/// address for as long as it likes, and two materializations of one callback
/// must compare equal.
///
/// A callback address is a value a program already holds, so there is no call
/// to fail: an absent libffi or a malformed descriptor ends the process with
/// the reason named, exactly as the runtime's own traps do.
///
/// # Safety
/// `descriptor` must address the complete descriptor blob emitted for this
/// callback, and `entry` must be a function with the libffi closure-entry
/// signature that stays callable for the process's life.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_ffi_closure(descriptor: *const u8, entry: *mut c_void) -> u64 {
    if entry.is_null() {
        fatal("a callback value named no entry");
    }
    let mut closures = CLOSURES
        .lock()
        .unwrap_or_else(|held: std::sync::PoisonError<_>| held.into_inner());
    if let Some(code) = closures.get(&(entry as usize)) {
        return *code;
    }
    // SAFETY: the caller's contract is that `descriptor` addresses the complete
    // blob emitted for this callback.
    let Ok((signature, aggregates)) = (unsafe { decode_bytes(descriptor) }) else {
        fatal("a callback carries a descriptor this build cannot read");
    };
    let Ok(runtime) = LibffiRuntime::load() else {
        fatal("a callback needs the bundled libffi runtime, which did not load");
    };
    // SAFETY: `entry` has the closure-entry signature by the caller's contract,
    // and the closure carries no user data to outlive.
    let prepared = unsafe {
        let entry: crate::FfiClosureCallback = std::mem::transmute(entry);
        crate::FfiClosure::new(
            &runtime,
            &signature,
            &aggregates,
            entry,
            std::ptr::null_mut(),
        )
    };
    let Ok(prepared) = prepared else {
        fatal("a callback signature could not be prepared for the C ABI");
    };
    let code = prepared.code() as usize as u64;
    // The closure stays alive for the process: nothing on this side learns when
    // C stops calling through the address it was given.
    std::mem::forget(prepared);
    closures.insert(entry as usize, code);
    code
}

/// Reports a condition a callback address cannot return from, and exits.
fn fatal(message: &str) -> ! {
    eprintln!("kira: {message}");
    std::process::exit(1);
}

/// Every callback entry's prepared closure, by entry address.
static CLOSURES: Mutex<BTreeMap<usize, u64>> = Mutex::new(BTreeMap::new());

/// Decodes the compact blob the LLVM backend emits beside a call site.
///
/// # Safety
/// `descriptor` must address a complete blob: at least [`HEADER_BYTES`] bytes,
/// whose second word is the blob's own total length in bytes.
unsafe fn decode_bytes(
    descriptor: *const u8,
) -> Result<(ForeignSignature, ForeignAggregates), u32> {
    if descriptor.is_null() {
        return Err(KIRA_FFI_INVALID_DESCRIPTOR);
    }
    // SAFETY: the caller guarantees the header is present, and the header is
    // what says how long the rest is.
    let header = unsafe { slice::from_raw_parts(descriptor, HEADER_BYTES) };
    let mut words = Words::new(header);
    if words.word()? != DESCRIPTOR_MAGIC {
        return Err(KIRA_FFI_INVALID_DESCRIPTOR);
    }
    let total = words.word()?;
    if !(MIN_DESCRIPTOR_BYTES..=MAX_DESCRIPTOR_BYTES).contains(&total) {
        return Err(KIRA_FFI_INVALID_DESCRIPTOR);
    }
    // SAFETY: the blob declares its own length, which the bound above keeps
    // within the emitted maximum; every read below is bounds-checked in safe
    // code against this slice.
    let bytes = unsafe { slice::from_raw_parts(descriptor, total as usize) };
    let mut words = Words::new(bytes);
    // The header again, now inside the bounded blob rather than ahead of it.
    words.word()?;
    words.word()?;
    decode_words(&mut words)
}

/// Decodes a blob's body, the header already consumed.
fn decode_words(words: &mut Words<'_>) -> Result<(ForeignSignature, ForeignAggregates), u32> {
    let parameter_count = words.word()?;
    let result = decode_type(words.type_descriptor()?)?;
    let aggregate_count = words.word()?;
    if parameter_count > MAX_DESCRIPTOR_ITEMS || aggregate_count > MAX_DESCRIPTOR_ITEMS {
        return Err(KIRA_FFI_INVALID_DESCRIPTOR);
    }
    let mut parameters = Vec::with_capacity(parameter_count as usize);
    for _ in 0..parameter_count {
        parameters.push(decode_type(words.type_descriptor()?)?);
    }
    let mut aggregates = ForeignAggregates::new();
    for _ in 0..aggregate_count {
        let member_count = words.word()?;
        if member_count > MAX_DESCRIPTOR_ITEMS {
            return Err(KIRA_FFI_INVALID_DESCRIPTOR);
        }
        let mut members = Vec::with_capacity(member_count as usize);
        for _ in 0..member_count {
            members.push(decode_member(words.member_descriptor()?)?);
        }
        aggregates
            .push(ForeignAggregate::new(members))
            .map_err(|_| KIRA_FFI_INVALID_DESCRIPTOR)?;
    }
    if !words.is_at_end() {
        return Err(KIRA_FFI_INVALID_DESCRIPTOR);
    }
    Ok((ForeignSignature::new(parameters, result), aggregates))
}

/// One scalar-or-aggregate position decoded out of a blob.
struct FfiTypeDescriptor {
    /// Zero for a scalar, one for an aggregate.
    kind: u32,
    /// The [`ForeignType::tag`] for a scalar.
    scalar: u32,
    /// The aggregate table index for an aggregate.
    aggregate: u32,
}

/// One member decoded out of an aggregate's blob section.
struct FfiMemberDescriptor {
    /// Zero for a scalar, one for a nested aggregate, two for an array.
    kind: u32,
    /// The scalar or array-element scalar tag.
    scalar: u32,
    /// The nested aggregate or array-element aggregate index.
    aggregate: u32,
    /// The fixed array extent, or zero for a non-array member.
    count: u32,
}

/// A bounds-checked little-endian word cursor over a descriptor blob.
struct Words<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Words<'a> {
    fn new(bytes: &'a [u8]) -> Words<'a> {
        Words { bytes, cursor: 0 }
    }

    fn word(&mut self) -> Result<u32, u32> {
        let end = self
            .cursor
            .checked_add(4)
            .ok_or(KIRA_FFI_INVALID_DESCRIPTOR)?;
        let word: [u8; 4] = self
            .bytes
            .get(self.cursor..end)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(KIRA_FFI_INVALID_DESCRIPTOR)?;
        self.cursor = end;
        Ok(u32::from_le_bytes(word))
    }

    fn type_descriptor(&mut self) -> Result<FfiTypeDescriptor, u32> {
        Ok(FfiTypeDescriptor {
            kind: self.word()?,
            scalar: self.word()?,
            aggregate: self.word()?,
        })
    }

    fn member_descriptor(&mut self) -> Result<FfiMemberDescriptor, u32> {
        Ok(FfiMemberDescriptor {
            kind: self.word()?,
            scalar: self.word()?,
            aggregate: self.word()?,
            count: self.word()?,
        })
    }

    fn is_at_end(&self) -> bool {
        self.cursor == self.bytes.len()
    }
}

/// Decodes one aggregate member, in either descriptor form.
fn decode_member(member: FfiMemberDescriptor) -> Result<ForeignMember, u32> {
    Ok(match member.kind {
        0 => ForeignMember::Scalar(
            ForeignType::from_tag(member.scalar as u8).ok_or(KIRA_FFI_INVALID_DESCRIPTOR)?,
        ),
        1 => ForeignMember::Aggregate(ForeignAggregateId(member.aggregate)),
        2 => ForeignMember::Array {
            element: if member.aggregate == u32::MAX {
                ForeignArrayElement::Scalar(
                    ForeignType::from_tag(member.scalar as u8)
                        .ok_or(KIRA_FFI_INVALID_DESCRIPTOR)?,
                )
            } else {
                ForeignArrayElement::Aggregate(ForeignAggregateId(member.aggregate))
            },
            count: member.count,
        },
        _ => return Err(KIRA_FFI_INVALID_DESCRIPTOR),
    })
}

fn decode_type(descriptor: FfiTypeDescriptor) -> Result<ForeignTypeSpec, u32> {
    match descriptor.kind {
        0 => ForeignType::from_tag(descriptor.scalar as u8)
            .map(ForeignTypeSpec::Scalar)
            .ok_or(KIRA_FFI_INVALID_DESCRIPTOR),
        1 => Ok(ForeignTypeSpec::Aggregate(ForeignAggregateId(
            descriptor.aggregate,
        ))),
        _ => Err(KIRA_FFI_INVALID_DESCRIPTOR),
    }
}
