//! What the emitted SPIR-V must say, checked end to end from KSL source.
//!
//! There is no validator on the build machines, so these read the word stream
//! itself: the header, then instruction by instruction. That is a lower bar than
//! `spirv-val` and an honest one — it pins what this backend decided, and says
//! nothing about the module being valid beyond the decisions it checks.

use kira_ksl_semantics::{Module, check};
use kira_shader_ir::{ShaderIr, lower};
use kira_shader_model::{BackendTarget, Stage};
use kira_source::SourceId;

use crate::spec::{self, built_in, decoration, execution_mode, execution_model, op, storage_class};
use crate::{SpirvError, emit};

/// Parses, checks, and lowers `text` for Vulkan.
fn build(text: &str) -> ShaderIr {
    let parsed = kira_ksl_parser::parse(SourceId::new(0), text);
    assert!(parsed.is_clean(), "{:?}", parsed.diagnostics);
    let checked = check(
        &Module {
            source: SourceId::new(0),
            tree: parsed.tree,
            interner: parsed.interner,
        },
        &[],
    );
    assert!(
        checked.is_clean(),
        "{:?}",
        checked
            .diagnostics
            .iter()
            .map(|d| (d.code.clone(), d.message.clone()))
            .collect::<Vec<_>>()
    );
    lower(checked.module, BackendTarget::Spirv)
}

/// One decoded instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Instruction {
    opcode: u16,
    operands: Vec<u32>,
}

/// Every instruction in a module, the five header words skipped.
///
/// Total: a stream whose length word runs past the end stops the walk rather
/// than indexing off it, so a malformed emission fails a test instead of the
/// test harness.
fn decode(words: &[u32]) -> Vec<Instruction> {
    let mut found = Vec::new();
    let mut at = 5;
    while at < words.len() {
        let header = words[at];
        let count = (header >> 16) as usize;
        let opcode = u16::try_from(header & 0xFFFF).unwrap_or(0);
        if count == 0 || at + count > words.len() {
            break;
        }
        found.push(Instruction {
            opcode,
            operands: words[at + 1..at + count].to_vec(),
        });
        at += count;
    }
    found
}

/// Every instruction with `opcode`.
fn all(words: &[u32], opcode: u16) -> Vec<Instruction> {
    decode(words)
        .into_iter()
        .filter(|instruction| instruction.opcode == opcode)
        .collect()
}

/// Whether any instruction carries `opcode`.
fn has(words: &[u32], opcode: u16) -> bool {
    !all(words, opcode).is_empty()
}

/// Whether some `OpDecorate` says exactly this.
fn decorated(words: &[u32], decoration: u32, operands: &[u32]) -> bool {
    all(words, op::DECORATE).iter().any(|instruction| {
        instruction.operands.get(1) == Some(&decoration)
            && instruction.operands.get(2..) == Some(operands)
    })
}

/// Whether some `OpMemberDecorate` says exactly this about member `member`.
fn member_decorated(words: &[u32], member: u32, decoration: u32, operands: &[u32]) -> bool {
    all(words, op::MEMBER_DECORATE).iter().any(|instruction| {
        instruction.operands.get(1) == Some(&member)
            && instruction.operands.get(2) == Some(&decoration)
            && instruction.operands.get(3..) == Some(operands)
    })
}

