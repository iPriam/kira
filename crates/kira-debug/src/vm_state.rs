//! The decoded VM stop a debugger reads out of a stopped process.
//!
//! A stopped Kira VM publishes its state twice: as a C struct for a debugger
//! that walks memory, and as the null-terminated text in `KIRA_VM_DEBUG_TEXT`
//! for one that only reads bytes. The text is the form a frontend can obtain
//! from any LLDB without calling a function in the debuggee — which matters,
//! because evaluating a target function at a stop is exactly what some LLDB
//! builds crash on.
//!
//! This parses that text back into values. The renderer lives beside the VM in
//! `kira_vm_runtime::format_debug_state`, and the round-trip test here is what
//! keeps the two from drifting apart.

use serde::{Deserialize, Serialize};

/// One Kira value at a stop, as the VM described it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmValue {
    /// The slot or stack index this value occupies.
    pub index: u32,
    /// The value's kind, such as `int`, `bool`, or `struct-handle`.
    pub kind: String,
    /// The rendered payload, absent for kinds that carry none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// One Kira call frame at a stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmFrame {
    /// The frame's depth, `0` being the stopped function.
    pub index: u32,
    /// The Kira function name.
    pub function: String,
    /// The function's identifier in module tables.
    pub function_id: u32,
    /// The instruction index within that function.
    pub pc: u32,
}

/// A decoded VM instruction stop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmStop {
    /// The stopped function's Kira name.
    pub function: String,
    /// The stopped function's identifier.
    pub function_id: u32,
    /// The instruction index the VM stopped before.
    pub pc: u32,
    /// The instruction's opcode byte.
    pub opcode: u8,
    /// How deep the Kira call stack is.
    pub call_depth: u32,
    /// How many values are on the operand stack.
    pub stack_depth: u32,
    /// The encoded bytes of the instruction about to run.
    pub instruction_bytes: Vec<u8>,
    /// The current frame's locals, in slot order.
    pub locals: Vec<VmValue>,
    /// The operand stack, bottom to top.
    pub stack: Vec<VmValue>,
    /// The Kira call frames, innermost first.
    pub backtrace: Vec<VmFrame>,
}

impl VmStop {
    /// Parses the text a stopped VM publishes, or `None` when it published none.
    ///
    /// An empty mirror is the normal state of a process that has not reached a
    /// probe yet, so it is absence rather than a parse failure.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let header = lines.find(|line| line.starts_with("kira-vm-stop "))?;
        let mut stop = Self {
            function: String::new(),
            ..Self::default()
        };
        for (key, value) in header
            .trim_start_matches("kira-vm-stop ")
            .split_whitespace()
            .filter_map(split_field)
        {
            match key {
                "function" => {
                    if let Some((name, id)) = split_named_identifier(value) {
                        stop.function = name.to_owned();
                        stop.function_id = id;
                    }
                }
                // One malformed field skips itself, not the whole stop: a
                // garbled number must not make a real stop look like the VM
                // published nothing.
                "pc" => stop.pc = value.parse().unwrap_or(stop.pc),
                "opcode" => stop.opcode = value.parse().unwrap_or(stop.opcode),
                "call_depth" => {
                    stop.call_depth = value.parse().unwrap_or(stop.call_depth);
                }
                "stack_depth" => {
                    stop.stack_depth = value.parse().unwrap_or(stop.stack_depth);
                }
                _ => {}
            }
        }

        let mut section = Section::None;
        for line in lines {
            let trimmed = line.trim();
            match trimmed {
                "locals:" => section = Section::Locals,
                "operand-stack:" => section = Section::Stack,
                "backtrace:" => section = Section::Backtrace,
                _ if trimmed.starts_with("instruction-bytes:") => {
                    stop.instruction_bytes =
                        parse_bytes(trimmed.trim_start_matches("instruction-bytes:").trim());
                }
                _ => match section {
                    Section::Locals => stop.locals.extend(parse_value(trimmed)),
                    Section::Stack => stop.stack.extend(parse_value(trimmed)),
                    Section::Backtrace => stop.backtrace.extend(parse_frame(trimmed)),
                    Section::None => {}
                },
            }
        }
        Some(stop)
    }
}

/// Which list the parser is reading entries into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Locals,
    Stack,
    Backtrace,
}

/// Splits `key=value`.
fn split_field(field: &str) -> Option<(&str, &str)> {
    field.split_once('=')
}

/// Splits `name(id)` into its two halves.
fn split_named_identifier(value: &str) -> Option<(&str, u32)> {
    let open = value.rfind('(')?;
    let close = value.rfind(')')?;
    let id = value.get(open + 1..close)?.parse().ok()?;
    Some((&value[..open], id))
}

/// Parses `aa bb cc` into bytes, ignoring anything that is not one.
fn parse_bytes(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .filter_map(|byte| u8::from_str_radix(byte, 16).ok())
        .collect()
}

/// Parses `[0] int 900` into a value.
fn parse_value(line: &str) -> Option<VmValue> {
    let (index, rest) = split_bracketed_index(line)?;
    let mut parts = rest.splitn(2, ' ');
    let kind = parts.next()?.to_owned();
    let value = parts
        .next()
        .map(str::to_owned)
        .filter(|text| !text.is_empty());
    Some(VmValue { index, kind, value })
}

