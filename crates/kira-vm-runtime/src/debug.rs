//! The VM's instruction-level debugger seam.
//!
//! The ordinary interpreter does not install an observer and therefore keeps
//! its hot dispatch loop free of debugger calls. A debug run opts in explicitly
//! and receives a borrowed view of each instruction before it executes.

use kira_bytecode::op::Instruction;
use std::fmt::Write;
use std::slice;

use crate::value::Value;

/// Tags carried by [`KiraVmDebugValue`].
pub mod value_tag {
    /// The slot contains `Void`.
    pub const VOID: u32 = 0;
    /// The slot contains an `Int`; the payload is its two's-complement bits.
    pub const INT: u32 = 1;
    /// The slot contains a `Float`; the payload is `f64::to_bits()`.
    pub const FLOAT: u32 = 2;
    /// The slot contains a `Bool`; the payload is zero or one.
    pub const BOOL: u32 = 3;
    /// The slot contains a string heap handle.
    pub const STRING: u32 = 4;
    /// The slot contains a struct heap handle.
    pub const STRUCT: u32 = 5;
    /// The slot contains an array heap handle.
    pub const ARRAY: u32 = 6;
    /// The slot contains an enum heap handle.
    pub const ENUM: u32 = 7;
    /// The slot contains an erased-value heap handle.
    pub const ERASED: u32 = 8;
    /// The slot contains a closure capture-cell handle.
    pub const CELL: u32 = 9;
    /// The slot contains an opaque raw pointer.
    pub const RAW_POINTER: u32 = 10;
    /// The slot contains a native-state token.
    pub const NATIVE_STATE: u32 = 11;
    /// The slot contains a native-state view token.
    pub const NATIVE_VIEW: u32 = 12;
    /// The slot contains a native-state snapshot handle.
    pub const NATIVE_SNAPSHOT: u32 = 13;
}

/// One debugger-stable Kira value.
///
/// The ordinary VM [`Value`] is a Rust enum whose layout is intentionally not
/// an ABI. This representation is the explicit C-shaped view LLDB can inspect
/// without guessing Rust enum discriminants or pointer widths.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KiraVmDebugValue {
    /// One of the tags in [`value_tag`].
    pub tag: u32,
    /// Scalar bits or an opaque heap/token word, according to `tag`.
    pub payload: u64,
}

/// One call frame in the LLDB-readable VM backtrace.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct KiraVmDebugFrame {
    /// The bytecode function id.
    pub function_id: u32,
    /// The next bytecode instruction in the frame.
    pub pc: u32,
    /// UTF-8 name bytes owned by the active VM module/event.
    pub function_name: *const u8,
    /// Number of bytes at `function_name`.
    pub function_name_len: u32,
}

/// The complete VM stop state exposed to real LLDB.
///
/// All pointers refer to buffers held by [`VmLldbObserver`] for the duration
/// of the probe call. LLDB can inspect this state while stopped, and the
/// exported [`kira_vm_debug_dump`] helper decodes the same bytes for a human.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KiraVmDebugState {
    /// Current bytecode function id.
    pub function_id: u32,
    /// Current bytecode program counter.
    pub pc: u32,
    /// Current instruction's opcode byte.
    pub opcode: u8,
    /// Padding kept explicit so the C layout is stable.
    pub _padding: [u8; 3],
    /// VM call depth at the stop.
    pub call_depth: u32,
    /// Operand stack depth at the stop.
    pub stack_depth: u32,
    /// Current function name bytes.
    pub function_name: *const u8,
    /// Current function name length.
    pub function_name_len: u32,
    /// Encoded instruction bytes.
    pub instruction: *const u8,
    /// Number of encoded instruction bytes.
    pub instruction_len: u32,
    /// Current frame's local values, slot order.
    pub locals: *const KiraVmDebugValue,
    /// Number of local values.
    pub locals_len: u32,
    /// Complete operand-stack values, bottom to top.
    pub stack: *const KiraVmDebugValue,
    /// Number of operand-stack values.
    pub stack_len: u32,
    /// Call frames from the current function outward.
    pub backtrace: *const KiraVmDebugFrame,
    /// Number of backtrace frames.
    pub backtrace_len: u32,
}

impl KiraVmDebugState {
    const EMPTY: Self = Self {
        function_id: 0,
        pc: 0,
        opcode: 0,
        _padding: [0; 3],
        call_depth: 0,
        stack_depth: 0,
        function_name: std::ptr::null(),
        function_name_len: 0,
        instruction: std::ptr::null(),
        instruction_len: 0,
        locals: std::ptr::null(),
        locals_len: 0,
        stack: std::ptr::null(),
        stack_len: 0,
        backtrace: std::ptr::null(),
        backtrace_len: 0,
    };
}