/// The literal string starting at `at` in `operands`.
fn string_at(operands: &[u32], at: usize) -> String {
    let bytes: Vec<u8> = operands[at..]
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .take_while(|&byte| byte != 0)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

const TEXTURED: &str = r#"
type Camera {
    let view_projection: Float4x4
}

type VIn {
    let position: Float3
    let uv: Float2
}

type VOut {
    @builtin(position)
    let clip_position: Float4
    let uv: Float2
}

type FOut {
    let color: Float4
}

shader Textured {
    group Frame {
        uniform camera: Camera
    }

    group Material {
        texture albedo: Texture2d
        sampler albedo_sampler: Sampler
    }

    vertex {
        input VIn
        output VOut
        function entry(v: VIn) -> VOut {
            let r: VOut
            r.clip_position = mul(camera.view_projection, Float4(v.position, 1.0))
            r.uv = v.uv
            return r
        }
    }

    fragment {
        input VOut
        output FOut
        function entry(f: VOut) -> FOut {
            let r: FOut
            r.color = sample(albedo, albedo_sampler, f.uv)
            return r
        }
    }
}
"#;

#[test]
fn the_header_is_the_one_a_driver_reads_before_anything_else() {
    let words = emit(&build(TEXTURED), Stage::Vertex).expect("vertex");
    assert_eq!(words[0], spec::MAGIC);
    assert_eq!(words[1], spec::VERSION, "SPIR-V 1.3");
    // The bound has to exceed every id the module defines, or a driver rejects
    // it before reading an instruction. Result ids are read from the two
    // opcodes whose operand positions are unambiguous.
    let defined: Vec<u32> = all(&words, op::LABEL)
        .iter()
        .filter_map(|instruction| instruction.operands.first().copied())
        .chain(
            all(&words, op::VARIABLE)
                .iter()
                .filter_map(|instruction| instruction.operands.get(1).copied()),
        )
        .collect();
    assert!(!defined.is_empty(), "the module defines ids at all");
    let largest = defined.iter().copied().max().unwrap_or(0);
    assert!(
        words[3] > largest,
        "the bound {} does not cover id {largest}",
        words[3]
    );
    assert_eq!(words[4], 0, "the reserved word");
}

#[test]
fn the_entry_point_names_its_stage_and_its_interface() {
    let words = emit(&build(TEXTURED), Stage::Vertex).expect("vertex");
    let entry = all(&words, op::ENTRY_POINT);
    let entry = entry.first().expect("one entry point");
    assert_eq!(entry.operands[0], execution_model::VERTEX);
    assert_eq!(string_at(&entry.operands, 2), "vertex_main");
    // A SPIR-V entry point takes no parameters: its interface is the variables
    // it names here, and one left out is one a driver will not link. The name
    // is NUL-terminated and padded to a whole word, so the interface starts
    // exactly that many words after it.
    let name_words = ("vertex_main".len() + 1).div_ceil(4);
    let interface = &entry.operands[2 + name_words..];
    let declared: Vec<u32> = all(&words, op::VARIABLE)
        .iter()
        .filter(|variable| {
            matches!(
                variable.operands.get(2),
                Some(&storage_class::INPUT | &storage_class::OUTPUT)
            )
        })
        .filter_map(|variable| variable.operands.get(1).copied())
        .collect();
    assert_eq!(
        interface.len(),
        declared.len(),
        "every input and output variable is listed: {interface:?} vs {declared:?}"
    );
    for id in &declared {
        assert!(interface.contains(id), "{id} is missing from the interface");
    }
}

#[test]
fn a_fragment_stage_says_where_its_origin_is() {
    // A shader that leaves this out reads `FragCoord`'s y from the other end of
    // the framebuffer, which is an upside-down image and not an error.
    let words = emit(&build(TEXTURED), Stage::Fragment).expect("fragment");
    let modes = all(&words, op::EXECUTION_MODE);
    assert!(
        modes
            .iter()
            .any(|mode| mode.operands.get(1) == Some(&execution_mode::ORIGIN_UPPER_LEFT)),
        "{modes:?}"
    );
}

#[test]
fn an_interface_member_is_a_variable_carrying_a_location_or_a_builtin() {
    let words = emit(&build(TEXTURED), Stage::Vertex).expect("vertex");
    assert!(decorated(
        &words,
        decoration::BUILT_IN,
        &[built_in::POSITION]
    ));
    assert!(decorated(&words, decoration::LOCATION, &[0]));
    assert!(decorated(&words, decoration::LOCATION, &[1]));
    let inputs = all(&words, op::VARIABLE)
        .iter()
        .filter(|variable| variable.operands.get(2) == Some(&storage_class::INPUT))
        .count();
    assert_eq!(inputs, 2, "one per member of the vertex input struct");
}

#[test]
fn a_uniform_block_carries_the_layout_the_host_packs_to() {
    let words = emit(&build(TEXTURED), Stage::Vertex).expect("vertex");
    assert!(decorated(&words, decoration::BLOCK, &[]));
    assert!(member_decorated(&words, 0, decoration::OFFSET, &[0]));
    // Both halves of a matrix have to be said. Either one missing and a driver
    // reads the host's bytes transposed or at the wrong stride.
    assert!(member_decorated(&words, 0, decoration::COL_MAJOR, &[]));
    assert!(member_decorated(
        &words,
        0,
        decoration::MATRIX_STRIDE,
        &[16]
    ));
    assert!(decorated(&words, decoration::DESCRIPTOR_SET, &[0]));
    assert!(decorated(&words, decoration::BINDING, &[0]));
}

#[test]
fn a_matrix_times_a_vector_is_not_a_componentwise_multiply() {
    let words = emit(&build(TEXTURED), Stage::Vertex).expect("vertex");
    assert!(has(&words, op::MATRIX_TIMES_VECTOR));
    assert!(
        !has(&words, op::F_MUL),
        "no lane-by-lane product was emitted"
    );
}

#[test]
fn a_sample_combines_the_image_and_the_sampler_it_was_given() {
    let words = emit(&build(TEXTURED), Stage::Fragment).expect("fragment");
    assert!(has(&words, op::SAMPLED_IMAGE));
    assert!(has(&words, op::IMAGE_SAMPLE_IMPLICIT_LOD));
    let handles = all(&words, op::VARIABLE)
        .iter()
        .filter(|variable| variable.operands.get(2) == Some(&storage_class::UNIFORM_CONSTANT))
        .count();
    assert_eq!(handles, 2, "the texture and the sampler");
}

#[test]
fn a_type_asked_for_twice_is_declared_once() {
    // Two `OpTypeFloat 32` instructions in one module are not two types, they
    // are an invalid module.
    let words = emit(&build(TEXTURED), Stage::Vertex).expect("vertex");
    assert_eq!(all(&words, op::TYPE_FLOAT).len(), 1);
    assert_eq!(all(&words, op::TYPE_VOID).len(), 1);
}

const COMPUTE: &str = r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}