/// Parses `#0 discountAmount(4) pc=0` into a frame.
fn parse_frame(line: &str) -> Option<VmFrame> {
    let rest = line.strip_prefix('#')?;
    let (index, rest) = rest.split_once(' ')?;
    let index = index.parse().ok()?;
    let (name, rest) = rest.split_once(' ')?;
    let (function, function_id) = split_named_identifier(name)?;
    let pc = rest.trim().strip_prefix("pc=")?.parse().ok()?;
    Some(VmFrame {
        index,
        function: function.to_owned(),
        function_id,
        pc,
    })
}

/// Parses `[7] rest` into its index and remainder.
fn split_bracketed_index(line: &str) -> Option<(u32, &str)> {
    let rest = line.strip_prefix('[')?;
    let (index, rest) = rest.split_once(']')?;
    Some((index.parse().ok()?, rest.trim_start()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_vm_runtime::{
        KiraVmDebugFrame, KiraVmDebugState, KiraVmDebugValue, format_debug_state,
    };

    /// A stop rendered by the VM itself, so the parser is tested against the
    /// bytes a real process publishes rather than against a hand-written copy.
    fn rendered() -> String {
        let name = "discountAmount";
        let caller = "main";
        let instruction = [0x06_u8, 0x01, 0x00];
        // `int` is tag 1 and `struct-handle` is tag 5 in the published tags.
        let locals = [
            KiraVmDebugValue {
                tag: 5,
                payload: 13,
            },
            KiraVmDebugValue {
                tag: 1,
                payload: 900,
            },
        ];
        let stack = [KiraVmDebugValue {
            tag: 1,
            payload: 900,
        }];
        let backtrace = [
            KiraVmDebugFrame {
                function_id: 4,
                pc: 0,
                function_name: name.as_ptr(),
                function_name_len: name.len() as u32,
            },
            KiraVmDebugFrame {
                function_id: 11,
                pc: 12,
                function_name: caller.as_ptr(),
                function_name_len: caller.len() as u32,
            },
        ];
        format_debug_state(KiraVmDebugState {
            function_id: 4,
            pc: 0,
            opcode: 6,
            _padding: [0; 3],
            call_depth: 1,
            stack_depth: 1,
            function_name: name.as_ptr(),
            function_name_len: name.len() as u32,
            instruction: instruction.as_ptr(),
            instruction_len: instruction.len() as u32,
            locals: locals.as_ptr(),
            locals_len: locals.len() as u32,
            stack: stack.as_ptr(),
            stack_len: stack.len() as u32,
            backtrace: backtrace.as_ptr(),
            backtrace_len: backtrace.len() as u32,
        })
    }

    #[test]
    fn a_rendered_stop_parses_back_into_the_state_that_produced_it() {
        let stop = VmStop::parse(&rendered()).expect("a stop");
        assert_eq!(stop.function, "discountAmount");
        assert_eq!(stop.function_id, 4);
        assert_eq!(stop.pc, 0);
        assert_eq!(stop.opcode, 6);
        assert_eq!(stop.call_depth, 1);
        assert_eq!(stop.stack_depth, 1);
        assert_eq!(stop.instruction_bytes, vec![0x06, 0x01, 0x00]);
    }

    #[test]
    fn locals_and_the_operand_stack_stay_separate_lists() {
        let stop = VmStop::parse(&rendered()).expect("a stop");
        assert_eq!(stop.locals.len(), 2);
        assert_eq!(stop.locals[1].kind, "int");
        assert_eq!(stop.locals[1].value.as_deref(), Some("900"));
        assert_eq!(stop.locals[1].index, 1);
        assert_eq!(stop.stack.len(), 1);
        assert_eq!(stop.stack[0].index, 0);
        assert_eq!(stop.stack[0].kind, "int");
    }

    #[test]
    fn the_kira_backtrace_keeps_its_order_and_identifiers() {
        let stop = VmStop::parse(&rendered()).expect("a stop");
        assert_eq!(
            stop.backtrace,
            vec![
                VmFrame {
                    index: 0,
                    function: "discountAmount".to_owned(),
                    function_id: 4,
                    pc: 0,
                },
                VmFrame {
                    index: 1,
                    function: "main".to_owned(),
                    function_id: 11,
                    pc: 12,
                },
            ]
        );
    }

    /// A process that has not reached a probe publishes nothing, and that is
    /// an absent stop rather than a malformed one.
    #[test]
    fn an_empty_mirror_is_no_stop_at_all() {
        assert!(VmStop::parse("").is_none());
        assert!(VmStop::parse("\0\0\0").is_none());
    }

    #[test]
    fn a_value_without_a_payload_keeps_only_its_kind() {
        let stop = VmStop::parse(
            "kira-vm-stop function=main(0) pc=0 opcode=1 call_depth=1 stack_depth=0\n\
             \x20 locals:\n\
             \x20   [0] void\n",
        )
        .expect("a stop");
        assert_eq!(stop.locals[0].kind, "void");
        assert_eq!(stop.locals[0].value, None);
    }

    #[test]
    fn a_stop_round_trips_through_its_json_contract() {
        let stop = VmStop::parse(&rendered()).expect("a stop");
        let text = serde_json::to_string(&stop).expect("serialize");
        let parsed: VmStop = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(parsed, stop);
    }
}
