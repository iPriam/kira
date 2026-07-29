//! The synthesized rebuild that carries one generic instantiation into another
//! whose type arguments are `Any`.
//!
//! `Result<Int, E>` and `Result<Any, E>` used to be one value here. They are two
//! since erasure started boxing: the first holds the ten inline, the second
//! holds an erasure box carrying the ten and the type it was. So the assignment
//! is no longer free on this side either, and letting it stay free is not a
//! shortcut but a wrong answer with a delay on it — a later `match` binds a
//! payload typed `Any`, and comparing it against a directly-erased value finds
//! a bare `Int` where a box belongs.
//!
//! # Why a function rather than inline code
//!
//! The rebuild has to read a value twice — once for its tag, once for its
//! payload — and both [`Instruction::EnumTag`] and [`Instruction::EnumPayload`]
//! consume the enum they read. There is no stack duplication instruction, so
//! the value has to live in a local, and a local means a frame. A helper also
//! makes the recursive case terminate: an instantiation whose payload is an
//! instantiation of itself asks for the helper it is already emitting, and
//! finds the registered index rather than recursing forever.
//!
//! This mirrors the LLVM backend's `codegen::widening`, which emits the same
//! rebuild as a module-local leaf. Each engine carrying its own is the same
//! arrangement the clone and free leaves already use, and it keeps a hybrid
//! build honest: neither half calls into the other's helpers.
//!
//! # Ownership
//!
//! The helper takes the value in slot 0 and every path returns exactly one
//! owned value. Each `LoadLocal` pushes a copy — one more hold — and the
//! instruction that reads it consumes that copy, so the pair nets out. The
//! original in slot 0 is released by the frame on return
//! (`Interpreter::run` drops every local of a finished frame), which is why no
//! path here drops it explicitly and no path returns it without copying.

use std::collections::HashMap;

use kira_ir::IrProgram;
use kira_semantics_model::{ErasedTypeId, Type};

use super::CompileError;
use crate::op::Instruction;

/// What a synthesized helper is called in the module's function table.
///
/// One name for all of them: the table is indexed, and nothing calls a helper
/// by name. It exists so a diagnostic naming a function has something to say.
pub(super) const HELPER_NAME: &str = "<widen>";

/// One variant whose payload type differs between the two rows.
struct ChangedVariant {
    /// The variant's discriminant, which the tag test compares against.
    tag: u32,
    /// The payload type the source row declares.
    from: Type,
    /// The payload type the destination row declares.
    to: Type,
}

/// The synthesized widen helpers a module needs, by the pair they carry.
///
/// A pair maps to `None` when the two rows share a runtime form and the value
/// passes through untouched — an enum template that never puts its parameter in
/// a payload, instantiated twice.
#[derive(Default)]
pub(super) struct WidenHelpers {
    /// The helper for each pair, memoized. `None` means no rebuild is needed.
    registered: HashMap<(Type, Type), Option<u32>>,
    /// Helpers whose body is not emitted yet, in registration order.
    ///
    /// Emitting one may register more, so this is drained as a worklist rather
    /// than iterated.
    pending: Vec<(u32, Type, Type)>,
    /// Bodies already emitted, by helper index, ready to append to the module.
    emitted: Vec<(u32, Vec<Instruction>)>,
    /// The index the next helper takes, counting on from the program's own
    /// functions so an existing call site keeps the index it was lowered with.
    next_index: u32,
}

impl WidenHelpers {
    /// A registry whose helpers begin after `function_count` program functions.
    pub(super) fn new(function_count: u32) -> Self {
        Self {
            next_index: function_count,
            ..Self::default()
        }
    }

    /// The helper index carrying `from` into `to`, or `None` when the two rows
    /// share a runtime form.
    ///
    /// Registers the helper if this is the first ask. The body is emitted later
    /// — registering first is what lets a self-referencing instantiation find
    /// an index instead of recursing.
    pub(super) fn helper_for(
        &mut self,
        program: &IrProgram,
        from: Type,
        to: Type,
    ) -> Result<Option<u32>, CompileError> {
        if let Some(&cached) = self.registered.get(&(from, to)) {
            return Ok(cached);
        }
        let changed = changed_variants(program, from, to)?;
        if changed.is_empty() {
            self.registered.insert((from, to), None);
            return Ok(None);
        }
        let index = self.next_index;
        self.next_index += 1;
        self.registered.insert((from, to), Some(index));
        self.pending.push((index, from, to));
        Ok(Some(index))
    }