/// Maximum size of the LLDB-readable text mirror.
const DEBUG_TEXT_CAPACITY: usize = 4096;

/// The native frame LLDB stops in while a VM program is executing.
///
/// The interpreter remains portable: this observer is opt-in and the probe is
/// only called by a debug run. LLDB sees the scalar VM location in the native
/// frame (`function_id`, `pc`, `opcode`, and the two depth values), while the
/// pointer/size pairs identify the live Kira local and operand-stack storage
/// available for `memory read` inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmLldbBreakpoint {
    /// The bytecode function id to match for the first LLDB stop.
    pub function_id: u32,
    /// The instruction index to match.
    pub pc: u32,
}

/// Observer used by a real LLDB-hosted VM.
#[derive(Debug, Default)]
pub struct VmLldbObserver {
    initial_breakpoints: Vec<VmLldbBreakpoint>,
    initial_stop_reached: bool,
    encoded_instruction: Vec<u8>,
    encoded_locals: Vec<KiraVmDebugValue>,
    encoded_stack: Vec<KiraVmDebugValue>,
    encoded_backtrace: Vec<KiraVmDebugFrame>,
}

impl VmLldbObserver {
    /// Creates an observer that probes at the first VM instruction.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an observer that waits for one of the requested VM locations.
    #[must_use]
    pub fn with_breakpoints(breakpoints: Vec<VmLldbBreakpoint>) -> Self {
        Self {
            initial_breakpoints: breakpoints,
            initial_stop_reached: false,
            encoded_instruction: Vec::with_capacity(16),
            encoded_locals: Vec::new(),
            encoded_stack: Vec::new(),
            encoded_backtrace: Vec::new(),
        }
    }
}

impl VmDebugObserver for VmLldbObserver {
    fn before_instruction(&mut self, event: VmDebugEvent<'_>) -> VmDebugAction {
        let requested_stop = self.initial_breakpoints.iter().any(|breakpoint| {
            breakpoint.function_id == event.function_id
                && usize::try_from(breakpoint.pc).is_ok_and(|pc| pc == event.pc)
        });
        let should_probe =
            self.initial_stop_reached || self.initial_breakpoints.is_empty() || requested_stop;
        if !should_probe {
            return VmDebugAction::Continue;
        }
        self.initial_stop_reached = true;
        self.encoded_instruction.clear();
        kira_bytecode::op::encode_one(event.instruction, &mut self.encoded_instruction);
        let opcode = self.encoded_instruction.first().copied().unwrap_or(0);
        encode_values(event.locals, &mut self.encoded_locals);
        encode_values(event.stack, &mut self.encoded_stack);
        self.encoded_backtrace.clear();
        self.encoded_backtrace
            .extend(event.backtrace.iter().map(|frame| KiraVmDebugFrame {
                function_id: frame.function_id,
                pc: debug_word(frame.pc),
                function_name: frame.function_name.as_ptr(),
                function_name_len: debug_word(frame.function_name.len()),
            }));
        // Publish before entering the native probe. LLDB stops at the probe's
        // first machine instruction, before the probe body can update a
        // global, so the state must already describe this VM instruction.
        publish_debug_state(KiraVmDebugState {
            function_id: event.function_id,
            pc: debug_word(event.pc),
            opcode,
            _padding: [0; 3],
            call_depth: debug_word(event.call_depth),
            stack_depth: debug_word(event.stack_depth),
            function_name: event.function_name.as_ptr(),
            function_name_len: debug_word(event.function_name.len()),
            instruction: self.encoded_instruction.as_ptr(),
            instruction_len: debug_word(self.encoded_instruction.len()),
            locals: self.encoded_locals.as_ptr(),
            locals_len: debug_word(self.encoded_locals.len()),
            stack: self.encoded_stack.as_ptr(),
            stack_len: debug_word(self.encoded_stack.len()),
            backtrace: self.encoded_backtrace.as_ptr(),
            backtrace_len: debug_word(self.encoded_backtrace.len()),
        });
        kira_vm_debug_probe(
            event.function_id,
            debug_word(event.pc),
            opcode,
            debug_word(event.call_depth),
            debug_word(event.stack_depth),
            event.function_name.as_ptr(),
            debug_word(event.function_name.len()),
            self.encoded_instruction.as_ptr(),
            debug_word(self.encoded_instruction.len()),
            self.encoded_locals.as_ptr(),
            debug_word(self.encoded_locals.len()),
            self.encoded_stack.as_ptr(),
            debug_word(self.encoded_stack.len()),
            self.encoded_backtrace.as_ptr(),
            debug_word(self.encoded_backtrace.len()),
        );
        VmDebugAction::Continue
    }
}

