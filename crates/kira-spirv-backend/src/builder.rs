//! Assembling a SPIR-V module: ids, sections, and the interned type and
//! constant pools.
//!
//! A SPIR-V module is not written in the order it is built. Its sections have a
//! fixed order — capabilities, then the memory model, then entry points, then
//! debug names, then decorations, then every type and global, and only then the
//! functions — while emission discovers a type in the middle of a function body
//! and a decoration in the middle of a resource. So each section is its own word
//! buffer here, appended to from wherever, and concatenated once at the end.
//!
//! Types and constants are pooled because SPIR-V requires it: two `OpTypeInt 32
//! 1` instructions in one module are not two types, they are an invalid module.

use std::collections::HashMap;

use crate::spec::{self, op};

/// A SPIR-V result id.
///
/// A newtype rather than a bare `u32`: an id, a literal, and a type id are all
/// words, and passing one where another belongs is the mistake this makes
/// impossible to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Id(u32);

impl Id {
    /// The raw word this id encodes as.
    pub(crate) fn word(self) -> u32 {
        self.0
    }
}

/// What a pooled type is, structurally.
///
/// A struct is keyed by name because KSL's structs are nominal — two structs
/// with identical fields are two types, and merging them would make a
/// diagnostic about one point at the other.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum TypeKey {
    Void,
    Bool,
    Int {
        signed: bool,
    },
    Float,
    Vector(Id, u32),
    Matrix(Id, u32),
    Struct(String),
    RuntimeArray(Id),
    Pointer(u32, Id),
    Function(Id, Vec<Id>),
    Image {
        dim: u32,
        sampled_type: Id,
        depth: u32,
    },
    Sampler,
    SampledImage(Id),
}

/// The module being assembled.
pub(crate) struct Builder {
    next_id: u32,
    capabilities: Vec<u32>,
    ext_inst_imports: Vec<u32>,
    memory_model: Vec<u32>,
    entry_points: Vec<u32>,
    execution_modes: Vec<u32>,
    debug: Vec<u32>,
    annotations: Vec<u32>,
    globals: Vec<u32>,
    functions: Vec<u32>,
    types: HashMap<TypeKey, Id>,
    constants: HashMap<(Id, u64), Id>,
    /// The `GLSL.std.450` import every extended math call names.
    pub(crate) glsl: Id,
}

impl Builder {
    /// A builder holding the fixed preamble every module starts with.
    pub(crate) fn new() -> Self {
        let mut builder = Self {
            next_id: 1,
            capabilities: Vec::new(),
            ext_inst_imports: Vec::new(),
            memory_model: Vec::new(),
            entry_points: Vec::new(),
            execution_modes: Vec::new(),
            debug: Vec::new(),
            annotations: Vec::new(),
            globals: Vec::new(),
            functions: Vec::new(),
            types: HashMap::new(),
            constants: HashMap::new(),
            glsl: Id(0),
        };
        instruction(
            &mut builder.capabilities,
            op::CAPABILITY,
            &[spec::capability::SHADER],
        );
        let glsl = builder.fresh();
        let mut operands = vec![glsl.word()];
        operands.extend(literal_string(spec::glsl::NAME));
        instruction(
            &mut builder.ext_inst_imports,
            op::EXT_INST_IMPORT,
            &operands,
        );
        builder.glsl = glsl;
        instruction(
            &mut builder.memory_model,
            op::MEMORY_MODEL,
            &[spec::memory_model::LOGICAL, spec::memory_model::GLSL450],
        );
        builder
    }

    /// The next unused id.
    pub(crate) fn fresh(&mut self) -> Id {
        let at = self.next_id;
        self.next_id += 1;
        Id(at)
    }

    /// Records the module's one entry point and the interface it reaches.
    pub(crate) fn entry_point(&mut self, model: u32, entry: Id, name: &str, interface: &[Id]) {
        let mut operands = vec![model, entry.word()];
        operands.extend(literal_string(name));
        operands.extend(interface.iter().map(|id| id.word()));
        instruction(&mut self.entry_points, op::ENTRY_POINT, &operands);
    }

    /// Records one `OpExecutionMode`.
    pub(crate) fn execution_mode(&mut self, entry: Id, mode: u32, operands: &[u32]) {
        let mut words = vec![entry.word(), mode];
        words.extend_from_slice(operands);
        instruction(&mut self.execution_modes, op::EXECUTION_MODE, &words);
    }

    /// Names `target`, so a disassembly reads like the KSL it came from.
    pub(crate) fn name(&mut self, target: Id, name: &str) {
        let mut operands = vec![target.word()];
        operands.extend(literal_string(name));
        instruction(&mut self.debug, op::NAME, &operands);
    }

    /// Names one member of a struct type.
    pub(crate) fn member_name(&mut self, target: Id, member: u32, name: &str) {
        let mut operands = vec![target.word(), member];
        operands.extend(literal_string(name));
        instruction(&mut self.debug, op::MEMBER_NAME, &operands);
    }

    /// Decorates `target`.
    pub(crate) fn decorate(&mut self, target: Id, decoration: u32, operands: &[u32]) {
        let mut words = vec![target.word(), decoration];
        words.extend_from_slice(operands);
        instruction(&mut self.annotations, op::DECORATE, &words);
    }