    /// Emits every pending body, including any registered while emitting.
    pub(super) fn emit_pending(&mut self, program: &IrProgram) -> Result<(), CompileError> {
        while let Some((index, from, to)) = self.pending.pop() {
            let code = self.emit_body(program, from, to)?;
            self.emitted.push((index, code));
        }
        Ok(())
    }

    /// The emitted helpers in index order, ready to append to the module.
    pub(super) fn into_protos(mut self) -> Vec<(u32, Vec<Instruction>)> {
        self.emitted.sort_by_key(|(index, _)| *index);
        self.emitted
    }

    /// One helper's body: test each changed tag, rebuild that variant, and hand
    /// every other one straight back.
    fn emit_body(
        &mut self,
        program: &IrProgram,
        from: Type,
        to: Type,
    ) -> Result<Vec<Instruction>, CompileError> {
        let changed = changed_variants(program, from, to)?;
        let mut code = Vec::new();
        for variant in &changed {
            // `LoadLocal` copies, and `EnumTag` consumes that copy, so slot 0
            // still holds the value the next test reads.
            code.push(Instruction::LoadLocal(0));
            code.push(Instruction::EnumTag);
            code.push(Instruction::ConstInt(i64::from(variant.tag)));
            code.push(Instruction::EqInt);
            let to_next = code.len();
            code.push(Instruction::JumpIfFalse(0));

            code.push(Instruction::LoadLocal(0));
            code.push(Instruction::EnumPayload);
            self.emit_payload_crossing(program, &mut code, variant.from, variant.to)?;
            code.push(Instruction::NewEnum {
                tag: u16::try_from(variant.tag).map_err(|_| CompileError::TooManyVariants {
                    function: HELPER_NAME.to_owned(),
                    tag: variant.tag,
                })?,
                has_payload: true,
            });
            code.push(Instruction::Return);

            code[to_next] = Instruction::JumpIfFalse(code.len() as u32);
        }
        // No changed tag matched: the value passes through, copied out of the
        // slot the frame is about to drop.
        code.push(Instruction::LoadLocal(0));
        code.push(Instruction::Return);
        Ok(code)
    }

    /// Carries one payload from the source row's type to the destination row's.
    ///
    /// Exactly the two crossings the type rule admits — into the top type, or
    /// into another instantiation — which is what makes the last arm a guard on
    /// the type rule rather than a gap in this one.
    fn emit_payload_crossing(
        &mut self,
        program: &IrProgram,
        code: &mut Vec<Instruction>,
        from: Type,
        to: Type,
    ) -> Result<(), CompileError> {
        if from == to {
            return Ok(());
        }
        if to == Type::Any {
            let type_id = ErasedTypeId::of(from).ok_or(CompileError::ErasureOfAValuelessType)?;
            code.push(Instruction::Erase(type_id.as_u64()));
            return Ok(());
        }
        if !matches!((from, to), (Type::Enum(_), Type::Enum(_))) {
            return Err(CompileError::WidenedPayloadTypeRefused);
        }
        if let Some(index) = self.helper_for(program, from, to)? {
            code.push(Instruction::Call(index));
        }
        Ok(())
    }
}

/// The variants whose payload type differs between the two rows.
///
/// Empty means the rows have identical variants, which happens whenever a
/// template never puts its parameter in a payload — `enum Marker<T> { On Off }`
/// instantiated twice. Such a value needs no rebuild at all.
fn changed_variants(
    program: &IrProgram,
    from: Type,
    to: Type,
) -> Result<Vec<ChangedVariant>, CompileError> {
    let (Type::Enum(from_id), Type::Enum(to_id)) = (from, to) else {
        return Err(CompileError::WidenedNonEnum);
    };
    let enums = program.types.enums();
    let (from_def, to_def) = match (enums.get(from_id), enums.get(to_id)) {
        (Some(from_def), Some(to_def)) => (from_def, to_def),
        _ => return Err(CompileError::WidenedUndeclaredEnum),
    };
    if from_def.variants.len() != to_def.variants.len() {
        return Err(CompileError::WidenedMismatchedRows);
    }
    let mut changed = Vec::new();
    for (tag, (source, target)) in from_def
        .variants
        .iter()
        .zip(to_def.variants.iter())
        .enumerate()
    {
        match (source.payload, target.payload) {
            (Some(source_ty), Some(target_ty)) if source_ty != target_ty => {
                changed.push(ChangedVariant {
                    tag: tag as u32,
                    from: source_ty,
                    to: target_ty,
                });
            }
            (None, None) | (Some(_), Some(_)) => {}
            _ => return Err(CompileError::WidenedMismatchedRows),
        }
    }
    Ok(changed)
}