/// Stable native symbol used by real LLDB VM sessions.
///
/// This function publishes the current stop state and is the native frame LLDB
/// breaks on. Its `extern "C"` signature is the debugger ABI: the first scalar
/// arguments identify the Kira location, while the following pointers name
/// C-shaped instruction, local, operand-stack, and backtrace buffers.
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn kira_vm_debug_probe(
    function_id: u32,
    pc: u32,
    opcode: u8,
    call_depth: u32,
    stack_depth: u32,
    function_name: *const u8,
    function_name_len: u32,
    instruction: *const u8,
    instruction_len: u32,
    locals: *const KiraVmDebugValue,
    locals_len: u32,
    stack: *const KiraVmDebugValue,
    stack_len: u32,
    backtrace: *const KiraVmDebugFrame,
    backtrace_len: u32,
) {
    publish_debug_state(KiraVmDebugState {
        function_id,
        pc,
        opcode,
        _padding: [0; 3],
        call_depth,
        stack_depth,
        function_name,
        function_name_len,
        instruction,
        instruction_len,
        locals,
        locals_len,
        stack,
        stack_len,
        backtrace,
        backtrace_len,
    });
    std::hint::black_box((
        function_id,
        pc,
        opcode,
        call_depth,
        stack_depth,
        function_name,
        function_name_len,
        instruction,
        instruction_len,
        locals,
        locals_len,
        stack,
        stack_len,
        backtrace,
        backtrace_len,
    ));
}

fn publish_debug_state(state: KiraVmDebugState) {
    let text = format_debug_state(state);
    let copy_len = text.len().min(DEBUG_TEXT_CAPACITY.saturating_sub(1));
    // SAFETY: the VM calls the probe synchronously and LLDB reads this state
    // only while that call is stopped. The pointed-to buffers belong to the
    // active observer and remain alive until the probe returns.
    unsafe {
        KIRA_VM_DEBUG_STATE = state;
        let destination = std::ptr::addr_of_mut!(KIRA_VM_DEBUG_TEXT).cast::<u8>();
        std::ptr::copy_nonoverlapping(text.as_ptr(), destination, copy_len);
        destination.add(copy_len).write(0);
        KIRA_VM_DEBUG_TEXT_LEN = copy_len as u32;
    }
}

/// The last VM stop, exported as a C-shaped global for LLDB expressions.
///
/// `expr -- KIRA_VM_DEBUG_STATE.function_id` and `memory read
/// KIRA_VM_DEBUG_STATE.locals` are useful when a debugger wants the raw view;
/// [`kira_vm_debug_dump`] is the readable equivalent.
#[unsafe(no_mangle)]
pub static mut KIRA_VM_DEBUG_STATE: KiraVmDebugState = KiraVmDebugState::EMPTY;

/// A null-terminated human-readable mirror of [`KIRA_VM_DEBUG_STATE`].
///
/// LLDB can read this with `memory read --format s --size 1 --count 1
/// &KIRA_VM_DEBUG_TEXT` without evaluating a function in the stopped process.
/// That matters on Windows Swift LLDB, whose expression evaluator is unstable
/// when a target-side printing function is called repeatedly while stepping.
#[unsafe(no_mangle)]
pub static mut KIRA_VM_DEBUG_TEXT: [u8; DEBUG_TEXT_CAPACITY] = [0; DEBUG_TEXT_CAPACITY];

/// Number of meaningful bytes in [`KIRA_VM_DEBUG_TEXT`].
#[unsafe(no_mangle)]
pub static mut KIRA_VM_DEBUG_TEXT_LEN: u32 = 0;

/// Prints the stopped VM frame, decoded values, instruction bytes, and
/// backtrace from inside the debugged process.
///
/// LLDB can call this while stopped with:
///
/// ```text
/// expr -- (void)kira_vm_debug_dump()
/// ```
///
/// It is deliberately a debug-only side effect. Normal VM execution never
/// installs an observer and therefore never reaches this function.
#[unsafe(no_mangle)]
pub extern "C" fn kira_vm_debug_dump() {
    // SAFETY: LLDB calls this only while `kira_vm_debug_probe` is active, so
    // the state and all observer-owned buffers are alive for this read.
    let state = unsafe { KIRA_VM_DEBUG_STATE };
    print!("{}", format_debug_state(state));
}