type Particle {
    let px: Float
    let vy: Float
}

shader Step {
    group Work {
        storage read_write particles: [Particle]
    }
    compute {
        input QIn
        threads(64, 1, 1)
        function entry(q: QIn) {
            if q.gid.x < particles.count {
                particles[q.gid.x].px = particles[q.gid.x].px + 1.0
            }
            return
        }
    }
}
"#;

#[test]
fn a_compute_entry_declares_the_workgroup_it_runs_as() {
    let words = emit(&build(COMPUTE), Stage::Compute).expect("compute");
    let entry = all(&words, op::ENTRY_POINT);
    assert_eq!(entry[0].operands[0], execution_model::GL_COMPUTE);
    let modes = all(&words, op::EXECUTION_MODE);
    let local_size = modes
        .iter()
        .find(|mode| mode.operands.get(1) == Some(&execution_mode::LOCAL_SIZE))
        .expect("a local size");
    assert_eq!(local_size.operands[2..], [64, 1, 1]);
    assert!(decorated(
        &words,
        decoration::BUILT_IN,
        &[built_in::GLOBAL_INVOCATION_ID]
    ));
}

#[test]
fn a_storage_buffer_says_how_far_apart_its_elements_are() {
    let words = emit(&build(COMPUTE), Stage::Compute).expect("compute");
    // Two floats round up to 16 under this workspace's layout, and a driver
    // steps the array by what this says rather than by what it would guess.
    assert!(decorated(&words, decoration::ARRAY_STRIDE, &[16]));
    assert!(has(&words, op::TYPE_RUNTIME_ARRAY));
    let buffers = all(&words, op::VARIABLE)
        .iter()
        .filter(|variable| variable.operands.get(2) == Some(&storage_class::STORAGE_BUFFER))
        .count();
    assert_eq!(buffers, 1);
}

#[test]
fn a_buffers_length_is_asked_of_the_block_rather_than_the_array() {
    let words = emit(&build(COMPUTE), Stage::Compute).expect("compute");
    let lengths = all(&words, op::ARRAY_LENGTH);
    let length = lengths.first().expect("one length");
    // The member index: the array is the block's one member, and asking about
    // any other member would answer about nothing.
    assert_eq!(length.operands.get(3), Some(&0));
}