    /// Decorates one member of a struct type.
    pub(crate) fn member_decorate(
        &mut self,
        target: Id,
        member: u32,
        decoration: u32,
        operands: &[u32],
    ) {
        let mut words = vec![target.word(), member, decoration];
        words.extend_from_slice(operands);
        instruction(&mut self.annotations, op::MEMBER_DECORATE, &words);
    }

    /// Appends an instruction to the types-and-globals section.
    pub(crate) fn global(&mut self, opcode: u16, operands: &[u32]) {
        instruction(&mut self.globals, opcode, operands);
    }

    /// Appends an instruction to the function section.
    pub(crate) fn code(&mut self, opcode: u16, operands: &[u32]) {
        instruction(&mut self.functions, opcode, operands);
    }

    /// The id of a pooled type, built by `make` the first time it is asked for.
    pub(crate) fn pooled(&mut self, key: TypeKey, make: impl FnOnce(&mut Self, Id)) -> Id {
        if let Some(&found) = self.types.get(&key) {
            return found;
        }
        let id = self.fresh();
        self.types.insert(key, id);
        make(self, id);
        id
    }

    /// A scalar constant of `ty` whose value encodes to `bits`.
    ///
    /// Pooled on the pair, because a module may hold `0` as an `int` and `0` as
    /// a `float` and those are two constants with the same bits.
    pub(crate) fn constant(&mut self, ty: Id, bits: u32) -> Id {
        if let Some(&found) = self.constants.get(&(ty, u64::from(bits))) {
            return found;
        }
        let id = self.fresh();
        self.constants.insert((ty, u64::from(bits)), id);
        instruction(
            &mut self.globals,
            op::CONSTANT,
            &[ty.word(), id.word(), bits],
        );
        id
    }

    /// A boolean constant.
    pub(crate) fn constant_bool(&mut self, ty: Id, value: bool) -> Id {
        // Keyed off the bit pattern the way a scalar is, with `true` and
        // `false` two entries rather than one — they are two instructions.
        let key = (ty, u64::from(value) | 1 << 32);
        if let Some(&found) = self.constants.get(&key) {
            return found;
        }
        let id = self.fresh();
        self.constants.insert(key, id);
        let opcode = if value {
            op::CONSTANT_TRUE
        } else {
            op::CONSTANT_FALSE
        };
        instruction(&mut self.globals, opcode, &[ty.word(), id.word()]);
        id
    }

    /// The finished module, in the section order the specification fixes.
    pub(crate) fn finish(self) -> Vec<u32> {
        let mut words = vec![
            spec::MAGIC,
            spec::VERSION,
            spec::GENERATOR,
            // The id bound: one past the largest id handed out.
            self.next_id,
            0,
        ];
        for section in [
            self.capabilities,
            self.ext_inst_imports,
            self.memory_model,
            self.entry_points,
            self.execution_modes,
            self.debug,
            self.annotations,
            self.globals,
            self.functions,
        ] {
            words.extend(section);
        }
        words
    }
}

/// Appends one instruction to `into`.
///
/// The first word packs the operand count — the whole instruction's, this
/// header word included — into the high half and the opcode into the low half.
fn instruction(into: &mut Vec<u32>, opcode: u16, operands: &[u32]) {
    let count = u32::try_from(operands.len() + 1).unwrap_or(u32::MAX);
    into.push((count << 16) | u32::from(opcode));
    into.extend_from_slice(operands);
}

/// A literal string as SPIR-V encodes one: UTF-8, NUL-terminated, packed four
/// bytes to a word, little end first, padded with zeroes.
pub(crate) fn literal_string(text: &str) -> Vec<u32> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| {
            u32::from(chunk[0])
                | (u32::from(chunk[1]) << 8)
                | (u32::from(chunk[2]) << 16)
                | (u32::from(chunk[3]) << 24)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instruction_header_packs_its_whole_length_and_its_opcode() {
        let mut words = Vec::new();
        instruction(&mut words, op::RETURN, &[]);
        assert_eq!(words, vec![(1 << 16) | u32::from(op::RETURN)]);
    }

    #[test]
    fn a_string_is_nul_terminated_and_padded_to_a_whole_word() {
        // Four characters plus the NUL is five bytes, so it takes two words and
        // the second is three zero bytes after the terminator.
        assert_eq!(literal_string("main"), vec![0x6e69_616d, 0x0000_0000]);
        assert_eq!(literal_string("ab"), vec![0x0000_6261]);
    }

    #[test]
    fn the_header_bound_is_one_past_the_last_id_handed_out() {
        let mut builder = Builder::new();
        let last = builder.fresh();
        let words = builder.finish();
        assert_eq!(words[0], spec::MAGIC);
        assert_eq!(words[3], last.word() + 1);
    }

    #[test]
    fn a_type_asked_for_twice_is_one_instruction() {
        let mut builder = Builder::new();
        let mut built = 0;
        let mut make = |builder: &mut Builder, id: Id| {
            built += 1;
            builder.global(op::TYPE_BOOL, &[id.word()]);
        };
        let first = builder.pooled(TypeKey::Bool, &mut make);
        let second = builder.pooled(TypeKey::Bool, &mut make);
        assert_eq!(first, second);
        assert_eq!(built, 1, "the second ask reused the first instruction");
    }
}