fn format_debug_state(state: KiraVmDebugState) -> String {
    let name = unsafe_bytes(state.function_name, state.function_name_len);
    let instruction = unsafe_byte_slice(state.instruction, state.instruction_len);
    let mut output = String::new();
    let _ = writeln!(
        output,
        "kira-vm-stop function={}({}) pc={} opcode={} call_depth={} stack_depth={}",
        name, state.function_id, state.pc, state.opcode, state.call_depth, state.stack_depth,
    );
    let _ = writeln!(output, "  instruction-bytes: {}", format_bytes(instruction));
    let _ = writeln!(output, "  locals:");
    for (index, value) in unsafe_values(state.locals, state.locals_len)
        .iter()
        .enumerate()
    {
        let _ = writeln!(output, "    [{index}] {}", format_value(*value));
    }
    let _ = writeln!(output, "  operand-stack:");
    for (index, value) in unsafe_values(state.stack, state.stack_len)
        .iter()
        .enumerate()
    {
        let _ = writeln!(output, "    [{index}] {}", format_value(*value));
    }
    let _ = writeln!(output, "  backtrace:");
    for (index, frame) in unsafe_frames(state.backtrace, state.backtrace_len)
        .iter()
        .enumerate()
    {
        let _ = writeln!(
            output,
            "    #{index} {}({}) pc={}",
            unsafe_bytes(frame.function_name, frame.function_name_len),
            frame.function_id,
            frame.pc,
        );
    }
    output
}

fn encode_values(values: &[Value], output: &mut Vec<KiraVmDebugValue>) {
    output.clear();
    output.extend(values.iter().copied().map(encode_value));
}

fn encode_value(value: Value) -> KiraVmDebugValue {
    let (tag, payload) = match value {
        Value::Void => (value_tag::VOID, 0),
        Value::Int(value) => (value_tag::INT, value as u64),
        Value::Float(value) => (value_tag::FLOAT, value.to_bits()),
        Value::Bool(value) => (value_tag::BOOL, u64::from(value)),
        Value::Str(id) => (value_tag::STRING, id.debug_word()),
        Value::Struct(id) => (value_tag::STRUCT, id.debug_word()),
        Value::Array(id) => (value_tag::ARRAY, id.debug_word()),
        Value::Enum(id) => (value_tag::ENUM, id.debug_word()),
        Value::Erased(id) => (value_tag::ERASED, id.debug_word()),
        Value::Cell(id) => (value_tag::CELL, id.debug_word()),
        Value::RawPtr(value) => (value_tag::RAW_POINTER, value),
        Value::NativeState(token) => (value_tag::NATIVE_STATE, token.as_word()),
        Value::NativeView { token, .. } => (value_tag::NATIVE_VIEW, token.as_word()),
        Value::NativeSnapshot(id) => (value_tag::NATIVE_SNAPSHOT, id.debug_word()),
    };
    KiraVmDebugValue { tag, payload }
}

fn unsafe_bytes(pointer: *const u8, length: u32) -> String {
    let bytes = unsafe_byte_slice(pointer, length);
    String::from_utf8_lossy(bytes).into_owned()
}

fn unsafe_byte_slice<'a>(pointer: *const u8, length: u32) -> &'a [u8] {
    if length == 0 {
        return &[];
    }
    // SAFETY: callers pass observer-owned buffers with the exact byte length.
    unsafe { slice::from_raw_parts(pointer, length as usize) }
}

fn unsafe_values<'a>(pointer: *const KiraVmDebugValue, length: u32) -> &'a [KiraVmDebugValue] {
    if length == 0 {
        return &[];
    }
    // SAFETY: callers pass observer-owned arrays with the exact element count.
    unsafe { slice::from_raw_parts(pointer, length as usize) }
}

fn unsafe_frames<'a>(pointer: *const KiraVmDebugFrame, length: u32) -> &'a [KiraVmDebugFrame] {
    if length == 0 {
        return &[];
    }
    // SAFETY: callers pass observer-owned arrays with the exact element count.
    unsafe { slice::from_raw_parts(pointer, length as usize) }
}

fn format_value(value: KiraVmDebugValue) -> String {
    match value.tag {
        value_tag::VOID => "void".to_owned(),
        value_tag::INT => format!("int {}", value.payload as i64),
        value_tag::FLOAT => format!("float {}", f64::from_bits(value.payload)),
        value_tag::BOOL => format!("bool {}", value.payload != 0),
        value_tag::STRING => format!("string-handle {}", value.payload),
        value_tag::STRUCT => format!("struct-handle {}", value.payload),
        value_tag::ARRAY => format!("array-handle {}", value.payload),
        value_tag::ENUM => format!("enum-handle {}", value.payload),
        value_tag::ERASED => format!("erased-handle {}", value.payload),
        value_tag::CELL => format!("cell-handle {}", value.payload),
        value_tag::RAW_POINTER => format!("raw-pointer 0x{:x}", value.payload),
        value_tag::NATIVE_STATE => format!("native-state 0x{:x}", value.payload),
        value_tag::NATIVE_VIEW => format!("native-view 0x{:x}", value.payload),
        value_tag::NATIVE_SNAPSHOT => format!("native-snapshot {}", value.payload),
        tag => format!("unknown(tag={tag}, payload={})", value.payload),
    }
}