#[test]
fn an_if_names_where_its_branches_rejoin() {
    // A module whose control flow does not say where it merges is not a valid
    // module — a driver is entitled to reject it rather than work it out.
    let words = emit(&build(COMPUTE), Stage::Compute).expect("compute");
    assert!(has(&words, op::SELECTION_MERGE));
    assert!(has(&words, op::BRANCH_CONDITIONAL));
}

#[test]
fn a_read_only_buffer_says_it_is_never_written() {
    let ir = build(
        r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}
shader S {
    group Work {
        storage read page: [Float]
    }
    compute {
        input QIn
        threads(1, 1, 1)
        function entry(q: QIn) {
            let v = page[q.gid.x]
            return
        }
    }
}
"#,
    );
    let words = emit(&ir, Stage::Compute).expect("compute");
    assert!(decorated(&words, decoration::NON_WRITABLE, &[]));
    assert!(decorated(&words, decoration::ARRAY_STRIDE, &[4]));
}

#[test]
fn an_atomic_is_one_instruction_over_the_element_it_names() {
    let ir = build(
        r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}
shader S {
    group Work {
        storage read_write counter: [UInt]
    }
    compute {
        input QIn
        threads(1, 1, 1)
        function entry(q: QIn) {
            let zero: UInt = 0
            let one: UInt = 1
            let slot = atomicAdd(counter, zero, one)
            counter[slot] = q.gid.x
            return
        }
    }
}
"#,
    );
    let words = emit(&ir, Stage::Compute).expect("compute");
    assert!(has(&words, op::ATOMIC_I_ADD));
}

#[test]
fn a_loop_names_its_merge_and_its_continue_target() {
    let ir = build(
        r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}
shader S {
    group Work {
        storage read_write counter: [UInt]
    }
    compute {
        input QIn
        threads(1, 1, 1)
        function entry(q: QIn) {
            let i: UInt = 0
            while i < counter.count {
                i = i + 1
            }
            return
        }
    }
}
"#,
    );
    let words = emit(&ir, Stage::Compute).expect("compute");
    let merges = all(&words, op::LOOP_MERGE);
    let merge = merges.first().expect("a loop merge");
    assert_eq!(merge.operands.len(), 3, "merge, continue, and the flags");
    // The condition is inside the loop: a length read hoisted out of it would
    // be read once and never again.
    assert!(has(&words, op::ARRAY_LENGTH));
}

#[test]
fn a_compute_stage_that_declares_an_output_is_refused_by_name() {
    let ir = build(
        r#"
type QIn {
    @builtin(thread_id)
    let gid: UInt3
}
type QOut {
    let value: Float
}
shader S {
    group Work {
        storage read_write counter: [UInt]
    }
    compute {
        input QIn
        output QOut
        threads(1, 1, 1)
        function entry(q: QIn) -> QOut {
            let r: QOut
            r.value = 1.0
            return r
        }
    }
}
"#,
    );
    let error = emit(&ir, Stage::Compute).expect_err("a compute output");
    assert_eq!(
        error,
        SpirvError::ComputeStageWithOutput {
            shader: "S".to_owned(),
            output: "QOut".to_owned(),
        }
    );
}

#[test]
fn a_stage_the_shader_does_not_declare_emits_nothing() {
    let words = emit(&build(COMPUTE), Stage::Vertex).expect("no vertex stage");
    assert!(words.is_empty());
    assert_eq!(crate::hex(&words), "");
}

#[test]
fn the_hexadecimal_form_is_read_eight_characters_to_a_word() {
    let words = emit(&build(TEXTURED), Stage::Vertex).expect("vertex");
    let text = crate::hex(&words);
    // The magic first, so a host that got the string can tell immediately
    // whether it is holding a SPIR-V module at all.
    assert!(
        text.starts_with("07230203"),
        "{}",
        &text[..16.min(text.len())]
    );
    assert_eq!(&text[8..16], "00010300", "the version word");
    assert_eq!(text.len(), words.len() * 8);
    // Every character is a hexadecimal digit: a host parses this eight at a
    // time and has nothing else to skip.
    assert!(text.chars().all(|character| character.is_ascii_hexdigit()));
}