fn format_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// Rust's `no_mangle` gives the probe a stable object symbol, while LLDB builds
// on Windows resolve executable breakpoints most reliably through the PE
// export table. The COFF linker directive keeps the export local to Windows;
// ELF and Mach-O hosts use the ordinary public symbol directly.
#[cfg(windows)]
#[used]
#[unsafe(link_section = ".drectve")]
static KIRA_VM_DEBUG_PROBE_EXPORT: [u8; b"/EXPORT:kira_vm_debug_probe\0".len()] =
    *b"/EXPORT:kira_vm_debug_probe\0";

#[cfg(windows)]
#[used]
#[unsafe(link_section = ".drectve")]
static KIRA_VM_DEBUG_DUMP_EXPORT: [u8; b"/EXPORT:kira_vm_debug_dump\0".len()] =
    *b"/EXPORT:kira_vm_debug_dump\0";

#[cfg(windows)]
#[used]
#[unsafe(link_section = ".drectve")]
static KIRA_VM_DEBUG_STATE_EXPORT: [u8; b"/EXPORT:KIRA_VM_DEBUG_STATE\0".len()] =
    *b"/EXPORT:KIRA_VM_DEBUG_STATE\0";

#[cfg(windows)]
#[used]
#[unsafe(link_section = ".drectve")]
static KIRA_VM_DEBUG_TEXT_EXPORT: [u8; b"/EXPORT:KIRA_VM_DEBUG_TEXT\0".len()] =
    *b"/EXPORT:KIRA_VM_DEBUG_TEXT\0";

#[cfg(windows)]
#[used]
#[unsafe(link_section = ".drectve")]
static KIRA_VM_DEBUG_TEXT_LEN_EXPORT: [u8; b"/EXPORT:KIRA_VM_DEBUG_TEXT_LEN\0".len()] =
    *b"/EXPORT:KIRA_VM_DEBUG_TEXT_LEN\0";

fn debug_word(value: usize) -> u32 {
    value.min(u32::MAX as usize) as u32
}

/// One call frame visible at a VM stop.
#[derive(Debug, Clone, Copy)]
pub struct VmDebugFrame<'a> {
    /// The function index in the bytecode module.
    pub function_id: u32,
    /// The function's source-level name.
    pub function_name: &'a str,
    /// The next instruction associated with this frame.
    pub pc: usize,
}

/// The immutable state visible immediately before one instruction executes.
#[derive(Debug, Clone, Copy)]
pub struct VmDebugEvent<'a> {
    /// The function index in the bytecode module.
    pub function_id: u32,
    /// The function's source-level name.
    pub function_name: &'a str,
    /// The instruction index about to execute.
    pub pc: usize,
    /// The instruction about to execute.
    pub instruction: &'a Instruction,
    /// The complete code for the current function, for disassembly.
    pub code: &'a [Instruction],
    /// The current call-frame depth, with the entry frame at depth zero.
    pub call_depth: usize,
    /// The operand-stack depth before the instruction executes.
    pub stack_depth: usize,
    /// The current frame's locals, in bytecode slot order.
    ///
    /// Values are borrowed without changing ownership or heap accounting; the
    /// debugger can inspect them while stopped and the instruction still owns
    /// them when execution resumes.
    pub locals: &'a [Value],
    /// The complete operand stack before the instruction executes.
    ///
    /// The last element is the next value an instruction would pop. This is a
    /// read-only view for debugger inspection, not a second ownership root.
    pub stack: &'a [Value],
    /// Call frames from the current function outward to the entrypoint.
    pub backtrace: &'a [VmDebugFrame<'a>],
}

/// What an observer wants the interpreter to do after a stop callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmDebugAction {
    /// Continue interpreting.
    Continue,
    /// Stop and return control to the embedder.
    Stop,
}

/// A synchronous observer called before every instruction in a debug run.
pub trait VmDebugObserver {
    /// Inspect the next instruction and optionally pause or terminate the run.
    fn before_instruction(&mut self, event: VmDebugEvent<'_>) -> VmDebugAction;
}
